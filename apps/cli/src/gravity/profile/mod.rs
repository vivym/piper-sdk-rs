pub mod artifacts;
pub mod config;
pub mod context;
pub mod manifest;
pub mod status;
pub mod workflow;

use anyhow::Result;

pub async fn run(args: crate::commands::gravity::GravityProfileArgs) -> Result<()> {
    match args.action {
        crate::commands::gravity::GravityProfileAction::Init(args) => workflow::init_profile(args),
        crate::commands::gravity::GravityProfileAction::Status(args) => {
            workflow::print_status(args)
        },
        crate::commands::gravity::GravityProfileAction::Next(args) => workflow::print_next(args),
        crate::commands::gravity::GravityProfileAction::RecordPath(_) => {
            anyhow::bail!("gravity profile record-path is not implemented yet")
        },
        crate::commands::gravity::GravityProfileAction::ReplaySample(_) => {
            anyhow::bail!("gravity profile replay-sample is not implemented yet")
        },
        crate::commands::gravity::GravityProfileAction::ImportSamples(args) => {
            workflow::import_samples(args)
        },
        crate::commands::gravity::GravityProfileAction::FitAssess(_) => {
            anyhow::bail!("gravity profile fit-assess is not implemented yet")
        },
        crate::commands::gravity::GravityProfileAction::PromoteValidation(_) => {
            anyhow::bail!("gravity profile promote-validation is not implemented yet")
        },
    }
}
