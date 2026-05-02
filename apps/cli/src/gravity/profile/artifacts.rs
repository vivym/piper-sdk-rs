#![allow(dead_code)]

use std::{
    fs::{self, File},
    io::Read,
    path::{Component, Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};

use crate::gravity::{
    artifact::{read_path, read_quasi_static_samples},
    profile::{
        config::ProfileConfig,
        context::load_profile_context_unlocked,
        manifest::{ArtifactEntry, Manifest, ManifestLock, Split},
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
    pub source_path: Option<String>,
    pub source_sha256: Option<String>,
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
        arm_id: loaded.header.arm_id,
        target: loaded.header.target,
        joint_map: loaded.header.joint_map,
        load_profile: loaded.header.load_profile,
        torque_convention: loaded.header.torque_convention,
        source_path_id: None,
        source_path: None,
        source_sha256: None,
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
        arm_id: loaded.header.arm_id,
        target: loaded.header.target,
        joint_map: loaded.header.joint_map,
        load_profile: loaded.header.load_profile,
        torque_convention: loaded.header.torque_convention,
        source_path_id: None,
        source_path: Some(loaded.header.source_path),
        source_sha256: Some(loaded.header.source_sha256),
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
        let path = resolve_registered_artifact_path(profile_dir, &artifact.path)?;
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

pub fn registered_artifact_path(profile_dir: &Path, artifact: &ArtifactEntry) -> Result<PathBuf> {
    resolve_registered_artifact_path(profile_dir, &artifact.path)
}

pub fn validate_profile_generated_output_path(
    profile_dir: &Path,
    output_path: &Path,
) -> Result<PathBuf> {
    let canonical_profile = canonical_profile_dir(profile_dir)?;
    let resolved = canonical_existing_or_planned_path(output_path)?;
    ensure_resolved_under_profile(output_path, &resolved, &canonical_profile)?;
    Ok(resolved)
}

fn resolve_registered_artifact_path(profile_dir: &Path, artifact_path: &str) -> Result<PathBuf> {
    let relative = Path::new(artifact_path);
    if relative.is_absolute()
        || relative
            .components()
            .any(|component| matches!(component, Component::ParentDir | Component::Prefix(_)))
    {
        bail!("unsafe registered artifact path {artifact_path:?}");
    }

    let profile_dir = profile_dir
        .canonicalize()
        .with_context(|| format!("failed to canonicalize {}", profile_dir.display()))?;
    let resolved = profile_dir.join(relative).canonicalize().with_context(|| {
        format!(
            "failed to canonicalize registered artifact path {}",
            profile_dir.join(relative).display()
        )
    })?;
    if !resolved.starts_with(&profile_dir) {
        bail!(
            "unsafe registered artifact path {:?}: resolved outside {}",
            artifact_path,
            profile_dir.display()
        );
    }
    Ok(resolved)
}

pub fn register_profile_generated_path(
    profile_dir: &Path,
    split: Split,
    artifact_id: &str,
    output_path: &Path,
    unix_ms: u64,
) -> Result<()> {
    let _lock = ManifestLock::acquire(profile_dir)?;
    let mut context = load_profile_context_unlocked(profile_dir)?;
    validate_profile_generated_output_path(&context.profile_dir, output_path)?;
    let summary = analyze_path_artifact(output_path)
        .with_context(|| format!("failed to analyze {}", output_path.display()))?;
    if summary.kind != ArtifactKind::Path {
        bail!("expected path artifact summary");
    }
    validate_artifact_summary(&context.config, &summary)?;
    let relative_path = profile_relative_artifact_path(&context.profile_dir, output_path)?;
    reserve_planned_artifact_id(&mut context.manifest, "path", artifact_id, unix_ms)?;

    context.manifest.artifacts.push(profile_generated_entry(
        &context.config,
        GeneratedArtifactEntryInput {
            id: artifact_id.to_string(),
            kind: "path".to_string(),
            split,
            path: relative_path,
            source_path_id: None,
            unix_ms,
        },
        summary,
    ));
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

pub fn register_profile_generated_samples(
    profile_dir: &Path,
    split: Split,
    artifact_id: &str,
    source_path_id: &str,
    output_path: &Path,
    unix_ms: u64,
) -> Result<()> {
    let _lock = ManifestLock::acquire(profile_dir)?;
    let mut context = load_profile_context_unlocked(profile_dir)?;
    validate_profile_generated_output_path(&context.profile_dir, output_path)?;
    let summary = analyze_samples_artifact(output_path)
        .with_context(|| format!("failed to analyze {}", output_path.display()))?;
    if summary.kind != ArtifactKind::Samples {
        bail!("expected samples artifact summary");
    }
    validate_artifact_summary(&context.config, &summary)?;
    validate_generated_samples_source(
        &context.profile_dir,
        split,
        source_path_id,
        output_path,
        &summary,
        &context.manifest,
    )?;
    reject_active_other_split_duplicate(&context.manifest, split, &summary)?;
    let relative_path = profile_relative_artifact_path(&context.profile_dir, output_path)?;
    reserve_planned_artifact_id(&mut context.manifest, "samples", artifact_id, unix_ms)?;

    context.manifest.artifacts.push(profile_generated_entry(
        &context.config,
        GeneratedArtifactEntryInput {
            id: artifact_id.to_string(),
            kind: "samples".to_string(),
            split,
            path: relative_path,
            source_path_id: Some(source_path_id.to_string()),
            unix_ms,
        },
        summary,
    ));
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

pub fn register_imported_samples(
    profile_dir: &Path,
    split: Split,
    samples: &[PathBuf],
) -> Result<()> {
    let _lock = ManifestLock::acquire(profile_dir)?;
    let mut context = load_profile_context_unlocked(profile_dir)?;
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
        let (arm_id, arm_id_source) = match copied_summary.arm_id.clone() {
            Some(arm_id) => (arm_id, Some("artifact_header".to_string())),
            None => (
                context.config.arm_id.clone(),
                Some("legacy_import_profile_asserted".to_string()),
            ),
        };

        context.manifest.artifacts.push(ArtifactEntry {
            id: artifact_id,
            kind: "samples".to_string(),
            split,
            active: true,
            path: relative_path,
            sha256: copied_summary.sha256,
            source_path_id: None,
            role: copied_summary.role,
            arm_id,
            arm_id_source,
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

fn reserve_planned_artifact_id(
    manifest: &mut Manifest,
    kind: &str,
    artifact_id: &str,
    unix_ms: u64,
) -> Result<()> {
    if manifest.artifacts.iter().any(|artifact| artifact.id == artifact_id) {
        bail!("artifact id {artifact_id:?} is already registered");
    }

    let allocated = manifest.next_artifact_id(kind, unix_ms);
    if allocated != artifact_id {
        bail!("planned artifact id {artifact_id:?} no longer matches next id {allocated:?}");
    }
    Ok(())
}

fn validate_generated_samples_source(
    profile_dir: &Path,
    split: Split,
    source_path_id: &str,
    output_path: &Path,
    summary: &ArtifactSummary,
    manifest: &Manifest,
) -> Result<()> {
    let source_artifact = manifest
        .artifacts
        .iter()
        .find(|artifact| artifact.id == source_path_id)
        .ok_or_else(|| {
            anyhow::anyhow!("source path artifact {source_path_id:?} is not registered")
        })?;
    if source_artifact.kind != "path" {
        bail!(
            "source artifact {} must be kind \"path\", got {:?}",
            source_artifact.id,
            source_artifact.kind
        );
    }
    if source_artifact.split != split {
        bail!(
            "source path artifact {} is in {:?} split, requested {:?}",
            source_artifact.id,
            source_artifact.split,
            split
        );
    }
    if !source_artifact.active {
        bail!("source path artifact {} is not active", source_artifact.id);
    }

    verify_registered_artifacts(profile_dir, std::slice::from_ref(source_artifact))?;

    let source_sha256 = summary
        .source_sha256
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("generated samples missing source_sha256"))?;
    if source_sha256 != source_artifact.sha256 {
        bail!(
            "generated samples source_sha256 mismatch for source path {}: samples header has {}, manifest has {}",
            source_artifact.id,
            source_sha256,
            source_artifact.sha256
        );
    }

    let source_path = summary
        .source_path
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("generated samples missing source_path"))?;
    let resolved_source_path =
        resolve_samples_header_source_path(profile_dir, output_path, source_path)?;
    let registered_source_path = registered_artifact_path(profile_dir, source_artifact)?;
    if resolved_source_path != registered_source_path {
        bail!(
            "generated samples source_path {:?} resolves to {}, expected registered source path {}",
            source_path,
            resolved_source_path.display(),
            registered_source_path.display()
        );
    }

    Ok(())
}

fn resolve_samples_header_source_path(
    profile_dir: &Path,
    output_path: &Path,
    source_path: &str,
) -> Result<PathBuf> {
    let source_path = Path::new(source_path);
    if source_path.is_absolute() {
        return source_path.canonicalize().with_context(|| {
            format!(
                "failed to resolve samples source_path {}",
                source_path.display()
            )
        });
    }

    let mut candidates = Vec::new();
    candidates.push(profile_dir.join(source_path));
    if let Some(output_parent) = output_path.parent() {
        candidates.push(output_parent.join(source_path));
    }
    candidates.push(source_path.to_path_buf());

    for candidate in candidates {
        if let Ok(resolved) = candidate.canonicalize() {
            return Ok(resolved);
        }
    }

    bail!(
        "failed to resolve samples source_path {}",
        source_path.display()
    )
}

struct GeneratedArtifactEntryInput {
    id: String,
    kind: String,
    split: Split,
    path: String,
    source_path_id: Option<String>,
    unix_ms: u64,
}

fn profile_generated_entry(
    config: &ProfileConfig,
    input: GeneratedArtifactEntryInput,
    summary: ArtifactSummary,
) -> ArtifactEntry {
    ArtifactEntry {
        id: input.id,
        kind: input.kind,
        split: input.split,
        active: true,
        path: input.path,
        sha256: summary.sha256,
        source_path_id: input.source_path_id,
        role: summary.role,
        arm_id: config.arm_id.clone(),
        arm_id_source: Some("profile_generated".to_string()),
        target: summary.target,
        joint_map: summary.joint_map,
        load_profile: summary.load_profile,
        torque_convention: summary.torque_convention,
        basis: config.basis.clone(),
        sample_count: summary.sample_count,
        waypoint_count: Some(summary.waypoint_count),
        created_at_unix_ms: input.unix_ms,
        promoted_from_round_id: None,
        previous_paths: Vec::new(),
    }
}

fn profile_relative_artifact_path(profile_dir: &Path, path: &Path) -> Result<String> {
    let canonical_profile = canonical_profile_dir(profile_dir)?;
    let resolved = canonical_existing_or_planned_path(path)?;
    ensure_resolved_under_profile(path, &resolved, &canonical_profile)?;

    let relative = match path.strip_prefix(profile_dir) {
        Ok(relative) => relative.to_path_buf(),
        Err(_) => resolved
            .strip_prefix(&canonical_profile)
            .with_context(|| {
                format!(
                    "{} is not under profile directory {}",
                    path.display(),
                    profile_dir.display()
                )
            })?
            .to_path_buf(),
    };

    if relative.as_os_str().is_empty()
        || relative.is_absolute()
        || relative
            .components()
            .any(|component| matches!(component, Component::ParentDir | Component::Prefix(_)))
    {
        bail!("unsafe generated artifact path {}", path.display());
    }

    Ok(relative.to_string_lossy().replace('\\', "/"))
}

fn canonical_profile_dir(profile_dir: &Path) -> Result<PathBuf> {
    profile_dir
        .canonicalize()
        .with_context(|| format!("failed to canonicalize {}", profile_dir.display()))
}

fn canonical_existing_or_planned_path(path: &Path) -> Result<PathBuf> {
    if path.as_os_str().is_empty() {
        bail!("generated output path must not be empty");
    }
    if path.exists() {
        return path
            .canonicalize()
            .with_context(|| format!("failed to canonicalize {}", path.display()));
    }

    let file_name = path
        .file_name()
        .ok_or_else(|| anyhow::anyhow!("generated output path must include a file name"))?;
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .ok_or_else(|| anyhow::anyhow!("generated output path must include a parent directory"))?;
    let canonical_parent = parent.canonicalize().with_context(|| {
        format!(
            "failed to canonicalize parent directory {}",
            parent.display()
        )
    })?;
    Ok(canonical_parent.join(file_name))
}

fn ensure_resolved_under_profile(
    original_path: &Path,
    resolved_path: &Path,
    canonical_profile: &Path,
) -> Result<()> {
    if !resolved_path.starts_with(canonical_profile) {
        bail!(
            "generated output path {} resolves outside profile directory {}",
            original_path.display(),
            canonical_profile.display()
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
            source_path: Some("source.path.jsonl".to_string()),
            source_sha256: Some("source-sha256".to_string()),
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
                manifest::{ArtifactEntry, Manifest, Split},
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
    fn analyze_samples_propagates_optional_arm_id() {
        let dir = tempfile::tempdir().unwrap();
        let samples = write_samples_artifact_with_arm_id_for_tests(
            dir.path(),
            "arm.samples.jsonl",
            "piper-left",
        );

        let summary = analyze_samples_artifact(&samples).unwrap();

        assert_eq!(summary.arm_id.as_deref(), Some("piper-left"));
    }

    #[test]
    fn validate_artifact_summary_rejects_arm_id_mismatch() {
        let config = ProfileConfig::new(
            "profile",
            "slave",
            "piper-left",
            "socketcan:can1",
            "identity",
            "normal-gripper-d405",
        );
        let mut summary = ArtifactSummary::sample_for_tests();
        summary.arm_id = Some("piper-right".to_string());

        let err = validate_artifact_summary(&config, &summary).unwrap_err();

        assert!(err.to_string().contains("arm_id"));
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
    fn import_with_arm_id_records_artifact_header_arm_identity() {
        let dir = tempfile::tempdir().unwrap();
        let fixture = ProfileFixture::new_at(dir.path());
        let samples = write_samples_artifact_with_arm_id_for_tests(
            dir.path(),
            "arm.samples.jsonl",
            "piper-left",
        );

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
        assert_eq!(artifact.arm_id_source.as_deref(), Some("artifact_header"));
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

    #[test]
    fn verify_registered_artifacts_rejects_absolute_manifest_path() {
        let fixture = ProfileFixture::new_with_train_samples();
        let mut artifact = fixture
            .load_manifest()
            .artifacts
            .iter()
            .find(|artifact| artifact.kind == "samples" && artifact.active)
            .unwrap()
            .clone();
        artifact.path = fixture.profile_dir().join(&artifact.path).to_string_lossy().to_string();

        let err = verify_registered_artifacts(fixture.profile_dir(), &[artifact]).unwrap_err();

        assert!(err.to_string().contains("unsafe"));
    }

    #[test]
    fn verify_registered_artifacts_rejects_parent_dir_manifest_path() {
        let fixture = ProfileFixture::new_with_train_samples();
        let mut artifact = fixture
            .load_manifest()
            .artifacts
            .iter()
            .find(|artifact| artifact.kind == "samples" && artifact.active)
            .unwrap()
            .clone();
        let original_path = fixture.profile_dir().join(&artifact.path);
        let escaped_path = fixture.profile_dir().join("../outside.samples.jsonl");
        std::fs::copy(original_path, escaped_path).unwrap();
        artifact.path = "../outside.samples.jsonl".to_string();

        let err = verify_registered_artifacts(fixture.profile_dir(), &[artifact]).unwrap_err();

        assert!(err.to_string().contains("unsafe"));
    }

    #[test]
    fn register_generated_path_records_profile_generated_arm_identity() {
        let dir = tempfile::tempdir().unwrap();
        let fixture = ProfileFixture::new_at(dir.path());
        let artifact_id = "path-20260502-001530-0001";
        let path = fixture
            .profile_dir()
            .join("data/train/paths")
            .join(format!("{artifact_id}.path.jsonl"));
        let path_dir = path.parent().unwrap();
        std::fs::create_dir_all(path_dir).unwrap();
        write_path_artifact_for_tests(path_dir, &format!("{artifact_id}.path.jsonl"), 4);

        register_profile_generated_path(
            fixture.profile_dir(),
            Split::Train,
            artifact_id,
            &path,
            unix_ms_for_tests(),
        )
        .unwrap();

        let manifest = fixture.load_manifest();
        let artifact = manifest.artifacts.first().unwrap();
        assert_eq!(artifact.kind, "path");
        assert_eq!(artifact.arm_id, "piper-left");
        assert_eq!(artifact.arm_id_source.as_deref(), Some("profile_generated"));
    }

    #[test]
    fn register_generated_samples_records_source_path_and_profile_generated_arm_identity() {
        let dir = tempfile::tempdir().unwrap();
        let fixture = ProfileFixture::new_at(dir.path());
        let source = fixture.register_path_artifact_for_tests(Split::Train);
        let artifact_id = "samples-20260502-001530-0002";
        let samples = fixture
            .profile_dir()
            .join("data/train/samples")
            .join(format!("{artifact_id}.samples.jsonl"));
        std::fs::create_dir_all(samples.parent().unwrap()).unwrap();
        write_samples_artifact_with_source_for_tests(
            samples.parent().unwrap(),
            &format!("{artifact_id}.samples.jsonl"),
            12,
            4,
            &source.path.canonicalize().unwrap().display().to_string(),
            &source.sha256,
        );

        register_profile_generated_samples(
            fixture.profile_dir(),
            Split::Train,
            artifact_id,
            &source.id,
            &samples,
            unix_ms_for_tests(),
        )
        .unwrap();

        let manifest = fixture.load_manifest();
        let artifact =
            manifest.artifacts.iter().find(|artifact| artifact.kind == "samples").unwrap();
        assert_eq!(artifact.kind, "samples");
        assert_eq!(artifact.source_path_id.as_deref(), Some(source.id.as_str()));
        assert_eq!(artifact.arm_id, "piper-left");
        assert_eq!(artifact.arm_id_source.as_deref(), Some("profile_generated"));
    }

    #[test]
    fn register_generated_samples_rejects_source_hash_mismatch() {
        let dir = tempfile::tempdir().unwrap();
        let fixture = ProfileFixture::new_at(dir.path());
        let source = fixture.register_path_artifact_for_tests(Split::Train);
        let artifact_id = "samples-20260502-001530-0002";
        let samples = fixture
            .profile_dir()
            .join("data/train/samples")
            .join(format!("{artifact_id}.samples.jsonl"));
        std::fs::create_dir_all(samples.parent().unwrap()).unwrap();
        write_samples_artifact_with_source_for_tests(
            samples.parent().unwrap(),
            &format!("{artifact_id}.samples.jsonl"),
            12,
            4,
            &source.path.canonicalize().unwrap().display().to_string(),
            "wrong-source-sha256",
        );

        let err = register_profile_generated_samples(
            fixture.profile_dir(),
            Split::Train,
            artifact_id,
            &source.id,
            &samples,
            unix_ms_for_tests(),
        )
        .unwrap_err();

        assert!(err.to_string().contains("source_sha256"));
    }

    #[test]
    fn register_generated_samples_rejects_unresolved_source_path() {
        let dir = tempfile::tempdir().unwrap();
        let fixture = ProfileFixture::new_at(dir.path());
        let source = fixture.register_path_artifact_for_tests(Split::Train);
        let artifact_id = "samples-20260502-001530-0002";
        let samples = fixture
            .profile_dir()
            .join("data/train/samples")
            .join(format!("{artifact_id}.samples.jsonl"));
        std::fs::create_dir_all(samples.parent().unwrap()).unwrap();
        write_samples_artifact_with_source_for_tests(
            samples.parent().unwrap(),
            &format!("{artifact_id}.samples.jsonl"),
            12,
            4,
            "missing-source.path.jsonl",
            &source.sha256,
        );

        let err = register_profile_generated_samples(
            fixture.profile_dir(),
            Split::Train,
            artifact_id,
            &source.id,
            &samples,
            unix_ms_for_tests(),
        )
        .unwrap_err();

        assert!(err.to_string().contains("source_path"));
    }

    #[test]
    fn register_generated_path_rejects_existing_manifest_lock() {
        let dir = tempfile::tempdir().unwrap();
        let fixture = ProfileFixture::new_at(dir.path());
        let artifact_id = "path-20260502-001530-0001";
        let path = fixture
            .profile_dir()
            .join("data/train/paths")
            .join(format!("{artifact_id}.path.jsonl"));
        let path_dir = path.parent().unwrap();
        std::fs::create_dir_all(path_dir).unwrap();
        write_path_artifact_for_tests(path_dir, &format!("{artifact_id}.path.jsonl"), 4);
        std::fs::write(fixture.profile_dir().join(".manifest.lock"), "locked").unwrap();

        let err = register_profile_generated_path(
            fixture.profile_dir(),
            Split::Train,
            artifact_id,
            &path,
            unix_ms_for_tests(),
        )
        .unwrap_err();

        assert!(err.to_string().contains("manifest lock"));
    }

    #[test]
    fn import_samples_rejects_existing_manifest_lock() {
        let dir = tempfile::tempdir().unwrap();
        let fixture = ProfileFixture::new_at(dir.path());
        let samples = write_samples_artifact_for_tests(dir.path(), "legacy.samples.jsonl", 12, 4);
        std::fs::write(fixture.profile_dir().join(".manifest.lock"), "locked").unwrap();

        let err = import_samples(GravityProfileImportSamplesArgs {
            profile: fixture.profile_dir().to_path_buf(),
            split: "train".to_string(),
            samples: vec![samples],
        })
        .unwrap_err();

        assert!(err.to_string().contains("manifest lock"));
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

        fn register_path_artifact_for_tests(&self, split: Split) -> RegisteredPathForTests {
            let split_dir = split_dir_for_tests(split);
            let id = "path-20260502-001000-0001".to_string();
            let relative_path = format!("data/{split_dir}/paths/{id}.path.jsonl");
            let path = self.profile_dir.join(&relative_path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            write_path_artifact_for_tests(path.parent().unwrap(), &format!("{id}.path.jsonl"), 4);
            let sha256 = file_sha256(&path).unwrap();

            let mut manifest = self.load_manifest();
            manifest.next_artifact_seq = 2;
            manifest.artifacts.push(ArtifactEntry {
                id: id.clone(),
                kind: "path".to_string(),
                split,
                active: true,
                path: relative_path,
                sha256: sha256.clone(),
                source_path_id: None,
                role: "slave".to_string(),
                arm_id: "piper-left".to_string(),
                arm_id_source: Some("profile_generated".to_string()),
                target: "socketcan:can1".to_string(),
                joint_map: "identity".to_string(),
                load_profile: "normal-gripper-d405".to_string(),
                torque_convention: crate::gravity::TORQUE_CONVENTION.to_string(),
                basis: crate::gravity::BASIS_TRIG_V1.to_string(),
                sample_count: None,
                waypoint_count: Some(4),
                created_at_unix_ms: unix_ms_for_tests() - 30_000,
                promoted_from_round_id: None,
                previous_paths: Vec::new(),
            });
            manifest.save_atomic(self.profile_dir.join("manifest.json")).unwrap();

            RegisteredPathForTests { id, path, sha256 }
        }
    }

    struct RegisteredPathForTests {
        id: String,
        path: PathBuf,
        sha256: String,
    }

    fn unix_ms_for_tests() -> u64 {
        1_777_680_930_000
    }

    fn write_samples_artifact_for_tests(
        dir: &Path,
        name: &str,
        sample_count: usize,
        waypoint_count: usize,
    ) -> PathBuf {
        write_samples_artifact_with_optional_arm_id_for_tests(
            dir,
            name,
            sample_count,
            waypoint_count,
            None,
        )
    }

    fn write_samples_artifact_with_arm_id_for_tests(
        dir: &Path,
        name: &str,
        arm_id: &str,
    ) -> PathBuf {
        write_samples_artifact_with_optional_arm_id_for_tests(
            dir,
            name,
            12,
            4,
            Some(arm_id.to_string()),
        )
    }

    fn write_samples_artifact_with_optional_arm_id_for_tests(
        dir: &Path,
        name: &str,
        sample_count: usize,
        waypoint_count: usize,
        arm_id: Option<String>,
    ) -> PathBuf {
        write_samples_artifact_with_source_and_optional_arm_id_for_tests(
            dir,
            name,
            sample_count,
            waypoint_count,
            "legacy.path.jsonl",
            "source-sha256",
            arm_id,
        )
    }

    fn write_samples_artifact_with_source_for_tests(
        dir: &Path,
        name: &str,
        sample_count: usize,
        waypoint_count: usize,
        source_path: &str,
        source_sha256: &str,
    ) -> PathBuf {
        write_samples_artifact_with_source_and_optional_arm_id_for_tests(
            dir,
            name,
            sample_count,
            waypoint_count,
            source_path,
            source_sha256,
            None,
        )
    }

    fn write_samples_artifact_with_source_and_optional_arm_id_for_tests(
        dir: &Path,
        name: &str,
        sample_count: usize,
        waypoint_count: usize,
        source_path: &str,
        source_sha256: &str,
        arm_id: Option<String>,
    ) -> PathBuf {
        let path = dir.join(name);
        let mut file = std::fs::File::create(&path).unwrap();
        let header = SamplesHeader {
            row_type: "header".to_string(),
            artifact_kind: "quasi-static-samples".to_string(),
            schema_version: 1,
            source_path: source_path.to_string(),
            source_sha256: source_sha256.to_string(),
            role: "slave".to_string(),
            arm_id,
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

    fn split_dir_for_tests(split: Split) -> &'static str {
        match split {
            Split::Train => "train",
            Split::Validation => "validation",
        }
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
