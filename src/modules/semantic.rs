//! Semantic search of Memory and skills (`agent-sessions/modules#FR-14`, `FR-15`, `FR-16`, `AC-10`, `AC-11`, `AC-12`,
//! `agent-sessions#DEC-15`): the index of `rig` that the host builds and maintains, adapted so that its associated type does
//! not leak, and the texts that the host embeds for a Memory and for a skill. The library stores no vector and reindexes
//! nothing (`agent-sessions#CON-1`). See `src/modules/semantic.sdd`.

use std::fmt;
use std::num::NonZeroUsize;
use std::pin::Pin;
use std::sync::Arc;

use rig_core::vector_store::request::Filter;
use rig_core::vector_store::{VectorSearchRequest, VectorStoreError, VectorStoreIndex};

use super::{Memory, ModulesError, ModulesResult};

/// One hit of a semantic search: the identifier the index holds (the identifier of a Memory, the name of a skill) and the
/// score the index gives it. A higher score is a closer match.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct SemanticHit {
    id: String,
    score: f64,
}

impl SemanticHit {
    /// Returns the identifier the index holds.
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Returns the score the index gives the hit, which grows with the similarity.
    #[must_use]
    pub const fn score(&self) -> f64 {
        self.score
    }
}

/// Object-safe face of a `rig` index: the associated filter type is fixed to the default one, and the future is boxed.
/// `VectorStoreIndex` has an associated type and returns `impl Future`, so it cannot be a trait object.
trait Searcher: Send + Sync {
    /// The identifiers and scores of the `samples` closest documents to `query`, as `top_n_ids` of the index gives them.
    fn top_n_ids<'call>(&'call self, query: &'call str, samples: u64) -> SearchFuture<'call>;
}

/// The boxed future of a search of the index, which is `Send` so that the caller may await it on any executor.
type SearchFuture<'call> =
    Pin<Box<dyn Future<Output = Result<Vec<(f64, String)>, VectorStoreError>> + Send + 'call>>;

/// The adapter of one index of `rig` to [`Searcher`].
struct Adapter<I> {
    index: I,
}

impl<I> Searcher for Adapter<I>
where
    I: VectorStoreIndex<Filter = Filter<serde_json::Value>> + 'static,
{
    fn top_n_ids<'call>(&'call self, query: &'call str, samples: u64) -> SearchFuture<'call> {
        Box::pin(async move {
            let request = VectorSearchRequest::builder()
                .query(query.to_owned())
                .samples(samples)
                .build();
            self.index.top_n_ids(request).await
        })
    }
}

/// The semantic index of one scope of Memory, or of the skills, that the host provides.
///
/// It wraps an index of `rig` built by the host with the embedding model of its choice, and queries it by identifiers
/// only: the library imposes no document type on the index. The library neither builds nor updates the index. After a
/// `create`, `update` or `delete` of a Memory, a search reflects the change only once the host has reindexed it; the
/// `MemoryChanged` event of the session is what tells the host (`agent-sessions#DEC-15`).
///
/// The index is shared: cloning it shares the same index. Its `Debug` output shows nothing of the index.
#[derive(Clone)]
pub struct SemanticIndex {
    searcher: Arc<dyn Searcher>,
}

impl SemanticIndex {
    /// Wraps an index of `rig` whose filter is the default one. The index is kept as it is: the library never writes to it.
    #[must_use]
    pub fn new<I>(index: I) -> Self
    where
        I: VectorStoreIndex<Filter = Filter<serde_json::Value>> + 'static,
    {
        Self {
            searcher: Arc::new(Adapter { index }),
        }
    }

