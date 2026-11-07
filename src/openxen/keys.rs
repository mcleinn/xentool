//! Per-key state machine for openxen — the Wooting `step_musical_key` /
//! `emit_peak_note_ons` logic, driven by the module's own timestamps instead of
//! host arrival times (the stream carries every scan at ~4 kHz with a µs clock).
//!
//! Pure: `KeyMachine::step` takes one sample and returns at most one action; the serve
//! loop maps actions to MIDI. This keeps the velocity logic unit-testable.
//!
//! States (as in the Wooting backend):
//! - `Idle`     — waiting for travel ≥ press threshold. After a release the key re-arms
//!   either by falling below the threshold or by rising `release_delta` above the valley
//!   (rapid re-strike without full release).
//! - `Tracking` — threshold crossed; collects peak depth and peak speed for
//!   `velocity_window_us` of device time, then emits NoteOn.
//! - `Held`     — note sounding; releases when travel falls `release_delta` below its peak;
//!   emits polyphonic aftertouch per `AftertouchMode`.

use crate::wooting::modes::{AftertouchMode, VelocityProfile};

/// Where the note-on velocity comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VelocitySource {
    /// Peak key speed (travel per second) during the velocity window — what a piano
    /// player means by velocity. Needs the device timestamps.
    Speed,
    /// Peak depth during the window (the Wooting backend's method).
    Depth,
}

impl VelocitySource {
    pub fn name(self) -> &'static str {
        match self {
            VelocitySource::Speed => "speed",
            VelocitySource::Depth => "depth",
        }
    }
}

#[derive(Debug, Clone)]
pub struct KeyParams {
    pub press_threshold: f32,
    pub release_delta: f32,
    pub velocity_window_us: u64,
    pub velocity_source: VelocitySource,
    /// Speed (travel/s) that maps to full velocity.
    pub velocity_speed_max: f32,
    /// Time span over which speed is measured (smooths ADC noise).
    pub speed_span_us: u64,
    pub aftertouch_mode: AftertouchMode,
    pub aftertouch_delta: f32,
    pub aftertouch_speed_max: f32,
}

