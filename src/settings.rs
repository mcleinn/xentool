//! Central, user-editable settings file for the `xentool` CLI.
//!
//! Located next to `devices.json` (the auto-managed hardware↔board mapping) at
//! `%LOCALAPPDATA%\xentool\config\settings.json`. Two
//! top-level sections, `exquis` and `wooting`. The `wooting` section's field
//! shapes and default values are ported verbatim from the xenwooting project
//! (`C:\Dev-Free\xenwooting\xenwooting\src\config.rs`) so behaviour matches
//! the known-good tuning.
//!
//! Missing file, missing section, or missing field → use the xenwooting default.

use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Settings {
    #[serde(default)]
    pub exquis: ExquisSettings,
    #[serde(default)]
    pub wooting: WootingSettings,
    /// Basename of the most recently active `.wtn` layout, persisted across runs.
    #[serde(default)]
    pub last_wtn: Option<String>,
    /// Basename of the most recently active `.xtn` layout, persisted across runs.
    #[serde(default)]
    pub last_xtn: Option<String>,
}

/// Reserved for future migration of Exquis CLI defaults. Empty for now.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ExquisSettings {}

// --- Wooting section (verbatim from xenwooting/config.rs) ---

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WootingSettings {
    #[serde(default = "default_press_threshold")]
    pub press_threshold: f32,

    #[serde(default = "default_press_threshold_step")]
    pub press_threshold_step: f32,

    #[serde(default = "default_aftertouch_press_threshold")]
    pub aftertouch_press_threshold: f32,

    #[serde(default = "default_velocity_peak_track_ms")]
    pub velocity_peak_track_ms: u32,

    #[serde(default = "default_aftershock_ms")]
    pub aftershock_ms: u32,

    #[serde(default = "default_velocity_max_swing")]
    pub velocity_max_swing: f32,

    #[serde(default = "default_aftertouch_delta")]
    pub aftertouch_delta: f32,

    #[serde(default = "default_release_delta")]
    pub release_delta: f32,

    #[serde(default = "default_aftertouch_speed_max")]
    pub aftertouch_speed_max: f32,

    #[serde(default = "default_aftertouch_speed_step")]
    pub aftertouch_speed_step: f32,

    #[serde(default = "default_aftertouch_speed_attack_ms")]
    pub aftertouch_speed_attack_ms: u32,

    #[serde(default = "default_aftertouch_speed_decay_ms")]
    pub aftertouch_speed_decay_ms: u32,

    #[serde(default)]
    pub rgb: RgbSettings,

    #[serde(default)]
    pub control_bar: ControlBarSettings,

    #[serde(default)]
    pub boards: Vec<BoardSettings>,
}

