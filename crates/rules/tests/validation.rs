//! Rule set validation (spec Section 6.3).

mod common;

use common::*;
use protocol::RuleSet;
use rules::{compile_ruleset, ValidationError, MAX_RULES};

fn errors(rs: &RuleSet, active: Option<u64>) -> Vec<ValidationError> {
    compile_ruleset(rs, &r1s(), "R1S", active).unwrap_err().0
}

fn one_rule_errors(rule: &str) -> Vec<ValidationError> {
    errors(&ruleset(1, "R1S", &[rule]), None)
}

fn assert_invalid(rule: &str, needle: &str) {
    let errs = one_rule_errors(rule);
    assert_eq!(errs.len(), 1, "{rule}: {errs:?}");
    let msg = errs[0].to_string();
    assert!(
        msg.contains(needle),
        "{rule}: expected '{needle}' in '{msg}'"
    );
}

#[test]
fn default_ruleset_validates_against_both_catalogs() {
    let rs = RuleSet::from_json(DEFAULT_RULESET).unwrap();
    assert_eq!(rs.model, "*");
    let a = compile_ruleset(&rs, &r1s(), "R1S", None).unwrap();
    let b = compile_ruleset(&rs, &r2(), "R2", None).unwrap();
    assert_eq!(a.rules().len(), 5);
    assert_eq!(b.rules().len(), 5);
}

#[test]
fn rejects_unknown_signal() {
    assert_invalid(
        r#"{"id":"a","signal":"warp_speed","mode":"on_change"}"#,
        "unknown signal 'warp_speed'",
    );
}

#[test]
fn rejects_unknown_signal_in_condition() {
    let errs = one_rule_errors(
        r#"{"id":"a","signal":"gear","mode":"on_change","condition":"cabin_temp > 20.0"}"#,
    );
    assert_eq!(errs[0].reason(), "invalid_condition");
    assert!(errs[0].to_string().contains("unknown signal 'cabin_temp'"));
}

#[test]
fn rejects_bad_expression() {
    let errs = one_rule_errors(
        r#"{"id":"a","signal":"gear","mode":"on_change","condition":"motor_temp > && 1"}"#,
    );
    assert_eq!(errs[0].reason(), "invalid_condition");
    assert!(errs[0].to_string().contains("syntax error"));
}

#[test]
fn rejects_too_long_expression() {
    let cond = format!("motor_temp > {}", "1".repeat(600));
    let rule = format!(r#"{{"id":"a","signal":"gear","mode":"on_change","condition":"{cond}"}}"#);
    assert_eq!(one_rule_errors(&rule)[0].reason(), "invalid_condition");
}

#[test]
fn rejects_too_short_interval() {
    assert_invalid(
        r#"{"id":"a","signal":"vehicle_speed","mode":"periodic","interval_ms":99}"#,
        "interval_ms must be >= 100",
    );
    compile_ruleset(
        &ruleset(
            1,
            "R1S",
            &[r#"{"id":"a","signal":"vehicle_speed","mode":"periodic","interval_ms":100}"#],
        ),
        &r1s(),
        "R1S",
        None,
    )
    .unwrap();
}

#[test]
fn rejects_duplicate_ids() {
    let errs = errors(
        &ruleset(
            1,
            "R1S",
            &[
                r#"{"id":"a","signal":"gear","mode":"on_change"}"#,
                r#"{"id":"a","signal":"door_open","mode":"on_change"}"#,
            ],
        ),
        None,
    );
    assert!(
        errs[0].to_string().contains("duplicate rule id"),
        "{errs:?}"
    );
}

#[test]
fn rejects_too_many_rules() {
    let rules: Vec<String> = (0..=MAX_RULES)
        .map(|i| format!(r#"{{"id":"r{i}","signal":"gear","mode":"on_change"}}"#))
        .collect();
    let refs: Vec<&str> = rules.iter().map(String::as_str).collect();
    let errs = errors(&ruleset(1, "R1S", &refs), None);
    assert_eq!(errs, [ValidationError::TooManyRules(MAX_RULES + 1)]);

    let ok = compile_ruleset(&ruleset(1, "R1S", &refs[..MAX_RULES]), &r1s(), "R1S", None);
    assert_eq!(ok.unwrap().rules().len(), MAX_RULES);
}

#[test]
fn rejects_lower_or_equal_version() {
    let rs = ruleset(12, "R1S", &[]);
    for active in [12, 13] {
        let errs = errors(&rs, Some(active));
        assert_eq!(errs[0].reason(), "not_newer", "active {active}");
    }
    compile_ruleset(&rs, &r1s(), "R1S", Some(11)).unwrap();
    // First load from cache at boot has no active version to compare with.
    compile_ruleset(&rs, &r1s(), "R1S", None).unwrap();
}

#[test]
fn model_must_match_or_be_wildcard() {
    let errs = errors(&ruleset(1, "R2", &[]), None);
    assert_eq!(errs[0].reason(), "model_mismatch");
    compile_ruleset(&ruleset(1, "R1S", &[]), &r1s(), "R1S", None).unwrap();
    compile_ruleset(&ruleset(1, "*", &[]), &r1s(), "R1S", None).unwrap();
}

#[test]
fn rejects_bad_rule_ids() {
    for id in ["", "Speed", "a-b", &"a".repeat(65)] {
        let rule = format!(r#"{{"id":"{id}","signal":"gear","mode":"on_change"}}"#);
        assert_invalid(&rule, "id must match");
    }
}

#[test]
fn rejects_mode_field_mismatches() {
    assert_invalid(
        r#"{"id":"a","signal":"battery_soc","mode":"on_change","deadband":-1}"#,
        "deadband must be",
    );
    assert_invalid(
        r#"{"id":"a","signal":"gear","mode":"on_change","deadband":1}"#,
        "only to numeric signals",
    );
    assert_invalid(
        r#"{"id":"a","signal":"gear","mode":"on_change","interval_ms":1000}"#,
        "only to periodic",
    );
    assert_invalid(
        r#"{"id":"a","signal":"vehicle_speed","mode":"periodic"}"#,
        "require interval_ms",
    );
    assert_invalid(
        r#"{"id":"a","signal":"vehicle_speed","mode":"periodic","interval_ms":1000,"deadband":1}"#,
        "only to on_change",
    );
    assert_invalid(
        r#"{"id":"a","signal":"vehicle_speed","mode":"periodic","interval_ms":1000,"max_age_ms":0}"#,
        "max_age_ms must be > 0",
    );
}

#[test]
fn reports_every_problem_at_once() {
    let rs = ruleset(
        5,
        "R2",
        &[
            r#"{"id":"a","signal":"nope","mode":"on_change"}"#,
            r#"{"id":"b","signal":"vehicle_speed","mode":"periodic","interval_ms":10}"#,
        ],
    );
    let err = compile_ruleset(&rs, &r1s(), "R1S", Some(5)).unwrap_err();
    let reasons: Vec<_> = err.0.iter().map(ValidationError::reason).collect();
    assert_eq!(
        reasons,
        [
            "not_newer",
            "model_mismatch",
            "invalid_rule",
            "invalid_rule"
        ]
    );
    assert_eq!(err.reason(), "not_newer");
}
