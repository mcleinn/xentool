//! `.wtn` layout file parser/writer.
//!
//! Format is the same INI-style as `.xtn` / `.ltn`: `[BoardN]` sections with
//! `Key_N`, `Chan_N`, `Col_N` entries. Each board has 56 logical cells
//! (4 rows × 14 cols, with wide-key gaps absorbed by the HidMap).
//!
//! Colors are accepted as both 6-char `RRGGBB` and 8-char `AARRGGBB` (Lumatone
//! format — alpha byte ignored).

use anyhow::{Context, Result, bail};
use std::collections::HashMap;
use std::path::Path;

pub const WTN_CELLS_PER_BOARD: usize = 56; // 4 rows × 14 cols

/// A single logical cell in the WTN grid.
#[derive(Debug, Clone, Copy, Default)]
pub struct WtnCell {
    pub key: u8,
    /// Stored as file value (1..=16). 0 is our "missing / not set" marker.
    pub chan: u8,
    /// 8-bit RGB stored verbatim from the file.
    pub color: (u8, u8, u8),
}

#[derive(Debug, Clone, Default)]
pub struct Wtn {
    pub edo: Option<i32>,
    pub pitch_offset: i32,
    pub boards: HashMap<u8, Vec<WtnCell>>, // board index -> WTN_CELLS_PER_BOARD entries
}

impl Wtn {
    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("reading {}", path.display()))?;
        parse_wtn(&text)
    }

    pub fn cell(&self, board: u8, idx_0_55: usize) -> Option<WtnCell> {
        self.boards.get(&board).and_then(|v| v.get(idx_0_55)).copied()
    }
}

fn parse_hex_color_6_or_8(s: &str) -> Result<(u8, u8, u8)> {
    let mut s = s.trim();
    if s.starts_with('#') {
        s = &s[1..];
    }
    // Accept 8-char ARGB by taking the trailing 6 chars.
    if s.len() == 8 {
        s = &s[2..];
    }
    if s.len() != 6 {
        bail!("color must be 6 or 8 hex chars, got `{s}`");
    }
    let r = u8::from_str_radix(&s[0..2], 16).context("invalid R")?;
    let g = u8::from_str_radix(&s[2..4], 16).context("invalid G")?;
    let b = u8::from_str_radix(&s[4..6], 16).context("invalid B")?;
    Ok((r, g, b))
}

pub fn parse_wtn(text: &str) -> Result<Wtn> {
    let mut edo: Option<i32> = None;
    let mut pitch_offset: i32 = 0;
    let mut current: Option<u8> = None;
    let mut tmp: HashMap<(u8, usize), WtnCell> = HashMap::new();

    for (line_num, raw) in text.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        if line.starts_with('[') && line.ends_with(']') {
            let name = line[1..line.len() - 1].trim();
            let b: u8 = name
                .strip_prefix("Board")
                .or_else(|| name.strip_prefix("board"))
                .unwrap_or("")
                .parse()
                .with_context(|| format!("line {}: bad section header `{name}`", line_num + 1))?;
            current = Some(b);
            continue;
        }
        let Some((k, v)) = line.split_once('=') else {
            bail!("line {}: expected Key=Value", line_num + 1);
        };
        let k = k.trim();
        let v = v.trim();

        // Global header keys (before any [BoardN]).
        if current.is_none() {
            match k {
                "Edo" => {
                    edo = Some(v.parse().with_context(|| format!("line {}: bad Edo", line_num + 1))?);
                }
                "PitchOffset" => {
                    pitch_offset = v.parse().with_context(|| format!("line {}: bad PitchOffset", line_num + 1))?;
                }
                _ => {}
            }
            continue;
        }
        let b = current.unwrap();

        let Some((field, idx_str)) = k.rsplit_once('_') else { continue };
        let idx: usize = idx_str
            .parse()
            .with_context(|| format!("line {}: bad index in `{k}`", line_num + 1))?;
        if idx >= WTN_CELLS_PER_BOARD {
            continue; // ignore extras beyond 56
        }

        let cell = tmp.entry((b, idx)).or_insert_with(WtnCell::default);
        match field {
            "Key" => {
                cell.key = v
                    .parse()
                    .with_context(|| format!("line {}: bad Key value", line_num + 1))?;
            }
            "Chan" => {
                cell.chan = v
                    .parse()
                    .with_context(|| format!("line {}: bad Chan value", line_num + 1))?;
            }
            "Col" => {
                cell.color = parse_hex_color_6_or_8(v)
                    .with_context(|| format!("line {}: bad Col", line_num + 1))?;
            }
            _ => {}
        }
    }

    let mut boards: HashMap<u8, Vec<WtnCell>> = HashMap::new();
    for ((b, idx), cell) in tmp {
        let v = boards.entry(b).or_insert_with(|| vec![WtnCell::default(); WTN_CELLS_PER_BOARD]);
        v[idx] = cell;
    }

    Ok(Wtn { edo, pitch_offset, boards })
}

/// Serialize a WTN back to the INI format.
pub fn write_wtn(wtn: &Wtn) -> String {
    let mut out = String::new();
    if let Some(edo) = wtn.edo {
        out.push_str(&format!("Edo={edo}\n"));
    }
    out.push_str(&format!("PitchOffset={}\n", wtn.pitch_offset));

    let mut board_indices: Vec<u8> = wtn.boards.keys().copied().collect();
    board_indices.sort();
    for b in board_indices {
        out.push('\n');
        out.push_str(&format!("[Board{b}]\n"));
        let cells = &wtn.boards[&b];
        for (i, c) in cells.iter().enumerate() {
            out.push_str(&format!("Key_{i}={}\n", c.key));
            out.push_str(&format!("Chan_{i}={}\n", c.chan));
            out.push_str(&format!(
                "Col_{i}={:02X}{:02X}{:02X}\n",
                c.color.0, c.color.1, c.color.2
            ));
        }
    }
    out
}

/// Create a new blank WTN with N boards, all cells chan=0/key=0/black.
pub fn new_blank(edo: i32, boards: u8, pitch_offset: i32) -> Wtn {
    let mut bs = HashMap::new();
    for b in 0..boards {
        bs.insert(b, vec![WtnCell::default(); WTN_CELLS_PER_BOARD]);
    }
    Wtn { edo: Some(edo), pitch_offset, boards: bs }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_minimal_wtn() {
        let text = "\
Edo=31
PitchOffset=0

[Board0]
Key_0=60
Chan_0=1
Col_0=FFDD00
Key_1=62
Chan_1=1
Col_1=7CFF6B
";
        let w = parse_wtn(text).unwrap();
        assert_eq!(w.edo, Some(31));
        assert_eq!(w.boards.len(), 1);
        let c0 = w.cell(0, 0).unwrap();
        assert_eq!(c0.key, 60);
        assert_eq!(c0.chan, 1);
        assert_eq!(c0.color, (0xFF, 0xDD, 0x00));
    }

    #[test]
    fn accepts_argb_colors() {
        let text = "\
[Board0]
Key_0=60
Chan_0=1
Col_0=FFFFDD00
";
        let w = parse_wtn(text).unwrap();
        assert_eq!(w.cell(0, 0).unwrap().color, (0xFF, 0xDD, 0x00));
    }

    #[test]
    fn round_trips() {
        let w0 = new_blank(31, 2, 0);
        let s = write_wtn(&w0);
        let w1 = parse_wtn(&s).unwrap();
        assert_eq!(w0.edo, w1.edo);
        assert_eq!(w0.pitch_offset, w1.pitch_offset);
        assert_eq!(w0.boards.len(), w1.boards.len());
    }
}
