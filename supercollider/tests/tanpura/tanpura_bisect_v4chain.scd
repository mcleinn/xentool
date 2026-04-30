// tanpura_bisect_v4chain.scd
//
// Second-stage bisection: v4 architecture (jawari before drone-mix)
// progressively adds post-mix components to find which one re-
// introduces scratch. Uses n3 G3 bright=1 press=1 as the input.
//
// Variants (each adds ONE component to the previous):
//   v4_0_minimal   — jawari'd string + drone, env * 0.5
//   v4_1_delayC    — + DelayC sympathy
//   v4_2_hiShelf   — + BHiShelf 1500 Hz, gain (-24, +24) at bright
//   v4_3_loShelf   — + BLowShelf 400 Hz
//   v4_4_pressSwl  — + (0.3 + press*1.2) * 0.5 amp swell
//   v4_5_limiter   — + Limiter at 0.95 (= full v4)
//
// Listen in order; first scratchy variant identifies the culprit.

(
var scriptDir = PathName(thisProcess.nowExecutingPath).pathOnly;
var outDir = scriptDir +/+ "bisect_v4chain";
var duration = 4.0;
var gateOff = 2.0;
var paramSet = [\freq, 196.0, \bright, 1.0, \press, 1.0, \bend, 0.25, \vel, 0.2,
                \decay, 11];

File.mkdir(outDir);

s.options.numOutputBusChannels = 2;
s.options.memSize = 65536;

s.waitForBoot({

    // Common subgraph: jawari'd string + drone via v4 architecture.
    // Returns mixed signal, ready for any post-processing.
    var v4core = { |freq, bend, bright, press, vel, decay|
        var bendRatio, freqSm, brightSm, pressSm, brightMul, ksCoef;
        var burst, string, sustainEx, ampFol, sig;
        bendRatio = (2 ** (bend / 12)).lag(0.025);
        freqSm    = freq * bendRatio;
        brightSm  = bright.lag(0.10);
        pressSm   = press.lag(0.08);
        brightMul = brightSm.linexp(0, 1, 1.25, 20);
        ksCoef    = brightSm.linlin(0, 1, 0.59, 0.05).clip(0.001, 0.999);
        burst = WhiteNoise.ar(1) * EnvGen.kr(Env.perc(0.0001, 0.05)) * vel;
        burst = LPF.ar(burst, freqSm * brightMul);
        string = Pluck.ar(burst, Impulse.kr(0), 0.05, freqSm.reciprocal, decay, ksCoef);
        // Drone (v2 cap)
        sustainEx = WhiteNoise.ar(1) * 0.015 * pressSm;
        sustainEx = LPF.ar(sustainEx, min(freqSm * 4, 2000));
        // Jawari on bare string (v4 architecture)
        ampFol = Amplitude.ar(string, 0.005, 0.15) + (pressSm * 0.5);
        sig = (string * (1.0 + ampFol * 6)).tanh;
        sig = sig + (string.cubed * ampFol * 1.4);
        sig = (sig * 0.7) + (string * 0.3);
        // Drone after
        sig + CombL.ar(sustainEx, 0.05, freqSm.reciprocal, 4);
    };

    // 0: minimal — jawari + drone + env, nothing else
    SynthDef(\v4_0_minimal, { |freq=220, gate=1, bend=0, bright=0.5, press=0, vel=0.5, decay=11|
        var sig = SynthDef.wrap(v4core, prependArgs: [freq, bend, bright, press, vel, decay]);
        var env = EnvGen.kr(Env.asr(0.005, 1, 4.0, curve: \sin), gate, doneAction: 2);
        Out.ar(0, Pan2.ar(sig * env * 0.5, 0));
    }).add;

    // 1: + DelayC sympathy
    SynthDef(\v4_1_delayC, { |freq=220, gate=1, bend=0, bright=0.5, press=0, vel=0.5, decay=11|
        var sig = SynthDef.wrap(v4core, prependArgs: [freq, bend, bright, press, vel, decay]);
        var env = EnvGen.kr(Env.asr(0.005, 1, 4.0, curve: \sin), gate, doneAction: 2);
        sig = DelayC.ar(sig, 0.03, SinOsc.kr(0.27, Rand(0, 2pi)).range(0.012, 0.022));
        Out.ar(0, Pan2.ar(sig * env * 0.5, 0));
    }).add;

    // 2: + BHiShelf at 1500 Hz, gain (-24, +24)
    SynthDef(\v4_2_hiShelf, { |freq=220, gate=1, bend=0, bright=0.5, press=0, vel=0.5, decay=11|
        var sig = SynthDef.wrap(v4core, prependArgs: [freq, bend, bright, press, vel, decay]);
        var env = EnvGen.kr(Env.asr(0.005, 1, 4.0, curve: \sin), gate, doneAction: 2);
        var brightSm = bright.lag(0.10);
        sig = DelayC.ar(sig, 0.03, SinOsc.kr(0.27, Rand(0, 2pi)).range(0.012, 0.022));
        sig = BHiShelf.ar(sig, 1500, 1, brightSm.linlin(0, 1, -24, 24));
        Out.ar(0, Pan2.ar(sig * env * 0.5, 0));
    }).add;

    // 3: + BLowShelf at 400 Hz
    SynthDef(\v4_3_loShelf, { |freq=220, gate=1, bend=0, bright=0.5, press=0, vel=0.5, decay=11|
        var sig = SynthDef.wrap(v4core, prependArgs: [freq, bend, bright, press, vel, decay]);
        var env = EnvGen.kr(Env.asr(0.005, 1, 4.0, curve: \sin), gate, doneAction: 2);
        var brightSm = bright.lag(0.10);
        sig = DelayC.ar(sig, 0.03, SinOsc.kr(0.27, Rand(0, 2pi)).range(0.012, 0.022));
        sig = BHiShelf.ar(sig, 1500, 1, brightSm.linlin(0, 1, -24, 24));
        sig = BLowShelf.ar(sig, 400, 1, brightSm.linlin(0, 1, 8, -8));
        Out.ar(0, Pan2.ar(sig * env * 0.5, 0));
    }).add;

    // 4: + press amp swell
    SynthDef(\v4_4_pressSwl, { |freq=220, gate=1, bend=0, bright=0.5, press=0, vel=0.5, decay=11|
        var sig = SynthDef.wrap(v4core, prependArgs: [freq, bend, bright, press, vel, decay]);
        var env = EnvGen.kr(Env.asr(0.005, 1, 4.0, curve: \sin), gate, doneAction: 2);
        var brightSm = bright.lag(0.10);
        var pressSm = press.lag(0.08);
        sig = DelayC.ar(sig, 0.03, SinOsc.kr(0.27, Rand(0, 2pi)).range(0.012, 0.022));
        sig = BHiShelf.ar(sig, 1500, 1, brightSm.linlin(0, 1, -24, 24));
        sig = BLowShelf.ar(sig, 400, 1, brightSm.linlin(0, 1, 8, -8));
        Out.ar(0, Pan2.ar(sig * env * (0.3 + pressSm * 1.2) * 0.5, 0));
    }).add;

    // 5: + Limiter (= full v4)
    SynthDef(\v4_5_limiter, { |freq=220, gate=1, bend=0, bright=0.5, press=0, vel=0.5, decay=11|
        var sig = SynthDef.wrap(v4core, prependArgs: [freq, bend, bright, press, vel, decay]);
        var env = EnvGen.kr(Env.asr(0.005, 1, 4.0, curve: \sin), gate, doneAction: 2);
        var brightSm = bright.lag(0.10);
        var pressSm = press.lag(0.08);
        sig = DelayC.ar(sig, 0.03, SinOsc.kr(0.27, Rand(0, 2pi)).range(0.012, 0.022));
        sig = BHiShelf.ar(sig, 1500, 1, brightSm.linlin(0, 1, -24, 24));
        sig = BLowShelf.ar(sig, 400, 1, brightSm.linlin(0, 1, 8, -8));
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
        [\v4_0_minimal, \v4_1_delayC, \v4_2_hiShelf, \v4_3_loShelf, \v4_4_pressSwl, \v4_5_limiter].do { |def, i|
            var fname = outDir +/+ def.asString ++ ".wav";
            "[%/6] %".format(i + 1, fname.basename).postln;
            renderOne.value(fname, def);
        };
        "Done.".postln;
        s.quit; 0.5.wait; 0.exit;
    }).play;
});
)
