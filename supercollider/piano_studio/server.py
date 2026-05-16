"""
piano_studio — Flask + python-osc relay between the touchscreen UI
and the running midi_piano_xentool.scd.

UI -> HTTP -> this script -> OSC -> sclang on port 57123.
sclang updates ~kparams + pushes the change to every currently-held
voice via Synth.set, so live tweaking is immediate on sounding notes.

Run:
    python server.py
Then open http://localhost:9101/

Port allocations (deliberately offset from tanpura_studio so both can
coexist on the same machine):
    HTTP:  9101   (vs tanpura 9100)
    OSC:   57123  (vs tanpura 57121)

Saved presets land in presets/preset_<timestamp>.json — each save is a
new file. A user-default singleton lives at presets/_default.json: when
present it's loaded at startup and used by /api/reset.
"""

from __future__ import annotations

import json
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

# Match sclang's openUDPPort(57123) in midi_piano_xentool.scd.
SC_HOST = "127.0.0.1"
SC_PORT = 57123

osc = udp_client.SimpleUDPClient(SC_HOST, SC_PORT)


# Defaults mirror ~kparams in the SC patch. The relay tracks current state
# in memory so /api/save can snapshot without round-tripping to SC.
DEFAULTS: dict[str, float] = {
    # Voice & global
    "voice":          0.0,    # 0=piano, 1=organ, 2=Rhodes EP
    "velSensitivity": 1.0,
    # ADSR
    "attackTime":     0.001,
    "decayTime":      0.18,
    "sustainLevel":   1.0,
    "releaseTime":    0.18,
    # Piano-specific tone
    "dampScale":      1.0,
    "brightScale":    1.0,
    "hammerHardness": 100.0,
    "detuneAmt":      0.003,
    # Drone / press-driven sustain
    "droneAmt":       1.0,
    "droneType":     0.0,    # 0=CombL, 1=Sine, 2=Off
    "pressSwellLo":   1.0,
    "pressSwellHi":   1.0,
    # Y-axis effect
    "yMode":          5.0,    # 0=LPF, 1=Trem, 2=Leslie, 3=Chorus, 4=Vibrato, 5=Off
    "yRate":          5.0,
    "leslieMin":      0.7,
    "leslieMax":      7.0,
    # Y-axis input mapping (Bezier shape + pitch attenuation; defaults = identity)
    "yMin":           0.0,
    "yCenter":        0.5,
    "yMax":           1.0,
    "yPitchTrack":    0.0,
    "yPitchRefHz":    261.6,
    # Output
    "reverbMix":      0.18,
    "reverbRoom":     0.72,
    "masterAmp":      1.0,
}

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
    osc.send_message("/piano/set", [name, value])
    return jsonify({"ok": True, "name": name, "value": value})


@app.post("/api/batch")
def batch_set():
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
        osc.send_message("/piano/batch", flat)
    return jsonify({"ok": True, "applied": len(flat) // 2})


@app.get("/api/state")
def get_state():
    return jsonify({"state": state, "defaults": DEFAULTS})


@app.post("/api/reset")
def reset():
    target = _effective_defaults()
    state.clear()
    state.update(target)
    flat: list = []
    for name, value in target.items():
        flat.append(name)
        flat.append(float(value))
    osc.send_message("/piano/batch", flat)
    return jsonify({"ok": True, "state": state})


@app.post("/api/save-default")
def save_default():
    payload = {
        "saved_at": datetime.now().isoformat(timespec="seconds"),
        "note": "user default",
        "params": dict(state),
    }
    USER_DEFAULT_PATH.write_text(json.dumps(payload, indent=2), encoding="utf-8")
    return jsonify({"ok": True, "path": str(USER_DEFAULT_PATH)})


@app.post("/api/clear-default")
def clear_default():
    if USER_DEFAULT_PATH.is_file():
        USER_DEFAULT_PATH.unlink()
        return jsonify({"ok": True, "removed": True})
    return jsonify({"ok": True, "removed": False})


@app.post("/api/save")
def save_preset():
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
        osc.send_message("/piano/batch", flat)
    return jsonify({"ok": True, "applied": len(flat) // 2, "state": state})


def _retry_state_to_sc():
    """Re-send the in-memory state every 2 s for ~12 s after startup.

    Mirrors tanpura_studio's retry: defeats the race between this script's
    initial OSC batch and sclang's `openUDPPort(57123)` + `OSCdef(\\pianoStudioBatch)`
    registration. Each retry sends the CURRENT `state` so slider drags
    that landed between retries are preserved.
    """
    for _ in range(6):
        time.sleep(2.0)
        items = list(state.items())
        flat: list = []
        for name, value in items:
            flat.append(name)
            flat.append(float(value))
        try:
            osc.send_message("/piano/batch", flat)
        except Exception:
            pass


def main():
    print("=== piano_studio ===")
    print(f"  serving:  http://localhost:9101/")
    print(f"  OSC out:  udp://{SC_HOST}:{SC_PORT}  (must match sclang)")
    print(f"  presets:  {PRESETS_DIR}")
    user_default = _load_user_default()
    if user_default:
        print(f"  user default loaded from {USER_DEFAULT_PATH.name} ({len(user_default)} params)")
        flat: list = []
        for name, value in state.items():
            flat.append(name)
            flat.append(float(value))
        osc.send_message("/piano/batch", flat)
        threading.Thread(target=_retry_state_to_sc, daemon=True).start()
    else:
        print(f"  no user default; using factory DEFAULTS")
    app.run(host="127.0.0.1", port=9101, debug=False, use_reloader=False)


if __name__ == "__main__":
    main()
