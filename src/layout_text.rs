//! Layout files as text: reading any layout kind into one neutral form and writing edits
//! back without disturbing the rest of the file.
//!
//! `.xtn`, `.wtn` and `.otn` / `.ltn` share one INI dialect: `Edo=` / `PitchOffset=` in the
//! head, `[BoardN]` sections, `Key_n` / `Chan_n` / `Col_n` per key (`Chan_n=0`: the key is
//! off). Lumatone files carry more (`KTyp_n`, `CCInvert_n`, velocity tables, ..).
//!
//! The layout editor (`src/edit.rs`) works on a `LayoutDto`. Saving does not rewrite the
//! file from it: `LayoutDoc::apply` compares the new values with those in the text and
//! changes only the lines whose value differs, so a save without changes gives the same
//! bytes, and line ends, colour notation (`ff12dfe3` / `FF12DFE3` / `12DFE3`), comments and
//! every entry this module does not know stay as they are. Keys or boards that the new
//! layout does not mention are left alone, never deleted.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::geometry::GeometryInfo;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PadDto {
    pub key: u8,
    /// 1..16; 0: the key is off.
    pub chan: u8,
    /// 8-bit hex RGB, e.g. "FF0000"
    pub color: String,
}

impl PadDto {
    pub fn off() -> Self {
        PadDto { key: 0, chan: 0, color: "000000".to_string() }
    }
}

pub type BoardsDto = BTreeMap<String, BTreeMap<u8, PadDto>>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LayoutDto {
    pub edo: Option<i32>,
    pub pitch_offset: i32,
    /// Keyed by board name like "board0".
    pub boards: BoardsDto,
}

pub fn board_name(index: usize) -> String {
    format!("board{index}")
}

pub fn board_index(name: &str) -> Option<usize> {
    name.strip_prefix("board")?.parse().ok()
}

impl LayoutDto {
    /// Number of boards the layout spans (highest `boardN` + 1), None without boards.
    pub fn board_count(&self) -> Option<u8> {
        self.boards.keys().filter_map(|n| board_index(n)).max().map(|m| (m + 1).min(255) as u8)
    }

