//! LED overlay for the local API (`serve --api`) on Wooting keyboards.
//!
//! The serve loop paints single keys (`RgbCmd`: layout colours, press flash, control bar,
//! screensaver) through the RGB worker. The `Compositor` sits in that worker between those
//! paints and the RGB SDK: it remembers every paint as the *base* colour of its LED and
//! shows the API's overlay colour on top where one is set. The serve loop's painting code
//! therefore needs no knowledge of the overlay and cannot wipe it; clearing the overlay
//! brings back exactly what the serve loop painted last.
//!
//! All keyboards are one instrument: while any LED has an overlay colour, the base colours
//! of all of them are dimmed by `dim_base`.

use std::collections::HashMap;

pub type Rgb = (u8, u8, u8);
/// One LED: (RGB SDK device index, row, column).
pub type LedAddr = (u8, u8, u8);

/// The whole overlay, already resolved to LEDs (the serve loop knows which key is which
/// LED; that changes when keyboards come or go, and then it sends the overlay again).
#[derive(Debug, Clone, PartialEq)]
pub struct OverlayFrame {
    pub keys: Vec<(LedAddr, Rgb)>,
    /// Brightness factor of the base colours while `keys` is not empty (1.0 = unchanged).
    pub dim_base: f32,
}

pub struct Compositor {
    base: HashMap<LedAddr, Rgb>,
    overlay: HashMap<LedAddr, Rgb>,
    dim_base: f32,
}

fn scaled(c: Rgb, f: f32) -> Rgb {
    if f >= 1.0 {
        return c;
    }
    let s = |v: u8| (v as f32 * f).round() as u8;
    (s(c.0), s(c.1), s(c.2))
}

impl Compositor {
    pub fn new() -> Self {
        Self { base: HashMap::new(), overlay: HashMap::new(), dim_base: 1.0 }
    }

    /// Factor applied to base colours now.
    fn factor(&self) -> f32 {
        if self.overlay.is_empty() { 1.0 } else { self.dim_base }
    }

    /// What the LED should show now.
    pub fn shown(&self, addr: LedAddr) -> Rgb {
        match self.overlay.get(&addr) {
            Some(&c) => c,
            None => scaled(self.base.get(&addr).copied().unwrap_or((0, 0, 0)), self.factor()),
        }
    }

    /// A paint from the serve loop. Returns the colour to send to the keyboard, or None
    /// while the LED is covered by the overlay (the paint is remembered for later).
    pub fn set_base(&mut self, addr: LedAddr, rgb: Rgb) -> Option<Rgb> {
        self.base.insert(addr, rgb);
        if self.overlay.contains_key(&addr) {
            None
        } else {
            Some(scaled(rgb, self.factor()))
        }
    }

