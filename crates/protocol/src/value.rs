use serde::{Deserialize, Serialize};

/// A decoded, physical signal value.
///
/// Serializes untagged, so in NDJSON output it appears as a plain JSON
/// bool, number or string.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum SignalValue {
    Bool(bool),
    Num(f64),
    Enum(String),
}

impl SignalValue {
    pub fn as_num(&self) -> Option<f64> {
        match self {
            SignalValue::Num(v) => Some(*v),
            _ => None,
        }
    }
}

impl std::fmt::Display for SignalValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SignalValue::Bool(v) => write!(f, "{v}"),
            SignalValue::Num(v) => write!(f, "{v}"),
            SignalValue::Enum(v) => write!(f, "{v}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serializes_untagged() {
        let json = serde_json::to_string(&[
            SignalValue::Bool(true),
            SignalValue::Num(63.4),
            SignalValue::Enum("D".into()),
        ])
        .unwrap();
        assert_eq!(json, r#"[true,63.4,"D"]"#);
    }

    #[test]
    fn deserializes_untagged() {
        let v: Vec<SignalValue> = serde_json::from_str(r#"[false, 1, "P"]"#).unwrap();
        assert_eq!(
            v,
            vec![
                SignalValue::Bool(false),
                SignalValue::Num(1.0),
                SignalValue::Enum("P".into())
            ]
        );
    }
}
