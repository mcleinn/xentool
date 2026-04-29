//! Mutable context that the Exquis serve UI loop uses to build [`LiveState`]
//! snapshots and submit them to the HUD publisher.
//!
//! Held behind `Rc<RefCell<...>>` because both the UI loop (which reads it
//! to build snapshots) and the layout-cycle closure (which mutates it on
//! `Settings`-button presses) need access. Single-threaded — the UI loop
//! and the closure run on the same thread.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

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
/// Cheap when `--hud` is off (the publisher's `submit` is wait-free), but
/// the caller skips this entirely in that case via `Option<HudExquisHandle>`.
pub fn submit_state(handle: &HudExquisHandle, touches: &[TouchSummary], display: &ServeDisplay) {
    let ctx = handle.borrow();

    // Pressed: bucket each touch's abs_pitch by board name.
    let mut pressed: BTreeMap<String, Vec<i32>> = BTreeMap::new();
    for board_name in ctx.device_to_board.values() {
        pressed.insert(board_name.clone(), Vec::new());
    }
    for t in touches {
        let board_name = match ctx.device_to_board.get(&t.device) {
            Some(n) => n,
            None => continue,
        };
        let abs = match t.abs_pitch {
            Some(a) => a,
            // MTS-ESP path doesn't pre-compute abs_pitch on TouchSummary, so
            // derive it from the layout.
            None => match abs_pitch_from_layout(&ctx, board_name, t.note, display) {
                Some(a) => a,
                None => continue,
            },
        };
        pressed.entry(board_name.clone()).or_default().push(abs);
    }

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
        pressed,
        layout_pitches,
    };
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