    /// Replace the overlay. `out` receives every LED whose colour changes.
    pub fn set_overlay(&mut self, frame: &OverlayFrame, out: &mut Vec<(LedAddr, Rgb)>) {
        let old_factor = self.factor();
        let old = std::mem::take(&mut self.overlay);
        self.overlay = frame.keys.iter().copied().collect();
        self.dim_base = frame.dim_base.clamp(0.0, 1.0);
        if self.factor() != old_factor {
            // the dimming starts, ends or changes: every LED we know is repainted
            let mut addrs: Vec<LedAddr> = self.base.keys().chain(self.overlay.keys()).chain(old.keys()).copied().collect();
            addrs.sort_unstable();
            addrs.dedup();
            out.extend(addrs.into_iter().map(|a| (a, self.shown(a))));
            return;
        }
        let mut changed: Vec<LedAddr> = old.iter().filter(|(a, c)| self.overlay.get(a) != Some(c)).map(|(a, _)| *a).collect();
        changed.extend(self.overlay.keys().filter(|a| !old.contains_key(a)).copied());
        changed.sort_unstable();
        out.extend(changed.into_iter().map(|a| (a, self.shown(a))));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RED: Rgb = (200, 0, 0);
    const GREEN: Rgb = (0, 200, 0);
    const HINT: Rgb = (0, 0, 255);

    /// Two keyboards with three painted LEDs each.
    fn painted() -> Compositor {
        let mut c = Compositor::new();
        for dev in 0..2u8 {
            for col in 0..3u8 {
                assert_eq!(c.set_base((dev, 1, col), RED), Some(RED));
            }
        }
        c
    }

    fn frame(keys: &[(LedAddr, Rgb)], dim_base: f32) -> OverlayFrame {
        OverlayFrame { keys: keys.to_vec(), dim_base }
    }

    #[test]
    fn overlay_sits_on_the_layout_colours_and_clearing_restores_them() {
        let mut c = painted();
        let mut out = Vec::new();
        c.set_overlay(&frame(&[((0, 1, 0), HINT), ((0, 1, 1), GREEN)], 1.0), &mut out);
        assert_eq!(out, vec![((0, 1, 0), HINT), ((0, 1, 1), GREEN)], "only the overlay keys are touched");
        // one key cleared, one recoloured: the cleared one shows its layout colour again
        out.clear();
        c.set_overlay(&frame(&[((0, 1, 1), HINT)], 1.0), &mut out);
        assert_eq!(out, vec![((0, 1, 0), RED), ((0, 1, 1), HINT)]);
        // nothing changes: nothing is sent
        out.clear();
        c.set_overlay(&frame(&[((0, 1, 1), HINT)], 1.0), &mut out);
        assert!(out.is_empty());
        // clear all: exactly the layout colours
        c.set_overlay(&frame(&[], 1.0), &mut out);
        assert_eq!(out, vec![((0, 1, 1), RED)]);
        for dev in 0..2u8 {
            for col in 0..3u8 {
                assert_eq!(c.shown((dev, 1, col)), RED);
            }
        }
    }

    #[test]
    fn the_serve_loops_own_painting_does_not_wipe_the_overlay() {
        let mut c = painted();
        let mut out = Vec::new();
        c.set_overlay(&frame(&[((0, 1, 0), HINT)], 1.0), &mut out);
        // press flash and release repaint on the overlay key, a layout change, the
        // screensaver's blanking: nothing reaches the LED
        assert_eq!(c.set_base((0, 1, 0), (255, 255, 255)), None);
        assert_eq!(c.set_base((0, 1, 0), RED), None);
        assert_eq!(c.set_base((0, 1, 0), GREEN), None);
        assert_eq!(c.shown((0, 1, 0)), HINT);
        // other keys are painted as before
        assert_eq!(c.set_base((0, 1, 2), GREEN), Some(GREEN));
        // the overlay goes: the key shows what was painted last underneath
        out.clear();
        c.set_overlay(&frame(&[], 1.0), &mut out);
        assert_eq!(out, vec![((0, 1, 0), GREEN)]);
    }

    #[test]
    fn dim_base_dims_every_keyboard_while_any_overlay_key_is_set() {
        let mut c = painted();
        let mut out = Vec::new();
        // one overlay key on keyboard 0: all six LEDs change, also those of keyboard 1
        c.set_overlay(&frame(&[((0, 1, 0), HINT)], 0.25), &mut out);
        assert_eq!(out.len(), 6);
        assert_eq!(c.shown((0, 1, 0)), HINT, "the overlay colour itself is not dimmed");
        assert_eq!(c.shown((0, 1, 1)), (50, 0, 0));
        assert_eq!(c.shown((1, 1, 2)), (50, 0, 0));
        assert!(out.contains(&((1, 1, 2), (50, 0, 0))));
        // paints during the overlay arrive dimmed (control bar, press flash, layout change)
        assert_eq!(c.set_base((1, 1, 2), GREEN), Some((0, 50, 0)));
        assert_eq!(c.set_base((1, 5, 7), (255, 255, 255)), Some((64, 64, 64)));
        // the next hint lies on keyboard 1 only: keyboard 0 stays dimmed, only two LEDs change
        out.clear();
        c.set_overlay(&frame(&[((1, 1, 0), HINT)], 0.25), &mut out);
        assert_eq!(out, vec![((0, 1, 0), (50, 0, 0)), ((1, 1, 0), HINT)]);
        // overlay gone: full brightness everywhere, with what was painted meanwhile
        out.clear();
        c.set_overlay(&frame(&[], 0.25), &mut out);
        assert_eq!(out.len(), 7);
        assert_eq!(c.shown((0, 1, 1)), RED);
        assert_eq!(c.shown((1, 1, 0)), RED);
        assert_eq!(c.shown((1, 1, 2)), GREEN);
        assert_eq!(c.shown((1, 5, 7)), (255, 255, 255));
        // no overlay key: a dim value alone changes nothing
        out.clear();
        c.set_overlay(&frame(&[], 0.1), &mut out);
        assert!(out.is_empty());
        assert_eq!(c.set_base((0, 1, 1), RED), Some(RED));
    }

    #[test]
    fn an_led_never_painted_is_dark_under_the_overlay() {
        let mut c = Compositor::new();
        let mut out = Vec::new();
        c.set_overlay(&frame(&[((3, 2, 2), HINT)], 0.5), &mut out);
        assert_eq!(out, vec![((3, 2, 2), HINT)]);
        out.clear();
        c.set_overlay(&frame(&[], 0.5), &mut out);
        assert_eq!(out, vec![((3, 2, 2), (0, 0, 0))]);
    }
}
