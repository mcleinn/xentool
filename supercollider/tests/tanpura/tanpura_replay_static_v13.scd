// tanpura_replay_static_v13.scd
//
// V13 — V12 plus press amp swell reduced from (0.3 + press*1.2) to
//       (0.5 + press*0.5). At press=1, swell is now 1.0x (was 1.5x),
//       so signal stays out of the 0.95 Limiter ceiling and avoids
//       hard-clip distortion that contributed broadband noise at
//       max press. At press=0 the resting level is slightly higher
//       (0.5x vs 0.3x) so quiet notes are louder relative to before
//       — overall dynamic range from press is reduced 5x to 2x but
//       the audible distortion floor at max press is gone.
//
// V12 — V11 (drone fade above 500 Hz, halved BHiShelf, v4 reorder)
//       BUT with BHiShelf REMOVED ENTIRELY. v11 still had noise at
//       higher bright/press (n1, n3, n4) that scales with bright;
//       BHiShelf at +/-12 dB scaled with bright is the most likely
//       remaining culprit. Removing it sacrifices EQ-driven brightness
//       swing but bright is still controlled internally via burst LPF,
//       KS damping coefficient, and BLowShelf bass attenuation.
//
// If v12 is clean, BHiShelf is the dominant noise source and we can
// re-introduce a milder version (e.g., +8 dB above 8 kHz, "air only").
//
// V11 — based on v10 results (drone + jawari at G5 produces "sea-like
//       noise" because the comb at very short delay can't extract a
//       clean fundamental from broadband noise input). Two changes:
//
//   1) Drone amplitude fades to zero above 500 Hz fundamental:
//      droneFade = freqSm.linlin(300, 800, 1, 0)
//      G3 (196): droneFade=1, full drone (multi-tooth body).
//      E4 (330): droneFade=0.94, almost full.
//      G5 (784): droneFade=0.03, virtually no drone.
//      KS string + jawari alone is clean at G5 (variant A and C of
//      the original bisect). Removing drone there removes the
//      sea-like noise.
//
//   2) BHiShelf gain halved (-12 to +12) — addresses residual
//      scratch in v9 full chain at low pitches.
//
// V9 — V4 (jawari-before-drone) with a tighter pitch-aware drone
//      LPF cap. The G5 bisection showed that "Pluck + drone with the
//      v2 cap" already sounds bad at G5 because the 2 kHz cap admits
//      the 2nd harmonic (1568 Hz) into the comb where it rings as a
//      concentrated harsh resonance for 4 seconds.
//
//      Fix: cap at max(1000, freq*1.4). Below freq=714 Hz the cap is
//      flat at 1 kHz (low pitches keep multi-harmonic body). Above
//      714 Hz the cap rises at 1.4× fundamental — always BELOW the
//      2nd harmonic at 2× fundamental — so only the fundamental tooth
//      is excited at high pitches. Drone stays present at all pitches
//      but the harsh-presence band 2nd-harm resonance is gone.
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

outDir   = args.outDir ?? { scriptDir +/+ "v13_replays" };
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

        // FIX (v11): drone amplitude fades to zero above ~500 Hz
        // fundamental. Below 300 Hz: full drone (provides body for
        // tanpura character). Above 800 Hz: drone silent (KS string
        // alone is clean at high pitches per the bisect). Linear
        // taper between. Plus the v9 LPF cap retained as belt-and-
        // braces against spurious tooth excitation in the transition
        // band.
        sustainEx = WhiteNoise.ar(1) * 0.015 * pressSm
                    * freqSm.linlin(300, 800, 1, 0);
        sustainEx = LPF.ar(sustainEx, max(1000, freqSm * 1.4));

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

        // Drone added AFTER jawari, so it doesn't get clipped/smeared.
        sig = sig + CombL.ar(sustainEx, 0.05, freqSm.reciprocal, 4);

        // Slow string-to-string sympathy
        sig = DelayC.ar(sig, 0.03, SinOsc.kr(0.27, Rand(0, 2pi)).range(0.012, 0.022));

        // FIX (v12): BHiShelf REMOVED. v11 with halved gain still
        // produced bright/press-scaled noise; full removal isolates
        // whether BHiShelf is the dominant noise source. BLowShelf
        // retained — bass attenuation at high bright still gives
        // some EQ-driven brightness feel.
        sig = BLowShelf.ar(sig, 400,  1, brightSm.linlin(0, 1,   8, -8));

        // ASR envelope, long release so KS decay isn't truncated
        env = EnvGen.kr(
            Env.asr(0.005, 1, 4.0, curve: \sin),
            gate,
            doneAction: 2
        );

        // FIX (v13): press amp swell range tightened from (0.3, 1.5)
        // to (0.5, 1.0) so signal stays below the 0.95 Limiter
        // threshold even at max press. Eliminates hard-clip
        // intermodulation distortion that contributed broadband
        // noise at max bright/press.
        sig = sig * env * (0.5 + pressSm * 0.5) * 0.5;
        sig = Limiter.ar(sig, 0.95);

        Out.ar(0, Pan2.ar(sig, Rand(-0.3, 0.3)));
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
