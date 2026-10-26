//! Masking of the secrets held by the library, in any text or JSON value destined for events
//! (`agent-sessions/session#FR-8`, `agent-sessions#CON-3`).

use std::cmp::Reverse;
use std::fmt;

use serde_json::Value;

/// Text that replaces every occurrence of a secret.
const MASK: &str = "***";

/// Set of secrets to mask. Only the secrets declared with [`Redactor::add_secret`] are masked: nothing is guessed.
#[derive(Default)]
#[non_exhaustive]
pub struct Redactor {
    /// Declared secrets, kept longest first so that a longer secret is masked before a shorter one it contains.
    secrets: Vec<String>,
}

impl Redactor {
    /// Builds a redactor that masks nothing yet.
    #[must_use]
    pub const fn new() -> Self {
        Self { secrets: Vec::new() }
    }

    /// Declares a secret to mask. An empty text is ignored.
    pub fn add_secret<S: Into<String>>(&mut self, secret: S) {
        let secret = secret.into();
        if secret.is_empty() {
            return;
        }
        self.secrets.push(secret);
        self.secrets.sort_by_key(|known| Reverse(known.len()));
    }

    /// Returns `text` where every occurrence of a declared secret is replaced by `***`.
    ///
    /// At each position the longest secret that starts there is masked, so a secret that contains a shorter one
    /// leaves no remainder of it.
    #[must_use]
    pub fn redact(&self, text: &str) -> String {
        let mut redacted = String::with_capacity(text.len());
        let mut rest = text;
        while let Some(first) = rest.chars().next() {
            if let Some(secret) = self
                .secrets
                .iter()
                .find(|secret| rest.starts_with(secret.as_str()))
            {
                redacted.push_str(MASK);
                rest = rest.get(secret.len()..).unwrap_or_default();
            } else {
                redacted.push(first);
                rest = rest.get(first.len_utf8()..).unwrap_or_default();
            }
        }
        redacted
    }

    /// Returns `value` where every string, object value and object key is masked as [`Redactor::redact`] does.
    /// Numbers, booleans and nulls are returned unchanged.
    #[must_use]
    pub fn redact_json(&self, value: &Value) -> Value {
        match value {
            Value::String(text) => Value::String(self.redact(text)),
            Value::Array(items) => Value::Array(items.iter().map(|item| self.redact_json(item)).collect()),
            Value::Object(fields) => Value::Object(
                fields
                    .iter()
                    .map(|(key, item)| (self.redact(key), self.redact_json(item)))
                    .collect(),
            ),
            Value::Null | Value::Bool(_) | Value::Number(_) => value.clone(),
        }
    }
}

impl fmt::Debug for Redactor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Redactor")
            .field("secret_count", &self.secrets.len())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::Redactor;

    #[test]
    fn a_secret_is_masked_everywhere() {
        let mut redactor = Redactor::new();
        redactor.add_secret("tok-123");
        assert_eq!(
            redactor.redact("Bearer tok-123 and tok-123"),
            "Bearer *** and ***",
            "every occurrence of the secret must be masked"
        );
    }

    #[test]
    fn longest_secret_first() {
        let mut redactor = Redactor::new();
        redactor.add_secret("abc");
        redactor.add_secret("abcdef");
        assert_eq!(
            redactor.redact("xabcdefx"),
            "x***x",
            "the longer secret must be masked whole, leaving no remainder of it"
        );
    }

    #[test]
    fn json_strings_and_keys() {
        let mut redactor = Redactor::new();
        redactor.add_secret("s3cr3t");
        let value = json!({"s3cr3t": ["a s3cr3t b", 1]});
        assert_eq!(
            redactor.redact_json(&value),
            json!({"***": ["a *** b", 1]}),
            "the secret must be masked in object keys and in string values, and numbers stay as they are"
        );
    }

    #[test]
    fn debug_hides_the_secrets() {
        let mut redactor = Redactor::new();
        redactor.add_secret("s3cr3t");
        let text = format!("{redactor:?}");
        assert!(
            !text.contains("s3cr3t"),
            "the Debug text of a redactor must not show a secret, got {text}"
        );
    }

    #[test]
    fn empty_secret_ignored() {
        let mut redactor = Redactor::new();
        redactor.add_secret("");
        assert_eq!(
            redactor.redact("hello"),
            "hello",
            "an empty secret must not mask anything"
        );
    }

    #[test]
    fn masking_is_idempotent() {
        let mut redactor = Redactor::new();
        redactor.add_secret("tok-123");
        let once = redactor.redact("a tok-123 b");
        assert_eq!(
            redactor.redact(&once),
            once,
            "masking a masked text must change nothing"
        );
    }

    #[test]
    fn text_without_secret_is_unchanged() {
        let mut redactor = Redactor::new();
        redactor.add_secret("tok-123");
        assert_eq!(
            redactor.redact("nothing to hide here"),
            "nothing to hide here",
            "a text without a secret must come back unchanged"
        );
    }
}
