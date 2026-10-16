//! Android/Termux Exquis transport: raw USB-MIDI class driver over libusb.
//!
//! Termux has no ALSA sequencer and midir has no Android backend, so the
//! Exquis is driven directly as a USB Audio/MIDIStreaming device through
//! `rusb`. Android apps may not open `/dev/bus/usb/*` themselves; the
//! Termux:API app opens the device after the user accepts the permission
//! dialog and hands us an already-open file descriptor via
//! `termux-usb -E -e <cmd> <device>` (exported as `TERMUX_USB_FD`).
//!
//! `bootstrap()` (called first thing in `main`) makes that transparent:
//! when a command needs the Exquis and no fd is present, xentool
//! enumerates `termux-usb -l`, probes each device by re-running itself
//! under `termux-usb` with the hidden `usb-probe` subcommand, and finally
//! re-executes itself under `termux-usb` for the Exquis it found. The
//! original CLI arguments travel through `XENTOOL_REEXEC_ARGS` (JSON)
//! because `termux-callback` only word-splits its command string.
//!
//! Only one Exquis is supported on Android (one fd per `termux-usb`
//! invocation). The wire format is the USB-MIDI 1.0 4-byte event packet
//! (cable/CIN byte + up to 3 MIDI bytes); SysEx is reassembled per cable
//! so callers see whole messages exactly like midir delivers them.

use anyhow::{Context as _, Result, bail};
use rusb::{Context, DeviceHandle, Direction, TransferType, UsbContext};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use crate::exquis::midi::{DeviceSelection, ExquisDevice, select_devices};
use crate::exquis::mpe::InputMessage;
use crate::exquis::usb::UsbDeviceInfo;

pub const ENV_FD: &str = "TERMUX_USB_FD";
pub const ENV_REEXEC_ARGS: &str = "XENTOOL_REEXEC_ARGS";
const EXQUIS_VID: u16 = 0x2985;
const EXQUIS_PID: u16 = 0x0007;
const READ_TIMEOUT: Duration = Duration::from_millis(100);
const WRITE_TIMEOUT: Duration = Duration::from_millis(1000);

// ───────────────────────── device singleton ─────────────────────────

/// One opened Exquis (the fd handed over by termux-usb).
pub struct UsbMidi {
    handle: DeviceHandle<Context>,
    iface: u8,
    ep_in: u8,
    ep_out: u8,
    in_max_packet: usize,
    /// Number of embedded MIDI jacks on the bulk endpoints = number of
    /// "cables" (virtual ports) in each direction.
    out_cables: u8,
    in_cables: u8,
    /// Serialises bulk OUT writes (SysEx snapshots from the UI thread vs.
    /// control-colour pushes from the serve callback).
    write_lock: Mutex<()>,
    pub info: UsbDeviceInfo,
}

impl Drop for UsbMidi {
    fn drop(&mut self) {
        let _ = self.handle.release_interface(self.iface);
    }
}

static DEVICE: OnceLock<Mutex<Option<Arc<UsbMidi>>>> = OnceLock::new();

/// Returns the opened Exquis, opening it on first use. `Ok(None)` when no
/// termux-usb fd is present in the environment (not running under
/// `termux-usb`), `Err` when the fd is there but is not an Exquis or the
/// MIDI interface could not be claimed.
pub fn device() -> Result<Option<Arc<UsbMidi>>> {
    let slot = DEVICE.get_or_init(|| Mutex::new(None));
    let mut guard = slot.lock().unwrap();
    if let Some(dev) = guard.as_ref() {
        return Ok(Some(dev.clone()));
    }
    let Some(fd) = env_fd() else {
        return Ok(None);
    };
    let dev = Arc::new(open_fd(fd)?);
    *guard = Some(dev.clone());
    Ok(Some(dev))
}

fn env_fd() -> Option<i32> {
    std::env::var(ENV_FD).ok()?.trim().parse().ok()
}

