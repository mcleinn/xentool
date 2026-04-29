# xenharm_service

Small localhost-only HTTP service that wraps [xenharmlib] for microtonal
note-name rendering. Used by `xentool`'s Live HUD to render Unicode note
glyphs (Bravura SMuFL codepoints) for individual pitches in the active
layout.

Auto-detected by xentool: the HUD probes `GET /health` at startup and uses
the service if it answers — no flag needed. If unreachable, xentool falls
back to numeric labels.

Copied verbatim from the predecessor project `xenwooting` so xentool stays
self-sufficient on machines where you want note names.

[xenharmlib]: https://github.com/retooth2/xenharmlib

## Requirements

- Python 3.12 (xenharmlib targets recent Python).
- `pip install xenharmlib`

## Run (manual)

```bash
python3.12 server.py --host 127.0.0.1 --port 3199
```

## Install (systemd user service, Linux)

From anywhere:

```bash
bash /absolute/path/to/xentool/xenharm_service/install-systemd.sh
```

Or pass an explicit location if the checkout is elsewhere:

```bash
bash install-systemd.sh /path/to/xenharm_service
```

The script only needs a valid `xenharm_service` directory containing
`server.py`. It writes `~/.config/systemd/user/xenharm.service` with the
correct absolute paths, then runs:

```bash
systemctl --user daemon-reload
systemctl --user enable --now xenharm.service
```

`xenharm.service` in this directory is just a reference template.

Health check:

```bash
curl -s http://127.0.0.1:3199/health
```

## API

- `GET /health`
- `POST /v1/note-names` with `{ "edo": 31, "pitches": [0, 1, 2] }`
  - Returns `{ "edo": 31, "results": { "0": {"short": "C0", "unicode": "C0"}, ... } }`
  - Pitches with no resolvable name (unsupported EDO / error) are omitted.
- `POST /v1/scale/rotate` with `{ "edo": 31, "pitches": [0, 10, 18], "direction": 1 }`
- `POST /v1/scale/retune` with `{ "edoFrom": 31, "edoTo": 19, "pitches": [0, 10, 18] }`
