//! HID → physical key location mapping for the Wooting 60HE ANSI layout.
//!
//! Ported verbatim from the xenwooting project. Keeps the xenwooting
//! "gap handling" quirk: wide keys (Shift, Enter) have `led_col` that skips
//! over adjacent positions, while `midi_col` stays contiguous. This is
//! essential for correctly placing LED updates and for the WTN cell
//! compaction step.

use anyhow::Result;
use std::collections::HashMap;

/// USB HID usage code for a key on the keyboard (section 10 of the USB HID
/// Usage Tables). Stored as `u16` to match the Analog SDK's `read_full_buffer`
/// signature which returns `(code: u16, analog: f32)`.
pub type Hid = u16;

/// Named HID codes we care about on a 60% ANSI keyboard.
#[allow(dead_code)]
pub mod hid {
    use super::Hid;
    pub const ESCAPE: Hid = 0x29;
    pub const N1: Hid = 0x1E;
    pub const N2: Hid = 0x1F;
    pub const N3: Hid = 0x20;
    pub const N4: Hid = 0x21;
    pub const N5: Hid = 0x22;
    pub const N6: Hid = 0x23;
    pub const N7: Hid = 0x24;
    pub const N8: Hid = 0x25;
    pub const N9: Hid = 0x26;
    pub const N0: Hid = 0x27;
    pub const MINUS: Hid = 0x2D;
    pub const EQUAL: Hid = 0x2E;
    pub const BACKSPACE: Hid = 0x2A;

    pub const TAB: Hid = 0x2B;
    pub const Q: Hid = 0x14;
    pub const W: Hid = 0x1A;
    pub const E: Hid = 0x08;
    pub const R: Hid = 0x15;
    pub const T: Hid = 0x17;
    pub const Y: Hid = 0x1C;
    pub const U: Hid = 0x18;
    pub const I: Hid = 0x0C;
    pub const O: Hid = 0x12;
    pub const P: Hid = 0x13;
    pub const BRACKET_LEFT: Hid = 0x2F;
    pub const BRACKET_RIGHT: Hid = 0x30;
    pub const BACKSLASH: Hid = 0x31;

    pub const CAPSLOCK: Hid = 0x39;
    pub const A: Hid = 0x04;
    pub const S: Hid = 0x16;
    pub const D: Hid = 0x07;
    pub const F: Hid = 0x09;
    pub const G: Hid = 0x0A;
    pub const H: Hid = 0x0B;
    pub const J: Hid = 0x0D;
    pub const K: Hid = 0x0E;
    pub const L: Hid = 0x0F;
    pub const SEMICOLON: Hid = 0x33;
    pub const QUOTE: Hid = 0x34;
    pub const ENTER: Hid = 0x28;

    pub const LEFT_SHIFT: Hid = 0xE1;
    pub const Z: Hid = 0x1D;
    pub const X: Hid = 0x1B;
    pub const C: Hid = 0x06;
    pub const V: Hid = 0x19;
    pub const B: Hid = 0x05;
    pub const N: Hid = 0x11;
    pub const M: Hid = 0x10;
    pub const COMMA: Hid = 0x36;
    pub const PERIOD: Hid = 0x37;
    pub const SLASH: Hid = 0x38;
    pub const RIGHT_SHIFT: Hid = 0xE5;

    pub const LEFT_CONTROL: Hid = 0xE0;
    pub const LEFT_META: Hid = 0xE3;
    pub const LEFT_ALT: Hid = 0xE2;
    pub const SPACE: Hid = 0x2C;
    pub const RIGHT_ALT: Hid = 0xE6;
    pub const RIGHT_META: Hid = 0xE7;
    pub const FN: Hid = 0xFF; // non-standard; wooting-specific fn key
    pub const RIGHT_CONTROL: Hid = 0xE4;

    // Arrow cluster (USB HID usage IDs section 10).
    pub const ARROW_RIGHT: Hid = 0x4F;
    pub const ARROW_LEFT: Hid = 0x50;
    pub const ARROW_DOWN: Hid = 0x51;
    pub const ARROW_UP: Hid = 0x52;

    // The "application" / context-menu key (Fn layer on 60HE ANSI).
    pub const CONTEXT_MENU: Hid = 0x65;
}

/// Mapping of a physical HID key into logical MIDI grid (4×14) plus physical
/// LED grid (6×14). On wide keys the LED and MIDI columns diverge.
#[derive(Debug, Clone, Copy)]
pub struct KeyLoc {
    pub midi_row: u8, // 0..3 (playable rows)
    pub midi_col: u8, // 0..13
    pub led_row: u8,  // physical RGB row (0..5)
    pub led_col: u8,  // physical RGB col (0..13)
}

