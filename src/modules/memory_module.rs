//! Rhai modules of Memory, one per level: `memory_user`, `memory_project` and `memory_session`
//! (`agent-sessions/modules#FR-4`, `agent-sessions/modules#FR-5`, `agent-sessions/modules#AC-2`). Each function is synchronous
//! and calls the asynchronous store through `vynil_core::rt::block_on`. A successful `create`, `update` or `delete` emits one
//! `MemoryChanged` event that carries the level and the identifier, never the body; reading emits nothing.

use std::sync::Arc;

use rhai::{Array, Dynamic, EvalAltResult, ImmutableString, Map, Module, Position};
use vynil_core::rt::block_on;

use super::{Memory, MemoryLevel, MemoryScope, MemoryStore, MemorySummary, ModulesError, ModulesResult};
use crate::clock::Clock;
use crate::session::{Event, EventKind, EventSink, SessionId};

/// The scopes of the three levels of a session: the user and the project as the host names them, and the session.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct MemoryScopes {
    user: MemoryScope,
    project: MemoryScope,
    session: MemoryScope,
    session_id: SessionId,
}

impl MemoryScopes {
    /// Builds the scopes of a session from the user reference, the project reference and the session identifier.
    #[must_use]
    pub fn new(user: &str, project: &str, session: &SessionId) -> Self {
        Self {
            user: MemoryScope::user(user),
            project: MemoryScope::project(project),
            session: MemoryScope::session(session),
            session_id: session.clone(),
        }
    }

    /// Gives the scope of one level and the session identifier, consuming the scopes.
    fn into_scope(self, level: MemoryLevel) -> (MemoryScope, SessionId) {
        let scope = match level {
            MemoryLevel::User => self.user,
            MemoryLevel::Project => self.project,
            MemoryLevel::Session => self.session,
        };
        (scope, self.session_id)
    }
}

/// Returns the name of the Rhai module of a level: `memory_user`, `memory_project` or `memory_session`.
#[must_use]
pub const fn memory_module_name(level: MemoryLevel) -> &'static str {
    match level {
        MemoryLevel::User => "memory_user",
        MemoryLevel::Project => "memory_project",
        MemoryLevel::Session => "memory_session",
    }
}

/// Text of the refusal of an identifier that no Memory of the level carries.
const UNKNOWN_IDENTIFIER: &str = "unknown identifier";

/// Text of the refusal of a `create` on an identifier already taken in the level.
const TAKEN_IDENTIFIER: &str = "the identifier is already taken";

/// One level of Memory bound to its store: the scope of the level, the session and the event channel of the module.
struct Handle<S> {
    store: Arc<S>,
    level: MemoryLevel,
    scope: MemoryScope,
    session: SessionId,
    sink: Arc<dyn EventSink>,
    clock: Arc<dyn Clock>,
}

impl<S> Clone for Handle<S> {
    fn clone(&self) -> Self {
        Self {
            store: Arc::clone(&self.store),
            level: self.level,
            scope: self.scope.clone(),
            session: self.session.clone(),
            sink: Arc::clone(&self.sink),
            clock: Arc::clone(&self.clock),
        }
    }
}

impl<S: MemoryStore> Handle<S> {
    /// The refusal of a call in this level. It names the level and the identifier, never a body.
    fn refused(&self, id: &str, reason: &str) -> ModulesError {
        ModulesError::InvalidMemory {
            level: self.level.as_str().to_owned(),
            id: id.to_owned(),
            reason: reason.to_owned(),
        }
    }

    /// Builds a Memory of this level. A refusal of the Memory is renamed with the level of the call.
    fn build(&self, id: &str, title: &str, body: &str) -> ModulesResult<Memory> {
        let checked = Memory::new(id, title, body);
        let Err(ModulesError::InvalidMemory { reason, .. }) = checked else {
            return checked;
        };
        Err(self.refused(id, &reason))
    }

    /// Emits the `MemoryChanged` event of a successful change. It carries the level and the identifier only.
    fn changed(&self, id: &str) {
        self.sink.emit(Event::new(
            self.session.clone(),
            self.clock.now(),
            EventKind::MemoryChanged {
                level: self.level.as_str().to_owned(),
                id: id.to_owned(),
            },
        ));
    }

