// tanpura_test_rig.scd
//
// Non-interactive batch render rig for the mpeTanpuraXentool SynthDef.
// Generates short isolated WAVs sweeping pitch (horizontal test) and
// brightness (vertical test). The SynthDef body is copied verbatim from
// `mpe_tanpura_xentool.scd` — DO NOT modify it; that's the whole point
// of using a separate test file.
//
// Realtime rendering (s.record). Slower than NRT but rock-solid on
// Windows where NRT has historically had quirks. Each take is fully
// isolated: server stops recording and waits for disk flush before the
// next take, so no audio bleeds between WAVs.
//
// Usage (one sclang invocation, all renders inside):
//
//   sclang tanpura_test_rig.scd
//   sclang tanpura_test_rig.scd outDir=C:/tmp horizSteps=22
//
// Args are positional `key=value` pairs in any order. Defaults span a
// standard 88-key grand piano (A0=27.5 Hz to C8=4186 Hz) at 11 steps ×
// 11 brightness CC values.
//
//   outDir       output directory (default: <script dir>/renders)
//   horizSteps   number of horizontal-test steps         (default 11)
//   horizLo      horizontal lowest freq (Hz)             (default 27.50)
//   horizHi      horizontal highest freq (Hz)            (default 4186.01)
//   vertSteps    number of vertical-test steps           (default 11)
//   vertCcLo     vertical lowest CC value (0..127)       (default 0)
//   vertCcHi     vertical highest CC value (0..127)      (default 127)
//   vertFreq     vertical fixed pitch (Hz)               (default geometric
//                                                         mean of horiz range)
//   duration     render duration per take (s)            (default 4.0)
//   gateOff      time at which gate=0 fires (s)          (default 2.0)
//   bright       brightness for horizontal test (0..1)   (default 0.5)
//   vel          velocity (0..1)                         (default 0.7)
//   decay        KS decay seconds                        (default 11)

