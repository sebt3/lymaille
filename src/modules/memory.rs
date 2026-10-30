//! Memory of the session: a Memory (identifier, title, body) kept at one of three levels, user, project or session, and the
//! storage trait that the host implements, with an in-memory store for tests and demonstration hosts. The library performs
//! no I/O (`agent-sessions/modules#FR-2`, `agent-sessions/modules#FR-3`, `agent-sessions#DEC-6`, `agent-sessions#CON-1`).

use std::collections::BTreeMap;
use std::fmt;
use std::sync::{Mutex, MutexGuard};

use serde::{Deserialize, Serialize};

use super::{ModulesError, ModulesResult};
use crate::session::SessionId;

/// Level of a Memory: user, project or session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[non_exhaustive]
pub enum MemoryLevel {
    /// Memory of the user, scoped by the reference that the host provides.
    User,
    /// Memory of the project, scoped by the reference that the host provides.
    Project,
    /// Memory of one session, scoped by the identifier of the session.
    Session,
}

impl MemoryLevel {
    /// Returns the text of the level: `user`, `project` or `session`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Project => "project",
            Self::Session => "session",
        }
    }
}

/// A Memory: an identifier unique within its scope, a title and a body.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
#[serde(try_from = "MemoryRecord")]
pub struct Memory {
    id: String,
    title: String,
    body: String,
}

/// The stored form of a Memory, read back before the checks of [`Memory::new`] run.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MemoryRecord {
    id: String,
    title: String,
    body: String,
}

impl TryFrom<MemoryRecord> for Memory {
    type Error = ModulesError;

    fn try_from(record: MemoryRecord) -> Result<Self, Self::Error> {
        Self::new(&record.id, &record.title, &record.body)
    }
}

impl Memory {
    /// Builds a Memory. The identifier must not be blank and must not contain a blank; the title must not be blank. The body
    /// is kept as given.
    ///
    /// # Errors
    ///
    /// Returns [`ModulesError::InvalidMemory`] when the identifier is blank or contains a blank, or when the title is blank.
    /// The level is left empty in this error: a Memory does not know its level before it is put in a scope.
    pub fn new(id: &str, title: &str, body: &str) -> ModulesResult<Self> {
        if id.trim().is_empty() {
            return Err(refused(id, "the identifier must not be blank"));
        }
        if id.chars().any(char::is_whitespace) {
            return Err(refused(id, "the identifier must not contain a blank"));
        }
        if title.trim().is_empty() {
            return Err(refused(id, "the title must not be blank"));
        }
        Ok(Self {
            id: id.to_owned(),
            title: title.to_owned(),
            body: body.to_owned(),
        })
    }

    /// Returns the identifier of the Memory.
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Returns the title of the Memory.
    #[must_use]
    pub fn title(&self) -> &str {
        &self.title
    }

    /// Returns the body of the Memory.
    #[must_use]
    pub fn body(&self) -> &str {
        &self.body
    }
}

/// The refusal of a Memory. The level is empty: a Memory does not know its level before it is put in a scope.
fn refused(id: &str, reason: &str) -> ModulesError {
    ModulesError::InvalidMemory {
        level: String::new(),
        id: id.to_owned(),
        reason: reason.to_owned(),
    }
}

impl fmt::Debug for Memory {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Memory")
            .field("id", &self.id)
            .field("title", &self.title)
            .field("body_chars", &self.body.chars().count())
            .finish()
    }
}

/// Reads a Memory from its stored JSON text.
fn decode(text: &str) -> ModulesResult<Memory> {
    serde_json::from_str(text).map_err(|error| ModulesError::Store(error.to_string()))
}

/// Writes a Memory as its JSON text.
fn encode(memory: &Memory) -> ModulesResult<String> {
    serde_json::to_string(memory).map_err(|error| ModulesError::Store(error.to_string()))
}

/// The key of a Memory: its scope, then its identifier.
fn key(scope: &MemoryScope, id: &str) -> (MemoryScope, String) {
    (scope.clone(), id.to_owned())
}

/// The summary of a Memory: its identifier and its title, never its body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[non_exhaustive]
pub struct MemorySummary {
    id: String,
    title: String,
}

impl MemorySummary {
    /// Returns the identifier of the Memory.
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Returns the title of the Memory.
    #[must_use]
    pub fn title(&self) -> &str {
        &self.title
    }
}

impl From<&Memory> for MemorySummary {
    fn from(memory: &Memory) -> Self {
        Self {
            id: memory.id.clone(),
            title: memory.title.clone(),
        }
    }
}

