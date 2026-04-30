# Tanpura test rig

Systematic batch-render harness for the `mpeTanpuraXentool` SynthDef.
Generates short isolated WAV renders sweeping pitch (horizontal) and
brightness (vertical), extracts spectral features into structured data,
and produces a small spectrogram thumbnail per render. The intent is to
let an LLM correlate user audio ratings against numerical features so
it can propose targeted SynthDef edits — e.g. to reduce the "scratch"
the patch exhibits at high brightness × high pitch.

The SynthDef body is copied **verbatim** from
`supercollider/mpe_tanpura_xentool.scd` (lines 30–85). When the live
patch is updated, copy the updated body back into
`tanpura_test_rig.scd` before re-running the test — otherwise the
experiment compares a stale model to itself.

## Files in this folder

| File                  | Purpose |
|-----------------------|---------|
| `tanpura_test_rig.scd`| sclang harness; renders 22 WAVs (11 horizontal + 11 vertical) via realtime `s.record` |
| `analyze_renders.py`  | Python analyzer; per-WAV `.json` + `.png` spectrogram + flat `summary.csv` + `ratings.csv.template` |
| `renders/`            | output directory (created at runtime; not committed) |
| `README.md`           | this file |

## The two tests

### Horizontal (pitch sweep)
- 11 log-spaced frequencies from **A0 = 27.5 Hz to C8 = 4186 Hz** — the full 88-key grand-piano range.
- Brightness held at `0.5` (CC74 ≈ 64, MPE Y at middle position).
- Confirms or refutes a pitch-dependent scratch trend at default brightness.

### Vertical (brightness sweep)
- 11 evenly-spaced CC74 values **0..127** (mapped to `bright = cc/127`).
- Pitch held at the geometric mean of the horizontal range (≈ **339.4 Hz, ~E4**).
- Confirms or refutes a brightness-dependent scratch trend at the perceptual centre of the keyboard.

Both axes are tested independently because the failure mode is
expected to be roughly monotonic on each axis. If round 1 reveals a
strong interaction effect, run a focused 2-D mini-sweep at the worst
combination (e.g. 5×5 = 25 renders) for round 2.

## Defaults

- **11 steps per axis** (22 WAVs total)
- **4-second renders** (2 s gate-on + 2 s release tail)
- **44.1 kHz / 16-bit / stereo**
- Renders skip reverb / master (just the bare tanpura voice — isolates the model itself)
- Sustain analysis window: `0.2 s .. 1.8 s` (skip onset transient and post-gate tail)

## How to run

### 1. Render 22 WAVs

```bash
sclang supercollider/tests/tanpura/tanpura_test_rig.scd
```

(Resolve `sclang.exe` from PATH or use the absolute path, e.g.
`"/c/Program Files/SuperCollider-3.12.1/sclang.exe"`.)

Realtime rendering takes ~100 s for 22 renders. Output lands in
`supercollider/tests/tanpura/renders/`. The script exits cleanly when
done (no manual intervention needed).

### 2. Analyze WAVs to JSON / CSV / PNG

```bash
python supercollider/tests/tanpura/analyze_renders.py
```

For each `*.wav` produces `*.json` (per-render features) and `*.png`
(spectrogram thumbnail). Also writes:
- `summary.csv` — flat 22-row table, one column per scalar feature
- `ratings.csv.template` — copy of summary's `filename` column with empty
  `rating` and `notes` cells, ready for you to fill

### 3. Rate the renders by listening

Open `renders/` in a file browser, play the WAVs in order, copy
`ratings.csv.template` → `ratings.csv` and fill the `rating` column
(1 = scratchy/bad, 5 = clean/good — any consistent scale works). Add
free-form remarks in the `notes` column when useful.

### 4. Hand to an LLM

Provide the LLM with:
- `summary.csv` (the 22-row scalar table)
- `ratings.csv` (your filled-in ratings)
- The SynthDef source — `supercollider/mpe_tanpura_xentool.scd`,
  lines 30–85

Ask it to:
1. Find which features most strongly correlate with low ratings.
2. Map those features back to specific terms in the SynthDef.
3. Propose 1–3 targeted edits with predictions about which feature each
   edit will move and by how much.

### 5. Re-run

After the LLM proposes a change:
1. Edit `mpe_tanpura_xentool.scd` (the live patch).
2. Copy the updated SynthDef body back into `tanpura_test_rig.scd`
   (lines 80-ish through 145).
3. Re-render the renders.
4. Re-analyze.
5. Re-rate the previously-bad cases (you don't need to re-rate the
   good ones unless they regressed).