    /// A layout with every key of `geometry` switched off, as `xentool new` makes it.
    pub fn blank(geometry: &GeometryInfo, edo: Option<i32>, pitch_offset: i32) -> Self {
        let boards = geometry
            .boards
            .iter()
            .enumerate()
            .map(|(b, keys)| (board_name(b), keys.iter().map(|k| (k.0, PadDto::off())).collect()))
            .collect();
        LayoutDto { edo, pitch_offset, boards }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Field {
    Key,
    Chan,
    Col,
}

impl Field {
    fn name(self) -> &'static str {
        match self {
            Field::Key => "Key",
            Field::Chan => "Chan",
            Field::Col => "Col",
        }
    }
}

struct Line {
    text: String,
    eol: &'static str,
}

/// A layout file: its lines, the layout they describe, and where each value stands.
pub struct LayoutDoc {
    lines: Vec<Line>,
    pub dto: LayoutDto,
    /// Lumatone key types (`KTyp_n`) by (board name, key).
    pub key_types: HashMap<(String, u8), i32>,
    /// Names of entries that are not part of the layout (`KTyp`, `AfterTouchActive`, ..).
    pub other_entries: BTreeSet<String>,
    pub has_pitch_offset: bool,
    eol: &'static str,
    edo_line: Option<usize>,
    pitch_offset_line: Option<usize>,
    section_line: HashMap<String, usize>,
    field_line: HashMap<(String, u8, Field), usize>,
    /// Last line of a key, and of any key of a board.
    pad_last: HashMap<(String, u8), usize>,
    board_last: HashMap<String, usize>,
    last_pad_line: Option<usize>,
    /// Notation of the first colour in the file: (alpha prefix, lowercase).
    col_prefix: Option<String>,
    col_lower: Option<bool>,
}

fn value_of(line: &str) -> Option<(&str, &str)> {
    let (k, v) = line.split_once('=')?;
    Some((k.trim(), v.trim()))
}

fn replace_value(line: &str, new: &str) -> String {
    let Some(eq) = line.find('=') else { return line.to_string() };
    let rest = &line[eq + 1..];
    let lead = rest.len() - rest.trim_start().len();
    let tail = &rest[rest.trim_end().len()..];
    format!("{}{new}{tail}", &line[..eq + 1 + lead])
}

/// `RRGGBB` (uppercase) of a colour value; Lumatone's `AARRGGBB` loses the alpha byte.
fn normal_color(v: &str) -> String {
    let upper = v.to_uppercase();
    if upper.len() == 8 && upper.is_ascii() { upper[2..].to_string() } else { upper }
}

fn letter_case(v: &str) -> Option<bool> {
    if v.bytes().any(|b| b.is_ascii_lowercase()) {
        Some(true)
    } else if v.bytes().any(|b| b.is_ascii_uppercase()) {
        Some(false)
    } else {
        None
    }
}

fn is_rgb(s: &str) -> bool {
    s.len() == 6 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

impl LayoutDoc {
    pub fn parse(content: &str) -> Result<Self> {
        let mut doc = LayoutDoc {
            lines: Vec::new(),
            dto: LayoutDto { edo: None, pitch_offset: 0, boards: BTreeMap::new() },
            key_types: HashMap::new(),
            other_entries: BTreeSet::new(),
            has_pitch_offset: false,
            eol: "\n",
            edo_line: None,
            pitch_offset_line: None,
            section_line: HashMap::new(),
            field_line: HashMap::new(),
            pad_last: HashMap::new(),
            board_last: HashMap::new(),
            last_pad_line: None,
            col_prefix: None,
            col_lower: None,
        };
        for raw in content.split_inclusive('\n') {
            let (text, eol) = if let Some(t) = raw.strip_suffix("\r\n") {
                (t, "\r\n")
            } else if let Some(t) = raw.strip_suffix('\n') {
                (t, "\n")
            } else {
                (raw, "")
            };
            doc.lines.push(Line { text: text.to_string(), eol });
        }
        if let Some(first) = doc.lines.first().filter(|l| !l.eol.is_empty()) {
            doc.eol = first.eol;
        }

        let mut current: Option<String> = None;
        for i in 0..doc.lines.len() {
            let line = doc.lines[i].text.trim().to_string();
            let n = i + 1;
            if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
                continue;
            }
            if line.starts_with('[') && line.ends_with(']') {
                let name = line[1..line.len() - 1].trim().to_lowercase();
                doc.dto.boards.entry(name.clone()).or_default();
                doc.section_line.insert(name.clone(), i);
                current = Some(name);
                continue;
            }
            let Some((k, v)) = value_of(&line) else {
                bail!("line {n}: expected K=V");
            };
            let Some(board) = current.clone() else {
                match k {
                    "Edo" => {
                        doc.dto.edo = Some(v.parse().with_context(|| format!("line {n}: bad Edo"))?);
                        doc.edo_line = Some(i);
                    }
                    "PitchOffset" => {
                        doc.dto.pitch_offset = v.parse().with_context(|| format!("line {n}: bad PitchOffset"))?;
                        doc.pitch_offset_line = Some(i);
                        doc.has_pitch_offset = true;
                    }
                    _ => {
                        doc.other_entries.insert(k.to_string());
                    }
                }
                continue;
            };
            let indexed = k.rsplit_once('_').and_then(|(p, idx)| Some((p, idx.parse::<u8>().ok().filter(|i| *i <= 127)?)));
            let field = match indexed {
                Some(("Key", _)) => Field::Key,
                Some(("Chan", _)) => Field::Chan,
                Some(("Col", _)) => Field::Col,
                Some(("KTyp", idx)) => {
                    doc.key_types.insert((board, idx), v.parse().with_context(|| format!("line {n}: bad KTyp"))?);
                    doc.other_entries.insert("KTyp".to_string());
                    continue;
                }
                Some((p, _)) => {
                    doc.other_entries.insert(p.to_string());
                    continue;
                }
                None => {
                    doc.other_entries.insert(k.to_string());
                    continue;
                }
            };
            let idx = indexed.unwrap().1;
            let pad = doc
                .dto
                .boards
                .get_mut(&board)
                .unwrap()
                .entry(idx)
                .or_insert(PadDto { key: idx, chan: 1, color: "000000".to_string() });
            match field {
                Field::Key => pad.key = v.parse().with_context(|| format!("line {n}: bad Key"))?,
                Field::Chan => pad.chan = v.parse().with_context(|| format!("line {n}: bad Chan"))?,
                Field::Col => {
                    pad.color = normal_color(v);
                    if doc.col_prefix.is_none() && v.is_ascii() && v.len() >= 6 {
                        doc.col_prefix = Some(v[..v.len() - 6].to_string());
                    }
                    if doc.col_lower.is_none() {
                        doc.col_lower = letter_case(v);
                    }
                }
            }
            doc.field_line.insert((board.clone(), idx, field), i);
            doc.pad_last.insert((board.clone(), idx), i);
            doc.board_last.insert(board, i);
            doc.last_pad_line = Some(i);
        }
        Ok(doc)
    }

    /// A colour in the notation of `like` (the value it replaces) or of the file.
    fn styled_color(&self, rgb: &str, like: Option<&str>) -> String {
        let file_prefix = self.col_prefix.clone().unwrap_or_default();
        let file_lower = self.col_lower.unwrap_or(false);
        let (prefix, lower) = match like {
            Some(raw) if raw.is_ascii() && raw.len() >= 6 => {
                (raw[..raw.len() - 6].to_string(), letter_case(raw).unwrap_or(file_lower))
            }
            _ => (file_prefix, file_lower),
        };
        let rgb = if lower { rgb.to_lowercase() } else { rgb.to_uppercase() };
        format!("{prefix}{rgb}")
    }

    /// The text with the values of `new` in it. Only lines whose value differs change; a
    /// value without a line gets one (next to the key's other lines, a new key at the end of
    /// its board, a new board after the last board). Refuses a changed key with an invalid
    /// Key, Chan or colour, so that a save can not write a file that does not load.
    pub fn apply(&self, new: &LayoutDto) -> Result<String> {
        let old = &self.dto;
        for (board, pads) in &new.boards {
            for (idx, pad) in pads {
                if old.boards.get(board).and_then(|b| b.get(idx)) == Some(pad) {
                    continue;
                }
                if pad.key > 127 || pad.chan > 16 || !is_rgb(&pad.color) || *idx > 127 {
                    bail!("{board} key {idx}: invalid values (Key {}, Chan {}, Col {})", pad.key, pad.chan, pad.color);
                }
            }
        }
        if self.lines.iter().all(|l| l.text.trim().is_empty()) {
            return Ok(write_dto_string(new));
        }

        let mut text: Vec<Option<String>> = self.lines.iter().map(|l| Some(l.text.clone())).collect();
        // lines to put in before line `k` (k = number of lines: at the end)
        let mut before: BTreeMap<usize, Vec<String>> = BTreeMap::new();

        match (old.edo, new.edo) {
            (a, b) if a == b => {}
            (_, Some(v)) => match self.edo_line {
                Some(i) => text[i] = Some(replace_value(&self.lines[i].text, &v.to_string())),
                None => before.entry(0).or_default().push(format!("Edo={v}")),
            },
            (_, None) => {
                if let Some(i) = self.edo_line {
                    text[i] = None;
                }
            }
        }
        if old.pitch_offset != new.pitch_offset {
            match self.pitch_offset_line {
                Some(i) => text[i] = Some(replace_value(&self.lines[i].text, &new.pitch_offset.to_string())),
                None => before
                    .entry(self.edo_line.map(|i| i + 1).unwrap_or(0))
                    .or_default()
                    .push(format!("PitchOffset={}", new.pitch_offset)),
            }
        }

        let mut names: Vec<&String> = new.boards.keys().collect();
        names.sort_by_key(|n| (board_index(n).unwrap_or(usize::MAX), n.to_string()));
        // boards that are in the file first: their additions go in front of new boards
        names.sort_by_key(|n| !self.section_line.contains_key(*n));
        let end_of_boards = self.last_pad_line.map(|i| i + 1).unwrap_or(self.lines.len());
        for name in names {
            let section = self.section_line.get(name).copied();
            let mut added: Vec<String> = Vec::new();
            for (idx, pad) in &new.boards[name] {
                let old_pad = old.boards.get(name).and_then(|b| b.get(idx));
                if old_pad == Some(pad) {
                    continue;
                }
                for field in [Field::Key, Field::Chan, Field::Col] {
                    let changed = match (field, old_pad) {
                        (_, None) => true,
                        (Field::Key, Some(o)) => o.key != pad.key,
                        (Field::Chan, Some(o)) => o.chan != pad.chan,
                        (Field::Col, Some(o)) => o.color != pad.color,
                    };
                    if !changed {
                        continue;
                    }
                    let line = self.field_line.get(&(name.clone(), *idx, field)).copied();
                    let old_raw = line.and_then(|i| value_of(&self.lines[i].text)).map(|kv| kv.1);
                    let value = match field {
                        Field::Key => pad.key.to_string(),
                        Field::Chan => pad.chan.to_string(),
                        Field::Col => self.styled_color(&pad.color, old_raw),
                    };
                    match (line, self.pad_last.get(&(name.clone(), *idx))) {
                        (Some(i), _) => text[i] = Some(replace_value(&self.lines[i].text, &value)),
                        (None, Some(&last)) => {
                            before.entry(last + 1).or_default().push(format!("{}_{idx}={value}", field.name()))
                        }
                        (None, None) => added.push(format!("{}_{idx}={value}", field.name())),
                    }
                }
            }
            match section {
                Some(header) => {
                    if !added.is_empty() {
                        let at = self.board_last.get(name).copied().unwrap_or(header) + 1;
                        before.entry(at).or_default().extend(added);
                    }
                }
                None => {
                    let slot = before.entry(end_of_boards).or_default();
                    let prev_blank = end_of_boards == 0 || self.lines[end_of_boards - 1].text.trim().is_empty();
                    if !slot.is_empty() || !prev_blank {
                        slot.push(String::new());
                    }
                    let header = match name.strip_prefix("board") {
                        Some(rest) => format!("Board{rest}"),
                        None => name.to_string(),
                    };
                    slot.push(format!("[{header}]"));
                    slot.extend(added);
                }
            }
        }

        let mut out: Vec<(String, &str, bool)> = Vec::new();
        for k in 0..=self.lines.len() {
            for l in before.remove(&k).unwrap_or_default() {
                out.push((l, self.eol, true));
            }
            if let Some(Some(t)) = text.get(k) {
                out.push((t.clone(), self.lines[k].eol, false));
            }
        }
        let n = out.len();
        let mut s = String::new();
        for (i, (t, eol, _)) in out.iter().enumerate() {
            s.push_str(t);
            s.push_str(if eol.is_empty() && i + 1 < n { self.eol } else { eol });
        }
        Ok(s)
    }
}

/// Parse a layout file of any kind. Colours come out as `RRGGBB`.
pub fn parse_dto(content: &str) -> Result<LayoutDto> {
    Ok(LayoutDoc::parse(content)?.dto)
}

/// A layout as a new file: head, then every board with its keys in ascending order.
pub fn write_dto_string(dto: &LayoutDto) -> String {
    let mut out = String::new();
    if let Some(edo) = dto.edo {
        out.push_str(&format!("Edo={edo}\n"));
    }
    out.push_str(&format!("PitchOffset={}\n\n", dto.pitch_offset));

    let mut names: Vec<&String> = dto.boards.keys().collect();
    names.sort_by_key(|n| (board_index(n).unwrap_or(usize::MAX), n.to_string()));
    for (i, name) in names.iter().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        let header = match name.strip_prefix("board") {
            Some(rest) => format!("Board{rest}"),
            None => name.to_string(),
        };
        out.push_str(&format!("[{header}]\n"));
        for (idx, p) in &dto.boards[*name] {
            out.push_str(&format!("Key_{idx}={}\n", p.key));
            out.push_str(&format!("Chan_{idx}={}\n", p.chan));
            out.push_str(&format!("Col_{idx}={}\n", p.color.to_uppercase()));
        }
    }
    out
}

