//! Property tests: the decoder must never panic, and encode/decode agree.

use proptest::prelude::*;
use protocol::wire::{decode_value, encode_value};
use protocol::{decode_packet, encode_packet, Catalog, RawFrame};

const R1S: &str = include_str!("../../../catalogs/r1s.json");
const R2: &str = include_str!("../../../catalogs/r2.json");

fn catalogs() -> [Catalog; 2] {
    [
        Catalog::from_json(R1S).unwrap(),
        Catalog::from_json(R2).unwrap(),
    ]
}

/// Frames for known signals with correctly sized but random payloads.
fn known_frames() -> impl Strategy<Value = (usize, Vec<RawFrame>)> {
    (0..2usize).prop_flat_map(|cat_idx| {
        let cat = &catalogs()[cat_idx];
        let sigs: Vec<(u16, usize)> = cat.signals().map(|s| (s.id(), s.ty().size())).collect();
        let frame = (
            0..sigs.len(),
            any::<u64>(),
            prop::collection::vec(any::<u8>(), 8),
        )
            .prop_map(move |(i, ts, bytes)| {
                let (id, size) = sigs[i];
                RawFrame {
                    signal_id: id,
                    timestamp_us: ts,
                    payload: bytes[..size].to_vec(),
                }
            });
        (Just(cat_idx), prop::collection::vec(frame, 1..=64))
    })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(2000))]

    #[test]
    fn never_panics_on_arbitrary_bytes(bytes in prop::collection::vec(any::<u8>(), 0..2048)) {
        for cat in &catalogs() {
            let _ = decode_packet(&bytes, cat);
        }
    }

    #[test]
    fn never_panics_behind_a_valid_header(
        count in any::<u8>(),
        tail in prop::collection::vec(any::<u8>(), 0..1600),
    ) {
        let mut bytes = vec![0x54, 0x45, 0x01, count];
        bytes.extend_from_slice(&tail);
        for cat in &catalogs() {
            if let Ok(d) = decode_packet(&bytes, cat) {
                prop_assert!(d.samples.len() + d.errors.len() + d.unknown_signals <= 65);
            }
        }
    }

    /// Every frame is accounted for exactly once, and any value that decodes
    /// re-encodes to the same payload.
    #[test]
    fn decode_then_reencode_is_identity((cat_idx, frames) in known_frames()) {
        let cat = &catalogs()[cat_idx];
        let bytes = encode_packet(&frames).unwrap();
        let d = decode_packet(&bytes, cat).unwrap();
        prop_assert_eq!(d.unknown_signals, 0);
        prop_assert_eq!(d.samples.len() + d.errors.len(), frames.len());
        let mut samples = d.samples.iter();
        for (i, f) in frames.iter().enumerate() {
            if d.errors.iter().any(|e| e.index == i) {
                continue;
            }
            let s = samples.next().unwrap();
            prop_assert_eq!(s.signal_id, f.signal_id);
            prop_assert_eq!(s.timestamp_us, f.timestamp_us);
            let signal = cat.by_id(f.signal_id).unwrap();
            let reencoded = encode_value(signal, &s.value).unwrap();
            if signal.ty().is_float() {
                // `-0.0 + offset` normalises to `+0.0`, so compare values for floats.
                prop_assert_eq!(&decode_value(signal, &reencoded).unwrap(), &s.value);
            } else {
                prop_assert_eq!(&reencoded, &f.payload);
            }
        }
    }
}
