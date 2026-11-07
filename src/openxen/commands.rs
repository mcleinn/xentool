//! CLI command handlers for the openxen backend (dispatched for `.ltn` files).

use anyhow::{Context, Result, bail};
use std::path::PathBuf;

use crate::openxen::{hid, led, serve};
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

/// `xentool load FILE.ltn` — paint each attached module with its board's layout colours and
/// exit. The host timeout is disabled (0) so the colours stay until the module is reset; a
/// later `serve` takes over again.
pub fn cmd_load_ltn(file: PathBuf, settings: &OpenxenSettings) -> Result<()> {
    let text = std::fs::read_to_string(&file).with_context(|| format!("reading {}", file.display()))?;
    let wtn = crate::wooting::wtn::parse_wtn(&text)?;
    let mut api = hidapi::HidApi::new().context("initialising hidapi")?;
    let modules = hid::list_modules(&mut api)?;
    if modules.is_empty() {
        bail!("no openxen modules attached");
    }
    let cfg = led::LedConfig { timeout_ms: 0, ..serve::led_config(settings) };
    let mut taken = std::collections::BTreeMap::new();
    for m in &modules {
        let Some(board) = serve::assign_board(&m.serial, settings, &taken) else {
            println!("{}: no free board, skipped", m.serial);
            continue;
        };
        taken.insert(board, std::sync::Arc::<str>::from(m.serial.as_str()));
        let dev = api.open_path(&m.path).with_context(|| format!("opening {}", m.serial))?;
        let mut reports = vec![led::config_report(&cfg)];
        reports.extend(led::frame_reports(&serve::board_frame(&wtn, board)));
        for r in &reports {
            dev.write(&led::with_report_id(r)).with_context(|| format!("writing to {}", m.serial))?;
        }
        println!("{} -> board{board}: layout colours loaded", m.serial);
    }
    Ok(())
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
