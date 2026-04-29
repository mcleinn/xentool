//! Optional client for `xenharm_service` — the small Python HTTP server
//! ported verbatim from xenwooting (`xenharm_service/server.py`). When it
//! answers a `GET /health` probe at startup we use it to fetch microtonal
//! note-name glyphs (Bravura SMuFL codepoints); otherwise the HUD falls
//! back to numeric labels.
//!
//! Hot-loop discipline: the publishers and SSE thread never block on
//! xenharm. Resolution runs on a dedicated worker thread that POSTs to
//! `/v1/note-names` in batches; results land in a shared cache that the
//! SSE encoder reads non-blockingly. Misses degrade to "no name yet" —
//! the frontend renders numeric and the next snapshot may include the
//! resolved entry once the worker has answered.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crossbeam_channel::{Receiver, Sender, unbounded};
use serde::{Deserialize, Serialize};

use super::state::LiveState;

pub const DEFAULT_URL: &str = "http://127.0.0.1:3199";
const HEALTH_PROBE_TIMEOUT_MS: u64 = 250;
/// Generous: xenharmlib's first call for an EDO initialises `EDOTuning` +
/// `UpDownNotation`, and a 244-pitch batch goes through one Python loop —
/// can easily take 1–3 seconds on the first request.
const POST_TIMEOUT_MS: u64 = 5_000;
/// After a failure, hold off retrying for this long so we don't pile up
/// requests while xenharm is wedged. Cleared on the next successful POST.
const FAILURE_BACKOFF_SECS: u64 = 30;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NoteAlt {
    pub short: String,
    pub unicode: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NoteName {
    pub short: String,
    pub unicode: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub alts: Vec<NoteAlt>,
}

#[derive(Debug, Deserialize)]
struct NoteNamesResponse {
    #[allow(dead_code)]
    edo: i32,
    /// Keyed by stringified pitch ("60", "61", ...).
    results: HashMap<String, NoteName>,
}

/// Same wire shape as `NoteName`; the xenharm `/v1/interval-names` endpoint
/// returns `{short, unicode, alts}` in the exact same form. We keep them as
/// distinct types in Rust so the two caches and SSE fields stay clearly
/// separated.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IntervalName {
    pub short: String,
    pub unicode: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub alts: Vec<NoteAlt>,
}

#[derive(Debug, Deserialize)]
struct IntervalNamesResponse {
    #[allow(dead_code)]
    edo: i32,
    /// Keyed by stringified step count ("3", "10", "21", ...).
    results: HashMap<String, IntervalName>,
}

#[derive(Debug)]
enum XenharmRequest {
    ResolvePitches { edo: i32, pitches: Vec<i32> },
    ResolveIntervals { edo: i32, steps: Vec<i32> },
}

/// Lightweight health snapshot for the SSE wire shape. Frontend uses this
/// to render at most one small status line — never spams the console.
#[derive(Debug, Clone, Default, Serialize)]
pub struct XenharmStatus {
    pub available: bool,
    /// Latest error message; cleared on the next successful POST. `None`
    /// means "no error since startup or last success".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    /// Host wall-clock time the error was recorded (ms since UNIX epoch).
    /// Frontend can render age relative to now.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_error_at_ms: Option<u64>,
}

struct Inner {
    available: AtomicBool,
    base_url: String,
    /// Cache keyed by `(edo, pitch)`. `None` value = "asked, no result".
    cache: RwLock<HashMap<(i32, i32), Option<NoteName>>>,
    /// Cache keyed by `(edo, step_count)`.
    interval_cache: RwLock<HashMap<(i32, i32), Option<IntervalName>>>,
    request_tx: Sender<XenharmRequest>,
    /// Health/error state read by the SSE encoder. Mutated only by the worker.
    status: RwLock<XenharmStatus>,
    /// When the worker should next try a POST. Pushed forward on failure.
    skip_until: RwLock<Option<Instant>>,
}

#[derive(Clone)]
pub struct XenharmClient {
    inner: Arc<Inner>,
}

