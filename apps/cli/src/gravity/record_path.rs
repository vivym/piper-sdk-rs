use crate::commands::gravity::GravityRecordPathArgs;
use crate::connection::{client_builder, wait_for_initial_monitor_snapshot};
use crate::gravity::artifact::{PathHeader, PathSampleRow, write_jsonl_row};
use anyhow::{Context, Result, anyhow, bail};
use piper_client::observer::Observer;
use piper_client::state::CapabilityMarker;
use piper_client::{ConnectedPiper, MotionConnectedState};
use piper_control::TargetSpec;
use std::fs::OpenOptions;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};
use tokio::task::spawn_blocking;

const FLUSH_EVERY_SAMPLES: u64 = 50;
const CANCEL_POLL_INTERVAL: Duration = Duration::from_millis(100);

#[derive(Debug, Clone)]
struct PathRecordingStats {
    output_path: PathBuf,
    sample_count: u64,
}

pub async fn run(args: GravityRecordPathArgs) -> Result<()> {
    let target_spec = resolve_record_path_target(&args)?;
    let sample_period = sample_period_from_frequency_hz(args.frequency_hz)?;
    ensure_output_path_available(&args.out)?;

    let running = Arc::new(AtomicBool::new(true));
    let signal_running = Arc::clone(&running);
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            println!();
            println!("Ctrl-C received; finishing path recording...");
            signal_running.store(false, Ordering::SeqCst);
        }
    });

    println!("Connecting to robot...");
    println!("target: {target_spec}");
    println!("output: {}", args.out.display());
    println!("frequency: {:.3} Hz", args.frequency_hz);
    println!("Press Ctrl-C to stop recording.");
    println!();

    let task = spawn_blocking(move || record_path_sync(args, target_spec, sample_period, running));
    let stats = task.await.map_err(|error| anyhow!("record-path task failed: {error}"))??;

    println!();
    println!(
        "Saved passive gravity path: {} ({} samples)",
        stats.output_path.display(),
        stats.sample_count
    );

    Ok(())
}

fn record_path_sync(
    args: GravityRecordPathArgs,
    target_spec: TargetSpec,
    sample_period: Duration,
    running: Arc<AtomicBool>,
) -> Result<PathRecordingStats> {
    let target = target_spec.clone().into_connection_target();
    let connected = client_builder(&target)
        .build()
        .context("failed to connect to robot for passive path recording")?;

    let sample_count = match connected {
        ConnectedPiper::Strict(MotionConnectedState::Standby(standby)) => record_with_observer(
            standby.observer(),
            &args,
            &target_spec,
            sample_period,
            running,
        )?,
        ConnectedPiper::Soft(MotionConnectedState::Standby(standby)) => record_with_observer(
            standby.observer(),
            &args,
            &target_spec,
            sample_period,
            running,
        )?,
        ConnectedPiper::Monitor(standby) => record_with_observer(
            standby.observer(),
            &args,
            &target_spec,
            sample_period,
            running,
        )?,
        ConnectedPiper::Strict(MotionConnectedState::Maintenance(_))
        | ConnectedPiper::Soft(MotionConnectedState::Maintenance(_)) => {
            bail!("robot is not in confirmed Standby; run stop before passive path recording")
        },
    };

    Ok(PathRecordingStats {
        output_path: args.out,
        sample_count,
    })
}

fn record_with_observer<Capability>(
    observer: &Observer<Capability>,
    args: &GravityRecordPathArgs,
    target_spec: &TargetSpec,
    sample_period: Duration,
    running: Arc<AtomicBool>,
) -> Result<u64>
where
    Capability: CapabilityMarker,
{
    wait_for_initial_monitor_snapshot(|| observer.joint_positions())
        .context("timed out waiting for initial joint position feedback")?;
    wait_for_initial_monitor_snapshot(|| observer.joint_velocities())
        .context("timed out waiting for initial joint dynamic feedback")?;

    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&args.out)
        .with_context(|| format!("failed to create {}", args.out.display()))?;
    let mut writer = BufWriter::new(file);

    let target = target_spec.to_string();
    let header = build_path_header(
        args.role.as_str(),
        target,
        args.joint_map.as_str(),
        args.load_profile.as_str(),
        args.notes.clone(),
    );
    write_jsonl_row(&mut writer, &header)?;
    writer.flush()?;

    let sample_count = record_samples(observer, &mut writer, sample_period, running)?;
    writer.flush()?;

    Ok(sample_count)
}

fn record_samples<Capability, W>(
    observer: &Observer<Capability>,
    writer: &mut W,
    sample_period: Duration,
    running: Arc<AtomicBool>,
) -> Result<u64>
where
    Capability: CapabilityMarker,
    W: Write,
{
    let mut sample_count = 0_u64;
    let mut next_sample_at = Instant::now();

    while running.load(Ordering::SeqCst) {
        sleep_until_or_cancelled(next_sample_at, &running);
        if !running.load(Ordering::SeqCst) {
            break;
        }

        let row = sample_observer_state(observer, sample_count);
        write_jsonl_row(writer, &row)?;
        sample_count += 1;

        if sample_count.is_multiple_of(FLUSH_EVERY_SAMPLES) {
            writer.flush()?;
            print!("\rrecorded {sample_count} path samples");
            std::io::stdout().flush()?;
        }

        let now = Instant::now();
        next_sample_at =
            next_sample_at.checked_add(sample_period).unwrap_or_else(|| now + sample_period);
        if next_sample_at <= now {
            next_sample_at = now + sample_period;
        }
    }

    Ok(sample_count)
}

