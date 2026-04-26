//! `xentool edit <file.xtn>` — web-based visual editor.
//!
//! Starts a local HTTP server serving an embedded single-page app.
//! GET  /              → index.html
//! GET  /editor.css    → css
//! GET  /editor.js     → js
//! GET  /api/layout    → current layout JSON
//! POST /api/layout    → save layout (body = JSON)
//! GET  /api/geometry  → Exquis/LTN/WTN hex tuples
//! POST /api/import    → parse an uploaded .xtn/.wtn/.ltn file, return pads keyed by hex (x,y)

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;

use anyhow::{Context, Result};
use rouille::Response;
use serde::{Deserialize, Serialize};

use crate::geometry;
use crate::wooting::geometry as woot_geom;

const INDEX_HTML: &str = include_str!("../assets/editor.html");
const EDITOR_CSS: &str = include_str!("../assets/editor.css");
const EDITOR_JS: &str = include_str!("../assets/editor.js");

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PadDto {
    pub key: u8,
    pub chan: u8,
    /// 8-bit hex RGB, e.g. "FF0000"
    pub color: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LayoutDto {
    pub edo: Option<i32>,
    pub pitch_offset: i32,
    /// Keyed by board name like "board0".
    pub boards: HashMap<String, HashMap<u8, PadDto>>,
}

#[derive(Debug, Serialize)]
struct GeometryDto {
    /// "exquis" or "wooting" — which renderer the frontend should use.
    kind: String,
    /// Basename of the currently-open layout file, e.g. "edo31.wtn".
    current_file: String,
    exquis_boards: Vec<Vec<geometry::GridKey>>,
    ltn_boards: Vec<Vec<GridKeyDto>>,
    wtn_boards: Vec<Vec<GridKeyDto>>,
    hex_neighbor_deltas: Vec<[i32; 2]>,
    exquis_board_stride_x: i32,
    exquis_board_stride_y: i32,
    exquis_orientation: String,
    /// For Wooting kind: one key rectangle per playable key (56 slots dense, some empty).
    wooting_keys: Vec<woot_geom::WootingKey>,
    wooting_board_width: f32,
    wooting_board_height: f32,
    /// Horizontal shift (in the same px unit as `wooting_board_width`) applied
    /// to the top rotated board in a combined pair so the lattice lines up.
    wooting_pair_top_x_shift: f32,
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

#[derive(Debug, Serialize)]
struct GridKeyDto {
    key: u8,
    x: i32,
    y: i32,
}

#[derive(Debug, Deserialize)]
struct ImportRequest {
    content: String,
    /// "ltn", "wtn", or "xtn"
    kind: String,
}

#[derive(Debug, Serialize)]
struct ImportedPad {
    /// Source board index (0-based).
    src_board: u8,
    /// Source pad index within the board.
    src_pad: u8,
    /// Hex coordinates in the unified lattice.
    x: i32,
    y: i32,
    key: u8,
    chan: u8,
    color: String,
}

#[derive(Debug, Serialize)]
struct ImportResponse {
    pads: Vec<ImportedPad>,
    edo: Option<i32>,
    pitch_offset: i32,
}

/// Parse an .xtn file directly into a LayoutDto, preserving hex colors verbatim
/// (without going through the lossy 8-bit → 7-bit Color round-trip).
fn parse_dto(content: &str) -> Result<LayoutDto> {
    let mut boards: HashMap<String, HashMap<u8, PadDto>> = HashMap::new();
    let mut current: Option<String> = None;
    let mut edo: Option<i32> = None;
    let mut pitch_offset: i32 = 0;

    for (line_num, line) in content.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        if line.starts_with('[') && line.ends_with(']') {
            current = Some(line[1..line.len() - 1].trim().to_lowercase());
            boards.entry(current.clone().unwrap()).or_default();
            continue;
        }
        let Some((k, v)) = line.split_once('=') else {
            anyhow::bail!("line {}: expected K=V", line_num + 1);
        };
        let k = k.trim();
        let v = v.trim();
        if current.is_none() {
            match k {
                "Edo" => edo = Some(v.parse()?),
                "PitchOffset" => pitch_offset = v.parse()?,
                _ => {}
            }
            continue;
        }
        let Some((prefix, idx_str)) = k.rsplit_once('_') else { continue };
        let Ok(idx) = idx_str.parse::<u8>() else { continue };
        if idx > 127 {
            continue;
        }
        let pads = boards.get_mut(current.as_ref().unwrap()).unwrap();
        let entry = pads.entry(idx).or_insert(PadDto {
            key: idx,
            chan: 1,
            color: "000000".to_string(),
        });
        match prefix {
            "Key" => entry.key = v.parse()?,
            "Chan" => entry.chan = v.parse()?,
            "Col" => {
                // Accept both RRGGBB and AARRGGBB (Lumatone); drop the alpha byte.
                let upper = v.to_uppercase();
                entry.color = if upper.len() == 8 {
                    upper[2..].to_string()
                } else {
                    upper
                };
            }
            _ => {}
        }
    }
    Ok(LayoutDto {
        edo,
        pitch_offset,
        boards,
    })
}

/// Serialize a LayoutDto to the .xtn INI format, preserving the 8-bit hex
/// color strings exactly as received (no 8→7→8 round-trip through Color).
fn write_dto_string(dto: &LayoutDto) -> String {
    let mut out = String::new();
    if let Some(edo) = dto.edo {
        out.push_str(&format!("Edo={edo}\n"));
    }
    out.push_str(&format!("PitchOffset={}\n\n", dto.pitch_offset));

    let mut names: Vec<&String> = dto.boards.keys().collect();
    names.sort_by(|a, b| {
        let na = a.trim_start_matches("board").parse::<u32>().unwrap_or(u32::MAX);
        let nb = b.trim_start_matches("board").parse::<u32>().unwrap_or(u32::MAX);
        na.cmp(&nb).then_with(|| a.cmp(b))
    });

    for (i, name) in names.iter().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        let pads = &dto.boards[*name];
        let header = if let Some(rest) = name.strip_prefix("board") {
            format!("Board{rest}")
        } else {
            name.to_string()
        };
        out.push_str(&format!("[{header}]\n"));
        let mut indices: Vec<u8> = pads.keys().copied().collect();
        indices.sort();
        for idx in indices {
            let p = &pads[&idx];
            out.push_str(&format!("Key_{idx}={}\n", p.key));
            out.push_str(&format!("Chan_{idx}={}\n", p.chan));
            out.push_str(&format!("Col_{idx}={}\n", p.color.to_uppercase()));
        }
    }
    out
}

