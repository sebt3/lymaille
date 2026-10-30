//! Bounded rendering of a Rhai value returned to the LLM: the text of a string as is, compact JSON for the other values,
//! a cap in characters, and a truncation that keeps the start and the end of the text with an explicit notice.

use rhai::{Array, Blob, Dynamic, ImmutableString, Map};

/// Rendered text of a value, with the size of the original text and whether characters were removed.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Rendered {
    text: String,
    original_chars: usize,
    truncated: bool,
}

impl Rendered {
    /// Builds a rendered text from its text, the number of characters of the original and the truncation flag.
    #[must_use]
    pub const fn new(text: String, original_chars: usize, truncated: bool) -> Self {
        Self {
            text,
            original_chars,
            truncated,
        }
    }

    /// Rendered text, notice included when the text was truncated.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Number of characters of the original text.
    #[must_use]
    pub const fn original_chars(&self) -> usize {
        self.original_chars
    }

    /// Whether characters of the original text were removed.
    #[must_use]
    pub const fn truncated(&self) -> bool {
        self.truncated
    }
}

/// Complete text of a value, without cap.
///
/// A string is kept as is, a char is one character, the unit is `()`, a blob is its size, an array or a map is compact
/// JSON, and any other value, or a value that JSON refuses, is its Rhai text.
#[must_use]
pub fn full_text(value: &Dynamic) -> String {
    if let Some(text) = value.read_lock::<ImmutableString>() {
        return text.to_string();
    }
    if value.is_unit() {
        return "()".to_owned();
    }
    if let Some(blob) = value.read_lock::<Blob>() {
        return blob_text(blob.len());
    }
    if (value.is_array() || value.is_map())
        && let Some(text) = structured_text(value)
    {
        return text;
    }
    value.to_string()
}

/// Compact JSON of an array or a map, when every value inside has a JSON form.
fn structured_text(value: &Dynamic) -> Option<String> {
    let json = json_value(value)?;
    serde_json::to_string(&json).ok()
}

/// JSON form of a value: `None` when JSON refuses it (function pointer, non-finite float, custom type). A unit is `null`,
/// a char a string, a blob its description; maps keep the sorted order of Rhai.
pub(in crate::scratchpad) fn json_value(value: &Dynamic) -> Option<serde_json::Value> {
    use serde_json::Value;
    if value.is_unit() {
        return Some(Value::Null);
    }
    if let Some(text) = value.read_lock::<ImmutableString>() {
        return Some(Value::String(text.to_string()));
    }
    if let Ok(character) = value.as_char() {
        return Some(Value::String(character.to_string()));
    }
    if let Ok(flag) = value.as_bool() {
        return Some(Value::Bool(flag));
    }
    if let Ok(number) = value.as_int() {
        return Some(Value::from(number));
    }
    if let Ok(number) = value.as_float() {
        return serde_json::Number::from_f64(number).map(Value::Number);
    }
    if let Some(items) = value.read_lock::<Array>() {
        return items
            .iter()
            .map(json_value)
            .collect::<Option<Vec<Value>>>()
            .map(Value::Array);
    }
    if let Some(map) = value.read_lock::<Map>() {
        return map
            .iter()
            .map(|(key, item)| json_value(item).map(|json| (key.to_string(), json)))
            .collect::<Option<serde_json::Map<String, Value>>>()
            .map(Value::Object);
    }
    if let Some(blob) = value.read_lock::<Blob>() {
        return Some(Value::String(blob_text(blob.len())));
    }
    None
}

/// Description of a blob by its size.
fn blob_text(bytes: usize) -> String {
    format!("[blob: {bytes} bytes]")
}

/// Rendered text of a value under `cap` characters: the complete text when it fits, a truncated text otherwise.
#[must_use]
pub fn render_value(value: &Dynamic, cap: usize) -> Rendered {
    truncate_middle(&full_text(value), cap)
}

/// Truncates a text under `cap` characters: keeps its start and its end around a notice of the omitted characters.
#[must_use]
pub fn truncate_middle(text: &str, cap: usize) -> Rendered {
    let total = text.chars().count();
    if total <= cap {
        return Rendered::new(text.to_owned(), total, false);
    }
    // The notice is sized with the width of `total`, the widest count it can show: the text never exceeds the cap.
    let reserved = notice(total, total).chars().count();
    if cap <= reserved {
        let head: String = notice(total, total).chars().take(cap).collect();
        return Rendered::new(head, total, true);
    }
    let place = cap.saturating_sub(reserved);
    let head_len = place.div_ceil(2);
    let tail_len = place.saturating_sub(head_len);
    let omitted = total.saturating_sub(place);
    let head: String = text.chars().take(head_len).collect();
    let tail: String = text.chars().skip(total.saturating_sub(tail_len)).collect();
    Rendered::new(format!("{head}{}{tail}", notice(omitted, total)), total, true)
}