impl XenharmClient {
    /// Probe `<base_url>/health` and, if the service answers, spawn a
    /// background worker thread that handles resolution requests. Always
    /// returns a usable client — `is_available()` reflects the probe outcome
    /// so callers can skip enqueuing work when the service is offline.
    pub fn start(base_url: impl Into<String>) -> Self {
        let base_url = base_url.into();
        let available = probe_health(&base_url);
        let (tx, rx) = unbounded::<XenharmRequest>();
        let inner = Arc::new(Inner {
            available: AtomicBool::new(available),
            base_url: base_url.clone(),
            cache: RwLock::new(HashMap::new()),
            interval_cache: RwLock::new(HashMap::new()),
            request_tx: tx,
            status: RwLock::new(XenharmStatus {
                available,
                last_error: None,
                last_error_at_ms: None,
            }),
            skip_until: RwLock::new(None),
        });
        if available {
            // One short eprintln on success at startup is fine — single line,
            // never repeats.
            eprintln!("xentool HUD: xenharm reachable at {base_url}");
            let inner_w = inner.clone();
            let _ = thread::Builder::new()
                .name("hud-xenharm".into())
                .spawn(move || worker_loop(rx, inner_w));
        } else {
            eprintln!(
                "xentool HUD: xenharm not reachable at {base_url} (note glyphs disabled)"
            );
        }
        Self { inner }
    }

    /// Snapshot the current health status for the SSE encoder. Cheap (clones
    /// a small struct under a read lock).
    pub fn status(&self) -> XenharmStatus {
        self.inner.status.read().unwrap().clone()
    }

    pub fn is_available(&self) -> bool {
        self.inner.available.load(Ordering::Relaxed)
    }

    /// Schedule a batch of pitches for resolution. Cheap (channel send).
    /// Pitches already cached are filtered on the worker side.
    pub fn enqueue(&self, edo: i32, pitches: Vec<i32>) {
        if !self.is_available() || pitches.is_empty() {
            return;
        }
        let _ = self
            .inner
            .request_tx
            .try_send(XenharmRequest::ResolvePitches { edo, pitches });
    }

    /// Schedule a batch of EDO step counts for interval-name resolution.
    /// Same fire-and-forget semantics as `enqueue`.
    pub fn enqueue_intervals(&self, edo: i32, steps: Vec<i32>) {
        if !self.is_available() || steps.is_empty() {
            return;
        }
        let _ = self
            .inner
            .request_tx
            .try_send(XenharmRequest::ResolveIntervals { edo, steps });
    }

    /// Best-effort lookup of cached interval names for a list of step
    /// counts under a single EDO. Returns `"edo:steps"` → `IntervalName`.
    pub fn interval_names_for(
        &self,
        edo: i32,
        steps: &[i32],
    ) -> HashMap<String, IntervalName> {
        let mut out = HashMap::new();
        if !self.is_available() || edo <= 0 {
            return out;
        }
        let cache = self.inner.interval_cache.read().unwrap();
        for &s in steps {
            if let Some(Some(name)) = cache.get(&(edo, s)) {
                out.insert(format!("{edo}:{s}"), name.clone());
            }
        }
        out
    }

    /// Build the SSE-decoration map: `"edo:pitch"` → `NoteName`. Reads only
    /// from the in-memory cache; never blocks on the network. Misses are
    /// silently omitted so the frontend's `formatNoteUnicode || String(p)`
    /// fallback kicks in.
    pub fn names_for_state(&self, snap: &LiveState) -> HashMap<String, NoteName> {
        let mut out = HashMap::new();
        if !self.is_available() {
            return out;
        }
        let edo = snap.layout.edo as i32;
        if edo <= 0 {
            return out;
        }

        let cache = self.inner.cache.read().unwrap();
        let mut interesting: Vec<i32> = Vec::new();
        for arr in snap.pressed.values() {
            for &p in arr {
                interesting.push(p);
            }
        }
        for arr in snap.layout_pitches.values() {
            for entry in arr {
                if let Some(p) = entry {
                    interesting.push(*p);
                }
            }
        }
        interesting.sort_unstable();
        interesting.dedup();
        for p in interesting {
            if let Some(Some(name)) = cache.get(&(edo, p)) {
                out.insert(format!("{edo}:{p}"), name.clone());
            }
        }
        out
    }
}

fn probe_health(base_url: &str) -> bool {
    let url = format!("{}/health", base_url.trim_end_matches('/'));
    ureq::get(&url)
        .timeout(Duration::from_millis(HEALTH_PROBE_TIMEOUT_MS))
        .call()
        .is_ok()
}

