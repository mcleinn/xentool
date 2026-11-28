//! `xentool boards` — check and change the order of a keyboard's boards.
//!
//! A keyboard made of several identical modules (openxen: five USB modules of 56 keys) has
//! to know which module lies where. The command lights every board in the colour of its
//! position (`BOARD_COLOURS`, left to right), asks whether that is what the player sees, and
//! if not takes the colours as seen from left to right and works out the new pinning
//! (serial number → board) for the settings file.
//!
//! Two ways to reach the keyboard, behind one trait (`Keyboard`):
//! - a `xentool serve --api` is running: through its local API (`ApiKeyboard`; lights are
//!   an LED overlay, a new order is applied by the serve loop at once),
//! - nothing is serving: the openxen modules are opened directly (`DirectKeyboard`).
//!
//! The dialogue (`run`) is a small state machine over `Keyboard`, `Prompt` and `PinStore`,
//! so it is tested with scripted answers and without hardware. Whatever way it ends, the
//! identification colours do not stay: `Lights` restores the lighting exactly once on every
//! exit path (layout colours when serving, LEDs off otherwise).
//!
//! A keyboard with a fixed board order (Wooting, Exquis) is listed and lit the same way;
//! only reordering is refused.

use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use crossbeam_channel::{Receiver, RecvTimeoutError};

use crate::api::Rgb;
use crate::cli::{BoardsArgs, BoardsCommand};
use crate::settings::OpenxenBoardSettings;

/// Identification colour of each position, left to right. The one definition: the lights,
/// the listing and the input of `boards order` all read it. A keyboard with more boards
/// starts again with the first colour (its boards are then named by position).
pub const BOARD_COLOURS: [(&str, Rgb); 5] = [
    ("red", (255, 0, 0)),
    ("orange", (255, 90, 0)),
    ("green", (0, 255, 0)),
    ("blue", (0, 0, 255)),
    ("white", (255, 255, 255)),
];

pub fn colour_of(board: u8) -> (&'static str, Rgb) {
    BOARD_COLOURS[board as usize % BOARD_COLOURS.len()]
}

fn colour_names() -> String {
    BOARD_COLOURS.iter().map(|c| c.0).collect::<Vec<_>>().join(", ")
}

/// "2 orange (board1)": a board as the user is told about it.
fn board_label(board: u8) -> String {
    format!("{} {:<6} (board{board})", board as u32 + 1, colour_of(board).0)
}

// --- what is connected ---

/// One connected board. `serial`/`pinned` are None where the keyboard does not tell
/// (a backend with a fixed order, or a serve that predates this command).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub board: u8,
    pub serial: Option<String>,
    pub pinned: Option<bool>,
}

fn listing(rows: &[Row]) -> String {
    let mut out = String::from("  pos  colour  board   serial                    position is\n");
    for r in rows {
        out.push_str(&format!(
            "  {:<3}  {:<6}  {:<6}  {:<24}  {}\n",
            r.board as u32 + 1,
            colour_of(r.board).0,
            format!("board{}", r.board),
            r.serial.as_deref().unwrap_or("-"),
            match r.pinned {
                Some(true) => "pinned in the settings",
                Some(false) => "assigned automatically",
                None => "-",
            }
        ));
    }
    out
}

// --- the order as seen -> new pinning (pure) ---

/// Split what the user typed ("orange red, green" or "2 1 3") into its words.
pub fn split_tokens(line: &str) -> Vec<String> {
    line.split(|c: char| c.is_whitespace() || c == ',' || c == ';').filter(|t| !t.is_empty()).map(|t| t.to_string()).collect()
}

/// The boards the user sees from left to right, from colour names (or their beginnings)
/// and 1-based positions. `boards`: the boards that are connected (and lit). Every one of
/// them has to be named exactly once.
pub fn parse_seen(tokens: &[String], boards: &[u8]) -> Result<Vec<u8>, String> {
    let mut seen: Vec<u8> = Vec::new();
    for t in tokens {
        let word = t.to_ascii_lowercase();
        let board = if let Ok(n) = word.parse::<u32>() {
            if n == 0 || n > 255 {
                return Err(format!("'{t}': positions are counted from 1"));
            }
            (n - 1) as u8
        } else {
            let colours: Vec<usize> = (0..BOARD_COLOURS.len()).filter(|&i| BOARD_COLOURS[i].0.starts_with(&word)).collect();
            let &[colour] = colours.as_slice() else {
                return Err(format!("'{t}' is neither a colour ({}) nor a position (1..{})", colour_names(), boards.iter().max().map(|b| *b as u32 + 1).unwrap_or(1)));
            };
            let lit: Vec<u8> = boards.iter().copied().filter(|b| *b as usize % BOARD_COLOURS.len() == colour).collect();
            match lit.as_slice() {
                [b] => *b,
                [] => return Err(format!("no connected board is lit {}", BOARD_COLOURS[colour].0)),
                _ => return Err(format!("{} boards are lit {}; name them by position", lit.len(), BOARD_COLOURS[colour].0)),
            }
        };
        if !boards.contains(&board) {
            return Err(format!("position {} ({}) is not lit: no module is connected there", board as u32 + 1, colour_of(board).0));
        }
        if seen.contains(&board) {
            return Err(format!("{} (position {}) is named twice", colour_of(board).0, board as u32 + 1));
        }
        seen.push(board);
    }
    if seen.len() != boards.len() {
        return Err(format!(
            "{} board(s) are connected, {} named: name every board once, as seen from left to right",
            boards.len(),
            seen.len()
        ));
    }
    Ok(seen)
}

/// One module in a plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Move {
    pub serial: String,
    pub before: u8,
    pub after: u8,
    /// The settings file pinned this module to `before`.
    pub was_pinned: bool,
}

/// What a reorder (or `pin`) does: where every connected module goes and what the
/// settings file's pin list becomes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    /// Every connected module, by its new board.
    pub moves: Vec<Move>,
    /// The new `openxen.boards`: all connected modules, and the pins of modules that are
    /// not connected as far as they do not collide.
    pub pins: Vec<OpenxenBoardSettings>,
    /// Pins of modules that are not connected and whose board now belongs to a connected
    /// module: removed.
    pub dropped: Vec<OpenxenBoardSettings>,
    /// The pin list differs from the one in the settings file.
    pub pins_changed: bool,
}

impl Plan {
    pub fn assign(&self) -> Vec<(String, u8)> {
        self.moves.iter().map(|m| (m.serial.clone(), m.after)).collect()
    }

    pub fn moved(&self) -> bool {
        self.moves.iter().any(|m| m.before != m.after)
    }

    pub fn is_noop(&self) -> bool {
        !self.moved() && !self.pins_changed
    }

    pub fn describe(&self) -> String {
        let mut out = String::from("  serial                    before               after\n");
        for m in &self.moves {
            out.push_str(&format!(
                "  {:<24}  {:<19}  {:<19}  {}\n",
                m.serial,
                board_label(m.before),
                board_label(m.after),
                match (m.before != m.after, m.was_pinned) {
                    (true, _) => "moves",
                    (false, true) => "stays",
                    (false, false) => "stays, newly pinned",
                }
            ));
        }
        for d in &self.dropped {
            out.push_str(&format!(
                "  {:<24}  not connected; its pin to position {} is removed (a connected module is there now)\n",
                d.serial,
                d.board as u32 + 1
            ));
        }
        let kept: Vec<String> = self
            .pins
            .iter()
            .filter(|p| !self.moves.iter().any(|m| m.serial == p.serial))
            .map(|p| format!("{} (position {})", p.serial, p.board as u32 + 1))
            .collect();
        if !kept.is_empty() {
            out.push_str(&format!("  pins of modules that are not connected stay: {}\n", kept.join(", ")));
        }
        out
    }
}

fn same_pins(a: &[OpenxenBoardSettings], b: &[OpenxenBoardSettings]) -> bool {
    let key = |v: &[OpenxenBoardSettings]| {
        let mut k: Vec<(u8, String)> = v.iter().map(|p| (p.board, p.serial.clone())).collect();
        k.sort();
        k
    };
    key(a) == key(b)
}

/// The pin list after `connected` (serial, board) are pinned: every connected module, then
/// the old pins of modules that are not connected, except those whose board is now taken
/// (returned second). Sorted by board.
pub fn merge_pins(old: &[OpenxenBoardSettings], connected: &[(String, u8)]) -> (Vec<OpenxenBoardSettings>, Vec<OpenxenBoardSettings>) {
    let mut pins: Vec<OpenxenBoardSettings> = connected.iter().map(|(s, b)| OpenxenBoardSettings { serial: s.clone(), board: *b }).collect();
    let mut dropped = Vec::new();
    for p in old.iter().filter(|p| !connected.iter().any(|(s, _)| *s == p.serial)) {
        if connected.iter().any(|(_, b)| *b == p.board) {
            dropped.push(p.clone());
        } else if !pins.iter().any(|q| q.serial == p.serial) {
            pins.push(p.clone());
        }
    }
    pins.sort_by(|a, b| (a.board, &a.serial).cmp(&(b.board, &b.serial)));
    (pins, dropped)
}

