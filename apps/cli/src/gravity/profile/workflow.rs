use std::{
    collections::BTreeSet,
    fs::{self, OpenOptions},
    io::{BufWriter, Write},
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, bail};
use serde::Serialize;
use serde_json::Value;

use crate::{
    commands::gravity::{
        GravityProfileImportSamplesArgs, GravityProfileInitArgs, GravityProfilePathArgs,
        GravityProfileRecordPathArgs, GravityProfileReplaySampleArgs, GravityRecordPathArgs,
        GravityReplaySampleArgs,
    },
    gravity::{
        artifact::read_quasi_static_samples,
        eval::{evaluate_model_on_rows, validate_model_matches_samples},
        fit::{FitOptions, fit_model_from_rows},
        model::QuasiStaticTorqueModel,
        profile::{
            artifacts::{
                file_sha256, register_imported_samples, register_profile_generated_path,
                register_profile_generated_samples, registered_artifact_path,
                validate_profile_generated_output_path, verify_registered_artifacts,
            },
            assessment::{
                AssessmentCounts, DiagnosticHoldoutMetrics, build_assessment_report,
                build_count_only_assessment_report,
            },
            config::{ProfileConfig, StrictGateConfig},
            context::{load_profile_context, load_profile_context_unlocked},
            holdout::select_diagnostic_holdout_groups,
            manifest::{
                ArtifactEntry, CurrentBestModel, Manifest, ManifestLock, ProfileStatus, RoundEntry,
                RoundFailure, Split,
            },
            status::next_action,
        },
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

pub(crate) fn fit_assess(args: GravityProfilePathArgs) -> Result<()> {
    let _lock = ManifestLock::acquire(&args.profile)?;
    let mut context = load_profile_context_unlocked(&args.profile)?;
    let profile_dir = context.profile_dir.clone();
    let manifest_path = profile_dir.join("manifest.json");

    let train_artifacts = active_sample_artifacts(&context.manifest, Split::Train);
    let validation_artifacts = active_sample_artifacts(&context.manifest, Split::Validation);
    if train_artifacts.is_empty() {
        bail!("no active train sample artifacts registered");
    }
    if validation_artifacts.is_empty() {
        bail!("no active validation sample artifacts registered");
    }

    let counts = assessment_counts(&context.manifest);
    let round_id = context.manifest.next_round_id();
    let unix_ms = current_unix_ms();
    let train_sample_artifact_ids = active_sample_ids(&context.manifest, Split::Train);
    let validation_sample_artifact_ids = active_sample_ids(&context.manifest, Split::Validation);
    let validation_path_artifact_ids =
        validation_path_ids_for_sample_ids(&context.manifest, &validation_sample_artifact_ids);
    let gate_config = serde_json::to_value(&context.config.gate.strict_v1)
        .context("failed to serialize gate config")?;

    if counts_below_gate(&counts, &context.config.gate.strict_v1) {
        let reason = insufficient_data_reason(&counts, &context.config.gate.strict_v1);
        let report =
            build_count_only_assessment_report(&context.config.gate.strict_v1, counts, &reason);
        let report_relative = format!("reports/{round_id}.assess.json");
        let round_relative = format!("rounds/{round_id}.json");
        write_json_create_new(&profile_dir, &report_relative, &report)
            .with_context(|| format!("failed to write assessment report for {round_id}"))?;
        let report_sha256 = file_sha256(&profile_dir.join(&report_relative))?;

        let provenance = RoundProvenance::new(RoundProvenanceInput {
            round_id: &round_id,
            status: ProfileStatus::InsufficientData,
            train_sample_artifact_ids: &train_sample_artifact_ids,
            validation_sample_artifact_ids: &validation_sample_artifact_ids,
            validation_path_artifact_ids: &validation_path_artifact_ids,
            diagnostic_train_group_keys: &[],
            diagnostic_holdout_group_keys: &[],
            profile_identity_sha256: &context.manifest.profile_identity_sha256,
            profile_config_sha256: &context.manifest.profile_config_sha256,
            gate_config: &gate_config,
            failure: None,
            created_at_unix_ms: unix_ms,
        });
        write_json_create_new(&profile_dir, &round_relative, &provenance)
            .with_context(|| format!("failed to write round provenance for {round_id}"))?;
        let round_sha256 = file_sha256(&profile_dir.join(&round_relative))?;

        context.manifest.status = ProfileStatus::InsufficientData;
        context.manifest.rounds.push(RoundEntry {
            id: round_id,
            status: ProfileStatus::InsufficientData,
            model_path: None,
            model_sha256: None,
            report_path: Some(report_relative),
            report_sha256: Some(report_sha256),
            round_path: Some(round_relative),
            round_sha256: Some(round_sha256),
            train_sample_artifact_ids,
            validation_sample_artifact_ids,
            validation_path_artifact_ids,
            diagnostic_train_group_keys: Vec::new(),
            diagnostic_holdout_group_keys: Vec::new(),
            profile_identity_sha256: context.manifest.profile_identity_sha256.clone(),
            profile_config_sha256: context.manifest.profile_config_sha256.clone(),
            gate_config,
            created_at_unix_ms: unix_ms,
            failure: None,
        });
        context
            .manifest
            .save_atomic(&manifest_path)
            .with_context(|| format!("failed to save {}", manifest_path.display()))?;
        return Ok(());
    }

    match fit_assess_after_count_gate(PostGateFitInput {
        profile_dir: &profile_dir,
        config: &context.config,
        manifest: &context.manifest,
        round_id: &round_id,
        counts,
        train_artifacts: &train_artifacts,
        validation_artifacts: &validation_artifacts,
        train_sample_artifact_ids: &train_sample_artifact_ids,
        validation_sample_artifact_ids: &validation_sample_artifact_ids,
        validation_path_artifact_ids: &validation_path_artifact_ids,
        gate_config: &gate_config,
        unix_ms,
    }) {
        Ok(completed) => {
            context.manifest.status = completed.status;
            context.manifest.current_best_model = completed.current_best_model;
            context.manifest.rounds.push(completed.round_entry);
            context
                .manifest
                .save_atomic(&manifest_path)
                .with_context(|| format!("failed to save {}", manifest_path.display()))?;
            Ok(())
        },
        Err(error) => {
            let failure = RoundFailure {
                kind: "fit".to_string(),
                message: format!("{error:#}"),
            };
            context.manifest.status = ProfileStatus::FitFailed;
            let round_entry = write_failure_round(FailureRoundInput {
                profile_dir: &profile_dir,
                manifest: &context.manifest,
                gate: &context.config.gate.strict_v1,
                holdout_group_key: &context.config.fit.holdout_group_key,
                holdout_ratio: context.config.fit.holdout_ratio,
                train_artifacts: &train_artifacts,
                round_id: &round_id,
                counts,
                train_sample_artifact_ids: &train_sample_artifact_ids,
                validation_sample_artifact_ids: &validation_sample_artifact_ids,
                validation_path_artifact_ids: &validation_path_artifact_ids,
                gate_config: &gate_config,
                unix_ms,
                failure,
            });
            context.manifest.rounds.push(round_entry);
            context
                .manifest
                .save_atomic(&manifest_path)
                .with_context(|| format!("failed to save {}", manifest_path.display()))?;
            Err(error).with_context(|| format!("fit-assess failed for {round_id}"))
        },
    }
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
    validate_profile_generated_output_path(&context.profile_dir, &output)?;

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
    validate_profile_generated_output_path(&context.profile_dir, &output)?;

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

#[derive(Debug)]
struct CompletedFitAssess {
    status: ProfileStatus,
    current_best_model: Option<CurrentBestModel>,
    round_entry: RoundEntry,
}

#[derive(Debug, Serialize)]
struct RoundProvenance<'a> {
    round_id: &'a str,
    status: ProfileStatus,
    train_sample_artifact_ids: &'a [String],
    validation_sample_artifact_ids: &'a [String],
    validation_path_artifact_ids: &'a [String],
    diagnostic_train_group_keys: &'a [String],
    diagnostic_holdout_group_keys: &'a [String],
    profile_identity_sha256: &'a str,
    profile_config_sha256: &'a str,
    gate_config: &'a Value,
    created_at_unix_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    failure: Option<&'a RoundFailure>,
}

struct RoundProvenanceInput<'a> {
    round_id: &'a str,
    status: ProfileStatus,
    train_sample_artifact_ids: &'a [String],
    validation_sample_artifact_ids: &'a [String],
    validation_path_artifact_ids: &'a [String],
    diagnostic_train_group_keys: &'a [String],
    diagnostic_holdout_group_keys: &'a [String],
    profile_identity_sha256: &'a str,
    profile_config_sha256: &'a str,
    gate_config: &'a Value,
    failure: Option<&'a RoundFailure>,
    created_at_unix_ms: u64,
}

impl<'a> RoundProvenance<'a> {
    fn new(input: RoundProvenanceInput<'a>) -> Self {
        Self {
            round_id: input.round_id,
            status: input.status,
            train_sample_artifact_ids: input.train_sample_artifact_ids,
            validation_sample_artifact_ids: input.validation_sample_artifact_ids,
            validation_path_artifact_ids: input.validation_path_artifact_ids,
            diagnostic_train_group_keys: input.diagnostic_train_group_keys,
            diagnostic_holdout_group_keys: input.diagnostic_holdout_group_keys,
            profile_identity_sha256: input.profile_identity_sha256,
            profile_config_sha256: input.profile_config_sha256,
            gate_config: input.gate_config,
            created_at_unix_ms: input.created_at_unix_ms,
            failure: input.failure,
        }
    }
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

struct PostGateFitInput<'a> {
    profile_dir: &'a Path,
    config: &'a ProfileConfig,
    manifest: &'a Manifest,
    round_id: &'a str,
    counts: AssessmentCounts,
    train_artifacts: &'a [ArtifactEntry],
    validation_artifacts: &'a [ArtifactEntry],
    train_sample_artifact_ids: &'a [String],
    validation_sample_artifact_ids: &'a [String],
    validation_path_artifact_ids: &'a [String],
    gate_config: &'a Value,
    unix_ms: u64,
}

fn fit_assess_after_count_gate(input: PostGateFitInput<'_>) -> Result<CompletedFitAssess> {
    let profile_dir = input.profile_dir;
    let config = input.config;
    let manifest = input.manifest;
    let round_id = input.round_id;
    let counts = input.counts;
    let train_sample_artifact_ids = input.train_sample_artifact_ids;
    let validation_sample_artifact_ids = input.validation_sample_artifact_ids;
    let validation_path_artifact_ids = input.validation_path_artifact_ids;
    let gate_config = input.gate_config;
    let unix_ms = input.unix_ms;

    let mut all_artifacts =
        Vec::with_capacity(input.train_artifacts.len() + input.validation_artifacts.len());
    all_artifacts.extend_from_slice(input.train_artifacts);
    all_artifacts.extend_from_slice(input.validation_artifacts);
    verify_registered_artifacts(profile_dir, &all_artifacts)?;

    let train_paths = active_sample_paths(manifest, profile_dir, Split::Train);
    let validation_paths = active_sample_paths(manifest, profile_dir, Split::Validation);
    let train_loaded = read_quasi_static_samples(&train_paths)
        .with_context(|| "failed to read active train sample artifacts")?;
    let validation_loaded = read_quasi_static_samples(&validation_paths)
        .with_context(|| "failed to read active validation sample artifacts")?;

    let diagnostic_split = select_diagnostic_holdout_groups(
        &manifest.profile_identity_sha256,
        round_id,
        &config.fit.holdout_group_key,
        config.fit.holdout_ratio,
        input.train_artifacts,
    )?;
    let diagnostic_holdout = if diagnostic_split.available {
        let diagnostic_train_paths = sample_paths_for_ids(
            manifest,
            profile_dir,
            &diagnostic_split.train_sample_artifact_ids,
        );
        let diagnostic_holdout_paths = sample_paths_for_ids(
            input.manifest,
            input.profile_dir,
            &diagnostic_split.holdout_sample_artifact_ids,
        );
        let diagnostic_train_loaded = read_quasi_static_samples(&diagnostic_train_paths)
            .with_context(|| "failed to read diagnostic train sample artifacts")?;
        let diagnostic_holdout_loaded = read_quasi_static_samples(&diagnostic_holdout_paths)
            .with_context(|| "failed to read diagnostic holdout sample artifacts")?;
        let diagnostic_model = fit_model_from_rows(
            diagnostic_train_loaded.header,
            diagnostic_train_loaded.rows,
            FitOptions {
                ridge_lambda: config.fit.ridge_lambda,
                holdout_ratio: 0.0,
                regularize_bias: false,
            },
        )
        .with_context(|| "diagnostic fit failed")?;
        validate_model_matches_samples(&diagnostic_model, &diagnostic_holdout_loaded.header)?;
        let eval = evaluate_model_on_rows(&diagnostic_model, &diagnostic_holdout_loaded.rows)
            .with_context(|| "diagnostic holdout evaluation failed")?;
        DiagnosticHoldoutMetrics {
            available: true,
            sample_count: Some(eval.sample_count),
            rms_residual_nm: Some(eval.rms_residual_nm),
            p95_residual_nm: Some(eval.p95_residual_nm),
            max_residual_nm: Some(eval.max_residual_nm),
        }
    } else {
        DiagnosticHoldoutMetrics::unavailable()
    };

    let final_model = fit_model_from_rows(
        train_loaded.header.clone(),
        train_loaded.rows.clone(),
        FitOptions {
            ridge_lambda: config.fit.ridge_lambda,
            holdout_ratio: 0.0,
            regularize_bias: false,
        },
    )
    .with_context(|| "final fit failed")?;
    validate_model_matches_samples(&final_model, &train_loaded.header)?;
    validate_model_matches_samples(&final_model, &validation_loaded.header)?;
    let train_eval = evaluate_model_on_rows(&final_model, &train_loaded.rows)
        .with_context(|| "train evaluation failed")?;
    let validation_eval = evaluate_model_on_rows(&final_model, &validation_loaded.rows)
        .with_context(|| "validation evaluation failed")?;
    let report = build_assessment_report(
        &config.gate.strict_v1,
        counts,
        &train_eval,
        &validation_eval,
        &diagnostic_holdout,
        &final_model,
    );

    let model_relative = format!("models/{round_id}.model.toml");
    let report_relative = format!("reports/{round_id}.assess.json");
    let round_relative = format!("rounds/{round_id}.json");
    write_model_create_new(profile_dir, &model_relative, &final_model)
        .with_context(|| format!("failed to write model {model_relative}"))?;
    write_json_create_new(profile_dir, &report_relative, &report)
        .with_context(|| format!("failed to write assessment report {report_relative}"))?;
    let provenance = RoundProvenance::new(RoundProvenanceInput {
        round_id,
        status: if report.decision.pass {
            ProfileStatus::Passed
        } else {
            ProfileStatus::ValidationFailed
        },
        train_sample_artifact_ids,
        validation_sample_artifact_ids,
        validation_path_artifact_ids,
        diagnostic_train_group_keys: &diagnostic_split.train_group_keys,
        diagnostic_holdout_group_keys: &diagnostic_split.holdout_group_keys,
        profile_identity_sha256: &manifest.profile_identity_sha256,
        profile_config_sha256: &manifest.profile_config_sha256,
        gate_config,
        failure: None,
        created_at_unix_ms: unix_ms,
    });
    write_json_create_new(profile_dir, &round_relative, &provenance)
        .with_context(|| format!("failed to write round provenance {round_relative}"))?;

    let model_sha256 = file_sha256(&profile_dir.join(&model_relative))?;
    let report_sha256 = file_sha256(&profile_dir.join(&report_relative))?;
    let round_sha256 = file_sha256(&profile_dir.join(&round_relative))?;
    let status = if report.decision.pass {
        let best_relative = "models/best.model.toml".to_string();
        fs::copy(
            profile_dir.join(&model_relative),
            profile_dir.join(&best_relative),
        )
        .with_context(|| format!("failed to promote best model to {best_relative}"))?;
        ProfileStatus::Passed
    } else {
        ProfileStatus::ValidationFailed
    };
    let current_best_model = if report.decision.pass {
        Some(CurrentBestModel {
            round_id: round_id.to_string(),
            path: "models/best.model.toml".to_string(),
            sha256: file_sha256(&profile_dir.join("models/best.model.toml"))?,
            source_model_path: model_relative.clone(),
            source_model_sha256: model_sha256.clone(),
            promoted_at_unix_ms: unix_ms,
        })
    } else {
        None
    };

    Ok(CompletedFitAssess {
        status,
        current_best_model,
        round_entry: RoundEntry {
            id: round_id.to_string(),
            status,
            model_path: Some(model_relative),
            model_sha256: Some(model_sha256),
            report_path: Some(report_relative),
            report_sha256: Some(report_sha256),
            round_path: Some(round_relative),
            round_sha256: Some(round_sha256),
            train_sample_artifact_ids: train_sample_artifact_ids.to_vec(),
            validation_sample_artifact_ids: validation_sample_artifact_ids.to_vec(),
            validation_path_artifact_ids: validation_path_artifact_ids.to_vec(),
            diagnostic_train_group_keys: diagnostic_split.train_group_keys,
            diagnostic_holdout_group_keys: diagnostic_split.holdout_group_keys,
            profile_identity_sha256: manifest.profile_identity_sha256.clone(),
            profile_config_sha256: manifest.profile_config_sha256.clone(),
            gate_config: gate_config.clone(),
            created_at_unix_ms: unix_ms,
            failure: None,
        },
    })
}

struct FailureRoundInput<'a> {
    profile_dir: &'a Path,
    manifest: &'a Manifest,
    gate: &'a StrictGateConfig,
    holdout_group_key: &'a str,
    holdout_ratio: f64,
    train_artifacts: &'a [ArtifactEntry],
    round_id: &'a str,
    counts: AssessmentCounts,
    train_sample_artifact_ids: &'a [String],
    validation_sample_artifact_ids: &'a [String],
    validation_path_artifact_ids: &'a [String],
    gate_config: &'a Value,
    unix_ms: u64,
    failure: RoundFailure,
}