/// Notice that replaces the omitted characters of a text.
fn notice(omitted: usize, total: usize) -> String {
    format!(
        "\n[... output truncated: {omitted} of {total} characters omitted. Store the result in a variable and extract the useful part before returning it ...]\n"
    )
}

#[cfg(test)]
mod tests {
    use super::{Rendered, full_text, render_value, truncate_middle};
    use rhai::{Dynamic, Engine, ImmutableString};

    fn map_of(pairs: &[(&str, Dynamic)]) -> Dynamic {
        let mut map = rhai::Map::new();
        for (key, value) in pairs {
            let _previous = map.insert((*key).into(), value.clone());
        }
        Dynamic::from_map(map)
    }

    fn notice_for(omitted: usize, total: usize) -> String {
        format!(
            "\n[... output truncated: {omitted} of {total} characters omitted. Store the result in a variable and extract the useful part before returning it ...]\n"
        )
    }

    #[test]
    fn a_string_is_rendered_without_quotes() {
        let rendered = render_value(&Dynamic::from("hello"), 100);
        assert_eq!(rendered.text(), "hello", "the text");
        assert!(!rendered.truncated(), "not truncated");
    }

    #[test]
    fn a_map_is_rendered_as_compact_json() {
        let value = map_of(&[("b", Dynamic::from(2i64)), ("a", Dynamic::from("x"))]);
        let rendered = render_value(&value, 100);
        assert_eq!(
            rendered.text(),
            r#"{"a":"x","b":2}"#,
            "compact JSON with sorted keys"
        );
    }

    #[test]
    fn scalars_and_unit() {
        assert_eq!(full_text(&Dynamic::from(42i64)), "42", "integer");
        assert_eq!(full_text(&Dynamic::from(1.5f64)), "1.5", "float");
        assert_eq!(full_text(&Dynamic::from(true)), "true", "boolean");
        assert_eq!(full_text(&Dynamic::from('x')), "x", "char");
        assert_eq!(full_text(&Dynamic::UNIT), "()", "unit");
    }

    #[test]
    fn non_finite_float_keeps_its_rhai_text() {
        let nan = full_text(&Dynamic::from(f64::NAN));
        let infinity = full_text(&Dynamic::from(f64::INFINITY));
        assert_eq!(nan, "NaN", "NaN");
        assert_eq!(infinity, "inf", "infinity");
    }

    #[test]
    fn blob_is_described_not_dumped() {
        let value = Dynamic::from_blob(vec![1, 2, 3]);
        assert_eq!(full_text(&value), "[blob: 3 bytes]", "blob description");
    }

    // Scenario: a blob nested in a map or an array is described by its size
    #[test]
    fn nested_blob_is_described_by_its_size() {
        let in_map = map_of(&[("b", Dynamic::from_blob(vec![1, 2, 3]))]);
        assert_eq!(
            full_text(&in_map),
            r#"{"b":"[blob: 3 bytes]"}"#,
            "a blob inside a map"
        );
        let in_array = Dynamic::from(vec![Dynamic::from_blob(vec![9])]);
        assert_eq!(
            full_text(&in_array),
            r#"["[blob: 1 bytes]"]"#,
            "a blob inside an array"
        );
    }

    #[test]
    fn a_non_serializable_value_falls_back_to_its_display() {
        let engine = Engine::new();
        let value = match engine.eval::<Dynamic>("Fn(\"abs\")") {
            Ok(value) => value,
            Err(error) => panic!("the function pointer must evaluate: {error}"),
        };
        let text = full_text(&value);
        assert!(!text.is_empty(), "a text is produced: {text}");
    }

    #[test]
    fn text_at_exactly_the_cap_is_untouched() {
        let text = "a".repeat(8000);
        let rendered = render_value(&Dynamic::from(text.clone()), 8000);
        assert_eq!(rendered.text(), text, "the text is unchanged");
        assert!(!rendered.truncated(), "not truncated");
    }

    /// Number written after `truncated: ` in the notice of a text.
    fn omitted_in(text: &str) -> Option<usize> {
        text.split("truncated: ").nth(1)?.split(' ').next()?.parse().ok()
    }

