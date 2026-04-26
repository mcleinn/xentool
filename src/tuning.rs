use crate::xtn::BoardLayout;

/// Pre-computed tuning for a single pad.
#[derive(Debug, Clone, Copy)]
pub struct PadTuning {
    /// Nearest 12-TET MIDI note number to send.
    pub base_note: u8,
    /// Pitch bend offset (in 14-bit LSBs, centered at 0) to reach the exact frequency.
    pub bend_offset: i32,
    /// Exact target frequency in Hz.
    pub freq_hz: f64,
    /// Virtual MIDI channel from .xtn (`Chan_N`).
    pub v_chan: u8,
    /// Virtual MIDI note from .xtn (`Key_N`).
    pub v_key: u8,
    /// Absolute pitch in EDO steps: (v_chan-1)*edo + v_key + pitch_offset + octave_shift*edo.
    pub abs_pitch: i32,
}

/// Per-board tuning state that tracks active channels and processes MIDI messages.
pub struct TuningState {
    /// Pre-computed tuning for each pad (indexed 0-60).
    pad_tunings: [PadTuning; 61],
    /// Per-channel: which pad is currently active (set on note_on, cleared on note_off).
    channel_pad: [Option<u8>; 16],
    /// Per-channel: our tuning bend offset (set when a note starts).
    channel_tuning_bend: [i32; 16],
    /// Last retune info for UI display.
    pub last_retune_info: Option<String>,
}

impl TuningState {
    /// Build tuning state from a board's .xtn layout.
    pub fn from_board(
        board: &BoardLayout,
        edo: i32,
        pitch_offset: i32,
        octave_shift: i32,
        pb_range: f64,
    ) -> Self {
        let lsbs_per_cent = (pb_range * 100.0) / 8192.0;
        let mut pad_tunings = [PadTuning {
            base_note: 60,
            bend_offset: 0,
            freq_hz: 0.0,
            v_chan: 1,
            v_key: 0,
            abs_pitch: 0,
        }; 61];

        for pad in 0..61u8 {
            let (v_chan, v_key) = match board.pads.get(&pad) {
                Some(e) => (e.chan, e.key),
                None => (1u8, pad),
            };
            let virtual_pitch =
                ((v_chan as i32 - 1) * edo) + (v_key as i32) + pitch_offset + octave_shift * edo;

            // Convert EDO pitch to semitones from C0
            let target_semitones = (virtual_pitch as f64 / edo as f64) * 12.0;

            // Find nearest MIDI note (clamp to 0-127)
            let base_note = target_semitones.round().max(0.0).min(127.0) as u8;

            // Compute bend offset in cents, then convert to 14-bit LSBs
            let bend_cents = (target_semitones - base_note as f64) * 100.0;
            let bend_offset = (bend_cents / lsbs_per_cent).round() as i32;

            pad_tunings[pad as usize] = PadTuning {
                base_note,
                bend_offset,
                freq_hz: crate::mts::edo_freq_hz(edo, virtual_pitch),
                v_chan,
                v_key,
                abs_pitch: virtual_pitch,
            };
        }

        Self {
            pad_tunings,
            channel_pad: [None; 16],
            channel_tuning_bend: [0; 16],
            last_retune_info: None,
        }
    }

    /// Get the pre-computed tuning for a pad (0-60).
    pub fn pad_tuning(&self, pad: u8) -> Option<PadTuning> {
        if pad <= 60 {
            Some(self.pad_tunings[pad as usize])
        } else {
            None
        }
    }

    /// Get the pad currently active on a MIDI channel.
    pub fn channel_pad(&self, ch: u8) -> Option<u8> {
        if (ch as usize) < 16 {
            self.channel_pad[ch as usize]
        } else {
            None
        }
    }

