//! Wooting `serve` — poll analog keys, emit MIDI, drive RGB, broadcast MTS-ESP.
//!
//! Ported feature-by-feature from `C:\Dev-Free\xenwooting\xenwooting\src\bin\xenwooting.rs`.
//! The 1 kHz main loop is time-critical: no disk I/O, no `println!`/`eprintln!`,
//! no logging inside the loop. Startup + shutdown paths may print.

use anyhow::{Context, Result};
use crossbeam_channel::{Receiver, Sender, bounded, unbounded};
use midir::{MidiOutput, MidiOutputConnection};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use crate::layouts::{self, LayoutKind};
use crate::mts::{MtsMaster, edo_freq_hz};
use crate::settings::WootingSettings;
use crate::wooting::analog::{self, DeviceId};
use crate::wooting::control_bar::{self, RgbCmd};
use crate::wooting::hidmap::{
    HidMap, KeyLoc, compute_compact_col_offsets, hid, mirror_cols_4x14, rotate_4x14,
};
use crate::wooting::modes::{AftertouchMode, VelocityProfile, default_velocity_profiles};
use crate::wooting::rgb;
use crate::wooting::ui::{
    DeviceLine, HeldKeyDisplay, SNAPSHOT_INTERVAL, WootingSnapshot, run_wooting_serve_ui,
    snapshot_due,
};
use crate::wooting::wtn::{Wtn, WtnCell, parse_wtn};

// --- Tunables ---
const POLL_INTERVAL: Duration = Duration::from_millis(1);
const HOTPLUG_INTERVAL: Duration = Duration::from_millis(500);
const AFTERTOUCH_DEBOUNCE: Duration = Duration::from_millis(160);
const RGB_FLUSH_INTERVAL: Duration = Duration::from_millis(16);
const MAX_KEYS_PER_DEVICE: usize = 256;

// --- RGB worker thread ---

fn spawn_rgb_worker(rx: Receiver<RgbCmd>) {
    thread::spawn(move || {
        let mut last_flush = Instant::now();
        let mut dirty: HashMap<u8, bool> = HashMap::new();

        loop {
            if last_flush.elapsed() >= RGB_FLUSH_INTERVAL {
                for (dev_idx, is_dirty) in dirty.iter_mut() {
                    if !*is_dirty {
                        continue;
                    }
                    let _ = rgb::with_sdk(|sdk| sdk.array_update_keyboard(*dev_idx));
                    *is_dirty = false;
                }
                last_flush = Instant::now();
            }

            match rx.recv_timeout(Duration::from_millis(5)) {
                Ok(cmd) => {
                    let _ = rgb::with_sdk(|sdk| {
                        sdk.array_set_single(cmd.device_index, cmd.row, cmd.col, cmd.rgb)
                    });
                    *dirty.entry(cmd.device_index).or_insert(false) = true;
                }
                Err(crossbeam_channel::RecvTimeoutError::Timeout) => continue,
                Err(_) => return,
            }
        }
    });
}

fn try_send_drop(tx: &Sender<RgbCmd>, cmd: RgbCmd) {
    let _ = tx.try_send(cmd);
}

// --- Per-key state ---

#[derive(Debug, Clone, Copy)]
enum KeyState {
    Idle,
    Tracking {
        started: Instant,
        peak: f32,
        last_analog: f32,
        last_analog_ts: Instant,
        peak_speed: f32,
        out_ch: u8,
        note: u8,
        led_row: u8,
        led_col: u8,
        wtn_color: (u8, u8, u8),
    },
    Held {
        out_ch: u8,
        note: u8,
        led_row: u8,
        led_col: u8,
        wtn_color: (u8, u8, u8),
        last_analog: f32,
        last_analog_ts: Instant,
        peak: f32,
        peak_speed: f32,
        at_level: f32,
        last_pressure_sent: u8,
        held_since: Instant,
    },
}

#[derive(Debug, Default)]
struct DeviceState {
    keys: HashMap<u16 /* HID */, KeyState>,
}

// --- MIDI output ---

struct MidiOut {
    conn: MidiOutputConnection,
}

impl MidiOut {
    fn open(name: &str) -> Result<Self> {
        let out = MidiOutput::new("xentool-wooting")?;
        let port = out
            .ports()
            .into_iter()
            .find(|p| out.port_name(p).ok().as_deref() == Some(name))
            .with_context(|| format!("MIDI output port `{name}` not found"))?;
        let conn = out
            .connect(&port, "xentool-wooting-out")
            .with_context(|| format!("failed to open MIDI output `{name}`"))?;
        Ok(Self { conn })
    }
    fn note_on(&mut self, ch: u8, note: u8, vel: u8) -> Result<()> {
        self.conn.send(&[0x90 | (ch & 0x0F), note & 0x7F, vel & 0x7F])?;
        Ok(())
    }
    fn note_off(&mut self, ch: u8, note: u8) -> Result<()> {
        self.conn.send(&[0x80 | (ch & 0x0F), note & 0x7F, 0])?;
        Ok(())
    }
    fn poly_pressure(&mut self, ch: u8, note: u8, pressure: u8) -> Result<()> {
        self.conn.send(&[0xA0 | (ch & 0x0F), note & 0x7F, pressure & 0x7F])?;
        Ok(())
    }
    fn pitch_bend(&mut self, ch: u8, bend: i32) -> Result<()> {
        // `bend` is -8192..8191; MIDI encodes 0..16383 with center = 8192.
        let v = (bend + 8192).clamp(0, 16383) as u16;
        let lsb = (v & 0x7F) as u8;
        let msb = ((v >> 7) & 0x7F) as u8;
        self.conn.send(&[0xE0 | (ch & 0x0F), lsb, msb])?;
        Ok(())
    }
    fn cc(&mut self, ch: u8, cc: u8, value: u8) -> Result<()> {
        self.conn.send(&[0xB0 | (ch & 0x0F), cc & 0x7F, value & 0x7F])?;
        Ok(())
    }
    fn all_notes_off(&mut self) {
        for ch in 0u8..16 {
            let _ = self.conn.send(&[0xB0 | ch, 123, 0]);
        }
    }
}

// --- Helpers ---

