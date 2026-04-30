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

Top-level (cross-device or pure tooling):
- `src/main.rs` — thin dispatcher: parses CLI, routes `.wtn` → Wooting backend, `.xtn` → Exquis backend, holds the multi-device `cmd_list`/`cmd_geometries`/`cmd_geometry`.
- `src/cli.rs` — clap command definitions.
- `src/config.rs` — device config (devices.json) for multi-Exquis logical board names + board sync.
- `src/edit.rs` — web-based visual editor (rouille HTTP server, embedded HTML/JS via include_str!), edits .xtn / .wtn / .ltn.
- `src/geometry.rs` — hex grid tuples for Exquis/LTN/WTN and hex rotation math.
- `src/layouts.rs` — shared `.wtn` / `.xtn` discovery and cycle order.
- `src/logging.rs` — JSONL session logging.
- `src/mts.rs` — MTS-ESP FFI bindings via libloading (runtime-loaded LIBMTS.dll). Used by both backends.
- `src/settings.rs` — central settings.json (`exquis` section reserved; `wooting` section ports xenwooting defaults verbatim).
- `src/xtn.rs` — .xtn layout file parser (INI-style, compatible with .wtn/.ltn).

Exquis backend (`src/exquis/`):
- `commands.rs` — Exquis CLI command handlers (`cmd_dev`, `cmd_pads`, `cmd_pad_set`, `cmd_serve`, `cmd_new`, `cmd_load`, `cmd_control`, `cmd_highlight`, `cmd_midi`).
- `proto.rs` — Exquis SysEx message builders, Color type, zone bitmasks, control ID mapping.
- `midi.rs` — MIDI device discovery and I/O.
- `usb.rs` — USB enumeration for Exquis hardware.
- `mpe.rs` — MPE event decoding (pitch bend → X, CC74 → Y, pressure → Z).
- `tuning.rs` — pitch bend retuning engine (per-channel state, note remapping, bend injection).
- `ui.rs` — terminal UI for hybrid MIDI monitoring + serve (active-touches table, controls panel, scrolling event log).

Wooting backend (`src/wooting/`):
- `commands.rs` — Wooting CLI command handlers (`cmd_serve_wtn`, `cmd_load_wtn`, `cmd_new_wtn`, `list_wootings`).
- `serve.rs` — 1 kHz hot loop polling the Analog SDK, emitting MIDI + RGB + MTS-ESP. Time-critical: no I/O inside the loop.
- `ui.rs` — terminal UI for `serve` on Wooting (snapshots pushed every ~40 ms over a bounded crossbeam channel; runs on its own thread so the hot loop is never blocked).
- `hud_ctx.rs` — Wooting HUD ctx (mutable on layout cycle) and helpers that turn `KeyState::Held` into `LiveState`.
- `analog.rs`, `rgb.rs` — Wooting Analog and RGB SDK wrappers (libloading at runtime).
- `wtn.rs` — `.wtn` layout file parser.
- `hidmap.rs`, `geometry.rs`, `control_bar.rs`, `modes.rs` — HID-to-musical-key mapping, board geometry, control-bar key handling, velocity/aftertouch mode enums.

Live HUD (`src/hud/`):
- `state.rs` — unified `LiveState` wire shape (layout/mode/pressed/layout_pitches). Both backends submit it.
- `publisher.rs` — `HudPublisher` wrapping `ArcSwap<LiveState>`. `submit()` is wait-free (single atomic pointer swap, no JSON, no I/O — JSON serialization is deferred to the SSE handler thread).
- `server.rs` — opt-in HTTP/SSE server (rouille). Serves `/`, `/live.css`, `/live.js`, `/Bravura.otf`, `/api/live/state`, `/api/live/stream`. Decorates the live state with chord names from `chordnam.par`, note glyphs from xenharm, and OSC params/events from external programs.
- `chordnam.rs` — Rust port of xenwooting's `chords.js`. Parses Manuel Op de Coul's `assets/chordnam.par` (embedded via `include_bytes!`) into per-EDO step-pattern lookups; projects ratio/cents templates into the target EDO with a 15 c rounding cap.
- `xenharm.rs` — optional client for the bundled `xenharm_service/` Python sidecar. Probes `/health` at startup; resolves note glyphs on a worker thread; cache misses fall back to numeric labels at the frontend. Failure backoff (30 s) + capped one-line status surfaced in the SSE wire shape — no console spam on outage.
- `osc.rs` — UDP listener (default 9000). Accepts `/xentool/param/<group>/<name> <value> [<unit>]` (sticky params) and `/xentool/event <text>` (~5 s TTL). Used by `supercollider/mpe_tanpura_xentool.scd` to push synth state to the HUD's right-edge strip.
- `tui_url.rs` — small helper for the `h` shortcut in the TUIs: opens the HUD URL in the browser **and** copies it to the clipboard (`arboard`), pushing a single status line into the events log.

