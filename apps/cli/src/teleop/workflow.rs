use anyhow::{Result, bail};

use crate::commands::teleop::TeleopDualArmArgs;

pub async fn run_dual_arm(_args: TeleopDualArmArgs) -> Result<()> {
    bail!("teleop dual-arm is not implemented yet")
}
