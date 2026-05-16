mod cli;
mod config;
mod edit;
mod exquis;
mod geometry;
mod hud;
mod layouts;
mod logging;
mod midi_out;
mod mts;
mod settings;
mod wooting;
mod xtn;

use anyhow::{Context, Result, bail};
use clap::{CommandFactory, Parser};
use cli::{Cli, Commands};
use exquis::commands::{
    cmd_control, cmd_dev, cmd_highlight, cmd_load, cmd_midi, cmd_new, cmd_pad_set, cmd_pads,
    cmd_serve,
};
use exquis::midi::{DeviceSelection, list_devices};
use exquis::proto::color_help_text;

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Help { command } => print_help(command.as_deref()),
        Commands::List => cmd_list(),
        Commands::Midi(args) => cmd_midi(args),
        Commands::Dev { action, zones } => cmd_dev(action, zones),
        Commands::Pads(args) => cmd_pads(args),
        Commands::Pad {
            pad,
            color,
            device,
            legacy,
        } => cmd_pad_set(
            pad,
            color,
            device.unwrap_or(DeviceSelection::All),
            legacy,
        ),
        Commands::Serve {
            file,
            pb_range,
            x_gain,
            output,
            mts_esp,
            hud,
            hud_port,
            xenharm_url,
            osc_port,
            tune_supercollider,
            color,
        } => {
            let s = settings::load();
            let file = match file {
                Some(f) => layouts::resolve_layout_path(&f),
                None => match (&s.last_wtn, &s.last_xtn) {
                    (Some(w), _) => {
                        layouts::resolve_layout_path(std::path::Path::new(w))
                    }
                    (None, Some(x)) => {
                        layouts::resolve_layout_path(std::path::Path::new(x))
                    }
                    (None, None) => {
                        bail!("no FILE argument and no persisted layout; run e.g. `xentool serve edo31.wtn` once");
                    }
                },
            };
            if is_wtn(&file) {
                let output = output.unwrap_or_else(|| cli::DEFAULT_OUTPUT_WOOTING.to_string());
                wooting::commands::cmd_serve_wtn(file, output, hud, hud_port, xenharm_url, osc_port, tune_supercollider, &s.wooting)
            } else {
                let output = output.unwrap_or_else(|| cli::DEFAULT_OUTPUT_EXQUIS.to_string());
                cmd_serve(file, pb_range, x_gain, output, mts_esp, hud, hud_port, xenharm_url, osc_port, tune_supercollider, color.to_correction()?)
            }
        }
        Commands::New {
            file,
            edo,
            boards,
            pitch_offset,
            force,
        } => {
            let file = layouts::resolve_layout_path(&file);
            if is_wtn(&file) {
                wooting::commands::cmd_new_wtn(file, edo, boards, pitch_offset, force)
            } else {
                cmd_new(file, edo, boards, pitch_offset, force)
            }
        }
        Commands::Load { file, color } => {
            let file = layouts::resolve_layout_path(&file);
            if is_wtn(&file) {
                wooting::commands::cmd_load_wtn(file)
            } else {
                cmd_load(file, color.to_correction()?)
            }
        }
        Commands::Geometries => cmd_geometries(),
        Commands::Geometry { name, boards, out, no_open } => {
            cmd_geometry(name, boards, out, !no_open)
        }
        Commands::Edit { file, port, open } => {
            edit::run_edit_server(layouts::resolve_layout_path(&file), port, open)
        }
        Commands::Control {
            control,
            color,
            device,
        } => cmd_control(control, color, device),
        Commands::Highlight {
            note,
            velocity,
            device,
        } => cmd_highlight(note, velocity, device),
    }
}

fn print_help(command: Option<&str>) -> Result<()> {
    let mut cmd = Cli::command();
    if let Some(name) = command {
        match name {
            "list" => {
                let mut sub = cli::Commands::list_command();
                sub.print_long_help()?;
            }
            "midi" => {
                let mut sub = cli::Commands::midi_command();
                sub.print_long_help()?;
            }
            "dev" => {
                let mut sub = cli::Commands::dev_command();
                sub.print_long_help()?;
            }
            "pads" => {
                let mut sub = cli::Commands::pads_command();
                sub.print_long_help()?;
            }
            "pad" => {
                let mut sub = cli::Commands::pad_command();
                sub.print_long_help()?;
            }
            "load" => {
                let mut sub = cli::Commands::load_command();
                sub.print_long_help()?;
            }
            "control" => {
                let mut sub = cli::Commands::control_command();
                sub.print_long_help()?;
            }
            _ => bail!("unknown help topic `{name}`"),
        }
        println!();
        println!("Color syntax: {}", color_help_text());
        Ok(())
    } else {
        cmd.print_long_help()?;
        println!();
        println!();
        println!("Color syntax: {}", color_help_text());
        Ok(())
    }
}

