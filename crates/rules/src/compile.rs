//! Rule set validation and compilation (spec Section 6.3).

use std::collections::HashSet;

use protocol::{Catalog, Mode, Rule, RuleSet, ValueKind, ANY_MODEL};

use crate::condition::Condition;

pub const MAX_RULES: usize = 256;
pub const MIN_INTERVAL_MS: u64 = 100;

/// A validated rule, ready for the engine.
#[derive(Debug)]
pub struct CompiledRule {
    pub(crate) def: Rule,
    pub(crate) condition: Option<Condition>,
    pub(crate) unit: Option<String>,
    pub(crate) behaviour: Behaviour,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum Behaviour {
    OnChange { deadband: f64 },
    Periodic { interval_ms: u64, max_age_ms: u64 },
}

impl CompiledRule {
    pub fn def(&self) -> &Rule {
        &self.def
    }

    pub fn id(&self) -> &str {
        &self.def.id
    }

    pub fn condition(&self) -> Option<&Condition> {
        self.condition.as_ref()
    }
}

/// A validated, compiled rule set. Immutable once built; swapped atomically.
#[derive(Debug)]
pub struct CompiledRuleSet {
    version: u64,
    model: String,
    rules: Vec<CompiledRule>,
}

impl CompiledRuleSet {
    /// The rule set used before any rules are loaded: logs nothing.
    pub fn empty() -> Self {
        CompiledRuleSet {
            version: 0,
            model: ANY_MODEL.to_owned(),
            rules: Vec::new(),
        }
    }

    pub fn version(&self) -> u64 {
        self.version
    }

    pub fn model(&self) -> &str {
        &self.model
    }

    pub fn rules(&self) -> &[CompiledRule] {
        &self.rules
    }
}

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum ValidationError {
    #[error("ruleset_version {got} is not newer than the active version {active}")]
    NotNewer { got: u64, active: u64 },
    #[error("rule set model '{got}' does not match vehicle model '{vehicle}'")]
    ModelMismatch { got: String, vehicle: String },
    #[error("{0} rules exceeds the limit of {MAX_RULES}")]
    TooManyRules(usize),
    #[error("rule '{rule}': {reason}")]
    InvalidRule { rule: String, reason: String },
    #[error("rule '{rule}': condition: {reason}")]
    InvalidCondition { rule: String, reason: String },
    #[error("internal error: {0}")]
    Internal(String),
}

impl ValidationError {
    /// Metric label for `telemetryd_ruleset_update_failures_total{reason}`.
    pub fn reason(&self) -> &'static str {
        match self {
            ValidationError::NotNewer { .. } => "not_newer",
            ValidationError::ModelMismatch { .. } => "model_mismatch",
            ValidationError::TooManyRules(_) => "too_many_rules",
            ValidationError::InvalidRule { .. } => "invalid_rule",
            ValidationError::InvalidCondition { .. } => "invalid_condition",
            ValidationError::Internal(_) => "internal",
        }
    }
}

/// Every problem found in a rule set; the whole set is rejected if non-empty.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
#[error("{}", .0.iter().map(ToString::to_string).collect::<Vec<_>>().join("; "))]
pub struct ValidationErrors(pub Vec<ValidationError>);

impl ValidationErrors {
    /// Metric label of the first error.
    pub fn reason(&self) -> &'static str {
        self.0.first().map_or("internal", ValidationError::reason)
    }
}

