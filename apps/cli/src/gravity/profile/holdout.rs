#![allow(dead_code)]

use std::collections::{BTreeMap, HashSet};

use anyhow::ensure;
use sha2::{Digest, Sha256};

use crate::gravity::profile::manifest::{ArtifactEntry, Split};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiagnosticHoldoutSplit {
    pub available: bool,
    pub train_group_keys: Vec<String>,
    pub holdout_group_keys: Vec<String>,
    pub train_sample_artifact_ids: Vec<String>,
    pub holdout_sample_artifact_ids: Vec<String>,
}

pub fn select_diagnostic_holdout_groups(
    profile_identity_sha256: &str,
    round_id: &str,
    holdout_group_key: &str,
    holdout_ratio: f64,
    train_sample_artifacts: &[ArtifactEntry],
) -> anyhow::Result<DiagnosticHoldoutSplit> {
    ensure!(
        !profile_identity_sha256.trim().is_empty(),
        "profile_identity_sha256 must not be empty"
    );
    ensure!(!round_id.trim().is_empty(), "round_id must not be empty");
    ensure!(
        !holdout_group_key.trim().is_empty(),
        "holdout_group_key must not be empty"
    );
    ensure!(
        holdout_ratio.is_finite() && (0.0..1.0).contains(&holdout_ratio),
        "holdout_ratio must be finite and in [0.0, 1.0)"
    );
    ensure!(
        holdout_group_key == "source_path_id",
        "unsupported holdout_group_key {holdout_group_key:?}"
    );

    let groups = sample_groups_by_source_path(train_sample_artifacts);
    if groups.is_empty() {
        return Ok(DiagnosticHoldoutSplit {
            available: false,
            train_group_keys: Vec::new(),
            holdout_group_keys: Vec::new(),
            train_sample_artifact_ids: Vec::new(),
            holdout_sample_artifact_ids: Vec::new(),
        });
    }

    if groups.len() == 1 {
        let (group_key, group) = groups.iter().next().expect("group exists");
        return Ok(DiagnosticHoldoutSplit {
            available: false,
            train_group_keys: vec![group_key.clone()],
            holdout_group_keys: Vec::new(),
            train_sample_artifact_ids: group.artifact_ids.clone(),
            holdout_sample_artifact_ids: Vec::new(),
        });
    }

    let total_samples = groups.values().map(|group| group.sample_count).sum::<u64>();
    let target_holdout_samples = (total_samples as f64) * holdout_ratio;
    let mut candidates = groups
        .iter()
        .map(|(group_key, group)| {
            (
                holdout_sort_hash(profile_identity_sha256, round_id, group_key),
                group_key.clone(),
                group.sample_count,
            )
        })
        .collect::<Vec<_>>();
    candidates.sort_by(|left, right| left.0.cmp(&right.0).then_with(|| left.1.cmp(&right.1)));

    let mut selected_sample_count = 0_u64;
    let mut holdout_group_keys = Vec::new();
    if target_holdout_samples > 0.0 {
        let max_holdout_groups = candidates.len().saturating_sub(1);
        for (_, group_key, sample_count) in candidates {
            if holdout_group_keys.len() >= max_holdout_groups {
                break;
            }
            holdout_group_keys.push(group_key);
            selected_sample_count += sample_count;
            if (selected_sample_count as f64) >= target_holdout_samples {
                break;
            }
        }
    }

    let holdout_group_set = holdout_group_keys.iter().collect::<HashSet<_>>();
    let mut train_group_keys = Vec::new();
    let mut train_sample_artifact_ids = Vec::new();
    let mut train_sample_count = 0_u64;
    let mut holdout_sample_artifact_ids = Vec::new();
    let mut holdout_sample_count = 0_u64;

    for (group_key, group) in &groups {
        if holdout_group_set.contains(group_key) {
            holdout_sample_artifact_ids.extend(group.artifact_ids.clone());
            holdout_sample_count += group.sample_count;
        } else {
            train_group_keys.push(group_key.clone());
            train_sample_artifact_ids.extend(group.artifact_ids.clone());
            train_sample_count += group.sample_count;
        }
    }

    let available = !train_group_keys.is_empty()
        && !holdout_group_keys.is_empty()
        && !train_sample_artifact_ids.is_empty()
        && !holdout_sample_artifact_ids.is_empty()
        && train_sample_count > 0
        && holdout_sample_count > 0
        && (selected_sample_count as f64) >= target_holdout_samples;
    if !available {
        let mut all_group_keys = Vec::new();
        let mut all_artifact_ids = Vec::new();
        for (group_key, group) in &groups {
            all_group_keys.push(group_key.clone());
            all_artifact_ids.extend(group.artifact_ids.clone());
        }
        return Ok(DiagnosticHoldoutSplit {
            available: false,
            train_group_keys: all_group_keys,
            holdout_group_keys: Vec::new(),
            train_sample_artifact_ids: all_artifact_ids,
            holdout_sample_artifact_ids: Vec::new(),
        });
    }

    Ok(DiagnosticHoldoutSplit {
        available,
        train_group_keys,
        holdout_group_keys,
        train_sample_artifact_ids,
        holdout_sample_artifact_ids,
    })
}

