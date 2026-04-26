//! Hex grid geometry for the Exquis, Lumatone (`.ltn`), and Wooting (`.wtn`) layouts.
//!
//! All grids use the same doubled-y hex coordinate convention as xenwooting:
//! - neighbors: `(0, ±2)` and `(±1, ±1)`
//! - each pad has integer `(x, y)` coordinates in this shared lattice

use serde::Serialize;

pub const HEX_NEIGHBOR_DELTAS: [(i32, i32); 6] = [
    (0, -2),
    (0, 2),
    (-1, -1),
    (-1, 1),
    (1, -1),
    (1, 1),
];

#[derive(Debug, Clone, Copy, Serialize)]
pub struct GridKey {
    pub key: u8,
    pub x: i32,
    pub y: i32,
}

/// Build Exquis board tuples for a given board index (0-based).
///
/// Uses the xenwooting doubled-y hex coordinate convention shared with LTN/WTN:
/// - `x` = row index (0 = bottom, 10 = top)
/// - `y` = doubled-y lateral position (step of 2 between adjacent same-row pads)
///
/// Layout: 11 rows, alternating 6-5-6-5-…-6 pads = 61 pads total.
/// - Row 0 (bottom, x=0): 6 pads at y=0,2,4,6,8,10 → IDs 0..5
/// - Row 1 (x=1): 5 pads at y=1,3,5,7,9 → IDs 6..10
/// - … alternating up to row 10 (top, x=10).
///
/// Boards tile side-by-side via `(EXQUIS_BOARD_STRIDE_X, EXQUIS_BOARD_STRIDE_Y)
/// = (-1, 11)`: board N+1 is one row DOWN (x-1) and 11 lateral units right
/// (y+11) from board N, so board1's row 1 lines up with board0's row 0 height
/// (board1 is visually lower than board0).
pub const EXQUIS_BOARD_STRIDE_X: i32 = -1;
pub const EXQUIS_BOARD_STRIDE_Y: i32 = 11;

pub fn exquis_board_tuples(board_idx: u8) -> Vec<GridKey> {
    let x_offset = board_idx as i32 * EXQUIS_BOARD_STRIDE_X;
    let y_offset = board_idx as i32 * EXQUIS_BOARD_STRIDE_Y;
    let mut out = Vec::with_capacity(61);
    let mut pad: u8 = 0;
    for row in 0..11i32 {
        // Even rows (0, 2, …): 6 pads at y=0,2,4,6,8,10.
        // Odd rows: 5 pads at y=1,3,5,7,9.
        let (count, y_start) = if row % 2 == 0 { (6, 0) } else { (5, 1) };
        for col in 0..count {
            let y = y_start + col * 2;
            out.push(GridKey {
                key: pad,
                x: row + x_offset,
                y: y + y_offset,
            });
            pad += 1;
        }
    }
    out
}

// ==========================================================================
// Lumatone `.ltn` geometry — ported verbatim from xenwooting
// webconfigurator/web/src/hexgrid/boardGrids.ts
// ==========================================================================

pub const LTN_BOARD0_TUPLES: &[(u8, i32, i32)] = &[
    (0, 0, 0), (1, 0, 2),
    (2, 1, 1), (3, 1, 3), (4, 1, 5), (5, 1, 7), (6, 1, 9),
    (7, 2, 0), (8, 2, 2), (9, 2, 4), (10, 2, 6), (11, 2, 8), (12, 2, 10),
    (13, 3, 1), (14, 3, 3), (15, 3, 5), (16, 3, 7), (17, 3, 9), (18, 3, 11),
    (19, 4, 0), (20, 4, 2), (21, 4, 4), (22, 4, 6), (23, 4, 8), (24, 4, 10),
    (25, 5, 1), (26, 5, 3), (27, 5, 5), (28, 5, 7), (29, 5, 9), (30, 5, 11),
    (31, 6, 0), (32, 6, 2), (33, 6, 4), (34, 6, 6), (35, 6, 8), (36, 6, 10),
    (37, 7, 1), (38, 7, 3), (39, 7, 5), (40, 7, 7), (41, 7, 9), (42, 7, 11),
    (43, 8, 0), (44, 8, 2), (45, 8, 4), (46, 8, 6), (47, 8, 8), (48, 8, 10),
    (49, 9, 3), (50, 9, 5), (51, 9, 7), (52, 9, 9), (53, 9, 11),
    (54, 10, 8), (55, 10, 10),
];

