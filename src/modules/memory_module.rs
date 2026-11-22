//! Rhai modules of Memory, one per level: `memory_user`, `memory_shared` and `memory_session`
//! (`agent-sessions/modules#FR-4`, `agent-sessions/modules#FR-5`, `agent-sessions/modules#AC-2`). Each function is synchronous
//! and calls the asynchronous store through `vynil_core::rt::block_on`. A successful `create`, `update` or `delete` emits one
//! `MemoryChanged` event that carries the level and the identifier, never the body; reading emits nothing.

#[cfg(feature = "embedder")]
use std::collections::BTreeMap;
#[cfg(feature = "embedder")]
use std::num::NonZeroUsize;
use std::sync::Arc;

use rhai::{Array, Dynamic, EvalAltResult, ImmutableString, Map, Module, Position};
use vynil_core::rt::block_on;

#[cfg(feature = "embedder")]
use super::SemanticIndex;
use super::{Memory, MemoryLevel, MemoryScope, MemoryStore, MemorySummary, ModulesError, ModulesResult};
use crate::clock::Clock;
use crate::session::{Event, EventKind, EventSink, SessionId};

/// The scopes of the three levels of a session: the user and the shared level as the host names them, and the session.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct MemoryScopes {
    user: MemoryScope,
    shared: MemoryScope,
    session: MemoryScope,
    session_id: SessionId,
    #[cfg(feature = "embedder")]
    indexes: BTreeMap<MemoryLevel, SemanticIndex>,
}

impl MemoryScopes {
    /// Builds the scopes of a session from the user reference, the shared reference and the session identifier.
    #[must_use]
    pub fn new(user: &str, shared: &str, session: &SessionId) -> Self {
        Self {
            user: MemoryScope::user(user),
            shared: MemoryScope::shared(shared),
            session: MemoryScope::session(session),
            session_id: session.clone(),
            #[cfg(feature = "embedder")]
            indexes: BTreeMap::new(),
        }
    }

    /// Gives the semantic index of the scope of one level, replacing the one given before. A level without an index answers
    /// its `search` with an explicit error: no index is an error, never an empty result.
    #[cfg(feature = "embedder")]
    #[must_use]
    pub fn with_index(mut self, level: MemoryLevel, index: SemanticIndex) -> Self {
        let _previous = self.indexes.insert(level, index);
        self
    }

    /// Returns the semantic index of a level, when one is given.
    #[cfg(feature = "embedder")]
    fn index_of(&self, level: MemoryLevel) -> Option<SemanticIndex> {
        self.indexes.get(&level).cloned()
    }

    /// Gives the scope of one level and the session identifier, consuming the scopes.
    fn into_scope(self, level: MemoryLevel) -> (MemoryScope, SessionId) {
        let scope = match level {
            MemoryLevel::User => self.user,
            MemoryLevel::Shared => self.shared,
            MemoryLevel::Session => self.session,
        };
        (scope, self.session_id)
    }
}