fn worker_loop(rx: Receiver<XenharmRequest>, inner: Arc<Inner>) {
    let base = inner.base_url.trim_end_matches('/').to_string();
    let pitch_url = format!("{}/v1/note-names", base);
    let interval_url = format!("{}/v1/interval-names", base);

    while let Ok(req) = rx.recv() {
        // Backoff: skip POSTs while we're inside the failure window. Drains
        // the queue cheaply (the check is just a read lock) so SSE-thread
        // enqueues during an outage don't pile up.
        if let Some(until) = *inner.skip_until.read().unwrap() {
            if Instant::now() < until {
                continue;
            }
        }

        match req {
            XenharmRequest::ResolvePitches { edo, pitches } => {
                handle_pitch_request(&inner, &pitch_url, edo, pitches);
            }
            XenharmRequest::ResolveIntervals { edo, steps } => {
                handle_interval_request(&inner, &interval_url, edo, steps);
            }
        }
    }
}

fn handle_pitch_request(inner: &Arc<Inner>, url: &str, edo: i32, pitches: Vec<i32>) {
    let missing: Vec<i32> = {
        let cache = inner.cache.read().unwrap();
        pitches
            .into_iter()
            .filter(|p| !cache.contains_key(&(edo, *p)))
            .collect()
    };
    if missing.is_empty() {
        return;
    }
    let body = serde_json::json!({ "edo": edo, "pitches": &missing });
    let resp = ureq::post(url)
        .timeout(Duration::from_millis(POST_TIMEOUT_MS))
        .send_json(body);
    match resp {
        Ok(r) => match r.into_json::<NoteNamesResponse>() {
            Ok(parsed) => {
                let mut cache = inner.cache.write().unwrap();
                for (k, v) in parsed.results {
                    if let Ok(p) = k.parse::<i32>() {
                        cache.insert((edo, p), Some(v));
                    }
                }
                for p in missing {
                    cache.entry((edo, p)).or_insert(None);
                }
                mark_success(inner);
            }
            Err(e) => mark_failure(inner, format!("xenharm parse error: {e}")),
        },
        Err(e) => mark_failure(inner, format!("xenharm unreachable: {}", short_err(&e))),
    }
}

fn handle_interval_request(inner: &Arc<Inner>, url: &str, edo: i32, steps: Vec<i32>) {
    let missing: Vec<i32> = {
        let cache = inner.interval_cache.read().unwrap();
        steps
            .into_iter()
            .filter(|s| !cache.contains_key(&(edo, *s)))
            .collect()
    };
    if missing.is_empty() {
        return;
    }
    let body = serde_json::json!({ "edo": edo, "steps": &missing });
    let resp = ureq::post(url)
        .timeout(Duration::from_millis(POST_TIMEOUT_MS))
        .send_json(body);
    match resp {
        Ok(r) => match r.into_json::<IntervalNamesResponse>() {
            Ok(parsed) => {
                let mut cache = inner.interval_cache.write().unwrap();
                for (k, v) in parsed.results {
                    if let Ok(s) = k.parse::<i32>() {
                        cache.insert((edo, s), Some(v));
                    }
                }
                for s in missing {
                    cache.entry((edo, s)).or_insert(None);
                }
                mark_success(inner);
            }
            Err(e) => mark_failure(inner, format!("xenharm interval parse error: {e}")),
        },
        Err(e) => mark_failure(
            inner,
            format!("xenharm interval unreachable: {}", short_err(&e)),
        ),
    }
}

/// Trim ureq's verbose multi-line errors down to one terse line so the
/// status string the frontend renders stays readable.
fn short_err(e: &ureq::Error) -> String {
    let s = e.to_string();
    s.lines().next().unwrap_or(&s).to_string()
}

fn mark_success(inner: &Arc<Inner>) {
    *inner.skip_until.write().unwrap() = None;
    let mut st = inner.status.write().unwrap();
    st.available = true;
    st.last_error = None;
    st.last_error_at_ms = None;
    inner.available.store(true, Ordering::Relaxed);
}

fn mark_failure(inner: &Arc<Inner>, msg: String) {
    *inner.skip_until.write().unwrap() =
        Some(Instant::now() + Duration::from_secs(FAILURE_BACKOFF_SECS));
    let mut st = inner.status.write().unwrap();
    st.last_error = Some(msg);
    st.last_error_at_ms = Some(now_ms());
    // We don't flip `available` to false — the service might just be slow
    // on this batch. Frontend reads `last_error` to decide what to show.
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unavailable_when_no_service() {
        // No service running on a high random port → probe should fail and
        // `is_available` should be false. `enqueue` and `names_for_state`
        // must not panic.
        let client = XenharmClient::start("http://127.0.0.1:1");
        assert!(!client.is_available());
        client.enqueue(31, vec![60, 61, 62]);
        let snap = LiveState::empty("exquis");
        assert!(client.names_for_state(&snap).is_empty());
    }
}