pub const LTN_BOARD1_TUPLES: &[(u8, i32, i32)] = &[
    (0, 2, 12), (1, 2, 14),
    (2, 3, 13), (3, 3, 15), (4, 3, 17), (5, 3, 19), (6, 3, 21),
    (7, 4, 12), (8, 4, 14), (9, 4, 16), (10, 4, 18), (11, 4, 20), (12, 4, 22),
    (13, 5, 13), (14, 5, 15), (15, 5, 17), (16, 5, 19), (17, 5, 21), (18, 5, 23),
    (19, 6, 12), (20, 6, 14), (21, 6, 16), (22, 6, 18), (23, 6, 20), (24, 6, 22),
    (25, 7, 13), (26, 7, 15), (27, 7, 17), (28, 7, 19), (29, 7, 21), (30, 7, 23),
    (31, 8, 12), (32, 8, 14), (33, 8, 16), (34, 8, 18), (35, 8, 20), (36, 8, 22),
    (37, 9, 13), (38, 9, 15), (39, 9, 17), (40, 9, 19), (41, 9, 21), (42, 9, 23),
    (43, 10, 12), (44, 10, 14), (45, 10, 16), (46, 10, 18), (47, 10, 20), (48, 10, 22),
    (49, 11, 15), (50, 11, 17), (51, 11, 19), (52, 11, 21), (53, 11, 23),
    (54, 12, 20), (55, 12, 22),
];

pub const LTN_BOARD2_TUPLES: &[(u8, i32, i32)] = &[
    (0, 4, 24), (1, 4, 26),
    (2, 5, 25), (3, 5, 27), (4, 5, 29), (5, 5, 31), (6, 5, 33),
    (7, 6, 24), (8, 6, 26), (9, 6, 28), (10, 6, 30), (11, 6, 32), (12, 6, 34),
    (13, 7, 25), (14, 7, 27), (15, 7, 29), (16, 7, 31), (17, 7, 33), (18, 7, 35),
    (19, 8, 24), (20, 8, 26), (21, 8, 28), (22, 8, 30), (23, 8, 32), (24, 8, 34),
    (25, 9, 25), (26, 9, 27), (27, 9, 29), (28, 9, 31), (29, 9, 33), (30, 9, 35),
    (31, 10, 24), (32, 10, 26), (33, 10, 28), (34, 10, 30), (35, 10, 32), (36, 10, 34),
    (37, 11, 25), (38, 11, 27), (39, 11, 29), (40, 11, 31), (41, 11, 33), (42, 11, 35),
    (43, 12, 24), (44, 12, 26), (45, 12, 28), (46, 12, 30), (47, 12, 32), (48, 12, 34),
    (49, 13, 27), (50, 13, 29), (51, 13, 31), (52, 13, 33), (53, 13, 35),
    (54, 14, 32), (55, 14, 34),
];

pub const LTN_BOARD3_TUPLES: &[(u8, i32, i32)] = &[
    (0, 6, 36), (1, 6, 38),
    (2, 7, 37), (3, 7, 39), (4, 7, 41), (5, 7, 43), (6, 7, 45),
    (7, 8, 36), (8, 8, 38), (9, 8, 40), (10, 8, 42), (11, 8, 44), (12, 8, 46),
    (13, 9, 37), (14, 9, 39), (15, 9, 41), (16, 9, 43), (17, 9, 45), (18, 9, 47),
    (19, 10, 36), (20, 10, 38), (21, 10, 40), (22, 10, 42), (23, 10, 44), (24, 10, 46),
    (25, 11, 37), (26, 11, 39), (27, 11, 41), (28, 11, 43), (29, 11, 45), (30, 11, 47),
    (31, 12, 36), (32, 12, 38), (33, 12, 40), (34, 12, 42), (35, 12, 44), (36, 12, 46),
    (37, 13, 37), (38, 13, 39), (39, 13, 41), (40, 13, 43), (41, 13, 45), (42, 13, 47),
    (43, 14, 36), (44, 14, 38), (45, 14, 40), (46, 14, 42), (47, 14, 44), (48, 14, 46),
    (49, 15, 39), (50, 15, 41), (51, 15, 43), (52, 15, 45), (53, 15, 47),
    (54, 16, 44), (55, 16, 46),
];

