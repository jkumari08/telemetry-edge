//! Rule engine task: consumes samples, fires periodic rules, emits records.

use std::collections::BTreeMap;
use std::time::Duration;

use protocol::Sample;
use rules::{Clock, Engine, EvalReport, LogRecord, SystemClock};
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

/// How long to sleep when no periodic rule is scheduled.
const IDLE_SLEEP: Duration = Duration::from_secs(60);

#[derive(Debug, Default)]
pub struct EngineStats {
    pub samples: u64,
    pub records: BTreeMap<String, u64>,
    pub eval_errors: BTreeMap<String, u64>,
    pub stale_skips: BTreeMap<String, u64>,
}

impl EngineStats {
    fn log(&self) {
        info!(
            samples = self.samples,
            records = ?self.records,
            eval_errors = ?self.eval_errors,
            stale_skips = ?self.stale_skips,
            "engine stats"
        );
    }
}

/// Runs until the sample channel closes (ingest stopped), then returns.
pub async fn run(
    mut engine: Engine<SystemClock>,
    clock: SystemClock,
    mut samples: mpsc::Receiver<Sample>,
    records: mpsc::Sender<LogRecord>,
) -> EngineStats {
    let mut stats = EngineStats::default();
    let mut report_timer = tokio::time::interval(Duration::from_secs(10));
    report_timer.tick().await;
    loop {
        let report = engine.tick();
        emit(report, &mut stats, &records).await;
        let sleep_for = engine.next_deadline_ms().map_or(IDLE_SLEEP, |due| {
            Duration::from_millis(due.saturating_sub(clock.now_ms()))
        });
        tokio::select! {
            sample = samples.recv() => match sample {
                Some(sample) => {
                    stats.samples += 1;
                    let report = engine.on_sample(&sample);
                    emit(report, &mut stats, &records).await;
                }
                None => break,
            },
            () = tokio::time::sleep(sleep_for) => {}
            _ = report_timer.tick() => stats.log(),
        }
    }
    stats.log();
    stats
}

async fn emit(report: EvalReport, stats: &mut EngineStats, out: &mpsc::Sender<LogRecord>) {
    for (rule_id, error) in report.eval_errors {
        let count = stats.eval_errors.entry(rule_id.clone()).or_default();
        *count += 1;
        if *count == 1 {
            warn!(rule_id, %error, "condition evaluation failed (treated as false)");
        }
    }
    for rule_id in report.stale_skips {
        debug!(rule_id, "periodic rule skipped: stale value");
        *stats.stale_skips.entry(rule_id).or_default() += 1;
    }
    for record in report.records {
        *stats.records.entry(record.rule_id.clone()).or_default() += 1;
        if out.send(record).await.is_err() {
            warn!("output task stopped; dropping record");
        }
    }
}
