#![allow(dead_code)]

use std::{
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

use crate::gravity::profile::{
    config::ProfileConfig,
    manifest::{EventEntry, Manifest, ManifestLock, ProfileConfigSectionHashes, ProfileStatus},
    status::derive_readiness_status,
};

#[derive(Debug, Clone)]
pub struct ProfileContext {
    pub profile_dir: PathBuf,
    pub config: ProfileConfig,
    pub manifest: Manifest,
}

pub fn load_profile_context(profile_dir: &Path) -> Result<ProfileContext> {
    let _lock = ManifestLock::acquire(profile_dir)?;
    load_profile_context_unlocked(profile_dir)
}

pub(crate) fn load_profile_context_unlocked(profile_dir: &Path) -> Result<ProfileContext> {
    let config_path = profile_dir.join("profile.toml");
    let manifest_path = profile_dir.join("manifest.json");
    let config = ProfileConfig::load(&config_path)?;
    let mut manifest = Manifest::load(&manifest_path)?;

    let identity_sha256 = config.identity_sha256()?;
    if manifest.profile_identity_sha256 != identity_sha256 {
        bail!("profile directory contains a different profile identity");
    }

    let config_sha256 = config.config_sha256()?;
    let section_hashes = config.section_sha256()?;
    let mut mutated = false;
    if manifest.profile_config_sha256 != config_sha256 {
        apply_config_change(
            &mut manifest,
            &identity_sha256,
            &config_sha256,
            section_hashes,
        )?;
        mutated = true;
    } else if manifest.profile_config_sections_sha256.is_none() {
        manifest.profile_config_sections_sha256 = Some(section_hashes);
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
    let _lock = ManifestLock::acquire(&context.profile_dir)?;
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
    manifest: &mut Manifest,
    identity_sha256: &str,
    config_sha256: &str,
    section_hashes: ProfileConfigSectionHashes,
) -> Result<()> {
    let previous_config_sha256 = manifest.profile_config_sha256.clone();
    let status_before = manifest.status;
    let changed_sections = changed_config_sections(
        manifest.profile_config_sections_sha256.as_ref(),
        &section_hashes,
    );

    if config_change_invalidates_round_status(&changed_sections) {
        manifest.status = derive_readiness_status(manifest);
    }

    let event_id = manifest.next_event_id();
    let status_after = manifest.status;
    manifest.profile_config_sha256 = config_sha256.to_string();
    manifest.profile_config_sections_sha256 = Some(section_hashes);
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

fn changed_config_sections(
    previous: Option<&ProfileConfigSectionHashes>,
    current: &ProfileConfigSectionHashes,
) -> Vec<String> {
    let Some(previous) = previous else {
        return vec!["legacy_unknown".to_string()];
    };

    let mut changed_sections = Vec::new();

    if previous.name != current.name {
        changed_sections.push("name".to_string());
    }
    if previous.target != current.target {
        changed_sections.push("target".to_string());
    }
    if previous.replay != current.replay {
        changed_sections.push("replay".to_string());
    }
    if previous.fit != current.fit {
        changed_sections.push("fit".to_string());
    }
    if previous.gate_strict_v1 != current.gate_strict_v1 {
        changed_sections.push("gate.strict_v1".to_string());
    }

    if changed_sections.is_empty() {
        changed_sections.push("unknown".to_string());
    }

    changed_sections
}

fn config_change_invalidates_round_status(changed_sections: &[String]) -> bool {
    changed_sections
        .iter()
        .any(|section| !matches!(section.as_str(), "name" | "target" | "replay"))
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
            manifest.profile_config_sections_sha256 = Some(config.section_sha256().unwrap());
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

        fn write_custom_fit_config_with_section_hashes(&self) {
            let mut config = config_for_tests();
            config.fit.ridge_lambda = 0.25;
            config.save(self.profile_dir.join("profile.toml")).unwrap();

            let mut manifest = Manifest::load(self.profile_dir.join("manifest.json")).unwrap();
            manifest.profile_config_sha256 = config.config_sha256().unwrap();
            manifest.profile_config_sections_sha256 = Some(config.section_sha256().unwrap());
            manifest.save_atomic(self.profile_dir.join("manifest.json")).unwrap();
        }

        fn remove_section_hashes(&self) {
            let mut manifest = Manifest::load(self.profile_dir.join("manifest.json")).unwrap();
            manifest.profile_config_sections_sha256 = None;
            manifest.save_atomic(self.profile_dir.join("manifest.json")).unwrap();
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

    #[test]
    fn target_change_with_preexisting_custom_fit_preserves_round_status() {
        let fixture = ProfileFixture::new_passed();
        fixture.write_custom_fit_config_with_section_hashes();

        let mut config = ProfileConfig::load(fixture.profile_dir().join("profile.toml")).unwrap();
        config.target = "socketcan:can0".to_string();
        config.save(fixture.profile_dir().join("profile.toml")).unwrap();

        let context = load_profile_context(fixture.profile_dir()).unwrap();

        assert_eq!(context.manifest.status, ProfileStatus::Passed);
        assert!(context.manifest.current_best_model.is_some());
        let event = context.manifest.events.last().unwrap();
        assert_eq!(
            event.details["changed_sections"],
            serde_json::json!(["target"])
        );
    }

    #[test]
    fn missing_section_hashes_with_config_mismatch_conservatively_recomputes_status() {
        let fixture = ProfileFixture::new_passed();
        fixture.remove_section_hashes();
        fixture.write_config_with_target("socketcan:can0");

        let context = load_profile_context(fixture.profile_dir()).unwrap();

        assert_eq!(context.manifest.status, ProfileStatus::ReadyToFit);
        assert!(context.manifest.current_best_model.is_some());
        let event = context.manifest.events.last().unwrap();
        assert_eq!(
            event.details["changed_sections"],
            serde_json::json!(["legacy_unknown"])
        );
        assert!(context.manifest.profile_config_sections_sha256.is_some());
    }

    #[test]
    fn unchanged_config_backfills_missing_section_hashes_without_event() {
        let fixture = ProfileFixture::new_passed();
        fixture.remove_section_hashes();

        let context = load_profile_context(fixture.profile_dir()).unwrap();

        assert_eq!(context.manifest.status, ProfileStatus::Passed);
        assert!(context.manifest.profile_config_sections_sha256.is_some());
        assert!(context.manifest.events.is_empty());
    }

    #[test]
    fn load_profile_context_rejects_existing_manifest_lock() {
        let fixture = ProfileFixture::new();
        std::fs::write(fixture.profile_dir().join(".manifest.lock"), "locked").unwrap();

        let err = load_profile_context(fixture.profile_dir()).unwrap_err();

        assert!(err.to_string().contains("manifest lock"));
    }

    #[test]
    fn name_only_change_preserves_status_and_updates_profile_name() {
        let fixture = ProfileFixture::new_passed();
        let mut config = config_for_tests();
        config.name = "renamed-profile".to_string();
        config.save(fixture.profile_dir().join("profile.toml")).unwrap();

        let context = load_profile_context(fixture.profile_dir()).unwrap();

        assert_eq!(context.manifest.status, ProfileStatus::Passed);
        assert_eq!(context.manifest.profile_name, "renamed-profile");
        let event = context.manifest.events.last().unwrap();
        assert_eq!(
            event.details["changed_sections"],
            serde_json::json!(["name"])
        );
    }
}
