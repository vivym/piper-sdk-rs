use anyhow::Result;
use clap::Parser;
use piper_svs_collect::{args::Args, collector::run_from_args};

fn main() -> Result<()> {
    run_from_args(Args::parse()).map(|_| ())
}