impl Default for KeyParams {
    fn default() -> Self {
        Self {
            press_threshold: 0.15,
            release_delta: 0.08,
            velocity_window_us: 3_000,
            velocity_source: VelocitySource::Speed,
            velocity_speed_max: 60.0,
            speed_span_us: 1_000,
            aftertouch_mode: AftertouchMode::Off,
            aftertouch_delta: 0.01,
            aftertouch_speed_max: 100.0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    NoteOn { velocity: u8 },
    NoteOff,
    Pressure { value: u8 },
}

#[derive(Debug, Clone, Copy)]
enum State {
    Idle { valley: f32 },
    Tracking { started_us: u64, peak: f32, peak_speed: f32 },
    Held { peak: f32, peak_speed: f32, at_level: f32, last_pressure: u8, since_us: u64 },
}

const HIST: usize = 16;

#[derive(Debug, Clone)]
pub struct KeyMachine {
    state: State,
    hist: [(u64, f32); HIST],
    len: usize,
    pos: usize,
}

impl Default for KeyMachine {
    fn default() -> Self {
        Self { state: State::Idle { valley: 0.0 }, hist: [(0, 0.0); HIST], len: 0, pos: 0 }
    }
}

impl KeyMachine {
    pub fn is_held(&self) -> bool {
        matches!(self.state, State::Held { .. })
    }

    pub fn held_since_us(&self) -> Option<u64> {
        match self.state {
            State::Held { since_us, .. } => Some(since_us),
            _ => None,
        }
    }

    pub fn last_pressure(&self) -> u8 {
        match self.state {
            State::Held { last_pressure, .. } => last_pressure,
            _ => 0,
        }
    }

    /// Forget any press in progress (layout change, device loss). Returns NoteOff if a note
    /// was sounding.
    pub fn reset(&mut self) -> Option<Action> {
        let was_held = self.is_held();
        *self = Self::default();
        was_held.then_some(Action::NoteOff)
    }

    /// Speed over at least `span_us` (oldest sample within the history that is that old,
    /// else the oldest available), travel per second.
    fn speed(&self, now_us: u64, travel: f32, span_us: u64) -> f32 {
        if self.len == 0 {
            return 0.0;
        }
        let mut best: Option<(u64, f32)> = None;
        for i in 0..self.len {
            let (ts, tr) = self.hist[(self.pos + HIST - 1 - i) % HIST];
            best = Some((ts, tr));
            if now_us.saturating_sub(ts) >= span_us {
                break;
            }
        }
        let (ts, tr) = best.unwrap();
        let dt = now_us.saturating_sub(ts);
        if dt == 0 {
            return 0.0;
        }
        ((travel - tr) / (dt as f32 * 1e-6)).max(0.0)
    }

    fn push(&mut self, now_us: u64, travel: f32) {
        self.hist[self.pos] = (now_us, travel);
        self.pos = (self.pos + 1) % HIST;
        self.len = (self.len + 1).min(HIST);
    }

    /// Feed one sample (device time in µs, travel 0..1). NaN travel (key not calibrated
    /// yet) is ignored.
    pub fn step(&mut self, now_us: u64, travel: f32, p: &KeyParams, profile: &VelocityProfile) -> Option<Action> {
        if !travel.is_finite() {
            return None;
        }
        let speed = self.speed(now_us, travel, p.speed_span_us);
        self.push(now_us, travel);
        match &mut self.state {
            State::Idle { valley } => {
                let armed = *valley < p.press_threshold || travel - *valley >= p.release_delta;
                if travel >= p.press_threshold && armed {
                    self.state = State::Tracking { started_us: now_us, peak: travel, peak_speed: speed };
                } else {
                    *valley = valley.min(travel);
                }
                None
            }
            State::Tracking { started_us, peak, peak_speed } => {
                *peak = peak.max(travel);
                *peak_speed = peak_speed.max(speed);
                if travel < p.press_threshold - p.release_delta {
                    self.state = State::Idle { valley: travel };       // abandoned before note-on
                    return None;
                }
                if now_us.saturating_sub(*started_us) >= p.velocity_window_us {
                    let raw = match p.velocity_source {
                        VelocitySource::Speed => *peak_speed / p.velocity_speed_max.max(0.001),
                        VelocitySource::Depth => *peak,
                    };
                    let shaped = profile.apply(raw.clamp(0.0, 1.0));
                    let velocity = (shaped * 126.0).round().clamp(0.0, 126.0) as u8 + 1;
                    self.state = State::Held {
                        peak: *peak,
                        peak_speed: *peak_speed,
                        at_level: 0.0,
                        last_pressure: 0,
                        since_us: now_us,
                    };
                    return Some(Action::NoteOn { velocity });
                }
                None
            }
            State::Held { peak, peak_speed, at_level, last_pressure, .. } => {
                *peak = peak.max(travel);
                *peak_speed = peak_speed.max(speed);
                if travel + p.release_delta < *peak {
                    self.state = State::Idle { valley: travel };
                    return Some(Action::NoteOff);
                }
                let raw = match p.aftertouch_mode {
                    AftertouchMode::Off => return None,
                    AftertouchMode::SpeedMapped => (*peak_speed / p.aftertouch_speed_max.max(0.001)).clamp(0.0, 1.0),
                    AftertouchMode::PeakMapped => {
                        let denom = (1.0 - p.press_threshold).max(0.001);
                        ((*peak - p.press_threshold) / denom).clamp(0.0, 1.0)
                    }
                };
                let shaped = profile.apply(raw);
                if shaped > *at_level {
                    *at_level = shaped;
                }
                let v = (at_level.clamp(0.0, 1.0) * 127.0).round() as u8;
                let step = ((p.aftertouch_delta * 127.0).round() as u32).max(1);
                if (v as i32 - *last_pressure as i32).unsigned_abs() >= step
                    || (v != *last_pressure && (v == 127 || v == 0))
                {
                    *last_pressure = v;
                    return Some(Action::Pressure { value: v });
                }
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCAN_US: u64 = 250; // ~4 kHz

    /// Run a travel profile (function of time in µs) for `dur_us` at the scan rate and
    /// collect (time, action) pairs.
    fn run(m: &mut KeyMachine, p: &KeyParams, t0: u64, dur_us: u64, f: impl Fn(u64) -> f32) -> Vec<(u64, Action)> {
        let mut out = Vec::new();
        let mut t = t0;
        while t <= t0 + dur_us {
            if let Some(a) = m.step(t, f(t - t0), p, &VelocityProfile::Linear) {
                out.push((t, a));
            }
            t += SCAN_US;
        }
        out
    }

    fn strike(full_travel_ms: f32) -> impl Fn(u64) -> f32 {
        move |t| ((t as f32 / 1000.0) / full_travel_ms).min(1.0)
    }

    fn velocity_of(actions: &[(u64, Action)]) -> u8 {
        actions.iter().find_map(|(_, a)| match a { Action::NoteOn { velocity } => Some(*velocity), _ => None }).unwrap()
    }

    #[test]
    fn fast_strike_is_loud_slow_press_is_soft() {
        let p = KeyParams::default();
        // full travel in 10 ms = 100 travel/s >= speed max 60 -> velocity 127
        let fast = run(&mut KeyMachine::default(), &p, 0, 30_000, strike(10.0));
        assert_eq!(velocity_of(&fast), 127);
        // full travel in 100 ms = 10 travel/s -> about 10/60 of full scale
        let slow = run(&mut KeyMachine::default(), &p, 0, 200_000, strike(100.0));
        let v = velocity_of(&slow) as f32;
        assert!((v - (1.0 + 126.0 * 10.0 / 60.0)).abs() <= 2.0, "slow velocity {v}");
    }

    #[test]
    fn velocity_is_monotonic_in_strike_speed() {
        let p = KeyParams::default();
        let mut last = 0;
        for ms in [200.0, 100.0, 50.0, 30.0, 20.0, 15.0] {
            let v = velocity_of(&run(&mut KeyMachine::default(), &p, 0, 400_000, strike(ms)));
            assert!(v >= last, "{ms} ms -> {v} after {last}");
            last = v;
        }
    }

    #[test]
    fn note_on_after_window_and_off_on_release() {
        let p = KeyParams::default();
        let mut m = KeyMachine::default();
        let on = run(&mut m, &p, 0, 30_000, strike(10.0));
        assert_eq!(on.len(), 1);
        // threshold 0.15 reached at 1.5 ms; window 3 ms -> note-on at ~4.5 ms device time
        assert!((4_250..=5_000).contains(&on[0].0), "note-on at {} us", on[0].0);
        assert!(m.is_held());
        let off = run(&mut m, &p, 30_250, 20_000, |t| (1.0 - t as f32 / 10_000.0).max(0.0));
        assert_eq!(off.iter().filter(|(_, a)| *a == Action::NoteOff).count(), 1);
        assert!(!m.is_held());
    }

    #[test]
    fn noise_at_rest_never_triggers() {
        let p = KeyParams::default();
        let mut m = KeyMachine::default();
        // +-0.03 travel jitter (about 45 ADC counts on a 1500-count swing)
        let acts = run(&mut m, &p, 0, 2_000_000, |t| 0.05 + 0.03 * ((t / 250 % 7) as f32 / 3.0 - 1.0));
        assert!(acts.is_empty());
    }

    #[test]
    fn abandoned_press_emits_nothing() {
        let p = KeyParams::default();
        let mut m = KeyMachine::default();
        // touches 0.2 for 1 ms then falls back before the 3 ms window ends
        let acts = run(&mut m, &p, 0, 20_000, |t| if t < 1_000 { 0.2 } else { 0.0 });
        assert!(acts.is_empty() && !m.is_held());
    }

    #[test]
    fn rapid_restrike_without_full_release() {
        let p = KeyParams::default();
        let mut m = KeyMachine::default();
        run(&mut m, &p, 0, 30_000, strike(10.0));
        // lift to 0.6 (released: 0.4 below peak), then press again to 1.0
        let acts = run(&mut m, &p, 30_250, 40_000, |t| {
            if t < 10_000 { 1.0 - 0.4 * t as f32 / 10_000.0 } else { (0.6 + 0.4 * (t - 10_000) as f32 / 5_000.0).min(1.0) }
        });
        let kinds: Vec<&Action> = acts.iter().map(|(_, a)| a).collect();
        assert_eq!(kinds.len(), 2);
        assert_eq!(*kinds[0], Action::NoteOff);
        assert!(matches!(kinds[1], Action::NoteOn { .. }));
    }

    #[test]
    fn depth_source_matches_wooting_behaviour() {
        let p = KeyParams { velocity_source: VelocitySource::Depth, ..KeyParams::default() };
        // slow press that reaches only 0.5 within the window
        let acts = run(&mut KeyMachine::default(), &p, 0, 40_000, |t| (t as f32 / 1000.0 * 0.12).min(0.5));
        let v = velocity_of(&acts);
        // peak depth at note-on time: crossing at 1.25 ms, +3 ms window -> ~0.51 capped at 0.5
        assert!((v as f32 - (1.0 + 126.0 * 0.5)).abs() <= 2.0, "depth velocity {v}");
    }

    #[test]
    fn peak_mapped_aftertouch_rises_with_depth() {
        let p = KeyParams { aftertouch_mode: AftertouchMode::PeakMapped, ..KeyParams::default() };
        let mut m = KeyMachine::default();
        let acts = run(&mut m, &p, 0, 60_000, |t| (t as f32 / 40_000.0).min(1.0));
        let pressures: Vec<u8> = acts.iter().filter_map(|(_, a)| match a { Action::Pressure { value } => Some(*value), _ => None }).collect();
        assert!(!pressures.is_empty());
        assert!(pressures.windows(2).all(|w| w[1] > w[0]));
        assert_eq!(*pressures.last().unwrap(), 127);
    }

    #[test]
    fn uncalibrated_samples_are_ignored_and_reset_releases() {
        let p = KeyParams::default();
        let mut m = KeyMachine::default();
        assert!(m.step(0, f32::NAN, &p, &VelocityProfile::Linear).is_none());
        run(&mut m, &p, 250, 30_000, strike(10.0));
        assert_eq!(m.reset(), Some(Action::NoteOff));
        assert_eq!(m.reset(), None);
    }

    #[test]
    fn lost_scans_do_not_inflate_speed() {
        // Same 10 ms strike, but every other scan missing: device timestamps keep the speed right.
        let p = KeyParams::default();
        let mut m = KeyMachine::default();
        let mut v = None;
        let mut t = 0;
        while t <= 30_000 {
            if (t / SCAN_US) % 2 == 0 {
                if let Some(Action::NoteOn { velocity }) = m.step(t, strike(20.0)(t), &p, &VelocityProfile::Linear) {
                    v = Some(velocity);
                }
            }
            t += SCAN_US;
        }
        // 20 ms full travel = 50 travel/s -> 50/60 of full scale
        let v = v.unwrap() as f32;
        assert!((v - (1.0 + 126.0 * 50.0 / 60.0)).abs() <= 2.0, "velocity {v}");
    }
}
