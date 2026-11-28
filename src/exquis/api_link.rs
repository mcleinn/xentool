//! The Exquis serve loops' side of the local API (`serve --api`, `src/api.rs`).
//!
//! Key addresses are (board, key) with `board` = the number in the board's name
//! (`board3` = `[Board3]` of the `.xtn` file = 3) and `key` = the pad, 0..60. Boards keep
//! their pinned number (`src/config.rs`), so the connected ones can be 0, 1 and 3.
//!
//! Lattice: `geometry::exquis_board_tuples` places board N `N` strides of (-1, 11) from
//! board 0, so neighbouring boards continue each other's rows and all boards are one
//! keyfield. A board that is not connected leaves its place empty: with boards 0, 1 and 3
//! there is a gap of one board between 1 and 3 and no pad of 3 is a neighbour of a pad of
//! 1. Rows count away from the player (`Orientation::YRightXUp`).
//!
//! Pitch: what `TuningState::from_board` computes for the pad with the board's octave
//! shift (Up/Down buttons) as it is now, i.e. the pitch that sounds. Every shift and every
//! layout change publishes a new description with a `setup` event. Every pad of a board
//! that has a section in the layout is listed: a pad the file leaves out still sounds
//! (channel 1, key = pad number) and is listed black. A board without a section is
//! connected but silent; it is in `boards` and has no keys.
//!
//! `/api/notes` in pitch-bend mode: the output is an MPE stream in which every note needs
//! a channel of its own with the right bend. The link takes a member channel that no pad
//! of any board and no other API note is using (from channel 15 downwards), lets the
//! board's `TuningState` turn a pad note-on on that channel into bend + note exactly as
//! for a touch, and releases the note itself (`TimedNotes`). If a real touch arrives on
//! that channel while the API note sounds, the API note is released first.
//!
//! With `--mts-esp` xentool has no MIDI output (the synth listens to the Exquis itself):
//! `/api/notes` and `/api/midi` are refused with a 501.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Instant;

use anyhow::Result;
use crossbeam_channel::{Receiver, Sender};

use crate::api::{self, ApiHub, Command, KeyEvent, KeyKind, LayoutCell, MidiSink, OverlayState, Setup, TimedNotes};
use crate::config::BoardAssignment;
use crate::exquis::hud_ctx::BASE_OCTAVE_SHIFT;
use crate::exquis::overlay::{LedMsg, PADS, pad_addr};
use crate::exquis::proto::{Color, ColorCorrection};
use crate::exquis::tuning::TuningState;
use crate::wooting::overlay::OverlayFrame;
use crate::xtn::{BoardLayout, XtnLayout};

pub const BACKEND: &str = "exquis";
/// "Device number" with which the serve loops call their control callback for a layout
/// the API asked for (no button was pressed on any board).
pub const API_DEVICE: usize = usize::MAX;
/// MPE member channels (0-based) an API note may take: 2..15. Channel 1 is the master
/// channel, channel 16 carries the Exquis's buttons and encoders.
const NOTE_CHANNELS: std::ops::RangeInclusive<u8> = 1..=14;

const NO_MIDI_REASON: &str = "serve --mts-esp has no MIDI output of its own (the synth listens to the Exquis directly); run serve without --mts-esp to sound notes through the API";

pub type ApiHandle = Rc<RefCell<ApiLink>>;

impl MidiSink for crate::midi_out::MidiOutPort {
    fn send3(&mut self, msg: [u8; 3]) {
        let _ = self.send(&msg);
    }
}

/// For the MTS-ESP loop, which has no MIDI output.
pub struct NoMidi;

impl MidiSink for NoMidi {
    fn send3(&mut self, _msg: [u8; 3]) {}
}

/// A connected board as the API needs it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BoardInfo {
    /// The N of `boardN`.
    pub board: u8,
    /// `ExquisDevice::number`, which incoming MIDI messages carry.
    pub device: usize,
}

/// `board3` -> 3.
pub fn board_index(name: &str) -> Option<u8> {
    name.strip_prefix("board")?.parse().ok()
}

pub fn board_infos(boards: &[BoardAssignment]) -> Vec<BoardInfo> {
    boards.iter().filter_map(|b| Some(BoardInfo { board: board_index(&b.board_name)?, device: b.device.number })).collect()
}

fn section(layout: &XtnLayout, board: u8) -> Option<&BoardLayout> {
    layout.boards.get(&format!("board{board}"))
}

/// The pitch of every pad of a board in steps of the tuning, octave shift included: the
/// retuner's own numbers.
pub fn pad_pitches(bl: &BoardLayout, edo: i32, pitch_offset: i32, shift: i32) -> [i32; PADS] {
    // the bend range and the X gain do not enter the pitch
    let tuning = TuningState::from_board(bl, edo, pitch_offset, BASE_OCTAVE_SHIFT + shift, 2.0, 1.0);
    let mut out = [0; PADS];
    for (pad, p) in out.iter_mut().enumerate() {
        *p = tuning.pad_tuning(pad as u8).map(|t| t.abs_pitch).unwrap_or(0);
    }
    out
}

/// The layout colours of a board as the pads show them (device colours).
pub fn pad_colors(bl: &BoardLayout, correction: &ColorCorrection) -> [Color; PADS] {
    let mut out = [Color::new(0, 0, 0); PADS];
    for (pad, c) in out.iter_mut().enumerate() {
        *c = bl.pads.get(&(pad as u8)).map(|e| e.color).unwrap_or(Color::new(0, 0, 0)).corrected(correction);
    }
    out
}

