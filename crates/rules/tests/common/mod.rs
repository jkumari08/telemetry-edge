#![allow(dead_code)]

use std::sync::Arc;

use protocol::{Catalog, RuleSet, Sample, SignalValue};
use rules::{compile_ruleset, CompiledRuleSet, Engine, FakeClock};

pub const R1S: &str = include_str!("../../../../catalogs/r1s.json");
pub const R2: &str = include_str!("../../../../catalogs/r2.json");
pub const DEFAULT_RULESET: &str = include_str!("../../../../rulesets/default.json");

pub fn r1s() -> Catalog {
    Catalog::from_json(R1S).unwrap()
}

pub fn r2() -> Catalog {
    Catalog::from_json(R2).unwrap()
}

/// Builds a rule set JSON document from rule objects.
pub fn ruleset(version: u64, model: &str, rules: &[&str]) -> RuleSet {
    RuleSet::from_json(&format!(
        r#"{{"ruleset_version":{version},"model":"{model}","rules":[{}]}}"#,
        rules.join(",")
    ))
    .unwrap()
}

pub fn compiled(version: u64, rules: &[&str]) -> Arc<CompiledRuleSet> {
    Arc::new(compile_ruleset(&ruleset(version, "R1S", rules), &r1s(), "R1S", None).unwrap())
}

pub fn engine(rules: &[&str]) -> (Engine<FakeClock>, FakeClock) {
    let clock = FakeClock::new(0);
    let mut engine = Engine::new(clock.clone(), "DEV0000001", "R1S");
    engine.apply_ruleset(compiled(1, rules));
    (engine, clock)
}

pub fn num(name: &str, v: f64, ts: u64) -> Sample {
    sample(name, SignalValue::Num(v), ts)
}

pub fn label(name: &str, v: &str, ts: u64) -> Sample {
    sample(name, SignalValue::Enum(v.into()), ts)
}

pub fn flag(name: &str, v: bool, ts: u64) -> Sample {
    sample(name, SignalValue::Bool(v), ts)
}

pub fn sample(name: &str, value: SignalValue, ts: u64) -> Sample {
    Sample {
        signal_id: 0,
        name: name.into(),
        timestamp_us: ts,
        value,
    }
}
