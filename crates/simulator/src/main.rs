use std::net::SocketAddr;
use std::path::PathBuf;

use clap::{Parser, ValueEnum};

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
    #[arg(long, default_value_t = 50)]
    rate_hz: u32,

    /// Transport used to publish samples.
    #[arg(long, value_enum, default_value_t = Mode::Udp)]
    mode: Mode,

    /// Driving scenario to simulate.
    #[arg(long, value_enum, default_value_t = Scenario::Drive)]
    scenario: Scenario,
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

fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    eprintln!("simulator: scaffold only, args = {args:?}");
    Ok(())
}
