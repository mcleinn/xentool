//! HTTP/SSE server for the Live HUD.
//!
//! Endpoints (all GET):
//! - `/`                  → embedded `live.html`
//! - `/live.css`          → embedded stylesheet
//! - `/live.js`           → embedded script
//! - `/api/live/state`    → one-shot JSON snapshot of the current `LiveState`
//! - `/api/live/stream`   → Server-Sent Events; emits `event: state` whenever
//!                          the snapshot's `seq` advances. Always-on heartbeat
//!                          comments keep proxies/browsers from timing out.
//!
//! Listens on `0.0.0.0:<port>` so phones / tablets on the LAN can connect —
//! the same deployment shape as xenwooting's webconfigurator. The HTTP thread
//! pool serializes JSON itself; the audio path never blocks on SSE.

use std::collections::{BTreeSet, HashMap};
use std::io::{self, Read};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use rouille::{Response, ResponseBody};
use serde::Serialize;

use super::HudPublisher;
use super::chordnam::{self, ChordResult};
use super::osc::{OscClient, OscState};
use super::state::LiveState;
use super::xenharm::{IntervalName, NoteName, XenharmClient, XenharmStatus};

/// Drop OSC events older than this when decorating a snapshot.
const OSC_EVENT_TTL_MS: u64 = 5_000;

const INDEX_HTML: &str = include_str!("../../assets/live.html");
const LIVE_CSS: &str = include_str!("../../assets/live.css");
const LIVE_JS: &str = include_str!("../../assets/live.js");
/// Bravura.otf (SMuFL, ~512 KB) ships in the binary so users only need
/// xentool to see microtonal note glyphs — no separate font install.
const BRAVURA_OTF: &[u8] = include_bytes!("../../assets/Bravura.otf");

/// Spawn the HUD HTTP server on a background daemon thread. Returns once the
/// listener is bound; the thread runs until the process exits. `osc` is
/// optional — when `None`, the SSE shape just has empty `osc.params` /
/// `osc.events` arrays.
pub fn spawn(
    publisher: HudPublisher,
    port: u16,
    xenharm: XenharmClient,
    osc: Option<OscClient>,
) -> Result<()> {
    let addr = format!("0.0.0.0:{port}");
    let url = format!("http://localhost:{port}/");
    let pub_for_handler = publisher.clone();
    let xen_for_handler = xenharm.clone();
    let osc_for_handler = osc.clone();

    let server = rouille::Server::new(addr.clone(), move |request| {
        handle(request, &pub_for_handler, &xen_for_handler, osc_for_handler.as_ref())
    })
    .map_err(|e| anyhow::anyhow!("HUD bind failed on {addr}: {e}"))
    .with_context(|| "HUD server")?;

    eprintln!("xentool HUD: listening on {url}");

    thread::Builder::new()
        .name("hud-server".into())
        .spawn(move || server.run())
        .context("spawning HUD server thread")?;

    Ok(())
}

fn handle(
    request: &rouille::Request,
    publisher: &HudPublisher,
    xenharm: &XenharmClient,
    osc: Option<&OscClient>,
) -> Response {
    // The HTML/CSS/JS bundle changes with every xentool build. Without
    // an explicit no-cache header browsers happily serve a stale copy
    // after a reinstall, which produced a confusing "frontend ignores
    // backend changes" symptom (e.g. chord names not rendering even
    // though the backend was sending them). Bravura.otf stays
    // immutable — its bytes are pinned to xentool's build.
    let no_cache = ("Cache-Control", "no-store");
    match (request.method(), request.url().as_str()) {
        ("GET", "/") => Response::html(INDEX_HTML).with_additional_header(no_cache.0, no_cache.1),
        ("GET", "/live.css") => Response::from_data("text/css; charset=utf-8", LIVE_CSS)
            .with_additional_header(no_cache.0, no_cache.1),
        ("GET", "/live.js") => {
            Response::from_data("application/javascript; charset=utf-8", LIVE_JS)
                .with_additional_header(no_cache.0, no_cache.1)
        }
        ("GET", "/Bravura.otf") => {
            Response::from_data("font/otf", BRAVURA_OTF)
                .with_additional_header("Cache-Control", "public, max-age=31536000, immutable")
        }
        ("GET", "/api/live/state") => {
            let snap = publisher.snapshot();
            enqueue_xenharm(&snap, xenharm);
            let chord = chord_for_snapshot(&snap);
            let note_names = xenharm.names_for_state(&snap);
            let interval_names = interval_names_for_snapshot(&snap, xenharm);
            let osc_state = osc.map(|o| o.snapshot(OSC_EVENT_TTL_MS)).unwrap_or_default();
            Response::json(&Decorated {
                state: &snap,
                chord,
                note_names,
                interval_names,
                xenharm: xenharm.status(),
                osc: osc_state,
            })
        }
        ("GET", "/api/live/stream") => sse_response(publisher.clone(), xenharm.clone(), osc.cloned()),
        _ => Response::empty_404(),
    }
}

