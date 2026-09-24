//! Engine semantics with a fake clock (spec Section 6.2 and 6.4).

mod common;

use std::sync::Arc;

use common::*;
use protocol::{RuleSet, SignalValue};
use rules::{compile_ruleset, Engine, EvalError, FakeClock, LogRecord};

const SOC: &str = r#"{"id":"soc_change","signal":"battery_soc","mode":"on_change","deadband":1.0}"#;
const GEAR: &str = r#"{"id":"gear_change","signal":"gear","mode":"on_change"}"#;
const DOOR: &str = r#"{"id":"door","signal":"door_open","mode":"on_change"}"#;
const SPEED_1HZ: &str =
    r#"{"id":"speed_1hz","signal":"vehicle_speed","mode":"periodic","interval_ms":1000}"#;
const HOT_MOTOR: &str = r#"{"id":"hot_motor","signal":"motor_temp","mode":"periodic","interval_ms":200,"condition":"motor_temp > 90.0 && gear == 'D'"}"#;
const LOW_SOC_DRIVE: &str = r#"{"id":"low_soc_drive","signal":"vehicle_speed","mode":"on_change","deadband":0.5,"condition":"gear == 'D' && battery_soc < 20.0"}"#;

fn values(records: &[LogRecord]) -> Vec<SignalValue> {
    records.iter().map(|r| r.value.clone()).collect()
}

// ------------------------------------------------------------------ on_change

#[test]
fn on_change_logs_first_value() {
    let (mut e, _) = engine(&[SOC]);
    let r = e.on_sample(&num("battery_soc", 50.0, 111));
    assert_eq!(
        r.records,
        [LogRecord {
            ts_us: 111,
            vin: "DEV0000001".into(),
            model: "R1S".into(),
            rule_id: "soc_change".into(),
            ruleset_version: 1,
            signal: "battery_soc".into(),
            value: SignalValue::Num(50.0),
            unit: Some("%".into()),
        }]
    );
}

#[test]
fn on_change_suppresses_within_deadband() {
    let (mut e, _) = engine(&[SOC]);
    e.on_sample(&num("battery_soc", 50.0, 1));
    for v in [50.5, 49.2, 51.0, 49.0] {
        // |v - 50| <= 1.0 is inside the deadband (strictly greater is required)
        assert!(
            e.on_sample(&num("battery_soc", v, 2)).records.is_empty(),
            "{v}"
        );
    }
}

#[test]
fn on_change_logs_drift_relative_to_last_logged() {
    let (mut e, _) = engine(&[SOC]);
    e.on_sample(&num("battery_soc", 50.0, 1));
    // Each step is below the deadband, but the drift from the last *logged*
    // value eventually exceeds it.
    assert!(e.on_sample(&num("battery_soc", 50.4, 2)).records.is_empty());
    assert!(e.on_sample(&num("battery_soc", 50.8, 3)).records.is_empty());
    let r = e.on_sample(&num("battery_soc", 51.2, 4));
    assert_eq!(values(&r.records), [SignalValue::Num(51.2)]);
    // Now relative to 51.2.
    assert!(e.on_sample(&num("battery_soc", 52.0, 5)).records.is_empty());
    assert_eq!(e.on_sample(&num("battery_soc", 50.1, 6)).records.len(), 1);
}

#[test]
fn on_change_default_deadband_logs_any_change() {
    let speed = r#"{"id":"s","signal":"vehicle_speed","mode":"on_change"}"#;
    let (mut e, _) = engine(&[speed]);
    assert_eq!(e.on_sample(&num("vehicle_speed", 10.0, 1)).records.len(), 1);
    assert!(e
        .on_sample(&num("vehicle_speed", 10.0, 2))
        .records
        .is_empty());
    assert_eq!(
        e.on_sample(&num("vehicle_speed", 10.01, 3)).records.len(),
        1
    );
}

#[test]
fn on_change_enum_and_bool() {
    let (mut e, _) = engine(&[GEAR, DOOR]);
    let mut logged = Vec::new();
    for s in [
        label("gear", "P", 1),
        label("gear", "P", 2),
        label("gear", "D", 3),
        label("gear", "D", 4),
        label("gear", "P", 5),
        flag("door_open", false, 6),
        flag("door_open", false, 7),
        flag("door_open", true, 8),
    ] {
        logged.extend(e.on_sample(&s).records);
    }
    let ts: Vec<u64> = logged.iter().map(|r| r.ts_us).collect();
    assert_eq!(ts, [1, 3, 5, 6, 8]);
    assert_eq!(logged[1].value, SignalValue::Enum("D".into()));
    assert_eq!(logged[1].unit, None);
}

#[test]
fn on_change_ignores_other_signals() {
    let (mut e, _) = engine(&[SOC]);
    assert!(e
        .on_sample(&num("vehicle_speed", 10.0, 1))
        .records
        .is_empty());
}

// ------------------------------------------------------------------- periodic