    /// Lists the summaries of the level.
    async fn list(&self) -> ModulesResult<Vec<MemorySummary>> {
        self.store.list(&self.scope).await
    }

    /// Reads the Memory of the identifier, or refuses an unknown identifier.
    async fn read(&self, id: &str) -> ModulesResult<Memory> {
        self.store
            .get(&self.scope, id)
            .await?
            .ok_or_else(|| self.refused(id, UNKNOWN_IDENTIFIER))
    }

    /// Stores a new Memory, or refuses an identifier already taken.
    async fn create(&self, id: &str, title: &str, body: &str) -> ModulesResult<()> {
        let memory = self.build(id, title, body)?;
        if self.store.get(&self.scope, id).await?.is_some() {
            return Err(self.refused(id, TAKEN_IDENTIFIER));
        }
        self.store.put(&self.scope, &memory).await
    }

    /// Replaces the Memory of the identifier, or refuses an unknown identifier.
    async fn update(&self, id: &str, title: &str, body: &str) -> ModulesResult<()> {
        if self.store.get(&self.scope, id).await?.is_none() {
            return Err(self.refused(id, UNKNOWN_IDENTIFIER));
        }
        let memory = self.build(id, title, body)?;
        self.store.put(&self.scope, &memory).await
    }

    /// Deletes the Memory of the identifier, or refuses an unknown identifier.
    async fn delete(&self, id: &str) -> ModulesResult<()> {
        if self.store.delete(&self.scope, id).await? {
            Ok(())
        } else {
            Err(self.refused(id, UNKNOWN_IDENTIFIER))
        }
    }
}

/// Runs a storage future to completion through the `vynil-core` bridge. A refusal of the store and a refusal of the bridge
/// (a `current_thread` runtime, a cancelled run) both become Rhai runtime errors that carry their message.
fn bridge<T>(future: impl Future<Output = ModulesResult<T>>) -> Result<T, Box<EvalAltResult>> {
    match block_on(future) {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(error)) => Err(Box::new(runtime_error(&error))),
        Err(error) => Err(vynil_core::rhai_err(error)),
    }
}

/// Wraps a refusal as a Rhai runtime error that carries its message.
fn runtime_error(error: &ModulesError) -> EvalAltResult {
    EvalAltResult::ErrorRuntime(Dynamic::from(error.to_string()), Position::NONE)
}

/// Builds a Rhai map from its named fields.
fn map_value<const N: usize>(fields: [(&str, Dynamic); N]) -> Dynamic {
    Dynamic::from(
        fields
            .into_iter()
            .map(|(key, value)| (key.into(), value))
            .collect::<Map>(),
    )
}

/// The map `#{id, title}` of a summary: never the body.
fn summary_value(summary: &MemorySummary) -> Dynamic {
    map_value([
        ("id", Dynamic::from(summary.id().to_owned())),
        ("title", Dynamic::from(summary.title().to_owned())),
    ])
}

/// The map `#{id, title, body}` of a Memory.
fn memory_value(memory: &Memory) -> Dynamic {
    map_value([
        ("id", Dynamic::from(memory.id().to_owned())),
        ("title", Dynamic::from(memory.title().to_owned())),
        ("body", Dynamic::from(memory.body().to_owned())),
    ])
}

