//! Terminal UI for `xentool serve` on the Wooting backend.
//!
//! Runs on its own thread. The serve hot loop pushes a `WootingSnapshot` onto
//! `snap_rx` at ~25 Hz and `LogLine`s onto `log_rx` only on state transitions
//! (layout cycle, aftertouch toggle, screensaver). The TUI never touches the
//! hot loop's state directly — everything it draws is read from the latest
//! snapshot.
//!
//! The snapshot/log structs are deliberately plain data so a future combined
//! Exquis+Wooting TUI can hold both backends' receivers and switch between
//! them without coupling either backend to ratatui.

use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::Result;
use crossbeam_channel::{Receiver, RecvTimeoutError};
use crossterm::event::{self, Event as CEvent, KeyCode};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Modifier, Style};
use ratatui::widgets::{Block, Borders, Paragraph, Row, Table};

use crate::wooting::modes::AftertouchMode;

/// One snapshot of the Wooting serve loop's currently-displayable state.
///
/// Built every ~40 ms inside the hot loop, sent over a `bounded(2)` channel
/// with `try_send` (drop on overflow). The receiver redraws on the most
/// recent snapshot.
#[derive(Debug, Clone, Default)]
pub struct WootingSnapshot {
    pub edo: i32,
    pub midi_port: String,
    pub layout_filename: String,
    pub aftertouch_mode_name: &'static str,
    pub velocity_profile_name: String,
    pub manual_press_threshold: f32,
    pub aftertouch_speed_max: f32,
    pub screensaver_active: bool,
    pub octave_holds: Vec<DeviceLine>,
    pub held_keys: Vec<HeldKeyDisplay>,
    /// Total devices currently enumerated.
    pub device_count: u8,
}

#[derive(Debug, Clone)]
pub struct DeviceLine {
    pub wtn_board: u8,
    pub octave_hold: bool,
}

#[derive(Debug, Clone)]
pub struct HeldKeyDisplay {
    pub wtn_board: u8,
    pub channel: u8,
    pub note: u8,
    /// Most recent MIDI poly-pressure value sent (0..=127). 0 if none yet.
    pub pressure: u8,
    /// Milliseconds since the note went into Held state.
    pub age_ms: u32,
}

/// Run the Wooting serve TUI on the calling thread.
///
/// Returns when:
/// - both senders are dropped (hot loop shutting down), or
/// - the user presses `q` (which also flips `shutdown` so the hot loop
///   notices on its next iteration).
pub fn run_wooting_serve_ui(
    snap_rx: Receiver<WootingSnapshot>,
    log_rx: Receiver<String>,
    shutdown: Arc<AtomicBool>,
) -> Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut last_snapshot: WootingSnapshot = WootingSnapshot::default();
    let mut log_lines: Vec<String> = Vec::with_capacity(256);
    const LOG_CAP: usize = 256;

    let result = loop {
        // Pull all currently-pending snapshots; keep the newest. Pull pending
        // log lines as well. Both calls are non-blocking.
        let mut got_anything = false;
        while let Ok(s) = snap_rx.try_recv() {
            last_snapshot = s;
            got_anything = true;
        }
        while let Ok(line) = log_rx.try_recv() {
            log_lines.push(line);
            if log_lines.len() > LOG_CAP {
                let drop_n = log_lines.len() - LOG_CAP;
                log_lines.drain(0..drop_n);
            }
            got_anything = true;
        }

        // If nothing was pending, sleep a little so we don't spin. Use a
        // blocking wait on either channel to wake immediately when data
        // arrives. The deadline gives us a redraw at least every 40 ms even
        // when idle.
        if !got_anything {
            match snap_rx.recv_timeout(Duration::from_millis(40)) {
                Ok(s) => last_snapshot = s,
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => break Ok(()),
            }
        }

        draw(&mut terminal, &last_snapshot, &log_lines)?;

        // Keyboard: q quits.
        if event::poll(Duration::from_millis(1))? {
            if let CEvent::Key(key) = event::read()? {
                if key.code == KeyCode::Char('q') {
                    shutdown.store(true, Ordering::Relaxed);
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

fn draw(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    snap: &WootingSnapshot,
    log_lines: &[String],
) -> Result<()> {
    terminal.draw(|frame| {
        let vertical = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(9), Constraint::Min(8)])
            .split(frame.area());
        let top = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
            .split(vertical[0]);

        let rows = snap.held_keys.iter().map(|k| {
            Row::new(vec![
                format!("{}", k.wtn_board),
                format!("{}", k.channel),
                format!("{}", k.note),
                format!("{}", k.pressure),
                format!("{}", k.age_ms),
            ])
        });
        let touches_table = Table::new(
            rows,
            [
                Constraint::Length(4),  // Board
                Constraint::Length(3),  // Ch
                Constraint::Length(4),  // Note
                Constraint::Length(5),  // Pressure
                Constraint::Length(6),  // Age (ms)
            ],
        )
        .header(
            Row::new(["Brd", "Ch", "Note", "Press", "Age"])
                .style(Style::default().add_modifier(Modifier::BOLD)),
        )
        .block(Block::default().title("Held Notes").borders(Borders::ALL));

        let controls_widget = Paragraph::new(controls_text(snap))
            .block(Block::default().title("Controls").borders(Borders::ALL));

        let log_text = log_lines
            .iter()
            .rev()
            .take(vertical[1].height.saturating_sub(2) as usize)
            .rev()
            .cloned()
            .collect::<Vec<_>>()
            .join("\n");
        let title = format!(
            "Events (q to quit) — {}-EDO | MIDI → {} | MTS-ESP",
            snap.edo, snap.midi_port,
        );
        let events_widget = Paragraph::new(log_text)
            .block(Block::default().title(title).borders(Borders::ALL));

        frame.render_widget(touches_table, top[0]);
        frame.render_widget(controls_widget, top[1]);
        frame.render_widget(events_widget, vertical[1]);
    })?;
    Ok(())
}

fn controls_text(snap: &WootingSnapshot) -> String {
    let mut lines: Vec<String> = Vec::new();
    lines.push(format!("Layout: {}", snap.layout_filename));
    lines.push(format!("Tuning: {}-EDO", snap.edo));
    lines.push(format!("Aftertouch: {}", snap.aftertouch_mode_name));
    lines.push(format!("Velocity: {}", snap.velocity_profile_name));
    lines.push(format!(
        "Press threshold: {:.2}  |  AT speed max: {:.0}",
        snap.manual_press_threshold, snap.aftertouch_speed_max
    ));
    if snap.screensaver_active {
        lines.push("Screensaver: ON".to_string());
    }
    lines.push(format!("Devices: {}", snap.device_count));
    for d in &snap.octave_holds {
        let tag = if d.octave_hold { " [hold]" } else { "" };
        lines.push(format!("  board{}{}", d.wtn_board, tag));
    }
    lines.join("\n")
}

/// Cadence at which the hot loop should publish snapshots. Matches the
/// Exquis serve TUI's `recv_timeout(40 ms)` redraw cadence.
pub const SNAPSHOT_INTERVAL: Duration = Duration::from_millis(40);

/// Helper used by the hot loop: returns true when it's time to publish the
/// next snapshot, and bumps `next_at` in place.
pub fn snapshot_due(now: Instant, next_at: &mut Instant) -> bool {
    if now >= *next_at {
        *next_at = now + SNAPSHOT_INTERVAL;
        true
    } else {
        false
    }
}
