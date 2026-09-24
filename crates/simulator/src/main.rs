mod garbage;
mod rng;
mod vehicle;

use std::collections::BTreeMap;
use std::net::{SocketAddr, UdpSocket};
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context};
use clap::{Parser, ValueEnum};
use protocol::{encode_packet, Catalog};
use tracing::{info, warn};

use crate::rng::Rng;
use crate::vehicle::{Behaviour, Vehicle, KNOWN_SIGNALS};

/// Fake vehicle bus: generates smoothly varying signals and sends them to
/// telemetryd as binary UDP frames or a gRPC stream.
#[derive(Debug, Parser)]
#[command(version, about)]
struct Args {
    /// Address of the telemetryd ingest endpoint.
    #[arg(long, default_value = "127.0.0.1:5005")]
    target: SocketAddr,

    /// Signal catalog for the simulated vehicle model.
    #[arg(long, default_value = "catalogs/r1s.json")]
    catalog: PathBuf,

    /// Packets sent per second.
    #[arg(long, default_value_t = 50, value_parser = clap::value_parser!(u32).range(1..=10_000))]
    rate_hz: u32,

    /// Transport used to publish samples.
    #[arg(long, value_enum, default_value_t = Mode::Udp)]
    mode: Mode,

    /// Driving scenario to simulate.
    #[arg(long, value_enum, default_value_t = Scenario::Drive)]
    scenario: Scenario,

    /// Stop after this many seconds (default: run until interrupted).
    #[arg(long)]
    duration_s: Option<f64>,

    /// Random seed, for reproducible runs (default: derived from the clock).
    #[arg(long)]
    seed: Option<u64>,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum Mode {
    Udp,
    Grpc,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum Scenario {
    /// Speed ramps, gear P->D->P, motor warms up, SOC drains.
    Drive,
    /// Parked vehicle: gear P, zero speed, occasional door toggles.
    Park,
    /// Malformed packets mixed with valid ones.
    Garbage,
}

fn now_us() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_micros() as u64)
}

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .with_writer(std::io::stderr)
        .init();
    let args = Args::parse();

    if matches!(args.mode, Mode::Grpc) {
        bail!("--mode grpc is not implemented yet (milestone M11); use --mode udp");
    }

    let json = std::fs::read_to_string(&args.catalog)
        .with_context(|| format!("reading catalog {}", args.catalog.display()))?;
    let catalog = Catalog::from_json(&json)
        .with_context(|| format!("loading catalog {}", args.catalog.display()))?;
    let unsimulated: Vec<&str> = catalog
        .signals()
        .map(|s| s.name())
        .filter(|n| !KNOWN_SIGNALS.contains(n))
        .collect();
    if !unsimulated.is_empty() {
        warn!(
            ?unsimulated,
            "catalog signals the simulator does not generate"
        );
    }

    let bind = if args.target.is_ipv4() {
        "0.0.0.0:0"
    } else {
        "[::]:0"
    };
    let socket = UdpSocket::bind(bind).context("binding UDP socket")?;
    let seed = args.seed.unwrap_or_else(now_us);
    info!(
        target = %args.target,
        model = catalog.model(),
        scenario = ?args.scenario,
        rate_hz = args.rate_hz,
        seed,
        "simulator started"
    );

    let behaviour = match args.scenario {
        Scenario::Park => Behaviour::Park,
        Scenario::Drive | Scenario::Garbage => Behaviour::Drive,
    };
    let mut vehicle = Vehicle::new(behaviour);
    let mut rng = Rng::new(seed);
    let period = Duration::from_secs_f64(1.0 / f64::from(args.rate_hz));
    let start = Instant::now();
    let mut next_tick = start;
    let mut last_report = start;
    let (mut packets, mut bytes_sent, mut send_errors) = (0u64, 0u64, 0u64);
    let mut garbage_counts: BTreeMap<String, u64> = BTreeMap::new();

    loop {
        if args
            .duration_s
            .is_some_and(|d| start.elapsed().as_secs_f64() >= d)
        {
            break;
        }
        vehicle.step(period.as_secs_f64(), &mut rng);
        let frames = vehicle.frames(&catalog, now_us())?;
        let packet = match args.scenario {
            Scenario::Garbage => {
                let (kind, bytes) = garbage::packet(&frames, &catalog, &mut rng)?;
                let key = kind.map_or("valid".to_owned(), |k| format!("{k:?}"));
                *garbage_counts.entry(key).or_default() += 1;
                bytes
            }
            Scenario::Drive | Scenario::Park => encode_packet(&frames)?,
        };
        match socket.send_to(&packet, args.target) {
            Ok(n) => {
                packets += 1;
                bytes_sent += n as u64;
            }
            Err(e) => {
                send_errors += 1;
                if send_errors == 1 {
                    warn!(error = %e, "UDP send failed (is telemetryd running?)");
                }
            }
        }

        if last_report.elapsed() >= Duration::from_secs(5) {
            last_report = Instant::now();
            info!(
                packets,
                bytes_sent,
                send_errors,
                speed_kmh = format!("{:.1}", vehicle.speed_kmh),
                gear = ?vehicle.gear,
                motor_temp_c = format!("{:.1}", vehicle.motor_temp_c),
                soc_pct = format!("{:.1}", vehicle.soc_pct),
                "progress"
            );
            if !garbage_counts.is_empty() {
                info!(?garbage_counts, "garbage mix");
            }
        }

        next_tick += period;
        let now = Instant::now();
        if next_tick > now {
            std::thread::sleep(next_tick - now);
        } else {
            next_tick = now; // Fell behind; don't try to catch up in a burst.
        }
    }
    info!(packets, bytes_sent, send_errors, "simulator finished");
    Ok(())
}
