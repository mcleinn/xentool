//! openxen `serve` — drain module streams, run the key machines, emit MIDI, broadcast
//! MTS-ESP, publish HUD/TUI snapshots.
//!
//! Same shape as the Wooting serve loop: a ~1 kHz hot loop with no disk I/O and no
//! printing inside (TUI log lines go through a bounded channel on state transitions only).
//! Differences: frames arrive from reader threads (`hid.rs`) at ~4 kHz per module and are
//! processed in order with the module's own timestamps; keys are addressed by board and
//! Lumatone key index (no HID keycodes, no control bar). LEDs are not driven yet.

use anyhow::{Context, Result};
use crossbeam_channel::{bounded, Receiver};
use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use crate::mts::MtsMaster;
use crate::openxen::hid::{Event, Manager};
use crate::openxen::keys::{Action, KeyMachine, KeyParams, VelocitySource};
use crate::openxen::proto::MAX_KEYS;
use crate::settings::OpenxenSettings;
use crate::wooting::modes::{AftertouchMode, VelocityProfile, default_velocity_profiles};
use crate::wooting::serve::publish_tuning;
use crate::wooting::ui::{DeviceLine, HeldKeyDisplay, SNAPSHOT_INTERVAL, WootingSnapshot, run_wooting_serve_ui, snapshot_due};
use crate::wooting::wtn::{Wtn, parse_wtn};

const POLL_INTERVAL: Duration = Duration::from_millis(1);
const HOTPLUG_INTERVAL: Duration = Duration::from_millis(500);
const STATS_INTERVAL: Duration = Duration::from_secs(10);
/// Frames buffered between the reader threads and the hot loop (~0.8 s of 5 modules).
const CHANNEL_CAPACITY: usize = 16_384;
pub const MAX_BOARDS: u8 = 5;

// --- MIDI output (same transports as the Wooting backend) ---

struct MidiOut {
    #[cfg(target_os = "linux")]
    conn: crate::midi_out::HwMidiOut,
    #[cfg(not(target_os = "linux"))]
    conn: crate::midi_out::MidiOutPort,
}

impl MidiOut {
    fn open(name: &str) -> Result<Self> {
        #[cfg(target_os = "linux")]
        let conn = crate::midi_out::open_output_bidirectional("xentool-openxen", name)?;
        #[cfg(not(target_os = "linux"))]
        let conn = crate::midi_out::open_output("xentool-openxen", name)?;
        Ok(Self { conn })
    }
    fn send(&mut self, msg: [u8; 3]) {
        let _ = self.conn.send(&msg);
    }
    fn all_notes_off(&mut self) {
        for ch in 0u8..16 {
            self.send([0xB0 | ch, 123, 0]);
        }
    }
}

// --- settings -> key parameters ---

pub fn key_params(s: &OpenxenSettings) -> KeyParams {
    KeyParams {
        press_threshold: s.press_threshold,
        release_delta: s.release_delta,
        velocity_window_us: (s.velocity_window_ms.max(0.0) * 1000.0) as u64,
        velocity_source: if s.velocity_source.eq_ignore_ascii_case("depth") { VelocitySource::Depth } else { VelocitySource::Speed },
        velocity_speed_max: s.velocity_speed_max,
        speed_span_us: (s.speed_span_ms.max(0.0) * 1000.0) as u64,
        aftertouch_mode: match s.aftertouch_mode.to_ascii_lowercase().as_str() {
            "speed-mapped" | "speed" => AftertouchMode::SpeedMapped,
            "peak-mapped" | "peak" => AftertouchMode::PeakMapped,
            _ => AftertouchMode::Off,
        },
        aftertouch_delta: s.aftertouch_delta,
        aftertouch_speed_max: s.aftertouch_speed_max,
    }
}

// --- board assignment (serial -> .ltn board) ---

/// Configured serials keep their board; others take the lowest free board in order of
/// appearance. Returns None when all boards are taken.
pub fn assign_board(serial: &str, settings: &OpenxenSettings, taken: &BTreeMap<u8, Arc<str>>) -> Option<u8> {
    if let Some(b) = settings.boards.iter().find(|b| b.serial == serial).map(|b| b.board) {
        return (b < MAX_BOARDS && !taken.contains_key(&b)).then_some(b);
    }
    let reserved: Vec<u8> = settings.boards.iter().map(|b| b.board).collect();
    (0..MAX_BOARDS).find(|b| !taken.contains_key(b) && !reserved.contains(b))
        .or_else(|| (0..MAX_BOARDS).find(|b| !taken.contains_key(b)))
}

// --- per-module state ---

struct Module {
    board: u8,
    keys: Vec<KeyMachine>,
    /// (out channel 0..15, note) of the note sounding on each key, resolved at note-on.
    sounding: Vec<Option<(u8, u8)>>,
    frames: u64,
    lost: u64,
    last_time_us: u64,
}

