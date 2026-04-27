# Isomorphic Dual-Arm Teleop CLI Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build `piper-cli teleop dual-arm`, a production-oriented isomorphic dual-arm teleoperation host for two StrictRealtime SocketCAN Piper arms.

**Architecture:** Keep realtime command generation in `piper-client::dual_arm`; the CLI adds production orchestration around it. Add focused `apps/cli/src/teleop/*` modules for target/config/calibration/controller/console/report/workflow boundaries, with a fakeable workflow backend so safety startup paths are tested without hardware. First harden SDK MIT enable failure cleanup so the CLI can rely on safe enable semantics.

**Tech Stack:** Rust 2024, clap, serde/toml/serde_json, anyhow/thiserror, ctrlc, piper-client dual-arm API, piper-control `TargetSpec`, TDD via cargo unit/integration tests.

---

## Execution Status

As of 2026-04-27:

- [x] Task 1: Harden SDK MIT Enable Failure Cleanup.
- [x] Task 2: Add Teleop Command Skeleton.
- [x] Task 3: Implement Teleop Target Parsing and Config Resolution.
- [x] Task 4: Implement Calibration File and Posture Compatibility.
- [x] Task 5: Implement Runtime Teleop Controller.
- [x] Task 6: Implement Runtime Console Parser.
- [x] Task 7: Implement Report and Exit Classification.
- [x] Task 8: Implement Fakeable Workflow Orchestration.
- [x] Task 9: Add Real Dual-Arm Backend and Command Execution.
- [x] Task 10: Add Operator Documentation.
- [x] Task 11: Final Verification.

Task 9 review loop is complete. The final targeted Ctrl+C idempotency fix is commit
`edac468` (`fix: preserve first teleop ctrlc signal`), approved by reviewer with
`gpt-5.5` and `xhigh` reasoning.

Task 10 review loop is complete. Documentation commits through `02bcdd6`
(`docs: clarify teleop stop attempt semantics`) are approved by reviewers with
`gpt-5.5` and `xhigh` reasoning.

Task 11 final verification is complete. The final verification pass ran
`cargo fmt --all -- --check`, `cargo test -p piper-cli --all-targets`,
`cargo test -p piper-client enable_mit_mode_timeout_after_enable_dispatch_sends_disable_all -- --nocapture`,
`cargo test --workspace --all-targets --all-features`,
`cargo clippy --workspace --all-targets --all-features -- -D warnings`, and
`cargo run -p piper-cli -- teleop dual-arm --help`.

---

## Scope Check

This plan implements the reviewed v1 spec:

- Runtime support is StrictRealtime SocketCAN only.
- GS-USB concrete target syntax is parsed/documented but rejected for runtime with an explicit SDK SoftRealtime dual-arm prerequisite error.
- The CLI does not reimplement MIT frame emission.
- Default tests must not require hardware.

Out of scope:

- SoftRealtime / GS-USB dual-arm runtime support.
- Cartesian teleoperation.
- MuJoCo compensation integration.
- Network-distributed teleoperation.

## File Structure

Create focused modules under `apps/cli/src/teleop`:

- `apps/cli/src/teleop/mod.rs`: public module wiring and shared re-exports.
- `apps/cli/src/teleop/target.rs`: concrete master/slave target parsing, duplicate detection, runtime support rejection.
- `apps/cli/src/teleop/config.rs`: CLI/config merge, hard safety limits, profile-to-loop config mapping.
- `apps/cli/src/teleop/calibration.rs`: calibration TOML v1 load/save/validation and posture compatibility math.
- `apps/cli/src/teleop/controller.rs`: `RuntimeTeleopController` and shared runtime settings handle.
- `apps/cli/src/teleop/console.rs`: small stdin console parser and command application.
- `apps/cli/src/teleop/report.rs`: human/JSON report v1 and exit-code classification.
- `apps/cli/src/teleop/workflow.rs`: fakeable workflow orchestration and real backend adapter boundary.

Modify existing files:

- `crates/piper-client/src/state/machine.rs`: add MIT enable cleanup guard and tests.
- `apps/cli/Cargo.toml`: add `ctrlc` dependency and `tempfile` dev dependency.
- `apps/cli/src/main.rs`: add `Teleop` command dispatch.
- `apps/cli/src/commands/mod.rs`: export teleop command.
- `apps/cli/src/commands/teleop.rs`: clap shape for `teleop dual-arm`.
- `apps/cli/README.md`: link the operator guide.

Create documentation:

- `apps/cli/TELEOP_DUAL_ARM.md`: operator bring-up guide and HIL acceptance checklist.

## Task 1: Harden SDK MIT Enable Failure Cleanup

**Files:**
- Modify: `crates/piper-client/src/state/machine.rs`
- Test: `crates/piper-client/src/dual_arm.rs`
- Test: `crates/piper-client/src/state/machine.rs`

This is a prerequisite. The CLI spec requires cleanup even when an enable or MIT-mode command may have been dispatched but confirmation fails before an `Active<MitMode>` value exists.

- [ ] **Step 1: Write failing test for enable confirmation timeout cleanup**

Add a test near existing state-machine drop/enable tests:

```rust
#[test]
fn enable_mit_mode_timeout_after_enable_dispatch_sends_disable_all() {
    use piper_protocol::control::MotorEnableCommand;

    let sent = Arc::new(Mutex::new(Vec::new()));
    let standby = build_standby_piper(IdleRxAdapter::new(), sent.clone());
    let config = MitModeConfig {
        timeout: Duration::from_millis(1),
        poll_interval: Duration::from_millis(1),
        ..MitModeConfig::default()
    };

    let err = standby
        .enable_mit_mode(config)
        .expect_err("enable confirmation timeout must fail");

    assert!(matches!(err, RobotError::Timeout { .. }));
    let frames = wait_for_sent_frames(&sent, 2);
    assert!(
        frames
            .iter()
            .any(|frame| *frame == MotorEnableCommand::disable_all().to_frame()),
        "failed MIT enable after dispatch must send disable_all"
    );
}
```

If current helpers require a scripted RX adapter instead of `IdleRxAdapter`, use the existing state-machine test helpers in the same module. The key assertion is that `disable_all` is sent after enable dispatch has occurred.

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p piper-client enable_mit_mode_timeout_after_enable_dispatch_sends_disable_all -- --nocapture`

Expected: FAIL because current `enable_mit_mode` can return after confirmation failure without sending disable.

- [ ] **Step 3: Implement an internal cleanup guard**

Add an internal helper in `crates/piper-client/src/state/machine.rs`:

```rust
struct EnableCleanupGuard<'a> {
    piper: &'a piper_driver::Piper,
    armed: bool,
}

