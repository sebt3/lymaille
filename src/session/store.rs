//! Session storage trait that the host implements, and an in-memory store for tests and demonstration hosts. The library
//! performs no I/O: the in-memory store keeps the JSON text of each state, so that a reload goes through the same format as
//! a real storage (`agent-sessions/session#FR-1`, `agent-sessions/session#DEC-1`, `agent-sessions#NFR-1`).

use std::collections::BTreeMap;
use std::sync::{Mutex, MutexGuard};

use super::{SessionError, SessionId, SessionResult, SessionState};

/// Storage of session states, implemented by the host.
///
/// `load` and `save` are plain: there is no version token and no conflict detection between processes, and the last
/// `save` wins (`agent-sessions/session#DEC-1`).
///
/// The returned futures are `Send`. The trait is used through a generic parameter only, never as a trait object.
pub trait SessionStore: Send + Sync {
    /// Reads the state saved under `id`, or `None` when there is none.
    ///
    /// # Errors
    ///
    /// Returns [`SessionError::Store`] when the host storage fails, and [`SessionError::Poisoned`] when an in-memory
    /// lock is poisoned.
    fn load(&self, id: &SessionId) -> impl Future<Output = SessionResult<Option<SessionState>>> + Send;

    /// Saves `state` under its identifier, replacing any earlier state with the same identifier.
    ///
    /// # Errors
    ///
    /// Returns [`SessionError::Store`] when the host storage fails, and [`SessionError::Serialization`] when the state
    /// cannot be written.
    fn save(&self, state: &SessionState) -> impl Future<Output = SessionResult<()>> + Send;
}

/// In-memory session store. It keeps the JSON text of each state, never the live object: a state reloaded from it goes
/// through the same reading as a state read from a real storage. It performs no I/O.
#[derive(Debug, Default)]
#[non_exhaustive]
pub struct InMemoryStore {
    texts: Mutex<BTreeMap<SessionId, String>>,
}

impl InMemoryStore {
    /// Builds an empty store.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            texts: Mutex::new(BTreeMap::new()),
        }
    }

    /// Locks the stored texts. A poisoned lock is reported as [`SessionError::Poisoned`].
    fn lock(&self) -> SessionResult<MutexGuard<'_, BTreeMap<SessionId, String>>> {
        match self.texts.lock() {
            Ok(guard) => Ok(guard),
            Err(_poisoned) => Err(SessionError::Poisoned),
        }
    }

    /// Reads the state saved under `id` from its stored text.
    fn read(&self, id: &SessionId) -> SessionResult<Option<SessionState>> {
        let text = self.lock()?.get(id).cloned();
        text.map(|text| SessionState::from_json(&text)).transpose()
    }

    /// Stores the text of `state` under its identifier, replacing the previous text.
    fn write(&self, state: &SessionState) -> SessionResult<()> {
        let text = state.to_json()?;
        let _replaced = self.lock()?.insert(state.id().clone(), text);
        Ok(())
    }
}

impl SessionStore for InMemoryStore {
    // The in-memory operations do not wait on anything: they complete at once, as a ready future.
    fn load(&self, id: &SessionId) -> impl Future<Output = SessionResult<Option<SessionState>>> + Send {
        std::future::ready(self.read(id))
    }