fn sse_response(publisher: HudPublisher, xenharm: XenharmClient, osc: Option<OscClient>) -> Response {
    let reader = SseReader::new(publisher, xenharm, osc);
    Response {
        status_code: 200,
        headers: vec![
            ("Content-Type".into(), "text/event-stream; charset=utf-8".into()),
            ("Cache-Control".into(), "no-cache, no-transform".into()),
            // Hint to nginx-style proxies; harmless elsewhere.
            ("X-Accel-Buffering".into(), "no".into()),
            ("Connection".into(), "keep-alive".into()),
        ],
        data: ResponseBody::from_reader(Box::new(reader) as Box<dyn Read + Send>),
        upgrade: None,
    }
}

/// Streams SSE events from a `HudPublisher`. Implements `Read` so rouille can
/// pipe the body using chunked transfer encoding without knowing the length
/// up front.
///
/// One reader per connected client. The reader thread lives on rouille's
/// worker pool — when the client disconnects, the next write into the socket
/// fails and rouille drops the reader.
struct SseReader {
    publisher: HudPublisher,
    xenharm: XenharmClient,
    osc: Option<OscClient>,
    last_seq: u64,
    last_heartbeat: Instant,
    /// Pending bytes for the current SSE frame (event + data + terminator,
    /// or a heartbeat comment).
    buffer: Vec<u8>,
    pos: usize,
    /// Have we sent the initial retry hint yet?
    sent_prelude: bool,
}

impl SseReader {
    fn new(publisher: HudPublisher, xenharm: XenharmClient, osc: Option<OscClient>) -> Self {
        Self {
            publisher,
            xenharm,
            osc,
            // u64::MAX forces the first poll to look "different" from the
            // current seq, so we always emit the current snapshot to a freshly
            // connected client even if the publisher hasn't ticked yet.
            last_seq: u64::MAX,
            last_heartbeat: Instant::now(),
            buffer: Vec::with_capacity(2048),
            pos: 0,
            sent_prelude: false,
        }
    }
}

impl Read for SseReader {
    fn read(&mut self, dst: &mut [u8]) -> io::Result<usize> {
        // Refill buffer if we've drained it.
        while self.pos >= self.buffer.len() {
            self.buffer.clear();
            self.pos = 0;

            if !self.sent_prelude {
                // `retry:` instructs the browser's EventSource to reconnect
                // after 2 s if the connection drops. Sent once per client.
                self.buffer.extend_from_slice(b"retry: 2000\n\n");
                self.sent_prelude = true;
                break;
            }

            let snap = self.publisher.snapshot();
            // Re-emit when either the audio-side seq advanced *or* OSC
            // pushed something new. We don't gate on OSC's own seq counter
            // — instead just re-decorate when params/events differ from
            // last emit. Cheap snapshot + re-encode.
            let osc_state = self
                .osc
                .as_ref()
                .map(|o| o.snapshot(OSC_EVENT_TTL_MS))
                .unwrap_or_default();
            if snap.seq != self.last_seq {
                self.last_seq = snap.seq;
                self.last_heartbeat = Instant::now();
                enqueue_xenharm(&snap, &self.xenharm);
                let chord = chord_for_snapshot(&snap);
                let note_names = self.xenharm.names_for_state(&snap);
                let interval_names = interval_names_for_snapshot(&snap, &self.xenharm);
                let decorated = Decorated {
                    state: &snap,
                    chord,
                    note_names,
                    interval_names,
                    xenharm: self.xenharm.status(),
                    osc: osc_state,
                };
                self.buffer.extend_from_slice(b"event: state\ndata: ");
                serde_json::to_writer(&mut self.buffer, &decorated)
                    .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;
                self.buffer.extend_from_slice(b"\n\n");
                break;
            }

            if self.last_heartbeat.elapsed() >= Duration::from_secs(15) {
                self.last_heartbeat = Instant::now();
                self.buffer.extend_from_slice(b": ping\n\n");
                break;
            }

            // Idle wait — short enough that new snapshots reach the client
            // within ~33 ms, long enough that the thread is mostly parked.
            thread::sleep(Duration::from_millis(33));
        }

        let n = (self.buffer.len() - self.pos).min(dst.len());
        dst[..n].copy_from_slice(&self.buffer[self.pos..self.pos + n]);
        self.pos += n;
        Ok(n)
    }
}

