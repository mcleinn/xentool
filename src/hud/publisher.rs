//! Hot-loop-safe publisher for `LiveState`.
//!
//! Hot loops call [`HudPublisher::submit`] which performs a single atomic
//! pointer swap (`arc_swap.store(Arc::new(state))`). No locks held across
//! work, no JSON encoding, no I/O — JSON serialization happens on the SSE
//! thread when emitting events.
//!
//! [SSE thread]: ../server.rs

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use arc_swap::ArcSwap;

use super::state::LiveState;

/// Cheap-to-clone handle to the live snapshot. Hot loops own one handle and
/// only ever call `submit`. SSE handlers own a handle and only ever call
/// `snapshot`.
#[derive(Clone)]
pub struct HudPublisher {
    inner: Arc<Inner>,
}

struct Inner {
    state: ArcSwap<LiveState>,
    seq: AtomicU64,
}

impl HudPublisher {
    pub fn new(initial: LiveState) -> Self {
        Self {
            inner: Arc::new(Inner {
                state: ArcSwap::from_pointee(initial),
                seq: AtomicU64::new(0),
            }),
        }
    }

    /// Publish a new snapshot. Stamps `seq` (monotonic) and `ts_ms` (host
    /// wall clock) before storing. Wait-free for the caller.
    pub fn submit(&self, mut state: LiveState) {
        state.seq = self.inner.seq.fetch_add(1, Ordering::Relaxed) + 1;
        state.ts_ms = now_ms();
        self.inner.state.store(Arc::new(state));
    }

    /// Read the latest snapshot. Lock-free; cheap to call from any thread.
    pub fn snapshot(&self) -> Arc<LiveState> {
        self.inner.state.load_full()
    }

    /// Returns the current monotonic sequence counter. Useful for SSE
    /// handlers to skip emitting duplicates.
    pub fn current_seq(&self) -> u64 {
        self.inner.seq.load(Ordering::Relaxed)
    }
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
    use std::sync::Barrier;
    use std::thread;

    #[test]
    fn submit_advances_seq_and_updates_snapshot() {
        let pub_ = HudPublisher::new(LiveState::empty("exquis"));
        assert_eq!(pub_.snapshot().seq, 0);

        let mut s = LiveState::empty("exquis");
        s.layout.edo = 31;
        pub_.submit(s);

        let snap = pub_.snapshot();
        assert_eq!(snap.seq, 1);
        assert_eq!(snap.layout.edo, 31);
        assert!(snap.ts_ms > 0);

        pub_.submit(LiveState::empty("exquis"));
        assert_eq!(pub_.snapshot().seq, 2);
    }

    #[test]
    fn submit_is_concurrent_safe() {
        let pub_ = HudPublisher::new(LiveState::empty("wooting"));
        let n_threads = 4;
        let n_per = 500;
        let barrier = Arc::new(Barrier::new(n_threads));

        let handles: Vec<_> = (0..n_threads)
            .map(|_| {
                let p = pub_.clone();
                let b = Arc::clone(&barrier);
                thread::spawn(move || {
                    b.wait();
                    for _ in 0..n_per {
                        p.submit(LiveState::empty("wooting"));
                    }
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }

        let total = (n_threads * n_per) as u64;
        assert_eq!(pub_.current_seq(), total);
        assert_eq!(pub_.snapshot().seq, total);
    }

    #[test]
    fn snapshot_serializes_to_json() {
        let pub_ = HudPublisher::new(LiveState::empty("exquis"));
        let mut s = LiveState::empty("exquis");
        s.layout.id = "edo31".into();
        s.layout.name = "31-EDO".into();
        s.layout.edo = 31;
        s.pressed.insert("board0".into(), vec![60, 64, 67]);
        s.layout_pitches
            .insert("board0".into(), vec![Some(0), None, Some(2)]);
        pub_.submit(s);

        let snap = pub_.snapshot();
        let json = serde_json::to_string(&*snap).expect("serializes");
        assert!(json.contains("\"backend\":\"exquis\""));
        assert!(json.contains("\"edo\":31"));
        assert!(json.contains("\"board0\":[60,64,67]"));
        assert!(json.contains("\"layout_pitches\""));
        // press_threshold is None for Exquis — must be skipped.
        assert!(!json.contains("press_threshold"));
    }
}
