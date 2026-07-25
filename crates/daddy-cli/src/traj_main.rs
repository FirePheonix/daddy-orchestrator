use anyhow::Result;
use clap::Parser;
use daddy_storage::inspect_trajectory;
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "daddy-traj", about = "Inspect saved daddy trajectories")]
struct Cli {
    path: PathBuf,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    println!("{}", inspect_trajectory(cli.path)?);
    Ok(())
}