6. Hand the new `summary.csv` + ratings to the LLM. It compares
   round-1 features against round-2 features for the changed cases.

## Overriding test parameters

`tanpura_test_rig.scd` accepts positional `key=value` pairs as sclang
command-line args. Defaults match the grand-piano range; override only
when you want to focus on a region:

```bash
# Higher resolution on the brightness axis
sclang tanpura_test_rig.scd vertSteps=22

# Custom narrower pitch range, e.g. just the upper register
sclang tanpura_test_rig.scd horizLo=523 horizHi=4186 horizSteps=15

# Output to a different folder (useful for round-N comparisons)
sclang tanpura_test_rig.scd outDir=C:/tmp/tanpura_round2
```

Full list of args in the `.scd` file's top comment block:
`outDir`, `horizSteps`, `horizLo`, `horizHi`, `vertSteps`, `vertCcLo`,
`vertCcHi`, `vertFreq`, `duration`, `gateOff`, `bright`, `vel`, `decay`.

## What `summary.csv` contains

One row per render, ~29 columns. Per-render scalars:

- **identity**: `filename`, `test` (`horiz`/`vert`), `step`, `freq_hz`, `cc74`
- **amplitude**: `rms_dbfs`, `peak_dbfs`, `crest_db`
- **spectral centroid** (over sustain window): `centroid_mean_hz`,
  `centroid_max_hz`, `centroid_max_at_s`
- **spectral rolloff (85%)**: `rolloff_85_mean_hz`, `rolloff_85_max_hz`
- **flatness (noise-vs-tone)**: `flatness_mean`, `flatness_max`
- **HF energy ratios**: `hf_energy_4k_8k_db`, `hf_energy_8k_16k_db`
- **envelope**: `attack_ms`, `decay_to_half_ms`
- **top spectral peaks** (5 strongest, time-averaged):
  `peak1_freq_hz`, `peak1_mag_db`, ... , `peak5_freq_hz`, `peak5_mag_db`

The per-file `.json` adds downsampled trajectories (centroid, rolloff,
flatness over 20 frames; envelope over 40 frames) and the full
top-peaks list with timing info.

The per-file `.png` is a 256×128 false-colour spectrogram (low freq at
bottom, no axes/labels — a quick visual sanity check, not for analysis).

## Caveats / known issues

- **High-pitch breakdown.** KS waveguide is numerically unstable above
  ~Nyquist/2. Horizontal sweep frequencies above ~1500 Hz may show low
  reported centroid because the synth is producing little/no signal
  there. Listen to the WAVs to confirm whether it's silence, ringing,
  or actual notes.
- **No reverb** in test renders. The test rig loads only
  `\mpeTanpuraXentool`, not `\mpeReverb` / `\mpeMaster`. Useful for
  isolating the SynthDef itself; means the renders sound drier than
  live performance.
- **Realtime rendering.** Each render = real audio time
  (~4 s × 22 = ~96 s). NRT (Score.recordNRT) was attempted first but
  had Windows-specific issues. If you ever port this to Linux/macOS,
  NRT could speed it up ~10×.
- **Pan jitter.** The SynthDef has `Pan2.ar(sig, Rand(-0.3, 0.3))`, so
  each render's stereo position is randomized. The analyzer sums to
  mono before feature extraction, so this doesn't affect the numbers,
  but consecutive WAVs will sound slightly differently positioned.

## Round-trip example

1. Run rig + analyzer → 22 renders, all artifacts in place.
2. Listen, fill `ratings.csv` (suppose CC ≥ 89 gets rating 1–2,
   CC ≤ 64 gets rating 4–5).
3. LLM analysis: "HF-band 4-8 kHz energy correlates r=−0.91 with
   rating; centroid_mean_hz r=−0.85. Both rise sharply above CC=64.
   In the SynthDef, the term most directly responsible is
   `brightMul = brightSm.linexp(0, 1, 1.25, 20)` — exponential mapping
   means brightness 0.7 → brightMul 6.7, brightness 1.0 → brightMul 20,
   a 3× jump that drives the burst LPF cutoff well past audible HF.
   Suggested edit: change to `linexp(0, 1, 1.25, 8)` to cap the LPF
   cutoff multiplier at 8× freqSm; predicts HF 4-8 kHz energy will
   drop ~12 dB at CC=127, with minimal impact on CC ≤ 64 renders."
4. Edit, re-run, re-rate the CC ≥ 89 cases. If they move from rating
   1–2 to rating 3–4, the hypothesis was correct; ship the change.
   If not, the LLM iterates.

The whole loop is one analyst session of perhaps 30 minutes plus a few
minutes of LLM time per round.
