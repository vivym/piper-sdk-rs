use crate::teleop::config::TeleopMode;
use crate::teleop::controller::RuntimeTeleopSettingsHandle;
use anyhow::{Context, Result, bail, ensure};
use std::io::{self, BufRead};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::thread::JoinHandle;
use std::time::Instant;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GainName {
    TrackKp,
    TrackKd,
    MasterDamping,
    ReflectionGain,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ConsoleCommand {
    Status,
    SetMode(TeleopMode),
    SetGain { name: GainName, value: f64 },
    Quit,
    Help,
}

impl ConsoleCommand {
    pub fn parse(input: &str) -> Result<Self> {
        let mut parts = input.split_whitespace();
        let command = parts.next().context("empty console command")?;

        match command {
            "status" => {
                ensure_no_extra_args(parts, "status")?;
                Ok(Self::Status)
            },
            "help" | "?" => {
                ensure_no_extra_args(parts, "help")?;
                Ok(Self::Help)
            },
            "quit" | "exit" => {
                ensure_no_extra_args(parts, "quit")?;
                Ok(Self::Quit)
            },
            "mode" => parse_mode_command(parts),
            "gain" => parse_gain_command(parts),
            other => bail!("unknown console command '{other}'"),
        }
    }
}

pub fn apply_console_command(
    command: ConsoleCommand,
    settings_handle: &RuntimeTeleopSettingsHandle,
    cancel_signal: &Arc<AtomicBool>,
    started_at: Instant,
) -> Result<()> {
    match command {
        ConsoleCommand::Status => {
            print_status(settings_handle, started_at);
            Ok(())
        },
        ConsoleCommand::Help => {
            print_help();
            Ok(())
        },
        ConsoleCommand::Quit => {
            cancel_signal.store(true, Ordering::SeqCst);
            eprintln!("teleop console: cancellation requested");
            Ok(())
        },
        ConsoleCommand::SetMode(mode) => {
            settings_handle.update_mode(mode)?;
            eprintln!("teleop console: mode set to {}", mode_name(mode));
            Ok(())
        },
        ConsoleCommand::SetGain { name, value } => {
            match name {
                GainName::TrackKp => {
                    settings_handle.update_track_kp(value)?;
                },
                GainName::TrackKd => {
                    settings_handle.update_track_kd(value)?;
                },
                GainName::MasterDamping => settings_handle.update_master_damping(value)?,
                GainName::ReflectionGain => settings_handle.update_reflection_gain(value)?,
            }
            eprintln!("teleop console: {} set to {value}", gain_name(name));
            Ok(())
        },
    }
}

#[allow(dead_code)]
pub fn spawn_console_thread(
    settings_handle: RuntimeTeleopSettingsHandle,
    started_at: Instant,
    cancel_signal: Arc<AtomicBool>,
) -> JoinHandle<Result<()>> {
    std::thread::spawn(move || {
        let stdin = io::stdin();
        let result = run_console_reader(stdin.lock(), settings_handle, started_at, cancel_signal);
        if let Err(error) = &result {
            eprintln!("teleop console stopped: {error:#}");
        }
        result
    })
}

pub(crate) fn run_console_reader<R: BufRead>(
    mut reader: R,
    settings_handle: RuntimeTeleopSettingsHandle,
    started_at: Instant,
    cancel_signal: Arc<AtomicBool>,
) -> Result<()> {
    let mut line = String::new();
    while !cancel_signal.load(Ordering::SeqCst) {
        line.clear();
        let bytes_read =
            reader.read_line(&mut line).context("failed to read teleop console input")?;
        if bytes_read == 0 {
            break;
        }

        match ConsoleCommand::parse(&line) {
            Ok(command) => {
                if let Err(error) =
                    apply_console_command(command, &settings_handle, &cancel_signal, started_at)
                {
                    eprintln!("teleop console: {error:#}");
                }
            },
            Err(error) => eprintln!("teleop console: {error:#}"),
        }
    }

    Ok(())
}

fn parse_mode_command<'a>(mut parts: impl Iterator<Item = &'a str>) -> Result<ConsoleCommand> {
    let mode = match parts.next().context("mode command requires a mode")? {
        "master-follower" => TeleopMode::MasterFollower,
        "bilateral" => TeleopMode::Bilateral,
        other => bail!("unknown teleop mode '{other}'"),
    };
    ensure_no_extra_args(parts, "mode")?;
    Ok(ConsoleCommand::SetMode(mode))
}