/// Builds the Rhai module of one level of Memory.
///
/// The module offers `list()`, `read(id)`, `create(id, title, body)`, `update(id, title, body)` and `delete(id)`. It reads
/// and writes the scope of its level in `store`, and emits its `MemoryChanged` events through `sink`, dated by `clock` and
/// named after the session of `scopes`.
#[must_use]
pub fn memory_module<S>(
    level: MemoryLevel,
    store: Arc<S>,
    scopes: MemoryScopes,
    sink: Arc<dyn EventSink>,
    clock: Arc<dyn Clock>,
) -> Module
where
    S: MemoryStore + 'static,
{
    let (scope, session) = scopes.into_scope(level);
    let handle = Handle {
        store,
        level,
        scope,
        session,
        sink,
        clock,
    };
    let mut module = Module::new();
    let list_handle = handle.clone();
    let _list = module.set_native_fn("list", move || -> Result<Array, Box<EvalAltResult>> {
        let summaries = bridge(list_handle.list())?;
        Ok(summaries.iter().map(summary_value).collect())
    });
    let read_handle = handle.clone();
    let _read = module.set_native_fn(
        "read",
        move |id: ImmutableString| -> Result<Dynamic, Box<EvalAltResult>> {
            let memory = bridge(read_handle.read(id.as_str()))?;
            Ok(memory_value(&memory))
        },
    );
    let create_handle = handle.clone();
    let _create = module.set_native_fn(
        "create",
        move |id: ImmutableString,
              title: ImmutableString,
              body: ImmutableString|
              -> Result<(), Box<EvalAltResult>> {
            bridge(create_handle.create(id.as_str(), title.as_str(), body.as_str()))?;
            create_handle.changed(id.as_str());
            Ok(())
        },
    );
    let update_handle = handle.clone();
    let _update = module.set_native_fn(
        "update",
        move |id: ImmutableString,
              title: ImmutableString,
              body: ImmutableString|
              -> Result<(), Box<EvalAltResult>> {
            bridge(update_handle.update(id.as_str(), title.as_str(), body.as_str()))?;
            update_handle.changed(id.as_str());
            Ok(())
        },
    );
    let delete_handle = handle;
    let _delete = module.set_native_fn(
        "delete",
        move |id: ImmutableString| -> Result<(), Box<EvalAltResult>> {
            bridge(delete_handle.delete(id.as_str()))?;
            delete_handle.changed(id.as_str());
            Ok(())
        },
    );
    module
}

#[cfg(test)]
mod tests {
    use std::fmt::Debug;
    use std::sync::Arc;

    use rhai::{Array, Dynamic, Engine as RhaiEngine, EvalAltResult, Map};

    use super::{MemoryScopes, memory_module, memory_module_name};
    use crate::clock::{Clock, ManualClock, Timestamp};
    use crate::modules::{
        InMemoryMemoryStore, Memory, MemoryLevel, MemoryScope, MemoryStore, MemorySummary, ModulesError,
        ModulesResult,
    };
    use crate::session::{EventKind, EventSink, RecordingSink, SessionId};

    /// The store and the sink of one host under test. The modules of a test share the store.
    struct Host {
        store: Arc<InMemoryMemoryStore>,
        sink: Arc<RecordingSink>,
    }

    impl Host {
        fn new() -> Self {
            Self {
                store: Arc::new(InMemoryMemoryStore::new()),
                sink: Arc::new(RecordingSink::new()),
            }
        }

        /// A Rhai engine with the module of each given level, for the session `s-1`.
        fn engine(&self, levels: &[MemoryLevel]) -> RhaiEngine {
            self.engine_for("s-1", levels)
        }

        /// A Rhai engine with the module of each given level, for the given session. The user is `alice` and the project
        /// is `alpha`.
        fn engine_for(&self, session: &str, levels: &[MemoryLevel]) -> RhaiEngine {
            let session_id = SessionId::new(session).expect("a non-empty identifier");
            let scopes = MemoryScopes::new("alice", "alpha", &session_id);
            let recording: Arc<RecordingSink> = Arc::clone(&self.sink);
            let sink: Arc<dyn EventSink> = recording;
            let clock: Arc<dyn Clock> = Arc::new(ManualClock::new(Timestamp::from_millis(1_000)));
            let mut engine = RhaiEngine::new();
            for level in levels {
                let module = memory_module(
                    *level,
                    Arc::clone(&self.store),
                    scopes.clone(),
                    Arc::clone(&sink),
                    Arc::clone(&clock),
                );
                let _registered = engine.register_static_module(memory_module_name(*level), Arc::new(module));
            }
            engine
        }
    }

    /// The text of the error of a failed script. The script must fail.
    fn error_text<T: Debug>(result: Result<T, Box<EvalAltResult>>) -> String {
        match result {
            Ok(value) => panic!("the script must fail, it returned {value:?}"),
            Err(error) => error.to_string(),
        }
    }

    /// The `(level, identifier)` of every `MemoryChanged` event, in emission order.
    fn memory_changes(sink: &RecordingSink) -> Vec<(String, String)> {
        sink.events()
            .into_iter()
            .filter_map(|event| {
                let EventKind::MemoryChanged { level, id } = event.kind else {
                    return None;
                };
                Some((level, id))
            })
            .collect()
    }