fn write_failure_round(input: FailureRoundInput<'_>) -> RoundEntry {
    let profile_dir = input.profile_dir;
    let manifest = input.manifest;
    let round_id = input.round_id;
    let failure = input.failure;
    let report_relative = format!("reports/{round_id}.assess.json");
    let round_relative = format!("rounds/{round_id}.json");
    let diagnostic_split = select_diagnostic_holdout_groups(
        &manifest.profile_identity_sha256,
        round_id,
        input.holdout_group_key,
        input.holdout_ratio,
        input.train_artifacts,
    )
    .ok();
    let diagnostic_train_group_keys = diagnostic_split
        .as_ref()
        .map(|split| split.train_group_keys.clone())
        .unwrap_or_default();
    let diagnostic_holdout_group_keys = diagnostic_split
        .as_ref()
        .map(|split| split.holdout_group_keys.clone())
        .unwrap_or_default();
    let report = build_count_only_assessment_report(input.gate, input.counts, &failure.message);
    let report_sha256 = write_json_create_new(profile_dir, &report_relative, &report)
        .ok()
        .and_then(|_| file_sha256(&profile_dir.join(&report_relative)).ok());
    let provenance = RoundProvenance::new(RoundProvenanceInput {
        round_id,
        status: ProfileStatus::FitFailed,
        train_sample_artifact_ids: input.train_sample_artifact_ids,
        validation_sample_artifact_ids: input.validation_sample_artifact_ids,
        validation_path_artifact_ids: input.validation_path_artifact_ids,
        diagnostic_train_group_keys: &diagnostic_train_group_keys,
        diagnostic_holdout_group_keys: &diagnostic_holdout_group_keys,
        profile_identity_sha256: &manifest.profile_identity_sha256,
        profile_config_sha256: &manifest.profile_config_sha256,
        gate_config: input.gate_config,
        failure: Some(&failure),
        created_at_unix_ms: input.unix_ms,
    });
    let round_sha256 = write_json_create_new(profile_dir, &round_relative, &provenance)
        .ok()
        .and_then(|_| file_sha256(&profile_dir.join(&round_relative)).ok());

    RoundEntry {
        id: round_id.to_string(),
        status: ProfileStatus::FitFailed,
        model_path: None,
        model_sha256: None,
        report_path: report_sha256.as_ref().map(|_| report_relative),
        report_sha256,
        round_path: round_sha256.as_ref().map(|_| round_relative),
        round_sha256,
        train_sample_artifact_ids: input.train_sample_artifact_ids.to_vec(),
        validation_sample_artifact_ids: input.validation_sample_artifact_ids.to_vec(),
        validation_path_artifact_ids: input.validation_path_artifact_ids.to_vec(),
        diagnostic_train_group_keys,
        diagnostic_holdout_group_keys,
        profile_identity_sha256: manifest.profile_identity_sha256.clone(),
        profile_config_sha256: manifest.profile_config_sha256.clone(),
        gate_config: input.gate_config.clone(),
        created_at_unix_ms: input.unix_ms,
        failure: Some(failure),
    }
}