/// A layout file taken over key by key for a keyboard of the same geometry: board n stays
/// board n, key k stays key k.
#[derive(Debug, Clone, Serialize)]
pub struct DirectImport {
    /// Boards of the file that the geometry has, with the geometry's keys only.
    pub boards: BoardsDto,
    /// None: the file does not say.
    pub edo: Option<i32>,
    pub pitch_offset: Option<i32>,
    /// Boards of the geometry that the file does not have.
    pub missing_boards: Vec<String>,
    /// Keys switched off because the file marks them as disabled (Lumatone `KTyp` 0 or 4).
    pub disabled_keys: usize,
    /// Keys the file marks as controller keys (Lumatone `KTyp` 2), taken over as notes.
    pub controller_keys: usize,
    /// Kinds of entries of the file that a layout does not hold.
    pub dropped: Vec<String>,
}

/// Lumatone key types (`LumatoneFirmwareDefinitions.h`): 0 disabledDefault, 1 note,
/// 2 continuous controller, 3 LumaTouch, 4 disabled.
const KEY_TYPE_DISABLED: [i32; 2] = [0, 4];
const KEY_TYPE_CONTROLLER: i32 = 2;

pub fn direct_import(content: &str, geometry: &GeometryInfo) -> Result<DirectImport> {
    let doc = LayoutDoc::parse(content)?;
    let mut out = DirectImport {
        boards: BTreeMap::new(),
        edo: doc.dto.edo,
        pitch_offset: doc.has_pitch_offset.then_some(doc.dto.pitch_offset),
        missing_boards: Vec::new(),
        disabled_keys: 0,
        controller_keys: 0,
        dropped: doc.other_entries.iter().cloned().collect(),
    };
    for (b, keys) in geometry.boards.iter().enumerate() {
        let name = board_name(b);
        let Some(src) = doc.dto.boards.get(&name).filter(|pads| !pads.is_empty()) else {
            out.missing_boards.push(name);
            continue;
        };
        let mut pads = BTreeMap::new();
        for &(k, _, _) in keys {
            let Some(mut pad) = src.get(&k).cloned() else { continue };
            match doc.key_types.get(&(name.clone(), k)) {
                Some(t) if KEY_TYPE_DISABLED.contains(t) => {
                    if pad.chan != 0 {
                        out.disabled_keys += 1;
                    }
                    pad.chan = 0;
                }
                Some(&KEY_TYPE_CONTROLLER) if pad.chan != 0 => out.controller_keys += 1,
                _ => {}
            }
            pads.insert(k, pad);
        }
        out.boards.insert(name, pads);
    }
    Ok(out)
}

