//! Aftertouch and velocity mode enums — verbatim ports from xenwooting.
//!
//! Source: `C:\Dev-Free\xenwooting\xenwooting\src\bin\xenwooting.rs`
//!   - `VelocityProfile` + `apply()`: lines 541–575.
//!   - `AftertouchMode` + `name()`: lines 942–956.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AftertouchMode {
    SpeedMapped,
    PeakMapped,
    Off,
}

impl AftertouchMode {
    pub fn name(&self) -> &'static str {
        match self {
            AftertouchMode::SpeedMapped => "speed-mapped",
            AftertouchMode::PeakMapped => "peak-mapped",
            AftertouchMode::Off => "off",
        }
    }

    /// Cycle per xenwooting's RightAlt dispatch: SpeedMapped→PeakMapped→Off→SpeedMapped.
    pub fn next(self) -> Self {
        match self {
            AftertouchMode::SpeedMapped => AftertouchMode::PeakMapped,
            AftertouchMode::PeakMapped => AftertouchMode::Off,
            AftertouchMode::Off => AftertouchMode::SpeedMapped,
        }
    }
}

#[derive(Debug, Clone)]
pub enum VelocityProfile {
    Linear,
    Gamma { gamma: f32 },
    Log { k: f32 },
    InvLog { k: f32 },
}

impl VelocityProfile {
    pub fn name(&self) -> String {
        match self {
            VelocityProfile::Linear => "linear".to_string(),
            VelocityProfile::Gamma { gamma } => format!("gamma={}", gamma),
            VelocityProfile::Log { k } => format!("log k={}", k),
            VelocityProfile::InvLog { k } => format!("invlog k={}", k),
        }
    }

    pub fn apply(&self, n: f32) -> f32 {
        let n = n.clamp(0.0, 1.0);
        match self {
            VelocityProfile::Linear => n,
            VelocityProfile::Gamma { gamma } => n.powf(gamma.max(0.01)),
            VelocityProfile::Log { k } => {
                let kk = k.max(0.01);
                (1.0 + kk * n).ln() / (1.0 + kk).ln()
            }
            VelocityProfile::InvLog { k } => {
                let kk = k.max(0.01);
                let x = 1.0 - n;
                1.0 - (1.0 + kk * x).ln() / (1.0 + kk).ln()
            }
        }
    }
}

/// The 5 built-in velocity profiles, as used by xenwooting.rs (lines 1967–1972).
pub fn default_velocity_profiles() -> Vec<VelocityProfile> {
    vec![
        VelocityProfile::Linear,
        VelocityProfile::Gamma { gamma: 1.6 },
        VelocityProfile::Gamma { gamma: 0.7 },
        VelocityProfile::Log { k: 12.0 },
        VelocityProfile::InvLog { k: 12.0 },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aftertouch_cycle_rotates() {
        assert_eq!(AftertouchMode::SpeedMapped.next(), AftertouchMode::PeakMapped);
        assert_eq!(AftertouchMode::PeakMapped.next(), AftertouchMode::Off);
        assert_eq!(AftertouchMode::Off.next(), AftertouchMode::SpeedMapped);
    }

    #[test]
    fn velocity_linear_is_identity() {
        assert!((VelocityProfile::Linear.apply(0.0) - 0.0).abs() < 1e-6);
        assert!((VelocityProfile::Linear.apply(0.5) - 0.5).abs() < 1e-6);
        assert!((VelocityProfile::Linear.apply(1.0) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn velocity_gamma_matches_formula() {
        // Gamma{1.6}(0.5) = 0.5^1.6 ≈ 0.3299
        let g = VelocityProfile::Gamma { gamma: 1.6 };
        assert!((g.apply(0.5) - 0.5f32.powf(1.6)).abs() < 1e-6);
        // Gamma{0.7} darker end
        let g2 = VelocityProfile::Gamma { gamma: 0.7 };
        assert!((g2.apply(0.25) - 0.25f32.powf(0.7)).abs() < 1e-6);
    }

    #[test]
    fn velocity_log_invlog_endpoints() {
        for p in [VelocityProfile::Log { k: 12.0 }, VelocityProfile::InvLog { k: 12.0 }] {
            assert!((p.apply(0.0) - 0.0).abs() < 1e-6, "0 → 0 for {}", p.name());
            assert!((p.apply(1.0) - 1.0).abs() < 1e-6, "1 → 1 for {}", p.name());
        }
    }

    #[test]
    fn velocity_profiles_clamp_input() {
        // Out-of-range inputs must still produce valid 0..1 output.
        let p = VelocityProfile::Linear;
        assert_eq!(p.apply(-0.5), 0.0);
        assert_eq!(p.apply(2.0), 1.0);
    }

    #[test]
    fn default_profiles_count() {
        assert_eq!(default_velocity_profiles().len(), 5);
    }
}
