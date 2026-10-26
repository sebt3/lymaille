//! Scratchpad variable value tagged with its real type, so that a restore gives back an identical type where JSON does not
//! distinguish one (`char`, `blob`, integer and float) (`agent-sessions/session#FR-9`, `agent-sessions/session#AC-5`).

use std::collections::BTreeMap;

use serde::ser::{SerializeStruct as _, Serializer};
use serde::{Deserialize, Serialize};

/// A variable value of the scratchpad, tagged with its real type.
///
/// JSON serialization tags each value with an explicit type field, such as `{"type": "char", "value": "x"}`, so that a
/// `char`, a string, an integer, a float and a blob never run into each other. A non-finite float cannot be written in
/// JSON: its serialization fails instead of writing `null`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
#[non_exhaustive]
pub enum TaggedValue {
    /// The unit value.
    Unit,
    /// A boolean.
    Bool(bool),
    /// A signed integer.
    Int(i64),
    /// A floating point number. A non-finite value cannot be serialized.
    Float(f64),
    /// A single character.
    Char(char),
    /// A string.
    String(String),
    /// Raw bytes, serialized as an array of integers.
    Blob(Vec<u8>),
    /// An ordered array of values.
    Array(Vec<Self>),
    /// A table of values, ordered by key so that the serialization is reproducible.
    Map(BTreeMap<String, Self>),
}

/// Writes a value as `{"type": tag, "value": value}`.
fn write_tagged<S, V>(serializer: S, tag: &'static str, value: &V) -> Result<S::Ok, S::Error>
where
    S: Serializer,
    V: Serialize + ?Sized,
{
    let mut state = serializer.serialize_struct("TaggedValue", 2)?;
    state.serialize_field("type", tag)?;
    state.serialize_field("value", value)?;
    state.end()
}

impl Serialize for TaggedValue {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Unit => {
                let mut state = serializer.serialize_struct("TaggedValue", 1)?;
                state.serialize_field("type", "unit")?;
                state.end()
            }
            Self::Bool(value) => write_tagged(serializer, "bool", value),
            Self::Int(value) => write_tagged(serializer, "int", value),
            Self::Float(value) => {
                if value.is_finite() {
                    write_tagged(serializer, "float", value)
                } else {
                    Err(<S::Error as serde::ser::Error>::custom(
                        "a non-finite float cannot be serialized",
                    ))
                }
            }
            Self::Char(value) => write_tagged(serializer, "char", value),
            Self::String(value) => write_tagged(serializer, "string", value),
            Self::Blob(bytes) => write_tagged(serializer, "blob", bytes),
            Self::Array(items) => write_tagged(serializer, "array", items),
            Self::Map(table) => write_tagged(serializer, "map", table),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::TaggedValue;

    fn round_trip(value: &TaggedValue) -> TaggedValue {
        let json = serde_json::to_string(value).expect("a finite value serializes");
        serde_json::from_str(&json).expect("a serialized value deserializes")
    }

    #[test]
    fn char_blob_and_integer_keep_their_type() {
        let char_value = TaggedValue::Char('x');
        let blob_value = TaggedValue::Blob(vec![1, 2, 255]);
        let int_value = TaggedValue::Int(7);

        let char_back = round_trip(&char_value);
        let blob_back = round_trip(&blob_value);
        let int_back = round_trip(&int_value);

        assert!(
            matches!(char_back, TaggedValue::Char('x')),
            "the char `x` must come back as a char, not as a string"
        );
        assert!(
            matches!(&blob_back, TaggedValue::Blob(bytes) if bytes == &[1, 2, 255]),
            "the blob must come back as a blob with the same three bytes"
        );
        assert!(
            matches!(int_back, TaggedValue::Int(7)),
            "the integer 7 must come back as an integer"
        );
    }

    #[test]
    fn char_serializes_with_an_explicit_tag() {
        let json = serde_json::to_string(&TaggedValue::Char('x')).expect("a char serializes");
        assert_eq!(
            json, r#"{"type":"char","value":"x"}"#,
            "a char must be tagged by its explicit type field"
        );
    }

    #[test]
    fn ambiguous_pairs_stay_distinct() {
        let string = round_trip(&TaggedValue::String("x".to_owned()));
        let char_value = round_trip(&TaggedValue::Char('x'));
        let int = round_trip(&TaggedValue::Int(1));
        let float = round_trip(&TaggedValue::Float(1.0));

        assert!(
            matches!(&string, TaggedValue::String(text) if text == "x"),
            "the string `x` must come back as a string"
        );
        assert!(
            matches!(char_value, TaggedValue::Char('x')),
            "the char `x` must come back as a char"
        );
        assert!(
            matches!(int, TaggedValue::Int(1)),
            "the integer 1 must come back as an integer"
        );
        assert!(
            matches!(float, TaggedValue::Float(value) if value.to_bits() == 1.0f64.to_bits()),
            "the float 1.0 must come back as a float"
        );
    }

    #[test]
    fn nested_values() {
        let table: BTreeMap<String, TaggedValue> = [
            (
                "items".to_owned(),
                TaggedValue::Array(vec![TaggedValue::Char('a'), TaggedValue::Blob(vec![9, 8])]),
            ),
            ("unit".to_owned(), TaggedValue::Unit),
            ("flag".to_owned(), TaggedValue::Bool(true)),
        ]
        .into_iter()
        .collect();
        let value = TaggedValue::Map(table);

        assert_eq!(
            round_trip(&value),
            value,
            "a map holding an array of a char and a blob must come back with the same nested structure"
        );
    }

    #[test]
    fn non_finite_float_refused() {
        let result = serde_json::to_string(&TaggedValue::Float(f64::NAN));
        assert!(
            result.is_err(),
            "a NaN float must fail to serialize instead of being written"
        );
        let infinite = serde_json::to_string(&TaggedValue::Float(f64::INFINITY));
        assert!(
            infinite.is_err(),
            "an infinite float must fail to serialize instead of being written"
        );
    }

    #[test]
    fn map_serializes_in_key_order() {
        let table: BTreeMap<String, TaggedValue> = [
            ("zeta".to_owned(), TaggedValue::Int(2)),
            ("alpha".to_owned(), TaggedValue::Int(1)),
        ]
        .into_iter()
        .collect();
        let json = serde_json::to_string(&TaggedValue::Map(table)).expect("a map serializes");
        let alpha = json.find("alpha").expect("the key alpha must be serialized");
        let zeta = json.find("zeta").expect("the key zeta must be serialized");
        assert!(
            alpha < zeta,
            "the map entries must be serialized in key order, so that the output is reproducible"
        );
    }

    #[test]
    fn unknown_tag_refused() {
        let result: Result<TaggedValue, _> = serde_json::from_str(r#"{"type": "decimal", "value": "1.5"}"#);
        assert!(
            result.is_err(),
            "an unknown type tag must fail to deserialize, not fall back to a default value"
        );
    }
}
