#!/usr/bin/env python3
"""
analyze_renders.py — turn each WAV in renders/ into:
  - <wav>.json   per-render scalar features + 1D trajectories
  - <wav>.png    small spectrogram thumbnail (no axes/text)
and consolidate scalars across all renders into:
  - summary.csv          one row per render, joinable with ratings.csv
  - ratings.csv.template one row per render, blank rating column

Run *after* the SC test rig has produced WAVs. Defaults to ./renders
relative to this script.

Usage:
    python analyze_renders.py
    python analyze_renders.py /path/to/renders
"""

from __future__ import annotations

import argparse
import csv
import json
import re
import sys
from pathlib import Path

import numpy as np
from scipy.io import wavfile
from scipy.signal import stft, find_peaks
from PIL import Image


# Filename schema:
#   horiz_<step>_freq_<hz>.wav
#   vert_<step>_cc_<cc>.wav
HORIZ_RE = re.compile(r"^horiz_(\d+)_freq_([0-9.]+)\.wav$", re.IGNORECASE)
VERT_RE = re.compile(r"^vert_(\d+)_cc_(\d+)\.wav$", re.IGNORECASE)


def parse_filename(name: str) -> dict | None:
    m = HORIZ_RE.match(name)
    if m:
        return {"test": "horiz", "step": int(m.group(1)), "freq_hz": float(m.group(2)),
                "cc74": None}
    m = VERT_RE.match(name)
    if m:
        return {"test": "vert", "step": int(m.group(1)), "freq_hz": None,
                "cc74": int(m.group(2))}
    return None


def load_mono(path: Path) -> tuple[np.ndarray, int]:
    sr, data = wavfile.read(str(path))
    if data.ndim == 2:
        data = data.mean(axis=1)
    # Normalize to float32 in [-1, 1]
    if np.issubdtype(data.dtype, np.integer):
        info = np.iinfo(data.dtype)
        peak = max(abs(info.min), abs(info.max))
        data = data.astype(np.float32) / peak
    else:
        data = data.astype(np.float32)
    return data, sr


def db(x: np.ndarray, eps: float = 1e-12) -> np.ndarray:
    return 20.0 * np.log10(np.maximum(np.abs(x), eps))


def hz_band_energy_db(mag: np.ndarray, freqs: np.ndarray, lo: float, hi: float) -> float:
    mask = (freqs >= lo) & (freqs < hi)
    if not mask.any():
        return float("-inf")
    e = (mag[mask] ** 2).sum()
    total = (mag ** 2).sum() + 1e-20
    return 10.0 * float(np.log10((e / total) + 1e-20))


def spectral_centroid(mag: np.ndarray, freqs: np.ndarray) -> np.ndarray:
    # Per-frame centroid; mag shape (freq, time).
    s = mag.sum(axis=0)
    s = np.where(s == 0, 1e-20, s)
    return (mag * freqs[:, None]).sum(axis=0) / s


def spectral_rolloff(mag: np.ndarray, freqs: np.ndarray, frac: float = 0.85) -> np.ndarray:
    # Per-frame frequency below which `frac` of total energy lies.
    energy = mag ** 2
    cum = energy.cumsum(axis=0)
    total = cum[-1, :]
    total = np.where(total == 0, 1e-20, total)
    target = total * frac
    # First freq bin where cum >= target
    out = np.empty(mag.shape[1], dtype=np.float64)
    for t in range(mag.shape[1]):
        idx = np.searchsorted(cum[:, t], target[t])
        idx = min(idx, len(freqs) - 1)
        out[t] = freqs[idx]
    return out


def spectral_flatness(mag: np.ndarray) -> np.ndarray:
    # Per-frame: geometric mean / arithmetic mean of magnitude. 1.0 = white
    # noise, ~0.0 = single sinusoid.
    eps = 1e-12
    log_mean = np.log(mag + eps).mean(axis=0)
    arith = mag.mean(axis=0) + eps
    return np.exp(log_mean) / arith


