//! Tiny helpers used by the Exquis and Wooting TUIs for the HUD-URL `h`
//! shortcut.
//!
//! Pressing `h` in either TUI copies the HUD URL to the clipboard and
//! attempts to open it in the system's default browser. Both actions are
//! independent — clipboard copy succeeds even on console-only hosts where
//! `open::that` can't find a display. A single one-line status is pushed
//! into the TUI's events log so the user knows what happened, never grows.

/// Attempt to open `url` in the system's default browser. Returns `Ok` only
/// when the spawn succeeded; on failure returns a short human-readable
/// error suitable for a single status line.
pub fn try_open_browser(url: &str) -> Result<(), String> {
    match open::that(url) {
        Ok(()) => Ok(()),
        Err(e) => Err(e.to_string()),
    }
}

/// Cross-platform clipboard write. Uses `arboard` (same crate as `mrs`).
pub fn copy_to_clipboard(text: &str) -> Result<(), String> {
    let mut cb = arboard::Clipboard::new().map_err(|e| e.to_string())?;
    cb.set_text(text.to_string()).map_err(|e| e.to_string())
}

/// Convenience: do both at once and return a single-line status describing
/// the outcome, suitable for pushing into a TUI events buffer.
///
/// Examples:
/// - "HUD: opened in browser; URL copied"
/// - "HUD: URL copied (open failed: <reason>)"
/// - "HUD: opened in browser (clipboard error: <reason>)"
/// - "HUD: open failed; clipboard error"
pub fn copy_and_open(url: &str) -> String {
    let copied = copy_to_clipboard(url);
    let opened = try_open_browser(url);
    match (opened, copied) {
        (Ok(()), Ok(())) => format!("HUD: opened {url} in browser; URL copied"),
        (Ok(()), Err(e)) => format!("HUD: opened {url} in browser (clipboard error: {e})"),
        (Err(e), Ok(())) => format!("HUD: URL copied to clipboard (open failed: {e}). URL: {url}"),
        (Err(o), Err(c)) => format!("HUD: open failed: {o}; clipboard error: {c}. URL: {url}"),
    }
}
