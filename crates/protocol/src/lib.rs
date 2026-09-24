//! Wire protocol for telemetry-edge.
//!
//! Frame encoding/decoding, signal catalog types, rule set types and the
//! signed rule set envelope. This crate performs no network or filesystem I/O.

#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

/// Packet magic, `"TE"` in ASCII, big-endian.
pub const MAGIC: u16 = 0x5445;

/// Current wire protocol version.
pub const VERSION: u8 = 1;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn magic_is_te() {
        assert_eq!(MAGIC.to_be_bytes(), *b"TE");
    }
}