/// xenwooting.rs 1068–1075 — combine up/down amounts into a signed 14-bit bend.
fn bend_from_amounts(up_amt: f32, down_amt: f32) -> i32 {
    let x = (up_amt - down_amt).clamp(-1.0, 1.0);
    if x >= 0.0 {
        (x * 8191.0).round().clamp(0.0, 8191.0) as i32
    } else {
        (x * 8192.0).round().clamp(-8192.0, 0.0) as i32
    }
}

/// xenwooting.rs 4418–4432 — set of MIDI channels used by this board's cells,
/// shifted by the persistent `octave_shift` and the per-device `octave_hold`.
fn used_channels(wtn: &Wtn, board: u8, octave_shift: i16, octave_hold: bool) -> Vec<u8> {
    let hold = if octave_hold { 1 } else { 0 };
    let mut set: HashSet<u8> = HashSet::new();
    for idx in 0..56usize {
        if let Some(c) = wtn.cell(board, idx) {
            if c.chan == 0 {
                continue;
            }
            let base = c.chan.saturating_sub(1) as i16;
            let shifted = (base + octave_shift + hold).clamp(0, 15) as u8;
            set.insert(shifted);
        }
    }
    let mut v: Vec<u8> = set.into_iter().collect();
    v.sort();
    v
}

fn build_tuning_table(wtn: &Wtn, edo: i32, pitch_offset: i32) -> [f64; 128] {
    let mut freqs = [0.0f64; 128];
    for n in 0..128 {
        freqs[n] = edo_freq_hz(12, n as i32);
    }
    if let Some(cells) = wtn.boards.get(&0) {
        for c in cells.iter() {
            if c.chan == 0 {
                continue;
            }
            let virtual_pitch = (c.chan as i32 - 1) * edo + c.key as i32 + pitch_offset + 2 * edo;
            if (c.key as usize) < 128 {
                freqs[c.key as usize] = edo_freq_hz(edo, virtual_pitch);
            }
        }
    }
    freqs
}

fn resolve_cell(
    hid: u16,
    map: &HidMap,
    compact: &[u8; 4],
    rotation_deg: u16,
    wtn: &Wtn,
    wtn_board: u8,
) -> Option<(KeyLoc, WtnCell)> {
    let loc0 = map.loc_for(hid)?;
    // The physical `loc0` drives the LED (unrotated coords). For the WTN
    // cell lookup we rotate + mirror into the logical grid so rotated boards
    // map to the correct cells. Uses the rotation-aware `compact` table.
    let idx = crate::wooting::hidmap::wtn_index_for_loc(loc0, rotation_deg, compact)?;
    let cell = wtn.cell(wtn_board, idx)?;
    if cell.chan == 0 {
        return None;
    }
    Some((loc0, cell))
}

// --- Musical key state machine ---

#[allow(clippy::too_many_arguments)]
fn step_musical_key(
    state: &mut DeviceState,
    hid: u16,
    analog_val: f32,
    rgb_idx: u8,
    wtn_board: u8,
    wtn: &Wtn,
    map: &HidMap,
    compact: &[u8; 4],
    rotation_deg: u16,
    midi: &mut MidiOut,
    rgb_tx: &Sender<RgbCmd>,
    aftertouch_mode: AftertouchMode,
    velocity_profile: &VelocityProfile,
    press_threshold: f32,
    release_delta: f32,
    aftertouch_speed_max: f32,
    aftertouch_delta: f32,
    octave_shift: i16,
    octave_hold: bool,
) {
    let now = Instant::now();
    let prev = state.keys.entry(hid).or_insert(KeyState::Idle);
    match *prev {
        KeyState::Idle => {
            if analog_val >= press_threshold {
                if let Some((loc, cell)) = resolve_cell(hid, map, compact, rotation_deg, wtn, wtn_board) {
                    let base = cell.chan.saturating_sub(1) as i16;
                    let hold = if octave_hold { 1 } else { 0 };
                    let out_ch = (base + octave_shift + hold).clamp(0, 15) as u8;
                    *prev = KeyState::Tracking {
                        started: now,
                        peak: analog_val,
                        last_analog: analog_val,
                        last_analog_ts: now,
                        peak_speed: 0.0,
                        out_ch,
                        note: cell.key,
                        led_row: loc.led_row,
                        led_col: loc.led_col,
                        wtn_color: cell.color,
                    };
                    // Flash white on press.
                    try_send_drop(
                        rgb_tx,
                        RgbCmd {
                            device_index: rgb_idx,
                            row: loc.led_row,
                            col: loc.led_col,
                            rgb: control_bar::HIGHLIGHT_RGB,
                        },
                    );
                }
            }
        }
        KeyState::Tracking {
            ref mut peak,
            ref mut last_analog,
            ref mut last_analog_ts,
            ref mut peak_speed,
            ..
        } => {
            if analog_val > *peak {
                *peak = analog_val;
            }
            let dt = now.duration_since(*last_analog_ts).as_secs_f32();
            if dt > 0.0 {
                let s = (analog_val - *last_analog) / dt;
                if s.is_finite() && s > *peak_speed {
                    *peak_speed = s.max(0.0);
                }
            }
            *last_analog = analog_val;
            *last_analog_ts = now;
            // Abandoned press before note_on emission.
            if analog_val < press_threshold - release_delta {
                *prev = KeyState::Idle;
            }
        }
        KeyState::Held {
            out_ch,
            note,
            led_row,
            led_col,
            wtn_color,
            ref mut last_analog,
            ref mut last_analog_ts,
            ref mut peak,
            ref mut peak_speed,
            ref mut at_level,
            ref mut last_pressure_sent,
            held_since: _,
        } => {
            if analog_val > *peak {
                *peak = analog_val;
            }
            let dt = now.duration_since(*last_analog_ts).as_secs_f32();
            if dt > 0.0 {
                let s = (analog_val - *last_analog) / dt;
                if s.is_finite() && s > *peak_speed {
                    *peak_speed = s.max(0.0);
                }
            }
            *last_analog = analog_val;
            *last_analog_ts = now;

            // Release check (rapid-trigger style): drop below peak by `release_delta`.
            if analog_val + release_delta < *peak {
                let _ = midi.note_off(out_ch, note);
                try_send_drop(
                    rgb_tx,
                    RgbCmd {
                        device_index: rgb_idx,
                        row: led_row,
                        col: led_col,
                        rgb: wtn_color,
                    },
                );
                *prev = KeyState::Idle;
                return;
            }

            // Aftertouch emission.
            match aftertouch_mode {
                AftertouchMode::Off => {}
                AftertouchMode::SpeedMapped | AftertouchMode::PeakMapped => {
                    let raw: f32 = match aftertouch_mode {
                        AftertouchMode::SpeedMapped => {
                            (*peak_speed / aftertouch_speed_max.max(0.001)).clamp(0.0, 1.0)
                        }
                        AftertouchMode::PeakMapped => {
                            let denom = (1.0 - press_threshold).max(0.001);
                            ((*peak - press_threshold) / denom).clamp(0.0, 1.0)
                        }
                        AftertouchMode::Off => 0.0,
                    };
                    let shaped = velocity_profile.apply(raw);
                    if shaped > *at_level {
                        *at_level = shaped;
                    }
                    let at = at_level.clamp(0.0, 1.0);
                    let p = (at * 127.0).round().clamp(0.0, 127.0) as u8;
                    // Only emit when the MIDI pressure changes by at least delta*127.
                    if (p as i32 - *last_pressure_sent as i32).unsigned_abs()
                        >= ((aftertouch_delta * 127.0).round() as u32).max(1)
                        || (p != *last_pressure_sent && p == 127)
                        || (p != *last_pressure_sent && p == 0)
                    {
                        let _ = midi.poly_pressure(out_ch, note, p);
                        *last_pressure_sent = p;
                    }
                }
            }
        }
    }
}

