#!/usr/bin/env python3.12

import argparse
import json
import re
import threading
from collections import OrderedDict
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer


# xenharmlib is installed for python3.12 on this machine.
from xenharmlib import EDOTuning  # type: ignore
from xenharmlib import UpDownNotation  # type: ignore
from xenharmlib.notation.updown import (  # type: ignore
    DownwardsEnharmStrategy,
    MixedLeftEnharmStrategy,
    MixedRightEnharmStrategy,
    UpwardsEnharmStrategy,
)


_NOTATION_LOCK = threading.Lock()
_NOTATION_BY_EDO: dict[int, EDOTuning] = {}
_UPDOWN_BY_EDO_STRAT: dict[tuple[int, str], UpDownNotation] = {}


_CACHE_LOCK = threading.Lock()
_NOTE_CACHE: "OrderedDict[tuple[int, int], dict[str, str] | None]" = OrderedDict()
_NOTE_CACHE_MAX = 8192

_INTERVAL_CACHE_LOCK = threading.Lock()
_INTERVAL_CACHE: "OrderedDict[tuple[int, int], dict | None]" = OrderedDict()
_INTERVAL_CACHE_MAX = 4096


# Bravura SMuFL Private Use Area codepoints for up/down arrow accidentals
# (xenharmlib's `^` / `v` prefixes). Ported from xenassist/server.py so HUD
# glyphs match the rest of the family.
_NOTATION_REPLACEMENTS: dict[str, str] = {
    "^":     "",
    "^^":    "",
    "^^^":   "",
    "vvv#":  "",
    "vv#":   "",
    "v#":    "",
    "#":     "",
    "^#":    "",
    "^^#":   "",
    "^^^#":  "",
    "vvvx":  "",
    "vvx":   "",
    "vx":    "",
    "x":     "",
    "^x":    "",
    "^^x":   "",
    "^^^x":  "",
    "v":     "",
    "vv":    "",
    "vvv":   "",
    "^^^b":  "",
    "^^b":   "",
    "^b":    "",
    "b":     "",
    "vb":    "",
    "vvb":   "",
    "vvvb":  "",
    "^^^bb": "",
    "^^bb":  "",
    "^bb":   "",
    "bb":    "",
    "vbb":   "",
    "vvbb":  "",
    "vvvbb": "",
}


_NOTE_RE = re.compile(r"^([v\^]*)([A-G])([#xb]+)?(-?\d+)?$")
_INTERVAL_RE = re.compile(r"^([v\^]*)(.*)$")
# Up/down arrow prefixes only (no accidental). Used to render xenharmlib's
# interval `short_repr` such as "vM3" or "^^P5" with Bravura SMuFL glyphs.
# Quality (m/M/P/A/d) and number stay as plain ASCII.
_PREFIX_GLYPHS: dict[str, str] = {
    "":     "",
    "^":    "",
    "^^":   "",
    "^^^":  "",
    "v":    "",
    "vv":   "",
    "vvv":  "",
}


def encode_notation(short_repr: str) -> str:
    m = _NOTE_RE.match(short_repr)
    if not m:
        return short_repr
    prefix, note, suffix, octave = m.groups()
    suffix = suffix or ""
    octave = octave or ""
    key = f"{prefix}{suffix}"
    return f"{note}{_NOTATION_REPLACEMENTS.get(key, '')}{octave}"


def encode_interval(short_repr: str) -> str:
    """Render an interval short-form (e.g. ``vM3``, ``^^P5``, ``P1``) with
    Bravura glyphs for the up/down-arrow prefix. Quality (m/M/P/A/d) and
    number stay as plain ASCII -- they're already legible without a music
    font."""
    m = _INTERVAL_RE.match(short_repr)
    if not m:
        return short_repr
    prefix, rest = m.groups()
    glyph = _PREFIX_GLYPHS.get(prefix)
    if glyph is None:
        return short_repr
    return f"{glyph}{rest}"


def _get_tuning(edo: int) -> EDOTuning | None:
    if edo < 5 or edo > 72:
        return None
    with _NOTATION_LOCK:
        hit = _NOTATION_BY_EDO.get(edo)
        if hit is not None:
            return hit
        try:
            tuning = EDOTuning(edo)
        except Exception:
            return None
        _NOTATION_BY_EDO[edo] = tuning
        return tuning


def _get_updown(edo: int, strat: str) -> tuple[EDOTuning, UpDownNotation] | None:
    tuning = _get_tuning(edo)
    if tuning is None:
        return None
    key = (edo, strat)
    with _NOTATION_LOCK:
        n = _UPDOWN_BY_EDO_STRAT.get(key)
        if n is not None:
            return tuning, n
        try:
            n = UpDownNotation(tuning)
            if strat == "up":
                n.enharm_strategy = UpwardsEnharmStrategy(n)
            elif strat == "down":
                n.enharm_strategy = DownwardsEnharmStrategy(n)
            elif strat == "mixL":
                n.enharm_strategy = MixedLeftEnharmStrategy(n)
            elif strat == "mixR":
                n.enharm_strategy = MixedRightEnharmStrategy(n)
            else:
                return None
        except Exception:
            return None
        _UPDOWN_BY_EDO_STRAT[key] = n
        return tuning, n


