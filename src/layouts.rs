//! Layout file discovery + path resolution.
//!
//! Both the Wooting and Exquis backends keep their layout files in
//! hardcoded relative directories (`./wtn/` for `.wtn`, `./xtn/` for `.xtn`).
//! This module centralises:
//!
//! - **bare-filename resolution**: `edo31.wtn` with no path separator is
//!   interpreted as `./wtn/edo31.wtn`;
//! - **cycle order**: the alphanumeric listing used by the hardware-button
//!   "layout_next" feature and by the editor dropdown;
//! - **next-with-wrap**: the single primitive both callers use.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayoutKind {
    Wtn,
    Xtn,
}

impl LayoutKind {
    pub fn from_extension(p: &Path) -> Option<Self> {
        let ext = p.extension()?.to_str()?.to_ascii_lowercase();
        match ext.as_str() {
            "wtn" => Some(LayoutKind::Wtn),
            "xtn" => Some(LayoutKind::Xtn),
            _ => None,
        }
    }

    pub fn dir(self) -> &'static str {
        match self {
            LayoutKind::Wtn => "wtn",
            LayoutKind::Xtn => "xtn",
        }
    }

    pub fn ext(self) -> &'static str {
        match self {
            LayoutKind::Wtn => "wtn",
            LayoutKind::Xtn => "xtn",
        }
    }
}

fn has_separator(s: &str) -> bool {
    s.contains('/') || s.contains('\\')
}

/// Resolve a user-supplied argument to a layout file.
///
/// - If the input contains a path separator (`/`, `\`) or is absolute, return it unchanged.
/// - If the input has a recognised extension (`.wtn` / `.xtn`), return `./<kind-dir>/<input>`.
/// - Otherwise return unchanged (the caller will eventually surface the error).
pub fn resolve_layout_path(input: &Path) -> PathBuf {
    if input.is_absolute() {
        return input.to_path_buf();
    }
    let s = input.to_string_lossy();
    if has_separator(&s) {
        return input.to_path_buf();
    }
    match LayoutKind::from_extension(input) {
        Some(kind) => PathBuf::from(kind.dir()).join(input),
        None => input.to_path_buf(),
    }
}

/// List all layout files of the given kind, sorted alphanumerically.
///
/// Returns paths like `./wtn/edo12.wtn`, `./wtn/edo19.wtn`, … in ascending
/// order. Lexicographic sort on the filename — for the current `edoNN.*`
/// naming scheme this matches natural order.
pub fn list_layouts(kind: LayoutKind) -> Result<Vec<PathBuf>> {
    let dir = PathBuf::from(kind.dir());
    let target_ext = kind.ext();
    let entries = std::fs::read_dir(&dir)
        .with_context(|| format!("reading {}", dir.display()))?;
    let mut files: Vec<PathBuf> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let Some(ext) = path.extension().and_then(|e| e.to_str()) else {
            continue;
        };
        if !ext.eq_ignore_ascii_case(target_ext) {
            continue;
        }
        if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
            if seen.insert(name.to_string()) {
                files.push(path);
            }
        }
    }
    files.sort_by(|a, b| {
        let an = a.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let bn = b.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        an.cmp(&bn)
    });
    Ok(files)
}

/// Index of `current` in `list` by basename comparison. `None` if absent.
pub fn current_index(list: &[PathBuf], current: &Path) -> Option<usize> {
    let target = current.file_name()?.to_string_lossy().into_owned();
    list.iter().position(|p| {
        p.file_name()
            .map(|n| n.to_string_lossy() == target)
            .unwrap_or(false)
    })
}

/// Return the next path in the list after `current`, wrapping. If `current`
/// isn't in the list, returns the first entry. Errors if the directory is
/// empty.
pub fn next(kind: LayoutKind, current: &Path) -> Result<PathBuf> {
    let list = list_layouts(kind)?;
    if list.is_empty() {
        anyhow::bail!("no .{} files found in {}/", kind.ext(), kind.dir());
    }
    let idx = match current_index(&list, current) {
        Some(i) => (i + 1) % list.len(),
        None => 0,
    };
    Ok(list[idx].clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bare_wtn_goes_to_wtn_dir() {
        let r = resolve_layout_path(Path::new("edo31.wtn"));
        assert_eq!(r, PathBuf::from("wtn").join("edo31.wtn"));
    }

    #[test]
    fn bare_xtn_goes_to_xtn_dir() {
        let r = resolve_layout_path(Path::new("edo31.xtn"));
        assert_eq!(r, PathBuf::from("xtn").join("edo31.xtn"));
    }

    #[test]
    fn slashed_path_passes_through() {
        let r = resolve_layout_path(Path::new("wtn/edo31.wtn"));
        assert_eq!(r, PathBuf::from("wtn/edo31.wtn"));
        let r2 = resolve_layout_path(Path::new("wtn\\edo31.wtn"));
        assert_eq!(r2, PathBuf::from("wtn\\edo31.wtn"));
    }

    #[test]
    fn absolute_path_passes_through() {
        #[cfg(windows)]
        let abs = PathBuf::from(r"C:\Temp\edo31.wtn");
        #[cfg(not(windows))]
        let abs = PathBuf::from("/tmp/edo31.wtn");
        assert_eq!(resolve_layout_path(&abs), abs);
    }

    #[test]
    fn unknown_extension_passes_through() {
        let r = resolve_layout_path(Path::new("notes.txt"));
        assert_eq!(r, PathBuf::from("notes.txt"));
    }

    #[test]
    fn kind_detected_from_extension() {
        assert_eq!(
            LayoutKind::from_extension(Path::new("a.wtn")),
            Some(LayoutKind::Wtn)
        );
        assert_eq!(
            LayoutKind::from_extension(Path::new("a.XTN")),
            Some(LayoutKind::Xtn)
        );
        assert_eq!(LayoutKind::from_extension(Path::new("a.ini")), None);
    }

    #[test]
    fn current_index_matches_by_basename() {
        let list = vec![
            PathBuf::from("wtn/edo12.wtn"),
            PathBuf::from("wtn/edo19.wtn"),
            PathBuf::from("wtn/edo31.wtn"),
        ];
        assert_eq!(
            current_index(&list, Path::new("wtn/edo19.wtn")),
            Some(1)
        );
        // Basename-only match (current path may be given as bare filename).
        assert_eq!(current_index(&list, Path::new("edo31.wtn")), Some(2));
        assert_eq!(current_index(&list, Path::new("edo999.wtn")), None);
    }
}
