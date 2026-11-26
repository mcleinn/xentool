//! Pad LEDs of the Exquis boards for the local API (`serve --api`): layout colours with the
//! API's overlay on top.
//!
//! The device protocol knows no "set one pad" that keeps MPE alive (see CLAUDE.md,
//! "snapshot-based LED control"): pad colours travel as a snapshot of all 61 pads of a
//! board, 262 bytes. So the unit of painting is a board, not a pad:
//!
//! - `LedState` (pure, unit-tested) keeps per board the layout colours `serve` painted last
//!   (the *base*) and composes them with the overlay through the Wooting backend's
//!   `Compositor` (LED address = (board, 0, pad)): an overlay colour wins, the base of
//!   ALL boards is dimmed by `dim_base` while any overlay key is set, and a cleared key
//!   shows exactly its base again. It hands out a board's snapshot only when the composed
//!   colours differ from what the board shows, and at most one per board every
//!   `MIN_SNAPSHOT_INTERVAL`; changes in between are merged into the next one.
//! - The LED worker thread owns a `LedState` and does the sending, so the serve loop never
//!   waits for the device (`send_to_outputs` opens the MIDI port for every message). With
//!   `--api` every paint of `serve` goes through it (pad colours of a layout change, the
//!   Settings/Up/Down buttons), which keeps the messages to one board in order.
//!
//! A snapshot is sent the way the layout cycling of `serve` has always sent it: developer
//! mode for the non-pad zones, the snapshot, developer mode again, then the control
//! buttons (their colours are remembered here for that).
//!
//! Colours in this module are device colours: 7 bit per channel, colour correction applied.

use std::collections::BTreeMap;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, RecvTimeoutError, Sender};

use crate::exquis::midi::{DeviceSelection, ExquisDevice, send_to_outputs};
use crate::exquis::proto::{self, Color};
use crate::wooting::overlay::{Compositor, LedAddr, OverlayFrame};

/// Pads per board.
pub const PADS: usize = 61;
/// Shortest time between two snapshots to the same board.
pub const MIN_SNAPSHOT_INTERVAL: Duration = Duration::from_millis(50);

pub type PadColors = [Color; PADS];

/// Compositor address of a pad.
pub fn pad_addr(board: u8, pad: u8) -> LedAddr {
    (board, 0, pad)
}

fn rgb(c: Color) -> (u8, u8, u8) {
    (c.red, c.green, c.blue)
}

/// What the serve loop asks the LED worker to do.
pub enum LedMsg {
    /// The layout colours of a board (a layout was loaded).
    Pads { board: u8, colors: Box<PadColors> },
    /// Colour of a control button (Settings 100, Down 106, Up 107).
    Ctrl { board: u8, cc: u8, color: Color },
    /// The whole overlay, keys addressed with `pad_addr`.
    Overlay(OverlayFrame),
    /// `serve` ends: show the layout colours without overlay and stop.
    Shutdown,
}

/// One board's pads to send, with the control buttons to repaint after it.
#[derive(Debug, Clone, PartialEq)]
pub struct Snapshot {
    pub board: u8,
    pub colors: PadColors,
    pub ctrl: Vec<(u8, Color)>,
}

#[derive(Default)]
struct PadBoard {
    /// What the board shows (the last snapshot sent).
    sent: Option<PadColors>,
    last_send: Option<Instant>,
}

pub struct LedState {
    comp: Compositor,
    /// Boards that have layout colours. A board without a section in the layout is never
    /// sent a snapshot (it keeps its own colours, as without `--api`).
    pads: BTreeMap<u8, PadBoard>,
    ctrl: BTreeMap<(u8, u8), Color>,
}

impl LedState {
    pub fn new() -> Self {
        Self { comp: Compositor::new(), pads: BTreeMap::new(), ctrl: BTreeMap::new() }
    }

