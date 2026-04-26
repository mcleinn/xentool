use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context, Result, bail};

use crate::exquis::proto::Color;

/// A single pad entry from an .xtn file.
#[derive(Debug, Clone)]
pub struct PadEntry {
    pub key: u8,
    pub chan: u8,
    pub color: Color,
}

/// Layout data for one board.
#[derive(Debug, Clone)]
pub struct BoardLayout {
    /// Keyed by pad index (0-60).
    pub pads: HashMap<u8, PadEntry>,
}

/// Parsed .xtn layout file containing one or more boards.
#[derive(Debug, Clone)]
pub struct XtnLayout {
    /// EDO divisions (steps per octave), e.g. 31 for 31-EDO.
    pub edo: Option<i32>,
    /// Pitch offset in EDO steps (default 0).
    pub pitch_offset: i32,
    /// Keyed by board name (e.g. "Board0", "Board1"). Stored lowercase.
    pub boards: HashMap<String, BoardLayout>,
}

/// Parse an .xtn file (INI-style, compatible with .wtn/.ltn).
///
/// Format:
/// ```text
/// [Board0]
/// Key_0=48
/// Chan_0=3
/// Col_0=507BD8
/// Key_1=50
/// ...
/// ```
pub fn parse_xtn(path: &Path) -> Result<XtnLayout> {
    let path_str = path.display().to_string();
    let content =
        std::fs::read_to_string(path).with_context(|| format!("reading {path_str}"))?;
    parse_xtn_str(&content, &path_str)
}

/// Parse an .xtn/.wtn/.ltn formatted string. `source` is used only for error messages.
/// This variant also accepts pad indices up to 60 for .xtn, but for .ltn/.wtn imports
/// the frontend supplies the geometry to reinterpret indices into hex coords.
pub fn parse_xtn_str(content: &str, source: &str) -> Result<XtnLayout> {
    let mut boards: HashMap<String, HashMap<u8, (Option<u8>, Option<u8>, Option<Color>)>> =
        HashMap::new();
    let mut current_board: Option<String> = None;
    let mut edo: Option<i32> = None;
    let mut pitch_offset: i32 = 0;

    for (line_num, line) in content.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }

        // Section header: [Board0]
        if line.starts_with('[') && line.ends_with(']') {
            let name = line[1..line.len() - 1].trim().to_lowercase();
            boards.entry(name.clone()).or_default();
            current_board = Some(name);
            continue;
        }

        // Key=Value
        let Some((key_part, value)) = line.split_once('=') else {
            bail!("{}:{}: expected Key=Value, got `{line}`", source, line_num + 1);
        };
        let key_part = key_part.trim();
        let value = value.trim();

        // Global header keys (before any [Board] section)
        if current_board.is_none() {
            match key_part {
                "Edo" => {
                    edo = Some(value.parse().with_context(|| {
                        format!("{}:{}: invalid Edo value", source, line_num + 1)
                    })?);
                }
                "PitchOffset" => {
                    pitch_offset = value.parse().with_context(|| {
                        format!("{}:{}: invalid PitchOffset value", source, line_num + 1)
                    })?;
                }
                _ => {} // ignore unknown header keys
            }
            continue;
        }
        let board_name = current_board.as_ref().unwrap();

        // Parse Key_N, Chan_N, Col_N
        let (prefix, index_str) = key_part
            .rsplit_once('_')
            .with_context(|| format!("{}:{}: expected Key_N, Chan_N, or Col_N", source, line_num + 1))?;

        let index: u8 = index_str
            .parse()
            .with_context(|| format!("{}:{}: invalid pad index `{index_str}`", source, line_num + 1))?;

        if index > 127 {
            bail!("{}:{}: pad index {index} out of range 0-127", source, line_num + 1);
        }

        let entry = boards
            .get_mut(board_name)
            .unwrap()
            .entry(index)
            .or_insert((None, None, None));

        match prefix {
            "Key" => {
                let v: u8 = value
                    .parse()
                    .with_context(|| format!("{}:{}: invalid Key value", source, line_num + 1))?;
                entry.0 = Some(v);
            }
            "Chan" => {
                let v: u8 = value
                    .parse()
                    .with_context(|| format!("{}:{}: invalid Chan value", source, line_num + 1))?;
                entry.1 = Some(v);
            }
            "Col" => {
                let color = Color::from_hex(value)
                    .with_context(|| format!("{}:{}: invalid Col value", source, line_num + 1))?;
                entry.2 = Some(color);
            }
            _ => {
                // Ignore unknown prefixes for forward compatibility
            }
        }
    }

    // Convert raw entries to structured BoardLayouts
    let mut result = HashMap::new();
    for (board_name, raw_pads) in boards {
        let mut pads = HashMap::new();
        for (index, (key, chan, color)) in raw_pads {
            pads.insert(
                index,
                PadEntry {
                    key: key.unwrap_or(index),
                    chan: chan.unwrap_or(1),
                    color: color.unwrap_or(Color::new(0, 0, 0)),
                },
            );
        }
        result.insert(board_name, BoardLayout { pads });
    }

    Ok(XtnLayout {
        edo,
        pitch_offset,
        boards: result,
    })
}

