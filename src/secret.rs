//! Heuristic that tells whether a text looks like a secret, shared by the context and provider checks, so that the two
//! can never diverge (`agent-sessions/toolsets#FR-3`, `agent-sessions#CON-3`). The real control stays with the host.

/// Decides whether a text looks like a secret.
///
/// The host can replace the library heuristic with its own detector: a detector given by the host replaces
/// [`DefaultSecretDetector`] entirely, never adds to it. An implementation does no I/O and does not panic.
///
/// The default heuristic ([`DefaultSecretDetector`]) flags credentials in a URL (`://` followed by `@`), `sk-` at the
/// start of the text, and anywhere on a word boundary the prefix `sk-` or `Bearer`, in any ASCII case, followed by a
/// plausible token of at least 16 characters.
pub trait SecretDetector: Send + Sync {
    /// Returns true when `text` looks like a secret.
    fn looks_like_secret(&self, text: &str) -> bool;
}

/// Default detector: the heuristic of the library, built with [`Default`].
///
/// A text looks like a secret when it holds credentials in a URL (`://` followed by `@`), or starts with `sk-`, or
/// carries the prefix `sk-` or `Bearer`, in any ASCII case, on a word boundary followed by a plausible token: at least
/// 16 characters of `A-Z a-z 0-9 . _ ~ + / = -`, right after the prefix or after its spaces.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct DefaultSecretDetector;

impl SecretDetector for DefaultSecretDetector {
    fn looks_like_secret(&self, text: &str) -> bool {
        looks_like_secret(text)
    }
}

/// Tells whether `text` looks like a secret:
///
/// - credentials in a URL: `://` followed by `@` somewhere after it;
/// - `sk-` at the very start of the text, whatever follows it;
/// - anywhere in the text, on a word boundary (the start, or a preceding character that is not alphanumeric), the
///   prefix `sk-` or `Bearer`, ignoring ASCII case, followed by a plausible token: at least 16 characters of
///   `A-Z a-z 0-9 . _ ~ + / = -`, right after the prefix or after its spaces.
///
/// Never panics: the boundary and the prefixes are read character by character, and a prefix that does not land on a
/// character boundary never matches.
#[expect(
    clippy::pub_with_shorthand,
    reason = "rustfmt rewrites pub(in crate) into pub(crate), which this lint refuses: both are the crate visibility that unreachable_pub requires"
)]
pub(crate) fn looks_like_secret(text: &str) -> bool {
    has_url_credentials(text)
        || starts_with_sk(text)
        || has_prefixed_token(text, "sk-")
        || has_prefixed_token(text, "bearer")
}

/// Tells whether `text` holds credentials in a URL: `://` followed by `@`.
fn has_url_credentials(text: &str) -> bool {
    text.split_once("://").is_some_and(|(_, rest)| rest.contains('@'))
}

/// Tells whether `text` starts with `sk-`, ignoring the case of ASCII letters: at the start of the text no minimum
/// token length is required (the original rule, kept at that one position). Never panics on a multi-byte text.
fn starts_with_sk(text: &str) -> bool {
    text.get(..3).is_some_and(|head| head.eq_ignore_ascii_case("sk-"))
}

/// Shortest token that still counts as plausible after a prefix.
const MIN_TOKEN_LEN: usize = 16;

/// Tells whether `text` carries `prefix`, ignoring the case of ASCII letters, on a word boundary at any position,
/// followed by a plausible token. Never panics on a multi-byte text: every slice goes through `get`, which matches
/// only on character boundaries.
fn has_prefixed_token(text: &str, prefix: &str) -> bool {
    text.char_indices().any(|(index, _)| {
        let Some(end) = index.checked_add(prefix.len()) else {
            return false;
        };
        text.get(end..).is_some_and(|rest| {
            text.get(index..end)
                .is_some_and(|head| head.eq_ignore_ascii_case(prefix))
                && on_word_boundary(text, index)
                && has_plausible_token(rest)
        })
    })
}

/// Tells whether `index`, a character boundary of `text`, is the start of a word: the start of the text, or a
/// preceding character that is not alphanumeric.
fn on_word_boundary(text: &str, index: usize) -> bool {
    index == 0
        || text
            .get(..index)
            .and_then(|before| before.chars().next_back())
            .is_none_or(|previous| !previous.is_alphanumeric())
}

