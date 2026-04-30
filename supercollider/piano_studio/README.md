# piano studio

Touchscreen-friendly web UI for tweaking the `midi_piano_xentool.scd`
SynthDef live, with Save / Load / Make-default / Factory-reset preset
support. Sister of `tanpura_studio/` — same shape, different synth.
Communicates with sclang via OSC. Independent from the existing
xentool HUD.

## Run

1. **Start sclang with the piano patch.** It opens UDP port 57123 for
   studio messages on startup; you'll see
   `Piano studio OSC listening on udp://127.0.0.1:57123`.

2. **Start the relay.**
   ```
   pip install -r requirements.txt
   python server.py
   ```
   Default HTTP port: `9101` (offset from tanpura's 9100 so both can
   coexist).

3. **Open `http://localhost:9101/`** on the touchscreen. Big sliders,
   accordion sections per layer.

## What the UI exposes

Sliders and dropdowns for every studio-controllable param. With all
factory defaults, the synth sounds identical to the original
`midi_piano_xentool.scd`.

- **Voice & dynamics**: voice mode (Piano / Organ / Rhodes EP),
  velocity sensitivity, ADSR (attack/decay/sustain/release).
- **Piano tone** (only shown when voice = Piano): KS damp, body
  brightness, hammer hardness, three-string detune.
- **Drone / press sustain**: amount + type (CombL / Sine / Off),
  press swell range.
- **Y-axis effect** (CC74-driven; the Wooting Y axis can be configured
  to emit CC74): Off / LPF / Tremolo / Leslie / Chorus / Vibrato.
  Modulation rate + Leslie slow/fast speeds appear conditionally.
- **Reverb & master**: reverb mix/room, master amp.

Slider/dropdown changes are pushed to all currently-held voices
(`synth.set`) and to `~kparams` for new noteOns. Holding a chord and
moving controls re-shapes the held voices in real time.

## Save / Load / Make default / Factory reset

- **Save preset** writes `presets/preset_<YYYYMMDD-HHMMSS>.json` —
  never overwrites.
- **Load** opens a list of saved presets (newest first); pick one to
  apply atomically (single OSC `/piano/batch`).
- **Make default** writes the current state to `presets/_default.json`.
  On next `python server.py` startup, that file is auto-loaded into
  `state` and pushed to SC. `Reset` also goes to this user default if
  present, otherwise to factory.
- **Factory reset** deletes `_default.json` and resets all params to
  the SC patch's hardcoded defaults.

## API surface (relay)

Same shape as `tanpura_studio/`, but routes a separate `/piano/*` OSC
namespace and uses a distinct HTTP port:

| route | what it does |
|---|---|
| `GET /` | the UI HTML |
| `GET /api/state` | current values + factory defaults |
| `POST /api/set` `{name, value}` | set one param |
| `POST /api/batch` `{params: {…}}` | apply many atomically |
| `POST /api/save` `{note?}` | write new timestamped preset |
| `GET /api/presets` | list saved presets |
| `POST /api/load` `{filename}` | apply preset to SC |
| `POST /api/reset` | restore defaults (user or factory) |
| `POST /api/save-default` | write `_default.json` |
| `POST /api/clear-default` | remove `_default.json` |

## OSC contract (relay → sclang on udp:57123)

| address | args | effect |
|---|---|---|
| `/piano/set` | name (str), value (float) | update one param |
| `/piano/batch` | name1, val1, name2, val2, … | apply many atomically |
| `/piano/dump` | (none) | sclang replies on udp:57124 with all params |
| `/piano/reset` | (none) | restore defaults |

## Port allocations (avoid collisions)

| process | HTTP | OSC in | OSC reply |
|---|---|---|---|
| `tanpura_studio` | 9100 | 57121 | 57122 |
| `piano_studio`   | 9101 | 57123 | 57124 |
| `xentool HUD`    | 9099 | — | — |
| `xentool OSC in` | — | 9000 | — |
