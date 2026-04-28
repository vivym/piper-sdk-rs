#![allow(dead_code)]

use anyhow::{Context, Result, bail};
use piper_control::TargetSpec;
use piper_sdk::driver::ConnectionTarget;
use std::str::FromStr;

use crate::commands::teleop::TeleopDualArmArgs;
use crate::teleop::config::TeleopConfigFile;

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

#[derive(Debug, Clone, Copy)]
enum Role {
    Master,
    Slave,
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
            TargetSpec::GsUsbBusAddress { bus, address } => {
                Ok(Self::GsUsbBusAddress { bus, address })
            },
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

    pub fn ensure_experimental_raw_clock_supported(&self, platform: TeleopPlatform) -> Result<()> {
        match self {
            Self::SocketCan { .. } => match platform {
                TeleopPlatform::Linux => Ok(()),
                TeleopPlatform::Other => {
                    bail!("experimental calibrated raw clock requires Linux SocketCAN targets")
                },
            },
            Self::GsUsbSerial { .. } | Self::GsUsbBusAddress { .. } => {
                bail!(
                    "GS-USB targets are not supported by experimental calibrated raw clock; use explicit SocketCAN targets"
                )
            },
        }
    }

    pub fn to_connection_target(&self) -> ConnectionTarget {
        match self {
            Self::SocketCan { iface } => ConnectionTarget::SocketCan {
                iface: iface.clone(),
            },
            Self::GsUsbSerial { serial } => ConnectionTarget::GsUsbSerial {
                serial: serial.clone(),
            },
            Self::GsUsbBusAddress { bus, address } => ConnectionTarget::GsUsbBusAddress {
                bus: *bus,
                address: *address,
            },
        }
    }
}

impl RoleTargets {
    pub fn validate_no_duplicates(&self) -> Result<()> {
        match (&self.master, &self.slave) {
            (
                ConcreteTeleopTarget::SocketCan { iface: master },
                ConcreteTeleopTarget::SocketCan { iface: slave },
            ) if master == slave => bail!("duplicate SocketCAN teleop target: {master}"),
            (
                ConcreteTeleopTarget::GsUsbSerial { serial: master },
                ConcreteTeleopTarget::GsUsbSerial { serial: slave },
            ) if master == slave => bail!("duplicate GS-USB serial teleop target: {master}"),
            (
                ConcreteTeleopTarget::GsUsbBusAddress {
                    bus: master_bus,
                    address: master_address,
                },
                ConcreteTeleopTarget::GsUsbBusAddress {
                    bus: slave_bus,
                    address: slave_address,
                },
            ) if master_bus == slave_bus && master_address == slave_address => {
                bail!("duplicate GS-USB bus/address teleop target: {master_bus}:{master_address}")
            },
            (
                ConcreteTeleopTarget::GsUsbSerial { .. },
                ConcreteTeleopTarget::GsUsbBusAddress { .. },
            )
            | (
                ConcreteTeleopTarget::GsUsbBusAddress { .. },
                ConcreteTeleopTarget::GsUsbSerial { .. },
            ) => bail!(
                "mixed GS-USB selector kinds are not allowed for dual-arm teleop; use serials or bus addresses for both roles"
            ),
            _ => Ok(()),
        }
    }

    pub fn ensure_v1_runtime_supported(&self, platform: TeleopPlatform) -> Result<()> {
        self.master.ensure_v1_runtime_supported(platform)?;
        self.slave.ensure_v1_runtime_supported(platform)?;
        Ok(())
    }

    pub fn ensure_experimental_raw_clock_supported(&self, platform: TeleopPlatform) -> Result<()> {
        self.master.ensure_experimental_raw_clock_supported(platform)?;
        self.slave.ensure_experimental_raw_clock_supported(platform)?;
        Ok(())
    }
}

