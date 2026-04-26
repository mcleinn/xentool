mod cli;
mod config;
mod exquis_proto;
mod logging;
mod midi;
mod mpe;
mod ui;
mod usb;
mod edit;
mod geometry;
mod wooting;
mod layouts;
mod mts;
mod settings;
mod tuning;
mod xtn;

use anyhow::{Context, Result, bail};
use clap::{CommandFactory, Parser};
use cli::{Cli, Commands, DevAction, PadsCommands};
use crossterm::tty::IsTty;
use exquis_proto::{Color, NamedZone, color_help_text};
use logging::{JsonlLogger, default_log_path};
use midi::{DeviceSelection, list_devices, open_inputs, send_to_outputs};
use mpe::DecodedEvent;
use std::io;
use std::sync::mpsc;

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
            output,
            mts_esp,
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
                wooting::commands::cmd_serve_wtn(file, output, &s.wooting)
            } else {
                cmd_serve(file, pb_range, output, mts_esp, color.to_correction()?)
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
        Commands::Edit { file, port, no_open } => {
            edit::run_edit_server(layouts::resolve_layout_path(&file), port, !no_open)
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
            device.input_name.as_deref().unwrap_or("<missing input>")
        );
        println!(
            "  midi-out: {}",
            device.output_name.as_deref().unwrap_or("<missing output>")
        );
    }

    // Append connected Wooting keyboards, numbered after the last Exquis.
    for block in wooting::commands::list_wootings(exquis_count + 1) {
        print!("{block}");
    }

    Ok(())
}

fn cmd_dev(action: DevAction, zones: Vec<NamedZone>) -> Result<()> {
    let devices = list_devices()?;
    if devices.is_empty() {
        println!("No Exquis MIDI devices found.");
        return Ok(());
    }

    let mask = if zones.is_empty() {
        cli::default_zone_mask()
    } else {
        zones.iter().fold(0u8, |acc, zone| acc | zone.bit())
    };

    let bytes = match action {
        DevAction::On => exquis_proto::enter_dev_mode(mask),
        DevAction::Off => exquis_proto::exit_dev_mode(),
    };

    send_to_outputs(&devices, DeviceSelection::All, &bytes)?;
    for device in devices {
        println!(
            "[{}] developer mode {}",
            device.number,
            match action {
                DevAction::On => format!("enabled (mask 0x{mask:02X})"),
                DevAction::Off => "disabled".to_string(),
            }
        );
    }
    Ok(())
}

fn cmd_pads(args: cli::PadsArgs) -> Result<()> {
    match args.command {
        PadsCommands::Clear { device, legacy } => {
            cmd_pads_fill(Color::named("black").unwrap(), device, legacy)
        }
        PadsCommands::Fill {
            color,
            device,
            legacy,
        } => cmd_pads_fill(color, device, legacy),
        PadsCommands::Test { device, legacy } => cmd_pads_test(device, legacy),
    }
}

fn cmd_pads_fill(color: Color, device: DeviceSelection, legacy: bool) -> Result<()> {
    let devices = list_devices()?;
    let selected = midi::select_devices(&devices, &device)?;
    if selected.is_empty() {
        println!("No Exquis MIDI devices found.");
        return Ok(());
    }

    if legacy {
        // Legacy: take over pad zone (breaks MPE)
        let messages = vec![
            exquis_proto::enter_dev_mode(NamedZone::Pads.bit()),
            exquis_proto::fill_all_pads(color),
        ];
        for message in messages {
            send_to_outputs(&selected, DeviceSelection::All, &message)?;
        }
        for target in selected {
            println!(
                "[{}] filled 61 pads with {} (legacy, MPE disabled)",
                target.number, color,
            );
        }
    } else {
        // Default: snapshot approach — preserves MPE
        send_to_outputs(
            &selected,
            DeviceSelection::All,
            &exquis_proto::enter_dev_mode(exquis_proto::DEV_MASK_NO_PADS),
        )?;
        send_to_outputs(
            &selected,
            DeviceSelection::All,
            &exquis_proto::snapshot_fill_color(color),
        )?;
        for target in selected {
            println!(
                "[{}] filled 61 pads with {} (MPE preserved)",
                target.number, color,
            );
        }
    }
    Ok(())
}