_MISSING = object()


def _cache_get(edo: int, pitch: int) -> dict[str, str] | None | object:
    key = (edo, pitch)
    with _CACHE_LOCK:
        if key not in _NOTE_CACHE:
            return _MISSING
        _NOTE_CACHE.move_to_end(key)
        return _NOTE_CACHE[key]


def _cache_set(edo: int, pitch: int, value: dict[str, str] | None) -> None:
    key = (edo, pitch)
    with _CACHE_LOCK:
        _NOTE_CACHE[key] = value
        _NOTE_CACHE.move_to_end(key)
        while len(_NOTE_CACHE) > _NOTE_CACHE_MAX:
            _NOTE_CACHE.popitem(last=False)


def _interval_cache_get(edo: int, n_steps: int):
    key = (edo, n_steps)
    with _INTERVAL_CACHE_LOCK:
        if key not in _INTERVAL_CACHE:
            return _MISSING
        _INTERVAL_CACHE.move_to_end(key)
        return _INTERVAL_CACHE[key]


def _interval_cache_set(edo: int, n_steps: int, value) -> None:
    key = (edo, n_steps)
    with _INTERVAL_CACHE_LOCK:
        _INTERVAL_CACHE[key] = value
        _INTERVAL_CACHE.move_to_end(key)
        while len(_INTERVAL_CACHE) > _INTERVAL_CACHE_MAX:
            _INTERVAL_CACHE.popitem(last=False)


def note_name_for_pitch(edo: int, pitch: int) -> dict | None:
    cached = _cache_get(edo, pitch)
    if cached is not _MISSING:
        return cached  # may be None

    hit = _get_updown(edo, "mixL")
    if hit is None:
        _cache_set(edo, pitch, None)
        return None
    tuning, notation = hit
    try:
        ep = tuning.pitch(pitch)

        def one(n: UpDownNotation):
            note = n.guess_note(ep)
            short = note.short_repr
            return {"short": short, "unicode": encode_notation(short)}

        primary = one(notation)

        alts: list[dict[str, str]] = []
        seen = {primary["short"]}
        for strat in ("up", "down", "mixR"):
            hit2 = _get_updown(edo, strat)
            if hit2 is None:
                continue
            _, n2 = hit2
            v = one(n2)
            if v["short"] in seen:
                continue
            seen.add(v["short"])
            alts.append(v)

        out = {"short": primary["short"], "unicode": primary["unicode"], "alts": alts}
        _cache_set(edo, pitch, out)
        return out
    except Exception:
        _cache_set(edo, pitch, None)
        return None


def _make_ed_interval(tuning: EDOTuning, n_steps: int):
    """xenharmlib's interval-construction API has shifted across versions;
    try a few common patterns. Returns ``None`` if all fail. The caller is
    expected to feed this into ``notation.guess_interval(...)``."""
    # Method 1: tuning.interval(n)
    try:
        return tuning.interval(n_steps)
    except Exception:
        pass
    # Method 2: difference of two pitches
    try:
        return tuning.pitch(n_steps) - tuning.pitch(0)
    except Exception:
        pass
    # Method 3: explicit EDOInterval class (newer xenharmlib).
    try:
        from xenharmlib import EDOInterval  # type: ignore
        return EDOInterval(tuning, n_steps)
    except Exception:
        pass
    return None


def interval_name_for_steps(edo: int, n_steps: int) -> dict | None:
    cached = _interval_cache_get(edo, n_steps)
    if cached is not _MISSING:
        return cached

    hit = _get_updown(edo, "mixL")
    if hit is None:
        _interval_cache_set(edo, n_steps, None)
        return None
    tuning, notation = hit

    ed_interval = _make_ed_interval(tuning, n_steps)
    if ed_interval is None:
        _interval_cache_set(edo, n_steps, None)
        return None

    try:
        def one(n: UpDownNotation):
            iv = n.guess_interval(ed_interval)
            short = iv.short_repr
            return {"short": short, "unicode": encode_interval(short)}

        primary = one(notation)

        alts: list[dict[str, str]] = []
        seen = {primary["short"]}
        for strat in ("up", "down", "mixR"):
            hit2 = _get_updown(edo, strat)
            if hit2 is None:
                continue
            _, n2 = hit2
            try:
                v = one(n2)
            except Exception:
                continue
            if v["short"] in seen:
                continue
            seen.add(v["short"])
            alts.append(v)

        out = {"short": primary["short"], "unicode": primary["unicode"], "alts": alts}
        _interval_cache_set(edo, n_steps, out)
        return out
    except Exception:
        _interval_cache_set(edo, n_steps, None)
        return None


def _json_body(handler: BaseHTTPRequestHandler) -> dict:
    n = int(handler.headers.get("Content-Length", "0") or "0")
    raw = handler.rfile.read(n) if n > 0 else b""
    if not raw:
        return {}
    return json.loads(raw.decode("utf-8"))