impl<'a> EnableCleanupGuard<'a> {
    fn armed(piper: &'a piper_driver::Piper) -> Self {
        Self { piper, armed: true }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for EnableCleanupGuard<'_> {
    fn drop(&mut self) {
        if self.armed {
            self.piper.best_effort_disable_or_shutdown_on_drop(EMERGENCY_STOP_LANE_TIMEOUT);
        }
    }
}
```

Then arm it immediately after an enable command is successfully dispatched in `Piper<Standby, Capability>::enable_mit_mode`:

```rust
let enable_commit_host_mono_us = self
    .driver
    .send_reliable_frame_confirmed_commit_marker(enable_cmd.to_frame(), config.timeout)?;
let mut enable_cleanup = EnableCleanupGuard::armed(&self.driver);

self.wait_for_enabled(...)?;
...
self.wait_for_mode_confirmation(...)?;
enable_cleanup.disarm();
drop(enable_cleanup);
Ok(transition_piper_state(...))
```

Do not broaden this to position mode in this task unless tests prove the same bug affects code needed by this feature. Keep the change minimal.

- [ ] **Step 4: Run targeted tests**

Run: `cargo test -p piper-client enable_mit_mode_timeout_after_enable_dispatch_sends_disable_all -- --nocapture`

Expected: PASS.

Run: `cargo test -p piper-client active_drop_sends_disable_all_but_standby_replay_error_and_monitor_do_not -- --nocapture`

Expected: PASS.

- [ ] **Step 5: Write dual-arm partial-enable regression tests**

In `crates/piper-client/src/dual_arm.rs`, add tests near existing dual-arm enable/run tests:

```rust
#[test]
fn dual_arm_enable_mit_right_failure_drops_left_active_and_sends_disable() {
    let left_sent = Arc::new(Mutex::new(Vec::new()));
    let right_sent = Arc::new(Mutex::new(Vec::new()));
    let left = build_standby_piper(1_000, left_sent.clone(), Duration::from_millis(20));
    let right = build_standby_piper_with_tx_adapter(
        1_000,
        FailingModeConfirmTxAdapter::new(right_sent.clone()),
        Duration::from_millis(20),
        Standby,
    );
    let arms = DualArmStandby { left, right };

    let err = arms
        .enable_mit(MitModeConfig::default(), MitModeConfig::default())
        .expect_err("right-arm enable failure should fail the dual-arm enable");

    assert!(err.to_string().contains("timeout") || err.to_string().contains("confirm"));
    let left_frames = wait_for_sent_frames(&left_sent, 1);
    assert!(
        left_frames
            .iter()
            .any(|frame| *frame == MotorEnableCommand::disable_all().to_frame()),
        "dropping left active arm after right failure must disable left"
    );
}

#[test]
fn dual_arm_enable_mit_left_confirmation_failure_sends_disable() {
    let left_sent = Arc::new(Mutex::new(Vec::new()));
    let right_sent = Arc::new(Mutex::new(Vec::new()));
    let left = build_standby_piper_with_tx_adapter(
        1_000,
        FailingEnableConfirmTxAdapter::new(left_sent.clone()),
        Duration::from_millis(20),
        Standby,
    );
    let right = build_standby_piper(1_000, right_sent.clone(), Duration::from_millis(20));
    let arms = DualArmStandby { left, right };

    let _ = arms
        .enable_mit(MitModeConfig::default(), MitModeConfig::default())
        .expect_err("left enable confirmation failure should fail");

    let left_frames = wait_for_sent_frames(&left_sent, 2);
    assert!(
        left_frames
            .iter()
            .any(|frame| *frame == MotorEnableCommand::disable_all().to_frame()),
        "left confirmation failure after enable dispatch must disable left"
    );
    assert!(
        right_sent.lock().expect("right sent frames lock").is_empty(),
        "right arm must not be enabled after left enable failure"
    );
}
```

Use or add focused fake TX adapters in the existing dual-arm test module. The important contract is SDK-level proof, not CLI-level fake cleanup.

- [ ] **Step 6: Run dual-arm cleanup tests**

Run: `cargo test -p piper-client dual_arm_enable_mit_ -- --nocapture`

Expected: PASS after the state-machine guard; if not, fix `DualArmStandby::enable_mit` without changing the public API.

- [ ] **Step 7: Commit**

```bash
git add crates/piper-client/src/state/machine.rs crates/piper-client/src/dual_arm.rs
git commit -m "fix: disable after failed mit enable confirmation"
```

## Task 2: Add Teleop Command Skeleton

**Files:**
- Create: `apps/cli/src/commands/teleop.rs`
- Create: `apps/cli/src/teleop/mod.rs`
- Modify: `apps/cli/src/commands/mod.rs`
- Modify: `apps/cli/src/main.rs`
- Modify: `apps/cli/Cargo.toml`
- Test: `apps/cli/src/commands/teleop.rs`

- [ ] **Step 1: Write failing clap parse tests**

Create `apps/cli/src/commands/teleop.rs` with tests first:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::teleop::TeleopDualArmArgs;
    use crate::teleop::config::{TeleopArmsConfig, TeleopConfigFile, TeleopRoleTargetConfig};
    use clap::Parser;

    #[test]
    fn dual_arm_command_parses_socketcan_targets() {
        let cmd = TeleopCommand::try_parse_from([
            "teleop",
            "dual-arm",
            "--master-interface",
            "can0",
            "--slave-interface",
            "can1",
            "--mode",
            "master-follower",
        ])
        .expect("teleop dual-arm command should parse");

        match cmd.action {
            TeleopAction::DualArm(args) => {
                assert_eq!(args.master_interface.as_deref(), Some("can0"));
                assert_eq!(args.slave_interface.as_deref(), Some("can1"));
                assert_eq!(args.mode, Some(crate::teleop::config::TeleopMode::MasterFollower));
            },
        }
    }

    #[test]
    fn dual_arm_command_parses_canonical_targets() {
        let cmd = TeleopCommand::try_parse_from([
            "teleop",
            "dual-arm",
            "--master-target",
            "socketcan:can0",
            "--slave-target",
            "socketcan:can1",
        ])
        .expect("canonical targets should parse");

        match cmd.action {
            TeleopAction::DualArm(args) => {
                assert_eq!(args.master_target.as_deref(), Some("socketcan:can0"));
                assert_eq!(args.slave_target.as_deref(), Some("socketcan:can1"));
            },
        }
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p piper-cli teleop -- --nocapture`

Expected: FAIL because `TeleopCommand` and `crate::teleop` do not exist.

- [ ] **Step 3: Implement skeleton types and routing**

Add to `apps/cli/Cargo.toml`:

```toml
ctrlc = { workspace = true }

[dev-dependencies]
tempfile = "3.24"
```

Create `apps/cli/src/teleop/mod.rs`:

```rust
pub mod calibration;
pub mod config;
pub mod console;
pub mod controller;
pub mod report;
pub mod target;
pub mod workflow;
```

Create empty `calibration.rs`, `console.rs`, `controller.rs`, `report.rs`, `target.rs`, and `workflow.rs` files. Create `apps/cli/src/teleop/config.rs` with the minimum clap enums used by the command skeleton:

```rust
use clap::ValueEnum;

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum TeleopMode {
    MasterFollower,
    Bilateral,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum TeleopProfile {
    Production,
    Debug,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum TeleopTimingMode {
    Sleep,
    Spin,
}
```

Task 3 expands this skeleton with serde, defaults, validation, and config-file resolution.

Create `apps/cli/src/commands/teleop.rs`:

```rust
use anyhow::Result;
use clap::{Args, Parser, Subcommand};
use std::path::PathBuf;

#[derive(Debug, Parser)]
#[command(name = "teleop")]
pub struct TeleopCommand {
    #[command(subcommand)]
    pub action: TeleopAction,
}

#[derive(Debug, Subcommand)]
pub enum TeleopAction {
    DualArm(TeleopDualArmArgs),
}

#[derive(Debug, Args, Clone)]
pub struct TeleopDualArmArgs {
    #[arg(long)]
    pub config: Option<PathBuf>,
    #[arg(long)]
    pub master_target: Option<String>,
    #[arg(long)]
    pub slave_target: Option<String>,
    #[arg(long)]
    pub master_interface: Option<String>,
    #[arg(long)]
    pub slave_interface: Option<String>,
    #[arg(long)]
    pub master_serial: Option<String>,
    #[arg(long)]
    pub slave_serial: Option<String>,
    #[arg(long)]
    pub master_gs_usb_bus_address: Option<String>,
    #[arg(long)]
    pub slave_gs_usb_bus_address: Option<String>,
    #[arg(long, default_value_t = 1_000_000)]
    pub baud_rate: u32,
    #[arg(long, value_enum)]
    pub mode: Option<crate::teleop::config::TeleopMode>,
    #[arg(long, value_enum)]
    pub profile: Option<crate::teleop::config::TeleopProfile>,
    #[arg(long)]
    pub frequency_hz: Option<f64>,
    #[arg(long)]
    pub track_kp: Option<f64>,
    #[arg(long)]
    pub track_kd: Option<f64>,
    #[arg(long)]
    pub master_damping: Option<f64>,
    #[arg(long)]
    pub reflection_gain: Option<f64>,
    #[arg(long)]
    pub disable_gripper_mirror: bool,
    #[arg(long)]
    pub calibration_file: Option<PathBuf>,
    #[arg(long)]
    pub calibration_max_error_rad: Option<f64>,
    #[arg(long)]
    pub save_calibration: Option<PathBuf>,
    #[arg(long)]
    pub report_json: Option<PathBuf>,
    #[arg(long)]
    pub yes: bool,
    #[arg(long)]
    pub max_iterations: Option<usize>,
    #[arg(long, value_enum)]
    pub timing_mode: Option<crate::teleop::config::TeleopTimingMode>,
}

impl TeleopCommand {
    pub async fn execute(self) -> Result<()> {
        match self.action {
            TeleopAction::DualArm(args) => crate::teleop::workflow::run_dual_arm(args).await,
        }
    }
}
```

Modify `apps/cli/src/main.rs`:

```rust
mod teleop;
...
use commands::{TeleopAction, TeleopCommand};
...
Teleop {
    #[command(subcommand)]
    action: TeleopAction,
},
...
Commands::Teleop { action } => TeleopCommand { action }.execute().await,
```

Modify `apps/cli/src/commands/mod.rs`:

```rust
pub mod teleop;
pub use teleop::{TeleopAction, TeleopCommand};
```

- [ ] **Step 4: Run skeleton tests**

Run: `cargo test -p piper-cli teleop -- --nocapture`

Expected: PASS for parse tests.

- [ ] **Step 5: Commit**

```bash
git add apps/cli/Cargo.toml apps/cli/src/main.rs apps/cli/src/commands/mod.rs apps/cli/src/commands/teleop.rs apps/cli/src/teleop
git commit -m "feat: add teleop command skeleton"
```

## Task 3: Implement Teleop Target Parsing and Config Resolution

**Files:**
- Modify: `apps/cli/src/teleop/target.rs`
- Modify: `apps/cli/src/teleop/config.rs`
- Modify: `apps/cli/src/commands/teleop.rs`
- Test: `apps/cli/src/teleop/target.rs`
- Test: `apps/cli/src/teleop/config.rs`

- [ ] **Step 1: Write failing target tests**

In `apps/cli/src/teleop/target.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn concrete_socketcan_target_parses() {
        assert_eq!(
            ConcreteTeleopTarget::parse("socketcan:can0").unwrap(),
            ConcreteTeleopTarget::SocketCan { iface: "can0".to_string() }
        );
    }

    #[test]
    fn auto_targets_are_rejected() {
        assert!(ConcreteTeleopTarget::parse("auto-strict").is_err());
        assert!(ConcreteTeleopTarget::parse("auto-any").is_err());
        assert!(ConcreteTeleopTarget::parse("gs-usb-auto").is_err());
    }

    #[test]
    fn duplicate_socketcan_targets_are_rejected() {
        let targets = RoleTargets {
            master: ConcreteTeleopTarget::SocketCan { iface: "can0".to_string() },
            slave: ConcreteTeleopTarget::SocketCan { iface: "can0".to_string() },
        };

        assert!(targets.validate_no_duplicates().is_err());
    }

    #[test]
    fn gs_usb_runtime_is_rejected_for_v1() {
        let target = ConcreteTeleopTarget::GsUsbSerial { serial: "A".to_string() };
        assert!(target.ensure_v1_runtime_supported(TeleopPlatform::Linux).is_err());
    }

    #[test]
    fn socketcan_runtime_is_rejected_on_non_linux() {
        let target = ConcreteTeleopTarget::SocketCan { iface: "can0".to_string() };
        let err = target
            .ensure_v1_runtime_supported(TeleopPlatform::Other)
            .expect_err("SocketCAN runtime is Linux-only in v1");

        assert!(err.to_string().contains("Linux"));
    }

    #[test]
    fn role_targets_cli_overrides_config_targets() {
        let file = TeleopConfigFile {
            arms: Some(TeleopArmsConfig {
                master: Some(TeleopRoleTargetConfig {
                    target: Some("socketcan:can4".to_string()),
                }),
                slave: Some(TeleopRoleTargetConfig {
                    target: Some("socketcan:can5".to_string()),
                }),
            }),
            ..Default::default()
        };
        let args = TeleopDualArmArgs {
            master_interface: Some("can0".to_string()),
            slave_interface: Some("can1".to_string()),
            ..TeleopDualArmArgs::default_for_tests()
        };

        let targets = resolve_role_targets(&args, Some(&file), TeleopPlatform::Linux).unwrap();

        assert_eq!(
            targets.master,
            ConcreteTeleopTarget::SocketCan { iface: "can0".to_string() }
        );
        assert_eq!(
            targets.slave,
            ConcreteTeleopTarget::SocketCan { iface: "can1".to_string() }
        );
    }

    #[test]
    fn role_targets_default_to_can0_can1_on_linux() {
        let args = TeleopDualArmArgs::default_for_tests();

        let targets = resolve_role_targets(&args, None, TeleopPlatform::Linux).unwrap();

        assert_eq!(
            targets.master,
            ConcreteTeleopTarget::SocketCan { iface: "can0".to_string() }
        );
        assert_eq!(
            targets.slave,
            ConcreteTeleopTarget::SocketCan { iface: "can1".to_string() }
        );
    }

    #[test]
    fn role_targets_missing_on_non_linux_fails() {
        let args = TeleopDualArmArgs::default_for_tests();

        let err = resolve_role_targets(&args, None, TeleopPlatform::Other)
            .expect_err("non-Linux requires explicit concrete targets");

        assert!(err.to_string().contains("master"));
        assert!(err.to_string().contains("slave"));
    }

    #[test]
    fn role_targets_reject_multiple_selectors_for_same_role() {
        let args = TeleopDualArmArgs {
            master_target: Some("socketcan:can0".to_string()),
            master_interface: Some("can1".to_string()),
            slave_interface: Some("can2".to_string()),
            ..TeleopDualArmArgs::default_for_tests()
        };

        assert!(resolve_role_targets(&args, None, TeleopPlatform::Linux).is_err());
    }

    #[test]
    fn role_targets_reject_mixed_cli_selector_conflict_before_config_merge() {
        let file = TeleopConfigFile {
            arms: Some(TeleopArmsConfig {
                master: Some(TeleopRoleTargetConfig {
                    target: Some("socketcan:can4".to_string()),
                }),
                slave: Some(TeleopRoleTargetConfig {
                    target: Some("socketcan:can5".to_string()),
                }),
            }),
            ..Default::default()
        };
        let args = TeleopDualArmArgs {
            master_serial: Some("A".to_string()),
            master_gs_usb_bus_address: Some("1:2".to_string()),
            slave_interface: Some("can1".to_string()),
            ..TeleopDualArmArgs::default_for_tests()
        };

        let err = resolve_role_targets(&args, Some(&file), TeleopPlatform::Linux)
            .expect_err("multiple CLI selectors for one role must fail even if config exists");

        assert!(err.to_string().contains("master"));
    }

    #[test]
    fn config_file_target_fields_are_strings() {
        let file: TeleopConfigFile = toml::from_str(
            r#"
            [arms.master]
            target = "socketcan:can0"

            [arms.slave]
            target = "socketcan:can1"
            "#,
        )
        .unwrap();

        let arms = file.arms.expect("arms section");
        assert_eq!(
            arms.master.as_ref().and_then(|role| role.target.as_deref()),
            Some("socketcan:can0")
        );
        assert_eq!(
            arms.slave.as_ref().and_then(|role| role.target.as_deref()),
            Some("socketcan:can1")
        );
    }
}
```

- [ ] **Step 2: Write failing config merge tests**

In `apps/cli/src/teleop/config.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::teleop::TeleopDualArmArgs;

    #[test]
    fn cli_values_override_file_values() {
        let file = TeleopConfigFile {
            control: Some(TeleopControlConfig {
                mode: Some(TeleopMode::Bilateral),
                frequency_hz: Some(100.0),
                ..Default::default()
            }),
            ..Default::default()
        };
        let args = TeleopDualArmArgs {
            mode: Some(TeleopMode::MasterFollower),
            frequency_hz: Some(200.0),
            ..TeleopDualArmArgs::default_for_tests()
        };

        let resolved = ResolvedTeleopConfig::resolve(args, Some(file)).unwrap();

        assert_eq!(resolved.control.mode, TeleopMode::MasterFollower);
        assert_eq!(resolved.control.frequency_hz, 200.0);
    }

    #[test]
    fn hard_limits_reject_unsafe_gains() {
        let err = TeleopControlSettings {
            track_kp: 21.0,
            ..TeleopControlSettings::default()
        }
        .validate()
        .expect_err("track_kp above hard cap must fail");

        assert!(err.to_string().contains("track_kp"));
    }

    #[test]
    fn default_mode_is_master_follower() {
        let resolved =
            ResolvedTeleopConfig::resolve(TeleopDualArmArgs::default_for_tests(), None).unwrap();

        assert_eq!(resolved.control.mode, TeleopMode::MasterFollower);
    }
}
```

Add a test-only helper constructor in `TeleopDualArmArgs` if needed:

```rust
#[cfg(test)]
impl TeleopDualArmArgs {
    pub fn default_for_tests() -> Self {
        Self { /* all Option fields None, flags false, baud_rate 1_000_000 */ }
    }
}
```

- [ ] **Step 3: Run tests to verify they fail**

Run: `cargo test -p piper-cli teleop::target -- --nocapture`

Expected: FAIL because target and target-resolution types are not implemented.

Run: `cargo test -p piper-cli teleop::config -- --nocapture`

Expected: FAIL because config types are not implemented.

- [ ] **Step 4: Implement target parsing**

In `apps/cli/src/teleop/target.rs`:

```rust
use anyhow::{Result, bail};
use crate::commands::teleop::TeleopDualArmArgs;
use crate::teleop::config::TeleopConfigFile;
use piper_control::TargetSpec;
use piper_sdk::driver::ConnectionTarget;
use std::str::FromStr;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConcreteTeleopTarget {
    SocketCan { iface: String },
    GsUsbSerial { serial: String },
    GsUsbBusAddress { bus: u8, address: u8 },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoleTargets {
    pub master: ConcreteTeleopTarget,
    pub slave: ConcreteTeleopTarget,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TeleopPlatform {
    Linux,
    Other,
}

impl TeleopPlatform {
    pub fn current() -> Self {
        if cfg!(target_os = "linux") {
            Self::Linux
        } else {
            Self::Other
        }
    }
}

impl ConcreteTeleopTarget {
    pub fn parse(value: &str) -> Result<Self> {
        match TargetSpec::from_str(value).map_err(anyhow::Error::msg)? {
            TargetSpec::SocketCan { iface } => Ok(Self::SocketCan { iface }),
            TargetSpec::GsUsbSerial { serial } => Ok(Self::GsUsbSerial { serial }),
            TargetSpec::GsUsbBusAddress { bus, address } => Ok(Self::GsUsbBusAddress { bus, address }),
            TargetSpec::AutoStrict | TargetSpec::AutoAny | TargetSpec::GsUsbAuto => {
                bail!("dual-arm teleop requires concrete targets; got {value}")
            },
        }
    }

    pub fn ensure_v1_runtime_supported(&self, platform: TeleopPlatform) -> Result<()> {
        match self {
            Self::SocketCan { .. } => match platform {
                TeleopPlatform::Linux => Ok(()),
                TeleopPlatform::Other => bail!("SocketCAN dual-arm teleop requires Linux in v1"),
            },
            Self::GsUsbSerial { .. } | Self::GsUsbBusAddress { .. } => {
                bail!("GS-USB dual-arm teleop requires future SDK SoftRealtime dual-arm support")
            },
        }
    }

    pub fn to_connection_target(&self) -> ConnectionTarget {
        match self {
            Self::SocketCan { iface } => ConnectionTarget::SocketCan { iface: iface.clone() },
            Self::GsUsbSerial { serial } => ConnectionTarget::GsUsbSerial { serial: serial.clone() },
            Self::GsUsbBusAddress { bus, address } => ConnectionTarget::GsUsbBusAddress {
                bus: *bus,
                address: *address,
            },
        }
    }
}
```

Implement `RoleTargets::validate_no_duplicates()` and `RoleTargets::ensure_v1_runtime_supported(platform)`. Reject mixed GS-USB selector kinds if both roles are GS-USB and not normalized. Runtime support validation must reject SocketCAN on non-Linux before attempting connection.

Implement target resolution in the same file:

```rust
pub fn resolve_role_targets(
    args: &TeleopDualArmArgs,
    file: Option<&TeleopConfigFile>,
    platform: TeleopPlatform,
) -> Result<RoleTargets> {
    let master = resolve_one_role(Role::Master, args, file, platform)?;
    let slave = resolve_one_role(Role::Slave, args, file, platform)?;
    let targets = RoleTargets { master, slave };
    targets.validate_no_duplicates()?;
    Ok(targets)
}
```

Resolution rules are intentionally strict:

- For each role, CLI may provide exactly one selector: `--*-target`, `--*-interface`, `--*-serial`, or `--*-gs-usb-bus-address`.
- If a CLI selector is present for a role, it overrides that role's config-file target.
- If no CLI selector exists, use `[arms.master].target` / `[arms.slave].target` string values from TOML.
- If no CLI or config selector exists on Linux, default to `socketcan:can0` for master and `socketcan:can1` for slave.
- If no selector exists on non-Linux, fail before connecting and name both missing roles in the error.
- Multiple CLI selectors for one role are always an error, even when a config-file target exists.
- Mixed CLI/config roles are allowed only when each role resolves to exactly one concrete target.

- [ ] **Step 5: Implement config types and hard limits**

In `apps/cli/src/teleop/config.rs`, add:

```rust
use anyhow::{Result, bail};
use clap::ValueEnum;
use serde::{Deserialize, Serialize};

pub const DEFAULT_FREQUENCY_HZ: f64 = 200.0;
pub const MAX_CALIBRATION_ERROR_RAD: f64 = 0.05;

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TeleopMode {
    MasterFollower,
    Bilateral,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TeleopProfile {
    Production,
    Debug,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum TeleopTimingMode {
    Sleep,
    Spin,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct TeleopConfigFile {
    pub arms: Option<TeleopArmsConfig>,
    pub control: Option<TeleopControlConfig>,
    pub safety: Option<TeleopSafetyConfig>,
    pub calibration: Option<TeleopCalibrationConfig>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct TeleopArmsConfig {
    pub master: Option<TeleopRoleTargetConfig>,
    pub slave: Option<TeleopRoleTargetConfig>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct TeleopRoleTargetConfig {
    pub target: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TeleopControlSettings {
    pub mode: TeleopMode,
    pub frequency_hz: f64,
    pub track_kp: f64,
    pub track_kd: f64,
    pub master_damping: f64,
    pub reflection_gain: f64,
}
```

Implement:

- `TeleopConfigFile::load(path: &Path) -> Result<Self>`
- `ResolvedTeleopConfig::resolve(args, file) -> Result<Self>`
- `TeleopControlSettings::validate() -> Result<()>`
- `ResolvedTeleopConfig::loop_config(cancel_signal) -> BilateralLoopConfig`

Use hard caps from the spec exactly:

- `10.0 <= frequency_hz <= 500.0`
- `0.0 <= track_kp <= 20.0`
- `0.0 <= track_kd <= 5.0`
- `0.0 <= master_damping <= 2.0`
- `0.0 <= reflection_gain <= 0.5`
- `0.0 < calibration_max_error_rad <= 0.05`

- [ ] **Step 6: Run tests**

Run: `cargo test -p piper-cli teleop::target -- --nocapture`

Expected: PASS.

Run: `cargo test -p piper-cli teleop::config -- --nocapture`

Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add apps/cli/src/teleop/target.rs apps/cli/src/teleop/config.rs apps/cli/src/commands/teleop.rs
git commit -m "feat: add teleop config and target validation"
```

## Task 4: Implement Calibration File and Posture Compatibility

**Files:**
- Modify: `apps/cli/src/teleop/calibration.rs`
- Test: `apps/cli/src/teleop/calibration.rs`

- [ ] **Step 1: Write failing calibration tests**

Add tests:

```rust
#[test]
fn calibration_file_round_trips() {
    let file = CalibrationFile::from_calibration(
        &sample_calibration(),
        Some("bench A".to_string()),
        1_770_000_000_000,
    );
    let toml = toml::to_string(&file).unwrap();
    let decoded: CalibrationFile = toml::from_str(&toml).unwrap();

    assert_eq!(decoded.version, 1);
    assert_eq!(decoded.to_calibration().unwrap(), sample_calibration());
}

#[test]
fn calibration_rejects_invalid_signs() {
    let mut file = CalibrationFile::from_calibration(&sample_calibration(), None, 1);
    file.map.position_sign[0] = 0.0;
    assert!(file.validate().is_err());
}

#[test]
fn compatibility_check_detects_slave_mismatch() {
    let calibration = sample_calibration();
    let master = JointArray::splat(Rad(0.0));
    let slave = JointArray::splat(Rad(1.0));

    let err = check_posture_compatibility(&calibration, master, slave, 0.05)
        .expect_err("mismatch should fail");

    assert!(err.max_error_rad > 0.05);
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p piper-cli teleop::calibration -- --nocapture`

Expected: FAIL because calibration types are not implemented.

- [ ] **Step 3: Implement calibration TOML v1**

In `apps/cli/src/teleop/calibration.rs`:

```rust
use anyhow::{Result, bail};
use piper_client::dual_arm::{DualArmCalibration, JointMirrorMap};
use piper_client::types::{Joint, JointArray, Rad};
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CalibrationFile {
    pub version: u8,
    pub created_at_unix_ms: u64,
    pub note: Option<String>,
    pub map: MirrorMapFile,
    pub zero: CalibrationZeroFile,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MirrorMapFile {
    pub permutation: [String; 6],
    pub position_sign: [f64; 6],
    pub velocity_sign: [f64; 6],
    pub torque_sign: [f64; 6],
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CalibrationZeroFile {
    pub master: [f64; 6],
    pub slave: [f64; 6],
}
```

Implement:

- `CalibrationFile::load(path: &Path) -> Result<Self>`
- `CalibrationFile::save_new(&self, path: &Path) -> Result<()>` that writes this calibration file and fails if `path` already exists
- `CalibrationFile::validate() -> Result<()>`
- `CalibrationFile::to_calibration() -> Result<DualArmCalibration>`
- `CalibrationFile::from_calibration(...) -> Self`
- `check_posture_compatibility(calibration, master, slave, max_error_rad) -> Result<(), CompatibilityError>`

Use joint names `J1` through `J6`. Convert to `Joint::ALL` indices.

- [ ] **Step 4: Run calibration tests**

Run: `cargo test -p piper-cli teleop::calibration -- --nocapture`

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add apps/cli/src/teleop/calibration.rs
git commit -m "feat: add teleop calibration files"
```

## Task 5: Implement Runtime Teleop Controller

**Files:**
- Modify: `apps/cli/src/teleop/controller.rs`
- Test: `apps/cli/src/teleop/controller.rs`

- [ ] **Step 1: Write failing controller equivalence tests**

Use public `DualArmSnapshot` fields to build sample snapshots:

```rust
fn snapshot(
    master_position: JointArray<Rad>,
    master_velocity: JointArray<RadPerSecond>,
    slave_torque: JointArray<NewtonMeter>,
) -> DualArmSnapshot {
    DualArmSnapshot {
        left: ControlSnapshotFull {
            state: ControlSnapshot {
                position: master_position,
                velocity: master_velocity,
                torque: JointArray::splat(NewtonMeter::ZERO),
                position_timestamp_us: 1,
                dynamic_timestamp_us: 1,
                skew_us: 0,
            },
            position_host_rx_mono_us: 1,
            dynamic_host_rx_mono_us: 1,
            feedback_age: Duration::from_millis(1),
        },
        right: ControlSnapshotFull {
            state: ControlSnapshot {
                position: JointArray::splat(Rad(0.0)),
                velocity: JointArray::splat(RadPerSecond(0.0)),
                torque: slave_torque,
                position_timestamp_us: 1,
                dynamic_timestamp_us: 1,
                skew_us: 0,
            },
            position_host_rx_mono_us: 1,
            dynamic_host_rx_mono_us: 1,
            feedback_age: Duration::from_millis(1),
        },
        inter_arm_skew: Duration::ZERO,
        host_cycle_timestamp: Instant::now(),
    }
}
```

Tests:

```rust
#[test]
fn runtime_controller_matches_master_follower_controller() {
    let calibration = sample_calibration();
    let settings = RuntimeTeleopSettings::production(calibration.clone())
        .with_mode(TeleopMode::MasterFollower)
        .with_track_gains(8.0, 1.0)
        .with_master_damping(0.4);
    let handle = RuntimeTeleopSettingsHandle::new(settings);
    let mut runtime = RuntimeTeleopController::new(handle);
    let mut reference = MasterFollowerController::new(calibration)
        .with_track_gains(JointArray::splat(8.0), JointArray::splat(1.0))
        .with_master_damping(JointArray::splat(0.4));
    let snapshot = sample_snapshot();

    assert_eq!(
        runtime.tick(&snapshot, Duration::from_millis(5)).unwrap(),
        reference.tick(&snapshot, Duration::from_millis(5)).unwrap()
    );
}

#[test]
fn mode_update_affects_next_tick() {
    let handle = RuntimeTeleopSettingsHandle::new(RuntimeTeleopSettings::production(sample_calibration()));
    let mut controller = RuntimeTeleopController::new(handle.clone());

    handle.update_mode(TeleopMode::Bilateral).unwrap();
    let command = controller.tick(&sample_snapshot(), Duration::from_millis(5)).unwrap();

    assert!(command.master_interaction_torque.iter().any(|tau| tau.0 != 0.0));
}

#[test]
fn runtime_controller_matches_joint_space_bilateral_controller() {
    let calibration = sample_calibration();
    let settings = RuntimeTeleopSettings::production(calibration.clone())
        .with_mode(TeleopMode::Bilateral)
        .with_track_gains(8.0, 1.0)
        .with_master_damping(0.4)
        .with_reflection_gain(0.25);
    let handle = RuntimeTeleopSettingsHandle::new(settings);
    let mut runtime = RuntimeTeleopController::new(handle);
    let mut reference = JointSpaceBilateralController::new(calibration)
        .with_track_gains(JointArray::splat(8.0), JointArray::splat(1.0))
        .with_master_damping(JointArray::splat(0.4))
        .with_reflection_gain(JointArray::splat(0.25));
    let snapshot = sample_snapshot();

    assert_eq!(
        runtime.tick(&snapshot, Duration::from_millis(5)).unwrap(),
        reference.tick(&snapshot, Duration::from_millis(5)).unwrap()
    );
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p piper-cli teleop::controller -- --nocapture`

Expected: FAIL because controller types are not implemented.

- [ ] **Step 3: Implement controller and settings handle**

In `apps/cli/src/teleop/controller.rs`:

```rust
use crate::teleop::config::TeleopMode;
use anyhow::{Result, bail};
use piper_client::dual_arm::*;
use piper_client::types::*;
use std::convert::Infallible;
use std::sync::{Arc, RwLock};
use std::time::Duration;

#[derive(Debug, Clone)]
pub struct RuntimeTeleopSettings {
    pub calibration: DualArmCalibration,
    pub mode: TeleopMode,
    pub track_kp: f64,
    pub track_kd: f64,
    pub master_damping: f64,
    pub reflection_gain: f64,
}

#[derive(Debug, Clone)]
pub struct RuntimeTeleopSettingsHandle {
    inner: Arc<RwLock<RuntimeTeleopSettings>>,
}

pub struct RuntimeTeleopController {
    settings: RuntimeTeleopSettingsHandle,
}
```

Implement `BilateralController` by constructing equivalent `BilateralCommand` directly from the current settings. Do not call existing controllers from inside `tick`; copy the already-reviewed formulas from `MasterFollowerController` and `JointSpaceBilateralController` so runtime updates are cheap.

Runtime update methods:

- `update_mode`
- `update_gain`
- `snapshot`

All updates must validate hard caps through `TeleopControlSettings::validate`.

- [ ] **Step 4: Run controller tests**

Run: `cargo test -p piper-cli teleop::controller -- --nocapture`

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add apps/cli/src/teleop/controller.rs
git commit -m "feat: add runtime teleop controller"
```

## Task 6: Implement Runtime Console Parser

**Files:**
- Modify: `apps/cli/src/teleop/console.rs`
- Test: `apps/cli/src/teleop/console.rs`

- [ ] **Step 1: Write failing parser tests**

```rust
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
        ConsoleCommand::SetGain { name: GainName::TrackKp, value: 9.5 }
    );
}

#[test]
fn invalid_gain_value_is_rejected() {
    assert!(ConsoleCommand::parse("gain track-kp NaN").is_err());
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p piper-cli teleop::console -- --nocapture`

Expected: FAIL because console parser does not exist.

- [ ] **Step 3: Implement parser and input thread**

In `apps/cli/src/teleop/console.rs`:

```rust
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
```

Implement:

- `ConsoleCommand::parse(&str) -> Result<Self>`
- `apply_console_command(command, settings_handle, cancel_signal, started_at) -> Result<()>`
- `spawn_console_thread(...) -> JoinHandle<()>`

The console thread reads stdin line by line. It must never call raw motor APIs.

- [ ] **Step 4: Run console tests**

Run: `cargo test -p piper-cli teleop::console -- --nocapture`

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add apps/cli/src/teleop/console.rs
git commit -m "feat: add teleop runtime console"
```

## Task 7: Implement Report and Exit Classification

**Files:**
- Modify: `apps/cli/src/teleop/report.rs`
- Test: `apps/cli/src/teleop/report.rs`

- [ ] **Step 1: Write failing report tests**

```rust
#[test]
fn cancelled_report_is_success() {
    let report = BilateralRunReport {
        exit_reason: Some(BilateralExitReason::Cancelled),
        ..BilateralRunReport::default()
    };

    assert_eq!(classify_exit(false, &report), TeleopExitStatus::Success);
}

#[test]
fn standby_read_fault_is_failure() {
    let report = BilateralRunReport {
        exit_reason: Some(BilateralExitReason::ReadFault),
        ..BilateralRunReport::default()
    };

    assert_eq!(classify_exit(false, &report), TeleopExitStatus::Failure);
}

#[test]
fn json_report_uses_master_slave_names_and_us_units() {
    let json = serde_json::to_value(sample_json_report()).unwrap();

    assert!(json["metrics"]["max_inter_arm_skew_us"].is_number());
    assert!(json["metrics"].get("left_tx_frames_sent_total").is_none());
    assert!(json["metrics"]["master_tx_frames_sent_total"].is_number());
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p piper-cli teleop::report -- --nocapture`

Expected: FAIL because report types are not implemented.

- [ ] **Step 3: Implement report schema**

In `apps/cli/src/teleop/report.rs`, implement CLI-owned serializable types:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TeleopExitStatus {
    Success,
    Failure,
}

#[derive(Debug, Clone, Serialize)]
pub struct TeleopJsonReport {
    pub schema_version: u8,
    pub command: &'static str,
    pub platform: String,
    pub targets: ReportTargets,
    pub profile: String,
    pub mode: ReportMode,
    pub control: ReportControl,
    pub calibration: ReportCalibration,
    pub exit: ReportExit,
    pub metrics: ReportMetrics,
}
```

Map SDK `left` to `master`, `right` to `slave`. Convert all durations to integer microseconds. Use snake_case strings for enums.

Implement:

- `classify_exit(faulted: bool, report: &BilateralRunReport) -> TeleopExitStatus`
- `print_human_report(...)`
- `TeleopJsonReport::from_run(...)`
- `write_json_report(path, report)`

- [ ] **Step 4: Run report tests**

Run: `cargo test -p piper-cli teleop::report -- --nocapture`

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add apps/cli/src/teleop/report.rs
git commit -m "feat: add teleop reports"
```

## Task 8: Implement Fakeable Workflow Orchestration

**Files:**
- Modify: `apps/cli/src/teleop/workflow.rs`
- Modify: `apps/cli/src/commands/teleop.rs`
- Test: `apps/cli/src/teleop/workflow.rs`

- [ ] **Step 1: Write failing workflow tests**

Use a fake backend and fake IO sharing a `WorkflowTrace` so tests can assert cross-boundary ordering such as `RunLoop` before `WriteReport`:

```rust
#[test]
fn malformed_calibration_fails_before_connect() {
    let temp = tempfile::tempdir().unwrap();
    let cal_path = temp.path().join("bad.toml");
    std::fs::write(&cal_path, "not toml").unwrap();
    let backend = FakeTeleopBackend::default();

    let err = run_workflow_for_test(args_with_calibration(&cal_path), backend.clone())
        .expect_err("malformed calibration must fail");

    assert!(err.to_string().contains("calibration"));
    assert_eq!(backend.calls(), Vec::<WorkflowCall>::new());
}

#[test]
fn save_calibration_existing_file_fails_before_connect() {
    let temp = tempfile::tempdir().unwrap();
    let save_path = temp.path().join("calibration.toml");
    std::fs::write(&save_path, "existing").unwrap();
    let backend = FakeTeleopBackend::default();

    let err = run_workflow_for_test(args_with_save_calibration(&save_path), backend.clone())
        .expect_err("existing save path must fail before connect");

    assert!(err.to_string().contains("exists"));
    assert_eq!(backend.calls(), Vec::<WorkflowCall>::new());
}

#[test]
fn declined_confirmation_exits_without_enable() {
    let backend = FakeTeleopBackend::default();
    let io = FakeTeleopIo::decline_confirmation();

    let err = run_workflow_for_test_with_io(valid_args(), backend.clone(), io)
        .expect_err("declining confirmation must stop before enable");

    assert!(err.to_string().contains("confirmation"));
    assert!(backend.calls().contains(&WorkflowCall::Connect));
    assert!(!backend.calls().contains(&WorkflowCall::Enable));
    assert!(!backend.calls().contains(&WorkflowCall::RunLoop));
}

#[test]
fn cancel_before_enable_exits_without_enable() {
    let backend = FakeTeleopBackend::default();
    let io = FakeTeleopIo::cancel_before_confirmation();

    let err = run_workflow_for_test_with_io(valid_args(), backend.clone(), io)
        .expect_err("Ctrl+C before enable must stop before enable");

    assert!(err.to_string().contains("cancel"));
    assert!(backend.calls().contains(&WorkflowCall::Connect));
    assert!(!backend.calls().contains(&WorkflowCall::Enable));
    assert!(!backend.calls().contains(&WorkflowCall::RunLoop));
}

#[test]
fn pre_enable_mismatch_fails_before_enable() {
    let backend = FakeTeleopBackend::with_standby_snapshot_mismatch();

    let err = run_workflow_for_test(valid_args(), backend.clone())
        .expect_err("pre-enable mismatch should fail");

    assert!(err.to_string().contains("calibration"));
    assert!(backend.calls().contains(&WorkflowCall::StandbySnapshot));
    assert!(!backend.calls().contains(&WorkflowCall::Enable));
    assert!(!backend.calls().contains(&WorkflowCall::RunLoop));
}

#[test]
fn unhealthy_runtime_health_fails_before_calibration_or_enable() {
    let backend = FakeTeleopBackend::with_unhealthy_runtime();

    let err = run_workflow_for_test(valid_args(), backend.clone())
        .expect_err("unhealthy runtime must fail before calibration or enable");

    assert!(err.to_string().contains("runtime health"));
    assert_call_order(
        backend.calls(),
        &[WorkflowCall::Connect, WorkflowCall::RuntimeHealth],
    );
    assert!(!backend.calls().contains(&WorkflowCall::CaptureCalibration));
    assert!(!backend.calls().contains(&WorkflowCall::Enable));
    assert!(!backend.calls().contains(&WorkflowCall::RunLoop));
}

#[test]
fn post_enable_mismatch_disables_and_does_not_run_loop() {
    let backend = FakeTeleopBackend::with_active_snapshot_mismatch();

    let err = run_workflow_for_test(valid_args(), backend.clone())
        .expect_err("post-enable mismatch should fail");

    assert!(err.to_string().contains("calibration"));
    assert!(backend.calls().contains(&WorkflowCall::DisableActive));
    assert!(!backend.calls().contains(&WorkflowCall::RunLoop));
}

#[test]
fn enable_confirmation_failure_is_returned_without_cli_cleanup_call() {
    let backend = FakeTeleopBackend::enable_confirmation_failure_after_dispatch();

    let err = run_workflow_for_test(valid_args(), backend.clone())
        .expect_err("enable failure should fail");

    assert!(err.to_string().contains("enable"));
    assert!(backend.calls().contains(&WorkflowCall::Enable));
    assert!(!backend.calls().contains(&WorkflowCall::DisableActive));
    assert!(!backend.calls().contains(&WorkflowCall::RunLoop));
}

#[test]
fn cancel_during_enable_disables_active_before_exit() {
    let backend = FakeTeleopBackend::cancel_during_enable_after_active();
    let io = FakeTeleopIo::cancel_during_enable();

    let err = run_workflow_for_test_with_io(valid_args(), backend.clone(), io)
        .expect_err("Ctrl+C during enable must exit safely");

    assert!(err.to_string().contains("cancel"));
    assert_call_order(
        backend.calls(),
        &[WorkflowCall::Enable, WorkflowCall::DisableActive],
    );
    assert!(!backend.calls().contains(&WorkflowCall::RunLoop));
}

#[test]
fn report_write_failure_happens_after_disabled_or_faulted_state() {
    let trace = WorkflowTrace::default();
    let backend = FakeTeleopBackend::loop_exits_cancelled_with_trace(trace.clone());
    let io = FakeTeleopIo::report_write_error_with_trace(trace.clone());

    let err = run_workflow_for_test_with_io(args_with_report_json(), backend.clone(), io)
        .expect_err("report write failure should be surfaced");

    assert!(err.to_string().contains("report"));
    assert_call_order(
        trace.calls(),
        &[WorkflowCall::RunLoop, WorkflowCall::WriteReport],
    );
    assert!(backend.was_disabled_or_faulted_before_report());
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p piper-cli teleop::workflow -- --nocapture`

Expected: FAIL because workflow backend and orchestration do not exist.

- [ ] **Step 3: Implement workflow backend trait**

In `apps/cli/src/teleop/workflow.rs`:

```rust
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::thread::JoinHandle;
use std::time::Instant;

pub trait TeleopBackend {
    fn connect(&mut self, targets: &RoleTargets, baud_rate: u32) -> Result<()>;
    fn runtime_health_ok(&self) -> Result<()>;
    fn standby_snapshot(&self, policy: DualArmReadPolicy) -> Result<DualArmSnapshot>;
    fn capture_calibration(&self, map: JointMirrorMap) -> Result<DualArmCalibration>;
    fn enable_mit(&mut self, master: MitModeConfig, slave: MitModeConfig) -> Result<EnableOutcome>;
    fn disable_active(&mut self) -> Result<()>;
    fn active_snapshot(&self, policy: DualArmReadPolicy) -> Result<DualArmSnapshot>;
    fn run_loop(
        &mut self,
        controller: RuntimeTeleopController,
        cfg: BilateralLoopConfig,
    ) -> Result<TeleopLoopExit>;
}

pub enum EnableOutcome {
    Active,
}

pub struct TeleopLoopExit {
    pub faulted: bool,
    pub report: BilateralRunReport,
}

pub trait TeleopIo {
    fn cancel_signal(&self) -> Arc<AtomicBool>;
    fn confirm_start(&mut self, summary: &StartupSummary) -> Result<bool>;
    fn cancel_requested(&self) -> bool;
    fn start_console(
        &mut self,
        settings: RuntimeTeleopSettingsHandle,
        started_at: Instant,
    ) -> Result<Option<JoinHandle<()>>>;
    fn write_json_report(&mut self, path: &Path, report: &TeleopJsonReport) -> Result<()>;
}
```

`FakeTeleopIo::start_console` returns `Ok(None)` unless a test explicitly needs to assert console startup. `RealTeleopIo::start_console` spawns the stdin console thread from Task 6 and passes the same cancel signal returned by `cancel_signal()`.

Keep this trait private to the CLI module unless tests need `pub(crate)`.

Backend contract:

- `enable_mit` consumes the standby state internally. If it returns `Err`, the CLI may not own an active or standby handle to clean up.
- Therefore every backend must return from `enable_mit` only after any cleanup that is possible for partially-dispatched enable commands has already happened.
- The real backend relies on Task 1 SDK guards and dual-arm partial-enable tests for this guarantee.
- Workflow tests must not model a fake `CleanupMaybeEnabled` call that the real backend cannot faithfully perform.

- [ ] **Step 4: Implement pure workflow function**

Implement:

```rust
pub fn run_workflow<B: TeleopBackend>(
    args: TeleopDualArmArgs,
    backend: &mut B,
    io: &mut dyn TeleopIo,
) -> Result<TeleopExitStatus>
```

Also add a test-only/internal variant so non-Linux target behavior is unit-testable without changing the host OS:

```rust
pub(crate) fn run_workflow_on_platform<B: TeleopBackend>(
    args: TeleopDualArmArgs,
    backend: &mut B,
    io: &mut dyn TeleopIo,
    platform: TeleopPlatform,
) -> Result<TeleopExitStatus>
```

`run_workflow` calls `run_workflow_on_platform(..., TeleopPlatform::current())`.

Order must match the spec:

1. parse config
2. load calibration file before connect
3. reject existing save path before connect
4. resolve concrete targets and reject unsupported runtime targets
5. read the shared cancellation token from `io.cancel_signal()`; this is the same `Arc<AtomicBool>` later passed into `BilateralLoopConfig`
6. connect
7. health check
8. capture calibration if needed
9. save calibration if requested
10. confirmation
11. pre-enable posture compatibility
12. enable
13. if enable returns `Err`, return nonzero immediately; do not call a fake cleanup hook, because the backend/SDK contract already handled possible cleanup
14. post-enable posture compatibility
15. call `io.start_console(settings_handle.clone(), started_at)`; fake IO may return `None`, real IO starts the console thread
16. build `BilateralLoopConfig` through `ResolvedTeleopConfig::loop_config(io.cancel_signal())` so Ctrl+C and `quit` cancel the SDK loop through the same token
17. run loop; when it returns, the backend must already be in standby or faulted state
18. write report after disabled/faulted state
19. classify exit

- [ ] **Step 5: Run workflow tests**

Run: `cargo test -p piper-cli teleop::workflow -- --nocapture`

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add apps/cli/src/teleop/workflow.rs apps/cli/src/commands/teleop.rs
git commit -m "feat: add teleop workflow orchestration"
```

## Task 9: Add Real Dual-Arm Backend and Command Execution

**Files:**
- Modify: `apps/cli/src/teleop/workflow.rs`
- Modify: `apps/cli/src/commands/teleop.rs`
- Test: `apps/cli/src/commands/teleop.rs`

- [ ] **Step 1: Write failing command-level tests**

Add tests that do not connect to hardware:

```rust
#[test]
fn gs_usb_runtime_target_is_rejected_before_backend_connect() {
    let args = TeleopDualArmArgs {
        master_target: Some("gs-usb-serial:A".to_string()),
        slave_target: Some("gs-usb-serial:B".to_string()),
        ..TeleopDualArmArgs::default_for_tests()
    };
    let backend = FakeTeleopBackend::default();

    let err = run_workflow_for_test(args, backend.clone())
        .expect_err("gs-usb runtime should be rejected in v1");

    assert!(err.to_string().contains("SoftRealtime dual-arm"));
    assert_eq!(backend.calls(), Vec::<WorkflowCall>::new());
}
```

Add an assert_cmd help smoke test if the existing CLI test style supports it:

```rust
#[test]
fn teleop_dual_arm_help_mentions_strict_realtime() {
    let mut cmd = assert_cmd::Command::cargo_bin("piper-cli").unwrap();
    cmd.args(["teleop", "dual-arm", "--help"])
        .assert()
        .success()
        .stdout(predicates::str::contains("StrictRealtime"));
}

#[test]
fn socketcan_runtime_target_is_rejected_on_non_linux_before_backend_connect() {
    let args = TeleopDualArmArgs {
        master_target: Some("socketcan:can0".to_string()),
        slave_target: Some("socketcan:can1".to_string()),
        ..TeleopDualArmArgs::default_for_tests()
    };
    let backend = FakeTeleopBackend::default();

    let err = run_workflow_for_test_on_platform(args, backend.clone(), TeleopPlatform::Other)
        .expect_err("SocketCAN runtime is Linux-only in v1");

    assert!(err.to_string().contains("Linux"));
    assert_eq!(backend.calls(), Vec::<WorkflowCall>::new());
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p piper-cli runtime_target_is_rejected -- --nocapture`

Expected: FAIL until command-level path is wired.

- [ ] **Step 3: Implement real backend**

In `apps/cli/src/teleop/workflow.rs`, add `RealTeleopBackend`:

```rust
pub struct RealTeleopBackend {
    standby: Option<DualArmStandby>,
    active: Option<DualArmActiveMit>,
}
```

Implement `TeleopBackend`:

- `connect`: build `PiperBuilder` from `RoleTargets`, then `DualArmBuilder::new(master_builder, slave_builder).build()`.
- `runtime_health_ok`: use `standby.observer().runtime_health().any_unhealthy()`.
- `standby_snapshot`: use `standby.observer().snapshot(policy)`.
- `capture_calibration`: use `standby.capture_calibration(map)`.
- `enable_mit`: take standby, call `enable_mit`, store active on success. On `Err`, return the error directly; there is no standby or active handle left in the CLI to clean up.
- `active_snapshot`: use `active.observer().snapshot(policy)`.
- `run_loop`: take active and call `run_bilateral`. Map `DualArmLoopExit::Standby { arms, report }` to `TeleopLoopExit { faulted: false, report }`, store `arms` back into `standby`, and set an internal `disabled_or_faulted = true` marker. Map `DualArmLoopExit::Faulted { arms: _, report }` to `TeleopLoopExit { faulted: true, report }`, drop the faulted state after extracting the report, and set `disabled_or_faulted = true`; no active/standby handle remains.
- `disable_active`: take active and call `disable_both(DisableConfig::default())`, storing returned standby on success. On error, return the error; consumed active state must rely on SDK drop/fault-shutdown behavior already covered by SDK loop tests.

The `enable_mit` error path is safe only because Task 1 proves SDK-level cleanup for left-confirmation failure, right-after-left failure, and confirmation timeout after dispatch. Do not add a CLI-level fake cleanup path that cannot be implemented faithfully with the consumed real state.

Do not support GS-USB runtime in this backend. That rejection happens before `connect`.

- [ ] **Step 4: Wire command execution**

Add production IO and blocking runner in `apps/cli/src/teleop/workflow.rs`:

```rust
pub struct RealTeleopIo {
    cancel: Arc<AtomicBool>,
}

impl RealTeleopIo {
    pub fn install_ctrlc() -> Result<Self> {
        let cancel = Arc::new(AtomicBool::new(false));
        let handler_cancel = cancel.clone();
        ctrlc::set_handler(move || {
            handler_cancel.store(true, Ordering::SeqCst);
        })?;
        Ok(Self { cancel })
    }
}

impl TeleopIo for RealTeleopIo {
    fn cancel_signal(&self) -> Arc<AtomicBool> {
        self.cancel.clone()
    }

    fn confirm_start(&mut self, summary: &StartupSummary) -> Result<bool> {
        print_startup_summary(summary);
        if summary.yes {
            return Ok(true);
        }
        read_yes_from_stdin_unless_cancelled(&self.cancel)
    }

    fn cancel_requested(&self) -> bool {
        self.cancel.load(Ordering::SeqCst)
    }

    fn start_console(
        &mut self,
        settings: RuntimeTeleopSettingsHandle,
        started_at: Instant,
    ) -> Result<Option<JoinHandle<()>>> {
        Ok(Some(crate::teleop::console::spawn_console_thread(
            settings,
            self.cancel.clone(),
            started_at,
        )?))
    }

    fn write_json_report(&mut self, path: &Path, report: &TeleopJsonReport) -> Result<()> {
        crate::teleop::report::write_json_report(path, report)
    }
}

pub fn run_dual_arm_blocking(args: TeleopDualArmArgs, backend: &mut RealTeleopBackend) -> Result<()> {
    let mut io = RealTeleopIo::install_ctrlc()?;
    let status = run_workflow(args, backend, &mut io)?;
    match status {
        TeleopExitStatus::Success => Ok(()),
        TeleopExitStatus::Failure => bail!("teleop dual-arm failed"),
    }
}
```

`RealTeleopIo::install_ctrlc()` must run before `run_workflow` can connect or enable either arm. Confirmation must return `Ok(false)` when the operator declines, and must return an error if cancellation is already requested while waiting for input. `start_console` must use the same `Arc<AtomicBool>` as `BilateralLoopConfig.cancel_signal`, so `quit` and Ctrl+C converge on the same SDK cancellation path.

Implement `read_yes_from_stdin_unless_cancelled` with a one-shot stdin reader thread that sends the line over a channel while the main workflow polls `cancel.load(Ordering::SeqCst)` with a short timeout. Do not block indefinitely in `stdin.read_line()` on the main control orchestration thread after Ctrl+C has been requested.

In `TeleopCommand::execute`, create `RealTeleopBackend` and call the blocking runner. Because main is Tokio but teleop is blocking hardware control, wrap in `spawn_blocking`:

```rust
pub async fn execute(self) -> Result<()> {
    match self.action {
        TeleopAction::DualArm(args) => tokio::task::spawn_blocking(move || {
            let mut backend = crate::teleop::workflow::RealTeleopBackend::default();
            crate::teleop::workflow::run_dual_arm_blocking(args, &mut backend)
        })
        .await??,
    }
    Ok(())
}
```

If `TeleopExitStatus::Failure`, return an `anyhow::Error` so process exit is non-zero.

- [ ] **Step 5: Run command tests**

Run: `cargo test -p piper-cli teleop -- --nocapture`

Expected: PASS.

Run: `cargo run -p piper-cli -- teleop dual-arm --help`

Expected: help output includes master/slave targets, mode, profile, calibration, report JSON, and StrictRealtime/GS-USB v1 limitation text.

- [ ] **Step 6: Commit**

```bash
git add apps/cli/src/commands/teleop.rs apps/cli/src/teleop/workflow.rs
git commit -m "feat: wire teleop dual-arm command"
```

## Task 10: Add Operator Documentation

**Files:**
- Create: `apps/cli/TELEOP_DUAL_ARM.md`
- Modify: `apps/cli/README.md`
- Test: documentation grep only

- [ ] **Step 1: Write operator guide**

Create `apps/cli/TELEOP_DUAL_ARM.md` with these sections:

```markdown
# Piper CLI Dual-Arm Teleoperation

## Supported Topology

v1 supports two independent StrictRealtime SocketCAN links, normally `can0` and
`can1`. GS-USB target syntax is accepted by config/help for future compatibility,
but runtime execution is rejected until SDK SoftRealtime dual-arm support exists.

## First Run

If you do not pass `--calibration-file`, place both arms in the intended
mirrored zero pose before running the command. Startup captures that posture
automatically before the enable confirmation.

```bash
piper-cli teleop dual-arm \
  --master-interface can0 \
  --slave-interface can1 \
  --mode master-follower
```

## Calibration

With `--calibration-file`, the CLI loads the baseline and checks the current
posture before enabling. Without it, startup connects to both arms, checks
runtime health, then captures the current posture as the baseline before asking
for operator confirmation. `--save-calibration <path>` writes that captured
baseline before confirmation if no calibration file is supplied.

## Runtime Console

- `status`
- `mode master-follower`
- `mode bilateral`
- `gain track-kp <value>`
- `gain track-kd <value>`
- `gain master-damping <value>`
- `gain reflection-gain <value>`
- `quit`

## Report Interpretation

The human report and JSON report use master/slave naming even though the SDK
internally stores the two arms as left/right. Durations are integer
microseconds. In JSON, `exit.reason = cancelled` is a clean operator stop, and
the human report prints the same value as `reason=cancelled`.
`metrics.read_faults` or `metrics.submission_faults` mean the run is unsafe to
continue without inspecting the CAN link and arm status.

## Exit Codes

Exit code `0` means the loop ended cleanly with `exit.reason = cancelled` or
`exit.reason = max_iterations` and post-disable reporting succeeded. Nonzero
means startup validation failed, runtime faulted, or the JSON report could not
be written after the arms were already disabled or faulted.

## Fault Response

Unsupported runtime targets are rejected before hardware connect. Posture
mismatch and Ctrl+C before MIT enable stop startup without entering active
control. After enable, clean cancellation and `max_iterations` exit through
normal disable. Read, controller, and compensation faults also attempt to return
both arms to standby when possible. Submission faults and runtime transport
faults use the SDK fault-shutdown path and record per-arm stop-attempt results.

## Stop Attempt Results

The human report prints separate master/slave lines with `stop_attempt=...`.
JSON reports store the same values at `metrics.master_stop_attempt` and
`metrics.slave_stop_attempt`.

Possible values are `not_attempted`, `confirmed_sent`, `timeout`,
`channel_closed`, `queue_rejected`, and `transport_failed`. `not_attempted` is
expected for clean normal-disable exits such as `cancelled` and
`max_iterations`, and it can also appear when a non-clean path returned to
standby without using fault shutdown. For submission or runtime transport
faults, inspect these per-arm fields before the next run; `confirmed_sent` means
the fault-shutdown stop command was accepted, while timeout, closed-channel,
queue-rejected, or transport-failed values require hardware and CAN-link
inspection.

## JSON Report Schema

`schema_version = 1` is intentionally incompatible with future schemas unless a
new version is declared. Consumers must check `schema_version`, `exit.reason`,
`metrics.read_faults`, `metrics.submission_faults`,
`metrics.master_stop_attempt`, and `metrics.slave_stop_attempt` before treating
data as a successful run.

## Manual Acceptance Checklist

1. Confirm both links are independent StrictRealtime SocketCAN links.
2. Run master-follower at default gains.
3. Verify every mirrored joint direction.
4. Run for several minutes with zero read/submission faults.
5. Switch to bilateral with low reflection gain.
6. Verify soft-contact reflection direction.
7. Ctrl+C exits cleanly.
8. Disconnect one feedback path and confirm bounded shutdown.
```
```

- [ ] **Step 2: Link from README**

Add to `apps/cli/README.md` under quick start:

```markdown
### Dual-Arm Teleoperation

See [TELEOP_DUAL_ARM.md](TELEOP_DUAL_ARM.md) for the production dual-arm
teleoperation bring-up guide.
```

- [ ] **Step 3: Verify docs references**

Run: `rg -n "TELEOP_DUAL_ARM|StrictRealtime|GS-USB|teleop dual-arm|exit\\.reason|reason=|metrics\\.read_faults|metrics\\.submission_faults|stop_attempt|bounded shutdown|JSON Report Schema|master_stop_attempt|slave_stop_attempt|max_iterations|fault-shutdown" apps/cli/README.md apps/cli/TELEOP_DUAL_ARM.md`

Expected: output includes the README link and guide sections.

- [ ] **Step 4: Commit**

```bash
git add apps/cli/README.md apps/cli/TELEOP_DUAL_ARM.md
git commit -m "docs: add dual-arm teleop guide"
```

## Task 11: Final Verification

**Files:**
- No new files unless verification finds issues.

- [ ] **Step 1: Run formatting**

Run: `cargo fmt --all -- --check`

Expected: PASS.

If it fails, run `cargo fmt --all`, inspect the diff, and commit formatting with the affected task commit if still in progress; otherwise create a cleanup commit.

- [ ] **Step 2: Run piper-cli tests**

Run: `cargo test -p piper-cli --all-targets`

Expected: PASS.

- [ ] **Step 3: Run piper-client targeted tests**

Run: `cargo test -p piper-client enable_mit_mode_timeout_after_enable_dispatch_sends_disable_all -- --nocapture`

Expected: PASS.

- [ ] **Step 4: Run full workspace tests**

Run: `cargo test --workspace --all-targets --all-features`

Expected: PASS.

- [ ] **Step 5: Run clippy without bypass**

Run: `cargo clippy --workspace --all-targets --all-features -- -D warnings`

Expected: PASS.

Do not use `--no-verify`. Fix clippy errors directly.

- [ ] **Step 6: Smoke help output**

Run: `cargo run -p piper-cli -- teleop dual-arm --help`

Expected: exits 0 and documents:

- `--master-target`
- `--slave-target`
- `--mode`
- `--profile`
- `--calibration-file`
- `--report-json`
- StrictRealtime SocketCAN v1 runtime support
- GS-USB future SDK prerequisite

- [ ] **Step 7: Final commit if needed**

If verification required cleanup:

```bash
git add <changed-files>
git commit -m "test: verify dual-arm teleop cli"
```

If no cleanup was needed, do not create an empty commit.
