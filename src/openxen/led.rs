//! openxen LEDs (openxen repo: `docs/led-protocol.md`).
//!
//! Commands are 64-byte HID output reports on the module's openxen interface (the same one
//! that streams the keys). The firmware owns gamma, press highlight, the power budget and the
//! host-timeout fallback; xentool only sends colours, configuration and keepalives.
//!
//! Same shape as the Wooting RGB worker: the hot loop sends small messages over a channel;
//! a worker thread keeps one desired frame per module and flushes changed frames at most
//! every 16 ms (`FLUSH_INTERVAL`), plus a keepalive every 500 ms. `Scheduler` is the pure
//! part (what to send when) and is unit-tested; the thread only does I/O.

use std::collections::HashMap;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, RecvTimeoutError};

pub const REPORT_SIZE: usize = 64;
pub const KEYS: usize = 56;
pub const PIXELS_PER_REPORT: usize = 20;
const CMD_PIXELS: u8 = 0x10;
const CMD_CONFIG: u8 = 0x11;
const CMD_KEEPALIVE: u8 = 0x12;
const PIXELS_SHOW: u8 = 0x01;
pub const BUDGET_MAX_MA: u16 = 600;

pub const FLUSH_INTERVAL: Duration = Duration::from_millis(16);
pub const KEEPALIVE_INTERVAL: Duration = Duration::from_millis(500);

pub type Rgb = (u8, u8, u8);
pub type Frame = [Rgb; KEYS];

/// Module-wide LED settings (sent on attach and whenever they change).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LedConfig {
    pub brightness: u8,
    /// Press highlight colour shown by the firmware itself; None = off.
    pub highlight: Option<Rgb>,
    /// Firmware turns LEDs off when no command arrives for this long; 0 = never.
    pub timeout_ms: u16,
    pub budget_ma: u16,
}

// --- encoders ---

fn pad(mut v: Vec<u8>) -> [u8; REPORT_SIZE] {
    v.resize(REPORT_SIZE, 0);
    let mut out = [0u8; REPORT_SIZE];
    out.copy_from_slice(&v);
    out
}

pub fn pixels_report(first: usize, colours: &[Rgb], show: bool) -> [u8; REPORT_SIZE] {
    assert!(!colours.is_empty() && colours.len() <= PIXELS_PER_REPORT && first + colours.len() <= KEYS);
    let mut v = vec![CMD_PIXELS, if show { PIXELS_SHOW } else { 0 }, first as u8, colours.len() as u8];
    for &(r, g, b) in colours {
        v.extend_from_slice(&[r, g, b]);
    }
    pad(v)
}

/// A whole frame as 3 reports (keys 0-19, 20-39, 40-55); the last one shows it.
pub fn frame_reports(frame: &Frame) -> Vec<[u8; REPORT_SIZE]> {
    (0..KEYS)
        .step_by(PIXELS_PER_REPORT)
        .map(|first| {
            let end = (first + PIXELS_PER_REPORT).min(KEYS);
            pixels_report(first, &frame[first..end], end == KEYS)
        })
        .collect()
}

pub fn config_report(c: &LedConfig) -> [u8; REPORT_SIZE] {
    let (mode, (r, g, b)) = match c.highlight {
        Some(rgb) => (1u8, rgb),
        None => (0u8, (0, 0, 0)),
    };
    let budget = c.budget_ma.min(BUDGET_MAX_MA);
    let mut v = vec![CMD_CONFIG, c.brightness, mode, r, g, b];
    v.extend_from_slice(&c.timeout_ms.to_le_bytes());
    v.extend_from_slice(&budget.to_le_bytes());
    pad(v)
}

pub fn keepalive_report() -> [u8; REPORT_SIZE] {
    pad(vec![CMD_KEEPALIVE])
}

/// Prefix the HID report number (0: the interface has no report IDs) as hidapi expects.
pub fn with_report_id(r: &[u8; REPORT_SIZE]) -> [u8; REPORT_SIZE + 1] {
    let mut out = [0u8; REPORT_SIZE + 1];
    out[1..].copy_from_slice(r);
    out
}