fn cmd_pad_set(pad: u8, color: Color, device: DeviceSelection, legacy: bool) -> Result<()> {
    if pad > 60 {
        bail!("pad index must be in 0..=60");
    }

    let devices = list_devices()?;
    let selected = midi::select_devices(&devices, &device)?;
    if selected.is_empty() {
        println!("No Exquis MIDI devices found.");
        return Ok(());
    }

    if legacy {
        // Legacy: take over pad zone (breaks MPE)
        let messages = vec![
            exquis_proto::enter_dev_mode(NamedZone::Pads.bit()),
            exquis_proto::set_led_color(pad, color),
        ];
        for message in messages {
            send_to_outputs(&selected, DeviceSelection::All, &message)?;
        }
        for target in selected {
            println!(
                "[{}] set pad {} to {} (legacy, MPE disabled)",
                target.number, pad, color,
            );
        }
    } else {
        // Default: snapshot approach — set one pad color, preserve all others
        // We build a snapshot with default colors (black) and the target pad colored
        let mut colors = [Color::new(0, 0, 0); 61];
        colors[pad as usize] = color;
        send_to_outputs(
            &selected,
            DeviceSelection::All,
            &exquis_proto::enter_dev_mode(exquis_proto::DEV_MASK_NO_PADS),
        )?;
        send_to_outputs(
            &selected,
            DeviceSelection::All,
            &exquis_proto::snapshot_set_colors(&colors),
        )?;
        for target in selected {
            println!(
                "[{}] set pad {} to {} (MPE preserved)",
                target.number, pad, color,
            );
        }
    }
    Ok(())
}

// Control-button LED palette for Exquis serve.
//
// IMPORTANT: `Color::new(r, g, b)` stores and transmits **7-bit** values
// (0..=127) — `set_led_color` emits them verbatim inside the SysEx payload,
// and any byte with bit 7 set would corrupt the stream. Keep every channel
// below 128.
const CTRL_BASE_RGB: (u8, u8, u8) = (64, 0, 96); // dark violet
const CTRL_ACTIVE_SHIFT_RGB: (u8, u8, u8) = (127, 127, 0); // pure yellow
const CTRL_PRESSED_RGB: (u8, u8, u8) = (127, 127, 127); // white

/// Base color for a control button. Neither Settings, Up, nor Down is ever
/// dark:
/// - 100 (Settings): always violet.
/// - 107 (Up): yellow only if the board's `octave_shift > 0`, else violet.
/// - 106 (Down): yellow only if the board's `octave_shift < 0`, else violet.
fn ctrl_base_color(cc: u8, octave_shift: i32) -> exquis_proto::Color {
    match cc {
        107 if octave_shift > 0 => {
            Color::new(CTRL_ACTIVE_SHIFT_RGB.0, CTRL_ACTIVE_SHIFT_RGB.1, CTRL_ACTIVE_SHIFT_RGB.2)
        }
        106 if octave_shift < 0 => {
            Color::new(CTRL_ACTIVE_SHIFT_RGB.0, CTRL_ACTIVE_SHIFT_RGB.1, CTRL_ACTIVE_SHIFT_RGB.2)
        }
        _ => Color::new(CTRL_BASE_RGB.0, CTRL_BASE_RGB.1, CTRL_BASE_RGB.2),
    }
}

fn pressed_color() -> exquis_proto::Color {
    Color::new(CTRL_PRESSED_RGB.0, CTRL_PRESSED_RGB.1, CTRL_PRESSED_RGB.2)
}

/// Send a single `set_led_color`. Assumes dev mode is already active on the
/// device; avoid redundant `enter_dev_mode` calls in hot paths.
fn paint_ctrl_button(
    device: &crate::midi::ExquisDevice,
    cc: u8,
    color: exquis_proto::Color,
) {
    let _ = send_to_outputs(
        &[device.clone()],
        DeviceSelection::All,
        &exquis_proto::set_led_color(cc, color),
    );
}

/// Re-enter dev mode for the non-pad zones and paint all three control
/// buttons. Used at startup and after layout cycles.
fn paint_all_ctrl_buttons_for_board(
    device: &crate::midi::ExquisDevice,
    octave_shift: i32,
) {
    let _ = send_to_outputs(
        &[device.clone()],
        DeviceSelection::All,
        &exquis_proto::enter_dev_mode(exquis_proto::DEV_MASK_NO_PADS),
    );
    for &cc in &[100u8, 106u8, 107u8] {
        paint_ctrl_button(device, cc, ctrl_base_color(cc, octave_shift));
    }
}

/// Returns true if applying `new_shift` octaves to this board would keep every
/// mapped pad's 12-TET MIDI note inside [0, 127]. Empty boards trivially pass.
fn shift_in_range(bl: &xtn::BoardLayout, edo: i32, pitch_offset: i32, new_shift: i32) -> bool {
    for pad in bl.pads.values() {
        let abs_pitch = (pad.chan as i32 - 1) * edo
            + pad.key as i32
            + pitch_offset
            + 2 * edo
            + new_shift * edo;
        let midi_12tet = (abs_pitch as f64 * 12.0 / edo as f64).round() as i32;
        if !(0..=127).contains(&midi_12tet) {
            return false;
        }
    }
    true
}

