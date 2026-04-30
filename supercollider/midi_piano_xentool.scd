// Piano-style MIDI hello-world for xentool's Wooting (classic-MIDI) flow.
// Connects all MIDI inputs, plays a physical-model piano on noteOn,
// prints verbose debug for every incoming MIDI message.
//
// Aftertouch behaviour: if the player keeps a key held (no noteOff), a
// quiet noise→CombL "sustain body" tracks channel pressure (and per-note
// pressure when present) so the note keeps speaking with loudness driven
// by aftertouch. At `press = 0` the sustain layer is silent and the
// synth sounds identical to the original hammer-pluck patch.
//
// Tuning: SuperCollider doesn't ship with an MTS-ESP client (xentool's
// Wooting backend is the MTS-ESP master, but only `amsynth_multichannel`
// links libMTSClient out of the box). Instead of routing through MTS-ESP,
// we re-derive each note's frequency from the same formula xentool uses
// to fill its MTS table — `freq = C0 * 2^(virtual_pitch / edo)` where
// `virtual_pitch = (chan-1)*edo + note + pitch_offset`. The Lumatone
// channel-stripe encoding matches xentool's `build_tuning_table`, so the
// emitted frequencies are bit-identical to what an MTS-ESP client would
// receive.
//
// Set `~tuningEdo` and `~pitchOffset` below to match the active layout.
// The defaults match `wtn/edo31.wtn` (the layout that
// `scripts/run-all-wooting.bat` pins).

// ── Tuning configuration ─────────────────────────────────────────────────
// Edit these to match the layout xentool is currently serving. Both
// values are also used by xentool itself when it builds the MTS-ESP
// table (see `src/wooting/serve.rs::build_tuning_table` and
// `src/mts.rs::edo_freq_hz`). C0 reference is `16.351597831287414 Hz`,
// matching xentool's master so frequencies agree to within float
// precision.
~tuningEdo   = 31;   // edo31.wtn / edo31.xtn → 31; change to 12, 24, 53, ... as needed
~pitchOffset = 0;    // matches the .wtn / .xtn `PitchOffset=` header
~tuningC0    = 16.351597831287414;  // Hz; do not touch unless xentool's reference changes
~bendRange   = 2;    // semitones; pianistic ±2 semis at full press of a Wooting
                     // bend key. xentool's Wooting backend emits raw 14-bit
                     // pitch-bend without retune scaling (unlike the Exquis
                     // MPE flow, which uses ±16 to encode microtonal retunes
                     // into per-note bend values). Bump to 16 if you want a
                     // wider expressive bend window.

~edoFreq = { |abs_pitch|
    ~tuningC0 * (2.pow(abs_pitch / ~tuningEdo))
};

