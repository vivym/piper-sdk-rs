use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, bail};

use crate::{
    commands::gravity::{
        GravityProfileImportSamplesArgs, GravityProfileInitArgs, GravityProfilePathArgs,
        GravityProfileRecordPathArgs, GravityProfileReplaySampleArgs, GravityRecordPathArgs,
        GravityReplaySampleArgs,
    },
    gravity::profile::{
        artifacts::{
            register_imported_samples, register_profile_generated_path,
            register_profile_generated_samples, registered_artifact_path,
            verify_registered_artifacts,
        },
        config::ProfileConfig,
        context::load_profile_context,
        manifest::{ArtifactEntry, Manifest, ProfileStatus, Split},
        status::next_action,
    },
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ResolvedInitProfileLocation {
    pub name: String,
    pub profile_dir: PathBuf,
}

#[derive(Debug, Clone)]
pub(crate) struct PlannedRecordPath {
    pub artifact_id: String,
    pub args: GravityRecordPathArgs,
}

#[derive(Debug, Clone)]
pub(crate) struct PlannedReplaySample {
    pub artifact_id: String,
    pub source_path_id: String,
    pub args: GravityReplaySampleArgs,
}

pub fn init_profile(args: GravityProfileInitArgs) -> Result<()> {
    let resolved = resolve_init_profile_location(&args)?;
    let profile_toml = resolved.profile_dir.join("profile.toml");
    let manifest_json = resolved.profile_dir.join("manifest.json");

    if manifest_json.exists() {
        bail!("refusing to overwrite existing {}", manifest_json.display());
    }
    if profile_toml.exists() {
        bail!("refusing to overwrite existing {}", profile_toml.display());
    }

    let config = ProfileConfig::new(
        resolved.name.clone(),
        args.role,
        args.arm_id,
        args.target,
        args.joint_map,
        args.load_profile,
    );
    let mut manifest = Manifest::new(
        &config.name,
        config.identity_sha256()?,
        config.config_sha256()?,
    );
    manifest.profile_config_sections_sha256 = Some(config.section_sha256()?);

    for relative_dir in [
        "data/train/paths",
        "data/train/samples",
        "data/validation/paths",
        "data/validation/samples",
        "data/retired-validation",
        "models",
        "reports",
        "rounds",
    ] {
        fs::create_dir_all(resolved.profile_dir.join(relative_dir)).with_context(|| {
            format!(
                "failed to create {}",
                resolved.profile_dir.join(relative_dir).display()
            )
        })?;
    }

    config
        .save(&profile_toml)
        .with_context(|| format!("failed to save {}", profile_toml.display()))?;
    manifest
        .save_atomic(&manifest_json)
        .with_context(|| format!("failed to save {}", manifest_json.display()))?;

    println!(
        "Initialized gravity profile {}",
        resolved.profile_dir.display()
    );
    Ok(())
}

pub fn print_status(args: GravityProfilePathArgs) -> Result<()> {
    let context = load_profile_context(&args.profile)?;
    let config = &context.config;
    let manifest = &context.manifest;
    let counts = active_sample_counts(manifest);

    println!("Profile: {}", manifest.profile_name);
    println!("Directory: {}", context.profile_dir.display());
    println!(
        "Identity: role={} arm_id={} joint_map={} load_profile={} basis={}",
        config.role, config.arm_id, config.joint_map, config.load_profile, config.basis
    );
    println!("Current target: {}", config.target);

    let artifact_targets: BTreeSet<_> = manifest
        .artifacts
        .iter()
        .filter(|artifact| artifact.active && artifact.target != config.target)
        .map(|artifact| artifact.target.as_str())
        .collect();
    if !artifact_targets.is_empty() {
        println!(
            "Active artifact targets: {}",
            artifact_targets.into_iter().collect::<Vec<_>>().join(", ")
        );
    }

    println!(
        "Train samples: {} artifacts, {} samples, {} waypoints",
        counts.train_artifacts, counts.train_samples, counts.train_waypoints
    );
    println!(
        "Validation samples: {} artifacts, {} samples, {} waypoints",
        counts.validation_artifacts, counts.validation_samples, counts.validation_waypoints
    );

    if let Some(round) = manifest.rounds.last() {
        println!(
            "Latest round: {} ({})",
            round.id,
            status_label(round.status)?
        );
    } else {
        println!("Latest round: none");
    }

    if let Some(best) = &manifest.current_best_model {
        println!("Best model: {} ({})", best.path, best.round_id);
    } else {
        println!("Best model: none");
    }

    println!("Status: {}", status_label(manifest.status)?);
    if let Some(failure) = manifest.rounds.iter().rev().find_map(|round| round.failure.as_ref()) {
        println!("Last failed checks: {}: {}", failure.kind, failure.message);
    }

    Ok(())
}

pub fn print_next(args: GravityProfilePathArgs) -> Result<()> {
    let context = load_profile_context(&args.profile)?;
    println!("{}", next_action(context.manifest.status));
    Ok(())
}

pub fn import_samples(args: GravityProfileImportSamplesArgs) -> Result<()> {
    let split = parse_split(&args.split)?;
    register_imported_samples(&args.profile, split, &args.samples)?;
    println!(
        "Imported {} samples artifact(s) into {}",
        args.samples.len(),
        args.profile.display()
    );
    Ok(())
}

pub async fn record_path(args: GravityProfileRecordPathArgs) -> Result<()> {
    let split = parse_split(&args.split)?;
    let unix_ms = current_unix_ms();
    let planned = plan_record_path(&args.profile, split, args.notes, unix_ms)?;
    if let Some(parent) = planned.args.out.parent().filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }

    let output_path = planned.args.out.clone();
    crate::gravity::record_path::run(planned.args).await?;
    register_profile_generated_path(
        &args.profile,
        split,
        &planned.artifact_id,
        &output_path,
        unix_ms,
    )?;
    println!("Registered profile path artifact {}", planned.artifact_id);
    Ok(())
}

