# telemetry-edge

Rule-driven vehicle telemetry logger in Rust with cloud-managed rules, binary signal decoding, and Prometheus metering.

> Status: work in progress, built milestone by milestone. Currently at **M4 (daemon end to end, rules from a local file)**.

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

## Try it locally

Run from the repo root in two terminals.

```sh
# Terminal 1: the daemon applies rulesets/default.json and writes telemetry to dev/state/out/
cargo run -p telemetryd -- --config dev/config.toml

# Terminal 2: simulated R1S driving at 50 packets/s
cargo run -p simulator -- --catalog catalogs/r1s.json --scenario drive

# Terminal 3: watch the rule-driven log records
tail -f dev/state/out/telemetry.ndjson
```

Example record:

```json
{"ts_us":1790270675517932,"vin":"DEV0000002","model":"R2","rule_id":"hot_motor","ruleset_version":1,"signal":"motor_temp","value":90.116,"unit":"C"}
```

- **Output files:** `telemetry.ndjson` rotates at `output.max_file_bytes` (10 MiB by default). Up to `output.max_files` files are kept in total (5 by default).
- **Shutdown:** Ctrl-C or SIGTERM stops ingest, drains queued samples through the engine, then flushes and syncs the output file.
- **Debugging:** `--debug-decode` also prints every decoded sample (stdout) and every rejected packet or frame (stderr).

- **Scenarios:**
  - `drive`: 60 s cycle of P, then D with a speed ramp to ~115 km/h (motor temperature crosses 90 °C), then back to P.
  - `park`: gear P, door toggling.
  - `garbage`: half the packets are malformed (bad magic or version, truncated, wrong lengths, unknown IDs, oversize, trailing or random bytes). The daemon keeps running and reports each rejection by reason.
- **R2 catalog:** use `--config dev/config.r2.toml` for the daemon and `--catalog catalogs/r2.json` for the simulator.
- **Useful options:** `--duration-s N` stops the simulator after N seconds; `--seed N` makes a run reproducible.

## Docs

- Wire format and catalogs: [`docs/PROTOCOL.md`](docs/PROTOCOL.md)
- Rule schema, semantics and condition language: [`docs/RULES.md`](docs/RULES.md)
