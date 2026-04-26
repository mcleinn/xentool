//! Control-bar LED painting (base < mode overlay < flash).
//!
//! Ported layout + order from xenwooting:
//!   - flash dispatch:           `bin/xenwooting.rs` 4373–4384
//!   - flash / restore body:     `bin/xenwooting.rs` 4685–4756
//!   - spacebar overlay:         `bin/xenwooting.rs` 2535–2564
//!   - aftertouch-mode overlay:  `bin/xenwooting.rs` 2596–2629
//!   - base per-trainer-mode:    `bin/xenwooting.rs` 2527–2533  (Off = red)
//!
//! The restore pipeline is NOT "paint base and done". It paints base red,
//! then re-applies every persistent overlay in order (Space hold, aftertouch
//! mode). This is the ordering that xenwooting fine-tuned over several bugs;
//! do not reorder or shortcut it.

use crossbeam_channel::Sender;

use crate::settings::{ControlBarSettings, OneOrManyU8};
use crate::wooting::hidmap::{hid, Hid};
use crate::wooting::modes::AftertouchMode;

/// RGB command type used by the paint worker in `serve.rs`.
#[derive(Debug, Clone, Copy)]
pub struct RgbCmd {
    pub device_index: u8,
    pub row: u8,
    pub col: u8,
    pub rgb: (u8, u8, u8),
}

// Colours ported verbatim from xenwooting.
pub const BASE_RGB: (u8, u8, u8) = (255, 0, 0); // red (TrainerMode::Off default)
pub const HIGHLIGHT_RGB: (u8, u8, u8) = (255, 255, 255); // flash white
pub const ARROW_FLASH_RGB: (u8, u8, u8) = (0, 255, 255); // cyan for arrow presses
pub const AFTERTOUCH_OFF_RGB: (u8, u8, u8) = (0, 128, 255);
pub const AFTERTOUCH_PEAK_RGB: (u8, u8, u8) = (255, 255, 0);

/// Look up the LED cols for a given HID name string (from settings).
fn cols_for_name<'a>(cb: &'a ControlBarSettings, name: &str) -> Option<Vec<u8>> {
    cb.led_cols_by_hid.get(name).map(OneOrManyU8::as_vec)
}

/// Map an HID code to the canonical string name used by the settings map.
pub fn hid_name(h: Hid) -> Option<&'static str> {
    Some(match h {
        hid::LEFT_CONTROL => "LeftCtrl",
        hid::LEFT_META => "LeftMeta",
        hid::LEFT_ALT => "LeftAlt",
        hid::SPACE => "Space",
        hid::RIGHT_ALT => "RightAlt",
        hid::CONTEXT_MENU => "ContextMenu",
        hid::RIGHT_CONTROL => "RightCtrl",
        _ => return None,
    })
}

/// Return the cols on the control bar for this HID, if it's mapped.
pub fn cols_for_hid(cb: &ControlBarSettings, h: Hid) -> Vec<u8> {
    hid_name(h)
        .and_then(|n| cols_for_name(cb, n))
        .unwrap_or_default()
}

pub fn is_control_bar(cb: &ControlBarSettings, h: Hid) -> bool {
    hid_name(h).is_some_and(|n| cb.led_cols_by_hid.contains_key(n))
        || matches!(h, hid::ARROW_LEFT | hid::ARROW_RIGHT | hid::ARROW_DOWN | hid::ARROW_UP)
}

/// For an arrow key, which control-bar LED should flash?
/// ArrowLeft → RightAlt, ArrowDown → ContextMenu, ArrowRight → RightCtrl.
pub fn arrow_flash_target(h: Hid) -> Option<Hid> {
    Some(match h {
        hid::ARROW_LEFT => hid::RIGHT_ALT,
        hid::ARROW_DOWN => hid::CONTEXT_MENU,
        hid::ARROW_RIGHT => hid::RIGHT_CONTROL,
        _ => return None,
    })
}

/// Persistent aftertouch-mode indicator colour for the RightAlt key.
pub fn aftertouch_mode_color(mode: AftertouchMode, base_rgb: (u8, u8, u8)) -> (u8, u8, u8) {
    match mode {
        AftertouchMode::SpeedMapped => base_rgb,
        AftertouchMode::PeakMapped => AFTERTOUCH_PEAK_RGB,
        AftertouchMode::Off => AFTERTOUCH_OFF_RGB,
    }
}

/// Persistent Space indicator colour (white when octave-hold active).
pub fn space_color(hold: bool, base_rgb: (u8, u8, u8)) -> (u8, u8, u8) {
    if hold {
        HIGHLIGHT_RGB
    } else {
        base_rgb
    }
}

// --- Public paint API ---

/// Paint the flash on press. Ported from xenwooting.rs 4373–4384 + 4692–4705.
///
/// For an arrow key: flash cyan on the arrow's indicator target.
/// For any other control-bar key: flash white on the key's own cols.
pub fn paint_flash_on_down(
    tx: &Sender<RgbCmd>,
    cb: &ControlBarSettings,
    dev_idx: u8,
    h: Hid,
) {
    let (led_hid, flash_rgb) = if let Some(target) = arrow_flash_target(h) {
        (target, ARROW_FLASH_RGB)
    } else if hid_name(h).is_some() {
        (h, HIGHLIGHT_RGB)
    } else {
        return;
    };
    for c in cols_for_hid(cb, led_hid) {
        let _ = tx.try_send(RgbCmd {
            device_index: dev_idx,
            row: cb.row,
            col: c,
            rgb: flash_rgb,
        });
    }
}