fn rebuild_mts_table(
    layout: &xtn::XtnLayout,
    boards: &[config::BoardAssignment],
    edo: i32,
    pitch_offset: i32,
    shifts: &std::collections::HashMap<usize, i32>,
) -> [f64; 128] {
    let mut freqs = [0.0f64; 128];
    for n in 0..128 {
        freqs[n] = mts::edo_freq_hz(12, n as i32);
    }
    for (board_idx, b) in boards.iter().enumerate() {
        let shift = shifts.get(&b.device.number).copied().unwrap_or(0);
        let note_base = (board_idx * 61) as u16;
        if let Some(bl) = layout.boards.get(&b.board_name) {
            for pad in 0..61u8 {
                let midi_note = note_base + pad as u16;
                if midi_note >= 128 {
                    break;
                }
                let virtual_pitch = match bl.pads.get(&pad) {
                    Some(e) => {
                        ((e.chan as i32 - 1) * edo)
                            + (e.key as i32)
                            + pitch_offset
                            + 2 * edo
                            + shift * edo
                    }
                    None => (midi_note as i32) + pitch_offset + 2 * edo + shift * edo,
                };
                freqs[midi_note as usize] = mts::edo_freq_hz(edo, virtual_pitch);
            }
        }
    }
    freqs
}