    // Scenario: création, lecture et événement
    #[test]
    fn events_name_their_level_and_their_session() {
        let host = Host::new();
        let engine = host.engine_for(
            "s-9",
            &[MemoryLevel::User, MemoryLevel::Project, MemoryLevel::Session],
        );
        engine
            .eval::<()>(
                r#"memory_user::create("u", "t", "b");
                   memory_project::create("p", "t", "b");
                   memory_session::create("s", "t", "b");"#,
            )
            .expect("the scripts succeed");
        assert_eq!(
            memory_changes(&host.sink),
            vec![
                ("user".to_owned(), "u".to_owned()),
                ("project".to_owned(), "p".to_owned()),
                ("session".to_owned(), "s".to_owned()),
            ],
            "each event names the level of its module"
        );
        for event in host.sink.events() {
            assert_eq!(
                event.session.as_str(),
                "s-9",
                "each event names the session of the scopes"
            );
        }
    }

    /// A store whose storage always fails.
    struct BrokenStore;

    impl MemoryStore for BrokenStore {
        fn list(
            &self,
            _scope: &MemoryScope,
        ) -> impl Future<Output = ModulesResult<Vec<MemorySummary>>> + Send {
            std::future::ready(Err(ModulesError::Store("disk gone".to_owned())))
        }

        fn get(
            &self,
            _scope: &MemoryScope,
            _id: &str,
        ) -> impl Future<Output = ModulesResult<Option<Memory>>> + Send {
            std::future::ready(Err(ModulesError::Store("disk gone".to_owned())))
        }

        fn put(
            &self,
            _scope: &MemoryScope,
            _memory: &Memory,
        ) -> impl Future<Output = ModulesResult<()>> + Send {
            std::future::ready(Err(ModulesError::Store("disk gone".to_owned())))
        }

        fn delete(
            &self,
            _scope: &MemoryScope,
            _id: &str,
        ) -> impl Future<Output = ModulesResult<bool>> + Send {
            std::future::ready(Err(ModulesError::Store("disk gone".to_owned())))
        }
    }