/// The keyboard description. `boards`: (board, octave shift) of the connected boards.
pub fn build_setup(layout: &XtnLayout, edo: i32, name: &str, boards: &[(u8, i32)]) -> Setup {
    let mut cells = Vec::new();
    for &(board, shift) in boards {
        let Some(bl) = section(layout, board) else { continue };
        let pitches = pad_pitches(bl, edo, layout.pitch_offset, shift);
        for pad in 0..PADS as u8 {
            let color = bl.pads.get(&pad).map(|e| e.color.to_rgb8()).unwrap_or((0, 0, 0));
            cells.push(LayoutCell { board, key: pad, pitch: pitches[pad as usize], color });
        }
    }
    let present: Vec<u8> = boards.iter().map(|b| b.0).collect();
    let count = present.iter().copied().max().map(|m| m.saturating_add(1)).unwrap_or(1);
    let geometry = crate::geometry::geometry_by_name("exquis", count).expect("the exquis geometry exists");
    api::build_setup(BACKEND, &geometry, name, edo, &cells, &present)
}

/// Bend and note with which `tuning` sounds `pad` on channel `ch` (0..15): what its
/// retuning makes of a touch. The channel is free again afterwards.
fn sound_pad(tuning: &mut TuningState, ch: u8, pad: u8, velocity: u8) -> Option<([u8; 3], u8)> {
    let on = tuning.process_message(&[0x90 | ch, pad, velocity]);
    let _ = tuning.process_message(&[0x80 | ch, pad, 0]);
    tuning.last_retune_info = None;
    let bend: [u8; 3] = on.first()?.as_slice().try_into().ok()?;
    let note = on.get(1)?;
    if bend[0] != (0xE0 | ch) || note.len() != 3 || note[0] != (0x90 | ch) {
        return None;
    }
    Some((bend, note[1]))
}

struct LinkBoard {
    board: u8,
    device: usize,
    shift: i32,
    /// None: the layout has no section for this board (it is silent).
    pitches: Option<[i32; PADS]>,
    /// Pitch each pad was last pressed with, for its release event.
    on_pitch: [i32; PADS],
}

pub struct ApiLink {
    pub hub: ApiHub,
    rx: Receiver<Command>,
    leds: Sender<LedMsg>,
    correction: ColorCorrection,
    layout: XtnLayout,
    name: String,
    edo: i32,
    /// The layout file being served; the API switches to its neighbours.
    file: PathBuf,
    boards: Vec<LinkBoard>,
    started: Instant,
    overlay: OverlayState,
    notes: TimedNotes,
    layout_request: Option<PathBuf>,
}

impl ApiLink {
    /// `retune`: pitch-bend mode (xentool forwards the notes itself); false = MTS-ESP.
    #[allow(clippy::too_many_arguments)]
    pub fn new(layout: &XtnLayout, edo: i32, file: &Path, boards: &[BoardInfo], retune: bool, correction: ColorCorrection, leds: Sender<LedMsg>, started: Instant) -> Self {
        let name = crate::hud::layout_id_from_path(file);
        let shifts: Vec<(u8, i32)> = boards.iter().map(|b| (b.board, 0)).collect();
        let (hub, rx) = ApiHub::new(build_setup(layout, edo, &name, &shifts));
        hub.set_layout_file(file);
        if !retune {
            hub.refuse("/api/notes", NO_MIDI_REASON);
            hub.refuse("/api/midi", NO_MIDI_REASON);
        }
        let mut link = Self {
            hub,
            rx,
            leds,
            correction,
            layout: layout.clone(),
            name,
            edo,
            file: file.to_path_buf(),
            boards: boards.iter().map(|b| LinkBoard { board: b.board, device: b.device, shift: 0, pitches: None, on_pitch: [0; PADS] }).collect(),
            started,
            overlay: OverlayState::default(),
            notes: TimedNotes::default(),
            layout_request: None,
        };
        link.refresh_pitches();
        link
    }

    pub fn into_handle(self) -> ApiHandle {
        Rc::new(RefCell::new(self))
    }

    /// Start the HTTP server. Returns the bound port.
    pub fn listen(&self, port: u16) -> Result<u16> {
        api::spawn(self.hub.clone(), port)
    }

    fn refresh_pitches(&mut self) {
        for b in &mut self.boards {
            b.pitches = section(&self.layout, b.board).map(|bl| pad_pitches(bl, self.edo, self.layout.pitch_offset, b.shift));
        }
    }

    fn publish_setup(&mut self) {
        self.refresh_pitches();
        let shifts: Vec<(u8, i32)> = self.boards.iter().map(|b| (b.board, b.shift)).collect();
        self.hub.set_setup(build_setup(&self.layout, self.edo, &self.name, &shifts));
    }

    /// The Up/Down buttons changed the octave shift of the board on `device`: new pitches,
    /// announced with a `setup` event.
    pub fn set_shift(&mut self, device: usize, shift: i32) {
        let Some(b) = self.boards.iter_mut().find(|b| b.device == device) else { return };
        if b.shift != shift {
            b.shift = shift;
            self.publish_setup();
        }
    }

