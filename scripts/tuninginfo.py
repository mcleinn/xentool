#!/usr/bin/env python3
"""
tuninginfo.py — print the playable pitch range and tuning summary for a
xentool layout file (.xtn / .wtn / .ltn).

Applies xentool's virtual-pitch formula

    absolute_pitch = (Chan - 1) * Edo + Key + PitchOffset
                     + (base_shift + user_shift) * Edo

across every active cell (cells with Chan == 0 are inactive and skipped),
then converts to frequency via

    freq_hz = C0_HZ * 2 ** (absolute_pitch / Edo)

with C0_HZ = 16.351597831287414 Hz (matches src/mts.rs and xenwooting).

`base_shift` mirrors the runtime convention each backend bakes into
its tuning construction:

    .xtn  →  Exquis backend  →  base_shift = 2  (BASE_OCTAVE_SHIFT in
                                                 src/exquis/hud_ctx.rs)
    .wtn  →  Wooting backend →  base_shift = 0  (raw formula)
    .ltn  →  Lumatone format →  base_shift = 0  (bare formula; xentool
                                                 itself has no Lumatone
                                                 runtime)

So all the pitches and frequency rows below are the *playing reality*
on each backend, not the bare INI-file numbers.

Usage:
    python scripts/tuninginfo.py wtn/edo53.wtn
    python scripts/tuninginfo.py xtn/edo31.xtn
    python scripts/tuninginfo.py mylayout.ltn
"""

from __future__ import annotations

import argparse
import math
import re
import sys
from pathlib import Path

# Match src/mts.rs::C0_HZ. A4=440 Hz reference.
C0_HZ = 16.351_597_831_287_414


def edo_freq_hz(edo: int, virtual_pitch: int) -> float:
    return C0_HZ * (2.0 ** (virtual_pitch / edo))


def midi_for_hz(hz: float) -> float:
    """12-EDO MIDI note equivalent (fractional). For human readability only."""
    return 69.0 + 12.0 * math.log2(hz / 440.0)


# Note name in 12-EDO for the fractional MIDI-equivalent — same convention
# as your DAW. Returns e.g. "A4 (+0c)" for an exact MIDI 69.
_PITCH_NAMES_12 = ["C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B"]


def midi_label(hz: float) -> str:
    midi_f = midi_for_hz(hz)
    midi_n = round(midi_f)
    cents = round((midi_f - midi_n) * 100)
    name = _PITCH_NAMES_12[midi_n % 12]
    octave = midi_n // 12 - 1  # MIDI 60 = C4
    sign = "+" if cents >= 0 else "-"
    return f"{name}{octave} ({sign}{abs(cents)}c)"


def parse_layout(path: Path) -> tuple[int, int, list[tuple[int, int, int, int]]]:
    """Return (edo, pitch_offset, cells).

    cells is a list of (board, idx, key, chan) tuples for every cell that
    has both Key_N and Chan_N defined (Chan == 0 cells are *kept* — the
    caller filters; we don't drop here so cell counts are accurate).
    """
    text = path.read_text(encoding="utf-8")

    # Split off the headerless prefix (Edo=, PitchOffset=) before [Board0].
    parts = re.split(r"(?=^\[)", text, flags=re.MULTILINE)
    header = parts[0] if parts and not parts[0].lstrip().startswith("[") else ""
    body = "".join(parts[1:]) if header else text

    edo: int | None = None
    pitch_offset: int = 0
    for raw in header.splitlines():
        line = raw.strip()
        if not line or line.startswith("#") or line.startswith(";"):
            continue
        if "=" not in line:
            continue
        k, _, v = line.partition("=")
        k_norm = k.strip().lower()
        v_norm = v.strip()
        if k_norm == "edo":
            edo = int(v_norm)
        elif k_norm == "pitchoffset":
            pitch_offset = int(v_norm)

    if edo is None:
        raise ValueError(f"{path}: no `Edo=` header field")

    # Walk sections; collect Key_N/Chan_N pairs per [BoardN].
    section_re = re.compile(r"^\[(?P<name>[^\]]+)\]\s*$", re.MULTILINE)
    key_re = re.compile(r"^\s*key_(\d+)\s*=\s*(-?\d+)\s*$", re.IGNORECASE | re.MULTILINE)
    chan_re = re.compile(r"^\s*chan_(\d+)\s*=\s*(-?\d+)\s*$", re.IGNORECASE | re.MULTILINE)

    sections: list[tuple[str, int, int]] = []
    for m in section_re.finditer(body):
        sections.append((m.group("name"), m.start(), m.end()))
    # Append a sentinel so the last section's body extends to EOF.
    section_bounds: list[tuple[str, int, int]] = []
    for i, (name, _start, end) in enumerate(sections):
        body_end = sections[i + 1][1] if i + 1 < len(sections) else len(body)
        section_bounds.append((name, end, body_end))

    cells: list[tuple[int, int, int, int]] = []
    for name, body_start, body_end in section_bounds:
        m = re.match(r"board\s*(\d+)$", name.strip(), re.IGNORECASE)
        if not m:
            continue
        board = int(m.group(1))
        chunk = body[body_start:body_end]
        keys: dict[int, int] = {}
        chans: dict[int, int] = {}
        for kmatch in key_re.finditer(chunk):
            keys[int(kmatch.group(1))] = int(kmatch.group(2))
        for cmatch in chan_re.finditer(chunk):
            chans[int(cmatch.group(1))] = int(cmatch.group(2))
        for idx, keyval in keys.items():
            chan = chans.get(idx, 0)
            cells.append((board, idx, keyval, chan))

    return edo, pitch_offset, cells


