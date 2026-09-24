//! Malformed packets for the `garbage` scenario, mixed with valid ones.
//!
//! Each kind targets one decoder path so the daemon's
//! `decode_errors_total{reason}` metrics can be demonstrated.

use protocol::wire::{FRAME_HEADER_LEN, MAX_DATAGRAM_LEN};
use protocol::{encode_packet, Catalog, EncodeError, RawFrame};

use crate::rng::Rng;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GarbageKind {
    BadMagic,
    BadVersion,
    ZeroFrames,
    Truncated,
    LengthMismatch,
    UnknownSignal,
    Oversize,
    TrailingBytes,
    RandomBytes,
}

impl GarbageKind {
    pub const ALL: [GarbageKind; 9] = [
        GarbageKind::BadMagic,
        GarbageKind::BadVersion,
        GarbageKind::ZeroFrames,
        GarbageKind::Truncated,
        GarbageKind::LengthMismatch,
        GarbageKind::UnknownSignal,
        GarbageKind::Oversize,
        GarbageKind::TrailingBytes,
        GarbageKind::RandomBytes,
    ];
}

/// Returns a packet that is valid half of the time and malformed otherwise.
pub fn packet(
    frames: &[RawFrame],
    catalog: &Catalog,
    rng: &mut Rng,
) -> Result<(Option<GarbageKind>, Vec<u8>), EncodeError> {
    if rng.chance(0.5) {
        return Ok((None, encode_packet(frames)?));
    }
    let kind = GarbageKind::ALL[rng.below(GarbageKind::ALL.len())];
    Ok((Some(kind), malformed(kind, frames, catalog, rng)?))
}

/// Builds one malformed packet of `kind` from valid `frames` (at least two).
pub fn malformed(
    kind: GarbageKind,
    frames: &[RawFrame],
    catalog: &Catalog,
    rng: &mut Rng,
) -> Result<Vec<u8>, EncodeError> {
    let mut bytes = encode_packet(frames)?;
    match kind {
        GarbageKind::BadMagic => bytes[..2].copy_from_slice(&0xDEADu16.to_be_bytes()),
        GarbageKind::BadVersion => bytes[2] = 2 + rng.below(254) as u8,
        GarbageKind::ZeroFrames => bytes = vec![0x54, 0x45, 0x01, 0x00],
        GarbageKind::Truncated => {
            // Cut into the last frame only, so earlier frames still decode.
            let last = FRAME_HEADER_LEN + frames.last().map_or(0, |f| f.payload.len());
            bytes.truncate(bytes.len() - 1 - rng.below(last));
        }
        GarbageKind::LengthMismatch => {
            let mut bad = frames.to_vec();
            bad[0].payload.push(0);
            bytes = encode_packet(&bad)?;
        }
        GarbageKind::UnknownSignal => {
            let id = (0xF000..=u16::MAX)
                .find(|id| catalog.by_id(*id).is_none())
                .unwrap_or(u16::MAX);
            let mut with_unknown = vec![RawFrame {
                signal_id: id,
                timestamp_us: frames[0].timestamp_us,
                payload: rng.some_bytes(8),
            }];
            with_unknown.extend_from_slice(frames);
            bytes = encode_packet(&with_unknown)?;
        }
        GarbageKind::Oversize => {
            let extra = MAX_DATAGRAM_LEN + 1 + rng.below(200) - bytes.len();
            bytes.extend(rng.bytes(extra));
        }
        GarbageKind::TrailingBytes => bytes.extend(rng.some_bytes(8)),
        GarbageKind::RandomBytes => bytes = rng.some_bytes(64),
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::{decode_packet, FrameErrorKind, SignalValue};

    fn setup(json: &str) -> (Catalog, Vec<RawFrame>) {
        let cat = Catalog::from_json(json).unwrap();
        let frames = cat
            .signals()
            .map(|s| {
                let v = match s.name() {
                    "gear" => SignalValue::Enum("D".into()),
                    "door_open" => SignalValue::Bool(false),
                    _ => SignalValue::Num(20.0),
                };
                protocol::encode_frame(s, 1, &v).unwrap()
            })
            .collect();
        (cat, frames)
    }

    fn reason_of(bytes: &[u8], cat: &Catalog) -> Vec<&'static str> {
        match decode_packet(bytes, cat) {
            Err(e) => vec![e.reason()],
            Ok(d) => d.errors.iter().map(|e| e.kind.reason()).collect(),
        }
    }

    #[test]
    fn each_kind_triggers_its_decoder_path() {
        for json in [
            include_str!("../../../catalogs/r1s.json"),
            include_str!("../../../catalogs/r2.json"),
        ] {
            let (cat, frames) = setup(json);
            let n = frames.len();
            let mut rng = Rng::new(3);
            for _ in 0..50 {
                let mut make = |k| malformed(k, &frames, &cat, &mut rng).unwrap();
                assert_eq!(reason_of(&make(GarbageKind::BadMagic), &cat), ["bad_magic"]);
                assert_eq!(
                    reason_of(&make(GarbageKind::BadVersion), &cat),
                    ["bad_version"]
                );
                assert_eq!(
                    reason_of(&make(GarbageKind::ZeroFrames), &cat),
                    ["zero_frames"]
                );
                assert_eq!(reason_of(&make(GarbageKind::Oversize), &cat), ["oversize"]);

                let d = decode_packet(&make(GarbageKind::Truncated), &cat).unwrap();
                assert_eq!(d.samples.len(), n - 1);
                assert_eq!(d.errors[0].kind, FrameErrorKind::Truncated);

                let d = decode_packet(&make(GarbageKind::LengthMismatch), &cat).unwrap();
                assert_eq!(d.samples.len(), n - 1);
                assert_eq!(d.errors[0].kind.reason(), "length_mismatch");

                let d = decode_packet(&make(GarbageKind::UnknownSignal), &cat).unwrap();
                assert_eq!((d.samples.len(), d.unknown_signals), (n, 1));

                let d = decode_packet(&make(GarbageKind::TrailingBytes), &cat).unwrap();
                assert_eq!(d.samples.len(), n);
                assert_eq!(d.errors[0].kind.reason(), "trailing_bytes");

                let _ = decode_packet(&make(GarbageKind::RandomBytes), &cat);
            }
        }
    }

    #[test]
    fn mix_is_roughly_half_valid() {
        let (cat, frames) = setup(include_str!("../../../catalogs/r1s.json"));
        let mut rng = Rng::new(9);
        let valid = (0..1000)
            .filter(|_| packet(&frames, &cat, &mut rng).unwrap().0.is_none())
            .count();
        assert!((400..600).contains(&valid), "{valid}");
    }
}
