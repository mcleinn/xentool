//! Live HUD: opt-in HTTP server that streams currently-played notes to a
//! browser-based view.
//!
//! Both the Exquis and Wooting backends publish the same [`state::LiveState`]
//! to a [`publisher::HudPublisher`]. The publisher is a lock-free wrapper
//! around an atomic pointer swap so the audio path never blocks on JSON or
//! I/O — JSON encoding is deferred to the SSE handler thread.
//!
//! Enable with `xentool serve --hud`. See `src/hud/server.rs` (T2) for the
//! HTTP endpoints.

pub mod chordnam;
pub mod osc;
pub mod publisher;
pub mod server;
pub mod state;
pub mod tui_url;
pub mod xenharm;

pub use publisher::HudPublisher;
pub use state::{LayoutInfo, LiveState, ModeInfo, SCHEMA_VERSION};

/// File stem (no directory, no extension) for use as a HUD layout id, e.g.
/// `"edo31"` for `xtn/edo31.xtn`. Falls back to the full path string if the
/// path has no stem.
pub fn layout_id_from_path(p: &std::path::Path) -> String {
    p.file_stem()
        .and_then(|s| s.to_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| p.display().to_string())
}
