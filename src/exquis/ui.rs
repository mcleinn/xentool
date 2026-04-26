use anyhow::Result;
use crossterm::event::{self, Event as CEvent, KeyCode};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Modifier, Style};
use ratatui::widgets::{Block, Borders, Cell, Paragraph, Row, Table};
use std::cell::RefCell;
use std::collections::HashMap;
use std::io;
use std::rc::Rc;
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::Duration;

use crate::exquis::mpe::{ControlStateTracker, Decoder, EventBuffer, InputMessage, TouchSummary};
use crate::exquis::proto::control_display_name;
use crate::exquis::tuning::TuningState;
use crate::logging::JsonlLogger;
use crate::mts::MtsMaster;

/// Shared state displayed in the serve TUI's Controls panel.
///
/// Mutated by the cycle/octave closures in `main.rs`, read by the UI draw
/// callbacks. Single-threaded (closures run inside the UI loop), so
/// `Rc<RefCell<_>>` is enough.
#[derive(Debug, Clone, Default)]
pub struct ServeDisplay {
    /// Short tuning label, e.g. `"edo31"`.
    pub tuning_name: String,
    /// Per-board octave shift keyed by device number.
    pub shifts: HashMap<usize, i32>,
}

pub type DisplayHandle = Rc<RefCell<ServeDisplay>>;

pub fn run_hybrid(
    rx: Receiver<InputMessage>,
    logger: &mut Option<JsonlLogger>,
    log_raw: bool,
    mpe_only: bool,
) -> Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut decoder = Decoder::default();
    let mut controls = ControlStateTracker::default();
    let mut active_touches = Vec::<TouchSummary>::new();
    let mut events = EventBuffer::default();

    let result = loop {
        match rx.recv_timeout(Duration::from_millis(40)) {
            Ok(message) => {
                controls.apply(&message.bytes);
                let decoded = decoder.process(message);
                if let Some(logger) = logger.as_mut() {
                    logger.write(&decoded, log_raw)?;
                }
                active_touches = decoded.touches.clone();
                for line in decoded.event_lines(mpe_only) {
                    events.push(line);
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break Ok(()),
        }

        terminal.draw(|frame| {
            let vertical = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Length(9), Constraint::Min(8)])
                .split(frame.area());
            let top = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
                .split(vertical[0]);

            let rows = active_touches.iter().map(render_touch_row);
            let touches_table = Table::new(
                rows,
                [
                    Constraint::Length(4),  // Dev
                    Constraint::Length(3),  // Ch
                    Constraint::Length(4),  // Note
                    Constraint::Length(4),  // VCh
                    Constraint::Length(5),  // VNote
                    Constraint::Length(4),  // Abs
                    Constraint::Length(8),  // Freq
                    Constraint::Length(3),  // Vel
                    Constraint::Length(6),  // X
                    Constraint::Length(3),  // Y
                    Constraint::Length(3),  // Z
                    Constraint::Length(5),  // Age
                ],
            )
            .header(
                Row::new(["Dev", "Ch", "Note", "VCh", "VNote", "Abs", "Freq", "Vel", "X", "Y", "Z", "Age"])
                    .style(Style::default().add_modifier(Modifier::BOLD)),
            )
            .block(
                Block::default()
                    .title("Active Touches")
                    .borders(Borders::ALL),
            );

            let controls_widget = Paragraph::new(controls_text(&controls, None))
                .block(Block::default().title("Controls").borders(Borders::ALL));

            let log_text = events
                .entries()
                .iter()
                .rev()
                .take(vertical[1].height.saturating_sub(2) as usize)
                .rev()
                .cloned()
                .collect::<Vec<_>>()
                .join("\n");
            let events_widget = Paragraph::new(log_text).block(
                Block::default()
                    .title("Events (q to quit)")
                    .borders(Borders::ALL),
            );

            frame.render_widget(touches_table, top[0]);
            frame.render_widget(controls_widget, top[1]);
            frame.render_widget(events_widget, vertical[1]);
        })?;

        if event::poll(Duration::from_millis(1))? {
            if let CEvent::Key(key) = event::read()? {
                if key.code == KeyCode::Char('q') {
                    break Ok(());
                }
            }
        }
    };

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    result
}