/// `seen[i]`: the board (by its colour) the user sees at the i-th place from the left. The
/// connected modules keep the set of boards they occupy now; the module seen at place i
/// gets the i-th of them. `pins`: the settings file's current list.
pub fn plan_order(rows: &[Row], seen: &[u8], pins: &[OpenxenBoardSettings]) -> Result<Plan, String> {
    let mut targets: Vec<u8> = rows.iter().map(|r| r.board).collect();
    targets.sort_unstable();
    if seen.len() != targets.len() {
        return Err(format!("{} board(s) are connected, {} named", targets.len(), seen.len()));
    }
    let mut moves = Vec::new();
    for (place, board) in seen.iter().enumerate() {
        let row = rows.iter().find(|r| r.board == *board).ok_or_else(|| format!("no module is connected at position {}", *board as u32 + 1))?;
        let serial = row.serial.clone().ok_or_else(|| "the serial numbers of the boards are not known".to_string())?;
        if moves.iter().any(|m: &Move| m.serial == serial) {
            return Err(format!("position {} is named twice", *board as u32 + 1));
        }
        let was_pinned = pins.iter().any(|p| p.serial == serial && p.board == row.board);
        moves.push(Move { serial, before: row.board, after: targets[place], was_pinned });
    }
    let connected: Vec<(String, u8)> = moves.iter().map(|m| (m.serial.clone(), m.after)).collect();
    let (new_pins, dropped) = merge_pins(pins, &connected);
    let pins_changed = !same_pins(&new_pins, pins);
    Ok(Plan { moves, pins: new_pins, dropped, pins_changed })
}

/// Words the user typed → plan. Err is for the user (wrong input, or an order that cannot
/// be changed); nothing has happened then.
pub fn make_plan(rows: &[Row], tokens: &[String], pins: &[OpenxenBoardSettings]) -> Result<Plan, String> {
    if rows.iter().any(|r| r.serial.is_none()) {
        return Err("the order of these boards cannot be changed here: it is not known which device is behind which board".into());
    }
    let mut boards: Vec<u8> = rows.iter().map(|r| r.board).collect();
    boards.sort_unstable();
    let seen = parse_seen(tokens, &boards)?;
    plan_order(rows, &seen, pins)
}

/// `boards pin`: every connected module is pinned where it is.
pub fn plan_pin(rows: &[Row], pins: &[OpenxenBoardSettings]) -> Result<Plan, String> {
    let mut boards: Vec<u8> = rows.iter().map(|r| r.board).collect();
    boards.sort_unstable();
    plan_order(rows, &boards, pins)
}

/// `boards unpin`: the pin list without `serial` (the whole serial, or a beginning or end
/// of it that fits one pin only), or without every pin. Returns (kept, removed).
pub fn unpin(pins: &[OpenxenBoardSettings], serial: Option<&str>) -> Result<(Vec<OpenxenBoardSettings>, Vec<OpenxenBoardSettings>), String> {
    let Some(serial) = serial else { return Ok((Vec::new(), pins.to_vec())) };
    let wanted = serial.to_ascii_uppercase();
    let matches = |p: &&OpenxenBoardSettings| {
        let s = p.serial.to_ascii_uppercase();
        s == wanted || s.starts_with(&wanted) || s.ends_with(&wanted)
    };
    let hits: Vec<&OpenxenBoardSettings> = pins.iter().filter(matches).collect();
    match hits.as_slice() {
        [] => Err(format!("no pinned serial matches '{serial}'. Pinned: {}", pins.iter().map(|p| p.serial.as_str()).collect::<Vec<_>>().join(", "))),
        [one] => {
            let one = (*one).clone();
            Ok((pins.iter().filter(|p| p.serial != one.serial).cloned().collect(), vec![one]))
        }
        many => Err(format!("'{serial}' fits {} pinned serials: {}", many.len(), many.iter().map(|p| p.serial.as_str()).collect::<Vec<_>>().join(", "))),
    }
}

/// For a serve that does not tell serials yet: which module is on which board, as far as
/// the settings decide it (every connected module pinned, to exactly the boards that are
/// connected). None when modules were assigned automatically.
pub fn rows_from_pins(boards: &[u8], serials: &[String], pins: &[OpenxenBoardSettings]) -> Option<Vec<Row>> {
    let mut rows: Vec<Row> = Vec::new();
    for s in serials {
        let pin = pins.iter().find(|p| p.serial == *s)?;
        if rows.iter().any(|r| r.board == pin.board) {
            return None;
        }
        rows.push(Row { board: pin.board, serial: Some(s.clone()), pinned: Some(true) });
    }
    rows.sort_by_key(|r| r.board);
    let mut connected = boards.to_vec();
    connected.sort_unstable();
    (rows.iter().map(|r| r.board).collect::<Vec<_>>() == connected).then_some(rows)
}

// --- the keyboard, the settings and the terminal as the dialogue sees them ---

/// How the lighting is put back after the identification colours.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Restore {
    /// A serve drives the LEDs: its layout colours, at full brightness.
    LayoutColours,
    /// No program drives the LEDs: off, not a stale frame.
    Off,
}

