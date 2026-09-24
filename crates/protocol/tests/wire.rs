//! Decoder and encoder tests against the spec in docs/PROTOCOL.md.
//!
//! Packets are built by hand here (not with the encoder) so the decoder is
//! checked against the spec rather than against itself.

use protocol::wire::{decode_value, encode_value, MAX_DATAGRAM_LEN};
use protocol::{
    decode_packet, encode_frame, encode_packet, Catalog, CatalogSpec, EncodeError, Endian,
    FrameError, FrameErrorKind, PacketError, RawFrame, SignalDef, SignalType, SignalValue,
};

const R1S: &str = include_str!("../../../catalogs/r1s.json");
const R2: &str = include_str!("../../../catalogs/r2.json");
const TS: u64 = 1_758_650_000_123_456;

fn r1s() -> Catalog {
    Catalog::from_json(R1S).unwrap()
}

fn r2() -> Catalog {
    Catalog::from_json(R2).unwrap()
}

fn frame(id: u16, ts: u64, payload: &[u8]) -> Vec<u8> {
    let mut v = id.to_be_bytes().to_vec();
    v.extend_from_slice(&ts.to_be_bytes());
    v.push(u8::try_from(payload.len()).unwrap());
    v.extend_from_slice(payload);
    v
}

fn packet_with_count(count: u8, frames: &[Vec<u8>]) -> Vec<u8> {
    let mut v = vec![0x54, 0x45, 0x01, count];
    for f in frames {
        v.extend_from_slice(f);
    }
    v
}

fn packet(frames: &[Vec<u8>]) -> Vec<u8> {
    packet_with_count(u8::try_from(frames.len()).unwrap(), frames)
}

fn num(v: &SignalValue) -> f64 {
    v.as_num()
        .unwrap_or_else(|| panic!("expected number, got {v:?}"))
}

fn assert_close(actual: f64, expected: f64) {
    assert!(
        (actual - expected).abs() < 1e-9,
        "expected {expected}, got {actual}"
    );
}

/// One signal per (type, endianness), scale 1, offset 0.
fn all_types_catalog() -> Catalog {
    let mut signals = Vec::new();
    let mut id = 1u16;
    for ty in SignalType::ALL {
        for endian in [Endian::Big, Endian::Little] {
            signals.push(SignalDef {
                id,
                name: format!("s{id}"),
                ty,
                endian: Some(endian),
                scale: 1.0,
                offset: 0.0,
                unit: None,
                enum_map: None,
            });
            id += 1;
        }
    }
    Catalog::new(CatalogSpec {
        model: "T".into(),
        catalog_version: 1,
        signals,
    })
    .unwrap()
}

fn test_values(ty: SignalType) -> Vec<SignalValue> {
    let two53 = 9_007_199_254_740_992.0;
    let nums: Vec<f64> = match ty {
        SignalType::Bool => return vec![SignalValue::Bool(false), SignalValue::Bool(true)],
        SignalType::U8 => vec![0.0, 1.0, 255.0],
        SignalType::I8 => vec![-128.0, -1.0, 0.0, 127.0],
        SignalType::U16 => vec![0.0, 258.0, 65535.0],
        SignalType::I16 => vec![-32768.0, -2.0, 32767.0],
        SignalType::U32 => vec![0.0, 16_909_060.0, f64::from(u32::MAX)],
        SignalType::I32 => vec![f64::from(i32::MIN), -1.0, f64::from(i32::MAX)],
        SignalType::U64 => vec![0.0, two53, 9_223_372_036_854_775_808.0],
        SignalType::I64 => vec![-9_223_372_036_854_775_808.0, -two53, two53],
        SignalType::F32 => vec![
            0.0,
            -1.5,
            1024.125,
            f64::from(f32::MAX),
            f64::from(f32::MIN_POSITIVE),
        ],
        SignalType::F64 => vec![0.0, -1.5, 1.0e300, f64::MIN_POSITIVE],
    };
    nums.into_iter().map(SignalValue::Num).collect()
}

// ---------------------------------------------------------------- round trips

#[test]
fn round_trip_every_type_both_endiannesses() {
    let cat = all_types_catalog();
    let mut checked = 0;
    for signal in cat.signals() {
        for value in test_values(signal.ty()) {
            let raw = encode_frame(signal, TS, &value).unwrap();
            assert_eq!(raw.payload.len(), signal.ty().size());
            let bytes = encode_packet(&[raw]).unwrap();
            let decoded = decode_packet(&bytes, &cat).unwrap();
            assert!(decoded.errors.is_empty(), "{:?}", decoded.errors);
            assert_eq!(decoded.samples.len(), 1);
            let s = &decoded.samples[0];
            assert_eq!(
                (s.signal_id, s.name.as_str(), s.timestamp_us),
                (signal.id(), signal.name(), TS)
            );
            assert_eq!(s.value, value, "{} {:?}", signal.name(), signal.ty());
            checked += 1;
        }
    }
    assert!(checked > 50);
}

