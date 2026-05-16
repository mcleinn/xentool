//! Cross-platform MIDI output port opener.
//!
//! On Linux, we use the `alsa` crate directly to create a sequencer client
//! with PORT_TYPE_HARDWARE so that JACK's `-X seq` marks the resulting
//! system:midi_* ports as `JackPortIsPhysical`. MODEP only lists physical
//! MIDI ports, so this is required for the device to appear in the UI.
//!
//! On macOS, `midir`'s `create_virtual` (CoreMIDI) works out of the box.
//!
//! On Windows, WinMM has no virtual-port API, so we connect to an
//! existing port by name (typically "loopMIDI Port" — installed by the
//! user via Tobias Erichsen's loopMIDI).

use anyhow::Result;
use midir::{MidiOutput, MidiOutputConnection};

/// A MIDI output that looks like a hardware device to JACK/MODEP on Linux.
/// On other platforms this is just a thin wrapper around midir.
pub struct HwMidiOut {
    #[cfg(target_os = "linux")]
    seq: alsa::seq::Seq,
    #[cfg(target_os = "linux")]
    port_id: i32,
    /// Separate ALSA client holding only the dummy input port. Using a separate
    /// client ensures JACK's `-X seq` numbers both directions as `_1`, so
    /// MODEP's title matching merges them into "(in+out)".
    #[cfg(target_os = "linux")]
    _input_seq: alsa::seq::Seq,
    #[cfg(not(target_os = "linux"))]
    conn: MidiOutputConnection,
}

impl HwMidiOut {
    pub fn send(&mut self, msg: &[u8]) -> Result<()> {
        #[cfg(target_os = "linux")]
        {
            use alsa::seq::{EvCtrl, EvNote, Event, EventType};

            let mut ev = match msg.len() {
                3 => {
                    let status = msg[0] & 0xF0;
                    let ch = msg[0] & 0x0F;
                    match status {
                        0x90 if msg[2] > 0 => Event::new(
                            EventType::Noteon,
                            &EvNote {
                                channel: ch,
                                note: msg[1],
                                velocity: msg[2],
                                off_velocity: 0,
                                duration: 0,
                            },
                        ),
                        0x80 | 0x90 => Event::new(
                            EventType::Noteoff,
                            &EvNote {
                                channel: ch,
                                note: msg[1],
                                velocity: msg[2],
                                off_velocity: 0,
                                duration: 0,
                            },
                        ),
                        0xB0 => Event::new(
                            EventType::Controller,
                            &EvCtrl {
                                channel: ch,
                                param: msg[1] as u32,
                                value: msg[2] as i32,
                            },
                        ),
                        0xE0 => {
                            let value = ((msg[2] as i32) << 7 | (msg[1] as i32)) - 8192;
                            Event::new(
                                EventType::Pitchbend,
                                &EvCtrl {
                                    channel: ch,
                                    param: 0,
                                    value,
                                },
                            )
                        }
                        0xD0 => Event::new(
                            EventType::Chanpress,
                            &EvCtrl {
                                channel: ch,
                                param: 0,
                                value: msg[1] as i32,
                            },
                        ),
                        _ => return Ok(()),
                    }
                }
                2 => {
                    let status = msg[0] & 0xF0;
                    let ch = msg[0] & 0x0F;
                    match status {
                        0xC0 => Event::new(
                            EventType::Pgmchange,
                            &EvCtrl {
                                channel: ch,
                                param: 0,
                                value: msg[1] as i32,
                            },
                        ),
                        0xD0 => Event::new(
                            EventType::Chanpress,
                            &EvCtrl {
                                channel: ch,
                                param: 0,
                                value: msg[1] as i32,
                            },
                        ),
                        _ => return Ok(()),
                    }
                }
                _ => return Ok(()),
            };

            ev.set_source(self.port_id);
            ev.set_subs();
            ev.set_direct();
            self.seq.event_output_direct(&mut ev)?;
            Ok(())
        }
        #[cfg(not(target_os = "linux"))]
        {
            self.conn
                .send(msg)
                .map_err(|e| anyhow::anyhow!("MIDI send error: {e}"))
        }
    }
}