fn cmd_serve(
    file: std::path::PathBuf,
    pb_range: f64,
    output: String,
    mts_esp: bool,
    correction: exquis_proto::ColorCorrection,
) -> Result<()> {
    let mut current_layout_path: std::path::PathBuf = file.clone();
    let layout = xtn::parse_xtn(&file)?;
    let devices = list_devices()?;

    if devices.is_empty() {
        println!("No Exquis MIDI devices found.");
        return Ok(());
    }

    let edo = layout.edo.with_context(|| {
        format!(
            "Edo= not set in {}. Add e.g. 'Edo=31' before the first [Board] section.",
            file.display()
        )
    })?;

    let scale_name = format!("{}-EDO", edo);

    // Sync connected devices → board0..boardN
    let boards = config::sync_boards(&devices)?;
    for board in &boards {
        println!(
            "  {} → [{}] {}",
            board.board_name, board.device.number, board.device.label
        );
    }

    if !correction.is_identity() {
        println!(
            "color correction: gamma={} saturation={} rgb_gain=({}, {}, {})",
            correction.gamma,
            correction.saturation,
            correction.r_gain,
            correction.g_gain,
            correction.b_gain
        );
    }

    // Set pad colors via snapshot (note = pad_id, colors from .xtn)
    for board in &boards {
        let board_layout = match layout.boards.get(&board.board_name) {
            Some(bl) => bl,
            None => continue,
        };

        let mut pads = [(0u8, Color::new(0, 0, 0)); 61];
        for i in 0..61u8 {
            let color = board_layout
                .pads
                .get(&i)
                .map(|entry| entry.color)
                .unwrap_or(Color::new(0, 0, 0))
                .corrected(&correction);
            pads[i as usize] = (i, color);
        }

        send_to_outputs(
            &[board.device.clone()],
            DeviceSelection::All,
            &exquis_proto::enter_dev_mode(exquis_proto::DEV_MASK_NO_PADS),
        )?;
        send_to_outputs(
            &[board.device.clone()],
            DeviceSelection::All,
            &exquis_proto::snapshot_set_pads(&pads),
        )?;

        println!("[{}] {} colors set (MPE preserved)", board.device.number, board.board_name);
    }

    // Build per-board tuning states
    let mut board_tunings: std::collections::HashMap<usize, tuning::TuningState> =
        std::collections::HashMap::new();
    for board in &boards {
        if let Some(bl) = layout.boards.get(&board.board_name) {
            let state =
                tuning::TuningState::from_board(bl, edo, layout.pitch_offset, 2, pb_range);
            board_tunings.insert(board.device.number, state);
        }
    }

    // Open MIDI inputs
    let (tx, rx) = mpsc::channel::<mpe::InputMessage>();
    let _connections = open_inputs(&devices, tx)?;

    if mts_esp {
        // MTS-ESP mode: register as master, Pianoteq listens directly on Exquis
        let mut freqs = [0.0f64; 128];
        for note in 0..128usize {
            freqs[note] = mts::edo_freq_hz(12, note as i32);
        }
        for (board_idx, board) in boards.iter().enumerate() {
            let note_base = (board_idx * 61) as u16;
            if let Some(bl) = layout.boards.get(&board.board_name) {
                for pad in 0..61u8 {
                    let midi_note = note_base + pad as u16;
                    if midi_note >= 128 { break; }
                    let virtual_pitch = match bl.pads.get(&pad) {
                        Some(e) => ((e.chan as i32 - 1) * edo) + (e.key as i32) + layout.pitch_offset + 2 * edo,
                        None => (midi_note as i32) + layout.pitch_offset + 2 * edo,
                    };
                    freqs[midi_note as usize] = mts::edo_freq_hz(edo, virtual_pitch);
                }
            }
        }
        let master = mts::MtsMaster::register()?;
        master.set_scale_name(&scale_name)?;
        master.set_note_tunings(&freqs)?;
        println!("\nMTS-ESP master registered ({scale_name})");
        let boards_cyc = boards.clone();
        let correction_cyc = correction;
        let mut current_layout: xtn::XtnLayout = layout.clone();
        let mut current_edo: i32 = edo;
        let mut current_shifts: std::collections::HashMap<usize, i32> =
            std::collections::HashMap::new();
        let display = std::rc::Rc::new(std::cell::RefCell::new(ui::ServeDisplay {
            tuning_name: format!("edo{edo}"),
            shifts: boards_cyc
                .iter()
                .map(|b| (b.device.number, 0))
                .collect(),
        }));
        let display_for_cb = display.clone();
        // Initial paint: dark violet on Settings/Up/Down for every board.
        for b in &boards_cyc {
            paint_all_ctrl_buttons_for_board(&b.device, 0);
        }
        ui::run_serve_ui(rx, &master, &scale_name, display, &mut |device_number, cc, pressed| {
            let Some(board_idx) = boards_cyc.iter().position(|b| b.device.number == device_number)
            else {
                return Ok(());
            };
            let board = boards_cyc[board_idx].clone();

            // RELEASE EDGE: restore the pressed button to its current base color
            // (which reflects the board's current octave_shift).
            if !pressed {
                let cur = current_shifts.get(&device_number).copied().unwrap_or(0);
                paint_ctrl_button(&board.device, cc, ctrl_base_color(cc, cur));
                return Ok(());
            }

            // PRESS EDGE.
            // Flash the pressed button white first. For non-cycling cases we
            // leave it white for the hold duration; the release branch above
            // restores the base.
            paint_ctrl_button(&board.device, cc, pressed_color());

            match cc {
                107 | 106 => {
                    let shift_before = current_shifts.get(&device_number).copied().unwrap_or(0);
                    let delta: i32 = if cc == 107 { 1 } else { -1 };
                    let new_shift = shift_before + delta;
                    let bl = match current_layout.boards.get(&board.board_name) {
                        Some(b) => b,
                        None => return Ok(()),
                    };
                    if !shift_in_range(
                        bl,
                        current_edo,
                        current_layout.pitch_offset,
                        new_shift,
                    ) {
                        // Clamp: keep shift, no retune.
                        return Ok(());
                    }
                    current_shifts.insert(device_number, new_shift);
                    display_for_cb
                        .borrow_mut()
                        .shifts
                        .insert(device_number, new_shift);
                    let freqs = rebuild_mts_table(
                        &current_layout,
                        &boards_cyc,
                        current_edo,
                        current_layout.pitch_offset,
                        &current_shifts,
                    );
                    let _ = master.set_note_tunings(&freqs);
                    let other_cc: u8 = if cc == 107 { 106 } else { 107 };
                    paint_ctrl_button(
                        &board.device,
                        other_cc,
                        ctrl_base_color(other_cc, new_shift),
                    );
                    return Ok(());
                }
                100 => {
                    // Cycle layout. (Settings button.)
                }
                _ => return Ok(()),
            }
            // --- Settings pressed: cycle to next .xtn ---
            let new_path = match layouts::next(
                layouts::LayoutKind::Xtn,
                &current_layout_path,
            ) {
                Ok(p) => p,
                Err(e) => {
                    eprintln!("layout cycle: {e:#}");
                    return Ok(());
                }
            };
            let new_layout = match xtn::parse_xtn(&new_path) {
                Ok(l) => l,
                Err(e) => {
                    eprintln!("layout cycle: {e:#}");
                    return Ok(());
                }
            };
            let new_edo = match new_layout.edo {
                Some(e) => e,
                None => {
                    eprintln!("layout cycle: {} has no Edo=", new_path.display());
                    return Ok(());
                }
            };
            current_shifts.clear();
            current_layout = new_layout.clone();
            current_edo = new_edo;
            current_layout_path = new_path.clone();
            {
                let mut d = display_for_cb.borrow_mut();
                d.tuning_name = format!("edo{new_edo}");
                d.shifts = boards_cyc.iter().map(|b| (b.device.number, 0)).collect();
            }
            let freqs = rebuild_mts_table(
                &current_layout,
                &boards_cyc,
                current_edo,
                current_layout.pitch_offset,
                &current_shifts,
            );
            let _ = master.set_scale_name(&format!("{new_edo}-EDO"));
            let _ = master.set_note_tunings(&freqs);
            // Repaint pads.
            for b in &boards_cyc {
                let Some(bl) = new_layout.boards.get(&b.board_name) else {
                    continue;
                };
                let mut pads = [(0u8, Color::new(0, 0, 0)); 61];
                for i in 0..61u8 {
                    let c = bl
                        .pads
                        .get(&i)
                        .map(|entry| entry.color)
                        .unwrap_or(Color::new(0, 0, 0))
                        .corrected(&correction_cyc);
                    pads[i as usize] = (i, c);
                }
                let _ = send_to_outputs(
                    &[b.device.clone()],
                    DeviceSelection::All,
                    &exquis_proto::enter_dev_mode(exquis_proto::DEV_MASK_NO_PADS),
                );
                let _ = send_to_outputs(
                    &[b.device.clone()],
                    DeviceSelection::All,
                    &exquis_proto::snapshot_set_pads(&pads),
                );
                // After the snapshot we may have lost dev mode — re-enter and
                // restore Up/Down for this board to the new (shift=0) base.
                // Leave Settings alone on the pressed board: it stays white
                // while the user holds the key; the release branch restores
                // its base color.
                let _ = send_to_outputs(
                    &[b.device.clone()],
                    DeviceSelection::All,
                    &exquis_proto::enter_dev_mode(exquis_proto::DEV_MASK_NO_PADS),
                );
                paint_ctrl_button(&b.device, 106, ctrl_base_color(106, 0));
                paint_ctrl_button(&b.device, 107, ctrl_base_color(107, 0));
                if b.device.number != device_number {
                    // Other boards: the user is not holding Settings there,
                    // so repaint Settings too.
                    paint_ctrl_button(&b.device, 100, ctrl_base_color(100, 0));
                }
            }
            // Persist on a background thread.
            let persist_path = new_path.clone();
            std::thread::spawn(move || {
                settings::store_last_layout(layouts::LayoutKind::Xtn, &persist_path);
            });
            Ok(())
        })?;
        drop(master);
    } else {
        // Default: pitch bend retuning via loopMIDI output port
        let mut midi_outputs: std::collections::HashMap<usize, midir::MidiOutputConnection> =
            std::collections::HashMap::new();

        for board in &boards {
            let out = midir::MidiOutput::new("xentool-serve-output")?;
            let port = out
                .ports()
                .into_iter()
                .find(|p| out.port_name(p).ok().as_deref() == Some(output.as_str()))
                .with_context(|| format!("output port `{output}` not found. Install loopMIDI and create a port named \"{output}\"."))?;
            let conn = out
                .connect(&port, "xentool-serve")
                .with_context(|| format!("failed to open output `{output}`"))?;
            midi_outputs.insert(board.device.number, conn);
        }

        println!("\n{scale_name} | pitch bend retuning → {output} | pb_range={pb_range}");
        let boards_cyc = boards.clone();
        let correction_cyc = correction;
        let mut current_layout: xtn::XtnLayout = layout.clone();
        let mut current_edo: i32 = edo;
        let mut current_shifts: std::collections::HashMap<usize, i32> =
            std::collections::HashMap::new();
        let display = std::rc::Rc::new(std::cell::RefCell::new(ui::ServeDisplay {
            tuning_name: format!("edo{edo}"),
            shifts: boards_cyc
                .iter()
                .map(|b| (b.device.number, 0))
                .collect(),
        }));
        let display_for_cb = display.clone();
        // Initial paint: dark violet on Settings/Up/Down for every board.
        for b in &boards_cyc {
            paint_all_ctrl_buttons_for_board(&b.device, 0);
        }
        ui::run_serve_retune_ui(
            rx,
            &scale_name,
            &mut board_tunings,
            &mut midi_outputs,
            display,
            &mut |device_number, cc, pressed, tunings, outputs| {
                let Some(board_idx) = boards_cyc
                    .iter()
                    .position(|b| b.device.number == device_number)
                else {
                    return Ok(());
                };
                let board = boards_cyc[board_idx].clone();

                if !pressed {
                    let cur = current_shifts.get(&device_number).copied().unwrap_or(0);
                    paint_ctrl_button(&board.device, cc, ctrl_base_color(cc, cur));
                    return Ok(());
                }

                paint_ctrl_button(&board.device, cc, pressed_color());

                match cc {
                    107 | 106 => {
                        let shift_before =
                            current_shifts.get(&device_number).copied().unwrap_or(0);
                        let delta: i32 = if cc == 107 { 1 } else { -1 };
                        let new_shift = shift_before + delta;
                        let bl = match current_layout.boards.get(&board.board_name) {
                            Some(b) => b,
                            None => return Ok(()),
                        };
                        if !shift_in_range(
                            bl,
                            current_edo,
                            current_layout.pitch_offset,
                            new_shift,
                        ) {
                            return Ok(());
                        }
                        current_shifts.insert(device_number, new_shift);
                        display_for_cb
                            .borrow_mut()
                            .shifts
                            .insert(device_number, new_shift);
                        tunings.insert(
                            device_number,
                            tuning::TuningState::from_board(
                                bl,
                                current_edo,
                                current_layout.pitch_offset,
                                2 + new_shift,
                                pb_range,
                            ),
                        );
                        // Repaint only the OTHER arrow — its color may have
                        // flipped. The pressed button stays white until release.
                        let other_cc: u8 = if cc == 107 { 106 } else { 107 };
                        paint_ctrl_button(
                            &board.device,
                            other_cc,
                            ctrl_base_color(other_cc, new_shift),
                        );
                        return Ok(());
                    }
                    100 => {
                        // Cycle layout.
                    }
                    _ => return Ok(()),
                }
                // --- Settings pressed: cycle to next .xtn ---
                // Release notes via CC 123 all-notes-off on every output.
                for conn in outputs.values_mut() {
                    for ch in 0u8..16 {
                        let _ = conn.send(&[0xB0 | ch, 123, 0]);
                    }
                }
                let new_path = match layouts::next(
                    layouts::LayoutKind::Xtn,
                    &current_layout_path,
                ) {
                    Ok(p) => p,
                    Err(e) => {
                        eprintln!("layout cycle: {e:#}");
                        return Ok(());
                    }
                };
                let new_layout = match xtn::parse_xtn(&new_path) {
                    Ok(l) => l,
                    Err(e) => {
                        eprintln!("layout cycle: {e:#}");
                        return Ok(());
                    }
                };
                let new_edo = match new_layout.edo {
                    Some(e) => e,
                    None => {
                        eprintln!("layout cycle: {} has no Edo=", new_path.display());
                        return Ok(());
                    }
                };
                current_shifts.clear();
                current_layout = new_layout.clone();
                current_edo = new_edo;
                current_layout_path = new_path.clone();
                {
                    let mut d = display_for_cb.borrow_mut();
                    d.tuning_name = format!("edo{new_edo}");
                    d.shifts = boards_cyc.iter().map(|b| (b.device.number, 0)).collect();
                }
                // Rebuild per-board tuning states at shift=0.
                tunings.clear();
                for b in &boards_cyc {
                    if let Some(bl) = new_layout.boards.get(&b.board_name) {
                        tunings.insert(
                            b.device.number,
                            tuning::TuningState::from_board(
                                bl,
                                new_edo,
                                new_layout.pitch_offset,
                                2,
                                pb_range,
                            ),
                        );
                    }
                }
                // Repaint pads.
                for b in &boards_cyc {
                    let Some(bl) = new_layout.boards.get(&b.board_name) else {
                        continue;
                    };
                    let mut pads = [(0u8, Color::new(0, 0, 0)); 61];
                    for i in 0..61u8 {
                        let c = bl
                            .pads
                            .get(&i)
                            .map(|entry| entry.color)
                            .unwrap_or(Color::new(0, 0, 0))
                            .corrected(&correction_cyc);
                        pads[i as usize] = (i, c);
                    }
                    let _ = send_to_outputs(
                        &[b.device.clone()],
                        DeviceSelection::All,
                        &exquis_proto::enter_dev_mode(exquis_proto::DEV_MASK_NO_PADS),
                    );
                    let _ = send_to_outputs(
                        &[b.device.clone()],
                        DeviceSelection::All,
                        &exquis_proto::snapshot_set_pads(&pads),
                    );
                    let _ = send_to_outputs(
                        &[b.device.clone()],
                        DeviceSelection::All,
                        &exquis_proto::enter_dev_mode(exquis_proto::DEV_MASK_NO_PADS),
                    );
                    paint_ctrl_button(&b.device, 106, ctrl_base_color(106, 0));
                    paint_ctrl_button(&b.device, 107, ctrl_base_color(107, 0));
                    if b.device.number != device_number {
                        paint_ctrl_button(&b.device, 100, ctrl_base_color(100, 0));
                    }
                }
                let persist_path = new_path.clone();
                std::thread::spawn(move || {
                    settings::store_last_layout(layouts::LayoutKind::Xtn, &persist_path);
                });
                Ok(())
            },
        )?;
    }

    Ok(())
}

