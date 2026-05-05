//! Conversions between `serde_json::Value` and `nodedb_types::Value`.
//!
//! Shared across crates to avoid duplicating JSON-to-Value logic.

use crate::Value;

/// Convert a `serde_json::Value` to a `Value` by consuming ownership.
///
/// Nested objects are preserved as `Value::Object`.
pub fn json_to_value(v: serde_json::Value) -> Value {
    match v {
        serde_json::Value::Null => Value::Null,
        serde_json::Value::Bool(b) => Value::Bool(b),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Value::Integer(i)
            } else {
                Value::Float(n.as_f64().unwrap_or(0.0))
            }
        }
        serde_json::Value::String(s) => Value::String(s),
        serde_json::Value::Array(arr) => Value::Array(arr.into_iter().map(json_to_value).collect()),
        serde_json::Value::Object(obj) => Value::Object(
            obj.into_iter()
                .map(|(k, v)| (k, json_to_value(v)))
                .collect(),
        ),
    }
}

/// Convert a `&serde_json::Value` to a `Value` by reference (cloning).
///
/// Nested objects are serialized to JSON strings for tabular display.
pub fn json_to_value_display(v: &serde_json::Value) -> Value {
    match v {
        serde_json::Value::Null => Value::Null,
        serde_json::Value::Bool(b) => Value::Bool(*b),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Value::Integer(i)
            } else {
                Value::Float(n.as_f64().unwrap_or(0.0))
            }
        }
        serde_json::Value::String(s) => Value::String(s.clone()),
        serde_json::Value::Array(arr) => {
            Value::Array(arr.iter().map(json_to_value_display).collect())
        }
        serde_json::Value::Object(_) => Value::String(sonic_rs::to_string(v).unwrap_or_default()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn owned_preserves_nested_objects() {
        let v = serde_json::json!({"a": 1, "b": {"nested": true}});
        let val = json_to_value(v);
        match val {
            Value::Object(map) => {
                assert_eq!(map.get("a"), Some(&Value::Integer(1)));
                assert!(matches!(map.get("b"), Some(Value::Object(_))));
            }
            _ => panic!("expected Object"),
        }
    }

    #[test]
    fn display_flattens_nested_objects() {
        let v = serde_json::json!({"nested": true});
        let val = json_to_value_display(&v);
        assert!(matches!(val, Value::String(_)));
    }

    #[test]
    fn primitives_roundtrip() {
        assert_eq!(json_to_value(serde_json::Value::Null), Value::Null);
        assert_eq!(
            json_to_value(serde_json::Value::Bool(true)),
            Value::Bool(true)
        );
        assert_eq!(json_to_value(serde_json::json!(42)), Value::Integer(42));
        assert_eq!(
            json_to_value(serde_json::json!("hello")),
            Value::String("hello".into())
        );
    }
}