#[test]
fn periodic_fires_at_interval() {
    let (mut e, clock) = engine(&[SPEED_1HZ]);
    e.on_sample(&num("vehicle_speed", 42.0, 7));
    assert_eq!(e.next_deadline_ms(), Some(1000));
    let mut fired_at = Vec::new();
    for t in (0..=3500).step_by(50) {
        clock.set(t);
        if !e.tick().records.is_empty() {
            fired_at.push(t);
            e.on_sample(&num("vehicle_speed", 42.0, t * 1000)); // keep fresh
        }
    }
    assert_eq!(fired_at, [1000, 2000, 3000]);
    assert_eq!(e.next_deadline_ms(), Some(4000));
}

#[test]
fn periodic_logs_latest_value_with_its_timestamp() {
    let (mut e, clock) = engine(&[SPEED_1HZ]);
    e.on_sample(&num("vehicle_speed", 10.0, 100));
    clock.set(400);
    e.on_sample(&num("vehicle_speed", 20.0, 500));
    clock.set(1000);
    let r = e.tick();
    assert_eq!(values(&r.records), [SignalValue::Num(20.0)]);
    assert_eq!(r.records[0].ts_us, 500);
    assert_eq!(r.records[0].unit.as_deref(), Some("km/h"));
}

#[test]
fn periodic_skips_stale_values() {
    let (mut e, clock) = engine(&[SPEED_1HZ]);
    e.on_sample(&num("vehicle_speed", 10.0, 1)); // received at t=0
                                                 // Default max_age = 3 * interval = 3000 ms.
    for (t, expect_log) in [(1000, true), (2000, true), (3000, true), (4000, false)] {
        clock.set(t);
        let r = e.tick();
        assert_eq!(r.records.len(), usize::from(expect_log), "t={t}");
        assert_eq!(r.stale_skips.len(), usize::from(!expect_log), "t={t}");
    }
    assert_eq!(e.tick().stale_skips, Vec::<String>::new()); // not due again yet
    clock.set(5000);
    assert_eq!(e.tick().stale_skips, ["speed_1hz"]);
    // A fresh sample makes it log again.
    e.on_sample(&num("vehicle_speed", 11.0, 2));
    clock.set(6000);
    assert_eq!(e.tick().records.len(), 1);
}

#[test]
fn periodic_custom_max_age() {
    let rule = r#"{"id":"p","signal":"vehicle_speed","mode":"periodic","interval_ms":1000,"max_age_ms":500}"#;
    let (mut e, clock) = engine(&[rule]);
    clock.set(600);
    e.on_sample(&num("vehicle_speed", 10.0, 1));
    clock.set(1000); // age 400 <= 500: fresh
    assert_eq!(e.tick().records.len(), 1);
    clock.set(2000); // age 1400 > 500: stale (the default 3000 would still log)
    let r = e.tick();
    assert!(r.records.is_empty());
    assert_eq!(r.stale_skips, ["p"]);
}

#[test]
fn periodic_without_any_value_does_nothing() {
    let (mut e, clock) = engine(&[SPEED_1HZ]);
    clock.set(1000);
    assert_eq!(e.tick(), Default::default());
}

#[test]
fn periodic_skips_missed_slots_instead_of_bursting() {
    let (mut e, clock) = engine(&[SPEED_1HZ]);
    clock.set(5400);
    e.on_sample(&num("vehicle_speed", 10.0, 1));
    assert_eq!(e.tick().records.len(), 1);
    assert_eq!(e.tick().records.len(), 0);
    assert_eq!(e.next_deadline_ms(), Some(6400));
}

// ------------------------------------------------------------------ condition

#[test]
fn condition_true_and_false() {
    let (mut e, clock) = engine(&[HOT_MOTOR]);
    e.on_sample(&num("motor_temp", 95.0, 1));
    e.on_sample(&label("gear", "D", 1));
    clock.set(200);
    assert_eq!(e.tick().records.len(), 1);

    e.on_sample(&label("gear", "P", 2));
    clock.set(400);
    assert!(e.tick().records.is_empty());

    e.on_sample(&label("gear", "D", 3));
    e.on_sample(&num("motor_temp", 85.0, 3));
    clock.set(600);
    assert!(e.tick().records.is_empty());
}

#[test]
fn condition_with_missing_signal_is_false() {
    let (mut e, clock) = engine(&[HOT_MOTOR]);
    e.on_sample(&num("motor_temp", 95.0, 1)); // no gear yet
    clock.set(200);
    let r = e.tick();
    assert!(r.records.is_empty());
    assert!(r.eval_errors.is_empty());
}

#[test]
fn condition_runtime_error_is_false_and_reported() {
    let rule = r#"{"id":"bad","signal":"motor_temp","mode":"on_change","condition":"gear > 1"}"#;
    let (mut e, _) = engine(&[rule]);
    e.on_sample(&label("gear", "D", 1));
    let r = e.on_sample(&num("motor_temp", 95.0, 1));
    assert!(r.records.is_empty());
    assert_eq!(r.eval_errors.len(), 1);
    assert_eq!(r.eval_errors[0].0, "bad");
    assert!(matches!(r.eval_errors[0].1, EvalError::Execution(_)));
}

