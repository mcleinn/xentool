//! Local control API (`serve --api`): lets another program on this machine see which keys
//! are played and put colours on top of the layout, e.g. a tutor that lights the keys to
//! press. Bound to 127.0.0.1 only; a client that serves other machines adds its own access
//! control.
//!
//! Endpoints:
//! - `GET  /api/setup`         the keyboard as one keyfield (see `Setup`)
//! - `GET  /api/keys/state`    keys held right now
//! - `GET  /api/events?after=N&wait_ms=M`  events with a sequence number above N: `key` per
//!                             press and release, `setup` when modules come or go. Waits up
//!                             to M ms (max 30 s) for the first one, then answers at once.
//!                             Without `after` it only tells the current number (`next`),
//!                             to start listening "from now". A long poll rather than a
//!                             stream: the HTTP library buffers streamed bodies in 8 KB
//!                             chunks, which would hold single key events back.
//! - `POST /api/leds/overlay`  `{"clear": "all" | [[board, key], ..], "set": [{"board", "key",
//!                             "rgb": "RRGGBB"}, ..], "dim_base": 0..1}`
//! - `POST /api/notes`         `{"on": [{"board", "key", "velocity", "ms"}], "off": [[board, key]]}`
//!                             sounds keys through the normal MIDI output (`ms`: note length,
//!                             the note is released by xentool)
//! - `POST /api/chords/name`   `{"edo", "pitch_classes": [..]}` → chord names per root
//! - `GET  /api/boards`        which module sits on which board (`modules`: board, serial,
//!                             pinned in the settings or not) and whether the order can be
//!                             changed while serving (`reassign`)
//! - `POST /api/boards`        `{"assign": [{"serial", "board"}, ..], "pins": [{"serial",
//!                             "board"}, ..]}`: move connected modules to other boards at once
//!                             (layout colours and key mapping follow, a `setup` event is
//!                             sent); `pins` (optional) replaces the pinning the serve loop
//!                             uses for modules that connect later. 501 on a backend with a
//!                             fixed board order. Used by `xentool boards`.
//!
//! The description is backend-neutral: every key has an address (board, key), a position in
//! the geometry's shared hex lattice (board shifts already applied) and a pitch. Clients
//! need no knowledge of a particular keyboard. All three backends feed it
//! (`openxen/serve.rs` `ApiLink`, `wooting/api_link.rs`, `exquis/api_link.rs`).
//!
//! Threading: the serve loop owns the devices. HTTP threads only read shared state
//! (`ApiHub`) and queue `Command`s, which the serve loop applies between scans.

use std::collections::{BTreeSet, HashMap, VecDeque};
use std::io::Read;
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use crossbeam_channel::{Receiver, Sender, unbounded};
use rouille::Response;
use serde::{Deserialize, Serialize};

use crate::geometry::{GeometryInfo, Orientation};
use crate::hud::chordnam;

/// Events kept for clients to fetch; a client further behind is told it missed some.
const EVENT_LOG: usize = 4096;
/// Longest wait of one `/api/events` request.
const MAX_WAIT: Duration = Duration::from_secs(30);

pub type Rgb = (u8, u8, u8);

// --- description of the keyboard ---

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct SetupKey {
    pub board: u8,
    pub key: u8,
    /// Position in the shared hex lattice (see `Setup::directions`).
    pub x: i32,
    pub y: i32,
    /// Steps of the tuning above its lowest reference (`pitch / edo` = octaves).
    pub pitch: i32,
    /// Frequency in Hz.
    pub hz: f64,
    /// Layout colour, `RRGGBB`.
    pub color: String,
}

/// Lattice offsets of the six neighbours, named as the player sees them.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Directions {
    pub left: (i32, i32),
    pub right: (i32, i32),
    pub upper_left: (i32, i32),
    pub upper_right: (i32, i32),
    pub lower_left: (i32, i32),
    pub lower_right: (i32, i32),
}

impl Directions {
    pub fn of(orientation: Orientation) -> Self {
        // y is the lateral axis in both conventions; x is the row, counted away from the
        // player (YRightXUp) or towards the player (YRightXDown).
        let up = match orientation {
            Orientation::YRightXDown => -1,
            Orientation::YRightXUp => 1,
        };
        Directions {
            left: (0, -2),
            right: (0, 2),
            upper_left: (up, -1),
            upper_right: (up, 1),
            lower_left: (-up, -1),
            lower_right: (-up, 1),
        }
    }
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Setup {
    pub backend: String,
    pub geometry: String,
    pub layout: String,
    pub edo: i32,
    /// Boards that are connected now, ascending.
    pub boards: Vec<u8>,
    /// The device behind each connected board, where the backend knows devices by serial
    /// number (openxen); empty otherwise.
    pub modules: Vec<SetupModule>,
    pub directions: Directions,
    /// Playable keys of the connected boards.
    pub keys: Vec<SetupKey>,
}

/// A connected device and the board it plays.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SetupModule {
    pub board: u8,
    pub serial: String,
    /// The settings file names this board for the serial (else: assigned automatically).
    pub pinned: bool,
}

/// One layout cell as the backend knows it: pitch and colour of (board, key).
pub struct LayoutCell {
    pub board: u8,
    pub key: u8,
    pub pitch: i32,
    pub color: Rgb,
}

pub fn hex_color(c: Rgb) -> String {
    format!("{:02X}{:02X}{:02X}", c.0, c.1, c.2)
}

pub fn parse_hex_color(s: &str) -> Option<Rgb> {
    let h = s.trim().trim_start_matches('#');
    if h.len() != 6 || !h.is_ascii() {
        return None;
    }
    Some((
        u8::from_str_radix(&h[0..2], 16).ok()?,
        u8::from_str_radix(&h[2..4], 16).ok()?,
        u8::from_str_radix(&h[4..6], 16).ok()?,
    ))
}

/// Join a geometry (where keys are) with a layout (what they play) for the connected boards.
pub fn build_setup(backend: &str, geometry: &GeometryInfo, layout: &str, edo: i32, cells: &[LayoutCell], present: &[u8]) -> Setup {
    let mut boards: Vec<u8> = present.to_vec();
    boards.sort_unstable();
    boards.dedup();
    let mut keys = Vec::new();
    for cell in cells.iter().filter(|c| boards.contains(&c.board)) {
        let Some(tuples) = geometry.boards.get(cell.board as usize) else { continue };
        let Some(&(_, x, y)) = tuples.iter().find(|t| t.0 == cell.key) else { continue };
        keys.push(SetupKey {
            board: cell.board,
            key: cell.key,
            x,
            y,
            pitch: cell.pitch,
            hz: crate::mts::edo_freq_hz(edo, cell.pitch),
            color: hex_color(cell.color),
        });
    }
    keys.sort_by_key(|k| (k.board, k.key));
    Setup {
        backend: backend.to_string(),
        geometry: geometry.name.to_string(),
        layout: layout.to_string(),
        edo,
        boards,
        modules: Vec::new(),
        directions: Directions::of(geometry.orientation),
        keys,
    }
}