    /// Returns the hits closest to `query`, at most `limit`, in decreasing order of relevance. An identifier that the index
    /// holds is returned as it is; the caller decides whether it still exists.
    ///
    /// # Errors
    ///
    /// Returns [`ModulesError::Index`] when the index fails. The message is the one of the index, without the query.
    pub async fn search(&self, query: &str, limit: NonZeroUsize) -> ModulesResult<Vec<SemanticHit>> {
        let samples = u64::try_from(limit.get()).unwrap_or(u64::MAX);
        let found = self
            .searcher
            .top_n_ids(query, samples)
            .await
            .map_err(|error| ModulesError::Index(error.to_string()))?;
        Ok(found
            .into_iter()
            .map(|(score, id)| SemanticHit { id, score })
            .collect())
    }
}

impl fmt::Debug for SemanticIndex {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SemanticIndex").finish_non_exhaustive()
    }
}

/// Returns the text that the host embeds for a Memory: its title, then its body, not truncated. The character limit of the
/// embedder applies at indexing, on the host side (`agent-sessions/agents#FR-14`).
#[must_use]
pub fn memory_embedding_text(memory: &Memory) -> String {
    format!("{}\n{}", memory.title(), memory.body())
}

/// Returns the text that the host embeds for a skill: its name, its description, then its body, not truncated.
#[must_use]
pub fn skill_embedding_text(name: &str, description: &str, body: &str) -> String {
    format!("{name}\n{description}\n{body}")
}

/// Helpers that build, for the tests of this crate, the index of `rig` that a host builds over the embedding texts.
#[cfg(test)]
#[expect(
    clippy::pub_with_shorthand,
    reason = "rustfmt rewrites pub(in crate) into pub(crate), which this lint refuses: the test helper is shared by the test suites of modules and toolsets"
)]
pub(crate) mod test_support {
    use std::num::NonZeroUsize;
    use std::sync::Arc;

    use rig_core::embeddings::Embedding;
    use rig_core::vector_store::in_memory_store::InMemoryVectorStore;

    use super::SemanticIndex;
    use crate::agents::{Embedder as _, EmbedderModel, TestEmbedder};