/// The scope of a Memory: its level and the reference of the level (the user or the project as the host names it, or the
/// identifier of the session).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
#[non_exhaustive]
pub struct MemoryScope {
    level: MemoryLevel,
    reference: String,
}

impl MemoryScope {
    /// Scope of the user, with the reference the host provides.
    #[must_use]
    pub fn user(reference: &str) -> Self {
        Self {
            level: MemoryLevel::User,
            reference: reference.to_owned(),
        }
    }

    /// Scope of the project, with the reference the host provides.
    #[must_use]
    pub fn project(reference: &str) -> Self {
        Self {
            level: MemoryLevel::Project,
            reference: reference.to_owned(),
        }
    }

    /// Scope of one session, qualified by its identifier.
    #[must_use]
    pub fn session(session: &SessionId) -> Self {
        Self {
            level: MemoryLevel::Session,
            reference: session.as_str().to_owned(),
        }
    }

    /// Returns the level of the scope.
    #[must_use]
    pub const fn level(&self) -> MemoryLevel {
        self.level
    }

    /// Returns the reference of the scope.
    #[must_use]
    pub fn reference(&self) -> &str {
        &self.reference
    }
}

/// Storage of Memories, implemented by the host. The key of a Memory is its scope, then its identifier.
///
/// The returned futures are `Send`. The trait is used through a generic parameter only, never as a trait object.
pub trait MemoryStore: Send + Sync {
    /// Lists the summaries of the scope, sorted by identifier, without bodies.
    ///
    /// # Errors
    ///
    /// Returns [`ModulesError::Store`] when the host storage fails, and [`ModulesError::Poisoned`] when an in-memory lock is
    /// poisoned.
    fn list(&self, scope: &MemoryScope) -> impl Future<Output = ModulesResult<Vec<MemorySummary>>> + Send;

    /// Reads the Memory of the scope that carries the identifier, or `None` when there is none.
    ///
    /// # Errors
    ///
    /// Returns [`ModulesError::Store`] when the host storage fails, and [`ModulesError::Poisoned`] when an in-memory lock is
    /// poisoned.
    fn get(
        &self,
        scope: &MemoryScope,
        id: &str,
    ) -> impl Future<Output = ModulesResult<Option<Memory>>> + Send;

    /// Stores the Memory in the scope, replacing the Memory of the same identifier in that scope.
    ///
    /// # Errors
    ///
    /// Returns [`ModulesError::Store`] when the host storage fails, and [`ModulesError::Poisoned`] when an in-memory lock is
    /// poisoned.
    fn put(&self, scope: &MemoryScope, memory: &Memory) -> impl Future<Output = ModulesResult<()>> + Send;

    /// Deletes the Memory of the scope that carries the identifier. Returns `true` when it existed.
    ///
    /// # Errors
    ///
    /// Returns [`ModulesError::Store`] when the host storage fails, and [`ModulesError::Poisoned`] when an in-memory lock is
    /// poisoned.
    fn delete(&self, scope: &MemoryScope, id: &str) -> impl Future<Output = ModulesResult<bool>> + Send;
}

/// Texts of the stored Memories, keyed by scope then identifier. The scope order keeps each scope sorted by identifier.
type Texts = BTreeMap<(MemoryScope, String), String>;

/// In-memory Memory store. It keeps the JSON text of each Memory, never the live object, and performs no I/O.
#[derive(Debug, Default)]
#[non_exhaustive]
pub struct InMemoryMemoryStore {
    texts: Mutex<Texts>,
}

impl InMemoryMemoryStore {
    /// Builds an empty store.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            texts: Mutex::new(BTreeMap::new()),
        }
    }

    /// Locks the stored texts. A poisoned lock is reported as [`ModulesError::Poisoned`].
    fn lock(&self) -> ModulesResult<MutexGuard<'_, Texts>> {
        self.texts.lock().map_err(|_poisoned| ModulesError::Poisoned)
    }

    /// Lists the summaries of a scope, in identifier order.
    fn summaries(&self, scope: &MemoryScope) -> ModulesResult<Vec<MemorySummary>> {
        self.lock()?
            .iter()
            .filter(|((owner, _id), _text)| owner == scope)
            .map(|(_key, text)| decode(text).map(|memory| MemorySummary::from(&memory)))
            .collect()
    }

    /// Reads the Memory of a scope and identifier.
    fn read(&self, scope: &MemoryScope, id: &str) -> ModulesResult<Option<Memory>> {
        let text = self.lock()?.get(&key(scope, id)).cloned();
        text.map(|text| decode(&text)).transpose()
    }

    /// Stores the text of a Memory in a scope, replacing the previous text.
    fn write(&self, scope: &MemoryScope, memory: &Memory) -> ModulesResult<()> {
        let text = encode(memory)?;
        let _replaced = self.lock()?.insert(key(scope, memory.id()), text);
        Ok(())
    }

    /// Removes the Memory of a scope and identifier. Returns `true` when it existed.
    fn remove(&self, scope: &MemoryScope, id: &str) -> ModulesResult<bool> {
        Ok(self.lock()?.remove(&key(scope, id)).is_some())
    }
}