    fn save(&self, state: &SessionState) -> impl Future<Output = SessionResult<()>> + Send {
        std::future::ready(self.write(state))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use serde_json::json;

    use super::{InMemoryStore, SessionStore as _};
    use crate::session::{SessionError, SessionId, SessionState, TaggedValue};

    fn session(text: &str) -> SessionId {
        SessionId::new(text).expect("a non-empty text makes a session identifier")
    }

    /// A state with a history entry, a variable and a function, read from its JSON form. `count` sets the variable.
    fn fixture(id: &str, count: i64) -> SessionState {
        let document = json!({
            "id": id,
            "agent": {"name": "main-agent", "fingerprint": "sha-1"},
            "format": {"major": 1, "minor": 0},
            "history": [{"role": "user", "content": "hello"}],
            "variables": {"count": {"type": "int", "value": count}},
            "functions": {"scale": "fn scale(n) { n }"},
        });
        SessionState::from_json(&document.to_string()).expect("the fixture document must load")
    }

    #[tokio::test]
    async fn save_then_load_restores_the_state() {
        let store = InMemoryStore::new();
        let original = fixture("s-1", 7);
        store
            .save(&original)
            .await
            .expect("saving into the in-memory store must succeed");
        let loaded = store
            .load(&session("s-1"))
            .await
            .expect("loading a saved identifier must succeed")
            .expect("the saved state must be found by its identifier");
        assert_eq!(
            loaded, original,
            "the state loaded by identifier must equal the state that was saved"
        );
    }

    #[tokio::test]
    async fn unknown_session_loads_nothing() {
        let store = InMemoryStore::new();
        store.save(&fixture("s-1", 1)).await.expect("saving must succeed");
        let loaded = store.load(&session("unknown")).await;
        assert!(
            matches!(loaded, Ok(None)),
            "loading an unknown identifier must return nothing and no error"
        );
        let known = store.load(&session("s-1")).await;
        assert!(
            matches!(known, Ok(Some(_))),
            "control: the saved identifier must still be found, so the empty answer above is about the unknown identifier"
        );
    }

    #[tokio::test]
    async fn last_save_wins() {
        let store = InMemoryStore::new();
        let first = fixture("s-1", 1);
        let second = fixture("s-1", 2);
        store.save(&first).await.expect("the first save must succeed");
        store.save(&second).await.expect("the second save must succeed");
        let loaded = store
            .load(&session("s-1"))
            .await
            .expect("loading must succeed")
            .expect("the session must be found");
        assert_eq!(
            loaded, second,
            "the last saved state must be the one returned when the session is loaded"
        );
    }

    #[tokio::test]
    async fn store_holds_serialized_form() {
        let store = InMemoryStore::new();
        let mut original = fixture("s-1", 7);
        store.save(&original).await.expect("saving must succeed");
        original
            .variables_mut()
            .extend([("count".to_owned(), TaggedValue::Int(99))]);
        let loaded = store
            .load(&session("s-1"))
            .await
            .expect("loading must succeed")
            .expect("the session must be found");
        assert_eq!(
            loaded,
            fixture("s-1", 7),
            "a change made to the original object after saving must not reach the stored state"
        );
    }

    #[tokio::test]
    async fn store_is_shared_between_tasks() {
        let store = Arc::new(InMemoryStore::new());
        let writer = Arc::clone(&store);
        let state = fixture("s-1", 3);
        let handle = tokio::spawn(async move { writer.save(&state).await });
        let saved = handle.await.expect("the writing task must not panic");
        assert!(saved.is_ok(), "a save from another task must succeed");
        let loaded = store
            .load(&session("s-1"))
            .await
            .expect("loading must succeed")
            .expect("the state saved from another task must be found");
        assert_eq!(
            loaded,
            fixture("s-1", 3),
            "a store shared behind an Arc must hold the state saved by another task"
        );
    }

    #[tokio::test]
    async fn poisoned_lock_is_reported() {
        let store = Arc::new(InMemoryStore::new());
        let poisoner = Arc::clone(&store);
        let joined = std::thread::spawn(move || {
            let _guard = poisoner
                .texts
                .lock()
                .expect("the lock is not poisoned before the test poisons it");
            panic!("poisoning the store lock for the test");
        })
        .join();
        assert!(
            joined.is_err(),
            "the poisoning thread must panic while holding the lock"
        );
        let loaded = store.load(&session("s-1")).await;
        assert!(
            matches!(loaded, Err(SessionError::Poisoned)),
            "a load on a poisoned store must report SessionError::Poisoned"
        );
        let saved = store.save(&fixture("s-1", 1)).await;
        assert!(
            matches!(saved, Err(SessionError::Poisoned)),
            "a save on a poisoned store must report SessionError::Poisoned"
        );
    }
}
