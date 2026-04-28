use std::fmt::Write as _;

use piper_sdk::JointMirrorMap;
use serde::{Deserialize, Serialize};
use thiserror::Error;

const EPSILON_ORTHONORMAL: f64 = 1e-6;
const MIRROR_MAP_KIND_LEFT_RIGHT: &str = "left-right";
const MIRROR_MAP_KIND_CUSTOM: &str = "custom";
const PHI_MODE_SIGNED: &str = "signed";
const PHI_MODE_ABSOLUTE: &str = "absolute";
const PHI_MODE_POSITIVE: &str = "positive";
const PHI_MODE_NEGATIVE: &str = "negative";
const DYNAMICS_MODE_GRAVITY: &str = "gravity";
const DYNAMICS_MODE_PARTIAL: &str = "partial";
const DYNAMICS_MODE_FULL: &str = "full";

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum ProfileError {
    #[error("invalid effective profile: {0}")]
    Invalid(String),
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EffectiveProfile {
    pub stiffness: StiffnessProfile,
    pub contact: ContactProfile,
    pub frames: FramesProfile,
    pub calibration: CalibrationProfile,
    pub mujoco: MujocoProfile,
    pub cue: CueProfile,
    pub dynamics: DynamicsProfile,
    pub control: ControlProfile,
    pub gripper: GripperProfile,
    pub writer: WriterProfile,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StiffnessProfile {
    pub k_min: [f64; 3],
    pub k_max: [f64; 3],
    pub k_base_free: [f64; 3],
    pub k_base_contact: [f64; 3],
    pub lpf_cutoff_hz: f64,
    pub max_delta_per_second: [f64; 3],
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContactProfile {
    pub residual_enter: f64,
    pub residual_exit: f64,
    pub min_hold_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FramesProfile {
    pub master_to_slave_rotation: [[f64; 3]; 3],
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CalibrationProfile {
    pub mirror_map_kind: String,
    pub calibration_max_error_rad: f64,
    pub mirror_map: MirrorMapProfile,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MirrorMapProfile {
    pub permutation: [usize; 6],
    pub position_sign: [f64; 6],
    pub velocity_sign: [f64; 6],
    pub torque_sign: [f64; 6],
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MujocoProfile {
    pub master_ee_site: String,
    pub slave_ee_site: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CueProfile {
    pub dls_lambda: f64,
    pub max_jacobian_condition: f64,
    pub master_lpf_cutoff_hz: f64,
    pub slave_lpf_cutoff_hz: f64,
    pub w_u: [[f64; 3]; 3],
    pub w_r: [[f64; 3]; 3],
    pub master_phi: PhiProfile,
    pub slave_phi: PhiProfile,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PhiProfile {
    pub mode: [String; 3],
    pub deadband: [f64; 3],
    pub scale: [f64; 3],
    pub limit: [f64; 3],
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DynamicsProfile {
    pub master_mode: String,
    pub slave_mode: String,
    pub qacc_lpf_cutoff_hz: f64,
    pub max_abs_qacc: f64,
    pub master_payload: PayloadProfile,
    pub slave_payload: PayloadProfile,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PayloadProfile {
    pub mass_kg: f64,
    pub com_m: [f64; 3],
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ControlProfile {
    pub loop_frequency_hz: f64,
    pub dt_clamp_multiplier: f64,
    pub warmup_cycles: u32,
    pub track_kp_min: [f64; 6],
    pub track_kp_max: [f64; 6],
    pub track_kd: [f64; 6],
    pub master_kp: [f64; 6],
    pub master_kd: [f64; 6],
    pub reflection_gain_min: [f64; 3],
    pub reflection_gain_max: [f64; 3],
    pub joint_stiffness_projection: [[f64; 3]; 6],
    pub reflection_projection: [[f64; 3]; 3],
    pub reflection_residual_deadband: f64,
    pub reflection_residual_attenuation: f64,
    pub reflection_residual_min_scale: f64,
    pub master_interaction_lpf_cutoff_hz: f64,
    pub master_interaction_limit_nm: [f64; 6],
    pub master_interaction_slew_limit_nm_per_s: [f64; 6],
    pub master_passivity_enabled: bool,
    pub master_passivity_max_damping: [f64; 6],
    pub slave_feedforward_limit_nm: [f64; 6],
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GripperProfile {
    pub mirror_enabled: bool,
    pub update_divider: u32,
    pub position_deadband: f64,
    pub effort_scale: f64,
    pub max_feedback_age_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WriterProfile {
    pub queue_capacity: usize,
    pub queue_full_stop_events: usize,
    pub queue_full_stop_duration_ms: u64,
    pub flush_timeout_ms: u64,
}

impl EffectiveProfile {
    pub fn default_for_tests() -> Self {
        Self::default()
    }

    pub fn validate(&self) -> Result<(), ProfileError> {
        self.stiffness.validate()?;
        self.contact.validate()?;
        self.frames.validate()?;
        self.calibration.validate()?;
        self.mujoco.validate()?;
        self.cue.validate()?;
        self.dynamics.validate()?;
        self.control.validate()?;
        self.gripper.validate()?;
        self.writer.validate()?;
        Ok(())
    }

    pub fn to_canonical_toml_bytes(&self) -> Result<Vec<u8>, ProfileError> {
        self.validate()?;

        let mut out = String::new();

        push_table(&mut out, "stiffness");
        push_f64_array_line(&mut out, "k_min", &self.stiffness.k_min);
        push_f64_array_line(&mut out, "k_max", &self.stiffness.k_max);
        push_f64_array_line(&mut out, "k_base_free", &self.stiffness.k_base_free);
        push_f64_array_line(&mut out, "k_base_contact", &self.stiffness.k_base_contact);
        push_f64_line(&mut out, "lpf_cutoff_hz", self.stiffness.lpf_cutoff_hz);
        push_f64_array_line(
            &mut out,
            "max_delta_per_second",
            &self.stiffness.max_delta_per_second,
        );

        push_table(&mut out, "contact");
        push_f64_line(&mut out, "residual_enter", self.contact.residual_enter);
        push_f64_line(&mut out, "residual_exit", self.contact.residual_exit);
        push_u64_line(&mut out, "min_hold_ms", self.contact.min_hold_ms);

        push_table(&mut out, "frames");
        push_f64_matrix_line(
            &mut out,
            "master_to_slave_rotation",
            &self.frames.master_to_slave_rotation,
        );

        push_table(&mut out, "calibration");
        push_string_line(
            &mut out,
            "mirror_map_kind",
            &self.calibration.mirror_map_kind,
        );
        push_f64_line(
            &mut out,
            "calibration_max_error_rad",
            self.calibration.calibration_max_error_rad,
        );

        push_table(&mut out, "calibration.mirror_map");
        push_usize_array_line(
            &mut out,
            "permutation",
            &self.calibration.mirror_map.permutation,
        );
        push_f64_array_line(
            &mut out,
            "position_sign",
            &self.calibration.mirror_map.position_sign,
        );
        push_f64_array_line(
            &mut out,
            "velocity_sign",
            &self.calibration.mirror_map.velocity_sign,
        );
        push_f64_array_line(
            &mut out,
            "torque_sign",
            &self.calibration.mirror_map.torque_sign,
        );

        push_table(&mut out, "mujoco");
        push_string_line(&mut out, "master_ee_site", &self.mujoco.master_ee_site);
        push_string_line(&mut out, "slave_ee_site", &self.mujoco.slave_ee_site);

        push_table(&mut out, "cue");
        push_f64_line(&mut out, "dls_lambda", self.cue.dls_lambda);
        push_f64_line(
            &mut out,
            "max_jacobian_condition",
            self.cue.max_jacobian_condition,
        );
        push_f64_line(
            &mut out,
            "master_lpf_cutoff_hz",
            self.cue.master_lpf_cutoff_hz,
        );
        push_f64_line(
            &mut out,
            "slave_lpf_cutoff_hz",
            self.cue.slave_lpf_cutoff_hz,
        );
        push_f64_matrix_line(&mut out, "w_u", &self.cue.w_u);
        push_f64_matrix_line(&mut out, "w_r", &self.cue.w_r);

        push_table(&mut out, "cue.master_phi");
        push_string_array_line(&mut out, "mode", &self.cue.master_phi.mode);
        push_f64_array_line(&mut out, "deadband", &self.cue.master_phi.deadband);
        push_f64_array_line(&mut out, "scale", &self.cue.master_phi.scale);
        push_f64_array_line(&mut out, "limit", &self.cue.master_phi.limit);

        push_table(&mut out, "cue.slave_phi");
        push_string_array_line(&mut out, "mode", &self.cue.slave_phi.mode);
        push_f64_array_line(&mut out, "deadband", &self.cue.slave_phi.deadband);
        push_f64_array_line(&mut out, "scale", &self.cue.slave_phi.scale);
        push_f64_array_line(&mut out, "limit", &self.cue.slave_phi.limit);

        push_table(&mut out, "dynamics");
        push_string_line(&mut out, "master_mode", &self.dynamics.master_mode);
        push_string_line(&mut out, "slave_mode", &self.dynamics.slave_mode);
        push_f64_line(
            &mut out,
            "qacc_lpf_cutoff_hz",
            self.dynamics.qacc_lpf_cutoff_hz,
        );
        push_f64_line(&mut out, "max_abs_qacc", self.dynamics.max_abs_qacc);

        push_table(&mut out, "dynamics.master_payload");
        push_f64_line(&mut out, "mass_kg", self.dynamics.master_payload.mass_kg);
        push_f64_array_line(&mut out, "com_m", &self.dynamics.master_payload.com_m);

        push_table(&mut out, "dynamics.slave_payload");
        push_f64_line(&mut out, "mass_kg", self.dynamics.slave_payload.mass_kg);
        push_f64_array_line(&mut out, "com_m", &self.dynamics.slave_payload.com_m);

        push_table(&mut out, "control");
        push_f64_line(
            &mut out,
            "loop_frequency_hz",
            self.control.loop_frequency_hz,
        );
        push_f64_line(
            &mut out,
            "dt_clamp_multiplier",
            self.control.dt_clamp_multiplier,
        );
        push_u32_line(&mut out, "warmup_cycles", self.control.warmup_cycles);
        push_f64_array_line(&mut out, "track_kp_min", &self.control.track_kp_min);
        push_f64_array_line(&mut out, "track_kp_max", &self.control.track_kp_max);
        push_f64_array_line(&mut out, "track_kd", &self.control.track_kd);
        push_f64_array_line(&mut out, "master_kp", &self.control.master_kp);
        push_f64_array_line(&mut out, "master_kd", &self.control.master_kd);
        push_f64_array_line(
            &mut out,
            "reflection_gain_min",
            &self.control.reflection_gain_min,
        );
        push_f64_array_line(
            &mut out,
            "reflection_gain_max",
            &self.control.reflection_gain_max,
        );
        push_f64_matrix_line(
            &mut out,
            "joint_stiffness_projection",
            &self.control.joint_stiffness_projection,
        );
        push_f64_matrix_line(
            &mut out,
            "reflection_projection",
            &self.control.reflection_projection,
        );
        push_f64_line(
            &mut out,
            "reflection_residual_deadband",
            self.control.reflection_residual_deadband,
        );
        push_f64_line(
            &mut out,
            "reflection_residual_attenuation",
            self.control.reflection_residual_attenuation,
        );
        push_f64_line(
            &mut out,
            "reflection_residual_min_scale",
            self.control.reflection_residual_min_scale,
        );
        push_f64_line(
            &mut out,
            "master_interaction_lpf_cutoff_hz",
            self.control.master_interaction_lpf_cutoff_hz,
        );
        push_f64_array_line(
            &mut out,
            "master_interaction_limit_nm",
            &self.control.master_interaction_limit_nm,
        );
        push_f64_array_line(
            &mut out,
            "master_interaction_slew_limit_nm_per_s",
            &self.control.master_interaction_slew_limit_nm_per_s,
        );
        push_bool_line(
            &mut out,
            "master_passivity_enabled",
            self.control.master_passivity_enabled,
        );
        push_f64_array_line(
            &mut out,
            "master_passivity_max_damping",
            &self.control.master_passivity_max_damping,
        );
        push_f64_array_line(
            &mut out,
            "slave_feedforward_limit_nm",
            &self.control.slave_feedforward_limit_nm,
        );

        push_table(&mut out, "gripper");
        push_bool_line(&mut out, "mirror_enabled", self.gripper.mirror_enabled);
        push_u32_line(&mut out, "update_divider", self.gripper.update_divider);
        push_f64_line(
            &mut out,
            "position_deadband",
            self.gripper.position_deadband,
        );
        push_f64_line(&mut out, "effort_scale", self.gripper.effort_scale);
        push_u64_line(
            &mut out,
            "max_feedback_age_ms",
            self.gripper.max_feedback_age_ms,
        );

        push_table(&mut out, "writer");
        push_usize_line(&mut out, "queue_capacity", self.writer.queue_capacity);
        push_usize_line(
            &mut out,
            "queue_full_stop_events",
            self.writer.queue_full_stop_events,
        );
        push_u64_line(
            &mut out,
            "queue_full_stop_duration_ms",
            self.writer.queue_full_stop_duration_ms,
        );
        push_u64_line(&mut out, "flush_timeout_ms", self.writer.flush_timeout_ms);

        Ok(out.into_bytes())
    }
}

impl Default for StiffnessProfile {
    fn default() -> Self {
        Self {
            k_min: [50.0, 50.0, 50.0],
            k_max: [800.0, 800.0, 800.0],
            k_base_free: [120.0, 120.0, 120.0],
            k_base_contact: [220.0, 220.0, 180.0],
            lpf_cutoff_hz: 8.0,
            max_delta_per_second: [300.0, 300.0, 300.0],
        }
    }
}

impl Default for ContactProfile {
    fn default() -> Self {
        Self {
            residual_enter: 3.0,
            residual_exit: 1.5,
            min_hold_ms: 80,
        }
    }
}

impl Default for FramesProfile {
    fn default() -> Self {
        Self {
            master_to_slave_rotation: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
        }
    }
}

impl Default for CalibrationProfile {
    fn default() -> Self {
        Self {
            mirror_map_kind: MIRROR_MAP_KIND_LEFT_RIGHT.to_string(),
            calibration_max_error_rad: 0.05,
            mirror_map: MirrorMapProfile::left_right_mirror(),
        }
    }
}

impl MirrorMapProfile {
    pub fn left_right_mirror() -> Self {
        let map = JointMirrorMap::left_right_mirror();
        Self {
            permutation: map.permutation.map(|joint| joint.index()),
            position_sign: map.position_sign,
            velocity_sign: map.velocity_sign,
            torque_sign: map.torque_sign,
        }
    }
}

impl Default for MujocoProfile {
    fn default() -> Self {
        Self {
            master_ee_site: "master_tool_center".to_string(),
            slave_ee_site: "slave_tool_center".to_string(),
        }
    }
}

impl Default for CueProfile {
    fn default() -> Self {
        Self {
            dls_lambda: 0.01,
            max_jacobian_condition: 250.0,
            master_lpf_cutoff_hz: 20.0,
            slave_lpf_cutoff_hz: 20.0,
            w_u: [[0.0, 0.0, 0.0]; 3],
            w_r: [[0.0, 0.0, 0.0]; 3],
            master_phi: PhiProfile::default(),
            slave_phi: PhiProfile::default(),
        }
    }
}

impl Default for PhiProfile {
    fn default() -> Self {
        Self {
            mode: [PHI_MODE_SIGNED, PHI_MODE_SIGNED, PHI_MODE_SIGNED].map(String::from),
            deadband: [0.2, 0.2, 0.2],
            scale: [1.0, 1.0, 1.0],
            limit: [10.0, 10.0, 10.0],
        }
    }
}

impl Default for DynamicsProfile {
    fn default() -> Self {
        Self {
            master_mode: DYNAMICS_MODE_GRAVITY.to_string(),
            slave_mode: DYNAMICS_MODE_PARTIAL.to_string(),
            qacc_lpf_cutoff_hz: 20.0,
            max_abs_qacc: 50.0,
            master_payload: PayloadProfile::default(),
            slave_payload: PayloadProfile::default(),
        }
    }
}

impl Default for PayloadProfile {
    fn default() -> Self {
        Self {
            mass_kg: 0.0,
            com_m: [0.0, 0.0, 0.0],
        }
    }
}

impl Default for ControlProfile {
    fn default() -> Self {
        Self {
            loop_frequency_hz: 200.0,
            dt_clamp_multiplier: 2.0,
            warmup_cycles: 3,
            track_kp_min: [2.0, 2.0, 2.0, 1.0, 1.0, 1.0],
            track_kp_max: [10.0, 10.0, 10.0, 4.0, 4.0, 4.0],
            track_kd: [1.0, 1.0, 1.0, 0.4, 0.4, 0.4],
            master_kp: [0.0, 0.0, 0.0, 0.0, 0.0, 0.0],
            master_kd: [0.3, 0.3, 0.3, 0.15, 0.15, 0.15],
            reflection_gain_min: [0.05, 0.05, 0.05],
            reflection_gain_max: [0.30, 0.30, 0.30],
            joint_stiffness_projection: [
                [1.0, 0.0, 0.0],
                [0.0, 1.0, 0.0],
                [0.0, 0.0, 1.0],
                [0.3, 0.3, 0.0],
                [0.0, 0.3, 0.3],
                [0.3, 0.0, 0.3],
            ],
            reflection_projection: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
            reflection_residual_deadband: 3.0,
            reflection_residual_attenuation: 0.15,
            reflection_residual_min_scale: 0.2,
            master_interaction_lpf_cutoff_hz: 20.0,
            master_interaction_limit_nm: [1.5, 1.5, 1.5, 1.0, 1.0, 1.0],
            master_interaction_slew_limit_nm_per_s: [50.0, 50.0, 50.0, 30.0, 30.0, 30.0],
            master_passivity_enabled: true,
            master_passivity_max_damping: [1.0, 1.0, 1.0, 0.5, 0.5, 0.5],
            slave_feedforward_limit_nm: [4.0, 4.0, 4.0, 2.5, 2.5, 2.5],
        }
    }
}

impl Default for GripperProfile {
    fn default() -> Self {
        Self {
            mirror_enabled: true,
            update_divider: 4,
            position_deadband: 0.02,
            effort_scale: 1.0,
            max_feedback_age_ms: 100,
        }
    }
}

impl Default for WriterProfile {
    fn default() -> Self {
        Self {
            queue_capacity: 8192,
            queue_full_stop_events: 10,
            queue_full_stop_duration_ms: 100,
            flush_timeout_ms: 5000,
        }
    }
}

impl StiffnessProfile {
    pub fn test_with_limits(k_min: [f64; 3], k_max: [f64; 3]) -> Self {
        Self {
            k_min,
            k_max,
            k_base_free: k_min,
            k_base_contact: k_max,
            ..Self::default()
        }
    }

    pub(crate) fn validate(&self) -> Result<(), ProfileError> {
        for (axis, ((k_min, k_max), (k_base_free, k_base_contact))) in self
            .k_min
            .iter()
            .zip(self.k_max.iter())
            .zip(self.k_base_free.iter().zip(self.k_base_contact.iter()))
            .enumerate()
        {
            validate_range(&format!("stiffness.k_min[{axis}]"), *k_min, 0.0, 5000.0)?;
            validate_range(&format!("stiffness.k_max[{axis}]"), *k_max, 0.0, 5000.0)?;
            if k_min >= k_max {
                return invalid(format!(
                    "stiffness.k_min[{axis}] must be less than stiffness.k_max[{axis}]"
                ));
            }
            validate_range(
                &format!("stiffness.k_base_free[{axis}]"),
                *k_base_free,
                *k_min,
                *k_max,
            )?;
            validate_range(
                &format!("stiffness.k_base_contact[{axis}]"),
                *k_base_contact,
                *k_min,
                *k_max,
            )?;
        }
        validate_positive("stiffness.lpf_cutoff_hz", self.lpf_cutoff_hz)?;
        validate_slice_non_negative("stiffness.max_delta_per_second", &self.max_delta_per_second)?;
        Ok(())
    }
}

impl ContactProfile {
    pub(crate) fn validate(&self) -> Result<(), ProfileError> {
        validate_non_negative("contact.residual_enter", self.residual_enter)?;
        validate_non_negative("contact.residual_exit", self.residual_exit)?;
        if self.residual_enter <= self.residual_exit {
            return invalid("contact.residual_enter must be greater than contact.residual_exit");
        }
        Ok(())
    }
}

impl FramesProfile {
    pub(crate) fn validate(&self) -> Result<(), ProfileError> {
        validate_matrix_finite(
            "frames.master_to_slave_rotation",
            &self.master_to_slave_rotation,
        )?;
        validate_rotation_matrix(
            "frames.master_to_slave_rotation",
            &self.master_to_slave_rotation,
        )
    }
}

impl CalibrationProfile {
    fn validate(&self) -> Result<(), ProfileError> {
        match self.mirror_map_kind.as_str() {
            MIRROR_MAP_KIND_LEFT_RIGHT => {
                if self.mirror_map != MirrorMapProfile::left_right_mirror() {
                    return invalid(
                        "calibration.mirror_map must match left-right when mirror_map_kind is left-right",
                    );
                }
            },
            MIRROR_MAP_KIND_CUSTOM => {},
            _ => {
                return invalid("calibration.mirror_map_kind must be \"left-right\" or \"custom\"");
            },
        }
        validate_positive(
            "calibration.calibration_max_error_rad",
            self.calibration_max_error_rad,
        )?;
        self.mirror_map.validate()
    }
}

impl MirrorMapProfile {
    fn validate(&self) -> Result<(), ProfileError> {
        let mut seen = [false; 6];
        for (index, joint) in self.permutation.iter().copied().enumerate() {
            if joint >= seen.len() {
                return invalid(format!(
                    "calibration.mirror_map.permutation[{index}] must be in 0..6"
                ));
            }
            if seen[joint] {
                return invalid("calibration.mirror_map.permutation must contain each joint once");
            }
            seen[joint] = true;
        }

        for name in [
            "calibration.mirror_map.position_sign",
            "calibration.mirror_map.velocity_sign",
            "calibration.mirror_map.torque_sign",
        ] {
            let signs = match name {
                "calibration.mirror_map.position_sign" => &self.position_sign,
                "calibration.mirror_map.velocity_sign" => &self.velocity_sign,
                _ => &self.torque_sign,
            };
            for (index, sign) in signs.iter().copied().enumerate() {
                validate_finite(&format!("{name}[{index}]"), sign)?;
                if !is_unit_sign(sign) {
                    return invalid(format!("{name}[{index}] must be exactly -1.0 or 1.0"));
                }
            }
        }

        Ok(())
    }
}

impl MujocoProfile {
    fn validate(&self) -> Result<(), ProfileError> {
        validate_non_empty_string("mujoco.master_ee_site", &self.master_ee_site)?;
        validate_non_empty_string("mujoco.slave_ee_site", &self.slave_ee_site)
    }
}

impl CueProfile {
    pub(crate) fn validate(&self) -> Result<(), ProfileError> {
        validate_positive("cue.dls_lambda", self.dls_lambda)?;
        validate_positive("cue.max_jacobian_condition", self.max_jacobian_condition)?;
        validate_positive("cue.master_lpf_cutoff_hz", self.master_lpf_cutoff_hz)?;
        validate_positive("cue.slave_lpf_cutoff_hz", self.slave_lpf_cutoff_hz)?;
        validate_matrix_finite("cue.w_u", &self.w_u)?;
        validate_matrix_finite("cue.w_r", &self.w_r)?;
        self.master_phi.validate("cue.master_phi")?;
        self.slave_phi.validate("cue.slave_phi")?;
        Ok(())
    }
}

impl PhiProfile {
    fn validate(&self, prefix: &str) -> Result<(), ProfileError> {
        for (index, mode) in self.mode.iter().enumerate() {
            if !matches!(
                mode.as_str(),
                PHI_MODE_SIGNED | PHI_MODE_ABSOLUTE | PHI_MODE_POSITIVE | PHI_MODE_NEGATIVE
            ) {
                return invalid(format!("{prefix}.mode[{index}] is unsupported"));
            }
        }
        validate_slice_non_negative(&format!("{prefix}.deadband"), &self.deadband)?;
        validate_slice_non_negative(&format!("{prefix}.scale"), &self.scale)?;
        validate_slice_positive(&format!("{prefix}.limit"), &self.limit)?;
        Ok(())
    }
}

impl DynamicsProfile {
    fn validate(&self) -> Result<(), ProfileError> {
        validate_dynamics_mode("dynamics.master_mode", &self.master_mode)?;
        validate_dynamics_mode("dynamics.slave_mode", &self.slave_mode)?;
        validate_positive("dynamics.qacc_lpf_cutoff_hz", self.qacc_lpf_cutoff_hz)?;
        validate_range("dynamics.max_abs_qacc", self.max_abs_qacc, 0.0, 200.0)?;
        if self.max_abs_qacc <= 0.0 {
            return invalid("dynamics.max_abs_qacc must be positive");
        }
        self.master_payload.validate("dynamics.master_payload")?;
        self.slave_payload.validate("dynamics.slave_payload")?;
        Ok(())
    }
}

impl PayloadProfile {
    fn validate(&self, prefix: &str) -> Result<(), ProfileError> {
        validate_non_negative(&format!("{prefix}.mass_kg"), self.mass_kg)?;
        validate_slice_finite(&format!("{prefix}.com_m"), &self.com_m)
    }
}

impl ControlProfile {
    fn validate(&self) -> Result<(), ProfileError> {
        validate_finite("control.loop_frequency_hz", self.loop_frequency_hz)?;
        if self.loop_frequency_hz.to_bits() != 200.0f64.to_bits() {
            return invalid("control.loop_frequency_hz must be exactly 200.0");
        }
        validate_positive("control.dt_clamp_multiplier", self.dt_clamp_multiplier)?;
        if !(0..=100).contains(&self.warmup_cycles) {
            return invalid("control.warmup_cycles must be in 0..=100");
        }

        for (joint, (min, max)) in self
            .track_kp_min
            .iter()
            .copied()
            .zip(self.track_kp_max.iter().copied())
            .enumerate()
        {
            validate_range(&format!("control.track_kp_min[{joint}]"), min, 0.0, 500.0)?;
            validate_range(&format!("control.track_kp_max[{joint}]"), max, 0.0, 500.0)?;
            if min > max {
                return invalid(format!(
                    "control.track_kp_min[{joint}] must be <= control.track_kp_max[{joint}]"
                ));
            }
        }
        validate_slice_range("control.track_kd", &self.track_kd, 0.0, 5.0)?;
        validate_slice_range("control.master_kp", &self.master_kp, 0.0, 500.0)?;
        validate_slice_range("control.master_kd", &self.master_kd, 0.0, 5.0)?;

        for (axis, (min, max)) in self
            .reflection_gain_min
            .iter()
            .copied()
            .zip(self.reflection_gain_max.iter().copied())
            .enumerate()
        {
            validate_range(
                &format!("control.reflection_gain_min[{axis}]"),
                min,
                0.0,
                2.0,
            )?;
            validate_range(
                &format!("control.reflection_gain_max[{axis}]"),
                max,
                0.0,
                2.0,
            )?;
            if min > max {
                return invalid(format!(
                    "control.reflection_gain_min[{axis}] must be <= control.reflection_gain_max[{axis}]"
                ));
            }
        }

        validate_matrix_finite(
            "control.joint_stiffness_projection",
            &self.joint_stiffness_projection,
        )?;
        validate_matrix_finite("control.reflection_projection", &self.reflection_projection)?;
        validate_non_negative(
            "control.reflection_residual_deadband",
            self.reflection_residual_deadband,
        )?;
        validate_non_negative(
            "control.reflection_residual_attenuation",
            self.reflection_residual_attenuation,
        )?;
        validate_range(
            "control.reflection_residual_min_scale",
            self.reflection_residual_min_scale,
            0.0,
            1.0,
        )?;
        validate_positive(
            "control.master_interaction_lpf_cutoff_hz",
            self.master_interaction_lpf_cutoff_hz,
        )?;
        validate_slice_range(
            "control.master_interaction_limit_nm",
            &self.master_interaction_limit_nm,
            0.0,
            8.0,
        )?;
        validate_slice_range(
            "control.master_interaction_slew_limit_nm_per_s",
            &self.master_interaction_slew_limit_nm_per_s,
            0.0,
            200.0,
        )?;
        validate_slice_range(
            "control.master_passivity_max_damping",
            &self.master_passivity_max_damping,
            0.0,
            10.0,
        )?;
        validate_slice_range(
            "control.slave_feedforward_limit_nm",
            &self.slave_feedforward_limit_nm,
            0.0,
            8.0,
        )?;
        Ok(())
    }
}

impl GripperProfile {
    fn validate(&self) -> Result<(), ProfileError> {
        if self.update_divider < 1 {
            return invalid("gripper.update_divider must be at least 1");
        }
        validate_non_negative("gripper.position_deadband", self.position_deadband)?;
        validate_non_negative("gripper.effort_scale", self.effort_scale)?;
        if self.max_feedback_age_ms == 0 {
            return invalid("gripper.max_feedback_age_ms must be positive");
        }
        Ok(())
    }
}

impl WriterProfile {
    fn validate(&self) -> Result<(), ProfileError> {
        if self.queue_capacity == 0 {
            return invalid("writer.queue_capacity must be positive");
        }
        if self.queue_full_stop_events == 0 {
            return invalid("writer.queue_full_stop_events must be positive");
        }
        if self.queue_full_stop_duration_ms == 0 {
            return invalid("writer.queue_full_stop_duration_ms must be positive");
        }
        if self.flush_timeout_ms == 0 {
            return invalid("writer.flush_timeout_ms must be positive");
        }
        Ok(())
    }
}

fn invalid<T>(message: impl Into<String>) -> Result<T, ProfileError> {
    Err(ProfileError::Invalid(message.into()))
}

fn validate_finite(name: &str, value: f64) -> Result<(), ProfileError> {
    if value.is_finite() {
        Ok(())
    } else {
        invalid(format!("{name} must be finite"))
    }
}

fn validate_positive(name: &str, value: f64) -> Result<(), ProfileError> {
    validate_finite(name, value)?;
    if value > 0.0 {
        Ok(())
    } else {
        invalid(format!("{name} must be positive"))
    }
}

fn validate_non_negative(name: &str, value: f64) -> Result<(), ProfileError> {
    validate_finite(name, value)?;
    if value >= 0.0 {
        Ok(())
    } else {
        invalid(format!("{name} must be non-negative"))
    }
}

fn validate_range(name: &str, value: f64, min: f64, max: f64) -> Result<(), ProfileError> {
    validate_finite(name, value)?;
    if (min..=max).contains(&value) {
        Ok(())
    } else {
        invalid(format!("{name} must be in [{min}, {max}]"))
    }
}

fn validate_slice_finite(name: &str, values: &[f64]) -> Result<(), ProfileError> {
    for (index, value) in values.iter().copied().enumerate() {
        validate_finite(&format!("{name}[{index}]"), value)?;
    }
    Ok(())
}

fn validate_slice_positive(name: &str, values: &[f64]) -> Result<(), ProfileError> {
    for (index, value) in values.iter().copied().enumerate() {
        validate_positive(&format!("{name}[{index}]"), value)?;
    }
    Ok(())
}

fn validate_slice_non_negative(name: &str, values: &[f64]) -> Result<(), ProfileError> {
    for (index, value) in values.iter().copied().enumerate() {
        validate_non_negative(&format!("{name}[{index}]"), value)?;
    }
    Ok(())
}

fn validate_slice_range(
    name: &str,
    values: &[f64],
    min: f64,
    max: f64,
) -> Result<(), ProfileError> {
    for (index, value) in values.iter().copied().enumerate() {
        validate_range(&format!("{name}[{index}]"), value, min, max)?;
    }
    Ok(())
}

fn validate_matrix_finite<const ROWS: usize>(
    name: &str,
    matrix: &[[f64; 3]; ROWS],
) -> Result<(), ProfileError> {
    for (row_index, row) in matrix.iter().enumerate() {
        validate_slice_finite(&format!("{name}[{row_index}]"), row)?;
    }
    Ok(())
}

fn validate_rotation_matrix(name: &str, matrix: &[[f64; 3]; 3]) -> Result<(), ProfileError> {
    let mut max_error = 0.0f64;
    for col_a in 0..3 {
        for col_b in 0..3 {
            let dot = matrix.iter().map(|row| row[col_a] * row[col_b]).sum::<f64>();
            let expected = if col_a == col_b { 1.0 } else { 0.0 };
            max_error = max_error.max((dot - expected).abs());
        }
    }
    if max_error > EPSILON_ORTHONORMAL {
        return invalid(format!("{name} must be approximately orthonormal"));
    }

    let det = determinant_3x3(matrix);
    if (det - 1.0).abs() > EPSILON_ORTHONORMAL {
        return invalid(format!("{name} determinant must be approximately +1"));
    }

    Ok(())
}

fn determinant_3x3(matrix: &[[f64; 3]; 3]) -> f64 {
    matrix[0][0] * (matrix[1][1] * matrix[2][2] - matrix[1][2] * matrix[2][1])
        - matrix[0][1] * (matrix[1][0] * matrix[2][2] - matrix[1][2] * matrix[2][0])
        + matrix[0][2] * (matrix[1][0] * matrix[2][1] - matrix[1][1] * matrix[2][0])
}

fn is_unit_sign(value: f64) -> bool {
    matches!(value.to_bits(), bits if bits == 1.0f64.to_bits() || bits == (-1.0f64).to_bits())
}

fn validate_non_empty_string(name: &str, value: &str) -> Result<(), ProfileError> {
    if value.trim().is_empty() {
        invalid(format!("{name} must be non-empty"))
    } else {
        Ok(())
    }
}

fn validate_dynamics_mode(name: &str, value: &str) -> Result<(), ProfileError> {
    if matches!(
        value,
        DYNAMICS_MODE_GRAVITY | DYNAMICS_MODE_PARTIAL | DYNAMICS_MODE_FULL
    ) {
        Ok(())
    } else {
        invalid(format!("{name} is unsupported"))
    }
}

fn push_table(out: &mut String, name: &str) {
    if !out.is_empty() {
        out.push('\n');
    }
    out.push('[');
    out.push_str(name);
    out.push_str("]\n");
}

fn push_f64(out: &mut String, value: f64) {
    let mut buf = ryu::Buffer::new();
    out.push_str(buf.format_finite(value));
}

fn push_f64_line(out: &mut String, key: &str, value: f64) {
    out.push_str(key);
    out.push_str(" = ");
    push_f64(out, value);
    out.push('\n');
}

fn push_bool_line(out: &mut String, key: &str, value: bool) {
    out.push_str(key);
    out.push_str(" = ");
    out.push_str(if value { "true" } else { "false" });
    out.push('\n');
}

fn push_u32_line(out: &mut String, key: &str, value: u32) {
    out.push_str(key);
    out.push_str(" = ");
    write!(out, "{value}").expect("writing to string cannot fail");
    out.push('\n');
}

fn push_u64_line(out: &mut String, key: &str, value: u64) {
    out.push_str(key);
    out.push_str(" = ");
    write!(out, "{value}").expect("writing to string cannot fail");
    out.push('\n');
}

fn push_usize_line(out: &mut String, key: &str, value: usize) {
    out.push_str(key);
    out.push_str(" = ");
    write!(out, "{value}").expect("writing to string cannot fail");
    out.push('\n');
}

fn push_string_line(out: &mut String, key: &str, value: &str) {
    out.push_str(key);
    out.push_str(" = ");
    push_basic_string(out, value);
    out.push('\n');
}

fn push_f64_array_line(out: &mut String, key: &str, values: &[f64]) {
    out.push_str(key);
    out.push_str(" = [");
    for (index, value) in values.iter().copied().enumerate() {
        if index > 0 {
            out.push_str(", ");
        }
        push_f64(out, value);
    }
    out.push_str("]\n");
}

fn push_usize_array_line(out: &mut String, key: &str, values: &[usize]) {
    out.push_str(key);
    out.push_str(" = [");
    for (index, value) in values.iter().copied().enumerate() {
        if index > 0 {
            out.push_str(", ");
        }
        write!(out, "{value}").expect("writing to string cannot fail");
    }
    out.push_str("]\n");
}

fn push_string_array_line(out: &mut String, key: &str, values: &[String]) {
    out.push_str(key);
    out.push_str(" = [");
    for (index, value) in values.iter().enumerate() {
        if index > 0 {
            out.push_str(", ");
        }
        push_basic_string(out, value);
    }
    out.push_str("]\n");
}

fn push_f64_matrix_line<const ROWS: usize>(out: &mut String, key: &str, matrix: &[[f64; 3]; ROWS]) {
    out.push_str(key);
    out.push_str(" = [\n");
    for row in matrix {
        out.push_str("  [");
        for (index, value) in row.iter().copied().enumerate() {
            if index > 0 {
                out.push_str(", ");
            }
            push_f64(out, value);
        }
        out.push_str("],\n");
    }
    out.push_str("]\n");
}

fn push_basic_string(out: &mut String, value: &str) {
    out.push('"');
    for ch in value.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\u{08}' => out.push_str("\\b"),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\u{0C}' => out.push_str("\\f"),
            '\r' => out.push_str("\\r"),
            ch if ch.is_control() => {
                write!(out, "\\u{:04X}", u32::from(ch)).expect("writing to string cannot fail");
            },
            ch => out.push(ch),
        }
    }
    out.push('"');
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn effective_profile_serialization_is_canonical_and_inlines_mirror_map() {
        let profile = EffectiveProfile::default_for_tests();
        let bytes = profile.to_canonical_toml_bytes().expect("serialize");
        let text = std::str::from_utf8(&bytes).unwrap();

        assert!(text.contains("[calibration.mirror_map]\n"));
        assert!(text.contains("mirror_map_kind = \"left-right\"\n"));
        assert!(text.contains("w_u = [\n  [0.0, 0.0, 0.0],\n"));
        assert!(text.ends_with('\n'));
        assert!(!text.ends_with("\n\n"));
    }

    #[test]
    fn validation_rejects_fixed_frequency_changes() {
        let mut profile = EffectiveProfile::default_for_tests();
        profile.control.loop_frequency_hz = 199.0;

        assert!(profile.validate().is_err());
    }

    #[test]
    fn validation_allows_absolute_phi_mode() {
        let mut profile = EffectiveProfile::default_for_tests();
        profile.cue.master_phi.mode = ["absolute", "absolute", "absolute"].map(String::from);
        profile.cue.slave_phi.mode = ["absolute", "absolute", "absolute"].map(String::from);

        profile.validate().unwrap();
    }

    #[test]
    fn validation_rejects_invalid_mirror_maps() {
        let mut profile = EffectiveProfile::default_for_tests();
        profile.calibration.mirror_map.permutation = [0, 1, 2, 3, 4, 4];

        assert!(profile.validate().is_err());
    }

    #[test]
    fn validation_rejects_non_positive_gripper_feedback_age() {
        let mut profile = EffectiveProfile::default_for_tests();
        profile.gripper.max_feedback_age_ms = 0;

        assert!(profile.validate().is_err());
    }

    #[test]
    fn validation_rejects_feedforward_limits_outside_mit_bounds() {
        let mut profile = EffectiveProfile::default_for_tests();
        profile.control.slave_feedforward_limit_nm[0] = 8.1;

        assert!(profile.validate().is_err());
    }
}
