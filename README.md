# xentool

`xentool` is a Rust CLI for two microtonal MIDI controllers, treated as
co-equal backends:

- **Exquis backend** — Intuitive Instruments Exquis MPE controller (61 hex pads
  per board, multi-board), driven over USB MIDI / SysEx.
- **Wooting backend** — Wooting analog keyboards (xenwooting use case),
  driven via the Wooting Analog and RGB SDKs at runtime.

Targets Windows and Linux including Ubuntu-class environments and Raspberry
Pi setups such as Patchbox OS. Exquis support targets the official 2025
Developer Mode MIDI specification (firmware `3.0.0`+); Wooting support is
delivered through the Wooting Analog and RGB SDKs (loaded at runtime — see
`scripts/install-wooting-sdks.*`).

## File-extension routing

A single subcommand handles both backends — the file extension picks the
backend:

| Extension | Backend | Layout format          |
| --------- | ------- | ---------------------- |
| `.xtn`    | Exquis  | xentool-native (INI)   |
| `.wtn`    | Wooting | xenwooting (INI)       |
| `.ltn`    | import  | Lumatone (read-only via `edit` import) |

So `xentool serve foo.xtn` runs the Exquis serve loop and `xentool serve
foo.wtn` runs the Wooting serve loop. Same for `load` / `new`.

## Features at a glance