    /// Builds the index of `rig` that a host builds over the embedding texts: one `(identifier, text)` pair per document,
    /// embedded by the test embedder. The identifier is what a search returns.
    #[expect(
        clippy::pub_with_shorthand,
        reason = "rustfmt rewrites pub(in crate) into pub(crate), which this lint refuses: the helper must be reachable from the test suite of toolsets"
    )]
    pub(crate) async fn index_of(documents: &[(&str, &str)]) -> SemanticIndex {
        let test_embedder = Arc::new(TestEmbedder::new(
            NonZeroUsize::new(64).expect("a test size is not zero"),
            NonZeroUsize::new(100).expect("a test size is not zero"),
        ));
        let texts: Vec<String> = documents.iter().map(|(_id, text)| (*text).to_owned()).collect();
        let embedded = test_embedder
            .embed(&texts)
            .await
            .expect("the test embedder does not fail");
        let store = InMemoryVectorStore::from_documents_with_ids(documents.iter().zip(embedded).map(
            |((id, text), one)| {
                let embedding = Embedding {
                    document: (*text).to_owned(),
                    vec: one.into_vector(),
                };
                ((*id).to_owned(), (*text).to_owned(), vec![embedding])
            },
        ));
        SemanticIndex::new(store.index(EmbedderModel::new(test_embedder)))
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroUsize;

    use rig_core::vector_store::request::Filter;
    use rig_core::vector_store::{VectorStoreError, VectorStoreIndex};
    use serde::de::DeserializeOwned;

    use super::test_support::index_of;
    use super::{SemanticIndex, VectorSearchRequest, memory_embedding_text, skill_embedding_text};
    use crate::modules::{Memory, ModulesError};

    fn size(value: usize) -> NonZeroUsize {
        NonZeroUsize::new(value).expect("a test size is not zero")
    }

    /// An index whose every query fails with a message of its own.
    struct Failing;

    impl VectorStoreIndex for Failing {
        type Filter = Filter<serde_json::Value>;

        fn top_n<T: DeserializeOwned + Send>(
            &self,
            _request: VectorSearchRequest<Self::Filter>,
        ) -> impl Future<Output = Result<Vec<(f64, String, T)>, VectorStoreError>> + Send {
            std::future::ready(Err(VectorStoreError::DatastoreError(
                "the backend is down".into(),
            )))
        }

        fn top_n_ids(
            &self,
            _request: VectorSearchRequest<Self::Filter>,
        ) -> impl Future<Output = Result<Vec<(f64, String)>, VectorStoreError>> + Send {
            std::future::ready(Err(VectorStoreError::DatastoreError(
                "the backend is down".into(),
            )))
        }
    }

    #[tokio::test]
    async fn search_ranks_the_close_document_first() {
        let index = index_of(&[("a", "alpha beta"), ("b", "gamma delta"), ("c", "omega zeta")]).await;
        let hits = index
            .search("alpha", size(2))
            .await
            .expect("the index answers the query");
        let best = hits.first().expect("the index holds three documents");
        assert_eq!(best.id(), "a", "the close document comes first");
        assert!(best.score() > 0.0, "the hit carries its score");
    }

    #[tokio::test]
    async fn search_returns_at_most_the_limit() {
        let index = index_of(&[("a", "alpha beta"), ("b", "gamma delta"), ("c", "omega zeta")]).await;
        let hits = index
            .search("alpha", size(1))
            .await
            .expect("the index answers the query");
        assert_eq!(hits.len(), 1, "one hit at most when the limit is one");
    }

    #[tokio::test]
    async fn debug_output_shows_nothing_of_the_index() {
        let index = index_of(&[("a", "alpha beta")]).await;
        let shown = format!("{index:?}");
        assert!(
            !shown.contains("alpha"),
            "no text of the index in the debug output: {shown}"
        );
    }

    #[tokio::test]
    async fn index_failure_carries_its_message_without_the_query() {
        let index = SemanticIndex::new(Failing);
        let error = index
            .search("secret query", size(3))
            .await
            .expect_err("the index is down");
        assert!(matches!(error, ModulesError::Index(_)), "{error:?}");
        let shown = error.to_string();
        assert!(shown.contains("the backend is down"), "{shown}");
        assert!(!shown.contains("secret query"), "{shown}");
    }

    #[test]
    fn memory_text_is_the_title_then_the_body() {
        let memory = Memory::new("note", "Titre de la note", "Corps de la note").expect("a valid memory");
        let text = memory_embedding_text(&memory);
        let title = text.find("Titre de la note");
        let body = text.find("Corps de la note");
        assert!(title.is_some(), "the title is embedded: {text}");
        assert!(body.is_some(), "the body is embedded: {text}");
        assert!(title < body, "the title comes before the body: {text}");
    }

    #[test]
    fn memory_text_is_not_truncated() {
        let body = "mot ".repeat(2_000);
        let memory = Memory::new("long", "Long", &body).expect("a valid memory");
        assert!(
            memory_embedding_text(&memory).ends_with(&body),
            "the whole body is kept"
        );
    }

    #[test]
    fn skill_text_is_the_name_the_description_then_the_body() {
        let text = skill_embedding_text("deploy", "Deploy to prod", "Run the rollout");
        let name = text.find("deploy");
        let description = text.find("Deploy to prod");
        let body = text.find("Run the rollout");
        assert!(
            name.is_some() && description.is_some() && body.is_some(),
            "all three are embedded: {text}"
        );
        assert!(
            name < description && description < body,
            "in the order name, description, body: {text}"
        );
    }
}

/// End-to-end checks of the Memory side of the batch (`agent-sessions/modules#AC-10`, `AC-12`): three levels, each with its
/// own index, an absent index, an explicit reindex, and two sessions that do not see each other.
#[cfg(test)]
mod end_to_end {
    use std::sync::Arc;

    use rhai::{Array, Dynamic, Engine as RhaiEngine, Map};

    use serde_json::{Value, json};