(
// All `var` declarations must precede any other statements in this block.
var argv = thisProcess.argv;
var args = ();
var scriptDir = PathName(thisProcess.nowExecutingPath).pathOnly;
var asNum, asInt, pad, fmtFreq;
var outDir, horizSteps, horizLo, horizHi, vertSteps, vertCcLo, vertCcHi;
var vertFreq, duration, gateOff, bright, vel, decay;
var horizFreqs, vertCcs;

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
pad = { |i, w| i.asString.padLeft(w, "0") };
fmtFreq = { |f| f.round(0.01).asString };

outDir     = args.outDir ?? { scriptDir +/+ "renders" };
horizSteps = asInt.(\horizSteps, 11);
horizLo    = asNum.(\horizLo, 27.5);
horizHi    = asNum.(\horizHi, 4186.01);
vertSteps  = asInt.(\vertSteps, 11);
vertCcLo   = asInt.(\vertCcLo, 0);
vertCcHi   = asInt.(\vertCcHi, 127);
vertFreq   = asNum.(\vertFreq, (horizLo * horizHi).sqrt);
duration   = asNum.(\duration, 4.0);
gateOff    = asNum.(\gateOff, 2.0);
bright     = asNum.(\bright, 0.5);
vel        = asNum.(\vel, 0.7);
decay      = asNum.(\decay, 11);

horizFreqs = if(horizSteps > 1) {
    Array.geom(horizSteps, horizLo, (horizHi / horizLo) ** (1 / (horizSteps - 1)))
} {
    [horizLo]
};
vertCcs = if(vertSteps > 1) {
    Array.fill(vertSteps, { |i|
        (vertCcLo + (i * (vertCcHi - vertCcLo) / (vertSteps - 1))).round.asInteger
    })
} {
    [vertCcLo]
};

File.mkdir(outDir);
"output dir:    %".format(outDir).postln;
"horiz steps:   %".format(horizSteps).postln;
"horiz range:   % - % Hz".format(horizLo.round(0.01), horizHi.round(0.01)).postln;
"vert steps:    %".format(vertSteps).postln;
"vert CC range: % - %".format(vertCcLo, vertCcHi).postln;
"vert freq:     % Hz".format(vertFreq.round(0.01)).postln;
"duration:      % s  (gate off at %)".format(duration, gateOff).postln;
"".postln;

s.options.numOutputBusChannels = 2;
s.options.memSize = 65536;

s.waitForBoot({
    // ============================================================
    // SynthDef — copied verbatim from mpe_tanpura_xentool.scd:30-85
    // DO NOT MODIFY. Any divergence here invalidates the experiment.
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

        // Z → sustain drone via parallel CombL waveguide
        sustainEx = WhiteNoise.ar(1) * 0.015 * pressSm;
        sustainEx = LPF.ar(sustainEx, freqSm * brightMul);
        string    = string + CombL.ar(sustainEx, 0.05, freqSm.reciprocal, 4);

        // Amplitude-driven jawari
        ampFol = Amplitude.ar(string, 0.005, 0.15) + (pressSm * 0.5);
        sig    = (string * (1.0 + ampFol * 6)).tanh;
        sig    = sig + (string.cubed * ampFol * 1.4);
        sig    = (sig * 0.7) + (string * 0.3);

        // Slow string-to-string sympathy
        sig = DelayC.ar(sig, 0.03, SinOsc.kr(0.27, Rand(0, 2pi)).range(0.012, 0.022));

        // Post-synth tilt EQ — audible part of Y.
        sig = BHiShelf.ar(sig, 1500, 1, brightSm.linlin(0, 1, -24,  24));
        sig = BLowShelf.ar(sig, 400,  1, brightSm.linlin(0, 1,   8, -8));

        // ASR envelope, long release so KS decay isn't truncated
        env = EnvGen.kr(
            Env.asr(0.005, 1, 4.0, curve: \sin),
            gate,
            doneAction: 2
        );

        // Z → wide amp swell
        sig = sig * env * (0.3 + pressSm * 1.2) * 0.5;
        sig = Limiter.ar(sig, 0.95);

        Out.ar(0, Pan2.ar(sig, Rand(-0.3, 0.3)));
    }).add;
    // ============================================================
    // End of verbatim SynthDef block.
    // ============================================================

    s.sync;

    Routine({
        var t0 = Date.getDate.rawSeconds;
        var renderOne = { |outFile, params|
            var syn;
            // Configure recording format and start. s.record sets up a
            // hidden DiskOut on bus 0, so this captures whatever the
            // synth writes to Out.ar(0, ...).
            s.recHeaderFormat = "WAV";
            s.recSampleFormat = "int16";
            s.recChannels = 2;
            s.record(outFile, numChannels: 2);
            s.sync;
            // Tiny safety wait so the recorder is definitely live before
            // the synth starts writing samples.
            0.05.wait;

            syn = Synth(\mpeTanpuraXentool, params ++ [\gate, 1]);
            gateOff.wait;
            syn.set(\gate, 0);
            (duration - gateOff).wait;

            s.stopRecording;
            s.sync;
            // Brief gap so disk write fully flushes and the next take
            // starts cleanly — no overlap, no half-written tail.
            0.3.wait;
        };

        // ---- horizontal sweep ----
        "horizontal sweep: brightness=% (default), freq varies".format(bright).postln;
        horizFreqs.do { |f, i|
            var fname = outDir +/+ "horiz_%_freq_%.wav".format(pad.(i, 2), fmtFreq.(f));
            "  [%/%] %  freq=% Hz".format(i + 1, horizFreqs.size, fname.basename, f.round(0.01)).postln;
            renderOne.value(fname, [
                \freq, f, \bright, bright, \vel, vel, \decay, decay
            ]);
        };

        "".postln;

        // ---- vertical sweep ----
        "vertical sweep: freq=% Hz, CC74 varies (bright = cc/127)".format(vertFreq.round(0.01)).postln;
        vertCcs.do { |cc, i|
            var b = cc / 127;
            var fname = outDir +/+ "vert_%_cc_%.wav".format(pad.(i, 2), pad.(cc, 3));
            "  [%/%] %  cc=%  bright=%".format(
                i + 1, vertCcs.size, fname.basename, cc, b.round(0.001)
            ).postln;
            renderOne.value(fname, [
                \freq, vertFreq, \bright, b, \vel, vel, \decay, decay
            ]);
        };

        "".postln;
        "Done in % s. Wrote % files to %.".format(
            (Date.getDate.rawSeconds - t0).round(0.1),
            horizFreqs.size + vertCcs.size,
            outDir
        ).postln;

        // Quit server cleanly, then exit sclang.
        s.quit;
        0.5.wait;
        0.exit;
    }).play;
});
)