fn active_sample_artifacts(manifest: &Manifest, split: Split) -> Vec<ArtifactEntry> {
    manifest
        .artifacts
        .iter()
        .filter(|artifact| artifact.active && artifact.kind == "samples" && artifact.split == split)
        .cloned()
        .collect()
}

fn assessment_counts(manifest: &Manifest) -> AssessmentCounts {
    let counts = active_sample_counts(manifest);
    AssessmentCounts {
        train_samples: counts.train_samples as usize,
        train_waypoints: counts.train_waypoints as usize,
        validation_samples: counts.validation_samples as usize,
        validation_waypoints: counts.validation_waypoints as usize,
    }
}

pub(crate) fn active_sample_paths(
    manifest: &Manifest,
    profile_dir: &Path,
    split: Split,
) -> Vec<PathBuf> {
    active_sample_artifacts(manifest, split)
        .iter()
        .filter_map(|artifact| registered_artifact_path(profile_dir, artifact).ok())
        .collect()
}

pub(crate) fn active_sample_ids(manifest: &Manifest, split: Split) -> Vec<String> {
    manifest
        .artifacts
        .iter()
        .filter(|artifact| artifact.active && artifact.kind == "samples" && artifact.split == split)
        .map(|artifact| artifact.id.clone())
        .collect()
}