pub const LTN_BOARD4_TUPLES: &[(u8, i32, i32)] = &[
    (0, 8, 48), (1, 8, 50),
    (2, 9, 49), (3, 9, 51), (4, 9, 53), (5, 9, 55), (6, 9, 57),
    (7, 10, 48), (8, 10, 50), (9, 10, 52), (10, 10, 54), (11, 10, 56), (12, 10, 58),
    (13, 11, 49), (14, 11, 51), (15, 11, 53), (16, 11, 55), (17, 11, 57), (18, 11, 59),
    (19, 12, 48), (20, 12, 50), (21, 12, 52), (22, 12, 54), (23, 12, 56), (24, 12, 58),
    (25, 13, 49), (26, 13, 51), (27, 13, 53), (28, 13, 55), (29, 13, 57), (30, 13, 59),
    (31, 14, 48), (32, 14, 50), (33, 14, 52), (34, 14, 54), (35, 14, 56), (36, 14, 58),
    (37, 15, 49), (38, 15, 51), (39, 15, 53), (40, 15, 55), (41, 15, 57), (42, 15, 59),
    (43, 16, 48), (44, 16, 50), (45, 16, 52), (46, 16, 54), (47, 16, 56), (48, 16, 58),
    (49, 17, 51), (50, 17, 53), (51, 17, 55), (52, 17, 57), (53, 17, 59),
    (54, 18, 56), (55, 18, 58),
];

pub fn ltn_boards_tuples() -> [&'static [(u8, i32, i32)]; 5] {
    [LTN_BOARD0_TUPLES, LTN_BOARD1_TUPLES, LTN_BOARD2_TUPLES, LTN_BOARD3_TUPLES, LTN_BOARD4_TUPLES]
}

// ==========================================================================
// Wooting `.wtn` geometry — ported verbatim from xenwooting
// ==========================================================================

pub const WTN_BOARD0_TUPLES: &[(u8, i32, i32)] = &[
    (0, 0, 1), (1, 0, 3), (2, 0, 5), (3, 0, 7), (4, 0, 9), (5, 0, 11),
    (6, 0, 13), (7, 0, 15), (8, 0, 17), (9, 0, 19), (10, 0, 21), (11, 0, 23),
    (12, 1, 0), (13, 1, 2), (14, 1, 4), (15, 1, 6), (16, 1, 8), (17, 1, 10),
    (18, 1, 12), (19, 1, 14), (20, 1, 16), (21, 1, 18), (22, 1, 20), (23, 1, 22),
    (24, 1, 24),
    (25, 2, -1), (26, 2, 1), (27, 2, 3), (28, 2, 5), (29, 2, 7), (30, 2, 9),
    (31, 2, 11), (32, 2, 13), (33, 2, 15), (34, 2, 17), (35, 2, 19), (36, 2, 21),
    (37, 2, 23), (38, 2, 25),
    (39, 3, 0), (40, 3, 2), (41, 3, 4), (42, 3, 6), (43, 3, 8), (44, 3, 10),
    (45, 3, 12), (46, 3, 14), (47, 3, 16), (48, 3, 18), (49, 3, 20), (50, 3, 22),
    (51, 3, 24), (52, 3, 26),
];

pub const WTN_BOARD1_TUPLES: &[(u8, i32, i32)] = &[
    (0, 4, -5), (1, 4, -3), (2, 4, -1), (3, 4, 1), (4, 4, 3), (5, 4, 5),
    (6, 4, 7), (7, 4, 9), (8, 4, 11), (9, 4, 13), (10, 4, 15), (11, 4, 17),
    (12, 4, 19), (13, 4, 21),
    (14, 5, -4), (15, 5, -2), (16, 5, 0), (17, 5, 2), (18, 5, 4), (19, 5, 6),
    (20, 5, 8), (21, 5, 10), (22, 5, 12), (23, 5, 14), (24, 5, 16), (25, 5, 18),
    (26, 5, 20), (27, 5, 22),
    (28, 6, -3), (29, 6, -1), (30, 6, 1), (31, 6, 3), (32, 6, 5), (33, 6, 7),
    (34, 6, 9), (35, 6, 11), (36, 6, 13), (37, 6, 15), (38, 6, 17), (39, 6, 19),
    (40, 6, 21),
    (41, 7, -2), (42, 7, 0), (43, 7, 2), (44, 7, 4), (45, 7, 6), (46, 7, 8),
    (47, 7, 10), (48, 7, 12), (49, 7, 14), (50, 7, 16), (51, 7, 18), (52, 7, 20),
];

pub fn wtn_boards_tuples() -> [&'static [(u8, i32, i32)]; 2] {
    [WTN_BOARD0_TUPLES, WTN_BOARD1_TUPLES]
}