// --- scheduling (pure) ---

/// Which overlay keys an `Overlay` update removes before it sets new ones.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum OverlayClear {
    #[default]
    None,
    Keys(Vec<u8>),
    All,
}

#[derive(Debug, Clone)]
struct ModuleLeds {
    /// Layout colours (what `Frame` and `Pixel` set).
    base: Frame,
    /// Overlay above the base (local API, e.g. a tutor's hints); None = base colour shows.
    overlay: [Option<Rgb>; KEYS],
    /// Brightness factor of the base right now (1.0 = unchanged). The modules are one
    /// keyboard: the base dims on all of them while any of them shows an overlay.
    dim: f32,
    /// Some module shows an overlay (see `Scheduler::refresh`).
    overlay_active: bool,
    /// What the module should show: base, dimmed if asked, with the overlay on top.
    desired: Frame,
    frame_dirty: bool,
    sent_config: Option<LedConfig>,
    last_frame_at: Option<Instant>,
    last_send_at: Option<Instant>,
}

impl ModuleLeds {
    fn has_overlay(&self) -> bool {
        self.overlay.iter().any(|o| o.is_some())
    }

    fn recompose(&mut self) {
        let dim = self.dim;
        let scale = |v: u8| (v as f32 * dim).round() as u8;
        let mut f = self.base;
        for (px, over) in f.iter_mut().zip(self.overlay.iter()) {
            match over {
                Some(c) => *px = *c,
                None if dim < 1.0 => *px = (scale(px.0), scale(px.1), scale(px.2)),
                None => {}
            }
        }
        if f != self.desired {
            self.desired = f;
            self.frame_dirty = true;
        }
    }
}

/// What to send to which module, given the desired state and the clock.
#[derive(Debug, Default)]
pub struct Scheduler {
    config: Option<LedConfig>,
    modules: HashMap<Arc<str>, ModuleLeds>,
    /// Brightness factor of the base colours while an overlay is shown; None = unchanged.
    dim_base: Option<f32>,
}

impl Scheduler {
    pub fn attach(&mut self, serial: Arc<str>) {
        self.modules.insert(serial, ModuleLeds {
            base: [(0, 0, 0); KEYS],
            overlay: [None; KEYS],
            dim: 1.0,
            overlay_active: false,
            desired: [(0, 0, 0); KEYS],
            frame_dirty: false,
            sent_config: None,
            last_frame_at: None,
            last_send_at: None,
        });
        self.refresh();
    }

    pub fn detach(&mut self, serial: &str) {
        self.modules.remove(serial);
        self.refresh();
    }

    /// The modules are one keyboard: while any of them shows an overlay, the base colours
    /// dim and the press highlight is off on all of them, also on those without an
    /// overlay key of their own.
    fn refresh(&mut self) {
        let active = self.modules.values().any(|m| m.has_overlay());
        let dim = if active { self.dim_base.unwrap_or(1.0).clamp(0.0, 1.0) } else { 1.0 };
        for m in self.modules.values_mut() {
            m.overlay_active = active;
            m.dim = dim;
            m.recompose();
        }
    }

    pub fn set_config(&mut self, c: LedConfig) {
        self.config = Some(c);
    }

    /// The base (layout) colours; an overlay stays on top of them.
    pub fn set_frame(&mut self, serial: &str, frame: &Frame) {
        if let Some(m) = self.modules.get_mut(serial) {
            m.base = *frame;
            m.recompose();
        }
    }

    pub fn set_pixel(&mut self, serial: &str, key: usize, rgb: Rgb) {
        if let Some(m) = self.modules.get_mut(serial) {
            if key < KEYS {
                m.base[key] = rgb;
                m.recompose();
            }
        }
    }

    /// Change a module's overlay: first `clear`, then `set`. A cleared key shows its base
    /// colour again; nothing else changes. `dim_base` (if given) applies to all modules and
    /// is kept until changed.
    pub fn set_overlay(&mut self, serial: &str, set: &[(u8, Rgb)], clear: &OverlayClear, dim_base: Option<f32>) {
        if let Some(d) = dim_base {
            self.dim_base = Some(d);
        }
        self.overlay_keys(serial, set, clear);
        self.refresh();
    }

