use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::episode::wire::StepFileSummary;

const EPISODE_ID_MAX_BYTES: usize = 128;
const RANDOM_SUFFIX_HEX_BYTES: usize = 12;
const SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Error)]
pub enum ManifestError {
    #[error("invalid task slug: {0}")]
    InvalidTaskSlug(String),
    #[error("invalid UTC timestamp: {0}")]
    InvalidTimestamp(String),
    #[error("invalid episode id: {0}")]
    InvalidEpisodeId(String),
    #[error("episode id directory collided after {attempts} attempts")]
    EpisodeIdCollision { attempts: usize },
    #[error("invalid manifest: {0}")]
    InvalidManifest(String),
    #[error("invalid report: {0}")]
    InvalidReport(String),
    #[error("random suffix generation failed: {0}")]
    Random(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EpisodeId {
    pub task_slug: String,
    pub episode_id: String,
    pub relative_dir: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct UtcTimestamp(String);

pub trait EpisodeRng {
    fn suffix_hex(&self) -> Result<String, ManifestError>;
}

#[derive(Debug, Clone, Copy, Default)]
pub struct OsEpisodeRng;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FixedEpisodeRng {
    suffix_hex: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EpisodeReservation {
    pub id: EpisodeId,
    pub absolute_dir: PathBuf,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EpisodeStatus {
    Running,
    Complete,
    Cancelled,
    Faulted,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProfileSourceKind {
    File,
    Generated,
    Captured,
    #[serde(rename = "built-in-defaults")]
    BuiltInDefaults,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ManifestV1 {
    pub schema_version: u32,
    pub episode_id: String,
    pub task: TaskManifest,
    pub timestamps: EpisodeTimestamps,
    pub status: EpisodeStatus,
    pub targets: TargetSpecsManifest,
    pub mujoco: MujocoManifest,
    pub dynamics: DynamicsManifest,
    pub task_profile: SourceArtifactManifest,
    pub effective_profile: EffectiveProfileManifest,
    pub gripper: GripperMirrorManifest,
    pub calibration: CalibrationManifest,
    pub mirror_map: MirrorMapManifest,
    pub collector: CollectorIdentityManifest,
    pub raw_can: RawCanManifest,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw_clock: Option<RawClockManifest>,
    pub step_file: StepFileManifest,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TaskManifest {
    pub raw_name: String,
    pub slug: String,
    pub operator: Option<String>,
    pub notes: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct EpisodeTimestamps {
    pub started_unix_ns: u64,
    pub ended_unix_ns: Option<u64>,
    pub episode_start_host_mono_us: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TargetSpecsManifest {
    pub master: TargetSpecManifest,
    pub slave: TargetSpecManifest,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TargetSpecManifest {
    pub requested: String,
    pub resolved: String,
    pub backend: String,
    pub identifier: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MujocoManifest {
    pub source_kind: ProfileSourceKind,
    pub source_path: Option<PathBuf>,
    pub root_xml_relative_path: PathBuf,
    pub model: HashedArtifactManifest,
    pub runtime: MujocoRuntimeIdentity,
    pub master_ee_site: String,
    pub slave_ee_site: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MujocoRuntimeIdentity {
    pub version: Option<String>,
    pub build_string: Option<String>,
    pub rust_binding_version: Option<String>,
    pub native_library_sha256_hex: Option<String>,
    pub static_build_identity: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DynamicsManifest {
    pub master: ArmDynamicsManifest,
    pub slave: ArmDynamicsManifest,
    pub qacc_lpf_cutoff_hz: f64,
    pub acceleration_clamp: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ArmDynamicsManifest {
    pub mode: String,
    pub payload: PayloadManifest,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub struct PayloadManifest {
    pub mass_kg: f64,
    pub com_m: [f64; 3],
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SourceArtifactManifest {
    pub source_kind: ProfileSourceKind,
    pub source_path: Option<PathBuf>,
    pub hash_algorithm: Option<String>,
    pub sha256_hex: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EffectiveProfileManifest {
    pub path: PathBuf,
    pub hash_algorithm: String,
    pub sha256_hex: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HashedArtifactManifest {
    pub source_kind: ProfileSourceKind,
    pub source_path: Option<PathBuf>,
    pub hash_algorithm: String,
    pub sha256_hex: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GripperMirrorManifest {
    pub mirror_enabled: bool,
    pub disable_gripper_requested: bool,
    pub disable_gripper_effective: bool,
    pub update_divider: u32,
    pub position_deadband: f64,
    pub effort_scale: f64,
    pub max_feedback_age_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CalibrationManifest {
    pub source_path: Option<PathBuf>,
    pub hash_algorithm: String,
    pub sha256_hex: String,
    pub master_zero_rad: [f64; 6],
    pub slave_zero_rad: [f64; 6],
    pub effective_mirror_map: JointMirrorMapManifest,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MirrorMapManifest {
    pub source_kind: Option<ProfileSourceKind>,
    pub source_path: Option<PathBuf>,
    pub hash_algorithm: Option<String>,
    pub sha256_hex: Option<String>,
    pub effective: JointMirrorMapManifest,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub struct JointMirrorMapManifest {
    pub permutation: [usize; 6],
    pub position_sign: [f64; 6],
    pub velocity_sign: [f64; 6],
    pub torque_sign: [f64; 6],
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CollectorIdentityManifest {
    pub version: Option<String>,
    pub revision: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RawCanManifest {
    pub enabled: bool,
    pub finalizer_status: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StepFileManifest {
    pub relative_path: PathBuf,
    pub step_count: u64,
    pub last_step_index: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RawClockManifest {
    pub timing_source: String,
    pub strict_realtime: bool,
    pub experimental: bool,
    pub warmup_secs: u64,
    pub residual_p95_us: u64,
    pub residual_max_us: u64,
    pub drift_abs_ppm: f64,
    pub sample_gap_max_ms: u64,
    pub last_sample_age_ms: u64,
    pub selected_sample_age_ms: u64,
    pub inter_arm_skew_max_us: u64,
    pub state_skew_max_us: u64,
    pub residual_max_consecutive_failures: u32,
    pub alignment_lag_us: u64,
    pub alignment_search_window_us: u64,
    pub alignment_buffer_miss_consecutive_failures: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ReportJson {
    pub schema_version: u32,
    pub episode_id: String,
    pub status: EpisodeStatus,
    pub fault_classification: Option<String>,
    pub raw_can_enabled: bool,
    pub raw_can_degraded: bool,
    pub raw_can_finalizer_status: Option<String>,
    pub final_flush_result: WriterFlushResultJson,
    pub started_unix_ns: u64,
    pub ended_unix_ns: u64,
    pub step_count: u64,
    pub last_step_index: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw_clock: Option<RawClockReportJson>,
    pub dual_arm: DualArmReportJson,
    pub writer: WriterReportJson,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RawClockReportJson {
    pub timing_source: String,
    pub strict_realtime: bool,
    pub experimental: bool,
    pub warmup_secs: u64,
    pub residual_p95_us: u64,
    pub residual_max_us: u64,
    pub drift_abs_ppm: f64,
    pub sample_gap_max_ms: u64,
    pub last_sample_age_ms: u64,
    pub selected_sample_age_ms: u64,
    pub inter_arm_skew_max_us: u64,
    pub state_skew_max_us: u64,
    pub residual_max_consecutive_failures: u32,
    pub alignment_buffer_miss_consecutive_failure_threshold: u32,
    pub master_clock_drift_ppm: f64,
    pub slave_clock_drift_ppm: f64,
    pub master_residual_p95_us: u64,
    pub slave_residual_p95_us: u64,
    pub selected_inter_arm_skew_max_us: u64,
    pub selected_inter_arm_skew_p95_us: u64,
    pub latest_inter_arm_skew_max_us: u64,
    pub latest_inter_arm_skew_p95_us: u64,
    pub alignment_lag_us: u64,
    pub alignment_search_window_us: u64,
    pub alignment_buffer_misses: u64,
    pub alignment_buffer_miss_consecutive_max: u32,
    pub alignment_buffer_miss_consecutive_failures: u32,
    pub master_residual_max_spikes: u64,
    pub slave_residual_max_spikes: u64,
    pub master_residual_max_consecutive_failures: u32,
    pub slave_residual_max_consecutive_failures: u32,
    pub clock_health_failures: u64,
    pub read_faults: u32,
    pub submission_faults: u32,
    pub runtime_faults: u32,
    pub compensation_faults: u32,
    pub controller_faults: u32,
    pub telemetry_sink_faults: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub final_failure_kind: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WriterFlushResultJson {
    pub success: bool,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WriterReportJson {
    pub queue_full_stop_events: u64,
    pub queue_full_stop_duration_ms: u64,
    pub queue_full_events: u64,
    pub dropped_step_count: u64,
    pub first_queue_full_host_mono_us: Option<u64>,
    pub latest_queue_full_host_mono_us: Option<u64>,
    pub max_queue_depth: u32,
    pub final_queue_depth: u32,
    pub backpressure_threshold_tripped: bool,
    pub flush_failed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DualArmReportJson {
    pub iterations: u64,
    pub read_faults: u32,
    pub submission_faults: u32,
    pub last_submission_failed_arm: Option<String>,
    pub peer_command_may_have_applied: bool,
    pub deadline_misses: u64,
    pub max_inter_arm_skew_ns: u64,
    pub max_real_dt_ns: u64,
    pub max_cycle_lag_ns: u64,
    pub left_tx_realtime_overwrites_total: u64,
    pub right_tx_realtime_overwrites_total: u64,
    pub left_tx_frames_sent_total: u64,
    pub right_tx_frames_sent_total: u64,
    pub left_tx_fault_aborts_total: u64,
    pub right_tx_fault_aborts_total: u64,
    pub last_runtime_fault_left: Option<String>,
    pub last_runtime_fault_right: Option<String>,
    pub exit_reason: Option<String>,
    pub left_stop_attempt: String,
    pub right_stop_attempt: String,
    pub last_error: Option<String>,
}

impl EpisodeId {
    pub fn generate(
        raw_task_name: &str,
        timestamp: UtcTimestamp,
        rng: &impl EpisodeRng,
    ) -> Result<Self, ManifestError> {
        let task_slug = slugify_task_name(raw_task_name)?;
        let suffix_hex = rng.suffix_hex()?;
        validate_suffix_hex(&suffix_hex)?;

        let episode_id = format!("{}-{task_slug}-{suffix_hex}", timestamp.as_str());
        if episode_id.len() > EPISODE_ID_MAX_BYTES {
            return Err(ManifestError::InvalidEpisodeId(format!(
                "episode id is {} bytes; max is {EPISODE_ID_MAX_BYTES}",
                episode_id.len()
            )));
        }

        Ok(Self {
            relative_dir: PathBuf::from(&task_slug).join(&episode_id),
            task_slug,
            episode_id,
        })
    }

    pub fn reserve_directory(
        output_dir: impl AsRef<Path>,
        raw_task_name: &str,
        timestamp: UtcTimestamp,
        rng: &impl EpisodeRng,
        max_attempts: usize,
    ) -> Result<EpisodeReservation, ManifestError> {
        if max_attempts == 0 {
            return Err(ManifestError::EpisodeIdCollision { attempts: 0 });
        }

        let output_dir = output_dir.as_ref();
        for _ in 0..max_attempts {
            let id = Self::generate(raw_task_name, timestamp.clone(), rng)?;
            let task_dir = output_dir.join(&id.task_slug);
            let absolute_dir = output_dir.join(&id.relative_dir);

            fs::create_dir_all(&task_dir)?;
            match fs::create_dir(&absolute_dir) {
                Ok(()) => return Ok(EpisodeReservation { id, absolute_dir }),
                Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(err) => return Err(err.into()),
            }
        }

        Err(ManifestError::EpisodeIdCollision {
            attempts: max_attempts,
        })
    }
}

impl UtcTimestamp {
    pub fn parse(value: &str) -> Result<Self, ManifestError> {
        validate_utc_timestamp(value)?;
        Ok(Self(value.to_string()))
    }

    pub fn for_tests() -> Self {
        Self::parse("20260428T010203Z").expect("valid test timestamp")
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl EpisodeRng for OsEpisodeRng {
    fn suffix_hex(&self) -> Result<String, ManifestError> {
        let mut bytes = [0_u8; 6];
        getrandom::fill(&mut bytes).map_err(|err| ManifestError::Random(err.to_string()))?;
        Ok(hex_encode(&bytes))
    }
}

impl FixedEpisodeRng {
    pub fn from_hex(hex: &str) -> Self {
        validate_suffix_hex(hex).expect("fixed test episode RNG suffix must be 12 hex chars");
        Self {
            suffix_hex: hex.to_ascii_lowercase(),
        }
    }

    pub fn zero() -> Self {
        Self::from_hex("000000000000")
    }
}

impl EpisodeRng for FixedEpisodeRng {
    fn suffix_hex(&self) -> Result<String, ManifestError> {
        Ok(self.suffix_hex.clone())
    }
}

impl ManifestV1 {
    pub fn validate(&self) -> Result<(), ManifestError> {
        if self.schema_version != SCHEMA_VERSION {
            return Err(ManifestError::InvalidManifest(format!(
                "schema_version must be {SCHEMA_VERSION}"
            )));
        }
        if self.episode_id.is_empty() || self.episode_id.len() > EPISODE_ID_MAX_BYTES {
            return Err(ManifestError::InvalidManifest(
                "episode_id must be non-empty and <=128 bytes".to_string(),
            ));
        }
        if slugify_task_name(&self.task.raw_name)? != self.task.slug {
            return Err(ManifestError::InvalidManifest(
                "task.slug must match raw task name".to_string(),
            ));
        }
        validate_timestamps(&self.timestamps)?;
        self.targets.validate()?;
        self.mujoco.validate()?;
        self.dynamics.validate()?;
        self.task_profile.validate("task_profile")?;
        self.effective_profile.validate()?;
        self.gripper.validate()?;
        self.calibration.validate()?;
        self.mirror_map.validate()?;
        self.collector.validate()?;
        if let Some(raw_clock) = &self.raw_clock {
            raw_clock.validate()?;
        }
        self.step_file.validate()?;
        Ok(())
    }

    pub fn validate_against_step_file_summary(
        &self,
        summary: &StepFileSummary,
    ) -> Result<(), ManifestError> {
        self.validate()?;
        validate_step_summary(
            "manifest.step_file",
            &self.episode_id,
            self.step_file.step_count,
            self.step_file.last_step_index,
            summary,
        )
    }

    pub fn without_mujoco_runtime_identity(mut self) -> Self {
        self.mujoco.runtime.version = None;
        self.mujoco.runtime.build_string = None;
        self.mujoco.runtime.native_library_sha256_hex = None;
        self.mujoco.runtime.static_build_identity = None;
        self
    }

    pub fn without_effective_profile_hash(mut self) -> Self {
        self.effective_profile.sha256_hex.clear();
        self
    }

    pub fn for_test_complete() -> Self {
        let slug = "surface-following-demo".to_string();
        let episode_id = "20260428T010203Z-surface-following-demo-a1b2c3d4e5f6".to_string();
        Self {
            schema_version: SCHEMA_VERSION,
            episode_id: episode_id.clone(),
            task: TaskManifest {
                raw_name: "Surface Following Demo!".to_string(),
                slug,
                operator: Some("operator".to_string()),
                notes: Some("test manifest".to_string()),
            },
            timestamps: EpisodeTimestamps {
                started_unix_ns: 1_777_292_523_000_000_000,
                ended_unix_ns: Some(1_777_292_524_000_000_000),
                episode_start_host_mono_us: 42,
            },
            status: EpisodeStatus::Complete,
            targets: TargetSpecsManifest::for_tests(),
            mujoco: MujocoManifest::for_tests(),
            dynamics: DynamicsManifest::for_tests(),
            task_profile: SourceArtifactManifest {
                source_kind: ProfileSourceKind::File,
                source_path: Some(PathBuf::from("profiles/surface-following.toml")),
                hash_algorithm: Some("sha256".to_string()),
                sha256_hex: Some(test_hash('1')),
            },
            effective_profile: EffectiveProfileManifest {
                path: PathBuf::from("effective-profile.toml"),
                hash_algorithm: "sha256".to_string(),
                sha256_hex: test_hash('2'),
            },
            gripper: GripperMirrorManifest {
                mirror_enabled: true,
                disable_gripper_requested: false,
                disable_gripper_effective: false,
                update_divider: 4,
                position_deadband: 0.02,
                effort_scale: 1.0,
                max_feedback_age_ms: 100,
            },
            calibration: CalibrationManifest {
                source_path: Some(PathBuf::from("calibration.toml")),
                hash_algorithm: "sha256".to_string(),
                sha256_hex: test_hash('3'),
                master_zero_rad: [0.0; 6],
                slave_zero_rad: [0.0; 6],
                effective_mirror_map: JointMirrorMapManifest::for_tests(),
            },
            mirror_map: MirrorMapManifest {
                source_kind: Some(ProfileSourceKind::File),
                source_path: Some(PathBuf::from("mirror-map.toml")),
                hash_algorithm: Some("sha256".to_string()),
                sha256_hex: Some(test_hash('4')),
                effective: JointMirrorMapManifest::for_tests(),
            },
            collector: CollectorIdentityManifest {
                version: Some(env!("CARGO_PKG_VERSION").to_string()),
                revision: option_env!("GIT_COMMIT").map(str::to_string),
            },
            raw_can: RawCanManifest {
                enabled: false,
                finalizer_status: Some("not_enabled".to_string()),
            },
            raw_clock: None,
            step_file: StepFileManifest {
                relative_path: PathBuf::from("steps.bin"),
                step_count: 1,
                last_step_index: Some(0),
            },
        }
    }
}

impl TargetSpecsManifest {
    fn for_tests() -> Self {
        Self {
            master: TargetSpecManifest {
                requested: "socketcan:vcan0".to_string(),
                resolved: "socketcan:vcan0".to_string(),
                backend: "socketcan".to_string(),
                identifier: "vcan0".to_string(),
            },
            slave: TargetSpecManifest {
                requested: "socketcan:vcan1".to_string(),
                resolved: "socketcan:vcan1".to_string(),
                backend: "socketcan".to_string(),
                identifier: "vcan1".to_string(),
            },
        }
    }

    fn validate(&self) -> Result<(), ManifestError> {
        self.master.validate("targets.master")?;
        self.slave.validate("targets.slave")
    }
}

impl TargetSpecManifest {
    fn validate(&self, field: &str) -> Result<(), ManifestError> {
        require_nonempty(&self.requested, &format!("{field}.requested"))?;
        require_nonempty(&self.resolved, &format!("{field}.resolved"))?;
        require_nonempty(&self.backend, &format!("{field}.backend"))?;
        require_nonempty(&self.identifier, &format!("{field}.identifier"))
    }
}

impl MujocoManifest {
    fn for_tests() -> Self {
        Self {
            source_kind: ProfileSourceKind::File,
            source_path: Some(PathBuf::from("models/piper_scene.xml")),
            root_xml_relative_path: PathBuf::from("models/piper_scene.xml"),
            model: HashedArtifactManifest {
                source_kind: ProfileSourceKind::File,
                source_path: Some(PathBuf::from("models/piper_scene.xml")),
                hash_algorithm: "sha256".to_string(),
                sha256_hex: test_hash('5'),
            },
            runtime: MujocoRuntimeIdentity {
                version: Some("3.3.7".to_string()),
                build_string: Some("mujoco-3.3.7-test".to_string()),
                rust_binding_version: Some("2.3.5".to_string()),
                native_library_sha256_hex: Some(test_hash('6')),
                static_build_identity: None,
            },
            master_ee_site: "master_ee".to_string(),
            slave_ee_site: "slave_ee".to_string(),
        }
    }

    fn validate(&self) -> Result<(), ManifestError> {
        if self.root_xml_relative_path.as_os_str().is_empty() {
            return Err(ManifestError::InvalidManifest(
                "mujoco.root_xml_relative_path is required".to_string(),
            ));
        }
        self.model.validate("mujoco.model")?;
        self.runtime.validate()?;
        require_nonempty(&self.master_ee_site, "mujoco.master_ee_site")?;
        require_nonempty(&self.slave_ee_site, "mujoco.slave_ee_site")
    }
}

impl MujocoRuntimeIdentity {
    fn validate(&self) -> Result<(), ManifestError> {
        require_option_nonempty(self.version.as_deref(), "mujoco.runtime.version")?;
        require_option_nonempty(self.build_string.as_deref(), "mujoco.runtime.build_string")?;
        match (
            self.native_library_sha256_hex.as_deref(),
            self.static_build_identity.as_deref(),
        ) {
            (Some(hash), _) => {
                validate_sha256_hex("mujoco.runtime.native_library_sha256_hex", hash)
            },
            (None, Some(identity)) if !identity.trim().is_empty() => Ok(()),
            _ => Err(ManifestError::InvalidManifest(
                "mujoco.runtime requires native library hash or static build identity".to_string(),
            )),
        }
    }
}

impl DynamicsManifest {
    fn for_tests() -> Self {
        Self {
            master: ArmDynamicsManifest::for_tests("full"),
            slave: ArmDynamicsManifest::for_tests("full"),
            qacc_lpf_cutoff_hz: 20.0,
            acceleration_clamp: 200.0,
        }
    }

    fn validate(&self) -> Result<(), ManifestError> {
        self.master.validate("dynamics.master")?;
        self.slave.validate("dynamics.slave")?;
        validate_finite("dynamics.qacc_lpf_cutoff_hz", self.qacc_lpf_cutoff_hz)?;
        validate_finite("dynamics.acceleration_clamp", self.acceleration_clamp)
    }
}

impl ArmDynamicsManifest {
    fn for_tests(mode: &str) -> Self {
        Self {
            mode: mode.to_string(),
            payload: PayloadManifest {
                mass_kg: 0.0,
                com_m: [0.0; 3],
            },
        }
    }

    fn validate(&self, field: &str) -> Result<(), ManifestError> {
        require_nonempty(&self.mode, &format!("{field}.mode"))?;
        validate_finite(&format!("{field}.payload.mass_kg"), self.payload.mass_kg)?;
        validate_finite_array(&format!("{field}.payload.com_m"), &self.payload.com_m)
    }
}

impl SourceArtifactManifest {
    fn validate(&self, field: &str) -> Result<(), ManifestError> {
        if self.source_kind == ProfileSourceKind::File {
            if self.source_path.as_ref().is_none_or(|path| path.as_os_str().is_empty()) {
                return Err(ManifestError::InvalidManifest(format!(
                    "{field}.source_path is required for file source"
                )));
            }
            require_option_nonempty(
                self.hash_algorithm.as_deref(),
                &format!("{field}.hash_algorithm"),
            )?;
            validate_optional_sha256_hex(
                &format!("{field}.sha256_hex"),
                self.sha256_hex.as_deref(),
            )?;
        }
        if self.source_kind == ProfileSourceKind::BuiltInDefaults
            && (self.source_path.is_some()
                || self.hash_algorithm.is_some()
                || self.sha256_hex.is_some())
        {
            return Err(ManifestError::InvalidManifest(format!(
                "{field} built-in-defaults source must not include file path or hash"
            )));
        }
        Ok(())
    }
}

impl EffectiveProfileManifest {
    fn validate(&self) -> Result<(), ManifestError> {
        if self.path.as_os_str().is_empty() {
            return Err(ManifestError::InvalidManifest(
                "effective_profile.path is required".to_string(),
            ));
        }
        require_nonempty(&self.hash_algorithm, "effective_profile.hash_algorithm")?;
        validate_sha256_hex("effective_profile.sha256_hex", &self.sha256_hex)
    }
}

impl HashedArtifactManifest {
    fn validate(&self, field: &str) -> Result<(), ManifestError> {
        if self.source_kind == ProfileSourceKind::File
            && self.source_path.as_ref().is_none_or(|path| path.as_os_str().is_empty())
        {
            return Err(ManifestError::InvalidManifest(format!(
                "{field}.source_path is required for file source"
            )));
        }
        require_nonempty(&self.hash_algorithm, &format!("{field}.hash_algorithm"))?;
        validate_sha256_hex(&format!("{field}.sha256_hex"), &self.sha256_hex)
    }
}

impl GripperMirrorManifest {
    fn validate(&self) -> Result<(), ManifestError> {
        if self.update_divider == 0 {
            return Err(ManifestError::InvalidManifest(
                "gripper.update_divider must be non-zero".to_string(),
            ));
        }
        if self.max_feedback_age_ms == 0 {
            return Err(ManifestError::InvalidManifest(
                "gripper.max_feedback_age_ms must be positive".to_string(),
            ));
        }
        validate_finite("gripper.position_deadband", self.position_deadband)?;
        validate_finite("gripper.effort_scale", self.effort_scale)
    }
}

impl CalibrationManifest {
    fn validate(&self) -> Result<(), ManifestError> {
        require_nonempty(&self.hash_algorithm, "calibration.hash_algorithm")?;
        validate_sha256_hex("calibration.sha256_hex", &self.sha256_hex)?;
        validate_finite_array("calibration.master_zero_rad", &self.master_zero_rad)?;
        validate_finite_array("calibration.slave_zero_rad", &self.slave_zero_rad)?;
        self.effective_mirror_map.validate("calibration.effective_mirror_map")
    }
}

impl MirrorMapManifest {
    fn validate(&self) -> Result<(), ManifestError> {
        if self.source_kind.is_none() {
            return Err(ManifestError::InvalidManifest(
                "mirror_map.source_kind is required".to_string(),
            ));
        }
        if self.source_kind == Some(ProfileSourceKind::File) {
            if self.source_path.as_ref().is_none_or(|path| path.as_os_str().is_empty()) {
                return Err(ManifestError::InvalidManifest(
                    "mirror_map.source_path is required for file source".to_string(),
                ));
            }
            require_option_nonempty(self.hash_algorithm.as_deref(), "mirror_map.hash_algorithm")?;
            validate_optional_sha256_hex("mirror_map.sha256_hex", self.sha256_hex.as_deref())?;
        }
        self.effective.validate("mirror_map.effective")
    }
}

impl JointMirrorMapManifest {
    fn for_tests() -> Self {
        Self {
            permutation: [0, 1, 2, 3, 4, 5],
            position_sign: [1.0; 6],
            velocity_sign: [1.0; 6],
            torque_sign: [1.0; 6],
        }
    }

    fn validate(&self, field: &str) -> Result<(), ManifestError> {
        let mut seen = [false; 6];
        for (index, value) in self.permutation.iter().copied().enumerate() {
            if value >= 6 || seen[value] {
                return Err(ManifestError::InvalidManifest(format!(
                    "{field}.permutation[{index}] is invalid"
                )));
            }
            seen[value] = true;
        }
        validate_sign_array(&format!("{field}.position_sign"), &self.position_sign)?;
        validate_sign_array(&format!("{field}.velocity_sign"), &self.velocity_sign)?;
        validate_sign_array(&format!("{field}.torque_sign"), &self.torque_sign)
    }
}

impl CollectorIdentityManifest {
    fn validate(&self) -> Result<(), ManifestError> {
        if self.version.as_deref().is_some_and(|value| !value.trim().is_empty())
            || self.revision.as_deref().is_some_and(|value| !value.trim().is_empty())
        {
            Ok(())
        } else {
            Err(ManifestError::InvalidManifest(
                "collector.version or collector.revision is required".to_string(),
            ))
        }
    }
}

impl StepFileManifest {
    fn validate(&self) -> Result<(), ManifestError> {
        if self.relative_path != Path::new("steps.bin") {
            return Err(ManifestError::InvalidManifest(
                "step_file.relative_path must be exactly steps.bin".to_string(),
            ));
        }
        if self.step_count == 0 && self.last_step_index.is_some() {
            return Err(ManifestError::InvalidManifest(
                "step_file.last_step_index must be absent when step_count is zero".to_string(),
            ));
        }
        if self.step_count > 0 && self.last_step_index != Some(self.step_count - 1) {
            return Err(ManifestError::InvalidManifest(
                "step_file.last_step_index must equal step_count - 1".to_string(),
            ));
        }
        Ok(())
    }
}

impl RawClockManifest {
    fn validate(&self) -> Result<(), ManifestError> {
        require_nonempty(&self.timing_source, "raw_clock.timing_source")?;
        if self.timing_source != "calibrated_hw_raw" {
            return Err(ManifestError::InvalidManifest(
                "raw_clock.timing_source must be calibrated_hw_raw".to_string(),
            ));
        }
        if self.strict_realtime || !self.experimental {
            return Err(ManifestError::InvalidManifest(
                "raw_clock must be experimental and non-strict".to_string(),
            ));
        }
        validate_finite("raw_clock.drift_abs_ppm", self.drift_abs_ppm)
    }
}

impl ReportJson {
    pub fn validate(&self) -> Result<(), ManifestError> {
        if self.schema_version != SCHEMA_VERSION {
            return Err(ManifestError::InvalidReport(format!(
                "schema_version must be {SCHEMA_VERSION}"
            )));
        }
        if self.episode_id.is_empty() || self.episode_id.len() > EPISODE_ID_MAX_BYTES {
            return Err(ManifestError::InvalidReport(
                "episode_id must be non-empty and <=128 bytes".to_string(),
            ));
        }
        if self.started_unix_ns == 0 || self.ended_unix_ns < self.started_unix_ns {
            return Err(ManifestError::InvalidReport(
                "invalid report timestamps".to_string(),
            ));
        }
        if self.step_count == 0 && self.last_step_index.is_some() {
            return Err(ManifestError::InvalidReport(
                "last_step_index must be absent when step_count is zero".to_string(),
            ));
        }
        if self.step_count > 0 && self.last_step_index != Some(self.step_count - 1) {
            return Err(ManifestError::InvalidReport(
                "last_step_index must equal step_count - 1".to_string(),
            ));
        }
        if self.writer.final_queue_depth > self.writer.max_queue_depth {
            return Err(ManifestError::InvalidReport(
                "writer.final_queue_depth must be <= max_queue_depth".to_string(),
            ));
        }
        if self.dual_arm.iterations < self.step_count {
            return Err(ManifestError::InvalidReport(
                "dual_arm.iterations must be >= step_count".to_string(),
            ));
        }
        if let Some(raw_clock) = &self.raw_clock {
            raw_clock.validate()?;
        }
        let has_writer_fault_indicator = !self.final_flush_result.success
            || self.final_flush_result.error.is_some()
            || self.writer.flush_failed
            || self.writer.queue_full_events > 0
            || self.writer.dropped_step_count > 0
            || self.writer.backpressure_threshold_tripped;
        if self.status != EpisodeStatus::Faulted && has_writer_fault_indicator {
            return Err(ManifestError::InvalidReport(
                "writer fault indicators require faulted status".to_string(),
            ));
        }
        Ok(())
    }

    pub fn validate_against_step_file_summary(
        &self,
        summary: &StepFileSummary,
    ) -> Result<(), ManifestError> {
        self.validate()?;
        validate_step_summary(
            "report",
            &self.episode_id,
            self.step_count,
            self.last_step_index,
            summary,
        )
    }

    pub fn for_test_faulted() -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            episode_id: "20260428T010203Z-surface-following-demo-a1b2c3d4e5f6".to_string(),
            status: EpisodeStatus::Faulted,
            fault_classification: Some("writer_queue_full".to_string()),
            raw_can_enabled: true,
            raw_can_degraded: true,
            raw_can_finalizer_status: Some("degraded".to_string()),
            final_flush_result: WriterFlushResultJson {
                success: false,
                error: Some("queue full".to_string()),
            },
            started_unix_ns: 1_777_292_523_000_000_000,
            ended_unix_ns: 1_777_292_524_000_000_000,
            step_count: 10,
            last_step_index: Some(9),
            raw_clock: None,
            dual_arm: DualArmReportJson {
                iterations: 10,
                read_faults: 0,
                submission_faults: 0,
                last_submission_failed_arm: None,
                peer_command_may_have_applied: false,
                deadline_misses: 0,
                max_inter_arm_skew_ns: 0,
                max_real_dt_ns: 0,
                max_cycle_lag_ns: 0,
                left_tx_realtime_overwrites_total: 0,
                right_tx_realtime_overwrites_total: 0,
                left_tx_frames_sent_total: 10,
                right_tx_frames_sent_total: 10,
                left_tx_fault_aborts_total: 0,
                right_tx_fault_aborts_total: 0,
                last_runtime_fault_left: None,
                last_runtime_fault_right: None,
                exit_reason: Some("TelemetrySinkFault".to_string()),
                left_stop_attempt: "ConfirmedSent".to_string(),
                right_stop_attempt: "ConfirmedSent".to_string(),
                last_error: Some("writer queue full".to_string()),
            },
            writer: WriterReportJson {
                queue_full_stop_events: 2,
                queue_full_stop_duration_ms: 100,
                queue_full_events: 1,
                dropped_step_count: 1,
                first_queue_full_host_mono_us: Some(100),
                latest_queue_full_host_mono_us: Some(100),
                max_queue_depth: 1,
                final_queue_depth: 0,
                backpressure_threshold_tripped: false,
                flush_failed: false,
            },
        }
    }
}

impl RawClockReportJson {
    fn validate(&self) -> Result<(), ManifestError> {
        require_report_nonempty(&self.timing_source, "raw_clock.timing_source")?;
        if self.timing_source != "calibrated_hw_raw" {
            return Err(ManifestError::InvalidReport(
                "raw_clock.timing_source must be calibrated_hw_raw".to_string(),
            ));
        }
        if self.strict_realtime || !self.experimental {
            return Err(ManifestError::InvalidReport(
                "raw_clock report must be experimental and non-strict".to_string(),
            ));
        }
        validate_report_finite("raw_clock.drift_abs_ppm", self.drift_abs_ppm)?;
        validate_report_finite(
            "raw_clock.master_clock_drift_ppm",
            self.master_clock_drift_ppm,
        )?;
        validate_report_finite(
            "raw_clock.slave_clock_drift_ppm",
            self.slave_clock_drift_ppm,
        )
    }
}

pub fn slugify_task_name(raw_task_name: &str) -> Result<String, ManifestError> {
    let mut slug = String::new();
    let mut previous_was_separator = true;

    for ch in raw_task_name.chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch.to_ascii_lowercase());
            previous_was_separator = false;
        } else if !previous_was_separator {
            slug.push('-');
            previous_was_separator = true;
        }
    }

    while slug.ends_with('-') {
        slug.pop();
    }

    if slug.is_empty() {
        return Err(ManifestError::InvalidTaskSlug(
            "task name must contain ASCII alphanumeric characters".to_string(),
        ));
    }

    let worst_case_episode_id_len =
        "20260428T010203Z".len() + 1 + slug.len() + 1 + RANDOM_SUFFIX_HEX_BYTES;
    if worst_case_episode_id_len > EPISODE_ID_MAX_BYTES {
        return Err(ManifestError::InvalidTaskSlug(format!(
            "slug would make episode id {worst_case_episode_id_len} bytes; max is {EPISODE_ID_MAX_BYTES}"
        )));
    }

    Ok(slug)
}

fn validate_utc_timestamp(value: &str) -> Result<(), ManifestError> {
    if value.len() != 16
        || value.as_bytes()[8] != b'T'
        || value.as_bytes()[15] != b'Z'
        || !value[..8].chars().all(|ch| ch.is_ascii_digit())
        || !value[9..15].chars().all(|ch| ch.is_ascii_digit())
    {
        return Err(ManifestError::InvalidTimestamp(value.to_string()));
    }

    let month: u32 = value[4..6]
        .parse()
        .map_err(|_| ManifestError::InvalidTimestamp(value.to_string()))?;
    let day: u32 = value[6..8]
        .parse()
        .map_err(|_| ManifestError::InvalidTimestamp(value.to_string()))?;
    let hour: u32 = value[9..11]
        .parse()
        .map_err(|_| ManifestError::InvalidTimestamp(value.to_string()))?;
    let minute: u32 = value[11..13]
        .parse()
        .map_err(|_| ManifestError::InvalidTimestamp(value.to_string()))?;
    let second: u32 = value[13..15]
        .parse()
        .map_err(|_| ManifestError::InvalidTimestamp(value.to_string()))?;

    if !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
        || hour > 23
        || minute > 59
        || second > 59
    {
        return Err(ManifestError::InvalidTimestamp(value.to_string()));
    }

    Ok(())
}

fn validate_suffix_hex(hex: &str) -> Result<(), ManifestError> {
    if hex.len() == RANDOM_SUFFIX_HEX_BYTES && hex.chars().all(|ch| ch.is_ascii_hexdigit()) {
        Ok(())
    } else {
        Err(ManifestError::InvalidEpisodeId(
            "random suffix must be 12 hex characters".to_string(),
        ))
    }
}

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

fn test_hash(ch: char) -> String {
    std::iter::repeat_n(ch, 64).collect()
}

fn validate_timestamps(timestamps: &EpisodeTimestamps) -> Result<(), ManifestError> {
    if timestamps.started_unix_ns == 0 {
        return Err(ManifestError::InvalidManifest(
            "timestamps.started_unix_ns is required".to_string(),
        ));
    }
    if timestamps.ended_unix_ns.is_some_and(|ended| ended < timestamps.started_unix_ns) {
        return Err(ManifestError::InvalidManifest(
            "timestamps.ended_unix_ns must be >= started_unix_ns".to_string(),
        ));
    }
    Ok(())
}

fn validate_step_summary(
    field: &str,
    episode_id: &str,
    step_count: u64,
    last_step_index: Option<u64>,
    summary: &StepFileSummary,
) -> Result<(), ManifestError> {
    if episode_id != summary.episode_id
        || step_count != summary.step_count
        || last_step_index != summary.last_step_index
    {
        return Err(ManifestError::InvalidManifest(format!(
            "{field} step metadata does not match decoded steps.bin"
        )));
    }
    Ok(())
}

fn validate_sha256_hex(field: &str, value: &str) -> Result<(), ManifestError> {
    if value.len() == 64 && value.chars().all(|ch| ch.is_ascii_hexdigit()) {
        Ok(())
    } else {
        Err(ManifestError::InvalidManifest(format!(
            "{field} must be a 64-character SHA-256 hex digest"
        )))
    }
}

fn validate_optional_sha256_hex(field: &str, value: Option<&str>) -> Result<(), ManifestError> {
    match value {
        Some(value) => validate_sha256_hex(field, value),
        None => Err(ManifestError::InvalidManifest(format!(
            "{field} is required"
        ))),
    }
}

fn require_nonempty(value: &str, field: &str) -> Result<(), ManifestError> {
    if value.trim().is_empty() {
        Err(ManifestError::InvalidManifest(format!(
            "{field} is required"
        )))
    } else {
        Ok(())
    }
}

fn require_option_nonempty(value: Option<&str>, field: &str) -> Result<(), ManifestError> {
    match value {
        Some(value) => require_nonempty(value, field),
        None => Err(ManifestError::InvalidManifest(format!(
            "{field} is required"
        ))),
    }
}

fn require_report_nonempty(value: &str, field: &str) -> Result<(), ManifestError> {
    if value.trim().is_empty() {
        Err(ManifestError::InvalidReport(format!("{field} is required")))
    } else {
        Ok(())
    }
}

fn validate_finite(field: &str, value: f64) -> Result<(), ManifestError> {
    if value.is_finite() {
        Ok(())
    } else {
        Err(ManifestError::InvalidManifest(format!(
            "{field} must be finite"
        )))
    }
}

fn validate_report_finite(name: &str, value: f64) -> Result<(), ManifestError> {
    if !value.is_finite() {
        return Err(ManifestError::InvalidReport(format!(
            "{name} must be finite"
        )));
    }
    Ok(())
}

fn validate_finite_array<const N: usize>(
    field: &str,
    values: &[f64; N],
) -> Result<(), ManifestError> {
    for (index, value) in values.iter().copied().enumerate() {
        if !value.is_finite() {
            return Err(ManifestError::InvalidManifest(format!(
                "{field}[{index}] must be finite"
            )));
        }
    }
    Ok(())
}

fn validate_sign_array(field: &str, values: &[f64; 6]) -> Result<(), ManifestError> {
    for (index, value) in values.iter().copied().enumerate() {
        if value != -1.0 && value != 1.0 {
            return Err(ManifestError::InvalidManifest(format!(
                "{field}[{index}] must be -1 or 1"
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use crate::episode::wire::StepFileSummary;

    use super::*;

    #[test]
    fn episode_id_uses_task_slug_and_random_suffix() {
        let rng = FixedEpisodeRng::from_hex("a1b2c3d4e5f6");
        let id = EpisodeId::generate(
            "Surface Following Demo!",
            UtcTimestamp::parse("20260428T010203Z").unwrap(),
            &rng,
        )
        .unwrap();

        assert_eq!(id.task_slug, "surface-following-demo");
        assert_eq!(
            id.episode_id,
            "20260428T010203Z-surface-following-demo-a1b2c3d4e5f6"
        );
        assert_eq!(
            id.relative_dir,
            PathBuf::from(
                "surface-following-demo/20260428T010203Z-surface-following-demo-a1b2c3d4e5f6"
            )
        );
    }

    #[test]
    fn invalid_task_slug_is_rejected_before_directory_creation() {
        assert!(
            EpisodeId::generate("!!!", UtcTimestamp::for_tests(), &FixedEpisodeRng::zero())
                .is_err()
        );
        assert!(
            EpisodeId::generate(
                &"a".repeat(140),
                UtcTimestamp::for_tests(),
                &FixedEpisodeRng::zero()
            )
            .is_err()
        );
    }

    #[test]
    fn manifest_requires_reproducibility_metadata() {
        let manifest = ManifestV1::for_test_complete()
            .without_mujoco_runtime_identity()
            .without_effective_profile_hash();

        assert!(manifest.validate().is_err());

        let manifest = ManifestV1::for_test_complete();
        assert_eq!(manifest.schema_version, 1);
        assert_eq!(manifest.task_profile.source_kind, ProfileSourceKind::File);
        assert!(manifest.effective_profile.sha256_hex.len() == 64);
        assert!(manifest.mujoco.model.sha256_hex.len() == 64);
        assert!(manifest.calibration.sha256_hex.len() == 64);
        assert!(manifest.mirror_map.source_kind.is_some());
        assert!(manifest.collector.revision.is_some() || manifest.collector.version.is_some());
        assert_eq!(manifest.task.raw_name, "Surface Following Demo!");
        assert_eq!(manifest.task.slug, "surface-following-demo");
        assert_eq!(manifest.gripper.max_feedback_age_ms, 100);
    }

    #[test]
    fn built_in_defaults_source_kind_serializes_and_validates_without_file_hash() {
        assert_eq!(
            serde_json::to_value(ProfileSourceKind::BuiltInDefaults).unwrap(),
            serde_json::json!("built-in-defaults")
        );

        let artifact = SourceArtifactManifest {
            source_kind: ProfileSourceKind::BuiltInDefaults,
            source_path: None,
            hash_algorithm: None,
            sha256_hex: None,
        };
        artifact.validate("task_profile").unwrap();
    }

    #[test]
    fn gripper_manifest_records_and_validates_max_feedback_age() {
        let mut manifest = ManifestV1::for_test_complete();
        assert_eq!(manifest.gripper.max_feedback_age_ms, 100);

        manifest.gripper.max_feedback_age_ms = 0;
        assert!(manifest.validate().is_err());
    }

    #[test]
    fn strict_realtime_test_manifest_has_no_raw_clock_section() {
        let manifest = ManifestV1::for_test_complete();
        assert!(manifest.raw_clock.is_none());

        let text = toml::to_string_pretty(&manifest).expect("manifest should serialize");
        assert!(!text.contains("[raw_clock]"));
        let decoded: ManifestV1 = toml::from_str(&text).expect("manifest should deserialize");
        assert!(decoded.raw_clock.is_none());
    }

    #[test]
    fn report_records_run_and_writer_summary() {
        let report = ReportJson::for_test_faulted();

        assert_eq!(report.schema_version, 1);
        assert!(report.started_unix_ns > 0);
        assert!(report.ended_unix_ns >= report.started_unix_ns);
        assert!(report.dual_arm.iterations >= report.step_count);
        assert!(report.writer.max_queue_depth >= report.writer.final_queue_depth);
        assert_eq!(report.writer.queue_full_stop_events, 2);
        assert_eq!(report.writer.queue_full_stop_duration_ms, 100);
    }

    #[test]
    fn strict_realtime_test_report_has_no_raw_clock_section() {
        let report = ReportJson::for_test_faulted();
        assert!(report.raw_clock.is_none());

        let text = serde_json::to_string_pretty(&report).expect("report should serialize");
        assert!(!text.contains("\"raw_clock\""));
        let decoded: ReportJson = serde_json::from_str(&text).expect("report should deserialize");
        assert!(decoded.raw_clock.is_none());
    }

    #[test]
    fn raw_clock_report_validation_uses_report_errors() {
        let mut report = ReportJson::for_test_faulted();
        let mut raw_clock = raw_clock_report_for_tests();
        raw_clock.timing_source.clear();
        report.raw_clock = Some(raw_clock);

        let err = report.validate().unwrap_err();

        assert!(matches!(err, ManifestError::InvalidReport(_)));
    }

    #[test]
    fn complete_report_rejects_writer_fault_indicators() {
        let mut report = ReportJson::for_test_faulted();
        report.status = EpisodeStatus::Complete;

        assert!(report.validate().is_err());

        let mut report = ReportJson::for_test_faulted();
        report.status = EpisodeStatus::Complete;
        report.final_flush_result.success = true;
        report.final_flush_result.error = None;
        report.writer.flush_failed = false;
        report.writer.queue_full_events = 0;
        report.writer.dropped_step_count = 0;
        report.writer.backpressure_threshold_tripped = false;
        assert!(report.validate().is_ok());

        report.writer.dropped_step_count = 1;
        assert!(report.validate().is_err());
    }

    #[test]
    fn non_faulted_report_status_rejects_writer_fault_indicators() {
        for status in [EpisodeStatus::Cancelled, EpisodeStatus::Running] {
            let mut report = ReportJson::for_test_faulted();
            report.status = status;
            assert!(report.validate().is_err());
        }
    }

    #[test]
    fn step_file_manifest_rejects_absolute_or_traversal_paths() {
        let mut manifest = ManifestV1::for_test_complete();
        manifest.step_file.relative_path = PathBuf::from("/tmp/steps.bin");
        assert!(manifest.validate().is_err());

        let mut manifest = ManifestV1::for_test_complete();
        manifest.step_file.relative_path = PathBuf::from("../steps.bin");
        assert!(manifest.validate().is_err());

        let mut manifest = ManifestV1::for_test_complete();
        manifest.step_file.relative_path = PathBuf::from("nested/steps.bin");
        assert!(manifest.validate().is_err());
    }

    fn raw_clock_report_for_tests() -> RawClockReportJson {
        RawClockReportJson {
            timing_source: "calibrated_hw_raw".to_string(),
            strict_realtime: false,
            experimental: true,
            warmup_secs: 10,
            residual_p95_us: 2_000,
            residual_max_us: 3_000,
            drift_abs_ppm: 500.0,
            sample_gap_max_ms: 50,
            last_sample_age_ms: 20,
            selected_sample_age_ms: 50,
            inter_arm_skew_max_us: 20_000,
            state_skew_max_us: 10_000,
            residual_max_consecutive_failures: 3,
            alignment_buffer_miss_consecutive_failure_threshold: 3,
            master_clock_drift_ppm: 0.0,
            slave_clock_drift_ppm: 0.0,
            master_residual_p95_us: 0,
            slave_residual_p95_us: 0,
            selected_inter_arm_skew_max_us: 0,
            selected_inter_arm_skew_p95_us: 0,
            latest_inter_arm_skew_max_us: 0,
            latest_inter_arm_skew_p95_us: 0,
            alignment_lag_us: 5_000,
            alignment_search_window_us: 25_000,
            alignment_buffer_misses: 0,
            alignment_buffer_miss_consecutive_max: 0,
            alignment_buffer_miss_consecutive_failures: 0,
            master_residual_max_spikes: 0,
            slave_residual_max_spikes: 0,
            master_residual_max_consecutive_failures: 0,
            slave_residual_max_consecutive_failures: 0,
            clock_health_failures: 0,
            read_faults: 0,
            submission_faults: 0,
            runtime_faults: 0,
            compensation_faults: 0,
            controller_faults: 0,
            telemetry_sink_faults: 0,
            final_failure_kind: None,
        }
    }

    #[test]
    fn manifest_step_summary_rejects_mismatched_episode_id() {
        let manifest = ManifestV1::for_test_complete();
        let summary = StepFileSummary {
            episode_id: "20260428T010203Z-other-a1b2c3d4e5f6".to_string(),
            step_count: manifest.step_file.step_count,
            last_step_index: manifest.step_file.last_step_index,
        };

        assert!(manifest.validate_against_step_file_summary(&summary).is_err());
    }

    #[test]
    fn report_step_summary_rejects_mismatched_episode_id() {
        let report = ReportJson::for_test_faulted();
        let summary = StepFileSummary {
            episode_id: "20260428T010203Z-other-a1b2c3d4e5f6".to_string(),
            step_count: report.step_count,
            last_step_index: report.last_step_index,
        };

        assert!(report.validate_against_step_file_summary(&summary).is_err());
    }
}
