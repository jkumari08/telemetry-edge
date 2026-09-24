use std::path::PathBuf;

use clap::Parser;

/// Cloud rule server: serves versioned, signed rule sets per vehicle model,
/// ingests uploaded telemetry batches, and hosts the rule editor UI.
#[derive(Debug, Parser)]
#[command(version, about)]
struct Args {
    /// Path to the cloud server configuration file (TOML).
    #[arg(long, default_value = "dev/cloud.toml")]
    config: PathBuf,
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    eprintln!("cloud: scaffold only, config = {}", args.config.display());
    Ok(())
}
