//! openxen `serve` — drain module streams, run the key machines, emit MIDI, broadcast
//! MTS-ESP, publish HUD/TUI snapshots.
//!
//! Same shape as the Wooting serve loop: a ~1 kHz hot loop with no disk I/O and no
//! printing inside (TUI log lines go through a bounded channel on state transitions only).
//! Differences: frames arrive from reader threads (`hid.rs`) at ~4 kHz per module and are
//! processed in order with the module's own timestamps; keys are addressed by board and
//! Lumatone key index (no HID keycodes, no control bar). LEDs: the layout colours of each
//! board are sent to its module on connect (LED worker in `led.rs`); the module itself shows
//! the press highlight and turns its LEDs off when xentool stops (host timeout).

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
use crate::openxen::led::{self, LedConfig, LedMsg};
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

impl crate::api::MidiSink for MidiOut {
    fn send3(&mut self, msg: [u8; 3]) {
        self.send(msg);
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
            "depth" => AftertouchMode::Depth,
            _ => AftertouchMode::Off,
        },
        aftertouch_delta: s.aftertouch_delta,
        aftertouch_speed_max: s.aftertouch_speed_max,
    }
}

// --- LEDs ---

/// LED configuration from settings (highlight "off" or hex RRGGBB).
pub fn led_config(s: &OpenxenSettings) -> LedConfig {
    let h = s.leds.highlight.trim().trim_start_matches('#');
    let highlight = if h.eq_ignore_ascii_case("off") || h.len() != 6 {
        None
    } else {
        match (u8::from_str_radix(&h[0..2], 16), u8::from_str_radix(&h[2..4], 16), u8::from_str_radix(&h[4..6], 16)) {
            (Ok(r), Ok(g), Ok(b)) => Some((r, g, b)),
            _ => None,
        }
    };
    LedConfig {
        brightness: s.leds.brightness,
        highlight,
        timeout_ms: s.leds.timeout_ms,
        budget_ma: s.leds.power_budget_ma.min(led::BUDGET_MAX_MA),
    }
}

/// LED screensaver: blank after `timeout` without a press while no note sounds; the next
/// press wakes. Activity is a note-on (the key passed the press threshold), so rest noise
/// and single spikes below the threshold never keep the LEDs on or wake them.
pub(crate) struct Screensaver {
    timeout: Option<Duration>,
    last_activity: Instant,
    pub active: bool,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ScreensaverChange {
    Blank,
    Wake,
}

impl Screensaver {
    pub fn new(timeout_sec: u32, now: Instant) -> Self {
        let timeout = (timeout_sec > 0).then(|| Duration::from_secs(timeout_sec as u64));
        Self { timeout, last_activity: now, active: false }
    }