def detect_backend(path: Path) -> tuple[str, int]:
    """Backend name + base_shift from the file extension.

    Mirrors the runtime convention each xentool backend bakes in:
    Exquis adds 2 octaves silently (BASE_OCTAVE_SHIFT in
    src/exquis/hud_ctx.rs); Wooting uses the bare formula. Lumatone
    is treated as Wooting (no native xentool runtime)."""
    ext = path.suffix.lower()
    if ext == ".xtn":
        return ("Exquis", 2)
    if ext == ".wtn":
        return ("Wooting", 0)
    if ext == ".ltn":
        return ("Lumatone", 0)
    return ("unknown", 0)


def summarise(path: Path) -> int:
    try:
        edo, offset, cells = parse_layout(path)
    except (OSError, ValueError) as exc:
        print(f"error: {exc}", file=sys.stderr)
        return 2

    if edo <= 0:
        print(f"error: Edo must be > 0; got {edo}", file=sys.stderr)
        return 2

    active = [(b, i, k, c) for (b, i, k, c) in cells if c != 0]
    if not active:
        print(f"warning: {path} has no active cells (every Chan == 0)", file=sys.stderr)
        return 1

    backend, base_shift = detect_backend(path)

    # Per-cell absolute pitch at user_shift=0 with the backend's
    # base_shift folded in. Every downstream number reflects the
    # playing reality, not the bare INI-file values.
    cell_pitch: list[tuple[int, int, int, int, int]] = [
        (b, i, k, c, (c - 1) * edo + k + offset + base_shift * edo)
        for (b, i, k, c) in active
    ]
    pitches_by_cell: list[int] = [p for (_, _, _, _, p) in cell_pitch]
    distinct = sorted(set(pitches_by_cell))
    cell_pitch.sort(key=lambda t: t[4])
    lo_cell = cell_pitch[0]
    hi_cell = cell_pitch[-1]
    boards = sorted({b for (b, _, _, _) in active})
    by_board: dict[int, int] = {b: 0 for b in boards}
    for (b, _, _, _) in active:
        by_board[b] += 1

    lo, hi = distinct[0], distinct[-1]
    lo_hz, hi_hz = edo_freq_hz(edo, lo), edo_freq_hz(edo, hi)
    span_steps = hi - lo
    span_octaves = span_steps / edo
    span_cents = span_steps * 1200 / edo

    # "Average pitch" candidates the rest of the test rig might want.
    median_pitch = distinct[len(distinct) // 2]
    median_hz = edo_freq_hz(edo, median_pitch)

    base_shift_label = f"{base_shift:+d}" if base_shift else " 0"
    print(f"file:               {path.name}")
    print(f"backend:            {backend}  (base_shift = {base_shift_label})")
    print(f"edo:                {edo}")
    print(f"pitch offset:       {offset}")
    print(f"boards:             {len(boards)}  ({', '.join(f'board{b}' for b in boards)})")
    print(f"active cells:       {len(active)}  "
          f"({', '.join(f'board{b}={n}' for b, n in by_board.items())})")
    print(f"distinct pitches:   {len(distinct)}")
    print()
    print(f"lowest absolute pitch:  {lo:>5}   "
          f"{lo_hz:>10.3f} Hz   ~{midi_label(lo_hz)}")
    print(f"highest absolute pitch: {hi:>5}   "
          f"{hi_hz:>10.3f} Hz   ~{midi_label(hi_hz)}")
    print(f"median absolute pitch:  {median_pitch:>5}   "
          f"{median_hz:>10.3f} Hz   ~{midi_label(median_hz)}")
    print()
    print(f"range:              {span_steps} EDO{edo} steps  "
          f"=  {span_octaves:.3f} octaves  =  {span_cents:.0f} cents")
    print(f"step size:          {1200 / edo:.2f} cents per EDO{edo} step")

    # Per-user-shift coverage. user_shift is the value the player sees
    # ("octave shift" ±N from the live UI / control bar). It's stacked
    # on top of base_shift, so user_shift=0 is the runtime baseline.
    print()
    print("user-shift coverage (lowest .. median .. highest, base_shift already applied):")
    print(f"  {'shift':>5}   {'lo Hz':>10}   {'med Hz':>10}   {'hi Hz':>10}   "
          f"{'lo':>10}  {'med':>10}  {'hi':>10}")
    for user_shift in range(-3, 4):
        s_lo = lo + user_shift * edo
        s_med = median_pitch + user_shift * edo
        s_hi = hi + user_shift * edo
        s_lo_hz = edo_freq_hz(edo, s_lo)
        s_med_hz = edo_freq_hz(edo, s_med)
        s_hi_hz = edo_freq_hz(edo, s_hi)
        sign = f"{user_shift:+d}" if user_shift else " 0"
        print(f"  {sign:>5}   "
              f"{s_lo_hz:>10.2f}   {s_med_hz:>10.2f}   {s_hi_hz:>10.2f}   "
              f"{midi_label(s_lo_hz):>10}  {midi_label(s_med_hz):>10}  {midi_label(s_hi_hz):>10}")

    # Formula footer + extreme cells. user_shift is what's spelled out
    # in the formula because base_shift is already folded into
    # absolute_pitch above; we surface it as a constant so the math
    # is reproducible from the file alone.
    print()
    print("formula: absolute_pitch = (Chan - 1) * Edo + Key + PitchOffset"
          " + (base_shift + user_shift) * Edo")
    print(f"         Edo = {edo},  PitchOffset = {offset},  "
          f"base_shift = {base_shift_label}  ({backend})")
    print()

    lo_b, lo_i, lo_k, lo_c, _ = lo_cell
    hi_b, hi_i, hi_k, hi_c, _ = hi_cell
    print(f"lowest  cell: board{lo_b} idx{lo_i:>3}  Chan={lo_c:>2}  Key={lo_k:>3}")
    print(f"highest cell: board{hi_b} idx{hi_i:>3}  Chan={hi_c:>2}  Key={hi_k:>3}")
    print()
    print("per-user-shift absolute pitches for those two cells:")
    print(f"  {'shift':>5}   {'lo pitch':>9}  {'lo Hz':>10}  {'lo note':>12}   "
          f"{'hi pitch':>9}  {'hi Hz':>10}  {'hi note':>12}")
    for user_shift in range(-3, 4):
        total_shift = base_shift + user_shift
        ap_lo = (lo_c - 1) * edo + lo_k + offset + total_shift * edo
        ap_hi = (hi_c - 1) * edo + hi_k + offset + total_shift * edo
        f_lo = edo_freq_hz(edo, ap_lo)
        f_hi = edo_freq_hz(edo, ap_hi)
        sign = f"{user_shift:+d}" if user_shift else " 0"
        print(f"  {sign:>5}   {ap_lo:>9}  {f_lo:>10.2f}  {midi_label(f_lo):>12}   "
              f"{ap_hi:>9}  {f_hi:>10.2f}  {midi_label(f_hi):>12}")
    return 0


def main() -> int:
    ap = argparse.ArgumentParser(
        description="Print pitch range / tuning summary for a xentool layout file.",
    )
    ap.add_argument("file", type=Path, help=".xtn / .wtn / .ltn layout file")
    args = ap.parse_args()
    return summarise(args.file)


if __name__ == "__main__":
    raise SystemExit(main())
