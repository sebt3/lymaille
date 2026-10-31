//! Restore of a session from its saved state: the state is loaded, the pad is put back in its saved state, and the system
//! messages say what changed since the save. No script is replayed and no turn is opened.

use std::sync::Arc;

use crate::scratchpad::{Scratchpad, ScratchpadEngine};
use crate::session::{
    Provenance, SessionError, SessionId, SessionResult, SessionState, SessionStore, restore_notices,
    unresolved_call_notices,
};

/// A session restored from its saved state: the loaded state, the restored pad, and the system messages that tell what
/// changed since the save.
#[derive(Debug)]
#[non_exhaustive]
pub struct RestoredSession {
    state: SessionState,
    pad: Scratchpad,
    notices: Vec<String>,
}

impl RestoredSession {
    /// Returns the loaded state. It is the state saved at the end of the previous turn, unchanged.
    #[must_use]
    pub const fn state(&self) -> &SessionState {
        &self.state
    }

    /// Returns the restored pad.
    #[must_use]
    pub const fn pad(&self) -> &Scratchpad {
        &self.pad
    }

    /// Returns the system messages of the restore, in order: the agent definition, then the toolsets by name, then the
    /// broken functions. Empty when nothing changed and every function still compiles.
    #[must_use]
    pub fn notices(&self) -> &[String] {
        &self.notices
    }

    /// Consumes the restored session and returns its three parts: the state, the pad and the system messages.
    #[must_use]
    pub fn into_parts(self) -> (SessionState, Scratchpad, Vec<String>) {
        (self.state, self.pad, self.notices)
    }
}