fn cmd_new(
    file: std::path::PathBuf,
    edo: i32,
    boards: u8,
    pitch_offset: i32,
    force: bool,
) -> Result<()> {
    if edo < 1 {
        bail!("edo must be >= 1");
    }
    if boards < 1 {
        bail!("boards must be >= 1");
    }
    if file.exists() && !force {
        bail!(
            "{} already exists; pass --force to overwrite",
            file.display()
        );
    }

    let mut board_map: std::collections::HashMap<String, xtn::BoardLayout> =
        std::collections::HashMap::new();
    for b in 0..boards {
        let mut pads = std::collections::HashMap::new();
        for i in 0..61u8 {
            pads.insert(
                i,
                xtn::PadEntry {
                    key: 0,
                    chan: 0,
                    color: Color::new(0, 0, 0),
                },
            );
        }
        board_map.insert(format!("board{b}"), xtn::BoardLayout { pads });
    }
    let layout = xtn::XtnLayout {
        edo: Some(edo),
        pitch_offset,
        boards: board_map,
    };

    if let Some(parent) = file.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }
    }
    let s = xtn::write_xtn_layout(&layout);
    std::fs::write(&file, s).with_context(|| format!("writing {}", file.display()))?;
    println!(
        "created {} (edo={edo}, pitch_offset={pitch_offset}, {boards} board{}, all pads 0/0/black)",
        file.display(),
        if boards == 1 { "" } else { "s" }
    );
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

