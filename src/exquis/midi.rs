use anyhow::{Context, Result, bail};
use midir::{Ignore, MidiInput, MidiInputConnection, MidiOutput};
#[cfg(not(windows))]
use std::collections::BTreeMap;
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
    pub input_names: Vec<String>,
    pub output_names: Vec<String>,
    pub usb_info: Option<UsbDeviceInfo>,
}

pub fn list_devices() -> Result<Vec<ExquisDevice>> {
    let mut input = MidiInput::new("xentool-list-input")?;
    input.ignore(Ignore::None);
    let output = MidiOutput::new("xentool-list-output")?;

    #[cfg(not(windows))]
    {
        let usb_devices = list_exquis_usb_devices().unwrap_or_default();
        let mut groups: BTreeMap<u32, LinuxExquisPorts> = BTreeMap::new();

        for port in input.ports() {
            let name = input.port_name(&port)?;
            let Some((client_id, client_name)) = parse_linux_exquis_port(&name) else {
                continue;
            };
            let group = groups.entry(client_id).or_insert_with(|| LinuxExquisPorts {
                client_name,
                input_names: Vec::new(),
                output_names: Vec::new(),
            });
            group.input_names.push(name);
        }

        for port in output.ports() {
            let name = output.port_name(&port)?;
            let Some((client_id, client_name)) = parse_linux_exquis_port(&name) else {
                continue;
            };
            let group = groups.entry(client_id).or_insert_with(|| LinuxExquisPorts {
                client_name,
                input_names: Vec::new(),
                output_names: Vec::new(),
            });
            group.output_names.push(name);
        }

        let mut devices = Vec::new();
        let count = groups.len();
        for (i, (_, mut group)) in groups.into_iter().enumerate() {
            group.input_names.sort();
            group.output_names.sort();
            let usb_info = match_usb_info(&usb_devices, &group.client_name, i);
            let display_label = if count > 1 {
                format!("{} ({})", group.client_name, i + 1)
            } else {
                group.client_name.clone()
            };
            devices.push(ExquisDevice {
                number: i + 1,
                label: display_label,
                input_name: group.input_names.first().cloned(),
                output_name: group.output_names.first().cloned(),
                input_names: group.input_names,
                output_names: group.output_names,
                usb_info,
            });
        }

        return Ok(devices);
    }

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
            input_names: input_pair.as_ref().map(|(_, n)| vec![n.clone()]).unwrap_or_default(),
            output_names: output_pair.as_ref().map(|(_, n)| vec![n.clone()]).unwrap_or_default(),
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
        let target_names: Vec<&str> = if device.output_names.is_empty() {
            device.output_name.iter().map(String::as_str).collect()
        } else {
            device.output_names.iter().map(String::as_str).collect()
        };
        if target_names.is_empty() {
            bail!("device #{} has no output port", device.number);
        }
        for target_name in target_names {
            let output = MidiOutput::new("xentool-output")?;
            #[cfg(windows)]
            let nth = device.number - 1;
            #[cfg(not(windows))]
            let nth = 0;
            let port = find_nth_port_by_name(
                &output.ports(),
                |p| output.port_name(p).ok(),
                target_name,
                nth,
            )
            .with_context(|| {
                format!(
                    "failed to find output port `{target_name}` (device #{})",
                    device.number
                )
            })?;

            let mut connection = output.connect(&port, "xentool-send").map_err(|e| {
                anyhow::anyhow!("failed to open output `{target_name}`: {e}")
            })?;
            connection.send(bytes)?;
        }
    }
    Ok(())
}

pub fn open_inputs(
    devices: &[ExquisDevice],
    tx: Sender<InputMessage>,
) -> Result<Vec<MidiInputConnection<()>>> {
    let mut connections = Vec::new();
    for device in devices {
        let target_names: Vec<&str> = if device.input_names.is_empty() {
            device.input_name.iter().map(String::as_str).collect()
        } else {
            device.input_names.iter().map(String::as_str).collect()
        };
        if target_names.is_empty() {
            continue;
        }
        for (port_idx, target_name) in target_names.into_iter().enumerate() {
            let mut input = MidiInput::new("xentool-input")?;
            input.ignore(Ignore::None);
            #[cfg(windows)]
            let nth = device.number - 1;
            #[cfg(not(windows))]
            let nth = 0;
            let port = find_nth_port_by_name(
                &input.ports(),
                |p| input.port_name(p).ok(),
                target_name,
                nth,
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
                &format!("xentool-input-{}-{}", device.number, port_idx + 1),
                move |timestamp, bytes, _| {
                    let _ = tx.send(InputMessage {
                        _timestamp: timestamp,
                        device_number,
                        port_name: port_name.clone(),
                        bytes: bytes.to_vec(),
                    });
                },
                (),
            )
            .map_err(|e| anyhow::anyhow!(
                "failed to open input `{target_name}` for device #{}: {e}",
                device.number
            ))?;
            connections.push(connection);
        }
    }

    Ok(connections)
}

#[cfg(not(windows))]
#[derive(Debug)]
struct LinuxExquisPorts {
    client_name: String,
    input_names: Vec<String>,
    output_names: Vec<String>,
}

#[cfg(not(windows))]
fn parse_linux_exquis_port(name: &str) -> Option<(u32, String)> {
    let (client_name, rest) = name.split_once(':')?;
    if !client_name.eq_ignore_ascii_case("Exquis") {
        return None;
    }
    let (port_label, client_port) = rest.rsplit_once(' ')?;
    if !port_label.trim_start().starts_with("Exquis MIDI ") {
        return None;
    }
    let (client_id, _port_id) = client_port.split_once(':')?;
    let client_id = client_id.parse().ok()?;
    Some((client_id, client_name.to_string()))
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
    let matches: Vec<T> = ports
        .iter()
        .filter(|p| get_name(p).as_deref() == Some(target_name))
        .cloned()
        .collect();
    matches
        .get(nth)
        .cloned()
        .or_else(|| matches.first().cloned())
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
