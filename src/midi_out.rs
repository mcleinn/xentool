//! Cross-platform MIDI output port opener.
//!
//! On Unix (ALSA seq / JACK / CoreMIDI) `midir`'s `create_virtual` lets
//! xentool publish its own subscribable port, so other apps see a source
//! named e.g. "Xentool Wooting" or "Xentool Exquis MPE" directly — same
//! pattern as xenwooting's "XenWTN" port. No virtual cable needed.
//!
//! On Windows, WinMM has no virtual-port API, so we connect to an
//! existing port by name (typically "loopMIDI Port" — installed by the
//! user via Tobias Erichsen's loopMIDI).

use anyhow::{Context, Result};
use midir::{MidiOutput, MidiOutputConnection};

#[cfg(unix)]
pub fn open_output(client: &str, port_name: &str) -> Result<MidiOutputConnection> {
    use midir::os::unix::VirtualOutput;
    let out = MidiOutput::new(client)?;
    out.create_virtual(port_name)
        .map_err(|e| anyhow::anyhow!("failed to create virtual MIDI port `{port_name}`: {e}"))
}

#[cfg(not(unix))]
pub fn open_output(client: &str, port_name: &str) -> Result<MidiOutputConnection> {
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
