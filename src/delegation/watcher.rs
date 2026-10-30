//! Watchers: persisted subscriptions to an event source, whose filter is a Rhai predicate evaluated in an isolated engine,
//! and whose single action is a notification for the next turn or a request to launch a turn
//! (`agent-sessions/delegation#FR-8`, `AC-4`, `AC-9`).
//!
//! Nothing here performs I/O. A predicate is evaluated on a fresh engine built from the pure base of `vynil-core` alone,
//! with the event as its only input: a predicate that fails triggers nothing and is reported by name.

use std::sync::atomic::{AtomicU64, Ordering};

use rhai::{Dynamic, Map, Scope};
use serde::Deserialize;
use serde_json::json;

use super::error::{DelegationError, DelegationResult};
use super::sources::WatchEvent;
use crate::scratchpad::{ScratchpadLimits, base_engine};

/// Maximum number of operations of one predicate evaluation. The DSD leaves the value open (`watcher.sdd`, `#2`).
const PREDICATE_MAX_OPERATIONS: u64 = 10_000;

/// Name of the constant that carries the event in a predicate's scope.
const EVENT_CONSTANT: &str = "event";

/// Engine a predicate is evaluated on: the pure base of the scratchpad, plus the pure group of `vynil-core` and the
/// operation ceiling. No group, module or import is granted. Built fresh for each use, so that no evaluation sees
/// another one.
///
/// The ceiling counts the progress callbacks of Rhai, not `set_max_operations`: a closure called by a native function
/// (`map`, for instance) starts its own count, so the native ceiling does not bound the work of such a predicate.
fn predicate_engine() -> rhai::Engine {
    let mut engine = base_engine(&ScratchpadLimits::new());
    vynil_core::engine::core_pure_rhai_register(&mut engine);
    // The base engine writes `print` and `debug` into the output buffer of the run open on this thread: a predicate must not.
    let _print = engine.on_print(|_| {});
    let _debug = engine.on_debug(|_, _, _| {});
    let operations = AtomicU64::new(0);
    let _progress = engine.on_progress(move |_count| {
        if operations.fetch_add(1, Ordering::Relaxed) < PREDICATE_MAX_OPERATIONS {
            None
        } else {
            Some(Dynamic::UNIT)
        }
    });
    engine
}

/// Evaluates `predicate` on `event`. `Some(true)` or `Some(false)` when it yields a boolean; `None` when it fails to
/// compile, fails at run time, yields another type, or reaches the operation ceiling.
fn predicate_holds(predicate: &str, event: &WatchEvent) -> Option<bool> {
    let engine = predicate_engine();
    let ast = engine.compile_expression(predicate).ok()?;
    let mut scope = Scope::new();
    let _event = scope.push_constant(EVENT_CONSTANT, event_map(event)?);
    engine
        .eval_ast_with_scope::<Dynamic>(&mut scope, &ast)
        .ok()?
        .as_bool()
        .ok()
}

/// The event as the predicate sees it: a map with `source`, `name`, `data` and `at` (milliseconds).
fn event_map(event: &WatchEvent) -> Option<Map> {
    let at = i64::try_from(event.at().as_millis()).ok()?;
    let data = rhai::serde::to_dynamic(event.data()).ok()?;
    let mut map = Map::new();
    let _source = map.insert("source".into(), Dynamic::from(event.source().to_owned()));
    let _name = map.insert("name".into(), Dynamic::from(event.name().to_owned()));
    let _data = map.insert("data".into(), data);
    let _at = map.insert("at".into(), Dynamic::from(at));
    Some(map)
}

/// Persisted form of an action, read back by [`WatcherSet::from_values`].
#[derive(Deserialize)]
#[serde(tag = "kind", content = "text", rename_all = "snake_case")]
enum StoredAction {
    Inject(String),
    LaunchTurn(String),
}

/// Persisted form of a watcher, read back by [`WatcherSet::from_values`].
#[derive(Deserialize)]
struct StoredWatcher {
    id: String,
    source: String,
    predicate: String,
    action: StoredAction,
}

