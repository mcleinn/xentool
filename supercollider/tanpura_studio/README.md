# tanpura studio

Touchscreen-friendly web UI for tweaking the `mpe_tanpura_xentool.scd`
SynthDef live, with Save / Load preset support. Communicates with sclang
via OSC. Independent from the existing xentool HUD.

## Run

1. **Start sclang with the live tanpura patch.** It opens UDP port 57121
   for studio messages on startup; you'll see
   `Studio OSC listening on udp://127.0.0.1:57121`.

2. **Start the relay.**
   ```
   pip install -r requirements.txt
   python server.py
   ```
   Default port: `9100`.

3. **Open `http://localhost:9100/`** on the touchscreen device. Big
   sliders, accordion sections per layer.

## What the UI exposes

Sliders for every continuous `~kparam` in the patch and dropdowns for
the four mode switches (alternative effects):

- **String**: decay (KS), damp scale, body / brightness scale.
- **Drone**: drone amount; **type**: CombL feedback / Sine / Off.
- **Jawari**: amount, drive, cubed mix; **shape**: tanh+cubed /
  tanh-only / fold.
- **Sympathy**: mix.
- **EQ shelves**: hi/lo corner freq + min/max dB; **type** per shelf:
  Shelf / Peaking EQ / Off.
- **Dynamics**: press-swell range, limiter threshold.
- **Reverb & master**: reverb mix, reverb room, master amp.

Slider/dropdown changes are pushed to all currently-held voices
(`synth.set`) and to `~kparams` for new noteOns. So holding a chord
and dragging a slider (or switching a mode) re-shapes the held voices
in real time.

## Save / Load

- **Save** writes `presets/preset_<YYYYMMDD-HHMMSS>.json` with the
  current parameter snapshot. Each save is a new file; never overwrites.
- **Load** opens a list of saved presets (newest first) and applies
  the chosen one's values atomically (single OSC `/tanpura/batch`).
- **Reset** restores all params to the SC patch's compile-time defaults.

## API surface (relay)

| route | what it does |
|---|---|
| `GET /` | the UI HTML |
| `GET /api/state` | current values + defaults |
| `POST /api/set` `{name, value}` | set one param |
| `POST /api/batch` `{params: {…}}` | apply many atomically |
| `POST /api/save` `{note?}` | write new preset file |
| `GET /api/presets` | list saved presets |
| `POST /api/load` `{filename}` | apply preset to SC |
| `POST /api/reset` | restore defaults |

## OSC contract (relay → sclang on udp:57121)

| address | args | effect |
|---|---|---|
| `/tanpura/set` | name (str), value (float) | update one param |
| `/tanpura/batch` | name1, val1, name2, val2, … | apply many atomically |
| `/tanpura/dump` | (none) | sclang replies on udp:57122 with all params |
| `/tanpura/reset` | (none) | restore defaults |

## Future extensions

- Load preset deletion / rename in the UI.
- Preset metadata (notes, tags).
- Per-mode A/B compare buttons.