// --- events and commands ---

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum KeyKind {
    On,
    Off,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct KeyEvent {
    /// Microseconds since `serve` started (host clock).
    pub t_us: u64,
    pub board: u8,
    pub key: u8,
    pub kind: KeyKind,
    /// 1..127 for `on`, 0 for `off`.
    pub velocity: u8,
    pub pitch: i32,
}

#[derive(Debug, Clone, PartialEq)]
pub enum OverlayClear {
    None,
    Keys(Vec<(u8, u8)>),
    All,
}

#[derive(Debug, Clone, PartialEq)]
pub struct NoteOn {
    pub board: u8,
    pub key: u8,
    pub velocity: u8,
    /// Note length; None = until an explicit `off`.
    pub ms: Option<u32>,
}

/// What the serve loop is asked to do.
#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    Overlay { clear: OverlayClear, set: Vec<(u8, u8, Rgb)>, dim_base: Option<f32> },
    Notes { on: Vec<NoteOn>, off: Vec<(u8, u8)> },
    /// Load another layout (a name from `GET /api/layouts`) while serving.
    Layout { name: String },
    /// Notes sent as they are on a MIDI channel of the client's choice, outside the layout:
    /// percussion and metronome clicks (channel 10 by General MIDI convention).
    Midi { notes: Vec<MidiNote> },
    /// Move connected devices to other boards (`assign`: serial → board) and, if given,
    /// replace the pinning used for devices that connect later (`pins`).
    Boards { assign: Vec<(String, u8)>, pins: Option<Vec<(String, u8)>> },
}

#[derive(Deserialize)]
struct BoardPinBody {
    serial: String,
    board: u8,
}

#[derive(Deserialize)]
struct BoardsBody {
    #[serde(default)]
    assign: Vec<BoardPinBody>,
    #[serde(default)]
    pins: Option<Vec<BoardPinBody>>,
}

pub fn parse_boards(body: &[u8]) -> Result<Command, String> {
    let b: BoardsBody = serde_json::from_slice(body).map_err(|e| e.to_string())?;
    let pairs = |v: Vec<BoardPinBody>| v.into_iter().map(|p| (p.serial, p.board)).collect::<Vec<_>>();
    Ok(Command::Boards { assign: pairs(b.assign), pins: b.pins.map(pairs) })
}

/// The assignment (serial, board) after moving the devices named in `assign`; devices not
/// named keep their board. Err = why it cannot be done (nothing is changed then). The one
/// place that checks a reassignment: the HTTP handler answers with it, the serve loop
/// applies its result.
pub fn reassigned(current: &[(String, u8)], assign: &[(String, u8)], max_boards: u8) -> Result<Vec<(String, u8)>, String> {
    let mut out = current.to_vec();
    let mut named: Vec<&str> = Vec::new();
    for (serial, board) in assign {
        if named.contains(&serial.as_str()) {
            return Err(format!("serial {serial} is named twice"));
        }
        named.push(serial);
        if *board >= max_boards {
            return Err(format!("board {board} does not exist (boards 0..{})", max_boards.saturating_sub(1)));
        }
        match out.iter_mut().find(|(s, _)| s == serial) {
            Some(entry) => entry.1 = *board,
            None => return Err(format!("no connected module has the serial {serial}")),
        }
    }
    let mut boards: Vec<u8> = out.iter().map(|(_, b)| *b).collect();
    boards.sort_unstable();
    if let Some(w) = boards.windows(2).find(|w| w[0] == w[1]) {
        return Err(format!("board {} would be given to two modules", w[0]));
    }
    Ok(out)
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct MidiNote {
    /// 1..16.
    pub channel: u8,
    pub note: u8,
    #[serde(default = "default_velocity")]
    pub velocity: u8,
    /// Length in milliseconds.
    #[serde(default = "default_click_ms")]
    pub ms: u32,
}

fn default_click_ms() -> u32 {
    60
}

#[derive(Deserialize)]
struct MidiBody {
    notes: Vec<MidiNote>,
}

pub fn parse_midi(body: &[u8]) -> Result<Command, String> {
    let b: MidiBody = serde_json::from_slice(body).map_err(|e| e.to_string())?;
    for n in &b.notes {
        if !(1..=16).contains(&n.channel) {
            return Err(format!("channel must be 1..16, not {}", n.channel));
        }
        if n.note > 127 {
            return Err(format!("note must be 0..127, not {}", n.note));
        }
    }
    let notes = b.notes.into_iter().map(|n| MidiNote { velocity: n.velocity.clamp(1, 127), ms: n.ms.clamp(1, 10_000), ..n }).collect();
    Ok(Command::Midi { notes })
}

// --- request bodies ---

#[derive(Deserialize)]
#[serde(untagged)]
enum ClearBody {
    Word(String),
    Keys(Vec<(u8, u8)>),
}

#[derive(Deserialize)]
struct OverlayKeyBody {
    board: u8,
    key: u8,
    rgb: String,
}

#[derive(Deserialize)]
struct OverlayBody {
    #[serde(default)]
    clear: Option<ClearBody>,
    #[serde(default)]
    set: Vec<OverlayKeyBody>,
    #[serde(default)]
    dim_base: Option<f32>,
}

#[derive(Deserialize)]
struct NoteOnBody {
    board: u8,
    key: u8,
    #[serde(default = "default_velocity")]
    velocity: u8,
    #[serde(default)]
    ms: Option<u32>,
}

fn default_velocity() -> u8 {
    80
}

#[derive(Deserialize)]
struct NotesBody {
    #[serde(default)]
    on: Vec<NoteOnBody>,
    #[serde(default)]
    off: Vec<(u8, u8)>,
}

#[derive(Deserialize)]
struct ChordBody {
    edo: i32,
    pitch_classes: Vec<i32>,
}

pub fn parse_overlay(body: &[u8]) -> Result<Command, String> {
    let b: OverlayBody = serde_json::from_slice(body).map_err(|e| e.to_string())?;
    let clear = match b.clear {
        None => OverlayClear::None,
        Some(ClearBody::Word(w)) if w.eq_ignore_ascii_case("all") => OverlayClear::All,
        Some(ClearBody::Word(w)) => return Err(format!("clear: expected \"all\" or a list of [board, key], got \"{w}\"")),
        Some(ClearBody::Keys(k)) => OverlayClear::Keys(k),
    };
    let mut set = Vec::with_capacity(b.set.len());
    for k in b.set {
        let rgb = parse_hex_color(&k.rgb).ok_or_else(|| format!("rgb: expected RRGGBB, got \"{}\"", k.rgb))?;
        set.push((k.board, k.key, rgb));
    }
    if let Some(d) = b.dim_base {
        if !(0.0..=1.0).contains(&d) {
            return Err(format!("dim_base: expected 0..1, got {d}"));
        }
    }
    Ok(Command::Overlay { clear, set, dim_base: b.dim_base })
}

pub fn parse_notes(body: &[u8]) -> Result<Command, String> {
    let b: NotesBody = serde_json::from_slice(body).map_err(|e| e.to_string())?;
    let on = b
        .on
        .into_iter()
        .map(|n| NoteOn { board: n.board, key: n.key, velocity: n.velocity.clamp(1, 127), ms: n.ms })
        .collect();
    Ok(Command::Notes { on, off: b.off })
}

#[derive(Serialize)]
struct ChordName<'a> {
    root_pc: i32,
    /// Steps of the chord's notes above the root (the root itself is 0).
    rel: &'a [i32],
    best: &'a str,
    names: &'a [String],
}

