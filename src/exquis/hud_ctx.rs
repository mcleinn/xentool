//! Mutable context that the Exquis serve UI loop uses to build [`LiveState`]
//! snapshots and submit them to the HUD publisher.
//!
//! Held behind `Rc<RefCell<...>>` because both the UI loop (which reads it
//! to build snapshots) and the layout-cycle closure (which mutates it on
//! `Settings`-button presses) need access. Single-threaded — the UI loop
//! and the closure run on the same thread.

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::rc::Rc;
use std::time::{Duration, Instant};

use crate::exquis::mpe::TouchSummary;
use crate::exquis::ui::ServeDisplay;
use crate::hud::{HudPublisher, LayoutInfo, LiveState, ModeInfo, SCHEMA_VERSION};
use crate::xtn::XtnLayout;

/// Base octave shift baked into Exquis tuning construction. Mirrors the
/// `2 + new_shift` convention in `cmd_serve` so HUD-reported absolute pitches
/// match what the synth hears.
pub const BASE_OCTAVE_SHIFT: i32 = 2;

pub struct HudExquisCtx {
    pub publisher: HudPublisher,
    pub layout: XtnLayout,
    pub layout_id: String,
    pub layout_name: String,
    pub edo: i32,
    pub pitch_offset: i32,
    /// `device_number` → board name (`"board0"`, etc.). Stable for the
    /// lifetime of the serve loop.
    pub device_to_board: BTreeMap<usize, String>,
    /// EDO steps per raw 14-bit x unit, per EDO division at 12 — multiply by
    /// the *current* `self.edo` at use time (edo changes on layout cycle).
    /// Retune mode: x_gain * pb_range / 8192.0 / 12.0. MTS-ESP mode: 0.0
    /// (bend passes through un-amplified; the synth's PB range is unknowable,
    /// so bend display is disabled).
    pub bend_steps_per_x: f64,
    /// Hysteresis + dedup runtime state. Default-initialized.
    pub runtime: HudRuntime,
}

#[derive(Default)]
pub struct HudRuntime {
    /// (device, channel, note) → currently displayed step offset. Rebuilt on
    /// every submit; entries for released touches are pruned by the swap.
    bend_read: HashMap<(usize, u8, u8), i32>,
    /// Scratch map swapped with `bend_read` each rebuild (keeps capacity,
    /// avoids alloc churn in the hot path).
    bend_write: HashMap<(usize, u8, u8), i32>,
    last: Option<LastSubmit>,
}

/// Fingerprint of the last actually-submitted state, for the dedup gate.
struct LastSubmit {
    pressed: BTreeMap<String, Vec<i32>>,
    bent: BTreeMap<String, Vec<i32>>,
    /// Sorted snapshot of `display.shifts` — MUST be included: an octave
    /// shift on a device with no held notes changes only `layout_pitches`.
    shifts: Vec<(usize, i32)>,
    layout_id: String,
    edo: i32,
    pitch_offset: i32,
    at: Instant,
}

/// Margin (in EDO steps) past the step midpoint before the displayed step
/// switches. Prevents label flicker when x jitters at a boundary.
const BEND_HYSTERESIS_STEPS: f64 = 0.1;

/// Given the previously displayed step offset and the current continuous
/// offset (in EDO steps), return the new displayed step offset.
/// From prev=0, switching to 1 requires off > 0.6; once at 1, returning to 0
/// requires off < 0.4. Large jumps resolve in one call via round()
/// (e.g. prev=0, off=1.7 → 2).
pub(crate) fn displayed_step_offset(prev: i32, off_steps: f64) -> i32 {
    if (off_steps - prev as f64).abs() <= 0.5 + BEND_HYSTERESIS_STEPS {
        prev
    } else {
        off_steps.round() as i32
    }
}

pub type HudExquisHandle = Rc<RefCell<HudExquisCtx>>;

impl HudExquisCtx {
    pub fn into_handle(self) -> HudExquisHandle {
        Rc::new(RefCell::new(self))
    }