/// Tells whether `rest` starts with a plausible token: at least [`MIN_TOKEN_LEN`] characters of the token alphabet
/// right away, or after the prefix's spaces.
fn has_plausible_token(rest: &str) -> bool {
    rest.trim_start_matches(' ')
        .chars()
        .take_while(|c| is_token_char(*c))
        .count()
        >= MIN_TOKEN_LEN
}

/// Tells whether `c` belongs to the token alphabet `A-Z a-z 0-9 . _ ~ + / = -`.
const fn is_token_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '~' | '+' | '/' | '=' | '-')
}

#[cfg(test)]
mod tests {
    use super::{DefaultSecretDetector, SecretDetector, looks_like_secret};

    /// A host detector that only knows the prefix `tok_`.
    struct TokDetector;

    impl SecretDetector for TokDetector {
        fn looks_like_secret(&self, text: &str) -> bool {
            text.starts_with("tok_")
        }
    }

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

    // Scenario: les préfixes de jeton au début du texte ressemblent à un secret, quelle que soit la casse
    #[test]
    fn token_prefixes_at_the_start_are_secret_like_in_any_case() {
        for text in ["sk-abc123", "SK-abc123"] {
            assert!(
                looks_like_secret(text),
                "an sk- prefix at the start must look like a secret, in any case: {text}"
            );
        }
    }

    // Scenario: un jeton plausible se reconnaît à toute position sur une frontière de mot
    // (the two French Givens are written with escaped é: clippy::non_ascii_literal forbids the raw form)
    #[test]
    fn plausible_tokens_are_secret_like_at_any_word_boundary() {
        for text in [
            "la cl\u{e} sk-abcdefghijklmnop0 est dans le fichier",
            "Authorization: Bearer abcdefghijklmnop0123",
            "x=SK-abcdefghijklmnop0;y",
        ] {
            assert!(
                looks_like_secret(text),
                "a plausible token on a word boundary must look like a secret, at any position: {text}"
            );
        }
    }

    // Scenario: la prose et les jetons tronqués ne sont pas des secrets
    #[test]
    fn prose_and_truncated_tokens_are_not_secret_like() {
        for text in [
            "Bearer abc",
            "bearer-abc",
            "Bearer tokens are sent in a header",
            "cl\u{e} = sk-abc",
        ] {
            assert!(
                !looks_like_secret(text),
                "prose and truncated tokens must not look like a secret: {text}"
            );
        }
    }

    // Must (contrat du jeton plausible) : la borne est « au moins 16 caractères », bornée ici exactement : le
    // Scenario « un jeton plausible se reconnaît à toute position sur une frontière de mot » couvre le reste.
    #[test]
    fn a_token_of_exactly_sixteen_characters_marks_the_plausible_boundary() {
        // 15 characters: one short of the contract, so the run of token characters is truncated.
        assert!(
            !looks_like_secret("x sk-abcdefghijklmno"),
            "a 15-character token after a boundary prefix must not look like a secret"
        );
        // 16 characters: exactly the contract minimum, at the same position.
        assert!(
            looks_like_secret("x sk-abcdefghijklmnop"),
            "a 16-character token after a boundary prefix must look like a secret"
        );
    }

    // Scenario: les références ordinaires ne ressemblent pas à un secret
    #[test]
    fn ordinary_references_are_not_secret_like() {
        for text in [
            "vllm-strix",
            "prod-k8s",
            "https://host/v1",
            "desk-lamp",
            "disk-usage",
            "risk-free",
            "task-list",
        ] {
            assert!(
                !looks_like_secret(text),
                "an ordinary reference must not look like a secret: {text}"
            );
        }
        // Must not (« voir un préfixe à l'intérieur d'un mot ») : le jeton est plausible, seul le garde de
        // frontière écarte ce préfixe collé à une lettre — le mutant qui l'inverse doit mourir ici.
        assert!(
            !looks_like_secret("xsk-abcdefghijklmnop0"),
            "a prefix glued inside a word must not look like a secret, even with a plausible token"
        );
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

    // Scenario: un détecteur de l'hôte remplace le défaut
    #[test]
    fn host_detector_replaces_the_default() {
        let host = TokDetector;
        let default = DefaultSecretDetector;
        assert!(!host.looks_like_secret("sk-abc"), "the host detector ignores sk-");
        assert!(host.looks_like_secret("tok_abc"), "the host detector knows tok_");
        assert!(default.looks_like_secret("sk-abc"), "the default knows sk-");
        assert!(!default.looks_like_secret("tok_abc"), "the default ignores tok_");
    }
}