#[test]
fn on_change_condition_false_does_not_update_last_logged() {
    let (mut e, _) = engine(&[LOW_SOC_DRIVE]);
    e.on_sample(&num("battery_soc", 15.0, 1));
    e.on_sample(&label("gear", "P", 1));
    assert!(e
        .on_sample(&num("vehicle_speed", 0.0, 2))
        .records
        .is_empty());
    e.on_sample(&label("gear", "D", 3));
    // First qualifying sample logs even though an earlier one was suppressed.
    let r = e.on_sample(&num("vehicle_speed", 0.2, 4));
    assert_eq!(values(&r.records), [SignalValue::Num(0.2)]);
    assert!(e
        .on_sample(&num("vehicle_speed", 0.6, 5))
        .records
        .is_empty());
    assert_eq!(e.on_sample(&num("vehicle_speed", 0.8, 6)).records.len(), 1);
}

// --------------------------------------------------------------------- swaps

#[test]
fn swap_preserves_state_for_unchanged_rules() {
    let (mut e, clock) = engine(&[GEAR, SPEED_1HZ]);
    e.on_sample(&label("gear", "D", 1));
    e.on_sample(&num("vehicle_speed", 10.0, 1));
    clock.set(600);
    e.apply_ruleset(compiled(2, &[GEAR, SPEED_1HZ, SOC]));
    assert_eq!(e.ruleset().version(), 2);
    // gear_change still remembers "D": no duplicate log.
    assert!(e.on_sample(&label("gear", "D", 2)).records.is_empty());
    // speed_1hz timer was not reset by the swap: still due at 1000, not 1600.
    assert_eq!(e.next_deadline_ms(), Some(1000));
    clock.set(1000);
    let r = e.tick();
    assert_eq!(r.records.len(), 1);
    assert_eq!(r.records[0].ruleset_version, 2);
}

#[test]
fn swap_resets_modified_and_new_rules() {
    let (mut e, clock) = engine(&[SOC, SPEED_1HZ]);
    e.on_sample(&num("battery_soc", 50.0, 1));
    e.on_sample(&num("vehicle_speed", 10.0, 1));
    clock.set(700);
    let soc_modified =
        r#"{"id":"soc_change","signal":"battery_soc","mode":"on_change","deadband":2.0}"#;
    let speed_modified =
        r#"{"id":"speed_1hz","signal":"vehicle_speed","mode":"periodic","interval_ms":100}"#;
    e.apply_ruleset(compiled(2, &[soc_modified, speed_modified]));
    // Modified on_change rule forgot its last logged value.
    assert_eq!(e.on_sample(&num("battery_soc", 50.0, 2)).records.len(), 1);
    // Modified periodic rule restarts its timer from the swap time.
    assert_eq!(e.next_deadline_ms(), Some(800));
}

#[test]
fn swap_drops_removed_rules() {
    let (mut e, clock) = engine(&[GEAR, SPEED_1HZ]);
    e.on_sample(&num("vehicle_speed", 10.0, 1));
    e.apply_ruleset(compiled(2, &[GEAR]));
    assert_eq!(e.next_deadline_ms(), None);
    clock.set(5000);
    assert!(e.tick().records.is_empty());
    // Re-adding the rule later starts from scratch.
    e.apply_ruleset(compiled(3, &[GEAR, SPEED_1HZ]));
    assert_eq!(e.next_deadline_ms(), Some(6000));
}

#[test]
fn signal_values_survive_swaps() {
    let (mut e, clock) = engine(&[]);
    e.on_sample(&num("motor_temp", 95.0, 1));
    e.on_sample(&label("gear", "D", 1));
    e.apply_ruleset(compiled(2, &[HOT_MOTOR]));
    clock.set(200);
    assert_eq!(e.tick().records.len(), 1);
}

// ------------------------------------------------------------ both catalogs

#[test]
fn same_ruleset_drives_both_models() {
    let rs = RuleSet::from_json(DEFAULT_RULESET).unwrap();
    for (cat, model) in [(r1s(), "R1S"), (r2(), "R2")] {
        let clock = FakeClock::new(0);
        let mut e = Engine::new(clock.clone(), "VIN", model);
        e.apply_ruleset(Arc::new(compile_ruleset(&rs, &cat, model, None).unwrap()));
        clock.set(900); // fresh for hot_motor's 600 ms default max age
        e.on_sample(&label("gear", "D", 1));
        e.on_sample(&num("motor_temp", 95.0, 1));
        e.on_sample(&num("vehicle_speed", 80.0, 1));
        clock.set(1000);
        let mut ids: Vec<String> = e.tick().records.into_iter().map(|r| r.rule_id).collect();
        ids.sort();
        assert_eq!(ids, ["hot_motor", "speed_1hz"], "{model}");
    }
}

#[test]
fn log_record_json_matches_spec() {
    let (mut e, _) = engine(&[SOC]);
    let r = e.on_sample(&num("battery_soc", 63.5, 1_758_650_000_123_456));
    let json = serde_json::to_string(&r.records[0]).unwrap();
    assert_eq!(
        json,
        r#"{"ts_us":1758650000123456,"vin":"DEV0000001","model":"R1S","rule_id":"soc_change","ruleset_version":1,"signal":"battery_soc","value":63.5,"unit":"%"}"#
    );
}