pub fn resolve_role_targets(
    args: &TeleopDualArmArgs,
    file: Option<&TeleopConfigFile>,
    platform: TeleopPlatform,
) -> Result<RoleTargets> {
    ensure_one_cli_selector(Role::Master, args)?;
    ensure_one_cli_selector(Role::Slave, args)?;

    let missing_roles = missing_roles(args, file);
    if args.experimental_calibrated_raw && !missing_roles.is_empty() {
        bail!(
            "experimental calibrated raw clock requires explicit SocketCAN targets for master and slave; defaults are not allowed; missing {}",
            missing_roles.join(" and ")
        );
    }
    if platform == TeleopPlatform::Other && !missing_roles.is_empty() {
        bail!(
            "dual-arm teleop targets are required on non-Linux; missing {}",
            missing_roles.join(" and ")
        );
    }

    let targets = RoleTargets {
        master: resolve_one_role(Role::Master, args, file)?,
        slave: resolve_one_role(Role::Slave, args, file)?,
    };

    targets.validate_no_duplicates()?;
    if args.experimental_calibrated_raw {
        targets.ensure_experimental_raw_clock_supported(platform)?;
    } else {
        targets.ensure_v1_runtime_supported(platform)?;
    }
    Ok(targets)
}

fn resolve_one_role(
    role: Role,
    args: &TeleopDualArmArgs,
    file: Option<&TeleopConfigFile>,
) -> Result<ConcreteTeleopTarget> {
    let cli_selectors = cli_selectors(role, args);
    if let Some((flag, target)) = cli_selectors.into_iter().next() {
        return parse_role_target(role, flag, &target);
    }
    if let Some(target) = config_target(role, file) {
        return parse_role_target(role, role.config_source(), target);
    }
    parse_role_target(role, "default", role.default_linux_target())
}

fn ensure_one_cli_selector(role: Role, args: &TeleopDualArmArgs) -> Result<()> {
    let cli_selectors = cli_selectors(role, args);
    if cli_selectors.len() > 1 {
        let flags = cli_selectors.iter().map(|(flag, _)| *flag).collect::<Vec<_>>().join(", ");
        bail!(
            "{} role has multiple CLI target selectors ({flags}); use exactly one",
            role.name(),
        );
    }
    Ok(())
}

fn parse_role_target(role: Role, source: &str, value: &str) -> Result<ConcreteTeleopTarget> {
    ConcreteTeleopTarget::parse(value)
        .with_context(|| format!("invalid {} target from {source}", role.name()))
}

fn missing_roles(args: &TeleopDualArmArgs, file: Option<&TeleopConfigFile>) -> Vec<&'static str> {
    [Role::Master, Role::Slave]
        .into_iter()
        .filter(|role| {
            cli_selectors(*role, args).is_empty() && config_target(*role, file).is_none()
        })
        .map(|role| role.name())
        .collect()
}

fn cli_selectors(role: Role, args: &TeleopDualArmArgs) -> Vec<(&'static str, String)> {
    let mut selectors = Vec::new();
    match role {
        Role::Master => {
            push_selector(
                &mut selectors,
                "--master-target",
                args.master_target.as_ref(),
            );
            push_selector(
                &mut selectors,
                "--master-interface",
                args.master_interface
                    .as_ref()
                    .map(|iface| format!("socketcan:{iface}"))
                    .as_ref(),
            );
            push_selector(
                &mut selectors,
                "--master-serial",
                args.master_serial
                    .as_ref()
                    .map(|serial| format!("gs-usb-serial:{serial}"))
                    .as_ref(),
            );
            push_selector(
                &mut selectors,
                "--master-gs-usb-bus-address",
                args.master_gs_usb_bus_address
                    .as_ref()
                    .map(|bus_address| format!("gs-usb-bus-address:{bus_address}"))
                    .as_ref(),
            );
        },
        Role::Slave => {
            push_selector(&mut selectors, "--slave-target", args.slave_target.as_ref());
            push_selector(
                &mut selectors,
                "--slave-interface",
                args.slave_interface.as_ref().map(|iface| format!("socketcan:{iface}")).as_ref(),
            );
            push_selector(
                &mut selectors,
                "--slave-serial",
                args.slave_serial
                    .as_ref()
                    .map(|serial| format!("gs-usb-serial:{serial}"))
                    .as_ref(),
            );
            push_selector(
                &mut selectors,
                "--slave-gs-usb-bus-address",
                args.slave_gs_usb_bus_address
                    .as_ref()
                    .map(|bus_address| format!("gs-usb-bus-address:{bus_address}"))
                    .as_ref(),
            );
        },
    }
    selectors
}

