#![allow(dead_code)]

use std::{
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

use crate::gravity::profile::{
    config::ProfileConfig,
    manifest::{EventEntry, Manifest, ProfileStatus},
    status::derive_readiness_status,
};

#[derive(Debug, Clone)]
pub struct ProfileContext {
    pub profile_dir: PathBuf,
    pub config: ProfileConfig,
    pub manifest: Manifest,
}

pub fn load_profile_context(profile_dir: &Path) -> Result<ProfileContext> {
    let config_path = profile_dir.join("profile.toml");
    let manifest_path = profile_dir.join("manifest.json");
    let config = ProfileConfig::load(&config_path)?;
    let mut manifest = Manifest::load(&manifest_path)?;

    let identity_sha256 = config.identity_sha256()?;
    if manifest.profile_identity_sha256 != identity_sha256 {
        bail!("profile directory contains a different profile identity");
    }

    let config_sha256 = config.config_sha256()?;
    let mut mutated = false;
    if manifest.profile_config_sha256 != config_sha256 {
        apply_config_change(&config, &mut manifest, &identity_sha256, &config_sha256)?;
        mutated = true;
    }

    if manifest.profile_name != config.name {
        manifest.profile_name = config.name.clone();
        mutated = true;
    }

    if mutated {
        manifest
            .save_atomic(&manifest_path)
            .with_context(|| format!("failed to save {}", manifest_path.display()))?;
    }

    Ok(ProfileContext {
        profile_dir: profile_dir.to_path_buf(),
        config,
        manifest,
    })
}

pub fn save_profile_context(context: &ProfileContext) -> Result<()> {
    context
        .config
        .save(context.profile_dir.join("profile.toml"))
        .with_context(|| format!("failed to save {}", context.profile_dir.display()))?;
    context
        .manifest
        .save_atomic(context.profile_dir.join("manifest.json"))
        .with_context(|| format!("failed to save {}", context.profile_dir.display()))
}

fn apply_config_change(
    config: &ProfileConfig,
    manifest: &mut Manifest,
    identity_sha256: &str,
    config_sha256: &str,
) -> Result<()> {
    let previous_config_sha256 = manifest.profile_config_sha256.clone();
    let status_before = manifest.status;
    let changed_sections = changed_config_sections(config, manifest)?;

    if changed_sections
        .iter()
        .any(|section| matches!(section.as_str(), "fit" | "gate.strict_v1"))
    {
        manifest.status = derive_readiness_status(manifest);
    }

    let event_id = manifest.next_event_id();
    let status_after = manifest.status;
    manifest.profile_config_sha256 = config_sha256.to_string();
    manifest.events.push(EventEntry {
        id: event_id,
        kind: "profile_config_changed".to_string(),
        created_at_unix_ms: current_unix_ms(),
        profile_identity_sha256: identity_sha256.to_string(),
        profile_config_sha256_before: Some(previous_config_sha256),
        profile_config_sha256_after: Some(config_sha256.to_string()),
        round_id: None,
        artifact_ids: Vec::new(),
        details: json!({
            "changed_sections": changed_sections,
            "status_before": status_json(status_before)?,
            "status_after": status_json(status_after)?,
        }),
    });
    Ok(())
}

fn changed_config_sections(config: &ProfileConfig, manifest: &Manifest) -> Result<Vec<String>> {
    let default_config = ProfileConfig::new(
        &config.name,
        &config.role,
        &config.arm_id,
        &config.target,
        &config.joint_map,
        &config.load_profile,
    );
    let mut changed_sections = Vec::new();

    if config.fit != default_config.fit {
        changed_sections.push("fit".to_string());
    }

    let gate_changed = match manifest.rounds.last() {
        Some(round) => {
            serde_json::to_value(&config.gate).context("failed to serialize gate config")?
                != round.gate_config
        },
        None => config.gate != default_config.gate,
    };
    if gate_changed {
        changed_sections.push("gate.strict_v1".to_string());
    }

    if changed_sections.is_empty() {
        changed_sections.push("target_or_replay".to_string());
    }

    Ok(changed_sections)
}

fn status_json(status: ProfileStatus) -> Result<Value> {
    serde_json::to_value(status).context("failed to serialize profile status")
}

fn current_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis().min(u128::from(u64::MAX)) as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gravity::profile::{
        config::ProfileConfig,
        manifest::{ArtifactEntry, CurrentBestModel, Manifest, ProfileStatus, Split},
    };
    use std::path::Path;
    use tempfile::TempDir;

    struct ProfileFixture {
        _temp_dir: TempDir,
        profile_dir: std::path::PathBuf,
    }

    impl ProfileFixture {
        fn new() -> Self {
            Self::with_manifest_status(ProfileStatus::NeedsTrainData)
        }

        fn new_passed() -> Self {
            Self::with_manifest_status(ProfileStatus::Passed)
        }

        fn with_manifest_status(status: ProfileStatus) -> Self {
            let temp_dir = tempfile::tempdir().unwrap();
            let profile_dir = temp_dir.path().join("profile");
            std::fs::create_dir_all(&profile_dir).unwrap();

            let config = config_for_tests();
            config.save(profile_dir.join("profile.toml")).unwrap();

            let mut manifest = Manifest::new(
                &config.name,
                config.identity_sha256().unwrap(),
                config.config_sha256().unwrap(),
            );
            manifest.status = status;
            if status == ProfileStatus::Passed {
                manifest.artifacts.push(ArtifactEntry::sample_for_tests(
                    "train-1",
                    Split::Train,
                    true,
                    300,
                    150,
                ));
                manifest.artifacts.push(ArtifactEntry::sample_for_tests(
                    "validation-1",
                    Split::Validation,
                    true,
                    80,
                    40,
                ));
                manifest.current_best_model = Some(CurrentBestModel {
                    round_id: "round-0001".to_string(),
                    path: "models/current.json".to_string(),
                    sha256: "current-sha256".to_string(),
                    source_model_path: "rounds/round-0001/model.json".to_string(),
                    source_model_sha256: "source-sha256".to_string(),
                    promoted_at_unix_ms: 1_777_680_930_000,
                });
            }
            manifest.save_atomic(profile_dir.join("manifest.json")).unwrap();

            Self {
                _temp_dir: temp_dir,
                profile_dir,
            }
        }

        fn profile_dir(&self) -> &Path {
            &self.profile_dir
        }

        fn write_config_with_arm_id(&self, arm_id: &str) {
            let mut config = config_for_tests();
            config.arm_id = arm_id.to_string();
            config.save(self.profile_dir.join("profile.toml")).unwrap();
        }

        fn write_config_with_target(&self, target: &str) {
            let mut config = config_for_tests();
            config.target = target.to_string();
            config.save(self.profile_dir.join("profile.toml")).unwrap();
        }

        fn write_config_with_min_validation_samples(&self, min_validation_samples: usize) {
            let mut config = config_for_tests();
            config.gate.strict_v1.min_validation_samples = min_validation_samples;
            config.save(self.profile_dir.join("profile.toml")).unwrap();
        }
    }

    fn config_for_tests() -> ProfileConfig {
        ProfileConfig::new(
            "slave-piper-left-normal-gripper-d405",
            "slave",
            "piper-left",
            "socketcan:can1",
            "identity",
            "normal-gripper-d405",
        )
    }

    #[test]
    fn identity_hash_mismatch_rejects_profile_directory() {
        let fixture = ProfileFixture::new();
        fixture.write_config_with_arm_id("different-arm");

        let err = load_profile_context(fixture.profile_dir()).unwrap_err();

        assert!(err.to_string().contains("different profile"));
    }

    #[test]
    fn target_change_preserves_status_and_best_model() {
        let fixture = ProfileFixture::new_passed();
        fixture.write_config_with_target("socketcan:can0");

        let context = load_profile_context(fixture.profile_dir()).unwrap();

        assert_eq!(context.manifest.status, ProfileStatus::Passed);
        assert!(context.manifest.current_best_model.is_some());
        assert!(
            context
                .manifest
                .events
                .iter()
                .any(|event| event.kind == "profile_config_changed")
        );
    }

    #[test]
    fn gate_change_invalidates_round_result_status() {
        let fixture = ProfileFixture::new_passed();
        fixture.write_config_with_min_validation_samples(9999);

        let context = load_profile_context(fixture.profile_dir()).unwrap();

        assert_eq!(context.manifest.status, ProfileStatus::ReadyToFit);
        assert!(context.manifest.current_best_model.is_some());
        assert!(
            context
                .manifest
                .events
                .iter()
                .any(|event| event.kind == "profile_config_changed")
        );
    }
}