/// Serialize an XtnLayout to the INI format readable by `parse_xtn`.
/// Colors use "000000".."FFFFFF" 8-bit hex (inverse of Color::from_hex scaling).
pub fn write_xtn_layout(layout: &XtnLayout) -> String {
    let mut out = String::new();
    if let Some(edo) = layout.edo {
        out.push_str(&format!("Edo={edo}\n"));
    }
    out.push_str(&format!("PitchOffset={}\n", layout.pitch_offset));

    let mut names: Vec<&String> = layout.boards.keys().collect();
    names.sort_by(|a, b| {
        let na = a.trim_start_matches("board").parse::<u32>().unwrap_or(u32::MAX);
        let nb = b.trim_start_matches("board").parse::<u32>().unwrap_or(u32::MAX);
        na.cmp(&nb).then_with(|| a.cmp(b))
    });

    for name in names {
        out.push('\n');
        let header = if let Some(rest) = name.strip_prefix("board") {
            format!("Board{rest}")
        } else {
            name.clone()
        };
        out.push_str(&format!("[{header}]\n"));
        let board = &layout.boards[name];
        let mut indices: Vec<u8> = board.pads.keys().copied().collect();
        indices.sort();
        for idx in indices {
            let p = &board.pads[&idx];
            let to8 = |v: u8| -> u8 { ((v as u32 * 255 + 63) / 127).min(255) as u8 };
            let hex = format!(
                "{:02X}{:02X}{:02X}",
                to8(p.color.red),
                to8(p.color.green),
                to8(p.color.blue),
            );
            out.push_str(&format!("Key_{idx}={}\n", p.key));
            out.push_str(&format!("Chan_{idx}={}\n", p.chan));
            out.push_str(&format!("Col_{idx}={hex}\n"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_minimal_xtn() {
        let path = std::env::temp_dir().join(format!("exquis_test_{}.xtn", std::process::id()));
        std::fs::write(
            &path,
            "[Board0]\n\
             Key_0=48\n\
             Chan_0=3\n\
             Col_0=FF0000\n\
             Key_1=50\n\
             Chan_1=3\n\
             Col_1=00FF00\n",
        )
        .unwrap();

        let layout = parse_xtn(&path).unwrap();
        let _ = std::fs::remove_file(&path);

        assert!(layout.boards.contains_key("board0"));
        let board = &layout.boards["board0"];
        assert_eq!(board.pads[&0].key, 48);
        assert_eq!(board.pads[&0].chan, 3);
        assert_eq!(board.pads[&0].color.red, 127); // FF scaled to 127
        assert_eq!(board.pads[&1].key, 50);
        assert_eq!(board.pads[&1].color, Color::new(0, 127, 0));
    }
}
