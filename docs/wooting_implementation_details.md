# Implementation Details — Wooting backend

This file documents a few non-obvious implementation choices in xentool's
Wooting backend (`src/wooting/`). The Exquis backend has a different
runtime model (MPE pitch-bend retuning) and is out of scope here.

> **Provenance:** the design described below was first developed for the
> xenwooting project. xentool's Wooting mode is a port — most algorithms
> are identical; the surrounding plumbing (config storage, multi-board
> rotation rule, MIDI output port creation) was simplified.

## Layout Logic (Grids and Mappings)

This project has to reconcile multiple coordinate systems:

- a logical per-board musical grid (what the user edits in `.wtn`)
- physical keys reported by the Analog SDK (HID codes)
- the RGB LED matrix addressed by the RGB SDK (row/col with holes)

When you change anything related to keyboard layout (rotation, ISO vs ANSI,
split-backspace, etc.) you must know which layer you are changing.

### 1) `.wtn` logical grid (musical)

Files: `wtn/*.wtn` in the repo (or anywhere the user passes via
`xentool serve <file>.wtn`).

Each `[BoardN]` section stores 56 cells as `Key_0..Key_55`,
`Chan_0..Chan_55`, `Col_0..Col_55`.

Important semantics:

- The 56 indices represent a *4-row musical grid*.
- Index `i` maps to a logical `(row, col)` as:

  - `row = i / 14` (0..3)
  - `col = i % 14` (0..13)

- **Holes are not part of `.wtn` semantics.**
  Wide keys create holes in the physical LED column grid; `.wtn` indices
  are treated as "first key, second key, ..." within each row from the
  user's perspective. (Implementation: rows are compacted/left-justified
  after rotation/mirror. See "WTN Compaction" below.)

Cell meanings:

- `Key_i` is the MIDI note number (0..127) emitted for the pressed key.
- `Chan_i` is the MIDI channel (1..16) (stored 1-based in the file).
- `Col_i` is the base LED color (RRGGBB) used for the idle layout and the
  base color to restore after the white press-flash.

The header fields `Edo=N` and `PitchOffset=M` precede the first `[Board]`
section; the absolute pitch sounded by a cell is
`virtual_pitch = (Chan-1)*Edo + Key + PitchOffset` EDO steps above C0.
The MTS-ESP master xentool registers (see `src/mts.rs`) publishes both a
global 128-table and a 16×128 multichannel table derived from this
formula.

### 2) Physical key identity (Analog SDK)

xentool reads per-key analog values from Wooting's Analog SDK.

- Each physical keyboard is identified by an **Analog `device_id`** (`u64`).
- Each key on that keyboard is identified by a **HID usage code** (Wooting
  uses HID keycodes).

We map:

`(device_id, HID) -> KeyLoc { midi_row, midi_col, led_row, led_col }`

The mapping table lives in `src/wooting/hidmap.rs`:

- `HidMap::default_60he_ansi_guess()` provides a best-effort mapping for a
  60% ANSI layout — currently the only built-in HID map.

xentool does **not** support per-key `hid_overrides` in config (xenwooting
did). For non-ANSI / non-60% boards you currently have to add a new
preset map in `hidmap.rs`.

### 3) MIDI grid (per board)

`KeyLoc.midi_row/midi_col` is the *musical grid coordinate* for a physical
key, before per-board transforms.

This grid is **always 4x14** (rows 0..3, cols 0..13).

Note: for certain physical layouts (ANSI wide keys), not every (row, col)
exists as a physical key. Those missing positions are the "holes".

### 4) RGB LED grid (physical)

The RGB SDK addresses keys via a matrix:

- `led_row` is the physical RGB row (0..5)
- `led_col` is the physical RGB column (0..13 on 60HE)

Wide keys create holes here. Example from `src/wooting/hidmap.rs`:

- RightShift is a wide key; it uses `midi_col=11` but `led_col=13`.
- Enter is forced to `led_col=13`.

Highlighting must use the LED grid (physical), not the MIDI grid.

### 5) Per-board logical transforms (rotation/mirror)

xenwooting let each `[[boards]]` config entry set `rotation_deg` and
`mirror_cols` independently. xentool currently ignores the per-board
config: rotation is derived automatically from the board index using a
paired-pair rule:

```rust
// src/wooting/geometry.rs
fn rotated(wtn_board: u8, total_boards: u8) -> bool { ... }
```

For two-board setups (the common case), the second board renders rotated
180° relative to the first so the two stack into a single contiguous
2×56 hex lattice. For odd counts, a trailing single board renders solo
(unrotated).

`BoardSettings.rotation_deg` and `BoardSettings.mirror_cols` exist in
`settings.json` for forward compatibility but are not currently consulted
at runtime. If you need a custom rotation today, edit
`src/wooting/geometry.rs::rotated`.

The actual `KeyLoc` rotation/mirror helpers — `rotate_4x14` and
`mirror_cols_4x14` in `src/wooting/hidmap.rs` — apply only to the `.wtn`
*lookup* (musical space). They mutate `midi_row/midi_col` and leave
`led_row/led_col` unchanged, so a rotated board still addresses its LEDs
in the physical orientation.

