//! Wire shape for the Live HUD.
//!
//! Both backends publish the same `LiveState` to the HUD. The publisher fills
//! `seq` and `ts_ms` itself; everything else is provided by the call site.

use std::collections::BTreeMap;

use serde::Serialize;

/// Schema version for the SSE payload. Bump when the frontend needs to break
/// compatibility with older xentool builds.
pub const SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize)]
pub struct LiveState {
    pub version: u32,
    pub seq: u64,
    pub ts_ms: u64,
    pub layout: LayoutInfo,
    pub mode: ModeInfo,
    /// Currently sounding absolute pitches per board.
    /// `BTreeMap` keeps board ordering stable in the JSON output ("board0",
    /// "board1", ...).
    pub pressed: BTreeMap<String, Vec<i32>>,
    /// Full per-cell pitch list per board, used by the frontend to prefetch
    /// note names. `None` entries mark inactive cells.
    pub layout_pitches: BTreeMap<String, Vec<Option<i32>>>,
}

#[derive(Debug, Clone, Serialize)]
pub struct LayoutInfo {
    pub id: String,
    pub name: String,
    pub edo: u32,
    pub pitch_offset: i32,
}

#[derive(Debug, Clone, Serialize)]
pub struct ModeInfo {
    /// `"exquis"` or `"wooting"`. Used by the frontend to conditionally render
    /// backend-specific status corners.
    pub backend: &'static str,
    pub octave_shift: i8,

    /// Wooting-specific fields. Set to `None` from the Exquis backend.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub press_threshold: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub aftertouch: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub aftertouch_speed_max: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub velocity_profile: Option<String>,
}

impl LiveState {
    /// Empty snapshot used as the publisher's initial value before any backend
    /// has submitted real state.
    pub fn empty(backend: &'static str) -> Self {
        Self {
            version: SCHEMA_VERSION,
            seq: 0,
            ts_ms: 0,
            layout: LayoutInfo {
                id: String::new(),
                name: String::new(),
                edo: 12,
                pitch_offset: 0,
            },
            mode: ModeInfo {
                backend,
                octave_shift: 0,
                press_threshold: None,
                aftertouch: None,
                aftertouch_speed_max: None,
                velocity_profile: None,
            },
            pressed: BTreeMap::new(),
            layout_pitches: BTreeMap::new(),
        }
    }
}
