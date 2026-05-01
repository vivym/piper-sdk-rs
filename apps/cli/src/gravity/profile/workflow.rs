use std::{collections::BTreeSet, fs, path::PathBuf};

use anyhow::{Context, Result, bail};

use crate::{
    commands::gravity::{GravityProfileInitArgs, GravityProfilePathArgs},
    gravity::profile::{
        config::ProfileConfig,
        context::load_profile_context,
        manifest::{Manifest, ProfileStatus, Split},
        status::next_action,
    },
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ResolvedInitProfileLocation {
    pub name: String,
    pub profile_dir: PathBuf,
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

pub(crate) fn resolve_init_profile_location(
    args: &GravityProfileInitArgs,
) -> Result<ResolvedInitProfileLocation> {
    let name = args
        .name
        .clone()
        .unwrap_or_else(|| format!("{}-{}-{}", args.role, args.arm_id, args.load_profile));
    let profile_dir = match &args.profile {
        Some(profile) => profile.clone(),
        None => std::env::current_dir()
            .context("failed to resolve current directory")?
            .join("artifacts/gravity/profiles")
            .join(&name),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::gravity::GravityProfileInitArgs;

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
        let current = std::env::current_dir().unwrap();
        std::env::set_current_dir(dir.path()).unwrap();

        let resolved = resolve_init_profile_location(&GravityProfileInitArgs {
            profile: None,
            name: None,
            role: "slave".to_string(),
            arm_id: "piper-left".to_string(),
            target: "socketcan:can1".to_string(),
            joint_map: "identity".to_string(),
            load_profile: "normal-gripper-d405".to_string(),
        })
        .unwrap();

        std::env::set_current_dir(current).unwrap();

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
}