fn geometry_dto(kind: &str, current_file: &str) -> GeometryDto {
    let exquis_boards: Vec<Vec<geometry::GridKey>> =
        (0u8..4).map(geometry::exquis_board_tuples).collect();
    let ltn_boards: Vec<Vec<GridKeyDto>> = geometry::ltn_boards_tuples()
        .iter()
        .map(|t| t.iter().map(|&(k, x, y)| GridKeyDto { key: k, x, y }).collect())
        .collect();
    let wtn_boards: Vec<Vec<GridKeyDto>> = geometry::wtn_boards_tuples()
        .iter()
        .map(|t| t.iter().map(|&(k, x, y)| GridKeyDto { key: k, x, y }).collect())
        .collect();
    let hex_neighbor_deltas: Vec<[i32; 2]> = geometry::HEX_NEIGHBOR_DELTAS
        .iter()
        .map(|&(a, b)| [a, b])
        .collect();
    GeometryDto {
        kind: kind.to_string(),
        current_file: current_file.to_string(),
        exquis_boards,
        ltn_boards,
        wtn_boards,
        hex_neighbor_deltas,
        exquis_board_stride_x: geometry::EXQUIS_BOARD_STRIDE_X,
        exquis_board_stride_y: geometry::EXQUIS_BOARD_STRIDE_Y,
        exquis_orientation: "YRightXUp".to_string(),
        wooting_keys: woot_geom::keys_60he(),
        wooting_board_width: woot_geom::board_width_px(),
        wooting_board_height: woot_geom::board_height_px(),
        wooting_pair_top_x_shift: woot_geom::pair_top_x_shift_px(),
    }
}

fn import_request(req: ImportRequest) -> Result<ImportResponse> {
    let layout = parse_dto(&req.content)?;

    // Pick geometry based on kind.
    let exquis_tuples: Vec<(u8, i32, i32)>;
    let kind = req.kind.to_lowercase();
    let geom: Vec<&[(u8, i32, i32)]> = match kind.as_str() {
        "ltn" => geometry::ltn_boards_tuples().to_vec(),
        "wtn" => geometry::wtn_boards_tuples().to_vec(),
        _ => {
            exquis_tuples = geometry::exquis_board_tuples(0)
                .into_iter()
                .map(|k| (k.key, k.x, k.y))
                .collect();
            vec![exquis_tuples.as_slice()]
        }
    };

    // LTN and WTN use xenwooting's `YRightXDown` orientation (x grows downward).
    // The Exquis target uses `YRightXUp` (x grows upward). Map source x via
    // `max_x - x` so the source's visual TOP lines up with Exquis's visual TOP
    // (near pad 55+) while preserving relative board positions (boards lower in
    // the source stay lower after the flip). Hex adjacency is preserved.
    let flip_x = matches!(kind.as_str(), "ltn" | "wtn");
    let max_x: i32 = if flip_x {
        geom.iter()
            .flat_map(|g| g.iter().map(|(_, x, _)| *x))
            .max()
            .unwrap_or(0)
    } else {
        0
    };

    let mut pads = Vec::new();
    for (b_idx, board_geom) in geom.iter().enumerate() {
        let board_key = format!("board{b_idx}");
        if let Some(board) = layout.boards.get(&board_key) {
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
    }

    Ok(ImportResponse {
        pads,
        edo: layout.edo,
        pitch_offset: layout.pitch_offset,
    })
}

fn basename_of(p: &std::path::Path) -> String {
    p.file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_string()
}

fn kind_of(p: &std::path::Path) -> &'static str {
    if p.extension()
        .and_then(|e| e.to_str())
        .map(|e| e.eq_ignore_ascii_case("wtn"))
        .unwrap_or(false)
    {
        "wooting"
    } else {
        "exquis"
    }
}

