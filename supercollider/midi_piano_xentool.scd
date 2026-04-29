// Piano-style MIDI hello-world.
// Connects all MIDI inputs, plays a physical-model piano on noteOn,
// prints verbose debug for every incoming MIDI message.

s.waitForBoot({

    SynthDef(\piano, { |freq=440, amp=0.3, gate=1|
        var hammer, strings, sig, env;

        // Hammer strike: filtered noise burst
        hammer = WhiteNoise.ar(0.5) * EnvGen.kr(Env.perc(0.001, 0.02));
        hammer = HPF.ar(hammer, 100);

        // Three slightly detuned strings (real pianos have multiple per note)
        strings = Mix([
            Pluck.ar(hammer, Impulse.kr(0), 0.05, freq.reciprocal,           8, 0.45),
            Pluck.ar(hammer, Impulse.kr(0), 0.05, (freq * 1.003).reciprocal, 8, 0.45) * 0.6,
            Pluck.ar(hammer, Impulse.kr(0), 0.05, (freq * 0.997).reciprocal, 8, 0.45) * 0.6,
        ]);

        // Velocity → brightness; a hint of soundboard resonance
        sig = LPF.ar(strings, 800 + (amp * 4500));
        sig = sig + (CombC.ar(sig, 0.04, 0.03, 1) * 0.15);

        // Release on noteOff via gate=0
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

    ~notes      = ();
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
            ~notes[num] = Synth(\piano, [
                \freq, num.midicps,
                \amp,  vel.linexp(1, 127, 0.04, 0.5)
            ]);
        } {
            // velocity-zero noteOn == noteOff per MIDI spec
            if(~notes[num].notNil) { ~notes[num].release; ~notes[num] = nil };
        };
    });

    MIDIdef.noteOff(\dbgOff, { |vel, num, chan, src|
        ~log.value("noteOff", chan,
            "note=% (%) vel=%".format(num, ~noteName.value(num), vel), src);
        if(~notes[num].notNil) { ~notes[num].release; ~notes[num] = nil };
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

    MIDIdef.touch(\dbgTouch, { |val, chan, src|
        ~log.value("aftrtch", chan, "val=%".format(val), src);
    });

    MIDIdef.polytouch(\dbgPoly, { |val, num, chan, src|
        ~log.value("polyAt ", chan, "note=% val=%".format(num, val), src);
    });

});