    #[test]
    fn oversized_output_is_truncated_with_a_notice() {
        let text = "az".repeat(10_000);
        let rendered = render_value(&Dynamic::from(text), 8000);
        let length = rendered.text().chars().count();
        assert!(length <= 8000, "at most the cap: {length}");
        assert!(
            rendered.text().starts_with('a'),
            "starts with the first characters"
        );
        assert!(rendered.text().ends_with('z'), "ends with the last characters");
        assert!(
            rendered.text().contains("output truncated"),
            "the notice is there"
        );
        assert!(rendered.truncated(), "truncated");
        assert_eq!(rendered.original_chars(), 20_000, "original size");
        let omitted = omitted_in(rendered.text()).unwrap_or(0);
        let notice = notice_for(omitted, 20_000);
        let Some((head, tail)) = rendered.text().split_once(&notice) else {
            panic!("the notice `{notice}` must sit between the kept start and end");
        };
        let kept = head.chars().count() + tail.chars().count();
        assert_eq!(
            20_000usize.checked_sub(kept),
            Some(omitted),
            "the notice states the original size minus the {kept} kept characters"
        );
        assert!(
            omitted > 0,
            "the notice gives a positive omitted count: {omitted}"
        );
    }

    #[test]
    fn head_and_tail_sizes() {
        let text = "abcdefghij".repeat(100);
        let rendered = truncate_middle(&text, 400);
        let notice_len = notice_for(1000, 1000).chars().count();
        let place = 400usize.saturating_sub(notice_len);
        let head_len = place.div_ceil(2);
        let tail_len = place.saturating_sub(head_len);
        let head: String = text.chars().take(head_len).collect();
        let tail: String = text.chars().skip(1000usize.saturating_sub(tail_len)).collect();
        assert!(
            rendered.text().starts_with(&head),
            "the head holds ceil(place / 2) characters"
        );
        assert!(
            rendered.text().ends_with(&tail),
            "the tail holds floor(place / 2) characters"
        );
        let length = rendered.text().chars().count();
        assert!(
            length == 400 || length.saturating_add(1) == 400,
            "the total is the cap or one shorter: {length}"
        );
    }

    #[test]
    fn truncation_does_not_split_a_multi_byte_character() {
        let text = "\u{e9}".repeat(20_000);
        let rendered = render_value(&Dynamic::from(text), 1000);
        let notice_chars: Vec<char> = notice_for(20_000, 20_000).chars().collect();
        for character in rendered.text().chars() {
            assert!(
                character == '\u{e9}' || character.is_ascii_digit() || notice_chars.contains(&character),
                "unexpected character `{character}`"
            );
        }
        assert!(rendered.text().chars().count() <= 1000, "at most the cap");
        assert!(rendered.truncated(), "truncated");
        assert!(
            rendered.text().contains('\u{e9}'),
            "the start and the end of the text are kept"
        );
    }

    #[test]
    fn cap_smaller_than_the_notice() {
        let text = "x".repeat(500);
        let rendered = truncate_middle(&text, 10);
        let expected: String = notice_for(500, 500).chars().take(10).collect();
        assert_eq!(rendered.text(), expected, "the first 10 characters of the notice");
        assert!(rendered.truncated(), "truncated");
    }

    #[test]
    fn zero_cap() {
        let rendered = truncate_middle("abc", 0);
        assert_eq!(rendered.text(), "", "empty text");
        assert!(rendered.truncated(), "truncated");
    }

    #[test]
    fn empty_text_is_never_truncated() {
        let rendered = render_value(&Dynamic::from(ImmutableString::from("")), 0);
        assert_eq!(
            rendered,
            Rendered::new(String::new(), 0, false),
            "empty and not truncated"
        );
        let control = render_value(&Dynamic::from("x"), 0);
        assert!(
            control.truncated(),
            "control: a non empty text under a zero cap is truncated"
        );
    }

    #[test]
    fn full_text_has_no_cap() {
        let array: Vec<Dynamic> = (0..100_000i64).map(Dynamic::from).collect();
        let text = full_text(&Dynamic::from_array(array));
        assert!(text.starts_with("[0,1,2,"), "compact JSON of the whole array");
        assert!(text.ends_with(",99999]"), "the last element is there");
    }

    #[test]
    fn rendering_is_deterministic() {
        let keys: Vec<String> = (0..10).map(|index| format!("k{index}")).collect();
        let forward: Vec<(&str, Dynamic)> = keys
            .iter()
            .map(|key| (key.as_str(), Dynamic::from(1i64)))
            .collect();
        let backward: Vec<(&str, Dynamic)> = forward.iter().rev().cloned().collect();
        let first = full_text(&map_of(&forward));
        let second = full_text(&map_of(&backward));
        assert_eq!(first, second, "the same map gives the same text");
        assert!(
            first.starts_with("{\"k0\":1"),
            "the keys are in the sorted order of Rhai: {first}"
        );
    }
}
