use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::exquis::midi::ExquisDevice;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceConfig {
    #[serde(default)]
    pub devices: HashMap<String, DeviceIdentifier>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceIdentifier {
    pub serial: Option<String>,
    pub usb_location: Option<String>,
}

/// A connected device matched to a board name.
#[derive(Debug, Clone)]
pub struct BoardAssignment {
    pub board_name: String,
    pub device: ExquisDevice,
}

/// Default config path: %LOCALAPPDATA%\xentool\config\devices.json
pub fn default_config_path() -> PathBuf {
    if let Some(dirs) = directories::BaseDirs::new() {
        dirs.data_local_dir()
            .join("xentool")
            .join("config")
            .join("devices.json")
    } else {
        PathBuf::from("devices.json")
    }
}

pub fn load_device_config() -> Result<DeviceConfig> {
    let path = default_config_path();
    if !path.exists() {
        return Ok(DeviceConfig {
            devices: HashMap::new(),
        });
    }
    let content =
        std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    let config: DeviceConfig =
        serde_json::from_str(&content).with_context(|| format!("parsing {}", path.display()))?;
    Ok(config)
}

fn save_device_config(config: &DeviceConfig) -> Result<()> {
    let path = default_config_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    let json = serde_json::to_string_pretty(config)?;
    std::fs::write(&path, json).with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

/// Check if a device matches a config identifier.
fn matches_ident(device: &ExquisDevice, ident: &DeviceIdentifier) -> bool {
    if let Some(usb) = &device.usb_info {
        if let Some(serial) = &ident.serial {
            if usb.serial_number.as_deref() == Some(serial.as_str()) {
                return true;
            }
        }
        if let Some(location) = &ident.usb_location {
            if usb.location == *location {
                return true;
            }
        }
    }
    false
}

/// Extract the board index from a name like "board0" → Some(0).
fn parse_board_index(name: &str) -> Option<usize> {
    name.strip_prefix("board").and_then(|s| s.parse().ok())
}

/// Sync connected devices with the config to produce board0..boardN assignments.
///
/// Algorithm:
/// 1. For each connected device, check if the config knows it and at which position
/// 2. Try to place known devices at their preferred board index
/// 3. Fill remaining slots with unknown devices
/// 4. Result: every connected device gets exactly one board name, no gaps
/// 5. Save updated config
pub fn sync_boards(devices: &[ExquisDevice]) -> Result<Vec<BoardAssignment>> {
    let config = load_device_config()?;
    let total = devices.len();
    if total == 0 {
        return Ok(vec![]);
    }

    // Step 1: Find preferred positions for known devices
    let mut preferred: Vec<(usize, usize)> = Vec::new(); // (board_index, device_index)
    let mut unassigned_device_indices: Vec<usize> = Vec::new();

    for (di, device) in devices.iter().enumerate() {
        let mut found = false;
        for (name, ident) in &config.devices {
            if matches_ident(device, ident) {
                if let Some(board_idx) = parse_board_index(name) {
                    preferred.push((board_idx, di));
                    found = true;
                    break;
                }
            }
        }
        if !found {
            unassigned_device_indices.push(di);
        }
    }

    // Step 2: Sort preferred by board index
    preferred.sort_by_key(|(idx, _)| *idx);

    // Step 3: Place into slots
    let mut slots: Vec<Option<usize>> = vec![None; total];
    let mut placed: Vec<bool> = vec![false; devices.len()];

    for &(pref_idx, di) in &preferred {
        if pref_idx < total && slots[pref_idx].is_none() {
            slots[pref_idx] = Some(di);
            placed[di] = true;
        }
    }

    // Devices that had a preferred position but couldn't be placed (slot taken or out of range)
    for &(_, di) in &preferred {
        if !placed[di] {
            unassigned_device_indices.push(di);
        }
    }

    // Step 4: Fill remaining slots with unassigned devices
    let mut unassigned_iter = unassigned_device_indices.into_iter();
    for slot in &mut slots {
        if slot.is_none() {
            if let Some(di) = unassigned_iter.next() {
                *slot = Some(di);
            }
        }
    }

    // Step 5: Build result
    let mut result = Vec::with_capacity(total);
    let mut new_config_devices = HashMap::new();

    for (i, slot) in slots.iter().enumerate() {
        if let Some(di) = slot {
            let device = &devices[*di];
            let board_name = format!("board{i}");

            // Build config entry from device
            if let Some(usb) = &device.usb_info {
                new_config_devices.insert(
                    board_name.clone(),
                    DeviceIdentifier {
                        serial: usb.serial_number.clone(),
                        usb_location: if usb.serial_number.is_none() {
                            Some(usb.location.clone())
                        } else {
                            None
                        },
                    },
                );
            }

            result.push(BoardAssignment {
                board_name,
                device: device.clone(),
            });
        }
    }

    // Step 6: Save updated config
    let new_config = DeviceConfig {
        devices: new_config_devices,
    };
    if let Err(err) = save_device_config(&new_config) {
        eprintln!("warning: could not save device config: {err}");
    }

    Ok(result)
}