    /// The layout colours of `board`, to be sent.
    pub fn set_pads(&mut self, board: u8, colors: &PadColors) {
        for (pad, c) in colors.iter().enumerate() {
            let _ = self.comp.set_base(pad_addr(board, pad as u8), rgb(*c));
        }
        self.pads.entry(board).or_default();
    }

    /// The layout colours of `board` that the caller has already sent itself.
    pub fn seed_pads(&mut self, board: u8, colors: &PadColors) {
        self.set_pads(board, colors);
        let shown = self.frame(board);
        if let Some(p) = self.pads.get_mut(&board) {
            p.sent = Some(shown);
        }
    }

    pub fn set_ctrl(&mut self, board: u8, cc: u8, color: Color) {
        self.ctrl.insert((board, cc), color);
    }

    pub fn set_overlay(&mut self, frame: &OverlayFrame) {
        // which LEDs change is of no interest here: boards are compared as a whole
        let mut changed = Vec::new();
        self.comp.set_overlay(frame, &mut changed);
    }

    /// What `board` should show now: overlay above the (possibly dimmed) layout colours.
    pub fn frame(&self, board: u8) -> PadColors {
        let mut out = [Color::new(0, 0, 0); PADS];
        for (pad, c) in out.iter_mut().enumerate() {
            let (r, g, b) = self.comp.shown(pad_addr(board, pad as u8));
            *c = Color::new(r, g, b);
        }
        out
    }

    fn snapshot(&self, board: u8, colors: PadColors) -> Snapshot {
        let ctrl = self.ctrl.range((board, 0)..=(board, u8::MAX)).map(|(&(_, cc), &c)| (cc, c)).collect();
        Snapshot { board, colors, ctrl }
    }

    /// The snapshots to send now: boards that should show something else than they do and
    /// had no snapshot within `MIN_SNAPSHOT_INTERVAL` (`force`: regardless of that). They
    /// count as sent.
    pub fn due(&mut self, now: Instant, force: bool) -> Vec<Snapshot> {
        let mut out = Vec::new();
        let boards: Vec<u8> = self.pads.keys().copied().collect();
        for board in boards {
            let want = self.frame(board);
            let p = &self.pads[&board];
            if p.sent == Some(want) {
                continue;
            }
            if !force && p.last_send.is_some_and(|t| now.saturating_duration_since(t) < MIN_SNAPSHOT_INTERVAL) {
                continue;
            }
            out.push(self.snapshot(board, want));
            if let Some(p) = self.pads.get_mut(&board) {
                p.sent = Some(want);
                p.last_send = Some(now);
            }
        }
        out
    }

    /// How long until `due` has something to send; None: every board shows what it should.
    pub fn next_due(&self, now: Instant) -> Option<Duration> {
        self.pads
            .iter()
            .filter(|(board, p)| p.sent != Some(self.frame(**board)))
            .map(|(_, p)| match p.last_send {
                Some(t) => MIN_SNAPSHOT_INTERVAL.saturating_sub(now.saturating_duration_since(t)),
                None => Duration::ZERO,
            })
            .min()
    }
}

// --- the worker ---

/// The LED worker thread. Dropping it shows the layout colours again and waits for the
/// thread, so no overlay stays on the pads after `serve`.
pub struct LedWorker {
    tx: Sender<LedMsg>,
    join: Option<JoinHandle<()>>,
}

impl LedWorker {
    /// `devices`: (board, its device). `state`: what the boards show now.
    pub fn spawn(state: LedState, devices: Vec<(u8, ExquisDevice)>) -> std::io::Result<Self> {
        let (tx, rx) = crossbeam_channel::unbounded();
        let join = std::thread::Builder::new().name("exquis-leds".into()).spawn(move || run(state, devices, rx))?;
        Ok(Self { tx, join: Some(join) })
    }

    pub fn sender(&self) -> Sender<LedMsg> {
        self.tx.clone()
    }
}