impl MemoryStore for InMemoryMemoryStore {
    // The in-memory operations do not wait on anything: they complete at once, as a ready future.
    fn list(&self, scope: &MemoryScope) -> impl Future<Output = ModulesResult<Vec<MemorySummary>>> + Send {
        std::future::ready(self.summaries(scope))
    }

    fn get(
        &self,
        scope: &MemoryScope,
        id: &str,
    ) -> impl Future<Output = ModulesResult<Option<Memory>>> + Send {
        std::future::ready(self.read(scope, id))
    }

    fn put(&self, scope: &MemoryScope, memory: &Memory) -> impl Future<Output = ModulesResult<()>> + Send {
        std::future::ready(self.write(scope, memory))
    }

    fn delete(&self, scope: &MemoryScope, id: &str) -> impl Future<Output = ModulesResult<bool>> + Send {
        std::future::ready(self.remove(scope, id))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::thread;

    use serde_json::json;

    use super::{InMemoryMemoryStore, Memory, MemoryLevel, MemoryScope, MemoryStore as _, MemorySummary};
    use crate::modules::ModulesError;
    use crate::session::SessionId;

    fn memory(id: &str, title: &str, body: &str) -> Memory {
        Memory::new(id, title, body).expect("a valid memory")
    }

    fn session(text: &str) -> SessionId {
        SessionId::new(text).expect("a non-empty text makes a session identifier")
    }

    fn assert_send_sync_clone<T: Send + Sync + Clone>() {}

    // Scenario: même identifiant, portées indépendantes
    #[tokio::test]
    async fn same_identifier_in_two_scopes_stays_independent() {
        let store = InMemoryMemoryStore::new();
        let user = MemoryScope::user("alice");
        let project = MemoryScope::project("alpha");
        store
            .put(&user, &memory("note", "user note", "user body"))
            .await
            .expect("putting in the user scope succeeds");
        store
            .put(&project, &memory("note", "project note", "project body"))
            .await
            .expect("putting in the project scope succeeds");
        let from_user = store
            .get(&user, "note")
            .await
            .expect("reading the user scope succeeds")
            .expect("the user note exists");
        let from_project = store
            .get(&project, "note")
            .await
            .expect("reading the project scope succeeds")
            .expect("the project note exists");
        assert_eq!(
            from_user.title(),
            "user note",
            "the user scope keeps its own memory"
        );
        assert_eq!(
            from_project.title(),
            "project note",
            "the project scope keeps its own memory"
        );
    }

    // Scenario: la liste ne rend pas le corps
    #[tokio::test]
    async fn list_returns_summaries_sorted_without_body() {
        let store = InMemoryMemoryStore::new();
        let scope = MemoryScope::project("alpha");
        store
            .put(&scope, &memory("beta", "second", "secret body two"))
            .await
            .expect("putting succeeds");
        store
            .put(&scope, &memory("alpha", "first", "secret body one"))
            .await
            .expect("putting succeeds");
        let summaries = store.list(&scope).await.expect("listing succeeds");
        let ids: Vec<&str> = summaries.iter().map(MemorySummary::id).collect();
        assert_eq!(
            ids,
            vec!["alpha", "beta"],
            "the summaries are sorted by identifier"
        );
        assert_eq!(summaries.first().map(MemorySummary::title), Some("first"));
        let text = serde_json::to_string(&summaries).expect("summaries serialize");
        assert!(
            !text.contains("secret body"),
            "a summary must never carry a body: {text}"
        );
    }

    // Scenario: même identifiant, portées indépendantes
    #[tokio::test]
    async fn list_keeps_levels_with_the_same_reference_apart() {
        let store = InMemoryMemoryStore::new();
        store
            .put(&MemoryScope::user("x"), &memory("only-user", "u", "b"))
            .await
            .expect("putting succeeds");
        store
            .put(&MemoryScope::project("x"), &memory("only-project", "p", "b"))
            .await
            .expect("putting succeeds");
        let users = store
            .list(&MemoryScope::user("x"))
            .await
            .expect("listing succeeds");
        let ids: Vec<&str> = users.iter().map(MemorySummary::id).collect();
        assert_eq!(
            ids,
            vec!["only-user"],
            "the project level must not leak into the user level"
        );
    }

    // Scenario: remplacer une Memory
    #[tokio::test]
    async fn put_replaces_memory_with_same_identifier() {
        let store = InMemoryMemoryStore::new();
        let scope = MemoryScope::user("alice");
        store
            .put(&scope, &memory("note", "first version", "first body"))
            .await
            .expect("the first put succeeds");
        store
            .put(&scope, &memory("note", "second version", "second body"))
            .await
            .expect("the second put succeeds");
        let read = store
            .get(&scope, "note")
            .await
            .expect("reading succeeds")
            .expect("the memory exists");
        assert_eq!(read.title(), "second version", "the last put is read back");
        assert_eq!(read.body(), "second body");
        assert_eq!(
            store.list(&scope).await.expect("listing succeeds").len(),
            1,
            "no duplicate remains"
        );
    }

    // Scenario: supprimer
    #[tokio::test]
    async fn delete_twice_reports_true_then_false() {
        let store = InMemoryMemoryStore::new();
        let scope = MemoryScope::user("alice");
        store
            .put(&scope, &memory("note", "title", "body"))
            .await
            .expect("putting succeeds");
        assert!(
            store
                .delete(&scope, "note")
                .await
                .expect("the first delete succeeds")
        );
        assert!(
            !store
                .delete(&scope, "note")
                .await
                .expect("the second delete succeeds")
        );
        assert!(
            store
                .get(&scope, "note")
                .await
                .expect("reading succeeds")
                .is_none(),
            "reading after the delete returns nothing"
        );
    }

    // Scenario: la session qualifie la portée
    #[tokio::test]
    async fn session_scope_is_qualified_by_session_identifier() {
        let store = InMemoryMemoryStore::new();
        let first = MemoryScope::session(&session("s-1"));
        let second = MemoryScope::session(&session("s-2"));
        store
            .put(&first, &memory("note", "for s-1", "body one"))
            .await
            .expect("putting for the first session succeeds");
        store
            .put(&second, &memory("note", "for s-2", "body two"))
            .await
            .expect("putting for the second session succeeds");
        let read_first = store
            .get(&MemoryScope::session(&session("s-1")), "note")
            .await
            .expect("reading succeeds")
            .expect("the first session's memory exists");
        let read_second = store
            .get(&MemoryScope::session(&session("s-2")), "note")
            .await
            .expect("reading succeeds")
            .expect("the second session's memory exists");
        assert_eq!(read_first.title(), "for s-1", "each session reads its own memory");
        assert_eq!(
            read_second.title(),
            "for s-2",
            "each session reads its own memory"
        );
        assert_eq!(
            store.list(&first).await.expect("listing succeeds").len(),
            1,
            "a session lists only its own memories"
        );
    }

    // Scenario: Memory invalide refusée
    #[test]
    fn invalid_memory_with_empty_identifier_is_refused() {
        let result = Memory::new("", "title", "body");
        assert!(
            matches!(result, Err(ModulesError::InvalidMemory { .. })),
            "an empty identifier must give InvalidMemory, got {result:?}"
        );
    }

    // Scenario: Memory invalide refusée
    #[test]
    fn invalid_memory_with_blank_in_identifier_is_refused() {
        let result = Memory::new("two words", "title", "body");
        assert!(
            matches!(result, Err(ModulesError::InvalidMemory { .. })),
            "an identifier with a blank must give InvalidMemory, got {result:?}"
        );
    }

    // Scenario: Memory invalide refusée
    #[test]
    fn invalid_memory_with_tab_or_unicode_space_in_identifier_is_refused() {
        for id in ["a\tb", "a\u{00a0}b", "a\nb"] {
            let result = Memory::new(id, "title", "body");
            assert!(
                matches!(result, Err(ModulesError::InvalidMemory { .. })),
                "an identifier with a whitespace character must give InvalidMemory, got {result:?}"
            );
        }
    }

    // Scenario: Memory invalide refusée
    #[test]
    fn invalid_memory_with_blank_title_is_refused() {
        let result = Memory::new("note", "   ", "body");
        assert!(
            matches!(result, Err(ModulesError::InvalidMemory { .. })),
            "a blank title must give InvalidMemory, got {result:?}"
        );
    }

    #[test]
    fn invalid_memory_names_identifier_and_reason_not_body() {
        let error =
            Memory::new("two words", "title", "hunter2-secret").expect_err("the identifier has a blank");
        let text = error.to_string();
        assert!(text.contains("two words"), "the identifier is named: {text}");
        assert!(
            !text.contains("hunter2"),
            "the body is never in the message: {text}"
        );
    }

    #[test]
    fn stored_form_with_blank_identifier_is_refused_when_read() {
        let refused = serde_json::from_value::<Memory>(json!({"id": "", "title": "t", "body": "b"}));
        assert!(
            refused.is_err(),
            "a stored memory with an empty identifier is refused"
        );
    }

    // Scenario: le Debug cache le corps
    #[test]
    fn debug_hides_body() {
        let secret = memory("note", "public title", "hunter2-secret");
        let text = format!("{secret:?}");
        assert!(
            !text.contains("hunter2"),
            "the body must not appear in Debug: {text}"
        );
        assert!(text.contains("public title"), "the title may appear: {text}");
    }

    // Scenario: le Debug cache le corps
    #[test]
    fn debug_counts_characters_not_bytes() {
        let text = format!("{:?}", memory("note", "title", "\u{e9}\u{e9}\u{e9}"));
        assert!(text.contains('3'), "the body length is 3 characters: {text}");
        assert!(
            !text.contains('6'),
            "the body length must not be the byte count: {text}"
        );
    }

    // Scenario: le stockage garde la forme sérialisée
    #[tokio::test]
    async fn store_keeps_serialized_form() {
        let store = InMemoryMemoryStore::new();
        let scope = MemoryScope::user("alice");
        let mut original = memory("note", "title", "body");
        store.put(&scope, &original).await.expect("putting succeeds");
        original = memory("note", "changed", "changed body");
        let read = store
            .get(&scope, "note")
            .await
            .expect("reading succeeds")
            .expect("the memory exists");
        assert_eq!(read.title(), "title", "the state read is the state that was put");
        assert_eq!(read.body(), "body");
        let texts = store.texts.lock().expect("the lock is fresh");
        let stored = texts.values().next().expect("one text is stored");
        let value: serde_json::Value = serde_json::from_str(stored).expect("the stored text is JSON");
        assert_eq!(value, json!({"id": "note", "title": "title", "body": "body"}));
        drop(texts);
        assert_eq!(
            original.title(),
            "changed",
            "the object itself was rebound, not the store"
        );
    }

    #[test]
    fn memory_serializes_to_identifier_title_and_body() {
        let value = serde_json::to_value(memory("note", "title", "body")).expect("a memory serializes");
        assert_eq!(value, json!({"id": "note", "title": "title", "body": "body"}));
        let back: Memory = serde_json::from_value(value).expect("a memory reads back");
        assert_eq!(back.body(), "body");
    }

    #[test]
    fn level_has_its_text() {
        assert_eq!(MemoryLevel::User.as_str(), "user");
        assert_eq!(MemoryLevel::Project.as_str(), "project");
        assert_eq!(MemoryLevel::Session.as_str(), "session");
    }

    #[test]
    fn scope_reports_its_level_and_reference() {
        let scope = MemoryScope::session(&session("s-9"));
        assert_eq!(scope.level(), MemoryLevel::Session);
        assert_eq!(scope.reference(), "s-9");
        assert_eq!(MemoryScope::project("alpha").level(), MemoryLevel::Project);
    }

    #[test]
    fn types_are_send_sync_and_clone() {
        assert_send_sync_clone::<Memory>();
        assert_send_sync_clone::<MemorySummary>();
        assert_send_sync_clone::<MemoryScope>();
        assert_send_sync_clone::<MemoryLevel>();
        assert_send_sync_clone::<Arc<InMemoryMemoryStore>>();
    }

    #[tokio::test]
    async fn poisoned_lock_is_reported() {
        let store = Arc::new(InMemoryMemoryStore::new());
        let poisoner = Arc::clone(&store);
        let joined = thread::spawn(move || {
            let _guard = poisoner
                .texts
                .lock()
                .expect("the lock is fresh before the test poisons it");
            panic!("poisoning the store lock for the test");
        })
        .join();
        assert!(
            joined.is_err(),
            "the poisoning thread must panic while holding the lock"
        );
        let scope = MemoryScope::user("alice");
        assert!(matches!(
            store.get(&scope, "note").await,
            Err(ModulesError::Poisoned)
        ));
        assert!(matches!(
            store.put(&scope, &memory("note", "t", "b")).await,
            Err(ModulesError::Poisoned)
        ));
    }
}