fn push_selector(
    selectors: &mut Vec<(&'static str, String)>,
    name: &'static str,
    value: Option<&String>,
) {
    if let Some(value) = value {
        selectors.push((name, value.clone()));
    }
}

fn config_target(role: Role, file: Option<&TeleopConfigFile>) -> Option<&str> {
    let arms = file?.arms.as_ref()?;
    match role {
        Role::Master => arms.master.as_ref()?.target.as_deref(),
        Role::Slave => arms.slave.as_ref()?.target.as_deref(),
    }
}

impl Role {
    fn name(self) -> &'static str {
        match self {
            Self::Master => "master",
            Self::Slave => "slave",
        }
    }

    fn default_linux_target(self) -> &'static str {
        match self {
            Self::Master => "socketcan:can0",
            Self::Slave => "socketcan:can1",
        }
    }

    fn config_source(self) -> &'static str {
        match self {
            Self::Master => "config [arms.master].target",
            Self::Slave => "config [arms.slave].target",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::teleop::TeleopDualArmArgs;
    use crate::teleop::config::TeleopConfigFile;

    #[test]
    fn parses_socketcan_target() {
        assert_eq!(
            ConcreteTeleopTarget::parse("socketcan:can0").unwrap(),
            ConcreteTeleopTarget::SocketCan {
                iface: "can0".to_string()
            }
        );
    }

    #[test]
    fn current_platform_matches_compile_target() {
        let expected = if cfg!(target_os = "linux") {
            TeleopPlatform::Linux
        } else {
            TeleopPlatform::Other
        };

        assert_eq!(TeleopPlatform::current(), expected);
    }

    #[test]
    fn converts_to_connection_target() {
        let target = ConcreteTeleopTarget::GsUsbBusAddress { bus: 1, address: 2 };

        assert_eq!(
            target.to_connection_target(),
            ConnectionTarget::GsUsbBusAddress { bus: 1, address: 2 }
        );
    }

    #[test]
    fn rejects_non_concrete_targets() {
        for target in ["auto-strict", "auto-any", "gs-usb-auto"] {
            assert!(
                ConcreteTeleopTarget::parse(target).is_err(),
                "{target} should be rejected"
            );
        }
    }

    #[test]
    fn duplicate_socketcan_targets_are_rejected() {
        let targets = RoleTargets {
            master: ConcreteTeleopTarget::SocketCan {
                iface: "can0".to_string(),
            },
            slave: ConcreteTeleopTarget::SocketCan {
                iface: "can0".to_string(),
            },
        };

        let err = targets
            .validate_no_duplicates()
            .expect_err("duplicate SocketCAN interface must fail");

        assert!(err.to_string().contains("duplicate"));
    }

    #[test]
    fn gs_usb_runtime_target_is_rejected_for_v1() {
        let target = ConcreteTeleopTarget::GsUsbSerial {
            serial: "ABC".to_string(),
        };

        let err = target
            .ensure_v1_runtime_supported(TeleopPlatform::Linux)
            .expect_err("GS-USB runtime target must fail in v1");

        let message = err.to_string();
        assert!(message.contains("GS-USB"));
        assert!(message.contains("SoftRealtime"));
        assert!(message.contains("dual-arm"));
    }

    #[test]
    fn socketcan_runtime_is_rejected_on_non_linux() {
        let target = ConcreteTeleopTarget::SocketCan {
            iface: "can0".to_string(),
        };

        let err = target
            .ensure_v1_runtime_supported(TeleopPlatform::Other)
            .expect_err("SocketCAN must fail outside Linux");

        assert!(err.to_string().contains("SocketCAN"));
    }

    #[test]
    fn cli_role_selectors_override_config_file_role_targets() {
        let file: TeleopConfigFile = toml::from_str(
            r#"
            [arms.master]
            target = "socketcan:file_master"

            [arms.slave]
            target = "socketcan:file_slave"
            "#,
        )
        .unwrap();
        let args = TeleopDualArmArgs {
            master_interface: Some("cli_master".to_string()),
            slave_interface: Some("cli_slave".to_string()),
            ..TeleopDualArmArgs::default_for_tests()
        };

        let targets = resolve_role_targets(&args, Some(&file), TeleopPlatform::Linux).unwrap();

        assert_eq!(
            targets.master,
            ConcreteTeleopTarget::SocketCan {
                iface: "cli_master".to_string()
            }
        );
        assert_eq!(
            targets.slave,
            ConcreteTeleopTarget::SocketCan {
                iface: "cli_slave".to_string()
            }
        );
    }

    #[test]
    fn linux_defaults_resolve_to_can0_and_can1() {
        let targets = resolve_role_targets(
            &TeleopDualArmArgs::default_for_tests(),
            None,
            TeleopPlatform::Linux,
        )
        .unwrap();

        assert_eq!(
            targets.master,
            ConcreteTeleopTarget::SocketCan {
                iface: "can0".to_string()
            }
        );
        assert_eq!(
            targets.slave,
            ConcreteTeleopTarget::SocketCan {
                iface: "can1".to_string()
            }
        );
    }

    #[test]
    fn experimental_raw_clock_requires_socketcan_targets() {
        let args = TeleopDualArmArgs {
            experimental_calibrated_raw: true,
            master_serial: Some("MASTER".to_string()),
            slave_serial: Some("SLAVE".to_string()),
            ..TeleopDualArmArgs::default_for_tests()
        };

        let err = resolve_role_targets(&args, None, TeleopPlatform::Linux)
            .expect_err("experimental raw-clock teleop must reject GS-USB targets");

        assert!(err.to_string().contains("GS-USB"));
    }

    #[test]
    fn experimental_raw_clock_rejects_default_or_omitted_targets() {
        let args = TeleopDualArmArgs {
            experimental_calibrated_raw: true,
            master_interface: None,
            slave_interface: None,
            master_serial: None,
            slave_serial: None,
            ..TeleopDualArmArgs::default_for_tests()
        };

        let err = resolve_role_targets(&args, None, TeleopPlatform::Linux)
            .expect_err("experimental raw-clock teleop must reject implicit defaults");

        assert!(err.to_string().contains("explicit SocketCAN"));
    }

    #[test]
    fn non_linux_missing_targets_fail_and_mention_both_roles() {
        let err = resolve_role_targets(
            &TeleopDualArmArgs::default_for_tests(),
            None,
            TeleopPlatform::Other,
        )
        .expect_err("missing targets must fail on non-Linux");

        let message = err.to_string();
        assert!(message.contains("master"));
        assert!(message.contains("slave"));
    }

    #[test]
    fn multiple_cli_selectors_for_one_role_are_rejected() {
        let args = TeleopDualArmArgs {
            master_target: Some("socketcan:can0".to_string()),
            master_interface: Some("can0".to_string()),
            slave_interface: Some("can1".to_string()),
            ..TeleopDualArmArgs::default_for_tests()
        };

        let err = resolve_role_targets(&args, None, TeleopPlatform::Linux)
            .expect_err("multiple selectors must fail");

        assert!(err.to_string().contains("master"));
        assert!(err.to_string().contains("--master-target"));
        assert!(err.to_string().contains("--master-interface"));
    }

    #[test]
    fn multiple_cli_selectors_are_rejected_even_when_config_target_exists() {
        let file: TeleopConfigFile = toml::from_str(
            r#"
            [arms.master]
            target = "socketcan:file_master"

            [arms.slave]
            target = "socketcan:file_slave"
            "#,
        )
        .unwrap();
        let args = TeleopDualArmArgs {
            slave_target: Some("socketcan:can1".to_string()),
            slave_interface: Some("can1".to_string()),
            ..TeleopDualArmArgs::default_for_tests()
        };

        let err = resolve_role_targets(&args, Some(&file), TeleopPlatform::Linux)
            .expect_err("multiple selectors must fail even with config");

        assert!(err.to_string().contains("slave"));
    }

    #[test]
    fn malformed_cli_target_error_names_role_and_flag() {
        let args = TeleopDualArmArgs {
            master_target: Some("socketcan:".to_string()),
            slave_interface: Some("can1".to_string()),
            ..TeleopDualArmArgs::default_for_tests()
        };

        let err = resolve_role_targets(&args, None, TeleopPlatform::Linux)
            .expect_err("malformed CLI target must fail");

        let message = err.to_string();
        assert!(message.contains("master"));
        assert!(message.contains("--master-target"));
    }

    #[test]
    fn malformed_config_target_error_names_role_and_source() {
        let file: TeleopConfigFile = toml::from_str(
            r#"
            [arms.master]
            target = "socketcan:can0"

            [arms.slave]
            target = "socketcan:"
            "#,
        )
        .unwrap();

        let err = resolve_role_targets(
            &TeleopDualArmArgs::default_for_tests(),
            Some(&file),
            TeleopPlatform::Linux,
        )
        .expect_err("malformed config target must fail");

        let message = err.to_string();
        assert!(message.contains("slave"));
        assert!(message.contains("config"));
    }

    #[test]
    fn toml_config_uses_spec_shape() {
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