    /// Another layout was loaded (Settings button or API): all shifts are 0 again, and the
    /// overlay, which belonged to the old notes, is gone.
    pub fn set_layout(&mut self, layout: &XtnLayout, edo: i32, file: &Path) {
        self.layout = layout.clone();
        self.edo = edo;
        self.name = crate::hud::layout_id_from_path(file);
        self.file = file.to_path_buf();
        for b in &mut self.boards {
            b.shift = 0;
        }
        self.overlay.keys.clear();
        self.send_overlay();
        self.hub.set_layout_file(file);
        self.publish_setup();
        self.hub.layout_result(None);
    }

    /// A layout change failed; the old layout stays. Shown by `GET /api/layouts`.
    pub fn layout_failed(&self, error: String) {
        self.hub.layout_result(Some(error));
    }

    /// The file of the layout the API asked for, once.
    pub fn take_layout_request(&mut self) -> Option<PathBuf> {
        self.layout_request.take()
    }

    /// A MIDI message from the board on `device`. A pad's note-on or note-off becomes a
    /// key event; everything else returns at once. `midi`: the serve loop's output, used
    /// only to end an API note that sounds on the channel a new touch arrives on.
    pub fn midi_in(&mut self, device: usize, bytes: &[u8], midi: &mut impl MidiSink) {
        if bytes.len() < 3 {
            return;
        }
        let (status, ch, pad) = (bytes[0] & 0xF0, bytes[0] & 0x0F, bytes[1]);
        if (status != 0x90 && status != 0x80) || ch == 15 || pad as usize >= PADS {
            return;
        }
        let on = status == 0x90 && bytes[2] > 0;
        if on && self.notes.uses_channel(ch) {
            self.notes.release_channel(ch, midi);
        }
        let Some(b) = self.boards.iter_mut().find(|b| b.device == device) else { return };
        let Some(pitches) = &b.pitches else { return };
        let pitch = if on {
            b.on_pitch[pad as usize] = pitches[pad as usize];
            pitches[pad as usize]
        } else {
            b.on_pitch[pad as usize]
        };
        self.hub.publish_key(&KeyEvent {
            t_us: self.started.elapsed().as_micros() as u64,
            board: b.board,
            key: pad,
            kind: if on { KeyKind::On } else { KeyKind::Off },
            velocity: if on { bytes[2] } else { 0 },
            pitch,
        });
    }

    fn send_overlay(&self) {
        let mut keys: Vec<_> = self
            .overlay
            .keys
            .iter()
            .map(|(&(board, pad), &(r, g, b))| {
                let c = Color::from_rgb8(r, g, b).corrected(&self.correction);
                (pad_addr(board, pad), (c.red, c.green, c.blue))
            })
            .collect();
        keys.sort_unstable();
        let _ = self.leds.send(LedMsg::Overlay(OverlayFrame { keys, dim_base: self.overlay.dim_base.unwrap_or(1.0) }));
    }

    /// A member channel on which neither a pad nor an API note sounds; failing that, one
    /// with an API note only (which is released).
    fn free_channel(&mut self, tunings: &HashMap<usize, TuningState>, midi: &mut impl MidiSink) -> Option<u8> {
        let touched = |ch: u8| tunings.values().any(|t| t.channel_pad(ch).is_some());
        if let Some(ch) = NOTE_CHANNELS.rev().find(|&ch| !touched(ch) && !self.notes.uses_channel(ch)) {
            return Some(ch);
        }
        let ch = NOTE_CHANNELS.rev().find(|&ch| !touched(ch))?;
        self.notes.release_channel(ch, midi);
        Some(ch)
    }

    /// Apply queued commands. `tunings`: the retuners by device number (None with
    /// MTS-ESP). Returns true if a layout change is waiting (`take_layout_request`).
    pub fn apply_commands(&mut self, mut tunings: Option<&mut HashMap<usize, TuningState>>, now: Instant, midi: &mut impl MidiSink) -> bool {
        while let Ok(cmd) = self.rx.try_recv() {
            match cmd {
                Command::Overlay { clear, set, dim_base } => {
                    self.overlay.apply(&clear, &set, dim_base, |_, k| (k as usize) < PADS);
                    self.send_overlay();
                }
                Command::Layout { name } => {
                    let ext = self.file.extension().and_then(|e| e.to_str()).unwrap_or("xtn");
                    let file = format!("{name}.{ext}");
                    self.layout_request = Some(match self.file.parent() {
                        Some(dir) => dir.join(file),
                        None => PathBuf::from(file),
                    });
                }
                // refused by the hub (501): the board order of this backend is fixed
                Command::Boards { .. } => {}
                Command::Midi { notes } => {
                    if tunings.is_some() {
                        for n in &notes {
                            self.notes.raw_on(n, now, midi);
                        }
                    }
                }
                Command::Notes { on, off } => {
                    for key in off {
                        self.notes.key_off(key, midi);
                    }
                    let Some(tunings) = tunings.as_deref_mut() else { continue };
                    for n in on {
                        let key = (n.board, n.key);
                        self.notes.key_off(key, midi);
                        if n.key as usize >= PADS {
                            continue;
                        }
                        let Some(device) = self.boards.iter().find(|b| b.board == n.board).map(|b| b.device) else { continue };
                        if !tunings.contains_key(&device) {
                            continue;
                        }
                        let Some(ch) = self.free_channel(tunings, midi) else { continue };
                        let Some(tuning) = tunings.get_mut(&device) else { continue };
                        if let Some((bend, note)) = sound_pad(tuning, ch, n.key, n.velocity) {
                            midi.send3(bend);
                            self.notes.key_on(key, ch, note, n.velocity, n.ms, now, midi);
                        }
                    }
                }
            }
        }
        self.layout_request.is_some()
    }

