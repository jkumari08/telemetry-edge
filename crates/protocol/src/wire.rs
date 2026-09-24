//! UDP wire format: packet header plus self-delimiting frames.
//!
//! See `docs/PROTOCOL.md`. The header is always big-endian; payload byte
//! order comes from the catalog. The decoder never panics: every read is
//! bounds-checked and failures are returned as values.

use thiserror::Error;

use crate::catalog::{Catalog, Endian, Signal, SignalType, ValueKind};
use crate::value::SignalValue;

pub const MAGIC: u16 = 0x5445;
pub const VERSION: u8 = 1;
pub const PACKET_HEADER_LEN: usize = 4;
pub const FRAME_HEADER_LEN: usize = 11;
pub const MAX_FRAMES: usize = 64;
pub const MAX_DATAGRAM_LEN: usize = 1500;

/// A decoded sample: catalog signal plus physical value.
#[derive(Debug, Clone, PartialEq)]
pub struct Sample {
    pub signal_id: u16,
    pub name: String,
    pub timestamp_us: u64,
    pub value: SignalValue,
}

/// Errors that reject the whole packet.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum PacketError {
    #[error("datagram of {0} bytes exceeds {MAX_DATAGRAM_LEN}")]
    Oversize(usize),
    #[error("datagram of {0} bytes is shorter than the packet header")]
    HeaderTruncated(usize),
    #[error("bad magic 0x{0:04x}")]
    BadMagic(u16),
    #[error("unsupported version {0}")]
    BadVersion(u8),
    #[error("frame_count is zero")]
    ZeroFrames,
    #[error("frame_count {0} exceeds {MAX_FRAMES}")]
    TooManyFrames(u8),
}

impl PacketError {
    /// Metric label for `telemetryd_decode_errors_total{reason}`.
    pub fn reason(&self) -> &'static str {
        match self {
            PacketError::Oversize(_) => "oversize",
            PacketError::HeaderTruncated(_) => "truncated",
            PacketError::BadMagic(_) => "bad_magic",
            PacketError::BadVersion(_) => "bad_version",
            PacketError::ZeroFrames => "zero_frames",
            PacketError::TooManyFrames(_) => "too_many_frames",
        }
    }
}

/// Problems with individual frames. Other frames in the packet still count.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum FrameErrorKind {
    /// Declared lengths run past the end of the datagram; later frames are dropped.
    #[error("frame truncated")]
    Truncated,
    #[error("payload length {actual} does not match type size {expected}")]
    LengthMismatch { expected: usize, actual: usize },
    /// Bool not 0/1, enum raw value not in the catalog, or non-finite number.
    #[error("invalid value")]
    InvalidValue,
    #[error("{0} trailing bytes after the last frame")]
    TrailingBytes(usize),
}

impl FrameErrorKind {
    /// Metric label for `telemetryd_decode_errors_total{reason}`.
    pub fn reason(&self) -> &'static str {
        match self {
            FrameErrorKind::Truncated => "truncated",
            FrameErrorKind::LengthMismatch { .. } => "length_mismatch",
            FrameErrorKind::InvalidValue => "invalid_value",
            FrameErrorKind::TrailingBytes(_) => "trailing_bytes",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrameError {
    /// Zero-based frame index within the packet.
    pub index: usize,
    /// Signal id, if the frame header was readable.
    pub signal_id: Option<u16>,
    pub kind: FrameErrorKind,
}

/// Result of decoding one packet that passed header validation.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DecodedPacket {
    pub samples: Vec<Sample>,
    pub errors: Vec<FrameError>,
    /// Frames skipped because their signal id is not in the catalog.
    pub unknown_signals: usize,
}

/// Bounds-checked cursor over a byte slice.
struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn new(buf: &'a [u8]) -> Self {
        Reader { buf, pos: 0 }
    }

    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let end = self.pos.checked_add(n)?;
        let bytes = self.buf.get(self.pos..end)?;
        self.pos = end;
        Some(bytes)
    }

    fn array<const N: usize>(&mut self) -> Option<[u8; N]> {
        self.take(N)?.try_into().ok()
    }

    fn u8(&mut self) -> Option<u8> {
        self.array::<1>().map(|[b]| b)
    }

    fn u16_be(&mut self) -> Option<u16> {
        self.array().map(u16::from_be_bytes)
    }

    fn u64_be(&mut self) -> Option<u64> {
        self.array().map(u64::from_be_bytes)
    }

    fn remaining(&self) -> usize {
        self.buf.len().saturating_sub(self.pos)
    }
}