/// What a watcher does when its predicate accepts an event.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum WatcherAction {
    /// Injects a notification with this fixed text into the next turn.
    Inject(String),
    /// Asks the host to launch a turn with this fixed message.
    LaunchTurn(String),
}

/// Persisted subscription: an identifier, the source it observes, its predicate (the text of a Rhai expression that
/// returns a boolean) and its action.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Watcher {
    id: String,
    source: String,
    predicate: String,
    action: WatcherAction,
}

impl Watcher {
    /// Builds a watcher `id` observing `source`, with the predicate `predicate` and the action `action`.
    ///
    /// # Errors
    ///
    /// Returns [`DelegationError::InvalidWatcher`] when the identifier or the source is empty, or when the predicate does
    /// not compile as an expression.
    pub fn new(id: &str, source: &str, predicate: &str, action: WatcherAction) -> DelegationResult<Self> {
        if id.is_empty() {
            return Err(DelegationError::InvalidWatcher {
                id: String::new(),
                reason: "identifier is empty",
            });
        }
        if source.is_empty() {
            return Err(DelegationError::InvalidWatcher {
                id: id.to_owned(),
                reason: "source name is empty",
            });
        }
        if predicate_engine().compile_expression(predicate).is_err() {
            return Err(DelegationError::InvalidWatcher {
                id: id.to_owned(),
                reason: "the predicate does not compile as an expression",
            });
        }
        Ok(Self {
            id: id.to_owned(),
            source: source.to_owned(),
            predicate: predicate.to_owned(),
            action,
        })
    }

    /// Identifier of the watcher.
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Name of the source the watcher observes.
    #[must_use]
    pub fn source(&self) -> &str {
        &self.source
    }

    /// Text of the predicate.
    #[must_use]
    pub fn predicate(&self) -> &str {
        &self.predicate
    }

    /// Action of the watcher.
    #[must_use]
    pub const fn action(&self) -> &WatcherAction {
        &self.action
    }
}

/// Ordered set of the watchers of a session, persisted with it (`SessionState::watchers`).
#[derive(Debug, Default, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct WatcherSet {
    watchers: Vec<Watcher>,
}

impl WatcherSet {
    /// Builds an empty set.
    #[must_use]
    pub const fn new() -> Self {
        Self { watchers: Vec::new() }
    }

    /// Adds a watcher at the end of the set.
    ///
    /// # Errors
    ///
    /// Returns [`DelegationError::InvalidWatcher`] when a watcher of the set already has the same identifier.
    pub fn add(&mut self, watcher: Watcher) -> DelegationResult<()> {
        if self.watchers.iter().any(|existing| existing.id() == watcher.id()) {
            return Err(DelegationError::InvalidWatcher {
                id: watcher.id().to_owned(),
                reason: "identifier is already in use",
            });
        }
        self.watchers.push(watcher);
        Ok(())
    }

    /// Removes the watcher `id`. Returns whether a watcher was removed.
    pub fn remove(&mut self, id: &str) -> bool {
        let before = self.watchers.len();
        self.watchers.retain(|watcher| watcher.id() != id);
        self.watchers.len() != before
    }

    /// The watchers, in insertion order.
    #[must_use]
    pub const fn watchers(&self) -> &[Watcher] {
        self.watchers.as_slice()
    }

    /// The persisted form of the set: one JSON object per watcher, in insertion order.
    #[must_use]
    pub fn to_values(&self) -> Vec<serde_json::Value> {
        self.watchers
            .iter()
            .map(|watcher| {
                let action = match watcher.action() {
                    WatcherAction::Inject(text) => json!({"kind": "inject", "text": text}),
                    WatcherAction::LaunchTurn(text) => json!({"kind": "launch_turn", "text": text}),
                };
                json!({
                    "id": watcher.id(),
                    "source": watcher.source(),
                    "predicate": watcher.predicate(),
                    "action": action,
                })
            })
            .collect()
    }