/// Board-to-board shift in the unified WTN hex lattice (xenwooting convention).
/// `(dx_row, dy_doubled_y)`. Used by the editor's combined-pair view to
/// compute the horizontal offset between the rotated top board and the
/// upright bottom board.
pub const WTN_BOARD_SHIFT: (i32, i32) = (4, -6);

/// How a geometry maps its abstract (x, y) hex coords to screen pixels.
///
/// In all variants, `y` is the "doubled-y" lateral axis and `x` is the row
/// index. They differ only by vertical flip (origin at top vs bottom).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum Orientation {
    /// xenwooting-style: y → horizontal, x → vertical (row 0 at TOP).
    YRightXDown,
    /// Exquis-style: y → horizontal, x → vertical (row 0 at BOTTOM).
    YRightXUp,
}

/// High-level description of a geometry for listing/visualization.
pub struct GeometryInfo {
    pub name: &'static str,
    pub aliases: &'static [&'static str],
    pub description: &'static str,
    /// Returns boards as owned Vec of (pad, x, y) for convenience.
    pub boards: Vec<Vec<(u8, i32, i32)>>,
    /// Board-to-board shift in the unified lattice, if regular.
    pub board_shift: Option<(i32, i32)>,
    /// Rendering convention.
    pub orientation: Orientation,
}

pub fn geometries(exquis_board_count: u8) -> Vec<GeometryInfo> {
    let exquis_boards: Vec<Vec<(u8, i32, i32)>> = (0..exquis_board_count)
        .map(|b| {
            exquis_board_tuples(b)
                .into_iter()
                .map(|k| (k.key, k.x, k.y))
                .collect()
        })
        .collect();
    let ltn_boards: Vec<Vec<(u8, i32, i32)>> =
        ltn_boards_tuples().iter().map(|t| t.to_vec()).collect();
    let wtn_boards: Vec<Vec<(u8, i32, i32)>> =
        wtn_boards_tuples().iter().map(|t| t.to_vec()).collect();

    vec![
        GeometryInfo {
            name: "exquis",
            aliases: &["xtn"],
            description: "Intuitive Instruments Exquis — 11 rows alternating 6-5-6-5-…-6 pads = 61 per board",
            boards: exquis_boards,
            board_shift: Some((EXQUIS_BOARD_STRIDE_X, EXQUIS_BOARD_STRIDE_Y)),
            orientation: Orientation::YRightXUp,
        },
        GeometryInfo {
            name: "lumatone",
            aliases: &["ltn"],
            description: "Lumatone — 56 pads per board, 5 boards tile into one hexagonal instrument",
            boards: ltn_boards,
            board_shift: Some((2, 12)),
            orientation: Orientation::YRightXDown,
        },
        GeometryInfo {
            name: "wooting",
            aliases: &["wtn"],
            description: "xenwooting — 56 logical cells per board (with holes), 2 boards",
            boards: wtn_boards,
            board_shift: Some(WTN_BOARD_SHIFT),
            orientation: Orientation::YRightXDown,
        },
    ]
}

pub fn geometry_by_name(name: &str, exquis_board_count: u8) -> Option<GeometryInfo> {
    let lname = name.to_lowercase();
    geometries(exquis_board_count)
        .into_iter()
        .find(|g| g.name == lname || g.aliases.iter().any(|a| *a == lname))
}

// ==========================================================================
// Hex math — `rotateHex` etc. ported from xenwooting project.ts
// Uses cube coordinates internally; converts from/to doubled-y axial.
// ==========================================================================

fn doubled_y_to_cube(x: i32, y: i32) -> (i32, i32, i32) {
    // In xenwooting's convention:
    //   col = x, row = (y - col) / 2 effectively
    // Cube conversion: q = col, r = (y - col) / 2  (safe if y-col is even for valid cells)
    let q = x;
    let r = (y - x).div_euclid(2);
    let s = -q - r;
    (q, r, s)
}

fn cube_to_doubled_y(q: i32, r: i32, _s: i32) -> (i32, i32) {
    let x = q;
    let y = 2 * r + q;
    (x, y)
}

/// Rotate a doubled-y hex point by `steps` increments of 60° around the origin.
pub fn rotate_hex(x: i32, y: i32, steps: i32) -> (i32, i32) {
    let (mut q, mut r, mut s) = doubled_y_to_cube(x, y);
    let k = steps.rem_euclid(6);
    for _ in 0..k {
        // 60° cube rotation: (q, r, s) -> (-r, -s, -q)
        let (nq, nr, ns) = (-r, -s, -q);
        q = nq;
        r = nr;
        s = ns;
    }
    cube_to_doubled_y(q, r, s)
}

