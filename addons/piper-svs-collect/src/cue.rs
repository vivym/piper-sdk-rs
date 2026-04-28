use std::collections::VecDeque;

use nalgebra::{Matrix3, SMatrix, SVector, Vector3};
use piper_physics::EndEffectorKinematics;
use thiserror::Error;

use crate::profile::{CueProfile, EffectiveProfile, FramesProfile};

const DEFAULT_FEEDBACK_HISTORY_CAPACITY: usize = 512;

type Jacobian3x6 = SMatrix<f64, 3, 6>;
type Torque6 = SVector<f64, 6>;

#[derive(Debug, Error, Clone, PartialEq)]
pub enum CueError {
    #[error("{field} contains a non-finite value")]
    NonFiniteInput { field: &'static str },
    #[error("dls_lambda must be finite and positive")]
    InvalidDamping,
    #[error("max_jacobian_condition must be finite and positive")]
    InvalidJacobianConditionLimit,
    #[error("{field} jacobian condition {condition} exceeds maximum {max}")]
    JacobianCondition {
        field: &'static str,
        condition: f64,
        max: f64,
    },
    #[error("DLS matrix is singular")]
    SingularDlsMatrix,
    #[error("dt_us must be positive")]
    InvalidDt,
    #[error("{field} produced a non-finite value")]
    NonFiniteOutput { field: &'static str },
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AppliedMasterFeedback {
    pub master_tx_finished_host_mono_us: u64,
    pub shaped_master_interaction_nm: [f64; 6],
}

#[derive(Debug, Clone, PartialEq)]
pub struct FeedbackHistory {
    entries: VecDeque<AppliedMasterFeedback>,
    capacity: usize,
}

pub type AppliedMasterFeedbackHistory = FeedbackHistory;

#[derive(Debug, Clone, PartialEq)]
pub struct SvsCueInput {
    pub master_dynamic_host_rx_mono_us: u64,
    pub master_tau_measured_nm: [f64; 6],
    pub master_tau_model_nm: [f64; 6],
    pub slave_tau_measured_nm: [f64; 6],
    pub slave_tau_model_nm: [f64; 6],
    pub master_ee: EndEffectorKinematics,
    pub slave_ee: EndEffectorKinematics,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SvsCueOutput {
    pub tau_master_effort_residual_nm: [f64; 6],
    pub tau_master_feedback_subtracted_nm: [f64; 6],
    pub tau_slave_residual_nm: [f64; 6],
    pub u_ee_raw: [f64; 3],
    pub r_ee_raw: [f64; 3],
    pub u_ee: [f64; 3],
    pub r_ee: [f64; 3],
}

#[derive(Debug, Clone)]
pub struct SvsCueState {
    profile: CueProfile,
    r_slave_base_from_master_base: Matrix3<f64>,
    master_lpf_state: [f64; 3],
    slave_lpf_state: [f64; 3],
}

impl Default for FeedbackHistory {
    fn default() -> Self {
        Self::new(DEFAULT_FEEDBACK_HISTORY_CAPACITY)
    }
}

impl FeedbackHistory {
    pub fn new(capacity: usize) -> Self {
        Self {
            entries: VecDeque::with_capacity(capacity),
            capacity,
        }
    }

    pub fn from_entries<I>(entries: I) -> Self
    where
        I: IntoIterator<Item = AppliedMasterFeedback>,
    {
        let mut history = Self::default();
        for entry in entries {
            history.push(entry);
        }
        history
    }

    pub fn push(&mut self, feedback: AppliedMasterFeedback) {
        if self.capacity == 0 {
            return;
        }

        while self.entries.len() >= self.capacity {
            self.entries.pop_front();
        }
        self.entries.push_back(feedback);
    }

    pub fn select_for_dynamic_rx(&self, master_dynamic_host_rx_mono_us: u64) -> [f64; 6] {
        let mut selected = None;
        for entry in &self.entries {
            if entry.master_tx_finished_host_mono_us <= master_dynamic_host_rx_mono_us
                && selected.is_none_or(|selected_entry: &AppliedMasterFeedback| {
                    entry.master_tx_finished_host_mono_us
                        >= selected_entry.master_tx_finished_host_mono_us
                })
            {
                selected = Some(entry);
            }
        }

        selected.map_or([0.0; 6], |entry| entry.shaped_master_interaction_nm)
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

impl SvsCueState {
    pub fn new(profile: &CueProfile, frames: &FramesProfile) -> Result<Self, CueError> {
        validate_positive("cue.dls_lambda", profile.dls_lambda)?;
        validate_positive("cue.max_jacobian_condition", profile.max_jacobian_condition)?;
        validate_positive("cue.master_lpf_cutoff_hz", profile.master_lpf_cutoff_hz)?;
        validate_positive("cue.slave_lpf_cutoff_hz", profile.slave_lpf_cutoff_hz)?;

        Ok(Self {
            profile: profile.clone(),
            r_slave_base_from_master_base: matrix3_from_rows(
                "frames.master_to_slave_rotation",
                &frames.master_to_slave_rotation,
            )?,
            master_lpf_state: [0.0; 3],
            slave_lpf_state: [0.0; 3],
        })
    }

    pub fn from_effective_profile(profile: &EffectiveProfile) -> Result<Self, CueError> {
        Self::new(&profile.cue, &profile.frames)
    }

    pub fn update(
        &mut self,
        input: &SvsCueInput,
        feedback_history: &FeedbackHistory,
        dt_us: u64,
    ) -> Result<SvsCueOutput, CueError> {
        if dt_us == 0 {
            return Err(CueError::InvalidDt);
        }

        validate_finite_array6("master_tau_measured_nm", &input.master_tau_measured_nm)?;
        validate_finite_array6("master_tau_model_nm", &input.master_tau_model_nm)?;
        validate_finite_array6("slave_tau_measured_nm", &input.slave_tau_measured_nm)?;
        validate_finite_array6("slave_tau_model_nm", &input.slave_tau_model_nm)?;
        validate_condition(
            "master_ee.jacobian_condition",
            input.master_ee.jacobian_condition,
            self.profile.max_jacobian_condition,
        )?;
        validate_condition(
            "slave_ee.jacobian_condition",
            input.slave_ee.jacobian_condition,
            self.profile.max_jacobian_condition,
        )?;

        let tau_master_feedback_subtracted_nm =
            feedback_history.select_for_dynamic_rx(input.master_dynamic_host_rx_mono_us);
        let tau_master_effort_residual_nm = subtract6(
            subtract6(input.master_tau_measured_nm, input.master_tau_model_nm),
            tau_master_feedback_subtracted_nm,
        );
        let tau_slave_residual_nm =
            subtract6(input.slave_tau_measured_nm, input.slave_tau_model_nm);

        let f_master_proxy_base_raw = dls_task_space_proxy_with_condition_limit(
            &input.master_ee.translational_jacobian_base,
            &tau_master_effort_residual_nm,
            self.profile.dls_lambda,
            self.profile.max_jacobian_condition,
        )?;
        let f_slave_proxy_base_raw = dls_task_space_proxy_with_condition_limit(
            &input.slave_ee.translational_jacobian_base,
            &tau_slave_residual_nm,
            self.profile.dls_lambda,
            self.profile.max_jacobian_condition,
        )?;

        let r_slave_ee_from_base = matrix3_from_rows(
            "slave_ee.rotation_base_from_ee",
            &input.slave_ee.rotation_base_from_ee,
        )?
        .transpose();
        let u_slave_base_raw =
            self.r_slave_base_from_master_base * Vector3::from(f_master_proxy_base_raw);
        let u_ee_raw = vector3_to_array(r_slave_ee_from_base * u_slave_base_raw);
        let r_ee_raw =
            vector3_to_array(r_slave_ee_from_base * Vector3::from(f_slave_proxy_base_raw));

        validate_finite_array3("u_ee_raw", &u_ee_raw)?;
        validate_finite_array3("r_ee_raw", &r_ee_raw)?;

        let dt_sec = dt_us as f64 / 1_000_000.0;
        let u_ee = lpf_vec3(
            &mut self.master_lpf_state,
            u_ee_raw,
            self.profile.master_lpf_cutoff_hz,
            dt_sec,
        );
        let r_ee = lpf_vec3(
            &mut self.slave_lpf_state,
            r_ee_raw,
            self.profile.slave_lpf_cutoff_hz,
            dt_sec,
        );

        validate_finite_array3("u_ee", &u_ee)?;
        validate_finite_array3("r_ee", &r_ee)?;

        Ok(SvsCueOutput {
            tau_master_effort_residual_nm,
            tau_master_feedback_subtracted_nm,
            tau_slave_residual_nm,
            u_ee_raw,
            r_ee_raw,
            u_ee,
            r_ee,
        })
    }
}

pub fn dls_task_space_proxy(
    translational_jacobian_base: &[[f64; 6]; 3],
    tau_cue_residual_nm: &[f64; 6],
    lambda: f64,
) -> Result<[f64; 3], CueError> {
    dls_task_space_proxy_with_condition_limit(
        translational_jacobian_base,
        tau_cue_residual_nm,
        lambda,
        f64::INFINITY,
    )
}

pub fn dls_task_space_proxy_with_condition_limit(
    translational_jacobian_base: &[[f64; 6]; 3],
    tau_cue_residual_nm: &[f64; 6],
    lambda: f64,
    max_jacobian_condition: f64,
) -> Result<[f64; 3], CueError> {
    if !lambda.is_finite() || lambda <= 0.0 {
        return Err(CueError::InvalidDamping);
    }
    if max_jacobian_condition <= 0.0 || max_jacobian_condition.is_nan() {
        return Err(CueError::InvalidJacobianConditionLimit);
    }

    let j = jacobian_from_rows(translational_jacobian_base)?;
    let tau = torque_from_array(tau_cue_residual_nm)?;
    let condition = jacobian_condition(&j)?;
    if condition > max_jacobian_condition {
        return Err(CueError::JacobianCondition {
            field: "translational_jacobian_base",
            condition,
            max: max_jacobian_condition,
        });
    }

    let dls_matrix = j * j.transpose() + Matrix3::identity() * lambda.powi(2);
    let rhs = j * tau;
    let f_base = dls_matrix.lu().solve(&rhs).ok_or(CueError::SingularDlsMatrix)?;
    let out = vector3_to_array(f_base);
    validate_finite_array3("dls_task_space_proxy", &out)?;
    Ok(out)
}

fn jacobian_from_rows(rows: &[[f64; 6]; 3]) -> Result<Jacobian3x6, CueError> {
    if rows.iter().flat_map(|row| row.iter()).any(|value| !value.is_finite()) {
        return Err(CueError::NonFiniteInput {
            field: "translational_jacobian_base",
        });
    }

    Ok(Jacobian3x6::from_row_slice(&[
        rows[0][0], rows[0][1], rows[0][2], rows[0][3], rows[0][4], rows[0][5], rows[1][0],
        rows[1][1], rows[1][2], rows[1][3], rows[1][4], rows[1][5], rows[2][0], rows[2][1],
        rows[2][2], rows[2][3], rows[2][4], rows[2][5],
    ]))
}

fn torque_from_array(values: &[f64; 6]) -> Result<Torque6, CueError> {
    validate_finite_array6("tau_cue_residual_nm", values)?;
    Ok(Torque6::from_column_slice(values))
}

fn jacobian_condition(jacobian: &Jacobian3x6) -> Result<f64, CueError> {
    let singular_values = jacobian.svd(false, false).singular_values;
    let mut min = f64::INFINITY;
    let mut max = 0.0_f64;

    for value in singular_values.iter().copied() {
        if !value.is_finite() {
            return Err(CueError::NonFiniteOutput {
                field: "jacobian_singular_values",
            });
        }
        min = min.min(value);
        max = max.max(value);
    }

    if min <= f64::EPSILON {
        Ok(f64::INFINITY)
    } else {
        Ok(max / min)
    }
}

fn validate_condition(field: &'static str, condition: f64, max: f64) -> Result<(), CueError> {
    if !condition.is_finite() {
        return Err(CueError::NonFiniteInput { field });
    }
    if condition > max {
        return Err(CueError::JacobianCondition {
            field,
            condition,
            max,
        });
    }
    Ok(())
}

fn validate_positive(field: &'static str, value: f64) -> Result<(), CueError> {
    if !value.is_finite() {
        return Err(CueError::NonFiniteInput { field });
    }
    if value <= 0.0 {
        return Err(match field {
            "cue.max_jacobian_condition" => CueError::InvalidJacobianConditionLimit,
            "cue.dls_lambda" => CueError::InvalidDamping,
            _ => CueError::NonFiniteInput { field },
        });
    }
    Ok(())
}

fn validate_finite_array3(field: &'static str, values: &[f64; 3]) -> Result<(), CueError> {
    if values.iter().any(|value| !value.is_finite()) {
        return Err(CueError::NonFiniteInput { field });
    }
    Ok(())
}

fn validate_finite_array6(field: &'static str, values: &[f64; 6]) -> Result<(), CueError> {
    if values.iter().any(|value| !value.is_finite()) {
        return Err(CueError::NonFiniteInput { field });
    }
    Ok(())
}

fn matrix3_from_rows(field: &'static str, rows: &[[f64; 3]; 3]) -> Result<Matrix3<f64>, CueError> {
    if rows.iter().flat_map(|row| row.iter()).any(|value| !value.is_finite()) {
        return Err(CueError::NonFiniteInput { field });
    }

    Ok(Matrix3::from_row_slice(&[
        rows[0][0], rows[0][1], rows[0][2], rows[1][0], rows[1][1], rows[1][2], rows[2][0],
        rows[2][1], rows[2][2],
    ]))
}

fn vector3_to_array(vector: Vector3<f64>) -> [f64; 3] {
    [vector[0], vector[1], vector[2]]
}

fn subtract6(lhs: [f64; 6], rhs: [f64; 6]) -> [f64; 6] {
    [
        lhs[0] - rhs[0],
        lhs[1] - rhs[1],
        lhs[2] - rhs[2],
        lhs[3] - rhs[3],
        lhs[4] - rhs[4],
        lhs[5] - rhs[5],
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dls_uses_lambda_squared() {
        let j = [
            [1.0, 0.0, 0.0, 0.0, 0.0, 0.0],
            [0.0, 1.0, 0.0, 0.0, 0.0, 0.0],
            [0.0, 0.0, 1.0, 0.0, 0.0, 0.0],
        ];
        let tau = [1.0, 0.0, 0.0, 0.0, 0.0, 0.0];
        let cue = dls_task_space_proxy(&j, &tau, 2.0).unwrap();
        assert!((cue[0] - 0.2).abs() < 1e-12);
    }

    #[test]
    fn master_feedback_subtraction_uses_latest_not_newer_than_snapshot() {
        let history = FeedbackHistory::from_entries([
            AppliedMasterFeedback {
                master_tx_finished_host_mono_us: 100,
                shaped_master_interaction_nm: [1.0; 6],
            },
            AppliedMasterFeedback {
                master_tx_finished_host_mono_us: 200,
                shaped_master_interaction_nm: [2.0; 6],
            },
        ]);
        assert_eq!(history.select_for_dynamic_rx(150), [1.0; 6]);
    }
}
