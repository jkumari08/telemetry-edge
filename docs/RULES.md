# Rules

Implemented in `crates/rules` (validation, compilation, engine) and `crates/protocol/src/ruleset.rs` (schema types). The seed rule set is `rulesets/default.json`.

## Rule set schema

```json
{
  "ruleset_version": 12,
  "model": "R1S",
  "rules": [
    {"id": "soc_change",    "signal": "battery_soc",   "mode": "on_change", "deadband": 1.0},
    {"id": "speed_1hz",     "signal": "vehicle_speed", "mode": "periodic",  "interval_ms": 1000},
    {"id": "gear_change",   "signal": "gear",          "mode": "on_change"},
    {"id": "hot_motor",     "signal": "motor_temp",    "mode": "periodic",  "interval_ms": 200,
     "condition": "motor_temp > 90.0 && gear == 'D'"},
    {"id": "low_soc_drive", "signal": "vehicle_speed", "mode": "on_change", "deadband": 0.5,
     "condition": "gear == 'D' && battery_soc < 20.0"}
  ]
}
```

| Field | Modes | Meaning |
|---|---|---|
| `id` | all | Unique, matches `^[a-z0-9_]{1,64}$` |
| `signal` | all | Catalog signal name; rules never reference wire IDs |
| `mode` | all | `on_change` or `periodic` |
| `deadband` | `on_change`, numeric signals | Minimum change to log. Default 0 (any change). |
| `interval_ms` | `periodic` (required) | Emit interval, ≥ 100 |
| `max_age_ms` | `periodic` | Skip values older than this. Default `3 * interval_ms`. |
| `condition` | all | Optional boolean expression (see below) |

- `model` may be `"*"` to match any vehicle model.
- Unknown fields, and fields that don't apply to the rule's mode, are rejected.

## Semantics

- **Signal state:** the engine keeps the latest value for every signal name, with its vehicle timestamp and the time the daemon received it. Every decoded sample updates this state before any rule is evaluated.
- **`on_change`:** evaluated when a sample for `signal` arrives. It logs when:
  - nothing has been logged yet for this rule; or
  - for numbers: `|new - last_logged| > deadband`; or
  - for bools and enums: the value differs from the last logged value.

  The comparison is against the **last logged** value, so slow drift below the deadband is eventually logged.
- **`periodic`:**
  - Every `interval_ms`, logs the latest value of `signal`, provided it was received within `max_age_ms`.
  - If the value is stale, nothing is logged and a stale skip is counted (`telemetryd_stale_skips_total{rule_id}`).
  - If no value exists yet, the slot is skipped silently.
  - If the engine falls behind, missed slots are skipped rather than emitted as a burst.
- **`condition`:**
  - Evaluated when the rule would otherwise log; the log is emitted only if it evaluates to `true`.
  - If a referenced signal has no value yet, the result is `false`.
  - A runtime error (such as comparing an enum with a number) counts as `false` and is reported (`telemetryd_rule_eval_errors_total{rule_id}`).
  - A suppressed `on_change` log does not update the last logged value.
- **Clock:** the engine reads time from an injectable `Clock` (`SystemClock` in production, `FakeClock` in tests).

### Log record (NDJSON)

```json
{"ts_us": 1758650000123456, "vin": "DEV0000001", "model": "R1S", "rule_id": "speed_1hz", "ruleset_version": 12, "signal": "vehicle_speed", "value": 63.4, "unit": "km/h"}
```

- `ts_us` is the **vehicle timestamp of the logged sample**. For periodic rules, that is the latest sample's timestamp.
- `model` is the vehicle's model (never `"*"`).
- `unit` is omitted for signals without one (enums, bools).

## Condition language

Conditions use a restricted subset of [CEL](https://cel.dev), via the `cel` crate (the successor to `cel-interpreter`).

- **Operands:** catalog signal names and literals (numbers, strings, booleans).
  - Numbers are doubles; integer literals also work (`motor_temp > 90`).
  - Enums compare as strings (`gear == 'D'`); bools as bools (`!door_open`).
- **Operators:** `&&` `||` `!` `==` `!=` `<` `<=` `>` `>=` `+` `-` `*` `/` `%` `? :`
- **Not allowed:** function calls, macros (`all`, `map`...), lists, maps, field access, `null`, bytes.
- **Limits:** at most 512 characters, and an expression tree at most 24 levels deep.

## Validation

The whole rule set is rejected if any check fails. Every failure is reported at once.

| Check | `reason` label |
|---|---|
| `ruleset_version` ≤ active version (skipped on first load from cache) | `not_newer` |
| `model` is neither the vehicle's model nor `"*"` | `model_mismatch` |
| More than 256 rules | `too_many_rules` |
| Bad or duplicate id, unknown signal, bad mode fields, `interval_ms` < 100, negative `deadband` | `invalid_rule` |
| Condition fails to parse, uses a disallowed construct, references an unknown signal, or exceeds limits | `invalid_condition` |

Rules come from the network and are treated as untrusted input. The limits protect CPU, memory and the uplink.

## Dynamic updates

- A new rule set is fully parsed, verified, validated and compiled **before** it replaces the active one. On failure, the old set stays active.
- `Engine::apply_ruleset` reconciles per-rule state:
  - **unchanged rules** (same `id` and identical definition) keep their last logged value and periodic timer;
  - **new or modified rules** start fresh (periodic timers start at swap time);
  - **removed rules** drop their state.
- Signal values are independent of rules and survive every swap.
