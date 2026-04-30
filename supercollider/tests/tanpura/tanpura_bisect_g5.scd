// tanpura_bisect_g5.scd
//
// Same variant set as tanpura_bisect.scd but with n1 G5 input
// parameters (freq=784, bright=0.71, press=0.84). Diagnostic for the
// pitch-dependent scratch observed in v8 — n3 G3 was clean across
// stripped variants but n1 G5 was bad. Locating where in the chain
// the high-pitch scratch begins.
//
// Bisection diagnostic: renders ONE frame (n3 G3 bright=1 press=1, the
// worst of the 4 logged scratchy notes) through 8 progressively-simpler
// SynthDef variants. The variant that first sounds clean tells us which
// term produces the scratch — no more guessing.
//
// All variants share the SAME parameter input (freq=196 Hz, bright=1,
// press=1, vel=0.2). Only the SynthDef body differs.
//
// Output: bisect_renders/  (one WAV per variant, named by what's
// disabled). Listen in order; the first one that sounds not-scratchy
// is the answer.

(
var scriptDir = PathName(thisProcess.nowExecutingPath).pathOnly;
var outDir = scriptDir +/+ "bisect_g5";
var duration = 4.0;
var gateOff = 2.0;
// n1 G5 input parameters from the live session log (the worst of the
// 4 frozen frames in v7/v8 testing).
var paramSet = [\freq, 784.0, \bright, 0.71, \press, 0.84, \bend, -0.43, \vel, 0.248,
                \decay, 11, \dampScale, 1, \brightScale, 1];

File.mkdir(outDir);
"output dir: %".format(outDir).postln;
"".postln;

s.options.numOutputBusChannels = 2;
s.options.memSize = 65536;

s.waitForBoot({

    // === Common subgraph: produces `string` ===
    // Pluck (KS waveguide) excited by burst. Same in all variants. This
    // is the "clean string" baseline — if even THIS sounds scratchy
    // then the issue is in Pluck/burst itself.
    var stringSub = { |freq, bend, bright, vel, decay|
        var bendRatio, freqSm, brightSm, brightMul, ksCoef, burst;
        bendRatio = (2 ** (bend / 12)).lag(0.025);
        freqSm    = freq * bendRatio;
        brightSm  = bright.lag(0.10);
        brightMul = brightSm.linexp(0, 1, 1.25, 20);
        ksCoef    = brightSm.linlin(0, 1, 0.59, 0.05).clip(0.001, 0.999);
        burst = WhiteNoise.ar(1) * EnvGen.kr(Env.perc(0.0001, 0.05)) * vel;
        burst = LPF.ar(burst, freqSm * brightMul);
        Pluck.ar(burst, Impulse.kr(0), 0.05, freqSm.reciprocal, decay, ksCoef);
    };

    // ──────────────────────────────────────────────────────────────────
    //  A — Pluck only. No drone, no jawari, no shelves, no limiter, no
    //      press-driven amp swell. Just the bare KS string.
    // ──────────────────────────────────────────────────────────────────
    SynthDef(\bA, { |freq=220, gate=1, bend=0, bright=0.5, press=0, vel=0.5, decay=11|
        var sig, env;
        sig = SynthDef.wrap(stringSub, prependArgs: [freq, bend, bright, vel, decay]);
        env = EnvGen.kr(Env.asr(0.005, 1, 4.0, curve: \sin), gate, doneAction: 2);
        Out.ar(0, Pan2.ar(sig * env * 0.5, 0));
    }).add;

    // ──────────────────────────────────────────────────────────────────
    //  B — Pluck + sustain drone (CombL). v2 + v3's drone LPF cap
    //      kept (cutoff at min(4*freq, 2k)). No jawari, no shelves.
    // ──────────────────────────────────────────────────────────────────
    SynthDef(\bB, { |freq=220, gate=1, bend=0, bright=0.5, press=0, vel=0.5, decay=11|
        var sig, env, string, sustainEx;
        var bendRatio = (2 ** (bend / 12)).lag(0.025);
        var freqSm = freq * bendRatio;
        var pressSm = press.lag(0.08);
        string = SynthDef.wrap(stringSub, prependArgs: [freq, bend, bright, vel, decay]);
        sustainEx = WhiteNoise.ar(1) * 0.015 * pressSm;
        sustainEx = LPF.ar(sustainEx, min(freqSm * 4, 2000));
        sig = string + CombL.ar(sustainEx, 0.05, freqSm.reciprocal, 4);
        env = EnvGen.kr(Env.asr(0.005, 1, 4.0, curve: \sin), gate, doneAction: 2);
        Out.ar(0, Pan2.ar(sig * env * 0.5, 0));
    }).add;

    // ──────────────────────────────────────────────────────────────────
    //  C — Pluck + jawari (tanh + cubed). No drone, no shelves.
    //      Tests whether the press-driven nonlinearity alone is the
    //      scratch source.
    // ──────────────────────────────────────────────────────────────────
    SynthDef(\bC, { |freq=220, gate=1, bend=0, bright=0.5, press=0, vel=0.5, decay=11|
        var sig, env, string, ampFol;
        var pressSm = press.lag(0.08);
        string = SynthDef.wrap(stringSub, prependArgs: [freq, bend, bright, vel, decay]);
        ampFol = Amplitude.ar(string, 0.005, 0.15) + (pressSm * 0.5);
        sig = (string * (1.0 + ampFol * 6)).tanh;
        sig = sig + (string.cubed * ampFol * 1.4);
        sig = (sig * 0.7) + (string * 0.3);
        env = EnvGen.kr(Env.asr(0.005, 1, 4.0, curve: \sin), gate, doneAction: 2);
        Out.ar(0, Pan2.ar(sig * env * 0.5, 0));
    }).add;

    // ──────────────────────────────────────────────────────────────────
    //  D — Pluck + shelves (BHiShelf at 1500 Hz +24, BLowShelf 400 Hz
    //      +8). No drone, no jawari. Tests EQ alone.
    // ──────────────────────────────────────────────────────────────────
    SynthDef(\bD, { |freq=220, gate=1, bend=0, bright=0.5, press=0, vel=0.5, decay=11|
        var sig, env, string;
        var brightSm = bright.lag(0.10);
        string = SynthDef.wrap(stringSub, prependArgs: [freq, bend, bright, vel, decay]);
        sig = BHiShelf.ar(string, 1500, 1, brightSm.linlin(0, 1, -24, 24));
        sig = BLowShelf.ar(sig, 400, 1, brightSm.linlin(0, 1, 8, -8));
        env = EnvGen.kr(Env.asr(0.005, 1, 4.0, curve: \sin), gate, doneAction: 2);
        Out.ar(0, Pan2.ar(sig * env * 0.5, 0));
    }).add;

    // ──────────────────────────────────────────────────────────────────
    //  E — Pluck + jawari + drone (everything except shelves).
    // ──────────────────────────────────────────────────────────────────
    SynthDef(\bE, { |freq=220, gate=1, bend=0, bright=0.5, press=0, vel=0.5, decay=11|
        var sig, env, string, sustainEx, ampFol;
        var bendRatio = (2 ** (bend / 12)).lag(0.025);
        var freqSm = freq * bendRatio;
        var pressSm = press.lag(0.08);
        string = SynthDef.wrap(stringSub, prependArgs: [freq, bend, bright, vel, decay]);
        sustainEx = WhiteNoise.ar(1) * 0.015 * pressSm;
        sustainEx = LPF.ar(sustainEx, min(freqSm * 4, 2000));
        string = string + CombL.ar(sustainEx, 0.05, freqSm.reciprocal, 4);
        ampFol = Amplitude.ar(string, 0.005, 0.15) + (pressSm * 0.5);
        sig = (string * (1.0 + ampFol * 6)).tanh;
        sig = sig + (string.cubed * ampFol * 1.4);
        sig = (sig * 0.7) + (string * 0.3);
        env = EnvGen.kr(Env.asr(0.005, 1, 4.0, curve: \sin), gate, doneAction: 2);
        Out.ar(0, Pan2.ar(sig * env * 0.5, 0));
    }).add;

    // ──────────────────────────────────────────────────────────────────
    //  F — full chain, but jawari nonlinearity reduced (drive 6→2,
    //      cubed 1.4→0.3). Tests whether JUST the strong nonlinearity
    //      drives the scratch.
    // ──────────────────────────────────────────────────────────────────
    SynthDef(\bF, { |freq=220, gate=1, bend=0, bright=0.5, press=0, vel=0.5, decay=11|
        var sig, env, string, sustainEx, ampFol;
        var bendRatio = (2 ** (bend / 12)).lag(0.025);
        var freqSm = freq * bendRatio;
        var brightSm = bright.lag(0.10);
        var pressSm = press.lag(0.08);
        string = SynthDef.wrap(stringSub, prependArgs: [freq, bend, bright, vel, decay]);
        sustainEx = WhiteNoise.ar(1) * 0.015 * pressSm;
        sustainEx = LPF.ar(sustainEx, min(freqSm * 4, 2000));
        string = string + CombL.ar(sustainEx, 0.05, freqSm.reciprocal, 4);
        ampFol = Amplitude.ar(string, 0.005, 0.15) + (pressSm * 0.5);
        // REDUCED nonlinearity:
        sig = (string * (1.0 + ampFol * 2)).tanh;       // was * 6
        sig = sig + (string.cubed * ampFol * 0.3);       // was * 1.4
        sig = (sig * 0.7) + (string * 0.3);
        sig = BHiShelf.ar(sig, 1500, 1, brightSm.linlin(0, 1, -24, 24));
        sig = BLowShelf.ar(sig, 400, 1, brightSm.linlin(0, 1, 8, -8));
        env = EnvGen.kr(Env.asr(0.005, 1, 4.0, curve: \sin), gate, doneAction: 2);
        Out.ar(0, Pan2.ar(sig * env * 0.5, 0));
    }).add;

    // ──────────────────────────────────────────────────────────────────
    //  G — full chain, but press-amp-swell removed (sig * env * 0.5
    //      instead of * env * (0.3 + press*1.2) * 0.5). Tests whether
    //      the volume bump from press is what makes scratch audible.
    // ──────────────────────────────────────────────────────────────────
    SynthDef(\bG, { |freq=220, gate=1, bend=0, bright=0.5, press=0, vel=0.5, decay=11|
        var sig, env, string, sustainEx, ampFol;
        var bendRatio = (2 ** (bend / 12)).lag(0.025);
        var freqSm = freq * bendRatio;
        var brightSm = bright.lag(0.10);
        var pressSm = press.lag(0.08);
        string = SynthDef.wrap(stringSub, prependArgs: [freq, bend, bright, vel, decay]);
        sustainEx = WhiteNoise.ar(1) * 0.015 * pressSm;
        sustainEx = LPF.ar(sustainEx, min(freqSm * 4, 2000));
        string = string + CombL.ar(sustainEx, 0.05, freqSm.reciprocal, 4);
        ampFol = Amplitude.ar(string, 0.005, 0.15) + (pressSm * 0.5);
        sig = (string * (1.0 + ampFol * 6)).tanh;
        sig = sig + (string.cubed * ampFol * 1.4);
        sig = (sig * 0.7) + (string * 0.3);
        sig = DelayC.ar(sig, 0.03, SinOsc.kr(0.27, Rand(0, 2pi)).range(0.012, 0.022));
        sig = BHiShelf.ar(sig, 1500, 1, brightSm.linlin(0, 1, -24, 24));
        sig = BLowShelf.ar(sig, 400, 1, brightSm.linlin(0, 1, 8, -8));
        env = EnvGen.kr(Env.asr(0.005, 1, 4.0, curve: \sin), gate, doneAction: 2);
        // CHANGED: env factor only, no press swell.
        sig = sig * env * 0.5;
        sig = Limiter.ar(sig, 0.95);
        Out.ar(0, Pan2.ar(sig, Rand(-0.3, 0.3)));
    }).add;

    // ──────────────────────────────────────────────────────────────────
    //  H — full ORIGINAL chain (v1 — no fixes). Reference: should
    //      sound exactly like v1 scratch_n3 from the replay rig.
    // ──────────────────────────────────────────────────────────────────
    SynthDef(\bH, { |freq=220, gate=1, bend=0, bright=0.5, press=0, vel=0.5, decay=11|
        var sig, env, string, sustainEx, ampFol;
        var bendRatio = (2 ** (bend / 12)).lag(0.025);
        var freqSm = freq * bendRatio;
        var brightSm = bright.lag(0.10);
        var pressSm = press.lag(0.08);
        string = SynthDef.wrap(stringSub, prependArgs: [freq, bend, bright, vel, decay]);
        sustainEx = WhiteNoise.ar(1) * 0.015 * pressSm;
        sustainEx = LPF.ar(sustainEx, freqSm * brightSm.linexp(0, 1, 1.25, 20));
        string = string + CombL.ar(sustainEx, 0.05, freqSm.reciprocal, 4);
        ampFol = Amplitude.ar(string, 0.005, 0.15) + (pressSm * 0.5);
        sig = (string * (1.0 + ampFol * 6)).tanh;
        sig = sig + (string.cubed * ampFol * 1.4);
        sig = (sig * 0.7) + (string * 0.3);
        sig = DelayC.ar(sig, 0.03, SinOsc.kr(0.27, Rand(0, 2pi)).range(0.012, 0.022));
        sig = BHiShelf.ar(sig, 1500, 1, brightSm.linlin(0, 1, -24, 24));
        sig = BLowShelf.ar(sig, 400, 1, brightSm.linlin(0, 1, 8, -8));
        env = EnvGen.kr(Env.asr(0.005, 1, 4.0, curve: \sin), gate, doneAction: 2);
        sig = sig * env * (0.3 + pressSm * 1.2) * 0.5;
        sig = Limiter.ar(sig, 0.95);
        Out.ar(0, Pan2.ar(sig, Rand(-0.3, 0.3)));
    }).add;

    s.sync;

    Routine({
        var renderOne = { |outFile, defName|
            var syn;
            s.recHeaderFormat = "WAV";
            s.recSampleFormat = "int16";
            s.recChannels = 2;
            s.record(outFile, numChannels: 2);
            s.sync;
            0.05.wait;
            syn = Synth(defName, paramSet ++ [\gate, 1]);
            gateOff.wait;
            syn.set(\gate, 0);
            (duration - gateOff).wait;
            s.stopRecording;
            s.sync;
            0.3.wait;
        };

        [
            (id: "A_pluck_only",                   def: \bA),
            (id: "B_pluck_drone",                  def: \bB),
            (id: "C_pluck_jawari",                 def: \bC),
            (id: "D_pluck_shelves",                def: \bD),
            (id: "E_pluck_drone_jawari",           def: \bE),
            (id: "F_full_reduced_jawari",          def: \bF),
            (id: "G_full_no_press_swell",          def: \bG),
            (id: "H_full_v1_original",             def: \bH),
        ].do { |variant, i|
            var fname = outDir +/+ "bisect_" ++ variant.id ++ ".wav";
            "  [%/%] %".format(i + 1, 8, fname.basename).postln;
            renderOne.value(fname, variant.def);
        };

        "Done. Listen in order:".postln;
        "  A → bare Pluck only (should be clean)".postln;
        "  B → +drone".postln;
        "  C → +jawari nonlinearity".postln;
        "  D → +shelves".postln;
        "  E → +drone +jawari".postln;
        "  F → full chain with reduced jawari".postln;
        "  G → full chain with press-swell removed".postln;
        "  H → full v1 (should sound like the original n3 scratch)".postln;
        "The FIRST variant that sounds NOT scratchy tells you which".postln;
        "added component is responsible. If A is clean and B isn't,".postln;
        "the drone is the culprit. If A-D are clean and E isn't, it's".postln;
        "the drone+jawari combo. Etc.".postln;

        s.quit;
        0.5.wait;
        0.exit;
    }).play;
});
)