fn chord_names_json(edo: i32, pitch_classes: &[i32]) -> serde_json::Value {
    let results = chordnam::find_chord_names(chordnam::db(), edo, pitch_classes);
    let out: Vec<ChordName> = results
        .iter()
        .filter(|r| !r.names.is_empty())
        .map(|r| ChordName { root_pc: r.root_pc, rel: &r.rel, best: chordnam::best_name(&r.names), names: &r.names })
        .collect();
    serde_json::json!({ "edo": edo, "chords": out })
}

// --- shared state ---

/// The last `EVENT_LOG` events, numbered from 1 in the order they happened.
#[derive(Default)]
struct EventLog {
    last_seq: u64,
    events: VecDeque<(u64, serde_json::Value)>,
}

/// Answer of `/api/events`.
#[derive(Debug, Serialize, PartialEq)]
pub struct Events {
    /// Pass this as `after` in the next request.
    pub next: u64,
    /// True if events between `after` and the first one returned were dropped from the log.
    pub missed: bool,
    pub events: Vec<serde_json::Value>,
}

/// Which layouts can be loaded, and how the last switch went.
#[derive(Default)]
struct LayoutState {
    /// Folder of the layout being served; its other layout files can be switched to.
    dir: Option<std::path::PathBuf>,
    extension: String,
    switching: bool,
    last_error: Option<String>,
}

struct Inner {
    /// Endpoints this session cannot serve: (path, why).
    refused: Mutex<Vec<(&'static str, String)>>,
    layouts: Mutex<LayoutState>,
    /// Number of boards devices can be moved between while serving; None = fixed order.
    reassign: Mutex<Option<u8>>,
    setup: Mutex<Arc<Setup>>,
    held: Mutex<BTreeSet<(u8, u8)>>,
    log: Mutex<EventLog>,
    log_changed: Condvar,
    commands: Sender<Command>,
}

/// Handle shared by the serve loop (publishes) and the HTTP threads (read, queue commands).
#[derive(Clone)]
pub struct ApiHub {
    inner: Arc<Inner>,
}

impl ApiHub {
    /// The hub and the receiving end of its command queue (for the serve loop).
    pub fn new(setup: Setup) -> (Self, Receiver<Command>) {
        let (tx, rx) = unbounded();
        let inner = Inner {
            refused: Mutex::new(Vec::new()),
            layouts: Mutex::new(LayoutState::default()),
            reassign: Mutex::new(None),
            setup: Mutex::new(Arc::new(setup)),
            held: Mutex::new(BTreeSet::new()),
            log: Mutex::new(EventLog::default()),
            log_changed: Condvar::new(),
            commands: tx,
        };
        (Self { inner: Arc::new(inner) }, rx)
    }

    /// This session cannot serve `path` (e.g. no MIDI output of its own): requests get a
    /// 501 with `reason` instead of being queued and silently dropped.
    pub fn refuse(&self, path: &'static str, reason: &str) {
        self.inner.refused.lock().unwrap().push((path, reason.to_string()));
    }

    fn refusal(&self, path: &str) -> Option<String> {
        self.inner.refused.lock().unwrap().iter().find(|(p, _)| *p == path).map(|(_, r)| r.clone())
    }

    /// This backend can move its devices between `max_boards` boards while serving
    /// (`POST /api/boards`); without this call the endpoint answers 501.
    pub fn allow_reassign(&self, max_boards: u8) {
        *self.inner.reassign.lock().unwrap() = Some(max_boards);
    }

    fn boards_json(&self) -> serde_json::Value {
        let setup = self.setup();
        let max = *self.inner.reassign.lock().unwrap();
        serde_json::json!({ "backend": setup.backend, "reassign": max.is_some(), "max_boards": max, "boards": setup.boards, "modules": setup.modules })
    }

    /// Check a reassignment against what is connected and queue it. Err = (status, why).
    fn request_boards(&self, cmd: Command) -> Result<(), (u16, String)> {
        let setup = self.setup();
        let Some(max) = *self.inner.reassign.lock().unwrap() else {
            return Err((501, format!("the {} backend has a fixed board order; it cannot be changed while serving", setup.backend)));
        };
        if let Command::Boards { assign, .. } = &cmd {
            let current: Vec<(String, u8)> = setup.modules.iter().map(|m| (m.serial.clone(), m.board)).collect();
            reassigned(&current, assign, max).map_err(|e| (400, e))?;
        }
        if self.queue(cmd) { Ok(()) } else { Err((503, "serve loop has stopped".into())) }
    }

    /// Layout files next to `file` (same extension) become switchable.
    pub fn set_layout_file(&self, file: &std::path::Path) {
        let mut l = self.inner.layouts.lock().unwrap();
        l.dir = file.parent().map(|p| if p.as_os_str().is_empty() { std::path::PathBuf::from(".") } else { p.to_path_buf() });
        l.extension = file.extension().and_then(|e| e.to_str()).unwrap_or("").to_string();
    }

    /// The serve loop finished a switch: `None` = done (the new setup is published).
    pub fn layout_result(&self, error: Option<String>) {
        let mut l = self.inner.layouts.lock().unwrap();
        l.switching = false;
        l.last_error = error;
    }

    /// Names (file stems) and tunings of the layouts that can be loaded.
    pub fn available_layouts(&self) -> Vec<(String, Option<i32>)> {
        let l = self.inner.layouts.lock().unwrap();
        let Some(dir) = l.dir.as_ref() else { return Vec::new() };
        let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
        let mut out: Vec<(String, Option<i32>)> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().and_then(|e| e.to_str()) == Some(l.extension.as_str()))
            .filter_map(|p| {
                let name = p.file_stem()?.to_str()?.to_string();
                let edo = std::fs::read_to_string(&p).ok().and_then(|text| {
                    text.lines().find_map(|line| line.trim().strip_prefix("Edo=").and_then(|v| v.trim().parse::<i32>().ok()))
                });
                Some((name, edo))
            })
            .collect();
        out.sort();
        out
    }

    fn layouts_json(&self) -> serde_json::Value {
        let available: Vec<serde_json::Value> =
            self.available_layouts().into_iter().map(|(name, edo)| serde_json::json!({ "name": name, "edo": edo })).collect();
        let l = self.inner.layouts.lock().unwrap();
        serde_json::json!({ "current": self.setup().layout, "available": available, "switching": l.switching, "last_error": l.last_error })
    }

    /// Queue a switch to the layout `name`; Err = why not.
    fn request_layout(&self, name: &str) -> Result<(), String> {
        let names: Vec<String> = self.available_layouts().into_iter().map(|(n, _)| n).collect();
        if !names.iter().any(|n| n == name) {
            return Err(format!("no layout '{name}'. Available: {}", names.join(", ")));
        }
        {
            let mut l = self.inner.layouts.lock().unwrap();
            l.switching = true;
            l.last_error = None;
        }
        if self.queue(Command::Layout { name: name.to_string() }) {
            Ok(())
        } else {
            self.layout_result(Some("serve loop has stopped".into()));
            Err("serve loop has stopped".into())
        }
    }

    pub fn setup(&self) -> Arc<Setup> {
        self.inner.setup.lock().unwrap().clone()
    }

