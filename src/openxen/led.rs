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

#[derive(Debug, Clone)]
struct ModuleLeds {
    desired: Frame,
    frame_dirty: bool,
    config_sent: bool,
    last_frame_at: Option<Instant>,
    last_send_at: Option<Instant>,
}

/// What to send to which module, given the desired state and the clock.
#[derive(Debug, Default)]
pub struct Scheduler {
    config: Option<LedConfig>,
    modules: HashMap<Arc<str>, ModuleLeds>,
}

impl Scheduler {
    pub fn attach(&mut self, serial: Arc<str>) {
        self.modules.insert(serial, ModuleLeds {
            desired: [(0, 0, 0); KEYS],
            frame_dirty: false,
            config_sent: false,
            last_frame_at: None,
            last_send_at: None,
        });
    }

    pub fn detach(&mut self, serial: &str) {
        self.modules.remove(serial);
    }

    pub fn set_config(&mut self, c: LedConfig) {
        if self.config != Some(c) {
            self.config = Some(c);
            for m in self.modules.values_mut() {
                m.config_sent = false;
            }
        }
    }

    pub fn set_frame(&mut self, serial: &str, frame: &Frame) {
        if let Some(m) = self.modules.get_mut(serial) {
            if m.desired != *frame {
                m.desired = *frame;
                m.frame_dirty = true;
            }
        }
    }

    pub fn set_pixel(&mut self, serial: &str, key: usize, rgb: Rgb) {
        if let Some(m) = self.modules.get_mut(serial) {
            if key < KEYS && m.desired[key] != rgb {
                m.desired[key] = rgb;
                m.frame_dirty = true;
            }
        }
    }

    /// Reports due now, per module (config first, then a changed frame at most every
    /// FLUSH_INTERVAL, else a keepalive every KEEPALIVE_INTERVAL). Nothing is sent before
    /// a config exists, so a module never runs on firmware defaults by accident.
    pub fn due(&mut self, now: Instant) -> Vec<(Arc<str>, Vec<[u8; REPORT_SIZE]>)> {
        let Some(cfg) = self.config else { return Vec::new() };
        let mut out = Vec::new();
        for (serial, m) in self.modules.iter_mut() {
            let mut reports = Vec::new();
            if !m.config_sent {
                reports.push(config_report(&cfg));
                m.config_sent = true;
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
            m.config_sent = false;
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
}
