//! The Wooting serve loop's side of the local API (`serve --api`, `src/api.rs`).
//!
//! Key addresses are (board, key) with `key` = the `.wtn` cell index (row * 14 + column of
//! the board's logical 4x14 grid), the same number as in the layout file and in the editor.
//!
//! Lattice: `geometry::WTN_BOARD0_TUPLES` / `WTN_BOARD1_TUPLES` are the positions of a
//! pair of keyboards (board 0 turned by 180 degrees above board 1) in one hex lattice, so
//! a pair is one keyfield. Their key numbers count the keys of a board one after the other
//! (0..52), while a `.wtn` index leaves the unused columns of the short rows out; here the
//! positions are numbered with the `.wtn` index. A keyboard without a partner is upright
//! and gets the positions of the lower board. Further pairs (boards 2+3, ..) have no
//! defined place: they are put to the right of the pair before, with a gap of one key
//! width so that no key of one pair counts as a neighbour of a key of the other.
//!
//! Octave: the Space key of a keyboard toggles its octave hold, which raises every note
//! of that keyboard by one channel = one octave. `/api/setup` gives the pitches that sound
//! with the holds as they are now, and a `setup` event follows every toggle. Notes asked
//! for by `/api/notes` use the same channel as a press of that key would.

use std::path::Path;
use std::time::Instant;

use anyhow::Result;
use crossbeam_channel::{Receiver, Sender};

use crate::api::{self, ApiHub, Command, KeyEvent, KeyKind, LayoutCell, MidiSink, OverlayState, Setup, TimedNotes};
use crate::geometry::{GeometryInfo, Orientation, WTN_BOARD0_TUPLES, WTN_BOARD1_TUPLES, WTN_BOARD_SHIFT};
use crate::wooting::geometry::rotated;
use crate::wooting::hidmap::{HidMap, compute_compact_col_offsets, wtn_index_for_loc};
use crate::wooting::overlay::{LedAddr, OverlayFrame};
use crate::wooting::wtn::{WTN_CELLS_PER_BOARD, Wtn};

pub const BACKEND: &str = "wooting";
/// Lateral distance between two pairs of keyboards in the lattice (a pair spans y = -5..26).
const PAIR_STRIDE_Y: i32 = 34;

/// A connected keyboard as the API needs it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BoardInfo {
    /// `.wtn` board = index in the Analog SDK's device list.
    pub board: u8,
    /// Index of the same keyboard in the RGB SDK.
    pub rgb_idx: u8,
    pub octave_hold: bool,
}

/// MIDI channel (0..15) of a layout cell: its channel raised by the octave shift and hold.
pub fn out_channel(chan: u8, octave_shift: i16, octave_hold: bool) -> u8 {
    let base = chan.saturating_sub(1) as i16;
    let hold = if octave_hold { 1 } else { 0 };
    (base + octave_shift + hold).clamp(0, 15) as u8
}

/// Lattice positions of the keys of `board` when `total` keyboards are connected, as
/// (`.wtn` cell index, x, y).
pub fn board_positions(board: u8, total: u8) -> Vec<(u8, i32, i32)> {
    let tuples = if rotated(board, total) { WTN_BOARD0_TUPLES } else { WTN_BOARD1_TUPLES };
    let dy = (board / 2) as i32 * PAIR_STRIDE_Y;
    let first_row = tuples.iter().map(|t| t.1).min().unwrap_or(0);
    let mut out = Vec::with_capacity(tuples.len());
    let (mut row, mut col) = (first_row, 0u8);
    // the tuples list each row from left to right, rows from top to bottom
    for &(_, x, y) in tuples {
        if x != row {
            row = x;
            col = 0;
        }
        out.push(((x - first_row) as u8 * 14 + col, x, y + dy));
        col += 1;
    }
    out
}

/// The connected keyboards as one geometry for `api::build_setup`.
pub fn geometry_for(total: u8) -> GeometryInfo {
    GeometryInfo {
        name: "wooting",
        aliases: &["wtn"],
        description: "",
        boards: (0..total).map(|b| board_positions(b, total)).collect(),
        board_shift: Some(WTN_BOARD_SHIFT),
        orientation: Orientation::YRightXDown,
        outlines: (0..total).map(|b| crate::wooting::geometry::board_outlines(b, total)).collect(),
        display_rotation_deg: 0.0,
    }
}

