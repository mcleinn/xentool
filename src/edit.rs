//! `xentool edit <file>` — web-based visual layout editor for every layout kind
//! (`.xtn` Exquis, `.wtn` Wooting, `.otn` / `.ltn` openxen).
//!
//! Starts a local HTTP server serving an embedded single-page app.
//! GET  /               → index.html
//! GET  /editor.css     → css
//! GET  /editor-core.js → geometry and generator arithmetic (no DOM; also run by the tests)
//! GET  /editor.js      → the page
//! GET  /api/layout     → current layout JSON
//! POST /api/layout     → save layout (body = JSON)
//! GET  /api/geometry   → the keyboard of the open file (`GeometryDto`)
//! GET  /api/files      → layout files of the same kind
//! POST /api/load       → switch to another layout file
//! POST /api/import     → take another layout file over (`ImportResponse`)
//!
//! The editor is device agnostic. The keyboard comes from the central definition:
//! `LayoutKind::of_path` gives the kind of the open file and `geometry::layout_geometry`
//! its keyboard, which reaches the page as one description (`GeometryDto`: boards, keys with
//! their place in a shared hex lattice, optional keycap outlines, orientation and
//! directions). Neither this file nor the page names a keyboard; a further keyboard needs
//! a `LayoutKind` and an arm in `layout_geometry`.
//!
//! All kinds share one file dialect (`src/layout_text.rs`). A save changes only the lines
//! whose value differs, so nothing else in the file moves (see `LayoutDoc::apply`).

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::{Context, Result};
use rouille::Response;
use serde::{Deserialize, Serialize};

use crate::api::Directions;
use crate::geometry::{self, GeometryInfo, Orientation};
use crate::layout_text::{DirectImport, LayoutDoc, LayoutDto, board_name, direct_import, parse_dto};
use crate::layouts::LayoutKind;

const INDEX_HTML: &str = include_str!("../assets/editor.html");
const EDITOR_CSS: &str = include_str!("../assets/editor.css");
const EDITOR_JS: &str = include_str!("../assets/editor.js");
const EDITOR_CORE_JS: &str = include_str!("../assets/editor-core.js");

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct KeyDto {
    /// Index of the key in the layout file (`Key_n`).
    pub key: u8,
    /// Position in the hex lattice shared by all boards.
    pub x: i32,
    pub y: i32,
    /// Keycap as [x, y, w, h] in a plane shared by all boards; absent: a hexagon at (x, y).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub outline: Option<[f32; 4]>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct BoardDto {
    pub index: u8,
    /// Name of the board in the layout JSON ("board0").
    pub name: String,
    pub keys: Vec<KeyDto>,
}

/// The keyboard of the open file, as the page gets it. Built from a `GeometryInfo` alone.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct GeometryDto {
    /// Layout kind of the open file by its usual extension ("xtn", "wtn", "otn").
    pub kind: String,
    /// Name of the central geometry ("exquis", "wooting", "lumatone").
    pub geometry: String,
    pub description: String,
    /// Basename of the currently-open layout file, e.g. "edo31.wtn".
    pub current_file: String,
    /// "YRightXUp" (row 0 at the bottom) or "YRightXDown" (row 0 at the top).
    pub orientation: Orientation,
    /// Lattice offsets of the six neighbours as the player sees them.
    pub directions: Directions,
    pub neighbor_deltas: Vec<[i32; 2]>,
    pub board_shift: Option<[i32; 2]>,
    pub boards: Vec<BoardDto>,
    /// Extensions of layout files that can be imported.
    pub import_extensions: Vec<String>,
}

pub fn geometry_dto(info: &GeometryInfo, kind: LayoutKind, current_file: &str) -> GeometryDto {
    let boards = info
        .boards
        .iter()
        .enumerate()
        .map(|(b, keys)| {
            let outlines = info.outlines.get(b);
            BoardDto {
                index: b as u8,
                name: board_name(b),
                keys: keys
                    .iter()
                    .map(|&(key, x, y)| KeyDto {
                        key,
                        x,
                        y,
                        outline: outlines.and_then(|o| o.iter().find(|o| o.0 == key)).map(|o| o.1),
                    })
                    .collect(),
            }
        })
        .collect();
    GeometryDto {
        kind: kind.ext().to_string(),
        geometry: info.name.to_string(),
        description: info.description.to_string(),
        current_file: current_file.to_string(),
        orientation: info.orientation,
        directions: Directions::of(info.orientation),
        neighbor_deltas: geometry::HEX_NEIGHBOR_DELTAS.iter().map(|&(a, b)| [a, b]).collect(),
        board_shift: info.board_shift.map(|(a, b)| [a, b]),
        boards,
        import_extensions: LayoutKind::ALL.iter().flat_map(|k| k.extensions()).map(|e| e.to_string()).collect(),
    }
}