/// Returns true if `id` matches `^[a-z0-9_]{1,64}$`.
pub fn is_valid_rule_id(id: &str) -> bool {
    (1..=64).contains(&id.len())
        && id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

/// Validates and compiles `rules` for a vehicle.
///
/// `active_version` is the currently active rule set version, used for
/// rollback protection. Pass `None` on the first load at boot (from cache).
pub fn compile_ruleset(
    rules: &RuleSet,
    catalog: &Catalog,
    vehicle_model: &str,
    active_version: Option<u64>,
) -> Result<CompiledRuleSet, ValidationErrors> {
    let mut errors = Vec::new();
    if let Some(active) = active_version {
        if rules.ruleset_version <= active {
            errors.push(ValidationError::NotNewer {
                got: rules.ruleset_version,
                active,
            });
        }
    }
    if rules.model != ANY_MODEL && rules.model != vehicle_model {
        errors.push(ValidationError::ModelMismatch {
            got: rules.model.clone(),
            vehicle: vehicle_model.to_owned(),
        });
    }
    if rules.rules.len() > MAX_RULES {
        // Don't spend CPU on the individual rules of an oversized set.
        errors.push(ValidationError::TooManyRules(rules.rules.len()));
        return Err(ValidationErrors(errors));
    }

    let compiled = crate::with_big_stack(|| {
        let mut errors = Vec::new();
        let mut seen = HashSet::new();
        let mut compiled = Vec::with_capacity(rules.rules.len());
        for rule in &rules.rules {
            if !seen.insert(rule.id.as_str()) {
                errors.push(invalid(rule, "duplicate rule id".into()));
                continue;
            }
            match compile_rule(rule, catalog) {
                Ok(c) => compiled.push(c),
                Err(e) => errors.push(e),
            }
        }
        (compiled, errors)
    });
    match compiled {
        Ok((compiled, rule_errors)) => {
            errors.extend(rule_errors);
            if errors.is_empty() {
                Ok(CompiledRuleSet {
                    version: rules.ruleset_version,
                    model: rules.model.clone(),
                    rules: compiled,
                })
            } else {
                Err(ValidationErrors(errors))
            }
        }
        Err(e) => {
            errors.push(ValidationError::Internal(e));
            Err(ValidationErrors(errors))
        }
    }
}

fn invalid(rule: &Rule, reason: String) -> ValidationError {
    ValidationError::InvalidRule {
        rule: rule.id.clone(),
        reason,
    }
}

fn compile_rule(rule: &Rule, catalog: &Catalog) -> Result<CompiledRule, ValidationError> {
    let fail = |reason: &str| Err(invalid(rule, reason.to_owned()));
    if !is_valid_rule_id(&rule.id) {
        return fail("id must match ^[a-z0-9_]{1,64}$");
    }
    let Some(signal) = catalog.by_name(&rule.signal) else {
        return Err(invalid(rule, format!("unknown signal '{}'", rule.signal)));
    };
    let behaviour = match rule.mode {
        Mode::OnChange => {
            if rule.interval_ms.is_some() || rule.max_age_ms.is_some() {
                return fail("interval_ms and max_age_ms apply only to periodic rules");
            }
            let deadband = rule.deadband.unwrap_or(0.0);
            if !deadband.is_finite() || deadband < 0.0 {
                return fail("deadband must be a finite number >= 0");
            }
            if rule.deadband.is_some() && signal.kind() != ValueKind::Num {
                return fail("deadband applies only to numeric signals");
            }
            Behaviour::OnChange { deadband }
        }
        Mode::Periodic => {
            if rule.deadband.is_some() {
                return fail("deadband applies only to on_change rules");
            }
            let Some(interval_ms) = rule.interval_ms else {
                return fail("periodic rules require interval_ms");
            };
            if interval_ms < MIN_INTERVAL_MS {
                return Err(invalid(
                    rule,
                    format!("interval_ms must be >= {MIN_INTERVAL_MS}"),
                ));
            }
            let max_age_ms = rule
                .max_age_ms
                .unwrap_or_else(|| interval_ms.saturating_mul(3));
            if max_age_ms == 0 {
                return fail("max_age_ms must be > 0");
            }
            Behaviour::Periodic {
                interval_ms,
                max_age_ms,
            }
        }
    };
    let condition = rule
        .condition
        .as_deref()
        .map(|src| Condition::compile(src, catalog))
        .transpose()
        .map_err(|reason| ValidationError::InvalidCondition {
            rule: rule.id.clone(),
            reason,
        })?;
    Ok(CompiledRule {
        def: rule.clone(),
        condition,
        unit: signal.unit().map(str::to_owned),
        behaviour,
    })
}
