//! Wire protocol for telemetry-edge.
//!
//! Frame encoding/decoding, signal catalog types, rule set types and the
//! signed rule set envelope. This crate performs no network or filesystem I/O.

#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

pub mod catalog;
pub mod ruleset;
pub mod value;
pub mod wire;

pub use catalog::{
    Catalog, CatalogError, CatalogSpec, Endian, Signal, SignalDef, SignalType, ValueKind,
};
pub use ruleset::{Mode, Rule, RuleSet, ANY_MODEL};
pub use value::SignalValue;
pub use wire::{
    decode_packet, encode_frame, encode_packet, DecodedPacket, EncodeError, FrameError,
    FrameErrorKind, PacketError, RawFrame, Sample,
};