/// Paint the restore sequence on release. Ported from xenwooting.rs 4719–4753:
/// base red → Space overlay → aftertouch-mode overlay.
///
/// `affected` is Some(HID) for a targeted repaint (key-up) or None to repaint
/// the whole control bar (e.g. initial paint, screensaver wake, mode change).
pub fn paint_restore(
    tx: &Sender<RgbCmd>,
    cb: &ControlBarSettings,
    dev_idx: u8,
    affected: Option<Hid>,
    aftertouch_mode: AftertouchMode,
    octave_hold: bool,
) {
    // 1. Base red on affected cols. When repainting the whole bar (affected=None),
    // paint EVERY column 0..=13 of the control-bar row, so keys without an HID
    // (notably Fn, which has an LED but emits no HID code) are lit too. Matches
    // xenwooting's paint_base behavior.
    match affected {
        Some(h) => {
            let target = arrow_flash_target(h).unwrap_or(h);
            if let Some(name) = hid_name(target) {
                if let Some(cols) = cols_for_name(cb, name) {
                    for c in cols {
                        let _ = tx.try_send(RgbCmd {
                            device_index: dev_idx,
                            row: cb.row,
                            col: c,
                            rgb: BASE_RGB,
                        });
                    }
                }
            }
        }
        None => {
            for c in 0u8..=13 {
                let _ = tx.try_send(RgbCmd {
                    device_index: dev_idx,
                    row: cb.row,
                    col: c,
                    rgb: BASE_RGB,
                });
            }
        }
    }

    // 2. Space overlay (white when octave hold active).
    paint_spacebar_indicator(tx, cb, dev_idx, BASE_RGB, octave_hold);

    // 3. Aftertouch mode overlay on RightAlt cols.
    paint_aftertouch_mode_indicator(tx, cb, dev_idx, BASE_RGB, aftertouch_mode);
}

pub fn paint_spacebar_indicator(
    tx: &Sender<RgbCmd>,
    cb: &ControlBarSettings,
    dev_idx: u8,
    base_rgb: (u8, u8, u8),
    hold: bool,
) {
    let Some(cols) = cols_for_name(cb, "Space") else {
        return;
    };
    let rgb = space_color(hold, base_rgb);
    for c in cols {
        let _ = tx.try_send(RgbCmd {
            device_index: dev_idx,
            row: cb.row,
            col: c,
            rgb,
        });
    }
}

pub fn paint_aftertouch_mode_indicator(
    tx: &Sender<RgbCmd>,
    cb: &ControlBarSettings,
    dev_idx: u8,
    base_rgb: (u8, u8, u8),
    mode: AftertouchMode,
) {
    let Some(cols) = cols_for_name(cb, "RightAlt") else {
        return;
    };
    let rgb = aftertouch_mode_color(mode, base_rgb);
    for c in cols {
        let _ = tx.try_send(RgbCmd {
            device_index: dev_idx,
            row: cb.row,
            col: c,
            rgb,
        });
    }
}

/// Blank the entire control-bar row (for the screensaver). Paints every
/// column 0..=13 black so unnamed LEDs — e.g. Fn, which has an LED but no
/// HID code — go dark along with the rest.
pub fn paint_off(tx: &Sender<RgbCmd>, cb: &ControlBarSettings, dev_idx: u8) {
    for c in 0u8..=13 {
        let _ = tx.try_send(RgbCmd {
            device_index: dev_idx,
            row: cb.row,
            col: c,
            rgb: (0, 0, 0),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arrow_targets_match_xenwooting() {
        assert_eq!(arrow_flash_target(hid::ARROW_LEFT), Some(hid::RIGHT_ALT));
        assert_eq!(arrow_flash_target(hid::ARROW_DOWN), Some(hid::CONTEXT_MENU));
        assert_eq!(arrow_flash_target(hid::ARROW_RIGHT), Some(hid::RIGHT_CONTROL));
        assert_eq!(arrow_flash_target(hid::SPACE), None);
    }

    #[test]
    fn aftertouch_colors_match() {
        let base = BASE_RGB;
        assert_eq!(aftertouch_mode_color(AftertouchMode::SpeedMapped, base), base);
        assert_eq!(aftertouch_mode_color(AftertouchMode::PeakMapped, base), AFTERTOUCH_PEAK_RGB);
        assert_eq!(aftertouch_mode_color(AftertouchMode::Off, base), AFTERTOUCH_OFF_RGB);
    }

    #[test]
    fn space_color_overrides_when_hold() {
        assert_eq!(space_color(true, BASE_RGB), HIGHLIGHT_RGB);
        assert_eq!(space_color(false, BASE_RGB), BASE_RGB);
    }

    #[test]
    fn is_control_bar_identifies_keys() {
        let cb = ControlBarSettings::default();
        assert!(is_control_bar(&cb, hid::SPACE));
        assert!(is_control_bar(&cb, hid::RIGHT_ALT));
        assert!(is_control_bar(&cb, hid::ARROW_LEFT));
        assert!(!is_control_bar(&cb, hid::Q));
    }
}
