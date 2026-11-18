//! Authentication headers of an MCP host: the set of headers the host resolves and the trait that renders them, compiled
//! with or without the `mcp-http` feature (`agent-sessions/modules#FR-13`, `agent-sessions#CON-3`, `agent-sessions#CON-5`).
//! Neither `rmcp` nor `http` appears here: converting the headers to transport types belongs to the real connector
//! (`mcp_client`).

use std::collections::BTreeMap;
use std::fmt;

// The `rustls` dev-dependency is referenced only by the `mcp-http` tests of the real connector (`mcp_client`): keep it
// named while those tests are built out, since `unused_crate_dependencies` (denied by the harness) would otherwise
// reject the crate as soon as the tests build without the `mcp-http` feature (same pattern as `ts_rs` in the crate root).
#[cfg(all(test, not(feature = "mcp-http")))]
use rustls as _;

use super::mcp::McpError;

/// Authentication headers of a server, as the host resolves them. The values are opaque and never shown.
#[derive(Clone, Default)]
pub struct McpHeaders {
    bearer_token: Option<String>,
    extra: BTreeMap<String, String>,
}

impl McpHeaders {
    /// Builds an empty set of headers.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a bearer token, given without the `Bearer` prefix.
    #[must_use]
    pub fn with_bearer(mut self, token: &str) -> Self {
        self.bearer_token = Some(token.to_owned());
        self
    }

    /// Adds a header with its name and its value.
    #[must_use]
    pub fn with_header(mut self, name: &str, value: &str) -> Self {
        let _previous = self.extra.insert(name.to_owned(), value.to_owned());
        self
    }

    /// Returns the bearer token set by [`McpHeaders::with_bearer`], without the `Bearer` prefix, or `None` when none was
    /// set. This is the only public read of a value: it lets a host check what its own [`McpAuth`] renders.
    #[must_use]
    pub fn bearer(&self) -> Option<&str> {
        self.bearer_token.as_deref()
    }

    /// Yields the custom headers (name, value) in name order, for the conversion to transport types made by the real
    /// connector. Not part of the public API.
    #[cfg_attr(
        all(not(test), not(feature = "mcp-http")),
        expect(
            dead_code,
            reason = "its only reader is the transport conversion of `mcp_client`, compiled only with `mcp-http`; the tests read it directly"
        )
    )]
    pub(in crate::modules) fn custom(&self) -> impl Iterator<Item = (&str, &str)> {
        self.extra
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_str()))
    }
}

impl fmt::Debug for McpHeaders {
    // Only the names and the presence of a token: a value never shows (`agent-sessions#CON-3`).
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("McpHeaders")
            .field("bearer", &self.bearer_token.is_some())
            .field("headers", &self.extra.keys().collect::<Vec<_>>())
            .finish()
    }
}

/// Source of the authentication headers, implemented by the host. It is called at each connection and at each
/// reconnection, so that a renewed token is taken into account.
pub trait McpAuth: Send + Sync {
    /// Returns the current headers.
    ///
    /// # Errors
    ///
    /// Returns the [`McpError`] of the host when it cannot resolve the headers; the connector gives it back as is.
    fn headers(&self) -> impl Future<Output = Result<McpHeaders, McpError>> + Send;
}

#[cfg(test)]
mod tests {
    use super::{McpAuth, McpError, McpHeaders};

    fn assert_send_sync<T: Send + Sync>() {}

    // Scenario: le jeton posé se relit
    #[test]
    fn bearer_reads_back_the_token_set_by_with_bearer() {
        let with_token = McpHeaders::new().with_bearer("tok-123");
        assert_eq!(
            with_token.bearer(),
            Some("tok-123"),
            "the token set by with_bearer reads back without the Bearer prefix"
        );
        let without_token = McpHeaders::new();
        assert_eq!(
            without_token.bearer(),
            None,
            "headers built without a token read none"
        );
    }

    // Contrat Returns (`mcp_auth.sdd`): custom rend les en-têtes propres (nom, valeur) dans l'ordre des noms.
    #[test]
    fn custom_reads_own_headers_in_name_order() {
        let headers = McpHeaders::new()
            .with_header("x-tenant", "acme")
            .with_header("beta", "two")
            .with_header("alpha", "one");
        let read: Vec<(&str, &str)> = headers.custom().collect();
        assert_eq!(
            read,
            vec![("alpha", "one"), ("beta", "two"), ("x-tenant", "acme")],
            "the custom headers are read as (name, value) in the order of the names"
        );
    }

    // Scenario: les en-têtes ne se montrent pas
    #[test]
    fn headers_debug_shows_names_and_never_values() {
        let headers = McpHeaders::new()
            .with_bearer("tok-secret")
            .with_header("x-api-key", "key-secret");
        let text = format!("{headers:?}");
        assert!(text.contains("x-api-key"), "the header name is shown: {text}");
        assert!(
            text.contains("bearer"),
            "the presence of a token is shown: {text}"
        );
        assert!(!text.contains("tok-secret"), "the token is never shown: {text}");
        assert!(!text.contains("key-secret"), "a value is never shown: {text}");
        let bare = format!("{:?}", McpHeaders::new());
        assert!(!bare.contains("true"), "no token, none announced: {bare}");
    }

    /// Host authentication that always renders the same token.
    struct TokenAuth(&'static str);

    impl McpAuth for TokenAuth {
        fn headers(&self) -> impl Future<Output = Result<McpHeaders, McpError>> + Send {
            std::future::ready(Ok(McpHeaders::new().with_bearer(self.0)))
        }
    }

    // Scenario: une authentification d'hôte s'écrit sans le connecteur réel
    #[tokio::test]
    async fn host_auth_renders_a_token_without_the_real_connector() {
        let auth = TokenAuth("tok-123");
        let headers = auth.headers().await.expect("the host resolves its headers");
        assert_eq!(
            headers.bearer(),
            Some("tok-123"),
            "the headers rendered carry the token, readable by bearer"
        );
    }

    // Must (`mcp_auth.sdd`): @McpAuth est `Send + Sync`; les en-têtes aussi (assertion déplacée depuis mcp_client.rs).
    #[test]
    fn auth_and_headers_are_send_and_sync() {
        fn assert_auth<T: McpAuth>() {
            assert_send_sync::<T>();
        }
        assert_send_sync::<McpHeaders>();
        assert_auth::<TokenAuth>();
    }
}