pub fn restore_for(serving: bool) -> Restore {
    if serving { Restore::LayoutColours } else { Restore::Off }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Applied {
    /// The keyboard plays and shows the new order now.
    Live,
    /// The settings are written but what is running did not take the order over; the text
    /// says what to do.
    RestartNeeded(String),
}

pub trait Keyboard {
    /// How the keyboard is reached, for the heading of the listing.
    fn describe(&self) -> String;
    /// Things the user should know (e.g. why serials are missing).
    fn notes(&self) -> Vec<String> {
        Vec::new()
    }
    /// A serve drives the LEDs (decides what `Lights` restores).
    fn serving(&self) -> bool;
    /// The connected boards, ascending.
    fn rows(&mut self) -> Result<Vec<Row>>;
    /// Every board in the colour of its position.
    fn light(&mut self, rows: &[Row]) -> Result<()>;
    /// Called a few times per second while a question waits (keepalives).
    fn tick(&mut self) {}
    /// Move modules (`assign`: serial → board) and take over the new pin list.
    fn apply(&mut self, assign: &[(String, u8)], pins: &[OpenxenBoardSettings]) -> Result<Applied>;
    fn restore(&mut self, how: Restore) -> Result<()>;
}

/// The pin list of the settings file.
pub trait PinStore {
    fn pins(&self) -> Vec<OpenxenBoardSettings>;
    fn write(&mut self, pins: &[OpenxenBoardSettings]) -> Result<()>;
}

/// `openxen.boards` of a settings file; a backup copy is made before the first write.
pub struct SettingsFile {
    pub path: PathBuf,
    backed_up: bool,
}

impl SettingsFile {
    pub fn new(path: PathBuf) -> Self {
        Self { path, backed_up: false }
    }
}

impl PinStore for SettingsFile {
    fn pins(&self) -> Vec<OpenxenBoardSettings> {
        crate::settings::load_from(&self.path).openxen.boards
    }

    fn write(&mut self, pins: &[OpenxenBoardSettings]) -> Result<()> {
        let backup = (!self.backed_up).then(|| crate::settings::backup_path(&self.path, "before-boards"));
        let made = crate::settings::store_openxen_boards(&self.path, pins, backup.as_deref())?;
        self.backed_up = true;
        match made {
            Some(b) => println!("Settings written: {} (previous version kept as {})", self.path.display(), b.display()),
            None => println!("Settings written: {}", self.path.display()),
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Question {
    /// Is the order correct as seen?
    Correct,
    /// The order as seen, or quit.
    Order,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Input {
    Line(String),
    /// Nobody answered in time.
    Timeout,
    /// Ctrl+C, or the terminal went away.
    Interrupted,
    /// No question was asked (`--yes`, or not a terminal); the lights were shown for the
    /// stated time.
    NotAsked,
}

pub trait Prompt {
    /// `idle` is called regularly while waiting.
    fn ask(&mut self, question: Question, idle: &mut dyn FnMut()) -> Input;
}

// --- the lights guard ---

/// Identification colours on the boards, and the promise that they go away again: `restore`
/// is issued once after `light`, by whichever comes first of an explicit call and the drop
/// (error paths).
pub struct Lights<'a> {
    kb: &'a mut dyn Keyboard,
    lit: bool,
}

impl<'a> Lights<'a> {
    pub fn new(kb: &'a mut dyn Keyboard) -> Self {
        Self { kb, lit: false }
    }

    fn light(&mut self, rows: &[Row]) -> Result<()> {
        // lit even if this fails half way: some boards may show their colour already
        self.lit = true;
        self.kb.light(rows)
    }

    fn restore(&mut self) {
        if !self.lit {
            return;
        }
        self.lit = false;
        let how = restore_for(self.kb.serving());
        match self.kb.restore(how) {
            Ok(()) => match how {
                Restore::LayoutColours => println!("Identification colours off: the layout colours are back."),
                Restore::Off => println!("Identification colours off: LEDs dark (nothing is serving; `xentool serve` brings the layout colours)."),
            },
            Err(e) => eprintln!("xentool boards: could not put the lighting back: {e:#}"),
        }
    }
}

impl Drop for Lights<'_> {
    fn drop(&mut self) {
        self.restore();
    }
}

// --- the dialogue ---

/// How the dialogue ended. The lighting is restored in every case.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// The user said the order is correct.
    Confirmed,
    /// Shown for the stated time without a question.
    Shown,
    /// The user quit instead of entering an order.
    Declined,
    TimedOut,
    Interrupted,
    /// Settings written, but the running serve has to be restarted.
    RestartNeeded,
    /// Listed only (lights switched off with `--seconds 0`).
    Listed,
}

fn is_yes(line: &str) -> bool {
    matches!(line.trim().to_ascii_lowercase().as_str(), "" | "y" | "yes" | "j" | "ja" | "correct" | "ok")
}

fn is_no(line: &str) -> bool {
    matches!(line.trim().to_ascii_lowercase().as_str(), "n" | "no" | "nein" | "wrong")
}

fn is_quit(line: &str) -> bool {
    matches!(line.trim().to_ascii_lowercase().as_str(), "" | "q" | "quit" | "exit")
}

/// Write the plan's pins and hand the order to the keyboard.
fn execute(kb: &mut dyn Keyboard, store: &mut dyn PinStore, plan: &Plan) -> Result<Applied> {
    print!("Board order, by serial number:\n{}", plan.describe());
    if plan.is_noop() {
        println!("Nothing to change: this is the order in use, and it is pinned.");
        return Ok(Applied::Live);
    }
    if plan.pins_changed {
        store.write(&plan.pins)?;
    }
    let applied = kb.apply(&plan.assign(), &plan.pins)?;
    if applied == Applied::Live && plan.moved() {
        println!("New order applied.");
    }
    Ok(applied)
}

/// List the boards, light them, ask whether the order is correct; on "no" take the order
/// as seen, apply it, light and ask again. `initial`: an order to apply before the first
/// listing (`boards order ...`); an error in it ends the command. `lights`: false lists
/// (and applies) without lighting or asking.
pub fn run(kb: &mut dyn Keyboard, prompt: &mut dyn Prompt, store: &mut dyn PinStore, initial: Option<Vec<String>>, lights: bool) -> Result<Outcome> {
    let mut guard = Lights::new(kb);
    let mut rows = guard.kb.rows()?;
    if rows.is_empty() {
        bail!("no boards are connected");
    }
    if let Some(tokens) = initial {
        let plan = make_plan(&rows, &tokens, &store.pins()).map_err(|e| anyhow!(e))?;
        if let Applied::RestartNeeded(why) = execute(&mut *guard.kb, store, &plan)? {
            println!("{why}");
            return Ok(Outcome::RestartNeeded);
        }
        rows = guard.kb.rows()?;
    }
    loop {
        print!("Boards {}, left to right:\n{}", guard.kb.describe(), listing(&rows));
        for n in guard.kb.notes() {
            println!("Note: {n}");
        }
        if !lights {
            return Ok(Outcome::Listed);
        }
        guard.light(&rows)?;
        println!("Every board is lit in the colour of its position.");
        // one question after the other until the order is confirmed, changed or given up
        let mut question = Question::Correct;
        let plan = loop {
            let input = prompt.ask(question, &mut || guard.kb.tick());
            let line = match input {
                Input::Line(l) => l,
                Input::NotAsked => {
                    guard.restore();
                    return Ok(Outcome::Shown);
                }
                Input::Timeout => {
                    println!("\nNo answer: ending the check.");
                    guard.restore();
                    return Ok(Outcome::TimedOut);
                }
                Input::Interrupted => {
                    println!();
                    guard.restore();
                    return Ok(Outcome::Interrupted);
                }
            };
            if question == Question::Correct {
                if is_yes(&line) {
                    guard.restore();
                    return Ok(Outcome::Confirmed);
                }
                if is_no(&line) {
                    question = Question::Order;
                    continue;
                }
                // anything else is taken as the order itself
            } else if is_quit(&line) {
                guard.restore();
                return Ok(Outcome::Declined);
            }
            match make_plan(&rows, &split_tokens(&line), &store.pins()) {
                Ok(plan) => break plan,
                Err(e) => {
                    println!("  {e}.");
                    question = Question::Order;
                }
            }
        };
        if let Applied::RestartNeeded(why) = execute(&mut *guard.kb, store, &plan)? {
            println!("{why}");
            guard.restore();
            return Ok(Outcome::RestartNeeded);
        }
        rows = guard.kb.rows()?;
    }
}

// --- the terminal ---

/// Questions on the terminal. Not a terminal, or `--yes`: no question, the lights stay for
/// `timeout`. The timeout is also the longest wait for an answer, so that boards are never
/// left lit when nobody answers; Ctrl+C and a closed terminal end the wait at once.
pub struct TerminalPrompt {
    lines: Option<Receiver<Option<String>>>,
    timeout: Duration,
    interrupted: Arc<AtomicBool>,
}

impl TerminalPrompt {
    pub fn new(yes: bool, timeout: Duration, interrupted: Arc<AtomicBool>) -> Self {
        let lines = (!yes && std::io::stdin().is_terminal()).then(|| {
            let (tx, rx) = crossbeam_channel::unbounded();
            std::thread::spawn(move || {
                loop {
                    let mut line = String::new();
                    match std::io::stdin().read_line(&mut line) {
                        Ok(n) if n > 0 => {
                            if tx.send(Some(line)).is_err() {
                                return;
                            }
                        }
                        _ => {
                            let _ = tx.send(None);
                            return;
                        }
                    }
                }
            });
            rx
        });
        Self { lines, timeout, interrupted }
    }

}

impl Prompt for TerminalPrompt {
    fn ask(&mut self, question: Question, idle: &mut dyn FnMut()) -> Input {
        let deadline = Instant::now() + self.timeout;
        let step = Duration::from_millis(100);
        let Some(lines) = &self.lines else {
            println!("Shown for {} s (Ctrl+C ends it).", self.timeout.as_secs());
            while Instant::now() < deadline {
                if self.interrupted.load(Ordering::Relaxed) {
                    return Input::Interrupted;
                }
                std::thread::sleep(step.min(deadline.saturating_duration_since(Instant::now())));
                idle();
            }
            return Input::NotAsked;
        };
        match question {
            Question::Correct => print!(
                "Do the boards read {} from left to right? [Y/n, or the colours as you see them] ",
                BOARD_COLOURS.iter().map(|c| c.0).collect::<Vec<_>>().join(", ")
            ),
            Question::Order => print!("Colours (or positions) as you see them, left to right; Enter or q to leave it as it is: "),
        }
        let _ = std::io::stdout().flush();
        loop {
            if self.interrupted.load(Ordering::Relaxed) {
                return Input::Interrupted;
            }
            match lines.recv_timeout(step) {
                Ok(Some(line)) => return Input::Line(line),
                // end of input: the terminal is gone
                Ok(None) | Err(RecvTimeoutError::Disconnected) => return Input::Interrupted,
                Err(RecvTimeoutError::Timeout) => {}
            }
            idle();
            if Instant::now() >= deadline {
                return Input::Timeout;
            }
        }
    }
}

// --- a running serve, through its local API ---

enum HttpError {
    /// Nothing answers on the port.
    Unreachable(String),
    Status(u16, String),
}

struct Http {
    agent: ureq::Agent,
    base: String,
}

impl Http {
    fn new(port: u16) -> Self {
        let agent = ureq::AgentBuilder::new().timeout_connect(Duration::from_millis(700)).timeout(Duration::from_secs(4)).build();
        Self { agent, base: format!("http://127.0.0.1:{port}") }
    }

    fn done(r: std::result::Result<ureq::Response, ureq::Error>) -> std::result::Result<serde_json::Value, HttpError> {
        match r {
            Ok(resp) => resp.into_json().map_err(|e| HttpError::Status(200, e.to_string())),
            Err(ureq::Error::Status(code, resp)) => {
                let body: serde_json::Value = resp.into_json().unwrap_or(serde_json::Value::Null);
                Err(HttpError::Status(code, body.get("error").and_then(|e| e.as_str()).unwrap_or("").to_string()))
            }
            Err(e) => Err(HttpError::Unreachable(e.to_string())),
        }
    }

    fn get(&self, path: &str) -> std::result::Result<serde_json::Value, HttpError> {
        Self::done(self.agent.get(&format!("{}{path}", self.base)).call())
    }

    fn post(&self, path: &str, body: &serde_json::Value) -> std::result::Result<serde_json::Value, HttpError> {
        Self::done(self.agent.post(&format!("{}{path}", self.base)).send_json(body))
    }
}

fn http_err(what: &str, e: HttpError) -> anyhow::Error {
    match e {
        HttpError::Unreachable(e) => anyhow!("{what}: the serve does not answer ({e})"),
        HttpError::Status(code, msg) => anyhow!("{what}: HTTP {code} {msg}"),
    }
}

/// The overlay that lights every playable key of each board in its position's colour.
pub fn lights_body(setup: &serde_json::Value, boards: &[u8]) -> serde_json::Value {
    let set: Vec<serde_json::Value> = setup["keys"]
        .as_array()
        .map(|keys| {
            keys.iter()
                .filter_map(|k| {
                    let board = k["board"].as_u64()? as u8;
                    let key = k["key"].as_u64()?;
                    boards.contains(&board).then(|| serde_json::json!({ "board": board, "key": key, "rgb": crate::api::hex_color(colour_of(board).1) }))
                })
                .collect()
        })
        .unwrap_or_default();
    serde_json::json!({ "clear": "all", "set": set })
}

/// The overlay is cleared and the layout colours are back at full brightness.
pub fn restore_body() -> serde_json::Value {
    serde_json::json!({ "clear": "all", "dim_base": 1.0 })
}

pub struct ApiKeyboard {
    http: Http,
    port: u16,
    backend: String,
    /// The serve has `/api/boards` (else it predates this command).
    modern: bool,
    /// The backend can move boards while serving.
    reassign: bool,
    settings: PathBuf,
    /// Serials of the attached openxen modules (HID enumeration), for a serve that does
    /// not tell them.
    attached: Vec<String>,
}

impl ApiKeyboard {
    /// None when nothing answers on the port.
    pub fn connect(port: u16, settings: &Path, attached: Vec<String>) -> Result<Option<Self>> {
        let http = Http::new(port);
        let setup = match http.get("/api/setup") {
            Ok(s) => s,
            Err(HttpError::Unreachable(_)) => return Ok(None),
            Err(e) => return Err(http_err("reading /api/setup", e)),
        };
        let backend = setup["backend"].as_str().unwrap_or("?").to_string();
        let (modern, reassign) = match http.get("/api/boards") {
            Ok(b) => (true, b["reassign"].as_bool().unwrap_or(false)),
            Err(_) => (false, false),
        };
        Ok(Some(Self { http, port, backend, modern, reassign, settings: settings.to_path_buf(), attached }))
    }

    fn connected_boards(v: &serde_json::Value) -> Vec<u8> {
        v["boards"].as_array().map(|b| b.iter().filter_map(|x| x.as_u64().map(|n| n as u8)).collect()).unwrap_or_default()
    }

    fn old_openxen(&self) -> bool {
        !self.modern && self.backend == "openxen"
    }
}

impl Keyboard for ApiKeyboard {
    fn describe(&self) -> String {
        format!("of the {} keyboard (through the running `xentool serve`, API port {})", self.backend, self.port)
    }

    fn notes(&self) -> Vec<String> {
        if self.old_openxen() {
            vec!["the running serve is older than this command and does not tell which module is on which board; the serials are taken from the settings where they decide it. A new order needs a restart of serve.".into()]
        } else if !self.reassign {
            vec![format!("the {} backend has a fixed board order; it can be checked here, not changed.", self.backend)]
        } else {
            Vec::new()
        }
    }

    fn serving(&self) -> bool {
        true
    }

    fn rows(&mut self) -> Result<Vec<Row>> {
        if self.modern {
            let v = self.http.get("/api/boards").map_err(|e| http_err("reading /api/boards", e))?;
            let modules: Vec<crate::api::SetupModule> = serde_json::from_value(v["modules"].clone()).unwrap_or_default();
            if !modules.is_empty() {
                return Ok(modules.into_iter().map(|m| Row { board: m.board, serial: Some(m.serial), pinned: Some(m.pinned) }).collect());
            }
            return Ok(Self::connected_boards(&v).into_iter().map(|board| Row { board, serial: None, pinned: None }).collect());
        }
        let setup = self.http.get("/api/setup").map_err(|e| http_err("reading /api/setup", e))?;
        let boards = Self::connected_boards(&setup);
        if self.old_openxen() {
            let pins = crate::settings::load_from(&self.settings).openxen.boards;
            if let Some(rows) = rows_from_pins(&boards, &self.attached, &pins) {
                return Ok(rows);
            }
        }
        Ok(boards.into_iter().map(|board| Row { board, serial: None, pinned: None }).collect())
    }

    fn light(&mut self, rows: &[Row]) -> Result<()> {
        let setup = self.http.get("/api/setup").map_err(|e| http_err("reading /api/setup", e))?;
        let boards: Vec<u8> = rows.iter().map(|r| r.board).collect();
        self.http.post("/api/leds/overlay", &lights_body(&setup, &boards)).map_err(|e| http_err("lighting the boards", e))?;
        Ok(())
    }

    fn apply(&mut self, assign: &[(String, u8)], pins: &[OpenxenBoardSettings]) -> Result<Applied> {
        let pairs = |v: Vec<(&str, u8)>| v.into_iter().map(|(s, b)| serde_json::json!({ "serial": s, "board": b })).collect::<Vec<_>>();
        let body = serde_json::json!({
            "assign": pairs(assign.iter().map(|(s, b)| (s.as_str(), *b)).collect()),
            "pins": pairs(pins.iter().map(|p| (p.serial.as_str(), p.board)).collect()),
        });
        match self.http.post("/api/boards", &body) {
            Ok(_) => {}
            Err(HttpError::Status(404, _)) => {
                return Ok(Applied::RestartNeeded(
                    "The running serve is older than this command and cannot change the order while it runs: restart `xentool serve` to play in the new order.".into(),
                ));
            }
            Err(e) => return Err(http_err("changing the board order in the running serve", e)),
        }
        // the serve loop applies it between two scans; wait until it says so
        let mut want: Vec<(String, u8)> = assign.to_vec();
        want.sort();
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            let v = self.http.get("/api/boards").map_err(|e| http_err("reading /api/boards", e))?;
            let modules: Vec<crate::api::SetupModule> = serde_json::from_value(v["modules"].clone()).unwrap_or_default();
            if want.iter().all(|(s, b)| modules.iter().any(|m| m.serial == *s && m.board == *b)) {
                return Ok(Applied::Live);
            }
            if Instant::now() >= deadline {
                return Ok(Applied::RestartNeeded(
                    "The running serve accepted the new order but does not report it (did a module disconnect?): restart `xentool serve` to play in the order now in the settings.".into(),
                ));
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    fn restore(&mut self, _how: Restore) -> Result<()> {
        self.http.post("/api/leds/overlay", &restore_body()).map_err(|e| http_err("clearing the identification colours", e))?;
        Ok(())
    }
}

// --- openxen modules, opened directly (nothing is serving) ---

#[cfg(not(target_os = "android"))]
mod direct {
    use super::*;
    use crate::openxen::{hid, led, serve};
    use crate::settings::OpenxenSettings;

    /// Firmware host timeout while this command lights the modules: should the command be
    /// killed, the modules switch their LEDs off by themselves after this time (openxen
    /// `docs/led-protocol.md`, "Host timeout"). Keepalives hold it off meanwhile.
    const HOST_TIMEOUT_MS: u16 = 2000;

    pub struct DirectKeyboard {
        devs: Vec<(String, hidapi::HidDevice)>,
        pins: Vec<OpenxenBoardSettings>,
        cfg: led::LedConfig,
        last_send: Instant,
    }

    /// Serials of the attached modules; empty when none or when HID cannot be read.
    pub fn attached_serials() -> Vec<String> {
        let Ok(mut api) = hidapi::HidApi::new() else { return Vec::new() };
        hid::list_modules(&mut api).map(|m| m.into_iter().map(|m| m.serial).collect()).unwrap_or_default()
    }

    /// Board of each module (serials in ascending order, as `xentool load` assigns them).
    pub fn assign_all(serials: &[String], pins: &[OpenxenBoardSettings]) -> Vec<Row> {
        let settings = OpenxenSettings { boards: pins.to_vec(), ..OpenxenSettings::default() };
        let mut taken = std::collections::BTreeMap::new();
        let mut rows = Vec::new();
        for s in serials {
            if let Some(board) = serve::assign_board(s, &settings, &taken) {
                taken.insert(board, Arc::<str>::from(s.as_str()));
                rows.push(Row { board, serial: Some(s.clone()), pinned: Some(pins.iter().any(|p| p.serial == *s && p.board == board)) });
            }
        }
        rows.sort_by_key(|r| r.board);
        rows
    }

    impl DirectKeyboard {
        /// None when no module is attached.
        pub fn open(settings: &crate::settings::Settings) -> Result<Option<Self>> {
            let mut api = hidapi::HidApi::new().context("initialising hidapi")?;
            let modules = hid::list_modules(&mut api)?;
            if modules.is_empty() {
                return Ok(None);
            }
            let mut devs = Vec::new();
            for m in &modules {
                devs.push((m.serial.clone(), api.open_path(&m.path).with_context(|| format!("opening module {}", m.serial))?));
            }
            let cfg = led::LedConfig { highlight: None, timeout_ms: HOST_TIMEOUT_MS, ..serve::led_config(&settings.openxen) };
            Ok(Some(Self { devs, pins: settings.openxen.boards.clone(), cfg, last_send: Instant::now() }))
        }

        fn send(&mut self, serial: &str, reports: &[[u8; led::REPORT_SIZE]]) -> Result<()> {
            let Some((_, dev)) = self.devs.iter().find(|(s, _)| s == serial) else { return Ok(()) };
            for r in reports {
                dev.write(&led::with_report_id(r)).with_context(|| format!("writing to module {serial}"))?;
            }
            Ok(())
        }
    }

    impl Keyboard for DirectKeyboard {
        fn describe(&self) -> String {
            "of the openxen keyboard (modules opened directly: no `xentool serve --api` is running)".into()
        }

        fn serving(&self) -> bool {
            false
        }

        fn rows(&mut self) -> Result<Vec<Row>> {
            let serials: Vec<String> = self.devs.iter().map(|(s, _)| s.clone()).collect();
            Ok(assign_all(&serials, &self.pins))
        }

        fn light(&mut self, rows: &[Row]) -> Result<()> {
            for r in rows {
                let Some(serial) = r.serial.clone() else { continue };
                let mut reports = vec![led::config_report(&self.cfg)];
                reports.extend(led::frame_reports(&[colour_of(r.board).1; led::KEYS]));
                self.send(&serial, &reports)?;
            }
            self.last_send = Instant::now();
            Ok(())
        }

        fn tick(&mut self) {
            if self.last_send.elapsed() >= led::KEEPALIVE_INTERVAL {
                for (_, dev) in &self.devs {
                    let _ = dev.write(&led::with_report_id(&led::keepalive_report()));
                }
                self.last_send = Instant::now();
            }
        }

        fn apply(&mut self, _assign: &[(String, u8)], pins: &[OpenxenBoardSettings]) -> Result<Applied> {
            // nothing plays: the order is what the settings say from now on
            self.pins = pins.to_vec();
            Ok(Applied::Live)
        }

        /// Dark on purpose: a black frame now (not a colour left standing until the
        /// firmware's host timeout, which then keeps the LEDs off).
        fn restore(&mut self, _how: Restore) -> Result<()> {
            let dark = led::frame_reports(&[(0, 0, 0); led::KEYS]);
            let serials: Vec<String> = self.devs.iter().map(|(s, _)| s.clone()).collect();
            let mut first_error = None;
            for s in serials {
                if let Err(e) = self.send(&s, &dark) {
                    first_error.get_or_insert(e);
                }
            }
            first_error.map_or(Ok(()), Err)
        }
    }
}

// --- the command ---

fn connect(port: u16, store: &SettingsFile) -> Result<Box<dyn Keyboard>> {
    #[cfg(not(target_os = "android"))]
    let attached = direct::attached_serials();
    #[cfg(target_os = "android")]
    let attached = Vec::new();
    if let Some(kb) = ApiKeyboard::connect(port, &store.path, attached)? {
        return Ok(Box::new(kb));
    }
    #[cfg(not(target_os = "android"))]
    if let Some(kb) = direct::DirectKeyboard::open(&crate::settings::load_from(&store.path))? {
        return Ok(Box::new(kb));
    }
    bail!("no `xentool serve --api` answers on port {port} and no openxen modules are attached")
}

pub fn cmd_boards(args: BoardsArgs) -> Result<()> {
    let mut store = SettingsFile::new(crate::settings::default_settings_path());
    let timeout = Duration::from_secs(args.seconds);
    let lights = args.seconds > 0;
    let interrupted = Arc::new(AtomicBool::new(false));
    let dialogue = |store: &mut SettingsFile, initial: Option<Vec<String>>| -> Result<()> {
        let mut kb = connect(args.api_port, store)?;
        let flag = interrupted.clone();
        ctrlc::set_handler(move || flag.store(true, Ordering::Relaxed)).context("failed to set Ctrl+C handler")?;
        let mut prompt = TerminalPrompt::new(args.yes, timeout, interrupted.clone());
        match run(kb.as_mut(), &mut prompt, store, initial, lights)? {
            Outcome::Confirmed => println!("Order confirmed."),
            Outcome::Declined => println!("Order left as it is."),
            Outcome::TimedOut | Outcome::Interrupted | Outcome::Shown | Outcome::Listed | Outcome::RestartNeeded => {}
        }
        Ok(())
    };
    match args.command.clone() {
        None => dialogue(&mut store, None),
        Some(BoardsCommand::Order { seen, dry_run }) => {
            if !dry_run {
                return dialogue(&mut store, Some(seen));
            }
            let mut kb = connect(args.api_port, &store)?;
            let rows = kb.rows()?;
            print!("Boards {}, left to right:\n{}", kb.describe(), listing(&rows));
            let plan = make_plan(&rows, &seen, &store.pins()).map_err(|e| anyhow!(e))?;
            print!("Board order, by serial number:\n{}", plan.describe());
            if plan.is_noop() {
                println!("Nothing to change: this is the order in use, and it is pinned.");
            }
            println!("Dry run: nothing written to {}, nothing changed on the keyboard.", store.path.display());
            Ok(())
        }
        Some(BoardsCommand::Pin { dry_run }) => {
            let mut kb = connect(args.api_port, &store)?;
            let rows = kb.rows()?;
            let plan = plan_pin(&rows, &store.pins()).map_err(|e| anyhow!(e))?;
            if dry_run {
                print!("Board order, by serial number:\n{}", plan.describe());
                println!("Dry run: nothing written to {}.", store.path.display());
                return Ok(());
            }
            if let Applied::RestartNeeded(why) = execute(kb.as_mut(), &mut store, &plan)? {
                println!("{why}");
            }
            Ok(())
        }
        Some(BoardsCommand::Unpin { serial, all, dry_run }) => {
            if all == serial.is_some() {
                bail!("say which pin to remove: a serial number, or --all");
            }
            let old = store.pins();
            let (kept, removed) = unpin(&old, serial.as_deref()).map_err(|e| anyhow!(e))?;
            if removed.is_empty() {
                println!("No board is pinned in {}.", store.path.display());
                return Ok(());
            }
            for r in &removed {
                println!("  {}  no longer pinned to position {} (board{})", r.serial, r.board as u32 + 1, r.board);
            }
            if dry_run {
                println!("Dry run: nothing written to {}.", store.path.display());
                return Ok(());
            }
            store.write(&kept)?;
            // a running serve takes the new pin list for modules that connect later
            match ApiKeyboard::connect(args.api_port, &store.path, Vec::new())? {
                Some(mut kb) if kb.reassign => {
                    kb.apply(&[], &kept)?;
                    println!("The running serve keeps every connected module where it is; unpinned modules take the lowest free position when they connect again.");
                }
                Some(kb) if kb.backend == "openxen" => println!("Restart `xentool serve` for it to use the new pinning."),
                _ => {}
            }
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    fn pin(serial: &str, board: u8) -> OpenxenBoardSettings {
        OpenxenBoardSettings { serial: serial.into(), board }
    }

    fn row(serial: &str, board: u8, pinned: bool) -> Row {
        Row { board, serial: Some(serial.into()), pinned: Some(pinned) }
    }

    fn words(s: &str) -> Vec<String> {
        split_tokens(s)
    }

    fn five() -> Vec<Row> {
        vec![row("A", 0, true), row("B", 1, true), row("C", 2, true), row("D", 3, true), row("E", 4, true)]
    }

    fn five_pins() -> Vec<OpenxenBoardSettings> {
        vec![pin("A", 0), pin("B", 1), pin("C", 2), pin("D", 3), pin("E", 4)]
    }

    fn pairs(p: &[OpenxenBoardSettings]) -> Vec<(&str, u8)> {
        p.iter().map(|p| (p.serial.as_str(), p.board)).collect()
    }

    #[test]
    fn the_palette_is_defined_once_and_names_five_positions() {
        let names: Vec<&str> = BOARD_COLOURS.iter().map(|c| c.0).collect();
        assert_eq!(names, vec!["red", "orange", "green", "blue", "white"]);
        // five different colours, none of them dark
        let mut rgb: Vec<Rgb> = BOARD_COLOURS.iter().map(|c| c.1).collect();
        rgb.sort();
        rgb.dedup();
        assert_eq!(rgb.len(), 5);
        assert_eq!(colour_of(1), ("orange", BOARD_COLOURS[1].1));
        assert_eq!(colour_of(5).0, "red");
        // the listing, the lights and the input all go through it
        assert!(listing(&five()).contains("2    orange  board1  B"));
        let setup = serde_json::json!({ "keys": [{"board": 1, "key": 7}, {"board": 4, "key": 0}, {"board": 2, "key": 3}] });
        let body = lights_body(&setup, &[1, 4]);
        assert_eq!(body["clear"], "all");
        assert_eq!(body["set"], serde_json::json!([{"board": 1, "key": 7, "rgb": "FF5A00"}, {"board": 4, "key": 0, "rgb": "FFFFFF"}]));
        assert_eq!(parse_seen(&words("orange white"), &[1, 4]).unwrap(), vec![1, 4]);
    }

    #[test]
    fn seen_order_from_colours_numbers_and_beginnings() {
        let all = [0u8, 1, 2, 3, 4];
        assert_eq!(parse_seen(&words("orange red green blue white"), &all).unwrap(), vec![1, 0, 2, 3, 4]);
        assert_eq!(parse_seen(&words("2 1 3 4 5"), &all).unwrap(), vec![1, 0, 2, 3, 4]);
        assert_eq!(parse_seen(&words("Orange, 1, G, bl, WHITE"), &all).unwrap(), vec![1, 0, 2, 3, 4]);
        // three modules on boards 0, 2, 3
        assert_eq!(parse_seen(&words("blue red green"), &[0, 2, 3]).unwrap(), vec![3, 0, 2]);
    }

    #[test]
    fn seen_order_is_validated() {
        let all = [0u8, 1, 2, 3, 4];
        let err = |s: &str, boards: &[u8]| parse_seen(&words(s), boards).unwrap_err();
        assert!(err("orange red green blue", &all).contains("5 board(s) are connected, 4 named"));
        assert!(err("", &all).contains("0 named"));
        assert!(err("orange red green blue white red", &all).contains("named twice"));
        assert!(err("red red green blue white", &all).contains("red (position 1) is named twice"));
        assert!(err("1 red green blue white", &all).contains("named twice"));
        assert!(err("purple red green blue white", &all).contains("neither a colour"));
        assert!(err("0 1 2 3 4", &all).contains("counted from 1"));
        assert!(err("1 2 3 4 6", &all).contains("not lit"));
        // "b" is not a beginning of one colour only? it is: blue. "o" too. An empty-ish
        // beginning that fits several colours does not exist in this palette, but a colour
        // that is not lit is refused
        assert!(err("red orange green", &[0, 2, 3]).contains("no connected board is lit orange"));
        assert!(err("2 1 3", &[0, 2, 3]).contains("position 2 (orange) is not lit"));
    }

    #[test]
    fn a_swap_gives_the_new_pinning() {
        let plan = make_plan(&five(), &words("orange red green blue white"), &five_pins()).unwrap();
        let moves: Vec<(&str, u8, u8)> = plan.moves.iter().map(|m| (m.serial.as_str(), m.before, m.after)).collect();
        // the module lit orange lies leftmost: it becomes board 0
        assert_eq!(moves, vec![("B", 1, 0), ("A", 0, 1), ("C", 2, 2), ("D", 3, 3), ("E", 4, 4)]);
        assert_eq!(pairs(&plan.pins), vec![("B", 0), ("A", 1), ("C", 2), ("D", 3), ("E", 4)]);
        assert!(plan.moved() && plan.pins_changed && plan.dropped.is_empty());
        let text = plan.describe();
        assert!(text.contains("B                         2 orange (board1)    1 red    (board0)    moves"), "{text}");
        assert!(text.contains("stays"));
        // the same with numbers
        assert_eq!(make_plan(&five(), &words("2 1 3 4 5"), &five_pins()).unwrap(), plan);
        // a rotation: what is seen as "white red orange green blue"
        let plan = make_plan(&five(), &words("white red orange green blue"), &five_pins()).unwrap();
        assert_eq!(pairs(&plan.pins), vec![("E", 0), ("A", 1), ("B", 2), ("C", 3), ("D", 4)]);
    }

    #[test]
    fn the_identity_order_changes_nothing_when_all_is_pinned() {
        let plan = make_plan(&five(), &words("red orange green blue white"), &five_pins()).unwrap();
        assert!(plan.is_noop());
        assert_eq!(plan.pins, five_pins());
        assert_eq!(plan_pin(&five(), &five_pins()).unwrap(), plan);
    }

    #[test]
    fn unpinned_modules_are_pinned_by_an_order() {
        // only C is pinned (to board 2); the others were assigned automatically
        let rows = vec![row("A", 0, false), row("B", 1, false), row("C", 2, true), row("D", 3, false), row("E", 4, false)];
        let pins = vec![pin("C", 2)];
        let plan = make_plan(&rows, &words("red orange green white blue"), &pins).unwrap();
        assert_eq!(pairs(&plan.pins), vec![("A", 0), ("B", 1), ("C", 2), ("E", 3), ("D", 4)]);
        assert!(plan.moves.iter().find(|m| m.serial == "C").unwrap().was_pinned);
        assert!(!plan.moves.iter().find(|m| m.serial == "A").unwrap().was_pinned);
        // `pin` alone: nothing moves, but the settings change
        let plan = plan_pin(&rows, &pins).unwrap();
        assert!(!plan.moved() && plan.pins_changed && !plan.is_noop());
        assert_eq!(plan.pins.len(), 5);
        assert!(plan.describe().contains("stays, newly pinned"));
    }

    #[test]
    fn pins_of_absent_modules_stay_unless_their_board_is_taken() {
        // X is pinned to board 1 but not connected; three modules play boards 0, 2, 3
        let rows = vec![row("A", 0, true), row("B", 2, false), row("C", 3, false)];
        let pins = vec![pin("A", 0), pin("X", 1)];
        let plan = make_plan(&rows, &words("green red blue"), &pins).unwrap();
        // the connected modules keep the boards they occupy; X keeps its place between them
        assert_eq!(pairs(&plan.pins), vec![("B", 0), ("X", 1), ("A", 2), ("C", 3)]);
        assert!(plan.dropped.is_empty());
        assert!(plan.describe().contains("pins of modules that are not connected stay: X (position 2)"));

        // Y is pinned to board 0, absent, and an unpinned module had to take its board
        // (five connected): the connected module wins, Y's pin is removed and reported
        let pins = vec![pin("Y", 0), pin("B", 1)];
        let rows = vec![row("A", 0, false), row("B", 1, true), row("C", 2, false), row("D", 3, false), row("E", 4, false)];
        let plan = plan_pin(&rows, &pins).unwrap();
        assert_eq!(pairs(&plan.pins), vec![("A", 0), ("B", 1), ("C", 2), ("D", 3), ("E", 4)]);
        assert_eq!(pairs(&plan.dropped), vec![("Y", 0)]);
        assert!(plan.describe().contains("Y                         not connected; its pin to position 1 is removed"));
    }

    #[test]
    fn boards_without_serials_cannot_be_reordered() {
        let rows = vec![Row { board: 0, serial: None, pinned: None }, Row { board: 1, serial: None, pinned: None }];
        assert!(make_plan(&rows, &words("2 1"), &[]).unwrap_err().contains("cannot be changed"));
    }

    #[test]
    fn unpin_one_or_all() {
        let pins = vec![pin("ED8B2813", 0), pin("EB8B9C13", 1), pin("ED8B8513", 3)];
        let (kept, removed) = unpin(&pins, None).unwrap();
        assert!(kept.is_empty() && removed.len() == 3);
        let (kept, removed) = unpin(&pins, Some("EB8B9C13")).unwrap();
        assert_eq!((pairs(&kept), pairs(&removed)), (vec![("ED8B2813", 0), ("ED8B8513", 3)], vec![("EB8B9C13", 1)]));
        // the end of a serial is enough when it fits one pin
        assert_eq!(pairs(&unpin(&pins, Some("8513")).unwrap().1), vec![("ED8B8513", 3)]);
        assert_eq!(pairs(&unpin(&pins, Some("eb")).unwrap().1), vec![("EB8B9C13", 1)]);
        assert!(unpin(&pins, Some("ED8B")).unwrap_err().contains("fits 2"));
        assert!(unpin(&pins, Some("FFFF")).unwrap_err().contains("no pinned serial"));
    }

    #[test]
    fn an_older_serve_is_read_through_the_settings_when_they_decide_the_order() {
        let serials: Vec<String> = ["A", "B", "C"].iter().map(|s| s.to_string()).collect();
        let pins = vec![pin("B", 0), pin("A", 2), pin("C", 4), pin("X", 1)];
        let rows = rows_from_pins(&[0, 2, 4], &serials, &pins).unwrap();
        assert_eq!(rows, vec![row("B", 0, true), row("A", 2, true), row("C", 4, true)]);
        // a module that is not pinned was assigned by order of appearance: unknown
        assert_eq!(rows_from_pins(&[0, 1, 2], &serials, &[pin("A", 0), pin("B", 1)]), None);
        // the serve plays other boards than the pins say (settings edited since it started)
        assert_eq!(rows_from_pins(&[0, 1, 2], &serials, &pins), None);
    }

    // --- settings file ---

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("xentool-boards-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn pins_are_written_and_read_back_with_one_backup_per_session() {
        let dir = temp_dir("settings");
        let path = dir.join("settings.json");
        std::fs::write(&path, r#"{ "openxen": { "press_threshold": 0.1, "boards": [ { "serial": "A", "board": 0 }, { "serial": "B", "board": 1 } ] }, "last_otn": "53ET.otn" }"#).unwrap();
        let before = std::fs::read_to_string(&path).unwrap();
        let mut store = SettingsFile::new(path.clone());
        assert_eq!(pairs(&store.pins()), vec![("A", 0), ("B", 1)]);

        store.write(&[pin("B", 0), pin("A", 1)]).unwrap();
        assert_eq!(pairs(&store.pins()), vec![("B", 0), ("A", 1)]);
        // everything else is as it was
        let s = crate::settings::load_from(&path);
        assert_eq!((s.openxen.press_threshold, s.last_otn.as_deref()), (0.1, Some("53ET.otn")));
        let backups = |dir: &Path| -> Vec<PathBuf> {
            std::fs::read_dir(dir).unwrap().flatten().map(|e| e.path()).filter(|p| p.file_name().unwrap().to_string_lossy().starts_with("settings.json.before-boards-")).collect()
        };
        let b = backups(&dir);
        assert_eq!(b.len(), 1);
        assert_eq!(std::fs::read_to_string(&b[0]).unwrap(), before, "the backup is the file as it was");

        // a second write of the same session: no further backup, the first one is untouched
        store.write(&[pin("A", 0), pin("B", 1), pin("C", 2)]).unwrap();
        assert_eq!(backups(&dir).len(), 1);
        assert_eq!(std::fs::read_to_string(&b[0]).unwrap(), before);
        assert_eq!(store.pins().len(), 3);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_missing_settings_file_is_created_and_a_broken_one_is_left_alone() {
        let dir = temp_dir("settings-new");
        let path = dir.join("config").join("settings.json");
        let mut store = SettingsFile::new(path.clone());
        assert!(store.pins().is_empty());
        store.write(&[pin("A", 0)]).unwrap();
        assert_eq!(pairs(&store.pins()), vec![("A", 0)]);
        assert_eq!(std::fs::read_dir(path.parent().unwrap()).unwrap().count(), 1, "nothing to back up");

        let broken = dir.join("broken.json");
        std::fs::write(&broken, "{ not json").unwrap();
        let mut store = SettingsFile::new(broken.clone());
        assert!(store.write(&[pin("A", 0)]).is_err());
        assert_eq!(std::fs::read_to_string(&broken).unwrap(), "{ not json");
        let _ = std::fs::remove_dir_all(&dir);
    }

    // --- the dialogue, with a keyboard, a terminal and a settings file made of lists ---

    #[derive(Default)]
    struct FakeKeyboard {
        rows: Vec<Row>,
        serving: bool,
        /// The running program cannot take a new order over.
        restart_needed: bool,
        fail_light: bool,
        fail_rows_after: Option<usize>,
        rows_calls: usize,
        /// What happened, in order: "light red,orange", "apply B=0,A=1", "restore layout".
        log: Vec<String>,
    }

    impl FakeKeyboard {
        fn new(rows: Vec<Row>, serving: bool) -> Self {
            Self { rows, serving, ..Default::default() }
        }
        fn restores(&self) -> Vec<&str> {
            self.log.iter().filter(|l| l.starts_with("restore")).map(|l| l.as_str()).collect()
        }
        fn count(&self, what: &str) -> usize {
            self.log.iter().filter(|l| l.starts_with(what)).count()
        }
    }

    impl Keyboard for FakeKeyboard {
        fn describe(&self) -> String {
            "of a test keyboard".into()
        }
        fn serving(&self) -> bool {
            self.serving
        }
        fn rows(&mut self) -> Result<Vec<Row>> {
            self.rows_calls += 1;
            if self.fail_rows_after.is_some_and(|n| self.rows_calls > n) {
                bail!("the keyboard is gone");
            }
            Ok(self.rows.clone())
        }
        fn light(&mut self, rows: &[Row]) -> Result<()> {
            let serials: Vec<&str> = rows.iter().map(|r| r.serial.as_deref().unwrap_or("-")).collect();
            self.log.push(format!("light {}", serials.join(",")));
            if self.fail_light {
                bail!("a module does not answer");
            }
            Ok(())
        }
        fn apply(&mut self, assign: &[(String, u8)], pins: &[OpenxenBoardSettings]) -> Result<Applied> {
            self.log.push(format!("apply {}", assign.iter().map(|(s, b)| format!("{s}={b}")).collect::<Vec<_>>().join(",")));
            if self.restart_needed {
                return Ok(Applied::RestartNeeded("restart it".into()));
            }
            for r in self.rows.iter_mut() {
                if let Some((_, b)) = assign.iter().find(|(s, _)| Some(s) == r.serial.as_ref()) {
                    r.board = *b;
                }
                r.pinned = Some(pins.iter().any(|p| Some(&p.serial) == r.serial.as_ref() && p.board == r.board));
            }
            self.rows.sort_by_key(|r| r.board);
            Ok(Applied::Live)
        }
        fn restore(&mut self, how: Restore) -> Result<()> {
            self.log.push(format!("restore {}", match how {
                Restore::LayoutColours => "layout",
                Restore::Off => "off",
            }));
            Ok(())
        }
    }

    struct Script {
        answers: VecDeque<Input>,
        asked: Vec<Question>,
    }

    impl Script {
        fn new(answers: &[Input]) -> Self {
            Self { answers: answers.iter().cloned().collect(), asked: Vec::new() }
        }
    }

    impl Prompt for Script {
        fn ask(&mut self, question: Question, idle: &mut dyn FnMut()) -> Input {
            idle();
            self.asked.push(question);
            self.answers.pop_front().expect("the dialogue asked more often than the script answers")
        }
    }

    #[derive(Default)]
    struct MemoryStore {
        pins: Vec<OpenxenBoardSettings>,
        writes: usize,
    }

    impl PinStore for MemoryStore {
        fn pins(&self) -> Vec<OpenxenBoardSettings> {
            self.pins.clone()
        }
        fn write(&mut self, pins: &[OpenxenBoardSettings]) -> Result<()> {
            self.pins = pins.to_vec();
            self.writes += 1;
            Ok(())
        }
    }

    fn line(s: &str) -> Input {
        Input::Line(format!("{s}\n"))
    }

    fn play(kb: &mut FakeKeyboard, answers: &[Input], initial: Option<&str>) -> (Result<Outcome>, MemoryStore, Vec<Question>) {
        let mut store = MemoryStore { pins: five_pins(), writes: 0 };
        let mut script = Script::new(answers);
        let r = run(kb, &mut script, &mut store, initial.map(words), true);
        assert!(script.answers.is_empty(), "answers left over: {:?}", script.answers);
        (r, store, script.asked)
    }

    #[test]
    fn restore_is_layout_colours_when_serving_and_off_otherwise() {
        assert_eq!(restore_for(true), Restore::LayoutColours);
        assert_eq!(restore_for(false), Restore::Off);
        let mut serving = FakeKeyboard::new(five(), true);
        play(&mut serving, &[line("y")], None).0.unwrap();
        assert_eq!(serving.restores(), vec!["restore layout"]);
        let mut direct = FakeKeyboard::new(five(), false);
        play(&mut direct, &[line("y")], None).0.unwrap();
        assert_eq!(direct.restores(), vec!["restore off"]);
        // what "layout colours" means on the wire: overlay cleared, base at full brightness
        assert_eq!(restore_body(), serde_json::json!({ "clear": "all", "dim_base": 1.0 }));
    }

    #[test]
    fn confirming_restores_at_once_and_changes_nothing() {
        for yes in ["y", "", "Yes", "ja"] {
            let mut kb = FakeKeyboard::new(five(), true);
            let (r, store, asked) = play(&mut kb, &[line(yes)], None);
            assert_eq!(r.unwrap(), Outcome::Confirmed, "{yes:?}");
            assert_eq!(kb.log, vec!["light A,B,C,D,E", "restore layout"]);
            assert_eq!((store.writes, asked), (0, vec![Question::Correct]));
        }
    }

    #[test]
    fn no_then_an_order_applies_it_lights_again_and_asks_again() {
        let mut kb = FakeKeyboard::new(five(), true);
        let (r, store, asked) = play(&mut kb, &[line("n"), line("orange red green blue white"), line("y")], None);
        assert_eq!(r.unwrap(), Outcome::Confirmed);
        assert_eq!(kb.log, vec!["light A,B,C,D,E", "apply B=0,A=1,C=2,D=3,E=4", "light B,A,C,D,E", "restore layout"]);
        assert_eq!(asked, vec![Question::Correct, Question::Order, Question::Correct]);
        assert_eq!((store.writes, pairs(&store.pins)), (1, vec![("B", 0), ("A", 1), ("C", 2), ("D", 3), ("E", 4)]));
    }

    #[test]
    fn the_order_can_be_typed_instead_of_no_and_corrected_twice() {
        let mut kb = FakeKeyboard::new(five(), false);
        let (r, store, asked) = play(&mut kb, &[line("2 1 3 4 5"), line("no"), line("1 2 3 5 4"), line("")], None);
        assert_eq!(r.unwrap(), Outcome::Confirmed);
        assert_eq!(kb.count("light"), 3);
        assert_eq!(kb.count("apply"), 2);
        assert_eq!(kb.restores(), vec!["restore off"]);
        assert_eq!(kb.log.last().unwrap(), "restore off", "restored after the last light, not between the rounds");
        assert_eq!(asked, vec![Question::Correct, Question::Correct, Question::Order, Question::Correct]);
        assert_eq!((store.writes, pairs(&store.pins)), (2, vec![("B", 0), ("A", 1), ("C", 2), ("E", 3), ("D", 4)]));
    }

    #[test]
    fn wrong_input_is_explained_and_asked_again_with_the_lights_on() {
        let mut kb = FakeKeyboard::new(five(), true);
        let (r, store, asked) = play(&mut kb, &[line("n"), line("orange red"), line("purple 1 2 3 4"), line("q")], None);
        assert_eq!(r.unwrap(), Outcome::Declined);
        assert_eq!(kb.log, vec!["light A,B,C,D,E", "restore layout"]);
        assert_eq!(asked, vec![Question::Correct, Question::Order, Question::Order, Question::Order]);
        assert_eq!(store.writes, 0);
        // Enter at the order question leaves it too
        let mut kb = FakeKeyboard::new(five(), true);
        let (r, _, _) = play(&mut kb, &[line("n"), line("")], None);
        assert_eq!((r.unwrap(), kb.restores()), (Outcome::Declined, vec!["restore layout"]));
    }

    #[test]
    fn every_way_out_restores_exactly_once() {
        let cases: Vec<(Vec<Input>, Outcome)> = vec![
            (vec![line("y")], Outcome::Confirmed),
            (vec![line("n"), line("q")], Outcome::Declined),
            (vec![Input::Timeout], Outcome::TimedOut),
            (vec![Input::Interrupted], Outcome::Interrupted),
            (vec![Input::NotAsked], Outcome::Shown),
            (vec![line("n"), Input::Timeout], Outcome::TimedOut),
            (vec![line("n"), Input::Interrupted], Outcome::Interrupted),
            (vec![line("2 1 3 4 5"), Input::Timeout], Outcome::TimedOut),
            (vec![line("2 1 3 4 5"), Input::Interrupted], Outcome::Interrupted),
            (vec![line("2 1 3 4 5"), line("n"), line("quit")], Outcome::Declined),
        ];
        for serving in [true, false] {
            for (answers, outcome) in &cases {
                let mut kb = FakeKeyboard::new(five(), serving);
                let (r, _, _) = play(&mut kb, answers, None);
                assert_eq!(&r.unwrap(), outcome, "{answers:?}");
                assert_eq!(kb.restores(), vec![if serving { "restore layout" } else { "restore off" }], "{answers:?}");
                assert_eq!(kb.log.last().map(|l| l.starts_with("restore")), Some(true), "{answers:?}: nothing is lit after the restore");
            }
        }
    }

    #[test]
    fn errors_restore_too() {
        // lighting fails half way: some boards may be lit
        let mut kb = FakeKeyboard::new(five(), true);
        kb.fail_light = true;
        let (r, _, _) = play(&mut kb, &[], None);
        assert!(r.is_err());
        assert_eq!(kb.log, vec!["light A,B,C,D,E", "restore layout"]);
        // the keyboard disappears after the new order was applied
        let mut kb = FakeKeyboard::new(five(), false);
        kb.fail_rows_after = Some(1);
        let (r, store, _) = play(&mut kb, &[line("2 1 3 4 5")], None);
        assert!(r.is_err());
        assert_eq!(kb.restores(), vec!["restore off"]);
        assert_eq!(store.writes, 1);
        // the settings cannot be written
        struct ReadOnly;
        impl PinStore for ReadOnly {
            fn pins(&self) -> Vec<OpenxenBoardSettings> {
                Vec::new()
            }
            fn write(&mut self, _: &[OpenxenBoardSettings]) -> Result<()> {
                bail!("read-only")
            }
        }
        let mut kb = FakeKeyboard::new(five(), true);
        let mut script = Script::new(&[line("2 1 3 4 5")]);
        assert!(run(&mut kb, &mut script, &mut ReadOnly, None, true).is_err());
        assert_eq!(kb.log, vec!["light A,B,C,D,E", "restore layout"], "not applied, and restored");
    }

    #[test]
    fn an_order_given_with_the_command_is_applied_first_then_checked() {
        let mut kb = FakeKeyboard::new(five(), true);
        let (r, store, asked) = play(&mut kb, &[line("y")], Some("orange red green blue white"));
        assert_eq!(r.unwrap(), Outcome::Confirmed);
        assert_eq!(kb.log, vec!["apply B=0,A=1,C=2,D=3,E=4", "light B,A,C,D,E", "restore layout"]);
        assert_eq!((store.writes, asked), (1, vec![Question::Correct]));
        // wrong input with the command: an error, nothing lit, nothing written, no restore needed
        let mut kb = FakeKeyboard::new(five(), true);
        let (r, store, _) = play(&mut kb, &[], Some("orange red"));
        assert!(r.unwrap_err().to_string().contains("5 board(s) are connected, 2 named"));
        assert!(kb.log.is_empty());
        assert_eq!(store.writes, 0);
        // the order in use: nothing is written or applied, the check still runs
        let mut kb = FakeKeyboard::new(five(), true);
        let (r, store, _) = play(&mut kb, &[line("y")], Some("1 2 3 4 5"));
        assert_eq!(r.unwrap(), Outcome::Confirmed);
        assert_eq!((kb.count("apply"), store.writes, kb.restores().len()), (0, 0, 1));
    }

    #[test]
    fn a_serve_that_cannot_reorder_gets_the_settings_and_a_restart_notice() {
        // from the command line: written, not lit
        let mut kb = FakeKeyboard::new(five(), true);
        kb.restart_needed = true;
        let (r, store, asked) = play(&mut kb, &[], Some("2 1 3 4 5"));
        assert_eq!(r.unwrap(), Outcome::RestartNeeded);
        assert_eq!((store.writes, asked.len(), kb.restores().len()), (1, 0, 0));
        // from the dialogue: the lights are on and go away
        let mut kb = FakeKeyboard::new(five(), true);
        kb.restart_needed = true;
        let (r, store, _) = play(&mut kb, &[line("2 1 3 4 5")], None);
        assert_eq!(r.unwrap(), Outcome::RestartNeeded);
        assert_eq!((store.writes, kb.restores()), (1, vec!["restore layout"]));
    }

    #[test]
    fn without_lights_the_boards_are_listed_only() {
        let mut kb = FakeKeyboard::new(five(), true);
        let mut store = MemoryStore { pins: five_pins(), writes: 0 };
        let mut script = Script::new(&[]);
        assert_eq!(run(&mut kb, &mut script, &mut store, None, false).unwrap(), Outcome::Listed);
        assert!(kb.log.is_empty());
        // an order is still applied
        assert_eq!(run(&mut kb, &mut script, &mut store, Some(words("2 1 3 4 5")), false).unwrap(), Outcome::Listed);
        assert_eq!(kb.log, vec!["apply B=0,A=1,C=2,D=3,E=4"]);
    }

    #[test]
    fn a_fixed_order_keyboard_is_shown_but_not_reordered() {
        let rows = vec![Row { board: 0, serial: None, pinned: None }, Row { board: 1, serial: None, pinned: None }];
        let mut kb = FakeKeyboard::new(rows.clone(), true);
        let (r, store, asked) = play(&mut kb, &[line("n"), line("2 1"), line("q")], None);
        assert_eq!(r.unwrap(), Outcome::Declined);
        assert_eq!((store.writes, kb.count("apply"), kb.restores().len()), (0, 0, 1));
        assert_eq!(asked.len(), 3);
        let mut kb = FakeKeyboard::new(rows, true);
        let (r, _, _) = play(&mut kb, &[], Some("2 1"));
        assert!(r.unwrap_err().to_string().contains("cannot be changed"));
    }

    // --- against the local API of a serve (the hub of `api.rs`, with a stand-in serve loop) ---

    /// An API server and a thread that does what the openxen serve loop does with board
    /// commands. Returns the port and the overlay commands it saw.
    fn serve_stand_in(modules: Vec<(&str, u8, bool)>, reassign: bool) -> (u16, Receiver<crate::api::Command>) {
        use crate::api::{ApiHub, Command, LayoutCell, SetupModule, build_setup};
        let g = crate::geometry::geometry_by_name("lumatone", 1).unwrap();
        let cells = |boards: &[u8]| -> Vec<LayoutCell> {
            boards.iter().flat_map(|&b| (0u8..56).map(move |k| LayoutCell { board: b, key: k, pitch: k as i32, color: (1, 2, 3) })).collect()
        };
        let describe = move |modules: &[SetupModule]| {
            let boards: Vec<u8> = modules.iter().map(|m| m.board).collect();
            let mut s = build_setup("openxen", &g, "x", 31, &cells(&boards), &boards);
            s.modules = modules.to_vec();
            s.modules.sort_by_key(|m| m.board);
            s
        };
        let mut state: Vec<SetupModule> = modules.iter().map(|(s, b, p)| SetupModule { board: *b, serial: s.to_string(), pinned: *p }).collect();
        let (hub, commands) = ApiHub::new(describe(&state));
        if reassign {
            hub.allow_reassign(5);
        }
        let port = crate::api::spawn(hub.clone(), 0).unwrap();
        let (seen_tx, seen_rx) = crossbeam_channel::unbounded();
        std::thread::spawn(move || {
            while let Ok(cmd) = commands.recv() {
                if let Command::Boards { assign, pins } = &cmd {
                    let current: Vec<(String, u8)> = state.iter().map(|m| (m.serial.clone(), m.board)).collect();
                    let target = crate::api::reassigned(&current, assign, 5).unwrap();
                    for m in state.iter_mut() {
                        m.board = target.iter().find(|(s, _)| *s == m.serial).unwrap().1;
                        if let Some(pins) = pins {
                            m.pinned = pins.iter().any(|(s, b)| *s == m.serial && *b == m.board);
                        }
                    }
                    hub.set_setup(describe(&state));
                }
                let _ = seen_tx.send(cmd);
            }
        });
        (port, seen_rx)
    }

    #[test]
    fn through_the_api_boards_are_listed_lit_reordered_and_restored() {
        use crate::api::{Command, OverlayClear};
        let (port, seen) = serve_stand_in(vec![("AA", 0, true), ("BB", 1, false), ("CC", 3, true)], true);
        let mut kb = ApiKeyboard::connect(port, Path::new("no-such-settings.json"), Vec::new()).unwrap().expect("the API answers");
        assert!(kb.serving() && kb.notes().is_empty());
        let rows = kb.rows().unwrap();
        assert_eq!(rows, vec![row("AA", 0, true), row("BB", 1, false), row("CC", 3, true)]);

        kb.light(&rows).unwrap();
        let wait = Duration::from_secs(5);
        match seen.recv_timeout(wait).unwrap() {
            Command::Overlay { clear, set, dim_base } => {
                assert_eq!((clear, dim_base), (OverlayClear::All, None));
                assert_eq!(set.len(), 3 * 56);
                assert!(set.contains(&(0, 0, BOARD_COLOURS[0].1)) && set.contains(&(1, 55, BOARD_COLOURS[1].1)) && set.contains(&(3, 7, BOARD_COLOURS[3].1)));
            }
            other => panic!("expected the lights, got {other:?}"),
        }

        // the module seen leftmost is lit blue (board 3), then red, then orange
        let plan = make_plan(&rows, &words("blue red orange"), &[pin("AA", 0), pin("CC", 3)]).unwrap();
        assert_eq!(kb.apply(&plan.assign(), &plan.pins).unwrap(), Applied::Live);
        assert!(matches!(seen.recv_timeout(wait).unwrap(), Command::Boards { .. }));
        assert_eq!(kb.rows().unwrap(), vec![row("CC", 0, true), row("AA", 1, true), row("BB", 3, true)]);

        kb.restore(Restore::LayoutColours).unwrap();
        assert_eq!(seen.recv_timeout(wait).unwrap(), Command::Overlay { clear: OverlayClear::All, set: vec![], dim_base: Some(1.0) });
    }

    #[test]
    fn a_backend_with_a_fixed_order_is_listed_and_refuses_a_new_one() {
        let (port, _seen) = serve_stand_in(vec![("AA", 0, true), ("BB", 1, true)], false);
        let mut kb = ApiKeyboard::connect(port, Path::new("no-such-settings.json"), Vec::new()).unwrap().unwrap();
        assert!(kb.notes()[0].contains("fixed board order"));
        assert_eq!(kb.rows().unwrap().len(), 2);
        let err = kb.apply(&[("AA".into(), 1), ("BB".into(), 0)], &[]).unwrap_err();
        assert!(err.to_string().contains("501"), "{err}");
    }

    #[test]
    fn nothing_serving_is_not_an_error() {
        // a port nobody listens on
        let free = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        assert!(ApiKeyboard::connect(free, Path::new("no-such-settings.json"), Vec::new()).unwrap().is_none());
    }

    #[cfg(not(target_os = "android"))]
    #[test]
    fn modules_opened_directly_are_assigned_like_serve_does() {
        let serials: Vec<String> = ["A", "B", "C"].iter().map(|s| s.to_string()).collect();
        let rows = direct::assign_all(&serials, &[pin("B", 0), pin("X", 1)]);
        // B is pinned; A and C take the lowest boards that are free and not reserved
        assert_eq!(rows, vec![row("B", 0, true), row("A", 2, false), row("C", 3, false)]);
    }
}