/// Returns true if the file looks like a Wooting layout file (routes to the
/// Wooting backend). All other cases fall back to the Exquis backend.
fn is_wtn(p: &std::path::Path) -> bool {
    p.extension()
        .and_then(|e| e.to_str())
        .map(|e| e.eq_ignore_ascii_case("wtn"))
        .unwrap_or(false)
}

fn cmd_list() -> Result<()> {
    let devices = list_devices()?;
    let exquis_count = devices.len();
    if devices.is_empty() {
        println!("No Exquis MIDI devices found.");
    }

    for device in devices {
        println!("[{}] {}", device.number, device.label);
        println!(
            "  id: {}",
            device
                .usb_info
                .as_ref()
                .map(|info| info.unique_id.as_str())
                .unwrap_or("unavailable")
        );
        if let Some(info) = device.usb_info.as_ref() {
            println!("  usb: {:04x}:{:04x}", info.vendor_id, info.product_id);
            if let Some(maker) = info.manufacturer.as_deref() {
                println!("  manufacturer: {maker}");
            }
            if let Some(serial) = info.serial_number.as_deref() {
                println!("  serial: {serial}");
            }
            println!("  location: {}", info.location);
            println!(
                "  usb-address: bus {} addr {}",
                info.bus_number, info.address
            );
            if !info.port_numbers.is_empty() {
                println!(
                    "  usb-ports: {}",
                    info.port_numbers
                        .iter()
                        .map(u8::to_string)
                        .collect::<Vec<_>>()
                        .join(".")
                );
            }
            println!(
                "  firmware: {}",
                info.firmware_version.as_deref().unwrap_or("unavailable")
            );
        } else {
            println!("  firmware: unavailable");
        }
        println!(
            "  midi-in: {}",
            if device.input_names.is_empty() {
                device.input_name.as_deref().unwrap_or("<missing input>").to_string()
            } else {
                device.input_names.join(", ")
            }
        );
        println!(
            "  midi-out: {}",
            if device.output_names.is_empty() {
                device.output_name.as_deref().unwrap_or("<missing output>").to_string()
            } else {
                device.output_names.join(", ")
            }
        );
    }

    // Append connected Wooting keyboards, numbered after the last Exquis.
    for block in wooting::commands::list_wootings(exquis_count + 1) {
        print!("{block}");
    }

    Ok(())
}


fn cmd_geometries() -> Result<()> {
    let geoms = geometry::geometries(4);
    for g in &geoms {
        let aliases = if g.aliases.is_empty() {
            String::new()
        } else {
            format!(" (aliases: {})", g.aliases.join(", "))
        };
        println!("{}{}", g.name, aliases);
        println!("  {}", g.description);
        println!("  boards: {}", g.boards.len());
        if let Some(first) = g.boards.first() {
            println!("  pads per board: {}", first.len());
        }
        match g.board_shift {
            Some((dx, dy)) => println!("  board shift (hex coords): (x={dx}, y={dy})"),
            None => println!("  board shift: irregular"),
        }
        // Every board shares the same internal geometry (only the position in
        // the unified lattice differs via `board shift`). Emit one definition.
        if let Some(board0) = g.boards.first() {
            let tuples: Vec<serde_json::Value> = board0
                .iter()
                .map(|&(pad, x, y)| serde_json::json!([pad, x, y]))
                .collect();
            let json = serde_json::to_string(&tuples).unwrap_or_else(|_| "[]".into());
            println!("  pads [pad, x, y]:");
            println!("    {json}");
        }
        println!();
    }
    Ok(())
}

fn cmd_geometry(
    name: String,
    boards: u8,
    out: Option<std::path::PathBuf>,
    open_browser: bool,
) -> Result<()> {
    let info = geometry::geometry_by_name(&name, boards).with_context(|| {
        format!(
            "unknown geometry `{name}`. Try: exquis, lumatone (ltn), wooting (wtn)"
        )
    })?;
    let svg = geometry::render_geometry_svg(&info);
    let path = match out {
        Some(p) => p,
        None => {
            let mut p = std::env::temp_dir();
            p.push(format!("xentool-geometry-{}.svg", info.name));
            p
        }
    };
    std::fs::write(&path, &svg).with_context(|| format!("writing {}", path.display()))?;
    println!("wrote {}", path.display());
    if open_browser {
        let _ = open::that(&path);
    }
    Ok(())
}