#[derive(Debug, Serialize)]
struct FilesDto {
    kind: String,
    current: String,
    files: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct LoadRequest {
    name: String,
}

#[derive(Debug, Deserialize)]
pub struct ImportRequest {
    pub content: String,
    /// Extension of the imported file ("ltn", "otn", "wtn", "xtn").
    pub kind: String,
}

#[derive(Debug, Serialize, PartialEq)]
pub struct ImportedPad {
    /// Source board index (0-based).
    pub src_board: u8,
    /// Source pad index within the board.
    pub src_pad: u8,
    /// Hex coordinates in the unified lattice.
    pub x: i32,
    pub y: i32,
    pub key: u8,
    pub chan: u8,
    pub color: String,
}

/// What an import gives the page.
#[derive(Debug, Serialize)]
#[serde(tag = "mode", rename_all = "lowercase")]
pub enum ImportResponse {
    /// The imported file is for the same geometry as the open one: its keys are taken over
    /// one to one (board n → board n, key k → key k), nothing to place.
    Direct(DirectImport),
    /// Another geometry: the keys with their lattice positions, for the page to lay over
    /// the open keyboard, move and turn.
    Place { pads: Vec<ImportedPad>, edo: Option<i32>, pitch_offset: i32 },
}

fn basename_of(p: &Path) -> String {
    p.file_name().and_then(|s| s.to_str()).unwrap_or("").to_string()
}

fn kind_of_extension(ext: &str) -> LayoutKind {
    LayoutKind::of_path(Path::new(&format!("x.{ext}")))
}

/// Import of `req` into a layout drawn on `target`.
pub fn import_request(req: &ImportRequest, target: &GeometryInfo) -> Result<ImportResponse> {
    let src_kind = kind_of_extension(&req.kind);
    let src_default = geometry::layout_geometry(src_kind, geometry::default_board_count(src_kind));
    if src_default.name == target.name {
        return Ok(ImportResponse::Direct(direct_import(&req.content, target)?));
    }

    let layout = parse_dto(&req.content)?;
    let source = geometry::layout_geometry(src_kind, layout.board_count().unwrap_or(1));
    // Where the two keyboards count their rows in opposite directions, mirror the source rows
    // (`max_x - x`) so that its far side stays the far side. Hex adjacency is preserved.
    let flip_x = source.orientation != target.orientation;
    let max_x = source.boards.iter().flatten().map(|k| k.1).max().unwrap_or(0);

    let mut pads = Vec::new();
    for (b_idx, board_geom) in source.boards.iter().enumerate() {
        let Some(board) = layout.boards.get(&board_name(b_idx)) else { continue };
        for (key_idx, x, y) in board_geom.iter() {
            if let Some(p) = board.get(key_idx) {
                pads.push(ImportedPad {
                    src_board: b_idx as u8,
                    src_pad: *key_idx,
                    x: if flip_x { max_x - *x } else { *x },
                    y: *y,
                    key: p.key,
                    chan: p.chan,
                    color: p.color.clone(),
                });
            }
        }
    }
    Ok(ImportResponse::Place { pads, edo: layout.edo, pitch_offset: layout.pitch_offset })
}

/// The open layout file. The HTTP handlers and the tests go through these methods.
pub struct Editor {
    path: PathBuf,
    layout: LayoutDto,
}

impl Editor {
    /// Open `path`. A file that does not exist yet starts as a blank layout of the kind its
    /// extension says (all keys off, as `xentool new` makes it) and is created by the first save.
    pub fn open(path: PathBuf) -> Result<Self> {
        let layout = if path.exists() {
            let content = std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
            parse_dto(&content).with_context(|| format!("parsing {}", path.display()))?
        } else {
            let kind = LayoutKind::of_path(&path);
            LayoutDto::blank(&geometry::layout_geometry(kind, geometry::default_board_count(kind)), Some(12), 0)
        };
        Ok(Editor { path, layout })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn kind(&self) -> LayoutKind {
        LayoutKind::of_path(&self.path)
    }

    pub fn layout(&self) -> &LayoutDto {
        &self.layout
    }

    /// The keyboard of the open file: the central geometry of its kind with as many boards
    /// as the file has.
    pub fn geometry_info(&self) -> GeometryInfo {
        let kind = self.kind();
        geometry::layout_geometry(kind, self.layout.board_count().unwrap_or_else(|| geometry::default_board_count(kind)))
    }

    pub fn geometry(&self) -> GeometryDto {
        geometry_dto(&self.geometry_info(), self.kind(), &basename_of(&self.path))
    }

    /// Write `dto` into the file as it is on disk now: lines whose value differs change,
    /// everything else (unknown entries, comments, notation, line ends) stays.
    pub fn save(&mut self, dto: &LayoutDto) -> Result<()> {
        let current = if self.path.exists() {
            std::fs::read_to_string(&self.path).with_context(|| format!("reading {}", self.path.display()))?
        } else {
            String::new()
        };
        let text = LayoutDoc::parse(&current)?.apply(dto)?;
        let saved = parse_dto(&text).context("the layout to save does not parse")?;
        if text != current {
            if let Some(parent) = self.path.parent().filter(|p| !p.as_os_str().is_empty()) {
                std::fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
            }
            std::fs::write(&self.path, &text).with_context(|| format!("writing {}", self.path.display()))?;
        }
        self.layout = saved;
        Ok(())
    }

    pub fn import(&self, req: &ImportRequest) -> Result<ImportResponse> {
        import_request(req, &self.geometry_info())
    }

    fn files(&self) -> FilesDto {
        let kind = self.kind();
        let files = crate::layouts::list_layouts(kind)
            .map(|v| v.iter().map(|p| basename_of(p)).collect())
            .unwrap_or_default();
        FilesDto { kind: kind.ext().to_string(), current: basename_of(&self.path), files }
    }
}

fn json_body<T: serde::de::DeserializeOwned>(request: &rouille::Request) -> Result<T, Response> {
    rouille::input::json_input(request).map_err(|e| Response::text(format!("invalid json: {e}")).with_status_code(400))
}

/// Run the editor HTTP server on the given port. Blocks forever.
pub fn run_edit_server(file: PathBuf, port: u16, open_browser: bool) -> Result<()> {
    let editor = Editor::open(file)?;
    let url = format!("http://localhost:{port}");
    println!("xentool edit: serving {} ({} layout, {} geometry)", editor.path().display(), editor.kind().ext(), editor.geometry_info().name);
    println!("xentool edit: listening on {url}");
    // Mutable so `POST /api/load` can swap to a different layout at runtime.
    let editor = std::sync::Arc::new(Mutex::new(editor));

    if open_browser {
        let _ = open::that(&url);
    }

    let addr = format!("0.0.0.0:{port}");
    rouille::start_server(addr, move |request| {
        let url = request.url();
        let method = request.method();
        match (method, url.as_str()) {
            ("GET", "/") => Response::html(INDEX_HTML),
            ("GET", "/editor.css") => Response::from_data("text/css; charset=utf-8", EDITOR_CSS),
            ("GET", "/editor.js") => Response::from_data("application/javascript; charset=utf-8", EDITOR_JS),
            ("GET", "/editor-core.js") => Response::from_data("application/javascript; charset=utf-8", EDITOR_CORE_JS),
            ("GET", "/api/geometry") => Response::json(&editor.lock().unwrap().geometry()),
            ("GET", "/api/layout") => Response::json(editor.lock().unwrap().layout()),
            ("GET", "/api/files") => Response::json(&editor.lock().unwrap().files()),
            ("POST", "/api/load") => {
                let req: LoadRequest = match json_body(request) {
                    Ok(v) => v,
                    Err(r) => return r,
                };
                let new_path = crate::layouts::resolve_layout_path(Path::new(&req.name));
                match Editor::open(new_path) {
                    Ok(e) => {
                        *editor.lock().unwrap() = e;
                        Response::text("loaded")
                    }
                    Err(e) => Response::text(format!("load error: {e:#}")).with_status_code(400),
                }
            }
            ("POST", "/api/layout") => {
                let dto: LayoutDto = match json_body(request) {
                    Ok(v) => v,
                    Err(r) => return r,
                };
                match editor.lock().unwrap().save(&dto) {
                    Ok(()) => Response::text("saved"),
                    Err(e) => Response::text(format!("not saved: {e:#}")).with_status_code(400),
                }
            }
            ("POST", "/api/import") => {
                let req: ImportRequest = match json_body(request) {
                    Ok(v) => v,
                    Err(r) => return r,
                };
                match editor.lock().unwrap().import(&req) {
                    Ok(resp) => Response::json(&resp),
                    Err(e) => Response::text(format!("import error: {e:#}")).with_status_code(400),
                }
            }
            _ => Response::empty_404(),
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout_text::PadDto;
    use crate::wooting::wtn::{Wtn, parse_wtn};
    use std::collections::{BTreeMap, HashMap, HashSet};

    /// Layouts of the openxen build, where this machine has them.
    const REAL_OTN_DIR: &str = r"C:\Dev-Free\openxen\build\layouts";
    /// Lumatone factory layouts, where this machine has them.
    const REAL_LTN_DIR: &str = r"C:\Dev-Free\freexen\xenAssist\static\ltn";

    fn repo(rel: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join(rel)
    }

    /// A fresh directory for one test.
    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("xentool-edit-test-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn files_with(dir: &Path, ext: &str) -> Vec<PathBuf> {
        let mut v: Vec<PathBuf> = std::fs::read_dir(dir)
            .map(|d| d.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|e| e == ext)).collect())
            .unwrap_or_default();
        v.sort();
        v
    }

    /// What the HTTP handler does with a save: the page's JSON → `LayoutDto` → `Editor::save`.
    fn post_layout(editor: &mut Editor, layout: &LayoutDto) -> Result<()> {
        let body = serde_json::to_string(layout).unwrap();
        editor.save(&serde_json::from_str::<LayoutDto>(&body).unwrap())
    }

    /// A five-board Lumatone layout as Lumatone's editor writes it (no Edo, extra entries).
    fn synthetic_ltn(boards: usize) -> String {
        let mut s = String::new();
        for b in 0..boards {
            s.push_str(&format!("[Board{b}]\r\n"));
            for k in 0..56 {
                s.push_str(&format!("Key_{k}={}\r\nChan_{k}={}\r\nCol_{k}={:02x}{:02x}40\r\n", (k * 2 + b) % 128, b + 1, k * 4, b * 50));
                if k == 3 {
                    s.push_str(&format!("KTyp_{k}=4\r\n"));
                }
                if k == 5 {
                    s.push_str(&format!("KTyp_{k}=2\r\nCCInvert_{k}=1\r\n"));
                }
            }
        }
        s.push_str("AfterTouchActive=1\r\nLightOnKeyStrokes=1\r\nVelocityIntrvlTbl=1 2 3 \r\n");
        s
    }

    fn synthetic_otn() -> String {
        format!("Edo=31\r\nPitchOffset=7\r\n\r\n{}", synthetic_ltn(5))
    }

    #[test]
    fn kind_by_extension() {
        for (file, kind, geometry) in [
            ("a.xtn", LayoutKind::Xtn, "exquis"),
            ("a.wtn", LayoutKind::Wtn, "wooting"),
            ("a.otn", LayoutKind::Otn, "lumatone"),
            ("a.ltn", LayoutKind::Otn, "lumatone"),
            ("A.OTN", LayoutKind::Otn, "lumatone"),
        ] {
            // a file that does not exist: the editor starts a new layout of that kind
            let e = Editor::open(temp_dir("kind").join(file)).unwrap();
            assert_eq!(e.kind(), kind, "{file}");
            let g = e.geometry();
            assert_eq!(g.geometry, geometry, "{file}");
            assert_eq!(g.kind, kind.ext());
            assert_eq!(g.boards.len(), geometry::default_board_count(kind) as usize);
            // blank: every key of the keyboard is there and off
            for b in &g.boards {
                let pads = &e.layout().boards[&b.name];
                assert_eq!(pads.len(), b.keys.len());
                assert!(pads.values().all(|p| p.chan == 0));
            }
        }
    }

    fn neighbours(g: &GeometryDto) -> HashMap<(i32, i32), usize> {
        let all: HashSet<(i32, i32)> = g.boards.iter().flat_map(|b| b.keys.iter().map(|k| (k.x, k.y))).collect();
        all.iter().map(|&(x, y)| ((x, y), g.neighbor_deltas.iter().filter(|d| all.contains(&(x + d[0], y + d[1]))).count())).collect()
    }

    #[test]
    fn lumatone_geometry_is_the_central_one() {
        let g = geometry_dto(&geometry::layout_geometry(LayoutKind::Otn, 5), LayoutKind::Otn, "x.otn");
        assert_eq!(g.boards.len(), 5);
        assert!(g.boards.iter().all(|b| b.keys.len() == 56 && b.keys.iter().all(|k| k.outline.is_none())));
        for (b, tuples) in geometry::ltn_boards_tuples().iter().enumerate() {
            let got: Vec<(u8, i32, i32)> = g.boards[b].keys.iter().map(|k| (k.key, k.x, k.y)).collect();
            assert_eq!(&got, tuples);
            assert_eq!(g.boards[b].name, format!("board{b}"));
        }
        let n = neighbours(&g);
        assert_eq!(n.len(), 280, "280 keys at 280 places");
        assert!(n.values().all(|c| (2..=6).contains(c)), "every key has 2..6 neighbours");
        assert_eq!(g.orientation, Orientation::YRightXDown);
        assert_eq!(g.board_shift, Some([2, 12]));
        // row 0 is at the top: one key up and to the right is a row less
        assert_eq!((g.directions.right, g.directions.upper_right), ((0, 2), (-1, 1)));
    }

    #[test]
    fn exquis_and_wooting_geometries_are_the_central_ones() {
        let g = geometry_dto(&geometry::layout_geometry(LayoutKind::Xtn, 4), LayoutKind::Xtn, "x.xtn");
        assert_eq!(g.boards.len(), 4);
        for (b, board) in g.boards.iter().enumerate() {
            let central: Vec<(u8, i32, i32)> = geometry::exquis_board_tuples(b as u8).iter().map(|k| (k.key, k.x, k.y)).collect();
            assert_eq!(board.keys.iter().map(|k| (k.key, k.x, k.y)).collect::<Vec<_>>(), central);
        }
        assert_eq!(neighbours(&g).len(), 4 * 61);
        assert_eq!((g.orientation, g.directions.upper_right), (Orientation::YRightXUp, (1, 1)));

        let g = geometry_dto(&geometry::layout_geometry(LayoutKind::Wtn, 2), LayoutKind::Wtn, "x.wtn");
        assert_eq!(g.boards.len(), 2);
        for (b, board) in g.boards.iter().enumerate() {
            assert_eq!(
                board.keys.iter().map(|k| (k.key, k.x, k.y)).collect::<Vec<_>>(),
                crate::wooting::api_link::board_positions(b as u8, 2)
            );
            assert!(board.keys.iter().all(|k| k.outline.is_some()), "a Wooting key has a keycap outline");
        }
        assert_eq!(neighbours(&g).len(), 2 * 53);
    }

    #[test]
    fn geometry_follows_the_boards_of_the_file() {
        let dir = temp_dir("boards");
        let path = dir.join("two.otn");
        std::fs::write(&path, synthetic_ltn(2)).unwrap();
        assert_eq!(Editor::open(path).unwrap().geometry().boards.len(), 2);
        let path = dir.join("one.wtn");
        std::fs::write(&path, "Edo=12\n[Board0]\nKey_0=1\nChan_0=1\nCol_0=FFFFFF\n").unwrap();
        // a single Wooting stands upright: the positions of the lower board of a pair
        let g = Editor::open(path).unwrap().geometry();
        assert_eq!(g.boards.len(), 1);
        assert_eq!(g.boards[0].keys.len(), 53);
    }

    fn assert_round_trip(src_text: &str, name: &str, dir: &Path) {
        let path = dir.join(name);
        std::fs::write(&path, src_text).unwrap();
        let mut e = Editor::open(path.clone()).unwrap();
        let layout = e.layout().clone();
        post_layout(&mut e, &layout).unwrap();
        let saved = std::fs::read(&path).unwrap();
        assert!(saved == src_text.as_bytes(), "{name}: an unchanged save changed the file");
        assert_eq!(e.layout(), &layout);
    }

    #[test]
    fn unchanged_save_keeps_every_byte_synthetic() {
        let dir = temp_dir("rt-syn");
        assert_round_trip(&synthetic_otn(), "syn.otn", &dir);
        assert_round_trip(&synthetic_ltn(5), "syn.ltn", &dir);
        assert_round_trip("Edo=31\nPitchOffset=0\n\n[Board0]\nKey_0=6\nChan_0=1\nCol_0=FF4CA01C\n# note\nKey_60=1\nChan_60=2\nCol_60=00ff00", "syn.xtn", &dir);
        assert_round_trip("Edo=12\nPitchOffset=0\n\n[Board0]\nKey_55=60\nChan_55=0\nCol_55=FFDD00\n\n[Board1]\nKey_0=1\nChan_0=1\nCol_0=000000\n", "syn.wtn", &dir);
    }

    #[test]
    fn unchanged_save_keeps_every_byte_real_files() {
        let dir = temp_dir("rt-real");
        let mut n = 0;
        for src in [files_with(Path::new(REAL_OTN_DIR), "otn"), files_with(Path::new(REAL_LTN_DIR), "ltn"), files_with(&repo("xtn"), "xtn"), files_with(&repo("wtn"), "wtn")].concat() {
            // a copy: the originals are never opened for writing
            let text = std::fs::read_to_string(&src).unwrap();
            assert_round_trip(&text, src.file_name().unwrap().to_str().unwrap(), &dir);
            n += 1;
        }
        assert!(n >= 9, "the repository's own .xtn and .wtn files are always there");
        println!("round trip: {n} real files");
    }

    #[test]
    fn editing_one_key_changes_exactly_that_key() {
        let dir = temp_dir("one-key");
        let path = dir.join("syn.otn");
        let before_text = synthetic_otn();
        std::fs::write(&path, &before_text).unwrap();
        let mut e = Editor::open(path.clone()).unwrap();
        let before = e.layout().clone();
        let mut layout = before.clone();
        layout.boards.get_mut("board3").unwrap().insert(17, PadDto { key: 99, chan: 9, color: "A1B2C3".into() });
        post_layout(&mut e, &layout).unwrap();

        let after_text = std::fs::read_to_string(&path).unwrap();
        let changed: Vec<(&str, &str)> = before_text.lines().zip(after_text.lines()).filter(|(a, b)| a != b).collect();
        assert_eq!(changed.len(), 3, "three lines: Key, Chan, Col of one key");
        assert_eq!(changed.iter().map(|c| c.1).collect::<Vec<_>>(), ["Key_17=99", "Chan_17=9", "Col_17=a1b2c3"]);
        assert_eq!(before_text.lines().count(), after_text.lines().count());

        // the same through xentool's own parser of .otn files
        let (a, b) = (parse_wtn(&before_text).unwrap(), parse_wtn(&after_text).unwrap());
        for board in 0..5u8 {
            for k in 0..56usize {
                let (x, y) = (a.cell(board, k).unwrap(), b.cell(board, k).unwrap());
                if (board, k) == (3, 17) {
                    assert_eq!((y.key, y.chan, y.color), (99, 9, (0xA1, 0xB2, 0xC3)));
                } else {
                    assert_eq!((x.key, x.chan, x.color), (y.key, y.chan, y.color), "board {board} key {k}");
                }
            }
        }
        assert_eq!((b.edo, b.pitch_offset), (Some(31), 7));
        // what is no layout entry is still there
        for other in ["KTyp_3=4", "CCInvert_5=1", "AfterTouchActive=1", "VelocityIntrvlTbl=1 2 3 "] {
            assert!(after_text.contains(other), "{other}");
        }
    }

    #[test]
    fn a_key_that_is_off_stays_off_and_can_be_switched_on() {
        let dir = temp_dir("off");
        let path = dir.join("off.otn");
        let text = "Edo=24\nPitchOffset=48\n\n[Board0]\nKey_0=3\nChan_0=1\nCol_0=ff12dfe3\nKey_1=0\nChan_1=0\nCol_1=ff000000\n";
        std::fs::write(&path, text).unwrap();
        let mut e = Editor::open(path.clone()).unwrap();
        assert_eq!(e.layout().boards["board0"][&1].chan, 0);
        let mut layout = e.layout().clone();
        layout.pitch_offset = 50;
        post_layout(&mut e, &layout).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), text.replace("PitchOffset=48", "PitchOffset=50"));
        assert_eq!(parse_wtn(&std::fs::read_to_string(&path).unwrap()).unwrap().cell(0, 1).unwrap().chan, 0);

        layout.boards.get_mut("board0").unwrap().get_mut(&1).unwrap().chan = 2;
        post_layout(&mut e, &layout).unwrap();
        let w = parse_wtn(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!((w.cell(0, 1).unwrap().chan, w.cell(0, 0).unwrap().chan), (2, 1));
        // and off again
        layout.boards.get_mut("board0").unwrap().get_mut(&1).unwrap().chan = 0;
        post_layout(&mut e, &layout).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), text.replace("PitchOffset=48", "PitchOffset=50"));
    }

