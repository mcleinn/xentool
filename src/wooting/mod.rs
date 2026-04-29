//! Wooting keyboard backend.
//!
//! Handles `.wtn` layout files, loading LEDs via the Wooting RGB SDK, and the
//! serve loop that polls the Wooting Analog SDK for per-key analog depths and
//! emits MIDI with MTS-ESP tuning.
//!
//! Both SDKs are loaded at runtime via `libloading` — no compile-time link
//! dependency on Wooting-specific Rust crates.

pub mod analog;
pub mod commands;
pub mod control_bar;
pub mod geometry;
pub mod hidmap;
pub mod hud_ctx;
pub mod modes;
pub mod rgb;
pub mod serve;
pub mod ui;
pub mod wtn;
