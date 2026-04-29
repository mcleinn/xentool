// Piano-style MIDI hello-world.
// Connects all MIDI inputs, plays a physical-model piano on noteOn,
// prints verbose debug for every incoming MIDI message.
//
// Aftertouch behaviour: if the player keeps a key held (no noteOff), a
// quiet noise→CombL "sustain body" tracks channel pressure (and per-note
// pressure when present) so the note keeps speaking with loudness driven
// by aftertouch. At `press = 0` the sustain layer is silent and the
// synth sounds identical to the original hammer-pluck patch.

s.waitForBoot({

    SynthDef(\piano, { |freq=440, amp=0.3, gate=1, press=0|
        var hammer, strings, sig, env, pressSm, sustainEx, sustain;

        // ── Hammer strike: filtered noise burst (unchanged) ──────────
        hammer = WhiteNoise.ar(0.5) * EnvGen.kr(Env.perc(0.001, 0.02));
        hammer = HPF.ar(hammer, 100);

        // ── Three slightly detuned strings (unchanged) ───────────────
        strings = Mix([
            Pluck.ar(hammer, Impulse.kr(0), 0.05, freq.reciprocal,           8, 0.45),
            Pluck.ar(hammer, Impulse.kr(0), 0.05, (freq * 1.003).reciprocal, 8, 0.45) * 0.6,
            Pluck.ar(hammer, Impulse.kr(0), 0.05, (freq * 0.997).reciprocal, 8, 0.45) * 0.6,
        ]);

        // Velocity → brightness; a hint of soundboard resonance (unchanged)
        sig = LPF.ar(strings, 800 + (amp * 4500));
        sig = sig + (CombC.ar(sig, 0.04, 0.03, 1) * 0.15);

        // ── Sustain body driven by channel/poly aftertouch ──────────
        // Quiet noise-excited CombL waveguide tuned to `freq`, scaled by
        // smoothed aftertouch. At `press = 0` `sustainEx` is exactly
        // zero, so this branch contributes nothing and the original
        // hammer-pluck timbre is unchanged. As pressure rises the
        // sustained body fades in and keeps the note speaking past the
        // ~8 s natural Pluck decay.
        pressSm   = press.lag(0.08);
        sustainEx = WhiteNoise.ar(1) * 0.02 * pressSm;
        sustainEx = LPF.ar(sustainEx, 800 + (pressSm * 4000));
        sustain   = CombL.ar(sustainEx, 0.05, freq.reciprocal, 4);
        sig       = sig + sustain;

        // Release on noteOff via gate=0 (unchanged)
        env = EnvGen.kr(Env.asr(0.001, 1, 0.18, curve: -4), gate, doneAction: 2);

        Out.ar(0, Pan2.ar(sig * env * amp * 0.5, 0));
    }).add;

    s.sync;

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
    "".postln;

    // Voices keyed by `(chan → (num → Synth))`. Wooting emits Lumatone
    // channel-striped MIDI: the same `num` on different channels is a
    // distinct microtonal pitch, so a flat dict keyed by `num` would
    // collide voices across channels.
    ~notes = ();
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
        ~log.value("noteOn ", chan,
            "note=% (%) vel=%".format(num, ~noteName.value(num), vel), src);
        if(vel > 0) {
            // Free any earlier voice on the same (chan, num) before
            // replacing it — defends against missing noteOff messages.
            ~notes[chan] = ~notes[chan] ?? ();
            ~notes[chan][num] !? { |old| old.release };
            ~notes[chan][num] = Synth(\piano, [
                \freq, num.midicps,
                \amp,  vel.linexp(1, 127, 0.04, 0.5)
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

    MIDIdef.bend(\dbgBend, { |val, chan, src|
        ~log.value("bend   ", chan, "val=%".format(val), src);
    });

    MIDIdef.program(\dbgProg, { |num, chan, src|
        ~log.value("program", chan, "num=%".format(num), src);
    });

    // Channel pressure → drive the `press` parameter on every voice
    // currently held on that channel. Loudness of the sustain body
    // tracks this in real time.
    MIDIdef.touch(\dbgTouch, { |val, chan, src|
        var press = (val / 127).clip(0, 1);
        ~log.value("aftrtch", chan, "val=%".format(val), src);
        ~notes[chan] !? { |dict|
            dict.do { |s| s !? { s.set(\press, press) } };
        };
    });

    // Per-note polyphonic aftertouch (rare on Wooting; common on MPE
    // controllers). Same mapping as channel pressure but scoped to a
    // single (chan, num) voice.
    MIDIdef.polytouch(\dbgPoly, { |val, num, chan, src|
        var press = (val / 127).clip(0, 1);
        ~log.value("polyAt ", chan, "note=% val=%".format(num, val), src);
        ~notes[chan] !? { |dict|
            dict[num] !? { |s| s.set(\press, press) };
        };
    });

});
