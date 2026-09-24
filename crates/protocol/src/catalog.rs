//! Per-vehicle-model signal catalog (similar to a CAN DBC file).

use std::collections::{BTreeMap, HashMap, HashSet};

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Wire type of a signal's raw payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SignalType {
    Bool,
    U8,
    I8,
    U16,
    I16,
    U32,
    I32,
    U64,
    I64,
    F32,
    F64,
}

impl SignalType {
    pub const ALL: [SignalType; 11] = [
        SignalType::Bool,
        SignalType::U8,
        SignalType::I8,
        SignalType::U16,
        SignalType::I16,
        SignalType::U32,
        SignalType::I32,
        SignalType::U64,
        SignalType::I64,
        SignalType::F32,
        SignalType::F64,
    ];

    /// Payload size in bytes.
    pub fn size(self) -> usize {
        match self {
            SignalType::Bool | SignalType::U8 | SignalType::I8 => 1,
            SignalType::U16 | SignalType::I16 => 2,
            SignalType::U32 | SignalType::I32 | SignalType::F32 => 4,
            SignalType::U64 | SignalType::I64 | SignalType::F64 => 8,
        }
    }

    pub fn is_float(self) -> bool {
        matches!(self, SignalType::F32 | SignalType::F64)
    }

    /// Inclusive raw range for integer types, `None` for bool and floats.
    pub fn int_range(self) -> Option<(i128, i128)> {
        let range = match self {
            SignalType::U8 => (0, i128::from(u8::MAX)),
            SignalType::I8 => (i128::from(i8::MIN), i128::from(i8::MAX)),
            SignalType::U16 => (0, i128::from(u16::MAX)),
            SignalType::I16 => (i128::from(i16::MIN), i128::from(i16::MAX)),
            SignalType::U32 => (0, i128::from(u32::MAX)),
            SignalType::I32 => (i128::from(i32::MIN), i128::from(i32::MAX)),
            SignalType::U64 => (0, i128::from(u64::MAX)),
            SignalType::I64 => (i128::from(i64::MIN), i128::from(i64::MAX)),
            SignalType::Bool | SignalType::F32 | SignalType::F64 => return None,
        };
        Some(range)
    }
}

/// Byte order of a multi-byte payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Endian {
    Big,
    Little,
}

/// Kind of physical value a signal decodes to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ValueKind {
    Bool,
    Num,
    Enum,
}

/// One signal definition, exactly as written in the catalog JSON.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SignalDef {
    pub id: u16,
    pub name: String,
    #[serde(rename = "type")]
    pub ty: SignalType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endian: Option<Endian>,
    #[serde(default = "default_scale")]
    pub scale: f64,
    #[serde(default)]
    pub offset: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
    #[serde(default, rename = "enum", skip_serializing_if = "Option::is_none")]
    pub enum_map: Option<BTreeMap<String, String>>,
}

fn default_scale() -> f64 {
    1.0
}

/// Catalog document, exactly as written in the catalog JSON.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CatalogSpec {
    pub model: String,
    pub catalog_version: u32,
    pub signals: Vec<SignalDef>,
}

#[derive(Debug, Error)]
pub enum CatalogError {
    #[error("invalid catalog JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("catalog model must not be empty")]
    EmptyModel,
    #[error("duplicate signal id {0}")]
    DuplicateId(u16),
    #[error("duplicate signal name '{0}'")]
    DuplicateName(String),
    #[error("invalid signal name '{0}' (must match ^[a-z][a-z0-9_]{{0,63}}$)")]
    InvalidName(String),
    #[error("signal '{0}' is multi-byte and must declare 'endian'")]
    MissingEndian(String),
    #[error("signal '{0}' has a non-finite or zero scale, or a non-finite offset")]
    InvalidScaleOffset(String),
    #[error("signal '{signal}' has an invalid enum: {reason}")]
    InvalidEnum { signal: String, reason: String },
}

