#![allow(dead_code)]

use std::{
    fs::{self, OpenOptions},
    io::{BufWriter, Write},
    path::{Path, PathBuf},
};

#[cfg(unix)]
use std::fs::File;

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProfileStatus {
    NeedsTrainData,
    NeedsValidationData,
    ReadyToFit,
    InsufficientData,
    FitFailed,
    ValidationFailed,
    Passed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Split {
    Train,
    Validation,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub schema_version: u32,
    pub profile_name: String,
    pub profile_identity_sha256: String,
    pub profile_config_sha256: String,
    pub status: ProfileStatus,
    pub next_artifact_seq: u64,
    pub next_event_seq: u64,
    pub next_round_seq: u64,
    pub current_best_model: Option<CurrentBestModel>,
    pub artifacts: Vec<ArtifactEntry>,
    pub rounds: Vec<RoundEntry>,
    pub events: Vec<EventEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ArtifactEntry {
    pub id: String,
    pub kind: String,
    pub split: Split,
    pub active: bool,
    pub path: String,
    pub sha256: String,
    pub source_path_id: Option<String>,
    pub role: String,
    pub arm_id: String,
    pub arm_id_source: Option<String>,
    pub target: String,
    pub joint_map: String,
    pub load_profile: String,
    pub torque_convention: String,
    pub basis: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sample_count: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub waypoint_count: Option<u64>,
    pub created_at_unix_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub promoted_from_round_id: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub previous_paths: Vec<PreviousPathEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PreviousPathEntry {
    pub split: Split,
    pub path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct RoundEntry {
    pub id: String,
    pub status: ProfileStatus,
    pub model_path: Option<String>,
    pub model_sha256: Option<String>,
    pub report_path: Option<String>,
    pub report_sha256: Option<String>,
    pub round_path: Option<String>,
    pub round_sha256: Option<String>,
    pub train_sample_artifact_ids: Vec<String>,
    pub validation_sample_artifact_ids: Vec<String>,
    pub validation_path_artifact_ids: Vec<String>,
    pub profile_identity_sha256: String,
    pub profile_config_sha256: String,
    pub gate_config: Value,
    pub created_at_unix_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure: Option<RoundFailure>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RoundFailure {
    pub kind: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CurrentBestModel {
    pub round_id: String,
    pub path: String,
    pub sha256: String,
    pub source_model_path: String,
    pub source_model_sha256: String,
    pub promoted_at_unix_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct EventEntry {
    pub id: String,
    pub kind: String,
    pub created_at_unix_ms: u64,
    pub profile_identity_sha256: String,
    pub profile_config_sha256_before: Option<String>,
    pub profile_config_sha256_after: Option<String>,
    pub round_id: Option<String>,
    pub artifact_ids: Vec<String>,
    #[serde(default)]
    pub details: Value,
}

impl Manifest {
    pub fn new(
        profile_name: impl Into<String>,
        profile_identity_sha256: impl Into<String>,
        profile_config_sha256: impl Into<String>,
    ) -> Self {
        Self {
            schema_version: 1,
            profile_name: profile_name.into(),
            profile_identity_sha256: profile_identity_sha256.into(),
            profile_config_sha256: profile_config_sha256.into(),
            status: ProfileStatus::NeedsTrainData,
            next_artifact_seq: 1,
            next_event_seq: 1,
            next_round_seq: 1,
            current_best_model: None,
            artifacts: Vec::new(),
            rounds: Vec::new(),
            events: Vec::new(),
        }
    }

    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let input = fs::read_to_string(path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        let manifest: Self = serde_json::from_str(&input)
            .with_context(|| format!("failed to parse {}", path.display()))?;
        ensure!(
            manifest.schema_version == 1,
            "unsupported manifest schema_version {}",
            manifest.schema_version
        );
        Ok(manifest)
    }

    pub fn save_atomic(&self, path: impl AsRef<Path>) -> Result<()> {
        let path = path.as_ref();
        let parent = manifest_parent_dir(path);
        if parent != Path::new(".") {
            fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }

        let temp_path = temp_manifest_path(path);
        let temp_file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp_path)
            .with_context(|| format!("failed to create temporary {}", temp_path.display()))?;

        let mut writer = BufWriter::new(temp_file);
        serde_json::to_writer_pretty(&mut writer, self).context("failed to serialize manifest")?;
        writer.write_all(b"\n").context("failed to write manifest")?;
        writer.flush().context("failed to flush manifest")?;
        let temp_file = writer.into_inner().context("failed to finish manifest write buffer")?;
        temp_file
            .sync_all()
            .with_context(|| format!("failed to sync {}", temp_path.display()))?;

        fs::rename(&temp_path, path).with_context(|| {
            format!(
                "failed to rename {} to {}",
                temp_path.display(),
                path.display()
            )
        })?;
        fsync_dir(parent)?;
        Ok(())
    }

    pub fn next_artifact_id(&mut self, kind: &str, unix_ms: u64) -> String {
        let seq = self.next_artifact_seq;
        self.next_artifact_seq += 1;
        let timestamp = format_utc_timestamp(unix_ms);
        format!("{kind}-{timestamp}-{seq:04}")
    }

    pub fn next_round_id(&mut self) -> String {
        let seq = self.next_round_seq;
        self.next_round_seq += 1;
        format!("round-{seq:04}")
    }

    pub fn next_event_id(&mut self) -> String {
        let seq = self.next_event_seq;
        self.next_event_seq += 1;
        format!("event-{seq:04}")
    }
}

#[cfg(test)]
impl ArtifactEntry {
    pub fn sample_for_tests(
        id: impl Into<String>,
        split: Split,
        active: bool,
        sample_count: u64,
        waypoint_count: u64,
    ) -> Self {
        let id = id.into();
        Self {
            path: format!("data/{split:?}/samples/{id}.samples.jsonl"),
            id,
            kind: "samples".to_string(),
            split,
            active,
            sha256: "sha256".to_string(),
            source_path_id: None,
            role: "role".to_string(),
            arm_id: "arm".to_string(),
            arm_id_source: None,
            target: "target".to_string(),
            joint_map: "identity".to_string(),
            load_profile: "load".to_string(),
            torque_convention: "piper-sdk-normalized-nm-v1".to_string(),
            basis: "trig-v1".to_string(),
            sample_count: Some(sample_count),
            waypoint_count: Some(waypoint_count),
            created_at_unix_ms: 0,
            promoted_from_round_id: None,
            previous_paths: Vec::new(),
        }
    }
}

#[cfg(test)]
impl RoundEntry {
    pub fn fit_failed_for_tests(id: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            status: ProfileStatus::FitFailed,
            model_path: None,
            model_sha256: None,
            report_path: None,
            report_sha256: None,
            round_path: None,
            round_sha256: None,
            train_sample_artifact_ids: Vec::new(),
            validation_sample_artifact_ids: Vec::new(),
            validation_path_artifact_ids: Vec::new(),
            profile_identity_sha256: "identity".to_string(),
            profile_config_sha256: "config".to_string(),
            gate_config: Value::Object(Default::default()),
            created_at_unix_ms: 0,
            failure: Some(RoundFailure {
                kind: "fit".to_string(),
                message: message.into(),
            }),
        }
    }
}

#[cfg(test)]
impl EventEntry {
    pub fn profile_config_changed_for_tests(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            kind: "profile_config_changed".to_string(),
            created_at_unix_ms: 0,
            profile_identity_sha256: "identity-hash".to_string(),
            profile_config_sha256_before: Some("old-config-hash".to_string()),
            profile_config_sha256_after: Some("config-hash".to_string()),
            round_id: None,
            artifact_ids: Vec::new(),
            details: serde_json::json!({
                "changed_sections": ["gate.strict_v1"],
                "status_before": "passed",
                "status_after": "ready_to_fit"
            }),
        }
    }
}

fn temp_manifest_path(path: &Path) -> PathBuf {
    let file_name = path.file_name().and_then(|name| name.to_str()).unwrap_or("manifest.json");
    let temp_name = format!(
        ".{file_name}.tmp-{}-{}",
        std::process::id(),
        unique_temp_suffix()
    );
    path.with_file_name(temp_name)
}

fn unique_temp_suffix() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0)
}

#[cfg(unix)]
fn fsync_dir(path: &Path) -> Result<()> {
    File::open(path)
        .and_then(|dir| dir.sync_all())
        .with_context(|| format!("failed to sync directory {}", path.display()))
}

#[cfg(not(unix))]
fn fsync_dir(_path: &Path) -> Result<()> {
    Ok(())
}

fn manifest_parent_dir(path: &Path) -> &Path {
    path.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
}

fn format_utc_timestamp(unix_ms: u64) -> String {
    let seconds = (unix_ms / 1_000) as i64;
    let days = seconds.div_euclid(86_400);
    let seconds_of_day = seconds.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    let hour = seconds_of_day / 3_600;
    let minute = (seconds_of_day % 3_600) / 60;
    let second = seconds_of_day % 60;
    format!("{year:04}{month:02}{day:02}-{hour:02}{minute:02}{second:02}")
}

fn civil_from_days(days_since_unix_epoch: i64) -> (i32, u32, u32) {
    let z = days_since_unix_epoch + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = mp + if mp < 10 { 3 } else { -9 };
    let year = y + if month <= 2 { 1 } else { 0 };
    (year as i32, month as u32, day as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unix_ms_for_tests() -> u64 {
        1_777_680_930_000
    }

    #[test]
    fn new_manifest_allocates_monotonic_ids() {
        let mut manifest = Manifest::new("profile", "identity-hash", "config-hash");

        assert_eq!(
            manifest.next_artifact_id("samples", unix_ms_for_tests()),
            "samples-20260502-001530-0001"
        );
        assert_eq!(
            manifest.next_artifact_id("path", unix_ms_for_tests()),
            "path-20260502-001530-0002"
        );
        assert_eq!(manifest.next_round_id(), "round-0001");
        assert_eq!(manifest.next_event_id(), "event-0001");
    }

    #[test]
    fn manifest_atomic_round_trip_preserves_entries() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("manifest.json");
        let mut manifest = Manifest::new("profile", "identity-hash", "config-hash");
        manifest.events.push(EventEntry::profile_config_changed_for_tests("event-0001"));

        manifest.save_atomic(&path).unwrap();
        let loaded = Manifest::load(&path).unwrap();

        assert_eq!(loaded.events.len(), 1);
        assert_eq!(loaded.schema_version, 1);
        let event = &loaded.events[0];
        assert_eq!(event.profile_identity_sha256, "identity-hash");
        assert_eq!(
            event.profile_config_sha256_before.as_deref(),
            Some("old-config-hash")
        );
        assert_eq!(
            event.profile_config_sha256_after.as_deref(),
            Some("config-hash")
        );
        assert_eq!(event.round_id, None);
        assert!(event.artifact_ids.is_empty());
    }

    #[test]
    fn manifest_atomic_save_accepts_bare_relative_path() {
        let dir = tempfile::tempdir().unwrap();
        let _guard = CurrentDirGuard::enter(dir.path());
        let manifest = Manifest::new("profile", "identity-hash", "config-hash");

        manifest.save_atomic(Path::new("manifest.json")).unwrap();
        let loaded = Manifest::load(Path::new("manifest.json")).unwrap();

        assert_eq!(loaded.profile_name, "profile");
    }

    #[test]
    fn event_details_null_serializes_as_present_null_field() {
        let event = EventEntry {
            id: "event-0001".to_string(),
            kind: "validation_promoted".to_string(),
            created_at_unix_ms: 0,
            profile_identity_sha256: "identity-hash".to_string(),
            profile_config_sha256_before: None,
            profile_config_sha256_after: None,
            round_id: Some("round-0001".to_string()),
            artifact_ids: vec!["samples-1".to_string()],
            details: Value::Null,
        };

        let event_json = serde_json::to_value(&event).unwrap();
        let event_object = event_json.as_object().unwrap();

        assert!(event_object.contains_key("details"));
        assert!(event_json["details"].is_null());
    }

    #[test]
    fn manifest_load_rejects_unknown_top_level_fields() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("manifest.json");
        let mut json =
            serde_json::to_value(Manifest::new("profile", "identity", "config")).unwrap();
        json.as_object_mut()
            .unwrap()
            .insert("unexpected".to_string(), serde_json::json!(true));
        std::fs::write(&path, serde_json::to_string_pretty(&json).unwrap()).unwrap();

        let error = Manifest::load(&path).unwrap_err();

        assert!(
            format!("{error:#}").contains("unknown field"),
            "unexpected error: {error:#}"
        );
    }

    static CURRENT_DIR_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    struct CurrentDirGuard {
        original: std::path::PathBuf,
        _lock: std::sync::MutexGuard<'static, ()>,
    }

    impl CurrentDirGuard {
        fn enter(path: &Path) -> Self {
            let lock = CURRENT_DIR_LOCK.lock().unwrap();
            let original = std::env::current_dir().unwrap();
            std::env::set_current_dir(path).unwrap();
            Self {
                original,
                _lock: lock,
            }
        }
    }

    impl Drop for CurrentDirGuard {
        fn drop(&mut self) {
            std::env::set_current_dir(&self.original).unwrap();
        }
    }
}