#[derive(Debug, Default)]
struct SampleGroup {
    sample_count: u64,
    artifact_ids: Vec<String>,
}

fn sample_groups_by_source_path(artifacts: &[ArtifactEntry]) -> BTreeMap<String, SampleGroup> {
    let mut groups = BTreeMap::new();
    for artifact in artifacts {
        if !(artifact.active && artifact.kind == "samples" && artifact.split == Split::Train) {
            continue;
        }

        let group_key = artifact.source_path_id.clone().unwrap_or_else(|| artifact.id.clone());
        let group = groups.entry(group_key).or_insert_with(SampleGroup::default);
        group.sample_count += artifact.sample_count.unwrap_or(0);
        group.artifact_ids.push(artifact.id.clone());
    }
    groups
}

fn holdout_sort_hash(profile_identity_sha256: &str, round_id: &str, group_key: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(format!("{profile_identity_sha256}:{round_id}:{group_key}"));
    format!("{:x}", hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gravity::profile::manifest::ArtifactEntry;

    #[test]
    fn source_path_holdout_is_deterministic_and_records_groups() {
        let artifacts = vec![
            sample_artifact_for_tests("samples-a", Some("path-a"), 50),
            sample_artifact_for_tests("samples-b", Some("path-b"), 50),
            sample_artifact_for_tests("samples-c", Some("path-c"), 50),
        ];

        let split = select_diagnostic_holdout_groups(
            "identity-hash",
            "round-0001",
            "source_path_id",
            0.34,
            &artifacts,
        )
        .unwrap();

        assert_eq!(
            split,
            select_diagnostic_holdout_groups(
                "identity-hash",
                "round-0001",
                "source_path_id",
                0.34,
                &artifacts
            )
            .unwrap()
        );
        assert!(!split.train_group_keys.is_empty());
        assert!(!split.holdout_group_keys.is_empty());
    }

    #[test]
    fn single_group_holdout_is_unavailable_not_error() {
        let artifacts = vec![sample_artifact_for_tests("samples-a", Some("path-a"), 100)];

        let split = select_diagnostic_holdout_groups(
            "identity-hash",
            "round-0001",
            "source_path_id",
            0.2,
            &artifacts,
        )
        .unwrap();

        assert!(!split.available);
        assert!(split.holdout_group_keys.is_empty());
        assert_eq!(split.train_group_keys, ["path-a"]);
    }

    #[test]
    fn holdout_unavailable_when_ratio_requires_all_groups() {
        let artifacts = vec![
            sample_artifact_for_tests("samples-a", Some("path-a"), 50),
            sample_artifact_for_tests("samples-b", Some("path-b"), 50),
        ];

        let split = select_diagnostic_holdout_groups(
            "identity-hash",
            "round-0001",
            "source_path_id",
            0.99,
            &artifacts,
        )
        .unwrap();

        assert!(!split.available);
        assert_eq!(split.train_group_keys, ["path-a", "path-b"]);
        assert!(split.holdout_group_keys.is_empty());
        assert_eq!(split.train_sample_artifact_ids, ["samples-a", "samples-b"]);
        assert!(split.holdout_sample_artifact_ids.is_empty());
    }

    #[test]
    fn high_ratio_holdout_available_when_target_can_be_met_with_train_remaining() {
        let artifacts = vec![
            sample_artifact_for_tests("samples-a", Some("path-a"), 10),
            sample_artifact_for_tests("samples-b", Some("path-b"), 10),
            sample_artifact_for_tests("samples-c", Some("path-c"), 80),
        ];

        let split = select_diagnostic_holdout_groups(
            "identity-hash",
            "round-0001",
            "source_path_id",
            0.8,
            &artifacts,
        )
        .unwrap();

        assert!(split.available);
        assert_eq!(split.holdout_group_keys, ["path-c"]);
        assert_eq!(split.holdout_sample_artifact_ids, ["samples-c"]);
        assert!(!split.train_group_keys.is_empty());
        assert!(!split.train_sample_artifact_ids.is_empty());
    }

    #[test]
    fn holdout_unavailable_when_remaining_train_has_no_effective_samples() {
        let mut zero_train = sample_artifact_for_tests("samples-a", Some("path-a"), 0);
        zero_train.sample_count = None;
        let artifacts = vec![
            zero_train,
            sample_artifact_for_tests("samples-c", Some("path-c"), 100),
        ];

        let split = select_diagnostic_holdout_groups(
            "identity-hash",
            "round-0001",
            "source_path_id",
            0.5,
            &artifacts,
        )
        .unwrap();

        assert!(!split.available);
        assert_eq!(split.train_group_keys, ["path-a", "path-c"]);
        assert!(split.holdout_group_keys.is_empty());
        assert_eq!(split.train_sample_artifact_ids, ["samples-a", "samples-c"]);
        assert!(split.holdout_sample_artifact_ids.is_empty());
    }

    #[test]
    fn missing_source_path_id_uses_artifact_id_group_key() {
        let artifacts = vec![
            sample_artifact_for_tests("samples-a", None, 30),
            sample_artifact_for_tests("samples-b", Some("path-b"), 70),
        ];

        let split = select_diagnostic_holdout_groups(
            "identity-hash",
            "round-0001",
            "source_path_id",
            0.3,
            &artifacts,
        )
        .unwrap();

        assert!(
            split.train_group_keys.iter().any(|key| key == "samples-a")
                || split.holdout_group_keys.iter().any(|key| key == "samples-a")
        );
    }

    #[test]
    fn invalid_ratio_or_unsupported_group_key_returns_error() {
        let artifacts = vec![sample_artifact_for_tests("samples-a", Some("path-a"), 50)];

        assert!(
            select_diagnostic_holdout_groups(
                "identity-hash",
                "round-0001",
                "source_path_id",
                1.0,
                &artifacts,
            )
            .is_err()
        );
        assert!(
            select_diagnostic_holdout_groups(
                "identity-hash",
                "round-0001",
                "unsupported",
                0.2,
                &artifacts,
            )
            .is_err()
        );
    }

    fn sample_artifact_for_tests(
        id: impl Into<String>,
        source_path_id: Option<&str>,
        sample_count: u64,
    ) -> ArtifactEntry {
        let mut artifact = ArtifactEntry::sample_for_tests(id, Split::Train, true, sample_count, 1);
        artifact.source_path_id = source_path_id.map(str::to_string);
        artifact
    }
}