/// Returns true if `name` matches `^[a-z][a-z0-9_]{0,63}$`.
pub fn is_valid_signal_name(name: &str) -> bool {
    let mut chars = name.chars();
    let first_ok = chars.next().is_some_and(|c| c.is_ascii_lowercase());
    first_ok
        && name.len() <= 64
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

/// A validated signal with its enum lookup tables.
#[derive(Debug, Clone)]
pub struct Signal {
    def: SignalDef,
    enum_by_raw: HashMap<i128, String>,
    enum_by_label: HashMap<String, i128>,
}

impl Signal {
    fn new(def: SignalDef) -> Result<Self, CatalogError> {
        if !is_valid_signal_name(&def.name) {
            return Err(CatalogError::InvalidName(def.name));
        }
        if def.ty.size() > 1 && def.endian.is_none() {
            return Err(CatalogError::MissingEndian(def.name));
        }
        if !def.scale.is_finite() || def.scale == 0.0 || !def.offset.is_finite() {
            return Err(CatalogError::InvalidScaleOffset(def.name));
        }
        let mut enum_by_raw = HashMap::new();
        let mut enum_by_label = HashMap::new();
        if let Some(map) = &def.enum_map {
            let invalid = |reason: String| CatalogError::InvalidEnum {
                signal: def.name.clone(),
                reason,
            };
            let Some((min, max)) = def.ty.int_range() else {
                return Err(invalid(format!(
                    "enum requires an integer type, got {:?}",
                    def.ty
                )));
            };
            if map.is_empty() {
                return Err(invalid("enum map is empty".into()));
            }
            for (key, label) in map {
                let raw: i128 = key
                    .parse()
                    .map_err(|_| invalid(format!("key '{key}' is not an integer")))?;
                if raw < min || raw > max {
                    return Err(invalid(format!(
                        "key {raw} is out of range for {:?}",
                        def.ty
                    )));
                }
                if label.is_empty() {
                    return Err(invalid(format!("key {raw} has an empty label")));
                }
                if enum_by_label.insert(label.clone(), raw).is_some() {
                    return Err(invalid(format!("duplicate label '{label}'")));
                }
                enum_by_raw.insert(raw, label.clone());
            }
        }
        Ok(Signal {
            def,
            enum_by_raw,
            enum_by_label,
        })
    }

    pub fn def(&self) -> &SignalDef {
        &self.def
    }

    pub fn id(&self) -> u16 {
        self.def.id
    }

    pub fn name(&self) -> &str {
        &self.def.name
    }

    pub fn ty(&self) -> SignalType {
        self.def.ty
    }

    pub fn unit(&self) -> Option<&str> {
        self.def.unit.as_deref()
    }

    /// Byte order; single-byte types have no byte order and report `Big`.
    pub fn endian(&self) -> Endian {
        self.def.endian.unwrap_or(Endian::Big)
    }

    pub fn kind(&self) -> ValueKind {
        if self.def.ty == SignalType::Bool {
            ValueKind::Bool
        } else if self.def.enum_map.is_some() {
            ValueKind::Enum
        } else {
            ValueKind::Num
        }
    }

    pub fn enum_label(&self, raw: i128) -> Option<&str> {
        self.enum_by_raw.get(&raw).map(String::as_str)
    }

    pub fn enum_raw(&self, label: &str) -> Option<i128> {
        self.enum_by_label.get(label).copied()
    }
}

/// A validated signal catalog with lookups by id and name.
#[derive(Debug, Clone)]
pub struct Catalog {
    model: String,
    catalog_version: u32,
    signals: Vec<Signal>,
    by_id: HashMap<u16, usize>,
    by_name: HashMap<String, usize>,
}

impl Catalog {
    /// Parses and validates a catalog from JSON text.
    pub fn from_json(json: &str) -> Result<Self, CatalogError> {
        Self::new(serde_json::from_str(json)?)
    }

    /// Validates a parsed catalog document.
    pub fn new(spec: CatalogSpec) -> Result<Self, CatalogError> {
        if spec.model.trim().is_empty() {
            return Err(CatalogError::EmptyModel);
        }
        let mut signals = Vec::with_capacity(spec.signals.len());
        let mut by_id = HashMap::new();
        let mut by_name = HashMap::new();
        let mut seen_names = HashSet::new();
        for (idx, def) in spec.signals.into_iter().enumerate() {
            if by_id.insert(def.id, idx).is_some() {
                return Err(CatalogError::DuplicateId(def.id));
            }
            if !seen_names.insert(def.name.clone()) {
                return Err(CatalogError::DuplicateName(def.name));
            }
            by_name.insert(def.name.clone(), idx);
            signals.push(Signal::new(def)?);
        }
        Ok(Catalog {
            model: spec.model,
            catalog_version: spec.catalog_version,
            signals,
            by_id,
            by_name,
        })
    }

    pub fn model(&self) -> &str {
        &self.model
    }

    pub fn catalog_version(&self) -> u32 {
        self.catalog_version
    }

    pub fn signals(&self) -> impl Iterator<Item = &Signal> {
        self.signals.iter()
    }

    pub fn by_id(&self, id: u16) -> Option<&Signal> {
        self.by_id.get(&id).and_then(|&i| self.signals.get(i))
    }

    pub fn by_name(&self, name: &str) -> Option<&Signal> {
        self.by_name.get(name).and_then(|&i| self.signals.get(i))
    }

    /// Rebuilds the catalog document, e.g. to serve it over HTTP.
    pub fn to_spec(&self) -> CatalogSpec {
        CatalogSpec {
            model: self.model.clone(),
            catalog_version: self.catalog_version,
            signals: self.signals.iter().map(|s| s.def.clone()).collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const R1S: &str = include_str!("../../../catalogs/r1s.json");
    const R2: &str = include_str!("../../../catalogs/r2.json");

    fn one_signal(signal: &str) -> Result<Catalog, CatalogError> {
        Catalog::from_json(&format!(
            r#"{{"model":"T","catalog_version":1,"signals":[{signal}]}}"#
        ))
    }

    #[test]
    fn shipped_catalogs_load() {
        let r1s = Catalog::from_json(R1S).unwrap();
        let r2 = Catalog::from_json(R2).unwrap();
        assert_eq!(r1s.model(), "R1S");
        assert_eq!(r2.model(), "R2");
    }

    #[test]
    fn shared_names_differ_in_id_and_endianness() {
        let r1s = Catalog::from_json(R1S).unwrap();
        let r2 = Catalog::from_json(R2).unwrap();
        let mut endian_differs = false;
        for name in [
            "vehicle_speed",
            "battery_soc",
            "motor_temp",
            "gear",
            "door_open",
        ] {
            let a = r1s.by_name(name).unwrap();
            let b = r2.by_name(name).unwrap();
            assert_ne!(a.id(), b.id(), "{name} should have different ids");
            assert_eq!(a.kind(), b.kind(), "{name} should decode to the same kind");
            endian_differs |= a.ty().size() > 1 && b.ty().size() > 1 && a.endian() != b.endian();
        }
        assert!(endian_differs);
        let speed = r2.by_name("vehicle_speed").unwrap();
        assert_eq!(
            (speed.id(), speed.ty(), speed.endian()),
            (1025, SignalType::U32, Endian::Little)
        );
    }

    #[test]
    fn lookups_by_id_and_name() {
        let cat = Catalog::from_json(R1S).unwrap();
        assert_eq!(cat.by_id(257).unwrap().name(), "vehicle_speed");
        assert_eq!(cat.by_name("gear").unwrap().id(), 300);
        assert_eq!(cat.by_name("gear").unwrap().kind(), ValueKind::Enum);
        assert_eq!(cat.by_name("door_open").unwrap().kind(), ValueKind::Bool);
        assert!(cat.by_id(9999).is_none());
    }

    #[test]
    fn spec_round_trips_through_json() {
        let cat = Catalog::from_json(R1S).unwrap();
        let json = serde_json::to_string(&cat.to_spec()).unwrap();
        let again = Catalog::from_json(&json).unwrap();
        assert_eq!(again.to_spec(), cat.to_spec());
    }

    #[test]
    fn name_pattern() {
        for ok in ["a", "vehicle_speed", "x9_", &"a".repeat(64)] {
            assert!(is_valid_signal_name(ok), "{ok}");
        }
        for bad in ["", "9a", "_a", "Speed", "a-b", "a b", &"a".repeat(65)] {
            assert!(!is_valid_signal_name(bad), "{bad}");
        }
    }

    #[test]
    fn rejects_duplicate_id() {
        let err = one_signal(r#"{"id":1,"name":"a","type":"u8"},{"id":1,"name":"b","type":"u8"}"#)
            .unwrap_err();
        assert!(matches!(err, CatalogError::DuplicateId(1)));
    }

    #[test]
    fn rejects_duplicate_name() {
        let err = one_signal(r#"{"id":1,"name":"a","type":"u8"},{"id":2,"name":"a","type":"u8"}"#)
            .unwrap_err();
        assert!(matches!(err, CatalogError::DuplicateName(n) if n == "a"));
    }

    #[test]
    fn rejects_invalid_name() {
        let err = one_signal(r#"{"id":1,"name":"Speed","type":"u8"}"#).unwrap_err();
        assert!(matches!(err, CatalogError::InvalidName(_)));
    }

    #[test]
    fn requires_endian_for_multibyte_only() {
        let err = one_signal(r#"{"id":1,"name":"a","type":"u16"}"#).unwrap_err();
        assert!(matches!(err, CatalogError::MissingEndian(_)));
        one_signal(r#"{"id":1,"name":"a","type":"u8"}"#).unwrap();
        one_signal(r#"{"id":1,"name":"a","type":"bool"}"#).unwrap();
    }

    #[test]
    fn rejects_zero_scale() {
        let err = one_signal(r#"{"id":1,"name":"a","type":"u8","scale":0}"#).unwrap_err();
        assert!(matches!(err, CatalogError::InvalidScaleOffset(_)));
    }

    #[test]
    fn rejects_bad_enums() {
        for sig in [
            r#"{"id":1,"name":"a","type":"f32","endian":"big","enum":{"0":"P"}}"#,
            r#"{"id":1,"name":"a","type":"bool","enum":{"0":"P"}}"#,
            r#"{"id":1,"name":"a","type":"u8","enum":{}}"#,
            r#"{"id":1,"name":"a","type":"u8","enum":{"x":"P"}}"#,
            r#"{"id":1,"name":"a","type":"u8","enum":{"256":"P"}}"#,
            r#"{"id":1,"name":"a","type":"u8","enum":{"0":"P","1":"P"}}"#,
            r#"{"id":1,"name":"a","type":"u8","enum":{"0":""}}"#,
        ] {
            let err = one_signal(sig).unwrap_err();
            assert!(
                matches!(err, CatalogError::InvalidEnum { .. }),
                "{sig}: {err}"
            );
        }
    }

    #[test]
    fn rejects_unknown_fields_and_types() {
        assert!(one_signal(r#"{"id":1,"name":"a","type":"u8","scal":2}"#).is_err());
        assert!(one_signal(r#"{"id":1,"name":"a","type":"u128"}"#).is_err());
    }

    #[test]
    fn rejects_empty_model() {
        let err =
            Catalog::from_json(r#"{"model":" ","catalog_version":1,"signals":[]}"#).unwrap_err();
        assert!(matches!(err, CatalogError::EmptyModel));
    }
}