Frontend assets:
- `assets/editor.{html,css,js}` — frontend for the visual editor (vanilla JS, SVG hex rendering).
- `assets/live.{html,css,js}` — Live HUD frontend (vanilla JS, port of xenwooting's `LivePage.tsx` — black canvas, auto-fit centered text, four tap-cycle views, Bravura font, OSC parameter strip, xenharm-error footer).
- `assets/chordnam.par` — Scala chord-name database (~30 KB, 844 chords) embedded into the binary.
- `assets/Bravura.otf` — SMuFL music font (~512 KB) embedded into the binary so users see microtonal accidentals without a separate install.

External integrations / sidecars:
- `xenharm_service/` — Python 3.10+ HTTP service (xenharmlib) that maps `(edo, abs_pitch)` to short ASCII + Bravura PUA Unicode. Auto-detected by the HUD; optional.
- `supercollider/mpe_tanpura_xentool.scd` — SC patch that listens on the loopMIDI/Midi-Through port and renders the MPE input as a microtonal tanpura. Sends OSC parameter updates to xentool's HUD.
- `supercollider/midi_piano_xentool.scd` — SC patch for the Wooting/classic-MIDI flow. Multi-voice (piano / Hammond organ / Rhodes EP), with press-driven sustain drone and a CC74-driven Y-axis effect (LPF / tremolo / Leslie / chorus / vibrato).
- `supercollider/tanpura_studio/` — Flask + python-osc relay (HTTP 9100, OSC 57121) and vanilla-JS touchscreen UI that tweaks the tanpura SynthDef live. Save/Load presets land in `presets/preset_<ts>.json`; a singleton `presets/_default.json` is auto-loaded at startup and used by the Reset button. Independent from the HUD.
- `supercollider/piano_studio/` — sister of `tanpura_studio/` for the piano patch (HTTP 9101, OSC 57123). Same shape; piano-specific SECTIONS and conditional `showWhen` controls (e.g. piano-tone sliders only when voice=piano; Leslie speed sliders only when yMode=Leslie).

Wire-shape summary for both studios — UI → relay → sclang:

```
browser (HTTP 9100/9101)
   │  POST /api/set {name,value} | /api/batch | /api/save | /api/load | /api/reset
   │  GET  /api/state | /api/presets
   ▼
Flask relay (server.py)              ─── tracks `state` dict in memory
   │  fire-and-forget UDP to sclang
   │  /tanpura/set <name> <float>    (or /piano/set ...)
   │  /tanpura/batch <name1> <v1> ...
   │  /tanpura/reset                  (handlers also push reset to SC)
   ▼
sclang (UDP 57121 / 57123)          ─── OSCdef updates ~kparams + Synth.set
   │                                     on every currently-held voice
   ▼
scsynth                              ─── audio out via ~xxxReverbSynth → ~xxxMasterSynth
```

User-default flow: clicking "Make default" POSTs `/api/save-default`, which writes `presets/_default.json`. On next `python server.py`, `_effective_defaults()` overlays that file on the hardcoded DEFAULTS, and `/api/reset` always returns to the user default if it exists. "Factory reset" POSTs `/api/clear-default` (deletes the file) then `/api/reset` (which then falls through to factory).

Linux install scripts (`scripts/`):
- `install-linux-common.sh` — sourced by both wrappers; defines `install_linux_main` (apt deps, Rust toolchain, build, optional xenharm venv, optional SuperCollider, optional studio web UI, systemd user units, tmux-wrapped xentool service).
- `install-linux-exquis.sh` / `install-linux-wooting.sh` — thin wrappers; the Wooting one additionally installs the Wooting Analog + RGB SDKs (logic inlined from the former `install-wooting-sdks.sh`).
- The xentool service runs inside `tmux -L xentool` so the TUI lives in a real pty; users attach via `xentool-tui` (a one-line wrapper installed at `~/.local/bin/`).
- `setup_studio()` is backend-aware: Exquis → tanpura_studio (HTTP 9100), Wooting → piano_studio (HTTP 9101). Reuses the xenharm venv when present (saves disk) and writes a `xentool-studio.service` unit that starts after `xentool-supercollider.service`.

Windows launchers (`scripts/`):
- `run-all-exquis.bat` / `run-all-wooting.bat` — open a single Windows Terminal window with up to 4 tabs (xentool, xenharm, supercollider, studio). The 4th tab is conditional on `STUDIO_SCRIPT` being set; exquis sets it to `_run-all-studio.bat` (tanpura), wooting sets it to `_run-all-piano-studio.bat`. Falls back to detached cmd windows when wt isn't installed.

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
- `09` — Snapshot (262 bytes: 17 header + 61×4 pad data + F7). Header prefix verified against fw 3.0.0 via GET-snapshot: device defaults to `00 01 01 0E 00 00 01 01 00 00 00`. PitchGridRack's `exquis.hpp:282` ships `00 01 00 0E ...` (older firmware); on fw 3.0.0 the byte-2 difference silently kills MPE per-note pitch bend (X axis) until the device is reset. We override byte 9 (`PBRange`, /48 of synth's bend range) from the default `0x0E` to `0x30` (= 48/48, max) so the player gets the Exquis's full X-slide output. Combined with `--pb-range 16` this yields ±1600 c at the synth; <2 % slide clipping in 31-EDO at the worst pads.

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

Key constraint: the synth's per-note pitch bend range must match `--pb-range` (default 16 semitones = ±1600 c). Set Pianoteq's MIDI per-note PB to ±1600 c, or pass `--pb-range 2` for Pianoteq's default and accept a weaker X-axis expression.

### MTS-ESP (`xentool serve --mts-esp`)
Registers as MTS-ESP master via runtime-loaded `LIBMTS.dll` (at `C:\Program Files\Common Files\MTS-ESP\LIBMTS.dll`). Broadcasts a global 128-note tuning table. Limited to one master, one table, max 128 unique notes.

## Multi-Exquis device config

`devices.json` at `%LOCALAPPDATA%\xentool\config\` maps logical board names to USB serial numbers. Auto-created/updated by `sync_boards()` on every `load`/`serve` command. Board sections in `.xtn` files are matched to physical devices via this config. All connected devices are always assigned to board0..boardN; the config is only a hint for preferred ordering.
