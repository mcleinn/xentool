//! Hardcoded Wooting 60HE ANSI geometry.
//!
//! Each `WootingKey` describes a playable cell: its position in the dense
//! 4×14 MIDI grid (`row`, `col` → maps to WTN index `row*14 + col`), its HID
//! code, and its rendering rectangle in the editor (`x`, `y`, `w`, `h` in a
//! shared unit grid where 1 unit = a standard key width).
//!
//! The unit sizes come from xenwooting's geometry dataset, which was derived
//! from the physical 60% ANSI keyboard layout.

use serde::Serialize;

use crate::wooting::hidmap::hid;

/// Unit size used by the geometry (1 unit = one standard key cell).
pub const UNIT: f32 = 50.0;
pub const GAP: f32 = 1.0;

#[derive(Debug, Clone, Copy, Serialize)]
pub struct WootingKey {
    /// WTN linear index (row * 14 + col), 0..56.
    pub idx: u16,
    /// Dense MIDI grid row (0..4).
    pub row: u8,
    /// Dense MIDI grid col (0..14).
    pub col: u8,
    /// USB HID usage code.
    pub hid: u16,
    /// Rendering rectangle in units (1 unit ≈ 50 px). `(x, y)` top-left, `(w, h)` size.
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

/// The 60% ANSI layout. Units match the 4-row × 15-unit-wide keyboard physique,
/// with wide keys (Backspace=2u, Tab=1.5u, Backslash=1.5u, Enter=2.25u,
/// LeftShift=2.25u, RightShift=2.75u, Space=6.25u).
pub fn keys_60he() -> Vec<WootingKey> {
    let mut keys = Vec::new();

    // Row 0: Esc 1 2 … 0 - = Backspace(2u)
    let row0: &[(u16, f32, u8)] = &[
        (hid::ESCAPE, 1.0, 0),
        (hid::N1, 1.0, 1),
        (hid::N2, 1.0, 2),
        (hid::N3, 1.0, 3),
        (hid::N4, 1.0, 4),
        (hid::N5, 1.0, 5),
        (hid::N6, 1.0, 6),
        (hid::N7, 1.0, 7),
        (hid::N8, 1.0, 8),
        (hid::N9, 1.0, 9),
        (hid::N0, 1.0, 10),
        (hid::MINUS, 1.0, 11),
        (hid::EQUAL, 1.0, 12),
        (hid::BACKSPACE, 2.0, 13),
    ];
    place_row(&mut keys, 0, 0.0, row0);

    // Row 1: Tab(1.5u) Q W E R T Y U I O P [ ] \(1.5u)
    let row1: &[(u16, f32, u8)] = &[
        (hid::TAB, 1.5, 0),
        (hid::Q, 1.0, 1),
        (hid::W, 1.0, 2),
        (hid::E, 1.0, 3),
        (hid::R, 1.0, 4),
        (hid::T, 1.0, 5),
        (hid::Y, 1.0, 6),
        (hid::U, 1.0, 7),
        (hid::I, 1.0, 8),
        (hid::O, 1.0, 9),
        (hid::P, 1.0, 10),
        (hid::BRACKET_LEFT, 1.0, 11),
        (hid::BRACKET_RIGHT, 1.0, 12),
        (hid::BACKSLASH, 1.5, 13),
    ];
    place_row(&mut keys, 1, 0.0, row1);

    // Row 2: Caps(1.75u) A S D F G H J K L ; ' Enter(2.25u)
    let row2: &[(u16, f32, u8)] = &[
        (hid::CAPSLOCK, 1.75, 0),
        (hid::A, 1.0, 1),
        (hid::S, 1.0, 2),
        (hid::D, 1.0, 3),
        (hid::F, 1.0, 4),
        (hid::G, 1.0, 5),
        (hid::H, 1.0, 6),
        (hid::J, 1.0, 7),
        (hid::K, 1.0, 8),
        (hid::L, 1.0, 9),
        (hid::SEMICOLON, 1.0, 10),
        (hid::QUOTE, 1.0, 11),
        (hid::ENTER, 2.25, 12),
    ];
    place_row(&mut keys, 2, 0.0, row2);

    // Row 3: LeftShift(2.25u) Z X C V B N M , . / RightShift(2.75u)
    let row3: &[(u16, f32, u8)] = &[
        (hid::LEFT_SHIFT, 2.25, 0),
        (hid::Z, 1.0, 1),
        (hid::X, 1.0, 2),
        (hid::C, 1.0, 3),
        (hid::V, 1.0, 4),
        (hid::B, 1.0, 5),
        (hid::N, 1.0, 6),
        (hid::M, 1.0, 7),
        (hid::COMMA, 1.0, 8),
        (hid::PERIOD, 1.0, 9),
        (hid::SLASH, 1.0, 10),
        (hid::RIGHT_SHIFT, 2.75, 11),
    ];
    place_row(&mut keys, 3, 0.0, row3);

    keys
}

fn place_row(keys: &mut Vec<WootingKey>, row: u8, y_start: f32, row_keys: &[(u16, f32, u8)]) {
    let mut x = 0.0_f32;
    for &(hid_code, width_u, col) in row_keys {
        let idx = (row as u16) * 14 + col as u16;
        keys.push(WootingKey {
            idx,
            row,
            col,
            hid: hid_code,
            x: x * UNIT,
            y: (row as f32) * UNIT + y_start,
            w: width_u * UNIT,
            h: UNIT,
        });
        x += width_u;
    }
}

/// Board dimensions in units (width = 15u, height = 4u; matches 60% ANSI).
pub fn board_width_px() -> f32 {
    15.0 * UNIT
}
pub fn board_height_px() -> f32 {
    4.0 * UNIT
}

/// Horizontal shift applied to the top (rotated) board in a combined pair,
/// so the musical lattice lines up with the bottom board.
///
/// In WTN doubled-y hex coords, the board-to-board shift (`WTN_BOARD_SHIFT`)
/// is `(dx=4, dy=-6)`. The rotated top board already absorbs the 4-row
/// vertical traversal's intrinsic zigzag (4 doubled-y of natural offset), so
/// the remaining NET horizontal shift that must be applied to the top canvas
/// is `|dy| - dx = 2` doubled-y — but only 1.5 of those translate to a
/// visible keycap shift because the rendered ANSI rows start at different
/// x-offsets due to wide keys (Esc = 1U, Tab = 1.5U, Caps = 1.75U, LShift
/// = 2.25U). The effective pixel offset is 1.5 × UNIT.
///
/// Equivalent formula: `(|dy| - dx) * 3 / 4` in 1U units, derived from the
/// `WTN_BOARD_SHIFT` constant so any future change to the shift automatically
/// propagates.
pub fn pair_top_x_shift_px() -> f32 {
    let (dx, dy) = crate::geometry::WTN_BOARD_SHIFT;
    let net_doubled_y = (dy.unsigned_abs() as i32 - dx).max(0) as f32;
    // Scale from doubled-y (2 units per 1U) by 3/4 to account for the
    // per-row-start misalignment in the ANSI layout (wide keys).
    let units = net_doubled_y * 3.0 / 4.0;
    units * UNIT
}

/// Whether board `i` of `n` should render rotated 180°.
/// Rule: rotated iff even-indexed AND has a partner (`i + 1 < n`).
pub fn rotated(i: u8, n: u8) -> bool {
    i % 2 == 0 && i + 1 < n
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn has_expected_key_count() {
        // Row 0: 14 keys (Esc..Backspace)
        // Row 1: 14 keys (Tab..Backslash)
        // Row 2: 13 keys (Caps..Enter)
        // Row 3: 12 keys (LeftShift..RightShift)
        // Total: 53 on 60% ANSI (row 4 Space-row deferred).
        assert_eq!(keys_60he().len(), 53);
    }

    #[test]
    fn rotation_rule() {
        assert!(!rotated(0, 1));            // lone board: upright
        assert!(rotated(0, 2));             // paired: even is rotated
        assert!(!rotated(1, 2));            // partner: upright
        assert!(rotated(0, 3)); assert!(!rotated(1, 3)); assert!(!rotated(2, 3));
        assert!(rotated(0, 4)); assert!(!rotated(1, 4)); assert!(rotated(2, 4)); assert!(!rotated(3, 4));
    }
}