    #[test]
    fn a_new_file_gets_the_format_of_its_kind() {
        let dir = temp_dir("new");
        let path = dir.join("sub").join("fresh.otn");
        let mut e = Editor::open(path.clone()).unwrap();
        assert!(!path.exists(), "nothing is written before the first save");
        let mut layout = e.layout().clone();
        layout.edo = Some(53);
        layout.boards.get_mut("board4").unwrap().insert(55, PadDto { key: 10, chan: 5, color: "FF8000".into() });
        post_layout(&mut e, &layout).unwrap();
        let w = parse_wtn(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!((w.edo, w.boards.len()), (Some(53), 5));
        let c = w.cell(4, 55).unwrap();
        assert_eq!((c.key, c.chan, c.color), (10, 5, (0xFF, 0x80, 0)));
        assert_eq!(w.boards.values().flatten().filter(|c| c.chan != 0).count(), 1);
        assert_eq!(Editor::open(path).unwrap().layout(), &layout);
    }

    #[test]
    fn a_save_with_an_invalid_value_is_refused_and_writes_nothing() {
        let dir = temp_dir("refuse");
        let path = dir.join("syn.otn");
        std::fs::write(&path, synthetic_otn()).unwrap();
        let mut e = Editor::open(path.clone()).unwrap();
        let mut layout = e.layout().clone();
        layout.boards.get_mut("board0").unwrap().get_mut(&0).unwrap().color = "red".into();
        assert!(post_layout(&mut e, &layout).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), synthetic_otn());
    }