impl Module {
    fn new(board: u8) -> Self {
        Self {
            board,
            keys: vec![KeyMachine::default(); MAX_KEYS],
            sounding: vec![None; MAX_KEYS],
            frames: 0,
            lost: 0,
            last_time_us: 0,
        }
    }

    fn release_all(&mut self, midi: &mut MidiOut) {
        for k in 0..MAX_KEYS {
            self.keys[k].reset();
            if let Some((ch, note)) = self.sounding[k].take() {
                midi.send([0x80 | ch, note, 0]);
            }
        }
    }
}

/// Map one key action to MIDI. Note-on resolves the layout cell (board, key) at press
/// time; note-off and pressure use what was resolved, so a layout change mid-note cannot
/// leave hanging notes.
fn apply(action: Action, m: &mut Module, key: usize, wtn: &Wtn, midi: &mut MidiOut) {
    match action {
        Action::NoteOn { velocity } => {
            if let Some(cell) = wtn.cell(m.board, key).filter(|c| c.chan != 0) {
                let ch = cell.chan.saturating_sub(1).min(15);
                midi.send([0x90 | ch, cell.key & 0x7F, velocity & 0x7F]);
                m.sounding[key] = Some((ch, cell.key & 0x7F));
            }
        }
        Action::NoteOff => {
            if let Some((ch, note)) = m.sounding[key].take() {
                midi.send([0x80 | ch, note, 0]);
            }
        }
        Action::Pressure { value } => {
            if let Some((ch, note)) = m.sounding[key] {
                midi.send([0xA0 | ch, note, value & 0x7F]);
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub fn cmd_serve_ltn(
    file: PathBuf,
    midi_port: String,
    hud: bool,
    hud_port: u16,
    xenharm_url: String,
    osc_port: u16,
    tune_supercollider: bool,
    settings: &OpenxenSettings,
) -> Result<()> {
    let content = std::fs::read_to_string(&file).with_context(|| format!("reading {}", file.display()))?;
    let wtn = parse_wtn(&content).with_context(|| format!("parsing {}", file.display()))?;
    let edo = wtn.edo.with_context(|| {
        format!("Edo= not set in {}; add e.g. `Edo=31` before the first [Board] section.", file.display())
    })?;
    let layout_filename = file.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();

    // HUD (same publisher/ctx as the Wooting backend: both read a Wtn).
    let hud_publisher = crate::hud::HudPublisher::new(crate::hud::LiveState::empty("openxen"));
    let hud_url: Option<String> = if hud {
        let xen = crate::hud::xenharm::XenharmClient::start(&xenharm_url);
        let osc = if osc_port > 0 {
            match crate::hud::osc::OscClient::start(osc_port) {
                Ok((c, port)) => {
                    eprintln!("xentool HUD: OSC listening on udp://0.0.0.0:{port}");
                    Some(c)
                }
                Err(e) => {
                    eprintln!("xentool HUD: OSC disabled ({e:#})");
                    None
                }
            }
        } else {
            None
        };
        crate::hud::server::spawn(hud_publisher.clone(), hud_port, xen, osc)?;
        Some(format!("http://localhost:{hud_port}/"))
    } else {
        None
    };
    if tune_supercollider {
        if let Err(e) = crate::hud::osc::spawn_tuning_broadcaster(hud_publisher.clone(), crate::hud::osc::DEFAULT_SC_TUNING_TARGET.into()) {
            eprintln!("xentool: tuning broadcast disabled ({e:#})");
        }
    }

    let master = MtsMaster::register().context("registering MTS-ESP master")?;
    master.set_scale_name(&format!("{edo}-EDO"))?;
    publish_tuning(&master, edo, wtn.pitch_offset)?;
    let mut midi = MidiOut::open(&midi_port)?;

    let (ev_tx, ev_rx) = bounded::<Event>(CHANNEL_CAPACITY);
    let mut manager = Manager::new(ev_tx)?;
    manager.refresh()?;

    let shutdown = Arc::new(AtomicBool::new(false));
    let shutdown_ctrlc = shutdown.clone();
    ctrlc::set_handler(move || shutdown_ctrlc.store(true, Ordering::Relaxed)).context("failed to set Ctrl+C handler")?;

    let (snap_tx, snap_rx) = bounded::<WootingSnapshot>(2);
    let (log_tx, log_rx) = bounded::<String>(64);
    let _ = log_tx.try_send(format!("xentool serve (openxen, experimental): {edo}-EDO | MIDI → `{midi_port}` | MTS-ESP master registered"));
    let _ = log_tx.try_send("xentool serve: reading openxen module streams (Ctrl+C / q to stop)".to_string());
    let shutdown_ui = shutdown.clone();
    let hud_url_for_ui = hud_url.clone();
    let ui_handle = thread::spawn(move || {
        if let Err(e) = run_wooting_serve_ui(snap_rx, log_rx, shutdown_ui, hud_url_for_ui) {
            eprintln!("xentool serve TUI error: {e}");
        }
    });

    let hud_ctx: Option<crate::wooting::hud_ctx::HudWootingHandle> = if hud || tune_supercollider {
        let layout_id = crate::hud::layout_id_from_path(&file);
        Some(
            crate::wooting::hud_ctx::HudWootingCtx {
                publisher: hud_publisher.clone(),
                layout_id: layout_id.clone(),
                layout_name: layout_id,
                edo,
                pitch_offset: wtn.pitch_offset,
                layout_pitches: crate::wooting::hud_ctx::build_layout_pitches(&wtn),
            }
            .into_handle(),
        )
    } else {
        None
    };

    // Persist the layout as the last used one (background thread; never in the hot loop).
    {
        let p = file.clone();
        thread::spawn(move || crate::settings::store_last_layout(crate::layouts::LayoutKind::Ltn, &p));
    }

    let params = key_params(settings);
    let profiles = default_velocity_profiles();
    let profile: VelocityProfile = profiles.get(settings.velocity_profile).cloned().unwrap_or(VelocityProfile::Linear);

    let mut modules: HashMap<Arc<str>, Module> = HashMap::new();
    let mut boards: BTreeMap<u8, Arc<str>> = BTreeMap::new();
    let mut last_hotplug = Instant::now();
    let mut last_stats = Instant::now();
    let mut last_overflow = 0u64;
    let mut next_snapshot_at = Instant::now() + SNAPSHOT_INTERVAL;
    let started = Instant::now();

    while !shutdown.load(Ordering::Relaxed) {
        if last_hotplug.elapsed() >= HOTPLUG_INTERVAL {
            if let Err(e) = manager.refresh() {
                let _ = log_tx.try_send(format!("openxen: device refresh failed: {e:#}"));
            }
            last_hotplug = Instant::now();
        }

        drain(&ev_rx, &mut modules, &mut boards, settings, &params, &profile, &wtn, &mut midi, &log_tx);

        if last_stats.elapsed() >= STATS_INTERVAL {
            let overflow = manager.overflow.load(Ordering::Relaxed);
            if overflow != last_overflow {
                let _ = log_tx.try_send(format!("openxen: {} frames dropped (hot loop behind)", overflow - last_overflow));
                last_overflow = overflow;
            }
            for (serial, m) in modules.iter_mut() {
                if m.lost > 0 {
                    let _ = log_tx.try_send(format!("openxen board{} ({serial}): {} scans lost of {}", m.board, m.lost, m.frames + m.lost));
                    m.lost = 0;
                    m.frames = 0;
                }
            }
            last_stats = Instant::now();
        }

        let now = Instant::now();
        if snapshot_due(now, &mut next_snapshot_at) {
            let snap = build_snapshot(edo, &midi_port, &layout_filename, &params, &profile, &modules, started.elapsed());
            let _ = snap_tx.try_send(snap);
            if let Some(h) = &hud_ctx {
                let held = modules.values().flat_map(|m| {
                    m.sounding.iter().filter_map(move |s| s.map(|(ch, note)| (m.board, ch, note)))
                });
                let present: Vec<u8> = boards.keys().copied().collect();
                let pressed = crate::wooting::hud_ctx::pressed_from_held(held, edo, wtn.pitch_offset, &present);
                let mode = crate::wooting::hud_ctx::HudWootingMode {
                    octave_shift: 0,
                    press_threshold: params.press_threshold,
                    aftertouch: params.aftertouch_mode.name().to_string(),
                    aftertouch_speed_max: params.aftertouch_speed_max,
                    velocity_profile: profile.name(),
                };
                crate::wooting::hud_ctx::submit_state(h, pressed, mode);
            }
        }
        thread::sleep(POLL_INTERVAL);
    }

    for m in modules.values_mut() {
        m.release_all(&mut midi);
    }
    midi.all_notes_off();
    drop(manager);
    drop(master);
    let _ = log_tx.try_send("xentool serve: shutdown.".to_string());
    drop(snap_tx);
    drop(log_tx);
    let _ = ui_handle.join();
    Ok(())
}

/// Process every pending event in arrival order (frames of one module stay in order).
#[allow(clippy::too_many_arguments)]
fn drain(
    rx: &Receiver<Event>,
    modules: &mut HashMap<Arc<str>, Module>,
    boards: &mut BTreeMap<u8, Arc<str>>,
    settings: &OpenxenSettings,
    params: &KeyParams,
    profile: &VelocityProfile,
    wtn: &Wtn,
    midi: &mut MidiOut,
    log_tx: &crossbeam_channel::Sender<String>,
) {
    while let Ok(ev) = rx.try_recv() {
        match ev {
            Event::Connected { serial } => {
                if modules.contains_key(&serial) {
                    continue;
                }
                match assign_board(&serial, settings, boards) {
                    Some(b) => {
                        boards.insert(b, serial.clone());
                        modules.insert(serial.clone(), Module::new(b));
                        let _ = log_tx.try_send(format!("openxen: module {serial} -> board{b}"));
                    }
                    None => {
                        let _ = log_tx.try_send(format!("openxen: module {serial} ignored (no free board)"));
                    }
                }
            }
            Event::Disconnected { serial, reason } => {
                if let Some(mut m) = modules.remove(&serial) {
                    m.release_all(midi);
                    boards.remove(&m.board);
                    let _ = log_tx.try_send(format!("openxen: board{} ({serial}) disconnected: {reason}", m.board));
                }
            }
            Event::BadReport { serial, reason } => {
                let _ = log_tx.try_send(format!("openxen: bad report from {serial}: {reason}"));
            }
            Event::Frame { serial, frame } => {
                let Some(m) = modules.get_mut(&serial) else { continue };
                m.frames += 1;
                m.lost += frame.lost_before;
                m.last_time_us = frame.time_us;
                for k in 0..(frame.num_keys as usize).min(MAX_KEYS) {
                    if let Some(a) = m.keys[k].step(frame.time_us, frame.travel[k], params, profile) {
                        apply(a, m, k, wtn, midi);
                    }
                }
            }
        }
    }
}

fn build_snapshot(
    edo: i32,
    midi_port: &str,
    layout_filename: &str,
    params: &KeyParams,
    profile: &VelocityProfile,
    modules: &HashMap<Arc<str>, Module>,
    _uptime: Duration,
) -> WootingSnapshot {
    let mut held_keys: Vec<HeldKeyDisplay> = Vec::new();
    let mut lines: Vec<DeviceLine> = Vec::new();
    for m in modules.values() {
        lines.push(DeviceLine { wtn_board: m.board, octave_hold: false });
        for (k, s) in m.sounding.iter().enumerate() {
            if let Some((ch, note)) = *s {
                let age_ms = m.keys[k].held_since_us().map(|t| (m.last_time_us.saturating_sub(t) / 1000) as u32).unwrap_or(0);
                held_keys.push(HeldKeyDisplay { wtn_board: m.board, channel: ch, note, pressure: m.keys[k].last_pressure(), age_ms });
            }
        }
    }
    held_keys.sort_by_key(|k| (k.wtn_board, k.channel, k.note));
    lines.sort_by_key(|d| d.wtn_board);
    WootingSnapshot {
        edo,
        midi_port: midi_port.to_string(),
        layout_filename: layout_filename.to_string(),
        aftertouch_mode_name: params.aftertouch_mode.name(),
        velocity_profile_name: format!("{} ({})", profile.name(), params.velocity_source.name()),
        manual_press_threshold: params.press_threshold,
        aftertouch_speed_max: params.aftertouch_speed_max,
        screensaver_active: false,
        device_count: modules.len() as u8,
        octave_holds: lines,
        held_keys,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::OpenxenBoardSettings;

    #[test]
    fn configured_serials_keep_their_board() {
        let s = OpenxenSettings { boards: vec![OpenxenBoardSettings { serial: "B".into(), board: 3 }], ..OpenxenSettings::default() };
        let mut taken = BTreeMap::new();
        assert_eq!(assign_board("B", &s, &taken), Some(3));
        // unknown modules skip the reserved board
        assert_eq!(assign_board("A", &s, &taken), Some(0));
        for b in [0u8, 1, 2, 4] {
            taken.insert(b, Arc::<str>::from("x"));
        }
        assert_eq!(assign_board("C", &s, &taken), Some(3));   // reserved board used when nothing else is free
        taken.insert(3, Arc::<str>::from("x"));
        assert_eq!(assign_board("D", &s, &taken), None);
    }

    #[test]
    fn settings_map_to_key_params() {
        let mut s = OpenxenSettings::default();
        let p = key_params(&s);
        assert_eq!(p.velocity_source, VelocitySource::Speed);
        assert_eq!(p.velocity_window_us, 3_000);
        assert_eq!(p.aftertouch_mode, AftertouchMode::Off);
        s.velocity_source = "depth".into();
        s.aftertouch_mode = "peak-mapped".into();
        let p = key_params(&s);
        assert_eq!(p.velocity_source, VelocitySource::Depth);
        assert_eq!(p.aftertouch_mode, AftertouchMode::PeakMapped);
    }
}
