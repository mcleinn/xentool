# xentool

`xentool` is a Rust CLI for the Intuitive Instruments Exquis MPE controller and for Wooting analog keyboards (xenwooting use case), targeting Windows and Linux systems including Ubuntu-class environments and Raspberry Pi setups such as Patchbox OS.

It targets the current Exquis developer API documented in the official 2025 Developer Mode MIDI specification and is intended for firmware `3.0.0` and newer official behavior. Wooting support is delivered through the Wooting Analog and RGB SDKs (loaded at runtime — see `scripts/install-wooting-sdks.*`).

## Features

- `xentool help` and standard `--help`
- `xentool list` to enumerate connected Exquis MIDI ports (and Wooting keyboards when the SDK is installed)
- `xentool midi` for live MPE monitoring with a compact terminal UI
- `xentool dev on|off` for explicit developer mode control
- `xentool pads clear`, `xentool pads fill <color>`, `xentool pad <pad> <color>` for MPE-safe LED control
- `xentool load <file.xtn>` to load per-pad colors from .xtn layout files (multi-board)
- `xentool edit <file.xtn>` to open a web-based visual editor for the layout
- `xentool control <name> <color>` for non-pad LED control (encoders, buttons, slider)
- `xentool highlight <note>` for note highlighting via channel 1
- multi-Exquis support via logical board names in `devices.json`
- automatic JSONL logging during `xentool midi`
- friendly names for documented Exquis buttons and encoders on developer-mode channel events

## LED color strategy — snapshot approach