/// Opens a bidirectional MIDI port that appears as a physical device in
/// JACK/MODEP on Linux. Returns the output handle.
///
/// Uses two separate ALSA seq clients (same name) — one for output, one for
/// input. JACK's `-X seq` numbers ports per-client, so both get suffix `_1`.
/// MODEP matches ports by title: both produce "xentool wooting MIDI 1" and
/// get merged into a single "(in+out)" entry.
#[cfg(target_os = "linux")]
pub fn open_output_bidirectional(
    client: &str,
    port_name: &str,
) -> Result<HwMidiOut> {
    use alsa::seq::{PortCap, PortType, Seq};

    let client_cstr = std::ffi::CString::new(client)?;
    let port_cstr = std::ffi::CString::new(port_name)?;

    // Client 1: output port (source) — subscribers receive our MIDI
    let seq = Seq::open(None, None, false)
        .map_err(|e| anyhow::anyhow!("failed to open ALSA seq (output): {e}"))?;
    seq.set_client_name(&client_cstr)?;
    let mut out_info = alsa::seq::PortInfo::empty()?;
    out_info.set_name(&port_cstr);
    out_info.set_capability(PortCap::READ | PortCap::SUBS_READ);
    out_info.set_type(PortType::MIDI_GENERIC | PortType::HARDWARE | PortType::PORT);
    seq.create_port(&out_info)
        .map_err(|e| anyhow::anyhow!("failed to create ALSA output port: {e}"))?;
    let port_id = out_info.get_port();

    // Client 2: input port (sink) — dummy, never read. Separate client so
    // JACK numbers it as midi_capture_1 (matching the output's midi_playback_1).
    let _input_seq = Seq::open(None, None, false)
        .map_err(|e| anyhow::anyhow!("failed to open ALSA seq (input): {e}"))?;
    _input_seq.set_client_name(&client_cstr)?;
    let mut inp_info = alsa::seq::PortInfo::empty()?;
    inp_info.set_name(&port_cstr);
    inp_info.set_capability(PortCap::WRITE | PortCap::SUBS_WRITE);
    inp_info.set_type(PortType::MIDI_GENERIC | PortType::HARDWARE | PortType::PORT);
    _input_seq
        .create_port(&inp_info)
        .map_err(|e| anyhow::anyhow!("failed to create ALSA input port: {e}"))?;

    Ok(HwMidiOut {
        seq,
        port_id,
        _input_seq,
    })
}

#[cfg(not(target_os = "linux"))]
pub fn open_output_bidirectional(
    client: &str,
    port_name: &str,
) -> Result<HwMidiOut> {
    let conn = open_output(client, port_name)?;
    Ok(HwMidiOut { conn })
}

/// Opens a plain virtual output (no hardware flags, no dummy input).
/// Used by the Exquis backend and as fallback.
#[cfg(unix)]
pub fn open_output(client: &str, port_name: &str) -> Result<MidiOutputConnection> {
    use midir::os::unix::VirtualOutput;
    let out = MidiOutput::new(client)?;
    out.create_virtual(port_name)
        .map_err(|e| anyhow::anyhow!("failed to create virtual MIDI port `{port_name}`: {e}"))
}

#[cfg(not(unix))]
pub fn open_output(client: &str, port_name: &str) -> Result<MidiOutputConnection> {
    use anyhow::Context;
    let out = MidiOutput::new(client)?;
    let port = out
        .ports()
        .into_iter()
        .find(|p| out.port_name(p).ok().as_deref() == Some(port_name))
        .with_context(|| {
            format!(
                "MIDI output port `{port_name}` not found. \
                 Install loopMIDI (https://www.tobias-erichsen.de/software/loopmidi.html) \
                 and create a port named \"{port_name}\"."
            )
        })?;
    out.connect(&port, "xentool-out")
        .with_context(|| format!("failed to open MIDI output `{port_name}`"))
}