pub fn run_serve_ui(
    rx: Receiver<InputMessage>,
    master: &MtsMaster,
    scale_name: &str,
    display: DisplayHandle,
    on_control_edge: &mut dyn FnMut(usize, u8, bool) -> Result<()>,
) -> Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut decoder = Decoder::default();
    let mut controls = ControlStateTracker::default();
    let mut active_touches = Vec::<TouchSummary>::new();
    let mut events = EventBuffer::default();
    let mut prev_control_state: std::collections::HashMap<(usize, u8), bool> =
        std::collections::HashMap::new();

    let result = loop {
        match rx.recv_timeout(Duration::from_millis(40)) {
            Ok(message) => {
                controls.apply(&message.bytes);
                // Per-device edge detection for Settings/Up/Down (CC 100/107/106 on channel 16).
                if message.bytes.len() == 3
                    && (message.bytes[0] & 0xF0) == 0xB0
                    && (message.bytes[0] & 0x0F) == 0x0F
                {
                    let cc = message.bytes[1];
                    if matches!(cc, 100 | 106 | 107) {
                        let pressed = message.bytes[2] != 0;
                        let key = (message.device_number, cc);
                        let prev = prev_control_state.get(&key).copied().unwrap_or(false);
                        if prev != pressed {
                            prev_control_state.insert(key, pressed);
                            let _ = on_control_edge(message.device_number, cc, pressed);
                        }
                    }
                }
                let decoded = decoder.process(message);
                active_touches = decoded.touches.clone();
                for line in decoded.event_lines(false) {
                    events.push(line);
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break Ok(()),
        }

        let clients = master.get_num_clients();
        let events_title = format!(
            "Events (q to quit) - MTS-ESP: {} | {} client(s)",
            scale_name, clients
        );

        terminal.draw(|frame| {
            let vertical = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Length(9), Constraint::Min(8)])
                .split(frame.area());
            let top = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
                .split(vertical[0]);

            let rows = active_touches.iter().map(render_touch_row);
            let touches_table = Table::new(
                rows,
                [
                    Constraint::Length(4),  // Dev
                    Constraint::Length(3),  // Ch
                    Constraint::Length(4),  // Note
                    Constraint::Length(4),  // VCh
                    Constraint::Length(5),  // VNote
                    Constraint::Length(4),  // Abs
                    Constraint::Length(8),  // Freq
                    Constraint::Length(3),  // Vel
                    Constraint::Length(6),  // X
                    Constraint::Length(3),  // Y
                    Constraint::Length(3),  // Z
                    Constraint::Length(5),  // Age
                ],
            )
            .header(
                Row::new(["Dev", "Ch", "Note", "VCh", "VNote", "Abs", "Freq", "Vel", "X", "Y", "Z", "Age"])
                    .style(Style::default().add_modifier(Modifier::BOLD)),
            )
            .block(
                Block::default()
                    .title("Active Touches")
                    .borders(Borders::ALL),
            );

            let disp = display.borrow();
            let controls_widget = Paragraph::new(controls_text(&controls, Some(&*disp)))
                .block(Block::default().title("Controls").borders(Borders::ALL));
            drop(disp);

            let log_text = events
                .entries()
                .iter()
                .rev()
                .take(vertical[1].height.saturating_sub(2) as usize)
                .rev()
                .cloned()
                .collect::<Vec<_>>()
                .join("\n");
            let events_widget = Paragraph::new(log_text).block(
                Block::default()
                    .title(events_title.clone())
                    .borders(Borders::ALL),
            );

            frame.render_widget(touches_table, top[0]);
            frame.render_widget(controls_widget, top[1]);
            frame.render_widget(events_widget, vertical[1]);
        })?;

        if event::poll(Duration::from_millis(1))? {
            if let CEvent::Key(key) = event::read()? {
                if key.code == KeyCode::Char('q') {
                    break Ok(());
                }
            }
        }
    };

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    result
}

pub type RetuneCycleCallback<'a> = &'a mut dyn FnMut(
    usize,
    u8,
    bool,
    &mut std::collections::HashMap<usize, TuningState>,
    &mut std::collections::HashMap<usize, midir::MidiOutputConnection>,
) -> Result<()>;

