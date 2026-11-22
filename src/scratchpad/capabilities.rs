//! Registry of raw capabilities: the Rhai modules of `vynil-core` that the Cargo features of this crate
//! compile, plus the ones the host adds (its own modules, MCP modules, the Prometheus and Loki clients).
//! Raw capabilities are dependencies of toolsets only: they are never presented to the productive LLM.

use std::collections::BTreeMap;
use std::fmt;

use rhai::{Module, Shared};

/// Registry of raw capabilities: a name to a shared Rhai module. Built by [`Capabilities::compiled`],
/// extended by the `with_*` methods. The `Debug` output shows only the module names, never their content.
#[derive(Clone)]
#[non_exhaustive]
pub struct Capabilities {
    modules: BTreeMap<String, Shared<Module>>,
}

impl Capabilities {
    /// Pre-filled register: the `host` module (always), plus one module under each compiled feature of
    /// `lymaille` that owns its module (`password`, `crypto`, `http`, `oci`, `feeds`). A feature that is
    /// not compiled adds neither a module nor a name. The `fs` and `shell` features enrich `host` itself,
    /// they add no name. `prometheus` and `loki` enter only through [`Capabilities::with_prometheus`] and
    /// [`Capabilities::with_loki`], because their modules need a host-configured client.
    #[must_use]
    pub fn compiled() -> Self {
        let mut registry = Self {
            modules: BTreeMap::new(),
        };
        // `get_env` and the `log_*` functions need no feature; the `fs` and `shell` features enrich this
        // very module, they add no name of their own.
        registry.insert("host", vynil_core::engine::host_module());
        #[cfg(feature = "password")]
        registry.insert("password", vynil_core::password::password_module());
        #[cfg(feature = "crypto")]
        registry.insert("crypto", vynil_core::hashes::crypto_module());
        #[cfg(feature = "http")]
        registry.insert("http", vynil_core::http::http_module());
        #[cfg(feature = "oci")]
        registry.insert("oci", vynil_core::oci::oci_module());
        #[cfg(feature = "feeds")]
        registry.insert("feeds", vynil_core::feeds::feeds_module());
        registry
    }

    /// Adds a host module (its own modules, already generated MCP modules) under `name`, a valid Rhai
    /// identifier. Returns nothing when the name is invalid or already taken, including by a module of a
    /// compiled feature.
    #[must_use]
    pub fn with_module(mut self, name: &str, module: Shared<Module>) -> Option<Self> {
        if !is_module_name(name) || self.modules.contains_key(name) {
            return None;
        }
        let _replaced = self.modules.insert(name.to_owned(), module);
        Some(self)
    }

    /// Registers the `prometheus` module over `client`, a [`vynil_core::query_client::QueryClient`] the
    /// host has built and configured. One instance per type: a second call replaces the first, without error.
    /// It also replaces a module named `prometheus` that [`Capabilities::with_module`] added before this call,
    /// without error. The order of the calls matters: a `with_module("prometheus", ..)` made after this call is
    /// refused, since the name is then taken.
    #[cfg(feature = "prometheus")]
    #[must_use]
    pub fn with_prometheus(mut self, client: vynil_core::query_client::QueryClient) -> Self {
        let module = vynil_core::prometheus::prometheus_module(client);
        self.insert("prometheus", module);
        self
    }

    /// Registers the `loki` module over `client`, a [`vynil_core::query_client::QueryClient`] the host
    /// has built and configured. One instance per type: a second call replaces the first, without error.
    /// It also replaces a module named `loki` that [`Capabilities::with_module`] added before this call,
    /// without error. The order of the calls matters: a `with_module("loki", ..)` made after this call is
    /// refused, since the name is then taken.
    #[cfg(feature = "loki")]
    #[must_use]
    pub fn with_loki(mut self, client: vynil_core::query_client::QueryClient) -> Self {
        let module = vynil_core::loki::loki_module(client);
        self.insert("loki", module);
        self
    }

    /// The modules, in alphabetical order of their name.
    pub fn modules(&self) -> impl Iterator<Item = (&str, &Shared<Module>)> {
        self.modules.iter().map(|(name, module)| (name.as_str(), module))
    }

    /// The module of a name, when it exists.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&Shared<Module>> {
        self.modules.get(name)
    }

    /// Registers a module of the build under its fixed name. A feature's own `#[cfg]` guards every call,
    /// so the name never appears twice here.
    fn insert(&mut self, name: &str, module: Module) {
        let _previous = self.modules.insert(name.to_owned(), Shared::new(module));
    }
}

/// Whether `name` is a valid name of a module: ASCII letters, digits and `_`, starting with a letter or
/// `_`, and not a reserved word or symbol of Rhai (the same rule as `PresentedModule::new`).
fn is_module_name(name: &str) -> bool {
    let starts_well = name
        .chars()
        .next()
        .is_some_and(|first| first.is_ascii_alphabetic() || first == '_');
    starts_well
        && name
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || character == '_')
        && rhai::is_valid_function_name(name)
}

