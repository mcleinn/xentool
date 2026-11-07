//! openxen backend (experimental).
//!
//! openxen is a 5-module Hall-effect hex keyboard (56 keys per module, Lumatone key order).
//! Each module streams one HID report per complete key scan (~4 kHz) carrying a device
//! timestamp, a sequence number, all 56 raw ADC values and a rotating calibration slice —
//! see `proto.rs` and the openxen repo's `docs/stream-protocol.md`.
//!
//! Structure mirrors the Wooting backend:
//! - `proto.rs`   — report parser + per-module decoder (unwrapping, losses, calibration, travel)
//! - `hid.rs`     — module discovery and one reader thread per module (hidapi), frames to the
//!   hot loop over a bounded crossbeam channel
//! - `keys.rs`    — per-key state machine (Idle → Tracking → Held), velocity from device
//!   timestamps; pure (returns MIDI actions), unit-tested
//! - `serve.rs`   — the 1 kHz hot loop: drains frames, runs the key machines, emits MIDI,
//!   publishes MTS-ESP, HUD and TUI snapshots (TUI and HUD helpers shared with Wooting)
//! - `commands.rs` — CLI handlers (`serve` for `.ltn`, module listing)
//!
//! Layouts are Lumatone-style `.ltn` files (`[Board0..4]`, `Key_n`/`Chan_n`/`Col_n` for
//! n = 0..55, plus xentool's `Edo=` / `PitchOffset=` header), parsed with the `.wtn` parser.
//! LEDs are not driven yet (firmware support pending).

pub mod commands;
pub mod hid;
pub mod keys;
pub mod proto;
pub mod serve;