    /// Rebuilds a set from its persisted form, in the same order.
    ///
    /// # Errors
    ///
    /// Returns [`DelegationError::InvalidWatcher`] when a record is malformed, when its predicate does not compile, or
    /// when two records share an identifier.
    pub fn from_values(values: &[serde_json::Value]) -> DelegationResult<Self> {
        let mut set = Self::new();
        for value in values {
            let stored: StoredWatcher =
                serde_json::from_value(value.clone()).map_err(|_error| DelegationError::InvalidWatcher {
                    id: String::new(),
                    reason: "persisted watcher is malformed",
                })?;
            let action = match stored.action {
                StoredAction::Inject(text) => WatcherAction::Inject(text),
                StoredAction::LaunchTurn(text) => WatcherAction::LaunchTurn(text),
            };
            set.add(Watcher::new(
                &stored.id,
                &stored.source,
                &stored.predicate,
                action,
            )?)?;
        }
        Ok(set)
    }

    /// Evaluates the watchers observing the source of `event`, in insertion order, and returns what they trigger.
    #[must_use]
    pub fn evaluate(&self, event: &WatchEvent) -> Triggered {
        let mut triggered = Triggered::default();
        for watcher in self
            .watchers
            .iter()
            .filter(|watcher| watcher.source() == event.source())
        {
            match predicate_holds(watcher.predicate(), event) {
                Some(true) => match watcher.action() {
                    WatcherAction::Inject(text) => {
                        let notification = Notification::rendered(watcher, text);
                        triggered.notifications.push(notification);
                    }
                    WatcherAction::LaunchTurn(text) => {
                        let launch = (watcher.id().to_owned(), text.clone());
                        triggered.launches.push(launch);
                    }
                },
                Some(false) => {}
                None => triggered.failures.push(watcher.id().to_owned()),
            }
        }
        triggered
    }
}

/// Notification to inject into the next turn. Its text is rendered by the watcher: it names the source and the watcher,
/// then gives the declared text. The content of the event never enters it.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Notification {
    watcher: String,
    source: String,
    text: String,
}

impl Notification {
    /// Renders the notification of `watcher`, which accepted an event: the text is framed by the source and the watcher,
    /// and never takes the content of the event.
    fn rendered(watcher: &Watcher, declared: &str) -> Self {
        Self {
            watcher: watcher.id().to_owned(),
            source: watcher.source().to_owned(),
            text: format!(
                "watcher `{}` on source `{}`: {declared}",
                watcher.id(),
                watcher.source()
            ),
        }
    }

    /// Identifier of the watcher that produced the notification.
    #[must_use]
    pub fn watcher(&self) -> &str {
        &self.watcher
    }

    /// Source of the event that triggered the notification.
    #[must_use]
    pub fn source(&self) -> &str {
        &self.source
    }

    /// Text rendered to the model: the source, the watcher identifier and the declared text.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }
}

/// Outcome of an evaluation of a set of watchers.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Triggered {
    notifications: Vec<Notification>,
    launches: Vec<(String, String)>,
    failures: Vec<String>,
}

impl Triggered {
    /// Notifications to inject into the next turn, in insertion order of their watchers.
    #[must_use]
    pub fn notifications(&self) -> &[Notification] {
        &self.notifications
    }

    /// Requests to launch a turn, as `(watcher identifier, message)`, in insertion order of their watchers.
    #[must_use]
    pub fn launches(&self) -> &[(String, String)] {
        &self.launches
    }