    /// Modules came or went: replace the description and tell stream clients.
    pub fn set_setup(&self, setup: Setup) {
        let boards = setup.boards.clone();
        *self.inner.setup.lock().unwrap() = Arc::new(setup);
        self.inner.held.lock().unwrap().retain(|(b, _)| boards.contains(b));
        self.broadcast("setup", &serde_json::json!({ "boards": boards }));
    }

    pub fn publish_key(&self, ev: &KeyEvent) {
        {
            let mut held = self.inner.held.lock().unwrap();
            match ev.kind {
                KeyKind::On => held.insert((ev.board, ev.key)),
                KeyKind::Off => held.remove(&(ev.board, ev.key)),
            };
        }
        self.broadcast("key", ev);
    }

    pub fn held(&self) -> Vec<(u8, u8)> {
        self.inner.held.lock().unwrap().iter().copied().collect()
    }

    fn broadcast<T: Serialize>(&self, event: &str, data: &T) {
        let Ok(serde_json::Value::Object(mut obj)) = serde_json::to_value(data) else { return };
        let mut log = self.inner.log.lock().unwrap();
        log.last_seq += 1;
        obj.insert("seq".into(), log.last_seq.into());
        obj.insert("type".into(), event.into());
        let seq = log.last_seq;
        log.events.push_back((seq, serde_json::Value::Object(obj)));
        if log.events.len() > EVENT_LOG {
            log.events.pop_front();
        }
        drop(log);
        self.inner.log_changed.notify_all();
    }

    /// Events numbered above `after`, waiting up to `wait` for the first one. `after` None:
    /// no events, only the current number.
    pub fn events(&self, after: Option<u64>, wait: Duration) -> Events {
        let mut log = self.inner.log.lock().unwrap();
        let Some(after) = after else {
            return Events { next: log.last_seq, missed: false, events: Vec::new() };
        };
        let deadline = Instant::now() + wait.min(MAX_WAIT);
        while log.last_seq <= after {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            log = self.inner.log_changed.wait_timeout(log, left).unwrap().0;
        }
        // `after` beyond the log's end: the client's number is from an earlier run.
        let after = after.min(log.last_seq);
        let events: Vec<serde_json::Value> = log.events.iter().filter(|(seq, _)| *seq > after).map(|(_, e)| e.clone()).collect();
        let first_kept = log.events.front().map(|(seq, _)| *seq).unwrap_or(log.last_seq + 1);
        Events { next: log.last_seq, missed: after + 1 < first_kept, events }
    }

    pub(crate) fn queue(&self, c: Command) -> bool {
        self.inner.commands.send(c).is_ok()
    }
}

// --- serve-loop side: state both backends keep for the API ---

/// Where the serve loop's MIDI goes (each backend's own output; a list in tests).
pub trait MidiSink {
    fn send3(&mut self, msg: [u8; 3]);
}

impl MidiSink for Vec<[u8; 3]> {
    fn send3(&mut self, msg: [u8; 3]) {
        self.push(msg);
    }
}

/// Notes sounded for the API, with the time at which xentool releases them.
#[derive(Default)]
pub struct TimedNotes {
    /// Layout keys: (board, key) -> (channel 0..15, note, when to release).
    keyed: HashMap<(u8, u8), (u8, u8, Option<Instant>)>,
    /// Raw notes (clicks, percussion): (channel 0..15, note, when to release).
    raw: Vec<(u8, u8, Instant)>,
}

impl TimedNotes {
    /// Sound `key` as (channel, note); a note still sounding on that key is released first.
    #[allow(clippy::too_many_arguments)]
    pub fn key_on(&mut self, key: (u8, u8), ch: u8, note: u8, velocity: u8, ms: Option<u32>, now: Instant, midi: &mut impl MidiSink) {
        self.key_off(key, midi);
        let (ch, note) = (ch & 0x0F, note & 0x7F);
        midi.send3([0x90 | ch, note, velocity & 0x7F]);
        self.keyed.insert(key, (ch, note, ms.map(|ms| now + Duration::from_millis(ms as u64))));
    }

    pub fn key_off(&mut self, key: (u8, u8), midi: &mut impl MidiSink) {
        if let Some((ch, note, _)) = self.keyed.remove(&key) {
            midi.send3([0x80 | ch, note, 0]);
        }
    }

    /// A note outside the layout, on the channel the client chose.
    pub fn raw_on(&mut self, n: &MidiNote, now: Instant, midi: &mut impl MidiSink) {
        let ch = n.channel.saturating_sub(1).min(15);
        midi.send3([0x90 | ch, n.note & 0x7F, n.velocity & 0x7F]);
        self.raw.push((ch, n.note & 0x7F, now + Duration::from_millis(n.ms as u64)));
    }

    /// Release the notes whose length has passed.
    pub fn tick(&mut self, now: Instant, midi: &mut impl MidiSink) {
        if !self.raw.is_empty() {
            self.raw.retain(|&(ch, note, until)| {
                if now >= until {
                    midi.send3([0x80 | ch, note, 0]);
                }
                now < until
            });
        }
        if self.keyed.is_empty() {
            return;
        }
        let due: Vec<(u8, u8)> = self.keyed.iter().filter(|(_, v)| v.2.is_some_and(|t| now >= t)).map(|(k, _)| *k).collect();
        for key in due {
            self.key_off(key, midi);
        }
    }

    /// True while a layout key sounds on `ch` (0..15). For backends that give every API
    /// note a channel of its own (MPE).
    pub fn uses_channel(&self, ch: u8) -> bool {
        self.keyed.values().any(|v| v.0 == ch)
    }

    /// Release the layout keys sounding on `ch`.
    pub fn release_channel(&mut self, ch: u8, midi: &mut impl MidiSink) {
        let keys: Vec<(u8, u8)> = self.keyed.iter().filter(|(_, v)| v.0 == ch).map(|(k, _)| *k).collect();
        for key in keys {
            self.key_off(key, midi);
        }
    }

    pub fn release_all(&mut self, midi: &mut impl MidiSink) {
        for (ch, note, _) in self.raw.drain(..) {
            midi.send3([0x80 | ch, note, 0]);
        }
        for (_, (ch, note, _)) in self.keyed.drain() {
            midi.send3([0x80 | ch, note, 0]);
        }
    }
}

/// The overlay as the API last asked for it, by key address. The serve loop keeps it so
/// that a board that reconnects (or whose LED mapping changes) gets its part back.
#[derive(Default)]
pub struct OverlayState {
    pub keys: HashMap<(u8, u8), Rgb>,
    /// Brightness of the layout colours while an overlay is shown; stays once given.
    pub dim_base: Option<f32>,
}

impl OverlayState {
    /// First `clear`, then `set` (keys for which `valid` is false are ignored).
    pub fn apply(&mut self, clear: &OverlayClear, set: &[(u8, u8, Rgb)], dim_base: Option<f32>, valid: impl Fn(u8, u8) -> bool) {
        match clear {
            OverlayClear::None => {}
            OverlayClear::All => self.keys.clear(),
            OverlayClear::Keys(keys) => {
                for k in keys {
                    self.keys.remove(k);
                }
            }
        }
        for &(b, k, rgb) in set {
            if valid(b, k) {
                self.keys.insert((b, k), rgb);
            }
        }
        if dim_base.is_some() {
            self.dim_base = dim_base;
        }
    }