def _send_json(handler: BaseHTTPRequestHandler, status: int, obj) -> None:
    data = json.dumps(obj, ensure_ascii=False).encode("utf-8")
    handler.send_response(status)
    handler.send_header("Content-Type", "application/json; charset=utf-8")
    handler.send_header("Content-Length", str(len(data)))
    handler.send_header("Cache-Control", "no-store")
    handler.end_headers()
    handler.wfile.write(data)


class Handler(BaseHTTPRequestHandler):
    server_version = "xenharm_service/0.2"

    def log_message(self, fmt: str, *args) -> None:
        # keep quiet under systemd unless something goes wrong
        return

    def do_GET(self) -> None:
        if self.path == "/health":
            _send_json(self, 200, {"ok": True})
            return
        _send_json(self, 404, {"error": "not found"})

    def do_POST(self) -> None:
        if self.path == "/v1/note-names":
            self._post_note_names()
            return
        if self.path == "/v1/interval-names":
            self._post_interval_names()
            return
        if self.path == "/v1/scale/rotate":
            self._post_scale_rotate()
            return
        if self.path == "/v1/scale/retune":
            self._post_scale_retune()
            return
        _send_json(self, 404, {"error": "not found"})

    def _post_note_names(self) -> None:
        try:
            body = _json_body(self)
        except Exception:
            _send_json(self, 400, {"error": "invalid json"})
            return

        edo = body.get("edo")
        pitches = body.get("pitches")
        if not isinstance(edo, int) or not isinstance(pitches, list):
            _send_json(self, 400, {"error": "expected { edo: int, pitches: int[] }"})
            return

        results: dict[str, dict[str, str]] = {}
        for p in pitches:
            if not isinstance(p, int):
                continue
            nn = note_name_for_pitch(edo, p)
            if nn is None:
                continue
            results[str(p)] = nn

        _send_json(self, 200, {"edo": edo, "results": results})

    def _post_interval_names(self) -> None:
        try:
            body = _json_body(self)
        except Exception:
            _send_json(self, 400, {"error": "invalid json"})
            return

        edo = body.get("edo")
        steps = body.get("steps")
        if not isinstance(edo, int) or not isinstance(steps, list):
            _send_json(self, 400, {"error": "expected { edo: int, steps: int[] }"})
            return

        results: dict[str, dict[str, str]] = {}
        for s in steps:
            if not isinstance(s, int):
                continue
            iv = interval_name_for_steps(edo, s)
            if iv is None:
                continue
            results[str(s)] = iv

        _send_json(self, 200, {"edo": edo, "results": results})

    def _post_scale_rotate(self) -> None:
        try:
            body = _json_body(self)
        except Exception:
            _send_json(self, 200, {})
            return

        edo = body.get("edo")
        pitches = body.get("pitches")
        direction = body.get("direction")
        if not isinstance(edo, int) or not isinstance(pitches, list) or not isinstance(direction, int):
            _send_json(self, 200, {})
            return

        try:
            t = EDOTuning(edo)
            scale = t.scale([t.pitch(int(p)) for p in pitches if isinstance(p, int)])
            if direction < 0:
                scale2 = scale.rotated_down()
            elif direction > 0:
                scale2 = scale.rotated_up()
            else:
                scale2 = scale
            out_pitches = [int(x.pitch_index) for x in scale2]
            _send_json(self, 200, {"edo": edo, "pitches": out_pitches})
        except Exception:
            _send_json(self, 200, {})

    def _post_scale_retune(self) -> None:
        try:
            body = _json_body(self)
        except Exception:
            _send_json(self, 200, {})
            return

        edo_from = body.get("edoFrom")
        edo_to = body.get("edoTo")
        pitches = body.get("pitches")
        if not isinstance(edo_from, int) or not isinstance(edo_to, int) or not isinstance(pitches, list):
            _send_json(self, 200, {})
            return

        try:
            t_from = EDOTuning(edo_from)
            t_to = EDOTuning(edo_to)
            scale = t_from.scale([t_from.pitch(int(p)) for p in pitches if isinstance(p, int)])
            scale2 = scale.retune(t_to)
            out_pitches = [int(x.pitch_index) for x in scale2]
            _send_json(self, 200, {"edo": edo_to, "pitches": out_pitches})
        except Exception:
            _send_json(self, 200, {})


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--host", default="127.0.0.1")
    ap.add_argument("--port", type=int, default=3199)
    args = ap.parse_args()

    httpd = ThreadingHTTPServer((args.host, args.port), Handler)
    # Per-request logging is silenced via Handler.log_message; keep one
    # startup line so an operator can confirm the service bound (and on
    # which port) without reaching for journalctl. flush=True bypasses
    # Python's stdio buffering under systemd.
    print(
        f"xenharm_service v{Handler.server_version.split('/')[-1]} "
        f"listening on http://{args.host}:{args.port} "
        f"(endpoints: /health, /v1/note-names, /v1/interval-names, /v1/scale/rotate, /v1/scale/retune)",
        flush=True,
    )
    try:
        httpd.serve_forever()
    except KeyboardInterrupt:
        print("xenharm_service: interrupted; shutting down.", flush=True)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
