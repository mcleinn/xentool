# CLAUDE.md

## Build & test

```bash
cargo check        # type-check
cargo test         # run unit tests
cargo build        # debug build
cargo run -- help  # run the CLI
```

## Architecture

This is a Rust CLI (`src/main.rs`, binary `xentool`) for the Intuitive Instruments Exquis MPE controller and for Wooting analog keyboards (xenwooting use case).

Key modules:
- `src/exquis_proto.rs` — Exquis SysEx message builders, Color type, zone bitmasks, control ID mapping
- `src/cli.rs` — clap command definitions
- `src/main.rs` — command handlers
- `src/midi.rs` — MIDI device discovery and I/O (Exquis backend)
- `src/mpe.rs` — MPE event decoding (pitch bend → X, CC74 → Y, pressure → Z)
- `src/ui.rs` — terminal UI for hybrid MIDI monitoring
- `src/logging.rs` — JSONL session logging
- `src/xtn.rs` — .xtn layout file parser (INI-style, compatible with .wtn/.ltn)
- `src/config.rs` — device config (devices.json) for multi-Exquis logical board names + board sync
- `src/tuning.rs` — pitch bend retuning engine (per-channel state, note remapping, bend injection)
- `src/mts.rs` — MTS-ESP FFI bindings via libloading (runtime-loaded LIBMTS.dll)
- `src/geometry.rs` — hex grid tuples for Exquis/LTN/WTN and hex rotation math
- `src/edit.rs` — web-based visual editor (rouille HTTP server, embedded HTML/JS via include_str!)
- `src/wooting/` — Wooting backend (analog SDK polling, RGB SDK, .wtn parsing, MTS-ESP serve)
- `src/settings.rs` — central settings.json (`exquis` section reserved; `wooting` section ports xenwooting defaults verbatim)
- `src/layouts.rs` — shared `.wtn` / `.xtn` discovery and cycle order
- `assets/editor.{html,css,js}` — frontend for the editor (vanilla JS, SVG hex rendering)

## Critical design decision: snapshot-based LED control

**Problem:** The Exquis developer mode API requires taking over the pad zone (`0x01`) to set LED colors via SysEx cmd `04`. But taking over pads disables normal MPE output (no pitch bend, CC74, or aftertouch). Colors set via dev mode also reset instantly when dev mode exits.

**Solution:** Use the Snapshot command (`09h`) instead. Enter dev mode for non-pad zones only (mask `0x3A`), then send a 262-byte snapshot SysEx that encodes both MIDI note mappings and RGB colors for all 61 pads. Pads stay in normal mode, so MPE is preserved.

This technique was discovered by studying [PitchGridRack](https://github.com/peterjungx/PitchGridRack) by peterjungx, which uses the same approach (`exquis.hpp:sendCustomSnapshotMessage`).

**All pad color commands must use the snapshot approach by default.** The `--legacy` flag exists for direct dev-mode takeover when MPE is not needed.

### What does NOT work (tested on FW 3.0.0)

- SysEx cmd `04` for pad LEDs when only non-pad zones are in dev mode — firmware rejects it
- Palette + CC on ch16 for pad LEDs without pad zone takeover — no effect
- Colors set via dev mode takeover do NOT persist after `dev off` — reset in <1 second
- Channel 1 Note On highlighting works but only produces green (velocity does not change color)

## SysEx reference

All messages: `F0 00 21 7E 7F [cmd] [data...] F7`

- `00` — Setup dev mode (mask byte: `01`=pads, `02`=encoders, `04`=slider, `08`=up/down, `10`=settings, `20`=other)
- `04` — Set LED color (requires zone to be taken over)
- `09` — Snapshot (262 bytes: 17 header + 61×4 pad data + F7)

Dev mask `0x3A` = everything except pads and slider. This is the default for color commands.

## .xtn layout files

INI-style format with `[Board0]`/`[Board1]` sections. Per pad: `Key_N`, `Chan_N`, `Col_N`.
- Header fields `Edo=N` and `PitchOffset=M` go before the first `[Board]` section
- `Key_N` and `Chan_N` encode the abstract EDO pitch for frequency calculation: `virtual_pitch = (Chan-1)*Edo + Key + PitchOffset`
- `Col_N` is a 6-char hex RGB (8-bit, scaled to 7-bit for Exquis SysEx)
- MIDI notes in snapshots always equal the pad ID (pad 0 → note 0)
- Format is compatible with xenwooting `.wtn` and Lumatone `.ltn` files

## Microtonal tuning — two modes

### Pitch bend retuning (default, `xentool serve`)
Intercepts MIDI from Exquis, remaps note numbers to nearest 12-TET, injects per-channel pitch bend to reach exact microtonal frequency, forwards to a virtual MIDI port (loopMIDI). MPE expression (X/Y/Z) is preserved — player X bends are added to the tuning offset. Each board has independent tuning state. Scales to 4+ boards.

Key constraint: the synth's pitch bend range must match `--pb-range` (default 2 semitones = Pianoteq default).

### MTS-ESP (`xentool serve --mts-esp`)
Registers as MTS-ESP master via runtime-loaded `LIBMTS.dll` (at `C:\Program Files\Common Files\MTS-ESP\LIBMTS.dll`). Broadcasts a global 128-note tuning table. Limited to one master, one table, max 128 unique notes.

## Multi-Exquis device config

`devices.json` at `%LOCALAPPDATA%\xentool\config\` maps logical board names to USB serial numbers. Auto-created/updated by `sync_boards()` on every `load`/`serve` command. Board sections in `.xtn` files are matched to physical devices via this config. All connected devices are always assigned to board0..boardN; the config is only a hint for preferred ordering.