    /// Recompute layout pitches per board, applying the current per-device
    /// octave shift held in `display.shifts`. Called from `submit_state` for
    /// every snapshot so the frontend can prefetch xenharm names that match
    /// the *current* shift.
    fn build_layout_pitches(&self, display: &ServeDisplay) -> BTreeMap<String, Vec<Option<i32>>> {
        let mut out: BTreeMap<String, Vec<Option<i32>>> = BTreeMap::new();
        for (&dev_num, board_name) in &self.device_to_board {
            let shift = display.shifts.get(&dev_num).copied().unwrap_or(0);
            let octave_shift = BASE_OCTAVE_SHIFT + shift;
            let mut row = vec![None; 61];
            if let Some(bl) = self.layout.boards.get(board_name) {
                for (&pad_idx, entry) in &bl.pads {
                    if (pad_idx as usize) >= row.len() {
                        continue;
                    }
                    let abs = (entry.chan as i32 - 1) * self.edo
                        + entry.key as i32
                        + self.pitch_offset
                        + octave_shift * self.edo;
                    row[pad_idx as usize] = Some(abs);
                }
            }
            out.insert(board_name.clone(), row);
        }
        out
    }
}

/// Build a [`LiveState`] from the ctx + currently-active touches + the
/// serve display, then submit it to the HUD publisher.
///
/// Pitches in `pressed` are the *displayed* pitches: when a touch's X bend
/// crosses a step midpoint (with hysteresis), the rounded neighboring step
/// is reported instead of the pad's base pitch, and that displayed pitch is
/// also listed in `bent`. A dedup gate skips the expensive snapshot build
/// (and the seq-advancing submit) when nothing displayed changed — but lets
/// a submit through whenever ≥250 ms passed since the last real one, so the
/// SSE thread keeps re-emitting for the OSC param strip.
pub fn submit_state(handle: &HudExquisHandle, touches: &[TouchSummary], display: &ServeDisplay) {
    let mut ctx = handle.borrow_mut();
    let ctx = &mut *ctx;

    // ---- cheap path: pressed + bent + hysteresis (≤15 touches) ----
    let steps_per_x = ctx.bend_steps_per_x * ctx.edo as f64; // 0.0 disables bend display
    let bend_on = steps_per_x != 0.0;

    let mut pressed: BTreeMap<String, Vec<i32>> = BTreeMap::new();
    for board_name in ctx.device_to_board.values() {
        pressed.insert(board_name.clone(), Vec::new());
    }
    // `bent` stays sparse (no pre-inserted empty boards) so
    // skip_serializing_if fires when nothing is bent.
    let mut bent: BTreeMap<String, Vec<i32>> = BTreeMap::new();

    // Swap maps: previous offsets are read from the old read side (now in
    // `bend_write`); the rebuilt set is written to `bend_read`. Released
    // touches simply never get re-inserted — pruned for free.
    std::mem::swap(&mut ctx.runtime.bend_read, &mut ctx.runtime.bend_write);
    ctx.runtime.bend_read.clear();

    for t in touches {
        let board_name = match ctx.device_to_board.get(&t.device) {
            Some(n) => n,
            None => continue,
        };
        let abs = match t.abs_pitch {
            Some(a) => a,
            // MTS-ESP path doesn't pre-compute abs_pitch on TouchSummary, so
            // derive it from the layout.
            None => match abs_pitch_from_layout(&*ctx, board_name, t.note, display) {
                Some(a) => a,
                None => continue,
            },
        };
        let mut shown = abs;
        if bend_on && t.x != 0 {
            let key = (t.device, t.channel, t.note);
            let prev = ctx.runtime.bend_write.get(&key).copied().unwrap_or(0);
            // NOTE: the retuner clamps its combined output bend to 0..16383
            // (tuning.rs); that saturation is not modeled here (<2 % worst-
            // case clipping per CLAUDE.md).
            let off = displayed_step_offset(prev, t.x as f64 * steps_per_x);
            if off != 0 {
                ctx.runtime.bend_read.insert(key, off);
                shown = abs + off;
                // If an unbent touch and a bent touch collapse to the same
                // displayed pitch, that pitch shows as bent ("any
                // contributing touch bent" rule) — not disambiguated.
                bent.entry(board_name.clone()).or_default().push(shown);
            }
        }
        pressed.entry(board_name.clone()).or_default().push(shown);
    }

    // ---- dedup gate ----
    let mut shifts: Vec<(usize, i32)> =
        display.shifts.iter().map(|(&d, &s)| (d, s)).collect();
    shifts.sort_unstable();
    if let Some(last) = &ctx.runtime.last {
        // The 250 ms force-through keeps the SSE OSC strip fresh: the server
        // only re-emits when seq advances.
        if last.at.elapsed() < Duration::from_millis(250)
            && last.pressed == pressed
            && last.bent == bent
            && last.shifts == shifts
            && last.layout_id == ctx.layout_id
            && last.edo == ctx.edo
            && last.pitch_offset == ctx.pitch_offset
        {
            return;
        }
    }

    // ---- expensive path: full snapshot build ----
    // Single mode.octave_shift on the wire shape — pick board0's shift if
    // present, else the lowest device's shift. Per-board shifts are still
    // reflected in layout_pitches (each board's pitches are computed with
    // its own shift), so chord/note labels stay correct.
    let octave_shift_i32 = ctx
        .device_to_board
        .iter()
        .find(|(_, name)| name.as_str() == "board0")
        .and_then(|(d, _)| display.shifts.get(d).copied())
        .or_else(|| {
            ctx.device_to_board
                .keys()
                .next()
                .and_then(|d| display.shifts.get(d).copied())
        })
        .unwrap_or(0);
    let octave_shift = octave_shift_i32.clamp(i8::MIN as i32, i8::MAX as i32) as i8;

    let layout_pitches = ctx.build_layout_pitches(display);

    let state = LiveState {
        version: SCHEMA_VERSION,
        seq: 0,
        ts_ms: 0,
        layout: LayoutInfo {
            id: ctx.layout_id.clone(),
            name: ctx.layout_name.clone(),
            edo: ctx.edo.max(0) as u32,
            pitch_offset: ctx.pitch_offset,
        },
        mode: ModeInfo {
            backend: "exquis",
            octave_shift,
            press_threshold: None,
            aftertouch: None,
            aftertouch_speed_max: None,
            velocity_profile: None,
        },
        pressed: pressed.clone(),
        bent: bent.clone(),
        layout_pitches,
    };

    ctx.runtime.last = Some(LastSubmit {
        pressed,
        bent,
        shifts,
        layout_id: ctx.layout_id.clone(),
        edo: ctx.edo,
        pitch_offset: ctx.pitch_offset,
        at: Instant::now(),
    });
    ctx.publisher.submit(state);
}