    // Scenario: création, lecture et événement
    #[tokio::test(flavor = "multi_thread")]
    async fn create_then_read_returns_the_memory_and_emits_one_event() {
        let host = Host::new();
        let engine = host.engine(&[MemoryLevel::User]);
        let title = engine
            .eval::<String>(r#"memory_user::create("k", "Titre", "Corps"); memory_user::read("k").title"#)
            .expect("create and read succeed");
        assert_eq!(title, "Titre", "the read gives the title");
        let body = engine
            .eval::<String>(r#"memory_user::read("k").body"#)
            .expect("read succeeds");
        assert_eq!(body, "Corps", "the read gives the body");
        assert_eq!(
            memory_changes(&host.sink),
            vec![("user".to_owned(), "k".to_owned())],
            "exactly one MemoryChanged of level user and identifier k"
        );
        assert!(
            !format!("{:?}", host.sink.events()).contains("Corps"),
            "the event never carries the body"
        );
    }

    // Scenario: niveaux indépendants
    #[tokio::test(flavor = "multi_thread")]
    async fn each_level_reads_its_own_memory() {
        let host = Host::new();
        let engine = host.engine(&[MemoryLevel::User, MemoryLevel::Project]);
        let both = engine
            .eval::<String>(
                r#"memory_user::create("note", "user", "u"); memory_project::create("note", "project", "p"); memory_user::read("note").title + "/" + memory_project::read("note").title"#,
            )
            .expect("the scripts succeed");
        assert_eq!(both, "user/project", "each module reads its own memory");
    }

    // Scenario: la liste ne rend que l'identifiant et le titre
    #[tokio::test(flavor = "multi_thread")]
    async fn list_gives_identifier_and_title_only() {
        let host = Host::new();
        let engine = host.engine(&[MemoryLevel::Project]);
        engine
            .eval::<()>(r#"memory_project::create("beta", "second", "secret body two"); memory_project::create("alpha", "first", "secret body one")"#)
            .expect("the memories are created");
        let items = engine
            .eval::<Array>("memory_project::list()")
            .expect("list succeeds");
        assert_eq!(items.len(), 2, "both memories are listed");
        for item in items {
            let map = item.try_cast::<Map>().expect("each summary is a map");
            assert_eq!(map.len(), 2, "a summary has two fields");
            assert!(
                map.contains_key("id") && map.contains_key("title"),
                "a summary has the identifier and the title"
            );
            assert!(!map.contains_key("body"), "a summary has no body");
        }
    }

    // Scenario: lecture sans événement
    #[tokio::test(flavor = "multi_thread")]
    async fn read_and_list_emit_nothing() {
        let host = Host::new();
        let memory = Memory::new("k", "Titre", "Corps").expect("a valid memory");
        host.store
            .put(&MemoryScope::user("alice"), &memory)
            .await
            .expect("the memory is stored");
        let engine = host.engine(&[MemoryLevel::User]);
        let title = engine
            .eval::<String>(r#"memory_user::read("k").title"#)
            .expect("read succeeds");
        let count = engine
            .eval::<i64>("memory_user::list().len()")
            .expect("list succeeds");
        assert_eq!(title, "Titre", "the memory is read");
        assert_eq!(count, 1, "the memory is listed");
        assert!(
            memory_changes(&host.sink).is_empty(),
            "reading and listing emit no event"
        );
    }

    // Scenario: appel refusé sans événement
    #[tokio::test(flavor = "multi_thread")]
    async fn refused_calls_name_level_and_identifier_and_emit_nothing() {
        let host = Host::new();
        let engine = host.engine(&[MemoryLevel::User]);
        for script in [
            r#"memory_user::read("absente")"#,
            r#"memory_user::update("absente", "t", "c")"#,
            r#"memory_user::delete("absente")"#,
        ] {
            let message = error_text(engine.eval::<Dynamic>(script));
            assert!(
                message.contains("user") && message.contains("absente"),
                "{script} names the level and the identifier: {message}"
            );
        }
        assert!(
            memory_changes(&host.sink).is_empty(),
            "refused calls emit no event"
        );
    }

    // Scenario: création d'un identifiant pris
    #[tokio::test(flavor = "multi_thread")]
    async fn create_of_a_taken_identifier_keeps_the_original() {
        let host = Host::new();
        let engine = host.engine(&[MemoryLevel::User]);
        engine
            .eval::<()>(r#"memory_user::create("k", "Titre", "Corps")"#)
            .expect("the first create succeeds");
        let message = error_text(engine.eval::<()>(r#"memory_user::create("k", "Autre", "Autre corps")"#));
        assert!(
            message.contains("user") && message.contains("`k`"),
            "the refusal names the level and the identifier: {message}"
        );
        let title = engine
            .eval::<String>(r#"memory_user::read("k").title"#)
            .expect("read succeeds");
        assert_eq!(title, "Titre", "the original memory is unchanged");
        assert_eq!(
            memory_changes(&host.sink).len(),
            1,
            "the refused create emits no event"
        );
    }

    // Scenario: suppression
    #[tokio::test(flavor = "multi_thread")]
    async fn delete_then_read_fails_and_emits_the_deletion() {
        let host = Host::new();
        let engine = host.engine(&[MemoryLevel::User]);
        engine
            .eval::<()>(r#"memory_user::create("k", "Titre", "Corps"); memory_user::delete("k")"#)
            .expect("create and delete succeed");
        let message = error_text(engine.eval::<Dynamic>(r#"memory_user::read("k")"#));
        assert!(
            message.contains("`k`"),
            "the read after the delete names the identifier: {message}"
        );
        assert_eq!(
            memory_changes(&host.sink).len(),
            2,
            "the create and the deletion each emit one event"
        );
    }

    // Scenario: la session qualifie le niveau session
    #[tokio::test(flavor = "multi_thread")]
    async fn session_level_is_qualified_by_the_session() {
        let host = Host::new();
        host.engine_for("s-1", &[MemoryLevel::Session])
            .eval::<()>(r#"memory_session::create("note", "pour s-1", "b")"#)
            .expect("the first session creates");
        host.engine_for("s-2", &[MemoryLevel::Session])
            .eval::<()>(r#"memory_session::create("note", "pour s-2", "b")"#)
            .expect("the second session creates");
        let first = host.engine_for("s-1", &[MemoryLevel::Session]);
        let second = host.engine_for("s-2", &[MemoryLevel::Session]);
        let read_first = first
            .eval::<String>(r#"memory_session::read("note").title"#)
            .expect("read succeeds");
        let read_second = second
            .eval::<String>(r#"memory_session::read("note").title"#)
            .expect("read succeeds");
        assert_eq!(read_first, "pour s-1", "the first session reads its own memory");
        assert_eq!(read_second, "pour s-2", "the second session reads its own memory");
        let listed = first
            .eval::<i64>("memory_session::list().len()")
            .expect("list succeeds");
        assert_eq!(listed, 1, "a session lists only its own memories");
    }

    // Scenario: runtime current_thread
    #[tokio::test]
    async fn current_thread_runtime_renders_the_explicit_error() {
        let host = Host::new();
        let engine = host.engine(&[MemoryLevel::User]);
        for script in [
            "memory_user::list()",
            r#"memory_user::read("k")"#,
            r#"memory_user::create("k", "t", "c")"#,
            r#"memory_user::update("k", "t", "c")"#,
            r#"memory_user::delete("k")"#,
        ] {
            let message = error_text(engine.eval::<Dynamic>(script));
            assert!(
                message.contains("multi-thread"),
                "{script} names the runtime it needs: {message}"
            );
        }
        assert!(memory_changes(&host.sink).is_empty(), "no call emits an event");
    }

    // Not a Scenario: a successful update replaces the memory and emits one event. The spec states it in `Must`; no
    // Scenario covers it yet.
    #[tokio::test(flavor = "multi_thread")]
    async fn update_replaces_the_memory_and_emits_one_event() {
        let host = Host::new();
        let engine = host.engine(&[MemoryLevel::User]);
        engine
            .eval::<()>(r#"memory_user::create("k", "Titre", "Corps"); memory_user::update("k", "Nouveau", "Nouveau corps")"#)
            .expect("create and update succeed");
        let title = engine
            .eval::<String>(r#"memory_user::read("k").title"#)
            .expect("read succeeds");
        let body = engine
            .eval::<String>(r#"memory_user::read("k").body"#)
            .expect("read succeeds");
        assert_eq!(title, "Nouveau", "the update replaces the title");
        assert_eq!(body, "Nouveau corps", "the update replaces the body");
        assert_eq!(
            memory_changes(&host.sink).len(),
            2,
            "the create and the update each emit one event"
        );
    }

    // Not a Scenario: a blank title is refused at create, and the refusal names the level and the identifier.
    #[tokio::test(flavor = "multi_thread")]
    async fn create_with_blank_title_names_level_and_identifier() {
        let host = Host::new();
        let engine = host.engine(&[MemoryLevel::Project]);
        let message = error_text(engine.eval::<()>(r#"memory_project::create("k", "   ", "c")"#));
        assert!(
            message.contains("project") && message.contains("`k`"),
            "the refusal names the level and the identifier: {message}"
        );
        assert!(
            memory_changes(&host.sink).is_empty(),
            "a refused create emits no event"
        );
    }

    // Not a Scenario: a storage failure of the host is an explicit Rhai error that carries its message.
    #[tokio::test(flavor = "multi_thread")]
    async fn store_failure_is_an_explicit_error() {
        let host = Host::new();
        let scopes = MemoryScopes::new("alice", "alpha", &SessionId::new("s-1").expect("a session id"));
        let recording: Arc<RecordingSink> = Arc::clone(&host.sink);
        let sink: Arc<dyn EventSink> = recording;
        let clock: Arc<dyn Clock> = Arc::new(ManualClock::new(Timestamp::from_millis(1_000)));
        let module = memory_module(MemoryLevel::User, Arc::new(BrokenStore), scopes, sink, clock);
        let mut engine = RhaiEngine::new();
        let _registered = engine.register_static_module("memory_user", Arc::new(module));
        let message = error_text(engine.eval::<Array>("memory_user::list()"));
        assert!(
            message.contains("disk gone"),
            "the error carries the store message: {message}"
        );
        assert!(
            memory_changes(&host.sink).is_empty(),
            "a failed call emits no event"
        );
    }

    #[test]
    fn module_names_are_the_three_identifiers() {
        assert_eq!(
            memory_module_name(MemoryLevel::User),
            "memory_user",
            "the user module"
        );
        assert_eq!(
            memory_module_name(MemoryLevel::Project),
            "memory_project",
            "the project module"
        );
        assert_eq!(
            memory_module_name(MemoryLevel::Session),
            "memory_session",
            "the session module"
        );
    }
}
