//! Inference provider as a named entity: its type, and the address and authentication it reaches through references
//! that the host resolves (`agent-sessions/agents#FR-6`).

use std::fmt;

use serde::{Deserialize, Deserializer, Serialize};

/// Message of the refusal of a provider read from its serialized form. It names no field value.
const REFUSED: &str = "invalid provider: a name, a type or a reference is empty or refused";

/// Level at which a named entity lives.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ProviderLevel {
    /// The user level, the default.
    #[default]
    User,
    /// The project level.
    Project,
}

/// Named inference provider. It holds references only, never an address in clear with credentials nor a secret.
#[derive(Clone, PartialEq, Eq, Serialize)]
#[non_exhaustive]
pub struct Provider {
    name: String,
    kind: String,
    address: String,
    auth: Option<String>,
    level: ProviderLevel,
}

impl Provider {
    /// Builds a provider. Returns nothing when the name or the type is blank, or when the address or the authentication
    /// reference is empty, contains a blank character, holds credentials in a URL, or is shaped like a token.
    #[must_use]
    pub fn new(
        name: String,
        kind: String,
        address: String,
        auth: Option<String>,
        level: ProviderLevel,
    ) -> Option<Self> {
        let valid = !name.trim().is_empty()
            && !kind.trim().is_empty()
            && is_reference(&address)
            && auth.as_deref().is_none_or(is_reference);
        valid.then_some(Self {
            name,
            kind,
            address,
            auth,
            level,
        })
    }

    /// Returns the name of the provider.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns the type of the provider, for example `openai`.
    #[must_use]
    pub fn kind(&self) -> &str {
        &self.kind
    }

    /// Returns the reference of the address.
    #[must_use]
    pub fn address(&self) -> &str {
        &self.address
    }

    /// Returns the reference of the authentication, if any.
    #[must_use]
    pub fn auth(&self) -> Option<&str> {
        self.auth.as_deref()
    }

    /// Returns the level of the provider.
    #[must_use]
    pub const fn level(&self) -> ProviderLevel {
        self.level
    }
}

impl fmt::Debug for Provider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Provider")
            .field("name", &self.name)
            .field("kind", &self.kind)
            .field("level", &self.level)
            .finish_non_exhaustive()
    }
}

impl<'de> Deserialize<'de> for Provider {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct Record {
            name: String,
            kind: String,
            address: String,
            auth: Option<String>,
            level: ProviderLevel,
        }
        let record = Record::deserialize(deserializer)?;
        Self::new(
            record.name,
            record.kind,
            record.address,
            record.auth,
            record.level,
        )
        .ok_or_else(|| <D::Error as serde::de::Error>::custom(REFUSED))
    }
}

/// Tells whether `text` is a reference that the host resolves: a non-empty text without blank characters, with no
/// credentials in a URL (`://` followed by `@`), and not shaped like a token (prefix `sk-` or `Bearer`, any case).
fn is_reference(text: &str) -> bool {
    !text.is_empty()
        && !text.chars().any(char::is_whitespace)
        && !has_url_credentials(text)
        && !looks_like_token(text)
}

/// Tells whether `text` holds credentials in a URL: `://` followed by `@`.
fn has_url_credentials(text: &str) -> bool {
    text.split_once("://").is_some_and(|(_, rest)| rest.contains('@'))
}

/// Tells whether `text` is shaped like a token: prefix `sk-` or `Bearer`, in any case for both.
fn looks_like_token(text: &str) -> bool {
    has_prefix(text, "sk-") || has_prefix(text, "bearer")
}

/// Tells whether `text` starts with `prefix`, ignoring the case of ASCII letters. Never panics on a multi-byte text.
fn has_prefix(text: &str, prefix: &str) -> bool {
    text.get(..prefix.len())
        .is_some_and(|head| head.eq_ignore_ascii_case(prefix))
}

#[cfg(test)]
mod tests {
    use super::{Provider, ProviderLevel};

    fn provider(address: &str, auth: Option<&str>) -> Option<Provider> {
        Provider::new(
            "strix".to_owned(),
            "openai".to_owned(),
            address.to_owned(),
            auth.map(str::to_owned),
            ProviderLevel::User,
        )
    }

    #[test]
    fn references_are_names() {
        let built = provider("vllm-strix", Some("vllm-key")).expect("plain names are accepted");
        let text = serde_json::to_string(&built).expect("a provider serializes");
        let back: Provider = serde_json::from_str(&text).expect("a provider deserializes");
        assert_eq!(back, built, "the round trip must be identical");
        assert_eq!(back.address(), "vllm-strix", "the address reference is exposed");
        assert_eq!(back.auth(), Some("vllm-key"), "the auth reference is exposed");
    }