    /// Release notes whose length has passed.
    pub fn tick_notes(&mut self, now: Instant, midi: &mut impl MidiSink) {
        self.notes.tick(now, midi);
    }

    pub fn release_all(&mut self, midi: &mut impl MidiSink) {
        self.notes.release_all(midi);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::{NoteOn, OverlayClear};
    use crate::exquis::overlay::LedState;
    use crate::xtn::parse_xtn_str;
    use std::collections::BTreeSet;
    use std::time::Duration;

    /// Boards 0, 1, 2, 3 in 31-EDO, pitch offset 3: pad p of board b plays key p on channel
    /// b + 1 (one octave per board), colour 8-bit (2p, 0, 2b) -- except pad 9 of board 1,
    /// which the file leaves out.
    fn layout() -> XtnLayout {
        let mut text = String::from("Edo=31\nPitchOffset=3\n");
        for b in 0..4 {
            text.push_str(&format!("[Board{b}]\n"));
            for p in 0..61 {
                if (b, p) != (1, 9) {
                    text.push_str(&format!("Key_{p}={p}\nChan_{p}={}\nCol_{p}={:02X}00{:02X}\n", b + 1, 2 * p, 2 * b));
                }
            }
        }
        parse_xtn_str(&text, "test").unwrap()
    }

    /// Boards with their device numbers: device = board + 11.
    fn infos(boards: &[u8]) -> Vec<BoardInfo> {
        boards.iter().map(|&b| BoardInfo { board: b, device: b as usize + 11 }).collect()
    }

    fn link(boards: &[u8], retune: bool) -> (ApiLink, Receiver<LedMsg>) {
        let (tx, rx) = crossbeam_channel::unbounded();
        let l = ApiLink::new(&layout(), 31, Path::new("xtn/edo31.xtn"), &infos(boards), retune, ColorCorrection::default(), tx, Instant::now());
        (l, rx)
    }

    fn tunings(boards: &[u8], shift: i32) -> HashMap<usize, TuningState> {
        let l = layout();
        boards
            .iter()
            .map(|&b| (b as usize + 11, TuningState::from_board(section(&l, b).unwrap(), 31, l.pitch_offset, BASE_OCTAVE_SHIFT + shift, 16.0, 1.0)))
            .collect()
    }

    /// Pitch of pad p of board b without shift: channel b + 1, key p, offset 3, two octaves up.
    fn pitch(b: i32, p: i32) -> i32 {
        b * 31 + p + 3 + 2 * 31
    }

    fn overlay_of(rx: &Receiver<LedMsg>) -> OverlayFrame {
        match rx.try_recv() {
            Ok(LedMsg::Overlay(f)) => f,
            _ => panic!("expected an overlay"),
        }
    }

    #[test]
    fn board_numbers_come_from_the_board_names() {
        assert_eq!((board_index("board0"), board_index("board3"), board_index("board12")), (Some(0), Some(3), Some(12)));
        assert_eq!((board_index("Board1"), board_index("board"), board_index("boardx")), (None, None, None));
    }

    #[test]
    fn setup_for_one_board() {
        let (l, _rx) = link(&[0], true);
        let s = l.hub.setup();
        assert_eq!((s.backend.as_str(), s.geometry.as_str(), s.layout.as_str(), s.edo), ("exquis", "exquis", "edo31", 31));
        assert_eq!(s.boards, vec![0]);
        assert_eq!(s.keys.len(), 61);
        // rows count away from the player: the upper neighbours are one row up
        assert_eq!((s.directions.upper_right, s.directions.lower_left, s.directions.right), ((1, 1), (-1, -1), (0, 2)));
        // pad 0 is the bottom left pad, pad 1 right of it, pad 6 its upper right neighbour
        let at = |k: u8| (s.keys[k as usize].x, s.keys[k as usize].y);
        assert_eq!((at(0), at(1), at(6), at(60)), ((0, 0), (0, 2), (1, 1), (10, 10)));
        let k5 = &s.keys[5];
        assert_eq!((k5.board, k5.key, k5.pitch, k5.color.as_str()), (0, 5, pitch(0, 5), "0A0000"));
        assert!((k5.hz - crate::mts::edo_freq_hz(31, pitch(0, 5))).abs() < 1e-9);
        let json = serde_json::to_value(&*s).unwrap();
        assert_eq!(json["keys"][5], serde_json::json!({"board":0,"key":5,"x":0,"y":10,"pitch":pitch(0, 5),"hz":k5.hz,"color":"0A0000"}));
        assert_eq!(json["directions"]["upper_left"], serde_json::json!([1, -1]));
    }

    #[test]
    fn setup_pitches_and_frequencies_are_the_retuners() {
        let (l, _rx) = link(&[0, 1], true);
        let s = l.hub.setup();
        let t = tunings(&[0, 1], 0);
        for k in &s.keys {
            let pt = t[&(k.board as usize + 11)].pad_tuning(k.key).unwrap();
            assert_eq!(k.pitch, pt.abs_pitch);
            assert!((k.hz - pt.freq_hz).abs() < 1e-9);
        }
        // a pad the file leaves out sounds as channel 1, key = pad, and is listed black
        let k = s.keys.iter().find(|k| (k.board, k.key) == (1, 9)).unwrap();
        assert_eq!((k.pitch, k.color.as_str()), (pitch(0, 9), "000000"));
    }

    #[test]
    fn boards_0_1_3_are_one_lattice_with_a_gap_for_the_missing_board() {
        let (l, _rx) = link(&[3, 0, 1], true);
        let s = l.hub.setup();
        assert_eq!(s.boards, vec![0, 1, 3]);
        assert_eq!(s.keys.len(), 183);
        assert!(s.keys.iter().all(|k| k.board != 2));
        // every pad has its own place, all on one sub-lattice
        let at: HashMap<(i32, i32), u8> = s.keys.iter().map(|k| ((k.x, k.y), k.board)).collect();
        assert_eq!(at.len(), 183);
        assert!(s.keys.iter().all(|k| (k.x + k.y).rem_euclid(2) == 0));
        // board N lies N strides of (-1, 11) from board 0, also when a board is missing
        let pos = |b: u8, p: u8| s.keys.iter().find(|k| (k.board, k.key) == (b, p)).map(|k| (k.x, k.y)).unwrap();
        assert_eq!((pos(0, 0), pos(1, 0), pos(3, 0)), ((0, 0), (-1, 11), (-3, 33)));
        // boards 0 and 1 touch (pad 5 of board 0 and pad 6 of board 1 are in one row),
        // board 3 touches neither
        assert_eq!((pos(0, 5), pos(1, 6)), ((0, 10), (0, 12)));
        let neighbours = |a: u8, b: u8| {
            at.iter().filter(|(_, bb)| **bb == a).any(|(&(x, y), _)| crate::geometry::HEX_NEIGHBOR_DELTAS.iter().any(|(dx, dy)| at.get(&(x + dx, y + dy)) == Some(&b)))
        };
        assert!(neighbours(0, 1));
        assert!(!neighbours(1, 3) && !neighbours(0, 3));
        // each board keeps the pitches of its own section
        let p = |b: u8, k: u8| s.keys.iter().find(|x| (x.board, x.key) == (b, k)).unwrap().pitch;
        assert_eq!((p(0, 7), p(1, 7), p(3, 7)), (pitch(0, 7), pitch(1, 7), pitch(3, 7)));
        assert_eq!(serde_json::to_value(&*s).unwrap()["boards"], serde_json::json!([0, 1, 3]));
    }

    #[test]
    fn a_connected_board_without_a_section_is_listed_but_has_no_keys() {
        let (l, _rx) = link(&[0, 5], true);
        let s = l.hub.setup();
        assert_eq!(s.boards, vec![0, 5]);
        assert!(s.keys.len() == 61 && s.keys.iter().all(|k| k.board == 0));
    }

    #[test]
    fn an_octave_shift_moves_the_pitches_of_its_board_and_is_announced() {
        let (mut l, _rx) = link(&[0, 1, 3], true);
        let before = l.hub.setup();
        let next = l.hub.events(None, Duration::ZERO).next;
        // Up on board 3 (device 14)
        l.set_shift(14, 1);
        let after = l.hub.setup();
        for (a, b) in before.keys.iter().zip(after.keys.iter()) {
            assert_eq!((a.board, a.key), (b.board, b.key));
            assert_eq!(b.pitch - a.pitch, if a.board == 3 { 31 } else { 0 });
            assert!((b.hz / a.hz - if a.board == 3 { 2.0 } else { 1.0 }).abs() < 1e-9);
        }
        // the same pitches the retuner has after that shift
        let t = tunings(&[3], 1);
        assert_eq!(after.keys.iter().find(|k| (k.board, k.key) == (3, 20)).unwrap().pitch, t[&14].pad_tuning(20).unwrap().abs_pitch);
        let e = l.hub.events(Some(next), Duration::ZERO);
        assert_eq!(e.events, vec![serde_json::json!({"seq": next + 1, "type": "setup", "boards": [0, 1, 3]})]);
        // the same shift again or an unknown device: no event
        l.set_shift(14, 1);
        l.set_shift(99, 2);
        assert_eq!(l.hub.events(Some(next), Duration::ZERO).events.len(), 1);
        // down by two on board 0
        l.set_shift(11, -2);
        assert_eq!(l.hub.setup().keys[4].pitch, pitch(0, 4) - 62);
    }

    #[test]
    fn pad_notes_become_key_events_with_address_velocity_and_sounding_pitch() {
        let (mut l, _rx) = link(&[0, 1, 3], true);
        let mut midi: Vec<[u8; 3]> = Vec::new();
        l.set_shift(14, 1);
        let next = l.hub.events(None, Duration::ZERO).next;
        // pad 20 of board 3 (device 14) on MPE channel 2, pad 7 of board 0 on channel 3
        l.midi_in(14, &[0x91, 20, 93], &mut midi);
        l.midi_in(11, &[0x92, 7, 40], &mut midi);
        assert_eq!(l.hub.held(), vec![(0, 7), (3, 20)]);
        // what is not a pad's note: X, Y, Z, a button on channel 16, a note above the pads,
        // a board that is not ours, a short message
        let other: [(usize, &[u8]); 7] = [(14, &[0xE1, 0, 64]), (14, &[0xB1, 74, 5]), (14, &[0xD1, 9, 0]), (14, &[0x9F, 20, 127]), (14, &[0x91, 61, 90]), (77, &[0x91, 20, 90]), (14, &[0x91, 20])];
        for (dev, msg) in other {
            l.midi_in(dev, msg, &mut midi);
        }
        // released while the board has been shifted further: the pitch of the press
        l.set_shift(14, 2);
        l.midi_in(14, &[0x81, 20, 64], &mut midi);
        // a note-on with velocity 0 is a release
        l.midi_in(11, &[0x92, 7, 0], &mut midi);
        assert!(l.hub.held().is_empty());
        let e: Vec<serde_json::Value> = l.hub.events(Some(next), Duration::ZERO).events.into_iter().filter(|e| e["type"] == "key").collect();
        assert_eq!(e.len(), 4);
        let mut first = e[0].clone();
        assert!(first["t_us"].is_u64());
        first["t_us"] = 0.into();
        assert_eq!(first, serde_json::json!({"seq": next + 1, "type":"key","t_us":0,"board":3,"key":20,"kind":"on","velocity":93,"pitch":pitch(3, 20) + 31}));
        assert_eq!((e[1]["board"].as_u64(), e[1]["key"].as_u64(), e[1]["velocity"].as_u64(), e[1]["pitch"].as_i64()), (Some(0), Some(7), Some(40), Some(pitch(0, 7) as i64)));
        assert_eq!((e[2]["kind"].as_str(), e[2]["key"].as_u64(), e[2]["velocity"].as_u64(), e[2]["pitch"].as_i64()), (Some("off"), Some(20), Some(0), Some(pitch(3, 20) as i64 + 31)));
        assert_eq!((e[3]["kind"].as_str(), e[3]["board"].as_u64(), e[3]["key"].as_u64()), (Some("off"), Some(0), Some(7)));
        assert!(midi.is_empty(), "key events send no MIDI");
    }

    #[test]
    fn an_api_note_sounds_as_a_touch_of_the_pad_would() {
        let (mut l, _rx) = link(&[0, 1, 3], true);
        let mut t = tunings(&[0, 1, 3], 0);
        let t0 = Instant::now();
        let mut midi: Vec<[u8; 3]> = Vec::new();
        l.hub.queue(Command::Notes { on: vec![NoteOn { board: 3, key: 20, velocity: 80, ms: Some(200) }], off: vec![] });
        assert!(!l.apply_commands(Some(&mut t), t0, &mut midi));
        // what the retuner sends for a real touch of that pad on the same channel (15)
        let mut reference = tunings(&[3], 0);
        let real = reference.get_mut(&14).unwrap().process_message(&[0x9E, 20, 80]);
        assert_eq!(midi.iter().map(|m| m.to_vec()).collect::<Vec<_>>(), real);
        assert_eq!((midi[0][0], midi[1][0]), (0xEE, 0x9E));
        let pt = t[&14].pad_tuning(20).unwrap();
        assert_eq!(midi[1][1], pt.base_note);
        let bend = (midi[0][1] as i32 | (midi[0][2] as i32) << 7) - 8192;
        assert_eq!(bend, pt.bend_offset);
        assert_ne!(bend, 0, "a 31-EDO pitch off the 12-EDO grid needs a bend");
        // the retuner's channel is free again: a touch on it is not mistaken for the pad
        assert_eq!(t[&14].channel_pad(14), None);
        // released by xentool after its length, with the note that was sent
        midi.clear();
        l.tick_notes(t0 + Duration::from_millis(199), &mut midi);
        assert!(midi.is_empty());
        l.tick_notes(t0 + Duration::from_millis(200), &mut midi);
        assert_eq!(midi, vec![[0x8E, pt.base_note, 0]]);
    }

    #[test]
    fn api_notes_take_channels_no_touch_and_no_other_api_note_is_using() {
        let (mut l, _rx) = link(&[0, 1], true);
        let mut t = tunings(&[0, 1], 0);
        let t0 = Instant::now();
        let mut midi: Vec<[u8; 3]> = Vec::new();
        // a finger on board 1 holds channel 15, one on board 0 channel 14
        let _ = t.get_mut(&12).unwrap().process_message(&[0x9E, 3, 100]);
        let _ = t.get_mut(&11).unwrap().process_message(&[0x9D, 4, 100]);
        let on = |board, key| NoteOn { board, key, velocity: 70, ms: None };
        l.hub.queue(Command::Notes { on: vec![on(0, 10), on(0, 11), on(1, 10)], off: vec![] });
        l.apply_commands(Some(&mut t), t0, &mut midi);
        let channels: Vec<u8> = midi.iter().filter(|m| m[0] & 0xF0 == 0x90).map(|m| m[0] & 0x0F).collect();
        assert_eq!(channels, vec![12, 11, 10]);
        // each note-on is preceded by the bend of its channel
        for pair in midi.chunks(2) {
            assert_eq!((pair[0][0] & 0xF0, pair[1][0] & 0xF0, pair[0][0] & 0x0F), (0xE0, 0x90, pair[1][0] & 0x0F));
        }
        // boards differ by an octave: same bend, note 12 higher
        assert_eq!((midi[4][1], midi[4][2], midi[5][1]), (midi[0][1], midi[0][2], midi[1][1] + 12));
        // the fingers' channels are untouched
        assert_eq!((t[&12].channel_pad(14), t[&11].channel_pad(13)), (Some(3), Some(4)));
        // a touch arrives on a channel an API note sounds on: that note ends first
        midi.clear();
        l.midi_in(11, &[0x9C, 30, 90], &mut midi);
        assert_eq!(midi.len(), 1);
        assert_eq!((midi[0][0], midi[0][2]), (0x8C, 0));
        // an explicit off; an off for a key that does not sound is nothing
        midi.clear();
        l.hub.queue(Command::Notes { on: vec![], off: vec![(0, 11), (0, 10), (3, 3)] });
        l.apply_commands(Some(&mut t), t0, &mut midi);
        assert_eq!(midi.len(), 1);
        assert_eq!(midi[0][0], 0x8B);
        // the same key again: the sounding note is released before the new one starts
        midi.clear();
        l.hub.queue(Command::Notes { on: vec![on(1, 10)], off: vec![] });
        l.apply_commands(Some(&mut t), t0, &mut midi);
        assert_eq!(midi.iter().map(|m| m[0]).collect::<Vec<_>>(), vec![0x8A, 0xEC, 0x9C]);
        midi.clear();
        l.release_all(&mut midi);
        assert_eq!(midi.len(), 1);
    }

    #[test]
    fn api_notes_that_cannot_sound_are_dropped_and_raw_midi_passes() {
        let (mut l, _rx) = link(&[0, 5], true);
        let mut t = tunings(&[0], 0);
        let t0 = Instant::now();
        let mut midi: Vec<[u8; 3]> = Vec::new();
        let on = |board, key| NoteOn { board, key, velocity: 70, ms: None };
        // no such pad, board not connected, board without a section (no retuner)
        l.hub.queue(Command::Notes { on: vec![on(0, 61), on(2, 1), on(5, 1)], off: vec![] });
        l.apply_commands(Some(&mut t), t0, &mut midi);
        assert!(midi.is_empty());
        l.hub.queue(api::parse_midi(br#"{"notes":[{"channel":10,"note":76,"velocity":100,"ms":30}]}"#).unwrap());
        l.apply_commands(Some(&mut t), t0, &mut midi);
        assert_eq!(midi, vec![[0x99, 76, 100]]);
        l.tick_notes(t0 + Duration::from_millis(30), &mut midi);
        assert_eq!(midi[1], [0x89, 76, 0]);
        // all fourteen member channels held by fingers: no channel, no note
        midi.clear();
        for ch in 1..=14u8 {
            let _ = t.get_mut(&11).unwrap().process_message(&[0x90 | ch, ch, 100]);
        }
        l.hub.queue(Command::Notes { on: vec![on(0, 40)], off: vec![] });
        l.apply_commands(Some(&mut t), t0, &mut midi);
        assert!(midi.is_empty());
        // fourteen API notes on a free board, then a fifteenth: the one on channel 15 gives way
        let mut t = tunings(&[0], 0);
        l.hub.queue(Command::Notes { on: (0..15).map(|k| on(0, k)).collect(), off: vec![] });
        l.apply_commands(Some(&mut t), t0, &mut midi);
        assert_eq!(midi.iter().filter(|m| m[0] & 0xF0 == 0x90).count(), 15);
        assert_eq!(midi.iter().filter(|m| m[0] & 0xF0 == 0x80).map(|m| m[0]).collect::<Vec<_>>(), vec![0x8E]);
    }

    #[test]
    fn with_mts_esp_notes_and_midi_are_refused_and_nothing_is_sent() {
        let (mut l, rx) = link(&[0], false);
        let port = l.listen(0).unwrap();
        let url = |p: &str| format!("http://127.0.0.1:{port}{p}");
        for (path, body) in [("/api/notes", r#"{"on":[{"board":0,"key":1}]}"#), ("/api/midi", r#"{"notes":[{"channel":10,"note":76}]}"#)] {
            match ureq::post(&url(path)).send_string(body) {
                Err(ureq::Error::Status(501, r)) => assert!(r.into_string().unwrap().contains("--mts-esp"), "{path}"),
                _ => panic!("{path}: expected 501"),
            }
        }
        // the rest of the API is there: overlay, key events, description
        assert_eq!(ureq::post(&url("/api/leds/overlay")).send_string(r#"{"set":[{"board":0,"key":2,"rgb":"00FF00"}]}"#).unwrap().status(), 200);
        let mut none = NoMidi;
        assert!(!l.apply_commands(None, Instant::now(), &mut none));
        assert_eq!(overlay_of(&rx).keys, vec![(pad_addr(0, 2), (0, 127, 0))]);
        l.midi_in(11, &[0x91, 2, 64], &mut none);
        assert_eq!(l.hub.held(), vec![(0, 2)]);
        let s: serde_json::Value = ureq::get(&url("/api/setup")).call().unwrap().into_json().unwrap();
        assert_eq!((s["backend"].as_str(), s["keys"].as_array().unwrap().len()), (Some("exquis"), 61));
    }

    #[test]
    fn the_overlay_reaches_the_pads_in_device_colours_and_clearing_restores_the_layout() {
        let (mut l, rx) = link(&[0, 1, 3], true);
        let lay = layout();
        // the boards as serve painted them at start
        let mut leds = LedState::new();
        let base: Vec<(u8, [Color; PADS])> = [0u8, 1, 3].iter().map(|&b| (b, pad_colors(section(&lay, b).unwrap(), &ColorCorrection::default()))).collect();
        for (b, colors) in &base {
            leds.seed_pads(*b, colors);
        }
        let t0 = Instant::now();
        let mut midi: Vec<[u8; 3]> = Vec::new();
        l.hub.queue(Command::Overlay { clear: OverlayClear::None, set: vec![(3, 20, (255, 0, 0)), (3, 61, (1, 1, 1)), (1, 0, (0, 255, 0))], dim_base: Some(0.5) });
        l.apply_commands(None, t0, &mut midi);
        // 8-bit API colours become the pads' 7-bit ones; pad 61 does not exist
        let f = overlay_of(&rx);
        assert_eq!(f, OverlayFrame { keys: vec![(pad_addr(1, 0), (0, 127, 0)), (pad_addr(3, 20), (127, 0, 0))], dim_base: 0.5 });
        leds.set_overlay(&f);
        let due = leds.due(t0, false);
        assert_eq!(due.len(), 3, "the layout colours of all boards are dimmed");
        let of = |b: u8| due.iter().find(|d| d.board == b).unwrap().colors;
        assert_eq!((of(3)[20], of(1)[0]), (Color::new(127, 0, 0), Color::new(0, 127, 0)));
        // board 0 pad 30: 8-bit (60, 0, 0) = device (30, 0, 0), at half brightness
        assert_eq!((base[0].1[30], of(0)[30]), (Color::new(30, 0, 0), Color::new(15, 0, 0)));
        // one key cleared: its layout colour (dimmed, the other key is still set)
        l.hub.queue(Command::Overlay { clear: OverlayClear::Keys(vec![(3, 20)]), set: vec![], dim_base: None });
        l.apply_commands(None, t0, &mut midi);
        leds.set_overlay(&overlay_of(&rx));
        let due = leds.due(t0 + Duration::from_secs(1), false);
        assert_eq!(due.len(), 1);
        // board 3 pad 20: 8-bit (40, 0, 6) = device (20, 0, 3)
        assert_eq!((base[2].1[20], due[0].colors[20]), (Color::new(20, 0, 3), Color::new(10, 0, 2)));
        // all cleared: every board shows exactly its layout colours
        l.hub.queue(Command::Overlay { clear: OverlayClear::All, set: vec![], dim_base: None });
        l.apply_commands(None, t0, &mut midi);
        leds.set_overlay(&overlay_of(&rx));
        let due = leds.due(t0 + Duration::from_secs(2), false);
        assert_eq!(due.len(), 3);
        for (b, colors) in &base {
            assert_eq!(&due.iter().find(|d| d.board == *b).unwrap().colors, colors);
        }
        assert!(midi.is_empty());
    }

    #[test]
    fn a_layout_request_names_the_file_next_to_the_served_one() {
        let (mut l, rx) = link(&[0, 1], true);
        let mut midi: Vec<[u8; 3]> = Vec::new();
        l.set_shift(12, 1);
        l.hub.queue(Command::Overlay { clear: OverlayClear::None, set: vec![(1, 0, (1, 2, 3))], dim_base: None });
        l.hub.queue(Command::Layout { name: "edo53".into() });
        assert!(l.apply_commands(None, Instant::now(), &mut midi));
        assert_eq!(l.take_layout_request(), Some(Path::new("xtn").join("edo53.xtn")));
        assert_eq!(l.take_layout_request(), None);
        assert_eq!(overlay_of(&rx).keys.len(), 1);
        // loading failed: the old description stays, the reason is kept for /api/layouts
        l.layout_failed("no Edo=".into());
        assert_eq!(l.hub.setup().layout, "edo31");
        // loaded: new description, shifts back to 0, overlay gone, events in the new tuning
        let new = parse_xtn_str("Edo=53\nPitchOffset=0\n[Board1]\nKey_0=9\nChan_0=2\nCol_0=FF8000\n", "t").unwrap();
        let next = l.hub.events(None, Duration::ZERO).next;
        l.set_layout(&new, 53, Path::new("xtn/edo53.xtn"));
        assert!(overlay_of(&rx).keys.is_empty());
        let s = l.hub.setup();
        assert_eq!((s.layout.as_str(), s.edo, s.boards.clone(), s.keys.len()), ("edo53", 53, vec![0, 1], 61));
        // the colour is the pad's (7 bit per channel) scaled back to 8 bit: 80 comes back as 81
        assert_eq!((s.keys[0].board, s.keys[0].pitch, s.keys[0].color.as_str()), (1, 53 + 9 + 2 * 53, "FF8100"));
        assert_eq!(l.hub.events(Some(next), Duration::ZERO).events, vec![serde_json::json!({"seq": next + 1, "type": "setup", "boards": [0, 1]})]);
        l.midi_in(12, &[0x91, 0, 64], &mut midi);
        assert_eq!(l.hub.events(Some(next + 1), Duration::ZERO).events[0]["pitch"], 53 + 9 + 2 * 53);
        // board 0 has no section in the new layout: its pads are silent and send no events
        l.midi_in(11, &[0x91, 0, 64], &mut midi);
        assert_eq!(l.hub.held(), vec![(1, 0)]);
    }

    #[test]
    fn the_bundled_layout_is_isomorphic_across_two_boards() {
        let lay = parse_xtn_str(include_str!("../../xtn/edo31.xtn"), "edo31.xtn").unwrap();
        let boards: Vec<(u8, i32)> = (0..4u8).filter(|b| section(&lay, *b).is_some()).map(|b| (b, 0)).collect();
        assert!(!boards.is_empty());
        let s = build_setup(&lay, 31, "edo31", &boards);
        let at: HashMap<(i32, i32), i32> = s.keys.iter().map(|k| ((k.x, k.y), k.pitch)).collect();
        let d = &s.directions;
        for (step, name) in [(d.right, "right"), (d.upper_right, "upper right"), (d.upper_left, "upper left")] {
            let intervals: BTreeSet<i32> = at.iter().filter_map(|(&(x, y), &p)| at.get(&(x + step.0, y + step.1)).map(|q| q - p)).collect();
            assert_eq!(intervals.len(), 1, "{name}: {intervals:?}");
        }
    }
}
