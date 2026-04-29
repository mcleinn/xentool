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
~bendRange   = 16;   // semitones; matches xentool's --pb-range default (Exquis +
                     // Wooting bend keys both produce raw 14-bit values; this
                     // constant is the synth-side interpretation)

~edoFreq = { |abs_pitch|
    ~tuningC0 * (2.pow(abs_pitch / ~tuningEdo))
};

s.waitForBoot({

    SynthDef(\piano, { |freq=440, amp=0.3, gate=1, press=0, bend=0|
        var hammer, strings, sig, env, pressSm, sustainEx, sustain;
        var bendRatio, freqSm;

        // ── Per-channel pitch bend ──────────────────────────────────
        // `bend` is in semitones (set per-voice from xentool's 14-bit
        // pitch-bend, scaled by `~bendRange`). `lag(0.025)` smooths
        // .set() jumps so glissando-style bend sweeps don't zip. At
        // `bend = 0` the ratio is exactly 1 so the synth is unchanged.
        bendRatio = (2 ** (bend / 12)).lag(0.025);
        freqSm    = freq * bendRatio;

        // ── Hammer strike: filtered noise burst (unchanged) ──────────
        hammer = WhiteNoise.ar(0.5) * EnvGen.kr(Env.perc(0.001, 0.02));
        hammer = HPF.ar(hammer, 100);

        // ── Three slightly detuned strings ──────────────────────────
        // Use `freqSm` so all three strings retune together when bend
        // arrives. The detune ratios (×1.003 / ×0.997) are unchanged.
        strings = Mix([
            Pluck.ar(hammer, Impulse.kr(0), 0.05, freqSm.reciprocal,           8, 0.45),
            Pluck.ar(hammer, Impulse.kr(0), 0.05, (freqSm * 1.003).reciprocal, 8, 0.45) * 0.6,
            Pluck.ar(hammer, Impulse.kr(0), 0.05, (freqSm * 0.997).reciprocal, 8, 0.45) * 0.6,
        ]);

        // Velocity → brightness; a hint of soundboard resonance (unchanged)
        sig = LPF.ar(strings, 800 + (amp * 4500));
        sig = sig + (CombC.ar(sig, 0.04, 0.03, 1) * 0.15);

        // ── Sustain body driven by channel/poly aftertouch ──────────
        // Quiet noise-excited CombL waveguide tuned to `freqSm`, scaled
        // by smoothed aftertouch. At `press = 0` `sustainEx` is exactly
        // zero, so this branch contributes nothing and the original
        // hammer-pluck timbre is unchanged. As pressure rises the
        // sustained body fades in and keeps the note speaking past the
        // ~8 s natural Pluck decay.
        pressSm   = press.lag(0.08);
        sustainEx = WhiteNoise.ar(1) * 0.02 * pressSm;
        sustainEx = LPF.ar(sustainEx, 800 + (pressSm * 4000));
        sustain   = CombL.ar(sustainEx, 0.05, freqSm.reciprocal, 4);
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
                \freq, freq,
                \amp,  vel.linexp(1, 127, 0.04, 0.5),
                \bend, bendSemi
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

});