pub fn run_serve_retune_ui(
    rx: Receiver<InputMessage>,
    scale_name: &str,
    tunings: &mut std::collections::HashMap<usize, TuningState>,
    outputs: &mut std::collections::HashMap<usize, midir::MidiOutputConnection>,
    display: DisplayHandle,
    on_control_edge: RetuneCycleCallback,
) -> Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut decoder = Decoder::default();
    let mut controls = ControlStateTracker::default();
    let mut active_touches = Vec::<TouchSummary>::new();
    let mut events = EventBuffer::default();
    let mut notes_retuned: u64 = 0;
    let mut prev_control_state: std::collections::HashMap<(usize, u8), bool> =
        std::collections::HashMap::new();

    let result = loop {
        match rx.recv_timeout(Duration::from_millis(40)) {
            Ok(message) => {
                controls.apply(&message.bytes);
                // Per-device edge detection for Settings/Up/Down.
                if message.bytes.len() == 3
                    && (message.bytes[0] & 0xF0) == 0xB0
                    && (message.bytes[0] & 0x0F) == 0x0F
                {
                    let cc = message.bytes[1];
                    if matches!(cc, 100 | 106 | 107) {
                        let pressed = message.bytes[2] != 0;
                        let key = (message.device_number, cc);
                        let prev = prev_control_state.get(&key).copied().unwrap_or(false);
                        if prev != pressed {
                            prev_control_state.insert(key, pressed);
                            let _ = on_control_edge(
                                message.device_number, cc, pressed, tunings, outputs,
                            );
                        }
                    }
                }
                if let Some(tuning) = tunings.get_mut(&message.device_number) {
                    let out_msgs = tuning.process_message(&message.bytes);
                    if let Some(conn) = outputs.get_mut(&message.device_number) {
                        for msg in &out_msgs {
                            let _ = conn.send(msg);
                        }
                    }
                    if let Some(info) = tuning.last_retune_info.take() {
                        events.push(format!("  retune: {info}"));
                        notes_retuned += 1;
                    }
                }

                let decoded = decoder.process(message);
                active_touches = decoded.touches.clone();

                // Annotate touches with tuning info (VCh, VNote, AbsPitch, Freq)
                for touch in &mut active_touches {
                    if let Some(tuning) = tunings.get(&touch.device) {
                        if let Some(pad) = tuning.channel_pad(touch.channel.saturating_sub(1)) {
                            if let Some(pt) = tuning.pad_tuning(pad) {
                                touch.freq_hz = Some(pt.freq_hz);
                                touch.v_chan = Some(pt.v_chan);
                                touch.v_key = Some(pt.v_key);
                                touch.abs_pitch = Some(pt.abs_pitch);
                            }
                        }
                    }
                }

                for line in decoded.event_lines(false) {
                    events.push(line);
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break Ok(()),
        }

        let events_title = format!(
            "Events (q to quit) - {} | pitch bend retuning | {} notes",
            scale_name, notes_retuned
        );

        terminal.draw(|frame| {
            let vertical = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Length(9), Constraint::Min(8)])
                .split(frame.area());
            let top = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
                .split(vertical[0]);

            let rows = active_touches.iter().map(render_touch_row);
            let touches_table = Table::new(
                rows,
                [
                    Constraint::Length(4),  // Dev
                    Constraint::Length(3),  // Ch
                    Constraint::Length(4),  // Note
                    Constraint::Length(4),  // VCh
                    Constraint::Length(5),  // VNote
                    Constraint::Length(4),  // Abs
                    Constraint::Length(8),  // Freq
                    Constraint::Length(3),  // Vel
                    Constraint::Length(6),  // X
                    Constraint::Length(3),  // Y
                    Constraint::Length(3),  // Z
                    Constraint::Length(5),  // Age
                ],
            )
            .header(
                Row::new(["Dev", "Ch", "Note", "VCh", "VNote", "Abs", "Freq", "Vel", "X", "Y", "Z", "Age"])
                    .style(Style::default().add_modifier(Modifier::BOLD)),
            )
            .block(
                Block::default()
                    .title("Active Touches")
                    .borders(Borders::ALL),
            );

            let disp = display.borrow();
            let controls_widget = Paragraph::new(controls_text(&controls, Some(&*disp)))
                .block(Block::default().title("Controls").borders(Borders::ALL));
            drop(disp);

            let log_text = events
                .entries()
                .iter()
                .rev()
                .take(vertical[1].height.saturating_sub(2) as usize)
                .rev()
                .cloned()
                .collect::<Vec<_>>()
                .join("\n");
            let events_widget = Paragraph::new(log_text).block(
                Block::default()
                    .title(events_title.clone())
                    .borders(Borders::ALL),
            );

            frame.render_widget(touches_table, top[0]);
            frame.render_widget(controls_widget, top[1]);
            frame.render_widget(events_widget, vertical[1]);
        })?;

        if event::poll(Duration::from_millis(1))? {
            if let CEvent::Key(key) = event::read()? {
                if key.code == KeyCode::Char('q') {
                    break Ok(());
                }
            }
        }
    };

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    result
}