#[test]
fn big_and_little_endian_payloads_are_byte_reversed() {
    let cat = all_types_catalog();
    let signals: Vec<_> = cat.signals().collect();
    for pair in signals.chunks(2) {
        let (big, little) = (pair[0], pair[1]);
        assert_eq!(
            (big.endian(), little.endian()),
            (Endian::Big, Endian::Little)
        );
        for value in test_values(big.ty()) {
            let mut b = encode_value(big, &value).unwrap();
            let l = encode_value(little, &value).unwrap();
            b.reverse();
            assert_eq!(b, l, "{:?} {value:?}", big.ty());
        }
    }
}

#[test]
fn known_byte_layouts() {
    let cat = all_types_catalog();
    let u16_be = cat.by_name("s7").unwrap();
    let u16_le = cat.by_name("s8").unwrap();
    assert_eq!(u16_be.ty(), SignalType::U16);
    let v = SignalValue::Num(258.0); // 0x0102
    assert_eq!(encode_value(u16_be, &v).unwrap(), [0x01, 0x02]);
    assert_eq!(encode_value(u16_le, &v).unwrap(), [0x02, 0x01]);

    let pkt = encode_packet(&[RawFrame {
        signal_id: 0x0101,
        timestamp_us: 0x0102_0304_0506_0708,
        payload: vec![0xAA],
    }])
    .unwrap();
    assert_eq!(
        pkt,
        [
            0x54, 0x45, 0x01, 0x01, // magic, version, frame_count
            0x01, 0x01, // signal_id (big-endian)
            0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, // timestamp_us (big-endian)
            0x01, // payload_len
            0xAA, // payload
        ]
    );
}

// ------------------------------------------------------------ scale / offset

#[test]
fn scale_and_offset_r1s() {
    let cat = r1s();
    let bytes = packet(&[
        frame(257, TS, &6340u16.to_be_bytes()), // vehicle_speed: 6340 * 0.01
        frame(258, TS, &[41]),                  // battery_soc: 41 * 0.5
        frame(259, TS, &1300i16.to_le_bytes()), // motor_temp: 1300 * 0.1 - 40
        frame(259, TS, &(-5i16).to_le_bytes()), // motor_temp: -0.5 - 40
    ]);
    let d = decode_packet(&bytes, &cat).unwrap();
    assert!(d.errors.is_empty());
    let values: Vec<f64> = d.samples.iter().map(|s| num(&s.value)).collect();
    assert_close(values[0], 63.4);
    assert_close(values[1], 20.5);
    assert_close(values[2], 90.0);
    assert_close(values[3], -40.5);
}

#[test]
fn scale_and_offset_r2_little_endian_u32() {
    let cat = r2();
    let bytes = packet(&[
        frame(1025, TS, &63_400u32.to_le_bytes()), // vehicle_speed: 63400 * 0.001
        frame(1027, TS, &91.5f32.to_be_bytes()),   // motor_temp: f32 big-endian
    ]);
    let d = decode_packet(&bytes, &cat).unwrap();
    assert_close(num(&d.samples[0].value), 63.4);
    assert_close(num(&d.samples[1].value), 91.5);
}

#[test]
fn encoder_applies_inverse_scale_and_rounds() {
    let cat = r1s();
    let speed = cat.by_name("vehicle_speed").unwrap();
    assert_eq!(
        encode_value(speed, &SignalValue::Num(63.4)).unwrap(),
        6340u16.to_be_bytes()
    );
    let temp = cat.by_name("motor_temp").unwrap();
    assert_eq!(
        encode_value(temp, &SignalValue::Num(90.04)).unwrap(),
        1300i16.to_le_bytes()
    );
}

// --------------------------------------------------------------- enum / bool

#[test]
fn enum_decoding_uses_each_catalogs_codes() {
    let d1 = decode_packet(&packet(&[frame(300, TS, &[3])]), &r1s()).unwrap();
    let d2 = decode_packet(&packet(&[frame(1030, TS, &[4])]), &r2()).unwrap();
    assert_eq!(d1.samples[0].value, SignalValue::Enum("D".into()));
    assert_eq!(d2.samples[0].value, SignalValue::Enum("D".into()));
}