/// Returns the name of the Rhai module of a level: `memory_user`, `memory_shared` or `memory_session`.
#[must_use]
pub const fn memory_module_name(level: MemoryLevel) -> &'static str {
    match level {
        MemoryLevel::User => "memory_user",
        MemoryLevel::Shared => "memory_shared",
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
    #[cfg(feature = "embedder")]
    index: Option<SemanticIndex>,
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
            #[cfg(feature = "embedder")]
            index: self.index.clone(),
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

    /// Searches the level through its index: the Memories closest to `query`, with the scores of the index, in the order of
    /// the index. An identifier that the index holds and the store does not know is skipped, so the count can fall below
    /// `limit`. The title comes from the store, never from the index.
    #[cfg(feature = "embedder")]
    async fn search(
        &self,
        index: &SemanticIndex,
        query: &str,
        limit: NonZeroUsize,
    ) -> ModulesResult<Vec<(Memory, f64)>> {
        let hits = index.search(query, limit).await?;
        let mut found = Vec::with_capacity(hits.len());
        for hit in hits {
            if let Some(memory) = self.store.get(&self.scope, hit.id()).await? {
                found.push((memory, hit.score()));
            }
        }
        Ok(found)
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

/// Reads the limit of a search: an integer of at least 1.
#[cfg(feature = "embedder")]
fn search_limit(limit: i64) -> Result<NonZeroUsize, Box<EvalAltResult>> {
    usize::try_from(limit)
        .ok()
        .and_then(NonZeroUsize::new)
        .ok_or_else(|| refusal("invalid argument `limit`: it must be an integer of at least 1").into())
}

/// Wraps a refusal of a call as a Rhai runtime error that carries its text.
#[cfg(feature = "embedder")]
fn refusal(text: &str) -> EvalAltResult {
    EvalAltResult::ErrorRuntime(Dynamic::from(text.to_owned()), Position::NONE)
}

/// The map `#{id, title, score}` of a search hit: never the body.
#[cfg(feature = "embedder")]
fn search_value(memory: &Memory, score: f64) -> Dynamic {
    map_value([
        ("id", Dynamic::from(memory.id().to_owned())),
        ("title", Dynamic::from(memory.title().to_owned())),
        ("score", Dynamic::from(score)),
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

/// Builds the Rhai module of one level of Memory with the full access: every function of the level is offered, as before.
///
/// The module offers `list()`, `read(id)`, `create(id, title, body)`, `update(id, title, body)` and `delete(id)`. It reads
/// and writes the scope of its level in `store`, and emits its `MemoryChanged` events through `sink`, dated by `clock` and
/// named after the session of `scopes`. The `MemoryAccess::Read` of the native table corresponds to
/// [`memory_module_read_only`].
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
    assemble(handle_of(level, store, scopes, sink, clock), true)
}

/// Builds the Rhai module of one level of Memory in read-only access: `list()` and `read(id)` only, no writing function.
///
/// It is the module of `MemoryAccess::Read` in the native table (native.sdd Tasks #2, `agent-sessions/modules#FR-4`).
/// The `store`, `scopes`, `sink` and `clock` are those of [`memory_module`]; the scope of the level is never written
/// through this module, so it emits no `MemoryChanged`.
#[must_use]
pub fn memory_module_read_only<S>(
    level: MemoryLevel,
    store: Arc<S>,
    scopes: MemoryScopes,
    sink: Arc<dyn EventSink>,
    clock: Arc<dyn Clock>,
) -> Module
where
    S: MemoryStore + 'static,
{
    assemble(handle_of(level, store, scopes, sink, clock), false)
}

/// Binds a level to its store: the scope of the level, the session and the event channel of the module.
fn handle_of<S>(
    level: MemoryLevel,
    store: Arc<S>,
    scopes: MemoryScopes,
    sink: Arc<dyn EventSink>,
    clock: Arc<dyn Clock>,
) -> Handle<S> {
    #[cfg(feature = "embedder")]
    let index = scopes.index_of(level);
    let (scope, session) = scopes.into_scope(level);
    Handle {
        store,
        level,
        scope,
        session,
        sink,
        clock,
        #[cfg(feature = "embedder")]
        index,
    }
}

/// Assembles the Rhai module of a level: the reading functions always, the writing functions only when `writes`.
fn assemble<S>(handle: Handle<S>, writes: bool) -> Module
where
    S: MemoryStore + 'static,
{
    let mut module = Module::new();
    let list_handle = handle.clone();
    let _list = module.set_native_fn("list", move || -> Result<Array, Box<EvalAltResult>> {
        let summaries = bridge(list_handle.list())?;
        Ok(summaries.iter().map(summary_value).collect())
    });
    #[cfg(feature = "embedder")]
    {
        let search_handle = handle.clone();
        let _search = module.set_native_fn(
            "search",
            move |query: ImmutableString, limit: i64| -> Result<Array, Box<EvalAltResult>> {
                let limit = search_limit(limit)?;
                if query.trim().is_empty() {
                    return Err(refusal("invalid argument `query`: the query is blank").into());
                }
                let Some(index) = search_handle.index.as_ref() else {
                    let missing = ModulesError::NoIndex {
                        level: search_handle.level.as_str().to_owned(),
                    };
                    return Err(runtime_error(&missing).into());
                };
                let found = bridge(search_handle.search(index, query.as_str(), limit))?;
                Ok(found
                    .iter()
                    .map(|(memory, score)| search_value(memory, *score))
                    .collect())
            },
        );
    }
    let read_handle = handle.clone();
    let _read = module.set_native_fn(
        "read",
        move |id: ImmutableString| -> Result<Dynamic, Box<EvalAltResult>> {
            let memory = bridge(read_handle.read(id.as_str()))?;
            Ok(memory_value(&memory))
        },
    );
    if writes {
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
    }
    module
}

#[cfg(test)]
mod tests {
    use std::fmt::Debug;
    use std::sync::Arc;

    use rhai::{Array, Dynamic, Engine as RhaiEngine, EvalAltResult, Map};

    use super::{MemoryScopes, memory_module, memory_module_name, memory_module_read_only};
    use crate::clock::{Clock, ManualClock, Timestamp};
    use crate::modules::{
        InMemoryMemoryStore, Memory, MemoryAccess, MemoryLevel, MemoryScope, MemoryStore, MemorySummary,
        ModulesError, ModulesResult,
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

        /// A Rhai engine with the module of each given level, for the given session. The user is `alice` and the shared reference
        /// is `alpha`.
        fn engine_for(&self, session: &str, levels: &[MemoryLevel]) -> RhaiEngine {
            self.engine_with_access(session, levels, &MemoryAccess::ReadWrite)
        }

        /// A Rhai engine with the module of each given level built with the given access, for the given session.
        fn engine_with_access(
            &self,
            session: &str,
            levels: &[MemoryLevel],
            access: &MemoryAccess,
        ) -> RhaiEngine {
            let session_id = SessionId::new(session).expect("a non-empty identifier");
            let scopes = MemoryScopes::new("alice", "alpha", &session_id);
            let recording: Arc<RecordingSink> = Arc::clone(&self.sink);
            let sink: Arc<dyn EventSink> = recording;
            let clock: Arc<dyn Clock> = Arc::new(ManualClock::new(Timestamp::from_millis(1_000)));
            let build = if matches!(access, MemoryAccess::Read) {
                memory_module_read_only::<InMemoryMemoryStore>
            } else {
                memory_module::<InMemoryMemoryStore>
            };
            let mut engine = RhaiEngine::new();
            for level in levels {
                let module = build(
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
            &[MemoryLevel::User, MemoryLevel::Shared, MemoryLevel::Session],
        );
        engine
            .eval::<()>(
                r#"memory_user::create("u", "t", "b");
                   memory_shared::create("p", "t", "b");
                   memory_session::create("s", "t", "b");"#,
            )
            .expect("the scripts succeed");
        assert_eq!(
            memory_changes(&host.sink),
            vec![
                ("user".to_owned(), "u".to_owned()),
                ("shared".to_owned(), "p".to_owned()),
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
        let engine = host.engine(&[MemoryLevel::User, MemoryLevel::Shared]);
        let both = engine
            .eval::<String>(
                r#"memory_user::create("note", "user", "u"); memory_shared::create("note", "shared", "p"); memory_user::read("note").title + "/" + memory_shared::read("note").title"#,
            )
            .expect("the scripts succeed");
        assert_eq!(both, "user/shared", "each module reads its own memory");
    }

    // Scenario: la liste ne rend que l'identifiant et le titre
    #[tokio::test(flavor = "multi_thread")]
    async fn list_gives_identifier_and_title_only() {
        let host = Host::new();
        let engine = host.engine(&[MemoryLevel::Shared]);
        engine
            .eval::<()>(r#"memory_shared::create("beta", "second", "secret body two"); memory_shared::create("alpha", "first", "secret body one")"#)
            .expect("the memories are created");
        let items = engine
            .eval::<Array>("memory_shared::list()")
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
        let engine = host.engine(&[MemoryLevel::Shared]);
        let message = error_text(engine.eval::<()>(r#"memory_shared::create("k", "   ", "c")"#));
        assert!(
            message.contains("shared") && message.contains("`k`"),
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

    // native.sdd Tasks #2: a module built with MemoryAccess::Read exposes the reading functions and no writing
    // function (native.sdd `Must`, agent-sessions/modules#FR-4).
    #[tokio::test(flavor = "multi_thread")]
    async fn read_access_module_has_no_writing_function() {
        let host = Host::new();
        host.engine(&[MemoryLevel::User])
            .eval::<()>(r#"memory_user::create("k", "Titre", "Corps")"#)
            .expect("the seed create succeeds with the full module");
        let engine = host.engine_with_access("s-1", &[MemoryLevel::User], &MemoryAccess::Read);
        let title = engine
            .eval::<String>(r#"memory_user::read("k").title"#)
            .expect("read is offered");
        let count = engine
            .eval::<i64>("memory_user::list().len()")
            .expect("list is offered");
        assert_eq!(title, "Titre", "the read gives the seeded memory");
        assert_eq!(count, 1, "the list gives the seeded memory");
        for script in [
            r#"memory_user::create("n", "t", "c")"#,
            r#"memory_user::update("k", "t", "c")"#,
            r#"memory_user::delete("k")"#,
        ] {
            let error = engine
                .eval::<()>(script)
                .expect_err("the writing function must not exist in a Read module");
            assert!(
                matches!(*error, EvalAltResult::ErrorFunctionNotFound { .. }),
                "{script} fails as a missing function, not at run time: {error}"
            );
        }
        assert_eq!(
            engine
                .eval::<String>(r#"memory_user::read("k").title"#)
                .expect("read still succeeds"),
            "Titre",
            "the refused calls changed nothing"
        );
        assert!(
            memory_changes(&host.sink).len() == 1,
            "only the seed create emitted an event"
        );
    }

    // native.sdd Tasks #2: with no access narrowed, the module exposes everything, as before.
    #[tokio::test(flavor = "multi_thread")]
    async fn read_write_access_module_exposes_every_function() {
        let host = Host::new();
        let engine = host.engine_with_access("s-1", &[MemoryLevel::User], &MemoryAccess::ReadWrite);
        engine
            .eval::<()>(
                r#"memory_user::create("k", "Titre", "Corps");
                   memory_user::update("k", "Nouveau", "Nouveau corps");
                   memory_user::delete("k")"#,
            )
            .expect("create, update and delete are offered with ReadWrite access");
        assert_eq!(
            memory_changes(&host.sink).len(),
            3,
            "the three changes each emitted one event"
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
            memory_module_name(MemoryLevel::Shared),
            "memory_shared",
            "the shared module"
        );
        assert_eq!(
            memory_module_name(MemoryLevel::Session),
            "memory_session",
            "the session module"
        );
    }
}

#[cfg(test)]
#[cfg(feature = "embedder")]
mod search_tests {
    use std::sync::Arc;

    use rhai::{Array, Engine as RhaiEngine, Map};

    use super::super::semantic::test_support::index_of;
    use super::{MemoryScopes, memory_module, memory_module_name, memory_module_read_only};
    use crate::clock::{Clock, ManualClock, Timestamp};
    use crate::modules::{InMemoryMemoryStore, Memory, MemoryLevel, SemanticIndex, memory_embedding_text};
    use crate::session::{EventSink, NullSink, SessionId};

    /// A current-thread runtime for building the test indexes, which are asynchronous.
    fn runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("a current-thread runtime builds")
    }

    /// The index a host builds over the given Memories, with their embedding texts and identifiers.
    fn host_index(memories: &[Memory]) -> SemanticIndex {
        let documents: Vec<(&str, String)> = memories
            .iter()
            .map(|memory| (memory.id(), memory_embedding_text(memory)))
            .collect();
        let borrowed: Vec<(&str, &str)> = documents.iter().map(|(id, text)| (*id, text.as_str())).collect();
        runtime().block_on(index_of(&borrowed))
    }

    /// A Memory built for the test; the store receives it through the Rhai functions of the modules.
    fn memory(id: &str, title: &str, body: &str) -> Memory {
        Memory::new(id, title, body).expect("a valid test memory")
    }

    /// The scopes of session `s-1` with the given semantic indexes per level.
    fn scopes(indexes: Vec<(MemoryLevel, SemanticIndex)>) -> MemoryScopes {
        let session = SessionId::new("s-1").expect("a non-empty identifier");
        indexes.into_iter().fold(
            MemoryScopes::new("alice", "alpha", &session),
            |scopes, (level, index)| scopes.with_index(level, index),
        )
    }

    /// A Rhai engine with the module of each given level over `store`, with write access or in read-only access.
    fn engine(
        store: &Arc<InMemoryMemoryStore>,
        scopes: &MemoryScopes,
        levels: &[MemoryLevel],
        read_only: bool,
    ) -> RhaiEngine {
        let sink: Arc<dyn EventSink> = Arc::new(NullSink);
        let clock: Arc<dyn Clock> = Arc::new(ManualClock::new(Timestamp::from_millis(1_000)));
        let mut engine = RhaiEngine::new();
        for level in levels {
            let module = if read_only {
                memory_module_read_only(
                    *level,
                    Arc::clone(store),
                    scopes.clone(),
                    Arc::clone(&sink),
                    Arc::clone(&clock),
                )
            } else {
                memory_module(
                    *level,
                    Arc::clone(store),
                    scopes.clone(),
                    Arc::clone(&sink),
                    Arc::clone(&clock),
                )
            };
            let _registered = engine.register_static_module(memory_module_name(*level), Arc::new(module));
        }
        engine
    }

    /// The hits of a search script, as maps. The script must succeed and return an array.
    fn hits(engine: &RhaiEngine, script: &str) -> Vec<Map> {
        let array: Array = engine
            .eval(script)
            .unwrap_or_else(|error| panic!("the search must succeed: {error}"));
        array
            .into_iter()
            .map(|item| item.try_cast::<Map>().expect("each hit is a map"))
            .collect()
    }

    /// The text of a field of a hit, empty when the field is missing or is not text.
    fn text_of(hit: &Map, key: &str) -> String {
        hit.get(key)
            .and_then(|value| value.clone().into_string().ok())
            .unwrap_or_default()
    }

    /// The error text of a script that must fail.
    fn refusal(engine: &RhaiEngine, script: &str) -> String {
        match engine.eval::<rhai::Dynamic>(script) {
            Ok(value) => panic!("the script must fail, it returned {value}"),
            Err(error) => error.to_string(),
        }
    }

    #[test]
    fn search_puts_the_close_memory_first() {
        let store = Arc::new(InMemoryMemoryStore::new());
        let memories = [
            memory("a", "Vacances", "plage soleil"),
            memory("b", "Courses", "pain lait"),
            memory("c", "Reunion", "projet budget"),
        ];
        let index = host_index(&memories);
        let scopes = scopes(vec![(MemoryLevel::User, index)]);
        let engine = engine(&store, &scopes, &[MemoryLevel::User], false);
        for memory in &memories {
            let script = format!(
                "memory_user::create(\"{}\", \"{}\", \"{}\");",
                memory.id(),
                memory.title(),
                memory.body()
            );
            engine.run(&script).expect("the memory is created");
        }
        let found = hits(&engine, "memory_user::search(\"plage soleil\", 3)");
        let first = found.first().expect("the index holds three memories");
        assert_eq!(text_of(first, "id"), "a", "the close memory comes first");
        assert_eq!(
            text_of(first, "title"),
            "Vacances",
            "the title is read from the store"
        );
        assert!(first.get("score").is_some(), "the hit carries its score");
        assert!(first.get("body").is_none(), "no hit carries the body");
        assert_eq!(found.len(), 3, "the limit is met");
    }

    #[test]
    fn search_never_uses_the_index_of_another_level() {
        let store = Arc::new(InMemoryMemoryStore::new());
        let user_index = host_index(&[
            memory("note", "Titre utilisateur", "pain lait"),
            memory("autre", "Autre", "plage soleil"),
        ]);
        let shared_index = host_index(&[memory("note", "Titre partage", "plage soleil")]);
        let scopes = scopes(vec![
            (MemoryLevel::User, user_index),
            (MemoryLevel::Shared, shared_index),
        ]);
        let engine = engine(&store, &scopes, &[MemoryLevel::User, MemoryLevel::Shared], false);
        engine
            .run("memory_user::create(\"note\", \"Titre utilisateur\", \"pain lait\");")
            .expect("the user note is created");
        engine
            .run("memory_user::create(\"autre\", \"Autre\", \"plage soleil\");")
            .expect("the other user memory is created");
        engine
            .run("memory_shared::create(\"note\", \"Titre partage\", \"plage soleil\");")
            .expect("the shared note is created");
        let found = hits(&engine, "memory_user::search(\"plage soleil\", 2)");
        assert_eq!(
            text_of(&found[0], "id"),
            "autre",
            "the user index decides the rank, not the shared index"
        );
        assert!(
            found.iter().all(|hit| text_of(hit, "title") != "Titre partage"),
            "the shared memory never comes out of the user search"
        );
    }

    #[test]
    fn search_is_limited_to_the_scope_of_the_level() {
        let store = Arc::new(InMemoryMemoryStore::new());
        let user_index = host_index(&[memory("note", "Titre utilisateur", "plage soleil")]);
        let scopes = scopes(vec![(MemoryLevel::User, user_index)]);
        let engine = engine(&store, &scopes, &[MemoryLevel::User, MemoryLevel::Shared], false);
        engine
            .run("memory_user::create(\"note\", \"Titre utilisateur\", \"plage soleil\");")
            .expect("the user memory is created");
        engine
            .run("memory_shared::create(\"note\", \"Titre partage\", \"plage soleil\");")
            .expect("the shared memory is created");
        let found = hits(&engine, "memory_user::search(\"plage soleil\", 3)");
        assert_eq!(found.len(), 1, "only the user memory is in the user scope");
        assert_eq!(
            text_of(&found[0], "title"),
            "Titre utilisateur",
            "the shared memory does not come"
        );
    }

    #[test]
    fn search_of_a_level_without_index_is_an_explicit_error() {
        let store = Arc::new(InMemoryMemoryStore::new());
        let scopes = scopes(Vec::new());
        let engine = engine(&store, &scopes, &[MemoryLevel::Shared], false);
        let text = refusal(&engine, "memory_shared::search(\"x\", 3)");
        assert!(
            text.ends_with("no semantic index is provided for memory level `shared`"),
            "the refusal is the typed no-index error of the level, behind the Rhai prefix: {text}"
        );
    }

    #[test]
    fn search_after_an_update_sees_the_store_title_and_the_host_reindex_decides_the_rank() {
        let store = Arc::new(InMemoryMemoryStore::new());
        let before = host_index(&[
            memory("note", "Titre ancien", "chat chien"),
            memory("autre", "Lecture", "roman policier"),
        ]);
        let scopes_before = scopes(vec![(MemoryLevel::User, before)]);
        let engine_before = engine(&store, &scopes_before, &[MemoryLevel::User], false);
        engine_before
            .run("memory_user::create(\"note\", \"Titre ancien\", \"chat chien\");")
            .expect("the memory is created");
        engine_before
            .run("memory_user::create(\"autre\", \"Lecture\", \"roman policier\");")
            .expect("the other memory is created");
        engine_before
            .run("memory_user::update(\"note\", \"Titre neuf\", \"bateau voile\");")
            .expect("the memory is updated without reindexing");
        let stale = hits(&engine_before, "memory_user::search(\"bateau voile\", 2)");
        let stale_note = stale
            .iter()
            .find(|hit| text_of(hit, "id") == "note")
            .expect("the index still holds the note");
        assert_eq!(
            text_of(stale_note, "title"),
            "Titre neuf",
            "the title is read from the store"
        );
        let stale_score = stale_note
            .get("score")
            .and_then(|value| value.as_float().ok())
            .expect("the hit carries its score");

        let after = host_index(&[
            memory("note", "Titre neuf", "bateau voile"),
            memory("autre", "Lecture", "roman policier"),
        ]);
        let scopes_after = scopes(vec![(MemoryLevel::User, after)]);
        let engine_after = engine(&store, &scopes_after, &[MemoryLevel::User], false);
        let reindexed = hits(&engine_after, "memory_user::search(\"bateau voile\", 2)");
        let first = reindexed.first().expect("the reindexed scope holds two memories");
        assert_eq!(
            text_of(first, "id"),
            "note",
            "after the host reindexes, the note comes first"
        );
        assert_eq!(text_of(first, "title"), "Titre neuf");
        let reindexed_score = first
            .get("score")
            .and_then(|value| value.as_float().ok())
            .expect("the hit carries its score");
        assert!(
            reindexed_score > stale_score,
            "the reindex brings the note closer: {reindexed_score} {stale_score}"
        );
    }

    #[test]
    fn identifier_deleted_from_the_store_is_ignored_without_error() {
        let store = Arc::new(InMemoryMemoryStore::new());
        let index = host_index(&[
            memory("note", "T", "plage soleil"),
            memory("autre", "U", "pain lait"),
        ]);
        let scopes = scopes(vec![(MemoryLevel::User, index)]);
        let engine = engine(&store, &scopes, &[MemoryLevel::User], false);
        engine
            .run("memory_user::create(\"note\", \"T\", \"plage soleil\");")
            .expect("the memory is created");
        engine
            .run("memory_user::create(\"autre\", \"U\", \"pain lait\");")
            .expect("the other memory is created");
        engine
            .run("memory_user::delete(\"note\");")
            .expect("the memory is deleted");
        let found = hits(&engine, "memory_user::search(\"plage soleil\", 2)");
        assert!(
            found.iter().all(|hit| text_of(hit, "id") != "note"),
            "the deleted identifier is ignored"
        );
        assert_eq!(found.len(), 1, "the count drops below the limit");
    }

    #[test]
    fn invalid_parameters_name_the_faulty_one() {
        let store = Arc::new(InMemoryMemoryStore::new());
        let index = host_index(&[memory("note", "T", "plage soleil")]);
        let scopes = scopes(vec![(MemoryLevel::User, index)]);
        let engine = engine(&store, &scopes, &[MemoryLevel::User], false);
        let blank = refusal(&engine, "memory_user::search(\"   \", 3)");
        assert!(blank.contains("query"), "the blank query is named: {blank}");
        let zero = refusal(&engine, "memory_user::search(\"x\", 0)");
        assert!(zero.contains("limit"), "the zero limit is named: {zero}");
        let negative = refusal(&engine, "memory_user::search(\"x\", -1)");
        assert!(
            negative.contains("limit"),
            "the negative limit is named: {negative}"
        );
    }

    #[test]
    fn read_only_level_searches_and_has_no_writing_function() {
        let store = Arc::new(InMemoryMemoryStore::new());
        let index = host_index(&[memory("note", "T", "plage soleil")]);
        let scopes = scopes(vec![(MemoryLevel::User, index)]);
        let writable = engine(&store, &scopes, &[MemoryLevel::User], false);
        writable
            .run("memory_user::create(\"note\", \"T\", \"plage soleil\");")
            .expect("the memory is created");
        let read_only = engine(&store, &scopes, &[MemoryLevel::User], true);
        let found = hits(&read_only, "memory_user::search(\"plage soleil\", 1)");
        assert_eq!(found.len(), 1, "the read-only level still searches");
        let created = read_only.run("memory_user::create(\"k\", \"t\", \"c\");");
        assert!(created.is_err(), "create is absent from the read-only level");
    }
}

/// Without the feature `embedder`, the modules of Memory have no `search` function (`agent-sessions/modules#FR-14`).
#[cfg(test)]
#[cfg(not(feature = "embedder"))]
mod without_feature_tests {
    use std::sync::Arc;

    use rhai::Engine as RhaiEngine;

    use super::{MemoryScopes, memory_module, memory_module_name};
    use crate::clock::{Clock, ManualClock, Timestamp};
    use crate::modules::{InMemoryMemoryStore, MemoryLevel};
    use crate::session::{EventSink, NullSink, SessionId};

    #[test]
    fn memory_module_has_no_search_function() {
        let store = Arc::new(InMemoryMemoryStore::new());
        let session = SessionId::new("s-1").expect("a non-empty identifier");
        let scopes = MemoryScopes::new("alice", "alpha", &session);
        let sink: Arc<dyn EventSink> = Arc::new(NullSink);
        let clock: Arc<dyn Clock> = Arc::new(ManualClock::new(Timestamp::from_millis(1_000)));
        let module = memory_module(MemoryLevel::User, store, scopes, sink, clock);
        let mut engine = RhaiEngine::new();
        let _registered =
            engine.register_static_module(memory_module_name(MemoryLevel::User), Arc::new(module));
        let outcome = engine.run("memory_user::search(\"x\", 3);");
        assert!(
            outcome.is_err(),
            "the function does not exist without the feature"
        );
    }
}