pub async fn replay_sample(args: GravityProfileReplaySampleArgs) -> Result<()> {
    let split = parse_split(&args.split)?;
    let unix_ms = current_unix_ms();
    let planned = plan_replay_sample(&args.profile, split, &args.path, args.dry_run, unix_ms)?;
    let output_path = planned.args.out.clone();
    let dry_run = planned.args.dry_run;
    if !dry_run
        && let Some(parent) =
            planned.args.out.parent().filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }

    crate::gravity::replay_sample::run(planned.args).await?;
    if dry_run {
        return Ok(());
    }

    register_profile_generated_samples(
        &args.profile,
        split,
        &planned.artifact_id,
        &planned.source_path_id,
        &output_path,
        unix_ms,
    )?;
    println!(
        "Registered profile samples artifact {}",
        planned.artifact_id
    );
    Ok(())
}

pub(crate) fn plan_record_path(
    profile_dir: &Path,
    split: Split,
    notes: Option<String>,
    unix_ms: u64,
) -> Result<PlannedRecordPath> {
    let mut context = load_profile_context(profile_dir)?;
    let artifact_id = context.manifest.next_artifact_id("path", unix_ms);
    let output = context
        .profile_dir
        .join("data")
        .join(split_dir(split))
        .join("paths")
        .join(format!("{artifact_id}.path.jsonl"));

    Ok(PlannedRecordPath {
        artifact_id,
        args: GravityRecordPathArgs {
            role: context.config.role,
            target: Some(context.config.target),
            interface: None,
            joint_map: context.config.joint_map,
            load_profile: context.config.load_profile,
            out: output,
            frequency_hz: 50.0,
            notes,
        },
    })
}

pub(crate) fn plan_replay_sample(
    profile_dir: &Path,
    split: Split,
    path_selector: &str,
    dry_run: bool,
    unix_ms: u64,
) -> Result<PlannedReplaySample> {
    let mut context = load_profile_context(profile_dir)?;
    let source_artifact = select_path_artifact(&context.manifest, split, path_selector)?;
    verify_registered_artifacts(&context.profile_dir, std::slice::from_ref(&source_artifact))?;
    let source_path = registered_artifact_path(&context.profile_dir, &source_artifact)?;

    let artifact_id = context.manifest.next_artifact_id("samples", unix_ms);
    let output = context
        .profile_dir
        .join("data")
        .join(split_dir(split))
        .join("samples")
        .join(format!("{artifact_id}.samples.jsonl"));

    Ok(PlannedReplaySample {
        artifact_id,
        source_path_id: source_artifact.id,
        args: GravityReplaySampleArgs {
            role: context.config.role,
            target: Some(context.config.target),
            interface: None,
            path: source_path,
            out: output,
            max_velocity_rad_s: context.config.replay.max_velocity_rad_s,
            max_step_rad: context.config.replay.max_step_rad,
            settle_ms: context.config.replay.settle_ms,
            sample_ms: context.config.replay.sample_ms,
            bidirectional: context.config.replay.bidirectional,
            dry_run,
        },
    })
}

