//! Rule conditions: a restricted subset of CEL.
//!
//! Rules arrive over the network, so conditions are treated as untrusted:
//! - at most [`MAX_CONDITION_CHARS`] characters;
//! - only signal identifiers, scalar literals and the operators in
//!   [`ALLOWED_OPERATORS`] (no functions, macros, lists, maps or field access);
//! - AST nesting depth at most [`MAX_CONDITION_DEPTH`], which bounds the
//!   interpreter's recursion at evaluation time.
//!
//! The CEL parser itself recurses deeply on nested input, so [`Condition::compile`]
//! must run on a thread with a large stack (see `compile_ruleset`).

use std::collections::BTreeSet;

use cel::common::ast::{Expr, IdedExpr, LiteralValue};
use cel::{Context, Program, Value};
use protocol::{Catalog, SignalValue};

pub const MAX_CONDITION_CHARS: usize = 512;
pub const MAX_CONDITION_DEPTH: usize = 24;

pub const ALLOWED_OPERATORS: [&str; 16] = [
    "_&&_", "_||_", "!_", "-_", "_==_", "_!=_", "_<_", "_<=_", "_>_", "_>=_", "_+_", "_-_", "_*_",
    "_/_", "_%_", "_?_:_",
];

/// A compiled, validated condition.
#[derive(Debug)]
pub struct Condition {
    program: Program,
    signals: Vec<String>,
}

// The engine and daemon share compiled rule sets across threads.
const _: () = {
    const fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<Condition>();
};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EvalError {
    #[error("evaluation failed: {0}")]
    Execution(String),
    #[error("condition returned a non-boolean value: {0}")]
    NotBool(String),
}

impl Condition {
    /// Parses and validates `source` against `catalog`.
    pub(crate) fn compile(source: &str, catalog: &Catalog) -> Result<Self, String> {
        let chars = source.chars().count();
        if chars > MAX_CONDITION_CHARS {
            return Err(format!(
                "condition is {chars} characters, limit is {MAX_CONDITION_CHARS}"
            ));
        }
        if source.trim().is_empty() {
            return Err("condition is empty".into());
        }
        let program = Program::compile(source).map_err(|e| format!("syntax error: {e}"))?;
        let signals = check_ast(program.expression(), catalog)?;
        Ok(Condition { program, signals })
    }

    /// Catalog signals referenced by the condition, sorted and deduplicated.
    pub fn signals(&self) -> &[String] {
        &self.signals
    }

    /// Evaluates against the latest signal values. A referenced signal with
    /// no value yet makes the condition `false` (not an error).
    pub fn evaluate<'a>(
        &self,
        lookup: impl Fn(&str) -> Option<&'a SignalValue>,
    ) -> Result<bool, EvalError> {
        let mut ctx = Context::empty();
        for name in &self.signals {
            let Some(value) = lookup(name) else {
                return Ok(false);
            };
            let value = match value {
                SignalValue::Bool(b) => Value::Bool(*b),
                SignalValue::Num(n) => Value::Float(*n),
                SignalValue::Enum(s) => Value::String(s.clone().into()),
            };
            ctx.add_variable_from_value(name.as_str(), value);
        }
        match self.program.execute(&ctx) {
            Ok(Value::Bool(b)) => Ok(b),
            Ok(other) => Err(EvalError::NotBool(format!("{other:?}"))),
            Err(e) => Err(EvalError::Execution(e.to_string())),
        }
    }
}

