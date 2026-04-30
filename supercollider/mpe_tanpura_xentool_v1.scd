// MPE tanpura — xentool-aware variant.
//
// Only listens on the virtual MIDI port that `xentool serve` writes to (so
// the raw Exquis port isn't double-handled). Auto-detects "loopMIDI Port"
// on Windows and "Midi Through" on Linux — both are xentool's defaults for
// their platforms; see ~xentoolPortMatches below to extend.
// Uses ~mpeBendRange = 16 to match xentool's default --pb-range. With
// xentool's snapshot byte 9 set to 0x30 (= 48/48 — full Exquis output) and
// --pb-range 16, full X slide produces ≈±16 semitones (±1600 c) at the
// synth.
//
// Logs are minimal: only noteOn/noteOff are printed, and noteOn includes
// the per-channel bend value that was active at the moment the note fired
// (= the tuning offset xentool injected). Y (CC74) and Z (channel pressure)
// are still wired into the synth but not logged, to keep the console quiet.
//
// Synth model is identical to mpe_tanpura_classic.scd:
//   X = pitch bend (meend — retunes the Karplus-Strong delay line)
//   Y = CC 74 — brightness (burst LPF + KS damping + tilt EQ)
//   Z = aftertouch — sustain drone + jawari intensity + amp swell
//   vel = pluck strength
//
// Make sure xentool serve is already running and emitting to its virtual
// MIDI port (loopMIDI Port on Windows, Midi Through on Linux) before
// running this script — otherwise the source-filter has nothing to match
// and no notes will be heard.