/// LED (row, column) of every `.wtn` cell, for an upright and for a turned keyboard.
pub struct LedTable {
    upright: [Option<(u8, u8)>; WTN_CELLS_PER_BOARD],
    turned: [Option<(u8, u8)>; WTN_CELLS_PER_BOARD],
}

impl LedTable {
    pub fn new(map: &HidMap) -> Self {
        let table = |rotation_deg: u16| {
            let compact = compute_compact_col_offsets(map, rotation_deg);
            let mut t = [None; WTN_CELLS_PER_BOARD];
            for (_, loc) in map.all_locs() {
                if let Some(slot) = wtn_index_for_loc(loc, rotation_deg, &compact).and_then(|i| t.get_mut(i)) {
                    *slot = Some((loc.led_row, loc.led_col));
                }
            }
            t
        };
        Self { upright: table(0), turned: table(180) }
    }

    /// None: the cell has no physical key.
    pub fn led(&self, key: u8, turned: bool) -> Option<(u8, u8)> {
        let t = if turned { &self.turned } else { &self.upright };
        t.get(key as usize).copied().flatten()
    }
}

/// Every cell of the connected keyboards that plays a note, with the pitch that sounds now.
pub fn layout_cells(wtn: &Wtn, edo: i32, boards: &[BoardInfo], octave_shift: i16, leds: &LedTable) -> Vec<LayoutCell> {
    let total = boards.len() as u8;
    let mut out = Vec::new();
    for b in boards {
        let turned = rotated(b.board, total);
        for key in 0..WTN_CELLS_PER_BOARD as u8 {
            let Some(cell) = wtn.cell(b.board, key as usize).filter(|c| c.chan != 0) else { continue };
            if leds.led(key, turned).is_none() {
                continue;
            }
            let ch = out_channel(cell.chan, octave_shift, b.octave_hold);
            out.push(LayoutCell { board: b.board, key, pitch: ch as i32 * edo + cell.key as i32 + wtn.pitch_offset, color: cell.color });
        }
    }
    out
}

pub fn build_setup(wtn: &Wtn, edo: i32, layout: &str, boards: &[BoardInfo], octave_shift: i16, leds: &LedTable) -> Setup {
    let present: Vec<u8> = boards.iter().map(|b| b.board).collect();
    let cells = layout_cells(wtn, edo, boards, octave_shift, leds);
    api::build_setup(BACKEND, &geometry_for(boards.len() as u8), layout, edo, &cells, &present)
}

/// The overlay resolved to LEDs of the keyboards connected now.
pub fn overlay_frame(overlay: &OverlayState, boards: &[BoardInfo], leds: &LedTable) -> OverlayFrame {
    let total = boards.len() as u8;
    let mut keys: Vec<(LedAddr, api::Rgb)> = overlay
        .keys
        .iter()
        .filter_map(|(&(board, key), &rgb)| {
            let b = boards.iter().find(|b| b.board == board)?;
            let (row, col) = leds.led(key, rotated(board, total))?;
            Some(((b.rgb_idx, row, col), rgb))
        })
        .collect();
    keys.sort_unstable();
    OverlayFrame { keys, dim_base: overlay.dim_base.unwrap_or(1.0) }
}

pub struct ApiLink {
    pub hub: ApiHub,
    rx: Receiver<Command>,
    overlay_tx: Sender<OverlayFrame>,
    leds: LedTable,
    layout: String,
    edo: i32,
    pitch_offset: i32,
    started: Instant,
    /// The overlay by key address; resolved to LEDs again when keyboards come or go.
    overlay: OverlayState,
    notes: TimedNotes,
    /// A layout the API asked to switch to; the serve loop takes it.
    pub layout_request: Option<String>,
}