fn open_fd(fd: i32) -> Result<UsbMidi> {
    // Android: libusb must not scan /dev/bus/usb (no permission) — we only
    // wrap the fd Termux:API gave us. Must be set before the first context.
    let _ = rusb::disable_device_discovery();
    let ctx = Context::new().context("libusb init")?;
    // SAFETY: fd is an open usbfs descriptor inherited from termux-usb; we
    // never close it ourselves, so it outlives the handle.
    let handle = unsafe { ctx.open_device_with_fd(fd) }
        .with_context(|| format!("libusb_wrap_sys_device(fd {fd})"))?;
    let device = handle.device();
    let desc = device.device_descriptor().context("device descriptor")?;

    let lang = handle
        .read_languages(READ_TIMEOUT)
        .ok()
        .and_then(|l| l.first().copied());
    let product = lang.and_then(|l| handle.read_product_string(l, &desc, READ_TIMEOUT).ok());
    let manufacturer =
        lang.and_then(|l| handle.read_manufacturer_string(l, &desc, READ_TIMEOUT).ok());
    let serial =
        lang.and_then(|l| handle.read_serial_number_string(l, &desc, READ_TIMEOUT).ok());

    let looks_exquis = (desc.vendor_id() == EXQUIS_VID && desc.product_id() == EXQUIS_PID)
        || product
            .as_deref()
            .is_some_and(|p| p.to_ascii_lowercase().contains("exquis"));
    if !looks_exquis {
        bail!(
            "USB device {:04x}:{:04x} ({}) is not an Exquis",
            desc.vendor_id(),
            desc.product_id(),
            product.as_deref().unwrap_or("?")
        );
    }

    // Find the MIDIStreaming interface (audio class 1, subclass 3) and its
    // bulk endpoints. Count cables from the class-specific endpoint
    // descriptor (CS_ENDPOINT / MS_GENERAL: bNumEmbMIDIJack).
    let config = device
        .active_config_descriptor()
        .or_else(|_| device.config_descriptor(0))
        .context("config descriptor")?;
    let mut found: Option<(u8, u8, u8, usize, u8, u8)> = None;
    'outer: for iface in config.interfaces() {
        for alt in iface.descriptors() {
            if alt.class_code() != 1 || alt.sub_class_code() != 3 {
                continue;
            }
            let mut ep_in = None;
            let mut ep_out = None;
            for ep in alt.endpoint_descriptors() {
                if ep.transfer_type() != TransferType::Bulk {
                    continue;
                }
                let cables = cable_count(ep.extra());
                match ep.direction() {
                    Direction::In => {
                        ep_in = Some((ep.address(), ep.max_packet_size() as usize, cables))
                    }
                    Direction::Out => ep_out = Some((ep.address(), cables)),
                }
            }
            if let (Some((i, max, ic)), Some((o, oc))) = (ep_in, ep_out) {
                found = Some((alt.interface_number(), i, o, max, ic, oc));
                break 'outer;
            }
        }
    }
    let Some((iface, ep_in, ep_out, in_max_packet, in_cables, out_cables)) = found else {
        bail!("no USB-MIDI (audio/MIDIStreaming) interface with bulk IN+OUT endpoints found");
    };

    let _ = handle.set_auto_detach_kernel_driver(true);
    handle
        .claim_interface(iface)
        .with_context(|| format!("claim USB-MIDI interface {iface}"))?;

    let serial_or_loc = serial.clone().unwrap_or_else(|| {
        format!(
            "usb:{:04x}:{:04x}:fd{fd}",
            desc.vendor_id(),
            desc.product_id()
        )
    });
    let info = UsbDeviceInfo {
        product_name: product.clone(),
        manufacturer,
        serial_number: serial,
        vendor_id: desc.vendor_id(),
        product_id: desc.product_id(),
        bus_number: device.bus_number(),
        address: device.address(),
        port_numbers: device.port_numbers().unwrap_or_default(),
        location: format!("termux-usb fd {fd}"),
        unique_id: serial_or_loc,
        firmware_version: None,
    };

    Ok(UsbMidi {
        handle,
        iface,
        ep_in,
        ep_out,
        in_max_packet: in_max_packet.max(4),
        out_cables: out_cables.max(1),
        in_cables: in_cables.max(1),
        write_lock: Mutex::new(()),
        info,
    })
}

