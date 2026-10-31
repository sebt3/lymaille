//! Heuristic that tells whether a text looks like a secret, shared by the context and provider checks, so that the two
//! can never diverge (`agent-sessions/toolsets#FR-3`, `agent-sessions#CON-3`). The real control stays with the host.

/// Tells whether `text` looks like a secret: credentials in a URL (`://` followed by `@`), or the prefix `sk-` or
/// `Bearer`, in any case. Only the start of the text is checked for the prefixes.
#[expect(
    clippy::pub_with_shorthand,
    reason = "rustfmt rewrites pub(in crate) into pub(crate), which this lint refuses: both are the crate visibility that unreachable_pub requires"
)]
pub(crate) fn looks_like_secret(text: &str) -> bool {
    has_url_credentials(text) || has_prefix(text, "sk-") || has_prefix(text, "bearer")
}

/// Tells whether `text` holds credentials in a URL: `://` followed by `@`.
fn has_url_credentials(text: &str) -> bool {
    text.split_once("://").is_some_and(|(_, rest)| rest.contains('@'))
}

/// Tells whether `text` starts with `prefix`, ignoring the case of ASCII letters. Never panics on a multi-byte text.
fn has_prefix(text: &str, prefix: &str) -> bool {
    text.get(..prefix.len())
        .is_some_and(|head| head.eq_ignore_ascii_case(prefix))
}

#[cfg(test)]
mod tests {
    use super::looks_like_secret;

    // Scenario: les identifiants dans une URL ressemblent à un secret
    #[test]
    fn credentials_in_url_are_secret_like() {
        for text in ["https://user:pw@host/v1", "ftp://user@host/path"] {
            assert!(
                looks_like_secret(text),
                "credentials in a URL must look like a secret: {text}"
            );
        }
    }

    // Scenario: les préfixes de jeton ressemblent à un secret, quelle que soit la casse
    #[test]
    fn token_prefixes_are_secret_like_in_any_case() {
        for text in ["sk-abc123", "SK-abc123", "Bearer abc", "bearer-abc"] {
            assert!(
                looks_like_secret(text),
                "a token prefix must look like a secret, in any case: {text}"
            );
        }
    }

    // Scenario: les références ordinaires ne ressemblent pas à un secret
    #[test]
    fn ordinary_references_are_not_secret_like() {
        for text in ["vllm-strix", "prod-k8s", "https://host/v1", "desk-lamp"] {
            assert!(
                !looks_like_secret(text),
                "an ordinary reference must not look like a secret: {text}"
            );
        }
    }

    // Scenario: un texte multi-octets ne provoque pas de panique
    #[test]
    fn multibyte_text_does_not_panic() {
        for text in ["\u{e9}\u{e9}", "\u{e9}"] {
            assert!(
                !looks_like_secret(text),
                "a multi-byte text must not look like a secret: {text}"
            );
        }
    }
}