    // --- import ---

    fn import(editor: &Editor, content: &str, kind: &str) -> ImportResponse {
        editor.import(&ImportRequest { content: content.to_string(), kind: kind.to_string() }).unwrap()
    }

    #[test]
    fn a_lumatone_file_is_taken_over_key_by_key_into_an_otn() {
        let dir = temp_dir("imp-ltn");
        let mut e = Editor::open(dir.join("empty.otn")).unwrap();
        let ltn = synthetic_ltn(5);
        let ImportResponse::Direct(imp) = import(&e, &ltn, "ltn") else { panic!("same geometry: no placing") };
        assert_eq!((imp.edo, imp.pitch_offset), (None, None), "a plain .ltn says neither; the open layout's stay");
        assert!(imp.missing_boards.is_empty());
        assert_eq!((imp.disabled_keys, imp.controller_keys), (5, 5));
        assert_eq!(imp.dropped, ["AfterTouchActive", "CCInvert", "KTyp", "LightOnKeyStrokes", "VelocityIntrvlTbl"]);

        let mut layout = e.layout().clone();
        layout.edo = Some(19);
        imp.merge_into(&mut layout);
        post_layout(&mut e, &layout).unwrap();
        let (src, out) = (parse_wtn(&ltn).unwrap(), Wtn::load(e.path()).unwrap());
        assert_eq!((out.edo, out.pitch_offset), (Some(19), 0));
        for board in 0..5u8 {
            for k in 0..56usize {
                let (s, o) = (src.cell(board, k).unwrap(), out.cell(board, k).unwrap());
                assert_eq!((o.key, o.color), (s.key, s.color), "board {board} key {k}");
                // a disabled key (KTyp 4) is off, every other key keeps its channel
                assert_eq!(o.chan, if k == 3 { 0 } else { s.chan }, "board {board} key {k}");
            }
        }
    }