### 6) WTN compaction ("ignore holes")

Problem:

- Some physical rows have fewer than 14 keys.
- On unrotated ANSI layouts, missing positions tend to be at the end, so
  `.wtn` indices feel left-justified.
- After rotation, those holes move to the start of the row, which would
  make `.wtn` index 0 correspond to a hole.

Desired behavior:

- `.wtn` indices always mean "first key, second key, ..." in that row from
  the user's perspective.
- Holes must never consume `.wtn` indices.

Solution:

- After applying rotation/mirror, compute per-row `min midi_col` among the
  actually present keys.
- Subtract that offset before indexing `.wtn`.

Implementation (`src/wooting/hidmap.rs`):

- `compute_compact_col_offsets(map, rotation_deg) -> [u8; 4]`
- `wtn_index_for_loc(loc, rotation_deg, compact) -> Option<usize>`

These are used everywhere we look up `.wtn`:

- MIDI note/channel lookup (`src/wooting/serve.rs::resolve_cell`)
- Base LED paint (`src/wooting/serve.rs::paint_initial_leds`)
- Static load command (`src/wooting/commands.rs::paint_board`)

## Dual Keyboard Mapping (Analog `device_id` ↔ WTN board ↔ RGB index)

Goal:

- Treat each physical Wooting keyboard as a stable identity.
- Ensure MIDI note/channel and LED behavior for a given `.wtn` cell always
  end up on the same physical key.
- Make the assignment stable across unplug/replug.

### Terms

- **Analog `device_id`**: `u64` identifier returned by the Wooting *Analog*
  SDK (`get_connected_devices_info`). Stable per physical keyboard.
- **WTN board**: logical board index used in `.wtn` files (`[Board0]`,
  `[Board1]`, ...), used for mapping note/channel/color.
- **RGB device index**: `u8` index used by the Wooting *RGB* SDK
  (`wooting_usb_select_device(index)`), used for sending LED updates.

### xentool's approach (simpler than xenwooting)

xenwooting reconstructs the analog `device_id` by reading
`/sys/bus/hid/devices/*/uevent` (HID_UNIQ, HID_ID, HID_PHYS) and re-hashing
those values to match the Analog SDK's `generate_device_id` so it can
deterministically pair Analog and RGB SDK orderings on Linux.

xentool does not do this. It maps `device_id → wtn_board` via Analog SDK
enumeration order (`refresh_devices` in `src/wooting/serve.rs`), and
lets the user override the RGB SDK index per board if the enumerations
disagree:

```jsonc
// settings.json (excerpt)
"wooting": {
  "boards": [
    { "device_id": "1234567890", "wtn_board": 0, "rgb_device_index": 1 },
    { "device_id": "9876543210", "wtn_board": 1, "rgb_device_index": 0 }
  ]
}
```

`BoardSettings.device_id` is stored as a string (because `u64::MAX`
doesn't round-trip via JSON number). `rgb_device_index` is the RGB SDK
index this device should drive — set it explicitly when the white
key-press flash appears on the wrong keyboard.

If you don't override `rgb_device_index`, xentool falls back to using the
analog enumeration index for both — which works on most setups but can
swap on hotplug.

### Base LED painting must use HID → KeyLoc mapping

Separate but related issue:

- Some ANSI keys are wider (Enter, Shift) which creates "holes" in the LED
  column grid.
- If base painting assumes `led_col == midi_col` for a synthetic 4×14
  grid, rows will be shifted/misaligned.

Fix:

- Base painting iterates the HID → `KeyLoc` map (same source of truth as
  highlighting), and uses `KeyLoc.led_row/led_col` for physical LEDs.
- Rotation/mirroring is applied only for `.wtn` lookup
  (`midi_row/midi_col`).

Code references:

- `src/wooting/hidmap.rs`: `HidMap::all_locs()`
- `src/wooting/serve.rs`: `paint_initial_leds` uses `map.all_locs()`

## How To Change Layouts (New Keyboard / ISO / Different Key Geometry)

There are three common types of changes:

### A) Change musical mapping (what note/channel/color each key means)

- Edit the `.wtn` file via xentool's web editor (`xentool edit my.wtn`,
  served at `http://localhost:8088`) or by hand.
- `.wtn` is the authoritative "musical" mapping.

Notes:

- For rotated boards, remember xentool applies rotation and then compacts
  rows (holes ignored). The web editor mirrors this.
- xentool's editor also accepts `.ltn` (Lumatone) and `.xtn` (Exquis) so
  you can import / cross-project layouts. See "Hex-Grid Board Geometry"
  below.

### B) Change board orientation (which keyboard renders rotated 180°)

xentool currently picks rotation automatically via the paired-pair rule
(see Section 5). To change it you have to edit
`src/wooting/geometry.rs::rotated` and rebuild — the per-board
`rotation_deg` field in `settings.json` is not yet wired in.

