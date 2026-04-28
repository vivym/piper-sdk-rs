use std::sync::{Arc, Mutex};
use std::time::Duration;

use piper_client::dual_arm::{
    BilateralCommand, BilateralControlFrame, BilateralController, DualArmCalibration,
    DualArmSnapshot,
};
use piper_client::types::{JointArray, NewtonMeter, Rad};
use piper_physics::EndEffectorKinematics;
use thiserror::Error;

use crate::cue::{AppliedMasterFeedbackHistory, CueError, SvsCueInput, SvsCueOutput, SvsCueState};
use crate::profile::{EffectiveProfile, ProfileError};
use crate::stiffness::{StiffnessError, SvsStiffnessOutput, SvsStiffnessState};
use crate::tick_frame::{
    SnapshotKey, SvsDynamicsFrame, SvsDynamicsSlot, SvsPendingTick, SvsTickFrameError,
    SvsTickStager,
};

#[derive(Debug, Error)]
pub enum SvsControllerError {
    #[error("invalid SVS profile: {0}")]
    InvalidProfile(String),
    #[error("dt is zero or too large for SVS controller")]
    InvalidDt,
    #[error("SVS feedback history lock is poisoned")]
    FeedbackHistoryPoisoned,
    #[error(transparent)]
    Cue(#[from] CueError),
    #[error(transparent)]
    Stiffness(#[from] StiffnessError),
    #[error(transparent)]
    TickFrame(#[from] SvsTickFrameError),
}

#[derive(Debug, Clone, PartialEq)]
pub struct SvsControlInput {
    pub snapshot: DualArmSnapshot,
    pub calibration: DualArmCalibration,
    pub master_model_torque_nm: [f64; 6],
    pub slave_model_torque_nm: [f64; 6],
    pub master_residual_nm: [f64; 6],
    pub slave_residual_nm: [f64; 6],
    pub master_ee: EndEffectorKinematics,
    pub slave_ee: EndEffectorKinematics,
    pub cues: SvsCueOutput,
    pub stiffness: SvsStiffnessOutput,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SvsControllerOutput {
    pub controller_slave_position_rad: [f64; 6],
    pub controller_slave_velocity_rad_s: [f64; 6],
    pub controller_slave_kp: [f64; 6],
    pub controller_slave_kd: [f64; 6],
    pub controller_slave_feedforward_nm: [f64; 6],
    pub controller_master_position_rad: [f64; 6],
    pub controller_master_velocity_rad_s: [f64; 6],
    pub controller_master_kp: [f64; 6],
    pub controller_master_kd: [f64; 6],
    pub controller_master_interaction_nm: [f64; 6],
    pub reflection_gain_xyz: [f64; 3],
    pub reflection_residual_scale: f64,
}

#[derive(Debug)]
pub struct SvsController {
    profile: EffectiveProfile,
    calibration: DualArmCalibration,
    cue_state: SvsCueState,
    stiffness_state: SvsStiffnessState,
    stager: Arc<SvsTickStager>,
    dynamics_slot: Arc<SvsDynamicsSlot>,
    feedback_history: Arc<Mutex<AppliedMasterFeedbackHistory>>,
}

impl From<ProfileError> for SvsControllerError {
    fn from(value: ProfileError) -> Self {
        Self::InvalidProfile(value.to_string())
    }
}

impl SvsControllerOutput {
    pub fn for_tests() -> Self {
        Self {
            controller_slave_position_rad: [0.0; 6],
            controller_slave_velocity_rad_s: [0.0; 6],
            controller_slave_kp: [0.0; 6],
            controller_slave_kd: [0.0; 6],
            controller_slave_feedforward_nm: [0.0; 6],
            controller_master_position_rad: [0.0; 6],
            controller_master_velocity_rad_s: [0.0; 6],
            controller_master_kp: [0.0; 6],
            controller_master_kd: [0.0; 6],
            controller_master_interaction_nm: [0.0; 6],
            reflection_gain_xyz: [0.0; 3],
            reflection_residual_scale: 1.0,
        }
    }

    pub fn to_bilateral_command(&self) -> BilateralCommand {
        BilateralCommand {
            slave_position: JointArray::new(self.controller_slave_position_rad.map(Rad)),
            slave_velocity: JointArray::new(self.controller_slave_velocity_rad_s),
            slave_kp: JointArray::new(self.controller_slave_kp),
            slave_kd: JointArray::new(self.controller_slave_kd),
            slave_feedforward_torque: JointArray::new(
                self.controller_slave_feedforward_nm.map(NewtonMeter),
            ),
            master_position: JointArray::new(self.controller_master_position_rad.map(Rad)),
            master_velocity: JointArray::new(self.controller_master_velocity_rad_s),
            master_kp: JointArray::new(self.controller_master_kp),
            master_kd: JointArray::new(self.controller_master_kd),
            master_interaction_torque: JointArray::new(
                self.controller_master_interaction_nm.map(NewtonMeter),
            ),
        }
    }
}

impl SvsController {
    pub fn new(
        profile: EffectiveProfile,
        calibration: DualArmCalibration,
    ) -> Result<Self, SvsControllerError> {
        Self::with_shared(
            profile,
            calibration,
            Arc::new(SvsTickStager::new()),
            Arc::new(SvsDynamicsSlot::new()),
            Arc::new(Mutex::new(AppliedMasterFeedbackHistory::default())),
        )
    }

    pub fn with_shared(
        profile: EffectiveProfile,
        calibration: DualArmCalibration,
        stager: Arc<SvsTickStager>,
        dynamics_slot: Arc<SvsDynamicsSlot>,
        feedback_history: Arc<Mutex<AppliedMasterFeedbackHistory>>,
    ) -> Result<Self, SvsControllerError> {
        profile.validate()?;
        let cue_state = SvsCueState::from_effective_profile(&profile)?;
        let stiffness_state = SvsStiffnessState::from_effective_profile(&profile)?;

        Ok(Self {
            profile,
            calibration,
            cue_state,
            stiffness_state,
            stager,
            dynamics_slot,
            feedback_history,
        })
    }

    pub fn stager(&self) -> Arc<SvsTickStager> {
        Arc::clone(&self.stager)
    }

    pub fn dynamics_slot(&self) -> Arc<SvsDynamicsSlot> {
        Arc::clone(&self.dynamics_slot)
    }

    pub fn feedback_history(&self) -> Arc<Mutex<AppliedMasterFeedbackHistory>> {
        Arc::clone(&self.feedback_history)
    }

    fn control_tick(
        &mut self,
        frame: &BilateralControlFrame,
        dt: Duration,
    ) -> Result<SvsControllerOutput, SvsControllerError> {
        let key = SnapshotKey::from_snapshot(&frame.snapshot);
        let dynamics = self.dynamics_slot.take_dynamics(key)?;
        let dt_us = duration_to_micros(dt)?;
        let cues = self.update_cues(&frame.snapshot, &dynamics, dt_us)?;
        let stiffness = self.stiffness_state.update(cues.u_ee, cues.r_ee, dt_us)?;
        let input = SvsControlInput {
            snapshot: frame.snapshot,
            calibration: self.calibration.clone(),
            master_model_torque_nm: dynamics.master_model_torque_nm,
            slave_model_torque_nm: dynamics.slave_model_torque_nm,
            master_residual_nm: dynamics.master_residual_nm,
            slave_residual_nm: dynamics.slave_residual_nm,
            master_ee: dynamics.master_ee.clone(),
            slave_ee: dynamics.slave_ee.clone(),
            cues,
            stiffness,
        };
        let output = compute_controller_output(&self.profile, &input)?;

        self.stager.store_controller_tick(SvsPendingTick {
            key,
            dynamics,
            cues,
            stiffness,
            controller_output: output.clone(),
        })?;

        Ok(output)
    }

    fn update_cues(
        &mut self,
        snapshot: &DualArmSnapshot,
        dynamics: &SvsDynamicsFrame,
        dt_us: u64,
    ) -> Result<SvsCueOutput, SvsControllerError> {
        let input = SvsCueInput {
            master_dynamic_host_rx_mono_us: snapshot.left.dynamic_host_rx_mono_us,
            master_tau_measured_nm: joint_newton_meter_to_array(snapshot.left.state.torque),
            master_tau_model_nm: dynamics.master_model_torque_nm,
            slave_tau_measured_nm: joint_newton_meter_to_array(snapshot.right.state.torque),
            slave_tau_model_nm: dynamics.slave_model_torque_nm,
            master_ee: dynamics.master_ee.clone(),
            slave_ee: dynamics.slave_ee.clone(),
        };
        let history = self
            .feedback_history
            .lock()
            .map_err(|_| SvsControllerError::FeedbackHistoryPoisoned)?;
        Ok(self.cue_state.update(&input, &history, dt_us)?)
    }
}

impl BilateralController for SvsController {
    type Error = SvsControllerError;

    fn tick(
        &mut self,
        snapshot: &DualArmSnapshot,
        dt: Duration,
    ) -> Result<BilateralCommand, Self::Error> {
        self.tick_with_compensation(
            &BilateralControlFrame {
                snapshot: *snapshot,
                compensation: None,
            },
            dt,
        )
    }

    fn tick_with_compensation(
        &mut self,
        frame: &BilateralControlFrame,
        dt: Duration,
    ) -> Result<BilateralCommand, Self::Error> {
        Ok(self.control_tick(frame, dt)?.to_bilateral_command())
    }

    fn reset(&mut self) -> Result<(), Self::Error> {
        self.cue_state = SvsCueState::from_effective_profile(&self.profile)?;
        self.stiffness_state = SvsStiffnessState::from_effective_profile(&self.profile)?;
        Ok(())
    }
}

pub fn compute_controller_output(
    profile: &EffectiveProfile,
    input: &SvsControlInput,
) -> Result<SvsControllerOutput, SvsControllerError> {
    profile.validate()?;

    let alpha_xyz = normalized_stiffness(
        input.stiffness.k_tele_n_per_m,
        profile.stiffness.k_min,
        profile.stiffness.k_max,
    );
    let alpha_joint = mat6x3_vec3_clamped(profile.control.joint_stiffness_projection, alpha_xyz);
    let controller_slave_kp = lerp6(
        profile.control.track_kp_min,
        profile.control.track_kp_max,
        alpha_joint,
    );
    let reflection_alpha = mat3_vec3_clamped(profile.control.reflection_projection, alpha_xyz);
    let reflection_residual_scale = reflection_residual_scale(profile, input.cues.r_ee);
    let mut reflection_gain_xyz = lerp3(
        profile.control.reflection_gain_min,
        profile.control.reflection_gain_max,
        reflection_alpha,
    );
    for value in &mut reflection_gain_xyz {
        *value *= reflection_residual_scale;
    }

    let f_reflect_slave_ee = mul3(reflection_gain_xyz, input.cues.r_ee);
    let f_reflect_slave_base = mat3_vec3(input.slave_ee.rotation_base_from_ee, f_reflect_slave_ee);
    let r_slave_base_from_master_base = profile.frames.master_to_slave_rotation;
    let f_reflect_master_base =
        mat3_transpose_vec3(r_slave_base_from_master_base, f_reflect_slave_base);
    let controller_master_interaction_nm = neg_jacobian_transpose_vec3(
        input.master_ee.translational_jacobian_base,
        f_reflect_master_base,
    );

    Ok(SvsControllerOutput {
        controller_slave_position_rad: joint_rad_to_array(
            input.calibration.master_to_slave_position(input.snapshot.left.state.position),
        ),
        controller_slave_velocity_rad_s: input
            .calibration
            .master_to_slave_velocity(input.snapshot.left.state.velocity)
            .into_array(),
        controller_slave_kp,
        controller_slave_kd: profile.control.track_kd,
        controller_slave_feedforward_nm: [0.0; 6],
        controller_master_position_rad: joint_rad_to_array(input.snapshot.left.state.position),
        controller_master_velocity_rad_s: [0.0; 6],
        controller_master_kp: profile.control.master_kp,
        controller_master_kd: profile.control.master_kd,
        controller_master_interaction_nm,
        reflection_gain_xyz,
        reflection_residual_scale,
    })
}

fn duration_to_micros(duration: Duration) -> Result<u64, SvsControllerError> {
    let micros = duration.as_micros();
    if micros == 0 || micros > u128::from(u64::MAX) {
        return Err(SvsControllerError::InvalidDt);
    }
    Ok(micros as u64)
}

fn normalized_stiffness(k_tele: [f64; 3], k_min: [f64; 3], k_max: [f64; 3]) -> [f64; 3] {
    [
        ((k_tele[0] - k_min[0]) / (k_max[0] - k_min[0])).clamp(0.0, 1.0),
        ((k_tele[1] - k_min[1]) / (k_max[1] - k_min[1])).clamp(0.0, 1.0),
        ((k_tele[2] - k_min[2]) / (k_max[2] - k_min[2])).clamp(0.0, 1.0),
    ]
}

fn reflection_residual_scale(profile: &EffectiveProfile, r_ee: [f64; 3]) -> f64 {
    let residual_excess = (norm3(r_ee) - profile.control.reflection_residual_deadband).max(0.0);
    let scale = 1.0 / (1.0 + profile.control.reflection_residual_attenuation * residual_excess);
    scale.max(profile.control.reflection_residual_min_scale)
}

fn mat6x3_vec3_clamped(matrix: [[f64; 3]; 6], vector: [f64; 3]) -> [f64; 6] {
    [
        dot3(matrix[0], vector).clamp(0.0, 1.0),
        dot3(matrix[1], vector).clamp(0.0, 1.0),
        dot3(matrix[2], vector).clamp(0.0, 1.0),
        dot3(matrix[3], vector).clamp(0.0, 1.0),
        dot3(matrix[4], vector).clamp(0.0, 1.0),
        dot3(matrix[5], vector).clamp(0.0, 1.0),
    ]
}

fn mat3_vec3_clamped(matrix: [[f64; 3]; 3], vector: [f64; 3]) -> [f64; 3] {
    [
        dot3(matrix[0], vector).clamp(0.0, 1.0),
        dot3(matrix[1], vector).clamp(0.0, 1.0),
        dot3(matrix[2], vector).clamp(0.0, 1.0),
    ]
}

fn mat3_vec3(matrix: [[f64; 3]; 3], vector: [f64; 3]) -> [f64; 3] {
    [
        dot3(matrix[0], vector),
        dot3(matrix[1], vector),
        dot3(matrix[2], vector),
    ]
}

fn mat3_transpose_vec3(matrix: [[f64; 3]; 3], vector: [f64; 3]) -> [f64; 3] {
    [
        matrix[0][0] * vector[0] + matrix[1][0] * vector[1] + matrix[2][0] * vector[2],
        matrix[0][1] * vector[0] + matrix[1][1] * vector[1] + matrix[2][1] * vector[2],
        matrix[0][2] * vector[0] + matrix[1][2] * vector[1] + matrix[2][2] * vector[2],
    ]
}

fn neg_jacobian_transpose_vec3(jacobian: [[f64; 6]; 3], vector: [f64; 3]) -> [f64; 6] {
    std::array::from_fn(|joint| {
        -(jacobian[0][joint] * vector[0]
            + jacobian[1][joint] * vector[1]
            + jacobian[2][joint] * vector[2])
    })
}

fn lerp6(min: [f64; 6], max: [f64; 6], alpha: [f64; 6]) -> [f64; 6] {
    std::array::from_fn(|index| min[index] + (max[index] - min[index]) * alpha[index])
}

fn lerp3(min: [f64; 3], max: [f64; 3], alpha: [f64; 3]) -> [f64; 3] {
    [
        min[0] + (max[0] - min[0]) * alpha[0],
        min[1] + (max[1] - min[1]) * alpha[1],
        min[2] + (max[2] - min[2]) * alpha[2],
    ]
}

fn mul3(lhs: [f64; 3], rhs: [f64; 3]) -> [f64; 3] {
    [lhs[0] * rhs[0], lhs[1] * rhs[1], lhs[2] * rhs[2]]
}

fn dot3(lhs: [f64; 3], rhs: [f64; 3]) -> f64 {
    lhs[0] * rhs[0] + lhs[1] * rhs[1] + lhs[2] * rhs[2]
}

fn norm3(values: [f64; 3]) -> f64 {
    dot3(values, values).sqrt()
}

fn joint_rad_to_array(values: JointArray<Rad>) -> [f64; 6] {
    values.map(|value| value.0).into_array()
}

fn joint_newton_meter_to_array(values: JointArray<NewtonMeter>) -> [f64; 6] {
    values.map(|value| value.0).into_array()
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use piper_client::dual_arm::{DualArmCalibration, DualArmSnapshot, JointMirrorMap};
    use piper_client::observer::{ControlSnapshot, ControlSnapshotFull};
    use piper_client::types::{JointArray, NewtonMeter, Rad, RadPerSecond};
    use piper_physics::EndEffectorKinematics;

    use crate::cue::SvsCueOutput;
    use crate::profile::EffectiveProfile;
    use crate::stiffness::{SvsContactState, SvsStiffnessOutput};

    use super::*;

    #[test]
    fn k_tele_schedules_slave_tracking_gains() {
        let mut profile = EffectiveProfile::default_for_tests();
        profile.control.joint_stiffness_projection[0] = [1.0, 0.0, 0.0];

        let mut input = control_input(&profile);
        input.stiffness.k_tele_n_per_m[0] = profile.stiffness.k_max[0];

        let output = compute_controller_output(&profile, &input).unwrap();

        assert_eq!(
            output.controller_slave_kp[0],
            profile.control.track_kp_max[0]
        );
    }

    #[test]
    fn master_reflection_uses_task_space_jacobian_transpose() {
        let mut profile = EffectiveProfile::default_for_tests();
        profile.control.reflection_gain_min = [1.0; 3];
        profile.control.reflection_gain_max = [1.0; 3];
        profile.control.reflection_residual_deadband = 0.0;
        profile.control.reflection_residual_attenuation = 0.0;
        profile.control.reflection_residual_min_scale = 1.0;

        let mut input = control_input(&profile);
        input.cues.r_ee = [2.0, 0.0, 0.0];
        input.master_ee.translational_jacobian_base = identity_translational_jacobian();

        let output = compute_controller_output(&profile, &input).unwrap();

        assert!(output.controller_master_interaction_nm[0] < 0.0);
        assert_eq!(output.controller_master_interaction_nm[1], 0.0);
        assert_eq!(output.controller_master_interaction_nm[2], 0.0);
    }

    fn control_input(profile: &EffectiveProfile) -> SvsControlInput {
        SvsControlInput {
            snapshot: sample_snapshot(1_000, 2_000),
            calibration: sample_calibration(),
            master_model_torque_nm: [0.0; 6],
            slave_model_torque_nm: [0.0; 6],
            master_residual_nm: [0.0; 6],
            slave_residual_nm: [0.0; 6],
            master_ee: ee_with_jacobian(identity_translational_jacobian()),
            slave_ee: ee_with_jacobian(identity_translational_jacobian()),
            cues: SvsCueOutput {
                tau_master_effort_residual_nm: [0.0; 6],
                tau_master_feedback_subtracted_nm: [0.0; 6],
                tau_slave_residual_nm: [0.0; 6],
                u_ee_raw: [0.0; 3],
                r_ee_raw: [0.0; 3],
                u_ee: [0.0; 3],
                r_ee: [0.0; 3],
            },
            stiffness: SvsStiffnessOutput {
                contact_state: SvsContactState::Free,
                k_state_raw_n_per_m: profile.stiffness.k_min,
                k_state_clipped_n_per_m: profile.stiffness.k_min,
                k_tele_n_per_m: profile.stiffness.k_min,
            },
        }
    }

    fn sample_calibration() -> DualArmCalibration {
        DualArmCalibration {
            master_zero: JointArray::splat(Rad::ZERO),
            slave_zero: JointArray::splat(Rad::ZERO),
            map: JointMirrorMap {
                permutation: piper_client::types::Joint::ALL,
                position_sign: [1.0; 6],
                velocity_sign: [1.0; 6],
                torque_sign: [1.0; 6],
            },
        }
    }

    fn sample_snapshot(
        master_dynamic_host_rx_mono_us: u64,
        slave_dynamic_host_rx_mono_us: u64,
    ) -> DualArmSnapshot {
        DualArmSnapshot {
            left: control_snapshot_full(master_dynamic_host_rx_mono_us),
            right: control_snapshot_full(slave_dynamic_host_rx_mono_us),
            inter_arm_skew: Duration::from_micros(
                master_dynamic_host_rx_mono_us.abs_diff(slave_dynamic_host_rx_mono_us),
            ),
            host_cycle_timestamp: Instant::now(),
        }
    }

    fn control_snapshot_full(dynamic_host_rx_mono_us: u64) -> ControlSnapshotFull {
        ControlSnapshotFull {
            state: ControlSnapshot {
                position: JointArray::new([0.1, 0.2, 0.3, 0.4, 0.5, 0.6].map(Rad)),
                velocity: JointArray::new([0.0; 6].map(RadPerSecond)),
                torque: JointArray::splat(NewtonMeter::ZERO),
                position_timestamp_us: dynamic_host_rx_mono_us - 1,
                dynamic_timestamp_us: dynamic_host_rx_mono_us,
                skew_us: 1,
            },
            position_host_rx_mono_us: dynamic_host_rx_mono_us - 1,
            dynamic_host_rx_mono_us,
            feedback_age: Duration::from_micros(500),
        }
    }

    fn ee_with_jacobian(translational_jacobian_base: [[f64; 6]; 3]) -> EndEffectorKinematics {
        EndEffectorKinematics {
            position_base_m: [0.0; 3],
            rotation_base_from_ee: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
            translational_jacobian_base,
            jacobian_condition: 1.0,
        }
    }

    fn identity_translational_jacobian() -> [[f64; 6]; 3] {
        [
            [1.0, 0.0, 0.0, 0.0, 0.0, 0.0],
            [0.0, 1.0, 0.0, 0.0, 0.0, 0.0],
            [0.0, 0.0, 1.0, 0.0, 0.0, 0.0],
        ]
    }
}
