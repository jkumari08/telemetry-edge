//! Rule engine for telemetry-edge.
//!
//! Validates, compiles and evaluates rule sets against decoded signal state.
//! This crate performs no network or filesystem I/O; time comes from an
//! injectable clock so behaviour is deterministic in tests.

#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