fn sleep_until_or_cancelled(deadline: Instant, running: &Arc<AtomicBool>) {
    while running.load(Ordering::SeqCst) {
        let now = Instant::now();
        if now >= deadline {
            return;
        }
        std::thread::sleep((deadline - now).min(CANCEL_POLL_INTERVAL));
    }
}

fn sample_observer_state<Capability>(
    observer: &Observer<Capability>,
    sample_index: u64,
) -> PathSampleRow
where
    Capability: CapabilityMarker,
{
    let position = observer.raw_joint_position_state();
    let dynamic = observer.raw_joint_dynamic_state();
    let latest_raw_timestamp_us = position.hardware_timestamp_us.max(dynamic.group_timestamp_us);
    let raw_timestamp_us = (latest_raw_timestamp_us != 0).then_some(latest_raw_timestamp_us);

    PathSampleRow {
        row_type: "path-sample".to_string(),
        sample_index,
        host_mono_us: position.host_rx_mono_us.max(dynamic.group_host_rx_mono_us),
        raw_timestamp_us,
        q_rad: position.joint_pos,
        dq_rad_s: dynamic.joint_vel,
        tau_nm: dynamic.get_all_torques(),
        position_valid_mask: position.frame_valid_mask,
        dynamic_valid_mask: dynamic.valid_mask,
        segment_id: None,
    }
}

fn build_path_header(
    role: impl Into<String>,
    target: impl Into<String>,
    joint_map: impl Into<String>,
    load_profile: impl Into<String>,
    notes: Option<String>,
) -> PathHeader {
    PathHeader::new(role, target, joint_map, load_profile, notes)
}

fn resolve_record_path_target(args: &GravityRecordPathArgs) -> Result<TargetSpec> {
    match (args.target.as_deref(), args.interface.as_deref()) {
        (Some(_), Some(_)) => bail!("use either --target or --interface, not both"),
        (Some(target), None) => target
            .parse::<TargetSpec>()
            .map_err(|error| anyhow!("invalid --target {target:?}: {error}")),
        (None, Some(interface)) => {
            if interface.trim().is_empty() {
                bail!("--interface must not be empty");
            }
            Ok(TargetSpec::SocketCan {
                iface: interface.to_string(),
            })
        },
        (None, None) => bail!("pass --target or --interface for passive path recording"),
    }
}

fn sample_period_from_frequency_hz(frequency_hz: f64) -> Result<Duration> {
    if !frequency_hz.is_finite() || frequency_hz <= 0.0 {
        bail!("frequency_hz must be finite and greater than zero");
    }

    let period = Duration::from_secs_f64(1.0 / frequency_hz);
    if period.is_zero() {
        bail!("frequency_hz is too high for the system timer");
    }
    Ok(period)
}

fn ensure_output_path_available(path: &Path) -> Result<()> {
    if path.exists() {
        bail!(
            "output file already exists: {}; refusing to overwrite",
            path.display()
        );
    }
    if let Some(parent) = path.parent().filter(|parent| !parent.as_os_str().is_empty())
        && !parent.exists()
    {
        bail!(
            "output parent directory does not exist: {}",
            parent.display()
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use tempfile::tempdir;

    fn args_with_target(target: Option<&str>, interface: Option<&str>) -> GravityRecordPathArgs {
        GravityRecordPathArgs {
            role: "slave".to_string(),
            target: target.map(str::to_string),
            interface: interface.map(str::to_string),
            joint_map: "identity".to_string(),
            load_profile: "normal-gripper-d405".to_string(),
            out: PathBuf::from("path.jsonl"),
            frequency_hz: 50.0,
            notes: None,
        }
    }

    #[test]
    fn record_path_header_contains_torque_convention_and_metadata() {
        let header = build_path_header(
            "slave",
            "socketcan:can0",
            "identity",
            "normal-gripper-d405",
            Some("operator note".to_string()),
        );

        assert_eq!(header.artifact_kind, "path");
        assert_eq!(header.row_type, "header");
        assert_eq!(header.role, "slave");
        assert_eq!(header.target, "socketcan:can0");
        assert_eq!(header.joint_map, "identity");
        assert_eq!(header.load_profile, "normal-gripper-d405");
        assert_eq!(header.torque_convention, crate::gravity::TORQUE_CONVENTION);
        assert_eq!(header.notes.as_deref(), Some("operator note"));
    }

    #[test]
    fn resolve_target_from_interface_uses_socketcan_prefix() {
        let args = args_with_target(None, Some("can0"));

        let target = resolve_record_path_target(&args).unwrap();

        assert_eq!(target.to_string(), "socketcan:can0");
    }

    #[test]
    fn resolve_target_rejects_target_and_interface_together() {
        let args = args_with_target(Some("socketcan:can0"), Some("can1"));

        let err = resolve_record_path_target(&args).unwrap_err();

        assert!(err.to_string().contains("target"));
        assert!(err.to_string().contains("interface"));
    }

    #[test]
    fn sample_period_rejects_non_positive_frequency() {
        let err = sample_period_from_frequency_hz(0.0).unwrap_err();

        assert!(err.to_string().contains("frequency"));
    }

    #[test]
    fn output_path_existing_is_refused_before_recording() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("path.jsonl");
        std::fs::write(&path, "existing").unwrap();

        let err = ensure_output_path_available(&path).unwrap_err();

        assert!(err.to_string().contains("refusing to overwrite"));
    }
}