pub(crate) fn validation_path_ids_for_sample_ids(
    manifest: &Manifest,
    validation_sample_ids: &[String],
) -> Vec<String> {
    let validation_sample_ids = validation_sample_ids.iter().collect::<BTreeSet<_>>();
    manifest
        .artifacts
        .iter()
        .filter(|artifact| {
            artifact.kind == "samples"
                && artifact.split == Split::Validation
                && validation_sample_ids.contains(&artifact.id)
        })
        .filter_map(|artifact| artifact.source_path_id.clone())
        .collect()
}

fn sample_paths_for_ids(manifest: &Manifest, profile_dir: &Path, ids: &[String]) -> Vec<PathBuf> {
    let ids = ids.iter().collect::<BTreeSet<_>>();
    manifest
        .artifacts
        .iter()
        .filter(|artifact| {
            artifact.active && artifact.kind == "samples" && ids.contains(&artifact.id)
        })
        .filter_map(|artifact| registered_artifact_path(profile_dir, artifact).ok())
        .collect()
}

fn counts_below_gate(counts: &AssessmentCounts, gate: &StrictGateConfig) -> bool {
    counts.train_samples < gate.min_train_samples
        || counts.validation_samples < gate.min_validation_samples
        || counts.train_waypoints < gate.min_train_waypoints
        || counts.validation_waypoints < gate.min_validation_waypoints
}

