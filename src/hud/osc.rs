//! Optional OSC listener — accepts parameter updates and ephemeral events
//! from external programs (typically SuperCollider) and exposes them to the
//! HUD frontend.
//!
//! Address scheme:
//! - `/xentool/param/<group>/<name> <value> [<unit>]` — sticky parameter.
//!   `<value>` is a float or int32; `<unit>` is an optional string (e.g.
//!   `"Hz"`, `"%"`). Re-sending overwrites; the HUD displays the latest.
//! - `/xentool/event <text>` — ephemeral event line (e.g. "Settings pressed").
//!   Stored with a timestamp; the HUD shows recent ones for ~5 seconds.
//!
//! Listens on UDP `0.0.0.0:<port>` so a SuperCollider scsynth running in a
//! WSL/VM can push too. All work happens on a background thread; the SSE
//! handler reads the shared state non-blockingly.

use std::collections::HashMap;
use std::net::UdpSocket;
use std::sync::{Arc, RwLock};
use std::thread;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use rosc::{OscMessage, OscPacket, OscType};
use serde::Serialize;

const RECV_BUFFER_BYTES: usize = 4096;
/// Newest events first; older events stay until evicted by capacity.
const EVENT_RING_CAP: usize = 32;

#[derive(Debug, Clone, Serialize)]
pub struct OscParam {
    pub group: String,
    pub name: String,
    /// Numeric value; OSC senders that pass int32 are widened to f64.
    pub value: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
    /// Host wall clock at receipt (ms since UNIX epoch).
    pub ts_ms: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct OscEvent {
    pub text: String,
    pub ts_ms: u64,
}

#[derive(Debug, Default, Clone, Serialize)]
pub struct OscState {
    /// Keyed by `"<group>/<name>"`. BTreeMap-ish would render in stable
    /// order, but the count is small and JSON ordering is cosmetic.
    pub params: HashMap<String, OscParam>,
    pub events: Vec<OscEvent>,
}

impl OscState {
    /// Drop events older than `max_age_ms`. Called by the SSE thread before
    /// each emit so stale "Settings pressed" lines don't linger forever.
    pub fn purge_old_events(&mut self, max_age_ms: u64) {
        let now = now_ms();
        self.events
            .retain(|ev| now.saturating_sub(ev.ts_ms) <= max_age_ms);
    }
}

#[derive(Clone)]
pub struct OscClient {
    inner: Arc<RwLock<OscState>>,
}

impl OscClient {
    /// Bind to `0.0.0.0:<port>` and spawn the receive thread. Returns the
    /// bound port (useful when `port == 0` requested an ephemeral assign).
    pub fn start(port: u16) -> Result<(Self, u16)> {
        let socket = UdpSocket::bind(("0.0.0.0", port))
            .with_context(|| format!("OSC bind failed on port {port}"))?;
        let bound_port = socket.local_addr().map(|a| a.port()).unwrap_or(port);
        // Block on recv; thread parks while idle. No timeout needed.
        let inner = Arc::new(RwLock::new(OscState::default()));
        let inner_w = inner.clone();
        thread::Builder::new()
            .name("hud-osc".into())
            .spawn(move || receive_loop(socket, inner_w))
            .context("spawning OSC receive thread")?;
        Ok((Self { inner }, bound_port))
    }

