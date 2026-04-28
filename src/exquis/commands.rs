//! CLI command handlers for the Exquis backend.
//!
//! Each `pub fn cmd_*` is invoked from `main.rs`'s dispatcher; the contents
//! were lifted verbatim from `main.rs` during the structural refactor that
//! moved Exquis-specific code into `src/exquis/`. Any `exquis::*` paths
//! resolve through the local `use crate::exquis;` import below.

use anyhow::{Context, Result, bail};
use crossterm::tty::IsTty;
use std::io;
use std::sync::mpsc;

use crate::cli::{self, DevAction, PadsCommands};
use crate::config;
use crate::exquis;
use crate::exquis::midi::{DeviceSelection, list_devices, open_inputs, send_to_outputs};
use crate::exquis::mpe::DecodedEvent;
use crate::exquis::proto::{Color, NamedZone};
use crate::geometry;
use crate::layouts;
use crate::logging::{JsonlLogger, default_log_path};
use crate::mts;
use crate::settings;
use crate::xtn;

pub fn cmd_dev(action: DevAction, zones: Vec<NamedZone>) -> Result<()> {
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
        DevAction::On => exquis::proto::enter_dev_mode(mask),
        DevAction::Off => exquis::proto::exit_dev_mode(),
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

pub fn cmd_pads(args: cli::PadsArgs) -> Result<()> {
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

pub fn cmd_pads_fill(color: Color, device: DeviceSelection, legacy: bool) -> Result<()> {
    let devices = list_devices()?;
    let selected = exquis::midi::select_devices(&devices, &device)?;
    if selected.is_empty() {
        println!("No Exquis MIDI devices found.");
        return Ok(());
    }

    if legacy {
        // Legacy: take over pad zone (breaks MPE)
        let messages = vec![
            exquis::proto::enter_dev_mode(NamedZone::Pads.bit()),
            exquis::proto::fill_all_pads(color),
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
            &exquis::proto::enter_dev_mode(exquis::proto::DEV_MASK_NO_PADS),
        )?;
        send_to_outputs(
            &selected,
            DeviceSelection::All,
            &exquis::proto::snapshot_fill_color(color),
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

pub fn cmd_pad_set(pad: u8, color: Color, device: DeviceSelection, legacy: bool) -> Result<()> {
    if pad > 60 {
        bail!("pad index must be in 0..=60");
    }

    let devices = list_devices()?;
    let selected = exquis::midi::select_devices(&devices, &device)?;
    if selected.is_empty() {
        println!("No Exquis MIDI devices found.");
        return Ok(());
    }

    if legacy {
        // Legacy: take over pad zone (breaks MPE)
        let messages = vec![
            exquis::proto::enter_dev_mode(NamedZone::Pads.bit()),
            exquis::proto::set_led_color(pad, color),
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
            &exquis::proto::enter_dev_mode(exquis::proto::DEV_MASK_NO_PADS),
        )?;
        send_to_outputs(
            &selected,
            DeviceSelection::All,
            &exquis::proto::snapshot_set_colors(&colors),
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
fn ctrl_base_color(cc: u8, octave_shift: i32) -> exquis::proto::Color {
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

fn pressed_color() -> exquis::proto::Color {
    Color::new(CTRL_PRESSED_RGB.0, CTRL_PRESSED_RGB.1, CTRL_PRESSED_RGB.2)
}

/// Send a single `set_led_color`. Assumes dev mode is already active on the
/// device; avoid redundant `enter_dev_mode` calls in hot paths.
fn paint_ctrl_button(
    device: &crate::exquis::midi::ExquisDevice,
    cc: u8,
    color: exquis::proto::Color,
) {
    let _ = send_to_outputs(
        &[device.clone()],
        DeviceSelection::All,
        &exquis::proto::set_led_color(cc, color),
    );
}

/// Re-enter dev mode for the non-pad zones and paint all three control
/// buttons. Used at startup and after layout cycles.
fn paint_all_ctrl_buttons_for_board(
    device: &crate::exquis::midi::ExquisDevice,
    octave_shift: i32,
) {
    let _ = send_to_outputs(
        &[device.clone()],
        DeviceSelection::All,
        &exquis::proto::enter_dev_mode(exquis::proto::DEV_MASK_NO_PADS),
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

pub fn cmd_serve(
    file: std::path::PathBuf,
    pb_range: f64,
    output: String,
    mts_esp: bool,
    correction: exquis::proto::ColorCorrection,
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
            &exquis::proto::enter_dev_mode(exquis::proto::DEV_MASK_NO_PADS),
        )?;
        send_to_outputs(
            &[board.device.clone()],
            DeviceSelection::All,
            &exquis::proto::snapshot_set_pads(&pads),
        )?;

        println!("[{}] {} colors set (MPE preserved)", board.device.number, board.board_name);
    }

    // Build per-board tuning states
    let mut board_tunings: std::collections::HashMap<usize, exquis::tuning::TuningState> =
        std::collections::HashMap::new();
    for board in &boards {
        if let Some(bl) = layout.boards.get(&board.board_name) {
            let state =
                exquis::tuning::TuningState::from_board(bl, edo, layout.pitch_offset, 2, pb_range);
            board_tunings.insert(board.device.number, state);
        }
    }

    // Open MIDI inputs
    let (tx, rx) = mpsc::channel::<exquis::mpe::InputMessage>();
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
        let display = std::rc::Rc::new(std::cell::RefCell::new(exquis::ui::ServeDisplay {
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
        exquis::ui::run_serve_ui(rx, &master, &scale_name, display, &mut |device_number, cc, pressed| {
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
                    &exquis::proto::enter_dev_mode(exquis::proto::DEV_MASK_NO_PADS),
                );
                let _ = send_to_outputs(
                    &[b.device.clone()],
                    DeviceSelection::All,
                    &exquis::proto::snapshot_set_pads(&pads),
                );
                // After the snapshot we may have lost dev mode — re-enter and
                // restore Up/Down for this board to the new (shift=0) base.
                // Leave Settings alone on the pressed board: it stays white
                // while the user holds the key; the release branch restores
                // its base color.
                let _ = send_to_outputs(
                    &[b.device.clone()],
                    DeviceSelection::All,
                    &exquis::proto::enter_dev_mode(exquis::proto::DEV_MASK_NO_PADS),
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
        let display = std::rc::Rc::new(std::cell::RefCell::new(exquis::ui::ServeDisplay {
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
        exquis::ui::run_serve_retune_ui(
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
                            exquis::tuning::TuningState::from_board(
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
                            exquis::tuning::TuningState::from_board(
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
                        &exquis::proto::enter_dev_mode(exquis::proto::DEV_MASK_NO_PADS),
                    );
                    let _ = send_to_outputs(
                        &[b.device.clone()],
                        DeviceSelection::All,
                        &exquis::proto::snapshot_set_pads(&pads),
                    );
                    let _ = send_to_outputs(
                        &[b.device.clone()],
                        DeviceSelection::All,
                        &exquis::proto::enter_dev_mode(exquis::proto::DEV_MASK_NO_PADS),
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

pub fn cmd_new(
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

pub fn cmd_load(
    file: std::path::PathBuf,
    correction: exquis::proto::ColorCorrection,
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
            &exquis::proto::enter_dev_mode(exquis::proto::DEV_MASK_NO_PADS),
        )?;

        // Send snapshot
        send_to_outputs(
            &[board.device.clone()],
            DeviceSelection::All,
            &exquis::proto::snapshot_set_pads(&pads),
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

pub fn cmd_control(control: String, color: Color, device: DeviceSelection) -> Result<()> {
    let id = exquis::proto::control_id_from_name(&control)
        .with_context(|| format!("unknown control `{control}`"))?;

    let devices = list_devices()?;
    let selected = exquis::midi::select_devices(&devices, &device)?;
    if selected.is_empty() {
        println!("No Exquis MIDI devices found.");
        return Ok(());
    }

    // Enter dev mode for non-pad zones (encoders, buttons, slider are covered by 0x3A)
    send_to_outputs(
        &selected,
        DeviceSelection::All,
        &exquis::proto::enter_dev_mode(exquis::proto::DEV_MASK_NO_PADS),
    )?;

    // Set LED color via SysEx cmd 04
    send_to_outputs(
        &selected,
        DeviceSelection::All,
        &exquis::proto::set_led_color(id, color),
    )?;

    let name = exquis::proto::control_display_name(id).unwrap_or_else(|| format!("#{id}"));
    for target in selected {
        println!("[{}] {} set to {}", target.number, name, color);
    }
    Ok(())
}
pub fn cmd_highlight(note: u8, velocity: u8, device: DeviceSelection) -> Result<()> {
    let devices = list_devices()?;
    let selected = exquis::midi::select_devices(&devices, &device)?;
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
    let selected = exquis::midi::select_devices(&devices, &device)?;
    if selected.is_empty() {
        println!("No Exquis MIDI devices found.");
        return Ok(());
    }

    if legacy {
        // Legacy: take over pad zone (breaks MPE)
        send_to_outputs(
            &selected,
            DeviceSelection::All,
            &exquis::proto::enter_dev_mode(NamedZone::Pads.bit()),
        )?;
        for (pad, color) in (0u8..=60).zip(palette.into_iter().cycle()) {
            send_to_outputs(
                &selected,
                DeviceSelection::All,
                &exquis::proto::set_led_color(pad, color),
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
            &exquis::proto::enter_dev_mode(exquis::proto::DEV_MASK_NO_PADS),
        )?;
        send_to_outputs(
            &selected,
            DeviceSelection::All,
            &exquis::proto::snapshot_set_colors(&colors),
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
pub fn cmd_midi(args: cli::MidiArgs) -> Result<()> {
    let devices = list_devices()?;
    let selected = exquis::midi::select_devices(&devices, &args.device.unwrap_or(DeviceSelection::All))?;
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

    let (tx, rx) = mpsc::channel::<exquis::mpe::InputMessage>();
    let _connections = open_inputs(&selected, tx)?;

    if args.mode == cli::MidiMode::Hybrid && io::stdout().is_tty() {
        exquis::ui::run_hybrid(rx, &mut logger, args.log_raw, args.mpe_only, selected)
    } else {
        run_stream(rx, &mut logger, args.mode, args.log_raw, args.mpe_only)
    }
}

fn run_stream(
    rx: mpsc::Receiver<exquis::mpe::InputMessage>,
    logger: &mut Option<JsonlLogger>,
    mode: cli::MidiMode,
    log_raw: bool,
    mpe_only: bool,
) -> Result<()> {
    let mut decoder = exquis::mpe::Decoder::default();
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