    /// Identifiers of the watchers whose predicate failed, in insertion order.
    #[must_use]
    pub fn failures(&self) -> &[String] {
        &self.failures
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{Notification, Watcher, WatcherAction, WatcherSet};
    use crate::clock::Timestamp;
    use crate::delegation::{DelegationError, WatchEvent};

    fn event(source: &str, name: &str, data: serde_json::Value) -> WatchEvent {
        WatchEvent::new(source, name, data, Timestamp::from_millis(42))
    }

    fn inject(id: &str, source: &str, predicate: &str) -> Watcher {
        Watcher::new(id, source, predicate, WatcherAction::Inject("alerte".to_owned()))
            .expect("the watcher is valid")
    }

    fn set_of(watchers: Vec<Watcher>) -> WatcherSet {
        let mut set = WatcherSet::new();
        for watcher in watchers {
            set.add(watcher).expect("the identifiers are distinct");
        }
        set
    }

    /// A predicate of `depth` nested `map` calls over ten elements: ten to the power `depth` closure calls.
    fn nested_maps(depth: usize) -> String {
        let mut text = "1".to_owned();
        for _ in 0..depth {
            text = format!("[1,2,3,4,5,6,7,8,9,10].map(|x| {text}).len()");
        }
        text.push_str(" == 10");
        text
    }

    fn ids(notifications: &[Notification]) -> Vec<&str> {
        notifications.iter().map(Notification::watcher).collect()
    }

    // Scenario: un prédicat qui accepte injecte
    #[test]
    fn predicate_that_accepts_injects() {
        let set = set_of(vec![inject("tools", "mcp", r#"event.name == "tools_changed""#)]);

        let triggered = set.evaluate(&event("mcp", "tools_changed", json!({})));

        assert_eq!(
            triggered.notifications().len(),
            1,
            "an accepting predicate must inject exactly one notification"
        );
        assert_eq!(
            triggered.notifications()[0].watcher(),
            "tools",
            "the notification names the watcher that accepted"
        );
        assert!(
            triggered.failures().is_empty(),
            "an accepting predicate is not a failure"
        );
    }

    // Scenario: un prédicat qui refuse n'injecte rien
    #[test]
    fn predicate_that_refuses_injects_nothing() {
        let set = set_of(vec![inject("tools", "mcp", r#"event.name == "tools_changed""#)]);

        let refused = set.evaluate(&event("mcp", "other", json!({})));
        assert!(
            refused.notifications().is_empty(),
            "a refusing predicate injects nothing"
        );
        assert!(
            refused.failures().is_empty(),
            "a refusing predicate is not reported as a failure"
        );
        assert!(
            refused.launches().is_empty(),
            "a refusing predicate launches no turn"
        );

        // Control: the same watcher does fire on the matching event, so the refusal is not vacuous.
        let accepted = set.evaluate(&event("mcp", "tools_changed", json!({})));
        assert_eq!(
            accepted.notifications().len(),
            1,
            "the control event must be accepted"
        );
    }

    // Scenario: un prédicat isolé échoue sans effet de bord
    #[test]
    fn isolated_predicates_fail_without_side_effect() {
        let set = set_of(vec![
            inject("grafana", "mcp", "grafana::query()"),
            inject("scope", "mcp", "secret_var"),
            inject("function", "mcp", "f()"),
        ]);
        let before = set.to_values();

        let triggered = set.evaluate(&event("mcp", "x", json!({"secret": "s3cr3t"})));

        assert!(
            triggered.notifications().is_empty(),
            "no isolated predicate may trigger"
        );
        assert_eq!(
            triggered.failures(),
            ["grafana", "scope", "function"],
            "the three failing predicates are reported, in insertion order"
        );
        assert_eq!(
            set.to_values(),
            before,
            "an evaluation leaves the watchers unchanged"
        );
    }

    // Scenario: un prédicat non booléen échoue
    #[test]
    fn non_boolean_predicate_fails() {
        let set = set_of(vec![inject("count", "mcp", "1 + 1")]);

        let triggered = set.evaluate(&event("mcp", "x", json!({})));

        assert!(
            triggered.notifications().is_empty(),
            "a non-boolean predicate triggers nothing"
        );
        assert_eq!(
            triggered.failures(),
            ["count"],
            "a non-boolean predicate is reported as a failure"
        );
    }

    // Scenario: seuls les watchers de la source sont évalués
    #[test]
    fn only_watchers_of_the_source_are_evaluated() {
        let set = set_of(vec![
            inject("mcp-watcher", "mcp", "true"),
            inject("workspace-watcher", "workspace", "f()"),
        ]);

        let triggered = set.evaluate(&event("mcp", "x", json!({})));

        assert_eq!(
            ids(triggered.notifications()),
            ["mcp-watcher"],
            "only the watcher of the event's source fires"
        );
        assert!(
            triggered.failures().is_empty(),
            "the watcher of another source must not be evaluated, so it cannot fail"
        );
    }

    // Scenario: demande de lancement de tour
    #[test]
    fn launch_turn_request_carries_the_declared_text() {
        let launch = Watcher::new(
            "wake",
            "subagent",
            r#"event.name == "finished""#,
            WatcherAction::LaunchTurn("le sous-agent a fini".to_owned()),
        )
        .expect("the watcher is valid");
        let set = set_of(vec![launch]);

        let triggered = set.evaluate(&event("subagent", "finished", json!({})));

        assert_eq!(
            triggered.launches(),
            [("wake".to_owned(), "le sous-agent a fini".to_owned())],
            "an accepting LaunchTurn watcher requests a turn with its declared text"
        );
        assert!(
            triggered.notifications().is_empty(),
            "a LaunchTurn watcher injects no notification"
        );
    }

    // Scenario: un prédicat qui boucle est borné
    #[test]
    fn looping_predicate_is_bounded() {
        let set = set_of(vec![inject("loop", "mcp", &nested_maps(6))]);

        let triggered = set.evaluate(&event("mcp", "x", json!({})));

        assert!(
            triggered.notifications().is_empty(),
            "a predicate stopped by the ceiling triggers nothing"
        );
        assert_eq!(
            triggered.failures(),
            ["loop"],
            "a predicate stopped by the operation ceiling is reported as a failure"
        );
    }

    // Scenario: aller-retour persistant
    #[test]
    fn persisted_form_round_trips_in_order() {
        let set = set_of(vec![
            inject("first", "mcp", r#"event.name == "a""#),
            Watcher::new(
                "second",
                "timer",
                "event.data.id == \"daily\"",
                WatcherAction::LaunchTurn("bonjour".to_owned()),
            )
            .expect("the watcher is valid"),
        ]);

        let values = set.to_values();
        let restored = WatcherSet::from_values(&values).expect("the persisted form is valid");

        assert_eq!(restored, set, "the restored set equals the original one");
        assert_eq!(
            restored.watchers().iter().map(Watcher::id).collect::<Vec<_>>(),
            ["first", "second"],
            "the restored watchers keep their order"
        );
    }

    // Scenario: prédicat invalide refusé
    #[test]
    fn invalid_predicates_are_refused() {
        for predicate in ["let x = ;", "1 + 1; 2", r#"import "m" as m; true"#] {
            let result = Watcher::new("bad", "mcp", predicate, WatcherAction::Inject("x".to_owned()));
            assert!(
                matches!(result, Err(DelegationError::InvalidWatcher { .. })),
                "`{predicate}` must be refused with InvalidWatcher"
            );
        }
        let valid = Watcher::new("good", "mcp", "1 + 1", WatcherAction::Inject("x".to_owned()));
        assert!(valid.is_ok(), "control: `1 + 1` is an expression and is accepted");
    }

    // Scenario: le contenu de l'événement n'est pas dans la notification
    #[test]
    fn notification_text_carries_no_event_content() {
        let secret = "tok-live-4f2a9c";
        let set = set_of(vec![inject("alerts", "mcp", r#"event.name == "x""#)]);

        let triggered = set.evaluate(&event("mcp", "x", json!({"token": secret})));

        let notification = triggered
            .notifications()
            .first()
            .expect("the watcher accepts the event");
        assert!(
            !notification.text().contains(secret),
            "the notification `{}` must not carry the event content",
            notification.text()
        );
        assert!(
            notification.text().contains("alerts") && notification.text().contains("mcp"),
            "the notification names its watcher and its source"
        );
        assert!(
            notification.text().contains("alerte"),
            "the notification carries the declared text"
        );
    }

    #[test]
    fn empty_identifier_is_refused() {
        let result = Watcher::new("", "mcp", "true", WatcherAction::Inject("x".to_owned()));
        assert!(
            matches!(result, Err(DelegationError::InvalidWatcher { .. })),
            "an empty identifier must be refused with InvalidWatcher"
        );
    }

    #[test]
    fn empty_source_is_refused() {
        let result = Watcher::new("w", "", "true", WatcherAction::Inject("x".to_owned()));
        assert!(
            matches!(result, Err(DelegationError::InvalidWatcher { .. })),
            "an empty source must be refused with InvalidWatcher"
        );
    }

    #[test]
    fn duplicate_identifier_is_refused() {
        let mut set = set_of(vec![inject("dup", "mcp", "true")]);

        let result = set.add(inject("dup", "workspace", "true"));

        assert!(
            matches!(result, Err(DelegationError::InvalidWatcher { .. })),
            "a second watcher with the same identifier must be refused"
        );
        assert_eq!(set.watchers().len(), 1, "the refused watcher is not added");
    }

    #[test]
    fn persisted_form_with_invalid_predicate_is_refused() {
        let values = vec![json!({
            "id": "bad",
            "source": "mcp",
            "predicate": "let x = ;",
            "action": {"kind": "inject", "text": "x"}
        })];

        let result = WatcherSet::from_values(&values);

        assert!(
            matches!(result, Err(DelegationError::InvalidWatcher { .. })),
            "a persisted watcher whose predicate does not compile must be refused"
        );
    }

    // Scenario: l'événement est une constante
    #[test]
    fn event_is_a_constant_predicates_cannot_assign_it() {
        let set = set_of(vec![inject(
            "assign",
            "mcp",
            r#"[1].map(|x| event.name = "z").len() == 1"#,
        )]);

        let triggered = set.evaluate(&event("mcp", "x", json!({})));

        assert!(
            triggered.notifications().is_empty(),
            "an assignment to the constant triggers nothing"
        );
        assert_eq!(
            triggered.failures(),
            ["assign"],
            "an assignment to `event` fails: the event is a constant of the scope"
        );
    }

    // Scenario: format persisté
    #[test]
    fn persisted_form_has_the_documented_shape() {
        let set = set_of(vec![
            Watcher::new("w", "mcp", "true", WatcherAction::Inject("t".to_owned())).expect("valid"),
            Watcher::new("l", "timer", "false", WatcherAction::LaunchTurn("u".to_owned())).expect("valid"),
        ]);

        assert_eq!(
            set.to_values(),
            [
                json!({"id": "w", "source": "mcp", "predicate": "true", "action": {"kind": "inject", "text": "t"}}),
                json!({"id": "l", "source": "timer", "predicate": "false", "action": {"kind": "launch_turn", "text": "u"}}),
            ],
            "the persisted form is one object per watcher, in insertion order"
        );
    }

    // Scenario: restauration refusée au premier enregistrement invalide
    #[test]
    fn from_values_refuses_at_the_first_invalid_record() {
        let good = json!({"id": "ok", "source": "mcp", "predicate": "true", "action": {"kind": "inject", "text": "t"}});
        let bad = json!({"id": "bad", "source": "mcp", "predicate": "let x = ;", "action": {"kind": "inject", "text": "t"}});

        let control = WatcherSet::from_values(std::slice::from_ref(&good));
        assert_eq!(
            control.map(|set| set.watchers().len()).ok(),
            Some(1),
            "control: the valid record is restored"
        );

        let result = WatcherSet::from_values(&[good.clone(), bad, good]);
        assert!(
            matches!(result, Err(DelegationError::InvalidWatcher { .. })),
            "one invalid record refuses the whole set, with no partial set"
        );
    }

    #[test]
    fn persisted_form_malformed_record_is_refused() {
        let values = vec![json!({"id": "half"})];

        let result = WatcherSet::from_values(&values);

        assert!(
            matches!(result, Err(DelegationError::InvalidWatcher { .. })),
            "a malformed persisted record must be refused with InvalidWatcher"
        );
    }

    #[test]
    fn predicate_sees_the_pure_group_and_nothing_else() {
        // Control: the pure group of `vynil-core` is granted by default (SHA-256 of "abc").
        let pure = set_of(vec![inject(
            "pure",
            "mcp",
            r#"sha256("abc") == "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad""#,
        )]);
        let triggered = pure.evaluate(&event("mcp", "x", json!({})));
        assert_eq!(
            ids(triggered.notifications()),
            ["pure"],
            "the pure group is available to predicates"
        );

        // A function of a group that is not granted is unknown to the predicate.
        let host = set_of(vec![inject("host", "mcp", r#"get_env("HOME") == "" "#)]);
        let ungranted = host.evaluate(&event("mcp", "x", json!({})));
        assert_eq!(
            ungranted.failures(),
            ["host"],
            "an ungranted group is unknown to predicates"
        );
    }

    #[test]
    fn predicate_cannot_import() {
        // `import` is a statement, not an expression: a predicate that contains one is refused when it is built.
        let refused = Watcher::new(
            "import",
            "mcp",
            r#"import "m" as m; true"#,
            WatcherAction::Inject("x".to_owned()),
        );
        assert!(
            matches!(refused, Err(DelegationError::InvalidWatcher { .. })),
            "a predicate that imports a module must be refused with InvalidWatcher"
        );

        // A call into a module the predicate has not been granted fails at run time.
        let set = set_of(vec![inject("module", "mcp", "m::f()")]);
        let triggered = set.evaluate(&event("mcp", "x", json!({})));
        assert!(
            triggered.notifications().is_empty(),
            "a module call triggers nothing"
        );
        assert_eq!(
            triggered.failures(),
            ["module"],
            "a module call fails: no module is granted"
        );
    }

    #[test]
    fn event_table_carries_source_name_data_and_at() {
        let set = set_of(vec![inject(
            "table",
            "mcp",
            r#"event.source == "mcp" && event.name == "n" && event.data.k == 3 && event.at == 42"#,
        )]);

        let triggered = set.evaluate(&event("mcp", "n", json!({"k": 3})));

        assert_eq!(
            ids(triggered.notifications()),
            ["table"],
            "the event constant exposes source, name, data and at"
        );
    }

    #[test]
    fn watchers_of_one_source_fire_in_insertion_order() {
        let set = set_of(vec![
            inject("first", "mcp", "true"),
            inject("second", "mcp", "true"),
            inject("third", "mcp", "true"),
        ]);

        let triggered = set.evaluate(&event("mcp", "x", json!({})));

        assert_eq!(ids(triggered.notifications()), ["first", "second", "third"]);
    }

    #[test]
    fn evaluation_is_repeatable() {
        let set = set_of(vec![inject("repeat", "mcp", r#"event.name == "x""#)]);
        let first = set.evaluate(&event("mcp", "x", json!({})));
        assert_eq!(
            first.notifications().len(),
            1,
            "control: the first evaluation accepts the event"
        );

        let second = set.evaluate(&event("mcp", "x", json!({})));

        assert_eq!(
            first, second,
            "each evaluation starts from a fresh engine, so it gives the same outcome"
        );
    }

    #[test]
    fn remove_reports_whether_a_watcher_was_removed() {
        let mut set = set_of(vec![inject("gone", "mcp", "true")]);

        assert!(set.remove("gone"), "an existing watcher is removed");
        assert!(
            set.watchers().is_empty(),
            "the removed watcher is no longer listed"
        );
        assert!(!set.remove("gone"), "a second removal finds nothing");
    }
}