impl Drop for LedWorker {
    fn drop(&mut self) {
        let _ = self.tx.send(LedMsg::Shutdown);
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

fn send(devices: &[(u8, ExquisDevice)], board: u8, bytes: &[u8]) {
    if let Some((_, device)) = devices.iter().find(|(b, _)| *b == board) {
        let _ = send_to_outputs(std::slice::from_ref(device), DeviceSelection::All, bytes);
    }
}

fn send_snapshot(devices: &[(u8, ExquisDevice)], s: &Snapshot) {
    let mut pads = [(0u8, Color::new(0, 0, 0)); PADS];
    for (i, p) in pads.iter_mut().enumerate() {
        *p = (i as u8, s.colors[i]);
    }
    let dev_mode = proto::enter_dev_mode(proto::DEV_MASK_NO_PADS);
    send(devices, s.board, &dev_mode);
    send(devices, s.board, &proto::snapshot_set_pads(&pads));
    // the snapshot may end developer mode; the buttons are painted again after it
    send(devices, s.board, &dev_mode);
    for &(cc, color) in &s.ctrl {
        send(devices, s.board, &proto::set_led_color(cc, color));
    }
}

/// Returns false on `Shutdown`.
fn apply(state: &mut LedState, devices: &[(u8, ExquisDevice)], msg: LedMsg) -> bool {
    match msg {
        LedMsg::Pads { board, colors } => state.set_pads(board, &colors),
        LedMsg::Ctrl { board, cc, color } => {
            state.set_ctrl(board, cc, color);
            send(devices, board, &proto::set_led_color(cc, color));
        }
        LedMsg::Overlay(frame) => state.set_overlay(&frame),
        LedMsg::Shutdown => return false,
    }
    true
}

fn run(mut state: LedState, devices: Vec<(u8, ExquisDevice)>, rx: Receiver<LedMsg>) {
    let mut running = true;
    while running {
        let wait = state.next_due(Instant::now()).unwrap_or(Duration::from_secs(3600));
        match rx.recv_timeout(wait) {
            Ok(msg) => {
                running = apply(&mut state, &devices, msg);
                // everything that is waiting goes into the same snapshot
                while running {
                    match rx.try_recv() {
                        Ok(msg) => running = apply(&mut state, &devices, msg),
                        Err(_) => break,
                    }
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => running = false,
        }
        if !running {
            state.set_overlay(&OverlayFrame { keys: Vec::new(), dim_base: 1.0 });
        }
        for s in state.due(Instant::now(), !running) {
            send_snapshot(&devices, &s);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RED: Color = Color::new(100, 0, 0);
    const GREEN: Color = Color::new(0, 100, 0);
    const HINT: Color = Color::new(0, 0, 127);
    const VIOLET: Color = Color::new(64, 0, 96);

    fn overlay(keys: &[(u8, u8, Color)], dim_base: f32) -> OverlayFrame {
        OverlayFrame { keys: keys.iter().map(|&(b, p, c)| (pad_addr(b, p), rgb(c))).collect(), dim_base }
    }

    /// Boards 0, 1 and 3 showing their layout colours (board b: all pads RED, pad 7 GREEN).
    fn painted() -> (LedState, PadColors) {
        let mut base = [RED; PADS];
        base[7] = GREEN;
        let mut s = LedState::new();
        for b in [0u8, 1, 3] {
            s.seed_pads(b, &base);
            s.set_ctrl(b, 100, VIOLET);
        }
        (s, base)
    }

    #[test]
    fn nothing_is_sent_while_the_boards_show_what_they_should() {
        let (mut s, base) = painted();
        let t0 = Instant::now();
        assert_eq!(s.next_due(t0), None);
        assert!(s.due(t0, false).is_empty());
        assert!(s.due(t0, true).is_empty());
        // the same layout colours again, an empty overlay, a dim value without overlay keys
        s.set_pads(1, &base);
        s.set_overlay(&overlay(&[], 0.2));
        assert!(s.due(t0, false).is_empty());
    }

    #[test]
    fn an_overlay_key_costs_one_snapshot_of_its_board_and_clearing_restores_the_layout_exactly() {
        let (mut s, base) = painted();
        let t0 = Instant::now();
        s.set_overlay(&overlay(&[(1, 7, HINT), (1, 60, HINT)], 1.0));
        assert_eq!(s.next_due(t0), Some(Duration::ZERO));
        let due = s.due(t0, false);
        assert_eq!(due.len(), 1, "boards 0 and 3 are not touched");
        let mut want = base;
        want[7] = HINT;
        want[60] = HINT;
        assert_eq!(due[0], Snapshot { board: 1, colors: want, ctrl: vec![(100, VIOLET)] });
        assert!(s.due(t0, false).is_empty(), "sent once");
        // pad 7 cleared: its layout colour (GREEN) is back, pad 60 stays
        let t1 = t0 + MIN_SNAPSHOT_INTERVAL;
        s.set_overlay(&overlay(&[(1, 60, HINT)], 1.0));
        let due = s.due(t1, false);
        assert_eq!(due.len(), 1);
        assert_eq!((due[0].colors[7], due[0].colors[60], due[0].colors[0]), (GREEN, HINT, RED));
        // all cleared: exactly the layout colours
        s.set_overlay(&overlay(&[], 1.0));
        let due = s.due(t1 + MIN_SNAPSHOT_INTERVAL, false);
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].colors, base);
    }

    #[test]
    fn dim_base_dims_all_boards_while_any_overlay_key_is_set() {
        let (mut s, base) = painted();
        let t0 = Instant::now();
        s.set_overlay(&overlay(&[(3, 0, HINT)], 0.25));
        let due = s.due(t0, false);
        assert_eq!(due.iter().map(|d| d.board).collect::<Vec<_>>(), vec![0, 1, 3]);
        for d in &due {
            assert_eq!(d.colors[1], Color::new(25, 0, 0));
            assert_eq!(d.colors[7], Color::new(0, 25, 0));
        }
        assert_eq!(due[2].colors[0], HINT, "the overlay colour itself is not dimmed");
        assert_eq!(due[0].colors[0], Color::new(25, 0, 0));
        // the hint moves to board 0: boards 0 and 3 change, board 1 stays as it is
        let t1 = t0 + MIN_SNAPSHOT_INTERVAL;
        s.set_overlay(&overlay(&[(0, 0, HINT)], 0.25));
        assert_eq!(s.due(t1, false).iter().map(|d| d.board).collect::<Vec<_>>(), vec![0, 3]);
        // overlay gone: every board shows its layout colours at full brightness
        s.set_overlay(&overlay(&[], 0.25));
        let due = s.due(t1 + MIN_SNAPSHOT_INTERVAL, false);
        assert_eq!(due.len(), 3);
        assert!(due.iter().all(|d| d.colors == base));
    }

    #[test]
    fn a_layout_change_under_the_overlay_keeps_the_overlay() {
        let (mut s, _) = painted();
        let t0 = Instant::now();
        s.set_overlay(&overlay(&[(0, 5, HINT)], 0.5));
        let _ = s.due(t0, false);
        // serve repaints board 0 with another layout: the hint stays, the rest is the new
        // layout, dimmed
        let new = [GREEN; PADS];
        s.set_pads(0, &new);
        let due = s.due(t0 + MIN_SNAPSHOT_INTERVAL, false);
        assert_eq!(due.len(), 1);
        assert_eq!((due[0].colors[5], due[0].colors[6]), (HINT, Color::new(0, 50, 0)));
        // overlay cleared: the new layout, not the old one
        s.set_overlay(&overlay(&[], 0.5));
        let due = s.due(t0 + 2 * MIN_SNAPSHOT_INTERVAL, false);
        assert_eq!(due.iter().find(|d| d.board == 0).unwrap().colors, new);
    }

    #[test]
    fn snapshots_to_a_board_are_spaced_and_changes_in_between_are_merged() {
        let (mut s, base) = painted();
        let t0 = Instant::now();
        let ms = Duration::from_millis;
        s.set_overlay(&overlay(&[(0, 1, HINT)], 1.0));
        assert_eq!(s.due(t0, false).len(), 1);
        // three changes within the interval: nothing goes out ...
        for (i, pad) in [2u8, 3, 4].into_iter().enumerate() {
            s.set_overlay(&overlay(&[(0, pad, HINT)], 1.0));
            let t = t0 + ms(10 * (i as u64 + 1));
            assert!(s.due(t, false).is_empty());
            assert_eq!(s.next_due(t), Some(MIN_SNAPSHOT_INTERVAL - ms(10 * (i as u64 + 1))));
        }
        // ... a board that had no snapshot yet is not held back
        s.set_overlay(&overlay(&[(0, 4, HINT), (1, 4, HINT)], 1.0));
        assert_eq!(s.due(t0 + ms(35), false).iter().map(|d| d.board).collect::<Vec<_>>(), vec![1]);
        // ... and when the interval is over, one snapshot with the latest state
        let due = s.due(t0 + MIN_SNAPSHOT_INTERVAL, false);
        assert_eq!(due.len(), 1);
        let mut want = base;
        want[4] = HINT;
        assert_eq!((due[0].board, due[0].colors), (0, want));
        assert_eq!(s.next_due(t0 + MIN_SNAPSHOT_INTERVAL), None);
        // back and forth within the interval ends where it started: nothing to send
        s.set_overlay(&overlay(&[(0, 9, HINT), (1, 4, HINT)], 1.0));
        s.set_overlay(&overlay(&[(0, 4, HINT), (1, 4, HINT)], 1.0));
        assert_eq!(s.next_due(t0 + ms(60)), None);
        // shutdown: the overlay goes at once, whatever the interval says
        s.set_overlay(&overlay(&[], 1.0));
        let due = s.due(t0 + ms(61), true);
        assert_eq!(due.len(), 2);
        assert!(due.iter().all(|d| d.colors == base));
    }

    #[test]
    fn a_board_without_layout_colours_gets_no_snapshot() {
        let (mut s, _) = painted();
        s.set_ctrl(2, 100, VIOLET);
        s.set_overlay(&overlay(&[(2, 0, HINT), (0, 200, HINT)], 1.0));
        assert!(s.due(Instant::now(), true).is_empty());
    }

    #[test]
    fn the_buttons_painted_last_follow_every_snapshot_of_their_board() {
        let (mut s, _) = painted();
        s.set_ctrl(0, 107, Color::new(127, 127, 0));
        s.set_ctrl(0, 100, Color::new(127, 127, 127));
        s.set_ctrl(1, 106, RED);
        s.set_overlay(&overlay(&[(0, 0, HINT)], 1.0));
        let due = s.due(Instant::now(), false);
        assert_eq!(due[0].ctrl, vec![(100, Color::new(127, 127, 127)), (107, Color::new(127, 127, 0))]);
    }

    #[test]
    fn the_worker_stops_when_dropped() {
        // no devices: nothing is sent, but the thread must take messages and end
        let (state, base) = painted();
        let w = LedWorker::spawn(state, Vec::new()).unwrap();
        let tx = w.sender();
        tx.send(LedMsg::Overlay(overlay(&[(0, 0, HINT)], 0.3))).unwrap();
        tx.send(LedMsg::Pads { board: 0, colors: Box::new(base) }).unwrap();
        tx.send(LedMsg::Ctrl { board: 0, cc: 100, color: VIOLET }).unwrap();
        drop(w);
        assert!(tx.send(LedMsg::Shutdown).is_err(), "the worker is gone");
    }
}