/// SSE/JSON envelope: the bare `LiveState` flattened in alongside fields the
/// SSE thread computes per emit — chord names from `chordnam.par`,
/// per-pitch Bravura glyph names from xenharm, and OSC-pushed parameters
/// / events from external programs (typically SuperCollider). The hot
/// loop never pays for any of it.
#[derive(Serialize)]
struct Decorated<'a> {
    #[serde(flatten)]
    state: &'a LiveState,
    /// One result per pressed pitch class (sorted ascending). `names` may be
    /// empty when no chord template matches at that root.
    chord: Vec<ChordResult>,
    /// `"edo:pitch"` → glyph name + alternates. Missing entries fall back to
    /// numeric on the frontend (`formatNoteUnicode || String(p)`).
    #[serde(skip_serializing_if = "HashMap::is_empty")]
    note_names: HashMap<String, NoteName>,
    /// `"edo:steps"` → xenharm-supplied interval name (e.g. `vM3`, `P5`).
    /// Frontend decorates the chord-line `+N` deltas with the matching
    /// short form when present.
    #[serde(skip_serializing_if = "HashMap::is_empty")]
    interval_names: HashMap<String, IntervalName>,
    /// xenharm health status (single line for the HUD footer when present).
    xenharm: XenharmStatus,
    /// External-process parameter strip + ephemeral event log. Empty when
    /// no OSC client is configured.
    osc: OscState,
}

/// Non-blocking: ask xenharm to resolve every pitch in this snapshot. The
/// worker dedupes against the cache, so re-enqueuing the same pitches every
/// snapshot is fine (no duplicate POSTs).
fn enqueue_xenharm(snap: &LiveState, xenharm: &XenharmClient) {
    if !xenharm.is_available() {
        return;
    }
    let edo = snap.layout.edo as i32;
    if edo <= 0 {
        return;
    }
    let mut pitches: Vec<i32> = Vec::new();
    for arr in snap.pressed.values() {
        for &p in arr {
            pitches.push(p);
        }
    }
    for arr in snap.layout_pitches.values() {
        for entry in arr {
            if let Some(p) = entry {
                pitches.push(*p);
            }
        }
    }
    pitches.sort_unstable();
    pitches.dedup();
    if !pitches.is_empty() {
        xenharm.enqueue(edo, pitches);
    }

    // Also enqueue interval-name resolution for every pairwise step
    // distance among the currently pressed pitch classes (mod edo). Small
    // set: at most C(N, 2) entries for N pcs, typically <20. Worker dedupes
    // against its interval cache.
    let mut pcs: BTreeSet<i32> = BTreeSet::new();
    for arr in snap.pressed.values() {
        for &p in arr {
            pcs.insert(p.rem_euclid(edo));
        }
    }
    if pcs.len() < 2 {
        return;
    }
    let pcs_vec: Vec<i32> = pcs.into_iter().collect();
    let mut steps: BTreeSet<i32> = BTreeSet::new();
    for i in 0..pcs_vec.len() {
        for j in 0..pcs_vec.len() {
            if i == j {
                continue;
            }
            let diff = (pcs_vec[j] - pcs_vec[i]).rem_euclid(edo);
            if diff > 0 {
                steps.insert(diff);
            }
        }
    }
    if !steps.is_empty() {
        xenharm.enqueue_intervals(edo, steps.into_iter().collect());
    }
}

