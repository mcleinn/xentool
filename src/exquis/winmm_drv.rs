//! Windows-only: read each MIDI port's underlying USB serial directly
//! from the Win32 winmm driver.
//!
//! Background: midir on Windows enumerates MIDI ports through winmm. The
//! port name is `"Exquis"` for every connected board and contains no
//! per-device identifier. Earlier code tried to recover the per-device
//! identity by separately enumerating USB devices via libusb (fails on
//! USB-Audio-Class endpoints, which the Exquis is) or by parsing
//! `Win32_PnPEntity` for matching names — that path returned the Windows
//! MIDI Service synthetic IDs (`MIDIU_KSA_*`), which are not stable
//! across power cycles. Users saw board0..board3 reshuffle between runs.
//!
//! Fix: `midiInMessage` / `midiOutMessage` accept the
//! `DRV_QUERYDEVICEINTERFACE` selector and hand back the winmm port's
//! underlying device interface path, e.g.
//! `\\?\usb#vid_2985&pid_0007#363534493233#{6994ad04-...}\global`. The
//! firmware-provided USB serial is right there in the third segment.
//! That string is stable for the lifetime of the device — exactly what
//! we need for the `devices.json` pin key.

#![cfg(windows)]

use anyhow::{Context, Result, bail};
use std::ffi::OsString;
use std::os::windows::ffi::OsStringExt;

const DRV_QUERYDEVICEINTERFACE: u32 = 0x080C;
const DRV_QUERYDEVICEINTERFACESIZE: u32 = 0x080D;
const CALLBACK_NULL: u32 = 0;

#[link(name = "winmm")]
unsafe extern "system" {
    fn midiInOpen(
        ph_midi_in: *mut usize,
        u_device_id: u32,
        dw_callback: usize,
        dw_instance: usize,
        dw_flags: u32,
    ) -> u32;
    fn midiInClose(h_midi_in: usize) -> u32;
    fn midiInMessage(h_midi_in: usize, u_msg: u32, dw_param1: usize, dw_param2: usize) -> u32;
    fn midiOutOpen(
        ph_midi_out: *mut usize,
        u_device_id: u32,
        dw_callback: usize,
        dw_instance: usize,
        dw_flags: u32,
    ) -> u32;
    fn midiOutClose(h_midi_out: usize) -> u32;
    fn midiOutMessage(h_midi_out: usize, u_msg: u32, dw_param1: usize, dw_param2: usize) -> u32;
}

#[derive(Debug, Clone, Copy)]
pub enum MidiDirection {
    Input,
    Output,
}

/// Query the device interface path for the given winmm device ID.
/// First tries passing the device ID cast as a handle (no port open
/// required, as documented for select queries); on failure falls back
/// to opening the port for the duration of the query and closing it.
pub fn query_device_interface(dir: MidiDirection, device_id: u32) -> Result<String> {
    if let Some(path) = try_query_with_handle(dir, device_id as usize) {
        return Ok(path);
    }

    let mut h: usize = 0;
    let rc = unsafe {
        match dir {
            MidiDirection::Input => midiInOpen(&mut h, device_id, 0, 0, CALLBACK_NULL),
            MidiDirection::Output => midiOutOpen(&mut h, device_id, 0, 0, CALLBACK_NULL),
        }
    };
    if rc != 0 {
        bail!("midi{:?}Open returned {rc} for device id {device_id}", dir);
    }
    let path = try_query_with_handle(dir, h);
    unsafe {
        match dir {
            MidiDirection::Input => {
                midiInClose(h);
            }
            MidiDirection::Output => {
                midiOutClose(h);
            }
        }
    }
    path.with_context(|| {
        format!("DRV_QUERYDEVICEINTERFACE failed after open for device id {device_id}")
    })
}

fn try_query_with_handle(dir: MidiDirection, handle: usize) -> Option<String> {
    let mut size: u32 = 0;
    let rc = unsafe {
        let p_size = (&mut size) as *mut u32 as usize;
        match dir {
            MidiDirection::Input => {
                midiInMessage(handle, DRV_QUERYDEVICEINTERFACESIZE, p_size, 0)
            }
            MidiDirection::Output => {
                midiOutMessage(handle, DRV_QUERYDEVICEINTERFACESIZE, p_size, 0)
            }
        }
    };
    if rc != 0 || size == 0 {
        return None;
    }
    let cb = size as usize;
    // The driver wants enough room for a wide-string with terminator.
    let mut buf: Vec<u16> = vec![0u16; cb / 2];
    let rc = unsafe {
        let p_buf = buf.as_mut_ptr() as usize;
        match dir {
            MidiDirection::Input => midiInMessage(handle, DRV_QUERYDEVICEINTERFACE, p_buf, cb),
            MidiDirection::Output => midiOutMessage(handle, DRV_QUERYDEVICEINTERFACE, p_buf, cb),
        }
    };
    if rc != 0 {
        return None;
    }
    let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    Some(OsString::from_wide(&buf[..len]).to_string_lossy().into_owned())
}

/// Parse a Windows USB device interface path of the form
/// `\\?\usb#vid_xxxx&pid_yyyy#<serial>#{class-guid}\global` into
/// `(vendor_id, product_id, serial_uppercase)`. Returns `None` for
/// non-USB MIDI devices (loopMIDI, the GS Wavetable Synth, etc.) whose
/// interface paths use a different prefix.
pub fn parse_usb_serial(path: &str) -> Option<(u16, u16, String)> {
    let lower = path.to_ascii_lowercase();
    let usb_pos = lower.find("usb#")?;
    let after_usb = &path[usb_pos + 4..];
    let mut parts = after_usb.splitn(3, '#');
    let vidpid = parts.next()?;
    let serial = parts.next()?;
    if serial.is_empty() {
        return None;
    }
    let lower_vidpid = vidpid.to_ascii_lowercase();
    let vid_at = lower_vidpid.find("vid_")? + 4;
    let pid_at = lower_vidpid.find("pid_")? + 4;
    if vidpid.len() < vid_at + 4 || vidpid.len() < pid_at + 4 {
        return None;
    }
    let vid = u16::from_str_radix(&vidpid[vid_at..vid_at + 4], 16).ok()?;
    let pid = u16::from_str_radix(&vidpid[pid_at..pid_at + 4], 16).ok()?;
    Some((vid, pid, serial.to_ascii_uppercase()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_usb_path_extracts_serial() {
        let path = r"\\?\usb#vid_2985&pid_0007#363534493233#{6994ad04-93ef-11d0-a3cc-00a0c9223196}\global";
        let (vid, pid, serial) = parse_usb_serial(path).unwrap();
        assert_eq!(vid, 0x2985);
        assert_eq!(pid, 0x0007);
        assert_eq!(serial, "363534493233");
    }

    #[test]
    fn parse_usb_path_uppercases_hex_serial() {
        let path = r"\\?\usb#vid_2985&pid_0007#3964353b3532#{6994ad04-93ef-11d0-a3cc-00a0c9223196}\global";
        let (_, _, serial) = parse_usb_serial(path).unwrap();
        assert_eq!(serial, "3964353B3532");
    }

    #[test]
    fn parse_rejects_non_usb_paths() {
        // loopMIDI / Microsoft GS Wavetable Synth / etc. use a different
        // interface-path prefix.
        let path = r"\\?\root#media#0000#{6994ad04-93ef-11d0-a3cc-00a0c9223196}\tevmidi0";
        assert!(parse_usb_serial(path).is_none());
    }

    #[test]
    fn parse_rejects_empty_path() {
        assert!(parse_usb_serial("").is_none());
    }
}