/// Restores the session `id` from `store`, on the scratchpad engine `engine` that the host built with the current
/// grants.
///
/// The provenance `current` is that of the running agent: the fingerprint of its definition and the version of each
/// toolset it uses.
///
/// # Errors
///
/// Returns [`SessionError::UnknownSession`](crate::session::SessionError::UnknownSession) when nothing is saved under
/// `id`, [`SessionError::IncompatibleFormat`](crate::session::SessionError::IncompatibleFormat) when the saved major
/// format version differs, and [`SessionError::Store`](crate::session::SessionError::Store) when the storage fails.
pub async fn restore_session<S: SessionStore>(
    store: &S,
    id: &SessionId,
    engine: Arc<ScratchpadEngine>,
    current: &Provenance,
) -> SessionResult<RestoredSession> {
    // The format is checked by the reading of the state (`SessionState::from_json`) and its error is propagated as is.
    let state = store
        .load(id)
        .await?
        .ok_or_else(|| SessionError::UnknownSession { id: id.clone() })?;
    let saved = Provenance::new(state.agent().fingerprint(), state.toolset_versions().clone());
    let restored = Scratchpad::restore(engine, state.variables(), state.functions());
    let mut notices = restore_notices(&saved, current, restored.broken());
    let calls: Vec<(String, String)> = restored
        .warnings()
        .iter()
        .map(|warning| (warning.function().to_owned(), warning.call().to_owned()))
        .collect();
    notices.extend(unresolved_call_notices(&calls));
    let pad = restored.into_pad();
    Ok(RestoredSession { state, pad, notices })
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use serde_json::json;

    use super::{RestoredSession, restore_session};
    use crate::clock::{ManualClock, Timestamp};
    use crate::scratchpad::{
        Grants, RunEnv, RunOutcome, RunStatus, Scratchpad, ScratchpadEngine, ScratchpadLimits,
    };
    use crate::session::{
        CancelHandle, InMemoryStore, Provenance, Redactor, SessionError, SessionId, SessionResult,
        SessionState, SessionStore,
    };

    fn session(text: &str) -> SessionId {
        SessionId::new(text).expect("a non-empty text makes a session identifier")
    }

    fn engine() -> Arc<ScratchpadEngine> {
        let built = ScratchpadEngine::builder(ScratchpadLimits::new(), Grants::new()).build();
        Arc::new(built.expect("an engine without grants must build"))
    }

    /// The JSON of a saved state: an integer `x` (5), a function `f/1` doubling its argument, the agent `main-agent`
    /// with the fingerprint `fingerprint`, and the toolset `grafana` at version `toolset`.
    fn document(id: &str, fingerprint: &str, toolset: &str) -> String {
        json!({
            "id": id,
            "format": {"major": 1, "minor": 0},
            "agent": {"name": "main-agent", "fingerprint": fingerprint},
            "toolset_versions": {"grafana": toolset},
            "variables": {"x": {"type": "int", "value": 5}},
            "functions": {"f/1": "fn f(n) { n * 2 }"},
        })
        .to_string()
    }

    fn provenance(fingerprint: &str, toolset: &str) -> Provenance {
        Provenance::new(
            fingerprint,
            BTreeMap::from([("grafana".to_owned(), toolset.to_owned())]),
        )
    }

    /// A store holding the state read from `text`.
    async fn saved(text: &str) -> InMemoryStore {
        let store = InMemoryStore::new();
        let state = SessionState::from_json(text).expect("the fixture document must load");
        store
            .save(&state)
            .await
            .expect("saving into the in-memory store must succeed");
        store
    }

    async fn eval(pad: &Scratchpad, script: &str) -> RunOutcome {
        let redactor = Redactor::new();
        let clock = ManualClock::new(Timestamp::from_millis(0));
        let cancel = CancelHandle::new();
        let env = RunEnv::new("agent", &redactor, &clock);
        pad.run(script, &cancel, &env).await
    }

    /// A store that reads `text` as the saved state, as a storage would.
    struct RawStore(String);

    impl SessionStore for RawStore {
        fn load(&self, _id: &SessionId) -> impl Future<Output = SessionResult<Option<SessionState>>> + Send {
            std::future::ready(SessionState::from_json(&self.0).map(Some))
        }

        fn save(&self, _state: &SessionState) -> impl Future<Output = SessionResult<()>> + Send {
            std::future::ready(Err(SessionError::Store("the test store is read-only".to_owned())))
        }
    }

    // Scenario: restaurer une session rend état, pad et messages
    #[tokio::test]
    async fn restoring_a_session_gives_state_pad_and_no_notice() {
        let store = saved(&document("s-1", "sha-1", "1.0.0")).await;
        let restored =
            restore_session(&store, &session("s-1"), engine(), &provenance("sha-1", "1.0.0")).await;
        let restored: RestoredSession = match restored {
            Ok(restored) => restored,
            Err(error) => panic!("a saved session with the same provenance must restore: {error}"),
        };
        assert!(
            restored.notices().is_empty(),
            "no system message when the provenance is identical"
        );
        let read = eval(restored.pad(), "x").await;
        assert!(
            matches!(read.status(), RunStatus::Ok),
            "reading the restored variable must succeed"
        );
        assert_eq!(read.text(), "5", "the restored pad holds the saved variable x");
        let call = eval(restored.pad(), "f(x)").await;
        assert!(
            matches!(call.status(), RunStatus::Ok),
            "calling the restored function must succeed"
        );
        assert_eq!(call.text(), "10", "the restored pad calls the saved function f");
    }

    // Scenario: les changements de provenance sont signalés
    #[tokio::test]
    async fn provenance_changes_give_two_notices_agent_then_toolset() {
        let store = saved(&document("s-1", "sha-old", "1.0.0")).await;
        let restored =
            restore_session(&store, &session("s-1"), engine(), &provenance("sha-new", "2.0.0")).await;
        let restored = match restored {
            Ok(restored) => restored,
            Err(error) => panic!("a provenance change must not refuse the restore: {error}"),
        };
        let notices = restored.notices();
        assert_eq!(notices.len(), 2, "one message for the agent, one for the toolset");
        assert!(
            notices[0].starts_with("the agent definition changed"),
            "the agent message comes first: {}",
            notices[0]
        );
        assert!(
            notices[1].starts_with("toolset `grafana` changed major version"),
            "the toolset message comes second: {}",
            notices[1]
        );
    }

    // Scenario: session inconnue ou format incompatible (session inconnue)
    #[tokio::test]
    async fn unknown_session_is_refused_with_its_identifier() {
        let store = saved(&document("s-1", "sha-1", "1.0.0")).await;
        let result = restore_session(
            &store,
            &session("unknown"),
            engine(),
            &provenance("sha-1", "1.0.0"),
        )
        .await;
        match result {
            Err(SessionError::UnknownSession { id }) => {
                assert_eq!(
                    id.as_str(),
                    "unknown",
                    "the error carries the identifier that was asked for"
                );
            }
            Err(other) => panic!("an unknown identifier must give UnknownSession, not {other}"),
            Ok(_) => panic!("an unknown identifier must not restore anything"),
        }
    }

    // Scenario: session inconnue ou format incompatible (format incompatible)
    #[tokio::test]
    async fn incompatible_major_format_is_refused() {
        let text = document("s-1", "sha-1", "1.0.0").replace("\"major\":1", "\"major\":2");
        let store = RawStore(text);
        let result = restore_session(&store, &session("s-1"), engine(), &provenance("sha-1", "1.0.0")).await;
        match result {
            Err(SessionError::IncompatibleFormat { saved, current }) => {
                assert_eq!(saved.major(), 2, "the saved major version is carried");
                assert_eq!(current.major(), 1, "the current major version is carried");
            }
            Err(other) => panic!("an other major version must give IncompatibleFormat, not {other}"),
            Ok(_) => panic!("an other major version must not restore anything"),
        }
    }

    /// A store that counts the saves it receives and otherwise delegates to an in-memory store.
    struct CountingStore {
        inner: InMemoryStore,
        saves: AtomicUsize,
    }

    impl SessionStore for CountingStore {
        fn load(&self, id: &SessionId) -> impl Future<Output = SessionResult<Option<SessionState>>> + Send {
            self.inner.load(id)
        }

        fn save(&self, state: &SessionState) -> impl Future<Output = SessionResult<()>> + Send {
            let _previous = self.saves.fetch_add(1, Ordering::SeqCst);
            self.inner.save(state)
        }
    }

    // Scenario: session inconnue ou format incompatible (rien n'est modifié)
    #[tokio::test]
    async fn a_refused_restore_modifies_nothing() {
        let store = CountingStore {
            inner: saved(&document("s-1", "sha-1", "1.0.0")).await,
            saves: AtomicUsize::new(0),
        };
        let before = store
            .load(&session("s-1"))
            .await
            .expect("the saved state loads")
            .expect("the state is there")
            .to_json()
            .expect("the state serializes");
        let unknown = restore_session(
            &store,
            &session("unknown"),
            engine(),
            &provenance("sha-1", "1.0.0"),
        )
        .await;
        assert!(
            matches!(unknown, Err(SessionError::UnknownSession { .. })),
            "an unknown identifier is refused"
        );
        let incompatible = restore_session(
            &RawStore(document("s-1", "sha-1", "1.0.0").replace("\"major\":1", "\"major\":2")),
            &session("s-1"),
            engine(),
            &provenance("sha-1", "1.0.0"),
        )
        .await;
        assert!(
            matches!(incompatible, Err(SessionError::IncompatibleFormat { .. })),
            "an incompatible format is refused"
        );
        assert_eq!(
            store.saves.load(Ordering::SeqCst),
            0,
            "the store received no save"
        );
        let after = store
            .load(&session("s-1"))
            .await
            .expect("the saved state loads")
            .expect("the state is still there")
            .to_json()
            .expect("the state serializes");
        assert_eq!(before, after, "the saved state is unchanged");
    }

    // Scenario: une fonction dont une API a disparu n'empêche pas la restauration
    #[tokio::test]
    async fn a_function_calling_a_vanished_api_does_not_prevent_the_restore() {
        let text = json!({
            "id": "s-1",
            "format": {"major": 1, "minor": 0},
            "agent": {"name": "main-agent", "fingerprint": "sha-1"},
            "toolset_versions": {"grafana": "1.0.0"},
            "variables": {"x": {"type": "int", "value": 5}},
            "functions": {
                "f/1": "fn f(n) { n * 2 }",
                "h/1": "fn h(n) { ops::disparue(n) }",
            },
        })
        .to_string();
        let store = saved(&text).await;
        let restored =
            restore_session(&store, &session("s-1"), engine(), &provenance("sha-1", "1.0.0")).await;
        let restored = match restored {
            Ok(restored) => restored,
            Err(error) => panic!("a vanished API must not refuse the restore: {error}"),
        };
        let read = eval(restored.pad(), "x").await;
        assert_eq!(read.text(), "5", "the variable is restored");
        let call = eval(restored.pad(), "f(x)").await;
        assert_eq!(call.text(), "10", "the other function is restored");
        let notices = restored.notices();
        assert_eq!(
            notices.len(),
            1,
            "one system message for the function with the vanished API: {notices:?}"
        );
        assert!(
            notices[0].contains("`h/1`") && notices[0].contains("`ops::disparue`"),
            "the message names the function and the call: {}",
            notices[0]
        );
    }

    #[tokio::test]
    async fn into_parts_gives_the_state_the_pad_and_the_notices() {
        let store = saved(&document("s-1", "sha-1", "1.0.0")).await;
        let restored =
            restore_session(&store, &session("s-1"), engine(), &provenance("sha-1", "1.0.0")).await;
        let restored = match restored {
            Ok(restored) => restored,
            Err(error) => panic!("the fixture must restore: {error}"),
        };
        let (state, pad, notices) = restored.into_parts();
        assert_eq!(state.id().as_str(), "s-1", "the state is the saved session");
        assert!(notices.is_empty(), "no notice for an identical provenance");
        let call = eval(&pad, "f(x)").await;
        assert_eq!(call.text(), "10", "the pad part is the restored pad");
    }
}
