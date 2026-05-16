"""
tanpura_studio — Flask + python-osc relay between the touchscreen UI
and the running mpe_tanpura_xentool.scd.

UI -> HTTP -> this script -> OSC -> sclang on port 57121.
sclang updates ~kparams + pushes the change to every currently-held
voice via Synth.set, so live tweaking is immediate on sounding notes.

Run:
    python server.py
Then open http://localhost:9100/

The web/ subdirectory is served as static. Saved presets land in
presets/preset_<timestamp>.json — each save is a new file (no
overwrite). Loading a preset replays its values via /tanpura/batch.

This relay is intentionally separate from the existing xentool HUD
server (which lives on 9099). No auth, localhost only.
"""

from __future__ import annotations

import json
import os
import threading
import time
from datetime import datetime
from pathlib import Path

from flask import Flask, jsonify, request, send_from_directory
from pythonosc import udp_client


HERE = Path(__file__).parent
WEB_DIR = HERE / "web"
PRESETS_DIR = HERE / "presets"
PRESETS_DIR.mkdir(exist_ok=True)

# Match sclang's openUDPPort(57121) in mpe_tanpura_xentool.scd.
SC_HOST = "127.0.0.1"
SC_PORT = 57121

osc = udp_client.SimpleUDPClient(SC_HOST, SC_PORT)


# Defaults must match ~kparams in the SC patch. The relay tracks current
# state in memory so /api/save can snapshot without round-tripping to SC.
DEFAULTS: dict[str, float] = {
    # original kparams (encoders/buttons cycle)
    "decay":         11.0,
    "dampScale":     1.0,
    "brightScale":   1.0,
    "reverbMix":     0.45,
    "reverbRoom":    0.88,
    "masterAmp":     1.0,
    # new studio-exposed kparams
    "droneAmt":      1.0,
    "droneType":     0.0,    # 0=CombL, 1=Sine+harm, 2=off, 3=Saw, 4=Tri, 5=Beating, 6=Pulse, 7=Vocal
    "droneHarmGain": 1.0,    # Sine+harm only: scales 2nd/3rd/4th harmonic gains
    "jawariAmt":     0.7,
    "jawariMode":    0.0,    # 0=tanh+cubed, 1=tanh-only, 2=fold
    "jawariDrive":   6.0,
    "jawariCubed":   1.4,
    "sympAmt":       1.0,
    "hiShelfFreq":   2600.0,
    "hiShelfMin":    -24.0,
    "hiShelfMax":    24.0,
    "hiShelfMode":   0.0,    # 0=BHiShelf, 1=BPeakEQ, 2=off
    "loShelfFreq":   400.0,
    "loShelfMin":    0.0,
    "loShelfMax":    -8.0,
    "loShelfMode":   0.0,    # 0=BLowShelf, 1=BPeakEQ, 2=off
    "yMode":         1.0,    # 0=shelves, 1=RLPF (sitar/sarod wah — factory pick), 2=BPF, 3=tremolo, 4=comb, 5=off
    "pressSwellLo":  0.3,
    "pressSwellHi":  2.0,
    "limiterThresh": 0.6,
    # Y-axis input mapping (Bezier shape + pitch attenuation; defaults = identity)
    "yMin":          0.0,
    "yCenter":       0.5,
    "yMax":          1.0,
    "yPitchTrack":   0.0,
    "yPitchRefHz":   261.6,
}

# User-default singleton: when present, overrides DEFAULTS at startup and on
# /api/reset. "Make default" in the UI writes here; deleting the file
# returns the synth to factory defaults on next start.
USER_DEFAULT_PATH = PRESETS_DIR / "_default.json"


def _load_user_default() -> dict[str, float] | None:
    if not USER_DEFAULT_PATH.is_file():
        return None
    try:
        data = json.loads(USER_DEFAULT_PATH.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError):
        return None
    params = data.get("params") or {}
    out: dict[str, float] = {}
    for k, v in params.items():
        if k not in DEFAULTS:
            continue
        try:
            out[k] = float(v)
        except (TypeError, ValueError):
            continue
    return out or None


def _effective_defaults() -> dict[str, float]:
    """Factory DEFAULTS overlaid with user default (if file exists)."""
    out = dict(DEFAULTS)
    user = _load_user_default()
    if user:
        out.update(user)
    return out

state: dict[str, float] = _effective_defaults()


app = Flask(__name__, static_folder=None)


@app.route("/")
def index():
    return send_from_directory(str(WEB_DIR), "index.html")


@app.route("/<path:asset>")
def asset(asset):
    return send_from_directory(str(WEB_DIR), asset)


@app.post("/api/set")
def set_param():
    """Set a single parameter. Body: {"name": "...", "value": float}."""
    body = request.get_json(force=True)
    name = body.get("name")
    value = body.get("value")
    if name is None or value is None:
        return jsonify({"error": "name+value required"}), 400
    try:
        value = float(value)
    except (TypeError, ValueError):
        return jsonify({"error": "value must be numeric"}), 400
    state[name] = value
    osc.send_message("/tanpura/set", [name, value])
    return jsonify({"ok": True, "name": name, "value": value})