fn parse_gain_command<'a>(mut parts: impl Iterator<Item = &'a str>) -> Result<ConsoleCommand> {
    let name = match parts.next().context("gain command requires a gain name")? {
        "track-kp" => GainName::TrackKp,
        "track-kd" => GainName::TrackKd,
        "master-damping" => GainName::MasterDamping,
        "reflection-gain" => GainName::ReflectionGain,
        other => bail!("unknown gain name '{other}'"),
    };
    let raw_value = parts.next().context("gain command requires a value")?;
    ensure_no_extra_args(parts, "gain")?;

    let value = raw_value
        .parse::<f64>()
        .with_context(|| format!("invalid gain value '{raw_value}'"))?;
    ensure!(value.is_finite(), "gain value must be finite");

    Ok(ConsoleCommand::SetGain { name, value })
}

fn ensure_no_extra_args<'a>(mut parts: impl Iterator<Item = &'a str>, command: &str) -> Result<()> {
    if let Some(extra) = parts.next() {
        bail!("{command} command received unexpected argument '{extra}'");
    }
    Ok(())
}

fn print_status(settings_handle: &RuntimeTeleopSettingsHandle, started_at: Instant) {
    let settings = settings_handle.snapshot();
    eprintln!(
        "teleop status: uptime={:.1}s mode={} track-kp={} track-kd={} master-damping={} reflection-gain={}",
        started_at.elapsed().as_secs_f64(),
        mode_name(settings.mode),
        settings.track_kp,
        settings.track_kd,
        settings.master_damping,
        settings.reflection_gain
    );
}

fn print_help() {
    eprintln!(
        "teleop commands: status | help | ? | quit | exit | mode <master-follower|bilateral> | gain <track-kp|track-kd|master-damping|reflection-gain> <value>"
    );
}

fn mode_name(mode: TeleopMode) -> &'static str {
    match mode {
        TeleopMode::MasterFollower => "master-follower",
        TeleopMode::Bilateral => "bilateral",
    }
}