s.waitForBoot({

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

    SynthDef(\mpeReverb, { |mix=0.45, room=0.88, damp=0.5|
        var sig = In.ar(0, 2);
        sig = FreeVerb2.ar(sig[0], sig[1], mix.lag(0.05), room.lag(0.05), damp);
        ReplaceOut.ar(0, sig);
    }).add;

    SynthDef(\mpeMaster, { |amp=1|
        var sig = In.ar(0, 2);
        sig = sig * amp.lag(0.05);
        ReplaceOut.ar(0, sig);
    }).add;

    s.sync;

    // Reverb is added first so master sits after it in the chain
    // (voices → reverb → master → output).
    ~mpeReverbSynth = Synth.tail(s, \mpeReverb);
    ~mpeMasterSynth = Synth.tail(s, \mpeMaster);

    MIDIClient.init;
    MIDIIn.connectAll;

    "".postln;
    "=== MIDI input sources ===".postln;
    MIDIClient.sources.do { |src, i|
        "  [%] uid=%  device=\"%\"  port=\"%\"".format(i, src.uid, src.device, src.name).postln;
    };
    "===========================".postln;

    // Pick the virtual MIDI port that xentool writes to. Matches the device
    // or port name (case-insensitive) against any substring in
    // ~xentoolPortMatches. Defaults cover the canonical xentool outputs on
    // both platforms: "loopMIDI Port" on Windows; "Xentool Wooting" /
    // "Xentool Exquis MPE" on Linux/macOS (xentool publishes its own
    // virtual ALSA seq / CoreMIDI port). The legacy "Midi Through" entry
    // is kept so old installs that override --output to it still work.
    // If you run xentool with `--output` set to something else, prepend
    // your port name here.
    ~xentoolPortMatches = ["xentool", "loopmidi", "midi through", "midithrough"];
    ~xentoolUid = MIDIClient.sources.detect({ |src|
        var dev = src.device.toLower, nam = src.name.toLower;
        ~xentoolPortMatches.any({ |m| dev.contains(m) or: { nam.contains(m) } })
    }) !? { |src| src.uid };

    if(~xentoolUid.isNil) {
        "".postln;
        "[!] No MIDI source matching any of % found.".format(~xentoolPortMatches).postln;
        "[!] Start `xentool serve` first. On Windows, install loopMIDI and create a port named 'loopMIDI Port'.".postln;
        "[!] On Linux, xentool defaults to 'Midi Through' (always present via snd-seq).".postln;
    } {
        "[ok] xentool input bound: uid=%  device=%".format(
            ~xentoolUid,
            MIDIClient.sources.detect({ |s| s.uid == ~xentoolUid }).device
        ).postln;
    };

    ~mpeVoices       = ();    // chan → Synth currently held on that channel
    ~mpeBendSemi     = ();    // chan → last bend in semitones (for noteOn log)
    ~mpeBendLogged   = ();    // chan → last bend value already logged
    ~mpeBendRange    = 16;    // MUST match xentool's --pb-range (default 16)
    ~mpeBendLogStep  = nil;   // nil = bend logging disabled (default).
                              //   Set to 0 to log every event, 0.05 for
                              //   ≥5-cent moves, etc. Useful for debugging.
    ~touchMax        = 64;    // Exquis aftertouch caps at ~64/127 in xentool's
                              //   serve TUI. Map 0..~touchMax → 0..1 so full
                              //   press reaches the synth's whole Z range.
                              //   Tune up/down per controller behaviour.

    // ── Session logger ─────────────────────────────────────────────────────
    // Off by default; flip to true to capture every MIDI event + the
    // synth-parameter values it produced into a JSONL file. Used to
    // reproduce a real play session inside the test rig
    // (`supercollider/tests/tanpura/`) — once you hear the issue live,
    // grep the log for that timestamp to recover the exact param vector
    // the SynthDef saw.
    //
    // Writes are deferred: the MIDIdef callbacks only append a tiny JSON
    // string to an in-memory list (microseconds, no I/O), and a separate
    // Routine on SystemClock flushes the list to disk every 100 ms. Even
    // at hard MPE event rates (300+/s) this stays well below the audio
    // noise floor. A ShutDown hook flushes the final batch on exit so
    // events from the last 100 ms aren't lost when sclang quits.
    //
    // Raw-MIDI fallback MIDIdefs (added after the source-filtered ones
    // below) catch events from ANY MIDI source — even if the xentool
    // source matcher picked the wrong UID, the log will still contain
    // every incoming MIDI byte.
    ~logEnabled = true;
    ~logDir = PathName(thisProcess.nowExecutingPath).pathOnly +/+ "logs";
    ~logQueue = List.new;
    ~logPath = nil;
    ~logCount = 0;

    ~jsonStr = { |s|
        "\"" ++ s.asString.replace("\\", "\\\\").replace("\"", "\\\"") ++ "\""
    };
    ~jsonNum = { |n|
        if(n.isInteger) { n.asString } { n.round(0.0001).asString }
    };
    ~jsonObj = { |evt|
        var pairs = List.new;
        evt.keysValuesDo { |k, v|
            var vStr = case
                { v.isNumber } { ~jsonNum.(v) }
                { v.isNil } { "null" }
                { v == true } { "true" }
                { v == false } { "false" }
                { ~jsonStr.(v) };
            pairs.add(~jsonStr.(k) ++ ":" ++ vStr);
        };
        "{" ++ pairs.asArray.join(",") ++ "}"
    };

    // Tiny when disabled (one boolean check + Event construction). Cheap
    // when enabled (string format, list append). No I/O on this thread.
    ~logEvent = { |evt|
        if(~logEnabled) {
            ~logQueue.add(~jsonObj.value(evt));
            ~logCount = ~logCount + 1;
        };
    };

    // Sync flush helper — opens, writes, closes (auto-flushes on close).
    // Called from the timed Routine and from the ShutDown hook.
    ~flushLogQueue = {
        if(~logEnabled and: { ~logQueue.size > 0 } and: { ~logPath.notNil }) {
            var batch = ~logQueue.copy;
            ~logQueue.clear;
            File.use(~logPath, "a", { |f|
                batch.do { |line|
                    f.putString(line); f.putString("\n");
                };
            });
        };
    };

    if(~logEnabled) {
        var ts = Date.getDate.format("%Y%m%d-%H%M%S");
        File.mkdir(~logDir);
        ~logPath = ~logDir +/+ "tanpura_" ++ ts ++ ".jsonl";
        "[log] session log: %".format(~logPath).postln;
        // SystemClock for tighter, more reliable cadence than AppClock.
        // 100 ms cadence keeps the worst-case lost-on-crash window small.
        Routine({
            loop {
                ~flushLogQueue.value;
                0.1.wait;
            };
        }).play(SystemClock);
        // Heartbeat every 5 s — confirms the logger is alive even when
        // there's no MIDI traffic, and gives a periodic timestamp anchor.
        Routine({
            loop {
                5.0.wait;
                ~logEvent.((t: Main.elapsedTime, kind: \heartbeat, count: ~logCount));
            };
        }).play(SystemClock);
        // Final flush on exit — guarantees no events sit in the queue
        // when sclang quits.
        ShutDown.add({
            ~flushLogQueue.value;
            "[log] flushed final batch on shutdown — % events total → %"
                .format(~logCount, ~logPath).postln;
        });
    };
    // ── end session logger ─────────────────────────────────────────────────

    "".postln;
    "MPE tanpura (xentool) active. Bend range = ±% semi (matches --pb-range). Y/Z prints disabled. Ctrl+C to quit.".format(~mpeBendRange).postln;
    if(~logEnabled) { "Session logging ON  →  %".format(~logPath).postln } { "Session logging off (set ~logEnabled = true at top to capture).".postln };
    "".postln;

    // ── OSC out to xentool's Live HUD ────────────────────────────────────────
    // The HUD listens on UDP 9000 by default (override with `--osc-port`).
    // `~hudParam` pushes a sticky parameter row; `~hudEvent` pushes a one-off
    // event line that fades from the HUD after a few seconds. Both calls are
    // fire-and-forget UDP so they never block the synth.
    //
    // On Windows, sending UDP to a port nothing is listening on triggers an
    // ICMP "Port Unreachable" reply, which sclang surfaces as
    // "SC_UdpInPort: received error - An existing connection was forcibly
    // closed by the remote host" — one error per send. To silence those when
    // xentool isn't running with `--hud`, set ~hudEnabled = false below
    // (or just start xentool first and reload this script).
    ~hudEnabled = true;
    ~xentoolHud = NetAddr("127.0.0.1", 9000);
    ~hudParam = { |group, name, value, unit|
        if(~hudEnabled) {
            var msg = ["/xentool/param/" ++ group ++ "/" ++ name, value.asFloat];
            if(unit.notNil and: { unit.size > 0 }) { msg = msg.add(unit) };
            ~xentoolHud.sendMsg(*msg);
        };
    };
    ~hudEvent = { |text|
        if(~hudEnabled) {
            ~xentoolHud.sendMsg("/xentool/event", text);
        };
    };

    // ── Performance parameter state ─────────────────────────────────────────
    // All start at the values that reproduce the current/classic sound, so
    // the synth behaves identically until you move an encoder or press a
    // button. Encoders 1-4 = CC 110-113, buttons 1-4 = CC 114-117, all on
    // MIDI channel 16 (chan index 15).
    ~kparams = (
        decay:       11,    // KS string decay (sec)         — Encoder 1
        reverbMix:   0.45,  // FreeVerb2 mix                  — Encoder 2
        reverbRoom:  0.88,  // FreeVerb2 room                 — Encoder 3
        masterAmp:   1.0,   // Output gain                    — Encoder 4
        dampScale:   1.0,   // KS loop-coef multiplier        — Button 2
        brightScale: 1.0    // Burst-LPF cutoff multiplier    — Button 3
    );

    // Cycle preset banks for the buttons. Index starts at the "classic" slot.
    ~decayPresets = [4, 11, 25];      ~decayPresetIdx  = 1;
    ~stringPresets = [0.6, 1.0, 1.5]; ~stringPresetIdx = 1;
    ~bodyPresets   = [0.5, 1.0, 2.0]; ~bodyPresetIdx   = 1;

    // Push the initial defaults so the HUD shows the current state on connect.
    // (Must run after `~kparams` is defined.)
    ~hudParam.value("string", "decay",       ~kparams.decay,       "s");
    ~hudParam.value("reverb", "mix",         ~kparams.reverbMix,   nil);
    ~hudParam.value("reverb", "room",        ~kparams.reverbRoom,  nil);
    ~hudParam.value("master", "amp",         ~kparams.masterAmp,   nil);
    ~hudParam.value("string", "dampScale",   ~kparams.dampScale,   nil);
    ~hudParam.value("string", "brightScale", ~kparams.brightScale, nil);
    ~hudEvent.value("synth ready");

    // Encoder 1 — decay (sec). 1 sec per detent.
    MIDIdef.cc(\xtEnc1, { |val, num, chan, src|
        if(chan == 15) {
            var delta = val - 64;
            ~kparams.decay = (~kparams.decay + delta).clip(0.5, 60);
            ~logEvent.((t: Main.elapsedTime, kind: \kparam, name: \decay, val: ~kparams.decay));
            "[enc1/decay]      → % s".format(~kparams.decay.round(0.1)).postln;
            ~hudParam.value("string", "decay", ~kparams.decay, "s");
            ~hudEvent.value("encoder 1: decay → %.1f s".format(~kparams.decay));
        };
    }, 110, srcID: ~xentoolUid);

    // Encoder 2 — reverb mix (0..1). ~0.05 per detent.
    MIDIdef.cc(\xtEnc2, { |val, num, chan, src|
        if(chan == 15) {
            var delta = val - 64;
            ~kparams.reverbMix = (~kparams.reverbMix + (delta * 0.05)).clip(0, 1);
            ~mpeReverbSynth.set(\mix, ~kparams.reverbMix);
            ~logEvent.((t: Main.elapsedTime, kind: \kparam, name: \reverbMix, val: ~kparams.reverbMix));
            "[enc2/rev mix]    → %".format(~kparams.reverbMix.round(0.01)).postln;
            ~hudParam.value("reverb", "mix", ~kparams.reverbMix, nil);
            ~hudEvent.value("encoder 2: reverb mix → %.2f".format(~kparams.reverbMix));
        };
    }, 111, srcID: ~xentoolUid);

    // Encoder 3 — reverb room (0..1). ~0.05 per detent.
    MIDIdef.cc(\xtEnc3, { |val, num, chan, src|
        if(chan == 15) {
            var delta = val - 64;
            ~kparams.reverbRoom = (~kparams.reverbRoom + (delta * 0.05)).clip(0, 1);
            ~mpeReverbSynth.set(\room, ~kparams.reverbRoom);
            ~logEvent.((t: Main.elapsedTime, kind: \kparam, name: \reverbRoom, val: ~kparams.reverbRoom));
            "[enc3/rev room]   → %".format(~kparams.reverbRoom.round(0.01)).postln;
            ~hudParam.value("reverb", "room", ~kparams.reverbRoom, nil);
            ~hudEvent.value("encoder 3: reverb room → %.2f".format(~kparams.reverbRoom));
        };
    }, 112, srcID: ~xentoolUid);

    // Encoder 4 — master amp (0..2). ~0.05 per detent.
    MIDIdef.cc(\xtEnc4, { |val, num, chan, src|
        if(chan == 15) {
            var delta = val - 64;
            ~kparams.masterAmp = (~kparams.masterAmp + (delta * 0.05)).clip(0, 2);
            ~mpeMasterSynth.set(\amp, ~kparams.masterAmp);
            ~logEvent.((t: Main.elapsedTime, kind: \kparam, name: \masterAmp, val: ~kparams.masterAmp));
            "[enc4/master]     → %".format(~kparams.masterAmp.round(0.01)).postln;
            ~hudParam.value("master", "amp", ~kparams.masterAmp, nil);
            ~hudEvent.value("encoder 4: master amp → %.2f".format(~kparams.masterAmp));
        };
    }, 113, srcID: ~xentoolUid);

    // Button 1 — cycle decay-time preset (short / medium / long).
    MIDIdef.cc(\xtBtn1, { |val, num, chan, src|
        if((chan == 15) and: { val > 0 }) {
            ~decayPresetIdx = (~decayPresetIdx + 1) % ~decayPresets.size;
            ~kparams.decay = ~decayPresets[~decayPresetIdx];
            ~logEvent.((t: Main.elapsedTime, kind: \kparam, name: \decay, val: ~kparams.decay, source: \btn1));
            "[btn1/decay-cyc]  → preset[%/%] = % s".format(
                ~decayPresetIdx + 1, ~decayPresets.size, ~kparams.decay).postln;
            ~hudParam.value("string", "decay", ~kparams.decay, "s");
            ~hudEvent.value("button 1: decay preset → % s".format(~kparams.decay));
        };
    }, 114, srcID: ~xentoolUid);

    // Button 2 — cycle string family (KS damping multiplier).
    //   0.6 = soft/dark gut · 1.0 = classic · 1.5 = hard/steel-bright
    MIDIdef.cc(\xtBtn2, { |val, num, chan, src|
        if((chan == 15) and: { val > 0 }) {
            var labels = ["soft", "classic", "hard"];
            ~stringPresetIdx = (~stringPresetIdx + 1) % ~stringPresets.size;
            ~kparams.dampScale = ~stringPresets[~stringPresetIdx];
            ~logEvent.((t: Main.elapsedTime, kind: \kparam, name: \dampScale, val: ~kparams.dampScale, label: labels[~stringPresetIdx]));
            "[btn2/string]     → % (dampScale=%)".format(
                labels[~stringPresetIdx], ~kparams.dampScale).postln;
            ~hudParam.value("string", "dampScale", ~kparams.dampScale, nil);
            ~hudEvent.value("button 2: string family → %".format(labels[~stringPresetIdx]));
        };
    }, 115, srcID: ~xentoolUid);

    // Button 3 — cycle body size (burst-LPF cutoff multiplier).
    //   0.5 = small/dark body · 1.0 = classic · 2.0 = large/bright body
    MIDIdef.cc(\xtBtn3, { |val, num, chan, src|
        if((chan == 15) and: { val > 0 }) {
            var labels = ["small", "classic", "large"];
            ~bodyPresetIdx = (~bodyPresetIdx + 1) % ~bodyPresets.size;
            ~kparams.brightScale = ~bodyPresets[~bodyPresetIdx];
            ~logEvent.((t: Main.elapsedTime, kind: \kparam, name: \brightScale, val: ~kparams.brightScale, label: labels[~bodyPresetIdx]));
            "[btn3/body]       → % (brightScale=%)".format(
                labels[~bodyPresetIdx], ~kparams.brightScale).postln;
            ~hudParam.value("string", "brightScale", ~kparams.brightScale, nil);
            ~hudEvent.value("button 3: body size → %".format(labels[~bodyPresetIdx]));
        };
    }, 116, srcID: ~xentoolUid);

    // Button 4 — reset all performance params to classic defaults.
    MIDIdef.cc(\xtBtn4, { |val, num, chan, src|
        if((chan == 15) and: { val > 0 }) {
            ~kparams.decay       = 11;
            ~kparams.reverbMix   = 0.45;
            ~kparams.reverbRoom  = 0.88;
            ~kparams.masterAmp   = 1.0;
            ~kparams.dampScale   = 1.0;
            ~kparams.brightScale = 1.0;
            ~decayPresetIdx  = 1;
            ~stringPresetIdx = 1;
            ~bodyPresetIdx   = 1;
            ~mpeReverbSynth.set(\mix, ~kparams.reverbMix, \room, ~kparams.reverbRoom);
            ~mpeMasterSynth.set(\amp, ~kparams.masterAmp);
            ~logEvent.((
                t: Main.elapsedTime, kind: \kparamReset,
                decay: ~kparams.decay, reverbMix: ~kparams.reverbMix,
                reverbRoom: ~kparams.reverbRoom, masterAmp: ~kparams.masterAmp,
                dampScale: ~kparams.dampScale, brightScale: ~kparams.brightScale
            ));
            "[btn4/reset]      → all params back to classic defaults".postln;
            ~hudParam.value("string", "decay",       ~kparams.decay,       "s");
            ~hudParam.value("reverb", "mix",         ~kparams.reverbMix,   nil);
            ~hudParam.value("reverb", "room",        ~kparams.reverbRoom,  nil);
            ~hudParam.value("master", "amp",         ~kparams.masterAmp,   nil);
            ~hudParam.value("string", "dampScale",   ~kparams.dampScale,   nil);
            ~hudParam.value("string", "brightScale", ~kparams.brightScale, nil);
            ~hudEvent.value("button 4: reset to classic defaults");
        };
    }, 117, srcID: ~xentoolUid);

    "Encoders/buttons on Exquis settings:".postln;
    "  Enc1 = decay      Btn1 = decay preset (4/11/25 s)".postln;
    "  Enc2 = reverb mix Btn2 = string family (soft/classic/hard)".postln;
    "  Enc3 = reverb room Btn3 = body size (small/classic/large)".postln;
    "  Enc4 = master amp Btn4 = reset all".postln;
    "".postln;

    ~deviceFor = { |uid|
        var d = MIDIClient.sources.detect({ |x| x.uid == uid });
        if(d.notNil) { "%/%".format(d.device, d.name) } { "uid=" ++ uid }
    };

    // Source-filter every MIDIdef to the xentool port only. If ~xentoolUid is
    // nil, srcID:nil makes MIDIdef listen to all sources — but in that state
    // the user already saw the warning above and can decide to fix or proceed.
    MIDIdef.noteOn(\xtTanpuraOn, { |vel, num, chan, src|
        var bendSemi = ~mpeBendSemi[chan] ? 0;
        var velNorm = vel.linexp(1, 127, 0.2, 1.0);
        if(vel > 0) {
            ~mpeVoices[chan] !? { |old| old.release };
            ~mpeVoices[chan] = Synth(\mpeTanpuraXentool, [
                \freq,        num.midicps,
                \vel,         velNorm,
                \bend,        bendSemi,
                \bright,      0.5,
                \press,       0,
                \decay,       ~kparams.decay,
                \dampScale,   ~kparams.dampScale,
                \brightScale, ~kparams.brightScale
            ]);
            ~logEvent.((
                t: Main.elapsedTime, kind: \noteOn,
                ch: chan + 1, note: num,
                freq: num.midicps, vel: velNorm,
                bend: bendSemi, bright: 0.5, press: 0,
                decay: ~kparams.decay,
                dampScale: ~kparams.dampScale,
                brightScale: ~kparams.brightScale
            ));
            "[noteOn]  ch=%  note=% vel=%  bend=% semi (% c)".format(
                chan+1, num, vel,
                bendSemi.round(0.001),
                (bendSemi * 100).round(0.1)
            ).postln;
        } {
            ~mpeVoices[chan] !? { |v| v.release };
            ~mpeVoices[chan] = nil;
            ~logEvent.((t: Main.elapsedTime, kind: \noteOff, ch: chan + 1, note: num, viaVel0: true));
        };
    }, srcID: ~xentoolUid);

    MIDIdef.noteOff(\xtTanpuraOff, { |vel, num, chan, src|
        ~mpeVoices[chan] !? { |v| v.release };
        ~mpeVoices[chan] = nil;
        // Reset the last-logged bend so the first bend on a new touch is
        // guaranteed to print.
        ~mpeBendLogged[chan] = nil;
        ~logEvent.((t: Main.elapsedTime, kind: \noteOff, ch: chan + 1, note: num));
        "[noteOff] ch=%  note=%".format(chan+1, num).postln;
    }, srcID: ~xentoolUid);

    // Y / CC74: route to synth, no postln (would flood). The session
    // logger captures every event anyway when enabled.
    MIDIdef.cc(\xtTanpuraCC74, { |val, num, chan, src|
        var bright = val / 127;
        ~mpeVoices[chan] !? { |v| v.set(\bright, bright) };
        ~logEvent.((t: Main.elapsedTime, kind: \cc74, ch: chan + 1, bright: bright));
    }, 74, srcID: ~xentoolUid);

    // Z / channel pressure: route to synth, no postln. Logger captures.
    MIDIdef.touch(\xtTanpuraTouch, { |val, chan, src|
        var press = (val / ~touchMax).clip(0, 1);
        ~mpeVoices[chan] !? { |v| v.set(\press, press) };
        ~logEvent.((t: Main.elapsedTime, kind: \touch, ch: chan + 1, press: press, raw: val));
    }, srcID: ~xentoolUid);

    // Pitch bend: track per-channel state so noteOn can log it, update synth,
    // and emit a throttled debug log (only when bend has moved more than
    // ~mpeBendLogStep semitones since the last printed value). Set
    // ~mpeBendLogStep to nil to disable bend logging entirely.
    MIDIdef.bend(\xtTanpuraBend, { |val, chan, src|
        var delta = val - 8192;
        var semi  = (delta / 8192) * ~mpeBendRange;
        var last  = ~mpeBendLogged[chan];
        var step  = ~mpeBendLogStep;
        ~mpeBendSemi[chan] = semi;
        ~mpeVoices[chan] !? { |v| v.set(\bend, semi) };
        ~logEvent.((t: Main.elapsedTime, kind: \bend, ch: chan + 1, semi: semi, raw: val));
        if(step.notNil and: { last.isNil or: { (semi - last).abs >= step } }) {
            ~mpeBendLogged[chan] = semi;
            "[bend]    ch=%  raw=%  Δ=%  → % semi (% c)  voice=%".format(
                chan+1, val, delta,
                semi.round(0.001),
                (semi * 100).round(0.1),
                if(~mpeVoices[chan].notNil) { "yes" } { "no" }
            ).postln;
        };
    }, srcID: ~xentoolUid);

    // ── Raw-MIDI fallback loggers ─────────────────────────────────────────
    // Belt-and-braces: capture EVERY incoming MIDI event regardless of
    // source. The structured handlers above only fire for events from
    // ~xentoolUid; if the source matcher picked the wrong UID (or none
    // matched at all), those events would be invisible. The raw loggers
    // record everything with the source UID so a partial-match scenario
    // is at least diagnosable from the log.
    //
    // For events that DO match ~xentoolUid, the log will contain BOTH
    // the structured entry (e.g. {kind:"cc74", bright:0.62}) AND the
    // raw entry ({kind:"rawCc", num:74, val:79, srcMatch:true}) — that
    // redundancy is intentional: structured = synth-side state, raw =
    // wire-level evidence.
    if(~logEnabled) {
        MIDIdef.noteOn(\xtRawNoteOn, { |val, num, chan, src|
            ~logEvent.((
                t: Main.elapsedTime, kind: \rawNoteOn,
                src: src, ch: chan + 1, note: num, vel: val,
                srcMatch: (src == ~xentoolUid)
            ));
        });
        MIDIdef.noteOff(\xtRawNoteOff, { |val, num, chan, src|
            ~logEvent.((
                t: Main.elapsedTime, kind: \rawNoteOff,
                src: src, ch: chan + 1, note: num, vel: val,
                srcMatch: (src == ~xentoolUid)
            ));
        });
        MIDIdef.cc(\xtRawCc, { |val, num, chan, src|
            ~logEvent.((
                t: Main.elapsedTime, kind: \rawCc,
                src: src, ch: chan + 1, num: num, val: val,
                srcMatch: (src == ~xentoolUid)
            ));
        });
        MIDIdef.bend(\xtRawBend, { |val, chan, src|
            ~logEvent.((
                t: Main.elapsedTime, kind: \rawBend,
                src: src, ch: chan + 1, val: val,
                srcMatch: (src == ~xentoolUid)
            ));
        });
        MIDIdef.touch(\xtRawTouch, { |val, chan, src|
            ~logEvent.((
                t: Main.elapsedTime, kind: \rawTouch,
                src: src, ch: chan + 1, val: val,
                srcMatch: (src == ~xentoolUid)
            ));
        });
        MIDIdef.polytouch(\xtRawPolyTouch, { |val, num, chan, src|
            ~logEvent.((
                t: Main.elapsedTime, kind: \rawPolyTouch,
                src: src, ch: chan + 1, note: num, val: val,
                srcMatch: (src == ~xentoolUid)
            ));
        });
        ~logEvent.((
            t: Main.elapsedTime, kind: \loggerStart,
            xentoolUid: ~xentoolUid,
            sourcesAvailable: MIDIClient.sources.collect { |s|
                "uid=" ++ s.uid ++ " device=\"" ++ s.device ++ "\" port=\"" ++ s.name ++ "\""
            }.asArray
        ));
    };
    // ── end raw-MIDI fallback ─────────────────────────────────────────────

});