/// Parse the class-specific MS bulk endpoint descriptor
/// `[bLength, 0x25 (CS_ENDPOINT), 0x01 (MS_GENERAL), bNumEmbMIDIJack, ...]`.
fn cable_count(extra: Option<&[u8]>) -> u8 {
    let mut rest = extra.unwrap_or(&[]);
    while rest.len() >= 4 {
        let len = rest[0] as usize;
        if len < 2 || len > rest.len() {
            break;
        }
        if rest[1] == 0x25 && rest[2] == 0x01 {
            return rest[3].max(1);
        }
        rest = &rest[len..];
    }
    1
}

// ───────────────────────── wire format ─────────────────────────

/// Length (in MIDI bytes) implied by a USB-MIDI Code Index Number, or
/// `None` for reserved/misc CINs that carry no MIDI data.
fn cin_len(cin: u8) -> Option<usize> {
    match cin {
        0x0 | 0x1 => None,
        0x2 | 0x6 | 0xC | 0xD => Some(2),
        0x3 | 0x4 | 0x7 | 0x8 | 0x9 | 0xA | 0xB | 0xE => Some(3),
        0x5 | 0xF => Some(1),
        _ => None,
    }
}

/// Encode one MIDI message into USB-MIDI 4-byte packets for `cable`.
pub fn encode_packets(cable: u8, msg: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity((msg.len() / 3 + 1) * 4);
    let cn = (cable & 0x0F) << 4;
    if msg.is_empty() {
        return out;
    }
    if msg[0] == 0xF0 {
        let mut chunks = msg.chunks(3).peekable();
        while let Some(chunk) = chunks.next() {
            let last = chunks.peek().is_none();
            let cin = if last {
                match chunk.len() {
                    1 => 0x5,
                    2 => 0x6,
                    _ => 0x7,
                }
            } else {
                0x4
            };
            out.push(cn | cin);
            out.push(chunk[0]);
            out.push(chunk.get(1).copied().unwrap_or(0));
            out.push(chunk.get(2).copied().unwrap_or(0));
        }
        return out;
    }
    let status = msg[0];
    let cin = match status {
        0x80..=0xEF => status >> 4,
        0xF1 | 0xF3 => 0x2,
        0xF2 => 0x3,
        0xF6 => 0x5,
        0xF8..=0xFF => 0xF,
        _ => return out,
    };
    out.push(cn | cin);
    out.push(status);
    out.push(msg.get(1).copied().unwrap_or(0));
    out.push(msg.get(2).copied().unwrap_or(0));
    out
}

/// Incremental USB-MIDI packet decoder with per-cable SysEx reassembly.
#[derive(Default)]
pub struct PacketDecoder {
    sysex: [Vec<u8>; 16],
}

impl PacketDecoder {
    /// Feed raw bulk-IN bytes; returns `(cable, message)` pairs.
    pub fn feed(&mut self, data: &[u8]) -> Vec<(u8, Vec<u8>)> {
        let mut out = Vec::new();
        for pkt in data.chunks_exact(4) {
            let cable = pkt[0] >> 4;
            let cin = pkt[0] & 0x0F;
            let Some(len) = cin_len(cin) else { continue };
            let bytes = &pkt[1..1 + len];
            let buf = &mut self.sysex[cable as usize];
            match cin {
                0x4 => buf.extend_from_slice(bytes),
                // SysEx end packets. CIN 5 doubles as "single-byte system
                // common" (e.g. F6 tune request) when no SysEx is pending.
                0x5 | 0x6 | 0x7 if !buf.is_empty() || bytes[0] == 0xF0 || cin != 0x5 => {
                    buf.extend_from_slice(bytes);
                    out.push((cable, std::mem::take(buf)));
                }
                _ => out.push((cable, bytes.to_vec())),
            }
        }
        out
    }
}