    /// `pressed`: a note-on happened since the last call; `sounding`: any note still held.
    pub fn tick(&mut self, now: Instant, pressed: bool, sounding: bool) -> Option<ScreensaverChange> {
        if pressed || sounding {
            self.last_activity = now;
        }
        if pressed && self.active {
            self.active = false;
            return Some(ScreensaverChange::Wake);
        }
        let timeout = self.timeout?;
        if !self.active && !sounding && now.duration_since(self.last_activity) >= timeout {
            self.active = true;
            return Some(ScreensaverChange::Blank);
        }
        None
    }
}

/// The layout colours of one board (keys without a channel stay dark, as on Wooting).
pub fn board_frame(wtn: &Wtn, board: u8) -> led::Frame {
    let mut f = [(0u8, 0u8, 0u8); led::KEYS];
    for (k, px) in f.iter_mut().enumerate() {
        if let Some(c) = wtn.cell(board, k).filter(|c| c.chan != 0) {
            *px = c.color;
        }
    }
    f
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
    led_status: u8,
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
            led_status: 0,
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

// --- local API (`--api`): key events out, LED overlay and demonstration notes in ---

/// Pitch of a layout cell: the same formula as the HUD and the tuning table.
fn cell_pitch(chan: u8, key: u8, edo: i32, pitch_offset: i32) -> i32 {
    (chan as i32 - 1) * edo + key as i32 + pitch_offset
}

/// Every playable cell of the layout, for the API's keyboard description.
pub(crate) fn layout_cells(wtn: &Wtn, edo: i32) -> Vec<crate::api::LayoutCell> {
    let mut out = Vec::new();
    for (&board, cells) in wtn.boards.iter() {
        for (k, c) in cells.iter().enumerate().filter(|(_, c)| c.chan != 0) {
            out.push(crate::api::LayoutCell { board, key: k as u8, pitch: cell_pitch(c.chan, c.key, edo, wtn.pitch_offset), color: c.color });
        }
    }
    out
}

/// The serve loop's side of the API: publishes what happens, applies what is asked.
struct ApiLink {
    hub: crate::api::ApiHub,
    rx: Receiver<crate::api::Command>,
    geometry: crate::geometry::GeometryInfo,
    cells: Vec<crate::api::LayoutCell>,
    layout: String,
    edo: i32,
    pitch_offset: i32,
    started: Instant,
    /// The overlay as last asked for, so a module that reconnects gets it back.
    overlay: crate::api::OverlayState,
    /// Notes sounded for the API, released by xentool.
    notes: crate::api::TimedNotes,
    /// A layout the API asked to switch to; the serve loop takes it.
    layout_request: Option<String>,
}

impl ApiLink {
    fn start(port: u16, wtn: &Wtn, edo: i32, file: &std::path::Path, started: Instant) -> Result<Self> {
        let layout = crate::hud::layout_id_from_path(file);
        let geometry = crate::geometry::geometry_by_name("lumatone", 1).context("lumatone geometry")?;
        let cells = layout_cells(wtn, edo);
        let (hub, rx) = crate::api::ApiHub::new(crate::api::build_setup("openxen", &geometry, &layout, edo, &cells, &[]));
        hub.set_layout_file(file);
        let port = crate::api::spawn(hub.clone(), port)?;
        eprintln!("xentool API: listening on http://127.0.0.1:{port}/ (this machine only)");
        Ok(Self { hub, rx, geometry, cells, layout, edo, pitch_offset: wtn.pitch_offset, started, overlay: Default::default(), notes: Default::default(), layout_request: None })
    }

    /// Another layout was loaded: new key description, and the overlay (which belonged to
    /// the old notes) is gone on every board.
    fn set_layout(&mut self, wtn: &Wtn, edo: i32, layout: String, boards: &BTreeMap<u8, Arc<str>>, led_tx: &crossbeam_channel::Sender<LedMsg>) {
        self.cells = layout_cells(wtn, edo);
        self.edo = edo;
        self.pitch_offset = wtn.pitch_offset;
        self.layout = layout;
        self.overlay.keys.clear();
        for serial in boards.values() {
            let _ = led_tx.send(LedMsg::Overlay { serial: serial.clone(), set: Vec::new(), clear: led::OverlayClear::All, dim_base: None });
        }
        self.publish_boards(boards);
        self.hub.layout_result(None);
    }

    fn publish_boards(&self, boards: &BTreeMap<u8, Arc<str>>) {
        let present: Vec<u8> = boards.keys().copied().collect();
        self.hub.set_setup(crate::api::build_setup("openxen", &self.geometry, &self.layout, self.edo, &self.cells, &present));
    }

    fn key_event(&self, board: u8, key: usize, kind: crate::api::KeyKind, velocity: u8, ch: u8, note: u8) {
        self.hub.publish_key(&crate::api::KeyEvent {
            t_us: self.started.elapsed().as_micros() as u64,
            board,
            key: key as u8,
            kind,
            velocity,
            pitch: ch as i32 * self.edo + note as i32 + self.pitch_offset,
        });
    }

    fn overlay_active(&self) -> bool {
        self.overlay.is_active()
    }

    /// Send a board its part of the overlay (after it connected).
    fn resend_overlay(&self, board: u8, serial: &Arc<str>, led_tx: &crossbeam_channel::Sender<LedMsg>) {
        let set: Vec<(u8, led::Rgb)> = self.overlay.keys.iter().filter(|((b, _), _)| *b == board).map(|((_, k), rgb)| (*k, *rgb)).collect();
        if !set.is_empty() || self.overlay.dim_base.is_some() {
            let _ = led_tx.send(LedMsg::Overlay { serial: serial.clone(), set, clear: led::OverlayClear::All, dim_base: self.overlay.dim_base });
        }
    }

    /// Apply queued commands. Returns true if there was any (counts as activity).
    fn apply_commands(&mut self, boards: &BTreeMap<u8, Arc<str>>, wtn: &Wtn, midi: &mut MidiOut, led_tx: &crossbeam_channel::Sender<LedMsg>) -> bool {
        use crate::api::{Command, OverlayClear};
        let mut any = false;
        while let Ok(cmd) = self.rx.try_recv() {
            any = true;
            match cmd {
                Command::Overlay { clear, set, dim_base } => {
                    self.overlay.apply(&clear, &set, dim_base, |_, k| (k as usize) < led::KEYS);
                    for (&b, serial) in boards.iter() {
                        let board_set: Vec<(u8, led::Rgb)> = set.iter().filter(|s| s.0 == b).map(|s| (s.1, s.2)).collect();
                        let board_clear = match &clear {
                            OverlayClear::None => led::OverlayClear::None,
                            OverlayClear::All => led::OverlayClear::All,
                            OverlayClear::Keys(keys) => led::OverlayClear::Keys(keys.iter().filter(|k| k.0 == b).map(|k| k.1).collect()),
                        };
                        let _ = led_tx.send(LedMsg::Overlay { serial: serial.clone(), set: board_set, clear: board_clear, dim_base });
                    }
                }
                Command::Layout { name } => self.layout_request = Some(name),
                Command::Midi { notes } => {
                    for n in &notes {
                        self.notes.raw_on(n, Instant::now(), midi);
                    }
                }
                Command::Notes { on, off } => {
                    for key in off {
                        self.notes.key_off(key, midi);
                    }
                    for n in on {
                        let key = (n.board, n.key);
                        self.notes.key_off(key, midi);
                        if !boards.contains_key(&n.board) {
                            continue;
                        }
                        if let Some(cell) = wtn.cell(n.board, n.key as usize).filter(|c| c.chan != 0) {
                            let ch = cell.chan.saturating_sub(1).min(15);
                            self.notes.key_on(key, ch, cell.key, n.velocity, n.ms, Instant::now(), midi);
                        }
                    }
                }
            }
        }
        any
    }

    /// Release notes whose length has passed.
    fn tick_notes(&mut self, now: Instant, midi: &mut MidiOut) {
        self.notes.tick(now, midi);
    }

    fn release_all(&mut self, midi: &mut MidiOut) {
        self.notes.release_all(midi);
    }
}

/// Map one key action to MIDI. Note-on resolves the layout cell (board, key) at press
/// time; note-off and pressure use what was resolved, so a layout change mid-note cannot
/// leave hanging notes.
fn apply(action: Action, m: &mut Module, key: usize, wtn: &Wtn, midi: &mut MidiOut, api: Option<&ApiLink>) {
    match action {
        Action::NoteOn { velocity } => {
            if let Some(cell) = wtn.cell(m.board, key).filter(|c| c.chan != 0) {
                let ch = cell.chan.saturating_sub(1).min(15);
                midi.send([0x90 | ch, cell.key & 0x7F, velocity & 0x7F]);
                m.sounding[key] = Some((ch, cell.key & 0x7F));
                if let Some(a) = api {
                    a.key_event(m.board, key, crate::api::KeyKind::On, velocity & 0x7F, ch, cell.key & 0x7F);
                }
            }
        }
        Action::NoteOff => {
            if let Some((ch, note)) = m.sounding[key].take() {
                midi.send([0x80 | ch, note, 0]);
                if let Some(a) = api {
                    a.key_event(m.board, key, crate::api::KeyKind::Off, 0, ch, note);
                }
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
/// Read a layout file: the layout and its tuning.
fn load_layout(file: &std::path::Path) -> Result<(Wtn, i32)> {
    let content = std::fs::read_to_string(file).with_context(|| format!("reading {}", file.display()))?;
    let wtn = parse_wtn(&content).with_context(|| format!("parsing {}", file.display()))?;
    let edo = wtn.edo.with_context(|| {
        format!("Edo= not set in {}; add e.g. `Edo=31` before the first [Board] section.", file.display())
    })?;
    Ok((wtn, edo))
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
    api_port: Option<u16>,
    settings: &OpenxenSettings,
) -> Result<()> {
    let mut file = file;
    let (mut wtn, mut edo) = load_layout(&file)?;
    let mut layout_filename = file.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();

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

    let (led_tx, led_rx) = crossbeam_channel::unbounded::<LedMsg>();
    let _led_worker = led::spawn_worker(led_rx);
    let _ = led_tx.send(LedMsg::Config(led_config(settings)));
    let (ev_tx, ev_rx) = bounded::<Event>(CHANNEL_CAPACITY);
    let mut manager = Manager::new(ev_tx, Some(led_tx.clone()))?;
    manager.refresh()?;

    let shutdown = Arc::new(AtomicBool::new(false));
    let shutdown_ctrlc = shutdown.clone();
    ctrlc::set_handler(move || shutdown_ctrlc.store(true, Ordering::Relaxed)).context("failed to set Ctrl+C handler")?;

    let (snap_tx, snap_rx) = bounded::<WootingSnapshot>(2);
    let (log_tx, log_rx) = bounded::<String>(64);
    let _ = log_tx.try_send(format!("xentool serve (openxen, experimental): {edo}-EDO | MIDI → `{midi_port}` | {}", crate::mts::status_text()));
    let _ = log_tx.try_send("xentool serve: reading openxen module streams (Ctrl+C / q to stop)".to_string());
    let shutdown_ui = shutdown.clone();
    let hud_url_for_ui = hud_url.clone();
    let ui_handle = thread::spawn(move || {
        if let Err(e) = run_wooting_serve_ui(snap_rx, log_rx, shutdown_ui, hud_url_for_ui) {
            eprintln!("xentool serve TUI error: {e}");
        }
    });

    let make_hud_ctx = |file: &std::path::Path, wtn: &Wtn, edo: i32| -> Option<crate::wooting::hud_ctx::HudWootingHandle> {
        if !(hud || tune_supercollider) {
            return None;
        }
        let layout_id = crate::hud::layout_id_from_path(file);
        Some(
            crate::wooting::hud_ctx::HudWootingCtx {
                publisher: hud_publisher.clone(),
                layout_id: layout_id.clone(),
                layout_name: layout_id,
                edo,
                pitch_offset: wtn.pitch_offset,
                layout_pitches: crate::wooting::hud_ctx::build_layout_pitches(wtn),
            }
            .into_handle(),
        )
    };
    let mut hud_ctx = make_hud_ctx(&file, &wtn, edo);

    // Persist the layout as the last used one (background thread; never in the hot loop).
    {
        let p = file.clone();
        thread::spawn(move || crate::settings::store_last_layout(crate::layouts::LayoutKind::Otn, &p));
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
    let mut screensaver = Screensaver::new(settings.leds.screensaver_timeout_sec, started);
    let mut api: Option<ApiLink> = match api_port {
        Some(port) => {
            let link = ApiLink::start(port, &wtn, edo, &file, started)?;
            let _ = log_tx.try_send(format!("API: http://127.0.0.1:{port}/api/setup (this machine only)"));
            Some(link)
        }
        None => None,
    };

    while !shutdown.load(Ordering::Relaxed) {
        if last_hotplug.elapsed() >= HOTPLUG_INTERVAL {
            if let Err(e) = manager.refresh() {
                let _ = log_tx.try_send(format!("openxen: device refresh failed: {e:#}"));
            }
            last_hotplug = Instant::now();
        }

        let pressed = drain(&ev_rx, &mut modules, &mut boards, settings, &params, &profile, &wtn, &mut midi, &log_tx, &led_tx, api.as_ref());
        let sounding = modules.values().any(|m| m.sounding.iter().any(|s| s.is_some()));
        // An API command wakes the LEDs like a key press, and an overlay keeps them on.
        let (api_command, overlay_on) = match api.as_mut() {
            Some(a) => {
                let any = a.apply_commands(&boards, &wtn, &mut midi, &led_tx);
                a.tick_notes(Instant::now(), &mut midi);
                (any, a.overlay_active())
            }
            None => (false, false),
        };
        // The API asked for another layout (tuning): everything that sounds is released, the
        // synth gets the new tuning, the keys their new colours, clients a new description.
        if let Some(name) = api.as_mut().and_then(|a| a.layout_request.take()) {
            let path = file.with_file_name(format!("{name}.{}", file.extension().and_then(|e| e.to_str()).unwrap_or("otn")));
            let loaded = load_layout(&path).and_then(|(new_wtn, new_edo)| {
                for m in modules.values_mut() {
                    m.release_all(&mut midi);
                }
                if let Some(a) = api.as_mut() {
                    a.release_all(&mut midi);
                }
                midi.all_notes_off();
                master.set_scale_name(&format!("{new_edo}-EDO"))?;
                publish_tuning(&master, new_edo, new_wtn.pitch_offset)?;
                Ok((new_wtn, new_edo))
            });
            match loaded {
                Ok((new_wtn, new_edo)) => {
                    wtn = new_wtn;
                    edo = new_edo;
                    file = path;
                    layout_filename = file.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                    hud_ctx = make_hud_ctx(&file, &wtn, edo);
                    for (b, serial) in boards.iter() {
                        let _ = led_tx.send(LedMsg::Frame { serial: serial.clone(), frame: Box::new(board_frame(&wtn, *b)) });
                    }
                    if let Some(a) = api.as_mut() {
                        a.set_layout(&wtn, edo, crate::hud::layout_id_from_path(&file), &boards, &led_tx);
                    }
                    let p = file.clone();
                    thread::spawn(move || crate::settings::store_last_layout(crate::layouts::LayoutKind::Otn, &p));
                    let _ = log_tx.try_send(format!("layout: {layout_filename} ({edo}-EDO)"));
                }
                Err(e) => {
                    // the old layout stays; make sure its tuning is the one published
                    let _ = master.set_scale_name(&format!("{edo}-EDO"));
                    let _ = publish_tuning(&master, edo, wtn.pitch_offset);
                    if let Some(a) = api.as_ref() {
                        a.hub.layout_result(Some(format!("{e:#}")));
                    }
                    let _ = log_tx.try_send(format!("layout: switching to {name} failed: {e:#}"));
                }
            }
        }
        match screensaver.tick(Instant::now(), pressed || api_command, sounding || overlay_on) {
            Some(ScreensaverChange::Blank) => {
                for serial in boards.values() {
                    let _ = led_tx.send(LedMsg::Frame { serial: serial.clone(), frame: Box::new([(0, 0, 0); led::KEYS]) });
                }
                let _ = log_tx.try_send("screensaver: LEDs off (idle)".to_string());
            }
            Some(ScreensaverChange::Wake) => {
                for (b, serial) in boards.iter() {
                    let _ = led_tx.send(LedMsg::Frame { serial: serial.clone(), frame: Box::new(board_frame(&wtn, *b)) });
                }
                let _ = log_tx.try_send("screensaver: wake".to_string());
            }
            None => {}
        }

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
            let snap = build_snapshot(edo, &midi_port, &layout_filename, &params, &profile, &modules, started.elapsed(), screensaver.active);
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
    if let Some(a) = api.as_mut() {
        a.release_all(&mut midi);
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
    led_tx: &crossbeam_channel::Sender<LedMsg>,
    api: Option<&ApiLink>,
) -> bool {
    let mut pressed = false;
    let mut boards_changed = false;
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
                        let _ = led_tx.send(LedMsg::Frame { serial: serial.clone(), frame: Box::new(board_frame(wtn, b)) });
                        if let Some(a) = api {
                            a.resend_overlay(b, &serial, led_tx);
                        }
                        boards_changed = true;
                        let _ = log_tx.try_send(format!("openxen: module {serial} -> board{b}"));
                    }
                    None => {
                        let _ = log_tx.try_send(format!("openxen: module {serial} ignored (no free board)"));
                    }
                }
            }
            Event::Disconnected { serial, reason } => {
                let _ = led_tx.send(LedMsg::Detach { serial: serial.clone() });
                if let Some(mut m) = modules.remove(&serial) {
                    m.release_all(midi);
                    boards.remove(&m.board);
                    boards_changed = true;
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
                if frame.led_status != m.led_status {
                    use crate::openxen::proto::{LED_STATUS_I2C_ERROR, LED_STATUS_TIMEOUT};
                    let new_bits = frame.led_status & !m.led_status;
                    if new_bits & LED_STATUS_I2C_ERROR != 0 {
                        let _ = log_tx.try_send(format!("openxen board{}: LED driver I2C error (module re-initialises it)", m.board));
                    }
                    if new_bits & LED_STATUS_TIMEOUT != 0 {
                        let _ = log_tx.try_send(format!("openxen board{}: LEDs timed out (no LED commands reached the module)", m.board));
                    }
                    m.led_status = frame.led_status;
                }
                for k in 0..(frame.num_keys as usize).min(MAX_KEYS) {
                    if let Some(a) = m.keys[k].step(frame.time_us, frame.travel[k], params, profile) {
                        pressed |= matches!(a, Action::NoteOn { .. });
                        apply(a, m, k, wtn, midi, api);
                    }
                }
            }
        }
    }
    if boards_changed {
        if let Some(a) = api {
            a.publish_boards(boards);
        }
    }
    pressed
}

fn build_snapshot(
    edo: i32,
    midi_port: &str,
    layout_filename: &str,
    params: &KeyParams,
    profile: &VelocityProfile,
    modules: &HashMap<Arc<str>, Module>,
    _uptime: Duration,
    screensaver_active: bool,
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
        screensaver_active,
        device_count: modules.len() as u8,
        octave_holds: lines,
        held_keys,
    }
}

#[cfg(test)]
mod tests {

    #[test]
    fn screensaver_blanks_after_idle_and_wakes_on_press() {
        let t0 = Instant::now();
        let s = |secs: u64| t0 + Duration::from_secs(secs);
        let mut ss = Screensaver::new(120, t0);
        assert_eq!(ss.tick(s(60), false, false), None);
        assert_eq!(ss.tick(s(119), false, false), None);
        assert_eq!(ss.tick(s(120), false, false), Some(ScreensaverChange::Blank));
        assert_eq!(ss.tick(s(500), false, false), None, "blank only once");
        assert_eq!(ss.tick(s(501), true, true), Some(ScreensaverChange::Wake));
        assert_eq!(ss.tick(s(502), false, false), None);
        // a held note keeps the LEDs on however long it sounds
        assert_eq!(ss.tick(s(1000), false, true), None);
        assert_eq!(ss.tick(s(1119), false, false), None);
        assert_eq!(ss.tick(s(1120), false, false), Some(ScreensaverChange::Blank));
    }

    #[test]
    fn screensaver_zero_means_never() {
        let t0 = Instant::now();
        let mut ss = Screensaver::new(0, t0);
        assert_eq!(ss.tick(t0 + Duration::from_secs(100_000), false, false), None);
    }
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
    fn led_config_from_settings() {
        let mut s = OpenxenSettings::default();
        let c = led_config(&s);
        assert_eq!(c.highlight, Some((255, 255, 255)));
        assert_eq!((c.timeout_ms, c.budget_ma), (2000, 300));
        s.leds.highlight = "off".into();
        s.leds.power_budget_ma = 9000;
        let c = led_config(&s);
        assert_eq!(c.highlight, None);
        assert_eq!(c.budget_ma, led::BUDGET_MAX_MA);
        s.leds.highlight = "#00ff7f".into();
        assert_eq!(led_config(&s).highlight, Some((0, 255, 127)));
    }

    #[test]
    fn board_frame_uses_layout_colours() {
        let text = "Edo=31\n[Board1]\nKey_0=1\nChan_0=2\nCol_0=FF8000\nKey_1=2\nChan_1=0\nCol_1=00FF00\n";
        let wtn = parse_wtn(text).unwrap();
        let f = board_frame(&wtn, 1);
        assert_eq!(f[0], (255, 128, 0));
        assert_eq!(f[1], (0, 0, 0)); // no channel -> dark
        assert_eq!(board_frame(&wtn, 0), [(0, 0, 0); led::KEYS]);
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