The Exquis developer mode creates a fundamental conflict: taking over the pad zone gives full RGB LED control but **disables MPE output** (pitch bend, CC74, aftertouch). This tool solves it using the **snapshot technique** discovered in the [PitchGridRack](https://github.com/peterjungx/PitchGridRack) project:

1. Enter developer mode for all zones **except pads** (mask `0x3A`)
2. Send a Snapshot command (`09h`) that encodes per-pad MIDI note mappings and RGB colors in a single 262-byte SysEx message
3. Pads remain in normal mode — **MPE X/Y/Z output is fully preserved**

All pad color commands (`pad`, `pads fill`, `pads clear`, `pads test`) use this approach by default. Pass `--legacy` to fall back to direct dev-mode takeover (which disables MPE).

### Snapshot message format

```
F0 00 21 7E 7F 09          — SysEx header + Snapshot command
00 01 00 0E 00 00 01 01 00 00 00  — 11-byte config header
[midinote r g b] × 61      — per-pad note + RGB (244 bytes)
F7                          — SysEx end
```

Total: 262 bytes. Each pad gets 4 bytes: MIDI note number (default 36+pad_id) and RGB color (0-127 per channel).

## API policy

- Uses SysEx `F0 00 21 7E 7F ... F7` commands for developer mode and LED control
- Default color commands use the snapshot technique to preserve MPE
- Pass `--legacy` on color commands to use direct dev-mode pad takeover (disables MPE)

## Build

```powershell
cargo build
```

## Commands

### Help

```powershell
xentool help
xentool help midi
xentool --help
```

### List devices

```powershell
xentool list
```

Example output:

```text
[1] Exquis
  id: usb:2fe3:0100:bus-001/ports-3/addr-002
  usb: 2fe3:0100
  manufacturer: Intuitive Instruments
  location: bus-001/ports-3/addr-002
  firmware: unavailable
  midi-in: Exquis
  midi-out: Exquis
```

`list` now reports best-effort cross-platform USB metadata in addition to MIDI endpoints:

- stable unique ID based on serial number when available, otherwise VID:PID plus bus/port location
- vendor and product IDs
- manufacturer and serial number when exposed by USB
- bus/port location on Windows and Linux
- firmware version only when a documented host query exists; currently shown as `unavailable`

### Monitor MIDI / MPE

```powershell
xentool midi
xentool midi --mode stream
xentool midi --mode stream --mpe-only
xentool midi --mode raw --log-raw
xentool midi --device 1 --log-file C:\temp\xentool-session.jsonl
```

Default mode is `hybrid`:

- top panel shows active touches with live `X/Y/Z`
- bottom panel shows a compact event stream
- press `q` to quit the terminal UI

The `stream` mode prints discrete events line by line. The `raw` mode prints raw MIDI bytes.

Use `--mpe-only` when you want to compare normal mode vs developer mode pad behavior. It filters the output down to MPE note and `X/Y/Z` events:

- `note_on` / `note_off`
- `x` as pitch bend
- `y` as `CC74`
- `z` as channel pressure or poly aftertouch

### Developer mode

```powershell
xentool dev on
xentool dev on --zone pads,encoders,slider,up-down,other-buttons
xentool dev off
```

Default `dev on` zones are:

- pads
- encoders
- slider
- up/down buttons
- other buttons

This intentionally avoids taking over the settings/sound buttons by default.

### Pad colors

```powershell
xentool pads fill amber
xentool pads fill 255,32,0 --device 1
xentool pads clear
xentool pads test
xentool pad 17 blue
xentool pad 17 0,127,0 --device 2
```

All color commands use the MPE-safe snapshot approach by default. MPE pitch bend, CC74, and aftertouch continue to work normally while custom colors are displayed.

Use `--legacy` to fall back to direct dev-mode pad takeover (full LED control but no MPE):

```powershell
xentool pads fill amber --legacy
xentool pad 17 blue --legacy
```

### Load .xtn layout

```powershell
xentool load my_layout.xtn
```

Loads an `.xtn` layout file (INI-style, compatible with xenwooting `.wtn` / Lumatone `.ltn`) and applies per-pad colors to connected Exquis boards. Each `[BoardN]` section maps to a logical device name configured in `devices.json`.

Example `.xtn` file:

```ini
Edo=31
PitchOffset=0

[Board0]
Key_0=0
Chan_0=1
Col_0=FFDD00
Key_1=1
Chan_1=1
Col_1=7981EC

[Board1]
Key_0=61
Chan_0=2
Col_0=E26ABC
```

- `Edo=N` — steps per octave (e.g. 31 for 31-EDO). Required for `serve` tuning.
- `PitchOffset=M` — optional pitch offset in EDO steps (default 0)
- `Key_N` — virtual MIDI note for frequency calculation (not sent to Exquis)
- `Chan_N` — virtual MIDI channel for frequency calculation (not sent to Exquis)
- `Col_N` — hex RGB color (8-bit, scaled to 7-bit for Exquis)

`Key_N` and `Chan_N` encode the abstract pitch in the EDO tuning system. The `serve` command uses them to calculate microtonal frequencies: `virtual_pitch = (Chan - 1) * Edo + Key + PitchOffset`. These values are never sent to the Exquis — pads always send their pad ID as the MIDI note.

If only one board section and one Exquis is connected, auto-matches without config.

### Edit — visual tuning editor

```powershell
xentool edit xtn/edo31.xtn
xentool edit xtn/edo31.xtn --port 8088
xentool edit xtn/edo31.xtn --no-open
```

Opens a web-based editor in your default browser. Each connected/configured board is rendered as a hex panel with 61 pads in the correct 6-5-6-5-…-6 layout. Click a pad to select (Shift/Ctrl for multi-select); edit Key/Chan/Color in the sidebar; Save writes back to the .xtn file.

Import:
- Click Import, pick a `.ltn` (Lumatone), `.wtn` (xenwooting), or another `.xtn`
- Use arrow keys to translate, `R` to rotate 60° around the hovered pad
- Enter applies the overlay to matching Exquis pads; Esc cancels

Colors are preserved verbatim through the round-trip (no 8-bit ↔ 7-bit lossy scaling during edit).

### Serve — microtonal tuning server

```powershell
xentool serve xtn/edo31.xtn
xentool serve xtn/edo31.xtn --pb-range 48
xentool serve xtn/edo31.xtn --output "My MIDI Port"
xentool serve xtn/edo31.xtn --mts-esp
```

Loads an `.xtn` layout, sets pad colors on connected boards, and runs a live microtonal tuning server with a terminal UI showing active touches and tuning status.

**Default mode: pitch bend retuning.** The serve command intercepts MIDI from each Exquis, remaps note numbers and injects per-channel pitch bends to shift each note to its exact microtonal frequency, then forwards the retuned MIDI to a virtual output port. This preserves full MPE expression (X/Y/Z) while adding microtonal tuning.

Requirements for pitch bend mode:
- Install [loopMIDI](https://www.tobias-erichsen.de/software/loopmidi.html) and create a port named "loopMIDI Port" (the default)
- In your synth (e.g. Pianoteq), disable the direct "Exquis" MIDI input and enable "loopMIDI Port" instead
- The synth's per-note pitch bend range must match `--pb-range` (default: 2 semitones, which is Pianoteq's default)

How pitch bend retuning works:
1. For each pad, the target frequency is computed from the .xtn's `Key_N`/`Chan_N` values and the `Edo` setting
2. The nearest 12-TET MIDI note is found, and the pitch bend offset needed to reach the exact microtonal frequency is calculated
3. On each note_on, a pitch bend message is injected before the note_on on the same MIDI channel
4. When the player uses the X axis (pitch bend expression), the player's bend is added to the tuning offset
5. All other MPE data (Y=CC74, Z=pressure) passes through unchanged

Multi-board support: each board gets its own tuning state. With separate loopMIDI ports or a single shared port, each board's pads are independently retuned. Scales to 4+ boards.

**Alternative: MTS-ESP mode (`--mts-esp`).** Registers as an MTS-ESP master and broadcasts a global 128-note tuning table. The synth must be an MTS-ESP client (Pianoteq supports this). Limitations: only one master allowed, one global tuning table shared by all clients, max 128 unique notes (2 boards).

### Device configuration

For multi-Exquis setups, create `devices.json` at `%LOCALAPPDATA%\xentool\config\devices.json`:

```json
{
  "devices": {
    "board0": { "serial": "ABC123" },
    "board1": { "serial": "DEF456" }
  }
}
```

Serial numbers are shown by `xentool list`. Board names in `.xtn` files are matched to these logical names.

### Non-pad LED control

```powershell
xentool control settings red
xentool control encoder-1 blue
xentool control slider-1 green
xentool control 110 cyan         # raw control ID
```

Sets the LED color of encoders, buttons, and slider portions. Accepts named controls or raw numeric IDs (see `xentool help control`).

### Note highlighting

```powershell
xentool highlight 60        # highlight middle C (green)
xentool highlight 60 0      # turn off highlight
```

Sends Note On/Off on MIDI channel 1. Works independently of developer mode. Currently produces green highlights only (firmware-defined).

Named colors currently include:

- `black`
- `red`
- `green`
- `blue`
- `amber`
- `yellow`
- `cyan`
- `magenta`
- `white`
- `orange`
- `purple`

RGB values can be entered as `r,g,b`. Values above `127` are scaled from `0..255` into the Exquis `0..127` MIDI range.

## Logging

`xentool midi` logs automatically to JSONL unless `--no-log` is passed.

Default location:

- `%LOCALAPPDATA%\xentool\logs\...` if available via Windows app data lookup
- otherwise `logs\...` inside the current working directory

Each record includes timestamp, device number, port name, channel, event kind, note/value fields, and optional raw bytes.

Example JSONL lines:

```json
{"ts":"2026-04-19T12:34:56Z","device":1,"port":"Exquis 1","channel":3,"kind":"note_on","note":64,"value":92,"label":null,"raw":null}
{"ts":"2026-04-19T12:34:56Z","device":1,"port":"Exquis 1","channel":16,"kind":"control","note":null,"value":127,"label":"play_stop","raw":null}
```

## MPE details surfaced by `xentool midi`

The current Exquis user guide documents:

- `X` as Pitch Bend
- `Y` as `CC74`
- `Z` as Channel Pressure or Polyphonic Aftertouch

The hybrid UI updates those values in place for active touches instead of printing a new line for every pressure or tilt change.

## Friendly control names

When developer-mode channel 16 events match documented control identifiers, `xentool midi` shows names like:

- `Settings`
- `Play/Stop`
- `Up`
- `Down`
- `Encoder 1`
- `Encoder 1 Button`

Unknown identifiers fall back to raw numeric output.

## Installation

```powershell
cargo install --path .
```

This installs `xentool.exe` to `~/.cargo/bin/` (in PATH). Run `cargo install --path .` again after code changes to update.