@app.post("/api/batch")
def batch_set():
    """Apply many parameters atomically. Body: {"params": {"a": 1.0, "b": 2.0}}."""
    body = request.get_json(force=True)
    params = body.get("params") or {}
    flat: list = []
    for name, value in params.items():
        try:
            v = float(value)
        except (TypeError, ValueError):
            continue
        state[name] = v
        flat.append(name)
        flat.append(v)
    if flat:
        osc.send_message("/tanpura/batch", flat)
    return jsonify({"ok": True, "applied": len(flat) // 2})


@app.get("/api/state")
def get_state():
    """Current in-memory state."""
    return jsonify({"state": state, "defaults": DEFAULTS})


@app.post("/api/reset")
def reset():
    """Restore defaults on both relay state and SC.

    If a user default exists (`presets/_default.json`), reset goes there;
    otherwise to factory DEFAULTS. Pushes via /tanpura/batch so the SC
    side stays in sync without needing to know about user defaults.
    """
    target = _effective_defaults()
    state.clear()
    state.update(target)
    flat: list = []
    for name, value in target.items():
        flat.append(name)
        flat.append(float(value))
    osc.send_message("/tanpura/batch", flat)
    return jsonify({"ok": True, "state": state})


@app.post("/api/save-default")
def save_default():
    """Save current state as the auto-loaded default for next startup.

    Writes `presets/_default.json` (overwrites). On next `python server.py`,
    state is initialized from this file. /api/reset also reads it.
    """
    payload = {
        "saved_at": datetime.now().isoformat(timespec="seconds"),
        "note": "user default",
        "params": dict(state),
    }
    USER_DEFAULT_PATH.write_text(json.dumps(payload, indent=2), encoding="utf-8")
    return jsonify({"ok": True, "path": str(USER_DEFAULT_PATH)})


@app.post("/api/clear-default")
def clear_default():
    """Remove user default file so next startup uses factory DEFAULTS."""
    if USER_DEFAULT_PATH.is_file():
        USER_DEFAULT_PATH.unlink()
        return jsonify({"ok": True, "removed": True})
    return jsonify({"ok": True, "removed": False})


@app.post("/api/save")
def save_preset():
    """Save current state to presets/preset_<timestamp>.json — new file
    every time, never overwrites. Optionally accepts a `note` field
    in the body that gets stored alongside the params."""
    body = request.get_json(silent=True) or {}
    note = body.get("note", "")
    ts = datetime.now().strftime("%Y%m%d-%H%M%S")
    fname = f"preset_{ts}.json"
    path = PRESETS_DIR / fname
    payload = {
        "saved_at": datetime.now().isoformat(timespec="seconds"),
        "note": note,
        "params": dict(state),
    }
    path.write_text(json.dumps(payload, indent=2), encoding="utf-8")
    return jsonify({"ok": True, "filename": fname, "path": str(path)})


@app.get("/api/presets")
def list_presets():
    """Return saved presets sorted newest-first by filename."""
    files = sorted(
        PRESETS_DIR.glob("preset_*.json"),
        key=lambda p: p.name, reverse=True,
    )
    out = []
    for f in files:
        try:
            data = json.loads(f.read_text(encoding="utf-8"))
            out.append({
                "filename": f.name,
                "saved_at": data.get("saved_at"),
                "note": data.get("note", ""),
                "params": data.get("params", {}),
            })
        except (OSError, json.JSONDecodeError):
            continue
    return jsonify({"presets": out})


@app.post("/api/load")
def load_preset():
    """Load a preset by filename and push its params to SC. Body:
    {"filename": "preset_XXX.json"}."""
    body = request.get_json(force=True)
    fname = body.get("filename")
    if not fname:
        return jsonify({"error": "filename required"}), 400
    path = PRESETS_DIR / fname
    if not path.is_file():
        return jsonify({"error": f"not found: {fname}"}), 404
    try:
        data = json.loads(path.read_text(encoding="utf-8"))
    except json.JSONDecodeError as e:
        return jsonify({"error": f"invalid JSON: {e}"}), 400
    params = data.get("params") or {}
    flat: list = []
    for name, value in params.items():
        try:
            v = float(value)
        except (TypeError, ValueError):
            continue
        state[name] = v
        flat.append(name)
        flat.append(v)
    if flat:
        osc.send_message("/tanpura/batch", flat)
    return jsonify({"ok": True, "applied": len(flat) // 2, "state": state})


def _retry_state_to_sc():
    """Re-send the in-memory state every 2 s for ~12 s after startup.

    The single fire-and-forget batch in main() races against sclang's boot:
    if it lands before `thisProcess.openUDPPort(57121)` + the matching
    `OSCdef(\\studioBatch)` registration, the packet is silently dropped and
    SC stays at factory `~kparams` until something else pushes (slider drag,
    Reset, preset load). Re-sending periodically defeats the race — each
    retry is idempotent on the SC side. The retry sends the CURRENT
    `state` (snapshotted via `list(state.items())`), so any slider drags
    that landed between retries are preserved instead of clobbered.
    """
    for _ in range(6):
        time.sleep(2.0)
        items = list(state.items())
        flat: list = []
        for name, value in items:
            flat.append(name)
            flat.append(float(value))
        try:
            osc.send_message("/tanpura/batch", flat)
        except Exception:
            # SC down, OSC unreachable, etc. — keep retrying anyway.
            pass


def main():
    print("=== tanpura_studio ===")
    print(f"  serving:  http://localhost:9100/")
    print(f"  OSC out:  udp://{SC_HOST}:{SC_PORT}  (must match sclang)")
    print(f"  presets:  {PRESETS_DIR}")
    user_default = _load_user_default()
    if user_default:
        print(f"  user default loaded from {USER_DEFAULT_PATH.name} ({len(user_default)} params)")
        # Fire-and-forget push to SC. The follow-up retry thread re-sends
        # for ~12 s in case sclang hadn't yet reached its OSC listener
        # registration when this initial packet arrived.
        flat: list = []
        for name, value in state.items():
            flat.append(name)
            flat.append(float(value))
        osc.send_message("/tanpura/batch", flat)
        threading.Thread(target=_retry_state_to_sc, daemon=True).start()
    else:
        print(f"  no user default; using factory DEFAULTS")
    app.run(host="127.0.0.1", port=9100, debug=False, use_reloader=False)


if __name__ == "__main__":
    main()