/// Walks the AST iteratively, enforcing the allowlist and depth limit.
/// Returns the referenced signal names.
fn check_ast(root: &IdedExpr, catalog: &Catalog) -> Result<Vec<String>, String> {
    let mut signals = BTreeSet::new();
    let mut stack = vec![(root, 1usize)];
    while let Some((node, depth)) = stack.pop() {
        if depth > MAX_CONDITION_DEPTH {
            return Err(format!(
                "condition is nested too deeply (limit {MAX_CONDITION_DEPTH})"
            ));
        }
        match &node.expr {
            Expr::Ident(name) => {
                if catalog.by_name(name).is_none() {
                    return Err(format!("unknown signal '{name}'"));
                }
                signals.insert(name.clone());
            }
            Expr::Literal(lit) => match lit {
                LiteralValue::Boolean(_)
                | LiteralValue::Double(_)
                | LiteralValue::Int(_)
                | LiteralValue::UInt(_)
                | LiteralValue::String(_) => {}
                LiteralValue::Bytes(_) | LiteralValue::Null => {
                    return Err("bytes and null literals are not allowed".into())
                }
            },
            Expr::Call(call) => {
                if call.target.is_some() || !ALLOWED_OPERATORS.contains(&call.func_name.as_str()) {
                    return Err(format!(
                        "'{}' is not allowed; only comparison, logical and arithmetic operators",
                        call.func_name
                    ));
                }
                stack.extend(call.args.iter().map(|a| (a, depth + 1)));
            }
            Expr::Select(_) => return Err("field access is not allowed".into()),
            Expr::Comprehension(_) => return Err("macros are not allowed".into()),
            Expr::List(_) | Expr::Map(_) | Expr::Struct(_) => {
                return Err("list, map and struct literals are not allowed".into())
            }
            Expr::Unspecified => return Err("unsupported expression".into()),
        }
    }
    Ok(signals.into_iter().collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    const R1S: &str = include_str!("../../../catalogs/r1s.json");

    fn compile(src: &str) -> Result<Condition, String> {
        crate::with_big_stack(|| Condition::compile(src, &Catalog::from_json(R1S).unwrap()))
            .unwrap()
    }

    fn eval(src: &str, values: &[(&str, SignalValue)]) -> Result<bool, EvalError> {
        compile(src)
            .unwrap()
            .evaluate(|n| values.iter().find(|(k, _)| *k == n).map(|(_, v)| v))
    }

    fn num(n: f64) -> SignalValue {
        SignalValue::Num(n)
    }

    fn label(s: &str) -> SignalValue {
        SignalValue::Enum(s.into())
    }

    #[test]
    fn evaluates_numbers_enums_and_bools() {
        let hot = [("motor_temp", num(95.0)), ("gear", label("D"))];
        assert_eq!(eval("motor_temp > 90.0 && gear == 'D'", &hot), Ok(true));
        assert_eq!(eval("motor_temp > 90 && gear == 'D'", &hot), Ok(true));
        let cool = [("motor_temp", num(80.0)), ("gear", label("D"))];
        assert_eq!(eval("motor_temp > 90 && gear == 'D'", &cool), Ok(false));
        let door = [("door_open", SignalValue::Bool(true))];
        assert_eq!(eval("door_open", &door), Ok(true));
        assert_eq!(eval("!door_open", &door), Ok(false));
        assert_eq!(
            eval(
                "vehicle_speed * 2.0 - 10.0 >= 50.0",
                &[("vehicle_speed", num(30.0))]
            ),
            Ok(true)
        );
    }

    #[test]
    fn missing_signal_is_false_not_error() {
        assert_eq!(
            eval("motor_temp > 90 && gear == 'D'", &[("gear", label("D"))]),
            Ok(false)
        );
    }

    #[test]
    fn runtime_type_errors_are_errors() {
        let r = eval("gear > 1", &[("gear", label("D"))]);
        assert!(matches!(r, Err(EvalError::Execution(_))), "{r:?}");
        let r = eval("motor_temp + 1.0", &[("motor_temp", num(1.0))]);
        assert!(matches!(r, Err(EvalError::NotBool(_))), "{r:?}");
    }

    #[test]
    fn records_referenced_signals() {
        let c = compile("gear == 'D' && motor_temp > 90.0 || gear == 'R'").unwrap();
        assert_eq!(c.signals(), ["gear", "motor_temp"]);
    }

    #[test]
    fn rejects_unsafe_or_invalid_conditions() {
        for (src, needle) in [
            ("unknown_sig > 1", "unknown signal"),
            ("motor_temp >", "syntax error"),
            ("size(gear) == 1", "not allowed"),
            ("gear.size() == 1", "not allowed"),
            ("[1, 2].all(x, x > 0)", "not allowed"),
            ("motor_temp in [1, 2]", "not allowed"),
            ("{'a': 1}['a'] == 1", "not allowed"),
            ("gear.name == 'D'", "field access"),
            ("gear == null", "null"),
            ("   ", "empty"),
        ] {
            let err = compile(src).unwrap_err();
            assert!(err.contains(needle), "{src}: {err}");
        }
    }

    #[test]
    fn enforces_length_limit() {
        let long = format!("motor_temp > {}", "1".repeat(MAX_CONDITION_CHARS));
        assert!(compile(&long).unwrap_err().contains("characters"));
        let ok = format!("motor_temp > 1{}", " ".repeat(MAX_CONDITION_CHARS - 14));
        assert_eq!(ok.chars().count(), MAX_CONDITION_CHARS);
        compile(&ok).unwrap();
    }

    /// Inputs like these overflowed a 2 MiB stack in the unguarded parser or
    /// interpreter (debug build) during investigation. Compiling must not
    /// crash, and anything accepted must evaluate safely on a 2 MiB thread
    /// (the size of a tokio worker's stack).
    #[test]
    fn deeply_nested_input_never_crashes() {
        let values = [
            ("motor_temp", num(1.0)),
            ("door_open", SignalValue::Bool(true)),
        ];
        let cases = [
            (
                format!("{}motor_temp{} > 1.0", "(".repeat(245), ")".repeat(245)),
                None,
            ),
            (format!("{}door_open", "!".repeat(500)), None),
            (
                format!("motor_temp{} > 0.0", " + motor_temp".repeat(36)),
                Some(false),
            ),
            (
                format!("{}1{}", "[".repeat(250), "]".repeat(250)),
                Some(false),
            ),
            // The parser balances `&&` chains, so this is shallow and accepted.
            (format!("{}true", "true && ".repeat(60)), Some(true)),
        ];
        for (src, expect_ok) in cases {
            assert!(src.chars().count() <= MAX_CONDITION_CHARS, "{}", src.len());
            let result = compile(&src);
            if let Some(expected) = expect_ok {
                assert_eq!(result.is_ok(), expected, "{src}: {result:?}");
            }
            if let Ok(cond) = result {
                let values = values.clone();
                std::thread::Builder::new()
                    .stack_size(2 * 1024 * 1024)
                    .spawn(move || {
                        let _ =
                            cond.evaluate(|n| values.iter().find(|(k, _)| *k == n).map(|(_, v)| v));
                    })
                    .unwrap()
                    .join()
                    .unwrap();
            }
        }
    }
}