fn load_dto_from_path(p: &std::path::Path) -> Result<LayoutDto> {
    if p.exists() {
        let content = std::fs::read_to_string(p)
            .with_context(|| format!("reading {}", p.display()))?;
        parse_dto(&content)
    } else {
        Ok(LayoutDto {
            edo: Some(12),
            pitch_offset: 0,
            boards: HashMap::new(),
        })
    }
}

/// Run the editor HTTP server on the given port. Blocks forever.
pub fn run_edit_server(file: PathBuf, port: u16, open_browser: bool) -> Result<()> {
    let initial = load_dto_from_path(&file)?;
    let state = std::sync::Arc::new(Mutex::new(initial));
    // Mutable so `POST /api/load` can swap to a different layout at runtime.
    let file_path = std::sync::Arc::new(Mutex::new(file));
    let file_path_for_handler = file_path.clone();
    let state_for_handler = state.clone();

    let url = format!("http://localhost:{port}");
    println!("xentool edit: serving {}", file_path.lock().unwrap().display());
    println!("xentool edit: listening on {url}");

    if open_browser {
        let _ = open::that(&url);
    }

    let addr = format!("0.0.0.0:{port}");
    rouille::start_server(addr, move |request| {
        let state = state_for_handler.clone();
        let file_path = file_path_for_handler.clone();
        let url = request.url();
        let method = request.method();
        match (method, url.as_str()) {
            ("GET", "/") => Response::html(INDEX_HTML),
            ("GET", "/editor.css") => {
                Response::from_data("text/css; charset=utf-8", EDITOR_CSS)
            }
            ("GET", "/editor.js") => {
                Response::from_data("application/javascript; charset=utf-8", EDITOR_JS)
            }
            ("GET", "/api/geometry") => {
                let fp = file_path.lock().unwrap();
                let kind = kind_of(&fp);
                let basename = basename_of(&fp);
                Response::json(&geometry_dto(kind, &basename))
            }
            ("GET", "/api/layout") => {
                let layout = state.lock().unwrap();
                Response::json(&*layout)
            }
            ("GET", "/api/files") => {
                let fp = file_path.lock().unwrap();
                let kind = kind_of(&fp);
                let layout_kind = if kind == "wooting" {
                    crate::layouts::LayoutKind::Wtn
                } else {
                    crate::layouts::LayoutKind::Xtn
                };
                let files = match crate::layouts::list_layouts(layout_kind) {
                    Ok(v) => v
                        .iter()
                        .filter_map(|p| p.file_name().and_then(|s| s.to_str()).map(String::from))
                        .collect::<Vec<_>>(),
                    Err(_) => Vec::new(),
                };
                let current = basename_of(&fp);
                Response::json(&FilesDto {
                    kind: kind.to_string(),
                    current,
                    files,
                })
            }
            ("POST", "/api/load") => {
                let req: LoadRequest = match rouille::input::json_input(request) {
                    Ok(v) => v,
                    Err(e) => {
                        return Response::text(format!("invalid json: {e}"))
                            .with_status_code(400);
                    }
                };
                let new_path = crate::layouts::resolve_layout_path(std::path::Path::new(&req.name));
                let new_dto = match load_dto_from_path(&new_path) {
                    Ok(d) => d,
                    Err(e) => {
                        return Response::text(format!("load error: {e}"))
                            .with_status_code(400);
                    }
                };
                *state.lock().unwrap() = new_dto;
                *file_path.lock().unwrap() = new_path;
                Response::text("loaded")
            }
            ("POST", "/api/layout") => {
                let dto: LayoutDto = match rouille::input::json_input(request) {
                    Ok(v) => v,
                    Err(e) => {
                        return Response::text(format!("invalid json: {e}"))
                            .with_status_code(400);
                    }
                };
                let serialized = write_dto_string(&dto);
                let fp = file_path.lock().unwrap();
                if let Err(e) = std::fs::write(&*fp, &serialized) {
                    return Response::text(format!("failed to write file: {e}"))
                        .with_status_code(500);
                }
                drop(fp);
                *state.lock().unwrap() = dto;
                Response::text("saved")
            }
            ("POST", "/api/import") => {
                let req: ImportRequest = match rouille::input::json_input(request) {
                    Ok(v) => v,
                    Err(e) => {
                        return Response::text(format!("invalid json: {e}"))
                            .with_status_code(400);
                    }
                };
                match import_request(req) {
                    Ok(resp) => Response::json(&resp),
                    Err(e) => Response::text(format!("import error: {e}"))
                        .with_status_code(400),
                }
            }
            _ => Response::empty_404(),
        }
    });
}
