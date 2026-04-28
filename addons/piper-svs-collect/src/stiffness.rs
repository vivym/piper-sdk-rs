use thiserror::Error;

use crate::profile::{ContactProfile, CueProfile, EffectiveProfile, PhiProfile, StiffnessProfile};

const PHI_MODE_SIGNED: &str = "signed";
const PHI_MODE_ABSOLUTE: &str = "absolute";
const PHI_MODE_POSITIVE: &str = "positive";
const PHI_MODE_NEGATIVE: &str = "negative";
const DEFAULT_LOOP_FREQUENCY_HZ: f64 = 200.0;

#[derive(Debug, Error, Clone, PartialEq)]
pub enum StiffnessError {
    #[error("invalid stiffness profile: {0}")]
    InvalidProfile(String),
    #[error("{field} contains a non-finite value")]
    NonFiniteInput { field: &'static str },
    #[error("dt_us must be positive")]
    InvalidDt,
    #[error("unsupported phi mode: {0}")]
    UnsupportedPhiMode(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SvsContactState {
    Free,
    Contact,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SvsStiffnessOutput {
    pub contact_state: SvsContactState,
    pub k_state_raw_n_per_m: [f64; 3],
    pub k_state_clipped_n_per_m: [f64; 3],
    pub k_tele_n_per_m: [f64; 3],
}

#[derive(Debug, Clone)]
pub struct SvsStiffnessState {
    stiffness: StiffnessProfile,
    contact: ContactProfile,
    cue: CueProfile,
    min_hold_ticks: u64,
    contact_state: SvsContactState,
    enter_ticks: u64,
    exit_ticks: u64,
    k_lpf_state_n_per_m: [f64; 3],
    previous_k_tele_n_per_m: [f64; 3],
}

impl SvsContactState {
    pub fn as_u8(self) -> u8 {
        match self {
            Self::Free => 0,
            Self::Contact => 1,
        }
    }
}

impl SvsStiffnessState {
    pub fn new(stiffness: &StiffnessProfile) -> Result<Self, StiffnessError> {
        Self::with_profiles(
            stiffness,
            &ContactProfile::default(),
            &CueProfile::default(),
            DEFAULT_LOOP_FREQUENCY_HZ,
        )
    }

    pub fn from_effective_profile(profile: &EffectiveProfile) -> Result<Self, StiffnessError> {
        profile
            .validate()
            .map_err(|err| StiffnessError::InvalidProfile(err.to_string()))?;
        Self::with_profiles(
            &profile.stiffness,
            &profile.contact,
            &profile.cue,
            profile.control.loop_frequency_hz,
        )
    }

    pub fn with_profiles(
        stiffness: &StiffnessProfile,
        contact: &ContactProfile,
        cue: &CueProfile,
        loop_frequency_hz: f64,
    ) -> Result<Self, StiffnessError> {
        stiffness
            .validate()
            .map_err(|err| StiffnessError::InvalidProfile(err.to_string()))?;
        contact
            .validate()
            .map_err(|err| StiffnessError::InvalidProfile(err.to_string()))?;
        cue.validate().map_err(|err| StiffnessError::InvalidProfile(err.to_string()))?;
        validate_loop_frequency(loop_frequency_hz)?;

        let min_hold_ticks =
            ((contact.min_hold_ms as f64 * loop_frequency_hz) / 1000.0).ceil().max(1.0) as u64;
        let initial_k = clip_vec3(stiffness.k_base_free, stiffness.k_min, stiffness.k_max);

        Ok(Self {
            stiffness: stiffness.clone(),
            contact: contact.clone(),
            cue: cue.clone(),
            min_hold_ticks,
            contact_state: SvsContactState::Free,
            enter_ticks: 0,
            exit_ticks: 0,
            k_lpf_state_n_per_m: initial_k,
            previous_k_tele_n_per_m: initial_k,
        })
    }

    pub fn update(
        &mut self,
        u_ee: [f64; 3],
        r_ee: [f64; 3],
        dt_us: u64,
    ) -> Result<SvsStiffnessOutput, StiffnessError> {
        if dt_us == 0 {
            return Err(StiffnessError::InvalidDt);
        }
        validate_finite_array3("u_ee", &u_ee)?;
        validate_finite_array3("r_ee", &r_ee)?;

        let (contact_state, enter_ticks, exit_ticks) = self.next_contact_state(norm3(r_ee));

        let phi_u = phi_vec3(u_ee, &self.cue.master_phi)?;
        let phi_r = phi_vec3(r_ee, &self.cue.slave_phi)?;
        let base = match contact_state {
            SvsContactState::Free => self.stiffness.k_base_free,
            SvsContactState::Contact => self.stiffness.k_base_contact,
        };
        let k_state_raw_n_per_m = add3(
            base,
            add3(
                mat3_vec3(self.cue.w_u, phi_u),
                mat3_vec3(self.cue.w_r, phi_r),
            ),
        );
        validate_finite_array3("k_state_raw_n_per_m", &k_state_raw_n_per_m)?;

        let k_state_clipped_n_per_m = clip_vec3(
            k_state_raw_n_per_m,
            self.stiffness.k_min,
            self.stiffness.k_max,
        );
        let dt_sec = dt_us as f64 / 1_000_000.0;
        let k_lpf = lpf_vec3(
            self.k_lpf_state_n_per_m,
            k_state_clipped_n_per_m,
            self.stiffness.lpf_cutoff_hz,
            dt_sec,
        );
        validate_finite_array3("k_lpf_n_per_m", &k_lpf)?;
        let k_tele_n_per_m = rate_limit_vec3(
            self.previous_k_tele_n_per_m,
            k_lpf,
            self.stiffness.max_delta_per_second,
            dt_sec,
            self.stiffness.k_min,
            self.stiffness.k_max,
        );
        validate_finite_array3("k_tele_n_per_m", &k_tele_n_per_m)?;
        self.contact_state = contact_state;
        self.enter_ticks = enter_ticks;
        self.exit_ticks = exit_ticks;
        self.k_lpf_state_n_per_m = k_lpf;
        self.previous_k_tele_n_per_m = k_tele_n_per_m;

        Ok(SvsStiffnessOutput {
            contact_state,
            k_state_raw_n_per_m,
            k_state_clipped_n_per_m,
            k_tele_n_per_m,
        })
    }

    fn next_contact_state(&self, residual_norm: f64) -> (SvsContactState, u64, u64) {
        match self.contact_state {
            SvsContactState::Free => {
                let mut enter_ticks = self.enter_ticks;
                if residual_norm >= self.contact.residual_enter {
                    enter_ticks = enter_ticks.saturating_add(1);
                } else {
                    enter_ticks = 0;
                }

                if enter_ticks >= self.min_hold_ticks {
                    (SvsContactState::Contact, 0, 0)
                } else {
                    (SvsContactState::Free, enter_ticks, self.exit_ticks)
                }
            },
            SvsContactState::Contact => {
                let mut exit_ticks = self.exit_ticks;
                if residual_norm <= self.contact.residual_exit {
                    exit_ticks = exit_ticks.saturating_add(1);
                } else {
                    exit_ticks = 0;
                }

                if exit_ticks >= self.min_hold_ticks {
                    (SvsContactState::Free, 0, 0)
                } else {
                    (SvsContactState::Contact, self.enter_ticks, exit_ticks)
                }
            },
        }
    }
}

fn validate_loop_frequency(value: f64) -> Result<(), StiffnessError> {
    let field = "control.loop_frequency_hz";
    if !value.is_finite() {
        return Err(StiffnessError::NonFiniteInput { field });
    }
    if value.to_bits() != DEFAULT_LOOP_FREQUENCY_HZ.to_bits() {
        return Err(StiffnessError::InvalidProfile(format!(
            "{field} must be exactly {DEFAULT_LOOP_FREQUENCY_HZ}"
        )));
    }
    Ok(())
}

fn validate_finite_array3(field: &'static str, values: &[f64; 3]) -> Result<(), StiffnessError> {
    if values.iter().any(|value| !value.is_finite()) {
        return Err(StiffnessError::NonFiniteInput { field });
    }
    Ok(())
}

fn norm3(values: [f64; 3]) -> f64 {
    (values[0].powi(2) + values[1].powi(2) + values[2].powi(2)).sqrt()
}

fn phi_vec3(values: [f64; 3], profile: &PhiProfile) -> Result<[f64; 3], StiffnessError> {
    Ok([
        phi_axis(values[0], profile, 0)?,
        phi_axis(values[1], profile, 1)?,
        phi_axis(values[2], profile, 2)?,
    ])
}

fn phi_axis(value: f64, profile: &PhiProfile, axis: usize) -> Result<f64, StiffnessError> {
    let deadband = profile.deadband[axis];
    let transformed = match profile.mode[axis].as_str() {
        PHI_MODE_SIGNED => value.signum() * (value.abs() - deadband).max(0.0),
        PHI_MODE_ABSOLUTE => (value.abs() - deadband).max(0.0),
        PHI_MODE_POSITIVE => (value - deadband).max(0.0),
        PHI_MODE_NEGATIVE => (-value - deadband).max(0.0),
        mode => return Err(StiffnessError::UnsupportedPhiMode(mode.to_string())),
    };
    let scaled = profile.scale[axis] * transformed;
    Ok(scaled.clamp(-profile.limit[axis], profile.limit[axis]))
}

fn mat3_vec3(matrix: [[f64; 3]; 3], vector: [f64; 3]) -> [f64; 3] {
    [
        dot3(matrix[0], vector),
        dot3(matrix[1], vector),
        dot3(matrix[2], vector),
    ]
}

fn dot3(lhs: [f64; 3], rhs: [f64; 3]) -> f64 {
    lhs[0] * rhs[0] + lhs[1] * rhs[1] + lhs[2] * rhs[2]
}

fn add3(lhs: [f64; 3], rhs: [f64; 3]) -> [f64; 3] {
    [lhs[0] + rhs[0], lhs[1] + rhs[1], lhs[2] + rhs[2]]
}

fn clip_vec3(values: [f64; 3], mins: [f64; 3], maxes: [f64; 3]) -> [f64; 3] {
    [
        values[0].clamp(mins[0], maxes[0]),
        values[1].clamp(mins[1], maxes[1]),
        values[2].clamp(mins[2], maxes[2]),
    ]
}

fn lpf_vec3(state: [f64; 3], input: [f64; 3], cutoff_hz: f64, dt_sec: f64) -> [f64; 3] {
    [
        lpf_update(state[0], input[0], cutoff_hz, dt_sec),
        lpf_update(state[1], input[1], cutoff_hz, dt_sec),
        lpf_update(state[2], input[2], cutoff_hz, dt_sec),
    ]
}

fn lpf_update(y: f64, x: f64, cutoff_hz: f64, dt_sec: f64) -> f64 {
    let rc = 1.0 / (2.0 * std::f64::consts::PI * cutoff_hz);
    let alpha = dt_sec / (rc + dt_sec);
    y + alpha * (x - y)
}

fn rate_limit_vec3(
    previous: [f64; 3],
    target: [f64; 3],
    max_delta_per_second: [f64; 3],
    dt_sec: f64,
    mins: [f64; 3],
    maxes: [f64; 3],
) -> [f64; 3] {
    [
        rate_limit_axis(previous[0], target[0], max_delta_per_second[0], dt_sec)
            .clamp(mins[0], maxes[0]),
        rate_limit_axis(previous[1], target[1], max_delta_per_second[1], dt_sec)
            .clamp(mins[1], maxes[1]),
        rate_limit_axis(previous[2], target[2], max_delta_per_second[2], dt_sec)
            .clamp(mins[2], maxes[2]),
    ]
}

fn rate_limit_axis(previous: f64, target: f64, max_delta_per_second: f64, dt_sec: f64) -> f64 {
    let max_delta = max_delta_per_second * dt_sec;
    previous + (target - previous).clamp(-max_delta, max_delta)
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::profile::{
        ContactProfile, CueProfile, EffectiveProfile, PhiProfile, StiffnessProfile,
    };

    #[test]
    fn stiffness_clips_before_lpf_and_rate_limit() {
        let profile = StiffnessProfile::test_with_limits([50.0; 3], [100.0; 3]);
        let mut cue = CueProfile {
            w_u: [[1.0, 0.0, 0.0], [0.0, 0.0, 0.0], [0.0, 0.0, 0.0]],
            ..CueProfile::default()
        };
        cue.master_phi.deadband = [0.0; 3];
        cue.master_phi.limit = [1.0e12; 3];

        let mut state =
            SvsStiffnessState::with_profiles(&profile, &ContactProfile::default(), &cue, 200.0)
                .unwrap();
        let output = state.update([1e9, 0.0, 0.0], [0.0; 3], 5_000).unwrap();
        assert!(output.k_state_raw_n_per_m[0] > profile.k_max[0]);
        assert_eq!(output.k_state_clipped_n_per_m[0], profile.k_max[0]);
        assert!(output.k_tele_n_per_m[0] <= profile.k_max[0]);
    }

    #[test]
    fn from_effective_profile_rejects_negative_phi_scale() {
        let mut profile = EffectiveProfile::default_for_tests();
        profile.cue.master_phi.scale[0] = -1.0;

        assert!(profile.validate().is_err());
        assert!(SvsStiffnessState::from_effective_profile(&profile).is_err());
    }

    #[test]
    fn from_effective_profile_rejects_stiffness_bounds_outside_profile_range() {
        let mut profile = EffectiveProfile::default_for_tests();
        profile.stiffness.k_max[0] = 5_000.1;

        assert!(profile.validate().is_err());
        assert!(SvsStiffnessState::from_effective_profile(&profile).is_err());
    }

    #[test]
    fn from_effective_profile_rejects_non_profile_loop_frequency() {
        let mut profile = EffectiveProfile::default_for_tests();
        profile.control.loop_frequency_hz = 199.0;

        assert!(profile.validate().is_err());
        assert!(SvsStiffnessState::from_effective_profile(&profile).is_err());
    }

    #[test]
    fn stiffness_update_failure_does_not_commit_contact_or_filter_state() {
        let profile = StiffnessProfile::test_with_limits([50.0; 3], [100.0; 3]);
        let contact = ContactProfile {
            residual_enter: 1.0,
            residual_exit: 0.5,
            min_hold_ms: 1,
        };
        let mut cue = CueProfile {
            w_u: [[f64::MAX, 0.0, 0.0], [0.0, 0.0, 0.0], [0.0, 0.0, 0.0]],
            ..CueProfile::default()
        };
        cue.master_phi.deadband = [0.0; 3];

        let mut state = SvsStiffnessState::with_profiles(&profile, &contact, &cue, 200.0).unwrap();
        let lpf_before_failure = state.k_lpf_state_n_per_m;
        let previous_k_before_failure = state.previous_k_tele_n_per_m;

        assert!(state.update([2.0, 0.0, 0.0], [1.0, 0.0, 0.0], 5_000).is_err());

        assert_eq!(state.contact_state, SvsContactState::Free);
        assert_eq!(state.enter_ticks, 0);
        assert_eq!(state.exit_ticks, 0);
        assert_eq!(state.k_lpf_state_n_per_m, lpf_before_failure);
        assert_eq!(state.previous_k_tele_n_per_m, previous_k_before_failure);
    }

    #[test]
    fn contact_hysteresis_uses_min_hold_ticks() {
        let profile = StiffnessProfile::test_with_limits([50.0; 3], [100.0; 3]);
        let contact = ContactProfile {
            residual_enter: 1.0,
            residual_exit: 0.5,
            min_hold_ms: 10,
        };
        let mut state =
            SvsStiffnessState::with_profiles(&profile, &contact, &CueProfile::default(), 200.0)
                .unwrap();

        assert_eq!(
            state.update([0.0; 3], [1.0, 0.0, 0.0], 5_000).unwrap().contact_state,
            SvsContactState::Free
        );
        assert_eq!(
            state.update([0.0; 3], [1.0, 0.0, 0.0], 5_000).unwrap().contact_state,
            SvsContactState::Contact
        );
        assert_eq!(
            state.update([0.0; 3], [0.0; 3], 5_000).unwrap().contact_state,
            SvsContactState::Contact
        );
        assert_eq!(
            state.update([0.0; 3], [0.0; 3], 5_000).unwrap().contact_state,
            SvsContactState::Free
        );
    }

    #[test]
    fn stiffness_lpf_uses_profile_formula_before_rate_limit() {
        let mut profile = StiffnessProfile::test_with_limits([0.0; 3], [1_000.0; 3]);
        profile.lpf_cutoff_hz = 10.0;
        profile.max_delta_per_second = [1.0e9; 3];

        let mut cue = CueProfile {
            w_u: [[1.0, 0.0, 0.0], [0.0, 0.0, 0.0], [0.0, 0.0, 0.0]],
            ..CueProfile::default()
        };
        cue.master_phi.deadband = [0.0; 3];
        cue.master_phi.limit = [1_000.0; 3];

        let mut state =
            SvsStiffnessState::with_profiles(&profile, &ContactProfile::default(), &cue, 200.0)
                .unwrap();
        let output = state.update([100.0, 0.0, 0.0], [0.0; 3], 5_000).unwrap();
        let dt_sec = 5_000.0 / 1_000_000.0;
        let rc = 1.0 / (2.0 * std::f64::consts::PI * profile.lpf_cutoff_hz);
        let alpha = dt_sec / (rc + dt_sec);

        assert!((output.k_tele_n_per_m[0] - alpha * 100.0).abs() < 1e-12);
    }

    #[test]
    fn phi_modes_apply_deadband_scale_and_limit() {
        let mut phi = PhiProfile {
            deadband: [1.0; 3],
            scale: [2.0; 3],
            limit: [10.0; 3],
            ..PhiProfile::default()
        };

        phi.mode[0] = "signed".to_string();
        assert_eq!(phi_axis(-3.0, &phi, 0).unwrap(), -4.0);

        phi.mode[0] = "absolute".to_string();
        assert_eq!(phi_axis(-3.0, &phi, 0).unwrap(), 4.0);

        phi.mode[0] = "positive".to_string();
        assert_eq!(phi_axis(3.0, &phi, 0).unwrap(), 4.0);
        assert_eq!(phi_axis(-3.0, &phi, 0).unwrap(), 0.0);

        phi.mode[0] = "negative".to_string();
        assert_eq!(phi_axis(-3.0, &phi, 0).unwrap(), 4.0);
        assert_eq!(phi_axis(3.0, &phi, 0).unwrap(), 0.0);
    }
}
