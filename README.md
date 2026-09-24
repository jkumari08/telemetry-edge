# telemetry-edge

Rule-driven vehicle telemetry logger in Rust with cloud-managed rules, binary signal decoding, and Prometheus metering.

> Status: work in progress, built milestone by milestone. Currently at **M1 (protocol crate)**.

## Workspace

| Crate | Kind | Purpose |
|---|---|---|
| `crates/protocol` | lib | Frame encode/decode, signal catalog, rule types, signing envelope (no I/O) |
| `crates/rules` | lib | Rule validation, compilation, evaluation engine (no I/O) |
| `crates/telemetryd` | bin | Edge daemon |
| `crates/simulator` | bin | Fake vehicle bus |
| `crates/cloud` | bin | Rule server, telemetry ingest, UI host |

## Build and test

Requires a stable Rust toolchain (`rustup`).

```sh
cargo build --workspace
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace

cargo run -p telemetryd -- --help
cargo run -p simulator -- --help
cargo run -p cloud -- --help
```

## Docs

- Wire format and catalogs: [`docs/PROTOCOL.md`](docs/PROTOCOL.md)