/// Decodes one UDP datagram against `catalog`.
pub fn decode_packet(buf: &[u8], catalog: &Catalog) -> Result<DecodedPacket, PacketError> {
    if buf.len() > MAX_DATAGRAM_LEN {
        return Err(PacketError::Oversize(buf.len()));
    }
    let mut r = Reader::new(buf);
    let (Some(magic), Some(version), Some(frame_count)) = (r.u16_be(), r.u8(), r.u8()) else {
        return Err(PacketError::HeaderTruncated(buf.len()));
    };
    if magic != MAGIC {
        return Err(PacketError::BadMagic(magic));
    }
    if version != VERSION {
        return Err(PacketError::BadVersion(version));
    }
    if frame_count == 0 {
        return Err(PacketError::ZeroFrames);
    }
    if usize::from(frame_count) > MAX_FRAMES {
        return Err(PacketError::TooManyFrames(frame_count));
    }

    let mut out = DecodedPacket::default();
    for index in 0..usize::from(frame_count) {
        let header = (r.u16_be(), r.u64_be(), r.u8());
        let (Some(signal_id), Some(timestamp_us), Some(len)) = header else {
            out.errors.push(FrameError {
                index,
                signal_id: header.0,
                kind: FrameErrorKind::Truncated,
            });
            return Ok(out);
        };
        let Some(payload) = r.take(usize::from(len)) else {
            out.errors.push(FrameError {
                index,
                signal_id: Some(signal_id),
                kind: FrameErrorKind::Truncated,
            });
            return Ok(out);
        };
        let Some(signal) = catalog.by_id(signal_id) else {
            out.unknown_signals += 1;
            continue;
        };
        match decode_value(signal, payload) {
            Ok(value) => out.samples.push(Sample {
                signal_id,
                name: signal.name().to_owned(),
                timestamp_us,
                value,
            }),
            Err(kind) => out.errors.push(FrameError {
                index,
                signal_id: Some(signal_id),
                kind,
            }),
        }
    }
    if r.remaining() > 0 {
        out.errors.push(FrameError {
            index: usize::from(frame_count),
            signal_id: None,
            kind: FrameErrorKind::TrailingBytes(r.remaining()),
        });
    }
    Ok(out)
}

/// Raw payload value before scaling or enum lookup.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Raw {
    Int(i128),
    Float(f64),
}

fn read_raw(ty: SignalType, endian: Endian, p: &[u8]) -> Option<Raw> {
    macro_rules! num {
        ($t:ty, $variant:ident, $conv:expr) => {{
            let bytes = p.try_into().ok()?;
            let v = match endian {
                Endian::Big => <$t>::from_be_bytes(bytes),
                Endian::Little => <$t>::from_le_bytes(bytes),
            };
            Raw::$variant($conv(v))
        }};
    }
    Some(match ty {
        SignalType::Bool | SignalType::U8 => num!(u8, Int, i128::from),
        SignalType::I8 => num!(i8, Int, i128::from),
        SignalType::U16 => num!(u16, Int, i128::from),
        SignalType::I16 => num!(i16, Int, i128::from),
        SignalType::U32 => num!(u32, Int, i128::from),
        SignalType::I32 => num!(i32, Int, i128::from),
        SignalType::U64 => num!(u64, Int, i128::from),
        SignalType::I64 => num!(i64, Int, i128::from),
        SignalType::F32 => num!(f32, Float, f64::from),
        SignalType::F64 => num!(f64, Float, |v| v),
    })
}

fn write_raw(ty: SignalType, endian: Endian, raw: Raw) -> Option<Vec<u8>> {
    macro_rules! int {
        ($t:ty) => {{
            let Raw::Int(i) = raw else { return None };
            let v = <$t>::try_from(i).ok()?;
            match endian {
                Endian::Big => v.to_be_bytes().to_vec(),
                Endian::Little => v.to_le_bytes().to_vec(),
            }
        }};
    }
    macro_rules! float {
        ($v:expr) => {{
            let v = $v;
            match endian {
                Endian::Big => v.to_be_bytes().to_vec(),
                Endian::Little => v.to_le_bytes().to_vec(),
            }
        }};
    }
    Some(match ty {
        SignalType::Bool | SignalType::U8 => int!(u8),
        SignalType::I8 => int!(i8),
        SignalType::U16 => int!(u16),
        SignalType::I16 => int!(i16),
        SignalType::U32 => int!(u32),
        SignalType::I32 => int!(i32),
        SignalType::U64 => int!(u64),
        SignalType::I64 => int!(i64),
        SignalType::F32 => {
            let Raw::Float(f) = raw else { return None };
            let v = f as f32;
            if !v.is_finite() {
                return None;
            }
            float!(v)
        }
        SignalType::F64 => {
            let Raw::Float(f) = raw else { return None };
            float!(f)
        }
    })
}