    use super::test_support::index_of;
    use crate::agents::AgentKind;
    use crate::clock::{Clock, ManualClock, Timestamp};
    use crate::modules::{
        InMemoryMemoryStore, Memory, MemoryAccess, MemoryLevel, MemoryScopes, NativeModule, SemanticIndex,
        memory_embedding_text, memory_module, memory_module_name, native_modules, skill_embedding_text,
    };
    use crate::scratchpad::ScratchpadLimits;
    use crate::session::{CancelHandle, EventSink, NativeTool, NullSink, Redactor, SessionId};
    use crate::toolsets::{
        ContextLevel, SessionMeta, Skill, SkillTool, ToolsetContext, ToolsetsError, builtin_rhai_pad_skill,
    };

    /// A current-thread runtime for building the test indexes, which are asynchronous.
    fn runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("a current-thread runtime builds")
    }

    /// The index a host builds over `(identifier, title, body)` triples, with their embedding texts.
    fn host_index(memories: &[(&str, &str, &str)]) -> SemanticIndex {
        let texts: Vec<(String, String)> = memories
            .iter()
            .map(|(id, title, body)| {
                let memory = Memory::new(id, title, body).expect("a valid test memory");
                ((*id).to_owned(), memory_embedding_text(&memory))
            })
            .collect();
        let borrowed: Vec<(&str, &str)> = texts
            .iter()
            .map(|(id, text)| (id.as_str(), text.as_str()))
            .collect();
        runtime().block_on(index_of(&borrowed))
    }

    /// A Rhai engine with the write-access module of each given level, over `store`, for the given scopes.
    fn engine(store: &Arc<InMemoryMemoryStore>, scopes: &MemoryScopes, levels: &[MemoryLevel]) -> RhaiEngine {
        let sink: Arc<dyn EventSink> = Arc::new(NullSink);
        let clock: Arc<dyn Clock> = Arc::new(ManualClock::new(Timestamp::from_millis(1_000)));
        let mut engine = RhaiEngine::new();
        for level in levels {
            let module = memory_module(
                *level,
                Arc::clone(store),
                scopes.clone(),
                Arc::clone(&sink),
                Arc::clone(&clock),
            );
            let _registered = engine.register_static_module(memory_module_name(*level), Arc::new(module));
        }
        engine
    }

    /// The hits of a search script. The script must succeed.
    fn hits(engine: &RhaiEngine, script: &str) -> Vec<Map> {
        let array: Array = engine
            .eval(script)
            .unwrap_or_else(|error| panic!("the search must succeed: {error}"));
        array
            .into_iter()
            .map(|item| item.try_cast::<Map>().expect("each hit is a map"))
            .collect()
    }

    /// The text of a field of a hit, empty when it is missing.
    fn text_of(hit: &Map, key: &str) -> String {
        hit.get(key)
            .and_then(|value: &Dynamic| value.clone().into_string().ok())
            .unwrap_or_default()
    }

    /// The scopes of one session with the given indexes per level.
    fn scopes_of(session: &str, indexes: Vec<(MemoryLevel, SemanticIndex)>) -> MemoryScopes {
        let session = SessionId::new(session).expect("a non-empty identifier");
        indexes.into_iter().fold(
            MemoryScopes::new("alice", "alpha", &session),
            |scopes, (level, index)| scopes.with_index(level, index),
        )
    }

    #[test]
    fn three_levels_each_search_their_own_index() {
        let store = Arc::new(InMemoryMemoryStore::new());
        let scopes = scopes_of(
            "s-1",
            vec![
                (
                    MemoryLevel::User,
                    host_index(&[("note", "Titre utilisateur", "plage soleil")]),
                ),
                (
                    MemoryLevel::Shared,
                    host_index(&[("note", "Titre partage", "pain lait")]),
                ),
                (
                    MemoryLevel::Session,
                    host_index(&[("note", "Titre session", "projet budget")]),
                ),
            ],
        );
        let engine = engine(
            &store,
            &scopes,
            &[MemoryLevel::User, MemoryLevel::Shared, MemoryLevel::Session],
        );
        for script in [
            "memory_user::create(\"note\", \"Titre utilisateur\", \"plage soleil\");",
            "memory_shared::create(\"note\", \"Titre partage\", \"pain lait\");",
            "memory_session::create(\"note\", \"Titre session\", \"projet budget\");",
        ] {
            engine.run(script).expect("the memory is created");
        }
        for (script, title) in [
            ("memory_user::search(\"plage soleil\", 3)", "Titre utilisateur"),
            ("memory_shared::search(\"pain lait\", 3)", "Titre partage"),
            ("memory_session::search(\"projet budget\", 3)", "Titre session"),
        ] {
            let found = hits(&engine, script);
            assert_eq!(found.len(), 1, "one memory per level, by {script}");
            assert_eq!(text_of(&found[0], "id"), "note");
            assert_eq!(
                text_of(&found[0], "title"),
                title,
                "the level answers its own memory: {script}"
            );
        }
    }

    #[test]
    fn a_level_without_index_refuses_the_search_explicitly() {
        let store = Arc::new(InMemoryMemoryStore::new());
        let scopes = scopes_of(
            "s-1",
            vec![(MemoryLevel::User, host_index(&[("note", "T", "plage soleil")]))],
        );
        let engine = engine(&store, &scopes, &[MemoryLevel::Shared]);
        let error = engine
            .eval::<Array>("memory_shared::search(\"x\", 3)")
            .expect_err("the shared level has no index");
        let text = error.to_string();
        assert!(text.contains("shared") && text.contains("index"), "{text}");
    }

    #[test]
    fn the_host_reindexes_explicitly_and_the_library_does_not() {
        let store = Arc::new(InMemoryMemoryStore::new());
        let before = scopes_of(
            "s-1",
            vec![(
                MemoryLevel::Shared,
                host_index(&[
                    ("note", "Titre", "pain lait"),
                    ("autre", "Lecture", "roman policier"),
                ]),
            )],
        );
        let engine_before = engine(&store, &before, &[MemoryLevel::Shared]);
        engine_before
            .run("memory_shared::create(\"note\", \"Titre\", \"pain lait\");")
            .expect("the memory is created");
        engine_before
            .run("memory_shared::create(\"autre\", \"Lecture\", \"roman policier\");")
            .expect("the other memory is created");
        engine_before
            .run("memory_shared::update(\"note\", \"Titre\", \"projet budget\");")
            .expect("the memory is updated without reindexing");
        let stale = hits(&engine_before, "memory_shared::search(\"projet budget\", 1)");
        assert_ne!(
            text_of(&stale[0], "id"),
            "note",
            "the index does not hold the new text yet"
        );

        let after = scopes_of(
            "s-1",
            vec![(
                MemoryLevel::Shared,
                host_index(&[
                    ("note", "Titre", "projet budget"),
                    ("autre", "Lecture", "roman policier"),
                ]),
            )],
        );
        let engine_after = engine(&store, &after, &[MemoryLevel::Shared]);
        let reindexed = hits(&engine_after, "memory_shared::search(\"projet budget\", 1)");
        assert_eq!(
            text_of(&reindexed[0], "id"),
            "note",
            "the reindexed memory is found"
        );
        assert_eq!(text_of(&reindexed[0], "title"), "Titre");
    }

    #[test]
    fn two_sessions_do_not_see_each_other_at_session_level() {
        let store = Arc::new(InMemoryMemoryStore::new());
        let first = scopes_of(
            "s-1",
            vec![(
                MemoryLevel::Session,
                host_index(&[("note", "Un", "plage soleil")]),
            )],
        );
        let second = scopes_of(
            "s-2",
            vec![(MemoryLevel::Session, host_index(&[("note", "Deux", "pain lait")]))],
        );
        let engine_first = engine(&store, &first, &[MemoryLevel::Session]);
        let engine_second = engine(&store, &second, &[MemoryLevel::Session]);
        engine_first
            .run("memory_session::create(\"note\", \"Un\", \"plage soleil\");")
            .expect("the first session creates its memory");
        engine_second
            .run("memory_session::create(\"note\", \"Deux\", \"pain lait\");")
            .expect("the second session creates its memory");
        let found = hits(&engine_first, "memory_session::search(\"pain lait\", 3)");
        assert!(
            found.iter().all(|hit| text_of(hit, "title") != "Deux"),
            "the other session is not seen"
        );
    }

    /// The `skill` tool over the given skills, with the given semantic index when there is one.
    fn skill_tool(presented: Vec<Skill>, index: Option<SemanticIndex>) -> SkillTool {
        let entries = presented
            .into_iter()
            .map(|skill| ("ops".to_owned(), skill))
            .collect();
        let builtin = builtin_rhai_pad_skill(&ScratchpadLimits::new()).expect("the built-in skill builds");
        let context = ToolsetContext::new("site".to_owned(), ContextLevel::Shared)
            .expect("a context name")
            .resolve(Vec::<(String, String, Value)>::new())
            .expect("the context resolves");
        let session = SessionMeta::new(
            "main".to_owned(),
            "2026-10-30T09:00:00Z".to_owned(),
            Vec::new(),
            Vec::new(),
            None,
        );
        let tool = SkillTool::new(
            entries,
            builtin,
            context,
            session,
            8_000,
            Arc::new(Redactor::new()),
        )
        .expect("the tool builds");
        match index {
            Some(index) => tool.with_semantic_index(index),
            None => tool,
        }
    }

    /// A skill built for the test.
    fn skill(name: &str, description: &str, body: &str) -> Skill {
        Skill::new(name.to_owned(), description.to_owned(), body.to_owned()).expect("a valid skill")
    }

    /// The index a host builds over `(name, description, body)` skills, held under the skill names.
    fn skill_index(skills: &[(&str, &str, &str)]) -> SemanticIndex {
        let texts: Vec<(String, String)> = skills
            .iter()
            .map(|(name, description, body)| {
                ((*name).to_owned(), skill_embedding_text(name, description, body))
            })
            .collect();
        let borrowed: Vec<(&str, &str)> = texts
            .iter()
            .map(|(name, text)| (name.as_str(), text.as_str()))
            .collect();
        runtime().block_on(index_of(&borrowed))
    }

    /// The answer of the `skill` tool to a JSON call.
    fn ask(tool: &SkillTool, arguments: &Value) -> Result<String, String> {
        let native: &dyn NativeTool = tool;
        runtime().block_on(native.call(arguments, &CancelHandle::new()))
    }

    #[test]
    fn title_comes_from_the_store_before_and_after_the_reindex() {
        let store = Arc::new(InMemoryMemoryStore::new());
        let before = scopes_of(
            "s-1",
            vec![(
                MemoryLevel::User,
                host_index(&[
                    ("note", "Titre ancien", "chat chien"),
                    ("autre", "Autre", "roman policier"),
                ]),
            )],
        );
        let engine_before = engine(&store, &before, &[MemoryLevel::User]);
        engine_before
            .run("memory_user::create(\"note\", \"Titre ancien\", \"chat chien\");")
            .expect("the memory is created");
        engine_before
            .run("memory_user::create(\"autre\", \"Autre\", \"roman policier\");")
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
            "the title is the stored one"
        );

        let after = scopes_of(
            "s-1",
            vec![(
                MemoryLevel::User,
                host_index(&[
                    ("note", "Titre neuf", "bateau voile"),
                    ("autre", "Autre", "roman policier"),
                ]),
            )],
        );
        let engine_after = engine(&store, &after, &[MemoryLevel::User]);
        let reindexed = hits(&engine_after, "memory_user::search(\"bateau voile\", 2)");
        assert_eq!(
            text_of(&reindexed[0], "id"),
            "note",
            "the reindex brings the note first"
        );
        assert_eq!(text_of(&reindexed[0], "title"), "Titre neuf");
    }

    #[test]
    fn skill_search_reads_the_close_skill_then_its_body_and_ignores_an_unknown_identifier() {
        let deploy = skill_embedding_text("deploy", "Deploy to prod", "Run the rollout");
        let index = runtime().block_on(index_of(&[
            ("ghost", "Deploy to prod"),
            ("deploy", deploy.as_str()),
        ]));
        let tool = skill_tool(
            vec![skill("deploy", "Deploy to prod", "Run the rollout")],
            Some(index),
        );
        let found = ask(&tool, &json!({"query": "Deploy to prod"})).expect("the search answers");
        assert_eq!(
            found.lines().count(),
            1,
            "the identifier unknown to the table does not count: {found}"
        );
        assert!(found.starts_with("- deploy"), "{found}");
        assert!(
            !found.contains("Run the rollout"),
            "no body in a search result: {found}"
        );
        assert!(
            !found.contains("ghost"),
            "the unknown identifier is ignored: {found}"
        );
        assert_eq!(
            ask(&tool, &json!({"name": "deploy"})).as_deref(),
            Ok("Run the rollout")
        );
    }

    #[test]
    fn skill_search_without_index_is_the_no_index_error() {
        let tool = skill_tool(vec![skill("deploy", "Deploy to prod", "Run the rollout")], None);
        let error = runtime()
            .block_on(tool.search("Deploy to prod", std::num::NonZeroUsize::MIN))
            .expect_err("no index is provided");
        assert!(matches!(error, ToolsetsError::NoIndex), "{error:?}");
    }

    #[test]
    fn skill_index_is_replaced_explicitly_by_the_host() {
        let presented = vec![
            skill("deploy", "Deploy to prod", "Run the rollout"),
            skill("backup", "Save the database", "dump the tables"),
        ];
        let reindexed = skill_index(&[
            ("deploy", "Ship the release to staging", "Run the rollout"),
            ("backup", "Save the database", "dump the tables"),
        ]);
        let tool = skill_tool(presented, Some(reindexed));
        let found = ask(
            &tool,
            &json!({"query": "Ship the release to staging", "limit": 1}),
        )
        .expect("the search answers");
        assert!(
            found.starts_with("- deploy"),
            "the reindexed skill comes first: {found}"
        );
    }

    #[test]
    fn a_level_the_session_nature_does_not_give_has_no_module() {
        let without = native_modules(AgentKind::Work, false, false);
        assert!(
            !without
                .iter()
                .any(|module| matches!(module, NativeModule::MemoryShared)),
            "no shared level, no shared module, so no search function"
        );
        assert!(
            without
                .iter()
                .any(|module| matches!(module, NativeModule::MemoryUser(MemoryAccess::Read))),
            "the user level is read-only for a work agent"
        );
        let with = native_modules(AgentKind::Work, false, true);
        assert!(
            with.iter()
                .any(|module| matches!(module, NativeModule::MemoryShared))
        );
    }

    #[test]
    fn memory_and_skill_searches_return_only_their_own_kind() {
        let store = Arc::new(InMemoryMemoryStore::new());
        let memories = scopes_of(
            "s-1",
            vec![(
                MemoryLevel::User,
                host_index(&[("a", "Vacances", "plage soleil")]),
            )],
        );
        let engine = engine(&store, &memories, &[MemoryLevel::User]);
        engine
            .run("memory_user::create(\"a\", \"Vacances\", \"plage soleil\");")
            .expect("the memory is created");
        let tool = skill_tool(
            vec![skill("deploy", "Deploy to prod", "Run the rollout")],
            Some(skill_index(&[("deploy", "Deploy to prod", "Run the rollout")])),
        );
        let memory_hits = hits(&engine, "memory_user::search(\"plage soleil\", 3)");
        assert_eq!(memory_hits.len(), 1);
        assert_eq!(text_of(&memory_hits[0], "id"), "a");
        let skill_hits = ask(&tool, &json!({"query": "Deploy to prod"})).expect("the search answers");
        assert!(
            skill_hits.starts_with("- deploy") && !skill_hits.contains("Vacances"),
            "{skill_hits}"
        );
    }
}