s.waitForBoot({

    SynthDef(\piano, { |freq=440, amp=0.3, gate=1, press=0, bend=0, bright=0.5,
        voice=0,                        // 0=piano, 1=organ, 2=Rhodes EP
        attackTime=0.001, decayTime=0.18, sustainLevel=1.0, releaseTime=0.18,
        velSensitivity=1.0,
        dampScale=1, brightScale=1, hammerHardness=100, detuneAmt=0.003,
        droneAmt=1.0, droneType=0,      // 0=CombL, 1=Sine, 2=Off
        pressSwellLo=1.0, pressSwellHi=1.0,
        yMode=5, yRate=5,               // 0=LPF, 1=Trem, 2=Leslie, 3=Chorus, 4=Vibrato, 5=Off
        leslieMin=0.7, leslieMax=7.0|

        var hammer, strings, pianoSig, organSig, rhodesSig, sig, env;
        var pressSm, brightSm, sustainEx, sustain;
        var bendRatio, freqSm;
        var rhodesAttack, leslieLfo;
        var yLpfSig, yTremSig, yLeslieSig, yChorusSig, yVibSig;

        // Smoothed expression: kill .set() zipper noise.
        bendRatio = (2 ** (bend / 12)).lag(0.025);
        freqSm    = freq * bendRatio;
        brightSm  = bright.lag(0.10);
        pressSm   = press.lag(0.08);

        // === Piano voice (Pluck/Karplus-Strong + soundboard) ===
        // Three slightly-detuned strings give the natural piano chorus.
        // detuneAmt sweeps from 0 (perfect unison) to ~0.01 (honky-tonk).
        hammer = WhiteNoise.ar(0.5) * EnvGen.kr(Env.perc(0.001, 0.02));
        hammer = HPF.ar(hammer, hammerHardness);
        strings = Mix([
            Pluck.ar(hammer, Impulse.kr(0), 0.05, freqSm.reciprocal,                       8 * dampScale, 0.45),
            Pluck.ar(hammer, Impulse.kr(0), 0.05, (freqSm * (1 + detuneAmt)).reciprocal,   8 * dampScale, 0.45) * 0.6,
            Pluck.ar(hammer, Impulse.kr(0), 0.05, (freqSm * (1 - detuneAmt)).reciprocal,   8 * dampScale, 0.45) * 0.6,
        ]);
        pianoSig = LPF.ar(strings, (800 + (amp * 4500)) * brightScale);
        pianoSig = pianoSig + (CombC.ar(pianoSig, 0.04, 0.03, 1) * 0.15);

        // === Organ voice (Hammond-style additive drawbars) ===
        // Fixed lower drawbars (8' + 4' equivalents at fundamental + 2nd)
        // give a body that's audible even at bright=0; brightness opens up
        // upper drawbars (3rd / 4th / 6th harmonics) for a more piercing
        // top-end-on full Y.
        organSig = (
            (SinOsc.ar(freqSm)     * 0.6)
            + (SinOsc.ar(freqSm * 2) * 0.4)
            + (SinOsc.ar(freqSm * 3) * 0.3 * brightSm)
            + (SinOsc.ar(freqSm * 4) * 0.25 * brightSm)
            + (SinOsc.ar(freqSm * 6) * 0.2 * brightSm.squared)
            + (SinOsc.ar(freqSm * 8) * 0.15 * brightSm.squared)
        ) * 0.4;

        // === Rhodes EP voice (sine + percussive bell harmonics) ===
        // Approximation of a Yamaha DX-style EP: pure sine fundamental with
        // a fast percussive bell-like overtone burst on the attack. Avoids
        // proper FM (PMOsc) for portability; close enough for the studio.
        rhodesAttack = EnvGen.kr(Env.perc(0.001, 0.25));
        rhodesSig = (
            (SinOsc.ar(freqSm) * 0.6)
            + (SinOsc.ar(freqSm * 4) * 0.4 * rhodesAttack)
            + (SinOsc.ar(freqSm * 7) * 0.2 * rhodesAttack)
        );

        // === Voice select ===
        sig = Select.ar(voice, [pianoSig, organSig, rhodesSig]);

        // === Press-driven sustain body (works for any voice) ===
        // Quiet noise-excited resonator tuned to freqSm. droneAmt scales
        // amplitude; droneType picks the sustain DSP. CombL keeps the
        // pluck-derived character past Pluck's natural decay.
        sustainEx = WhiteNoise.ar(1) * 0.02 * pressSm * droneAmt;
        sustainEx = LPF.ar(sustainEx, 800 + (pressSm * 4000));
        sustain = Select.ar(droneType, [
            CombL.ar(sustainEx, 0.05, freqSm.reciprocal, 4),
            SinOsc.ar(freqSm) * 0.05 * pressSm * droneAmt,
            DC.ar(0)
        ]);
        sig = sig + sustain;

        // === Y-axis effect (post-synth) ===
        // yMode picks what brightSm drives. Defaults to 5 (Off) so the
        // patch sounds identical to the original until the user opts in.
        //   0 = LPF (tone — Y closes the lid on a piano)
        //   1 = Tremolo (Rhodes / Wurlitzer amp wobble at yRate Hz)
        //   2 = Leslie (rotary speaker — Y drives speed slow→fast)
        //   3 = Chorus (organ/EP shimmer; Y drives mod depth)
        //   4 = Vibrato (delay-line pitch mod at yRate Hz)
        //   5 = Off (no post-synth effect)
        yLpfSig    = LPF.ar(sig, brightSm.linexp(0, 1, 250, 12000).clip(20, 18000));
        yTremSig   = sig * SinOsc.kr(yRate).range(1 - (brightSm * 0.6), 1);
        leslieLfo  = SinOsc.kr(brightSm.linlin(0, 1, leslieMin, leslieMax));
        yLeslieSig = sig * leslieLfo.range(0.75, 1.0)
                     + (DelayC.ar(sig, 0.02, leslieLfo.range(0.001, 0.005)) * 0.5);
        yChorusSig = sig + (DelayC.ar(sig, 0.03, SinOsc.kr(0.6).range(0.005, 0.020) * brightSm) * 0.5);
        yVibSig    = DelayC.ar(sig, 0.01, 0.005 + (SinOsc.kr(yRate).range(-1, 1) * 0.003 * brightSm));

        sig = Select.ar(yMode, [yLpfSig, yTremSig, yLeslieSig, yChorusSig, yVibSig, sig]);

        // === ADSR envelope ===
        // attackTime/decayTime/sustainLevel/releaseTime expose what was
        // hardcoded `Env.asr(0.001, 1, 0.18)` in the original.
        env = EnvGen.kr(
            Env.adsr(attackTime, decayTime, sustainLevel, releaseTime, curve: -4),
            gate, doneAction: 2);

        // === Press swell ===
        // Default 1.0/1.0 = no swell (preserves original loudness curve).
        sig = sig * env * (pressSwellLo + (pressSm * (pressSwellHi - pressSwellLo)));

        // velSensitivity bends the velocity → amp curve. 1.0 = linear (current),
        // <1 compresses (loud at low vel), >1 expands (quiet at low vel).
        sig = sig * (amp.pow(velSensitivity)) * 0.5;

        Out.ar(0, Pan2.ar(sig, 0));
    }).add;

    SynthDef(\pianoReverb, { |mix=0.18, room=0.72, damp=0.5|
        var sig = In.ar(0, 2);
        sig = FreeVerb2.ar(sig[0], sig[1], mix.lag(0.05), room.lag(0.05), damp);
        ReplaceOut.ar(0, sig);
    }).add;

    SynthDef(\pianoMaster, { |amp=1|
        var sig = In.ar(0, 2);
        sig = sig * amp.lag(0.05);
        ReplaceOut.ar(0, sig);
    }).add;

    s.sync;

    // Reverb is added first so master sits after it in the chain
    // (voices → reverb → master → output).
    ~pianoReverbSynth = Synth.tail(s, \pianoReverb);
    ~pianoMasterSynth = Synth.tail(s, \pianoMaster);

    MIDIClient.init;
    MIDIIn.connectAll;

    "".postln;
    "=== MIDI input sources ===".postln;
    MIDIClient.sources.do { |src, i|
        "  [%] uid=%  device=\"%\"  port=\"%\"".format(
            i, src.uid, src.device, src.name
        ).postln;
    };
    "===========================".postln;
    "Listening for MIDI. Press Ctrl+C to quit.".postln;
    "Tuning: %-EDO, pitch_offset=% (edit ~tuningEdo / ~pitchOffset to change)".format(
        ~tuningEdo, ~pitchOffset
    ).postln;
    "".postln;

    // ── Live tuning updates from xentool ─────────────────────────────
    // When xentool serve runs with `--tune-supercollider` it broadcasts
    //   /xentool/tuning <edo:int> <pitch_offset:int> <layout_id:str>
    // to udp://127.0.0.1:57120 (sclang's default OSC port) at startup,
    // on every layout cycle, and every 3 s as a resync. We track those
    // here so a layout swap in xentool reaches the synth without needing
    // to restart sclang.
    OSCdef(\xentoolTuning, { |msg|
        var edo = msg[1], offset = msg[2], layoutId = (msg[3] ? "");
        if(edo.notNil and: { edo.asInteger > 0 }) {
            ~tuningEdo = edo.asInteger;
        };
        if(offset.notNil) {
            ~pitchOffset = offset.asInteger;
        };
        "Tuning updated from xentool: %-EDO, pitch_offset=%, layout=%".format(
            ~tuningEdo, ~pitchOffset, layoutId
        ).postln;
    }, '/xentool/tuning');

    // ── Studio-controllable performance parameters ──────────────────────
    // All start at values that reproduce the original sound. The
    // piano_studio web UI tweaks these via OSC (/piano/set, /piano/batch);
    // changes propagate to currently-held voices via Synth.set and to new
    // voices through the noteOn handler below.
    ~kparams = (
        // Voice & global
        voice:          0,      // 0=piano, 1=organ, 2=Rhodes
        velSensitivity: 1.0,
        // ADSR
        attackTime:     0.001,
        decayTime:      0.18,
        sustainLevel:   1.0,
        releaseTime:    0.18,
        // Piano-specific
        dampScale:      1.0,
        brightScale:    1.0,
        hammerHardness: 100,
        detuneAmt:      0.003,
        // Drone/sustain
        droneAmt:       1.0,
        droneType:      0,      // 0=CombL, 1=Sine, 2=Off
        pressSwellLo:   1.0,
        pressSwellHi:   1.0,
        // Y-axis effect
        yMode:          5,      // 0=LPF, 1=Trem, 2=Leslie, 3=Chorus, 4=Vibrato, 5=Off
        yRate:          5.0,
        leslieMin:      0.7,
        leslieMax:      7.0,
        // Output
        reverbMix:      0.18,
        reverbRoom:     0.72,
        masterAmp:      1.0
    );

    // Voices keyed by `(chan → (num → Synth))`. Wooting emits Lumatone
    // channel-striped MIDI: the same `num` on different channels is a
    // distinct microtonal pitch, so a flat dict keyed by `num` would
    // collide voices across channels.
    ~notes = ();
    // Latest pitch-bend value per channel, in semitones. xentool's
    // Wooting backend broadcasts pitch bend to *all* channel-stripes
    // currently in use on a board (see
    // `src/wooting/serve.rs::emit_pitchbend_for_board`), so each new
    // note starts with the channel's current bend already applied.
    ~chanBendSemi = ();
    ~noteNames  = ["C","C#","D","D#","E","F","F#","G","G#","A","A#","B"];
    ~noteName   = { |n| ~noteNames[n % 12] ++ ((n / 12).floor - 1).asInteger };
    ~deviceFor  = { |uid|
        var d = MIDIClient.sources.detect({ |x| x.uid == uid });
        if(d.notNil) { "%/%".format(d.device, d.name) } { "uid=" ++ uid }
    };
    ~stamp      = { Date.getDate.format("%H:%M:%S") };
    ~log        = { |kind, chan, body, src|
        "[%] %  ch=%  %  device=%".format(
            ~stamp.value, kind, chan+1, body, ~deviceFor.value(src)
        ).postln;
    };

    MIDIdef.noteOn(\dbgOn, { |vel, num, chan, src|
        var virtual_pitch, freq, bendSemi;
        // Lumatone-style channel-stripe encoding (matches xentool's
        // `build_tuning_table`): each MIDI channel = a different EDO
        // offset, so the actual sounding pitch is
        // `(channel-1)*edo + note + pitch_offset` EDO steps above C0.
        // sclang gives `chan` zero-indexed (0..15), so the (chan-1)
        // already maps to plain `chan` here. `~edoFreq` returns the
        // same Hz value xentool puts into its MTS-ESP table for that
        // virtual pitch.
        virtual_pitch = (chan * ~tuningEdo) + num + ~pitchOffset;
        freq = ~edoFreq.value(virtual_pitch);
        bendSemi = ~chanBendSemi[chan] ? 0;
        ~log.value("noteOn ", chan,
            "note=% (%) vel=% → vp=% freq=%Hz bend=%s".format(
                num, ~noteName.value(num), vel, virtual_pitch,
                freq.round(0.01), bendSemi.round(0.001)
            ), src);
        if(vel > 0) {
            // Free any earlier voice on the same (chan, num) before
            // replacing it — defends against missing noteOff messages.
            ~notes[chan] = ~notes[chan] ?? ();
            ~notes[chan][num] !? { |old| old.release };
            ~notes[chan][num] = Synth(\piano, [
                \freq,           freq,
                \amp,            vel.linexp(1, 127, 0.04, 0.5),
                \bend,           bendSemi,
                \voice,          ~kparams.voice,
                \velSensitivity, ~kparams.velSensitivity,
                \attackTime,     ~kparams.attackTime,
                \decayTime,      ~kparams.decayTime,
                \sustainLevel,   ~kparams.sustainLevel,
                \releaseTime,    ~kparams.releaseTime,
                \dampScale,      ~kparams.dampScale,
                \brightScale,    ~kparams.brightScale,
                \hammerHardness, ~kparams.hammerHardness,
                \detuneAmt,      ~kparams.detuneAmt,
                \droneAmt,       ~kparams.droneAmt,
                \droneType,      ~kparams.droneType,
                \pressSwellLo,   ~kparams.pressSwellLo,
                \pressSwellHi,   ~kparams.pressSwellHi,
                \yMode,          ~kparams.yMode,
                \yRate,          ~kparams.yRate,
                \leslieMin,      ~kparams.leslieMin,
                \leslieMax,      ~kparams.leslieMax
            ]);
        } {
            // velocity-zero noteOn == noteOff per MIDI spec
            ~notes[chan] !? { |dict|
                dict[num] !? { |s| s.release };
                dict[num] = nil;
            };
        };
    });

    MIDIdef.noteOff(\dbgOff, { |vel, num, chan, src|
        ~log.value("noteOff", chan,
            "note=% (%) vel=%".format(num, ~noteName.value(num), vel), src);
        ~notes[chan] !? { |dict|
            dict[num] !? { |s| s.release };
            dict[num] = nil;
        };
    });

    MIDIdef.cc(\dbgCC, { |val, num, chan, src|
        ~log.value("cc     ", chan, "num=% val=%".format(num, val), src);
    });

    // Per-channel pitch bend (0xE0). xentool's Wooting backend broadcasts
    // bend to every channel currently used by a board (so a single bend
    // gesture retunes the whole stripe in lockstep). `val` is the raw
    // 14-bit bend (0..16383, center 8192); we map to semitones via
    // `~bendRange` (16 semis to match xentool's `--pb-range` default for
    // Exquis). The smoothed `bend` parameter on the SynthDef avoids
    // zipper noise when bend ramps quickly.
    MIDIdef.bend(\dbgBend, { |val, chan, src|
        var semi = (val - 8192) / 8192 * ~bendRange;
        ~chanBendSemi[chan] = semi;
        ~log.value("bend   ", chan,
            "val=% → % semi (range=±%)".format(
                val, semi.round(0.001), ~bendRange
            ), src);
        ~notes[chan] !? { |dict|
            dict.do { |s| s !? { s.set(\bend, semi) } };
        };
    });

    MIDIdef.program(\dbgProg, { |num, chan, src|
        ~log.value("program", chan, "num=%".format(num), src);
    });

    // Per-note polyphonic aftertouch (0xA0). This is the primary
    // pressure channel xentool's Wooting backend emits — see
    // `src/wooting/serve.rs::poly_pressure`. Loudness of the sustain
    // body for the matching `(chan, num)` voice tracks this in real
    // time.
    MIDIdef.polytouch(\dbgPoly, { |val, num, chan, src|
        var press = (val / 127).clip(0, 1);
        ~log.value("polyAt ", chan, "note=% val=%".format(num, val), src);
        ~notes[chan] !? { |dict|
            dict[num] !? { |s| s.set(\press, press) };
        };
    });

    // Channel pressure (0xD0). Wooting via xentool doesn't emit this
    // path (it uses 0xA0 per-note instead), but other controllers
    // routed to the same MIDI port might, so handle it: fan out to
    // every voice currently held on that channel.
    MIDIdef.touch(\dbgTouch, { |val, chan, src|
        var press = (val / 127).clip(0, 1);
        ~log.value("aftrtch", chan, "val=%".format(val), src);
        ~notes[chan] !? { |dict|
            dict.do { |s| s !? { s.set(\press, press) } };
        };
    });

    // CC74 → bright (Y axis). The SynthDef uses `bright` for the active
    // yMode (LPF/Trem/Leslie/Chorus/Vibrato). Wooting users can configure
    // the analog Y axis to emit CC74 if they want a live tonal control;
    // without that, `bright` stays at its 0.5 default and the post-synth
    // effect is static.
    MIDIdef.cc(\dbgCC74, { |val, num, chan, src|
        var bright = (val / 127).clip(0, 1);
        ~notes[chan] !? { |dict|
            dict.do { |s| s !? { s.set(\bright, bright) } };
        };
    }, 74);

    // ── Studio UI OSC listeners ──────────────────────────────────────────
    // The piano_studio/ web UI (port 9101) sends OSC to UDP 57123.
    // /piano/set <name> <value> updates ~kparams (so future noteOns pick
    // up the new value) AND pushes the change to every currently-held
    // voice via Synth.set, for live tweaking on already-sounding notes.
    //
    // Reverb / master are separate Synths; their set messages route to
    // ~pianoReverbSynth / ~pianoMasterSynth instead of voice synths.
    ~studioPort = 57123;
    ~studioReplyAddr = NetAddr("127.0.0.1", 57124);
    thisProcess.openUDPPort(~studioPort);

    ~studioApply = { |name, value|
        var key = name.asSymbol;
        var floatVal = value.asFloat;
        ~kparams[key] = floatVal;
        case
            { key == \reverbMix }  { ~pianoReverbSynth.set(\mix, floatVal) }
            { key == \reverbRoom } { ~pianoReverbSynth.set(\room, floatVal) }
            { key == \masterAmp }  { ~pianoMasterSynth.set(\amp, floatVal) }
            {
                ~notes.do { |dict|
                    dict.do { |v| v.notNil.if({ v.set(key, floatVal) }) };
                };
            };
    };

    OSCdef(\pianoStudioSet, { |msg|
        if(msg.size >= 3) { ~studioApply.value(msg[1], msg[2]) };
    }, '/piano/set', recvPort: ~studioPort);

    OSCdef(\pianoStudioBatch, { |msg|
        var i = 1;
        while({ i + 1 < msg.size }, {
            ~studioApply.value(msg[i], msg[i + 1]);
            i = i + 2;
        });
        "[piano studio] applied batch (% pairs)".format((msg.size - 1) / 2).postln;
    }, '/piano/batch', recvPort: ~studioPort);

    OSCdef(\pianoStudioDump, { |msg|
        var pairs = List.new;
        ~kparams.keysValuesDo { |k, v|
            pairs.add(k.asString); pairs.add(v.asFloat);
        };
        ~studioReplyAddr.sendMsg('/piano/dump.reply', *pairs.asArray);
        "[piano studio] dump replied (% params)".format(pairs.size / 2).postln;
    }, '/piano/dump', recvPort: ~studioPort);

    OSCdef(\pianoStudioReset, { |msg|
        var defaults = (
            voice: 0, velSensitivity: 1.0,
            attackTime: 0.001, decayTime: 0.18, sustainLevel: 1.0, releaseTime: 0.18,
            dampScale: 1.0, brightScale: 1.0, hammerHardness: 100, detuneAmt: 0.003,
            droneAmt: 1.0, droneType: 0, pressSwellLo: 1.0, pressSwellHi: 1.0,
            yMode: 5, yRate: 5.0, leslieMin: 0.7, leslieMax: 7.0,
            reverbMix: 0.18, reverbRoom: 0.72, masterAmp: 1.0
        );
        defaults.keysValuesDo { |k, v| ~studioApply.value(k.asString, v) };
        "[piano studio] reset to defaults".postln;
    }, '/piano/reset', recvPort: ~studioPort);

    "Piano studio OSC listening on udp://127.0.0.1:%".format(~studioPort).postln;

});