impl fmt::Debug for Capabilities {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_map()
            .entries(self.modules.keys().map(|name| (name, &())))
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::Capabilities;
    use rhai::{Module, Shared};

    /// Whether the module registers a function of that name, checked on the generated signatures
    /// without ever calling it.
    #[cfg(all(feature = "fs", feature = "shell"))]
    fn module_has_function(module: &Shared<Module>, name: &str) -> bool {
        let prefix = format!("{name}(");
        module
            .gen_fn_signatures_with_mapper(Into::into)
            .any(|signature| signature.starts_with(&prefix))
    }

    /// A host-style [`vynil_core::query_client::QueryClient`]: URL and bearer token, nothing read from
    /// the process, no connection (the client only parses its configuration).
    #[cfg(any(feature = "prometheus", feature = "loki"))]
    fn query_client(base_url: &str, token: &str) -> vynil_core::query_client::QueryClient {
        use vynil_core::query_client::{QueryAuth, QueryClient, QueryClientConfig};
        let config = QueryClientConfig {
            base_url: base_url.to_owned(),
            auth: QueryAuth::Bearer(token.to_owned()),
            ..Default::default()
        };
        QueryClient::new(config).expect("a valid host configuration")
    }

    // Scenario : sans feature optionnelle, seul `host` est présent
    #[cfg(not(any(
        feature = "fs",
        feature = "shell",
        feature = "password",
        feature = "crypto",
        feature = "http",
        feature = "oci",
        feature = "feeds",
        feature = "prometheus",
        feature = "loki"
    )))]
    #[test]
    fn only_host_present_without_optional_features() {
        let registry = Capabilities::compiled();
        let names: Vec<&str> = registry.modules().map(|(name, _)| name).collect();
        assert_eq!(names, ["host"], "host is the only name");
        let mut engine = rhai::Engine::new();
        let host = registry.get("host").expect("host is always there");
        let _registered = engine.register_static_module("host", Shared::clone(host));
        let value: String = engine
            .eval(r#"host::get_env("LYMAILLE_ABSENT")"#)
            .expect("get_env evaluates");
        assert_eq!(value, "", "an absent variable renders the empty string");
    }

    // Scenario : une feature compilée ajoute son module (positif sous la feature)
    #[cfg(feature = "password")]
    #[test]
    fn compiled_feature_adds_its_module() {
        let registry = Capabilities::compiled();
        assert!(
            registry.get("password").is_some(),
            "the compiled `password` feature adds its module"
        );
    }

    // Scenario : une feature compilée ajoute son module (négatif : absent sans la feature)
    #[cfg(not(feature = "password"))]
    #[test]
    fn uncompiled_feature_module_is_absent() {
        let registry = Capabilities::compiled();
        assert!(
            registry.get("password").is_none(),
            "without the feature, no `password` name"
        );
    }

    // Scenario : `fs` et `shell` complètent `host` (les fonctions existent, sans les appeler)
    #[cfg(all(feature = "fs", feature = "shell"))]
    #[test]
    fn fs_and_shell_complete_host() {
        let registry = Capabilities::compiled();
        let names: Vec<&str> = registry.modules().map(|(name, _)| name).collect();
        assert!(!names.contains(&"fs"), "`fs` adds no name of its own");
        assert!(!names.contains(&"shell"), "`shell` adds no name of its own");
        let host = registry.get("host").expect("host is always there");
        assert!(
            module_has_function(host, "file_read"),
            "with `fs`, the host module has file_read"
        );
        assert!(
            module_has_function(host, "shell_run"),
            "with `shell`, the host module has shell_run"
        );
    }

    // Scenario : `fs` et `shell` complètent `host` (sans les features, fonctions introuvables)
    #[cfg(not(any(feature = "fs", feature = "shell")))]
    #[test]
    fn fs_and_shell_functions_missing_without_features() {
        let registry = Capabilities::compiled();
        let host = registry.get("host").expect("host is always there");
        let signatures: Vec<String> = host.gen_fn_signatures_with_mapper(Into::into).collect();
        for name in ["file_read", "shell_run"] {
            let prefix = format!("{name}(");
            assert!(
                !signatures.iter().any(|s| s.starts_with(&prefix)),
                "`{name}` is a function-not-found error without its feature"
            );
        }
    }

    // Scenario : un module de l'hôte s'ajoute, un nom en double est refusé
    #[test]
    fn host_module_added_and_duplicate_names_refused() {
        let grafana = Shared::new(Module::new());
        let registry = Capabilities::compiled()
            .with_module("grafana", grafana)
            .expect("the first addition succeeds");
        assert!(
            registry
                .clone()
                .with_module("grafana", Shared::new(Module::new()))
                .is_none(),
            "the same name twice is refused"
        );
        assert!(
            registry
                .clone()
                .with_module("host", Shared::new(Module::new()))
                .is_none(),
            "`host` is already taken by the compiled build"
        );
        assert!(registry.get("grafana").is_some(), "the added module is kept");
        let names: Vec<&str> = registry.modules().map(|(name, _)| name).collect();
        let mut sorted = names.clone();
        sorted.sort_unstable();
        assert_eq!(names, sorted, "modules reads in alphabetical order");
        assert!(
            registry.get("k8s").is_none() && registry.get("s3").is_none(),
            "`k8s` and `s3` are not features of this crate: trivially absent"
        );
        for invalid in ["1abc", "a-b", "", "fn", "let"] {
            assert!(
                Capabilities::compiled()
                    .with_module(invalid, Shared::new(Module::new()))
                    .is_none(),
                "the invalid name `{invalid}` is refused, as PresentedModule::new does"
            );
        }
    }

    // Accepts rule of capabilities.sdd: `with_module` accepts a name that is a valid Rhai identifier,
    // same rules as `PresentedModule::new`: a first character that is a letter or `_`, then
    // alphanumeric characters or `_` (rule restated in presentation.sdd, task #5).
    #[test]
    fn underscore_module_names_accepted() {
        for name in ["grafana_prod", "_priv"] {
            assert!(
                Capabilities::compiled()
                    .with_module(name, Shared::new(Module::new()))
                    .is_some(),
                "`{name}` starts with a letter or `_` and is a valid Rhai identifier: accepted"
            );
        }
    }

    // Same rule, on the refusal side: a name that starts with a digit or contains `-` is not a
    // valid Rhai identifier, so `with_module` returns `None`.
    #[test]
    fn digit_start_and_hyphen_module_names_refused() {
        for name in ["1grafana", "grafana-prod"] {
            assert!(
                Capabilities::compiled()
                    .with_module(name, Shared::new(Module::new()))
                    .is_none(),
                "`{name}` is not a valid Rhai identifier: refused"
            );
        }
    }

    // Scenario : Prometheus et Loki, une instance par type (le second client remplace le premier)
    #[cfg(feature = "prometheus")]
    #[test]
    fn prometheus_one_instance_replaced_by_the_last_client() {
        let registry = Capabilities::compiled()
            .with_prometheus(query_client("https://prometheus-first.example", "token-first"));
        assert_eq!(
            registry
                .modules()
                .filter(|(name, _)| *name == "prometheus")
                .count(),
            1,
            "one prometheus module after the first call"
        );
        let before = registry.clone();
        let after =
            registry.with_prometheus(query_client("https://prometheus-second.example", "token-second"));
        assert_eq!(
            after.modules().filter(|(name, _)| *name == "prometheus").count(),
            1,
            "still one after the second call, which does not error"
        );
        assert_ne!(
            Shared::as_ptr(before.get("prometheus").expect("prometheus before")),
            Shared::as_ptr(after.get("prometheus").expect("prometheus after")),
            "the kept module is the one of the second client"
        );
    }

    // Scenario : Prometheus et Loki, une instance par type (côté Loki)
    #[cfg(feature = "loki")]
    #[test]
    fn loki_one_instance_replaced_by_the_last_client() {
        let registry =
            Capabilities::compiled().with_loki(query_client("https://loki-first.example", "token-first"));
        assert_eq!(
            registry.modules().filter(|(name, _)| *name == "loki").count(),
            1,
            "one loki module after the first call"
        );
        let before = registry.clone();
        let after = registry.with_loki(query_client("https://loki-second.example", "token-second"));
        assert_eq!(
            after.modules().filter(|(name, _)| *name == "loki").count(),
            1,
            "still one after the second call, which does not error"
        );
        assert_ne!(
            Shared::as_ptr(before.get("loki").expect("loki before")),
            Shared::as_ptr(after.get("loki").expect("loki after")),
            "the kept module is the one of the second client"
        );
    }

    // Scenario : Prometheus et Loki, une instance par type (un par type : les deux coexistent)
    #[cfg(all(feature = "prometheus", feature = "loki"))]
    #[test]
    fn prometheus_and_loki_coexist() {
        let registry = Capabilities::compiled()
            .with_prometheus(query_client("https://prometheus.example", "token-p"))
            .with_loki(query_client("https://loki.example", "token-l"));
        assert!(registry.get("prometheus").is_some(), "prometheus module");
        assert!(registry.get("loki").is_some(), "loki module");
    }

    // Scenario : le Debug ne montre que les noms
    #[test]
    fn debug_shows_only_module_names() {
        let mut module = Module::new();
        let _hash = rhai::FuncRegistration::new("secret_function")
            .set_into_module(&mut module, || "value".to_owned());
        let registry = Capabilities::compiled()
            .with_module("grafana", Shared::new(module))
            .expect("a fresh name");
        let shown = format!("{registry:?}");
        assert!(
            shown.contains("grafana") && shown.contains("host"),
            "the names appear: {shown}"
        );
        assert!(
            !shown.contains("secret_function"),
            "the module content stays hidden: {shown}"
        );
    }

    // Scenario : le Debug ne montre que les noms (client Prometheus avec URL et jeton)
    #[cfg(feature = "prometheus")]
    #[test]
    fn debug_hides_the_prometheus_client_url_and_token() {
        let registry = Capabilities::compiled().with_prometheus(query_client(
            "https://prom.internal.example",
            "super-secret-token",
        ));
        let shown = format!("{registry:?}");
        assert!(shown.contains("prometheus"), "the name appears: {shown}");
        assert!(
            !shown.contains("super-secret-token") && !shown.contains("prom.internal.example"),
            "neither the token nor the URL leaks: {shown}"
        );
    }

    // Must: the registry is Clone, Debug (names only) and Send + Sync
    #[test]
    fn registry_is_clone_and_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<Capabilities>();
        let registry = Capabilities::compiled();
        let clone = registry.clone();
        let names: Vec<&str> = registry.modules().map(|(name, _)| name).collect();
        let clone_names: Vec<&str> = clone.modules().map(|(name, _)| name).collect();
        assert_eq!(names, clone_names, "the clone holds the same modules");
    }

    // Task #2: no I/O at construction, observed under each shipped feature (the harness
    // `disallowed-*` already enforces it; here the test only constructs, and calls no
    // module function that would open a connection, a file or a process).
    #[cfg(feature = "fs")]
    #[test]
    fn constructing_with_fs_performs_no_io() {
        let registry = Capabilities::compiled();
        assert!(registry.get("host").is_some(), "host built with its fs functions");
    }

    // Task #2: construction under the `shell` feature, with no process started.
    #[cfg(feature = "shell")]
    #[test]
    fn constructing_with_shell_performs_no_io() {
        let registry = Capabilities::compiled();
        assert!(
            registry.get("host").is_some(),
            "host built with its shell functions"
        );
    }

    // Task #2: construction under the `password` feature.
    #[cfg(feature = "password")]
    #[test]
    fn constructing_with_password_performs_no_io() {
        let registry = Capabilities::compiled();
        assert!(registry.get("password").is_some(), "password module built");
    }

    // Task #2: construction under the `quantity` feature (a module of the `core` group, never of the registry).
    #[cfg(feature = "quantity")]
    #[test]
    fn constructing_with_quantity_performs_no_io() {
        let registry = Capabilities::compiled();
        assert!(
            registry.get("quantity").is_none(),
            "quantity lives in the core group, not in the registry"
        );
    }

    // Task #2: construction under the `crypto` feature.
    #[cfg(feature = "crypto")]
    #[test]
    fn constructing_with_crypto_performs_no_io() {
        let registry = Capabilities::compiled();
        assert!(registry.get("crypto").is_some(), "crypto module built");
    }

    // Task #2: construction under the `http` feature, with no connection opened (the module does nothing
    // until a script calls it).
    #[cfg(feature = "http")]
    #[test]
    fn constructing_with_http_performs_no_io() {
        let registry = Capabilities::compiled();
        assert!(registry.get("http").is_some(), "http module built, idle");
    }

    // Task #2: construction under the `oci` feature, with no registry contacted.
    #[cfg(feature = "oci")]
    #[test]
    fn constructing_with_oci_performs_no_io() {
        let registry = Capabilities::compiled();
        assert!(registry.get("oci").is_some(), "oci module built, idle");
    }

    // Task #2: construction under the `feeds` feature, with no feed fetched.
    #[cfg(feature = "feeds")]
    #[test]
    fn constructing_with_feeds_performs_no_io() {
        let registry = Capabilities::compiled();
        assert!(registry.get("feeds").is_some(), "feeds module built, idle");
    }

    // Task #2: construction under the `prometheus` feature: `QueryClient::new` only validates
    // the host configuration, with no connection.
    #[cfg(feature = "prometheus")]
    #[test]
    fn constructing_with_prometheus_performs_no_io() {
        let registry =
            Capabilities::compiled().with_prometheus(query_client("https://prom.example", "token"));
        assert!(
            registry.get("prometheus").is_some(),
            "prometheus module built, idle"
        );
    }

    // Task #2: construction under the `loki` feature, same contract as Prometheus.
    #[cfg(feature = "loki")]
    #[test]
    fn constructing_with_loki_performs_no_io() {
        let registry = Capabilities::compiled().with_loki(query_client("https://loki.example", "token"));
        assert!(registry.get("loki").is_some(), "loki module built, idle");
    }
}