fn insufficient_data_reason(counts: &AssessmentCounts, gate: &StrictGateConfig) -> String {
    format!(
        "insufficient data: train samples {}/{}, train waypoints {}/{}, validation samples {}/{}, validation waypoints {}/{}",
        counts.train_samples,
        gate.min_train_samples,
        counts.train_waypoints,
        gate.min_train_waypoints,
        counts.validation_samples,
        gate.min_validation_samples,
        counts.validation_waypoints,
        gate.min_validation_waypoints
    )
}

fn write_model_create_new(
    profile_dir: &Path,
    relative_path: &str,
    model: &QuasiStaticTorqueModel,
) -> Result<()> {
    let output_path = profile_dir.join(relative_path);
    validate_profile_generated_output_path(profile_dir, &output_path)?;
    let toml = toml::to_string_pretty(model).context("failed to serialize gravity model")?;
    write_bytes_create_new(&output_path, toml.as_bytes())
}

fn write_json_create_new<T: Serialize>(
    profile_dir: &Path,
    relative_path: &str,
    value: &T,
) -> Result<()> {
    let output_path = profile_dir.join(relative_path);
    validate_profile_generated_output_path(profile_dir, &output_path)?;
    let mut output = serde_json::to_vec_pretty(value).context("failed to serialize JSON")?;
    output.push(b'\n');
    write_bytes_create_new(&output_path, &output)
}

