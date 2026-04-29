//! Mutable context that the Wooting serve hot loop uses to build [`LiveState`]
//! snapshots and submit them to the HUD publisher.
//!
//! Mirrors the Exquis equivalent in `src/exquis/hud_ctx.rs`. Held behind
//! `Rc<RefCell<...>>` so the layout-cycle path (Context Menu key) can mutate
//! it while the snapshot site reads it. Single-threaded — both run on the
//! Wooting hot loop thread.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

use crate::hud::{HudPublisher, LayoutInfo, LiveState, ModeInfo, SCHEMA_VERSION};
use crate::wooting::wtn::Wtn;

pub struct HudWootingCtx {
    pub publisher: HudPublisher,
    pub layout_id: String,
    pub layout_name: String,
    pub edo: i32,
    pub pitch_offset: i32,
    /// `"board0"` → 56-element vec of `Option<abs_pitch>`. None for cells with
    /// `chan == 0` (the .wtn file's "missing / not set" marker).
    pub layout_pitches: BTreeMap<String, Vec<Option<i32>>>,
}

pub type HudWootingHandle = Rc<RefCell<HudWootingCtx>>;

impl HudWootingCtx {
    pub fn into_handle(self) -> HudWootingHandle {
        Rc::new(RefCell::new(self))
    }
}

/// Compute per-board absolute-pitch lists from a `.wtn`. Boards are keyed
/// `"board0"`, `"board1"`, etc. so the wire shape stays consistent with the
/// Exquis backend.
pub fn build_layout_pitches(wtn: &Wtn) -> BTreeMap<String, Vec<Option<i32>>> {
    let edo = wtn.edo.unwrap_or(12);
    let mut out: BTreeMap<String, Vec<Option<i32>>> = BTreeMap::new();
    let mut board_keys: Vec<u8> = wtn.boards.keys().copied().collect();
    board_keys.sort();
    for b in board_keys {
        let cells = match wtn.boards.get(&b) {
            Some(c) => c,
            None => continue,
        };
        let mut row = Vec::with_capacity(cells.len());
        for cell in cells {
            if cell.chan == 0 {
                row.push(None);
            } else {
                let abs = (cell.chan as i32 - 1) * edo + cell.key as i32 + wtn.pitch_offset;
                row.push(Some(abs));
            }
        }
        out.insert(format!("board{b}"), row);
    }
    out
}

pub struct HudWootingMode {
    pub octave_shift: i8,
    pub press_threshold: f32,
    pub aftertouch: String,
    pub aftertouch_speed_max: f32,
    pub velocity_profile: String,
}

/// Bucket currently-held keys by board name and compute their absolute
/// pitches. Each entry in `held` is `(wtn_board, out_ch, note)` — pulled
/// straight from `KeyState::Held` in the hot loop.
///
/// `abs_pitch = out_ch * edo + note + pitch_offset` — see the Wooting cell
/// resolution in `src/wooting/serve.rs` (`out_ch` already encodes
/// `chan-1 + octave_shift + octave_hold`).
pub fn pressed_from_held(
    held: impl Iterator<Item = (u8, u8, u8)>,
    edo: i32,
    pitch_offset: i32,
    boards_present: &[u8],
) -> BTreeMap<String, Vec<i32>> {
    let mut pressed: BTreeMap<String, Vec<i32>> = BTreeMap::new();
    for &b in boards_present {
        pressed.insert(format!("board{b}"), Vec::new());
    }
    for (wtn_board, out_ch, note) in held {
        let abs = out_ch as i32 * edo + note as i32 + pitch_offset;
        pressed
            .entry(format!("board{wtn_board}"))
            .or_default()
            .push(abs);
    }
    pressed
}

pub fn submit_state(
    handle: &HudWootingHandle,
    pressed: BTreeMap<String, Vec<i32>>,
    mode: HudWootingMode,
) {
    let ctx = handle.borrow();
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
            backend: "wooting",
            octave_shift: mode.octave_shift,
            press_threshold: Some(mode.press_threshold),
            aftertouch: Some(mode.aftertouch),
            aftertouch_speed_max: Some(mode.aftertouch_speed_max),
            velocity_profile: Some(mode.velocity_profile),
        },
        pressed,
        layout_pitches: ctx.layout_pitches.clone(),
    };
    ctx.publisher.submit(state);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wooting::wtn::WtnCell;
    use std::collections::HashMap;

    #[test]
    fn pressed_from_held_buckets_by_board() {
        let held = vec![
            (0u8, 1u8, 60u8), // board0, ch=2 (out_ch=1), note=60
            (1u8, 2u8, 62u8), // board1, ch=3, note=62
            (0u8, 1u8, 64u8), // board0 again
        ];
        let pressed = pressed_from_held(held.into_iter(), 31, 0, &[0, 1]);
        assert_eq!(pressed["board0"], vec![1 * 31 + 60, 1 * 31 + 64]);
        assert_eq!(pressed["board1"], vec![2 * 31 + 62]);
    }

    #[test]
    fn build_layout_pitches_skips_chan_zero_cells() {
        let mut boards = HashMap::new();
        boards.insert(
            0u8,
            vec![
                WtnCell { key: 60, chan: 1, color: (0, 0, 0) },
                WtnCell::default(), // chan=0 ⇒ None
                WtnCell { key: 64, chan: 2, color: (0, 0, 0) },
            ],
        );
        let wtn = Wtn { edo: Some(31), pitch_offset: 0, boards };
        let lp = build_layout_pitches(&wtn);
        assert_eq!(lp["board0"], vec![Some(60), None, Some(31 + 64)]);
    }
}
