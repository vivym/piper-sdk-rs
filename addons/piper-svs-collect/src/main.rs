use anyhow::Result;
use clap::Parser;
use piper_svs_collect::args::Args;

fn main() -> Result<()> {
    let _args = Args::parse();
    Ok(())
}