fn cmd_load(
    file: std::path::PathBuf,
    correction: exquis_proto::ColorCorrection,
) -> Result<()> {
    let layout = xtn::parse_xtn(&file)?;
    let devices = list_devices()?;

    if devices.is_empty() {
        println!("No Exquis MIDI devices found.");
        return Ok(());
    }

    // Sync connected devices → board0..boardN (auto-creates/updates config)
    let boards = config::sync_boards(&devices)?;
    for board in &boards {
        println!(
            "  {} → [{}] {}",
            board.board_name, board.device.number, board.device.label
        );
    }

    if !correction.is_identity() {
        println!(
            "color correction: gamma={} saturation={} rgb_gain=({}, {}, {})",
            correction.gamma,
            correction.saturation,
            correction.r_gain,
            correction.g_gain,
            correction.b_gain
        );
    }

    let mut any_matched = false;

    for board in &boards {
        let board_layout = match layout.boards.get(&board.board_name) {
            Some(bl) => bl,
            None => continue, // .xtn doesn't have a section for this board
        };

        // Build snapshot: note = pad_id (always), color from .xtn
        let mut pads = [(0u8, Color::new(0, 0, 0)); 61];
        for i in 0..61u8 {
            let color = board_layout
                .pads
                .get(&i)
                .map(|entry| entry.color)
                .unwrap_or(Color::new(0, 0, 0))
                .corrected(&correction);
            pads[i as usize] = (i, color);
        }

        // Enter dev mode for non-pad zones
        send_to_outputs(
            &[board.device.clone()],
            DeviceSelection::All,
            &exquis_proto::enter_dev_mode(exquis_proto::DEV_MASK_NO_PADS),
        )?;

        // Send snapshot
        send_to_outputs(
            &[board.device.clone()],
            DeviceSelection::All,
            &exquis_proto::snapshot_set_pads(&pads),
        )?;

        println!(
            "[{}] loaded {} ({} pads mapped, MPE preserved)",
            board.device.number,
            board.board_name,
            board_layout.pads.len()
        );
        any_matched = true;
    }

    if !any_matched {
        println!(
            "No boards from {} matched connected devices.",
            file.display()
        );
    }

    Ok(())
}

