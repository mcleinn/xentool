use anyhow::{Context, Result, bail};
use midir::{Ignore, MidiInput, MidiInputConnection, MidiOutput};
use std::fmt;
use std::str::FromStr;
use std::sync::mpsc::Sender;

use crate::exquis::mpe::InputMessage;
#[cfg(not(windows))]
use crate::exquis::usb::list_exquis_usb_devices;
use crate::exquis::usb::UsbDeviceInfo;
#[cfg(windows)]
use crate::exquis::winmm_drv::{MidiDirection, parse_usb_serial, query_device_interface};

/// Intuitive Instruments / Exquis USB IDs. Used to filter winmm ports
/// to actual hardware Exquis on Windows (see `resolve_usb_info`).
#[cfg(windows)]
const EXQUIS_VID: u16 = 0x2985;
#[cfg(windows)]
const EXQUIS_PID: u16 = 0x0007;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeviceSelection {
    All,
    One(usize),
}

impl fmt::Display for DeviceSelection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::All => write!(f, "all"),
            Self::One(value) => write!(f, "{value}"),
        }
    }
}

impl FromStr for DeviceSelection {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self> {
        if s.eq_ignore_ascii_case("all") {
            Ok(Self::All)
        } else {
            Ok(Self::One(s.parse()?))
        }
    }
}

#[derive(Debug, Clone)]
pub struct ExquisDevice {
    pub number: usize,
    pub label: String,
    pub input_name: Option<String>,
    pub output_name: Option<String>,
    pub usb_info: Option<UsbDeviceInfo>,
}

pub fn list_devices() -> Result<Vec<ExquisDevice>> {
    let mut input = MidiInput::new("xentool-list-input")?;
    input.ignore(Ignore::None);
    let output = MidiOutput::new("xentool-list-output")?;

    // Track each Exquis-named port together with its absolute winmm
    // device id (the index into midir's full ports() list). On Windows
    // we need that id to query DRV_QUERYDEVICEINTERFACE for the real USB
    // serial. On Linux it's harmless.
    let mut input_ports: Vec<(usize, String)> = Vec::new();
    for (idx, port) in input.ports().iter().enumerate() {
        let name = input.port_name(port)?;
        if is_exquis_port(&name) {
            input_ports.push((idx, name));
        }
    }

    let mut output_ports: Vec<(usize, String)> = Vec::new();
    for (idx, port) in output.ports().iter().enumerate() {
        let name = output.port_name(port)?;
        if is_exquis_port(&name) {
            output_ports.push((idx, name));
        }
    }

    // Pair inputs and outputs by position. Multiple devices with the same
    // name (e.g. four "Exquis") are kept as separate entries.
    let count = input_ports.len().max(output_ports.len());
    #[cfg(not(windows))]
    let usb_devices = list_exquis_usb_devices().unwrap_or_default();
    let mut devices = Vec::new();
    for i in 0..count {
        let input_pair = input_ports.get(i).cloned();
        let output_pair = output_ports.get(i).cloned();
        let label = input_pair
            .as_ref()
            .map(|(_, n)| n.clone())
            .or_else(|| output_pair.as_ref().map(|(_, n)| n.clone()))
            .unwrap_or_else(|| "Exquis".to_string());
        let display_label = if count > 1 {
            format!("{} ({})", label, i + 1)
        } else {
            label.clone()
        };

        #[cfg(windows)]
        let usb_info = resolve_usb_info_windows(
            input_pair.as_ref().map(|(idx, _)| *idx),
            output_pair.as_ref().map(|(idx, _)| *idx),
        );
        #[cfg(not(windows))]
        let usb_info = match_usb_info(&usb_devices, &label, i);

        devices.push(ExquisDevice {
            number: i + 1,
            label: display_label,
            input_name: input_pair.map(|(_, n)| n),
            output_name: output_pair.map(|(_, n)| n),
            usb_info,
        });
    }

    Ok(devices)
}

