#![allow(dead_code)]

use std::{
    fs,
    fs::File,
    io::Read,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};

use crate::gravity::{
    artifact::{read_path, read_quasi_static_samples},
    profile::{
        config::ProfileConfig,
        context::load_profile_context,
        manifest::{ArtifactEntry, Manifest, Split},
        status::derive_readiness_status,
    },
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArtifactKind {
    Path,
    Samples,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactSummary {
    pub kind: ArtifactKind,
    pub sha256: String,
    pub role: String,
    pub arm_id: Option<String>,
    pub target: String,
    pub joint_map: String,
    pub load_profile: String,
    pub torque_convention: String,
    pub source_path_id: Option<String>,
    pub sample_count: Option<u64>,
    pub waypoint_count: u64,
}

pub fn file_sha256(path: &Path) -> Result<String> {
    let mut file =
        File::open(path).with_context(|| format!("failed to open {}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 16 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .with_context(|| format!("failed to read {}", path.display()))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

pub fn analyze_path_artifact(path: &Path) -> Result<ArtifactSummary> {
    let loaded = read_path(path)?;
    Ok(ArtifactSummary {
        kind: ArtifactKind::Path,
        sha256: file_sha256(path)?,
        role: loaded.header.role,
        arm_id: None,
        target: loaded.header.target,
        joint_map: loaded.header.joint_map,
        load_profile: loaded.header.load_profile,
        torque_convention: loaded.header.torque_convention,
        source_path_id: None,
        sample_count: None,
        waypoint_count: loaded.rows.len() as u64,
    })
}

pub fn analyze_samples_artifact(path: &Path) -> Result<ArtifactSummary> {
    let loaded = read_quasi_static_samples(&[path.to_path_buf()])?;
    Ok(ArtifactSummary {
        kind: ArtifactKind::Samples,
        sha256: file_sha256(path)?,
        role: loaded.header.role,
        arm_id: None,
        target: loaded.header.target,
        joint_map: loaded.header.joint_map,
        load_profile: loaded.header.load_profile,
        torque_convention: loaded.header.torque_convention,
        source_path_id: None,
        sample_count: Some(loaded.rows.len() as u64),
        waypoint_count: loaded.header.waypoint_count as u64,
    })
}

pub fn validate_artifact_summary(config: &ProfileConfig, summary: &ArtifactSummary) -> Result<()> {
    validate_field("role", &config.role, &summary.role)?;
    if let Some(arm_id) = &summary.arm_id {
        validate_field("arm_id", &config.arm_id, arm_id)?;
    }
    validate_field("joint_map", &config.joint_map, &summary.joint_map)?;
    validate_field("load_profile", &config.load_profile, &summary.load_profile)?;
    validate_field(
        "torque_convention",
        &config.torque_convention,
        &summary.torque_convention,
    )?;
    Ok(())
}

pub fn verify_registered_artifacts(profile_dir: &Path, artifacts: &[ArtifactEntry]) -> Result<()> {
    for artifact in artifacts {
        let path = profile_dir.join(&artifact.path);
        let actual_sha256 = file_sha256(&path)?;
        if actual_sha256 != artifact.sha256 {
            bail!(
                "{} sha256 mismatch for artifact {}: manifest has {}, file has {}",
                path.display(),
                artifact.id,
                artifact.sha256,
                actual_sha256
            );
        }
    }
    Ok(())
}

pub fn register_imported_samples(
    profile_dir: &Path,
    split: Split,
    samples: &[PathBuf],
) -> Result<()> {
    let mut context = load_profile_context(profile_dir)?;
    let summaries = samples
        .iter()
        .map(|path| {
            let summary = analyze_samples_artifact(path)
                .with_context(|| format!("failed to analyze {}", path.display()))?;
            validate_artifact_summary(&context.config, &summary)?;
            Ok(summary)
        })
        .collect::<Result<Vec<_>>>()?;

    for summary in &summaries {
        reject_active_other_split_duplicate(&context.manifest, split, summary)?;
    }

    let now = current_unix_ms();
    let split_dir = split_dir(split);
    let samples_dir = profile_dir.join("data").join(split_dir).join("samples");
    fs::create_dir_all(&samples_dir)
        .with_context(|| format!("failed to create {}", samples_dir.display()))?;

    for source in samples {
        let artifact_id = context.manifest.next_artifact_id("samples", now);
        let relative_path = format!("data/{split_dir}/samples/{artifact_id}.samples.jsonl");
        let destination = profile_dir.join(&relative_path);
        if destination.exists() {
            bail!("{} already exists", destination.display());
        }

        fs::copy(source, &destination).with_context(|| {
            format!(
                "failed to copy {} to {}",
                source.display(),
                destination.display()
            )
        })?;
        let copied_summary = analyze_samples_artifact(&destination)
            .with_context(|| format!("failed to analyze {}", destination.display()))?;
        validate_artifact_summary(&context.config, &copied_summary)?;
        reject_active_other_split_duplicate(&context.manifest, split, &copied_summary)?;

        context.manifest.artifacts.push(ArtifactEntry {
            id: artifact_id,
            kind: "samples".to_string(),
            split,
            active: true,
            path: relative_path,
            sha256: copied_summary.sha256,
            source_path_id: None,
            role: copied_summary.role,
            arm_id: context.config.arm_id.clone(),
            arm_id_source: Some("legacy_import_profile_asserted".to_string()),
            target: copied_summary.target,
            joint_map: copied_summary.joint_map,
            load_profile: copied_summary.load_profile,
            torque_convention: copied_summary.torque_convention,
            basis: context.config.basis.clone(),
            sample_count: copied_summary.sample_count,
            waypoint_count: Some(copied_summary.waypoint_count),
            created_at_unix_ms: now,
            promoted_from_round_id: None,
            previous_paths: Vec::new(),
        });
    }

    context.manifest.status = derive_readiness_status(&context.manifest);
    context
        .manifest
        .save_atomic(profile_dir.join("manifest.json"))
        .with_context(|| {
            format!(
                "failed to save {}",
                profile_dir.join("manifest.json").display()
            )
        })
}

fn reject_active_other_split_duplicate(
    manifest: &Manifest,
    split: Split,
    summary: &ArtifactSummary,
) -> Result<()> {
    if let Some(existing) = manifest.artifacts.iter().find(|artifact| {
        artifact.active
            && artifact.kind == "samples"
            && artifact.split != split
            && artifact.sha256 == summary.sha256
    }) {
        bail!(
            "samples artifact content sha256 {} is already active in {:?} as {}",
            summary.sha256,
            existing.split,
            existing.id
        );
    }
    Ok(())
}

fn validate_field(name: &str, expected: &str, actual: &str) -> Result<()> {
    if expected != actual {
        bail!("{name} mismatch: profile has {expected:?}, artifact has {actual:?}");
    }
    Ok(())
}

fn split_dir(split: Split) -> &'static str {
    match split {
        Split::Train => "train",
        Split::Validation => "validation",
    }
}

fn current_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis().min(u128::from(u64::MAX)) as u64)
        .unwrap_or(0)
}

#[cfg(test)]
impl ArtifactSummary {
    fn sample_for_tests() -> Self {
        Self {
            kind: ArtifactKind::Samples,
            sha256: "sha256".to_string(),
            role: "slave".to_string(),
            arm_id: None,
            target: "socketcan:can1".to_string(),
            joint_map: "identity".to_string(),
            load_profile: "normal-gripper-d405".to_string(),
            torque_convention: crate::gravity::TORQUE_CONVENTION.to_string(),
            source_path_id: None,
            sample_count: Some(12),
            waypoint_count: 4,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::*;
    use crate::{
        commands::gravity::GravityProfileImportSamplesArgs,
        gravity::{
            artifact::{
                PassDirection, PathHeader, PathSampleRow, QuasiStaticSampleRow, SamplesHeader,
                write_jsonl_row,
            },
            profile::{
                config::ProfileConfig,
                manifest::Manifest,
                workflow::{import_samples, init_profile},
            },
        },
    };

    #[test]
    fn analyze_samples_reads_metadata_counts_hash_and_source_path() {
        let dir = tempfile::tempdir().unwrap();
        let samples = write_samples_artifact_for_tests(dir.path(), "sample.samples.jsonl", 12, 4);

        let summary = analyze_samples_artifact(&samples).unwrap();

        assert_eq!(summary.kind, ArtifactKind::Samples);
        assert_eq!(summary.sample_count, Some(12));
        assert_eq!(summary.waypoint_count, 4);
        assert_eq!(summary.role, "slave");
        assert_eq!(summary.source_path_id, None);
        assert_eq!(summary.sha256.len(), 64);
    }

    #[test]
    fn analyze_path_reads_metadata_count_and_hash() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_path_artifact_for_tests(dir.path(), "path.path.jsonl", 4);

        let summary = analyze_path_artifact(&path).unwrap();

        assert_eq!(summary.kind, ArtifactKind::Path);
        assert_eq!(summary.sample_count, None);
        assert_eq!(summary.waypoint_count, 4);
        assert_eq!(summary.role, "slave");
        assert_eq!(summary.sha256.len(), 64);
    }

    #[test]
    fn register_samples_rejects_identity_mismatch() {
        let config = ProfileConfig::new(
            "profile",
            "slave",
            "piper-left",
            "socketcan:can1",
            "identity",
            "normal-gripper-d405",
        );
        let mut summary = ArtifactSummary::sample_for_tests();
        summary.role = "master".to_string();

        let err = validate_artifact_summary(&config, &summary).unwrap_err();

        assert!(err.to_string().contains("role"));
    }

    #[test]
    fn legacy_import_without_arm_id_records_profile_asserted_arm_identity() {
        let dir = tempfile::tempdir().unwrap();
        let fixture = ProfileFixture::new_at(dir.path());
        let samples = write_samples_artifact_for_tests(dir.path(), "legacy.samples.jsonl", 12, 4);

        import_samples(GravityProfileImportSamplesArgs {
            profile: fixture.profile_dir().to_path_buf(),
            split: "train".to_string(),
            samples: vec![samples],
        })
        .unwrap();

        let manifest = fixture.load_manifest();
        let artifact =
            manifest.artifacts.iter().find(|artifact| artifact.kind == "samples").unwrap();
        assert_eq!(artifact.arm_id, "piper-left");
        assert_eq!(
            artifact.arm_id_source.as_deref(),
            Some("legacy_import_profile_asserted")
        );
    }

    #[test]
    fn register_samples_rejects_same_file_content_in_two_active_splits() {
        let dir = tempfile::tempdir().unwrap();
        let fixture = ProfileFixture::new_at(dir.path());
        let samples = write_samples_artifact_for_tests(dir.path(), "same.samples.jsonl", 12, 4);

        import_samples(GravityProfileImportSamplesArgs {
            profile: fixture.profile_dir().to_path_buf(),
            split: "train".to_string(),
            samples: vec![samples.clone()],
        })
        .unwrap();

        let err = import_samples(GravityProfileImportSamplesArgs {
            profile: fixture.profile_dir().to_path_buf(),
            split: "validation".to_string(),
            samples: vec![samples],
        })
        .unwrap_err();

        assert!(err.to_string().contains("already active"));
    }

    #[test]
    fn verify_registered_artifacts_rejects_hash_mismatch_before_consumption() {
        let fixture = ProfileFixture::new_with_train_samples();
        let artifact = fixture
            .load_manifest()
            .artifacts
            .iter()
            .find(|artifact| artifact.kind == "samples" && artifact.active)
            .unwrap()
            .clone();

        std::fs::write(fixture.profile_dir().join(&artifact.path), "tampered\n").unwrap();

        let err = verify_registered_artifacts(fixture.profile_dir(), &[artifact]).unwrap_err();

        assert!(err.to_string().contains("sha256"));
    }

    struct ProfileFixture {
        _temp_dir: tempfile::TempDir,
        profile_dir: PathBuf,
    }

    impl ProfileFixture {
        fn new_at(parent: &Path) -> Self {
            let temp_dir = tempfile::tempdir_in(parent).unwrap();
            let profile_dir = temp_dir.path().join("profile");
            init_profile(crate::commands::gravity::GravityProfileInitArgs {
                profile: Some(profile_dir.clone()),
                name: Some("slave-piper-left-normal-gripper-d405".to_string()),
                role: "slave".to_string(),
                arm_id: "piper-left".to_string(),
                target: "socketcan:can1".to_string(),
                joint_map: "identity".to_string(),
                load_profile: "normal-gripper-d405".to_string(),
            })
            .unwrap();
            Self {
                _temp_dir: temp_dir,
                profile_dir,
            }
        }

        fn new_with_train_samples() -> Self {
            let temp_dir = tempfile::tempdir().unwrap();
            let profile_dir = temp_dir.path().join("profile");
            init_profile(crate::commands::gravity::GravityProfileInitArgs {
                profile: Some(profile_dir.clone()),
                name: Some("slave-piper-left-normal-gripper-d405".to_string()),
                role: "slave".to_string(),
                arm_id: "piper-left".to_string(),
                target: "socketcan:can1".to_string(),
                joint_map: "identity".to_string(),
                load_profile: "normal-gripper-d405".to_string(),
            })
            .unwrap();
            let samples =
                write_samples_artifact_for_tests(temp_dir.path(), "train.samples.jsonl", 12, 4);
            import_samples(GravityProfileImportSamplesArgs {
                profile: profile_dir.clone(),
                split: "train".to_string(),
                samples: vec![samples],
            })
            .unwrap();
            Self {
                _temp_dir: temp_dir,
                profile_dir,
            }
        }

        fn profile_dir(&self) -> &Path {
            &self.profile_dir
        }

        fn load_manifest(&self) -> Manifest {
            Manifest::load(self.profile_dir.join("manifest.json")).unwrap()
        }
    }

    fn write_samples_artifact_for_tests(
        dir: &Path,
        name: &str,
        sample_count: usize,
        waypoint_count: usize,
    ) -> PathBuf {
        let path = dir.join(name);
        let mut file = std::fs::File::create(&path).unwrap();
        let header = SamplesHeader {
            row_type: "header".to_string(),
            artifact_kind: "quasi-static-samples".to_string(),
            schema_version: 1,
            source_path: "legacy.path.jsonl".to_string(),
            source_sha256: "source-sha256".to_string(),
            role: "slave".to_string(),
            target: "socketcan:can1".to_string(),
            joint_map: "identity".to_string(),
            load_profile: "normal-gripper-d405".to_string(),
            torque_convention: crate::gravity::TORQUE_CONVENTION.to_string(),
            frequency_hz: 100.0,
            max_velocity_rad_s: 0.08,
            max_step_rad: 0.02,
            settle_ms: 500,
            sample_ms: 300,
            stable_velocity_rad_s: 0.01,
            stable_tracking_error_rad: 0.03,
            stable_torque_std_nm: 0.08,
            waypoint_count,
            accepted_waypoint_count: waypoint_count,
            rejected_waypoint_count: 0,
        };
        write_jsonl_row(&mut file, &header).unwrap();
        for index in 0..sample_count {
            write_jsonl_row(&mut file, &sample_row_for_tests(index as u64)).unwrap();
        }
        path
    }

    fn write_path_artifact_for_tests(dir: &Path, name: &str, sample_count: usize) -> PathBuf {
        let path = dir.join(name);
        let mut file = std::fs::File::create(&path).unwrap();
        let header = PathHeader::new(
            "slave",
            "socketcan:can1",
            "identity",
            "normal-gripper-d405",
            None,
        );
        write_jsonl_row(&mut file, &header).unwrap();
        for index in 0..sample_count {
            write_jsonl_row(&mut file, &path_row_for_tests(index as u64)).unwrap();
        }
        path
    }

    fn path_row_for_tests(sample_index: u64) -> PathSampleRow {
        PathSampleRow {
            row_type: "path-sample".to_string(),
            sample_index,
            host_mono_us: sample_index,
            raw_timestamp_us: None,
            q_rad: [0.0; 6],
            dq_rad_s: [0.0; 6],
            tau_nm: [1.0, 2.0, 3.0, 4.0, 5.0, 6.0],
            position_valid_mask: 63,
            dynamic_valid_mask: 63,
            segment_id: Some("seg-a".to_string()),
        }
    }

    fn sample_row_for_tests(waypoint_id: u64) -> QuasiStaticSampleRow {
        QuasiStaticSampleRow {
            row_type: "quasi-static-sample".to_string(),
            waypoint_id,
            segment_id: Some("seg-a".to_string()),
            pass_direction: PassDirection::Forward,
            host_mono_us: waypoint_id,
            raw_timestamp_us: None,
            q_rad: [0.0; 6],
            dq_rad_s: [0.0; 6],
            tau_nm: [1.0, 2.0, 3.0, 4.0, 5.0, 6.0],
            position_valid_mask: 63,
            dynamic_valid_mask: 63,
            stable_velocity_rad_s: 0.0,
            stable_tracking_error_rad: 0.0,
            stable_torque_std_nm: 0.0,
        }
    }
}