    #[test]
    fn importing_an_otn_into_an_otn_equals_copying() {
        let dir = temp_dir("imp-otn");
        let mut e = Editor::open(dir.join("copy.otn")).unwrap();
        let src = synthetic_otn().replace("KTyp_3=4\r\n", "");
        let ImportResponse::Direct(imp) = import(&e, &src, "otn") else { panic!("same geometry") };
        assert_eq!((imp.edo, imp.pitch_offset), (Some(31), Some(7)));
        let mut layout = e.layout().clone();
        imp.merge_into(&mut layout);
        post_layout(&mut e, &layout).unwrap();
        assert_eq!(e.layout(), &parse_dto(&src).unwrap());
    }

    #[test]
    fn boards_missing_from_a_direct_import_stay() {
        let dir = temp_dir("imp-few");
        let path = dir.join("full.otn");
        std::fs::write(&path, synthetic_otn()).unwrap();
        let e = Editor::open(path).unwrap();
        let ImportResponse::Direct(imp) = import(&e, &synthetic_ltn(2).replace("Chan_0=1", "Chan_0=16"), "ltn") else { panic!() };
        assert_eq!(imp.missing_boards, ["board2", "board3", "board4"]);
        let mut layout = e.layout().clone();
        imp.merge_into(&mut layout);
        assert_eq!(layout.boards["board0"][&0].chan, 16);
        assert_eq!((layout.edo, layout.pitch_offset), (Some(31), 7));
        for b in ["board2", "board3", "board4"] {
            assert_eq!(layout.boards[b], e.layout().boards[b]);
        }
    }

