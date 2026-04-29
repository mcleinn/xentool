use std::fmt;
use std::str::FromStr;

use anyhow::{Result, anyhow, bail};
use clap::ValueEnum;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Color {
    pub red: u8,
    pub green: u8,
    pub blue: u8,
}

impl Color {
    pub const fn new(red: u8, green: u8, blue: u8) -> Self {
        Self { red, green, blue }
    }

    pub fn named(name: &str) -> Option<Self> {
        let lowered = name.trim().to_ascii_lowercase();
        Some(match lowered.as_str() {
            "black" => Self::new(0, 0, 0),
            "red" => Self::new(127, 0, 0),
            "green" => Self::new(0, 127, 0),
            "blue" => Self::new(0, 0, 127),
            "amber" => Self::new(127, 64, 0),
            "yellow" => Self::new(127, 127, 0),
            "cyan" => Self::new(0, 127, 127),
            "magenta" => Self::new(127, 0, 127),
            "white" => Self::new(127, 127, 127),
            "orange" => Self::new(127, 40, 0),
            "purple" => Self::new(72, 0, 96),
            _ => return None,
        })
    }

    pub fn to_7bit(self) -> [u8; 4] {
        [self.red, self.green, self.blue, 0x00]
    }

    /// Apply a color-correction pipeline (saturation → gamma → per-channel gain)
    /// and return the adjusted color, clamped to the Exquis 7-bit range.
    pub fn corrected(self, c: &ColorCorrection) -> Self {
        let max = 127.0f32;
        let (mut r, mut g, mut b) = (
            self.red as f32 / max,
            self.green as f32 / max,
            self.blue as f32 / max,
        );

        if (c.saturation - 1.0).abs() > f32::EPSILON {
            // Linear saturation: pull channels toward/away from luminance.
            let lum = 0.299 * r + 0.587 * g + 0.114 * b;
            r = lum + (r - lum) * c.saturation;
            g = lum + (g - lum) * c.saturation;
            b = lum + (b - lum) * c.saturation;
            r = r.max(0.0);
            g = g.max(0.0);
            b = b.max(0.0);
        }
        if (c.gamma - 1.0).abs() > f32::EPSILON {
            r = r.max(0.0).powf(c.gamma);
            g = g.max(0.0).powf(c.gamma);
            b = b.max(0.0).powf(c.gamma);
        }
        r *= c.r_gain;
        g *= c.g_gain;
        b *= c.b_gain;

        Self {
            red: (r.clamp(0.0, 1.0) * max).round() as u8,
            green: (g.clamp(0.0, 1.0) * max).round() as u8,
            blue: (b.clamp(0.0, 1.0) * max).round() as u8,
        }
    }

    /// Parse a hex RGB string (e.g. "507BD8") into a Color. Accepts both
    /// 6-character RGB (`RRGGBB`) and 8-character ARGB (`AARRGGBB` as used by
    /// Lumatone `.ltn` files) — the alpha byte is ignored.
    /// Input is 8-bit (0-255); uniformly scaled to the Exquis 7-bit range (0-127).
    pub fn from_hex(hex: &str) -> Result<Self> {
        let hex = hex.trim().trim_start_matches('#');
        // Strip a leading alpha byte from ARGB (Lumatone format).
        let hex = if hex.len() == 8 { &hex[2..] } else { hex };
        if hex.len() != 6 {
            bail!("hex color must be 6 or 8 characters, got `{hex}`");
        }
        let r = u8::from_str_radix(&hex[0..2], 16)
            .map_err(|_| anyhow!("invalid hex color `{hex}`"))?;
        let g = u8::from_str_radix(&hex[2..4], 16)
            .map_err(|_| anyhow!("invalid hex color `{hex}`"))?;
        let b = u8::from_str_radix(&hex[4..6], 16)
            .map_err(|_| anyhow!("invalid hex color `{hex}`"))?;
        let scale = |v: u8| -> u8 { ((v as u32 * 127 + 127) / 255) as u8 };
        Ok(Self::new(scale(r), scale(g), scale(b)))
    }
}

impl fmt::Display for Color {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{},{},{}", self.red, self.green, self.blue)
    }
}

