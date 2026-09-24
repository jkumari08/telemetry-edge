# Wire protocol (UDP)

Implemented in `crates/protocol/src/wire.rs`. Catalog types are in `crates/protocol/src/catalog.rs`.

## Packet layout

One UDP datagram carries one packet. A packet contains one or more frames. The maximum accepted datagram size is **1500 bytes**.

```
Packet header (all big-endian):
  offset size field
  0      2    magic         u16  = 0x5445  ("TE")
  2      1    version       u8   = 1
  3      1    frame_count   u8   (1..=64)

Then frame_count frames, each:
  0      2    signal_id     u16  big-endian
  2      8    timestamp_us  u64  big-endian (microseconds since UNIX epoch, vehicle clock)
  10     1    payload_len   u8
  11     N    payload       N = payload_len bytes, encoded per the catalog's type AND endianness
```

- The header is always big-endian (network byte order).
- Payload byte order is defined **per signal in the catalog**, simulating ECUs that differ.
- `payload_len` makes frames self-delimiting. A daemon with an older catalog can skip unknown signal IDs instead of failing the whole packet.

## Decode rules

Every read is bounds-checked. The decoder returns `Result` values and never panics. This is enforced with `#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::indexing_slicing)]` and tested with `proptest`.

### Whole packet rejected (`PacketError`)

| Condition | `reason` label |
|---|---|
| Datagram > 1500 bytes | `oversize` |
| Datagram shorter than the 4-byte header | `truncated` |
| Magic ≠ `0x5445` | `bad_magic` |
| Version ≠ 1 | `bad_version` |
| `frame_count == 0` | `zero_frames` |
| `frame_count > 64` | `too_many_frames` |

### Per frame (`FrameError`); other frames still count

| Condition | Effect | `reason` label |
|---|---|---|
| Frame header or payload runs past the datagram end | Stop. Frames already decoded are kept. | `truncated` |
| `payload_len` ≠ catalog type size | Skip frame | `length_mismatch` |
| Bool not 0/1, enum code not in catalog, or non-finite number | Skip frame | `invalid_value` |
| Bytes left over after `frame_count` frames | Frames kept; error reported | `trailing_bytes` |
| Unknown `signal_id` | Skip frame; counted in `unknown_signals` (metric `telemetryd_unknown_signals_total`), not as a decode error | n/a |

## Signal catalog

```json
{
  "model": "R1S",
  "catalog_version": 3,
  "signals": [
    {"id": 257, "name": "vehicle_speed", "type": "u16", "endian": "big",    "scale": 0.01, "offset": 0.0, "unit": "km/h"},
    {"id": 258, "name": "battery_soc",   "type": "u8",  "endian": "little", "scale": 0.5,  "offset": 0.0, "unit": "%"},
    {"id": 259, "name": "motor_temp",    "type": "i16", "endian": "little", "scale": 0.1,  "offset": -40.0, "unit": "C"},
    {"id": 300, "name": "gear",          "type": "u8",  "endian": "little", "enum": {"0": "P", "1": "R", "2": "N", "3": "D"}},
    {"id": 301, "name": "door_open",     "type": "bool"}
  ]
}
```

- Types: `bool` (1 byte, 0/1), `u8`, `i8`, `u16`, `i16`, `u32`, `i32`, `u64`, `i64`, `f32`, `f64`.
- Physical value = `raw * scale + offset` (defaults: scale 1.0, offset 0.0).
- Decoded value: `SignalValue::{Bool(bool), Num(f64), Enum(String)}`. Enum signals decode to their label, bool to bool, and everything else to f64.
- Encoding (simulator) is the inverse: `raw = round((value - offset) / scale)` for integer types. Values outside the wire type's range are an error, not clamped.

### Catalog validation

- Model is non-empty.
- Signal IDs and names are unique.
- Names match `^[a-z][a-z0-9_]{0,63}$`.
- `endian` is required for multi-byte types. It is optional and ignored for 1-byte types.
- `scale` is finite and non-zero; `offset` is finite.
- `enum` is only allowed on integer types. It must be non-empty, keys must be integers within the type's range, and labels must be non-empty and unique.
- Unknown JSON fields are rejected.

### Model catalogs

| Signal | R1S (`catalogs/r1s.json`) | R2 (`catalogs/r2.json`) |
|---|---|---|
| `vehicle_speed` | 257, u16 BE, ×0.01 | 1025, u32 **LE**, ×0.001 |
| `battery_soc` | 258, u8, ×0.5 | 1026, u16 BE, ×0.01 |
| `motor_temp` | 259, i16 **LE**, ×0.1 −40 | 1027, f32 **BE** |
| `gear` | 300, u8, `0..3 = P R N D` | 1030, u8, `1..4 = P R N D` |
| `door_open` | 301, bool | 1031, bool |
| `cabin_temp` | n/a | 1040, i16 BE, ×0.1 |

Rules reference signals **by name**, so the same rule set works on both models.

## gRPC ingest

Secondary input, added in M11. Defined in `proto/telemetry.proto`. Samples carry physical values by catalog name, so no binary decoding is involved.
