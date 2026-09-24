mod config;
mod ingest;

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{bail, Context};
use clap::Parser;
use protocol::{Catalog, Sample};
use tokio::net::UdpSocket;
use tracing::info;

use crate::config::Config;
use crate::ingest::IngestStats;

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

    /// Print every decoded sample to stdout and every decode error to stderr.
    #[arg(long)]
    debug_decode: bool,
}

fn print_sample(catalog: &Catalog, s: &Sample) {
    let unit = catalog
        .by_id(s.signal_id)
        .and_then(|c| c.unit())
        .unwrap_or("");
    let value = match s.value.as_num() {
        Some(v) => format!("{v:.3}"),
        None => s.value.to_string(),
    };
    println!("{} {:<14} {value} {unit}", s.timestamp_us, s.name);
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .with_writer(std::io::stderr)
        .init();
    let args = Args::parse();

    let config = Config::load(&args.config)?;
    let json = std::fs::read_to_string(&config.catalog_path)
        .with_context(|| format!("reading catalog {}", config.catalog_path.display()))?;
    let catalog = Arc::new(
        Catalog::from_json(&json)
            .with_context(|| format!("loading catalog {}", config.catalog_path.display()))?,
    );
    if catalog.model() != config.model {
        bail!(
            "config model '{}' does not match catalog model '{}'",
            config.model,
            catalog.model()
        );
    }

    let socket = UdpSocket::bind(config.ingest.udp_bind)
        .await
        .with_context(|| format!("binding UDP {}", config.ingest.udp_bind))?;
    info!(
        vin = config.vin,
        model = catalog.model(),
        catalog_version = catalog.catalog_version(),
        udp = %config.ingest.udp_bind,
        debug_decode = args.debug_decode,
        "telemetryd started"
    );

    let mut stats = IngestStats::default();
    let printer = Arc::clone(&catalog);
    tokio::select! {
        res = ingest::run_udp(socket, Arc::clone(&catalog), &mut stats, args.debug_decode, |s| {
            if args.debug_decode {
                print_sample(&printer, &s);
            }
        }) => res.context("UDP ingest failed")?,
        _ = tokio::signal::ctrl_c() => info!("shutting down"),
    }
    stats.log();
    Ok(())
}
