use std::collections::{HashMap, HashSet};
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
    let mut config: DeviceConfig =
        serde_json::from_str(&content).with_context(|| format!("parsing {}", path.display()))?;
    // Older xentool builds on Windows persisted Windows-MIDI-Service
    // synthetic IDs (`MIDIU_KSA_*`) as if they were USB serials. They
    // are not stable across power cycles, which was the original cause
    // of board0..boardN reshuffling. The current code reads real USB
    // serials via DRV_QUERYDEVICEINTERFACE; the synthetic-id pins can't
    // match anything we'll see going forward, so we drop them on load.
    // Doing this at load time means the next sync writes a clean file.
    config
        .devices
        .retain(|_, ident| !ident.serial.as_deref().is_some_and(is_legacy_synthetic_id));
    Ok(config)
}

fn is_legacy_synthetic_id(serial: &str) -> bool {
    serial.starts_with("MIDIU_KSA_")
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

/// Find the first board name in `config` whose identifier matches `device`.
/// Returns the board name itself ("board0", "board1", ...).
fn find_existing_board(device: &ExquisDevice, config: &DeviceConfig) -> Option<String> {
    // Sort by name so the lookup is deterministic — `config.devices` is a
    // HashMap with randomised iteration order, which would otherwise pick
    // an arbitrary winner if two entries somehow matched the same device
    // (duplicate serials, location collisions on a flaky USB enumerator).
    let mut entries: Vec<(&String, &DeviceIdentifier)> = config.devices.iter().collect();
    entries.sort_by_key(|(name, _)| name.as_str());
    for (name, ident) in entries {
        if matches_ident(device, ident) {
            return Some(name.clone());
        }
    }
    None
}

fn build_identifier(device: &ExquisDevice) -> Option<DeviceIdentifier> {
    let usb = device.usb_info.as_ref()?;
    Some(DeviceIdentifier {
        serial: usb.serial_number.clone(),
        usb_location: if usb.serial_number.is_none() {
            Some(usb.location.clone())
        } else {
            None
        },
    })
}

/// Compute the live board assignments for this run AND the new config to
/// persist, without doing any IO. Pure function — exposed so the algorithm
/// can be unit-tested without mocking the filesystem.
///
/// Two distinct concerns:
///   - **Live session boards.** A connected, pinned device keeps its
///     *pinned* index as its live `board_name`, even when a lower-indexed
///     board is currently absent. The live vector is therefore intentionally
///     NON-CONTIGUOUS (e.g. board0, board1, board3 when board2's device is
///     unplugged), so `layout.boards.get("board3")` loads the matching
///     `[Board3]` section onto the connected device and the absent board's
///     section loads onto nothing. Invariant: pin name == loaded section.
///   - **Persistent config.** A *durable* device → board name pin.
///     Survives transient disconnects and is never overwritten by a
///     different physical device taking a board's slot during a partial
///     enumeration. Once a device is pinned, the only way its mapping
///     moves is for the user to edit `devices.json` by hand.
///
/// Earlier versions rewrote the persistent config from scratch on every
/// sync, dropping pins for any device that wasn't enumerated this run —
/// so a single transient unplug could permanently scramble the saved
/// board0..boardN order. This implementation preserves all existing pins
/// and only allocates a fresh board name for genuinely new devices.
pub fn compute_board_assignments(
    config: &DeviceConfig,
    devices: &[ExquisDevice],
) -> (Vec<BoardAssignment>, DeviceConfig) {
    let total = devices.len();
    if total == 0 {
        return (vec![], config.clone());
    }

    // --- Phase A: resolve each connected device to a live board index ---
    // A pinned, present device keeps its pinned index verbatim (no range
    // restriction) so the live vector can be non-contiguous. Unpinned
    // devices — and, defensively, any second device that resolves to an
    // already-taken pin — get the lowest index that is neither taken this
    // run nor reserved by a config pin. Reserving config pins here is what
    // stops a brand-new device from stealing an absent device's slot, and
    // it mirrors Phase C exactly so the live and persisted views agree.
    let mut taken: HashSet<usize> = HashSet::new();
    let mut assignment: Vec<(usize, usize)> = Vec::with_capacity(total); // (board_idx, device_idx)
    let mut needs_fresh: Vec<usize> = Vec::new(); // device indices needing a fresh index

    // First pass: honour valid, non-colliding pins at their pinned index.
    for (di, device) in devices.iter().enumerate() {
        match find_existing_board(device, config).and_then(|n| parse_board_index(&n)) {
            Some(idx) if !taken.contains(&idx) => {
                taken.insert(idx);
                assignment.push((idx, di));
            }
            // Unpinned, or a pin that collides with another connected
            // device this run — fall through to fresh allocation below.
            _ => needs_fresh.push(di),
        }
    }

    // Second pass: lowest index not taken this run and not reserved by a
    // config pin. Enumeration order is preserved, matching Phase C.
    for di in needs_fresh {
        let mut idx = 0usize;
        loop {
            let reserved = config.devices.contains_key(&format!("board{idx}"));
            if !taken.contains(&idx) && !reserved {
                taken.insert(idx);
                assignment.push((idx, di));
                break;
            }
            idx += 1;
        }
    }

    // --- Phase B: build the live BoardAssignment vector ---
    // Order by board index so callers see board0, board1, ... (with gaps),
    // regardless of MIDI enumeration order.
    assignment.sort_by_key(|(board_idx, _)| *board_idx);
    let mut result = Vec::with_capacity(total);
    for (board_idx, di) in assignment {
        result.push(BoardAssignment {
            board_name: format!("board{board_idx}"),
            device: devices[di].clone(),
        });
    }

    // --- Phase C: update the persistent config WITHOUT dropping pins. ---
    // Start from the existing config so absent devices' pins survive.
    // Only insert for currently-connected devices that don't already have
    // a pin; never overwrite an existing pin. Iterating `devices` in
    // enumeration order makes the fresh-index choice identical to Phase A.
    let mut new_config_devices = config.devices.clone();
    for device in devices {
        if find_existing_board(device, config).is_some() {
            continue; // pin already exists — preserve it untouched.
        }
        let Some(ident) = build_identifier(device) else {
            continue;
        };
        // Brand-new device. Allocate the lowest board index that isn't
        // already pinned in the post-update config.
        let mut idx = 0usize;
        loop {
            let candidate = format!("board{idx}");
            if !new_config_devices.contains_key(&candidate) {
                new_config_devices.insert(candidate, ident);
                break;
            }
            idx += 1;
        }
    }

    let new_config = DeviceConfig {
        devices: new_config_devices,
    };
    (result, new_config)
}

/// Sync connected devices with the config and persist the result.
/// Thin IO wrapper around `compute_board_assignments`.
pub fn sync_boards(devices: &[ExquisDevice]) -> Result<Vec<BoardAssignment>> {
    let config = load_device_config()?;
    let (result, new_config) = compute_board_assignments(&config, devices);
    if let Err(err) = save_device_config(&new_config) {
        eprintln!("warning: could not save device config: {err}");
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exquis::usb::UsbDeviceInfo;

    fn make_device(number: usize, serial: &str) -> ExquisDevice {
        ExquisDevice {
            number,
            label: format!("Exquis ({number})"),
            input_name: Some("Exquis".to_string()),
            output_name: Some("Exquis".to_string()),
            input_names: vec!["Exquis".to_string()],
            output_names: vec!["Exquis".to_string()],
            usb_info: Some(UsbDeviceInfo {
                product_name: Some("Exquis".to_string()),
                manufacturer: Some("Intuitive Instruments".to_string()),
                serial_number: Some(serial.to_string()),
                vendor_id: 0x1234,
                product_id: 0x5678,
                bus_number: 0,
                address: 0,
                port_numbers: vec![],
                location: format!("loc-{serial}"),
                unique_id: serial.to_string(),
                firmware_version: None,
            }),
        }
    }

    fn make_config(pins: &[(&str, &str)]) -> DeviceConfig {
        let mut devices = HashMap::new();
        for (board, serial) in pins {
            devices.insert(
                board.to_string(),
                DeviceIdentifier {
                    serial: Some(serial.to_string()),
                    usb_location: None,
                },
            );
        }
        DeviceConfig { devices }
    }

    fn pin(config: &DeviceConfig, board: &str) -> Option<String> {
        config.devices.get(board).and_then(|i| i.serial.clone())
    }

    /// The original bug: a transient unplug rewrites the config from scratch
    /// with only present devices, so the absent device's mapping is lost.
    /// With the fix, B's pin must survive when only A, C, D are present.
    #[test]
    fn absent_device_pin_survives_partial_sync() {
        let config = make_config(&[
            ("board0", "A"),
            ("board1", "B"),
            ("board2", "C"),
            ("board3", "D"),
        ]);
        // B is unplugged. xentool sees A, C, D in arbitrary MIDI order.
        let devices = vec![make_device(1, "A"), make_device(2, "C"), make_device(3, "D")];

        let (assignments, new_config) = compute_board_assignments(&config, &devices);

        // Live session: each present device keeps its PINNED board index,
        // even though board1 (B) is absent. The result is intentionally
        // NON-CONTIGUOUS — board0, board2, board3 with board1 missing — so
        // every device serves the [BoardN] section matching its pin.
        assert_eq!(assignments.len(), 3);
        let by_board: HashMap<&str, &str> = assignments
            .iter()
            .map(|a| {
                (
                    a.board_name.as_str(),
                    a.device.usb_info.as_ref().unwrap().serial_number.as_deref().unwrap(),
                )
            })
            .collect();
        assert_eq!(by_board.get("board0"), Some(&"A"));
        assert_eq!(by_board.get("board2"), Some(&"C"));
        assert_eq!(by_board.get("board3"), Some(&"D"));
        // D keeps board3 (its pin); it does NOT gap-fill board1 anymore.
        // board1 (B's pin) is simply absent from the live vector.
        assert!(!by_board.contains_key("board1"));

        // Persistent config: B's pin is preserved. D stays pinned to
        // board3 (its original spot).
        assert_eq!(pin(&new_config, "board0").as_deref(), Some("A"));
        assert_eq!(pin(&new_config, "board1").as_deref(), Some("B"));
        assert_eq!(pin(&new_config, "board2").as_deref(), Some("C"));
        assert_eq!(pin(&new_config, "board3").as_deref(), Some("D"));
    }

    /// A mid-range absent pin produces a NON-CONTIGUOUS live vector: present
    /// pinned devices keep their indices (board0, board1, board3) with
    /// board2 skipped, and a brand-new device takes the lowest index that is
    /// neither taken this run nor reserved by a config pin — so it never
    /// steals the absent board2 slot. Live and persistent views agree.
    #[test]
    fn noncontiguous_live_assignment_with_absent_middle_pin() {
        let config = make_config(&[
            ("board0", "A"),
            ("board1", "B"),
            ("board2", "C"),
            ("board3", "D"),
        ]);
        // C (board2) is unplugged; A, B, D present plus a brand-new E.
        let devices = vec![
            make_device(1, "A"),
            make_device(2, "B"),
            make_device(3, "D"),
            make_device(4, "E"),
        ];

        let (assignments, new_config) = compute_board_assignments(&config, &devices);

        let by_board: HashMap<&str, &str> = assignments
            .iter()
            .map(|a| {
                (
                    a.board_name.as_str(),
                    a.device.usb_info.as_ref().unwrap().serial_number.as_deref().unwrap(),
                )
            })
            .collect();
        assert_eq!(assignments.len(), 4);

        // Present pins keep their indices; board2 is skipped (gap).
        assert_eq!(by_board.get("board0"), Some(&"A"));
        assert_eq!(by_board.get("board1"), Some(&"B"));
        assert_eq!(by_board.get("board3"), Some(&"D"));
        assert!(!by_board.contains_key("board2"));
        // New device E does NOT steal board2 (C's absent pin); it lands on
        // the lowest free, non-reserved index: board4.
        assert_eq!(by_board.get("board4"), Some(&"E"));

        // The live vector is ordered by board index, with the gap.
        let order: Vec<&str> = assignments.iter().map(|a| a.board_name.as_str()).collect();
        assert_eq!(order, vec!["board0", "board1", "board3", "board4"]);

        // Persistent config: C's pin survives, E pinned to board4.
        assert_eq!(pin(&new_config, "board0").as_deref(), Some("A"));
        assert_eq!(pin(&new_config, "board1").as_deref(), Some("B"));
        assert_eq!(pin(&new_config, "board2").as_deref(), Some("C"));
        assert_eq!(pin(&new_config, "board3").as_deref(), Some("D"));
        assert_eq!(pin(&new_config, "board4").as_deref(), Some("E"));
    }

    /// Edge case: the absent pin is board0. A brand-new device must NOT take
    /// board0 (reserved by the absent device's pin); it lands on the lowest
    /// free, non-reserved index instead.
    #[test]
    fn new_device_does_not_steal_absent_board0() {
        let config = make_config(&[("board0", "A"), ("board1", "B")]);
        // A (board0) absent; B present plus brand-new E.
        let devices = vec![make_device(1, "B"), make_device(2, "E")];

        let (assignments, new_config) = compute_board_assignments(&config, &devices);

        let by_board: HashMap<&str, &str> = assignments
            .iter()
            .map(|a| {
                (
                    a.board_name.as_str(),
                    a.device.usb_info.as_ref().unwrap().serial_number.as_deref().unwrap(),
                )
            })
            .collect();
        // B keeps board1; E does not take absent board0, gets board2.
        assert_eq!(by_board.get("board1"), Some(&"B"));
        assert_eq!(by_board.get("board2"), Some(&"E"));
        assert!(!by_board.contains_key("board0"));

        assert_eq!(pin(&new_config, "board0").as_deref(), Some("A"));
        assert_eq!(pin(&new_config, "board1").as_deref(), Some("B"));
        assert_eq!(pin(&new_config, "board2").as_deref(), Some("E"));
    }

    /// All four boards present after a partial-sync episode — the original
    /// pinning must reassert (this is the stability property the user
    /// expects across reboots).
    #[test]
    fn full_sync_preserves_existing_pins() {
        let config = make_config(&[
            ("board0", "A"),
            ("board1", "B"),
            ("board2", "C"),
            ("board3", "D"),
        ]);
        // MIDI enumeration order shuffled.
        let devices = vec![
            make_device(1, "C"),
            make_device(2, "A"),
            make_device(3, "D"),
            make_device(4, "B"),
        ];

        let (assignments, new_config) = compute_board_assignments(&config, &devices);

        let by_board: HashMap<&str, &str> = assignments
            .iter()
            .map(|a| {
                (
                    a.board_name.as_str(),
                    a.device.usb_info.as_ref().unwrap().serial_number.as_deref().unwrap(),
                )
            })
            .collect();
        assert_eq!(by_board.get("board0"), Some(&"A"));
        assert_eq!(by_board.get("board1"), Some(&"B"));
        assert_eq!(by_board.get("board2"), Some(&"C"));
        assert_eq!(by_board.get("board3"), Some(&"D"));

        assert_eq!(pin(&new_config, "board0").as_deref(), Some("A"));
        assert_eq!(pin(&new_config, "board1").as_deref(), Some("B"));
        assert_eq!(pin(&new_config, "board2").as_deref(), Some("C"));
        assert_eq!(pin(&new_config, "board3").as_deref(), Some("D"));
    }

    /// A genuinely new device should NOT take an existing absent device's
    /// slot; it gets the lowest unused board index instead.
    #[test]
    fn new_device_does_not_steal_absent_pin() {
        let config = make_config(&[
            ("board0", "A"),
            ("board1", "B"),
            ("board2", "C"),
            ("board3", "D"),
        ]);
        // B is unplugged today, E is plugged in for the first time.
        let devices = vec![
            make_device(1, "A"),
            make_device(2, "E"),
            make_device(3, "C"),
            make_device(4, "D"),
        ];

        let (_, new_config) = compute_board_assignments(&config, &devices);

        // B's pin survives. E gets a fresh slot (board4), not board1.
        assert_eq!(pin(&new_config, "board0").as_deref(), Some("A"));
        assert_eq!(pin(&new_config, "board1").as_deref(), Some("B"));
        assert_eq!(pin(&new_config, "board2").as_deref(), Some("C"));
        assert_eq!(pin(&new_config, "board3").as_deref(), Some("D"));
        assert_eq!(pin(&new_config, "board4").as_deref(), Some("E"));
    }

    /// First-ever sync (empty config) populates board0..boardN-1 in
    /// enumeration order.
    #[test]
    fn empty_config_assigns_in_enumeration_order() {
        let config = DeviceConfig {
            devices: HashMap::new(),
        };
        let devices = vec![make_device(1, "X"), make_device(2, "Y"), make_device(3, "Z")];

        let (assignments, new_config) = compute_board_assignments(&config, &devices);

        assert_eq!(assignments[0].board_name, "board0");
        assert_eq!(assignments[1].board_name, "board1");
        assert_eq!(assignments[2].board_name, "board2");

        assert_eq!(pin(&new_config, "board0").as_deref(), Some("X"));
        assert_eq!(pin(&new_config, "board1").as_deref(), Some("Y"));
        assert_eq!(pin(&new_config, "board2").as_deref(), Some("Z"));
    }
}
