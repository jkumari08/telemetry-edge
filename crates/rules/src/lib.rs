//! Rule engine for telemetry-edge.
//!
//! Validates, compiles and evaluates rule sets against decoded signal state.
//! This crate performs no network or filesystem I/O; time comes from an
//! injectable clock so behaviour is deterministic in tests.

#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

pub mod clock;
pub mod compile;
pub mod condition;
pub mod engine;

pub use clock::{Clock, FakeClock, SystemClock};
pub use compile::{
    compile_ruleset, is_valid_rule_id, CompiledRule, CompiledRuleSet, ValidationError,
    ValidationErrors, MAX_RULES, MIN_INTERVAL_MS,
};
pub use condition::{Condition, EvalError, MAX_CONDITION_CHARS, MAX_CONDITION_DEPTH};
pub use engine::{Engine, EvalReport, LogRecord};

/// Stack size for compiling conditions. The CEL parser recurses per nesting
/// level; in a debug build ~16 nested brackets overflowed a 2 MiB stack.
/// 512-character inputs stay well inside this budget, and the memory is
/// only committed as it is touched.
const COMPILE_STACK_BYTES: usize = 64 * 1024 * 1024;

/// Runs `f` on a scoped thread with a large stack. A panic inside `f`
/// (e.g. a parser bug on hostile input) becomes an error instead of
/// taking down the caller.
pub(crate) fn with_big_stack<T: Send>(f: impl FnOnce() -> T + Send) -> Result<T, String> {
    std::thread::scope(|scope| {
        std::thread::Builder::new()
            .name("rule-compiler".into())
            .stack_size(COMPILE_STACK_BYTES)
            .spawn_scoped(scope, f)
            .map_err(|e| format!("cannot start rule compiler thread: {e}"))?
            .join()
            .map_err(|_| "rule compiler panicked".to_owned())
    })
}
