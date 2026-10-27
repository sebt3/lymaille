//! Masking of the secrets held by the library, in any text or JSON value destined for events
//! (`agent-sessions/session#FR-8`, `agent-sessions#CON-3`).

use std::cmp::Reverse;
use std::fmt;

use serde_json::Value;

use super::{SessionError, SessionResult};

/// Text that replaces every occurrence of a secret.
const MASK: &str = "***";

/// Shortest secret, in characters, that may be declared: a shorter one would mask too much of the text.
const MIN_SECRET_CHARS: usize = 6;

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

    /// Declares a secret to mask. An empty text is ignored; a non-empty text shorter than six characters is refused.
    ///
    /// # Errors
    ///
    /// [`SessionError::SecretTooShort`] when the secret is non-empty and has fewer than six characters. The secret is then
    /// not retained.
    pub fn add_secret<S: Into<String>>(&mut self, secret: S) -> SessionResult<()> {
        let secret = secret.into();
        if secret.is_empty() {
            return Ok(());
        }
        if secret.chars().count() < MIN_SECRET_CHARS {
            return Err(SessionError::SecretTooShort);
        }
        self.secrets.push(secret);
        self.secrets.sort_by_key(|known| Reverse(known.len()));
        Ok(())
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
    use crate::session::SessionError;

    #[test]
    fn a_secret_is_masked_everywhere() {
        let mut redactor = Redactor::new();
        redactor
            .add_secret("tok-123")
            .expect("a seven-character secret is accepted");
        assert_eq!(
            redactor.redact("Bearer tok-123 and tok-123"),
            "Bearer *** and ***",
            "every occurrence of the secret must be masked"
        );
    }

    #[test]
    fn longest_secret_first() {
        let mut redactor = Redactor::new();
        redactor
            .add_secret("abcdef")
            .expect("a six-character secret is accepted");
        redactor
            .add_secret("abcdefgh")
            .expect("an eight-character secret is accepted");
        assert_eq!(
            redactor.redact("xabcdefghx"),
            "x***x",
            "the longer secret must be masked whole, leaving no remainder of it"
        );
    }

    #[test]
    fn short_secret_refused() {
        let mut redactor = Redactor::new();
        let refused = redactor.add_secret("abc");
        assert!(
            matches!(refused, Err(SessionError::SecretTooShort)),
            "a secret of three characters must be refused with SecretTooShort, got {refused:?}"
        );
        assert_eq!(
            redactor.redact("abc"),
            "abc",
            "a refused secret must not be retained, so nothing is masked"
        );
    }

    #[test]
    fn secret_of_six_characters_accepted() {
        let mut redactor = Redactor::new();
        redactor
            .add_secret("s3cr3t")
            .expect("a secret of six characters is accepted");
        assert_eq!(
            redactor.redact("s3cr3t"),
            "***",
            "an accepted six-character secret must be masked"
        );
    }

    #[test]
    fn length_counted_in_characters() {
        let mut redactor = Redactor::new();
        let five = redactor.add_secret("\u{e9}".repeat(5));
        assert!(
            matches!(five, Err(SessionError::SecretTooShort)),
            "five `\u{e9}` are five characters, so they are refused, got {five:?}"
        );
        redactor
            .add_secret("\u{e9}".repeat(6))
            .expect("six `\u{e9}` are six characters, so they are accepted");
    }

    #[test]
    fn json_strings_and_keys() {
        let mut redactor = Redactor::new();
        redactor
            .add_secret("s3cr3t")
            .expect("a six-character secret is accepted");
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
        redactor
            .add_secret("s3cr3t")
            .expect("a six-character secret is accepted");
        let text = format!("{redactor:?}");
        assert!(
            !text.contains("s3cr3t"),
            "the Debug text of a redactor must not show a secret, got {text}"
        );
    }

    #[test]
    fn empty_secret_ignored() {
        let mut redactor = Redactor::new();
        redactor
            .add_secret("")
            .expect("an empty secret is ignored, not refused");
        assert_eq!(
            redactor.redact("hello"),
            "hello",
            "an empty secret must not mask anything"
        );
    }

    #[test]
    fn masking_is_idempotent() {
        let mut redactor = Redactor::new();
        redactor
            .add_secret("tok-123")
            .expect("a seven-character secret is accepted");
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
        redactor
            .add_secret("tok-123")
            .expect("a seven-character secret is accepted");
        assert_eq!(
            redactor.redact("nothing to hide here"),
            "nothing to hide here",
            "a text without a secret must come back unchanged"
        );
    }
}
