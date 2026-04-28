use std::fs::OpenOptions;
use std::io::{Cursor, Write};
use std::path::{Path, PathBuf};

use bincode::Options;
use serde::{Deserialize, Serialize};
use thiserror::Error;

const MAGIC: [u8; 8] = *b"PIPERSVS";
const SCHEMA_VERSION: u16 = 1;
const ENCODING_ID_BINCODE_FIXINT_LE: u16 = 1;
const STEP_SCHEMA_VERSION: u16 = 1;
const EPISODE_ID_CAPACITY: usize = 128;
const TAU_RESIDUAL_TOLERANCE: f64 = 1.0e-9;

fn bincode_options() -> impl bincode::Options {
    bincode::DefaultOptions::new().with_fixint_encoding().with_little_endian()
}

#[derive(Debug, Error)]
pub enum WireError {
    #[error("invalid SVS header: {0}")]
    InvalidHeader(String),
    #[error("invalid SVS step {step_index}: {reason}")]
    InvalidStep { step_index: u64, reason: String },
    #[error("SVS step index mismatch: expected {expected}, got {actual}")]
    NonSequentialStepIndex { expected: u64, actual: u64 },
    #[error("bincode error while decoding SVS episode: {0}")]
    Bincode(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Utf8(#[from] std::str::Utf8Error),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SvsHeaderV1 {
    pub magic: [u8; 8],
    pub schema_version: u16,
    pub encoding_id: u16,
    pub step_schema_version: u16,
    pub reserved: u16,
    pub episode_id_len: u16,
    #[serde(with = "episode_id_bytes_serde")]
    pub episode_id_utf8: [u8; 128],
    pub created_unix_ms: u64,
    pub episode_start_host_mono_us: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SvsArmStepV1 {
    pub position_hw_timestamp_us: u64,
    pub dynamic_hw_timestamp_us: u64,
    pub position_host_rx_mono_us: u64,
    pub dynamic_host_rx_mono_us: u64,
    pub feedback_age_us: u64,
    pub state_skew_us: i64,
    pub q_rad: [f64; 6],
    pub dq_rad_s: [f64; 6],
    pub tau_measured_nm: [f64; 6],
    pub tau_model_mujoco_nm: [f64; 6],
    pub tau_residual_nm: [f64; 6],
    pub ee_position_base_m: [f64; 3],
    pub rotation_base_from_ee_row_major: [f64; 9],
    pub translational_jacobian_base_row_major: [f64; 18],
    pub jacobian_condition: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SvsCommandStepV1 {
    pub master_tx_finished_host_mono_us: u64,
    pub slave_tx_finished_host_mono_us: u64,
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
    pub shaped_master_interaction_nm: [f64; 6],
    pub shaped_slave_feedforward_nm: [f64; 6],
    pub sdk_master_feedforward_nm: [f64; 6],
    pub sdk_slave_feedforward_nm: [f64; 6],
    pub mit_master_t_ref_nm: [f64; 6],
    pub mit_slave_t_ref_nm: [f64; 6],
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SvsGripperStepV1 {
    pub mirror_enabled: u8,
    pub master_available: u8,
    pub slave_available: u8,
    pub command_status: u8,
    pub master_hw_timestamp_us: u64,
    pub slave_hw_timestamp_us: u64,
    pub master_host_rx_mono_us: u64,
    pub slave_host_rx_mono_us: u64,
    pub master_age_us: u64,
    pub slave_age_us: u64,
    pub master_position: f64,
    pub master_effort: f64,
    pub slave_position: f64,
    pub slave_effort: f64,
    pub command_position: f64,
    pub command_effort: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SvsStepV1 {
    pub step_index: u64,
    pub host_mono_us: u64,
    pub episode_elapsed_us: u64,
    pub dt_us: u64,
    pub inter_arm_skew_us: u64,
    pub deadline_missed: u8,
    pub contact_state: u8,
    pub raw_can_status: u8,
    pub master: SvsArmStepV1,
    pub slave: SvsArmStepV1,
    pub tau_master_effort_residual_nm: [f64; 6],
    pub tau_master_feedback_subtracted_nm: [f64; 6],
    pub u_ee_raw: [f64; 3],
    pub r_ee_raw: [f64; 3],
    pub u_ee: [f64; 3],
    pub r_ee: [f64; 3],
    pub k_state_raw_n_per_m: [f64; 3],
    pub k_state_clipped_n_per_m: [f64; 3],
    pub k_tele_n_per_m: [f64; 3],
    pub reflection_gain_xyz: [f64; 3],
    pub reflection_residual_scale: f64,
    pub command: SvsCommandStepV1,
    pub gripper: SvsGripperStepV1,
    pub writer_queue_depth: u32,
    pub writer_queue_full_events: u64,
    pub dropped_step_count: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DecodedStepsFileV1 {
    pub header: SvsHeaderV1,
    pub steps: Vec<SvsStepV1>,
    pub summary: StepFileSummary,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct StepFileSummary {
    pub episode_id: String,
    pub step_count: u64,
    pub last_step_index: Option<u64>,
}

impl SvsHeaderV1 {
    pub fn new(
        episode_id: &str,
        created_unix_ms: u64,
        episode_start_host_mono_us: u64,
    ) -> Result<Self, WireError> {
        let episode_id_bytes = episode_id.as_bytes();
        if episode_id_bytes.is_empty() || episode_id_bytes.len() > EPISODE_ID_CAPACITY {
            return Err(WireError::InvalidHeader(format!(
                "episode_id must be 1..={EPISODE_ID_CAPACITY} bytes"
            )));
        }

        let mut episode_id_utf8 = [0_u8; EPISODE_ID_CAPACITY];
        episode_id_utf8[..episode_id_bytes.len()].copy_from_slice(episode_id_bytes);

        let header = Self {
            magic: MAGIC,
            schema_version: SCHEMA_VERSION,
            encoding_id: ENCODING_ID_BINCODE_FIXINT_LE,
            step_schema_version: STEP_SCHEMA_VERSION,
            reserved: 0,
            episode_id_len: episode_id_bytes.len() as u16,
            episode_id_utf8,
            created_unix_ms,
            episode_start_host_mono_us,
        };
        header.validate()?;
        Ok(header)
    }

    pub fn for_test(episode_id: &str) -> Self {
        Self::new(episode_id, 1_777_247_999_000, 42).expect("valid test SVS header")
    }

    pub fn episode_id(&self) -> Result<&str, WireError> {
        let len = self.validate_episode_id_bytes()?;
        Ok(std::str::from_utf8(&self.episode_id_utf8[..len])?)
    }

    pub fn validate(&self) -> Result<(), WireError> {
        if self.magic != MAGIC {
            return Err(WireError::InvalidHeader("bad magic".to_string()));
        }
        if self.schema_version != SCHEMA_VERSION {
            return Err(WireError::InvalidHeader(format!(
                "schema_version must be {SCHEMA_VERSION}"
            )));
        }
        if self.encoding_id != ENCODING_ID_BINCODE_FIXINT_LE {
            return Err(WireError::InvalidHeader(format!(
                "encoding_id must be {ENCODING_ID_BINCODE_FIXINT_LE}"
            )));
        }
        if self.step_schema_version != STEP_SCHEMA_VERSION {
            return Err(WireError::InvalidHeader(format!(
                "step_schema_version must be {STEP_SCHEMA_VERSION}"
            )));
        }
        if self.reserved != 0 {
            return Err(WireError::InvalidHeader(
                "reserved must be zero".to_string(),
            ));
        }
        self.validate_episode_id_bytes()?;
        Ok(())
    }

    fn validate_episode_id_bytes(&self) -> Result<usize, WireError> {
        let len = usize::from(self.episode_id_len);
        if len == 0 || len > EPISODE_ID_CAPACITY {
            return Err(WireError::InvalidHeader(format!(
                "episode_id_len must be 1..={EPISODE_ID_CAPACITY}"
            )));
        }
        if self.episode_id_utf8[len..].iter().any(|byte| *byte != 0) {
            return Err(WireError::InvalidHeader(
                "episode_id padding must be zero".to_string(),
            ));
        }
        std::str::from_utf8(&self.episode_id_utf8[..len])?;
        Ok(len)
    }
}

impl SvsArmStepV1 {
    pub fn for_test() -> Self {
        Self {
            position_hw_timestamp_us: 0,
            dynamic_hw_timestamp_us: 0,
            position_host_rx_mono_us: 0,
            dynamic_host_rx_mono_us: 0,
            feedback_age_us: 0,
            state_skew_us: 0,
            q_rad: [0.0; 6],
            dq_rad_s: [0.0; 6],
            tau_measured_nm: [0.0; 6],
            tau_model_mujoco_nm: [0.0; 6],
            tau_residual_nm: [0.0; 6],
            ee_position_base_m: [0.0; 3],
            rotation_base_from_ee_row_major: [0.0; 9],
            translational_jacobian_base_row_major: [0.0; 18],
            jacobian_condition: 0.0,
        }
    }

    fn validate(&self, arm_name: &str, step_index: u64) -> Result<(), WireError> {
        validate_finite_array(step_index, &format!("{arm_name}.q_rad"), &self.q_rad)?;
        validate_finite_array(step_index, &format!("{arm_name}.dq_rad_s"), &self.dq_rad_s)?;
        validate_finite_array(
            step_index,
            &format!("{arm_name}.tau_measured_nm"),
            &self.tau_measured_nm,
        )?;
        validate_finite_array(
            step_index,
            &format!("{arm_name}.tau_model_mujoco_nm"),
            &self.tau_model_mujoco_nm,
        )?;
        validate_finite_array(
            step_index,
            &format!("{arm_name}.tau_residual_nm"),
            &self.tau_residual_nm,
        )?;
        validate_tau_residual(
            step_index,
            arm_name,
            &self.tau_measured_nm,
            &self.tau_model_mujoco_nm,
            &self.tau_residual_nm,
        )?;
        validate_finite_array(
            step_index,
            &format!("{arm_name}.ee_position_base_m"),
            &self.ee_position_base_m,
        )?;
        validate_finite_array(
            step_index,
            &format!("{arm_name}.rotation_base_from_ee_row_major"),
            &self.rotation_base_from_ee_row_major,
        )?;
        validate_finite_array(
            step_index,
            &format!("{arm_name}.translational_jacobian_base_row_major"),
            &self.translational_jacobian_base_row_major,
        )?;
        validate_finite_value(
            step_index,
            &format!("{arm_name}.jacobian_condition"),
            self.jacobian_condition,
        )
    }
}

impl SvsCommandStepV1 {
    pub fn for_test() -> Self {
        Self {
            master_tx_finished_host_mono_us: 0,
            slave_tx_finished_host_mono_us: 0,
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
            shaped_master_interaction_nm: [0.0; 6],
            shaped_slave_feedforward_nm: [0.0; 6],
            sdk_master_feedforward_nm: [0.0; 6],
            sdk_slave_feedforward_nm: [0.0; 6],
            mit_master_t_ref_nm: [0.0; 6],
            mit_slave_t_ref_nm: [0.0; 6],
        }
    }

    fn validate(&self, step_index: u64) -> Result<(), WireError> {
        validate_finite_array(
            step_index,
            "command.controller_slave_position_rad",
            &self.controller_slave_position_rad,
        )?;
        validate_finite_array(
            step_index,
            "command.controller_slave_velocity_rad_s",
            &self.controller_slave_velocity_rad_s,
        )?;
        validate_finite_array(
            step_index,
            "command.controller_slave_kp",
            &self.controller_slave_kp,
        )?;
        validate_finite_array(
            step_index,
            "command.controller_slave_kd",
            &self.controller_slave_kd,
        )?;
        validate_finite_array(
            step_index,
            "command.controller_slave_feedforward_nm",
            &self.controller_slave_feedforward_nm,
        )?;
        validate_finite_array(
            step_index,
            "command.controller_master_position_rad",
            &self.controller_master_position_rad,
        )?;
        validate_finite_array(
            step_index,
            "command.controller_master_velocity_rad_s",
            &self.controller_master_velocity_rad_s,
        )?;
        validate_finite_array(
            step_index,
            "command.controller_master_kp",
            &self.controller_master_kp,
        )?;
        validate_finite_array(
            step_index,
            "command.controller_master_kd",
            &self.controller_master_kd,
        )?;
        validate_finite_array(
            step_index,
            "command.controller_master_interaction_nm",
            &self.controller_master_interaction_nm,
        )?;
        validate_finite_array(
            step_index,
            "command.shaped_master_interaction_nm",
            &self.shaped_master_interaction_nm,
        )?;
        validate_finite_array(
            step_index,
            "command.shaped_slave_feedforward_nm",
            &self.shaped_slave_feedforward_nm,
        )?;
        validate_finite_array(
            step_index,
            "command.sdk_master_feedforward_nm",
            &self.sdk_master_feedforward_nm,
        )?;
        validate_finite_array(
            step_index,
            "command.sdk_slave_feedforward_nm",
            &self.sdk_slave_feedforward_nm,
        )?;
        validate_finite_array(
            step_index,
            "command.mit_master_t_ref_nm",
            &self.mit_master_t_ref_nm,
        )?;
        validate_finite_array(
            step_index,
            "command.mit_slave_t_ref_nm",
            &self.mit_slave_t_ref_nm,
        )
    }
}

impl SvsGripperStepV1 {
    pub fn for_test() -> Self {
        Self {
            mirror_enabled: 0,
            master_available: 0,
            slave_available: 0,
            command_status: 0,
            master_hw_timestamp_us: 0,
            slave_hw_timestamp_us: 0,
            master_host_rx_mono_us: 0,
            slave_host_rx_mono_us: 0,
            master_age_us: 0,
            slave_age_us: 0,
            master_position: 0.0,
            master_effort: 0.0,
            slave_position: 0.0,
            slave_effort: 0.0,
            command_position: 0.0,
            command_effort: 0.0,
        }
    }

    fn validate(&self, step_index: u64) -> Result<(), WireError> {
        validate_bool_byte(step_index, "gripper.mirror_enabled", self.mirror_enabled)?;
        validate_bool_byte(
            step_index,
            "gripper.master_available",
            self.master_available,
        )?;
        validate_bool_byte(step_index, "gripper.slave_available", self.slave_available)?;
        validate_enum_byte(
            step_index,
            "gripper.command_status",
            self.command_status,
            0..=3,
        )?;
        validate_finite_value(step_index, "gripper.master_position", self.master_position)?;
        validate_finite_value(step_index, "gripper.master_effort", self.master_effort)?;
        validate_finite_value(step_index, "gripper.slave_position", self.slave_position)?;
        validate_finite_value(step_index, "gripper.slave_effort", self.slave_effort)?;
        validate_finite_value(
            step_index,
            "gripper.command_position",
            self.command_position,
        )?;
        validate_finite_value(step_index, "gripper.command_effort", self.command_effort)
    }
}

impl SvsStepV1 {
    pub fn for_test(step_index: u64) -> Self {
        Self {
            step_index,
            host_mono_us: step_index,
            episode_elapsed_us: step_index,
            dt_us: 5_000,
            inter_arm_skew_us: 0,
            deadline_missed: 0,
            contact_state: 0,
            raw_can_status: 1,
            master: SvsArmStepV1::for_test(),
            slave: SvsArmStepV1::for_test(),
            tau_master_effort_residual_nm: [0.0; 6],
            tau_master_feedback_subtracted_nm: [0.0; 6],
            u_ee_raw: [0.0; 3],
            r_ee_raw: [0.0; 3],
            u_ee: [0.0; 3],
            r_ee: [0.0; 3],
            k_state_raw_n_per_m: [0.0; 3],
            k_state_clipped_n_per_m: [0.0; 3],
            k_tele_n_per_m: [0.0; 3],
            reflection_gain_xyz: [0.0; 3],
            reflection_residual_scale: 1.0,
            command: SvsCommandStepV1::for_test(),
            gripper: SvsGripperStepV1::for_test(),
            writer_queue_depth: 0,
            writer_queue_full_events: 0,
            dropped_step_count: 0,
        }
    }

    pub fn validate(&self) -> Result<(), WireError> {
        validate_bool_byte(self.step_index, "deadline_missed", self.deadline_missed)?;
        validate_bool_byte(self.step_index, "contact_state", self.contact_state)?;
        validate_enum_byte(
            self.step_index,
            "raw_can_status",
            self.raw_can_status,
            0..=2,
        )?;
        self.master.validate("master", self.step_index)?;
        self.slave.validate("slave", self.step_index)?;
        validate_finite_array(
            self.step_index,
            "tau_master_effort_residual_nm",
            &self.tau_master_effort_residual_nm,
        )?;
        validate_finite_array(
            self.step_index,
            "tau_master_feedback_subtracted_nm",
            &self.tau_master_feedback_subtracted_nm,
        )?;
        validate_finite_array(self.step_index, "u_ee_raw", &self.u_ee_raw)?;
        validate_finite_array(self.step_index, "r_ee_raw", &self.r_ee_raw)?;
        validate_finite_array(self.step_index, "u_ee", &self.u_ee)?;
        validate_finite_array(self.step_index, "r_ee", &self.r_ee)?;
        validate_finite_array(
            self.step_index,
            "k_state_raw_n_per_m",
            &self.k_state_raw_n_per_m,
        )?;
        validate_finite_array(
            self.step_index,
            "k_state_clipped_n_per_m",
            &self.k_state_clipped_n_per_m,
        )?;
        validate_finite_array(self.step_index, "k_tele_n_per_m", &self.k_tele_n_per_m)?;
        validate_finite_array(
            self.step_index,
            "reflection_gain_xyz",
            &self.reflection_gain_xyz,
        )?;
        validate_finite_value(
            self.step_index,
            "reflection_residual_scale",
            self.reflection_residual_scale,
        )?;
        self.command.validate(self.step_index)?;
        self.gripper.validate(self.step_index)
    }
}

impl StepFileSummary {
    pub fn new(episode_id: impl Into<String>, steps: &[SvsStepV1]) -> Self {
        Self {
            episode_id: episode_id.into(),
            step_count: steps.len() as u64,
            last_step_index: steps.last().map(|step| step.step_index),
        }
    }
}

pub fn write_steps_file(
    path: impl AsRef<Path>,
    header: &SvsHeaderV1,
    steps: &[SvsStepV1],
) -> Result<StepFileSummary, WireError> {
    header.validate()?;
    validate_steps_sequence(steps)?;
    let episode_id = header.episode_id()?.to_string();
    let path = path.as_ref();
    let temp_path = temp_path_for(path);

    let result = (|| {
        let mut file = OpenOptions::new().write(true).create_new(true).open(&temp_path)?;
        write_header(&mut file, header)?;
        for step in steps {
            write_step(&mut file, step)?;
        }
        file.flush()?;
        file.sync_all()?;
        drop(file);
        persist_temp_no_overwrite(&temp_path, path)?;
        Ok(StepFileSummary::new(episode_id, steps))
    })();

    if result.is_err() {
        let _ = std::fs::remove_file(&temp_path);
    }
    result
}

fn validate_steps_sequence(steps: &[SvsStepV1]) -> Result<(), WireError> {
    for (expected, step) in steps.iter().enumerate() {
        step.validate()?;
        let expected = expected as u64;
        if step.step_index != expected {
            return Err(WireError::NonSequentialStepIndex {
                expected,
                actual: step.step_index,
            });
        }
    }
    Ok(())
}

fn temp_path_for(path: &Path) -> PathBuf {
    let file_name = path.file_name().and_then(|name| name.to_str()).unwrap_or("steps.bin");
    path.with_file_name(format!(
        ".{file_name}.{}.{}.tmp",
        std::process::id(),
        piper_driver::heartbeat::monotonic_micros().max(1)
    ))
}

fn persist_temp_no_overwrite(temp_path: &Path, final_path: &Path) -> std::io::Result<()> {
    match std::fs::hard_link(temp_path, final_path) {
        Ok(()) => {
            std::fs::remove_file(temp_path)?;
            Ok(())
        },
        Err(err) => Err(err),
    }
}

pub fn read_steps_file(path: impl AsRef<Path>) -> Result<DecodedStepsFileV1, WireError> {
    let bytes = std::fs::read(path)?;
    let mut cursor = Cursor::new(bytes.as_slice());
    let header: SvsHeaderV1 = bincode_options()
        .deserialize_from(&mut cursor)
        .map_err(|err| WireError::Bincode(err.to_string()))?;
    header.validate()?;
    let episode_id = header.episode_id()?.to_string();

    let mut steps = Vec::new();
    let mut expected_step_index = 0_u64;
    while (cursor.position() as usize) < bytes.len() {
        let step: SvsStepV1 = bincode_options()
            .deserialize_from(&mut cursor)
            .map_err(|err| WireError::Bincode(err.to_string()))?;
        step.validate()?;
        if step.step_index != expected_step_index {
            return Err(WireError::NonSequentialStepIndex {
                expected: expected_step_index,
                actual: step.step_index,
            });
        }
        expected_step_index = expected_step_index.saturating_add(1);
        steps.push(step);
    }

    let summary = StepFileSummary::new(episode_id, &steps);
    Ok(DecodedStepsFileV1 {
        header,
        steps,
        summary,
    })
}

pub(crate) fn write_header<W: Write>(
    writer: &mut W,
    header: &SvsHeaderV1,
) -> Result<(), WireError> {
    header.validate()?;
    bincode_options()
        .serialize_into(writer, header)
        .map_err(|err| WireError::Bincode(err.to_string()))
}

pub(crate) fn write_step<W: Write>(writer: &mut W, step: &SvsStepV1) -> Result<(), WireError> {
    step.validate()?;
    bincode_options()
        .serialize_into(writer, step)
        .map_err(|err| WireError::Bincode(err.to_string()))
}

fn validate_bool_byte(step_index: u64, field: &str, value: u8) -> Result<(), WireError> {
    validate_enum_byte(step_index, field, value, 0..=1)
}

fn validate_enum_byte(
    step_index: u64,
    field: &str,
    value: u8,
    valid: std::ops::RangeInclusive<u8>,
) -> Result<(), WireError> {
    if valid.contains(&value) {
        Ok(())
    } else {
        Err(WireError::InvalidStep {
            step_index,
            reason: format!("{field} has invalid byte code {value}"),
        })
    }
}

fn validate_finite_array<const N: usize>(
    step_index: u64,
    field: &str,
    values: &[f64; N],
) -> Result<(), WireError> {
    for (index, value) in values.iter().copied().enumerate() {
        if !value.is_finite() {
            return Err(WireError::InvalidStep {
                step_index,
                reason: format!("{field}[{index}] must be finite"),
            });
        }
    }
    Ok(())
}

fn validate_finite_value(step_index: u64, field: &str, value: f64) -> Result<(), WireError> {
    if value.is_finite() {
        Ok(())
    } else {
        Err(WireError::InvalidStep {
            step_index,
            reason: format!("{field} must be finite"),
        })
    }
}

fn validate_tau_residual(
    step_index: u64,
    arm_name: &str,
    measured: &[f64; 6],
    model: &[f64; 6],
    residual: &[f64; 6],
) -> Result<(), WireError> {
    for joint_index in 0..6 {
        let expected = measured[joint_index] - model[joint_index];
        if (residual[joint_index] - expected).abs() > TAU_RESIDUAL_TOLERANCE {
            return Err(WireError::InvalidStep {
                step_index,
                reason: format!(
                    "{arm_name}.tau_residual_nm[{joint_index}] must equal measured - model"
                ),
            });
        }
    }
    Ok(())
}

mod episode_id_bytes_serde {
    use std::fmt;

    use serde::de::{SeqAccess, Visitor};
    use serde::ser::SerializeTuple;
    use serde::{Deserializer, Serializer};

    const LEN: usize = 128;

    pub fn serialize<S>(bytes: &[u8; LEN], serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut tuple = serializer.serialize_tuple(LEN)?;
        for byte in bytes {
            tuple.serialize_element(byte)?;
        }
        tuple.end()
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<[u8; LEN], D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_tuple(LEN, BytesVisitor)
    }

    struct BytesVisitor;

    impl<'de> Visitor<'de> for BytesVisitor {
        type Value = [u8; LEN];

        fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            write!(formatter, "a fixed {LEN}-byte episode id field")
        }

        fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
        where
            A: SeqAccess<'de>,
        {
            let mut bytes = [0_u8; LEN];
            for (index, byte) in bytes.iter_mut().enumerate() {
                *byte = seq
                    .next_element()?
                    .ok_or_else(|| serde::de::Error::invalid_length(index, &self))?;
            }
            Ok(bytes)
        }
    }
}

#[cfg(test)]
fn write_test_steps(dir: &tempfile::TempDir, step_count: u64) -> PathBuf {
    let path = dir.path().join("steps.bin");
    let header = SvsHeaderV1::for_test("20260427T000000Z-test-abcdef123456");
    let steps: Vec<SvsStepV1> = (0..step_count).map(SvsStepV1::for_test).collect();
    write_steps_file(&path, &header, &steps).unwrap();
    path
}

#[cfg(test)]
fn write_test_steps_with_indexes(dir: &tempfile::TempDir, indexes: &[u64]) -> PathBuf {
    let path = dir.path().join("steps.bin");
    let header = SvsHeaderV1::for_test("20260427T000000Z-test-abcdef123456");
    let steps: Vec<SvsStepV1> = indexes.iter().copied().map(SvsStepV1::for_test).collect();
    let mut file = std::fs::File::create(&path).unwrap();
    write_header(&mut file, &header).unwrap();
    for step in &steps {
        write_step(&mut file, step).unwrap();
    }
    path
}

#[cfg(test)]
mod tests {
    use std::io::Write as _;

    use super::*;

    #[test]
    fn svs_episode_v1_round_trips_and_counts_records() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("steps.bin");
        let header = SvsHeaderV1::for_test("20260427T000000Z-test-abcdef123456");
        let step = SvsStepV1::for_test(0);

        write_steps_file(&path, &header, std::slice::from_ref(&step)).unwrap();
        let decoded = read_steps_file(&path).unwrap();

        assert_eq!(decoded.steps.len(), 1);
        assert_eq!(decoded.steps[0].step_index, 0);
    }

    #[test]
    fn svs_episode_rejects_trailing_garbage() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_test_steps(&dir, 1);
        std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(&[0xaa])
            .unwrap();
        assert!(read_steps_file(&path).is_err());
    }

    #[test]
    fn svs_episode_rejects_nonsequential_step_index() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_test_steps_with_indexes(&dir, &[0, 2]);
        assert!(read_steps_file(&path).is_err());
    }

    #[test]
    fn public_write_steps_file_rejects_nonsequential_indexes_without_final_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("steps.bin");
        let header = SvsHeaderV1::for_test("20260427T000000Z-test-abcdef123456");
        let steps = [SvsStepV1::for_test(0), SvsStepV1::for_test(2)];

        assert!(write_steps_file(&path, &header, &steps).is_err());
        assert!(!path.exists());
    }

    #[test]
    fn hard_link_failure_does_not_fallback_copy_to_final_file() {
        let dir = tempfile::tempdir().unwrap();
        let temp_dir = dir.path().join("temp-as-directory");
        let final_path = dir.path().join("steps.bin");
        std::fs::create_dir(&temp_dir).unwrap();

        assert!(persist_temp_no_overwrite(&temp_dir, &final_path).is_err());
        assert!(!final_path.exists());
        assert!(temp_dir.exists());
    }
}
