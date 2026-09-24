use std::path::PathBuf;

use clap::Parser;

/// Rule-driven vehicle telemetry logger.
///
/// Ingests binary signal frames over UDP (and optionally gRPC), decodes them
/// with a per-model signal catalog, and logs signals according to rules
/// fetched from the cloud. Runs in the foreground; systemd integration is a
/// no-op when not started by systemd.
#[derive(Debug, Parser)]
#[command(version, about)]
struct Args {
    /// Path to the daemon configuration file (TOML).
    #[arg(long, default_value = "/etc/telemetryd/config.toml")]
    config: PathBuf,
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    eprintln!(
        "telemetryd: scaffold only, config = {}",
        args.config.display()
    );
    Ok(())
}