    #[test]
    fn an_import_across_geometries_is_placed() {
        let dir = temp_dir("imp-cross");
        let ltn = synthetic_ltn(5);
        for target in ["t.xtn", "t.wtn"] {
            let e = Editor::open(dir.join(target)).unwrap();
            let ImportResponse::Place { pads, edo, .. } = import(&e, &ltn, "ltn") else { panic!("{target}: other geometry") };
            assert_eq!((pads.len(), edo), (280, None));
            let places: HashSet<(i32, i32)> = pads.iter().map(|p| (p.x, p.y)).collect();
            assert_eq!(places.len(), 280);
        }
        // the Exquis counts its rows upwards, the Lumatone downwards: the rows are mirrored
        let e = Editor::open(dir.join("t.xtn")).unwrap();
        let ImportResponse::Place { pads, .. } = import(&e, &ltn, "ltn") else { panic!() };
        let first = pads.iter().find(|p| (p.src_board, p.src_pad) == (0, 0)).unwrap();
        assert_eq!((first.x, first.y, first.key, first.chan), (18, 0, 0, 1));
        // and an Exquis layout into an openxen one is placed as well
        let e = Editor::open(dir.join("t.otn")).unwrap();
        let xtn = std::fs::read_to_string(repo("xtn/edo31.xtn")).unwrap();
        let ImportResponse::Place { pads, edo, .. } = import(&e, &xtn, "xtn") else { panic!() };
        assert_eq!((pads.len(), edo), (4 * 61, Some(31)));
    }