impl DirectImport {
    /// Put the imported keys into `layout`; boards and keys the import lacks stay.
    pub fn merge_into(&self, layout: &mut LayoutDto) {
        if let Some(edo) = self.edo {
            layout.edo = Some(edo);
        }
        if let Some(po) = self.pitch_offset {
            layout.pitch_offset = po;
        }
        for (name, pads) in &self.boards {
            layout.boards.entry(name.clone()).or_default().extend(pads.iter().map(|(k, p)| (*k, p.clone())));
        }
    }
}

/// `xentool new OUT --from IN --edo N`: a new layout file with the keys of `from`, a
/// layout for the same keyboard, taken over one to one. Boards the file lacks are blank.
pub fn cmd_new_from(file: std::path::PathBuf, from: std::path::PathBuf, edo: i32, pitch_offset: i32, force: bool) -> Result<()> {
    use crate::geometry::{default_board_count, layout_geometry};
    use crate::layouts::LayoutKind;
    if edo < 1 {
        bail!("edo must be >= 1");
    }
    if file.exists() && !force {
        bail!("{} already exists; pass --force to overwrite", file.display());
    }
    let kind = LayoutKind::of_path(&file);
    let src_kind = LayoutKind::of_path(&from);
    let geometry = layout_geometry(kind, default_board_count(kind));
    let source = layout_geometry(src_kind, default_board_count(src_kind));
    if source.name != geometry.name {
        bail!(
            "{} is a layout for the {} keyboard, {} one for the {}: its keys have to be placed, use the import of `xentool edit`",
            from.display(),
            source.name,
            file.display(),
            geometry.name
        );
    }
    let text = std::fs::read_to_string(&from).with_context(|| format!("reading {}", from.display()))?;
    let imp = direct_import(&text, &geometry)?;
    let pitch_offset = if pitch_offset != 0 { pitch_offset } else { imp.pitch_offset.unwrap_or(0) };
    let mut layout = LayoutDto::blank(&geometry, Some(edo), pitch_offset);
    imp.merge_into(&mut layout);
    layout.edo = Some(edo);
    layout.pitch_offset = pitch_offset;
    if let Some(parent) = file.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    }
    std::fs::write(&file, write_dto_string(&layout)).with_context(|| format!("writing {}", file.display()))?;
    let keys: usize = imp.boards.values().map(|b| b.len()).sum();
    println!("created {} from {}: {keys} keys on {} board(s), edo={edo}, pitch_offset={pitch_offset}", file.display(), from.display(), imp.boards.len());
    if let Some(e) = imp.edo.filter(|e| *e != edo) {
        println!("  note: the file says Edo={e}; --edo {edo} was used");
    }
    if !imp.missing_boards.is_empty() {
        println!("  not in the file, left blank: {}", imp.missing_boards.join(", "));
    }
    if imp.disabled_keys > 0 {
        println!("  {} disabled keys (KTyp 0 or 4) switched off", imp.disabled_keys);
    }
    if imp.controller_keys > 0 {
        println!("  {} controller keys (KTyp 2) taken over as notes", imp.controller_keys);
    }
    if !imp.dropped.is_empty() {
        println!("  not part of a layout, dropped: {}", imp.dropped.join(", "));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SMALL: &str = "Edo=24\r\nPitchOffset=48\r\n\r\n[Board0]\r\nKey_0=3\r\nChan_0=1\r\nCol_0=ff12dfe3\r\nKTyp_0=2\r\nKey_1=7\r\nChan_1=0\r\nCol_1=ff000000\r\n\r\n[Board1]\r\nKey_0=10\r\nChan_0=2\r\nCol_0=ffffffff\r\nAfterTouchActive=1\r\nVelocityIntrvlTbl=1 2 3 \r\n";

    fn dto(text: &str) -> LayoutDto {
        parse_dto(text).unwrap()
    }

    #[test]
    fn unchanged_save_gives_the_same_bytes() {
        for text in [SMALL, "[Board0]\nKey_0=1\nChan_0=1\nCol_0=AABBCC", "# c\nEdo=31\n\n[board0]\n  Key_5 = 9  \n"] {
            let doc = LayoutDoc::parse(text).unwrap();
            assert_eq!(doc.apply(&doc.dto).unwrap(), text);
        }
    }

    #[test]
    fn editing_one_key_changes_only_its_lines_and_keeps_the_notation() {
        let doc = LayoutDoc::parse(SMALL).unwrap();
        let mut new = doc.dto.clone();
        let pad = new.boards.get_mut("board0").unwrap().get_mut(&1).unwrap();
        assert_eq!(pad.chan, 0, "an off key is read as off");
        pad.chan = 3;
        pad.color = "0A0B0C".to_string();
        let out = doc.apply(&new).unwrap();
        let expected = SMALL.replace("Chan_1=0", "Chan_1=3").replace("Col_1=ff000000", "Col_1=ff0a0b0c");
        assert_eq!(out, expected);
        assert_eq!(dto(&out), new);
    }

    #[test]
    fn head_values_are_changed_added_and_removed() {
        let doc = LayoutDoc::parse(SMALL).unwrap();
        let mut new = doc.dto.clone();
        new.edo = Some(31);
        new.pitch_offset = -5;
        let out = doc.apply(&new).unwrap();
        assert_eq!(out, SMALL.replace("Edo=24", "Edo=31").replace("PitchOffset=48", "PitchOffset=-5"));

        let plain = "[Board0]\nKey_0=1\nChan_0=1\nCol_0=AABBCC\n";
        let doc = LayoutDoc::parse(plain).unwrap();
        let mut new = doc.dto.clone();
        new.edo = Some(19);
        new.pitch_offset = 2;
        assert_eq!(doc.apply(&new).unwrap(), format!("Edo=19\nPitchOffset=2\n{plain}"));
    }

    #[test]
    fn new_keys_and_boards_are_added_in_front_of_trailing_settings() {
        let doc = LayoutDoc::parse(SMALL).unwrap();
        let mut new = doc.dto.clone();
        new.boards.get_mut("board1").unwrap().insert(4, PadDto { key: 9, chan: 1, color: "112233".into() });
        new.boards.entry("board2".into()).or_default().insert(0, PadDto { key: 1, chan: 2, color: "ABCDEF".into() });
        let out = doc.apply(&new).unwrap();
        let expected = SMALL.replace(
            "Col_0=ffffffff\r\n",
            "Col_0=ffffffff\r\nKey_4=9\r\nChan_4=1\r\nCol_4=ff112233\r\n\r\n[Board2]\r\nKey_0=1\r\nChan_0=2\r\nCol_0=ffabcdef\r\n",
        );
        assert_eq!(out, expected);
        // the trailing settings now stand behind Board2; the layout is what was asked for
        assert_eq!(dto(&out), new);
    }

    #[test]
    fn keys_missing_from_the_new_layout_stay() {
        let doc = LayoutDoc::parse(SMALL).unwrap();
        let mut new = doc.dto.clone();
        new.boards.remove("board1");
        new.boards.get_mut("board0").unwrap().remove(&0);
        assert_eq!(doc.apply(&new).unwrap(), SMALL);
    }

    #[test]
    fn invalid_values_are_refused() {
        let doc = LayoutDoc::parse(SMALL).unwrap();
        for bad in [PadDto { key: 200, chan: 1, color: "000000".into() }, PadDto { key: 1, chan: 17, color: "000000".into() }, PadDto { key: 1, chan: 1, color: "xyz".into() }] {
            let mut new = doc.dto.clone();
            new.boards.get_mut("board0").unwrap().insert(0, bad);
            assert!(doc.apply(&new).is_err());
        }
    }

    #[test]
    fn an_empty_file_is_written_whole() {
        let doc = LayoutDoc::parse("").unwrap();
        let mut new = doc.dto.clone();
        new.edo = Some(12);
        new.boards.entry("board0".into()).or_default().insert(0, PadDto { key: 60, chan: 1, color: "ff0000".into() });
        let out = doc.apply(&new).unwrap();
        assert_eq!(out, "Edo=12\nPitchOffset=0\n\n[Board0]\nKey_0=60\nChan_0=1\nCol_0=FF0000\n");
    }

    #[test]
    fn other_entries_are_named_and_key_types_read() {
        let doc = LayoutDoc::parse(SMALL).unwrap();
        assert_eq!(doc.other_entries.iter().map(String::as_str).collect::<Vec<_>>(), ["AfterTouchActive", "KTyp", "VelocityIntrvlTbl"]);
        assert_eq!(doc.key_types.get(&("board0".to_string(), 0)), Some(&2));
        // an entry that is not Key/Chan/Col makes no key
        assert_eq!(doc.dto.boards["board1"].len(), 1);
    }
}
