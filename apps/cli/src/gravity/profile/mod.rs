pub mod config;

use anyhow::Result;

pub async fn run(args: crate::commands::gravity::GravityProfileArgs) -> Result<()> {
    match args.action {
        crate::commands::gravity::GravityProfileAction::Init(_) => {
            anyhow::bail!("gravity profile init is not implemented yet")
        },
        crate::commands::gravity::GravityProfileAction::Status(_) => {
            anyhow::bail!("gravity profile status is not implemented yet")
        },
        crate::commands::gravity::GravityProfileAction::Next(_) => {
            anyhow::bail!("gravity profile next is not implemented yet")
        },
        crate::commands::gravity::GravityProfileAction::RecordPath(_) => {
            anyhow::bail!("gravity profile record-path is not implemented yet")
        },
        crate::commands::gravity::GravityProfileAction::ReplaySample(_) => {
            anyhow::bail!("gravity profile replay-sample is not implemented yet")
        },
        crate::commands::gravity::GravityProfileAction::ImportSamples(_) => {
            anyhow::bail!("gravity profile import-samples is not implemented yet")
        },
        crate::commands::gravity::GravityProfileAction::FitAssess(_) => {
            anyhow::bail!("gravity profile fit-assess is not implemented yet")
        },
        crate::commands::gravity::GravityProfileAction::PromoteValidation(_) => {
            anyhow::bail!("gravity profile promote-validation is not implemented yet")
        },
    }
}