/// Windows: resolve the real USB serial of the device backing a midir
/// port via DRV_QUERYDEVICEINTERFACE. Returns `None` if the port is not
/// a USB-attached Exquis (e.g. a phantom MIDISRV entry from a
/// previously-connected device whose USB parent is gone).
#[cfg(windows)]
fn resolve_usb_info_windows(
    input_winmm_id: Option<usize>,
    output_winmm_id: Option<usize>,
) -> Option<UsbDeviceInfo> {
    // Try the input side first (it's what xentool listens on); fall back
    // to the output if the input query fails.
    let path = input_winmm_id
        .and_then(|id| query_device_interface(MidiDirection::Input, id as u32).ok())
        .or_else(|| {
            output_winmm_id
                .and_then(|id| query_device_interface(MidiDirection::Output, id as u32).ok())
        })?;

    let (vid, pid, serial) = parse_usb_serial(&path)?;
    if vid != EXQUIS_VID || pid != EXQUIS_PID {
        return None;
    }

    Some(UsbDeviceInfo {
        product_name: Some("Exquis".to_string()),
        manufacturer: Some("Intuitive Instruments".to_string()),
        serial_number: Some(serial.clone()),
        vendor_id: vid,
        product_id: pid,
        bus_number: 0,
        address: 0,
        port_numbers: Vec::new(),
        location: path,
        unique_id: serial,
        firmware_version: None,
    })
}

pub fn select_devices(
    devices: &[ExquisDevice],
    selection: &DeviceSelection,
) -> Result<Vec<ExquisDevice>> {
    match selection {
        DeviceSelection::All => Ok(devices.to_vec()),
        DeviceSelection::One(number) => {
            let device = devices
                .iter()
                .find(|device| device.number == *number)
                .cloned()
                .with_context(|| format!("no Exquis device #{number}"))?;
            Ok(vec![device])
        }
    }
}

pub fn send_to_outputs(
    devices: &[ExquisDevice],
    selection: DeviceSelection,
    bytes: &[u8],
) -> Result<()> {
    let selected = select_devices(devices, &selection)?;
    for device in selected {
        let output = MidiOutput::new("xentool-output")?;
        let Some(target_name) = device.output_name.as_deref() else {
            bail!("device #{} has no output port", device.number);
        };
        // Find the Nth matching output port (0-indexed: device.number - 1)
        let port = find_nth_port_by_name(
            &output.ports(),
            |p| output.port_name(p).ok(),
            target_name,
            device.number - 1,
        )
        .with_context(|| {
            format!(
                "failed to find output port `{target_name}` (device #{})",
                device.number
            )
        })?;

        let mut connection = output
            .connect(&port, "xentool-send")
            .with_context(|| format!("failed to open output `{target_name}`"))?;
        connection.send(bytes)?;
    }
    Ok(())
}

pub fn open_inputs(
    devices: &[ExquisDevice],
    tx: Sender<InputMessage>,
) -> Result<Vec<MidiInputConnection<()>>> {
    let mut connections = Vec::new();
    for device in devices {
        let mut input = MidiInput::new("xentool-input")?;
        input.ignore(Ignore::None);
        let Some(target_name) = device.input_name.as_deref() else {
            continue;
        };
        // Find the Nth matching input port (0-indexed: device.number - 1)
        let port = find_nth_port_by_name(
            &input.ports(),
            |p| input.port_name(p).ok(),
            target_name,
            device.number - 1,
        )
        .with_context(|| {
            format!(
                "failed to find input port `{target_name}` (device #{})",
                device.number
            )
        })?;
        let tx = tx.clone();
        let device_number = device.number;
        let port_name = target_name.to_string();
        let connection = input.connect(
            &port,
            &format!("xentool-input-{}", device.number),
            move |timestamp, bytes, _| {
                let _ = tx.send(InputMessage {
                    _timestamp: timestamp,
                    device_number,
                    port_name: port_name.clone(),
                    bytes: bytes.to_vec(),
                });
            },
            (),
        )?;
        connections.push(connection);
    }

    Ok(connections)
}

fn is_exquis_port(name: &str) -> bool {
    name.to_ascii_lowercase().contains("exquis")
}

/// Find the Nth port whose name matches `target_name` (handling duplicate port names).
fn find_nth_port_by_name<T: Clone>(
    ports: &[T],
    get_name: impl Fn(&T) -> Option<String>,
    target_name: &str,
    nth: usize,
) -> Option<T> {
    ports
        .iter()
        .filter(|p| get_name(p).as_deref() == Some(target_name))
        .nth(nth)
        .cloned()
}

#[cfg(not(windows))]
fn match_usb_info(
    usb_devices: &[UsbDeviceInfo],
    label: &str,
    index: usize,
) -> Option<UsbDeviceInfo> {
    let mut matches = usb_devices
        .iter()
        .filter(|device| device.matches_label(label))
        .cloned()
        .collect::<Vec<_>>();
    matches.sort_by(|left, right| left.unique_id.cmp(&right.unique_id));
    matches.get(index).cloned().or_else(|| {
        if usb_devices.len() == 1 {
            usb_devices.first().cloned()
        } else {
            None
        }
    })
}
