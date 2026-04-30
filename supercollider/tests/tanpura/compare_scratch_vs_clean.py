#!/usr/bin/env python3
"""
compare_scratch_vs_clean.py — analytical diff between the 4 scratchy
frozen-frame renders (replays/) and the 22 baseline static-sweep
renders (renders/tests_without_scratching/) that the user judged not
scratchy.

Goal: identify, with numbers and not speculation, which spectral
features separate the two groups, and which terms in the SynthDef
plausibly produce those features.

Outputs:
  - stdout: per-feature mean ± std for each group, with effect size
  - stdout: top-N spectral peak comparison
  - feature_diff.csv next to this script: full numerical diff
"""

from __future__ import annotations

import csv
import json
import math
import sys
from pathlib import Path

import numpy as np


HERE = Path(__file__).parent
SCRATCH_DIR = HERE / "replays"
CLEAN_DIR = HERE / "renders" / "tests_without_scratching"

NUMERIC_FEATURES = [
    "rms_dbfs", "peak_dbfs", "crest_db",
    "centroid_mean_hz", "centroid_max_hz", "centroid_max_at_s",
    "rolloff_85_mean_hz", "rolloff_85_max_hz",
    "flatness_mean", "flatness_max",
    "hf_energy_4k_8k_db", "hf_energy_8k_16k_db",
    "attack_ms", "decay_to_half_ms",
    "peak1_freq_hz", "peak1_mag_db",
    "peak2_freq_hz", "peak2_mag_db",
    "peak3_freq_hz", "peak3_mag_db",
    "peak4_freq_hz", "peak4_mag_db",
    "peak5_freq_hz", "peak5_mag_db",
]


def load_summary(path: Path) -> list[dict]:
    rows = []
    with path.open(encoding="utf-8") as fh:
        for r in csv.DictReader(fh):
            rows.append(r)
    return rows


def to_float(v) -> float | None:
    if v in (None, "", "None"):
        return None
    try:
        return float(v)
    except (TypeError, ValueError):
        return None


def stats(rows: list[dict], feature: str) -> tuple[float, float, int]:
    """Returns (mean, std, n) for a numeric feature, dropping None values."""
    vals = [to_float(r.get(feature)) for r in rows]
    vals = [v for v in vals if v is not None and math.isfinite(v)]
    if not vals:
        return float("nan"), float("nan"), 0
    arr = np.asarray(vals)
    return float(arr.mean()), float(arr.std(ddof=0)), len(vals)


def cohens_d(m1: float, s1: float, n1: int, m2: float, s2: float, n2: int) -> float:
    """Pooled-std effect size. Big if the groups separate cleanly on this
    feature; small if they overlap."""
    if n1 == 0 or n2 == 0 or not math.isfinite(m1) or not math.isfinite(m2):
        return float("nan")
    pooled_var = ((n1 - 1) * s1 ** 2 + (n2 - 1) * s2 ** 2) / max(1, n1 + n2 - 2)
    if pooled_var <= 0:
        return float("nan")
    return (m1 - m2) / math.sqrt(pooled_var)


def main() -> int:
    if not SCRATCH_DIR.exists():
        print(f"missing: {SCRATCH_DIR}", file=sys.stderr); return 2
    if not CLEAN_DIR.exists():
        print(f"missing: {CLEAN_DIR}", file=sys.stderr); return 2

    scratch_rows = load_summary(SCRATCH_DIR / "summary.csv")
    clean_rows = load_summary(CLEAN_DIR / "summary.csv")
    print(f"scratch: {len(scratch_rows)} rows  ({SCRATCH_DIR})")
    print(f"clean:   {len(clean_rows)} rows  ({CLEAN_DIR})")
    print()

    # === Per-feature comparison ===
    print(f"{'feature':<24}  {'scratch':>14}  {'clean':>14}  {'diff':>10}  {'cohens d':>9}")
    print("-" * 80)

    diff_table: list[dict] = []
    for feat in NUMERIC_FEATURES:
        ms, ss, ns = stats(scratch_rows, feat)
        mc, sc, nc = stats(clean_rows, feat)
        d = cohens_d(ms, ss, ns, mc, sc, nc)
        diff = ms - mc if (math.isfinite(ms) and math.isfinite(mc)) else float("nan")

        def fmt(v):
            if not math.isfinite(v): return "  nan"
            if abs(v) >= 1000: return f"{v:,.0f}"
            if abs(v) >= 10:   return f"{v:.1f}"
            return f"{v:.3f}"

        print(f"{feat:<24}  "
              f"{fmt(ms):>8}±{fmt(ss):<5}  "
              f"{fmt(mc):>8}±{fmt(sc):<5}  "
              f"{fmt(diff):>10}  "
              f"{fmt(d):>9}")

        diff_table.append({
            "feature": feat,
            "scratch_mean": ms, "scratch_std": ss, "scratch_n": ns,
            "clean_mean": mc, "clean_std": sc, "clean_n": nc,
            "diff": diff, "cohens_d": d,
        })

    print()

    # === Sort by absolute effect size to spotlight the differentiators ===
    print("=== top 8 features by |Cohen's d| (most decisive separators) ===")
    sorted_d = sorted(
        [d for d in diff_table if math.isfinite(d["cohens_d"])],
        key=lambda x: abs(x["cohens_d"]), reverse=True,
    )
    for d in sorted_d[:8]:
        direction = "scratch HIGHER" if d["cohens_d"] > 0 else "scratch LOWER "
        print(f"  {d['feature']:<24}  d={d['cohens_d']:+.2f}  {direction}  "
              f"(scratch={d['scratch_mean']:.2f}, clean={d['clean_mean']:.2f})")
    print()

    # === Detailed per-render scratchy peak inspection ===
    print("=== per-render top-5 spectral peaks (scratchy renders) ===")
    for r in scratch_rows:
        peaks = []
        for i in range(1, 6):
            f = to_float(r.get(f"peak{i}_freq_hz"))
            m = to_float(r.get(f"peak{i}_mag_db"))
            if f is not None and m is not None:
                peaks.append((f, m))
        print(f"  {r['filename']}:  "
              + "  ".join(f"{f:.0f}Hz/{m:+.1f}dB" for f, m in peaks))

    print()
    print("=== summary peak-band coverage across scratchy renders ===")
    bands = [(0, 500), (500, 1500), (1500, 4000), (4000, 8000), (8000, 16000), (16000, 22050)]
    for lo, hi in bands:
        hits = 0
        total = 0
        for r in scratch_rows:
            for i in range(1, 6):
                f = to_float(r.get(f"peak{i}_freq_hz"))
                if f is None: continue
                total += 1
                if lo <= f < hi:
                    hits += 1
        print(f"  {lo:>5}–{hi:<5} Hz: {hits}/{total} of scratch peaks")

    # === Write feature_diff.csv ===
    out = HERE / "feature_diff.csv"
    fieldnames = list(diff_table[0].keys())
    with out.open("w", newline="", encoding="utf-8") as fh:
        w = csv.DictWriter(fh, fieldnames=fieldnames)
        w.writeheader()
        for row in diff_table:
            w.writerow(row)
    print()
    print(f"wrote feature_diff.csv -> {out}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
