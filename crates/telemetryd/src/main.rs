mod config;
mod engine_task;
mod ingest;
mod output;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Context};
use clap::Parser;
use protocol::{Catalog, RuleSet};
use rules::{compile_ruleset, CompiledRuleSet, Engine, SystemClock};
use tokio::net::UdpSocket;
use tokio::sync::{mpsc, watch};
use tracing::{error, info, warn};

use crate::config::Config;
use crate::output::RotatingWriter;

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

/// Queue sizes between tasks. When the engine falls behind, ingest waits and
/// the kernel's UDP buffer absorbs (then drops) the excess.
const SAMPLE_QUEUE: usize = 4096;
const RECORD_QUEUE: usize = 4096;
/// Upper bound on draining queues and flushing files at shutdown.
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(10);

fn load_catalog(config: &Config) -> anyhow::Result<Catalog> {
    let json = std::fs::read_to_string(&config.catalog_path)
        .with_context(|| format!("reading catalog {}", config.catalog_path.display()))?;
    let catalog = Catalog::from_json(&json)
        .with_context(|| format!("loading catalog {}", config.catalog_path.display()))?;
    if catalog.model() != config.model {
        bail!(
            "config model '{}' does not match catalog model '{}'",
            config.model,
            catalog.model()
        );
    }
    Ok(catalog)
}

fn load_rules(config: &Config, catalog: &Catalog) -> anyhow::Result<CompiledRuleSet> {
    let Some(path) = &config.rules_file else {
        warn!("no rules_file configured; running with an empty rule set");
        return Ok(CompiledRuleSet::empty());
    };
    let json = std::fs::read_to_string(path)
        .with_context(|| format!("reading rules {}", path.display()))?;
    let rules =
        RuleSet::from_json(&json).with_context(|| format!("parsing rules {}", path.display()))?;
    compile_ruleset(&rules, catalog, &config.model, None)
        .with_context(|| format!("invalid rules {}", path.display()))
}

async fn shutdown_signal() -> anyhow::Result<&'static str> {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        let mut term = signal(SignalKind::terminate()).context("installing SIGTERM handler")?;
        tokio::select! {
            r = tokio::signal::ctrl_c() => r.map(|()| "SIGINT").context("waiting for SIGINT"),
            _ = term.recv() => Ok("SIGTERM"),
        }
    }
    #[cfg(not(unix))]
    {
        tokio::signal::ctrl_c()
            .await
            .context("waiting for Ctrl-C")?;
        Ok("Ctrl-C")
    }
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
    let catalog = Arc::new(load_catalog(&config)?);
    let rules = Arc::new(load_rules(&config, &catalog)?);
    let writer = RotatingWriter::open(
        &config.output.log_dir,
        config.output.max_file_bytes,
        config.output.max_files,
    )
    .with_context(|| format!("opening output dir {}", config.output.log_dir.display()))?;
    let socket = UdpSocket::bind(config.ingest.udp_bind)
        .await
        .with_context(|| format!("binding UDP {}", config.ingest.udp_bind))?;

    let clock = SystemClock::new();
    let mut engine = Engine::new(clock.clone(), &config.vin, &config.model);
    engine.apply_ruleset(Arc::clone(&rules));

    let (sample_tx, sample_rx) = mpsc::channel(SAMPLE_QUEUE);
    let (record_tx, record_rx) = mpsc::channel(RECORD_QUEUE);
    let (stop_tx, stop_rx) = watch::channel(false);

    let output = output::spawn(writer, record_rx);
    let engine = tokio::spawn(engine_task::run(engine, clock, sample_rx, record_tx));
    let mut ingest = tokio::spawn(ingest::run_udp(
        socket,
        Arc::clone(&catalog),
        sample_tx,
        args.debug_decode,
        stop_rx,
    ));
    info!(
        vin = config.vin,
        model = catalog.model(),
        catalog_version = catalog.catalog_version(),
        ruleset_version = rules.version(),
        rules = rules.rules().len(),
        udp = %config.ingest.udp_bind,
        output = %config.output.log_dir.display(),
        "telemetryd started"
    );

    let ingest_result = tokio::select! {
        result = &mut ingest => result,
        signal = shutdown_signal() => {
            info!(signal = signal?, "shutting down");
            let _ = stop_tx.send(true);
            ingest.await
        }
    };

    // Ingest has stopped and dropped its sender, so the engine drains the
    // queue and exits, which in turn lets the output task flush and exit.
    let drained = tokio::time::timeout(SHUTDOWN_TIMEOUT, async {
        let engine_stats = engine.await;
        let output_stats = output.await;
        (engine_stats, output_stats)
    })
    .await;
    if drained.is_err() {
        error!("timed out draining queues at shutdown");
    }
    match ingest_result.context("ingest task panicked")? {
        Ok(stats) => stats.log(),
        Err(e) => return Err(e).context("UDP ingest failed"),
    }
    info!("telemetryd stopped");
    Ok(())
}