    pub fn is_active(&self) -> bool {
        !self.keys.is_empty()
    }
}

// --- HTTP ---

/// Start the API server on a background thread. Returns the bound port once the listener
/// is up (`port` 0 = any free port).
pub fn spawn(hub: ApiHub, port: u16) -> Result<u16> {
    let addr = format!("127.0.0.1:{port}");
    let server = rouille::Server::new(addr.clone(), move |request| handle(request, &hub))
        .map_err(|e| anyhow::anyhow!("API bind failed on {addr}: {e}"))
        .with_context(|| "API server")?;
    let bound = server.server_addr().port();
    thread::Builder::new().name("api-server".into()).spawn(move || server.run()).context("spawning API server thread")?;
    Ok(bound)
}

fn bad_request(msg: &str) -> Response {
    Response::json(&serde_json::json!({ "error": msg })).with_status_code(400)
}

fn body_of(request: &rouille::Request) -> Result<Vec<u8>, Response> {
    let mut buf = Vec::new();
    match request.data() {
        Some(mut d) => {
            if d.by_ref().take(1 << 20).read_to_end(&mut buf).is_err() {
                return Err(bad_request("could not read the request body"));
            }
        }
        None => return Err(bad_request("request body already taken")),
    }
    Ok(buf)
}

fn handle(request: &rouille::Request, hub: &ApiHub) -> Response {
    if let Some(reason) = hub.refusal(&request.url()) {
        return Response::json(&serde_json::json!({ "error": reason })).with_status_code(501);
    }
    match (request.method(), request.url().as_str()) {
        ("GET", "/api/setup") => Response::json(&*hub.setup()),
        ("GET", "/api/keys/state") => Response::json(&serde_json::json!({ "held": hub.held() })),
        ("GET", "/api/events") => {
            let after = match request.get_param("after") {
                None => None,
                Some(a) => match a.parse::<u64>() {
                    Ok(n) => Some(n),
                    Err(_) => return bad_request("after: expected a sequence number"),
                },
            };
            let wait_ms = request.get_param("wait_ms").and_then(|w| w.parse::<u64>().ok()).unwrap_or(0);
            Response::json(&hub.events(after, Duration::from_millis(wait_ms)))
        }
        ("GET", "/api/layouts") => Response::json(&hub.layouts_json()),
        ("POST", "/api/layout") => {
            let body = match body_of(request) {
                Ok(b) => b,
                Err(r) => return r,
            };
            let name = match serde_json::from_slice::<serde_json::Value>(&body) {
                Ok(v) => v.get("name").and_then(|n| n.as_str()).map(|s| s.to_string()),
                Err(e) => return bad_request(&e.to_string()),
            };
            match name {
                None => bad_request("expected {\"name\": \"<layout>\"}"),
                Some(name) => match hub.request_layout(&name) {
                    Ok(()) => Response::json(&serde_json::json!({ "ok": true, "switching_to": name })),
                    Err(e) => bad_request(&e),
                },
            }
        }
        ("GET", "/api/boards") => Response::json(&hub.boards_json()),
        ("POST", "/api/boards") => {
            let body = match body_of(request) {
                Ok(b) => b,
                Err(r) => return r,
            };
            match parse_boards(&body).map_err(|e| (400, e)).and_then(|cmd| hub.request_boards(cmd)) {
                Ok(()) => Response::json(&serde_json::json!({ "ok": true })),
                Err((status, e)) => Response::json(&serde_json::json!({ "error": e })).with_status_code(status),
            }
        }
        ("POST", "/api/leds/overlay") => command_response(request, hub, parse_overlay),
        ("POST", "/api/notes") => command_response(request, hub, parse_notes),
        ("POST", "/api/midi") => command_response(request, hub, parse_midi),
        ("POST", "/api/chords/name") => {
            let body = match body_of(request) {
                Ok(b) => b,
                Err(r) => return r,
            };
            match serde_json::from_slice::<ChordBody>(&body) {
                Ok(b) if b.edo >= 1 => Response::json(&chord_names_json(b.edo, &b.pitch_classes)),
                Ok(_) => bad_request("edo must be >= 1"),
                Err(e) => bad_request(&e.to_string()),
            }
        }
        _ => Response::empty_404(),
    }
}

