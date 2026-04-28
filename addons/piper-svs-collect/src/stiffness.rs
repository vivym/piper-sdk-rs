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
        validate_stiffness_profile(stiffness)?;
        validate_contact_profile(contact)?;
        validate_cue_profile(cue)?;
        validate_positive("control.loop_frequency_hz", loop_frequency_hz)?;

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

        self.update_contact_state(norm3(r_ee));

        let phi_u = phi_vec3(u_ee, &self.cue.master_phi)?;
        let phi_r = phi_vec3(r_ee, &self.cue.slave_phi)?;
        let base = match self.contact_state {
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
            &mut self.k_lpf_state_n_per_m,
            k_state_clipped_n_per_m,
            self.stiffness.lpf_cutoff_hz,
            dt_sec,
        );
        let k_tele_n_per_m = rate_limit_vec3(
            self.previous_k_tele_n_per_m,
            k_lpf,
            self.stiffness.max_delta_per_second,
            dt_sec,
            self.stiffness.k_min,
            self.stiffness.k_max,
        );
        validate_finite_array3("k_tele_n_per_m", &k_tele_n_per_m)?;
        self.previous_k_tele_n_per_m = k_tele_n_per_m;

        Ok(SvsStiffnessOutput {
            contact_state: self.contact_state,
            k_state_raw_n_per_m,
            k_state_clipped_n_per_m,
            k_tele_n_per_m,
        })
    }

    fn update_contact_state(&mut self, residual_norm: f64) {
        match self.contact_state {
            SvsContactState::Free => {
                if residual_norm >= self.contact.residual_enter {
                    self.enter_ticks = self.enter_ticks.saturating_add(1);
                } else {
                    self.enter_ticks = 0;
                }

                if self.enter_ticks >= self.min_hold_ticks {
                    self.contact_state = SvsContactState::Contact;
                    self.enter_ticks = 0;
                    self.exit_ticks = 0;
                }
            },
            SvsContactState::Contact => {
                if residual_norm <= self.contact.residual_exit {
                    self.exit_ticks = self.exit_ticks.saturating_add(1);
                } else {
                    self.exit_ticks = 0;
                }

                if self.exit_ticks >= self.min_hold_ticks {
                    self.contact_state = SvsContactState::Free;
                    self.enter_ticks = 0;
                    self.exit_ticks = 0;
                }
            },
        }
    }
}

fn validate_stiffness_profile(profile: &StiffnessProfile) -> Result<(), StiffnessError> {
    validate_finite_array3("stiffness.k_min", &profile.k_min)?;
    validate_finite_array3("stiffness.k_max", &profile.k_max)?;
    validate_finite_array3("stiffness.k_base_free", &profile.k_base_free)?;
    validate_finite_array3("stiffness.k_base_contact", &profile.k_base_contact)?;
    validate_finite_array3(
        "stiffness.max_delta_per_second",
        &profile.max_delta_per_second,
    )?;
    validate_positive("stiffness.lpf_cutoff_hz", profile.lpf_cutoff_hz)?;

    for (axis, (&k_min, &k_max)) in profile.k_min.iter().zip(profile.k_max.iter()).enumerate() {
        if k_min < 0.0 {
            return Err(StiffnessError::InvalidProfile(format!(
                "stiffness.k_min[{axis}] must be non-negative"
            )));
        }
        if k_min >= k_max {
            return Err(StiffnessError::InvalidProfile(format!(
                "stiffness.k_min[{axis}] must be less than stiffness.k_max[{axis}]"
            )));
        }
    }

    validate_base_in_limits(
        "stiffness.k_base_free",
        profile.k_base_free,
        profile.k_min,
        profile.k_max,
    )?;
    validate_base_in_limits(
        "stiffness.k_base_contact",
        profile.k_base_contact,
        profile.k_min,
        profile.k_max,
    )?;

    for (axis, value) in profile.max_delta_per_second.iter().copied().enumerate() {
        if value < 0.0 {
            return Err(StiffnessError::InvalidProfile(format!(
                "stiffness.max_delta_per_second[{axis}] must be non-negative"
            )));
        }
    }

    Ok(())
}

