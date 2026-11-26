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
    Held { peak: f32, peak_speed: f32, at_level: f32, last_pressure: u8, since_us: u64, last_sent_us: u64 },
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
                // Re-arm after a release: below the threshold, or `release_delta` deeper again.
                // With aftertouch on, re-strike at once while still above the threshold, as
                // the Wooting backend does: lifting a held key gives a quick staircase of new
                // notes at the current (lower) depth, which a long synth release blends into
                // a smooth fade ("pumping").
                let armed = *valley < p.press_threshold
                    || travel - *valley >= p.release_delta
                    || matches!(p.aftertouch_mode, AftertouchMode::PeakMapped | AftertouchMode::SpeedMapped);
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
                // Abandoned before note-on: back below threshold - release_delta, or near the
                // top (half the threshold). The second rule matters when release_delta >=
                // press_threshold (e.g. 0.12 / 0.10): without it a single noise spike above
                // the threshold could never be abandoned and fired a note 6 ms later that
                // ended on the next scan (ghost note; SuperCollider kept such voices forever).
                if travel < p.press_threshold - p.release_delta || travel < p.press_threshold * 0.5 {
                    self.state = State::Idle { valley: travel };
                    return None;
                }
                if now_us.saturating_sub(*started_us) >= p.velocity_window_us {
                    let raw = if p.aftertouch_mode == AftertouchMode::Depth {
                        depth_level(*peak, p)        // start from the depth: a note can swell from silence
                    } else {
                        match p.velocity_source {
                            VelocitySource::Speed => *peak_speed / p.velocity_speed_max.max(0.001),
                            VelocitySource::Depth => *peak,
                        }
                    };
                    let shaped = profile.apply(raw.clamp(0.0, 1.0));
                    let velocity = (shaped * 126.0).round().clamp(0.0, 126.0) as u8 + 1;
                    self.state = State::Held {
                        peak: *peak,
                        peak_speed: *peak_speed,
                        at_level: 0.0,
                        last_pressure: 0,
                        since_us: now_us,
                        last_sent_us: now_us,
                    };
                    return Some(Action::NoteOn { velocity });
                }
                None
            }
            State::Held { peak, peak_speed, at_level, last_pressure, last_sent_us, .. } => {
                *peak = peak.max(travel);
                *peak_speed = peak_speed.max(speed);
                // Release: the key came up `release_delta` from its deepest point, or it is
                // back near the top (below half the press threshold). The second rule keeps a
                // light press that never went `release_delta` past the threshold from hanging.
                let near_top = travel < p.press_threshold * 0.5;
                let continuous = p.aftertouch_mode == AftertouchMode::Depth;
                if near_top || (!continuous && travel + p.release_delta < *peak) {
                    self.state = State::Idle { valley: travel };
                    return Some(Action::NoteOff);
                }
                if continuous {
                    // pressure follows the current depth both ways
                    // average of the recent scans (~3 ms at 5 kHz) against sensor noise
                    let smooth = self.hist[..self.len].iter().map(|&(_, tr)| tr).sum::<f32>() / self.len.max(1) as f32;
                    let v = (profile.apply(depth_level(smooth, p)) * 127.0).round().clamp(0.0, 127.0) as u8;
                    *at_level = v as f32 / 127.0;
                    // Follows the key both ways, so sensor noise would toggle the value between
                    // neighbours on every scan (~2500 messages/s per held key, enough to make
                    // the synth drop note-offs). Send at most every DEPTH_PRESSURE_MIN_US and
                    // only for a change of at least DEPTH_PRESSURE_DEADBAND (end values always).
                    let step = ((p.aftertouch_delta * 127.0).round() as u32).max(DEPTH_PRESSURE_DEADBAND);
                    let due = now_us.saturating_sub(*last_sent_us) >= DEPTH_PRESSURE_MIN_US;
                    let moved = (v as i32 - *last_pressure as i32).unsigned_abs() >= step;
                    let end = v != *last_pressure && (v == 127 || v == 0);
                    if due && (moved || end) {
                        *last_pressure = v;
                        *last_sent_us = now_us;
                        return Some(Action::Pressure { value: v });
                    }
                    return None;
                }
                let raw = match p.aftertouch_mode {
                    AftertouchMode::Off | AftertouchMode::Depth => return None,
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

/// Depth mode: minimum time between two pressure messages of one key (250 per second max).
const DEPTH_PRESSURE_MIN_US: u64 = 4_000;
/// Depth mode: smallest pressure change that is sent (out of 127); hides +-1 noise jitter.
const DEPTH_PRESSURE_DEADBAND: u32 = 2;

/// Travel above the press threshold, 0 at the threshold .. 1 at the bottom.
fn depth_level(travel: f32, p: &KeyParams) -> f32 {
    ((travel - p.press_threshold) / (1.0 - p.press_threshold).max(0.001)).clamp(0.0, 1.0)
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
    fn aftertouch_mode_restrikes_while_lifting_like_wooting() {
        // Wooting position-mode settings: 10 % threshold, release 0.12, depth velocity, 6 ms.
        // Down to 90 %, then lift slowly to 0: a staircase of notes with falling velocity.
        let p = KeyParams {
            press_threshold: 0.10, release_delta: 0.12, velocity_window_us: 6_000,
            velocity_source: VelocitySource::Depth, aftertouch_mode: AftertouchMode::PeakMapped,
            ..KeyParams::default()
        };
        let mut m = KeyMachine::default();
        let acts = run(&mut m, &p, 0, 1_000_000, |t| {
            let ms = t as f32 / 1000.0;
            if ms < 20.0 { 0.045 * ms } else if ms < 100.0 { 0.90 } else { (0.90 - 0.0012 * (ms - 100.0)).max(0.0) }
        });
        let vels: Vec<u8> = acts.iter().filter_map(|(_, a)| match a { Action::NoteOn { velocity } => Some(*velocity), _ => None }).collect();
        assert!(vels.len() >= 5, "expected a staircase of notes, got {vels:?}");
        assert!(vels.windows(2).skip(1).all(|w| w[1] < w[0]), "velocities should fall while lifting: {vels:?}");
        assert!(matches!(acts.last(), Some((_, Action::NoteOff))), "{acts:?}");
        assert!(!m.is_held());
    }

    #[test]
    fn without_aftertouch_lifting_does_not_restrike() {
        let p = KeyParams { press_threshold: 0.10, release_delta: 0.12, ..KeyParams::default() };
        let mut m = KeyMachine::default();
        let acts = run(&mut m, &p, 0, 1_000_000, |t| {
            let ms = t as f32 / 1000.0;
            if ms < 20.0 { 0.045 * ms } else if ms < 100.0 { 0.90 } else { (0.90 - 0.0012 * (ms - 100.0)).max(0.0) }
        });
        let ons = acts.iter().filter(|(_, a)| matches!(a, Action::NoteOn { .. })).count();
        assert_eq!(ons, 1, "{acts:?}");
    }

    #[test]
    fn depth_mode_pumps_one_continuous_note() {
        // press to 90 %, pump between 30 % and 90 % three times, then let go
        let p = KeyParams {
            press_threshold: 0.10, release_delta: 0.12, aftertouch_mode: AftertouchMode::Depth,
            ..KeyParams::default()
        };
        let mut m = KeyMachine::default();
        let acts = run(&mut m, &p, 0, 700_000, |t| {
            let ms = t as f32 / 1000.0;
            if ms < 50.0 { 0.018 * ms }
            else if ms < 650.0 { 0.60 + 0.30 * ((ms - 50.0) / 200.0 * std::f32::consts::TAU).cos() }
            else { (0.90 - 0.05 * (ms - 650.0)).max(0.0) }
        });
        let ons = acts.iter().filter(|(_, a)| matches!(a, Action::NoteOn { .. })).count();
        let offs: Vec<u64> = acts.iter().filter(|(_, a)| matches!(a, Action::NoteOff)).map(|(t, _)| *t).collect();
        assert_eq!(ons, 1, "{acts:?}");
        assert_eq!(offs.len(), 1, "{acts:?}");
        assert!(offs[0] > 650_000, "note must only end on the final release");
        let pr: Vec<u8> = acts.iter().filter_map(|(_, a)| match a { Action::Pressure { value } => Some(*value), _ => None }).collect();
        let (lo, hi) = (*pr.iter().min().unwrap(), *pr.iter().max().unwrap());
        assert!(hi >= 110 && lo <= 40, "pressure should swing with the pumping: {lo}..{hi}");
        assert!(pr.windows(2).filter(|w| w[1] < w[0]).count() > 5 && pr.windows(2).filter(|w| w[1] > w[0]).count() > 5);
    }

    #[test]
    fn depth_mode_pressure_rate_with_sensor_noise() {
        // a key held still at 50 % with realistic rest noise (sigma 0.3 % of travel), 5 kHz scans
        let p = KeyParams { press_threshold: 0.10, aftertouch_mode: AftertouchMode::Depth, ..KeyParams::default() };
        let mut m = KeyMachine::default();
        let mut seed: u32 = 12345;
        let mut noise = move || {
            let mut s = 0.0f32;
            for _ in 0..12 { seed = seed.wrapping_mul(1664525).wrapping_add(1013904223); s += (seed >> 8) as f32 / 16_777_216.0; }
            (s - 6.0) * 0.003
        };
        let mut t = 0u64; let mut n_pressure = 0;
        while t < 1_200_000 {
            let base = if t < 20_000 { t as f32 / 40_000.0 } else { 0.5 };
            if let Some(Action::Pressure { .. }) = m.step(t, base + noise(), &p, &VelocityProfile::Linear) {
                if t >= 200_000 { n_pressure += 1; }
            }
            t += 200;
        }
        eprintln!("pressure messages in 1 s of holding still: {n_pressure}");
        assert!(n_pressure <= 20, "{n_pressure} aftertouch messages per second for one key held still");
    }

    #[test]
    fn single_noise_spike_never_fires_a_note() {
        // threshold 10 %, release_delta 12 % (Wooting-like); one scan at 12 % then rest
        for mode in [AftertouchMode::Depth, AftertouchMode::PeakMapped, AftertouchMode::Off] {
            let p = KeyParams {
                press_threshold: 0.10, release_delta: 0.12, velocity_window_us: 6_000,
                aftertouch_mode: mode, ..KeyParams::default()
            };
            let mut m = KeyMachine::default();
            let acts = run(&mut m, &p, 0, 50_000, |t| if (10_000..10_200).contains(&t) { 0.12 } else { 0.0 });
            assert!(acts.is_empty(), "{mode:?}: {acts:?}");
        }
    }

    #[test]
    fn depth_mode_note_starts_quiet_on_a_slow_press() {
        let p = KeyParams { press_threshold: 0.10, aftertouch_mode: AftertouchMode::Depth, ..KeyParams::default() };
        let mut m = KeyMachine::default();
        let acts = run(&mut m, &p, 0, 100_000, |t| 0.01 * (t as f32 / 1000.0));   // 1 % per ms
        assert!(velocity_of(&acts) <= 10, "{acts:?}");
    }

    #[test]
    fn light_press_shallower_than_release_delta_still_releases() {
        // press 10 %, release_delta 20 %: a press peaking at 17 % used to hang forever
        let p = KeyParams { press_threshold: 0.10, release_delta: 0.20, ..KeyParams::default() };
        let mut m = KeyMachine::default();
        let acts = run(&mut m, &p, 0, 60_000, |t| {
            let ms = t as f32 / 1000.0;
            if ms < 10.0 { 0.017 * ms } else if ms < 30.0 { 0.17 } else { (0.17 - 0.017 * (ms - 30.0)).max(0.0) }
        });
        assert!(matches!(acts.first(), Some((_, Action::NoteOn { .. }))), "{acts:?}");
        assert!(matches!(acts.last(), Some((_, Action::NoteOff))), "{acts:?}");
        assert!(!m.is_held());
    }

    #[test]
    fn hovering_light_press_does_not_retrigger() {
        // finger wobble of +-5 % around 30 % travel with release_delta 20 %: one note only
        let p = KeyParams { press_threshold: 0.10, release_delta: 0.20, ..KeyParams::default() };
        let mut m = KeyMachine::default();
        let acts = run(&mut m, &p, 0, 200_000, |t| {
            let ms = t as f32 / 1000.0;
            if ms < 10.0 { 0.03 * ms } else { 0.30 + 0.05 * (ms / 15.0).sin() }
        });
        let ons = acts.iter().filter(|(_, a)| matches!(a, Action::NoteOn { .. })).count();
        assert_eq!(ons, 1, "{acts:?}");
        assert!(m.is_held());
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
