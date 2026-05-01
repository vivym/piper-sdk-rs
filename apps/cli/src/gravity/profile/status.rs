#![allow(dead_code)]

pub use crate::gravity::profile::manifest::ProfileStatus;

use crate::gravity::profile::manifest::{Manifest, Split};

pub fn derive_readiness_status(manifest: &Manifest) -> ProfileStatus {
    let has_train = manifest.artifacts.iter().any(|artifact| {
        artifact.active && artifact.kind == "samples" && artifact.split == Split::Train
    });
    if !has_train {
        return ProfileStatus::NeedsTrainData;
    }

    let has_validation = manifest.artifacts.iter().any(|artifact| {
        artifact.active && artifact.kind == "samples" && artifact.split == Split::Validation
    });
    if !has_validation {
        return ProfileStatus::NeedsValidationData;
    }

    ProfileStatus::ReadyToFit
}

pub fn next_action(status: ProfileStatus) -> &'static str {
    match status {
        ProfileStatus::NeedsTrainData => "collect train samples",
        ProfileStatus::NeedsValidationData => "collect validation samples",
        ProfileStatus::ReadyToFit => "run fit-assess",
        ProfileStatus::InsufficientData => "collect more train or validation samples",
        ProfileStatus::FitFailed => "inspect fit error, fix data/config, rerun fit-assess",
        ProfileStatus::ValidationFailed => {
            "run promote-validation, then collect new validation samples"
        },
        ProfileStatus::Passed => "model is usable; optionally collect more validation",
    }
}

pub fn invalidate_round_status_after_sample_pool_change(manifest: &mut Manifest) {
    manifest.status = derive_readiness_status(manifest);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gravity::profile::manifest::{ArtifactEntry, Manifest, RoundEntry, Split};

    #[test]
    fn readiness_status_uses_active_sample_splits() {
        let mut manifest = Manifest::new("profile", "identity", "config");
        assert_eq!(
            derive_readiness_status(&manifest),
            ProfileStatus::NeedsTrainData
        );

        manifest.artifacts.push(ArtifactEntry::sample_for_tests(
            "train-1",
            Split::Train,
            true,
            10,
            4,
        ));
        assert_eq!(
            derive_readiness_status(&manifest),
            ProfileStatus::NeedsValidationData
        );

        manifest.artifacts.push(ArtifactEntry::sample_for_tests(
            "validation-1",
            Split::Validation,
            true,
            8,
            3,
        ));
        assert_eq!(
            derive_readiness_status(&manifest),
            ProfileStatus::ReadyToFit
        );
    }

    #[test]
    fn fit_failed_round_entry_allows_missing_model_and_report_outputs() {
        let round = RoundEntry::fit_failed_for_tests("round-0001", "solver singular matrix");
        let json = serde_json::to_value(&round).unwrap();

        assert_eq!(json["status"], "fit_failed");
        assert!(json["model_path"].is_null());
        assert!(json["model_sha256"].is_null());
        assert!(json["report_path"].is_null());
        assert!(json["report_sha256"].is_null());
        assert_eq!(json["failure"]["kind"], "fit");
        assert!(json["failure"]["message"].as_str().unwrap().contains("solver"));
    }
}