impl FromStr for Color {
    type Err = anyhow::Error;

    fn from_str(input: &str) -> Result<Self> {
        if let Some(color) = Self::named(input) {
            return Ok(color);
        }

        let parts: Vec<_> = input.split(',').map(str::trim).collect();
        if parts.len() != 3 {
            bail!("color must be a named color or r,g,b");
        }

        let mut rgb = [0u8; 3];
        for (index, part) in parts.iter().enumerate() {
            let value: u16 = part
                .parse()
                .map_err(|_| anyhow!("invalid color value `{part}`"))?;
            if value > 255 {
                bail!("RGB values must be 0..255");
            }
            // Uniform scale from 0-255 to 0-127.
            rgb[index] = ((value as u32 * 127 + 127) / 255) as u8;
        }

        Ok(Self::new(rgb[0], rgb[1], rgb[2]))
    }
}

pub fn color_help_text() -> &'static str {
    "named colors like black, amber, red or RGB like 255,128,0"
}

/// Runtime color correction applied before sending colors to the Exquis.
/// Default is identity (no change).
#[derive(Debug, Clone, Copy)]
pub struct ColorCorrection {
    /// Saturation multiplier (1.0 = no change, >1 boosts, <1 desaturates).
    pub saturation: f32,
    /// Gamma exponent applied per channel to the normalized value (1.0 = no change, >1 darkens mids).
    pub gamma: f32,
    /// Per-channel brightness multiplier, applied last. (1.0 = no change).
    pub r_gain: f32,
    pub g_gain: f32,
    pub b_gain: f32,
}

impl Default for ColorCorrection {
    fn default() -> Self {
        Self {
            saturation: 1.0,
            gamma: 1.0,
            r_gain: 1.0,
            g_gain: 1.0,
            b_gain: 1.0,
        }
    }
}

impl ColorCorrection {
    pub fn is_identity(&self) -> bool {
        (self.saturation - 1.0).abs() < f32::EPSILON
            && (self.gamma - 1.0).abs() < f32::EPSILON
            && (self.r_gain - 1.0).abs() < f32::EPSILON
            && (self.g_gain - 1.0).abs() < f32::EPSILON
            && (self.b_gain - 1.0).abs() < f32::EPSILON
    }

