//! Rule evaluation engine (spec Section 6.2 and 6.4).

use std::collections::HashMap;
use std::sync::Arc;

use protocol::{Sample, SignalValue};
use serde::{Deserialize, Serialize};

use crate::clock::Clock;
use crate::compile::{Behaviour, CompiledRule, CompiledRuleSet};
use crate::condition::EvalError;

/// One emitted telemetry log line (NDJSON).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LogRecord {
    /// Vehicle timestamp of the logged sample.
    pub ts_us: u64,
    pub vin: String,
    pub model: String,
    pub rule_id: String,
    pub ruleset_version: u64,
    pub signal: String,
    pub value: SignalValue,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
}

/// What happened during one engine step; the daemon turns this into
/// output lines and metrics.
#[derive(Debug, Default, PartialEq)]
pub struct EvalReport {
    pub records: Vec<LogRecord>,
    /// Rules whose condition failed to evaluate (treated as `false`).
    pub eval_errors: Vec<(String, EvalError)>,
    /// Periodic rules skipped because the latest value was too old.
    pub stale_skips: Vec<String>,
}

#[derive(Debug, Clone)]
struct Latest {
    value: SignalValue,
    timestamp_us: u64,
    received_ms: u64,
}

#[derive(Debug, Default, Clone)]
struct RuleState {
    last_logged: Option<SignalValue>,
    next_due_ms: Option<u64>,
}

pub struct Engine<C: Clock> {
    clock: C,
    vin: String,
    model: String,
    rules: Arc<CompiledRuleSet>,
    latest: HashMap<String, Latest>,
    state: HashMap<String, RuleState>,
    /// Indices of `on_change` rules per signal name.
    on_change_by_signal: HashMap<String, Vec<usize>>,
}

impl<C: Clock> Engine<C> {
    /// Creates an engine with the empty rule set.
    pub fn new(clock: C, vin: impl Into<String>, model: impl Into<String>) -> Self {
        Engine {
            clock,
            vin: vin.into(),
            model: model.into(),
            rules: Arc::new(CompiledRuleSet::empty()),
            latest: HashMap::new(),
            state: HashMap::new(),
            on_change_by_signal: HashMap::new(),
        }
    }

    pub fn ruleset(&self) -> &Arc<CompiledRuleSet> {
        &self.rules
    }

    /// Swaps in a new rule set. Runtime state (last logged value, periodic
    /// timer) is kept for rules whose definition is unchanged, reset for new
    /// or modified rules, and dropped for removed rules.
    pub fn apply_ruleset(&mut self, new: Arc<CompiledRuleSet>) {
        let now = self.clock.now_ms();
        let old_defs: HashMap<&str, &CompiledRule> =
            self.rules.rules().iter().map(|r| (r.id(), r)).collect();
        let mut state = HashMap::with_capacity(new.rules().len());
        for rule in new.rules() {
            let unchanged = old_defs
                .get(rule.id())
                .is_some_and(|old| old.def == rule.def);
            let kept = if unchanged {
                self.state.remove(rule.id())
            } else {
                None
            };
            let fresh = || RuleState {
                last_logged: None,
                next_due_ms: match rule.behaviour {
                    Behaviour::Periodic { interval_ms, .. } => {
                        Some(now.saturating_add(interval_ms))
                    }
                    Behaviour::OnChange { .. } => None,
                },
            };
            state.insert(rule.id().to_owned(), kept.unwrap_or_else(fresh));
        }
        self.state = state;
        self.on_change_by_signal.clear();
        for (i, rule) in new.rules().iter().enumerate() {
            if matches!(rule.behaviour, Behaviour::OnChange { .. }) {
                self.on_change_by_signal
                    .entry(rule.def.signal.clone())
                    .or_default()
                    .push(i);
            }
        }
        self.rules = new;
    }

    /// Records a decoded sample, then evaluates `on_change` rules for it.
    pub fn on_sample(&mut self, sample: &Sample) -> EvalReport {
        let mut report = EvalReport::default();
        self.latest.insert(
            sample.name.clone(),
            Latest {
                value: sample.value.clone(),
                timestamp_us: sample.timestamp_us,
                received_ms: self.clock.now_ms(),
            },
        );
        let rules = Arc::clone(&self.rules);
        let Some(indices) = self.on_change_by_signal.get(&sample.name) else {
            return report;
        };
        for rule in indices.iter().filter_map(|&i| rules.rules().get(i)) {
            let Behaviour::OnChange { deadband } = rule.behaviour else {
                continue;
            };
            let Some(state) = self.state.get_mut(rule.id()) else {
                continue;
            };
            let changed = match (&state.last_logged, &sample.value) {
                (None, _) => true,
                (Some(SignalValue::Num(last)), SignalValue::Num(new)) => {
                    (new - last).abs() > deadband
                }
                (Some(last), new) => last != new,
            };
            if changed && condition_passes(rule, &self.latest, &mut report) {
                state.last_logged = Some(sample.value.clone());
                report.records.push(self.record(
                    &rules,
                    rule,
                    sample.timestamp_us,
                    sample.value.clone(),
                ));
            }
        }
        report
    }

    /// Fires every `periodic` rule that is due.
    pub fn tick(&mut self) -> EvalReport {
        let mut report = EvalReport::default();
        let now = self.clock.now_ms();
        let rules = Arc::clone(&self.rules);
        for rule in rules.rules() {
            let Behaviour::Periodic {
                interval_ms,
                max_age_ms,
            } = rule.behaviour
            else {
                continue;
            };
            let Some(state) = self.state.get_mut(rule.id()) else {
                continue;
            };
            let Some(due) = state.next_due_ms.filter(|&d| d <= now) else {
                continue;
            };
            // Keep a steady cadence; if we fell behind, skip missed slots
            // rather than emitting a burst.
            let next = due.saturating_add(interval_ms);
            state.next_due_ms = Some(if next > now {
                next
            } else {
                now.saturating_add(interval_ms)
            });

            let Some(latest) = self.latest.get(&rule.def.signal) else {
                continue;
            };
            if now.saturating_sub(latest.received_ms) > max_age_ms {
                report.stale_skips.push(rule.id().to_owned());
                continue;
            }
            let (ts, value) = (latest.timestamp_us, latest.value.clone());
            if condition_passes(rule, &self.latest, &mut report) {
                state.last_logged = Some(value.clone());
                report.records.push(self.record(&rules, rule, ts, value));
            }
        }
        report
    }

    /// Earliest time (clock ms) at which [`Engine::tick`] has work to do.
    pub fn next_deadline_ms(&self) -> Option<u64> {
        self.state.values().filter_map(|s| s.next_due_ms).min()
    }

    fn record(
        &self,
        rules: &CompiledRuleSet,
        rule: &CompiledRule,
        ts_us: u64,
        value: SignalValue,
    ) -> LogRecord {
        LogRecord {
            ts_us,
            vin: self.vin.clone(),
            model: self.model.clone(),
            rule_id: rule.id().to_owned(),
            ruleset_version: rules.version(),
            signal: rule.def.signal.clone(),
            value,
            unit: rule.unit.clone(),
        }
    }
}

fn condition_passes(
    rule: &CompiledRule,
    latest: &HashMap<String, Latest>,
    report: &mut EvalReport,
) -> bool {
    let Some(condition) = &rule.condition else {
        return true;
    };
    match condition.evaluate(|name| latest.get(name).map(|l| &l.value)) {
        Ok(pass) => pass,
        Err(e) => {
            report.eval_errors.push((rule.id().to_owned(), e));
            false
        }
    }
}
