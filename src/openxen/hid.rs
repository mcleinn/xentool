//! openxen module discovery and stream readers (hidapi).
//!
//! Each module exposes its analog stream as a separate HID interface (vendor usage page
//! `0xFFAC`, input-only, 144-byte reports). `Manager::refresh` (called every 500 ms by the
//! serve loop, like the Wooting hotplug refresh) opens newly attached modules and starts one
//! reader thread per module. Readers decode reports and push `Event`s into a bounded channel;
//! the hot loop drains it. Readers never block the hot loop: when the channel is full the
//! frame is dropped and counted (`Event::Overflow`).

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread;

use anyhow::{Context, Result};
use crossbeam_channel::{Sender, TrySendError};

use super::proto::{self, Decoder, Frame, USAGE_PAGE};

/// openxen USB identity (pid.codes test VID until a PID is assigned) and the stream
/// interface number, used as a fallback where a HID backend does not report usage pages.
pub const VID: u16 = 0x1209;
pub const PID: u16 = 0x0000;
pub const STREAM_INTERFACE: i32 = 3;

#[derive(Debug, Clone)]
pub struct ModuleInfo {
    pub serial: String,
    pub product: String,
    pub vendor_id: u16,
    pub product_id: u16,
    pub path: std::ffi::CString,
}

#[derive(Debug)]
pub enum Event {
    Connected { serial: Arc<str> },
    Frame { serial: Arc<str>, frame: Box<Frame> },
    Disconnected { serial: Arc<str>, reason: String },
    /// A report could not be decoded (rate-limited by the reader: first of every 1000).
    BadReport { serial: Arc<str>, reason: String },
}

fn is_stream_interface(d: &hidapi::DeviceInfo) -> bool {
    d.usage_page() == USAGE_PAGE
        || (d.vendor_id() == VID && d.product_id() == PID && d.interface_number() == STREAM_INTERFACE)
}

/// All attached openxen modules (one entry per stream interface), sorted by serial.
pub fn list_modules(api: &mut hidapi::HidApi) -> Result<Vec<ModuleInfo>> {
    api.refresh_devices().context("enumerating HID devices")?;
    let mut seen: HashMap<String, ModuleInfo> = HashMap::new();
    for d in api.device_list().filter(|d| is_stream_interface(d)) {
        let serial = d.serial_number().unwrap_or("NO-SERIAL").to_string();
        seen.entry(serial.clone()).or_insert_with(|| ModuleInfo {
            serial,
            product: d.product_string().unwrap_or("openxen").to_string(),
            vendor_id: d.vendor_id(),
            product_id: d.product_id(),
            path: d.path().to_owned(),
        });
    }
    let mut v: Vec<ModuleInfo> = seen.into_values().collect();
    v.sort_by(|a, b| a.serial.cmp(&b.serial));
    Ok(v)
}

struct Reader {
    stop: Arc<AtomicBool>,
    alive: Arc<AtomicBool>,
}

pub struct Manager {
    api: hidapi::HidApi,
    tx: Sender<Event>,
    /// LED worker: receives a second (write) handle per module.
    led_tx: Option<Sender<super::led::LedMsg>>,
    readers: HashMap<String, Reader>,
    /// Frames dropped because the hot loop's channel was full, per module.
    pub overflow: Arc<AtomicU64>,
}

impl Manager {
    pub fn new(tx: Sender<Event>, led_tx: Option<Sender<super::led::LedMsg>>) -> Result<Self> {
        let api = hidapi::HidApi::new().context("initialising hidapi")?;
        Ok(Self { api, tx, led_tx, readers: HashMap::new(), overflow: Arc::new(AtomicU64::new(0)) })
    }

    /// Start readers for newly attached modules and forget finished ones.
    pub fn refresh(&mut self) -> Result<()> {
        self.readers.retain(|_, r| r.alive.load(Ordering::Relaxed));
        for m in list_modules(&mut self.api)? {
            if self.readers.contains_key(&m.serial) {
                continue;
            }
            let dev = match self.api.open_path(&m.path) {
                Ok(d) => d,
                Err(e) => {
                    let _ = self.tx.try_send(Event::Disconnected { serial: m.serial.as_str().into(), reason: format!("open failed: {e}") });
                    continue;
                }
            };
            if let Some(led_tx) = &self.led_tx {
                // HID devices may be opened twice; the writer handle goes to the LED worker
                if let Ok(writer) = self.api.open_path(&m.path) {
                    let _ = led_tx.send(super::led::LedMsg::Attach { serial: m.serial.as_str().into(), dev: writer });
                }
            }
            let stop = Arc::new(AtomicBool::new(false));
            let alive = Arc::new(AtomicBool::new(true));
            let serial: Arc<str> = m.serial.as_str().into();
            let tx = self.tx.clone();
            let overflow = self.overflow.clone();
            let (stop_t, alive_t) = (stop.clone(), alive.clone());
            thread::Builder::new()
                .name(format!("openxen-{}", m.serial))
                .spawn(move || {
                    read_loop(dev, serial, tx, stop_t, overflow);
                    alive_t.store(false, Ordering::Relaxed);
                })
                .context("spawning openxen reader")?;
            self.readers.insert(m.serial.clone(), Reader { stop, alive });
        }
        Ok(())
    }
}

impl Drop for Manager {
    fn drop(&mut self) {
        for r in self.readers.values() {
            r.stop.store(true, Ordering::Relaxed);
        }
    }
}

fn read_loop(dev: hidapi::HidDevice, serial: Arc<str>, tx: Sender<Event>, stop: Arc<AtomicBool>, overflow: Arc<AtomicU64>) {
    let _ = tx.try_send(Event::Connected { serial: serial.clone() });
    let mut decoder = Decoder::default();
    let mut buf = [0u8; proto::REPORT_SIZE + 1];
    let mut bad = 0u64;
    while !stop.load(Ordering::Relaxed) {
        match dev.read_timeout(&mut buf, 100) {
            Ok(0) => continue,
            Ok(n) => match decoder.feed(&buf[..n]) {
                Ok(frame) => match tx.try_send(Event::Frame { serial: serial.clone(), frame: Box::new(frame) }) {
                    Ok(()) => {}
                    Err(TrySendError::Full(_)) => {
                        overflow.fetch_add(1, Ordering::Relaxed);
                    }
                    Err(TrySendError::Disconnected(_)) => return,
                },
                Err(e) => {
                    if bad % 1000 == 0 {
                        let _ = tx.try_send(Event::BadReport { serial: serial.clone(), reason: format!("{e:#}") });
                    }
                    bad += 1;
                    // a repeated/odd sequence restarts the unwrapping cleanly
                    decoder = Decoder::default();
                }
            },
            Err(e) => {
                let _ = tx.try_send(Event::Disconnected { serial: serial.clone(), reason: format!("{e}") });
                return;
            }
        }
    }
    let _ = tx.try_send(Event::Disconnected { serial, reason: "stopped".into() });
}