    /// Parse `"r,g,b"` like `"1.0,1.0,0.5"` into (r_gain, g_gain, b_gain).
    pub fn parse_rgb_gain(s: &str) -> Result<(f32, f32, f32)> {
        let parts: Vec<&str> = s.split(',').map(str::trim).collect();
        if parts.len() != 3 {
            bail!("rgb-gain must be `r,g,b`, got `{s}`");
        }
        let r: f32 = parts[0].parse().map_err(|_| anyhow!("invalid r_gain `{}`", parts[0]))?;
        let g: f32 = parts[1].parse().map_err(|_| anyhow!("invalid g_gain `{}`", parts[1]))?;
        let b: f32 = parts[2].parse().map_err(|_| anyhow!("invalid b_gain `{}`", parts[2]))?;
        Ok((r, g, b))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum NamedZone {
    Pads,
    Encoders,
    Slider,
    UpDown,
    Settings,
    OtherButtons,
}

impl NamedZone {
    pub const fn bit(self) -> u8 {
        match self {
            Self::Pads => 0x01,
            Self::Encoders => 0x02,
            Self::Slider => 0x04,
            Self::UpDown => 0x08,
            Self::Settings => 0x10,
            Self::OtherButtons => 0x20,
        }
    }
}

pub fn enter_dev_mode(mask: u8) -> Vec<u8> {
    vec![0xF0, 0x00, 0x21, 0x7E, 0x7F, 0x00, mask, 0xF7]
}

pub fn exit_dev_mode() -> Vec<u8> {
    enter_dev_mode(0x00)
}

pub fn set_led_color(pad: u8, color: Color) -> Vec<u8> {
    let mut bytes = vec![0xF0, 0x00, 0x21, 0x7E, 0x7F, 0x04, pad];
    bytes.extend_from_slice(&color.to_7bit());
    bytes.push(0xF7);
    bytes
}

pub fn fill_all_pads(color: Color) -> Vec<u8> {
    let mut bytes = vec![0xF0, 0x00, 0x21, 0x7E, 0x7F, 0x04, 0x00];
    for _ in 0..61 {
        bytes.extend_from_slice(&color.to_7bit());
    }
    bytes.push(0xF7);
    bytes
}

/// Build a Snapshot message (cmd 09h) that sets note mappings AND colors for all 61 pads.
/// This works via dev mode WITHOUT taking over the pad zone, so MPE is preserved.
/// Format: F0 00 21 7E 7F 09 [11 header bytes] [61 × (midinote, r, g, b)] F7  (262 bytes)
/// `pads` is a slice of 61 `(midinote, Color)` tuples.
///
/// Header bytes were verified against the live device's GET-snapshot reply on
/// firmware 3.0.0 (device defaults to `00 01 01 0E 00 00 01 01 00 00 00`).
/// PitchGridRack's `exquis.hpp:282` ships `00 01 00 0E ...` — that targets
/// older firmware and does not work on 3.0.0; sending it silently disables
/// MPE per-note pitch bend (X axis) on the pads, even after dev mode exits.
///
/// Byte 9 (`PBRange`, in /48 of the synth's bend range) is overridden to
/// `0x30` (= 48/48 — the Exquis's max output) so the player can use the
/// pad's full X-slide range. At `--pb-range 16` this gives full slide
/// ±16 semitones (±1600 c) at the synth, with sub-cent tuning resolution
/// (~5 LSB/cent) and <2 % combined-clip loss even on worst-case retunes.
pub fn snapshot_set_pads(pads: &[(u8, Color); 61]) -> Vec<u8> {
    let mut bytes: Vec<u8> = vec![
        0xF0, 0x00, 0x21, 0x7E, 0x7F, 0x09,
        0x00, 0x01, 0x01, 0x30, 0x00, 0x00, 0x01, 0x01, 0x00, 0x00, 0x00,
    ];
    for &(midinote, color) in pads.iter() {
        bytes.push(midinote);
        bytes.push(color.red);
        bytes.push(color.green);
        bytes.push(color.blue);
    }
    bytes.push(0xF7);
    debug_assert_eq!(bytes.len(), 262);
    bytes
}

/// Build a Snapshot message that sets colors for all 61 pads using default note mapping
/// (pad 0 = MIDI 36, pad 1 = MIDI 37, ..., pad 60 = MIDI 96).
pub fn snapshot_fill_color(color: Color) -> Vec<u8> {
    let mut pads = [(0u8, color); 61];
    for i in 0..61 {
        pads[i].0 = 36 + i as u8;
    }
    snapshot_set_pads(&pads)
}

/// Build a Snapshot message that sets per-pad colors using default note mapping.
pub fn snapshot_set_colors(colors: &[Color; 61]) -> Vec<u8> {
    let mut pads = [(0u8, Color::new(0, 0, 0)); 61];
    for i in 0..61 {
        pads[i] = (36 + i as u8, colors[i]);
    }
    snapshot_set_pads(&pads)
}

/// Dev mode mask for all zones EXCEPT pads.
/// Activates dev mode (so SysEx commands work) without taking over pad input.
/// Covers encoders (0x02), slider (0x04), up/down (0x08), settings (0x10), other buttons (0x20).
pub const DEV_MASK_NO_PADS: u8 = 0x3E;

pub fn control_name(id: u8) -> Option<String> {
    let label = match id {
        0..=60 => format!("pad_{id}"),
        80..=85 => format!("slider_portion_{}", id - 79),
        90 => "slider_position".to_string(),
        100 => "settings".to_string(),
        101 => "sound".to_string(),
        102 => "record".to_string(),
        103 => "loop".to_string(),
        104 => "clips".to_string(),
        105 => "play_stop".to_string(),
        106 => "down".to_string(),
        107 => "up".to_string(),
        108 => "undo".to_string(),
        109 => "redo".to_string(),
        110..=113 => format!("encoder_{}", id - 109),
        114..=117 => format!("encoder_{}_button", id - 113),
        _ => return None,
    };
    Some(label)
}

pub fn control_display_name(id: u8) -> Option<String> {
    let label = match id {
        0..=60 => format!("Pad {id}"),
        80..=85 => format!("Slider Portion {}", id - 79),
        90 => "Slider Position".to_string(),
        100 => "Settings".to_string(),
        101 => "Sound".to_string(),
        102 => "Record".to_string(),
        103 => "Loop".to_string(),
        104 => "Clips".to_string(),
        105 => "Play/Stop".to_string(),
        106 => "Down".to_string(),
        107 => "Up".to_string(),
        108 => "Undo".to_string(),
        109 => "Redo".to_string(),
        110..=113 => format!("Encoder {}", id - 109),
        114..=117 => format!("Encoder {} Button", id - 113),
        _ => return None,
    };
    Some(label)
}

/// Parse a control name (e.g. "settings", "encoder-1", "slider-1") into its numeric ID.
/// Also accepts raw numeric IDs as strings.
pub fn control_id_from_name(name: &str) -> Option<u8> {
    let lowered = name.trim().to_ascii_lowercase();
    let lowered = lowered.replace('-', "_");
    match lowered.as_str() {
        "settings" => Some(100),
        "sound" => Some(101),
        "record" => Some(102),
        "loop" => Some(103),
        "clips" => Some(104),
        "play_stop" | "play" | "stop" => Some(105),
        "down" => Some(106),
        "up" => Some(107),
        "undo" => Some(108),
        "redo" => Some(109),
        "encoder_1" => Some(110),
        "encoder_2" => Some(111),
        "encoder_3" => Some(112),
        "encoder_4" => Some(113),
        "encoder_1_button" => Some(114),
        "encoder_2_button" => Some(115),
        "encoder_3_button" => Some(116),
        "encoder_4_button" => Some(117),
        "encoder_5_button" => Some(118),
        "slider_1" | "slider_portion_1" => Some(80),
        "slider_2" | "slider_portion_2" => Some(81),
        "slider_3" | "slider_portion_3" => Some(82),
        "slider_4" | "slider_portion_4" => Some(83),
        "slider_5" | "slider_portion_5" => Some(84),
        "slider_6" | "slider_portion_6" => Some(85),
        "slider_position" => Some(90),
        _ => lowered.parse::<u8>().ok(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_named_color() {
        assert_eq!(Color::from_str("amber").unwrap(), Color::new(127, 64, 0));
    }

    #[test]
    fn scales_rgb_255_to_127() {
        assert_eq!(
            Color::from_str("255,128,0").unwrap(),
            Color::new(127, 64, 0)
        );
        // Uniform scaling: 127 → 63 (was 127 under the asymmetric rule)
        assert_eq!(
            Color::from_str("127,127,127").unwrap(),
            Color::new(63, 63, 63)
        );
    }

    #[test]
    fn builds_fill_message() {
        let msg = fill_all_pads(Color::new(1, 2, 3));
        assert_eq!(&msg[..7], &[0xF0, 0x00, 0x21, 0x7E, 0x7F, 0x04, 0x00]);
        assert_eq!(msg.last().copied(), Some(0xF7));
        assert_eq!(msg.len(), 7 + 61 * 4 + 1);
    }

    #[test]
    fn parses_hex_color() {
        // Uniform 8→7-bit scaling: v_7 = round(v_8 * 127 / 255).
        assert_eq!(Color::from_hex("FF0000").unwrap(), Color::new(127, 0, 0));
        // 0x7F=127 → 127*127/255 ≈ 63
        assert_eq!(Color::from_hex("007F00").unwrap(), Color::new(0, 63, 0));
        // 0x50=80→40, 0x7B=123→61, 0xD8=216→108
        assert_eq!(Color::from_hex("507BD8").unwrap(), Color::new(40, 61, 108));
    }

    #[test]
    fn resolves_control_names() {
        assert_eq!(control_id_from_name("settings"), Some(100));
        assert_eq!(control_id_from_name("encoder-1"), Some(110));
        assert_eq!(control_id_from_name("slider-1"), Some(80));
        assert_eq!(control_id_from_name("110"), Some(110));
        assert_eq!(control_id_from_name("unknown"), None);
    }
}