fn validate_contact_profile(profile: &ContactProfile) -> Result<(), StiffnessError> {
    validate_positive_or_zero("contact.residual_enter", profile.residual_enter)?;
    validate_positive_or_zero("contact.residual_exit", profile.residual_exit)?;
    if profile.residual_enter <= profile.residual_exit {
        return Err(StiffnessError::InvalidProfile(
            "contact.residual_enter must be greater than contact.residual_exit".to_string(),
        ));
    }
    Ok(())
}

fn validate_cue_profile(profile: &CueProfile) -> Result<(), StiffnessError> {
    validate_matrix3("cue.w_u", &profile.w_u)?;
    validate_matrix3("cue.w_r", &profile.w_r)?;
    validate_phi_profile("cue.master_phi", &profile.master_phi)?;
    validate_phi_profile("cue.slave_phi", &profile.slave_phi)
}

fn validate_phi_profile(prefix: &'static str, profile: &PhiProfile) -> Result<(), StiffnessError> {
    for (axis, mode) in profile.mode.iter().enumerate() {
        if !matches!(
            mode.as_str(),
            PHI_MODE_SIGNED | PHI_MODE_ABSOLUTE | PHI_MODE_POSITIVE | PHI_MODE_NEGATIVE
        ) {
            return Err(StiffnessError::UnsupportedPhiMode(format!(
                "{prefix}.mode[{axis}]={mode}"
            )));
        }
    }
    validate_finite_array3("phi.deadband", &profile.deadband)?;
    validate_finite_array3("phi.scale", &profile.scale)?;
    validate_finite_array3("phi.limit", &profile.limit)?;

    for (axis, (&deadband, &limit)) in profile.deadband.iter().zip(profile.limit.iter()).enumerate()
    {
        if deadband < 0.0 {
            return Err(StiffnessError::InvalidProfile(format!(
                "{prefix}.deadband[{axis}] must be non-negative"
            )));
        }
        if limit <= 0.0 {
            return Err(StiffnessError::InvalidProfile(format!(
                "{prefix}.limit[{axis}] must be positive"
            )));
        }
    }

    Ok(())
}

fn validate_base_in_limits(
    name: &str,
    values: [f64; 3],
    mins: [f64; 3],
    maxes: [f64; 3],
) -> Result<(), StiffnessError> {
    for (axis, ((&value, &min), &max)) in
        values.iter().zip(mins.iter()).zip(maxes.iter()).enumerate()
    {
        if value < min || value > max {
            return Err(StiffnessError::InvalidProfile(format!(
                "{name}[{axis}] must be within stiffness limits"
            )));
        }
    }

    Ok(())
}

fn validate_positive(field: &'static str, value: f64) -> Result<(), StiffnessError> {
    if !value.is_finite() {
        return Err(StiffnessError::NonFiniteInput { field });
    }
    if value <= 0.0 {
        return Err(StiffnessError::InvalidProfile(format!(
            "{field} must be positive"
        )));
    }
    Ok(())
}

fn validate_positive_or_zero(field: &'static str, value: f64) -> Result<(), StiffnessError> {
    if !value.is_finite() {
        return Err(StiffnessError::NonFiniteInput { field });
    }
    if value < 0.0 {
        return Err(StiffnessError::InvalidProfile(format!(
            "{field} must be non-negative"
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

fn validate_matrix3(field: &'static str, matrix: &[[f64; 3]; 3]) -> Result<(), StiffnessError> {
    if matrix.iter().flat_map(|row| row.iter()).any(|value| !value.is_finite()) {
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

fn lpf_vec3(state: &mut [f64; 3], input: [f64; 3], cutoff_hz: f64, dt_sec: f64) -> [f64; 3] {
    state[0] = lpf_update(state[0], input[0], cutoff_hz, dt_sec);
    state[1] = lpf_update(state[1], input[1], cutoff_hz, dt_sec);
    state[2] = lpf_update(state[2], input[2], cutoff_hz, dt_sec);
    *state
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

    use crate::profile::StiffnessProfile;

    #[test]
    fn stiffness_clips_before_lpf_and_rate_limit() {
        let profile = StiffnessProfile::test_with_limits([50.0; 3], [100.0; 3]);
        let mut state = SvsStiffnessState::new(&profile).unwrap();
        let output = state.update([1e9, 0.0, 0.0], [0.0; 3], 5_000).unwrap();
        assert!(output.k_state_clipped_n_per_m[0] <= 100.0);
        assert!(output.k_tele_n_per_m[0] <= 100.0);
    }
}