fn cmd_control(control: String, color: Color, device: DeviceSelection) -> Result<()> {
    let id = exquis_proto::control_id_from_name(&control)
        .with_context(|| format!("unknown control `{control}`"))?;

    let devices = list_devices()?;
    let selected = midi::select_devices(&devices, &device)?;
    if selected.is_empty() {
        println!("No Exquis MIDI devices found.");
        return Ok(());
    }

    // Enter dev mode for non-pad zones (encoders, buttons, slider are covered by 0x3A)
    send_to_outputs(
        &selected,
        DeviceSelection::All,
        &exquis_proto::enter_dev_mode(exquis_proto::DEV_MASK_NO_PADS),
    )?;

    // Set LED color via SysEx cmd 04
    send_to_outputs(
        &selected,
        DeviceSelection::All,
        &exquis_proto::set_led_color(id, color),
    )?;

    let name = exquis_proto::control_display_name(id).unwrap_or_else(|| format!("#{id}"));
    for target in selected {
        println!("[{}] {} set to {}", target.number, name, color);
    }
    Ok(())
}

fn cmd_highlight(note: u8, velocity: u8, device: DeviceSelection) -> Result<()> {
    let devices = list_devices()?;
    let selected = midi::select_devices(&devices, &device)?;
    if selected.is_empty() {
        println!("No Exquis MIDI devices found.");
        return Ok(());
    }

    // Note On on channel 1 (MIDI channel 0): status 0x90
    let message = if velocity > 0 {
        vec![0x90, note, velocity]
    } else {
        // Note Off: status 0x80
        vec![0x80, note, 0]
    };

    send_to_outputs(&selected, DeviceSelection::All, &message)?;

    println!(
        "highlight note {} {}",
        note,
        if velocity > 0 { "on" } else { "off" }
    );
    Ok(())
}

