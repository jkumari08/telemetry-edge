//! Rule set document, exactly as fetched from the cloud (see docs/RULES.md).
//!
//! These are plain serde types. Validation and compilation live in the
//! `rules` crate.

use serde::{Deserialize, Serialize};

/// `model` value that matches every vehicle model.
pub const ANY_MODEL: &str = "*";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuleSet {
    pub ruleset_version: u64,
    pub model: String,
    pub rules: Vec<Rule>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    OnChange,
    Periodic,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rule {
    pub id: String,
    pub signal: String,
    pub mode: Mode,
    /// `periodic` only: emit interval.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interval_ms: Option<u64>,
    /// `periodic` only: skip values older than this (default `3 * interval_ms`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_age_ms: Option<u64>,
    /// `on_change` only, numeric signals: minimum change to log (default 0).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deadband: Option<f64>,
    /// Optional CEL boolean expression over catalog signal names.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub condition: Option<String>,
}

impl RuleSet {
    pub fn from_json(json: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(json)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_spec_example() {
        let rs = RuleSet::from_json(
            r#"{
              "ruleset_version": 12,
              "model": "R1S",
              "rules": [
                {"id": "soc_change", "signal": "battery_soc", "mode": "on_change", "deadband": 1.0},
                {"id": "speed_1hz", "signal": "vehicle_speed", "mode": "periodic", "interval_ms": 1000},
                {"id": "hot_motor", "signal": "motor_temp", "mode": "periodic", "interval_ms": 200,
                 "condition": "motor_temp > 90.0 && gear == 'D'"}
              ]
            }"#,
        )
        .unwrap();
        assert_eq!(rs.ruleset_version, 12);
        assert_eq!(rs.rules[0].mode, Mode::OnChange);
        assert_eq!(rs.rules[1].interval_ms, Some(1000));
        assert!(rs.rules[2].condition.is_some());
        let again = RuleSet::from_json(&serde_json::to_string(&rs).unwrap()).unwrap();
        assert_eq!(again, rs);
    }

    #[test]
    fn rejects_unknown_fields_and_modes() {
        let base = r#"{"ruleset_version":1,"model":"*","rules":[RULE]}"#;
        for rule in [
            r#"{"id":"a","signal":"s","mode":"on_change","dedband":1}"#,
            r#"{"id":"a","signal":"s","mode":"sometimes"}"#,
        ] {
            assert!(
                RuleSet::from_json(&base.replace("RULE", rule)).is_err(),
                "{rule}"
            );
        }
    }
}
