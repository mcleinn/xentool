//! Exquis device backend.
//!
//! Hosts the Intuitive Instruments Exquis-specific code: SysEx protocol,
//! USB/MIDI port discovery, MPE event decoding, pitch-bend retuning, and the
//! Exquis serve / monitor terminal UI. Top-level modules (`xtn`, `geometry`,
//! `edit`, `layouts`, `mts`, `settings`, `config`, `logging`, `cli`) hold
//! cross-device or pure-tooling concerns shared with the Wooting backend.

pub mod commands;
pub mod midi;
pub mod mpe;
pub mod proto;
pub mod tuning;
pub mod ui;
pub mod usb;