fn controls_text(controls: &ControlStateTracker, display: Option<&ServeDisplay>) -> String {
    let mut lines: Vec<String> = Vec::new();
    if let Some(d) = display {
        if !d.tuning_name.is_empty() {
            lines.push(format!("Tuning: {}", d.tuning_name));
        }
        // Only show boards whose shift is non-zero — minimises noise. Display
        // still includes board0=0 if explicitly tracked.
        let mut entries: Vec<(usize, i32)> =
            d.shifts.iter().map(|(k, v)| (*k, *v)).collect();
        entries.sort_by_key(|(k, _)| *k);
        let shift_parts: Vec<String> = entries
            .into_iter()
            .map(|(dev, shift)| format!("dev{dev}={shift:+}"))
            .collect();
        lines.push(if shift_parts.is_empty() {
            "Octave: -".to_string()
        } else {
            format!("Octave: {}", shift_parts.join(" "))
        });
    }

    let enc_parts: Vec<String> = (110..=113u8)
        .map(|id| {
            let val = controls.encoders.get(&id).copied().unwrap_or(0);
            format!("E{}:{:+}", id - 109, val)
        })
        .collect();
    lines.push(format!("Enc: {}", enc_parts.join(" ")));

    lines.push(match (controls.slider_portion, controls.slider_position) {
        (Some(p), Some(pos)) => format!("Sld: part{} @{}", p + 1, pos),
        (None, Some(pos)) => format!("Sld: @{}", pos),
        (Some(p), None) => format!("Sld: part{}", p + 1),
        (None, None) => "Sld: -".to_string(),
    });

    let mut pressed: Vec<String> = controls
        .buttons
        .iter()
        .filter(|(_, p)| **p)
        .map(|(id, _)| control_display_name(*id).unwrap_or_else(|| format!("#{id}")))
        .collect();
    pressed.sort();
    lines.push(if pressed.is_empty() {
        "Btn: -".to_string()
    } else {
        format!("Btn: {}", pressed.join(", "))
    });

    lines.join("\n")
}

fn render_touch_row(touch: &TouchSummary) -> Row<'static> {
    let freq_str = match touch.freq_hz {
        Some(f) if f >= 1000.0 => format!("{:.1}k", f / 1000.0),
        Some(f) => format!("{:.2}", f),
        None => "-".to_string(),
    };
    let vch_str = touch.v_chan.map(|v| v.to_string()).unwrap_or_else(|| "-".to_string());
    let vnote_str = touch.v_key.map(|v| v.to_string()).unwrap_or_else(|| "-".to_string());
    let abs_str = touch.abs_pitch.map(|v| v.to_string()).unwrap_or_else(|| "-".to_string());
    Row::new(vec![
        Cell::from(format!("[{}]", touch.device)),
        Cell::from(format!("#{}", touch.channel)),
        Cell::from(touch.note.to_string()),
        Cell::from(vch_str),
        Cell::from(vnote_str),
        Cell::from(abs_str),
        Cell::from(freq_str),
        Cell::from(touch.velocity.to_string()),
        Cell::from(format!("{:+}", touch.x)),
        Cell::from(touch.y.to_string()),
        Cell::from(touch.z.to_string()),
        Cell::from(format!("{:.2}s", touch.age.as_secs_f32())),
    ])
}