fn emit_peak_note_ons(
    state: &mut DeviceState,
    midi: &mut MidiOut,
    velocity_profile: &VelocityProfile,
    velocity_peak_track_ms: u32,
) {
    let now = Instant::now();
    let window = Duration::from_millis(velocity_peak_track_ms as u64);
    let mut to_upgrade: Vec<(u16, u8, u8, u8, u8, u8, (u8, u8, u8), f32, Instant, f32, f32)> =
        Vec::new();
    for (&hid, st) in state.keys.iter_mut() {
        if let KeyState::Tracking {
            started,
            peak,
            out_ch,
            note,
            led_row,
            led_col,
            wtn_color,
            last_analog,
            last_analog_ts,
            peak_speed,
        } = *st
        {
            if now.duration_since(started) >= window {
                let shaped = velocity_profile.apply(peak.clamp(0.0, 1.0));
                let velocity = (shaped * 126.0).round().clamp(0.0, 126.0) as u8 + 1;
                to_upgrade.push((
                    hid, out_ch, note, velocity, led_row, led_col, wtn_color, last_analog,
                    last_analog_ts.min(now), peak_speed, peak,
                ));
            }
        }
    }
    for (hid, ch, note, vel, led_row, led_col, wtn_color, last_analog, last_analog_ts, peak_speed, peak) in
        to_upgrade
    {
        let _ = midi.note_on(ch, note, vel);
        state.keys.insert(
            hid,
            KeyState::Held {
                out_ch: ch,
                note,
                led_row,
                led_col,
                wtn_color,
                last_analog,
                last_analog_ts,
                peak,
                peak_speed,
                at_level: 0.0,
                last_pressure_sent: 0,
                held_since: Instant::now(),
            },
        );
    }
}

// --- Initial LED paint ---

fn paint_initial_leds(
    wtn: &Wtn,
    map: &HidMap,
    compact_upright: &[u8; 4],
    compact_rotated: &[u8; 4],
    rgb_tx: &Sender<RgbCmd>,
    settings: &WootingSettings,
    aftertouch_mode: AftertouchMode,
    octave_hold_by_device: &HashSet<DeviceId>,
    board_rgb_pairs: &[(u8 /* wtn_board */, u8 /* rgb_idx */)],
) {
    let total = board_rgb_pairs.len() as u8;
    for &(wtn_board, rgb_idx) in board_rgb_pairs {
        let rotation_deg: u16 = if crate::wooting::geometry::rotated(wtn_board, total) {
            180
        } else {
            0
        };
        let compact = if rotation_deg == 180 { compact_rotated } else { compact_upright };
        for (_, loc) in map.all_locs() {
            let Some(idx) = crate::wooting::hidmap::wtn_index_for_loc(loc, rotation_deg, compact)
            else {
                continue;
            };
            let Some(cell) = wtn.cell(wtn_board, idx) else { continue };
            let color = if cell.chan == 0 { (0, 0, 0) } else { cell.color };
            try_send_drop(
                rgb_tx,
                RgbCmd {
                    // Paint goes to the RGB SDK index, which may differ from
                    // the analog-SDK enumeration index.
                    device_index: rgb_idx,
                    row: loc.led_row,
                    col: loc.led_col,
                    rgb: color,
                },
            );
        }
        // Control bar (full row) in BASE; overlays handled inside paint_restore.
        let _ = octave_hold_by_device; // reserved for future per-device lookup
        control_bar::paint_restore(
            rgb_tx,
            &settings.control_bar,
            rgb_idx,
            None,
            aftertouch_mode,
            false,
        );
    }
}

/// Resolve the RGB SDK index for a given `wtn_board`, honoring an explicit
/// `rgb_device_index` override in `BoardSettings` and falling back to
/// `wtn_board` itself.
fn rgb_index_for_board(settings: &WootingSettings, wtn_board: u8) -> u8 {
    settings
        .boards
        .iter()
        .find(|b| b.wtn_board == wtn_board)
        .and_then(|b| b.rgb_device_index)
        .unwrap_or(wtn_board)
}

fn refresh_devices(out: &mut HashMap<DeviceId, u8>) -> Result<()> {
    let devs = analog::with_sdk(|sdk| sdk.connected_devices(32))?;
    out.clear();
    for (i, d) in devs.iter().enumerate() {
        out.insert(d.id, i as u8);
    }
    Ok(())
}

// --- Control-bar action dispatch ---