    #[test]
    fn real_lumatone_files_convert_with_their_pitches() {
        let files = files_with(Path::new(REAL_LTN_DIR), "ltn");
        if files.is_empty() {
            println!("skipped: no Lumatone files in {REAL_LTN_DIR}");
            return;
        }
        let dir = temp_dir("imp-real");
        for (i, src) in files.iter().enumerate() {
            let text = std::fs::read_to_string(src).unwrap();
            let out = dir.join(format!("conv{i}.otn"));
            crate::layout_text::cmd_new_from(out.clone(), src.clone(), 31, 0, false).unwrap();
            let (a, b) = (LayoutDoc::parse(&text).unwrap(), Wtn::load(&out).unwrap());
            assert_eq!(b.edo, Some(31));
            for board in 0..5u8 {
                for k in 0..56u8 {
                    let s = &a.dto.boards[&board_name(board as usize)][&k];
                    let o = b.cell(board, k as usize).unwrap();
                    let disabled = matches!(a.key_types.get(&(board_name(board as usize), k)), Some(0) | Some(4));
                    if disabled {
                        assert_eq!(o.chan, 0);
                    } else {
                        // pitch = (Chan - 1) * Edo + Key + PitchOffset, as the file states Chan and Key
                        assert_eq!((o.chan, o.key), (s.chan, s.key), "{} board {board} key {k}", src.display());
                    }
                    assert_eq!(format!("{:02X}{:02X}{:02X}", o.color.0, o.color.1, o.color.2), s.color);
                }
            }
        }
        println!("converted {} real Lumatone files", files.len());
    }

    // --- the page's generator functions, run under Node ---

    #[derive(Deserialize)]
    struct Verify {
        #[serde(rename = "totalSet")]
        total_set: usize,
        mismatch: Vec<String>,
        missing: Vec<String>,
    }
    #[derive(Deserialize)]
    struct Guess {
        #[serde(rename = "gA")]
        g_a: i32,
        #[serde(rename = "gB")]
        g_b: i32,
    }
    #[derive(Deserialize)]
    struct Repair {
        changed: Vec<String>,
        skipped: Vec<String>,
    }
    #[derive(Deserialize)]
    struct Check {
        guess: Guess,
        verify: Verify,
        repair: Repair,
        layout: LayoutDto,
    }

    /// Run scripts/editor_core_check.js on the open layout; None without Node.
    fn node_check(editor: &Editor, dir: &Path) -> Option<Check> {
        use std::process::Command;
        if Command::new("node").arg("--version").output().map(|o| !o.status.success()).unwrap_or(true) {
            println!("skipped: node is not installed, the page's generator code was not run");
            return None;
        }
        let (g, l) = (dir.join("geometry.json"), dir.join("layout.json"));
        std::fs::write(&g, serde_json::to_string(&editor.geometry()).unwrap()).unwrap();
        std::fs::write(&l, serde_json::to_string(editor.layout()).unwrap()).unwrap();
        let out = Command::new("node").arg(repo("scripts/editor_core_check.js")).arg(&g).arg(&l).output().unwrap();
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        Some(serde_json::from_slice(&out.stdout).expect("output of editor_core_check.js"))
    }

    /// Pitch step to the neighbour in each of three lattice directions, over all keys.
    fn steps(text: &str) -> (usize, BTreeMap<(i32, i32), HashSet<i32>>) {
        let w = parse_wtn(text).unwrap();
        let edo = w.edo.unwrap();
        let mut pitch = HashMap::new();
        for (b, tuples) in geometry::ltn_boards_tuples().iter().enumerate() {
            for &(k, x, y) in tuples.iter() {
                let c = w.cell(b as u8, k as usize).unwrap();
                if c.chan != 0 {
                    pitch.insert((x, y), (c.chan as i32 - 1) * edo + c.key as i32 + w.pitch_offset);
                }
            }
        }
        let mut out: BTreeMap<(i32, i32), HashSet<i32>> = BTreeMap::new();
        for d in [(0, 2), (-1, 1), (-1, -1)] {
            for (&(x, y), p) in &pitch {
                if let Some(q) = pitch.get(&(x + d.0, y + d.1)) {
                    out.entry(d).or_default().insert(q - p);
                }
            }
        }
        (pitch.len(), out)
    }

    fn open_copy(src: &Path, dir: &Path) -> Option<Editor> {
        if !src.exists() {
            println!("skipped: {} is not on this machine", src.display());
            return None;
        }
        let path = dir.join(src.file_name().unwrap());
        std::fs::copy(src, &path).unwrap();
        Some(Editor::open(path).unwrap())
    }

