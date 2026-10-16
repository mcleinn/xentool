// xentool_osc_midi.scd — OSC → MIDIdef bridge for hosts without MIDI ports.
//
// On Android/Termux there is no ALSA sequencer, so `xentool serve` cannot
// publish a virtual MIDI port. Instead it is started with
//     --output osc://127.0.0.1:57120        (the Android default)
// and sends every retuned MIDI message as
//     /xentool/midi  <status> <data1> <data2>     (ints)
//     /xentool/sysex <blob>                        (ignored here)
//
// This file dispatches those messages through MIDIIn's class-side
// `do*Action` hooks, so every MIDIdef / MIDIFunc registered by the patch
// fires exactly as it would from a real MIDI source. It is loaded by
// mpe_tanpura_xentool.scd and midi_piano_xentool.scd on every platform;
// it is inert unless xentool actually sends OSC.
//
// Set ~xentoolOscSrc before loading when the patch filters MIDIdefs by
// `srcID:` (the tanpura patch sets it to ~xentoolUid when a MIDI port was
// found, so OSC events pass the same filter).

~xentoolOscSrc = ~xentoolOscSrc ? 0;

OSCdef(\xentoolOscMidi, { |msg|
    var status = msg[1], d1 = msg[2], d2 = msg[3];
    var src = ~xentoolOscSrc, chan = status bitAnd: 0x0F;
    switch(status bitAnd: 0xF0,
        // NoteOn with velocity 0 stays a NoteOn, matching what ALSA /
        // CoreMIDI deliver; the patches already treat vel 0 as release.
        0x90, { MIDIIn.doNoteOnAction(src, chan, d1, d2) },
        0x80, { MIDIIn.doNoteOffAction(src, chan, d1, d2) },
        0xA0, { MIDIIn.doPolyTouchAction(src, chan, d1, d2) },
        0xB0, { MIDIIn.doControlAction(src, chan, d1, d2) },
        0xC0, { MIDIIn.doProgramAction(src, chan, d1) },
        0xD0, { MIDIIn.doTouchAction(src, chan, d1) },
        0xE0, { MIDIIn.doBendAction(src, chan, (d2 << 7) | d1) }
    );
}, '/xentool/midi');

"[ok] xentool OSC→MIDI bridge listening on udp://0.0.0.0:% (/xentool/midi, src=%)"
    .format(NetAddr.langPort, ~xentoolOscSrc).postln;