fn gain_name(name: GainName) -> &'static str {
    match name {
        GainName::TrackKp => "track-kp",
        GainName::TrackKd => "track-kd",
        GainName::MasterDamping => "master-damping",
        GainName::ReflectionGain => "reflection-gain",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::teleop::controller::{RuntimeTeleopSettings, RuntimeTeleopSettingsHandle};
    use piper_client::dual_arm::{DualArmCalibration, JointMirrorMap};
    use piper_client::types::{Joint, JointArray, Rad};
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };
    use std::time::Instant;

    #[test]
    fn parses_mode_command() {
        assert_eq!(
            ConsoleCommand::parse("mode bilateral").unwrap(),
            ConsoleCommand::SetMode(TeleopMode::Bilateral)
        );
    }

    #[test]
    fn parses_gain_command() {
        assert_eq!(
            ConsoleCommand::parse("gain track-kp 9.5").unwrap(),
            ConsoleCommand::SetGain {
                name: GainName::TrackKp,
                value: 9.5
            }
        );
    }

    #[test]
    fn invalid_gain_value_is_rejected() {
        assert!(ConsoleCommand::parse("gain track-kp NaN").is_err());
        assert!(ConsoleCommand::parse("gain track-kp inf").is_err());
    }

    #[test]
    fn parses_control_commands() {
        assert_eq!(
            ConsoleCommand::parse("status").unwrap(),
            ConsoleCommand::Status
        );
        assert_eq!(ConsoleCommand::parse("help").unwrap(), ConsoleCommand::Help);
        assert_eq!(ConsoleCommand::parse("?").unwrap(), ConsoleCommand::Help);
        assert_eq!(ConsoleCommand::parse("quit").unwrap(), ConsoleCommand::Quit);
        assert_eq!(ConsoleCommand::parse("exit").unwrap(), ConsoleCommand::Quit);
    }

    #[test]
    fn rejects_unknown_empty_missing_and_extra_args() {
        assert!(ConsoleCommand::parse("").is_err());
        assert!(ConsoleCommand::parse("   ").is_err());
        assert!(ConsoleCommand::parse("mode").is_err());
        assert!(ConsoleCommand::parse("mode bilateral now").is_err());
        assert!(ConsoleCommand::parse("mode invalid").is_err());
        assert!(ConsoleCommand::parse("gain track-kp").is_err());
        assert!(ConsoleCommand::parse("gain track-kp 9.5 now").is_err());
        assert!(ConsoleCommand::parse("gain unknown 9.5").is_err());
        assert!(ConsoleCommand::parse("unknown").is_err());
    }

    #[test]
    fn quit_sets_cancel_signal() {
        let handle = sample_settings_handle();
        let cancel_signal = Arc::new(AtomicBool::new(false));

        apply_console_command(
            ConsoleCommand::Quit,
            &handle,
            &cancel_signal,
            Instant::now(),
        )
        .unwrap();

        assert!(cancel_signal.load(Ordering::SeqCst));
    }

    #[test]
    fn status_and_help_do_not_mutate_settings_or_cancel() {
        let handle = sample_settings_handle();
        let cancel_signal = Arc::new(AtomicBool::new(false));
        let before = handle.snapshot();

        apply_console_command(
            ConsoleCommand::Status,
            &handle,
            &cancel_signal,
            Instant::now(),
        )
        .unwrap();
        apply_console_command(
            ConsoleCommand::Help,
            &handle,
            &cancel_signal,
            Instant::now(),
        )
        .unwrap();

        assert_eq!(handle.snapshot(), before);
        assert!(!cancel_signal.load(Ordering::SeqCst));
    }

    #[test]
    fn gain_updates_preserve_the_other_track_gain() {
        let handle = sample_settings_handle();
        let cancel_signal = Arc::new(AtomicBool::new(false));

        apply_console_command(
            ConsoleCommand::SetGain {
                name: GainName::TrackKp,
                value: 9.5,
            },
            &handle,
            &cancel_signal,
            Instant::now(),
        )
        .unwrap();
        assert_eq!(handle.snapshot().track_kp, 9.5);
        assert_eq!(handle.snapshot().track_kd, 1.0);

        apply_console_command(
            ConsoleCommand::SetGain {
                name: GainName::TrackKd,
                value: 1.2,
            },
            &handle,
            &cancel_signal,
            Instant::now(),
        )
        .unwrap();
        assert_eq!(handle.snapshot().track_kp, 9.5);
        assert_eq!(handle.snapshot().track_kd, 1.2);
    }

    #[test]
    fn mode_and_dedicated_gain_commands_update_settings() {
        let handle = sample_settings_handle();
        let cancel_signal = Arc::new(AtomicBool::new(false));

        apply_console_command(
            ConsoleCommand::SetMode(TeleopMode::Bilateral),
            &handle,
            &cancel_signal,
            Instant::now(),
        )
        .unwrap();
        apply_console_command(
            ConsoleCommand::SetGain {
                name: GainName::MasterDamping,
                value: 0.8,
            },
            &handle,
            &cancel_signal,
            Instant::now(),
        )
        .unwrap();
        apply_console_command(
            ConsoleCommand::SetGain {
                name: GainName::ReflectionGain,
                value: 0.3,
            },
            &handle,
            &cancel_signal,
            Instant::now(),
        )
        .unwrap();

        let snapshot = handle.snapshot();
        assert_eq!(snapshot.mode, TeleopMode::Bilateral);
        assert_eq!(snapshot.master_damping, 0.8);
        assert_eq!(snapshot.reflection_gain, 0.3);
    }

    #[test]
    fn run_console_reader_applies_lines_and_stops_after_quit() {
        let handle = sample_settings_handle();
        let cancel_signal = Arc::new(AtomicBool::new(false));
        let input = b"mode bilateral\ngain track-kp 9.5\nquit\ngain track-kd 1.2\n";

        run_console_reader(
            &input[..],
            handle.clone(),
            Instant::now(),
            cancel_signal.clone(),
        )
        .unwrap();

        let snapshot = handle.snapshot();
        assert_eq!(snapshot.mode, TeleopMode::Bilateral);
        assert_eq!(snapshot.track_kp, 9.5);
        assert_eq!(snapshot.track_kd, 1.0);
        assert!(cancel_signal.load(Ordering::SeqCst));
    }

    #[test]
    fn run_console_reader_continues_after_apply_error_until_quit() {
        let handle = sample_settings_handle();
        let cancel_signal = Arc::new(AtomicBool::new(false));
        let input = b"gain track-kp 21\nquit\n";

        run_console_reader(
            &input[..],
            handle.clone(),
            Instant::now(),
            cancel_signal.clone(),
        )
        .unwrap();

        let snapshot = handle.snapshot();
        assert_eq!(snapshot.track_kp, 8.0);
        assert!(cancel_signal.load(Ordering::SeqCst));
    }

    #[test]
    fn spawn_console_thread_exposes_reader_result_in_join_handle() {
        let _: fn(
            RuntimeTeleopSettingsHandle,
            Instant,
            Arc<AtomicBool>,
        ) -> std::thread::JoinHandle<anyhow::Result<()>> = spawn_console_thread;
    }

    fn sample_settings_handle() -> RuntimeTeleopSettingsHandle {
        RuntimeTeleopSettingsHandle::new(RuntimeTeleopSettings::production(sample_calibration()))
            .unwrap()
    }

    fn sample_calibration() -> DualArmCalibration {
        DualArmCalibration {
            master_zero: JointArray::splat(Rad(0.0)),
            slave_zero: JointArray::splat(Rad(0.0)),
            map: JointMirrorMap {
                permutation: [
                    Joint::J1,
                    Joint::J2,
                    Joint::J3,
                    Joint::J4,
                    Joint::J5,
                    Joint::J6,
                ],
                position_sign: [1.0; 6],
                velocity_sign: [1.0; 6],
                torque_sign: [1.0; 6],
            },
        }
    }
}