    /// Cheap snapshot of the current OSC state for the SSE encoder. Cloned
    /// so the lock is held only briefly.
    pub fn snapshot(&self, max_event_age_ms: u64) -> OscState {
        let mut state = self.inner.read().unwrap().clone();
        state.purge_old_events(max_event_age_ms);
        state
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn receive_loop(socket: UdpSocket, inner: Arc<RwLock<OscState>>) {
    let mut buf = [0u8; RECV_BUFFER_BYTES];
    loop {
        let n = match socket.recv(&mut buf) {
            Ok(n) => n,
            Err(e) => {
                eprintln!("xentool HUD: OSC recv error: {e}");
                break;
            }
        };
        let pkt = match rosc::decoder::decode_udp(&buf[..n]) {
            Ok((_, packet)) => packet,
            Err(_) => continue,
        };
        handle_packet(pkt, &inner);
    }
}

fn handle_packet(packet: OscPacket, inner: &Arc<RwLock<OscState>>) {
    match packet {
        OscPacket::Message(msg) => handle_message(msg, inner),
        OscPacket::Bundle(bundle) => {
            for inner_pkt in bundle.content {
                handle_packet(inner_pkt, inner);
            }
        }
    }
}

fn handle_message(msg: OscMessage, inner: &Arc<RwLock<OscState>>) {
    let addr = msg.addr.as_str();
    if let Some(rest) = addr.strip_prefix("/xentool/param/") {
        let mut iter = rest.splitn(2, '/');
        let group = match iter.next() {
            Some(g) if !g.is_empty() => g.to_string(),
            _ => return,
        };
        let name = match iter.next() {
            Some(n) if !n.is_empty() => n.to_string(),
            _ => return,
        };
        let value = match msg.args.first() {
            Some(OscType::Float(v)) => *v as f64,
            Some(OscType::Double(v)) => *v,
            Some(OscType::Int(v)) => *v as f64,
            Some(OscType::Long(v)) => *v as f64,
            _ => return,
        };
        let unit = match msg.args.get(1) {
            Some(OscType::String(s)) => Some(s.clone()),
            _ => None,
        };
        let key = format!("{group}/{name}");
        let mut state = inner.write().unwrap();
        state.params.insert(
            key,
            OscParam { group, name, value, unit, ts_ms: now_ms() },
        );
    } else if addr == "/xentool/event" {
        let text = match msg.args.first() {
            Some(OscType::String(s)) => s.clone(),
            _ => return,
        };
        let mut state = inner.write().unwrap();
        state.events.insert(0, OscEvent { text, ts_ms: now_ms() });
        if state.events.len() > EVENT_RING_CAP {
            state.events.truncate(EVENT_RING_CAP);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rosc::encoder;

    fn synth_msg(addr: &str, args: Vec<OscType>) -> Vec<u8> {
        let msg = OscPacket::Message(OscMessage {
            addr: addr.to_string(),
            args,
        });
        encoder::encode(&msg).expect("encode")
    }

    #[test]
    fn param_message_round_trip() {
        let inner = Arc::new(RwLock::new(OscState::default()));
        let bytes = synth_msg(
            "/xentool/param/filter/cutoff",
            vec![OscType::Float(880.0), OscType::String("Hz".into())],
        );
        let pkt = rosc::decoder::decode_udp(&bytes).unwrap().1;
        handle_packet(pkt, &inner);
        let state = inner.read().unwrap();
        let p = state.params.get("filter/cutoff").expect("param stored");
        assert_eq!(p.group, "filter");
        assert_eq!(p.name, "cutoff");
        assert!((p.value - 880.0).abs() < 1e-6);
        assert_eq!(p.unit.as_deref(), Some("Hz"));
    }

    #[test]
    fn event_message_pushes_to_ring() {
        let inner = Arc::new(RwLock::new(OscState::default()));
        let bytes = synth_msg(
            "/xentool/event",
            vec![OscType::String("Settings pressed: cycle layout".into())],
        );
        let pkt = rosc::decoder::decode_udp(&bytes).unwrap().1;
        handle_packet(pkt, &inner);
        let state = inner.read().unwrap();
        assert_eq!(state.events.len(), 1);
        assert_eq!(state.events[0].text, "Settings pressed: cycle layout");
    }

    #[test]
    fn unknown_address_is_ignored() {
        let inner = Arc::new(RwLock::new(OscState::default()));
        let bytes = synth_msg("/random/path", vec![OscType::Float(1.0)]);
        let pkt = rosc::decoder::decode_udp(&bytes).unwrap().1;
        handle_packet(pkt, &inner);
        let state = inner.read().unwrap();
        assert!(state.params.is_empty());
        assert!(state.events.is_empty());
    }

    #[test]
    fn purge_old_events_drops_stale() {
        let mut state = OscState::default();
        // Fake an old event by manually setting ts_ms to 0.
        state.events.push(OscEvent { text: "old".into(), ts_ms: 0 });
        state.events.push(OscEvent { text: "fresh".into(), ts_ms: now_ms() });
        state.purge_old_events(1000);
        assert_eq!(state.events.len(), 1);
        assert_eq!(state.events[0].text, "fresh");
    }
}
