//! CLI command handlers for the Wooting backend (load, serve, new).
//!
//! Each is a sibling of the Exquis `cmd_*` functions and is dispatched when
//! the target file has the `.wtn` extension.

use anyhow::{Context, Result, bail};
use std::path::PathBuf;

use crate::settings::WootingSettings;
use crate::wooting::{
    analog,
    geometry as woot_geom,
    hidmap::{compute_compact_col_offsets, wtn_index_for_loc, HidMap},
    rgb, serve,
    wtn::{new_blank, parse_wtn, write_wtn, WtnCell, WTN_CELLS_PER_BOARD},
};

// ---------------------------------------------------------------------------
// `xentool new FILE.wtn`
// ---------------------------------------------------------------------------

pub fn cmd_new_wtn(file: PathBuf, edo: i32, boards: u8, pitch_offset: i32, force: bool) -> Result<()> {
    if edo < 1 {
        bail!("edo must be >= 1");
    }
    if boards < 1 {
        bail!("boards must be >= 1");
    }
    if file.exists() && !force {
        bail!("{} already exists; pass --force to overwrite", file.display());
    }
    if let Some(parent) = file.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }
    }
    let w = new_blank(edo, boards, pitch_offset);
    std::fs::write(&file, write_wtn(&w))
        .with_context(|| format!("writing {}", file.display()))?;
    println!(
        "created {} (edo={edo}, pitch_offset={pitch_offset}, {boards} board{}, 56 cells each, blank)",
        file.display(),
        if boards == 1 { "" } else { "s" }
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// `xentool load FILE.wtn` — write LED colors from the WTN to the connected
// Wooting(s). Non-hot path: single-shot, no threading. Uses
// `array_set_single` + `array_update_keyboard` for each device.
// ---------------------------------------------------------------------------

pub fn cmd_load_wtn(file: PathBuf) -> Result<()> {
    let wtn = parse_wtn(&std::fs::read_to_string(&file)
        .with_context(|| format!("reading {}", file.display()))?)?;

    let map = HidMap::default_60he_ansi_guess();

    // Enumerate connected Wootings (index 0..N-1 via the RGB SDK device count).
    rgb::with_sdk(|sdk| {
        let count = sdk.device_count();
        if count == 0 {
            println!("no Wooting keyboards detected");
            return Ok(());
        }
        for dev_idx in 0..count {
            // The board number of the .wtn we want to apply.
            let wtn_board = dev_idx;
            let Some(cells) = wtn.boards.get(&wtn_board) else {
                println!("[{dev_idx}] no [Board{wtn_board}] in file; skipping");
                continue;
            };
            // Paired-pair rotation rule: even-indexed boards rotate 180° when
            // they have a partner. Matches the editor / serve conventions.
            let rotation_deg: u16 = if woot_geom::rotated(wtn_board, count) { 180 } else { 0 };
            paint_board(sdk, dev_idx, cells, &map, rotation_deg)?;
            println!("[{dev_idx}] painted {} cells from Board{wtn_board}", WTN_CELLS_PER_BOARD);
        }
        Ok(())
    })
}

fn paint_board(
    sdk: &rgb::RgbSdk,
    dev_idx: u8,
    cells: &[WtnCell],
    map: &HidMap,
    rotation_deg: u16,
) -> Result<()> {
    // For each HID in the map, look up its WTN cell index (with rotation &
    // compact-col offsets applied), and set the LED at the PHYSICAL
    // (led_row, led_col) — the LED grid isn't rotated by our code; we just
    // change which cell the pad displays.
    let compact = compute_compact_col_offsets(map, rotation_deg);
    for (_, loc) in map.all_locs() {
        let Some(idx) = wtn_index_for_loc(loc, rotation_deg, &compact) else { continue };
        if idx >= cells.len() {
            continue;
        }
        let cell = cells[idx];
        if cell.chan == 0 {
            // "missing" marker → paint black.
            sdk.array_set_single(dev_idx, loc.led_row, loc.led_col, (0, 0, 0))?;
        } else {
            sdk.array_set_single(dev_idx, loc.led_row, loc.led_col, cell.color)?;
        }
    }
    sdk.array_update_keyboard(dev_idx)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// `xentool serve FILE.wtn` — see `serve` module for the full implementation.
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
pub fn cmd_serve_wtn(
    file: PathBuf,
    midi_port: String,
    hud: bool,
    hud_port: u16,
    xenharm_url: String,
    osc_port: u16,
    tune_supercollider: bool,
    jack_midi_mirror: bool,
    settings: &WootingSettings,
) -> Result<()> {
    serve::cmd_serve_wtn(file, midi_port, hud, hud_port, xenharm_url, osc_port, tune_supercollider, jack_midi_mirror, settings)
}

/// Pretty-print connected Wootings for `xentool list`. Silently returns empty
/// if the SDK isn't installed (so users who only own Exquis hardware aren't
/// blocked).
pub fn list_wootings(start_index: usize) -> Vec<String> {
    let devs = match analog::with_sdk(|sdk| sdk.connected_devices(32)) {
        Ok(d) => d,
        Err(_) => return Vec::new(),
    };
    let mut out = Vec::with_capacity(devs.len());
    for (i, d) in devs.iter().enumerate() {
        let idx = start_index + i;
        let mut s = String::new();
        s.push_str(&format!("[{}] {} {}\n", idx, d.manufacturer, d.name));
        s.push_str(&format!("  id: wooting:{:016x}\n", d.id));
        s.push_str(&format!("  usb: {:04x}:{:04x}\n", d.vendor_id, d.product_id));
        s.push_str(&format!("  kind: wooting-analog\n"));
        out.push(s);
    }
    out
}