This affects `.wtn` lookup only; LED addressing stays in physical
orientation.

### C) Change physical key/LED mapping (ISO, split keys, different model)

This is the part that changes *coordinates*, not notes.

1) Determine how HID codes map to your physical key positions.

2) Add a new preset to `HidMap` in `src/wooting/hidmap.rs`. xenwooting's
   per-key `hid_overrides` config is not implemented in xentool — adding
   a new preset (or modifying `default_60he_ansi_guess()`) is the only
   path right now.

Important:

- Holes: if the physical LED grid has holes, set `led_col` appropriately
  (like RightShift, Enter in the ANSI guess). Do not encode holes into
  `.wtn`.
- Rotation/mirror: do NOT change `led_row/led_col` when applying
  rotation/mirror; only `midi_row/midi_col` changes.

## Hex-Grid Board Geometry + LTN→WTN Mapping (Import / Placement Mode)

xentool's web editor can project `.ltn` layouts onto `.wtn` layouts using
explicit hex-grid geometry (not pixel hit-testing). Same algorithm as
xenwooting's webconfigurator, ported from React/TypeScript to vanilla JS
in a single `assets/editor.js`.

### Geometry Tables

`src/geometry.rs` defines the hex-grid tuples for each format:

- `WTN_BOARD0_TUPLES`, `WTN_BOARD1_TUPLES` (and `wtn_boards_tuples()`):
  WTN boards in a hex-grid **visible-key** index space (53 keys per
  board, indices `0..52`).
- `LTN_BOARD0_TUPLES` ... `LTN_BOARD4_TUPLES` (and `ltn_boards_tuples()`):
  Lumatone boards (56 keys per board, indices `0..55`).
- `exquis_board_tuples(board_idx)`: Exquis pad layout, generated from
  `EXQUIS_BOARD_STRIDE_X` / `EXQUIS_BOARD_STRIDE_Y` so the four-board
  setup tiles into one continuous hex lattice.

Each key has an integer coordinate `(x, y)` in a hex lattice where
neighbors are:

- `(0, -2)`, `(0, +2)`
- `(-1, -1)`, `(-1, +1)`, `(+1, -1)`, `(+1, +1)`

This is a "doubled-y" representation: adjacent columns differ by 2 in `y`.

Note the orientation difference:

- Exquis places row 0 at the **bottom** (YRightXUp).
- LTN/WTN place row 0 at the **top**.

`assets/editor.js::hexToPixel(x, y, orientation)` handles both.

### Combined Coordinate Space

Mapping uses a single combined coordinate space:

1) LTN: `(BoardN, Key_k) -> (x, y)` via the LTN grid.
2) WTN: `(x, y) -> (wtn_board, visible_key_index)` via a combined lookup
   built from the WTN grid.

This step is purely geometric. **Holes / wide-key gaps are not part of
the projection.**

### Transform (Translate + 60-degree Rotate)

Import places the LTN coordinates into the WTN space after an adjustable
transform:

```
world = rotate60(src, rot_steps) + (tx, ty)
```

- `rot_steps` is in `0..5` (60-degree steps around the hex axes)
- `(tx, ty)` is a translation in the same `(x, y)` coordinate system

Implementation: `assets/editor.js::rotateHex(x, y, steps)` on the
frontend, mirrored in `src/geometry.rs::rotate_hex(x, y, steps)` for any
Rust-side projection (e.g. SVG renderers).

Rotation is implemented by converting `(x, y)` to cube coordinates scaled
by 2, applying `(X, Y, Z) -> (-Z, -X, -Y)` per step, then converting
back. (See line 25 onward in `editor.js`, and `doubled_y_to_cube` /
`cube_to_doubled_y` in `geometry.rs`.)

### Visible-key Mapping vs `.wtn` 56-cell Indexing

WTN geometry tables use **visible** key indices (`0..52`). However `.wtn`
files operate on the 56-cell 4×14 musical grid (`Key_0..Key_55`).

After the geometric hit-test identifies `(wtn_board, visible_key_index)`,
the editor maps that visible index onto the 0..55 indexing the rest of
the codebase uses. This final step is intentionally separate so we can
later change `.wtn` handling (e.g. eliminate unused indices) without
changing the hex-grid projection logic.

### Overlay / Apply Semantics

- Imported cells render with a red border + red text overlay.
- Click "Apply" (or press Enter) to commit the overlay into the in-memory
  layout.
- Esc / "Cancel" aborts placement without applying.
- Missing / incomplete `.ltn` entries are ignored.

Relevant code:

- LTN/WTN/XTN parsing + hex projection in the editor: `assets/editor.js`
- Rust-side INI parser (handles all three extensions, since they share
  the same `[BoardN]` / `Key_N` / `Chan_N` / `Col_N` format):
  `src/xtn.rs::parse_xtn`, with a Wooting-specific wrapper at
  `src/wooting/wtn.rs::parse_wtn`
- Hex-grid geometry tables + rotation helpers: `src/geometry.rs`