pub(crate) fn resolve_init_profile_location(
    args: &GravityProfileInitArgs,
) -> Result<ResolvedInitProfileLocation> {
    let cwd = std::env::current_dir().context("failed to resolve current directory")?;
    resolve_init_profile_location_from_cwd(args, &cwd)
}

fn resolve_init_profile_location_from_cwd(
    args: &GravityProfileInitArgs,
    cwd: &Path,
) -> Result<ResolvedInitProfileLocation> {
    let name = args
        .name
        .clone()
        .unwrap_or_else(|| format!("{}-{}-{}", args.role, args.arm_id, args.load_profile));
    let profile_dir = match &args.profile {
        Some(profile) => profile.clone(),
        None => cwd.join("artifacts/gravity/profiles").join(&name),
    };

    Ok(ResolvedInitProfileLocation { name, profile_dir })
}

#[derive(Debug, Default)]
struct ActiveSampleCounts {
    train_artifacts: u64,
    train_samples: u64,
    train_waypoints: u64,
    validation_artifacts: u64,
    validation_samples: u64,
    validation_waypoints: u64,
}

fn active_sample_counts(manifest: &Manifest) -> ActiveSampleCounts {
    let mut counts = ActiveSampleCounts::default();
    for artifact in manifest
        .artifacts
        .iter()
        .filter(|artifact| artifact.active && artifact.kind == "samples")
    {
        match artifact.split {
            Split::Train => {
                counts.train_artifacts += 1;
                counts.train_samples += artifact.sample_count.unwrap_or(0);
                counts.train_waypoints += artifact.waypoint_count.unwrap_or(0);
            },
            Split::Validation => {
                counts.validation_artifacts += 1;
                counts.validation_samples += artifact.sample_count.unwrap_or(0);
                counts.validation_waypoints += artifact.waypoint_count.unwrap_or(0);
            },
        }
    }
    counts
}

fn status_label(status: ProfileStatus) -> Result<String> {
    let json = serde_json::to_string(&status).context("failed to serialize profile status")?;
    Ok(json.trim_matches('"').to_string())
}

fn parse_split(split: &str) -> Result<Split> {
    match split {
        "train" => Ok(Split::Train),
        "validation" => Ok(Split::Validation),
        _ => bail!("unsupported split {split:?}; expected \"train\" or \"validation\""),
    }
}

fn select_path_artifact(
    manifest: &Manifest,
    split: Split,
    path_selector: &str,
) -> Result<ArtifactEntry> {
    if path_selector == "latest" {
        return manifest
            .artifacts
            .iter()
            .filter(|artifact| {
                artifact.active && artifact.kind == "path" && artifact.split == split
            })
            .max_by(|left, right| {
                left.created_at_unix_ms
                    .cmp(&right.created_at_unix_ms)
                    .then_with(|| left.id.cmp(&right.id))
            })
            .cloned()
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "no active path artifact found in {} split",
                    split_label(split)
                )
            });
    }

    let artifact = manifest
        .artifacts
        .iter()
        .find(|artifact| artifact.kind == "path" && artifact.id == path_selector)
        .ok_or_else(|| anyhow::anyhow!("path artifact {path_selector:?} is not registered"))?;
    if artifact.split != split {
        bail!(
            "cross-split replay is forbidden: path artifact {} is in {} split, requested {}",
            artifact.id,
            split_label(artifact.split),
            split_label(split)
        );
    }
    if !artifact.active {
        bail!("path artifact {} is not active", artifact.id);
    }
    Ok(artifact.clone())
}

fn split_dir(split: Split) -> &'static str {
    match split {
        Split::Train => "train",
        Split::Validation => "validation",
    }
}

