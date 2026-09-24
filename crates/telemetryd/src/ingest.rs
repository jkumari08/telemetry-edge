//! UDP ingest: receive datagrams, decode them, and forward samples.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use protocol::wire::MAX_DATAGRAM_LEN;
use protocol::{decode_packet, Catalog, Sample};
use tokio::net::UdpSocket;
use tokio::sync::{mpsc, watch};
use tracing::{info, warn};

/// Running totals, reported periodically and at shutdown.
#[derive(Debug, Default)]
pub struct IngestStats {
    pub packets: u64,
    pub bytes: u64,
    pub frames_decoded: u64,
    pub unknown_signals: u64,
    pub decode_errors: BTreeMap<&'static str, u64>,
}

impl IngestStats {
    pub fn log(&self) {
        info!(
            packets = self.packets,
            bytes = self.bytes,
            frames_decoded = self.frames_decoded,
            unknown_signals = self.unknown_signals,
            decode_errors = ?self.decode_errors,
            "ingest stats"
        );
    }
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

/// Receives and decodes datagrams until `stop` flips to true, forwarding
/// samples to the engine. Dropping `samples` on return lets the engine
/// drain and finish.
pub async fn run_udp(
    socket: UdpSocket,
    catalog: Arc<Catalog>,
    samples: mpsc::Sender<Sample>,
    debug_decode: bool,
    mut stop: watch::Receiver<bool>,
) -> std::io::Result<IngestStats> {
    let mut stats = IngestStats::default();
    // Larger than the protocol maximum so oversize datagrams are received
    // (and rejected as `oversize`) rather than silently truncated to a valid length.
    let mut buf = vec![0u8; MAX_DATAGRAM_LEN * 2];
    let mut report = tokio::time::interval(Duration::from_secs(10));
    report.tick().await;
    loop {
        tokio::select! {
            recv = socket.recv_from(&mut buf) => {
                let (len, peer) = recv?;
                stats.packets += 1;
                stats.bytes += len as u64;
                let datagram = buf.get(..len).unwrap_or_default();
                match decode_packet(datagram, &catalog) {
                    Err(e) => {
                        *stats.decode_errors.entry(e.reason()).or_default() += 1;
                        if debug_decode {
                            warn!(%peer, reason = e.reason(), error = %e, "packet rejected");
                        }
                    }
                    Ok(d) => {
                        stats.unknown_signals += d.unknown_signals as u64;
                        stats.frames_decoded += d.samples.len() as u64;
                        for e in &d.errors {
                            *stats.decode_errors.entry(e.kind.reason()).or_default() += 1;
                            if debug_decode {
                                warn!(%peer, reason = e.kind.reason(), frame = e.index,
                                    signal_id = ?e.signal_id, "frame rejected");
                            }
                        }
                        if debug_decode && d.unknown_signals > 0 {
                            warn!(%peer, count = d.unknown_signals, "unknown signal ids skipped");
                        }
                        for sample in d.samples {
                            if debug_decode {
                                print_sample(&catalog, &sample);
                            }
                            if samples.send(sample).await.is_err() {
                                return Ok(stats); // engine gone
                            }
                        }
                    }
                }
            }
            _ = report.tick() => stats.log(),
            _ = stop.changed() => break,
        }
    }
    Ok(stats)
}