#[derive(Debug, Clone)]
pub struct HidMap {
    loc_by_hid: HashMap<Hid, KeyLoc>,
}

impl HidMap {
    /// Best-effort guess for a standard 60% ANSI Wooting layout. Users can
    /// override via `apply_overrides`.
    pub fn default_60he_ansi_guess() -> Self {
        let mut m: HashMap<Hid, KeyLoc> = HashMap::new();

        // Row 1 (physical RGB row 1) -> MIDI row 0
        let r_led = 1u8;
        let r_midi = 0u8;
        let row1: &[(Hid, u8)] = &[
            (hid::ESCAPE, 0),
            (hid::N1, 1), (hid::N2, 2), (hid::N3, 3), (hid::N4, 4),
            (hid::N5, 5), (hid::N6, 6), (hid::N7, 7), (hid::N8, 8),
            (hid::N9, 9), (hid::N0, 10),
            (hid::MINUS, 11), (hid::EQUAL, 12),
            (hid::BACKSPACE, 13),
        ];
        for (h, c) in row1 {
            m.insert(*h, KeyLoc { midi_row: r_midi, midi_col: *c, led_row: r_led, led_col: *c });
        }

        // Row 2 (physical RGB row 2) -> MIDI row 1
        let r_led = 2u8;
        let r_midi = 1u8;
        let row2: &[(Hid, u8)] = &[
            (hid::TAB, 0),
            (hid::Q, 1), (hid::W, 2), (hid::E, 3), (hid::R, 4), (hid::T, 5),
            (hid::Y, 6), (hid::U, 7), (hid::I, 8), (hid::O, 9), (hid::P, 10),
            (hid::BRACKET_LEFT, 11), (hid::BRACKET_RIGHT, 12),
            (hid::BACKSLASH, 13),
        ];
        for (h, c) in row2 {
            m.insert(*h, KeyLoc { midi_row: r_midi, midi_col: *c, led_row: r_led, led_col: *c });
        }

        // Row 3 (physical RGB row 3) -> MIDI row 2
        let r_led = 3u8;
        let r_midi = 2u8;
        let row3: &[(Hid, u8)] = &[
            (hid::CAPSLOCK, 0),
            (hid::A, 1), (hid::S, 2), (hid::D, 3), (hid::F, 4), (hid::G, 5),
            (hid::H, 6), (hid::J, 7), (hid::K, 8), (hid::L, 9),
            (hid::SEMICOLON, 10), (hid::QUOTE, 11),
            (hid::ENTER, 12),
        ];
        for (h, c) in row3 {
            m.insert(*h, KeyLoc { midi_row: r_midi, midi_col: *c, led_row: r_led, led_col: *c });
        }

        // Row 4 (physical RGB row 4) -> MIDI row 3
        let r_led = 4u8;
        let r_midi = 3u8;
        // Wide keys: `(hid, midi_col, led_col)` — LED col may skip gaps.
        let row4: &[(Hid, u8, u8)] = &[
            (hid::LEFT_SHIFT, 0, 0),
            (hid::Z, 1, 2), (hid::X, 2, 3), (hid::C, 3, 4), (hid::V, 4, 5),
            (hid::B, 5, 6), (hid::N, 6, 7), (hid::M, 7, 8),
            (hid::COMMA, 8, 9), (hid::PERIOD, 9, 10), (hid::SLASH, 10, 11),
            (hid::RIGHT_SHIFT, 11, 13),
        ];
        for (h, mc, lc) in row4 {
            m.insert(*h, KeyLoc { midi_row: r_midi, midi_col: *mc, led_row: r_led, led_col: *lc });
        }

        // Wide Enter key sits at the last LED column.
        if let Some(loc) = m.get_mut(&hid::ENTER) {
            loc.led_col = 13;
        }

        Self { loc_by_hid: m }
    }

    pub fn apply_overrides(&mut self, overrides: &[(Hid, u8, u8, u8, u8)]) -> Result<()> {
        for (h, midi_row, midi_col, led_row, led_col) in overrides {
            self.loc_by_hid.insert(*h, KeyLoc {
                midi_row: *midi_row, midi_col: *midi_col,
                led_row: *led_row, led_col: *led_col,
            });
        }
        Ok(())
    }

    pub fn loc_for(&self, hid: Hid) -> Option<KeyLoc> {
        self.loc_by_hid.get(&hid).copied()
    }

    pub fn all_locs(&self) -> Vec<(Hid, KeyLoc)> {
        self.loc_by_hid.iter().map(|(h, loc)| (*h, *loc)).collect()
    }
}