#[allow(clippy::too_many_arguments)]
fn emit_pitchbend_for_board(
    wtn: &Wtn,
    board: u8,
    device_id: DeviceId,
    bend: i32,
    octave_shift: i16,
    octave_hold: bool,
    last_pb_by_dev_ch: &mut HashMap<(DeviceId, u8), i32>,
    midi: &mut MidiOut,
) {
    for ch in used_channels(wtn, board, octave_shift, octave_hold) {
        let k = (device_id, ch);
        let last = last_pb_by_dev_ch.get(&k).copied().unwrap_or(i32::MIN);
        if last != bend {
            let _ = midi.pitch_bend(ch, bend);
            last_pb_by_dev_ch.insert(k, bend);
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn emit_cc_for_board(
    wtn: &Wtn,
    board: u8,
    device_id: DeviceId,
    cc_num: u8,
    value: u8,
    octave_shift: i16,
    octave_hold: bool,
    last_cc_by_dev_ch: &mut HashMap<(DeviceId, u8, u8), u8>,
    midi: &mut MidiOut,
) {
    for ch in used_channels(wtn, board, octave_shift, octave_hold) {
        let k = (device_id, cc_num, ch);
        let last = last_cc_by_dev_ch.get(&k).copied().unwrap_or(u8::MAX);
        if last != value {
            let _ = midi.cc(ch, cc_num, value);
            last_cc_by_dev_ch.insert(k, value);
        }
    }
}

// --- Main entry point ---

#[allow(clippy::too_many_arguments)]
pub fn cmd_serve_wtn(
    file: PathBuf,
    midi_port: String,
    hud: bool,
    hud_port: u16,
    xenharm_url: String,
    osc_port: u16,
    tune_supercollider: bool,
    settings: &WootingSettings,
) -> Result<()> {
    let content = std::fs::read_to_string(&file)
        .with_context(|| format!("reading {}", file.display()))?;
    let mut wtn = parse_wtn(&content)?;
    let mut active_wtn_path: PathBuf = file.clone();

    // HUD publisher: always created so the snapshot site can call into it
    // unconditionally; the HTTP server only starts when --hud is set, and
    // we only build the ctx in that case so the hot-loop fast-path skips
    // the LiveState build entirely otherwise.
    let hud_publisher = crate::hud::HudPublisher::new(crate::hud::LiveState::empty("wooting"));
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
        if let Err(e) = crate::hud::osc::spawn_tuning_broadcaster(
            hud_publisher.clone(),
            crate::hud::osc::DEFAULT_SC_TUNING_TARGET.into(),
        ) {
            eprintln!("xentool: tuning broadcast disabled ({e:#})");
        }
    }
    let edo = wtn.edo.with_context(|| {
        format!(
            "Edo= not set in {}; add e.g. `Edo=31` before the first [Board] section.",
            file.display()
        )
    })?;
    let map = HidMap::default_60he_ansi_guess();
    // Two compact-col tables: one for the unrotated boards, one for boards
    // rotated 180°. The correct table is picked per-device at event time.
    let compact_upright = compute_compact_col_offsets(&map, 0);
    let compact_rotated = compute_compact_col_offsets(&map, 180);

    // MTS-ESP master — one global 128-note table from Board0.
    let master = MtsMaster::register().context("registering MTS-ESP master")?;
    master.set_scale_name(&format!("{edo}-EDO"))?;
    let freqs = build_tuning_table(&wtn, edo, wtn.pitch_offset);
    master.set_note_tunings(&freqs)?;

    // MIDI output.
    let mut midi = MidiOut::open(&midi_port)?;

    // RGB worker + lazy-init Analog SDK.
    let (rgb_tx, rgb_rx) = unbounded::<RgbCmd>();
    spawn_rgb_worker(rgb_rx);
    analog::with_sdk(|_| Ok(()))?;

    // Shutdown flag — set by Ctrl-C and by the TUI on `q`.
    let shutdown = Arc::new(AtomicBool::new(false));
    let shutdown_ctrlc = shutdown.clone();
    ctrlc::set_handler(move || {
        shutdown_ctrlc.store(true, Ordering::Relaxed);
    })
    .context("failed to set Ctrl+C handler")?;

    // --- Mutable state (the "live" side of the loop) ---
    let mut aftertouch_mode = AftertouchMode::Off;
    let mut velocity_profile_idx: usize = 0;
    let velocity_profiles = default_velocity_profiles();

    let mut manual_press_threshold: f32 = settings.press_threshold;
    let mut aftertouch_speed_max: f32 = settings.aftertouch_speed_max;

    let mut octave_hold_by_device: HashSet<DeviceId> = HashSet::new();
    let mut bend_up_amt_by_device: HashMap<DeviceId, f32> = HashMap::new();
    let mut bend_down_amt_by_device: HashMap<DeviceId, f32> = HashMap::new();
    let mut last_pb_by_dev_ch: HashMap<(DeviceId, u8), i32> = HashMap::new();
    let mut last_cc_by_dev_ch: HashMap<(DeviceId, u8, u8), u8> = HashMap::new();
    let mut last_aftertouch_toggle_at: HashMap<DeviceId, Instant> = HashMap::new();

    let screensaver_timeout_sec = settings.rgb.screensaver_timeout_sec as u64;
    let mut last_activity = Instant::now();
    let mut screensaver_active = false;
    let mut pressed_keys: HashSet<(DeviceId, u16)> = HashSet::new();
    let mut suppressed_keys: HashSet<(DeviceId, u16)> = HashSet::new();

    let octave_shift: i16 = 0; // no default key binding; reserved

    let mut device_index_by_id: HashMap<DeviceId, u8> = HashMap::new();
    let mut last_hotplug = Instant::now();
    refresh_devices(&mut device_index_by_id)?;

    // TUI plumbing. Snapshot channel is bounded(2) + try_send so the hot
    // loop never blocks; the receiver always reads the freshest one. Log
    // channel is bounded(64) and only written to on state transitions
    // (layout cycle, aftertouch toggle, screensaver), never per poll.
    let (snap_tx, snap_rx) = bounded::<WootingSnapshot>(2);
    let (log_tx, log_rx) = bounded::<String>(64);
    let _ = log_tx.try_send(format!(
        "xentool serve: {edo}-EDO | MIDI → `{midi_port}` | MTS-ESP master registered"
    ));
    let _ = log_tx.try_send(
        "xentool serve: polling Wooting(s) at ~1 kHz (Ctrl+C / q to stop)".to_string(),
    );
    let shutdown_ui = shutdown.clone();
    let hud_url_for_ui = hud_url.clone();
    let ui_handle = thread::spawn(move || {
        if let Err(e) = run_wooting_serve_ui(snap_rx, log_rx, shutdown_ui, hud_url_for_ui) {
            eprintln!("xentool serve TUI error: {e}");
        }
    });
    let mut next_snapshot_at = Instant::now() + SNAPSHOT_INTERVAL;
    let mut active_layout_filename: String = active_wtn_path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();

    // HUD ctx: built whenever the publisher needs to see real layout
    // state — that's `--hud` (HTTP/SSE) or `--tune-supercollider` (the
    // OSC tuning broadcaster reads layout.edo / pitch_offset from the
    // same publisher). None disables the hot-loop submit fast path.
    // Mutated on layout cycle (Context Menu key).
    let hud_ctx: Option<crate::wooting::hud_ctx::HudWootingHandle> = if hud || tune_supercollider {
        let layout_id = crate::hud::layout_id_from_path(&active_wtn_path);
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

    let initial_pairs: Vec<(u8, u8)> = device_index_by_id
        .values()
        .copied()
        .map(|wtn_board| (wtn_board, rgb_index_for_board(settings, wtn_board)))
        .collect();
    paint_initial_leds(
        &wtn,
        &map,
        &compact_upright,
        &compact_rotated,
        &rgb_tx,
        settings,
        aftertouch_mode,
        &octave_hold_by_device,
        &initial_pairs,
    );

    let mut states: HashMap<DeviceId, DeviceState> = HashMap::new();

    loop {
        if shutdown.load(Ordering::Relaxed) {
            break;
        }
        if last_hotplug.elapsed() >= HOTPLUG_INTERVAL {
            let _ = refresh_devices(&mut device_index_by_id);
            last_hotplug = Instant::now();
        }

        let total_boards = device_index_by_id.len() as u8;
        for (&device_id, &dev_idx) in device_index_by_id.iter() {
            let wtn_board = dev_idx;
            // Paint target for RGB output may differ from analog enumeration
            // (configurable via BoardSettings.rgb_device_index).
            let rgb_idx = rgb_index_for_board(settings, wtn_board);
            // Paired-pair rotation rule: even-indexed boards with a partner
            // render rotated 180°, so their WTN cell lookup needs to rotate too.
            let rotation_deg: u16 = if crate::wooting::geometry::rotated(wtn_board, total_boards) {
                180
            } else {
                0
            };
            let compact: &[u8; 4] = if rotation_deg == 180 { &compact_rotated } else { &compact_upright };
            let Ok(buf) =
                analog::with_sdk(|sdk| sdk.read_full_buffer(device_id, MAX_KEYS_PER_DEVICE))
            else {
                continue;
            };
            let state = states.entry(device_id).or_default();

            let mut seen: HashMap<u16, f32> = HashMap::with_capacity(buf.len());
            for (h, a) in buf {
                seen.insert(h, a);
            }

            // --- Release events (in state or in pressed_keys but not in seen) ---
            let previously_pressed_here: Vec<u16> = pressed_keys
                .iter()
                .filter_map(|(d, h)| (*d == device_id).then_some(*h))
                .collect();
            for hid_code in previously_pressed_here {
                if seen.contains_key(&hid_code) {
                    continue;
                }
                pressed_keys.remove(&(device_id, hid_code));
                let was_suppressed = suppressed_keys.remove(&(device_id, hid_code));

                // Control-bar key-up: restore LEDs + zero continuous controllers.
                if control_bar::is_control_bar(&settings.control_bar, hid_code) {
                    if was_suppressed {
                        // Wake press: still run restore so the bar visual is correct.
                        control_bar::paint_restore(
                            &rgb_tx,
                            &settings.control_bar,
                            rgb_idx,
                            None,
                            aftertouch_mode,
                            octave_hold_by_device.contains(&device_id),
                        );
                        continue;
                    }
                    match hid_code {
                        hid::LEFT_CONTROL => {
                            bend_up_amt_by_device.remove(&device_id);
                            let bend = bend_from_amounts(
                                0.0,
                                bend_down_amt_by_device.get(&device_id).copied().unwrap_or(0.0),
                            );
                            emit_pitchbend_for_board(
                                &wtn, wtn_board, device_id, bend, octave_shift,
                                octave_hold_by_device.contains(&device_id),
                                &mut last_pb_by_dev_ch, &mut midi,
                            );
                        }
                        hid::LEFT_ALT => {
                            bend_down_amt_by_device.remove(&device_id);
                            let bend = bend_from_amounts(
                                bend_up_amt_by_device.get(&device_id).copied().unwrap_or(0.0),
                                0.0,
                            );
                            emit_pitchbend_for_board(
                                &wtn, wtn_board, device_id, bend, octave_shift,
                                octave_hold_by_device.contains(&device_id),
                                &mut last_pb_by_dev_ch, &mut midi,
                            );
                        }
                        hid::LEFT_META => {
                            // Emit CC=0 on release.
                            let board_cfg = settings
                                .boards
                                .iter()
                                .find(|b| b.wtn_board == wtn_board)
                                .cloned()
                                .unwrap_or_default();
                            if let Some((_, cc_num)) = board_cfg.cc_analog() {
                                emit_cc_for_board(
                                    &wtn, wtn_board, device_id, cc_num, 0,
                                    octave_shift,
                                    octave_hold_by_device.contains(&device_id),
                                    &mut last_cc_by_dev_ch, &mut midi,
                                );
                            }
                        }
                        _ => {}
                    }
                    control_bar::paint_restore(
                        &rgb_tx,
                        &settings.control_bar,
                        rgb_idx,
                        Some(hid_code),
                        aftertouch_mode,
                        octave_hold_by_device.contains(&device_id),
                    );
                    continue;
                }

                // Musical key-up.
                if let Some(st) = state.keys.remove(&hid_code) {
                    if let KeyState::Held {
                        out_ch, note, led_row, led_col, wtn_color, ..
                    } = st
                    {
                        let _ = midi.note_off(out_ch, note);
                        try_send_drop(
                            &rgb_tx,
                            RgbCmd {
                                device_index: rgb_idx,
                                row: led_row,
                                col: led_col,
                                rgb: wtn_color,
                            },
                        );
                    }
                }
            }

            // --- Press/update events ---
            for (&hid_code, &analog_val) in seen.iter() {
                let was_pressed = pressed_keys.contains(&(device_id, hid_code));
                last_activity = Instant::now();

                // Screensaver wake: first-press after blanking is consumed.
                if !was_pressed && screensaver_active {
                    screensaver_active = false;
                    let _ = log_tx.try_send("screensaver: wake".to_string());
                    suppressed_keys.insert((device_id, hid_code));
                    pressed_keys.insert((device_id, hid_code));
                    let pairs: Vec<(u8, u8)> = device_index_by_id
                        .values()
                        .copied()
                        .map(|b| (b, rgb_index_for_board(settings, b)))
                        .collect();
                    paint_initial_leds(
                        &wtn, &map, &compact_upright, &compact_rotated, &rgb_tx, settings,
                        aftertouch_mode, &octave_hold_by_device, &pairs,
                    );
                    continue;
                }

                if !was_pressed {
                    pressed_keys.insert((device_id, hid_code));
                }

                if suppressed_keys.contains(&(device_id, hid_code)) {
                    continue;
                }

                // Control-bar key handling.
                if control_bar::is_control_bar(&settings.control_bar, hid_code) {
                    if !was_pressed {
                        // Action on key-down.
                        match hid_code {
                            hid::SPACE => {
                                let now_held = if octave_hold_by_device.contains(&device_id) {
                                    octave_hold_by_device.remove(&device_id);
                                    false
                                } else {
                                    octave_hold_by_device.insert(device_id);
                                    true
                                };
                                let board = device_index_by_id
                                    .get(&device_id)
                                    .copied()
                                    .unwrap_or(0);
                                let _ = log_tx.try_send(format!(
                                    "octave hold board{board}: {}",
                                    if now_held { "ON" } else { "OFF" }
                                ));
                            }
                            hid::RIGHT_ALT => {
                                let now = Instant::now();
                                let skip = last_aftertouch_toggle_at
                                    .get(&device_id)
                                    .is_some_and(|t| now.duration_since(*t) < AFTERTOUCH_DEBOUNCE);
                                if !skip {
                                    last_aftertouch_toggle_at.insert(device_id, now);
                                    aftertouch_mode = aftertouch_mode.next();
                                    manual_press_threshold = match aftertouch_mode {
                                        AftertouchMode::Off => settings.press_threshold,
                                        _ => settings.aftertouch_press_threshold,
                                    };
                                    let _ = log_tx.try_send(format!(
                                        "aftertouch: {}",
                                        aftertouch_mode.name()
                                    ));
                                }
                            }
                            hid::ARROW_LEFT => {
                                if aftertouch_mode == AftertouchMode::Off {
                                    manual_press_threshold = (manual_press_threshold
                                        - settings.press_threshold_step)
                                        .clamp(0.02, 0.98);
                                } else {
                                    aftertouch_speed_max = (aftertouch_speed_max
                                        - settings.aftertouch_speed_step)
                                        .clamp(1.0, 1000.0);
                                }
                            }
                            hid::ARROW_RIGHT => {
                                if aftertouch_mode == AftertouchMode::Off {
                                    manual_press_threshold = (manual_press_threshold
                                        + settings.press_threshold_step)
                                        .clamp(0.02, 0.98);
                                } else {
                                    aftertouch_speed_max = (aftertouch_speed_max
                                        + settings.aftertouch_speed_step)
                                        .clamp(1.0, 1000.0);
                                }
                            }
                            hid::ARROW_DOWN => {
                                velocity_profile_idx =
                                    (velocity_profile_idx + 1) % velocity_profiles.len();
                            }
                            hid::LEFT_CONTROL => {
                                bend_up_amt_by_device.insert(device_id, analog_val.clamp(0.0, 1.0));
                                let bend = bend_from_amounts(
                                    analog_val.clamp(0.0, 1.0),
                                    bend_down_amt_by_device
                                        .get(&device_id)
                                        .copied()
                                        .unwrap_or(0.0),
                                );
                                emit_pitchbend_for_board(
                                    &wtn, wtn_board, device_id, bend, octave_shift,
                                    octave_hold_by_device.contains(&device_id),
                                    &mut last_pb_by_dev_ch, &mut midi,
                                );
                            }
                            hid::LEFT_ALT => {
                                bend_down_amt_by_device
                                    .insert(device_id, analog_val.clamp(0.0, 1.0));
                                let bend = bend_from_amounts(
                                    bend_up_amt_by_device.get(&device_id).copied().unwrap_or(0.0),
                                    analog_val.clamp(0.0, 1.0),
                                );
                                emit_pitchbend_for_board(
                                    &wtn, wtn_board, device_id, bend, octave_shift,
                                    octave_hold_by_device.contains(&device_id),
                                    &mut last_pb_by_dev_ch, &mut midi,
                                );
                            }
                            hid::LEFT_META => {
                                let board_cfg = settings
                                    .boards
                                    .iter()
                                    .find(|b| b.wtn_board == wtn_board)
                                    .cloned()
                                    .unwrap_or_default();
                                if let Some((_, cc_num)) = board_cfg.cc_analog() {
                                    let v = (analog_val.clamp(0.0, 1.0) * 127.0).round() as u8;
                                    emit_cc_for_board(
                                        &wtn, wtn_board, device_id, cc_num, v,
                                        octave_shift,
                                        octave_hold_by_device.contains(&device_id),
                                        &mut last_cc_by_dev_ch, &mut midi,
                                    );
                                }
                            }
                            hid::CONTEXT_MENU => {
                                // Layout cycle. Pressing ContextMenu advances
                                // to the next `.wtn` file in ./wtn/ with wrap.
                                if let Ok(new_path) =
                                    layouts::next(LayoutKind::Wtn, &active_wtn_path)
                                {
                                    if let Ok(content) = std::fs::read_to_string(&new_path)
                                    {
                                        if let Ok(new_wtn) = parse_wtn(&content) {
                                            // Release all held notes for the active
                                            // device. Other devices' notes are
                                            // covered by the `all_notes_off()` CC 123
                                            // broadcast on every channel (below).
                                            for (_, ks) in state.keys.iter_mut() {
                                                if let KeyState::Held { out_ch, note, .. } =
                                                    *ks
                                                {
                                                    let _ = midi.note_off(out_ch, note);
                                                }
                                                *ks = KeyState::Idle;
                                            }
                                            midi.all_notes_off();

                                            // Rebuild MTS table.
                                            if let Some(new_edo) = new_wtn.edo {
                                                let freqs = build_tuning_table(
                                                    &new_wtn,
                                                    new_edo,
                                                    new_wtn.pitch_offset,
                                                );
                                                let _ = master.set_note_tunings(&freqs);
                                                let _ = master.set_scale_name(&format!(
                                                    "{new_edo}-EDO"
                                                ));
                                            }

                                            // Swap active layout.
                                            wtn = new_wtn;
                                            active_wtn_path = new_path.clone();
                                            active_layout_filename = active_wtn_path
                                                .file_name()
                                                .map(|n| n.to_string_lossy().into_owned())
                                                .unwrap_or_default();
                                            let _ = log_tx.try_send(format!(
                                                "layout: {}",
                                                active_layout_filename
                                            ));

                                            // Refresh HUD ctx so the next
                                            // snapshot reflects the new layout.
                                            if let Some(h) = &hud_ctx {
                                                let mut ctx = h.borrow_mut();
                                                ctx.layout_id =
                                                    crate::hud::layout_id_from_path(&new_path);
                                                ctx.layout_name = ctx.layout_id.clone();
                                                if let Some(new_edo) = wtn.edo {
                                                    ctx.edo = new_edo;
                                                }
                                                ctx.pitch_offset = wtn.pitch_offset;
                                                ctx.layout_pitches =
                                                    crate::wooting::hud_ctx::build_layout_pitches(
                                                        &wtn,
                                                    );
                                            }

                                            // Repaint LEDs.
                                            let pairs: Vec<(u8, u8)> = device_index_by_id
                                                .values()
                                                .copied()
                                                .map(|b| {
                                                    (b, rgb_index_for_board(settings, b))
                                                })
                                                .collect();
                                            paint_initial_leds(
                                                &wtn, &map, &compact_upright, &compact_rotated,
                                                &rgb_tx, settings, aftertouch_mode,
                                                &octave_hold_by_device, &pairs,
                                            );

                                            // Persist on a background thread so the
                                            // hot loop never blocks on disk I/O.
                                            let persist_path = new_path.clone();
                                            std::thread::spawn(move || {
                                                crate::settings::store_last_layout(
                                                    LayoutKind::Wtn,
                                                    &persist_path,
                                                );
                                            });
                                        }
                                    }
                                }
                            }
                            _ => {}
                        }
                        // Flash paint.
                        control_bar::paint_flash_on_down(
                            &rgb_tx,
                            &settings.control_bar,
                            rgb_idx,
                            hid_code,
                        );
                    } else {
                        // Held: continuous updates for pitchbend / CC keys.
                        match hid_code {
                            hid::LEFT_CONTROL => {
                                bend_up_amt_by_device
                                    .insert(device_id, analog_val.clamp(0.0, 1.0));
                                let bend = bend_from_amounts(
                                    analog_val.clamp(0.0, 1.0),
                                    bend_down_amt_by_device
                                        .get(&device_id)
                                        .copied()
                                        .unwrap_or(0.0),
                                );
                                emit_pitchbend_for_board(
                                    &wtn, wtn_board, device_id, bend, octave_shift,
                                    octave_hold_by_device.contains(&device_id),
                                    &mut last_pb_by_dev_ch, &mut midi,
                                );
                            }
                            hid::LEFT_ALT => {
                                bend_down_amt_by_device
                                    .insert(device_id, analog_val.clamp(0.0, 1.0));
                                let bend = bend_from_amounts(
                                    bend_up_amt_by_device.get(&device_id).copied().unwrap_or(0.0),
                                    analog_val.clamp(0.0, 1.0),
                                );
                                emit_pitchbend_for_board(
                                    &wtn, wtn_board, device_id, bend, octave_shift,
                                    octave_hold_by_device.contains(&device_id),
                                    &mut last_pb_by_dev_ch, &mut midi,
                                );
                            }
                            hid::LEFT_META => {
                                let board_cfg = settings
                                    .boards
                                    .iter()
                                    .find(|b| b.wtn_board == wtn_board)
                                    .cloned()
                                    .unwrap_or_default();
                                if let Some((_, cc_num)) = board_cfg.cc_analog() {
                                    let v = (analog_val.clamp(0.0, 1.0) * 127.0).round() as u8;
                                    emit_cc_for_board(
                                        &wtn, wtn_board, device_id, cc_num, v,
                                        octave_shift,
                                        octave_hold_by_device.contains(&device_id),
                                        &mut last_cc_by_dev_ch, &mut midi,
                                    );
                                }
                            }
                            _ => {}
                        }
                    }
                    continue;
                }

                // Musical key.
                let velocity_profile = velocity_profiles
                    .get(velocity_profile_idx)
                    .cloned()
                    .unwrap_or(VelocityProfile::Linear);
                step_musical_key(
                    state,
                    hid_code,
                    analog_val,
                    rgb_idx,
                    wtn_board,
                    &wtn,
                    &map,
                    compact,
                    rotation_deg,
                    &mut midi,
                    &rgb_tx,
                    aftertouch_mode,
                    &velocity_profile,
                    manual_press_threshold,
                    settings.release_delta,
                    aftertouch_speed_max,
                    settings.aftertouch_delta,
                    octave_shift,
                    octave_hold_by_device.contains(&device_id),
                );
            }

            // Emit delayed note_ons whose peak window has elapsed.
            let velocity_profile = velocity_profiles
                .get(velocity_profile_idx)
                .cloned()
                .unwrap_or(VelocityProfile::Linear);
            emit_peak_note_ons(
                state,
                &mut midi,
                &velocity_profile,
                settings.velocity_peak_track_ms,
            );
        }

        // --- Screensaver activation ---
        if !screensaver_active
            && pressed_keys.is_empty()
            && screensaver_timeout_sec > 0
            && last_activity.elapsed() >= Duration::from_secs(screensaver_timeout_sec)
        {
            for &wtn_board in device_index_by_id.values() {
                let rgb_idx = rgb_index_for_board(settings, wtn_board);
                // Blank every LED cell we know about on this device.
                for (_, loc) in map.all_locs() {
                    try_send_drop(
                        &rgb_tx,
                        RgbCmd {
                            device_index: rgb_idx,
                            row: loc.led_row,
                            col: loc.led_col,
                            rgb: (0, 0, 0),
                        },
                    );
                }
                control_bar::paint_off(&rgb_tx, &settings.control_bar, rgb_idx);
            }
            screensaver_active = true;
            let _ = log_tx.try_send("screensaver: blanking LEDs (idle)".to_string());
        }

        // --- Snapshot for the TUI (drop on overflow). ---
        let now = Instant::now();
        if snapshot_due(now, &mut next_snapshot_at) {
            let snap = build_snapshot(
                edo,
                &midi_port,
                &active_layout_filename,
                aftertouch_mode,
                &velocity_profiles[velocity_profile_idx.min(velocity_profiles.len() - 1)],
                manual_press_threshold,
                aftertouch_speed_max,
                screensaver_active,
                &device_index_by_id,
                &octave_hold_by_device,
                &states,
                now,
            );
            let _ = snap_tx.try_send(snap);

            // HUD: same cadence as the TUI snapshot; submit() is wait-free.
            if let Some(h) = &hud_ctx {
                let mut held_iter: Vec<(u8, u8, u8)> = Vec::new();
                for (device_id, ds) in states.iter() {
                    let wtn_board = device_index_by_id
                        .get(device_id)
                        .copied()
                        .unwrap_or(0);
                    for ks in ds.keys.values() {
                        if let KeyState::Held { out_ch, note, .. } = *ks {
                            held_iter.push((wtn_board, out_ch, note));
                        }
                    }
                }
                let boards_present: Vec<u8> = device_index_by_id
                    .values()
                    .copied()
                    .collect();
                let pressed = crate::wooting::hud_ctx::pressed_from_held(
                    held_iter.into_iter(),
                    edo,
                    wtn.pitch_offset,
                    &boards_present,
                );
                let mode = crate::wooting::hud_ctx::HudWootingMode {
                    octave_shift: octave_shift.clamp(i8::MIN as i16, i8::MAX as i16) as i8,
                    press_threshold: manual_press_threshold,
                    aftertouch: aftertouch_mode.name().to_string(),
                    aftertouch_speed_max,
                    velocity_profile: velocity_profiles
                        [velocity_profile_idx.min(velocity_profiles.len() - 1)]
                    .name()
                    .to_string(),
                };
                crate::wooting::hud_ctx::submit_state(h, pressed, mode);
            }
        }

        thread::sleep(POLL_INTERVAL);
    }

    midi.all_notes_off();
    drop(master);
    let _ = log_tx.try_send("xentool serve: shutdown.".to_string());
    // Drop senders so the UI thread's recv_timeout returns Disconnected and
    // the loop exits cleanly.
    drop(snap_tx);
    drop(log_tx);
    let _ = ui_handle.join();
    Ok(())
}

/// Builds a single TUI snapshot from the live serve-loop state.
///
/// Allocations: one `Vec<HeldKeyDisplay>` (≤ N held keys) plus one
/// `Vec<DeviceLine>` (≤ device count). Both are tiny and bounded; called at
/// 25 Hz so the cost is negligible vs the 1 ms hot-loop budget.
#[allow(clippy::too_many_arguments)]
fn build_snapshot(
    edo: i32,
    midi_port: &str,
    layout_filename: &str,
    aftertouch_mode: AftertouchMode,
    velocity_profile: &VelocityProfile,
    manual_press_threshold: f32,
    aftertouch_speed_max: f32,
    screensaver_active: bool,
    device_index_by_id: &HashMap<DeviceId, u8>,
    octave_hold_by_device: &HashSet<DeviceId>,
    states: &HashMap<DeviceId, DeviceState>,
    now: Instant,
) -> WootingSnapshot {
    let mut held_keys: Vec<HeldKeyDisplay> = Vec::new();
    for (device_id, ds) in states.iter() {
        let wtn_board = device_index_by_id.get(device_id).copied().unwrap_or(0);
        for ks in ds.keys.values() {
            if let KeyState::Held {
                out_ch,
                note,
                last_pressure_sent,
                held_since,
                ..
            } = *ks
            {
                let age_ms = now.saturating_duration_since(held_since).as_millis() as u32;
                held_keys.push(HeldKeyDisplay {
                    wtn_board,
                    channel: out_ch,
                    note,
                    pressure: last_pressure_sent,
                    age_ms,
                });
            }
        }
    }
    held_keys.sort_by_key(|k| (k.wtn_board, k.channel, k.note));

    let mut octave_holds: Vec<DeviceLine> = device_index_by_id
        .iter()
        .map(|(id, &wtn_board)| DeviceLine {
            wtn_board,
            octave_hold: octave_hold_by_device.contains(id),
        })
        .collect();
    octave_holds.sort_by_key(|d| d.wtn_board);

    WootingSnapshot {
        edo,
        midi_port: midi_port.to_string(),
        layout_filename: layout_filename.to_string(),
        aftertouch_mode_name: aftertouch_mode.name(),
        velocity_profile_name: velocity_profile.name(),
        manual_press_threshold,
        aftertouch_speed_max,
        screensaver_active,
        device_count: device_index_by_id.len() as u8,
        octave_holds,
        held_keys,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bend_center_is_zero() {
        assert_eq!(bend_from_amounts(0.0, 0.0), 0);
    }

    #[test]
    fn bend_up_only_reaches_8191() {
        assert_eq!(bend_from_amounts(1.0, 0.0), 8191);
    }

    #[test]
    fn bend_down_only_reaches_minus_8192() {
        assert_eq!(bend_from_amounts(0.0, 1.0), -8192);
    }

    #[test]
    fn bend_partial_cancel() {
        assert_eq!(bend_from_amounts(0.6, 0.6), 0);
    }

    #[test]
    fn used_channels_includes_hold_and_shift() {
        use crate::wooting::wtn::{Wtn, WtnCell};
        use std::collections::HashMap;
        let mut cells = vec![WtnCell::default(); 56];
        cells[0] = WtnCell { key: 60, chan: 1, color: (0, 0, 0) };
        cells[1] = WtnCell { key: 62, chan: 2, color: (0, 0, 0) };
        let mut boards = HashMap::new();
        boards.insert(0u8, cells);
        let wtn = Wtn { edo: Some(31), pitch_offset: 0, boards };
        // shift=0, hold=false → ch0, ch1
        assert_eq!(used_channels(&wtn, 0, 0, false), vec![0u8, 1]);
        // hold=true → +1 on each
        assert_eq!(used_channels(&wtn, 0, 0, true), vec![1u8, 2]);
        // shift=+2, hold=false → ch2, ch3
        assert_eq!(used_channels(&wtn, 0, 2, false), vec![2u8, 3]);
    }
}