/// Convert (x, y) doubled-y hex coords to pixel (px, py) using a per-geometry
/// convention. Uses `R*sqrt(3)/2` / `R*3/2` scaling so all 6 hex neighbors
/// are equidistant in screen pixels.
fn hex_to_pixel(x: i32, y: i32, orientation: Orientation, r: f64) -> (f64, f64) {
    let short = r * 3f64.sqrt() / 2.0;
    let long = r * 1.5;
    // In both conventions y is the doubled-y lateral axis (horizontal pixels).
    let px = y as f64 * short;
    let py = match orientation {
        Orientation::YRightXDown => x as f64 * long,      // row 0 at top
        Orientation::YRightXUp => -(x as f64) * long,     // row 0 at bottom
    };
    (px, py)
}

/// Render a geometry as a standalone SVG string: dots for each pad, lines
/// between hex neighbors, distinct color per board.
pub fn render_geometry_svg(info: &GeometryInfo) -> String {
    const R: f64 = 18.0;
    let dot_radius = R * 0.5;

    // Bounding box in pixel space (accounts for orientation).
    let mut min_px = f64::INFINITY;
    let mut max_px = f64::NEG_INFINITY;
    let mut min_py = f64::INFINITY;
    let mut max_py = f64::NEG_INFINITY;
    for board in &info.boards {
        for &(_pad, x, y) in board {
            let (px, py) = hex_to_pixel(x, y, info.orientation, R);
            min_px = min_px.min(px);
            max_px = max_px.max(px);
            min_py = min_py.min(py);
            max_py = max_py.max(py);
        }
    }
    let pad = R + 8.0;
    let width = (max_px - min_px) + 2.0 * pad;
    let height = (max_py - min_py) + 2.0 * pad;
    let ox = -min_px + pad;
    let oy = -min_py + pad;

    // Unified lookup so we only draw a line between pads that both exist.
    let mut coord_to_board: std::collections::HashMap<(i32, i32), usize> =
        std::collections::HashMap::new();
    for (bi, board) in info.boards.iter().enumerate() {
        for &(_pad, x, y) in board {
            coord_to_board.insert((x, y), bi);
        }
    }

    let palette = [
        "#e06666", "#6fa8dc", "#93c47d", "#f6b26b", "#8e7cc3", "#76a5af", "#c27ba0",
    ];

    let mut svg = String::new();
    svg.push_str(&format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{width:.0}\" height=\"{height:.0}\" \
         viewBox=\"0 0 {width:.0} {height:.0}\" style=\"background:#1a1a1a; font-family:sans-serif\">\n"
    ));
    svg.push_str(&format!(
        "<title>{} geometry</title>\n",
        html_escape(info.name)
    ));

    // Draw neighbor lines first (behind dots).
    let mut drawn: std::collections::HashSet<((i32, i32), (i32, i32))> =
        std::collections::HashSet::new();
    for (bi, board) in info.boards.iter().enumerate() {
        for &(_pad, x, y) in board {
            for (dx, dy) in HEX_NEIGHBOR_DELTAS {
                let nx = x + dx;
                let ny = y + dy;
                if let Some(&nbi) = coord_to_board.get(&(nx, ny)) {
                    let edge = if (x, y) < (nx, ny) {
                        ((x, y), (nx, ny))
                    } else {
                        ((nx, ny), (x, y))
                    };
                    if drawn.insert(edge) {
                        let (p1x, p1y) = hex_to_pixel(x, y, info.orientation, R);
                        let (p2x, p2y) = hex_to_pixel(nx, ny, info.orientation, R);
                        let x1 = p1x + ox;
                        let y1 = p1y + oy;
                        let x2 = p2x + ox;
                        let y2 = p2y + oy;
                        let color = if bi == nbi { "#555" } else { "#333" };
                        svg.push_str(&format!(
                            "<line x1=\"{x1:.1}\" y1=\"{y1:.1}\" x2=\"{x2:.1}\" y2=\"{y2:.1}\" stroke=\"{color}\" stroke-width=\"1.5\"/>\n"
                        ));
                    }
                }
            }
        }
    }

    // Draw dots + pad labels.
    for (bi, board) in info.boards.iter().enumerate() {
        let color = palette[bi % palette.len()];
        for &(pad, x, y) in board {
            let (px, py) = hex_to_pixel(x, y, info.orientation, R);
            let cx = px + ox;
            let cy = py + oy;
            svg.push_str(&format!(
                "<circle cx=\"{cx:.1}\" cy=\"{cy:.1}\" r=\"{dot_radius:.1}\" fill=\"{color}\" stroke=\"#000\" stroke-width=\"1\"/>\n"
            ));
            svg.push_str(&format!(
                "<text x=\"{cx:.1}\" y=\"{:.1}\" font-size=\"9\" fill=\"#fff\" text-anchor=\"middle\" dominant-baseline=\"middle\" pointer-events=\"none\">{pad}</text>\n",
                cy + 0.5
            ));
        }
    }

    // Legend.
    let legend_y = 14.0;
    svg.push_str(&format!(
        "<text x=\"{:.1}\" y=\"{legend_y:.1}\" font-size=\"12\" fill=\"#ddd\">{} — {} boards</text>\n",
        pad,
        html_escape(info.name),
        info.boards.len()
    ));
    for (bi, _board) in info.boards.iter().enumerate() {
        let lx = pad + 200.0 + bi as f64 * 80.0;
        let color = palette[bi % palette.len()];
        svg.push_str(&format!(
            "<circle cx=\"{lx:.1}\" cy=\"{legend_y:.1}\" r=\"6\" fill=\"{color}\"/>\n\
             <text x=\"{:.1}\" y=\"{:.1}\" font-size=\"11\" fill=\"#ddd\">board{bi}</text>\n",
            lx + 10.0,
            legend_y + 4.0
        ));
    }

    svg.push_str("</svg>\n");
    svg
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exquis_board_has_61_pads() {
        let t = exquis_board_tuples(0);
        assert_eq!(t.len(), 61);
        assert_eq!(t[0].key, 0);
        assert_eq!(t[60].key, 60);
        // Convention: x = row, y = lateral (step 2). Pad 0 at (row=0, y=0).
        assert_eq!((t[0].x, t[0].y), (0, 0));
        // Pad 5 at row 0, y=10 (last of bottom row).
        assert_eq!((t[5].x, t[5].y), (0, 10));
        // Pad 6 at row 1, y=1 (first of second row).
        assert_eq!((t[6].x, t[6].y), (1, 1));
        // Pad 60 at top-right: row 10, y=10.
        assert_eq!((t[60].x, t[60].y), (10, 10));
    }

    #[test]
    fn multiple_boards_dont_overlap() {
        let b0 = exquis_board_tuples(0);
        let b1 = exquis_board_tuples(1);
        let coords0: std::collections::HashSet<_> = b0.iter().map(|k| (k.x, k.y)).collect();
        for k in &b1 {
            assert!(!coords0.contains(&(k.x, k.y)), "overlap at ({}, {})", k.x, k.y);
        }
    }

    #[test]
    fn board1_shifted_one_row_down_from_board0() {
        // Board1 is one row LOWER than board0: board1's row 1 shares the row
        // index (x) with board0's row 0.
        let b0 = exquis_board_tuples(0);
        let b1 = exquis_board_tuples(1);
        let b0_row0 = b0[0].x;
        let b1_row1 = b1[6].x; // pad 6 is first of row 1
        assert_eq!(b0_row0, b1_row1);
    }

    #[test]
    fn doubled_y_parity_holds_across_boards() {
        // In doubled-y, even rows must have even x, odd rows must have odd x.
        for b in 0u8..4 {
            for k in exquis_board_tuples(b) {
                assert_eq!(
                    k.x.rem_euclid(2),
                    k.y.rem_euclid(2),
                    "parity violation at board{b} pad{} ({},{})",
                    k.key,
                    k.x,
                    k.y
                );
            }
        }
    }

    #[test]
    fn rotate_hex_0_is_identity() {
        assert_eq!(rotate_hex(2, 4, 0), (2, 4));
    }

    #[test]
    fn rotate_hex_360_is_identity() {
        assert_eq!(rotate_hex(2, 4, 6), (2, 4));
    }

    #[test]
    fn all_neighbor_deltas_have_same_distance() {
        // Sanity: rotating (0, -2) by 60° should land on another neighbor delta.
        let rotated = rotate_hex(0, -2, 1);
        assert!(HEX_NEIGHBOR_DELTAS.contains(&rotated));
    }
}
