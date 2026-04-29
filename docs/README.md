# xentool docs

Architecture diagrams and longer-form notes about how xentool fits
together with its sidecars (xenharm) and downstream tools (the Live
HUD, SuperCollider). Implementation-level documentation lives in
- `../CLAUDE.md` — high-level architecture, invariants, design decisions
- `../README.md` — user-facing quick start, install, CLI reference
- inline source comments throughout `src/`

## Architecture diagrams

| Diagram                          | Setup                            |
|----------------------------------|----------------------------------|
| [`architecture-exquis.svg`](architecture-exquis.svg)   | 4 Exquis MPE controllers, MPE pitch-bend retuning, MPE tanpura SC patch |
| [`architecture-wooting.svg`](architecture-wooting.svg) | 4 Wooting analog keyboards, Lumatone channel-stripe MIDI + MTS-ESP, classic-MIDI piano SC patch with `/xentool/tuning` OSC broadcast |

Conventions in both diagrams:

- **Solid black arrows** — required data flow when the corresponding
  feature is enabled.
- **Dashed gray arrows** — optional / back-channel (e.g. SC pushing
  parameter values into xentool's HUD strip).
- **Green** is xentool itself (the protagonist), **blue** is the
  controllers, **yellow** is xenharm, **purple** is the Browser HUD,
  **pink** is SuperCollider, **gray** is virtual MIDI / audio out,
  **teal** annotations are MTS-ESP shared-memory state (Wooting only).

## Why the two diagrams differ

The high-level shape is the same — controllers → xentool → MIDI port
→ SuperCollider, with xenharm and the HUD as sidecars — but the
microtonality machinery is different per backend:

- **Exquis** controllers emit MPE: per-pad pitch bend, CC74 (Y), and
  channel pressure (Z). xentool retunes microtonally by *injecting*
  per-note pitch-bend before each note-on, and the SC `mpe_tanpura`
  patch receives that bend on a per-note channel and shapes the
  Karplus-Strong string accordingly.
- **Wooting** keyboards emit classic 12 / N-EDO MIDI on Lumatone-style
  channel-stripes (each channel = one EDO offset). xentool acts as
  the MTS-ESP master, but SuperCollider has no MTS-ESP client, so
  xentool *also* broadcasts `/xentool/tuning <edo, pitch_offset, layout_id>`
  over OSC when started with `--tune-supercollider`. The SC
  `midi_piano` patch listens for that broadcast and re-derives Hz
  from the same EDO formula xentool's master uses
  (`C0 * 2^(virtual_pitch / edo)`), giving frequencies bit-identical
  to what an MTS-ESP client would receive.

## Pitch-bend conventions

- **Exquis**: xentool's `--pb-range` (default 16 semitones) is what
  the synth's pitch-bend interpretation must be set to. The MPE
  tanpura's `~mpeBendRange` matches it. A wider range than the
  conventional ±2 is needed because the bend value carries microtonal
  retune *plus* the player's X-slide expression on the same channel.
- **Wooting**: xentool relays raw 14-bit pitch-bend without scaling,
  so the synth's pitch-bend range is the user's choice. ±2 for a
  pianistic feel, ±12 for organ-style portamento, ±16 to match Exquis,
  …. The bundled SC piano patch defaults to ±2; change `~bendRange` at
  the top of `midi_piano_xentool.scd` for a different feel. There is
  no analogue of the Exquis `--x-gain` multiplier here — Wooting
  analog keys already give full physical bend output.

## Re-rendering / editing

Both SVGs are hand-written, not generated, so you can open them in
any editor and tweak boxes / arrows directly. The structure is
documented inline at the top of each file. For PNG exports (e.g. for
slide decks):

```bash
inkscape --export-type=png --export-dpi=200 docs/architecture-exquis.svg
inkscape --export-type=png --export-dpi=200 docs/architecture-wooting.svg
```