**Both backends**
- `xentool list` enumerates connected Exquis MIDI ports and Wooting keyboards
- `xentool edit <file>` opens a web-based visual editor for `.xtn` / `.wtn` / `.ltn`
- `xentool geometries` / `xentool geometry <name>` describe / render hex-grid layouts
- Multi-board, microtonal tuning via MTS-ESP master
- Persistent "last layout" memory across runs (`settings.json`)
- Optional **Live HUD** during `serve` — text-only browser view of currently-played
  notes / chord names with auto-fit Bravura glyphs (see [Live HUD](#live-hud))

**Exquis-only**
- `xentool midi` — live MPE monitor (terminal UI showing X/Y/Z + event log)
- `xentool dev on|off` — explicit developer mode control
- `xentool pads clear|fill|test`, `xentool pad <pad> <color>` — MPE-safe LED control via the snapshot technique
- `xentool control <name> <color>` — non-pad LED control (encoders, buttons, slider)
- `xentool highlight <note>` — note highlighting via channel 1
- `xentool serve <file.xtn>` — pitch-bend retuning *or* MTS-ESP
- multi-Exquis support via logical board names in `devices.json`
- friendly names for documented Exquis buttons and encoders
- automatic JSONL logging during `xentool midi`

**Wooting-only**
- `xentool serve <file.wtn>` — 1 kHz analog-to-MIDI with MTS-ESP, terminal UI
- `xentool load <file.wtn>` — paint per-key LEDs from a `.wtn` layout
- `xentool new <file.wtn>` — create a blank `.wtn` for a given EDO
- live in-keyboard controls during `serve`: octave hold, aftertouch mode, velocity profile, pitch bend, layout cycle (see [Wooting serve](#serve--analog-to-midi-with-mts-esp))

## Installation

### Linux (Patchbox OS, Raspberry Pi OS, Ubuntu on Pi 4/5)

Pick the script that matches your hardware. Each one is **interactive** —
it shows the plan up front, prompts before installing system packages, and
asks separately whether you want the [xenharm sidecar](#optional-xenharm-note-glyphs)
(microtonal note glyphs) and the bundled SuperCollider tanpura synth.

```bash
# Exquis MPE controller:
bash scripts/install-linux-exquis.sh

# Wooting analog keyboard (also installs the Wooting Analog + RGB SDKs):
bash scripts/install-linux-wooting.sh
```

Either script will:

1. `apt install` the build prerequisites (build-essential, ALSA + USB dev
   headers, `tmux` for the TUI service, etc.).
2. Install Rust via rustup if `cargo` isn't on `PATH` already.
3. Build and install `xentool` to `~/.cargo/bin/`.
4. (Wooting only) install the Wooting Analog + RGB SDKs to `/usr/local/lib`.
5. (Optional, prompted) set up the **xenharm** Python sidecar in a per-user
   venv (`~/.local/share/xentool/venv`) — required only for Bravura SMuFL
   note glyphs in the HUD; without it, the HUD falls back to numeric
   labels.
6. (Optional, prompted) install **SuperCollider** + `sc3-plugins` and the
   matching bundled patch — `supercollider/mpe_tanpura_xentool.scd` for
   the Exquis MPE flow, `supercollider/midi_piano_xentool.scd` for the
   Wooting classic-MIDI flow.
7. Write `systemd --user` units (`xenharm`, `xentool`, optionally
   `xentool-supercollider`), enable user-linger, and start them in the
   right order.

After install:

| What you want to do                             | Command                                          |
|-------------------------------------------------|--------------------------------------------------|
| Open the **Live HUD**                           | <http://localhost:9099/> (also LAN-reachable)    |
| Attach to xentool's **TUI** (detach: `Ctrl-b d`)| `xentool-tui` (`~/.local/bin/xentool-tui`)       |
| Tail logs                                       | `journalctl --user -u xentool -f`                |
| Manage the service                              | `systemctl --user {status,restart,stop} xentool` |

The xentool serve loop runs under `tmux` (socket label `xentool`) so the
TUI lives in a real pty even though it's a background service. `xentool-tui`
is just a one-line wrapper around `tmux -L xentool attach -t xentool`.

### Windows

```powershell
# build + install:
cargo install --path .

# Wooting SDKs (only needed for the Wooting backend):
powershell -ExecutionPolicy Bypass -File scripts\install-wooting-sdks.ps1
```

Or run `scripts\install.bat` for the equivalent. xentool is a CLI on Windows
— there are no systemd units; start it from a terminal:

```powershell
xentool serve --hud
```

#### One-click full stack (Windows Terminal)

To bring up xentool, the xenharm sidecar and the matching SuperCollider
synth in **one** Windows Terminal window — three labelled tabs in
xentool / xenharm / supercollider order, started with the right delays
between them — pick the script for your hardware and double-click:

| Hardware | Script                          | Default layout    | SC patch                       |
|----------|---------------------------------|-------------------|--------------------------------|
| Exquis   | `scripts\run-all-exquis.bat`    | `xtn\edo31.xtn`   | `mpe_tanpura_xentool.scd`      |
| Wooting  | `scripts\run-all-wooting.bat`   | `wtn\edo31.wtn`   | `midi_piano_xentool.scd`       |

The two SC patches differ on purpose: the Exquis pads emit MPE
(per-note pitch bend, CC74, channel pressure), so the Exquis stack
runs the **MPE tanpura** sketch that maps X/Y/Z onto string-pluck
parameters. The Wooting keys emit classic 12/N-EDO MIDI on Lumatone-
style channel-stripes, so the Wooting stack runs the **classic-MIDI
piano** sketch instead.

Each script:

1. Always opens a **new** Windows Terminal window (your existing wt
   sessions are untouched).
2. Locates `wt.exe` via PATH → `%LOCALAPPDATA%\Microsoft\WindowsApps`
   → `%ProgramFiles%\WindowsApps\Microsoft.WindowsTerminal_*` →
   PowerShell `Get-Command` (so it works even when the App Execution
   Alias isn't on `cmd`'s PATH). If wt isn't found anywhere, falls
   back to three separate `cmd.exe` windows.
3. Starts xenharm immediately, xentool ~2 s later (so the HUD's
   `/health` probe sees a bound xenharm), supercollider ~5 s later
   (so `MIDIClient.init` sees xentool's MIDI port and the OSC strip
   doesn't fire into a closed UDP socket).
4. Pins the layout and the SC patch up front so xentool loads the
   right backend regardless of what `settings.json` last used. Set
   `LAYOUT` and / or `SC_SCRIPT` before calling either script to
   override:

   ```powershell
   set LAYOUT=C:\Dev-Free\xentool\xtn\edo24.xtn
   set SC_SCRIPT=mpe_tanpura_xentool.scd
   scripts\run-all-exquis.bat
   ```

5. The Wooting frontend additionally sets
   `XENTOOL_EXTRA_ARGS=--tune-supercollider` (see below). The Exquis
   frontend does not — Exquis routes through MPE pitch-bend retuning,
   so SC's standard MIDI handling already produces the correct
   microtonal pitch.

To stop a single subsystem, focus its tab and press Ctrl-C (or close
the tab). To stop everything, close the Windows Terminal window.

#### `--tune-supercollider` (tuning broadcast for SC)

SuperCollider doesn't ship with an MTS-ESP client, so on the **Wooting**
flow (where xentool is the MTS-ESP master and SC has no way to read the
table) xentool needs another channel to tell SC the active EDO + pitch
offset. With `--tune-supercollider` set, xentool emits

```
/xentool/tuning <edo:int> <pitch_offset:int> <layout_id:str>
```

over UDP to `127.0.0.1:57120` (sclang's default OSC port) at startup,
on every layout cycle, and every 3 s as a resync. The bundled
`supercollider/midi_piano_xentool.scd` registers an
`OSCdef('/xentool/tuning', …)` that updates `~tuningEdo` /
`~pitchOffset`, so a tuning swap reaches the synth without restarting
sclang.

The flag is **off** by default, **on** for `run-all-wooting.bat`, and
**not used** by `run-all-exquis.bat` (the MPE flow doesn't need it).
To override the Wooting orchestrator's default — for example to test
without the broadcast — set `XENTOOL_EXTRA_ARGS` to an empty string
before calling:

```powershell
set XENTOOL_EXTRA_ARGS=
scripts\run-all-wooting.bat
```

### macOS

`cargo install --path .` builds xentool. The Wooting backend currently has
no install script for macOS; build the SDKs from upstream sources if you
need them.

## Build (from source)

```bash
cargo build           # debug
cargo build --release # release
cargo test            # full test suite
```

---

## Commands shared between backends

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

Reports both connected Exquis MIDI ports and Wooting keyboards (when the
SDK is installed). Exquis example output:

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

USB metadata reported (when available): stable unique ID (serial-based or
VID:PID + bus/port location), vendor/product IDs, manufacturer, serial,
bus/port location, firmware version (only when a documented host query
exists; currently `unavailable`).

### Edit — visual layout editor

```powershell
xentool edit xtn/edo31.xtn
xentool edit wtn/edo31.wtn
xentool edit xtn/edo31.xtn --port 8088 --no-open
```

Opens a web-based editor in your default browser. Each connected/configured
board is rendered as a hex panel (Exquis: 61 pads in 6-5-6-5-…-6 layout;
Wooting: a 4×14 keyboard region) and you can edit Key / Chan / Color in
the sidebar. Save writes back to the file.

Import: click Import, pick a `.ltn` (Lumatone), `.wtn` (xenwooting), or
another `.xtn`. Use arrow keys to translate, `R` to rotate 60° around the
hovered pad. Enter applies the overlay; Esc cancels. Colors are preserved
verbatim through the round-trip (no 8-bit ↔ 7-bit lossy scaling during edit).

### Live HUD

```powershell
xentool serve xtn/edo31.xtn --hud
xentool serve wtn/edo31.wtn --hud --hud-port 9099
```

Opt-in via `--hud`. Starts a small HTTP server (default `0.0.0.0:9099` so
phones / tablets on the LAN can connect) and a browser-based view that shows
**currently-played notes in text form** with a special musical font (Bravura,
SMuFL). The page is dark, auto-fits to fill the viewport, and shows tiny
status corners (layout name, EDO, threshold, aftertouch mode, octave shift).
Tap (or click) anywhere to cycle four views:

| view        | shows                                                                      |
|-------------|-----------------------------------------------------------------------------|
| `notes`     | pressed pitches as Bravura glyphs (xenharm) or letter+octave / `o<oct>p<pc>` |
| `pcs`       | one entry per pitch class as `<pc>/<oct>` (e.g. `14/3-21/3-3/4`)            |
| `delta`     | root pitch (with `pc/oct`) + `+N` step offsets to the other notes          |
| `intervals` | chord-name candidates from the bundled Scala `chordnam.par` (844 chords)    |

Both backends feed the same wire shape; the page renders identically for
Exquis and Wooting. The hot loops never block on the HUD: snapshots are
published through a lock-free atomic pointer swap, JSON encoding happens on
the SSE handler thread.

#### Optional: xenharm note glyphs

For full microtonal note names with Bravura accidentals (up/down arrow
quartertone glyphs), the bundled `xenharm_service` Python sidecar maps each
absolute pitch to a Unicode SMuFL codepoint:

- **Linux:** the `install-linux-{exquis,wooting}.sh` scripts offer to
  install xenharm in a per-user venv and register a `systemd --user`
  service (`xenharm.service`). Nothing else to do — xentool's HUD probes
  `http://127.0.0.1:3199/health` at startup and uses it automatically.
- **Manual / other platforms:**

  ```bash
  cd xenharm_service
  python3.12 -m venv .venv && .venv/bin/pip install xenharmlib
  .venv/bin/python server.py --host 127.0.0.1 --port 3199
  ```

If the probe fails, the HUD silently falls back to numeric / letter labels.
Override the URL with `--xenharm-url`.

#### Optional: SuperCollider synth + OSC parameter strip

The repo ships two SuperCollider sketches under `supercollider/` —
pick the one matching your controller:

- `mpe_tanpura_xentool.scd` — for the **Exquis MPE** flow. Turns the
  controller into a microtonal tanpura drone with X/Y/Z mapped to
  string-pluck parameters.
- `midi_piano_xentool.scd` — for the **Wooting classic-MIDI** flow.
  A microtonal piano that listens on the channel-striped MIDI that
  xentool emits for the Wooting backend.

Both push parameter changes back to the HUD via OSC — encoder turns
and button presses appear as a sticky parameter strip plus a brief
event log on the right edge of the HUD page.

- **Windows:** `scripts\run-all-{exquis,wooting}.bat` already starts
  the right patch; for a single-window manual launch use
  `scripts\start-supercollider.bat <patch>.scd`.
- **Linux:** the install scripts wire up `xentool-supercollider.service`
  with the patch matching the chosen backend.
- **Manual / other platforms:** `sclang supercollider/<patch>.scd`
  after xentool is already running.

xentool listens for OSC on UDP port 9000 by default (override with
`--osc-port`). Send `/xentool/param/<group>/<name> <value> [<unit>]` for a
sticky parameter or `/xentool/event <text>` for a one-off log line. From
SuperCollider:

```sclang
~hud = NetAddr("127.0.0.1", 9000);
~hud.sendMsg("/xentool/param/filter/cutoff", 880.0, "Hz");
~hud.sendMsg("/xentool/event", "preset → bright");
```

---

## Exquis backend

### Monitor MIDI / MPE

```powershell
xentool midi
xentool midi --mode stream
xentool midi --mode stream --mpe-only
xentool midi --mode raw --log-raw
xentool midi --device 1 --log-file C:\temp\xentool-session.jsonl
```

Default mode is `hybrid`: top panel shows active touches with live `X/Y/Z`,
bottom panel shows a compact event stream, press `q` to quit. The `stream`
mode prints discrete events line by line; `raw` prints raw MIDI bytes.

`--mpe-only` filters the output down to MPE note and `X/Y/Z` events
(`note_on`, `note_off`, `x` as pitch bend, `y` as `CC74`, `z` as channel
pressure or poly aftertouch).

### Developer mode

```powershell
xentool dev on
xentool dev on --zone pads,encoders,slider,up-down,other-buttons
xentool dev off
```

Default `dev on` zones are pads, encoders, slider, up/down buttons, and
other buttons — intentionally avoids taking over the settings/sound buttons.

### Pad colors

```powershell
xentool pads fill amber
xentool pads fill 255,32,0 --device 1
xentool pads clear
xentool pads test
xentool pad 17 blue
xentool pad 17 0,127,0 --device 2
```

All color commands use the MPE-safe **snapshot approach** by default — MPE
pitch bend, CC74, and aftertouch continue to work normally while custom
colors are displayed. Pass `--legacy` to fall back to direct dev-mode pad
takeover (full LED control but no MPE):

```powershell
xentool pads fill amber --legacy
xentool pad 17 blue --legacy
```

### Load .xtn layout

```powershell
xentool load my_layout.xtn
```

Loads an `.xtn` layout file (INI-style, compatible with xenwooting `.wtn` /
Lumatone `.ltn`) and applies per-pad colors to connected Exquis boards. Each
`[BoardN]` section maps to a logical device name configured in
`devices.json`. If only one board section and one Exquis is connected,
auto-matches without config.

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

`Key_N` and `Chan_N` encode the abstract pitch in the EDO tuning system. The
`serve` command uses them to calculate microtonal frequencies:
`virtual_pitch = (Chan - 1) * Edo + Key + PitchOffset`. These values are
never sent to the Exquis — pads always send their pad ID as the MIDI note.

### Serve — microtonal tuning server

```powershell
xentool serve xtn/edo31.xtn
xentool serve xtn/edo31.xtn --pb-range 48
xentool serve xtn/edo31.xtn --output "My MIDI Port"
xentool serve xtn/edo31.xtn --mts-esp
```

Loads the layout, sets pad colors on connected boards, and runs a live
microtonal tuning server with a terminal UI showing active touches and
tuning status.

**Default mode: pitch bend retuning.** Intercepts MIDI from each Exquis,
remaps note numbers and injects per-channel pitch bends to shift each note
to its exact microtonal frequency, then forwards the retuned MIDI to a
virtual output port. Preserves full MPE expression (X/Y/Z) while adding
microtonal tuning.

Requirements for pitch bend mode:
- Install [loopMIDI](https://www.tobias-erichsen.de/software/loopmidi.html) and create a port named `loopMIDI Port` (the default)
- In your synth (e.g. Pianoteq), disable the direct "Exquis" MIDI input and enable `loopMIDI Port` instead
- The synth's per-note pitch bend range must match `--pb-range` (default: 16 semitones = ±1600 cents). Set Pianoteq's per-note PB range to ±1600 c, or pass `--pb-range 2` to keep Pianoteq's default — but that weakens the Exquis X-axis slide considerably.

How pitch bend retuning works:
1. For each pad, the target frequency is computed from the .xtn's `Key_N`/`Chan_N` and the `Edo` setting.
2. The nearest 12-TET MIDI note is found and the pitch bend offset to reach the exact microtonal frequency is calculated.
3. On each `note_on`, a pitch bend message is injected before the `note_on` on the same MIDI channel.
4. When the player uses the X axis (pitch bend expression), the player's bend is added to the tuning offset.
5. All other MPE data (Y=CC74, Z=pressure) passes through unchanged.

Multi-board support: each board gets its own tuning state. Scales to 4+ boards.

**Alternative: MTS-ESP mode (`--mts-esp`).** Registers as an MTS-ESP master
and broadcasts a global 128-note tuning table. The synth must be an MTS-ESP
client (Pianoteq supports this). Limitations: only one master allowed, one
global tuning table shared by all clients, max 128 unique notes (2 boards).

### Non-pad LED control

```powershell
xentool control settings red
xentool control encoder-1 blue
xentool control slider-1 green
xentool control 110 cyan         # raw control ID
```

Sets the LED color of encoders, buttons, and slider portions. Accepts
named controls or raw numeric IDs (see `xentool help control`).

### Note highlighting

```powershell
xentool highlight 60        # highlight middle C (green)
xentool highlight 60 0      # turn off highlight
```

Sends Note On/Off on MIDI channel 1. Works independently of developer mode.
Currently produces green highlights only (firmware-defined).

### LED color strategy — snapshot approach

The Exquis developer mode creates a fundamental conflict: taking over the
pad zone gives full RGB LED control but **disables MPE output** (pitch bend,
CC74, aftertouch). This tool solves it using the **snapshot technique**
discovered in the [PitchGridRack](https://github.com/peterjungx/PitchGridRack)
project:

1. Enter developer mode for all zones **except pads** (mask `0x3A`).
2. Send a Snapshot command (`09h`) that encodes per-pad MIDI note mappings and RGB colors in a single 262-byte SysEx message.
3. Pads remain in normal mode — **MPE X/Y/Z output is fully preserved**.

All pad color commands (`pad`, `pads fill`, `pads clear`, `pads test`) use
this approach by default. Pass `--legacy` to fall back to direct dev-mode
takeover (which disables MPE).

Snapshot message format:

```
F0 00 21 7E 7F 09          — SysEx header + Snapshot command
00 01 00 0E 00 00 01 01 00 00 00  — 11-byte config header
[midinote r g b] × 61      — per-pad note + RGB (244 bytes)
F7                          — SysEx end
```

Total: 262 bytes. Each pad gets 4 bytes: MIDI note number (default
`36+pad_id`) and RGB color (0–127 per channel).

### Multi-Exquis device configuration

For multi-Exquis setups, create `devices.json` at
`%LOCALAPPDATA%\xentool\config\devices.json`:

```json
{
  "devices": {
    "board0": { "serial": "ABC123" },
    "board1": { "serial": "DEF456" }
  }
}
```

Serial numbers are shown by `xentool list`. Board names in `.xtn` files are
matched to these logical names. The file is auto-created/updated by
`sync_boards()` on every `load`/`serve` command — manual edits are only
needed to pin a preferred board ordering.

### MPE details surfaced by `xentool midi`

The current Exquis user guide documents:

- `X` as Pitch Bend
- `Y` as `CC74`
- `Z` as Channel Pressure or Polyphonic Aftertouch

The hybrid UI updates those values in place for active touches instead of
printing a new line for every pressure or tilt change.

### Friendly control names

When developer-mode channel 16 events match documented control identifiers,
`xentool midi` shows names like `Settings`, `Play/Stop`, `Up`, `Down`,
`Encoder 1`, `Encoder 1 Button`. Unknown identifiers fall back to raw
numeric output.

### Logging

`xentool midi` logs automatically to JSONL unless `--no-log` is passed.

Default location:
- `%LOCALAPPDATA%\xentool\logs\…` if available via Windows app data lookup
- otherwise `logs\…` inside the current working directory

Each record includes timestamp, device number, port name, channel, event
kind, note/value fields, and optional raw bytes:

```json
{"ts":"2026-04-19T12:34:56Z","device":1,"port":"Exquis 1","channel":3,"kind":"note_on","note":64,"value":92,"label":null,"raw":null}
{"ts":"2026-04-19T12:34:56Z","device":1,"port":"Exquis 1","channel":16,"kind":"control","note":null,"value":127,"label":"play_stop","raw":null}
```

---

## Wooting backend

The Wooting backend turns each analog key on a Wooting keyboard into a
microtonal MIDI key with continuous aftertouch, driven from `.wtn` layout
files. It loads the Wooting Analog and RGB SDKs at runtime (no compile-time
dependency).

### New .wtn layout

```powershell
xentool new my_layout.wtn --edo 31
xentool new my_layout.wtn --edo 31 --boards 2 --pitch-offset 0
xentool new my_layout.wtn --edo 31 --force
```

Creates a blank `.wtn` file for a given EDO and board count. Each board is
a 4×14 grid of cells; every cell starts at `Key=0 Chan=1 Col=000000`.

### Load .wtn — paint LEDs

```powershell
xentool load my_layout.wtn
```

Loads a `.wtn` file and writes per-key LED colors to all connected Wooting
keyboards via the RGB SDK. One-shot: no polling, no MIDI.

### Serve — analog-to-MIDI with MTS-ESP

```powershell
xentool serve wtn/edo31.wtn
xentool serve wtn/edo31.wtn --output "loopMIDI Port"
```

Loads the layout, paints LEDs, registers as an MTS-ESP master, and runs a
**1 kHz polling hot loop** that reads per-key analog depths from the Wooting
Analog SDK and emits velocity-mapped Note On/Off + continuous poly-pressure
on a virtual MIDI port. The terminal UI shows currently-held notes,
controls state, and an event log; press `q` to quit.

The hot loop is time-critical (it intentionally avoids any disk I/O); the
TUI runs on its own thread with snapshots pushed every ~40 ms over a
bounded channel, so playing latency is unaffected.

**Live in-keyboard controls during `serve`** (key bindings on the Wooting
itself):

| Key             | Action                                              |
| --------------- | --------------------------------------------------- |
| Right Alt       | Cycle aftertouch mode: speed-mapped → peak-mapped → off |
| Space (held)    | Octave hold (toggle per board)                       |
| Arrow Left/Right | Adjust press threshold *or* aftertouch speed max    |
| Arrow Down      | Cycle velocity profile (linear / gamma / log / inv-log) |
| Left Ctrl       | Pitch bend up (analog depth → bend)                  |
| Left Alt        | Pitch bend down                                      |
| Left Meta       | Configurable analog CC (default CC4 board0, CC3 board1) |
| Context Menu    | Cycle to next `.wtn` file in `./wtn/`                |

The screensaver blanks all LEDs after a configurable idle period
(`screensaver_timeout_sec` in `settings.json` → `wooting.rgb`); the next
key press wakes it and is suppressed (no spurious note).

### .wtn layout format

INI-style, structurally similar to `.xtn`:

```ini
Edo=31
PitchOffset=0

[Board0]
Key_0=0
Chan_0=1
Col_0=FFDD00
…
```

The cell grid is 4×14 per board. Indices are linearized row-major. The
xenwooting `.wtn` format is compatible — files from xenwooting load
directly.

### Wooting tuning behavior

Wooting `serve` is **MTS-ESP only** (no pitch-bend retuning mode). The
synth must be an MTS-ESP client (e.g. Pianoteq). The MTS-ESP master
broadcasts a global 128-note tuning table built from Board0 + Board1.

### Wooting settings

User-tunable parameters live in `%LOCALAPPDATA%\xentool\config\settings.json`
under the `wooting` section. Defaults are ported verbatim from xenwooting
and cover press threshold, peak-tracking window, aftertouch deltas,
screensaver timeout, control-bar key map, and per-board CC/RGB index. See
`src/settings.rs` for the full list and defaults.