// ───────────────────────── I/O ─────────────────────────

impl UsbMidi {
    pub fn send(&self, cable: u8, msg: &[u8]) -> Result<()> {
        let packets = encode_packets(cable, msg);
        if packets.is_empty() {
            return Ok(());
        }
        let _g = self.write_lock.lock().unwrap();
        let mut off = 0;
        while off < packets.len() {
            let n = self
                .handle
                .write_bulk(self.ep_out, &packets[off..], WRITE_TIMEOUT)
                .context("USB-MIDI bulk write")?;
            if n == 0 {
                bail!("USB-MIDI bulk write made no progress");
            }
            off += n;
        }
        Ok(())
    }

    fn port_names(count: u8) -> Vec<String> {
        (0..count).map(|c| format!("Exquis MIDI {}", c + 1)).collect()
    }
}

/// Handle returned by `open_inputs`; dropping it stops the reader thread.
pub struct InputConnection {
    stop: Arc<AtomicBool>,
    join: Option<std::thread::JoinHandle<()>>,
}

impl Drop for InputConnection {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

pub fn list_devices() -> Result<Vec<ExquisDevice>> {
    let Some(dev) = device()? else {
        return Ok(Vec::new());
    };
    let input_names = UsbMidi::port_names(dev.in_cables);
    let output_names = UsbMidi::port_names(dev.out_cables);
    Ok(vec![ExquisDevice {
        number: 1,
        label: dev
            .info
            .product_name
            .clone()
            .unwrap_or_else(|| "Exquis".into()),
        input_name: input_names.first().cloned(),
        output_name: output_names.first().cloned(),
        input_names,
        output_names,
        usb_info: Some(dev.info.clone()),
    }])
}

pub fn send_to_outputs(
    devices: &[ExquisDevice],
    selection: DeviceSelection,
    bytes: &[u8],
) -> Result<()> {
    let selected = select_devices(devices, &selection)?;
    if selected.is_empty() {
        return Ok(());
    }
    let dev = device()?.context("Exquis not opened (not running under termux-usb)")?;
    // Mirror the Linux backend: write to every output port (cable).
    for cable in 0..dev.out_cables {
        dev.send(cable, bytes)?;
    }
    Ok(())
}

pub fn open_inputs(
    devices: &[ExquisDevice],
    tx: Sender<InputMessage>,
) -> Result<Vec<InputConnection>> {
    let Some(first) = devices.first() else {
        return Ok(Vec::new());
    };
    let dev = device()?.context("Exquis not opened (not running under termux-usb)")?;
    let stop = Arc::new(AtomicBool::new(false));
    let stop_t = stop.clone();
    let device_number = first.number;
    let join = std::thread::Builder::new()
        .name("exquis-usbmidi-in".into())
        .spawn(move || {
            let started = std::time::Instant::now();
            let mut buf = vec![0u8; dev.in_max_packet.max(64)];
            let mut decoder = PacketDecoder::default();
            let mut consecutive_errors = 0u32;
            while !stop_t.load(Ordering::Relaxed) {
                match dev.handle.read_bulk(dev.ep_in, &mut buf, READ_TIMEOUT) {
                    Ok(n) => {
                        consecutive_errors = 0;
                        for (cable, bytes) in decoder.feed(&buf[..n]) {
                            let msg = InputMessage {
                                _timestamp: started.elapsed().as_micros() as u64,
                                device_number,
                                port_name: format!("Exquis MIDI {}", cable + 1),
                                bytes,
                            };
                            if tx.send(msg).is_err() {
                                return;
                            }
                        }
                    }
                    Err(rusb::Error::Timeout) => {}
                    Err(rusb::Error::NoDevice) => {
                        eprintln!("xentool: Exquis disconnected");
                        return;
                    }
                    Err(e) => {
                        consecutive_errors += 1;
                        if consecutive_errors > 50 {
                            eprintln!(
                                "xentool: USB-MIDI read failing repeatedly ({e}); giving up"
                            );
                            return;
                        }
                        std::thread::sleep(Duration::from_millis(20));
                    }
                }
            }
        })?;
    Ok(vec![InputConnection {
        stop,
        join: Some(join),
    }])
}

// ───────────────────────── termux-usb bootstrap ─────────────────────────

/// Called before CLI parsing on Android. Returns the argument vector to
/// parse (possibly the original one). May `exec` into `termux-usb` and
/// never return when the command needs the Exquis and no fd is present.
///
/// `needs_device` decides from the raw args whether the Exquis is
/// required (pure tooling commands such as `help`, `edit`, `geometry`
/// and all Wooting-routed commands skip the whole dance).
pub fn bootstrap(needs_device: impl Fn(&[String]) -> bool) -> Vec<String> {
    let argv: Vec<String> = std::env::args().collect();
    // Re-exec'd child: real args come from the env.
    if let Ok(json) = std::env::var(ENV_REEXEC_ARGS) {
        if let Ok(mut args) = serde_json::from_str::<Vec<String>>(&json) {
            args.insert(0, argv[0].clone());
            return args;
        }
    }
    if env_fd().is_some() || !needs_device(&argv[1..]) {
        return argv;
    }
    match reexec_under_termux_usb(&argv) {
        Ok(never) => never,
        Err(e) => {
            eprintln!("xentool: {e:#}");
            eprintln!(
                "xentool: run manually as  termux-usb -r -E -e \"xentool <args>\" <device>  \
                 (see `termux-usb -l`)"
            );
            argv
        }
    }
}

fn self_exe() -> Result<String> {
    let p = std::env::current_exe().context("current_exe")?;
    let s = p.to_string_lossy().into_owned();
    if s.contains(char::is_whitespace) {
        bail!("xentool binary path contains whitespace; termux-callback cannot exec it: {s}");
    }
    Ok(s)
}

fn termux_usb_list() -> Result<Vec<String>> {
    let out = std::process::Command::new("termux-usb")
        .arg("-l")
        .output()
        .context("running termux-usb -l (is the termux-api package + Termux:API app installed?)")?;
    if !out.status.success() {
        bail!(
            "termux-usb -l failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    let paths: Vec<String> = serde_json::from_slice(&out.stdout).with_context(|| {
        format!(
            "unexpected termux-usb -l output: {}",
            String::from_utf8_lossy(&out.stdout)
        )
    })?;
    Ok(paths)
}

/// Run `<self> usb-probe` under termux-usb for `path`; returns the probe's
/// first stdout line (`vid:pid product serial`) or an error.
fn probe_path(exe: &str, path: &str) -> Result<String> {
    let out = std::process::Command::new("termux-usb")
        .args(["-r", "-E", "-e", exe, path])
        .env(ENV_REEXEC_ARGS, r#"["usb-probe"]"#)
        .output()
        .context("termux-usb probe")?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    let line = stdout.lines().next().unwrap_or("").trim().to_string();
    if !out.status.success() || line.is_empty() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        bail!(
            "{path}: {}",
            stderr.trim().lines().last().unwrap_or("probe failed")
        );
    }
    Ok(line)
}

fn reexec_under_termux_usb(argv: &[String]) -> Result<Vec<String>> {
    use std::os::unix::process::CommandExt;
    let exe = self_exe()?;
    let paths = termux_usb_list()?;
    if paths.is_empty() {
        bail!("termux-usb -l lists no USB devices — is the Exquis plugged in (USB-C, OTG)?");
    }
    eprintln!(
        "xentool: probing {} USB device(s) via termux-usb (accept the permission dialog on the phone if asked)",
        paths.len()
    );
    let mut chosen = None;
    for path in &paths {
        match probe_path(&exe, path) {
            Ok(line) if line.to_ascii_lowercase().contains("exquis") => {
                eprintln!("xentool: Exquis at {path} ({line})");
                chosen = Some(path.clone());
                break;
            }
            Ok(line) => eprintln!("xentool: {path}: {line} (not an Exquis)"),
            Err(e) => eprintln!("xentool: {e:#}"),
        }
    }
    let Some(path) = chosen else {
        bail!(
            "no Exquis among {} USB device(s): {}",
            paths.len(),
            paths.join(", ")
        );
    };
    let args_json = serde_json::to_string(&argv[1..])?;
    // Permission was granted by the probe; no `-r` needed now.
    let err = std::process::Command::new("termux-usb")
        .args(["-E", "-e", &exe, &path])
        .env(ENV_REEXEC_ARGS, args_json)
        .exec();
    Err(anyhow::Error::from(err).context("exec termux-usb"))
}

/// Hidden `usb-probe` subcommand body: print `vid:pid product serial`.
pub fn cmd_usb_probe() -> Result<()> {
    let Some(fd) = env_fd() else {
        bail!("usb-probe: no {ENV_FD} in environment (run under termux-usb -E)");
    };
    let _ = rusb::disable_device_discovery();
    let ctx = Context::new()?;
    let handle = unsafe { ctx.open_device_with_fd(fd) }?;
    let desc = handle.device().device_descriptor()?;
    let lang = handle
        .read_languages(READ_TIMEOUT)
        .ok()
        .and_then(|l| l.first().copied());
    let product = lang
        .and_then(|l| handle.read_product_string(l, &desc, READ_TIMEOUT).ok())
        .unwrap_or_default();
    let serial = lang
        .and_then(|l| handle.read_serial_number_string(l, &desc, READ_TIMEOUT).ok())
        .unwrap_or_default();
    let tag = if desc.vendor_id() == EXQUIS_VID && desc.product_id() == EXQUIS_PID {
        " [Exquis]"
    } else {
        ""
    };
    println!(
        "{:04x}:{:04x} {product} {serial}{tag}",
        desc.vendor_id(),
        desc.product_id()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_channel_and_sysex() {
        assert_eq!(encode_packets(0, &[0x90, 60, 100]), vec![0x09, 0x90, 60, 100]);
        assert_eq!(encode_packets(1, &[0xC0, 5]), vec![0x1C, 0xC0, 5, 0]);
        assert_eq!(
            encode_packets(0, &[0xF0, 1, 2, 3, 4, 0xF7]),
            vec![0x04, 0xF0, 1, 2, 0x07, 3, 4, 0xF7]
        );
        assert_eq!(encode_packets(0, &[0xF0, 1, 0xF7]), vec![0x07, 0xF0, 1, 0xF7]);
        assert_eq!(
            encode_packets(0, &[0xF0, 1, 2, 3, 0xF7]),
            vec![0x04, 0xF0, 1, 2, 0x06, 3, 0xF7, 0]
        );
    }

    #[test]
    fn decodes_and_reassembles() {
        let mut d = PacketDecoder::default();
        let msgs = d.feed(&[0x09, 0x90, 60, 100, 0x04, 0xF0, 1, 2]);
        assert_eq!(msgs, vec![(0, vec![0x90, 60, 100])]);
        let msgs = d.feed(&[0x05, 0xF7, 0, 0, 0x1E, 0xE0, 0, 64]);
        assert_eq!(
            msgs,
            vec![(0, vec![0xF0, 1, 2, 0xF7]), (1, vec![0xE0, 0, 64])]
        );
        // single-byte system common on cable 0 while no sysex pending
        assert_eq!(d.feed(&[0x05, 0xF6, 0, 0]), vec![(0, vec![0xF6])]);
    }

    #[test]
    fn cable_count_from_cs_endpoint() {
        assert_eq!(cable_count(Some(&[0x05, 0x25, 0x01, 0x01, 0x01])), 1);
        assert_eq!(cable_count(Some(&[0x06, 0x25, 0x01, 0x02, 0x01, 0x03])), 2);
        assert_eq!(cable_count(None), 1);
    }
}