/// Decodes one frame payload into a physical value.
pub fn decode_value(signal: &Signal, payload: &[u8]) -> Result<SignalValue, FrameErrorKind> {
    let ty = signal.ty();
    let length_mismatch = FrameErrorKind::LengthMismatch {
        expected: ty.size(),
        actual: payload.len(),
    };
    if payload.len() != ty.size() {
        return Err(length_mismatch);
    }
    let raw = read_raw(ty, signal.endian(), payload).ok_or(length_mismatch)?;
    match (signal.kind(), raw) {
        (ValueKind::Bool, Raw::Int(0)) => Ok(SignalValue::Bool(false)),
        (ValueKind::Bool, Raw::Int(1)) => Ok(SignalValue::Bool(true)),
        (ValueKind::Enum, Raw::Int(i)) => signal
            .enum_label(i)
            .map(|l| SignalValue::Enum(l.to_owned()))
            .ok_or(FrameErrorKind::InvalidValue),
        (ValueKind::Num, raw) => {
            let raw = match raw {
                Raw::Int(i) => i as f64,
                Raw::Float(f) => f,
            };
            let def = signal.def();
            let phys = raw * def.scale + def.offset;
            if phys.is_finite() {
                Ok(SignalValue::Num(phys))
            } else {
                Err(FrameErrorKind::InvalidValue)
            }
        }
        _ => Err(FrameErrorKind::InvalidValue),
    }
}

/// A frame with an already-encoded payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawFrame {
    pub signal_id: u16,
    pub timestamp_us: u64,
    pub payload: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Error)]
pub enum EncodeError {
    #[error("packet must contain at least one frame")]
    NoFrames,
    #[error("{0} frames exceeds {MAX_FRAMES}")]
    TooManyFrames(usize),
    #[error("payload of {len} bytes for signal {signal_id} exceeds 255")]
    PayloadTooLong { signal_id: u16, len: usize },
    #[error("packet of {0} bytes exceeds {MAX_DATAGRAM_LEN}")]
    Oversize(usize),
    #[error("value {value} does not match the kind of signal '{signal}'")]
    KindMismatch { signal: String, value: SignalValue },
    #[error("value {value} is out of range for signal '{signal}'")]
    OutOfRange { signal: String, value: f64 },
    #[error("'{label}' is not an enum label of signal '{signal}'")]
    UnknownEnumLabel { signal: String, label: String },
}

/// Encodes a physical value into a payload (inverse of [`decode_value`]).
///
/// Numeric values are converted with `raw = round((value - offset) / scale)`
/// for integer types.
pub fn encode_value(signal: &Signal, value: &SignalValue) -> Result<Vec<u8>, EncodeError> {
    let name = || signal.name().to_owned();
    let raw = match (signal.kind(), value) {
        (ValueKind::Bool, SignalValue::Bool(b)) => Raw::Int(i128::from(*b)),
        (ValueKind::Enum, SignalValue::Enum(label)) => Raw::Int(
            signal
                .enum_raw(label)
                .ok_or_else(|| EncodeError::UnknownEnumLabel {
                    signal: name(),
                    label: label.clone(),
                })?,
        ),
        (ValueKind::Num, SignalValue::Num(v)) => {
            let def = signal.def();
            let scaled = (v - def.offset) / def.scale;
            let out_of_range = || EncodeError::OutOfRange {
                signal: name(),
                value: *v,
            };
            if !scaled.is_finite() {
                return Err(out_of_range());
            }
            if def.ty.is_float() {
                Raw::Float(scaled)
            } else {
                // `as` saturates; `write_raw` then rejects anything outside the wire type.
                Raw::Int(scaled.round() as i128)
            }
        }
        _ => {
            return Err(EncodeError::KindMismatch {
                signal: name(),
                value: value.clone(),
            })
        }
    };
    write_raw(signal.ty(), signal.endian(), raw).ok_or_else(|| EncodeError::OutOfRange {
        signal: name(),
        value: value.as_num().unwrap_or(f64::NAN),
    })
}

/// Encodes a physical value into a [`RawFrame`] for `signal`.
pub fn encode_frame(
    signal: &Signal,
    timestamp_us: u64,
    value: &SignalValue,
) -> Result<RawFrame, EncodeError> {
    Ok(RawFrame {
        signal_id: signal.id(),
        timestamp_us,
        payload: encode_value(signal, value)?,
    })
}

/// Encodes frames into one packet, enforcing the wire limits.
pub fn encode_packet(frames: &[RawFrame]) -> Result<Vec<u8>, EncodeError> {
    if frames.is_empty() {
        return Err(EncodeError::NoFrames);
    }
    let count = u8::try_from(frames.len())
        .ok()
        .filter(|&c| usize::from(c) <= MAX_FRAMES)
        .ok_or(EncodeError::TooManyFrames(frames.len()))?;
    let total = PACKET_HEADER_LEN
        + frames
            .iter()
            .map(|f| FRAME_HEADER_LEN + f.payload.len())
            .sum::<usize>();
    if total > MAX_DATAGRAM_LEN {
        return Err(EncodeError::Oversize(total));
    }
    let mut out = Vec::with_capacity(total);
    out.extend_from_slice(&MAGIC.to_be_bytes());
    out.push(VERSION);
    out.push(count);
    for f in frames {
        let len = u8::try_from(f.payload.len()).map_err(|_| EncodeError::PayloadTooLong {
            signal_id: f.signal_id,
            len: f.payload.len(),
        })?;
        out.extend_from_slice(&f.signal_id.to_be_bytes());
        out.extend_from_slice(&f.timestamp_us.to_be_bytes());
        out.push(len);
        out.extend_from_slice(&f.payload);
    }
    Ok(out)
}