/// Build the interval-name decoration map for the snapshot. Only includes
/// step counts that already happen to be cached — misses degrade silently
/// to plain `+N` on the frontend.
fn interval_names_for_snapshot(
    snap: &LiveState,
    xenharm: &XenharmClient,
) -> HashMap<String, IntervalName> {
    let edo = snap.layout.edo as i32;
    if edo <= 0 {
        return HashMap::new();
    }
    let mut pcs: BTreeSet<i32> = BTreeSet::new();
    for arr in snap.pressed.values() {
        for &p in arr {
            pcs.insert(p.rem_euclid(edo));
        }
    }
    if pcs.len() < 2 {
        return HashMap::new();
    }
    let pcs_vec: Vec<i32> = pcs.into_iter().collect();
    let mut steps: BTreeSet<i32> = BTreeSet::new();
    for i in 0..pcs_vec.len() {
        for j in 0..pcs_vec.len() {
            if i == j {
                continue;
            }
            let diff = (pcs_vec[j] - pcs_vec[i]).rem_euclid(edo);
            if diff > 0 {
                steps.insert(diff);
            }
        }
    }
    let steps_vec: Vec<i32> = steps.into_iter().collect();
    xenharm.interval_names_for(edo, &steps_vec)
}

fn chord_for_snapshot(snap: &LiveState) -> Vec<ChordResult> {
    let edo = snap.layout.edo as i32;
    if edo <= 0 {
        return Vec::new();
    }
    // Combine pressed pitches across all boards, fold to pitch classes,
    // dedupe (BTreeSet keeps them sorted ascending — matches xenwooting's
    // `uniqSorted` of `mod(p, edo)`).
    let mut pcs: BTreeSet<i32> = BTreeSet::new();
    for arr in snap.pressed.values() {
        for &p in arr {
            let pc = p.rem_euclid(edo);
            pcs.insert(pc);
        }
    }
    if pcs.len() < 2 {
        return Vec::new();
    }
    let pcs_vec: Vec<i32> = pcs.into_iter().collect();
    chordnam::find_chord_names(chordnam::db(), edo, &pcs_vec)
}

// `Arc<HudPublisher>` would also work but the publisher is cheap to clone
// (it holds an `Arc` internally), so passing by value avoids an extra layer.
#[allow(dead_code)]
fn _send_sync_check(p: HudPublisher) -> impl Send {
    move || {
        let _ = p.snapshot();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hud::state::LiveState;

    #[test]
    fn decorated_envelope_includes_chord_field() {
        // Build a snapshot with 3 pressed pitches that should match a Major
        // Triad template in 31-EDO (`4:5:6` projects to step pattern 10-8).
        // Verify the serialized wire shape includes a non-empty `chord`
        // array and the `Decorated` flatten merges `LiveState` fields.
        let mut snap = LiveState::empty("exquis");
        snap.layout.edo = 31;
        snap.pressed.insert("board0".into(), vec![62, 72, 80]); // pcs 0, 10, 18
        let chord = chord_for_snapshot(&snap);
        let decorated = Decorated {
            state: &snap,
            chord,
            note_names: HashMap::new(),
            interval_names: HashMap::new(),
            xenharm: super::super::xenharm::XenharmStatus::default(),
            osc: super::super::osc::OscState::default(),
        };
        let json = serde_json::to_string(&decorated).expect("serializes");
        assert!(json.contains("\"version\":1"), "missing version: {json}");
        assert!(json.contains("\"chord\":["), "missing chord key: {json}");
        assert!(
            json.contains("Major Triad"),
            "expected Major Triad name in JSON, got: {json}",
        );
        assert!(json.contains("\"rootPc\":"), "expected camelCase rootPc: {json}");
    }

    #[test]
    fn sse_reader_emits_snapshot_then_blocks() {
        let publisher = HudPublisher::new(LiveState::empty("exquis"));
        publisher.submit(LiveState::empty("exquis"));

        // Use an unavailable xenharm endpoint so the test stays fully local.
        let xen = XenharmClient::start("http://127.0.0.1:1");
        let mut reader = SseReader::new(publisher, xen, None);
        let mut buf = vec![0u8; 4096];

        // First read should yield the prelude (retry hint).
        let n = reader.read(&mut buf).unwrap();
        assert!(n > 0);
        let prelude = std::str::from_utf8(&buf[..n]).unwrap();
        assert!(prelude.starts_with("retry:"), "prelude: {prelude:?}");

        // Drain any remaining prelude bytes.
        while reader.pos < reader.buffer.len() {
            reader.read(&mut buf).unwrap();
        }

        // Second logical frame should be the state event.
        let n = reader.read(&mut buf).unwrap();
        let frame = std::str::from_utf8(&buf[..n]).unwrap();
        assert!(frame.starts_with("event: state\ndata: "), "frame: {frame:?}");
        assert!(frame.contains("\"backend\":\"exquis\""), "frame: {frame:?}");
        assert!(frame.ends_with("\n\n"), "frame: {frame:?}");
    }
}