fn abs_pitch_from_layout(
    ctx: &HudExquisCtx,
    board_name: &str,
    note: u8,
    display: &ServeDisplay,
) -> Option<i32> {
    let bl = ctx.layout.boards.get(board_name)?;
    let entry = bl.pads.get(&note)?;
    let dev_num = ctx
        .device_to_board
        .iter()
        .find(|(_, name)| name.as_str() == board_name)
        .map(|(d, _)| *d)?;
    let shift = display.shifts.get(&dev_num).copied().unwrap_or(0);
    let octave_shift = BASE_OCTAVE_SHIFT + shift;
    Some((entry.chan as i32 - 1) * ctx.edo + entry.key as i32 + ctx.pitch_offset + octave_shift * ctx.edo)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exquis::proto::Color;
    use crate::xtn::{BoardLayout, PadEntry};
    use std::collections::HashMap as StdHashMap;

    // ---- hysteresis table ----

    #[test]
    fn hysteresis_table() {
        let cases: &[(i32, f64, i32)] = &[
            (0, 0.55, 0),
            (0, 0.61, 1),
            (1, 0.45, 1), // sticky inside band
            (1, 0.39, 0),
            (0, 1.7, 2), // fast multi-step
            (2, 0.0, 0),
            (0, -0.61, -1), // negative symmetry
        ];
        for &(prev, off, want) in cases {
            assert_eq!(
                displayed_step_offset(prev, off),
                want,
                "prev={prev} off={off}"
            );
        }
    }

    // ---- end-to-end submit/dedup ----

    const BASE: i32 = 60;
    const EDO: i32 = 31;

    fn fixture(bend_steps_per_x: f64) -> (HudExquisHandle, ServeDisplay) {
        let publisher = HudPublisher::new(LiveState::empty("exquis"));
        let mut pads = StdHashMap::new();
        pads.insert(
            30u8,
            PadEntry {
                key: 5,
                chan: 3,
                color: Color::new(0, 0, 0),
            },
        );
        let mut boards = StdHashMap::new();
        boards.insert("board0".to_string(), BoardLayout { pads });
        let layout = XtnLayout {
            edo: Some(EDO),
            pitch_offset: 0,
            boards,
        };
        let mut device_to_board = BTreeMap::new();
        device_to_board.insert(0usize, "board0".to_string());
        let ctx = HudExquisCtx {
            publisher,
            layout,
            layout_id: "test".into(),
            layout_name: "test".into(),
            edo: EDO,
            pitch_offset: 0,
            device_to_board,
            bend_steps_per_x,
            runtime: Default::default(),
        };
        let display = ServeDisplay {
            tuning_name: "t".into(),
            shifts: StdHashMap::from([(0usize, 0i32)]),
        };
        (ctx.into_handle(), display)
    }

    fn touch(x: i16) -> TouchSummary {
        TouchSummary {
            device: 0,
            channel: 2,
            note: 30,
            velocity: 100,
            x,
            y: 0,
            z: 0,
            age: Duration::from_millis(0),
            freq_hz: None,
            v_chan: None,
            v_key: None,
            abs_pitch: Some(BASE),
        }
    }

    /// `bend_steps_per_x` such that steps_per_x = 0.01 per raw x unit at EDO.
    fn per_x() -> f64 {
        0.01 / EDO as f64
    }

    #[test]
    fn submit_bend_rounding_and_dedup() {
        let (handle, display) = fixture(per_x());
        let publisher = handle.borrow().publisher.clone();

        // 1. First submit, unbent → seq advances, pressed = [BASE], no bent.
        submit_state(&handle, &[touch(0)], &display);
        assert_eq!(publisher.current_seq(), 1);
        let snap = publisher.snapshot();
        assert_eq!(snap.pressed["board0"], vec![BASE]);
        assert!(snap.bent.is_empty());

        // 2. Identical resubmit → deduped, seq unchanged.
        submit_state(&handle, &[touch(0)], &display);
        assert_eq!(publisher.current_seq(), 1);

        // 3. x=40 → 0.4 steps, below switch threshold → still BASE, deduped.
        submit_state(&handle, &[touch(40)], &display);
        assert_eq!(publisher.current_seq(), 1);

        // 4. x=70 → 0.7 steps → displayed step +1, bent reported.
        submit_state(&handle, &[touch(70)], &display);
        assert_eq!(publisher.current_seq(), 2);
        let snap = publisher.snapshot();
        assert_eq!(snap.pressed["board0"], vec![BASE + 1]);
        assert_eq!(snap.bent["board0"], vec![BASE + 1]);

        // 5. x=50 → 0.5 steps, prev=1, inside hysteresis band → sticky at +1,
        //    deduped.
        submit_state(&handle, &[touch(50)], &display);
        assert_eq!(publisher.current_seq(), 2);
        assert_eq!(publisher.snapshot().pressed["board0"], vec![BASE + 1]);
    }

    #[test]
    fn release_prunes_hysteresis_state() {
        let (handle, display) = fixture(per_x());
        let publisher = handle.borrow().publisher.clone();

        submit_state(&handle, &[touch(70)], &display);
        assert_eq!(publisher.snapshot().pressed["board0"], vec![BASE + 1]);

        // 6. Release all → seq advances, bent gone.
        submit_state(&handle, &[], &display);
        let snap = publisher.snapshot();
        assert!(snap.pressed["board0"].is_empty());
        assert!(snap.bent.is_empty());

        // Re-press with x=40 → hysteresis key was pruned, so prev=0 and
        // 0.4 steps stays at BASE (not sticky at +1).
        submit_state(&handle, &[touch(40)], &display);
        let snap = publisher.snapshot();
        assert_eq!(snap.pressed["board0"], vec![BASE]);
        assert!(snap.bent.is_empty());
    }

    #[test]
    fn shift_change_passes_dedup_gate() {
        let (handle, mut display) = fixture(per_x());
        let publisher = handle.borrow().publisher.clone();

        submit_state(&handle, &[], &display);
        let seq = publisher.current_seq();

        // 7. Octave shift with no held notes changes only layout_pitches —
        //    the fingerprint must let it through.
        display.shifts.insert(0, 1);
        submit_state(&handle, &[], &display);
        assert_eq!(publisher.current_seq(), seq + 1);
        let snap = publisher.snapshot();
        assert_eq!(snap.mode.octave_shift, 1);
        // Pad 30: (chan-1)*edo + key + (BASE_OCTAVE_SHIFT + shift)*edo
        let want = 2 * EDO + 5 + (BASE_OCTAVE_SHIFT + 1) * EDO;
        assert_eq!(snap.layout_pitches["board0"][30], Some(want));
    }

    #[test]
    fn mts_mode_disables_bend_display() {
        // 8. bend_steps_per_x = 0.0 (MTS-ESP mode) → even a huge x changes
        //    nothing.
        let (handle, display) = fixture(0.0);
        let publisher = handle.borrow().publisher.clone();

        submit_state(&handle, &[touch(8000)], &display);
        let snap = publisher.snapshot();
        assert_eq!(snap.pressed["board0"], vec![BASE]);
        assert!(snap.bent.is_empty());
    }
}