fn write_bytes_create_new(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent().filter(|parent| !parent.as_os_str().is_empty()) {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .with_context(|| format!("failed to create {}", path.display()))?;
    let mut writer = BufWriter::new(file);
    writer
        .write_all(bytes)
        .with_context(|| format!("failed to write {}", path.display()))?;
    writer.flush().with_context(|| format!("failed to flush {}", path.display()))?;
    Ok(())
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
    use crate::gravity::{
        artifact::{PassDirection, QuasiStaticSampleRow, SamplesHeader, write_jsonl_row},
        model::{TRIG_V1_FEATURE_COUNT, trig_v1_features},
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

    #[cfg(unix)]
    #[test]
    fn profile_record_path_rejects_data_directory_symlink_escape() {
        let fixture = ProfileFixture::new();
        let outside_data = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(outside_data.path().join("train/paths")).unwrap();
        std::fs::remove_dir_all(fixture.profile_dir().join("data")).unwrap();
        std::os::unix::fs::symlink(outside_data.path(), fixture.profile_dir().join("data"))
            .unwrap();

        let err = plan_record_path(
            fixture.profile_dir(),
            Split::Train,
            None,
            unix_ms_for_tests(),
        )
        .unwrap_err();

        assert!(err.to_string().contains("outside profile directory"));
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

    #[test]
    fn fit_assess_writes_round_report_and_best_model_when_gate_passes() {
        let fixture = ProfileFixture::new();
        fixture.register_samples_artifact(Split::Train, "samples-train-0001", 320);
        fixture.register_samples_artifact(Split::Validation, "samples-validation-0001", 100);

        fit_assess(crate::commands::gravity::GravityProfilePathArgs {
            profile: fixture.profile_dir().to_path_buf(),
        })
        .unwrap();

        let manifest = Manifest::load(fixture.profile_dir().join("manifest.json")).unwrap();
        assert_eq!(manifest.status, ProfileStatus::Passed);
        assert_eq!(manifest.rounds.len(), 1);
        let round = manifest.rounds.first().unwrap();
        assert_eq!(round.id, "round-0001");
        assert_eq!(round.status, ProfileStatus::Passed);
        assert_eq!(round.train_sample_artifact_ids, ["samples-train-0001"]);
        assert_eq!(
            round.validation_sample_artifact_ids,
            ["samples-validation-0001"]
        );
        assert!(round.model_path.is_some());
        assert!(round.model_sha256.is_some());
        assert!(round.report_path.is_some());
        assert!(round.report_sha256.is_some());
        assert!(round.round_path.is_some());
        assert!(round.round_sha256.is_some());
        assert!(round.failure.is_none());
        assert!(fixture.profile_dir().join("models/round-0001.model.toml").exists());
        assert!(fixture.profile_dir().join("reports/round-0001.assess.json").exists());
        assert!(fixture.profile_dir().join("rounds/round-0001.json").exists());

        let best = manifest.current_best_model.as_ref().unwrap();
        assert_eq!(best.round_id, "round-0001");
        assert_eq!(best.path, "models/best.model.toml");
        assert_eq!(
            std::fs::read(fixture.profile_dir().join(&best.path)).unwrap(),
            std::fs::read(fixture.profile_dir().join("models/round-0001.model.toml")).unwrap()
        );
    }

    #[test]
    fn fit_assess_sets_insufficient_data_without_model_promotion() {
        let fixture = ProfileFixture::new();
        fixture.register_samples_artifact(Split::Train, "samples-train-0001", 20);
        fixture.register_samples_artifact(Split::Validation, "samples-validation-0001", 10);

        fit_assess(crate::commands::gravity::GravityProfilePathArgs {
            profile: fixture.profile_dir().to_path_buf(),
        })
        .unwrap();

        let manifest = Manifest::load(fixture.profile_dir().join("manifest.json")).unwrap();
        assert_eq!(manifest.status, ProfileStatus::InsufficientData);
        assert_eq!(manifest.rounds.len(), 1);
        let round = manifest.rounds.first().unwrap();
        assert_eq!(round.status, ProfileStatus::InsufficientData);
        assert_eq!(round.model_path, None);
        assert_eq!(round.model_sha256, None);
        assert!(round.report_path.is_some());
        assert!(round.round_path.is_some());
        assert!(manifest.current_best_model.is_none());
        assert!(!fixture.profile_dir().join("models/round-0001.model.toml").exists());
    }

    #[test]
    fn fit_assess_sets_fit_failed_when_solver_or_model_write_fails_after_inputs_pass_count_gate() {
        let fixture = ProfileFixture::new();
        fixture.register_samples_artifact(Split::Train, "samples-train-0001", 320);
        fixture.register_samples_artifact(Split::Validation, "samples-validation-0001", 100);
        std::fs::write(
            fixture.profile_dir().join("models/round-0001.model.toml"),
            "existing",
        )
        .unwrap();

        let err = fit_assess(crate::commands::gravity::GravityProfilePathArgs {
            profile: fixture.profile_dir().to_path_buf(),
        })
        .unwrap_err();

        let message = format!("{err:#}");
        assert!(
            message.contains("model") || message.contains("fit"),
            "{message}"
        );
        let manifest = Manifest::load(fixture.profile_dir().join("manifest.json")).unwrap();
        assert_eq!(manifest.status, ProfileStatus::FitFailed);
        assert_eq!(manifest.rounds.len(), 1);
        let round = manifest.rounds.first().unwrap();
        assert_eq!(round.status, ProfileStatus::FitFailed);
        assert!(round.failure.is_some());
        assert!(round.model_path.is_none());
        assert!(manifest.current_best_model.is_none());
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

        fn register_samples_artifact(&self, split: Split, artifact_id: &str, sample_count: usize) {
            let split_dir = split_dir_for_tests(split);
            let relative_path = format!("data/{split_dir}/samples/{artifact_id}.samples.jsonl");
            let samples_path = self.profile_dir.join(&relative_path);
            std::fs::create_dir_all(samples_path.parent().unwrap()).unwrap();
            write_samples_artifact_for_tests(&samples_path, sample_count);

            let mut manifest = Manifest::load(self.profile_dir.join("manifest.json")).unwrap();
            manifest.artifacts.push(ArtifactEntry {
                id: artifact_id.to_string(),
                kind: "samples".to_string(),
                split,
                active: true,
                path: relative_path,
                sha256: crate::gravity::profile::artifacts::file_sha256(&samples_path).unwrap(),
                source_path_id: Some(format!("source-{artifact_id}")),
                role: "slave".to_string(),
                arm_id: "piper-left".to_string(),
                arm_id_source: Some("profile_generated".to_string()),
                target: "socketcan:can1".to_string(),
                joint_map: "identity".to_string(),
                load_profile: "normal-gripper-d405".to_string(),
                torque_convention: crate::gravity::TORQUE_CONVENTION.to_string(),
                basis: crate::gravity::BASIS_TRIG_V1.to_string(),
                sample_count: Some(sample_count as u64),
                waypoint_count: Some(sample_count as u64),
                created_at_unix_ms: unix_ms_for_tests(),
                promoted_from_round_id: None,
                previous_paths: Vec::new(),
            });
            manifest.status = match split {
                Split::Train => ProfileStatus::NeedsValidationData,
                Split::Validation => ProfileStatus::ReadyToFit,
            };
            manifest.save_atomic(self.profile_dir.join("manifest.json")).unwrap();
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

    fn write_samples_artifact_for_tests(path: &std::path::Path, sample_count: usize) {
        let mut file = std::fs::File::create(path).unwrap();
        let header = SamplesHeader {
            row_type: "header".to_string(),
            artifact_kind: "quasi-static-samples".to_string(),
            schema_version: 1,
            source_path: "synthetic.path.jsonl".to_string(),
            source_sha256: "source-sha256".to_string(),
            role: "slave".to_string(),
            arm_id: Some("piper-left".to_string()),
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
            waypoint_count: sample_count,
            accepted_waypoint_count: sample_count,
            rejected_waypoint_count: 0,
        };
        write_jsonl_row(&mut file, &header).unwrap();
        for sample_index in 0..sample_count {
            write_jsonl_row(&mut file, &synthetic_sample_row_for_tests(sample_index)).unwrap();
        }
    }

    fn synthetic_sample_row_for_tests(sample_index: usize) -> QuasiStaticSampleRow {
        let q_rad = synthetic_q_for_tests(sample_index);
        let features = trig_v1_features(q_rad);
        let mut tau_nm = [0.0; 6];
        for joint in 0..6 {
            tau_nm[joint] = features[joint % TRIG_V1_FEATURE_COUNT] * 0.0;
        }
        QuasiStaticSampleRow {
            row_type: "quasi-static-sample".to_string(),
            waypoint_id: sample_index as u64,
            segment_id: Some(format!("segment-{}", sample_index / 20)),
            pass_direction: PassDirection::Forward,
            host_mono_us: sample_index as u64 * 10_000,
            raw_timestamp_us: None,
            q_rad,
            dq_rad_s: [0.0; 6],
            tau_nm,
            position_valid_mask: 0x3f,
            dynamic_valid_mask: 0x3f,
            stable_velocity_rad_s: 0.0,
            stable_tracking_error_rad: 0.0,
            stable_torque_std_nm: 0.0,
        }
    }

    fn synthetic_q_for_tests(sample_index: usize) -> [f64; 6] {
        let i = sample_index as f64;
        [
            ((i * 0.017) + (i * 0.003).sin()).sin() * 1.2,
            ((i * 0.023) + 0.4).cos() * 1.1,
            ((i * 0.031) + (i * 0.007).cos()).sin(),
            ((i * 0.037) + 0.8).cos() * 0.9,
            ((i * 0.041) + (i * 0.011).sin()).sin() * 1.3,
            ((i * 0.047) + 1.2).cos(),
        ]
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