    fn overlay_keys(&mut self, serial: &str, set: &[(u8, Rgb)], clear: &OverlayClear) {
        let Some(m) = self.modules.get_mut(serial) else { return };
        match clear {
            OverlayClear::None => {}
            OverlayClear::All => m.overlay = [None; KEYS],
            OverlayClear::Keys(keys) => {
                for &k in keys {
                    if let Some(o) = m.overlay.get_mut(k as usize) {
                        *o = None;
                    }
                }
            }
        }
        for &(k, rgb) in set {
            if let Some(o) = m.overlay.get_mut(k as usize) {
                *o = Some(rgb);
            }
        }
    }

    /// Reports due now, per module (config first, then a changed frame at most every
    /// FLUSH_INTERVAL, else a keepalive every KEEPALIVE_INTERVAL). Nothing is sent before
    /// a config exists, so a module never runs on firmware defaults by accident.
    ///
    /// While an overlay is shown (on any module), the firmware press highlight is switched
    /// off: the overlay's owner decides what a pressed key looks like.
    pub fn due(&mut self, now: Instant) -> Vec<(Arc<str>, Vec<[u8; REPORT_SIZE]>)> {
        let Some(cfg) = self.config else { return Vec::new() };
        let mut out = Vec::new();
        for (serial, m) in self.modules.iter_mut() {
            let mut reports = Vec::new();
            let effective = if m.overlay_active { LedConfig { highlight: None, ..cfg } } else { cfg };
            if m.sent_config != Some(effective) {
                reports.push(config_report(&effective));
                m.sent_config = Some(effective);
            }
            let frame_ok = m.last_frame_at.is_none_or(|t| now.duration_since(t) >= FLUSH_INTERVAL);
            if m.frame_dirty && frame_ok {
                reports.extend(frame_reports(&m.desired));
                m.frame_dirty = false;
                m.last_frame_at = Some(now);
            }
            if reports.is_empty() && m.last_send_at.is_none_or(|t| now.duration_since(t) >= KEEPALIVE_INTERVAL) {
                reports.push(keepalive_report());
            }
            if !reports.is_empty() {
                m.last_send_at = Some(now);
                out.push((serial.clone(), reports));
            }
        }
        out
    }

    /// A write failed: resend config and frame when the module comes back.
    pub fn invalidate(&mut self, serial: &str) {
        if let Some(m) = self.modules.get_mut(serial) {
            m.sent_config = None;
            m.frame_dirty = true;
        }
    }
}

// --- worker thread ---

pub enum LedMsg {
    /// A module's write handle (opened by `hid::Manager` next to its stream reader).
    Attach { serial: Arc<str>, dev: hidapi::HidDevice },
    Detach { serial: Arc<str> },
    Config(LedConfig),
    Frame { serial: Arc<str>, frame: Box<Frame> },
    /// Single-key update (for HUD-driven indicators; not used by serve yet).
    #[allow(dead_code)]
    Pixel { serial: Arc<str>, key: u8, rgb: Rgb },
    /// Overlay update from the local API (see `Scheduler::set_overlay`).
    Overlay { serial: Arc<str>, set: Vec<(u8, Rgb)>, clear: OverlayClear, dim_base: Option<f32> },
}