fn cmd_pads_test(device: DeviceSelection, legacy: bool) -> Result<()> {
    let palette = [
        Color::named("red").unwrap(),
        Color::named("amber").unwrap(),
        Color::named("green").unwrap(),
        Color::named("blue").unwrap(),
        Color::named("white").unwrap(),
        Color::named("black").unwrap(),
    ];

    let devices = list_devices()?;
    let selected = midi::select_devices(&devices, &device)?;
    if selected.is_empty() {
        println!("No Exquis MIDI devices found.");
        return Ok(());
    }

    if legacy {
        // Legacy: take over pad zone (breaks MPE)
        send_to_outputs(
            &selected,
            DeviceSelection::All,
            &exquis_proto::enter_dev_mode(NamedZone::Pads.bit()),
        )?;
        for (pad, color) in (0u8..=60).zip(palette.into_iter().cycle()) {
            send_to_outputs(
                &selected,
                DeviceSelection::All,
                &exquis_proto::set_led_color(pad, color),
            )?;
        }
        for target in selected {
            println!(
                "[{}] wrote pad test pattern (legacy, MPE disabled)",
                target.number,
            );
        }
    } else {
        // Default: snapshot approach — preserves MPE
        let mut colors = [Color::new(0, 0, 0); 61];
        for (i, color) in palette.into_iter().cycle().enumerate().take(61) {
            colors[i] = color;
        }
        send_to_outputs(
            &selected,
            DeviceSelection::All,
            &exquis_proto::enter_dev_mode(exquis_proto::DEV_MASK_NO_PADS),
        )?;
        send_to_outputs(
            &selected,
            DeviceSelection::All,
            &exquis_proto::snapshot_set_colors(&colors),
        )?;
        for target in selected {
            println!(
                "[{}] wrote pad test pattern (MPE preserved)",
                target.number,
            );
        }
    }
    Ok(())
}

fn cmd_midi(args: cli::MidiArgs) -> Result<()> {
    let devices = list_devices()?;
    let selected = midi::select_devices(&devices, &args.device.unwrap_or(DeviceSelection::All))?;
    if selected.is_empty() {
        println!("No Exquis MIDI devices found.");
        return Ok(());
    }

    let log_path = if args.no_log {
        None
    } else {
        Some(args.log_file.unwrap_or_else(default_log_path))
    };
    let mut logger = log_path
        .map(JsonlLogger::open)
        .transpose()
        .context("failed to create log file")?;

    if let Some(logger) = logger.as_ref() {
        println!("Logging to `{}`", logger.path().display());
    }

    let (tx, rx) = mpsc::channel::<mpe::InputMessage>();
    let _connections = open_inputs(&selected, tx)?;

    if args.mode == cli::MidiMode::Hybrid && io::stdout().is_tty() {
        ui::run_hybrid(rx, &mut logger, args.log_raw, args.mpe_only)
    } else {
        run_stream(rx, &mut logger, args.mode, args.log_raw, args.mpe_only)
    }
}

fn run_stream(
    rx: mpsc::Receiver<mpe::InputMessage>,
    logger: &mut Option<JsonlLogger>,
    mode: cli::MidiMode,
    log_raw: bool,
    mpe_only: bool,
) -> Result<()> {
    let mut decoder = mpe::Decoder::default();
    while let Ok(message) = rx.recv() {
        let decoded = decoder.process(message);
        if let Some(logger) = logger.as_mut() {
            logger.write(&decoded, log_raw)?;
        }
        for line in display_lines(&decoded, mode, mpe_only) {
            println!("{line}");
        }
    }
    Ok(())
}

fn display_lines(decoded: &DecodedEvent, mode: cli::MidiMode, mpe_only: bool) -> Vec<String> {
    match mode {
        cli::MidiMode::Raw => vec![decoded.raw_line()],
        cli::MidiMode::Dashboard => Vec::new(),
        cli::MidiMode::Stream | cli::MidiMode::Hybrid => decoded.event_lines(mpe_only),
    }
}