fn split_label(split: Split) -> &'static str {
    split_dir(split)
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
    use crate::commands::gravity::GravityProfileInitArgs;
    use crate::gravity::profile::{
        config::ProfileConfig,
        manifest::{ArtifactEntry, Manifest, ProfileStatus, Split},
    };

    #[test]
    fn profile_record_path_builds_low_level_args_from_config_and_split() {
        let fixture = ProfileFixture::new();

        let planned = plan_record_path(
            fixture.profile_dir(),
            Split::Train,
            Some("operator note".to_string()),
            unix_ms_for_tests(),
        )
        .unwrap();

        assert_eq!(planned.args.role, "slave");
        assert_eq!(planned.args.target.as_deref(), Some("socketcan:can1"));
        assert_eq!(planned.args.interface, None);
        assert_eq!(planned.args.joint_map, "identity");
        assert_eq!(planned.args.load_profile, "normal-gripper-d405");
        assert_eq!(planned.args.notes.as_deref(), Some("operator note"));
        assert_eq!(planned.artifact_id, "path-20260502-001530-0001");
        assert!(planned.args.out.starts_with(fixture.profile_dir().join("data/train/paths")));
        assert!(planned.args.out.ends_with("path-20260502-001530-0001.path.jsonl"));
    }

    #[test]
    fn profile_replay_sample_uses_latest_path_from_requested_split() {
        let fixture = ProfileFixture::new_with_registered_path(Split::Validation);

        let planned = plan_replay_sample(
            fixture.profile_dir(),
            Split::Validation,
            "latest",
            true,
            unix_ms_for_tests(),
        )
        .unwrap();

        assert_eq!(planned.source_path_id, fixture.latest_path_id());
        assert_eq!(planned.args.path, fixture.latest_path_file());
        assert_eq!(planned.args.target.as_deref(), Some("socketcan:can1"));
        assert_eq!(planned.args.interface, None);
        assert_eq!(planned.args.max_velocity_rad_s, 0.08);
        assert_eq!(planned.args.max_step_rad, 0.02);
        assert_eq!(planned.args.settle_ms, 500);
        assert_eq!(planned.args.sample_ms, 300);
        assert!(planned.args.bidirectional);
        assert!(planned.args.dry_run);
        assert!(
            planned
                .args
                .out
                .starts_with(fixture.profile_dir().join("data/validation/samples"))
        );
        assert!(planned.args.out.ends_with("samples-20260502-001530-0002.samples.jsonl"));
    }

    #[test]
    fn profile_replay_sample_accepts_explicit_path_artifact_id() {
        let fixture = ProfileFixture::new_with_registered_path(Split::Train);

        let planned = plan_replay_sample(
            fixture.profile_dir(),
            Split::Train,
            fixture.latest_path_id(),
            false,
            unix_ms_for_tests(),
        )
        .unwrap();

        assert_eq!(planned.source_path_id, fixture.latest_path_id());
        assert_eq!(planned.args.path, fixture.latest_path_file());
        assert!(!planned.args.dry_run);
    }

    #[test]
    fn profile_replay_sample_rejects_cross_split_path_artifact() {
        let fixture = ProfileFixture::new_with_registered_path(Split::Validation);

        let err = plan_replay_sample(
            fixture.profile_dir(),
            Split::Train,
            fixture.latest_path_id(),
            true,
            unix_ms_for_tests(),
        )
        .unwrap_err();

        assert!(err.to_string().contains("cross-split"));
    }

    #[test]
    fn profile_replay_sample_verifies_registered_path_sha_before_replay() {
        let fixture = ProfileFixture::new_with_registered_path(Split::Train);
        std::fs::write(fixture.latest_path_file(), "tampered\n").unwrap();

        let err = plan_replay_sample(
            fixture.profile_dir(),
            Split::Train,
            "latest",
            true,
            unix_ms_for_tests(),
        )
        .unwrap_err();

        assert!(err.to_string().contains("sha256"));
    }

    #[test]
    fn init_creates_profile_layout_config_and_manifest() {
        let dir = tempfile::tempdir().unwrap();
        let profile = dir.path().join("slave-piper-left-normal-gripper-d405");

        init_profile(GravityProfileInitArgs {
            profile: Some(profile.clone()),
            name: Some("slave-piper-left-normal-gripper-d405".to_string()),
            role: "slave".to_string(),
            arm_id: "piper-left".to_string(),
            target: "socketcan:can1".to_string(),
            joint_map: "identity".to_string(),
            load_profile: "normal-gripper-d405".to_string(),
        })
        .unwrap();

        assert!(profile.join("profile.toml").exists());
        assert!(profile.join("manifest.json").exists());
        assert!(profile.join("data/train/paths").is_dir());
        assert!(profile.join("data/validation/samples").is_dir());
        assert!(profile.join("models").is_dir());
    }

    #[test]
    fn init_derives_default_name_and_profile_path() {
        let dir = tempfile::tempdir().unwrap();

        let resolved =
            resolve_init_profile_location_from_cwd(&init_args_without_profile(), dir.path())
                .unwrap();

        assert_eq!(resolved.name, "slave-piper-left-normal-gripper-d405");
        assert_eq!(
            resolved.profile_dir,
            dir.path()
                .join("artifacts/gravity/profiles/slave-piper-left-normal-gripper-d405")
        );
    }

    #[test]
    fn init_refuses_existing_profile_manifest() {
        let dir = tempfile::tempdir().unwrap();
        let profile = dir.path().join("profile");
        std::fs::create_dir_all(&profile).unwrap();
        std::fs::write(profile.join("manifest.json"), "{}").unwrap();

        let err = init_profile(init_args_for_tests(profile)).unwrap_err();

        assert!(err.to_string().contains("manifest.json"));
    }

    #[test]
    fn init_refuses_existing_profile_config() {
        let dir = tempfile::tempdir().unwrap();
        let profile = dir.path().join("profile");
        std::fs::create_dir_all(&profile).unwrap();
        std::fs::write(profile.join("profile.toml"), "").unwrap();

        let err = init_profile(init_args_for_tests(profile)).unwrap_err();

        assert!(err.to_string().contains("profile.toml"));
    }

    #[test]
    fn init_manifest_hashes_match_loaded_config() {
        let dir = tempfile::tempdir().unwrap();
        let profile = dir.path().join("profile");

        init_profile(init_args_for_tests(profile.clone())).unwrap();

        let config = ProfileConfig::load(profile.join("profile.toml")).unwrap();
        let manifest = Manifest::load(profile.join("manifest.json")).unwrap();

        assert_eq!(
            manifest.profile_identity_sha256,
            config.identity_sha256().unwrap()
        );
        assert_eq!(
            manifest.profile_config_sha256,
            config.config_sha256().unwrap()
        );
        assert_eq!(
            manifest.profile_config_sections_sha256,
            Some(config.section_sha256().unwrap())
        );
    }

    #[test]
    fn print_next_applies_context_loader_config_change_side_effect() {
        let dir = tempfile::tempdir().unwrap();
        let profile = dir.path().join("profile");
        init_profile(init_args_for_tests(profile.clone())).unwrap();

        let mut config = ProfileConfig::load(profile.join("profile.toml")).unwrap();
        config.target = "socketcan:can0".to_string();
        config.save(profile.join("profile.toml")).unwrap();

        print_next(crate::commands::gravity::GravityProfilePathArgs {
            profile: profile.clone(),
        })
        .unwrap();

        let manifest = Manifest::load(profile.join("manifest.json")).unwrap();
        assert_eq!(
            manifest.profile_config_sha256,
            config.config_sha256().unwrap()
        );
        assert_eq!(manifest.status, ProfileStatus::NeedsTrainData);
        assert!(manifest.events.iter().any(|event| event.kind == "profile_config_changed"));
    }

    fn init_args_without_profile() -> GravityProfileInitArgs {
        GravityProfileInitArgs {
            profile: None,
            name: None,
            role: "slave".to_string(),
            arm_id: "piper-left".to_string(),
            target: "socketcan:can1".to_string(),
            joint_map: "identity".to_string(),
            load_profile: "normal-gripper-d405".to_string(),
        }
    }

    fn init_args_for_tests(profile: std::path::PathBuf) -> GravityProfileInitArgs {
        GravityProfileInitArgs {
            profile: Some(profile),
            name: Some("slave-piper-left-normal-gripper-d405".to_string()),
            role: "slave".to_string(),
            arm_id: "piper-left".to_string(),
            target: "socketcan:can1".to_string(),
            joint_map: "identity".to_string(),
            load_profile: "normal-gripper-d405".to_string(),
        }
    }

    fn unix_ms_for_tests() -> u64 {
        1_777_680_930_000
    }

    struct ProfileFixture {
        _temp_dir: tempfile::TempDir,
        profile_dir: std::path::PathBuf,
        latest_path_id: Option<String>,
        latest_path_file: Option<std::path::PathBuf>,
    }

    impl ProfileFixture {
        fn new() -> Self {
            let temp_dir = tempfile::tempdir().unwrap();
            let profile_dir = temp_dir.path().join("profile");
            init_profile(init_args_for_tests(profile_dir.clone())).unwrap();
            Self {
                _temp_dir: temp_dir,
                profile_dir,
                latest_path_id: None,
                latest_path_file: None,
            }
        }

        fn new_with_registered_path(split: Split) -> Self {
            let fixture = Self::new();
            let split_dir = split_dir_for_tests(split);
            let path_id = "path-20260502-001000-0001".to_string();
            let relative_path = format!("data/{split_dir}/paths/{path_id}.path.jsonl");
            let path_file = fixture.profile_dir.join(&relative_path);
            std::fs::create_dir_all(path_file.parent().unwrap()).unwrap();
            write_path_artifact_for_tests(&path_file);

            let mut manifest = Manifest::load(fixture.profile_dir.join("manifest.json")).unwrap();
            manifest.next_artifact_seq = 2;
            manifest.artifacts.push(ArtifactEntry {
                id: path_id.clone(),
                kind: "path".to_string(),
                split,
                active: true,
                path: relative_path,
                sha256: crate::gravity::profile::artifacts::file_sha256(&path_file).unwrap(),
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
                waypoint_count: Some(2),
                created_at_unix_ms: unix_ms_for_tests() - 30_000,
                promoted_from_round_id: None,
                previous_paths: Vec::new(),
            });
            manifest.save_atomic(fixture.profile_dir.join("manifest.json")).unwrap();

            Self {
                latest_path_id: Some(path_id),
                latest_path_file: Some(path_file),
                ..fixture
            }
        }

        fn profile_dir(&self) -> &std::path::Path {
            &self.profile_dir
        }

        fn latest_path_id(&self) -> &str {
            self.latest_path_id.as_deref().unwrap()
        }

        fn latest_path_file(&self) -> std::path::PathBuf {
            self.latest_path_file.clone().unwrap()
        }
    }

    fn split_dir_for_tests(split: Split) -> &'static str {
        match split {
            Split::Train => "train",
            Split::Validation => "validation",
        }
    }

    fn write_path_artifact_for_tests(path: &std::path::Path) {
        let mut file = std::fs::File::create(path).unwrap();
        let header = crate::gravity::artifact::PathHeader::new(
            "slave",
            "socketcan:can1",
            "identity",
            "normal-gripper-d405",
            None,
        );
        crate::gravity::artifact::write_jsonl_row(&mut file, &header).unwrap();
        for index in 0..2 {
            crate::gravity::artifact::write_jsonl_row(&mut file, &path_row_for_tests(index))
                .unwrap();
        }
    }

    fn path_row_for_tests(sample_index: u64) -> crate::gravity::artifact::PathSampleRow {
        crate::gravity::artifact::PathSampleRow {
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

    mod import {
        use std::path::{Path, PathBuf};

        use super::*;
        use crate::{
            commands::gravity::GravityProfileImportSamplesArgs,
            gravity::{
                artifact::{PassDirection, QuasiStaticSampleRow, SamplesHeader, write_jsonl_row},
                profile::manifest::Split,
            },
        };

        #[test]
        fn import_samples_registers_train_artifact_and_updates_readiness() {
            let dir = tempfile::tempdir().unwrap();
            let profile = dir.path().join("profile");
            init_profile(init_args_for_tests(profile.clone())).unwrap();
            let samples = write_samples_artifact_for_tests(dir.path(), "train.samples.jsonl");

            import_samples(GravityProfileImportSamplesArgs {
                profile: profile.clone(),
                split: "train".to_string(),
                samples: vec![samples],
            })
            .unwrap();

            let manifest = Manifest::load(profile.join("manifest.json")).unwrap();
            assert_eq!(manifest.status, ProfileStatus::NeedsValidationData);
            let artifact = manifest.artifacts.first().unwrap();
            assert_eq!(artifact.kind, "samples");
            assert_eq!(artifact.split, Split::Train);
            assert!(profile.join(&artifact.path).exists());
        }

        fn write_samples_artifact_for_tests(dir: &Path, name: &str) -> PathBuf {
            let path = dir.join(name);
            let mut file = std::fs::File::create(&path).unwrap();
            let header = SamplesHeader {
                row_type: "header".to_string(),
                artifact_kind: "quasi-static-samples".to_string(),
                schema_version: 1,
                source_path: "legacy.path.jsonl".to_string(),
                source_sha256: "source-sha256".to_string(),
                role: "slave".to_string(),
                arm_id: None,
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
                waypoint_count: 1,
                accepted_waypoint_count: 1,
                rejected_waypoint_count: 0,
            };
            write_jsonl_row(&mut file, &header).unwrap();
            write_jsonl_row(&mut file, &sample_row_for_tests()).unwrap();
            path
        }

        fn sample_row_for_tests() -> QuasiStaticSampleRow {
            QuasiStaticSampleRow {
                row_type: "quasi-static-sample".to_string(),
                waypoint_id: 0,
                segment_id: Some("seg-a".to_string()),
                pass_direction: PassDirection::Forward,
                host_mono_us: 0,
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
}
