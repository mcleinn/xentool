// tanpura_replay_static_v8.scd
//
// V8 — DIAGNOSTIC ONLY. Bare-bones v4 architecture: jawari'd string
//      + drone (with v2 LPF cap), env * 0.5. EVERYTHING ELSE STRIPPED:
//      no DelayC sympathy, no BHiShelf, no BLowShelf, no press swell,
//      no Limiter. This is the v4_0_minimal variant from the bisect
//      rig, applied to all 4 frozen frames.
//
// Goal: confirm whether the v4 architectural source is clean across
// all 4 pitches, not just at G3. If n1 (G5 bright=0.71), n2 (G5
// bright=0.58), n4 (E4 bright=1.0) sound clean here, the residual
// scratch from v4/v6/v7 is in the post-mix chain. If they sound
// scratchy even in this stripped form, the source itself has issues
// at higher pitches — KS at short delays, bright-driven Pluck filter
// behaviour, jawari on the bare string at high freq, etc.
//
// Not a final-fix candidate.
//
// V4 — based on bisection result: drone+jawari interaction is the
// scratch source. Structural reorder so jawari operates on the bare
// string, drone is mixed in AFTER the nonlinearity. Plus the v2 drone
// LPF cap is kept for belt-and-braces.
//
// Bisection evidence (bisect_renders/):
//   A pluck only         — clean
//   B pluck + drone      — clean
//   C pluck + jawari     — clean
//   D pluck + shelves    — clean
//   E pluck + drone + jawari — slightly scratchy
//   F-H full chains      — very scratchy
//
// Output goes to v4_replays/.
//
// Renders 4 frozen-frame sustained tones, one per scratchy note recorded
// in the live session at supercollider/logs/tanpura_20260430-143546.jsonl.
// Each render holds the peak observed (freq, bright, press, bend, vel)
// constant for the whole gate-on window — i.e. as if the user touched
// the pad with that exact gesture envelope locked in place.
//
// "Static" because we don't replay the trajectory; we just freeze the
// worst-case point. If the renders sound scratchy at this static peak,
// the issue is in the static state vector and a trajectory replay isn't
// strictly needed. If they sound fine, the scratch is dynamics-driven
// (rising-CC74 sweep through the BHiShelf, KS waveguide re-excitation,
// etc.) and the next iteration is the trajectory replay rig.
//
// SynthDef body is copied verbatim from
// `supercollider/mpe_tanpura_xentool.scd:30-85` — DO NOT MODIFY.
//
// Usage:
//   sclang tanpura_replay_static.scd
// Output: supercollider/tests/tanpura/replays/scratch_*.wav