pub fn spawn_worker(rx: Receiver<LedMsg>) -> thread::JoinHandle<()> {
    thread::Builder::new()
        .name("openxen-leds".into())
        .spawn(move || {
            let mut sched = Scheduler::default();
            let mut devs: HashMap<Arc<str>, hidapi::HidDevice> = HashMap::new();
            loop {
                match rx.recv_timeout(Duration::from_millis(4)) {
                    Ok(LedMsg::Attach { serial, dev }) => {
                        sched.attach(serial.clone());
                        devs.insert(serial, dev);
                    }
                    Ok(LedMsg::Detach { serial }) => {
                        sched.detach(&serial);
                        devs.remove(&serial);
                    }
                    Ok(LedMsg::Config(c)) => sched.set_config(c),
                    Ok(LedMsg::Frame { serial, frame }) => sched.set_frame(&serial, &frame),
                    Ok(LedMsg::Pixel { serial, key, rgb }) => sched.set_pixel(&serial, key as usize, rgb),
                    Ok(LedMsg::Overlay { serial, set, clear, dim_base }) => sched.set_overlay(&serial, &set, &clear, dim_base),
                    Err(RecvTimeoutError::Timeout) => {}
                    Err(RecvTimeoutError::Disconnected) => return,
                }
                for (serial, reports) in sched.due(Instant::now()) {
                    let Some(dev) = devs.get(&serial) else { continue };
                    for r in &reports {
                        if dev.write(&with_report_id(r)).is_err() {
                            sched.invalidate(&serial);
                            break;
                        }
                    }
                }
            }
        })
        .expect("spawning openxen LED worker")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> LedConfig {
        LedConfig { brightness: 200, highlight: Some((1, 2, 3)), timeout_ms: 2000, budget_ma: 450 }
    }

    /// Same expectations as the openxen repo's reference encoder test
    /// (tests/test_led.py::test_encoder_layout, host/oxl.py).
    #[test]
    fn encoder_matches_reference_layout() {
        let c = config_report(&cfg());
        assert_eq!(&c[..10], &[0x11, 200, 1, 1, 2, 3, 0xD0, 0x07, 0xC2, 0x01]);
        assert!(c[10..].iter().all(|&b| b == 0));
        let mut f = [(0u8, 0u8, 0u8); KEYS];
        for (k, px) in f.iter_mut().enumerate() {
            *px = (k as u8, k as u8, k as u8);
        }
        let r = frame_reports(&f);
        let heads: Vec<[u8; 4]> = r.iter().map(|x| [x[0], x[1], x[2], x[3]]).collect();
        assert_eq!(heads, vec![[0x10, 0, 0, 20], [0x10, 0, 20, 20], [0x10, 1, 40, 16]]);
        assert_eq!(&r[2][4..7], &[40, 40, 40]);
        assert_eq!(keepalive_report()[0], 0x12);
        assert_eq!(with_report_id(&keepalive_report())[..2], [0, 0x12]);
        // budget is capped like the firmware does
        let big = config_report(&LedConfig { budget_ma: 5000, ..cfg() });
        assert_eq!(u16::from_le_bytes([big[8], big[9]]), BUDGET_MAX_MA);
    }

    #[test]
    fn nothing_before_config_then_config_and_frame() {
        let mut s = Scheduler::default();
        let t0 = Instant::now();
        s.attach("A".into());
        s.set_frame("A", &[(9, 9, 9); KEYS]);
        assert!(s.due(t0).is_empty());
        s.set_config(cfg());
        let d = s.due(t0);
        assert_eq!(d.len(), 1);
        let kinds: Vec<u8> = d[0].1.iter().map(|r| r[0]).collect();
        assert_eq!(kinds, vec![0x11, 0x10, 0x10, 0x10]);
    }

    #[test]
    fn frames_rate_limited_and_coalesced() {
        let mut s = Scheduler::default();
        let t0 = Instant::now();
        s.attach("A".into());
        s.set_config(cfg());
        s.set_frame("A", &[(1, 1, 1); KEYS]);
        s.due(t0);
        // three changes within 16 ms -> one frame, after the interval, with the last state
        s.set_pixel("A", 3, (5, 5, 5));
        s.set_pixel("A", 4, (6, 6, 6));
        assert!(s.due(t0 + Duration::from_millis(5)).is_empty());
        s.set_pixel("A", 3, (7, 7, 7));
        let d = s.due(t0 + Duration::from_millis(17));
        assert_eq!(d[0].1.len(), 3);
        assert_eq!(&d[0].1[0][4 + 3 * 3..4 + 3 * 5], &[7, 7, 7, 6, 6, 6]);
        // an unchanged frame is not resent
        s.set_frame("A", &{
            let mut f = [(1u8, 1u8, 1u8); KEYS];
            f[3] = (7, 7, 7);
            f[4] = (6, 6, 6);
            f
        });
        assert!(s.due(t0 + Duration::from_millis(40)).is_empty());
    }

    #[test]
    fn keepalive_when_idle_and_resend_after_failure() {
        let mut s = Scheduler::default();
        let t0 = Instant::now();
        s.attach("A".into());
        s.set_config(cfg());
        s.due(t0);
        assert!(s.due(t0 + Duration::from_millis(400)).is_empty());
        let d = s.due(t0 + Duration::from_millis(501));
        assert_eq!(d[0].1, vec![keepalive_report()]);
        s.invalidate("A");
        let d = s.due(t0 + Duration::from_millis(600));
        assert_eq!(d[0].1.iter().map(|r| r[0]).collect::<Vec<_>>(), vec![0x11, 0x10, 0x10, 0x10]);
    }

    #[test]
    fn config_change_is_sent_to_every_module() {
        let mut s = Scheduler::default();
        let t0 = Instant::now();
        s.attach("A".into());
        s.attach("B".into());
        s.set_config(cfg());
        s.due(t0);
        s.set_config(LedConfig { brightness: 10, ..cfg() });
        let d = s.due(t0 + Duration::from_millis(1));
        assert_eq!(d.len(), 2);
        assert!(d.iter().all(|(_, r)| r[0][0] == 0x11 && r[0][1] == 10));
    }

    /// Colour of `key` in the frame reports of one `due` entry.
    fn shown(reports: &[[u8; REPORT_SIZE]], key: usize) -> Rgb {
        let r = reports.iter().find(|r| r[0] == 0x10 && (r[2] as usize..r[2] as usize + r[3] as usize).contains(&key)).expect("key in frame");
        let i = 4 + 3 * (key - r[2] as usize);
        (r[i], r[i + 1], r[i + 2])
    }

    fn frames(d: &[(Arc<str>, Vec<[u8; REPORT_SIZE]>)]) -> &[[u8; REPORT_SIZE]] {
        &d[0].1
    }

    #[test]
    fn overlay_sits_on_base_and_clearing_one_key_restores_only_it() {
        let mut s = Scheduler::default();
        let t = |ms: u64| Instant::now() + Duration::from_millis(ms);
        s.attach("A".into());
        s.set_config(cfg());
        s.set_frame("A", &[(10, 20, 30); KEYS]);
        s.due(t(0));
        s.set_overlay("A", &[(3, (255, 0, 0)), (4, (0, 255, 0))], &OverlayClear::None, None);
        let d = s.due(t(20));
        assert_eq!(shown(frames(&d), 3), (255, 0, 0));
        assert_eq!(shown(frames(&d), 4), (0, 255, 0));
        assert_eq!(shown(frames(&d), 5), (10, 20, 30));
        // a layout repaint (or the screensaver) changes the base only
        s.set_frame("A", &[(1, 1, 1); KEYS]);
        let d = s.due(t(40));
        assert_eq!(shown(frames(&d), 3), (255, 0, 0));
        assert_eq!(shown(frames(&d), 5), (1, 1, 1));
        // clearing key 3 shows its base colour again; key 4 keeps its overlay
        s.set_overlay("A", &[], &OverlayClear::Keys(vec![3]), None);
        let d = s.due(t(60));
        assert_eq!(shown(frames(&d), 3), (1, 1, 1));
        assert_eq!(shown(frames(&d), 4), (0, 255, 0));
        s.set_overlay("A", &[], &OverlayClear::All, None);
        let d = s.due(t(80));
        assert_eq!(shown(frames(&d), 4), (1, 1, 1));
        // out-of-range keys and unknown modules are ignored
        s.set_overlay("A", &[(200, (9, 9, 9))], &OverlayClear::Keys(vec![99]), None);
        s.set_overlay("nope", &[(1, (9, 9, 9))], &OverlayClear::All, None);
        assert!(s.due(t(100)).is_empty());
    }

    #[test]
    fn overlay_dims_base_and_switches_highlight_off() {
        let mut s = Scheduler::default();
        let t = |ms: u64| Instant::now() + Duration::from_millis(ms);
        s.attach("A".into());
        s.set_config(cfg());
        s.set_frame("A", &[(100, 200, 50); KEYS]);
        s.due(t(0));
        s.set_overlay("A", &[(0, (255, 255, 255))], &OverlayClear::None, Some(0.25));
        let d = s.due(t(20));
        // config without highlight first (mode byte 0), then the frame
        assert_eq!(&frames(&d)[0][..3], &[0x11, 200, 0]);
        assert_eq!(shown(frames(&d), 0), (255, 255, 255));
        assert_eq!(shown(frames(&d), 1), (25, 50, 13));
        // overlay gone: full base brightness and the highlight is back
        s.set_overlay("A", &[], &OverlayClear::All, None);
        let d = s.due(t(40));
        assert_eq!(&frames(&d)[0][..6], &[0x11, 200, 1, 1, 2, 3]);
        assert_eq!(shown(frames(&d), 1), (100, 200, 50));
    }

    /// Several modules are one keyboard: an overlay key on one of them dims the layout
    /// colours and switches the highlight off on all of them.
    #[test]
    fn overlay_on_one_module_dims_every_module() {
        let mut s = Scheduler::default();
        let t = |ms: u64| Instant::now() + Duration::from_millis(ms);
        let of = |d: &[(Arc<str>, Vec<[u8; REPORT_SIZE]>)], serial: &str| -> Vec<[u8; REPORT_SIZE]> {
            d.iter().find(|(s, _)| &**s == serial).map(|(_, r)| r.clone()).unwrap_or_default()
        };
        s.attach("A".into());
        s.attach("B".into());
        s.set_config(cfg());
        s.set_frame("A", &[(100, 200, 50); KEYS]);
        s.set_frame("B", &[(40, 80, 120); KEYS]);
        s.due(t(0));
        // what serve sends for a chord whose keys all lie on A
        s.set_overlay("A", &[(0, (255, 255, 255))], &OverlayClear::None, Some(0.25));
        s.set_overlay("B", &[], &OverlayClear::None, Some(0.25));
        let d = s.due(t(20));
        assert_eq!(shown(&of(&d, "A"), 1), (25, 50, 13));
        assert_eq!(&of(&d, "B")[0][..3], &[0x11, 200, 0], "highlight off on B too");
        assert_eq!(shown(&of(&d, "B"), 0), (10, 20, 30));
        assert_eq!(shown(&of(&d, "B"), 55), (10, 20, 30));
        // a module plugged in during the overlay joins dimmed
        s.attach("C".into());
        s.set_frame("C", &[(200, 200, 200); KEYS]);
        let d = s.due(t(40));
        assert_eq!(shown(&of(&d, "C"), 7), (50, 50, 50));
        // the next chord lies on B only: A stays dimmed although its own overlay is gone
        s.set_overlay("A", &[], &OverlayClear::Keys(vec![0]), None);
        s.set_overlay("B", &[(5, (0, 255, 0))], &OverlayClear::None, None);
        let d = s.due(t(60));
        assert_eq!(shown(&of(&d, "A"), 0), (25, 50, 13));
        assert_eq!(shown(&of(&d, "B"), 5), (0, 255, 0));
        // lights off: every module back to full layout colours and highlight
        for m in ["A", "B", "C"] {
            s.set_overlay(m, &[], &OverlayClear::All, None);
        }
        let d = s.due(t(80));
        assert_eq!(shown(&of(&d, "A"), 0), (100, 200, 50));
        assert_eq!(shown(&of(&d, "B"), 5), (40, 80, 120));
        assert_eq!(shown(&of(&d, "C"), 7), (200, 200, 200));
        assert!(["A", "B", "C"].iter().all(|m| of(&d, m)[0][..3] == [0x11, 200, 1]));
    }
}