    #[test]
    fn credentials_in_clear_are_refused() {
        assert!(
            provider("https://user:pw@host/v1", None).is_none(),
            "an address with credentials in clear must give nothing"
        );
        assert!(
            provider("vllm-strix", Some("Bearer abc")).is_none(),
            "an auth reference holding a bearer token must give nothing"
        );
    }

    #[test]
    fn empty_fields_refused() {
        assert!(
            Provider::new(
                String::new(),
                "openai".to_owned(),
                "vllm-strix".to_owned(),
                None,
                ProviderLevel::User
            )
            .is_none(),
            "an empty name must give nothing"
        );
        assert!(provider("", None).is_none(), "an empty address must give nothing");
        assert!(
            provider("vllm-strix", Some("")).is_none(),
            "an empty auth reference must give nothing"
        );
    }

    #[test]
    fn deserialization_refuses_what_the_constructor_refuses() {
        let text = r#"{"name":"","kind":"openai","address":"vllm-strix","auth":null,"level":"user"}"#;
        assert!(
            serde_json::from_str::<Provider>(text).is_err(),
            "a JSON provider with an empty name must fail to deserialize"
        );
    }

    #[test]
    fn debug_hides_references() {
        let built = Provider::new(
            "strix".to_owned(),
            "openai".to_owned(),
            "addr-ref-1".to_owned(),
            Some("key-ref-1".to_owned()),
            ProviderLevel::Project,
        )
        .expect("plain names are accepted");
        let text = format!("{built:?}");
        assert!(
            !text.contains("addr-ref-1"),
            "Debug must not show the address reference: {text}"
        );
        assert!(
            !text.contains("key-ref-1"),
            "Debug must not show the auth reference: {text}"
        );
        assert!(text.contains("strix"), "Debug shows the name: {text}");
    }

    #[test]
    fn token_prefix_refused_in_any_case() {
        for token in ["sk-abc123", "SK-abc123", "Sk-abc123"] {
            assert!(
                provider(token, None).is_none(),
                "an address shaped like a token must give nothing: {token}"
            );
            assert!(
                provider("vllm-strix", Some(token)).is_none(),
                "an auth reference shaped like a token must give nothing: {token}"
            );
        }
    }

    #[test]
    fn bearer_prefix_refused_in_any_case() {
        for token in ["Bearer-abc", "bearer-abc", "BEARER-abc"] {
            assert!(
                provider("vllm-strix", Some(token)).is_none(),
                "an auth reference starting with Bearer must give nothing: {token}"
            );
        }
    }

    #[test]
    fn blank_inside_a_reference_refused() {
        assert!(
            provider("vllm strix", None).is_none(),
            "an address with a space must give nothing"
        );
        assert!(
            provider("vllm\tstrix", None).is_none(),
            "an address with a tab must give nothing"
        );
        assert!(
            provider("vllm-strix", Some("vllm key")).is_none(),
            "an auth reference with a space must give nothing"
        );
    }

    #[test]
    fn blank_name_and_type_refused() {
        let blank_name = Provider::new(
            "  ".to_owned(),
            "openai".to_owned(),
            "vllm-strix".to_owned(),
            None,
            ProviderLevel::User,
        );
        assert!(blank_name.is_none(), "a name made of blanks must give nothing");
        let blank_type = Provider::new(
            "strix".to_owned(),
            "  ".to_owned(),
            "vllm-strix".to_owned(),
            None,
            ProviderLevel::User,
        );
        assert!(blank_type.is_none(), "a type made of blanks must give nothing");
        let empty_type = Provider::new(
            "strix".to_owned(),
            String::new(),
            "vllm-strix".to_owned(),
            None,
            ProviderLevel::User,
        );
        assert!(empty_type.is_none(), "an empty type must give nothing");
    }

    #[test]
    fn deserialization_refuses_clear_references() {
        let credentials_in_url = r#"{"name":"strix","kind":"openai","address":"https://user:pw@host/v1","auth":null,"level":"user"}"#;
        assert!(
            serde_json::from_str::<Provider>(credentials_in_url).is_err(),
            "a JSON provider with credentials in its address must fail to deserialize"
        );
        let token_in_auth =
            r#"{"name":"strix","kind":"openai","address":"vllm-strix","auth":"sk-abc123","level":"user"}"#;
        assert!(
            serde_json::from_str::<Provider>(token_in_auth).is_err(),
            "a JSON provider with a token in its auth reference must fail to deserialize"
        );
        let blank_type = r#"{"name":"strix","kind":"  ","address":"vllm-strix","auth":null,"level":"user"}"#;
        assert!(
            serde_json::from_str::<Provider>(blank_type).is_err(),
            "a JSON provider with a blank type must fail to deserialize"
        );
    }
}