def envelope(audio: np.ndarray, sr: int, hop_ms: float = 5.0) -> tuple[np.ndarray, np.ndarray]:
    hop = max(1, int(sr * hop_ms / 1000.0))
    # Window-RMS envelope at hop spacing.
    n = (len(audio) // hop) * hop
    if n == 0:
        return np.array([0.0]), np.array([0.0])
    blocks = audio[:n].reshape(-1, hop)
    rms = np.sqrt((blocks ** 2).mean(axis=1) + 1e-20)
    t = np.arange(rms.size) * (hop / sr)
    return t, rms


def find_top_peaks(avg_mag: np.ndarray, freqs: np.ndarray, n: int = 5) -> list[tuple[float, float]]:
    """Pick the n strongest spectral peaks from the time-averaged magnitude
    spectrum. Returns list of (freq_hz, mag_db). Peaks must be local maxima
    above a noise floor."""
    avg_db = db(avg_mag)
    # Prominence in dB to filter chaff. 6 dB is conservative.
    peaks_idx, _ = find_peaks(avg_db, prominence=6.0)
    if peaks_idx.size == 0:
        return []
    # Sort by magnitude, take top n.
    order = np.argsort(avg_db[peaks_idx])[::-1]
    out = []
    for i in order[:n]:
        idx = peaks_idx[i]
        out.append((float(freqs[idx]), float(avg_db[idx])))
    return out


def analyse(path: Path) -> dict:
    name = path.name
    meta = parse_filename(name) or {"test": None, "step": None, "freq_hz": None, "cc74": None}

    audio, sr = load_mono(path)
    if audio.size == 0:
        return {"filename": name, **meta, "error": "empty audio"}

    # STFT
    n_fft = 2048
    hop = 512
    f, t, Zxx = stft(audio, fs=sr, nperseg=n_fft, noverlap=n_fft - hop, padded=False, boundary=None)
    mag = np.abs(Zxx)

    # Restrict spectral analysis to the sustain window (skip onset / tail).
    # Onsets settle by ~50 ms; we don't analyze post-gate-off.
    sustain_lo = 0.2
    sustain_hi = 1.8
    sustain_mask = (t >= sustain_lo) & (t < sustain_hi)
    if not sustain_mask.any():
        sustain_mask = np.ones_like(t, dtype=bool)
    sus_mag = mag[:, sustain_mask]
    sus_t = t[sustain_mask]

    # Time-averaged spectrum (for peak picking)
    avg_mag = sus_mag.mean(axis=1)

    # Per-frame features over sustain window
    centroid = spectral_centroid(sus_mag, f)
    rolloff = spectral_rolloff(sus_mag, f, frac=0.85)
    flatness = spectral_flatness(sus_mag)

    # Amplitude features (whole-file)
    rms_full = np.sqrt((audio ** 2).mean() + 1e-20)
    peak_amp = float(np.max(np.abs(audio)))
    crest_db = float(db(np.array([peak_amp])) - db(np.array([rms_full])))
    rms_dbfs = float(db(np.array([rms_full]))[0])
    peak_dbfs = float(db(np.array([peak_amp]))[0])

    # Envelope (whole file)
    env_t, env_rms = envelope(audio, sr, hop_ms=5.0)
    env_db = db(env_rms)
    if env_rms.size > 0:
        peak_idx = int(np.argmax(env_rms))
        peak_val = env_rms[peak_idx]
        # attack: time from start to crossing 90% of peak
        thresh = peak_val * 0.9
        rise_idx = np.argmax(env_rms >= thresh)
        attack_ms = float(env_t[rise_idx] * 1000.0)
        # decay-to-half: time from peak until env_rms first drops below peak/2
        half = peak_val * 0.5
        post = env_rms[peak_idx:]
        below = np.where(post < half)[0]
        if below.size > 0:
            decay_to_half_ms = float((env_t[peak_idx + below[0]] - env_t[peak_idx]) * 1000.0)
        else:
            decay_to_half_ms = float((env_t[-1] - env_t[peak_idx]) * 1000.0)
    else:
        attack_ms = 0.0
        decay_to_half_ms = 0.0

    # Sustain-band scalar summaries
    centroid_mean_hz = float(centroid.mean())
    centroid_max_hz = float(centroid.max())
    centroid_max_at_s = float(sus_t[int(np.argmax(centroid))])
    rolloff_mean_hz = float(rolloff.mean())
    rolloff_max_hz = float(rolloff.max())
    flatness_mean = float(flatness.mean())
    flatness_max = float(flatness.max())

    # HF energy ratios (relative to whole spectrum, dB)
    hf_4k_8k_db = hz_band_energy_db(avg_mag, f, 4000, 8000)
    hf_8k_16k_db = hz_band_energy_db(avg_mag, f, 8000, 16000)

    # Top spectral peaks (averaged over sustain window)
    peaks = find_top_peaks(avg_mag, f, n=5)

    # ---- compose the per-render JSON / scalar dict ----
    scalars = {
        "filename": name,
        **meta,
        "rms_dbfs": rms_dbfs,
        "peak_dbfs": peak_dbfs,
        "crest_db": crest_db,
        "centroid_mean_hz": centroid_mean_hz,
        "centroid_max_hz": centroid_max_hz,
        "centroid_max_at_s": centroid_max_at_s,
        "rolloff_85_mean_hz": rolloff_mean_hz,
        "rolloff_85_max_hz": rolloff_max_hz,
        "flatness_mean": flatness_mean,
        "flatness_max": flatness_max,
        "hf_energy_4k_8k_db": hf_4k_8k_db,
        "hf_energy_8k_16k_db": hf_8k_16k_db,
        "attack_ms": attack_ms,
        "decay_to_half_ms": decay_to_half_ms,
    }
    for i, (pf, pm) in enumerate(peaks, 1):
        scalars[f"peak{i}_freq_hz"] = pf
        scalars[f"peak{i}_mag_db"] = pm
    # Pad missing peak slots so summary.csv has consistent columns
    for i in range(len(peaks) + 1, 6):
        scalars[f"peak{i}_freq_hz"] = None
        scalars[f"peak{i}_mag_db"] = None

    # ---- build the per-file JSON (scalars + downsampled trajectories) ----
    # Downsample trajectories to ~20 frames so the JSON stays small.
    def downsample(arr: np.ndarray, target: int = 20) -> list[float]:
        if arr.size <= target:
            return arr.astype(float).tolist()
        idx = np.linspace(0, arr.size - 1, target).astype(int)
        return arr[idx].astype(float).tolist()

    nested = dict(scalars)
    nested["sample_rate"] = int(sr)
    nested["duration_s"] = float(len(audio) / sr)
    nested["sustain_window_s"] = [sustain_lo, sustain_hi]
    nested["centroid_trajectory_hz"] = downsample(centroid, 20)
    nested["rolloff_85_trajectory_hz"] = downsample(rolloff, 20)
    nested["flatness_trajectory"] = downsample(flatness, 20)
    nested["envelope_db"] = downsample(env_db, 40)
    nested["top_peaks"] = [{"freq_hz": pf, "mag_db": pm} for pf, pm in peaks]

    return {"scalars": scalars, "nested": nested, "audio": audio, "sr": sr,
            "stft": (f, t, mag)}


def write_spectrogram_png(out_path: Path, stft_data, width: int = 256, height: int = 128) -> None:
    f, t, mag = stft_data
    # Magnitude → dB, normalize for image. Crop to <= sr/2 (already there)
    # and the audible band (we'll keep full Nyquist so the picture shows
    # any HF activity that's perceptually relevant).
    db_mag = db(mag)
    # Clip to a reasonable dynamic range (e.g. -90 dB ... 0 dB above max bin).
    top = float(db_mag.max())
    floor = top - 80.0
    img = np.clip(db_mag, floor, top)
    img = (img - floor) / (top - floor + 1e-12)  # [0..1]

    # Resize via PIL (bilinear). PIL expects (width, height) and uint8.
    # Source array is (freq, time); we want time on x, freq on y, with low
    # freq at the bottom (so flip vertically).
    src = (img * 255.0).astype(np.uint8)
    src = np.flipud(src)  # low freq → bottom
    pil = Image.fromarray(src, mode="L")
    pil = pil.resize((width, height), Image.BILINEAR)

    # Apply a simple colormap (viridis-ish) so the spectrogram is more
    # legible than grayscale. Avoid matplotlib dep — hand-roll a 256-stop
    # gradient through black → blue → green → yellow → white.
    lut = np.zeros((256, 3), dtype=np.uint8)
    for i in range(256):
        x = i / 255.0
        if x < 0.25:
            r, g, b = 0, 0, int(x / 0.25 * 255)
        elif x < 0.5:
            r = 0
            g = int((x - 0.25) / 0.25 * 255)
            b = 255 - int((x - 0.25) / 0.25 * 64)
        elif x < 0.75:
            r = int((x - 0.5) / 0.25 * 255)
            g = 255
            b = 191 - int((x - 0.5) / 0.25 * 191)
        else:
            r = 255
            g = 255
            b = int((x - 0.75) / 0.25 * 255)
        lut[i] = [r, g, b]
    arr = np.asarray(pil)
    rgb = lut[arr]
    Image.fromarray(rgb, mode="RGB").save(str(out_path))


def main() -> int:
    ap = argparse.ArgumentParser(description="Analyze tanpura render WAVs.")
    ap.add_argument("renders_dir", nargs="?",
                    default=str(Path(__file__).parent / "renders"),
                    help="directory containing the .wav files")
    args = ap.parse_args()

    renders_dir = Path(args.renders_dir)
    if not renders_dir.is_dir():
        print(f"error: {renders_dir} is not a directory", file=sys.stderr)
        return 2

    wavs = sorted(renders_dir.glob("*.wav"))
    if not wavs:
        print(f"error: no .wav files in {renders_dir}", file=sys.stderr)
        return 2

    print(f"analyzing {len(wavs)} renders in {renders_dir} ...")
    rows: list[dict] = []
    for wav in wavs:
        try:
            result = analyse(wav)
        except Exception as exc:
            print(f"  {wav.name}: ERROR {exc}", file=sys.stderr)
            continue
        scalars = result["scalars"]
        nested = result["nested"]
        rows.append(scalars)

        # Write per-render .json (drop bulky raw arrays before serializing).
        json_path = wav.with_suffix(".json")
        with json_path.open("w", encoding="utf-8") as fh:
            json.dump(nested, fh, indent=2)

        # Write per-render small spectrogram .png.
        png_path = wav.with_suffix(".png")
        try:
            write_spectrogram_png(png_path, result["stft"])
        except Exception as exc:
            print(f"  {wav.name}: spectrogram failed ({exc})", file=sys.stderr)

        print(f"  {wav.name}: centroid_mean={scalars['centroid_mean_hz']:.0f} Hz,  "
              f"flatness_mean={scalars['flatness_mean']:.3f},  "
              f"hf4-8={scalars['hf_energy_4k_8k_db']:+.1f} dB")

    if not rows:
        print("no analyzable renders", file=sys.stderr)
        return 2

    # ---- summary.csv ----
    fieldnames = list(rows[0].keys())
    summary_csv = renders_dir / "summary.csv"
    with summary_csv.open("w", newline="", encoding="utf-8") as fh:
        w = csv.DictWriter(fh, fieldnames=fieldnames)
        w.writeheader()
        for r in rows:
            w.writerow(r)
    print(f"wrote {summary_csv} ({len(rows)} rows × {len(fieldnames)} cols)")

    # ---- ratings.csv.template ----
    rating_csv = renders_dir / "ratings.csv.template"
    with rating_csv.open("w", newline="", encoding="utf-8") as fh:
        w = csv.writer(fh)
        w.writerow(["filename", "rating", "notes"])
        for r in rows:
            w.writerow([r["filename"], "", ""])
    print(f"wrote {rating_csv}  (copy to ratings.csv and fill in rating column 1..5)")

    return 0


if __name__ == "__main__":
    raise SystemExit(main())