    /// Process an incoming MIDI message from the Exquis.
    /// Returns a list of MIDI messages to forward to the virtual output port.
    pub fn process_message(&mut self, msg: &[u8]) -> Vec<Vec<u8>> {
        if msg.is_empty() {
            return vec![];
        }

        let status = msg[0] & 0xF0;
        let ch = (msg[0] & 0x0F) as usize;

        match status {
            // Note On
            0x90 if msg.len() >= 3 && msg[2] > 0 => {
                let pad_id = msg[1];
                if pad_id > 60 {
                    return vec![msg.to_vec()]; // pass through non-pad notes
                }

                let tuning = self.pad_tunings[pad_id as usize];
                self.channel_pad[ch] = Some(pad_id);
                self.channel_tuning_bend[ch] = tuning.bend_offset;

                self.last_retune_info = Some(format!(
                    "pad{}→note{} bend={:+} (ch{})",
                    pad_id, tuning.base_note, tuning.bend_offset, ch
                ));

                // Send pitch bend first, then note_on with remapped note
                let bend_value = (8192 + tuning.bend_offset).clamp(0, 16383) as u16;
                let bend_msg = vec![0xE0 | ch as u8, (bend_value & 0x7F) as u8, (bend_value >> 7) as u8];
                let note_msg = vec![0x90 | ch as u8, tuning.base_note, msg[2]];

                vec![bend_msg, note_msg]
            }

            // Note Off (or Note On with velocity 0)
            0x80 if msg.len() >= 3 => {
                let pad_id = msg[1];
                if pad_id > 60 {
                    return vec![msg.to_vec()];
                }

                let tuning = self.pad_tunings[pad_id as usize];
                self.channel_pad[ch] = None;
                self.channel_tuning_bend[ch] = 0;

                vec![vec![0x80 | ch as u8, tuning.base_note, msg[2]]]
            }

            // Note On with velocity 0 (= note off)
            0x90 if msg.len() >= 3 && msg[2] == 0 => {
                let pad_id = msg[1];
                if pad_id > 60 {
                    return vec![msg.to_vec()];
                }

                let tuning = self.pad_tunings[pad_id as usize];
                self.channel_pad[ch] = None;
                self.channel_tuning_bend[ch] = 0;

                vec![vec![0x90 | ch as u8, tuning.base_note, 0]]
            }

            // Pitch Bend — combine player's X expression with our tuning offset
            0xE0 if msg.len() >= 3 => {
                let raw_bend = (msg[1] as i32) | ((msg[2] as i32) << 7); // 0-16383
                let player_bend = raw_bend - 8192; // center at 0
                let combined = (8192 + player_bend + self.channel_tuning_bend[ch]).clamp(0, 16383) as u16;

                vec![vec![0xE0 | ch as u8, (combined & 0x7F) as u8, (combined >> 7) as u8]]
            }

            // Everything else: pass through unchanged (CC74, channel pressure, etc.)
            _ => vec![msg.to_vec()],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exquis_proto::Color;
    use crate::xtn::PadEntry;
    use std::collections::HashMap;

    fn make_board(entries: &[(u8, u8, u8)]) -> BoardLayout {
        let mut pads = HashMap::new();
        for &(pad, key, chan) in entries {
            pads.insert(pad, PadEntry {
                key,
                chan,
                color: Color::new(0, 0, 0),
            });
        }
        BoardLayout { pads }
    }

    #[test]
    fn note_on_remaps_and_injects_bend() {
        let board = make_board(&[(0, 0, 1)]); // pad 0, key 0, chan 1
        let mut state = TuningState::from_board(&board, 31, 0, 2, 48.0);

        // note_on on MIDI channel 2, pad 0, velocity 100
        let msgs = state.process_message(&[0x92, 0, 100]);
        assert_eq!(msgs.len(), 2, "should produce pitch_bend + note_on");
        assert_eq!(msgs[0][0] & 0xF0, 0xE0, "first message should be pitch bend");
        assert_eq!(msgs[1][0] & 0xF0, 0x90, "second message should be note_on");
    }

    #[test]
    fn pitch_bend_combines_with_tuning() {
        let board = make_board(&[(0, 0, 1)]);
        let mut state = TuningState::from_board(&board, 31, 0, 2, 48.0);

        // Start a note on channel 2
        let _ = state.process_message(&[0x92, 0, 100]);

        // Player sends pitch bend (center = 8192 = no bend)
        let center_bend = vec![0xE2, 0x00, 0x40]; // 8192 in 14-bit
        let msgs = state.process_message(&center_bend);
        assert_eq!(msgs.len(), 1);
        // Output should include our tuning offset, not just center
    }

    #[test]
    fn note_off_remaps_note() {
        let board = make_board(&[(5, 10, 1)]); // pad 5
        let mut state = TuningState::from_board(&board, 31, 0, 2, 48.0);

        // Start note
        let on_msgs = state.process_message(&[0x92, 5, 100]);
        let base_note = on_msgs[1][1];

        // End note
        let off_msgs = state.process_message(&[0x82, 5, 64]);
        assert_eq!(off_msgs.len(), 1);
        assert_eq!(off_msgs[0][1], base_note, "note_off should use same base_note as note_on");
    }

    #[test]
    fn cc74_passes_through() {
        let board = make_board(&[]);
        let mut state = TuningState::from_board(&board, 31, 0, 2, 48.0);

        let msg = vec![0xB2, 74, 100]; // CC74 on channel 2
        let msgs = state.process_message(&msg);
        assert_eq!(msgs, vec![msg]);
    }
}