impl ApiLink {
    #[allow(clippy::too_many_arguments)]
    pub fn new(wtn: &Wtn, edo: i32, file: &Path, map: &HidMap, boards: &[BoardInfo], octave_shift: i16, started: Instant, overlay_tx: Sender<OverlayFrame>) -> Self {
        let layout = crate::hud::layout_id_from_path(file);
        let leds = LedTable::new(map);
        let (hub, rx) = ApiHub::new(build_setup(wtn, edo, &layout, boards, octave_shift, &leds));
        hub.set_layout_file(file);
        Self {
            hub,
            rx,
            overlay_tx,
            leds,
            layout,
            edo,
            pitch_offset: wtn.pitch_offset,
            started,
            overlay: OverlayState::default(),
            notes: TimedNotes::default(),
            layout_request: None,
        }
    }

    /// Start the HTTP server. Returns the bound port.
    pub fn listen(&self, port: u16) -> Result<u16> {
        api::spawn(self.hub.clone(), port)
    }

    /// Keyboards came or went, or an octave hold was toggled: new description (with a
    /// `setup` event), and the overlay goes to the LEDs the keys have now.
    pub fn boards_changed(&self, wtn: &Wtn, boards: &[BoardInfo], octave_shift: i16) {
        self.hub.set_setup(build_setup(wtn, self.edo, &self.layout, boards, octave_shift, &self.leds));
        self.send_overlay(boards);
    }

    /// Another layout was loaded: new description, and the overlay (which belonged to the
    /// old notes) is gone.
    pub fn set_layout(&mut self, wtn: &Wtn, edo: i32, file: &Path, boards: &[BoardInfo], octave_shift: i16) {
        self.layout = crate::hud::layout_id_from_path(file);
        self.edo = edo;
        self.pitch_offset = wtn.pitch_offset;
        self.overlay.keys.clear();
        self.hub.set_layout_file(file);
        self.boards_changed(wtn, boards, octave_shift);
        self.hub.layout_result(None);
    }

    /// A note of a real key started or ended (`out_ch`, `note`: what was sent as MIDI).
    pub fn key_event(&self, board: u8, key: u8, kind: KeyKind, velocity: u8, out_ch: u8, note: u8) {
        self.hub.publish_key(&KeyEvent {
            t_us: self.started.elapsed().as_micros() as u64,
            board,
            key,
            kind,
            velocity,
            pitch: out_ch as i32 * self.edo + note as i32 + self.pitch_offset,
        });
    }

    pub fn overlay_active(&self) -> bool {
        self.overlay.is_active()
    }

    fn send_overlay(&self, boards: &[BoardInfo]) {
        let _ = self.overlay_tx.send(overlay_frame(&self.overlay, boards, &self.leds));
    }