impl Default for WootingSettings {
    fn default() -> Self {
        Self {
            press_threshold: default_press_threshold(),
            press_threshold_step: default_press_threshold_step(),
            aftertouch_press_threshold: default_aftertouch_press_threshold(),
            velocity_peak_track_ms: default_velocity_peak_track_ms(),
            aftershock_ms: default_aftershock_ms(),
            velocity_max_swing: default_velocity_max_swing(),
            aftertouch_delta: default_aftertouch_delta(),
            release_delta: default_release_delta(),
            aftertouch_speed_max: default_aftertouch_speed_max(),
            aftertouch_speed_step: default_aftertouch_speed_step(),
            aftertouch_speed_attack_ms: default_aftertouch_speed_attack_ms(),
            aftertouch_speed_decay_ms: default_aftertouch_speed_decay_ms(),
            rgb: RgbSettings::default(),
            control_bar: ControlBarSettings::default(),
            boards: default_boards(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RgbSettings {
    #[serde(default = "default_screensaver_timeout_sec")]
    pub screensaver_timeout_sec: u32,
}

impl Default for RgbSettings {
    fn default() -> Self {
        Self {
            screensaver_timeout_sec: default_screensaver_timeout_sec(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ControlBarSettings {
    #[serde(default = "default_control_bar_row")]
    pub row: u8,

    #[serde(default)]
    pub led_cols_by_hid: HashMap<String, OneOrManyU8>,
}

impl Default for ControlBarSettings {
    fn default() -> Self {
        let mut led_cols_by_hid: HashMap<String, OneOrManyU8> = HashMap::new();
        led_cols_by_hid.insert("LeftCtrl".to_string(), OneOrManyU8::One(0));
        led_cols_by_hid.insert("LeftMeta".to_string(), OneOrManyU8::One(1));
        led_cols_by_hid.insert("LeftAlt".to_string(), OneOrManyU8::One(2));
        led_cols_by_hid.insert("Space".to_string(), OneOrManyU8::Many(vec![4, 5, 6, 7, 8]));
        led_cols_by_hid.insert("RightAlt".to_string(), OneOrManyU8::One(10));
        led_cols_by_hid.insert("ContextMenu".to_string(), OneOrManyU8::One(11));
        led_cols_by_hid.insert("RightCtrl".to_string(), OneOrManyU8::One(12));
        Self {
            row: default_control_bar_row(),
            led_cols_by_hid,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum OneOrManyU8 {
    One(u8),
    Many(Vec<u8>),
}

impl OneOrManyU8 {
    pub fn as_vec(&self) -> Vec<u8> {
        match self {
            OneOrManyU8::One(v) => vec![*v],
            OneOrManyU8::Many(vs) => vs.clone(),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BoardSettings {
    /// Analog device_id (u64), stored as string (u64::MAX doesn't round-trip via JSON number).
    #[serde(default)]
    pub device_id: Option<String>,

    /// Which .wtn board section to use for this device, e.g. 0 or 1.
    pub wtn_board: u8,

    /// Rotation of the playable 4x14 grid in degrees. Supported: 0 or 180.
    #[serde(default)]
    pub rotation_deg: u16,

    /// Mirror the playable 4x14 grid left-right for .wtn lookup.
    #[serde(default)]
    pub mirror_cols: bool,

    /// HID key on this board that drives analog MIDI CC output.
    #[serde(default)]
    pub cc_analog_hid: Option<String>,

    /// MIDI CC number (0..127) to send from cc_analog_hid scaled by analog depth.
    /// Defaults per wtn_board: 0 -> 4, 1 -> 3.
    #[serde(default)]
    pub cc_analog_cc: Option<u8>,

    /// RGB SDK device index to target when painting LEDs on this board.
    ///
    /// The Wooting Analog SDK and Wooting RGB SDK enumerate connected
    /// keyboards independently and may produce different orderings. If you
    /// see the white key-press flash appearing on the wrong keyboard, set
    /// this per-board to explicitly pick the RGB SDK index. Defaults to
    /// `wtn_board`.
    #[serde(default)]
    pub rgb_device_index: Option<u8>,
}

impl BoardSettings {
    pub fn device_id_u64(&self) -> Result<Option<u64>> {
        let Some(s) = self.device_id.as_deref() else {
            return Ok(None);
        };
        let id: u64 = s
            .trim()
            .parse()
            .with_context(|| format!("invalid device_id `{s}`"))?;
        Ok(Some(id))
    }

    /// Resolved (hid_name, cc_number) for this board, with xenwooting defaults.
    pub fn cc_analog(&self) -> Option<(String, u8)> {
        let default_cc: Option<u8> = match self.wtn_board {
            0 => Some(4),
            1 => Some(3),
            _ => None,
        };
        let cc = self.cc_analog_cc.or(default_cc)?;
        let hid = self
            .cc_analog_hid
            .clone()
            .unwrap_or_else(|| "LeftMeta".to_string());
        Some((hid, cc))
    }
}

fn default_boards() -> Vec<BoardSettings> {
    vec![
        BoardSettings {
            wtn_board: 0,
            cc_analog_hid: Some("LeftMeta".to_string()),
            cc_analog_cc: Some(4),
            ..Default::default()
        },
        BoardSettings {
            wtn_board: 1,
            cc_analog_hid: Some("LeftMeta".to_string()),
            cc_analog_cc: Some(3),
            ..Default::default()
        },
    ]
}

// --- Default functions (verbatim from xenwooting/config.rs) ---

fn default_press_threshold() -> f32 {
    0.75
}
fn default_press_threshold_step() -> f32 {
    0.05
}
fn default_aftertouch_press_threshold() -> f32 {
    0.10
}
fn default_velocity_peak_track_ms() -> u32 {
    6
}
fn default_aftershock_ms() -> u32 {
    35
}
fn default_velocity_max_swing() -> f32 {
    1.0
}
fn default_aftertouch_delta() -> f32 {
    0.01
}
fn default_release_delta() -> f32 {
    0.12
}
fn default_aftertouch_speed_max() -> f32 {
    100.0
}
fn default_aftertouch_speed_step() -> f32 {
    2.0
}
fn default_aftertouch_speed_attack_ms() -> u32 {
    12
}
fn default_aftertouch_speed_decay_ms() -> u32 {
    250
}
fn default_screensaver_timeout_sec() -> u32 {
    300
}
fn default_control_bar_row() -> u8 {
    5
}

// --- File I/O ---

pub fn default_settings_path() -> PathBuf {
    if let Some(dirs) = directories::BaseDirs::new() {
        dirs.data_local_dir()
            .join("xentool")
            .join("config")
            .join("settings.json")
    } else {
        PathBuf::from("settings.json")
    }
}

/// Persist the most recently active layout's **basename** to `settings.json`.
///
/// Read-modify-write on the whole file. Non-fatal: errors are logged to stderr
/// and otherwise ignored so an un-writable disk doesn't crash serve.
///
/// Safe to call from any thread. Not safe to call from the Wooting 1 kHz
/// hot loop — callers there must spawn a short-lived thread for this.
pub fn store_last_layout(kind: crate::layouts::LayoutKind, path: &std::path::Path) {
    let Some(name) = path.file_name().and_then(|n| n.to_str()).map(|s| s.to_string())
    else {
        return;
    };
    let mut settings = load();
    match kind {
        crate::layouts::LayoutKind::Wtn => settings.last_wtn = Some(name),
        crate::layouts::LayoutKind::Xtn => settings.last_xtn = Some(name),
    }
    let p = default_settings_path();
    if let Some(parent) = p.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            eprintln!("settings: create_dir_all({}) failed: {e}", parent.display());
            return;
        }
    }
    let json = match serde_json::to_string_pretty(&settings) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("settings: serialize failed: {e}");
            return;
        }
    };
    if let Err(e) = std::fs::write(&p, json) {
        eprintln!("settings: write {} failed: {e}", p.display());
    }
}

/// Read the settings file. Missing file → all defaults (no error, no write).
pub fn load() -> Settings {
    let path = default_settings_path();
    if !path.exists() {
        return Settings::default();
    }
    let content = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("settings: failed to read {} ({e}); using defaults", path.display());
            return Settings::default();
        }
    };
    match serde_json::from_str::<Settings>(&content) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("settings: failed to parse {} ({e}); using defaults", path.display());
            Settings::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_xenwooting() {
        let w = WootingSettings::default();
        assert_eq!(w.press_threshold, 0.75);
        assert_eq!(w.press_threshold_step, 0.05);
        assert_eq!(w.aftertouch_press_threshold, 0.10);
        assert_eq!(w.aftertouch_speed_max, 100.0);
        assert_eq!(w.aftertouch_speed_step, 2.0);
        assert_eq!(w.rgb.screensaver_timeout_sec, 300);
        assert_eq!(w.control_bar.row, 5);
    }

    #[test]
    fn board0_defaults_to_cc4_leftmeta() {
        let b = BoardSettings { wtn_board: 0, ..Default::default() };
        assert_eq!(b.cc_analog(), Some(("LeftMeta".to_string(), 4)));
        let b1 = BoardSettings { wtn_board: 1, ..Default::default() };
        assert_eq!(b1.cc_analog(), Some(("LeftMeta".to_string(), 3)));
    }

    #[test]
    fn empty_json_parses_to_defaults() {
        let s: Settings = serde_json::from_str("{}").unwrap();
        assert_eq!(s.wooting.press_threshold, 0.75);
        assert_eq!(s.wooting.control_bar.row, 5);
        // The default control-bar map is populated with the xenwooting defaults.
        assert_eq!(
            s.wooting.control_bar.led_cols_by_hid["Space"].as_vec(),
            vec![4u8, 5, 6, 7, 8]
        );
        assert_eq!(
            s.wooting.control_bar.led_cols_by_hid["RightAlt"].as_vec(),
            vec![10u8]
        );
    }

    #[test]
    fn partial_wooting_merges_with_defaults() {
        let json = r#"{ "wooting": { "press_threshold": 0.5 } }"#;
        let s: Settings = serde_json::from_str(json).unwrap();
        assert_eq!(s.wooting.press_threshold, 0.5);
        assert_eq!(s.wooting.press_threshold_step, 0.05);
        assert_eq!(s.wooting.rgb.screensaver_timeout_sec, 300);
    }

    #[test]
    fn one_or_many_u8_round_trip() {
        let j = serde_json::json!({ "led_cols_by_hid": { "Space": [4, 5, 6, 7, 8], "LeftCtrl": 0 } });
        let cb: ControlBarSettings = serde_json::from_value(j).unwrap();
        assert_eq!(cb.led_cols_by_hid["Space"].as_vec(), vec![4u8, 5, 6, 7, 8]);
        assert_eq!(cb.led_cols_by_hid["LeftCtrl"].as_vec(), vec![0u8]);
    }
}