/// Rotate the 4×14 playable MIDI grid 0° or 180°. LED coords are untouched.
pub fn rotate_4x14(loc: KeyLoc, rotation_deg: u16) -> Result<KeyLoc> {
    match rotation_deg {
        0 => Ok(loc),
        180 => {
            if loc.midi_row >= 4 || loc.midi_col >= 14 {
                anyhow::bail!("KeyLoc out of 4x14 bounds");
            }
            Ok(KeyLoc {
                midi_row: 3 - loc.midi_row,
                midi_col: 13 - loc.midi_col,
                ..loc
            })
        }
        _ => anyhow::bail!("Unsupported rotation_deg {rotation_deg}; use 0 or 180"),
    }
}

/// Mirror the 4×14 playable MIDI grid left-right (for `.wtn` lookup).
pub fn mirror_cols_4x14(mut loc: KeyLoc, mirror: bool) -> Result<KeyLoc> {
    if !mirror {
        return Ok(loc);
    }
    if loc.midi_row >= 4 || loc.midi_col >= 14 {
        anyhow::bail!("KeyLoc out of 4x14 bounds");
    }
    loc.midi_col = 13 - loc.midi_col;
    Ok(loc)
}

/// Xenwooting quirk: wide keys (LeftShift, RightShift) create empty low/high
/// `midi_col` slots on certain rows. To avoid holes in the dense 14-wide WTN
/// lookup, compute the per-row min `midi_col` offset so `compact_col =
/// midi_col - offset[row]`.
///
/// `rotation_deg` must match the rotation that will later be applied in
/// `wtn_index_for_loc` — rotating the locs first ensures the computed
/// offsets align with the rotated coordinate system.
pub fn compute_compact_col_offsets(map: &HidMap, rotation_deg: u16) -> [u8; 4] {
    let mut min_col = [u8::MAX; 4];
    for (_, loc0) in map.all_locs() {
        let Ok(loc) = rotate_4x14(loc0, rotation_deg) else { continue };
        if (loc.midi_row as usize) < 4 && loc.midi_col < min_col[loc.midi_row as usize] {
            min_col[loc.midi_row as usize] = loc.midi_col;
        }
    }
    for slot in min_col.iter_mut() {
        if *slot == u8::MAX {
            *slot = 0;
        }
    }
    min_col
}

/// Resolve the final WTN cell index for an HID `loc`, honoring board rotation
/// and the rotation-aware compact-col offsets. Returns `None` if the loc
/// falls outside the 4×14 playable grid after transforms.
pub fn wtn_index_for_loc(
    loc: KeyLoc,
    rotation_deg: u16,
    compact: &[u8; 4],
) -> Option<usize> {
    let loc = rotate_4x14(loc, rotation_deg).ok()?;
    if loc.midi_row >= 4 {
        return None;
    }
    let off = compact[loc.midi_row as usize];
    if loc.midi_col < off {
        return None;
    }
    let col = (loc.midi_col - off) as usize;
    if col >= 14 {
        return None;
    }
    Some((loc.midi_row as usize) * 14 + col)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_map_has_expected_keys() {
        let m = HidMap::default_60he_ansi_guess();
        assert!(m.loc_for(hid::ESCAPE).is_some());
        assert!(m.loc_for(hid::ENTER).is_some());
        let escape = m.loc_for(hid::ESCAPE).unwrap();
        assert_eq!((escape.midi_row, escape.midi_col), (0, 0));
        let enter = m.loc_for(hid::ENTER).unwrap();
        assert_eq!(enter.midi_row, 2);
        assert_eq!(enter.led_col, 13);
    }

    #[test]
    fn rotate_180_flips() {
        let loc = KeyLoc { midi_row: 0, midi_col: 0, led_row: 0, led_col: 0 };
        let r = rotate_4x14(loc, 180).unwrap();
        assert_eq!((r.midi_row, r.midi_col), (3, 13));
    }

    #[test]
    fn mirror_flips_cols() {
        let loc = KeyLoc { midi_row: 1, midi_col: 2, led_row: 2, led_col: 2 };
        let r = mirror_cols_4x14(loc, true).unwrap();
        assert_eq!(r.midi_col, 11);
    }

    #[test]
    fn compact_offsets_are_zero_for_ansi_default() {
        // The default map has midi_col=0 in every row, so offsets should all be 0.
        let m = HidMap::default_60he_ansi_guess();
        assert_eq!(compute_compact_col_offsets(&m, 0), [0, 0, 0, 0]);
    }
}
