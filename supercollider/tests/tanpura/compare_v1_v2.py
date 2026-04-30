#!/usr/bin/env python3
"""
compare_v1_v2.py — analytical diff between the v1 (live patch) and v2
(SynthDef fix applied) renders.

Two comparisons run side-by-side:

  A. SWEEP (22 renders).  Goal: prove the fix doesn't change the
     clean character. Per-feature diffs should be small.

  B. SCRATCHY FRAMES (4 renders).  Goal: prove the fix removes the
     scratch signature. Features should move toward the clean baseline.

Source dirs:
  v1 sweep:    renders/tests_without_scratching/summary.csv
  v2 sweep:    v2_renders/summary.csv
  v1 frames:   replays/summary.csv
  v2 frames:   v2_replays/summary.csv
"""

from __future__ import annotations

import csv
import math
import sys
from pathlib import Path


HERE = Path(__file__).parent
V1_SWEEP = HERE / "renders" / "tests_without_scratching" / "summary.csv"
V2_SWEEP = HERE / "v2_renders" / "summary.csv"
V1_FRAMES = HERE / "replays" / "summary.csv"
V2_FRAMES = HERE / "v2_replays" / "summary.csv"

FEATURES = [
    "rms_dbfs", "peak_dbfs", "crest_db",
    "centroid_mean_hz", "centroid_max_hz",
    "rolloff_85_mean_hz", "rolloff_85_max_hz",
    "flatness_mean", "flatness_max",
    "hf_energy_4k_8k_db", "hf_energy_8k_16k_db",
]


def load_summary(path: Path) -> list[dict]:
    with path.open(encoding="utf-8") as fh:
        return list(csv.DictReader(fh))


def to_float(v):
    if v in (None, "", "None"): return None
    try:
        return float(v)
    except (TypeError, ValueError):
        return None


def by_filename(rows: list[dict]) -> dict[str, dict]:
    return {r["filename"]: r for r in rows}


def per_file_diff(v1_rows: list[dict], v2_rows: list[dict], features: list[str], label: str) -> None:
    """Print per-file v1 vs v2 numbers for each feature, joined by filename."""
    a = by_filename(v1_rows)
    b = by_filename(v2_rows)
    common = sorted(set(a) & set(b))
    if not common:
        print(f"  {label}: no overlapping filenames!"); return
    print(f"\n=== {label} ({len(common)} files matched) ===")
    print(f"  {'feature':<22} {'v1 mean':>10} {'v2 mean':>10} {'delta':>10}  per-file deltas (v2-v1) ...")
    for feat in features:
        v1_vals = [to_float(a[f].get(feat)) for f in common]
        v2_vals = [to_float(b[f].get(feat)) for f in common]
        pairs = [(x, y) for x, y in zip(v1_vals, v2_vals) if x is not None and y is not None]
        if not pairs:
            continue
        v1_mean = sum(p[0] for p in pairs) / len(pairs)
        v2_mean = sum(p[1] for p in pairs) / len(pairs)
        delta = v2_mean - v1_mean
        # show min/max per-file delta for spread
        deltas = [p[1] - p[0] for p in pairs]
        dmin, dmax = min(deltas), max(deltas)
        # absolute mean delta (magnitude of change averaged across files)
        absmean = sum(abs(d) for d in deltas) / len(deltas)
        print(f"  {feat:<22} {fmt(v1_mean):>10} {fmt(v2_mean):>10} {fmt(delta):>10}  "
              f"per-file: min={fmt(dmin)} max={fmt(dmax)} |mean|={fmt(absmean)}")


def fmt(v: float) -> str:
    if not isinstance(v, (int, float)) or not math.isfinite(v): return "nan"
    if abs(v) >= 1000: return f"{v:,.0f}"
    if abs(v) >= 10:   return f"{v:.1f}"
    return f"{v:.3f}"


def per_file_table(v1_rows, v2_rows, features, label):
    """For 4-frame comparison: show every file."""
    a = by_filename(v1_rows)
    b = by_filename(v2_rows)
    common = sorted(set(a) & set(b))
    print(f"\n=== {label} per-file v1 -> v2 ===")
    for feat in features:
        print(f"  {feat}:")
        for fn in common:
            v1 = to_float(a[fn].get(feat))
            v2 = to_float(b[fn].get(feat))
            if v1 is None or v2 is None: continue
            d = v2 - v1
            arrow = "(down)" if d < -1e-6 else "(up)" if d > 1e-6 else "(=)"
            print(f"    {fn:<48} v1={fmt(v1):>10}  v2={fmt(v2):>10}  delta={fmt(d):>10}  {arrow}")


def main() -> int:
    for p in (V1_SWEEP, V2_SWEEP, V1_FRAMES, V2_FRAMES):
        if not p.exists():
            print(f"missing: {p}", file=sys.stderr); return 2

    v1_sweep = load_summary(V1_SWEEP)
    v2_sweep = load_summary(V2_SWEEP)
    v1_frames = load_summary(V1_FRAMES)
    v2_frames = load_summary(V2_FRAMES)

    print(f"v1 sweep:   {len(v1_sweep)} rows ({V1_SWEEP.relative_to(HERE)})")
    print(f"v2 sweep:   {len(v2_sweep)} rows ({V2_SWEEP.relative_to(HERE)})")
    print(f"v1 frames:  {len(v1_frames)} rows ({V1_FRAMES.relative_to(HERE)})")
    print(f"v2 frames:  {len(v2_frames)} rows ({V2_FRAMES.relative_to(HERE)})")

    # --- Comparison A: 22-render sweep ---
    # Goal: minimal change.
    per_file_diff(v1_sweep, v2_sweep, FEATURES,
                  label="SWEEP — clean character preservation")

    # --- Comparison B: 4 frozen frames ---
    # Goal: scratch signature should drop dramatically.
    per_file_table(v1_frames, v2_frames, FEATURES,
                   label="FROZEN FRAMES — scratch removal")

    # --- Bonus: are the frozen frames now closer to the clean baseline? ---
    print("\n=== sanity: scratchy v2 features vs v1 clean BASELINE ===")
    print("  (does v2 frame data look like the v1 clean sweep would?)")
    # baseline = v1 sweep mean ± std
    for feat in FEATURES:
        v1_clean_vals = [to_float(r.get(feat)) for r in v1_sweep]
        v1_clean_vals = [v for v in v1_clean_vals if v is not None and math.isfinite(v)]
        v2_frame_vals = [to_float(r.get(feat)) for r in v2_frames]
        v2_frame_vals = [v for v in v2_frame_vals if v is not None and math.isfinite(v)]
        if not v1_clean_vals or not v2_frame_vals: continue
        clean_mean = sum(v1_clean_vals) / len(v1_clean_vals)
        clean_max = max(v1_clean_vals)
        v2_frame_mean = sum(v2_frame_vals) / len(v2_frame_vals)
        v2_frame_max = max(v2_frame_vals)
        # Inside the clean envelope?
        in_envelope = "INSIDE" if v2_frame_max <= clean_max * 1.5 else "STILL OUT"
        print(f"  {feat:<22} clean mean={fmt(clean_mean):>10} max={fmt(clean_max):>10}    "
              f"v2 frames mean={fmt(v2_frame_mean):>10} max={fmt(v2_frame_max):>10}    "
              f"{in_envelope}")

    return 0


if __name__ == "__main__":
    raise SystemExit(main())