    #[test]
    fn generators_fill_the_missing_keys_of_the_24_layout() {
        let dir = temp_dir("gen-24");
        let Some(mut e) = open_copy(&Path::new(REAL_OTN_DIR).join("24ET-isomorphic.otn"), &dir) else { return };
        let Some(c) = node_check(&e, &dir) else { return };
        // one key to the right +4, one key up-right +1 (so up-left -3)
        assert_eq!((c.guess.g_a, c.guess.g_b), (4, 1));
        assert_eq!((c.verify.total_set, c.verify.mismatch.len(), c.verify.missing.len()), (262, 0, 18));
        assert!(c.verify.missing.iter().all(|k| k.starts_with("board4:")));
        // repair fills exactly the missing keys
        assert_eq!(c.repair.changed, c.verify.missing);
        assert!(c.repair.skipped.is_empty());
        let before = e.layout().clone();
        for (name, pads) in &c.layout.boards {
            for (k, pad) in pads {
                let old = &before.boards[name][k];
                if c.verify.missing.contains(&format!("{name}:{k}")) {
                    assert!(pad.chan >= 1 && pad.chan <= 16 && pad.key <= 127);
                    assert_ne!(pad.color, "000000", "{name}:{k} takes the colour of its pitch class");
                } else {
                    assert_eq!(pad, old, "{name}:{k} is left alone");
                }
            }
        }
        // saved through the editor, the file loads in xentool and is isomorphic all over
        let original = std::fs::read_to_string(e.path()).unwrap();
        post_layout(&mut e, &c.layout).unwrap();
        let text = std::fs::read_to_string(e.path()).unwrap();
        let (active, by_direction) = steps(&text);
        assert_eq!(active, 280);
        assert_eq!(by_direction[&(0, 2)], HashSet::from([4]));
        assert_eq!(by_direction[&(-1, 1)], HashSet::from([1]));
        assert_eq!(by_direction[&(-1, -1)], HashSet::from([-3]));
        // 18 keys: their Key, Chan and Col lines, nothing else
        let changed = original.lines().zip(text.lines()).filter(|(a, b)| a != b).count();
        assert!((18..=18 * 3).contains(&changed), "{changed} lines changed");
        assert_eq!(original.lines().count(), text.lines().count());
    }

    #[test]
    fn generators_are_found_in_the_bosanquet_layouts() {
        let dir = temp_dir("gen-bq");
        // (the 31 layout has two keys switched off)
        for (file, g_a, g_b, off) in [("53ET-bosanquet.otn", 9, 5, 0), ("31ET-bosanquet.otn", 5, 2, 2)] {
            let Some(e) = open_copy(&Path::new(REAL_OTN_DIR).join(file), &dir) else { return };
            let Some(c) = node_check(&e, &dir) else { return };
            assert_eq!((c.guess.g_a, c.guess.g_b), (g_a, g_b), "{file}");
            assert_eq!((c.verify.total_set, c.verify.mismatch.len(), c.verify.missing.len()), (280 - off, 0, off), "{file}");
            // those two would sound below Key 0 of channel 1: repair says so and leaves them
            assert!(c.repair.changed.is_empty());
            assert_eq!(c.repair.skipped, c.verify.missing);
            assert_eq!(&c.layout, e.layout());
        }
    }

    #[test]
    fn generators_run_on_every_geometry() {
        let dir = temp_dir("gen-syn");
        // an isomorphic Exquis layout with a hole: Key = 2 * lateral step + 3 * row
        let g = geometry::layout_geometry(LayoutKind::Xtn, 2);
        let mut text = String::from("Edo=31\nPitchOffset=0\n");
        for (b, keys) in g.boards.iter().enumerate() {
            text.push_str(&format!("\n[Board{b}]\n"));
            for &(k, x, y) in keys {
                let pitch = 40 + (y - x) / 2 * 4 + x * 7;
                let chan = if (b, k) == (1, 30) { 0 } else { pitch / 31 + 1 };
                text.push_str(&format!("Key_{k}={}\nChan_{k}={chan}\nCol_{k}={:06X}\n", pitch % 31, (pitch % 31) * 0x010203 + 0x101010));
            }
        }
        let path = dir.join("iso.xtn");
        std::fs::write(&path, &text).unwrap();
        let e = Editor::open(path).unwrap();
        let Some(c) = node_check(&e, &dir) else { return };
        assert_eq!((c.guess.g_a, c.guess.g_b), (4, 7));
        assert_eq!((c.verify.mismatch.len(), c.verify.missing.clone()), (0, vec!["board1:30".to_string()]));
        assert_eq!(c.repair.changed, ["board1:30"]);
        // the filled key: octaves by channel as in the rest of the layout, colour of its pitch class
        let (x, y) = g.boards[1].iter().find(|t| t.0 == 30).map(|t| (t.1, t.2)).unwrap();
        let pitch = 40 + (y - x) / 2 * 4 + x * 7;
        let pad = &c.layout.boards["board1"][&30];
        assert_eq!((pad.chan as i32, pad.key as i32), (pitch / 31 + 1, pitch % 31));
        assert_eq!(pad.color, format!("{:06X}", (pitch % 31) * 0x010203 + 0x101010));
    }
}
