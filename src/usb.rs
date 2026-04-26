use anyhow::Result;
use rusb::{Context, Device, UsbContext};
use serde::Deserialize;
#[cfg(target_os = "windows")]
use std::process::Command;

#[derive(Debug, Clone)]
pub struct UsbDeviceInfo {
    pub product_name: Option<String>,
    pub manufacturer: Option<String>,
    pub serial_number: Option<String>,
    pub vendor_id: u16,
    pub product_id: u16,
    pub bus_number: u8,
    pub address: u8,
    pub port_numbers: Vec<u8>,
    pub location: String,
    pub unique_id: String,
    pub firmware_version: Option<String>,
}

impl UsbDeviceInfo {
    pub fn matches_label(&self, label: &str) -> bool {
        let normalized = normalize(label);
        self.product_name
            .as_deref()
            .map(normalize)
            .is_some_and(|product| product.contains(&normalized) || normalized.contains(&product))
            || self
                .manufacturer
                .as_deref()
                .map(normalize)
                .is_some_and(|maker| maker.contains("intuitive") || maker.contains("exquis"))
            || normalize(&self.unique_id).contains("exquis")
    }
}

pub fn list_exquis_usb_devices() -> Result<Vec<UsbDeviceInfo>> {
    let mut found = Vec::new();
    if let Ok(context) = Context::new() {
        if let Ok(devices) = context.devices() {
            for device in devices.iter() {
                if let Some(info) = read_usb_device(&context, &device)? {
                    found.push(info);
                }
            }
        }
    }

    #[cfg(target_os = "windows")]
    {
        for info in list_windows_pnp_devices()? {
            if !found
                .iter()
                .any(|existing| existing.unique_id == info.unique_id)
            {
                found.push(info);
            }
        }
    }

    Ok(found)
}

fn read_usb_device<T: UsbContext>(
    context: &T,
    device: &Device<T>,
) -> Result<Option<UsbDeviceInfo>> {
    let descriptor = device.device_descriptor()?;
    let handle = match device.open() {
        Ok(handle) => handle,
        Err(_) => return Ok(None),
    };

    let language = handle
        .read_languages(std::time::Duration::from_millis(100))
        .ok();
    let language = language.as_ref().and_then(|langs| langs.first()).copied();
    let product_name = language.and_then(|lang| {
        handle
            .read_product_string(lang, &descriptor, std::time::Duration::from_millis(100))
            .ok()
    });
    let manufacturer = language.and_then(|lang| {
        handle
            .read_manufacturer_string(lang, &descriptor, std::time::Duration::from_millis(100))
            .ok()
    });
    let serial_number = language.and_then(|lang| {
        handle
            .read_serial_number_string(lang, &descriptor, std::time::Duration::from_millis(100))
            .ok()
    });

    let is_exquis = product_name
        .as_deref()
        .map(normalize)
        .is_some_and(|name| name.contains("exquis"))
        || manufacturer
            .as_deref()
            .map(normalize)
            .is_some_and(|name| name.contains("intuitive") || name.contains("dualo"));

    if !is_exquis {
        return Ok(None);
    }

    let bus_number = device.bus_number();
    let address = device.address();
    let port_numbers = device.port_numbers().unwrap_or_default();
    let location = format_location(bus_number, address, &port_numbers);
    let unique_id = serial_number.clone().unwrap_or_else(|| {
        format!(
            "usb:{:04x}:{:04x}:{}",
            descriptor.vendor_id(),
            descriptor.product_id(),
            location
        )
    });

    let _ = context;

    Ok(Some(UsbDeviceInfo {
        product_name,
        manufacturer,
        serial_number,
        vendor_id: descriptor.vendor_id(),
        product_id: descriptor.product_id(),
        bus_number,
        address,
        port_numbers,
        location,
        unique_id,
        firmware_version: None,
    }))
}

fn format_location(bus_number: u8, address: u8, port_numbers: &[u8]) -> String {
    if port_numbers.is_empty() {
        format!("bus-{bus_number:03}/addr-{address:03}")
    } else {
        let ports = port_numbers
            .iter()
            .map(u8::to_string)
            .collect::<Vec<_>>()
            .join(".");
        format!("bus-{bus_number:03}/ports-{ports}/addr-{address:03}")
    }
}

fn normalize(value: &str) -> String {
    value.trim().to_ascii_lowercase()
}

#[cfg(target_os = "windows")]
fn list_windows_pnp_devices() -> Result<Vec<UsbDeviceInfo>> {
    let script = concat!(
        "$items = Get-CimInstance Win32_PnPEntity | Where-Object { $_.Name -match 'Exquis' -or $_.Manufacturer -match 'Intuitive' };",
        "$items | Select-Object Name,Manufacturer,@{Name='InstanceId';Expression={$_.PNPDeviceID}},HardwareID | ConvertTo-Json -Compress"
    );
    let output = Command::new("powershell")
        .args(["-NoProfile", "-Command", script])
        .output()?;
    if !output.status.success() {
        return Ok(Vec::new());
    }

    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if stdout.is_empty() || stdout == "null" {
        return Ok(Vec::new());
    }

    let devices = if stdout.starts_with('[') {
        serde_json::from_str::<Vec<WindowsPnpDevice>>(&stdout).unwrap_or_default()
    } else {
        serde_json::from_str::<WindowsPnpDevice>(&stdout)
            .map(|device| vec![device])
            .unwrap_or_default()
    };

    Ok(devices
        .into_iter()
        .map(|device| {
            let (vendor_id, product_id) =
                parse_vid_pid(device.hardware_ids.as_deref().unwrap_or(&[]));
            UsbDeviceInfo {
                product_name: device.name.clone(),
                manufacturer: device.manufacturer.clone(),
                serial_number: device
                    .instance_id
                    .as_deref()
                    .and_then(extract_tail_component),
                vendor_id,
                product_id,
                bus_number: 0,
                address: 0,
                port_numbers: Vec::new(),
                location: device
                    .instance_id
                    .clone()
                    .unwrap_or_else(|| "windows-pnp".to_string()),
                unique_id: device.instance_id.clone().unwrap_or_else(|| {
                    format!(
                        "usb:{:04x}:{:04x}:{}",
                        vendor_id,
                        product_id,
                        device.name.unwrap_or_else(|| "exquis".to_string())
                    )
                }),
                firmware_version: None,
            }
        })
        .collect())
}

#[cfg(target_os = "windows")]
#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct WindowsPnpDevice {
    name: Option<String>,
    instance_id: Option<String>,
    manufacturer: Option<String>,
    hardware_ids: Option<Vec<String>>,
}

#[cfg(target_os = "windows")]
fn parse_vid_pid(hardware_ids: &[String]) -> (u16, u16) {
    for hardware_id in hardware_ids {
        let lowered = hardware_id.to_ascii_lowercase();
        if let (Some(vid), Some(pid)) = (
            extract_hex_after(&lowered, "vid_"),
            extract_hex_after(&lowered, "pid_"),
        ) {
            return (vid, pid);
        }
    }
    (0, 0)
}

#[cfg(target_os = "windows")]
fn extract_hex_after(input: &str, needle: &str) -> Option<u16> {
    let start = input.find(needle)? + needle.len();
    let value = input.get(start..start + 4)?;
    u16::from_str_radix(value, 16).ok()
}

#[cfg(target_os = "windows")]
fn extract_tail_component(device_id: &str) -> Option<String> {
    let part = device_id.split('\\').next_back()?.trim();
    if part.is_empty() {
        None
    } else {
        Some(part.to_string())
    }
}
