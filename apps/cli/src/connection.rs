use clap::Args;
use piper_client::PiperBuilder as ClientPiperBuilder;
use piper_control::{TargetSpec, client_builder_for_target, driver_builder_for_target};
use piper_sdk::driver::{ConnectionTarget, PiperBuilder as DriverPiperBuilder};

use crate::commands::config::CliConfig;

#[derive(Args, Debug, Clone, Default)]
pub struct TargetArgs {
    /// 连接目标，示例: auto-strict / socketcan:can0 / gs-usb-serial:ABC123 / gs-usb-bus-address:1:8
    #[arg(long, value_name = "SPEC")]
    pub target: Option<TargetSpec>,
}

pub fn resolved_target_spec(
    config: &CliConfig,
    override_target: Option<&TargetSpec>,
) -> TargetSpec {
    config.resolved_target_spec(override_target)
}

pub fn resolved_target(
    config: &CliConfig,
    override_target: Option<&TargetSpec>,
) -> ConnectionTarget {
    resolved_target_spec(config, override_target).into_connection_target()
}

pub fn client_builder(target: &ConnectionTarget) -> ClientPiperBuilder {
    client_builder_for_target(target)
}

pub fn driver_builder(target: &ConnectionTarget) -> DriverPiperBuilder {
    driver_builder_for_target(target)
}
