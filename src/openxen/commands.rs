//! CLI command handlers for the openxen backend (dispatched for `.ltn` files).

use anyhow::{Context, Result, bail};
use std::path::PathBuf;

use crate::openxen::{hid, serve};
use crate::settings::OpenxenSettings;
use crate::wooting::wtn::{new_blank, write_wtn};

/// `xentool new FILE.ltn` — blank Lumatone-style layout (56 keys per board, default 5 boards).
pub fn cmd_new_ltn(file: PathBuf, edo: i32, boards: u8, pitch_offset: i32, force: bool) -> Result<()> {
    if edo < 1 {
        bail!("edo must be >= 1");
    }
    if boards < 1 || boards > serve::MAX_BOARDS {
        bail!("boards must be 1..={}", serve::MAX_BOARDS);
    }
    if file.exists() && !force {
        bail!("{} already exists; pass --force to overwrite", file.display());
    }
    if let Some(parent) = file.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
        }
    }
    std::fs::write(&file, write_wtn(&new_blank(edo, boards, pitch_offset)))
        .with_context(|| format!("writing {}", file.display()))?;
    println!("created {} (edo={edo}, pitch_offset={pitch_offset}, {boards} board(s), 56 keys each, blank)", file.display());
    Ok(())
}

/// `xentool load FILE.ltn` — openxen LEDs are not supported by the firmware yet.
pub fn cmd_load_ltn(file: PathBuf) -> Result<()> {
    bail!("{}: openxen LED colours are not supported yet (module firmware pending)", file.display())
}

#[allow(clippy::too_many_arguments)]
pub fn cmd_serve_ltn(
    file: PathBuf,
    midi_port: String,
    hud: bool,
    hud_port: u16,
    xenharm_url: String,
    osc_port: u16,
    tune_supercollider: bool,
    settings: &OpenxenSettings,
) -> Result<()> {
    serve::cmd_serve_ltn(file, midi_port, hud, hud_port, xenharm_url, osc_port, tune_supercollider, settings)
}

/// Pretty-print attached openxen modules for `xentool list`. Empty when hidapi cannot be
/// initialised or nothing is attached.
pub fn list_modules(start_index: usize) -> Vec<String> {
    let Ok(mut api) = hidapi::HidApi::new() else { return Vec::new() };
    let Ok(mods) = hid::list_modules(&mut api) else { return Vec::new() };
    mods.iter()
        .enumerate()
        .map(|(i, m)| {
            format!(
                "[{}] {}\n  id: openxen:{}\n  usb: {:04x}:{:04x}\n  kind: openxen-stream\n",
                start_index + i,
                m.product,
                m.serial,
                m.vendor_id,
                m.product_id
            )
        })
        .collect()
}