fn command_response(request: &rouille::Request, hub: &ApiHub, parse: fn(&[u8]) -> Result<Command, String>) -> Response {
    let body = match body_of(request) {
        Ok(b) => b,
        Err(r) => return r,
    };
    match parse(&body) {
        Ok(cmd) => {
            if hub.queue(cmd) {
                Response::json(&serde_json::json!({ "ok": true }))
            } else {
                Response::json(&serde_json::json!({ "error": "serve loop has stopped" })).with_status_code(503)
            }
        }
        Err(e) => bad_request(&e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geometry::geometry_by_name;

    fn cells(boards: &[u8]) -> Vec<LayoutCell> {
        let mut v = Vec::new();
        for &b in boards {
            for k in 0u8..56 {
                v.push(LayoutCell { board: b, key: k, pitch: b as i32 * 31 + k as i32, color: (k, 0, b) });
            }
        }
        v
    }

    #[test]
    fn setup_has_only_connected_boards_in_one_lattice() {
        let g = geometry_by_name("lumatone", 1).unwrap();
        let one = build_setup("openxen", &g, "edo31", 31, &cells(&[0, 1, 2, 3, 4]), &[1]);
        assert_eq!(one.boards, vec![1]);
        assert_eq!(one.keys.len(), 56);
        // board 1 key 0 sits at the shifted lattice position, not at the board-local one
        assert_eq!((one.keys[0].board, one.keys[0].key, one.keys[0].x, one.keys[0].y), (1, 0, 2, 12));
        assert_eq!(one.keys[0].color, "000001");
        let all = build_setup("openxen", &g, "edo31", 31, &cells(&[0, 1, 2, 3, 4]), &[4, 0, 2, 1, 3]);
        assert_eq!(all.boards, vec![0, 1, 2, 3, 4]);
        assert_eq!(all.keys.len(), 280);
        // every key has its own lattice position
        let mut pos: Vec<(i32, i32)> = all.keys.iter().map(|k| (k.x, k.y)).collect();
        pos.sort_unstable();
        pos.dedup();
        assert_eq!(pos.len(), 280);
        // keys the layout leaves out are not listed
        let part = build_setup("openxen", &g, "edo31", 31, &cells(&[0])[..10], &[0]);
        assert_eq!(part.keys.len(), 10);
    }

    #[test]
    fn directions_follow_the_orientation() {
        let down = Directions::of(Orientation::YRightXDown);
        assert_eq!((down.right, down.upper_right, down.lower_left), ((0, 2), (-1, 1), (1, -1)));
        let up = Directions::of(Orientation::YRightXUp);
        assert_eq!((up.left, up.upper_left, up.lower_right), ((0, -2), (1, -1), (-1, 1)));
        let json = serde_json::to_value(&down).unwrap();
        assert_eq!(json["upper_left"], serde_json::json!([-1, -1]));
    }

    #[test]
    fn overlay_body_is_parsed_and_checked() {
        let c = parse_overlay(br##"{"clear":"all","set":[{"board":1,"key":7,"rgb":"#ff8000"}],"dim_base":0.3}"##).unwrap();
        assert_eq!(c, Command::Overlay { clear: OverlayClear::All, set: vec![(1, 7, (255, 128, 0))], dim_base: Some(0.3) });
        let c = parse_overlay(br#"{"clear":[[0,3],[2,55]]}"#).unwrap();
        assert_eq!(c, Command::Overlay { clear: OverlayClear::Keys(vec![(0, 3), (2, 55)]), set: vec![], dim_base: None });
        assert!(parse_overlay(br#"{"clear":"some"}"#).is_err());
        assert!(parse_overlay(br#"{"set":[{"board":0,"key":0,"rgb":"red"}]}"#).is_err());
        assert!(parse_overlay(br#"{"dim_base":2}"#).is_err());
        assert!(parse_overlay(b"not json").is_err());
    }

    #[test]
    fn notes_body_is_parsed() {
        let c = parse_notes(br#"{"on":[{"board":0,"key":5,"ms":400},{"board":0,"key":6,"velocity":0}],"off":[[0,9]]}"#).unwrap();
        assert_eq!(
            c,
            Command::Notes {
                on: vec![
                    NoteOn { board: 0, key: 5, velocity: 80, ms: Some(400) },
                    NoteOn { board: 0, key: 6, velocity: 1, ms: None },
                ],
                off: vec![(0, 9)],
            }
        );
    }

    #[test]
    fn hub_tracks_held_keys_and_numbers_events() {
        let g = geometry_by_name("lumatone", 1).unwrap();
        let (hub, _rx) = ApiHub::new(build_setup("openxen", &g, "x", 31, &cells(&[0, 1]), &[0, 1]));
        let none = Duration::ZERO;
        assert_eq!(hub.events(None, none), Events { next: 0, missed: false, events: vec![] });
        let on = |board, key| KeyEvent { t_us: 5, board, key, kind: KeyKind::On, velocity: 90, pitch: 40 };
        hub.publish_key(&on(0, 3));
        hub.publish_key(&on(1, 4));
        assert_eq!(hub.held(), vec![(0, 3), (1, 4)]);
        let e = hub.events(Some(0), none);
        assert_eq!((e.next, e.missed, e.events.len()), (2, false, 2));
        assert_eq!(e.events[0], serde_json::json!({"seq":1,"type":"key","t_us":5,"board":0,"key":3,"kind":"on","velocity":90,"pitch":40}));
        // only what is new since the number the client has
        assert_eq!(hub.events(Some(1), none).events.len(), 1);
        assert!(hub.events(Some(2), none).events.is_empty());
        // "from now": no old events, just the number to continue from
        assert_eq!(hub.events(None, none), Events { next: 2, missed: false, events: vec![] });
        hub.publish_key(&KeyEvent { kind: KeyKind::Off, velocity: 0, ..on(0, 3) });
        assert_eq!(hub.held(), vec![(1, 4)]);
        // board 1 unplugged: its held key is forgotten and clients hear about the change
        hub.set_setup(build_setup("openxen", &g, "x", 31, &cells(&[0, 1]), &[0]));
        assert!(hub.held().is_empty());
        let e = hub.events(Some(3), none);
        assert_eq!(e.events, vec![serde_json::json!({"seq":4,"type":"setup","boards":[0]})]);
        // a number from an earlier run of xentool is treated as "nothing new"
        assert_eq!(hub.events(Some(999), none), Events { next: 4, missed: false, events: vec![] });
    }

    #[test]
    fn a_waiting_request_returns_as_soon_as_a_key_is_pressed() {
        let g = geometry_by_name("lumatone", 1).unwrap();
        let (hub, _rx) = ApiHub::new(build_setup("openxen", &g, "x", 31, &cells(&[0]), &[0]));
        // nothing happens: the wait runs out
        let t = Instant::now();
        assert!(hub.events(Some(0), Duration::from_millis(60)).events.is_empty());
        assert!(t.elapsed() >= Duration::from_millis(55));
        // a press 50 ms into a 10 s wait ends it
        let h2 = hub.clone();
        let presser = thread::spawn(move || {
            thread::sleep(Duration::from_millis(50));
            h2.publish_key(&KeyEvent { t_us: 1, board: 0, key: 9, kind: KeyKind::On, velocity: 64, pitch: 100 });
        });
        let t = Instant::now();
        let e = hub.events(Some(0), Duration::from_secs(10));
        assert_eq!(e.events.len(), 1);
        assert!(t.elapsed() < Duration::from_secs(2), "waited {:?}", t.elapsed());
        presser.join().unwrap();
    }

    #[test]
    fn a_client_too_far_behind_is_told_it_missed_events() {
        let g = geometry_by_name("lumatone", 1).unwrap();
        let (hub, _rx) = ApiHub::new(build_setup("openxen", &g, "x", 31, &cells(&[0]), &[0]));
        for i in 0..(EVENT_LOG as u64 + 10) {
            hub.publish_key(&KeyEvent { t_us: i, board: 0, key: 1, kind: KeyKind::On, velocity: 1, pitch: 0 });
        }
        let e = hub.events(Some(5), Duration::ZERO);
        assert!(e.missed);
        assert_eq!(e.events.len(), EVENT_LOG);
        assert!(!hub.events(Some(10), Duration::ZERO).missed);
    }

    #[test]
    fn http_round_trip() {
        let g = geometry_by_name("lumatone", 1).unwrap();
        let (hub, commands) = ApiHub::new(build_setup("openxen", &g, "edo31", 31, &cells(&[0]), &[0]));
        let port = spawn(hub.clone(), 0).unwrap();
        let url = |p: &str| format!("http://127.0.0.1:{port}{p}");
        let get = |p: &str| -> serde_json::Value { ureq::get(&url(p)).call().unwrap().into_json().unwrap() };

        let setup = get("/api/setup");
        assert_eq!(setup["geometry"], "lumatone");
        assert_eq!(setup["edo"], 31);
        assert_eq!(setup["keys"].as_array().unwrap().len(), 56);
        assert_eq!(setup["directions"]["right"], serde_json::json!([0, 2]));

        let r = ureq::post(&url("/api/leds/overlay")).send_string(r#"{"set":[{"board":0,"key":2,"rgb":"00FF00"}]}"#).unwrap();
        assert_eq!(r.status(), 200);
        assert_eq!(commands.try_recv().unwrap(), Command::Overlay { clear: OverlayClear::None, set: vec![(0, 2, (0, 255, 0))], dim_base: None });
        let bad = ureq::post(&url("/api/leds/overlay")).send_string(r#"{"set":[{"board":0,"key":2,"rgb":"green"}]}"#);
        assert!(matches!(bad, Err(ureq::Error::Status(400, _))));
        assert!(matches!(ureq::get(&url("/api/nope")).call(), Err(ureq::Error::Status(404, _))));
        assert!(matches!(ureq::get(&url("/api/events?after=x")).call(), Err(ureq::Error::Status(400, _))));

        assert_eq!(get("/api/events")["next"], 0);
        hub.publish_key(&KeyEvent { t_us: 1, board: 0, key: 9, kind: KeyKind::On, velocity: 64, pitch: 100 });
        let e = get("/api/events?after=0&wait_ms=2000");
        assert_eq!(e["next"], 1);
        assert_eq!((e["events"][0]["key"].as_u64(), e["events"][0]["kind"].as_str()), (Some(9), Some("on")));
        assert_eq!(get("/api/keys/state")["held"], serde_json::json!([[0, 9]]));

        let names: serde_json::Value = ureq::post(&url("/api/chords/name")).send_string(r#"{"edo":12,"pitch_classes":[0,4,7]}"#).unwrap().into_json().unwrap();
        assert!(names["chords"].as_array().unwrap().iter().any(|c| c["root_pc"] == 0 && c["best"] == "Major Triad \"maj\""), "{names}");
    }

    #[test]
    fn setup_names_the_module_behind_each_board() {
        let g = geometry_by_name("lumatone", 1).unwrap();
        let mut setup = build_setup("openxen", &g, "x", 31, &cells(&[0, 1]), &[0, 1]);
        assert!(setup.modules.is_empty());
        assert_eq!(serde_json::to_value(&setup).unwrap()["modules"], serde_json::json!([]));
        setup.modules = vec![
            SetupModule { board: 0, serial: "AA".into(), pinned: true },
            SetupModule { board: 1, serial: "BB".into(), pinned: false },
        ];
        let (hub, _rx) = ApiHub::new(setup);
        let port = spawn(hub.clone(), 0).unwrap();
        let v: serde_json::Value = ureq::get(&format!("http://127.0.0.1:{port}/api/setup")).call().unwrap().into_json().unwrap();
        assert_eq!(v["modules"], serde_json::json!([{"board":0,"serial":"AA","pinned":true},{"board":1,"serial":"BB","pinned":false}]));
    }

    #[test]
    fn reassignment_is_checked_in_one_place() {
        let cur = vec![("A".to_string(), 0u8), ("B".to_string(), 1), ("C".to_string(), 3)];
        let a = |v: &[(&str, u8)]| v.iter().map(|(s, b)| (s.to_string(), *b)).collect::<Vec<_>>();
        // a swap; C is not named and keeps its board
        assert_eq!(reassigned(&cur, &a(&[("A", 1), ("B", 0)]), 5).unwrap(), a(&[("A", 1), ("B", 0), ("C", 3)]));
        // onto a free board
        assert_eq!(reassigned(&cur, &a(&[("C", 4)]), 5).unwrap(), a(&[("A", 0), ("B", 1), ("C", 4)]));
        assert_eq!(reassigned(&cur, &[], 5).unwrap(), cur);
        assert!(reassigned(&cur, &a(&[("A", 1)]), 5).unwrap_err().contains("two modules"));
        assert!(reassigned(&cur, &a(&[("A", 5)]), 5).unwrap_err().contains("does not exist"));
        assert!(reassigned(&cur, &a(&[("Z", 2)]), 5).unwrap_err().contains("no connected module"));
        assert!(reassigned(&cur, &a(&[("A", 1), ("A", 2)]), 5).unwrap_err().contains("twice"));
    }

    #[test]
    fn boards_endpoint_lists_modules_and_queues_a_checked_reassignment() {
        let g = geometry_by_name("lumatone", 1).unwrap();
        let mut setup = build_setup("openxen", &g, "x", 31, &cells(&[0, 1]), &[0, 1]);
        setup.modules = vec![
            SetupModule { board: 0, serial: "AA".into(), pinned: true },
            SetupModule { board: 1, serial: "BB".into(), pinned: false },
        ];
        let (hub, commands) = ApiHub::new(setup);
        let port = spawn(hub.clone(), 0).unwrap();
        let url = format!("http://127.0.0.1:{port}/api/boards");
        let status = |body: &str| match ureq::post(&url).send_string(body) {
            Ok(r) => r.status(),
            Err(ureq::Error::Status(s, _)) => s,
            Err(e) => panic!("{e}"),
        };
        // a backend that has not said it can reassign: 501, nothing queued
        let v: serde_json::Value = ureq::get(&url).call().unwrap().into_json().unwrap();
        assert_eq!((v["reassign"].as_bool(), v["max_boards"].is_null()), (Some(false), true));
        assert_eq!(status(r#"{"assign":[{"serial":"AA","board":1},{"serial":"BB","board":0}]}"#), 501);
        assert!(commands.try_recv().is_err());

        hub.allow_reassign(5);
        let v: serde_json::Value = ureq::get(&url).call().unwrap().into_json().unwrap();
        assert_eq!((v["reassign"].as_bool(), v["max_boards"].as_u64(), v["backend"].as_str()), (Some(true), Some(5), Some("openxen")));
        assert_eq!(v["modules"][1], serde_json::json!({"board":1,"serial":"BB","pinned":false}));
        // refused: collision, unknown serial, board out of range, not JSON
        for bad in [r#"{"assign":[{"serial":"AA","board":1}]}"#, r#"{"assign":[{"serial":"ZZ","board":2}]}"#, r#"{"assign":[{"serial":"AA","board":7}]}"#, "nope"] {
            assert_eq!(status(bad), 400, "{bad}");
        }
        assert!(commands.try_recv().is_err());
        assert_eq!(status(r#"{"assign":[{"serial":"AA","board":1},{"serial":"BB","board":0}],"pins":[{"serial":"AA","board":1}]}"#), 200);
        assert_eq!(
            commands.try_recv().unwrap(),
            Command::Boards { assign: vec![("AA".into(), 1), ("BB".into(), 0)], pins: Some(vec![("AA".into(), 1)]) }
        );
        // pins alone (unpin): nothing moves
        assert_eq!(status(r#"{"pins":[]}"#), 200);
        assert_eq!(commands.try_recv().unwrap(), Command::Boards { assign: vec![], pins: Some(vec![]) });
    }

    #[test]
    fn midi_notes_are_checked_and_queued() {
        assert_eq!(
            parse_midi(br#"{"notes":[{"channel":10,"note":76,"velocity":200,"ms":40},{"channel":10,"note":77}]}"#).unwrap(),
            Command::Midi { notes: vec![
                MidiNote { channel: 10, note: 76, velocity: 127, ms: 40 },
                MidiNote { channel: 10, note: 77, velocity: 80, ms: 60 },
            ] }
        );
        assert!(parse_midi(br#"{"notes":[{"channel":0,"note":76}]}"#).unwrap_err().contains("channel"));
        assert!(parse_midi(br#"{"notes":[{"channel":17,"note":76}]}"#).is_err());
        assert!(parse_midi(br#"{"notes":[{"channel":10,"note":200}]}"#).unwrap_err().contains("note"));
        assert!(parse_midi(br#"{}"#).is_err());
    }

    #[test]
    fn layouts_are_listed_and_a_switch_is_queued() {
        let dir = std::env::temp_dir().join(format!("xentool-api-layouts-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("31ET-bosanquet.otn"), "Edo=31\n[Board0]\n").unwrap();
        std::fs::write(dir.join("53ET-bosanquet.otn"), "PitchOffset=0\nEdo=53\n[Board0]\n").unwrap();
        std::fs::write(dir.join("notes.txt"), "not a layout").unwrap();
        let g = geometry_by_name("lumatone", 1).unwrap();
        let (hub, commands) = ApiHub::new(build_setup("openxen", &g, "31ET-bosanquet", 31, &cells(&[0]), &[0]));
        hub.set_layout_file(&dir.join("31ET-bosanquet.otn"));
        let port = spawn(hub.clone(), 0).unwrap();
        let url = |p: &str| format!("http://127.0.0.1:{port}{p}");
        let get = |p: &str| -> serde_json::Value { ureq::get(&url(p)).call().unwrap().into_json().unwrap() };

        let l = get("/api/layouts");
        assert_eq!(l["current"], "31ET-bosanquet");
        assert_eq!(l["available"], serde_json::json!([{"name": "31ET-bosanquet", "edo": 31}, {"name": "53ET-bosanquet", "edo": 53}]));
        assert_eq!((l["switching"].as_bool(), l["last_error"].is_null()), (Some(false), true));

        // an unknown name (or a path) is refused with the list of what exists
        for bad in [r#"{"name":"72ET"}"#, r#"{"name":"../53ET-bosanquet"}"#, r#"{}"#] {
            assert!(matches!(ureq::post(&url("/api/layout")).send_string(bad), Err(ureq::Error::Status(400, _))), "{bad}");
        }
        assert!(commands.try_recv().is_err());

        let r = ureq::post(&url("/api/layout")).send_string(r#"{"name":"53ET-bosanquet"}"#).unwrap();
        assert_eq!(r.status(), 200);
        assert_eq!(commands.try_recv().unwrap(), Command::Layout { name: "53ET-bosanquet".into() });
        assert_eq!(get("/api/layouts")["switching"], true);
        // the serve loop reports back: failure keeps the old layout and says why ...
        hub.layout_result(Some("parsing failed".into()));
        let l = get("/api/layouts");
        assert_eq!((l["switching"].as_bool(), l["last_error"].as_str(), l["current"].as_str()), (Some(false), Some("parsing failed"), Some("31ET-bosanquet")));
        // ... success publishes the new setup
        hub.set_setup(build_setup("openxen", &g, "53ET-bosanquet", 53, &cells(&[0]), &[0]));
        hub.layout_result(None);
        let l = get("/api/layouts");
        assert_eq!((l["current"].as_str(), l["last_error"].is_null()), (Some("53ET-bosanquet"), true));
        assert_eq!(get("/api/setup")["edo"], 53);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn timed_notes_release_themselves() {
        let t0 = Instant::now();
        let at = |ms: u64| t0 + Duration::from_millis(ms);
        let mut out: Vec<[u8; 3]> = Vec::new();
        let mut n = TimedNotes::default();
        n.key_on((0, 5), 2, 60, 90, Some(100), t0, &mut out);
        n.key_on((0, 6), 2, 62, 90, None, t0, &mut out);
        n.raw_on(&MidiNote { channel: 10, note: 76, velocity: 100, ms: 40 }, t0, &mut out);
        assert_eq!(out, vec![[0x92, 60, 90], [0x92, 62, 90], [0x99, 76, 100]]);
        out.clear();
        n.tick(at(39), &mut out);
        assert!(out.is_empty());
        n.tick(at(40), &mut out);
        assert_eq!(out, vec![[0x89, 76, 0]]);
        n.tick(at(100), &mut out);
        assert_eq!(out[1..], [[0x82, 60, 0]]);
        // the same key again: the sounding note is released before the new one starts
        out.clear();
        n.key_on((0, 6), 3, 64, 80, None, at(200), &mut out);
        assert_eq!(out, vec![[0x82, 62, 0], [0x93, 64, 80]]);
        out.clear();
        n.release_all(&mut out);
        assert_eq!(out, vec![[0x83, 64, 0]]);
        n.tick(at(10_000), &mut out);
        assert_eq!(out.len(), 1);
    }

    #[test]
    fn timed_notes_know_their_channels() {
        let t0 = Instant::now();
        let mut out: Vec<[u8; 3]> = Vec::new();
        let mut n = TimedNotes::default();
        assert!(!n.uses_channel(14));
        n.key_on((0, 5), 14, 60, 90, None, t0, &mut out);
        n.key_on((1, 5), 13, 61, 90, None, t0, &mut out);
        // a raw note is not a layout key
        n.raw_on(&MidiNote { channel: 10, note: 76, velocity: 100, ms: 40 }, t0, &mut out);
        assert!(n.uses_channel(14) && n.uses_channel(13) && !n.uses_channel(9));
        out.clear();
        n.release_channel(14, &mut out);
        assert_eq!(out, vec![[0x8E, 60, 0]]);
        assert!(!n.uses_channel(14) && n.uses_channel(13));
    }

    #[test]
    fn a_refused_endpoint_answers_501_with_the_reason() {
        let g = geometry_by_name("lumatone", 1).unwrap();
        let (hub, commands) = ApiHub::new(build_setup("exquis", &g, "x", 31, &cells(&[0]), &[0]));
        hub.refuse("/api/notes", "no MIDI output in this mode");
        let port = spawn(hub.clone(), 0).unwrap();
        let url = |p: &str| format!("http://127.0.0.1:{port}{p}");
        match ureq::post(&url("/api/notes")).send_string(r#"{"on":[{"board":0,"key":1}]}"#) {
            Err(ureq::Error::Status(501, r)) => {
                let v: serde_json::Value = r.into_json().unwrap();
                assert_eq!(v["error"], "no MIDI output in this mode");
            }
            other => panic!("expected 501, got {other:?}"),
        }
        assert!(commands.try_recv().is_err(), "nothing is queued");
        // the other endpoints work as usual
        assert_eq!(ureq::post(&url("/api/leds/overlay")).send_string(r#"{"clear":"all"}"#).unwrap().status(), 200);
        assert!(commands.try_recv().is_ok());
    }

    #[test]
    fn overlay_state_clears_then_sets_and_keeps_the_dim() {
        let mut o = OverlayState::default();
        o.apply(&OverlayClear::None, &[(0, 1, (1, 1, 1)), (1, 2, (2, 2, 2)), (0, 99, (3, 3, 3))], Some(0.3), |_, k| k < 56);
        assert_eq!(o.keys.len(), 2);
        o.apply(&OverlayClear::Keys(vec![(0, 1)]), &[], None, |_, _| true);
        assert_eq!((o.keys.len(), o.dim_base, o.is_active()), (1, Some(0.3), true));
        o.apply(&OverlayClear::All, &[(0, 3, (4, 4, 4))], None, |_, _| true);
        assert_eq!(o.keys.get(&(0, 3)), Some(&(4, 4, 4)));
        o.apply(&OverlayClear::All, &[], None, |_, _| true);
        assert!(!o.is_active());
    }

    #[test]
    fn chord_names_for_a_31_edo_major_triad() {
        let v = chord_names_json(31, &[0, 10, 18]);
        let chords = v["chords"].as_array().unwrap();
        let root0 = chords.iter().find(|c| c["root_pc"] == 0).expect("a name with root 0");
        // "~5c": the template is 5 cents away from the 31-EDO chord
        assert_eq!(root0["best"], "Major Triad~5c", "{root0}");
        assert_eq!(root0["rel"], serde_json::json!([0, 10, 18]));
    }
}