    /// Apply queued commands. Returns true if there was any (counts as activity).
    pub fn apply_commands(&mut self, wtn: &Wtn, boards: &[BoardInfo], octave_shift: i16, now: Instant, midi: &mut impl MidiSink) -> bool {
        let mut any = false;
        while let Ok(cmd) = self.rx.try_recv() {
            any = true;
            match cmd {
                Command::Overlay { clear, set, dim_base } => {
                    self.overlay.apply(&clear, &set, dim_base, |_, k| (k as usize) < WTN_CELLS_PER_BOARD);
                    self.send_overlay(boards);
                }
                Command::Layout { name } => self.layout_request = Some(name),
                // refused by the hub (501): the board order of this backend is fixed
                Command::Boards { .. } => {}
                Command::Midi { notes } => {
                    for n in &notes {
                        self.notes.raw_on(n, now, midi);
                    }
                }
                Command::Notes { on, off } => {
                    for key in off {
                        self.notes.key_off(key, midi);
                    }
                    let total = boards.len() as u8;
                    for n in on {
                        let key = (n.board, n.key);
                        self.notes.key_off(key, midi);
                        let Some(b) = boards.iter().find(|b| b.board == n.board) else { continue };
                        if self.leds.led(n.key, rotated(n.board, total)).is_none() {
                            continue;
                        }
                        if let Some(cell) = wtn.cell(n.board, n.key as usize).filter(|c| c.chan != 0) {
                            let ch = out_channel(cell.chan, octave_shift, b.octave_hold);
                            self.notes.key_on(key, ch, cell.key, n.velocity, n.ms, now, midi);
                        }
                    }
                }
            }
        }
        any
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
    use crate::wooting::hidmap::hid;
    use crate::wooting::wtn::parse_wtn;
    use std::collections::{BTreeSet, HashMap};

    /// Two boards, every cell set: board b cell k plays note k on channel b + 1 (so one
    /// octave per board), colour (k, 0, b). Cell 5 of board 1 has no channel.
    fn layout() -> Wtn {
        let mut text = String::from("Edo=31\nPitchOffset=3\n");
        for b in 0..2 {
            text.push_str(&format!("[Board{b}]\n"));
            for k in 0..56 {
                let chan = if (b, k) == (1, 5) { 0 } else { b + 1 };
                text.push_str(&format!("Key_{k}={k}\nChan_{k}={chan}\nCol_{k}={k:02X}00{b:02X}\n"));
            }
        }
        parse_wtn(&text).unwrap()
    }

    fn boards(holds: &[bool]) -> Vec<BoardInfo> {
        holds.iter().enumerate().map(|(i, &h)| BoardInfo { board: i as u8, rgb_idx: i as u8, octave_hold: h }).collect()
    }

    fn link(b: &[BoardInfo]) -> (ApiLink, Receiver<OverlayFrame>, Wtn) {
        let wtn = layout();
        let (tx, rx) = crossbeam_channel::unbounded();
        let l = ApiLink::new(&wtn, 31, Path::new("wtn/edo31.wtn"), &HidMap::default_60he_ansi_guess(), b, 0, Instant::now(), tx);
        (l, rx, wtn)
    }

    #[test]
    fn positions_cover_exactly_the_physical_keys() {
        let map = HidMap::default_60he_ansi_guess();
        let leds = LedTable::new(&map);
        for (board, total, turned) in [(0u8, 1u8, false), (0, 2, true), (1, 2, false), (2, 3, false)] {
            let with_key: BTreeSet<u8> = (0..56u8).filter(|&k| leds.led(k, turned).is_some()).collect();
            let with_pos: BTreeSet<u8> = board_positions(board, total).iter().map(|p| p.0).collect();
            assert_eq!(with_key.len(), 53);
            assert_eq!(with_key, with_pos, "board {board} of {total}");
        }
        // row lengths: upright 14, 14, 13, 12; turned 12, 13, 14, 14
        assert!(leds.led(41, false).is_none() && leds.led(54, false).is_none() && leds.led(55, false).is_none());
        assert!(leds.led(12, true).is_none() && leds.led(13, true).is_none() && leds.led(27, true).is_none());
        // upright: cell 0 is Esc (LED row 1, column 0); turned: cell 0 is the right Shift
        assert_eq!(leds.led(0, false), Some((1, 0)));
        let rshift = map.loc_for(hid::RIGHT_SHIFT).unwrap();
        assert_eq!(leds.led(0, true), Some((rshift.led_row, rshift.led_col)));
        assert_eq!(leds.led(55, true), Some((1, 0)));
    }

    #[test]
    fn a_pair_is_one_lattice_and_more_pairs_do_not_touch() {
        let mut seen: HashMap<(i32, i32), u8> = HashMap::new();
        for b in 0..4u8 {
            for (_, x, y) in board_positions(b, 4) {
                assert_eq!((x + y).rem_euclid(2), 1, "all keys on one sub-lattice");
                assert!(seen.insert((x, y), b).is_none(), "two keys at ({x}, {y})");
            }
        }
        let neighbours = |a: u8, b: u8| {
            seen.iter().filter(|(_, bb)| **bb == a).any(|(&(x, y), _)| {
                crate::geometry::HEX_NEIGHBOR_DELTAS.iter().any(|(dx, dy)| seen.get(&(x + dx, y + dy)) == Some(&b))
            })
        };
        assert!(neighbours(0, 1) && neighbours(2, 3));
        assert!(!neighbours(0, 2) && !neighbours(1, 2) && !neighbours(1, 3) && !neighbours(0, 3));
        // the turned board lies above (smaller x) its upright partner
        assert!(board_positions(0, 2).iter().all(|p| p.1 <= 3) && board_positions(1, 2).iter().all(|p| p.1 >= 4));
    }

    /// The bundled layouts are isomorphic: in this lattice the same step leads to the same
    /// interval everywhere, also from the turned board to the upright one.
    #[test]
    fn bundled_layout_is_isomorphic_in_the_lattice() {
        let wtn = parse_wtn(include_str!("../../wtn/edo31.wtn")).unwrap();
        let leds = LedTable::new(&HidMap::default_60he_ansi_guess());
        let s = build_setup(&wtn, 31, "edo31", &boards(&[false, false]), 0, &leds);
        assert_eq!(s.keys.len(), 106);
        let at: HashMap<(i32, i32), i32> = s.keys.iter().map(|k| ((k.x, k.y), k.pitch)).collect();
        let d = &s.directions;
        for (step, name) in [(d.right, "right"), (d.lower_right, "lower right"), (d.lower_left, "lower left")] {
            let intervals: BTreeSet<i32> = at.iter().filter_map(|(&(x, y), &p)| at.get(&(x + step.0, y + step.1)).map(|q| q - p)).collect();
            assert_eq!(intervals.len(), 1, "{name}: {intervals:?}");
        }
        let right = at[&(0, 3)] - at[&(0, 1)];
        assert!(right > 0, "pitch rises to the right");
    }

    #[test]
    fn setup_for_one_keyboard() {
        let (l, _rx, _) = link(&boards(&[false]));
        let s = l.hub.setup();
        assert_eq!((s.backend.as_str(), s.geometry.as_str(), s.layout.as_str(), s.edo), ("wooting", "wooting", "edo31", 31));
        assert_eq!(s.boards, vec![0]);
        assert_eq!(s.keys.len(), 53);
        assert_eq!(s.directions.upper_right, (-1, 1));
        // upright: cell 0 (Esc) is the top left key; the key right of it is cell 1
        let k0 = &s.keys[0];
        assert_eq!((k0.board, k0.key, k0.pitch, k0.color.as_str()), (0, 0, 3, "000000"));
        let k1 = &s.keys[1];
        assert_eq!((k1.key, k1.x - k0.x, k1.y - k0.y), (1, 0, 2));
        assert!((k0.hz - crate::mts::edo_freq_hz(31, 3)).abs() < 1e-9);
        // cells without a physical key are not listed
        assert!(s.keys.iter().all(|k| ![41, 54, 55].contains(&k.key)));
        let json = serde_json::to_value(&*s).unwrap();
        assert_eq!(json["keys"][1], serde_json::json!({"board":0,"key":1,"x":4,"y":-3,"pitch":4,"hz":k1.hz,"color":"010000"}));
    }

    #[test]
    fn setup_for_a_pair_with_the_first_keyboard_turned() {
        let (l, _rx, _) = link(&boards(&[false, false]));
        let s = l.hub.setup();
        assert_eq!(s.boards, vec![0, 1]);
        // 53 keys each; board 1 cell 5 has no channel
        assert_eq!(s.keys.len(), 105);
        assert!(!s.keys.iter().any(|k| (k.board, k.key) == (1, 5)));
        let pos = |b: u8, k: u8| s.keys.iter().find(|x| (x.board, x.key) == (b, k)).map(|x| (x.x, x.y)).unwrap();
        // board 0 is turned: its cell 0 is the top left key of the field, row of 12 keys
        assert_eq!(pos(0, 0), (0, 1));
        assert_eq!(pos(0, 11), (0, 23));
        assert!(!s.keys.iter().any(|k| (k.board, k.key) == (0, 12)));
        assert_eq!(pos(0, 14), (1, 0));
        // board 1 below it, shifted to the left
        assert_eq!(pos(1, 0), (4, -5));
        assert_eq!(pos(1, 42), (7, -2));
        // one field: the last row of board 0 touches the first row of board 1
        assert_eq!(pos(0, 42), (3, 0));
        assert_eq!(pos(1, 3), (4, 1));
        // pitches: channel = board + 1 in this layout
        let pitch = |b: u8, k: u8| s.keys.iter().find(|x| (x.board, x.key) == (b, k)).unwrap().pitch;
        assert_eq!((pitch(0, 7), pitch(1, 7)), (7 + 3, 31 + 7 + 3));
    }

    #[test]
    fn octave_hold_raises_the_setup_pitches_of_its_keyboard_and_is_announced() {
        let (l, _rx, wtn) = link(&boards(&[false, false]));
        let before = l.hub.setup();
        let next = l.hub.events(None, std::time::Duration::ZERO).next;
        l.boards_changed(&wtn, &boards(&[false, true]), 0);
        let after = l.hub.setup();
        for (a, b) in before.keys.iter().zip(after.keys.iter()) {
            assert_eq!((a.board, a.key), (b.board, b.key));
            assert_eq!(b.pitch - a.pitch, if a.board == 1 { 31 } else { 0 });
        }
        let k = after.keys.iter().find(|k| k.board == 1).unwrap();
        assert!((k.hz / before.keys.iter().find(|x| (x.board, x.key) == (1, k.key)).unwrap().hz - 2.0).abs() < 1e-9);
        let e = l.hub.events(Some(next), std::time::Duration::ZERO);
        assert_eq!(e.events, vec![serde_json::json!({"seq": next + 1, "type": "setup", "boards": [0, 1]})]);
        // a global octave shift moves every board; channels stop at 16
        let s = build_setup(&wtn, 31, "x", &boards(&[false, true]), 15, &l.leds);
        assert!(s.keys.iter().all(|k| k.pitch == 15 * 31 + k.key as i32 + 3));
    }

    #[test]
    fn key_events_carry_address_velocity_and_sounding_pitch() {
        let (l, _rx, _) = link(&boards(&[false, false]));
        // board 1, cell 7, pressed with the octave hold on: channel index 2
        l.key_event(1, 7, KeyKind::On, 93, out_channel(2, 0, true), 7);
        assert_eq!(l.hub.held(), vec![(1, 7)]);
        l.key_event(1, 7, KeyKind::Off, 0, 2, 7);
        assert!(l.hub.held().is_empty());
        let e = l.hub.events(Some(0), std::time::Duration::ZERO).events;
        let mut on = e[0].clone();
        assert!(on["t_us"].is_u64());
        on["t_us"] = 0.into();
        assert_eq!(on, serde_json::json!({"seq":1,"type":"key","t_us":0,"board":1,"key":7,"kind":"on","velocity":93,"pitch":2 * 31 + 7 + 3}));
        assert_eq!((e[1]["kind"].as_str(), e[1]["velocity"].as_u64(), e[1]["pitch"].as_i64()), (Some("off"), Some(0), Some(72)));
    }

    #[test]
    fn api_notes_sound_like_the_key_including_the_octave_hold() {
        let b = boards(&[false, true]);
        let (mut l, _rx, wtn) = link(&b);
        let t0 = Instant::now();
        let mut midi: Vec<[u8; 3]> = Vec::new();
        let on = |board, key, ms| NoteOn { board, key, velocity: 80, ms };
        l.hub.queue(Command::Notes {
            on: vec![on(0, 7, Some(200)), on(1, 7, None), on(1, 5, None), on(0, 12, None), on(4, 1, None), on(0, 200, None)],
            off: vec![],
        });
        assert!(l.apply_commands(&wtn, &b, 0, t0, &mut midi));
        // board 0: channel 1 -> index 0; board 1: channel 2 plus the hold -> index 2.
        // No channel (1, 5), no physical key (0, 12 on the turned board), board not
        // connected, no such cell: silent.
        assert_eq!(midi, vec![[0x90, 7, 80], [0x92, 7, 80]]);
        assert!(!l.apply_commands(&wtn, &b, 0, t0, &mut midi));
        midi.clear();
        l.tick_notes(t0 + std::time::Duration::from_millis(200), &mut midi);
        assert_eq!(midi, vec![[0x80, 7, 0]]);
        // raw MIDI goes out as it is; an explicit off ends the held note
        midi.clear();
        l.hub.queue(api::parse_midi(br#"{"notes":[{"channel":10,"note":76,"velocity":100,"ms":30}]}"#).unwrap());
        l.hub.queue(Command::Notes { on: vec![], off: vec![(1, 7)] });
        l.apply_commands(&wtn, &b, 0, t0, &mut midi);
        assert_eq!(midi, vec![[0x99, 76, 100], [0x82, 7, 0]]);
        midi.clear();
        l.release_all(&mut midi);
        assert_eq!(midi, vec![[0x89, 76, 0]]);
        // with a global octave shift of one the same key sounds a channel higher
        midi.clear();
        l.hub.queue(Command::Notes { on: vec![on(0, 7, None)], off: vec![] });
        l.apply_commands(&wtn, &b, 1, t0, &mut midi);
        assert_eq!(midi, vec![[0x91, 7, 80]]);
    }

    #[test]
    fn overlay_goes_to_the_leds_of_the_keys_and_follows_the_keyboards() {
        let pair = boards(&[false, false]);
        let (mut l, rx, wtn) = link(&pair);
        let t0 = Instant::now();
        let mut midi: Vec<[u8; 3]> = Vec::new();
        let red = (255, 0, 0);
        l.hub.queue(Command::Overlay { clear: OverlayClear::None, set: vec![(0, 0, red), (1, 0, red), (1, 99, red), (0, 12, red)], dim_base: Some(0.3) });
        l.apply_commands(&wtn, &pair, 0, t0, &mut midi);
        assert!(l.overlay_active());
        // board 0 is turned: its cell 0 is the right Shift (LED row 4, column 13);
        // board 1 cell 0 is Esc. Cell 12 of the turned board has no key, 99 no cell.
        let f = rx.try_recv().unwrap();
        assert_eq!(f, OverlayFrame { keys: vec![((0, 4, 13), red), ((1, 1, 0), red)], dim_base: 0.3 });
        // keyboard 1 unplugged: board 0 is upright now, its cell 0 is Esc, and its cell 12
        // (asked for before) has a key: `=`
        let one = boards(&[false]);
        l.boards_changed(&wtn, &one, 0);
        assert_eq!(rx.try_recv().unwrap().keys, vec![((0, 1, 0), red), ((0, 1, 12), red)]);
        // plugged in again (RGB SDK numbers swapped by the settings): both keys are back
        let swapped = vec![BoardInfo { board: 0, rgb_idx: 1, octave_hold: false }, BoardInfo { board: 1, rgb_idx: 0, octave_hold: false }];
        l.boards_changed(&wtn, &swapped, 0);
        assert_eq!(rx.try_recv().unwrap().keys, vec![((0, 1, 0), red), ((1, 4, 13), red)]);
        // clearing one key, then all
        l.hub.queue(Command::Overlay { clear: OverlayClear::Keys(vec![(0, 0)]), set: vec![], dim_base: None });
        l.apply_commands(&wtn, &pair, 0, t0, &mut midi);
        assert_eq!(rx.try_recv().unwrap(), OverlayFrame { keys: vec![((1, 1, 0), red)], dim_base: 0.3 });
        l.hub.queue(Command::Overlay { clear: OverlayClear::All, set: vec![], dim_base: None });
        l.apply_commands(&wtn, &pair, 0, t0, &mut midi);
        assert!(rx.try_recv().unwrap().keys.is_empty());
        assert!(!l.overlay_active());
    }

    #[test]
    fn a_layout_switch_drops_the_overlay_and_publishes_the_new_setup() {
        let pair = boards(&[false, false]);
        let (mut l, rx, wtn) = link(&pair);
        let mut midi: Vec<[u8; 3]> = Vec::new();
        l.hub.queue(Command::Overlay { clear: OverlayClear::None, set: vec![(1, 0, (1, 2, 3))], dim_base: None });
        l.hub.queue(Command::Layout { name: "edo53".into() });
        l.apply_commands(&wtn, &pair, 0, Instant::now(), &mut midi);
        assert_eq!(l.layout_request.take().as_deref(), Some("edo53"));
        let _ = rx.try_recv().unwrap();
        let new = parse_wtn("Edo=53\nPitchOffset=0\n[Board0]\nKey_0=9\nChan_0=1\nCol_0=112233\n").unwrap();
        l.set_layout(&new, 53, Path::new("wtn/edo53.wtn"), &pair, 0);
        assert!(rx.try_recv().unwrap().keys.is_empty());
        let s = l.hub.setup();
        assert_eq!((s.layout.as_str(), s.edo, s.keys.len()), ("edo53", 53, 1));
        assert_eq!((s.keys[0].pitch, s.keys[0].color.as_str()), (9, "112233"));
        // events of keys use the new tuning
        l.key_event(0, 0, KeyKind::On, 64, 1, 9);
        let e = l.hub.events(Some(0), std::time::Duration::ZERO).events;
        assert_eq!(e.last().unwrap()["pitch"], 53 + 9);
    }
}