(
var argv = thisProcess.argv;
var args = ();
var scriptDir = PathName(thisProcess.nowExecutingPath).pathOnly;
var asNum, asInt;
var outDir, duration, gateOff;
var frames;

argv.do { |kv|
    var pair = kv.split($=);
    if(pair.size == 2) { args[pair[0].asSymbol] = pair[1] };
};
asNum = { |key, default|
    if(args[key].notNil) { args[key].asFloat } { default }
};
asInt = { |key, default|
    if(args[key].notNil) { args[key].asInteger } { default }
};

outDir   = args.outDir ?? { scriptDir +/+ "v8_replays" };
duration = asNum.(\duration, 4.0);
gateOff  = asNum.(\gateOff, 2.0);

// === Frozen frames extracted from the 2026-04-30 14:35 session log ===
// Each entry: peak (worst-case) value of every parameter the SynthDef
// receives. `vel` is the value already passed to Synth.new at note-on
// (already linexp-mapped from MIDI vel). `bend` is the bend value at
// noteOn time in semitones; `bright` and `press` are the peak values
// observed during the note's lifetime.
//
// id  description (from log)                              freq    bright press bend  vel
// n1  G5, scratchy throughout                              784.0   0.71   0.84  -0.43 0.248
// n2  G5, mildly scratchy                                  784.0   0.58   0.75  -0.21 0.224
// n3  G3, became scratchy late (note 4 in log)             196.0   1.00   1.00   0.25 0.200
// n4  E4, "became scratchy by X axis" (note 5 in log)      329.6   1.00   1.00   0.30 0.252
// (note 6 in log = F#2 92.5 Hz was clean; not replayed.)
frames = [
    (label: "n1_g5_bright071_press084", freq: 784.0,  bright: 0.71, press: 0.84, bend: -0.43, vel: 0.248),
    (label: "n2_g5_bright058_press075", freq: 784.0,  bright: 0.58, press: 0.75, bend: -0.21, vel: 0.224),
    (label: "n3_g3_bright100_press100", freq: 196.0,  bright: 1.00, press: 1.00, bend:  0.25, vel: 0.200),
    (label: "n4_e4_bright100_press100", freq: 329.6,  bright: 1.00, press: 1.00, bend:  0.30, vel: 0.252),
];

File.mkdir(outDir);
"output dir:    %".format(outDir).postln;
"duration:      % s  (gate off at %)".format(duration, gateOff).postln;
"frames:        %".format(frames.size).postln;
"".postln;

s.options.numOutputBusChannels = 2;
s.options.memSize = 65536;

s.waitForBoot({
    // ============================================================
    // SynthDef — copied verbatim from mpe_tanpura_xentool.scd:30-85
    // DO NOT MODIFY. Any divergence here invalidates the replay.
    // ============================================================
    SynthDef(\mpeTanpuraXentool, { |freq=220, gate=1, bend=0, bright=0.5, press=0, vel=0.5, decay=11, dampScale=1, brightScale=1|
        var bendRatio, freqSm, brightSm, pressSm, brightMul, ksCoef;
        var burst, string, sig, ampFol, env, sustainEx;

        // Smoothed controls — kill zipper noise on .set
        bendRatio = (2 ** (bend / 12)).lag(0.025);
        freqSm    = freq * bendRatio;
        brightSm  = bright.lag(0.10);
        pressSm   = press.lag(0.08);

        // dampScale (encoder/button cycle) multiplies the KS loop coefficient
        // — bigger value = harder/brighter string. brightScale multiplies the
        // burst-LPF cutoff multiplier — bigger value = larger/brighter body.
        // Both default to 1.0, so the synth sounds identical to the original
        // until the user moves something.
        brightMul = brightSm.linexp(0, 1, 1.25, 20) * brightScale;
        ksCoef    = (brightSm.linlin(0, 1, 0.59, 0.05) * dampScale).clip(0.001, 0.999);

        // Pluck excitation — same as classic, scaled by velocity, freq follows bend
        burst = WhiteNoise.ar(1) * EnvGen.kr(Env.perc(0.0001, 0.05)) * vel;
        burst = LPF.ar(burst, freqSm * brightMul);

        // Karplus-Strong waveguide
        string = Pluck.ar(burst, Impulse.kr(0), 0.05, freqSm.reciprocal, decay, ksCoef);

        // FIX (v2, retained): cap the drone LPF cutoff at min(4*freq,
        // 2 kHz) — limits drone HF content reaching the output.
        sustainEx = WhiteNoise.ar(1) * 0.015 * pressSm;
        sustainEx = LPF.ar(sustainEx, min(freqSm * 4, 2000));

        // Amplitude-driven jawari — applied to BARE STRING ONLY.
        // FIX (v4): bisection showed that running the jawari over
        // (string + drone) is the scratch source. With drone summed
        // in BEFORE the tanh, the tanh sees a larger input and clips
        // harder, smearing broadband drone content into HF distortion.
        // Operating on bare string avoids that cascade entirely.
        ampFol = Amplitude.ar(string, 0.005, 0.15) + (pressSm * 0.5);
        sig    = (string * (1.0 + ampFol * 6)).tanh;
        sig    = sig + (string.cubed * ampFol * 1.4);
        sig    = (sig * 0.7) + (string * 0.3);

        // Drone added AFTER jawari (v4 reorder).
        sig = sig + CombL.ar(sustainEx, 0.05, freqSm.reciprocal, 4);

        // STRIPPED (v8 diagnostic): no DelayC, no BHiShelf, no
        // BLowShelf, no press swell, no Limiter. Just envelope.
        env = EnvGen.kr(Env.asr(0.005, 1, 4.0, curve: \sin), gate, doneAction: 2);
        sig = sig * env * 0.5;

        Out.ar(0, Pan2.ar(sig, 0));
    }).add;
    // ============================================================

    s.sync;

    Routine({
        var t0 = Date.getDate.rawSeconds;
        var renderOne = { |outFile, params|
            var syn;
            s.recHeaderFormat = "WAV";
            s.recSampleFormat = "int16";
            s.recChannels = 2;
            s.record(outFile, numChannels: 2);
            s.sync;
            0.05.wait;

            syn = Synth(\mpeTanpuraXentool, params ++ [\gate, 1]);
            gateOff.wait;
            syn.set(\gate, 0);
            (duration - gateOff).wait;

            s.stopRecording;
            s.sync;
            0.3.wait;
        };

        "frozen-frame renders (peak param vector held constant):".postln;
        frames.do { |fr, i|
            var fname = outDir +/+ "scratch_" ++ fr.label ++ ".wav";
            "  [%/%] %".format(i + 1, frames.size, fname.basename).postln;
            "       freq=% bright=% press=% bend=% vel=%".format(
                fr.freq, fr.bright, fr.press, fr.bend, fr.vel
            ).postln;
            renderOne.value(fname, [
                \freq, fr.freq,
                \bright, fr.bright,
                \press, fr.press,
                \bend, fr.bend,
                \vel, fr.vel,
                \decay, 11, \dampScale, 1, \brightScale, 1
            ]);
        };

        "".postln;
        "Done in % s. Wrote % files to %.".format(
            (Date.getDate.rawSeconds - t0).round(0.1),
            frames.size, outDir
        ).postln;

        s.quit;
        0.5.wait;
        0.exit;
    }).play;
});
)