#[test]
fn unknown_enum_code_is_invalid_value() {
    let d = decode_packet(
        &packet(&[frame(300, TS, &[7]), frame(258, TS, &[10])]),
        &r1s(),
    )
    .unwrap();
    assert_eq!(
        d.errors,
        [FrameError {
            index: 0,
            signal_id: Some(300),
            kind: FrameErrorKind::InvalidValue
        }]
    );
    assert_eq!(d.samples.len(), 1);
    assert_eq!(d.errors[0].kind.reason(), "invalid_value");
}

#[test]
fn bool_decoding() {
    let d = decode_packet(
        &packet(&[
            frame(301, TS, &[0]),
            frame(301, TS, &[1]),
            frame(301, TS, &[2]),
        ]),
        &r1s(),
    )
    .unwrap();
    let values: Vec<_> = d.samples.iter().map(|s| s.value.clone()).collect();
    assert_eq!(values, [SignalValue::Bool(false), SignalValue::Bool(true)]);
    assert_eq!(d.errors[0].kind, FrameErrorKind::InvalidValue);
}

#[test]
fn non_finite_floats_are_invalid() {
    let cat = r2();
    let d = decode_packet(
        &packet(&[
            frame(1027, TS, &f32::NAN.to_be_bytes()),
            frame(1027, TS, &f32::INFINITY.to_be_bytes()),
        ]),
        &cat,
    )
    .unwrap();
    assert!(d.samples.is_empty());
    assert_eq!(d.errors.len(), 2);
    assert!(d
        .errors
        .iter()
        .all(|e| e.kind == FrameErrorKind::InvalidValue));
}

// ------------------------------------------------------------ packet errors

#[test]
fn bad_magic() {
    let mut bytes = packet(&[frame(258, TS, &[1])]);
    bytes[0] = 0x00;
    let err = decode_packet(&bytes, &r1s()).unwrap_err();
    assert_eq!(err, PacketError::BadMagic(0x0045));
    assert_eq!(err.reason(), "bad_magic");
}

#[test]
fn bad_version() {
    let mut bytes = packet(&[frame(258, TS, &[1])]);
    bytes[2] = 2;
    let err = decode_packet(&bytes, &r1s()).unwrap_err();
    assert_eq!(err, PacketError::BadVersion(2));
    assert_eq!(err.reason(), "bad_version");
}

#[test]
fn zero_frame_count() {
    let err = decode_packet(&packet_with_count(0, &[]), &r1s()).unwrap_err();
    assert_eq!(err, PacketError::ZeroFrames);
    assert_eq!(err.reason(), "zero_frames");
}

#[test]
fn frame_count_above_64() {
    let err = decode_packet(&packet_with_count(65, &[]), &r1s()).unwrap_err();
    assert_eq!(err, PacketError::TooManyFrames(65));
}

#[test]
fn header_truncated() {
    for len in 0..4 {
        let bytes = &[0x54, 0x45, 0x01][..len.min(3)];
        let err = decode_packet(bytes, &r1s()).unwrap_err();
        assert_eq!(err.reason(), "truncated");
    }
}

// ------------------------------------------------------------- frame errors

#[test]
fn truncated_frame_header_keeps_earlier_frames() {
    let mut bytes = packet_with_count(3, &[frame(258, TS, &[10]), frame(301, TS, &[1])]);
    bytes.extend_from_slice(&[0x01, 0x02, 0x00]); // partial third frame header
    let d = decode_packet(&bytes, &r1s()).unwrap();
    assert_eq!(d.samples.len(), 2);
    assert_eq!(
        d.errors,
        [FrameError {
            index: 2,
            signal_id: Some(0x0102),
            kind: FrameErrorKind::Truncated
        }]
    );
    assert_eq!(d.errors[0].kind.reason(), "truncated");
}

#[test]
fn truncated_payload_keeps_earlier_frames() {
    let mut bytes = packet_with_count(2, &[frame(258, TS, &[10])]);
    let mut third = frame(257, TS, &[0x01, 0x02]);
    third.pop(); // declared payload_len 2, only 1 byte present
    bytes.extend_from_slice(&third);
    let d = decode_packet(&bytes, &r1s()).unwrap();
    assert_eq!(d.samples.len(), 1);
    assert_eq!(d.errors[0].kind, FrameErrorKind::Truncated);
    assert_eq!(d.errors[0].index, 1);
}

#[test]
fn unknown_signal_is_skipped() {
    let bytes = packet(&[
        frame(999, TS, &[1, 2, 3]),
        frame(258, TS, &[10]),
        frame(4242, TS, &[]),
    ]);
    let d = decode_packet(&bytes, &r1s()).unwrap();
    assert_eq!(d.unknown_signals, 2);
    assert!(d.errors.is_empty());
    assert_eq!(d.samples.len(), 1);
    assert_eq!(d.samples[0].name, "battery_soc");
}

#[test]
fn length_mismatch_skips_only_that_frame() {
    let bytes = packet(&[
        frame(257, TS, &[0x01, 0x02, 0x03]), // vehicle_speed is u16
        frame(258, TS, &[10]),
    ]);
    let d = decode_packet(&bytes, &r1s()).unwrap();
    assert_eq!(
        d.errors,
        [FrameError {
            index: 0,
            signal_id: Some(257),
            kind: FrameErrorKind::LengthMismatch {
                expected: 2,
                actual: 3
            }
        }]
    );
    assert_eq!(d.errors[0].kind.reason(), "length_mismatch");
    assert_eq!(d.samples.len(), 1);
}

#[test]
fn trailing_bytes_are_reported_but_frames_kept() {
    let mut bytes = packet(&[frame(258, TS, &[10])]);
    bytes.push(0xFF);
    let d = decode_packet(&bytes, &r1s()).unwrap();
    assert_eq!(d.samples.len(), 1);
    assert_eq!(d.errors[0].kind, FrameErrorKind::TrailingBytes(1));
}

// ----------------------------------------------------------------- max size

/// Unknown-signal frames whose total packet size is exactly `total` bytes.
fn frames_totalling(total: usize) -> Vec<RawFrame> {
    let mut remaining = total - 4;
    let mut frames = Vec::new();
    while remaining > 0 {
        let payload = (remaining - 11).min(255);
        frames.push(RawFrame {
            signal_id: 60_000,
            timestamp_us: TS,
            payload: vec![0xAB; payload],
        });
        remaining -= 11 + payload;
    }
    frames
}

#[test]
fn max_size_datagram_is_accepted() {
    let frames = frames_totalling(MAX_DATAGRAM_LEN);
    let bytes = encode_packet(&frames).unwrap();
    assert_eq!(bytes.len(), MAX_DATAGRAM_LEN);
    let d = decode_packet(&bytes, &r1s()).unwrap();
    assert_eq!(d.unknown_signals, frames.len());
    assert!(d.errors.is_empty());
}

#[test]
fn oversize_datagram_is_rejected() {
    let frames = frames_totalling(MAX_DATAGRAM_LEN + 1);
    assert_eq!(
        encode_packet(&frames).unwrap_err(),
        EncodeError::Oversize(MAX_DATAGRAM_LEN + 1)
    );
    let mut bytes = packet(&[frame(258, TS, &[10])]);
    bytes.resize(MAX_DATAGRAM_LEN + 1, 0);
    let err = decode_packet(&bytes, &r1s()).unwrap_err();
    assert_eq!(err, PacketError::Oversize(MAX_DATAGRAM_LEN + 1));
    assert_eq!(err.reason(), "oversize");
}

// ----------------------------------------------------------- encoder errors

#[test]
fn encoder_rejects_bad_input() {
    let cat = r1s();
    let soc = cat.by_name("battery_soc").unwrap();
    let gear = cat.by_name("gear").unwrap();
    let door = cat.by_name("door_open").unwrap();

    assert_eq!(encode_packet(&[]).unwrap_err(), EncodeError::NoFrames);
    let one = RawFrame {
        signal_id: 1,
        timestamp_us: 0,
        payload: vec![0],
    };
    assert_eq!(
        encode_packet(&vec![one; 65]).unwrap_err(),
        EncodeError::TooManyFrames(65)
    );
    let long = RawFrame {
        signal_id: 1,
        timestamp_us: 0,
        payload: vec![0; 256],
    };
    assert!(matches!(
        encode_packet(&[long]).unwrap_err(),
        EncodeError::PayloadTooLong { len: 256, .. }
    ));

    // battery_soc is u8 with scale 0.5, so the max physical value is 127.5.
    for bad in [128.0, -1.0, f64::NAN, f64::INFINITY] {
        assert!(matches!(
            encode_value(soc, &SignalValue::Num(bad)).unwrap_err(),
            EncodeError::OutOfRange { .. }
        ));
    }
    assert!(matches!(
        encode_value(gear, &SignalValue::Enum("X".into())).unwrap_err(),
        EncodeError::UnknownEnumLabel { .. }
    ));
    assert!(matches!(
        encode_value(door, &SignalValue::Num(1.0)).unwrap_err(),
        EncodeError::KindMismatch { .. }
    ));
    assert!(matches!(
        encode_value(soc, &SignalValue::Bool(true)).unwrap_err(),
        EncodeError::KindMismatch { .. }
    ));
}

#[test]
fn decode_value_rejects_wrong_length_directly() {
    let cat = r1s();
    let speed = cat.by_name("vehicle_speed").unwrap();
    assert_eq!(
        decode_value(speed, &[]).unwrap_err(),
        FrameErrorKind::LengthMismatch {
            expected: 2,
            actual: 0
        }
    );
}
