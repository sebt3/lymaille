//! Observable events of the library and the channel through which the host receives them (`agent-sessions#FR-3`,
//! `agent-sessions/session#FR-6`).

use std::sync::{Mutex, MutexGuard, PoisonError};

use serde::{Deserialize, Serialize};

use super::{McpRecord, Outcome, RunId, ScriptRecord, SessionId, TurnId};
use crate::clock::Timestamp;

/// What happened, as reported to the host. Carries names, identifiers, counters and masked records only, never a
/// secret or the body of a Memory.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum EventKind {
    /// Tokens used by an inference call.
    TokenUsage {
        /// Tokens of the prompt.
        prompt: u64,
        /// Tokens of the completion.
        completion: u64,
    },
    /// The todo list changed.
    TodoChanged,
    /// A Memory changed. Only its level and identifier are reported, never its body.
    MemoryChanged {
        /// Level of the Memory: user, project or session.
        level: String,
        /// Identifier of the Memory within its level.
        id: String,
    },
    /// A script ran.
    Script(ScriptRecord),
    /// An MCP call was made.
    McpCall(McpRecord),
    /// A reconnection to an MCP server is attempted after a loss of the connection. One event per attempt. Carries the role
    /// of the server, never an argument nor a secret.
    McpReconnect {
        /// Role of the MCP server in the agent definition.
        role: String,
        /// Number of the attempt, starting at 1.
        attempt: u32,
        /// Delay before the attempt, in milliseconds.
        delay_ms: u64,
    },
    /// A turn exceeded its configured delay. The turn is not interrupted.
    Overrun {
        /// Time elapsed since the start of the turn, in milliseconds.
        elapsed_ms: u64,
    },
    /// An inference call is retried after a transient error.
    InferenceRetry {
        /// Number of the retry, starting at 1.
        attempt: u32,
        /// Delay before the retry, in milliseconds.
        delay_ms: u64,
        /// Class of the error that caused the retry.
        error_class: String,
    },
    /// A turn started.
    TurnStarted {
        /// Rank of the turn in its session.
        turn: TurnId,
    },
    /// A fragment of the streamed text of a turn, in emission order. The complete response comes with `TurnEnded`.
    TextDelta {
        /// Rank of the turn the fragment belongs to.
        turn: TurnId,
        /// Text of the fragment.
        text: String,
    },
    /// A turn ended with its outcome and its complete response.
    TurnEnded {
        /// Rank of the turn in its session.
        turn: TurnId,
        /// Outcome of the turn.
        outcome: Outcome,
        /// Complete response of the turn.
        response: String,
    },
    /// A run ended with its outcome.
    RunEnded {
        /// Identifier of the run.
        run: RunId,
        /// Outcome of the run.
        outcome: Outcome,
    },
}

/// Event observed by the host: the session it belongs to, the instant it happened and what it reports.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct Event {
    /// Session in which the event happened.
    pub session: SessionId,
    /// Instant of the event, from the injected clock.
    pub at: Timestamp,
    /// What happened.
    pub kind: EventKind,
}

impl Event {
    /// Builds an event of `session` at instant `at`.
    #[must_use]
    pub const fn new(session: SessionId, at: Timestamp, kind: EventKind) -> Self {
        Self { session, at, kind }
    }
}

/// Channel through which the library hands its events to the host. Implementations are shared between threads.
pub trait EventSink: Send + Sync {
    /// Receives one event. It does not block, and it cannot fail towards the library.
    fn emit(&self, event: Event);
}

/// Sink that keeps every event in emission order, for the host to read back or for tests.
#[derive(Debug, Default)]
#[non_exhaustive]
pub struct RecordingSink {
    events: Mutex<Vec<Event>>,
}

impl RecordingSink {
    /// Builds an empty recording sink.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns a copy of the events received so far, in emission order.
    #[must_use]
    pub fn events(&self) -> Vec<Event> {
        self.lock().clone()
    }

    /// Locks the recorded events. A poisoned lock is recovered: a sink must keep accepting events.
    fn lock(&self) -> MutexGuard<'_, Vec<Event>> {
        self.events.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl EventSink for RecordingSink {
    fn emit(&self, event: Event) {
        self.lock().push(event);
    }
}

/// Sink that discards every event.
#[derive(Debug, Default, Clone, Copy)]
#[non_exhaustive]
pub struct NullSink;

impl NullSink {
    /// Builds a sink that discards every event.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl EventSink for NullSink {
    fn emit(&self, _event: Event) {}
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::thread;

    use super::{Event, EventKind, RecordingSink};
    use crate::clock::Timestamp;
    use crate::session::{EventSink as _, Outcome, RunId, SessionId, TurnId};

    /// Names of the twelve variants of [`EventKind`], in the order of [`one_of_each_kind`].
    const ALL_VARIANTS: [&str; 12] = [
        "TokenUsage",
        "TodoChanged",
        "MemoryChanged",
        "Script",
        "McpCall",
        "McpReconnect",
        "Overrun",
        "InferenceRetry",
        "TurnStarted",
        "TextDelta",
        "TurnEnded",
        "RunEnded",
    ];

    fn session() -> SessionId {
        SessionId::new("session-1").expect("a non-empty text makes a session identifier")
    }

    /// A reconnection attempt of the MCP server `monitoring`.
    fn reconnect_sample() -> EventKind {
        EventKind::McpReconnect {
            role: "monitoring".to_owned(),
            attempt: 1,
            delay_ms: 500,
        }
    }

    /// One event of each kind, with the name of its variant, so that a test can check that none is left out.
    fn one_of_each_kind() -> Vec<(&'static str, EventKind)> {
        vec![
            (
                "TokenUsage",
                EventKind::TokenUsage {
                    prompt: 120,
                    completion: 45,
                },
            ),
            ("TodoChanged", EventKind::TodoChanged),
            (
                "MemoryChanged",
                EventKind::MemoryChanged {
                    level: "project".to_owned(),
                    id: "deploy-notes".to_owned(),
                },
            ),
            (
                "Script",
                EventKind::Script(crate::session::ScriptRecord::new(
                    &crate::session::Redactor::new(),
                    10,
                    crate::session::ScriptInput::new(
                        "agent-a",
                        "print(1)",
                        "1",
                        std::time::Duration::from_millis(4),
                    ),
                )),
            ),
            (
                "McpCall",
                EventKind::McpCall(crate::session::McpRecord::new(
                    &crate::session::Redactor::new(),
                    crate::session::McpInput::new(
                        "monitoring",
                        "query",
                        &serde_json::json!({"expr": "up"}),
                        "ok",
                        std::time::Duration::from_millis(9),
                    ),
                )),
            ),
            ("McpReconnect", reconnect_sample()),
            ("Overrun", EventKind::Overrun { elapsed_ms: 90_000 }),
            (
                "InferenceRetry",
                EventKind::InferenceRetry {
                    attempt: 2,
                    delay_ms: 2000,
                    error_class: "timeout".to_owned(),
                },
            ),
            ("TurnStarted", EventKind::TurnStarted { turn: TurnId::new(3) }),
            (
                "TextDelta",
                EventKind::TextDelta {
                    turn: TurnId::new(3),
                    text: "Bon".to_owned(),
                },
            ),
            (
                "TurnEnded",
                EventKind::TurnEnded {
                    turn: TurnId::new(3),
                    outcome: Outcome::Valid,
                    response: "done".to_owned(),
                },
            ),
            (
                "RunEnded",
                EventKind::RunEnded {
                    run: RunId::new("run-7").expect("a non-empty text makes a run identifier"),
                    outcome: Outcome::Cancelled,
                },
            ),
        ]
    }

    /// Name of a variant, matched without a wildcard: adding a variant breaks the build until the test covers it.
    fn variant_name(kind: &EventKind) -> &'static str {
        match kind {
            EventKind::TokenUsage { .. } => "TokenUsage",
            EventKind::TodoChanged => "TodoChanged",
            EventKind::MemoryChanged { .. } => "MemoryChanged",
            EventKind::Script(_) => "Script",
            EventKind::McpCall(_) => "McpCall",
            EventKind::McpReconnect { .. } => "McpReconnect",
            EventKind::Overrun { .. } => "Overrun",
            EventKind::InferenceRetry { .. } => "InferenceRetry",
            EventKind::TurnStarted { .. } => "TurnStarted",
            EventKind::TextDelta { .. } => "TextDelta",
            EventKind::TurnEnded { .. } => "TurnEnded",
            EventKind::RunEnded { .. } => "RunEnded",
        }
    }

    #[test]
    fn events_keep_emission_order() {
        let sink = RecordingSink::new();
        let first = Event::new(session(), Timestamp::from_millis(1), EventKind::TodoChanged);
        let second = Event::new(session(), Timestamp::from_millis(2), EventKind::TodoChanged);
        let third = Event::new(session(), Timestamp::from_millis(3), EventKind::TodoChanged);
        sink.emit(first.clone());
        sink.emit(second.clone());
        sink.emit(third.clone());
        assert_eq!(
            sink.events(),
            vec![first, second, third],
            "the recording sink must return the three events in the order they were emitted"
        );
    }

    #[test]
    fn text_fragments_keep_their_order() {
        let sink = RecordingSink::new();
        let fragments = ["Bon", "jour", "!"];
        for text in fragments {
            sink.emit(Event::new(
                session(),
                Timestamp::from_millis(5),
                EventKind::TextDelta {
                    turn: TurnId::new(3),
                    text: text.to_owned(),
                },
            ));
        }
        let received: Vec<String> = sink
            .events()
            .into_iter()
            .map(|event| {
                let EventKind::TextDelta { turn, text } = event.kind else {
                    panic!("only text fragments were emitted")
                };
                assert_eq!(turn, TurnId::new(3), "each fragment must carry its turn");
                text
            })
            .collect();
        assert_eq!(
            received, fragments,
            "the three fragments must come back with their text, in the order they were emitted"
        );
    }

    #[test]
    fn event_round_trips_through_json() {
        let kinds = one_of_each_kind();
        assert!(
            kinds.iter().map(|(name, _)| *name).eq(ALL_VARIANTS),
            "the samples must cover each of the eleven variants of EventKind, in order"
        );
        for (name, kind) in &kinds {
            assert_eq!(
                variant_name(kind),
                *name,
                "the sample for {name} must be of that variant"
            );
        }
        for (name, kind) in kinds {
            let event = Event::new(session(), Timestamp::from_millis(42), kind);
            let json = serde_json::to_string(&event).expect("an event serializes");
            let back: Event = serde_json::from_str(&json).expect("a serialized event deserializes");
            assert_eq!(
                back, event,
                "the {name} event must deserialize to the same value, json was {json}"
            );
        }
    }

    /// Emits ten todo events from `worker`, with instants that tell the workers apart.
    fn emit_ten_events(sink: &RecordingSink, worker: u64) {
        for step in 0..10 {
            let at = Timestamp::from_millis(worker.saturating_mul(100).saturating_add(step));
            sink.emit(Event::new(session(), at, EventKind::TodoChanged));
        }
    }

    #[test]
    fn recording_sink_is_shareable_across_threads() {
        let sink = Arc::new(RecordingSink::new());
        let workers: Vec<_> = (0..4)
            .map(|worker| {
                let sink = Arc::clone(&sink);
                thread::spawn(move || emit_ten_events(&sink, worker))
            })
            .collect();
        for worker in workers {
            worker.join().expect("an emitting thread does not panic");
        }
        assert_eq!(
            sink.events().len(),
            40,
            "four threads emitting ten events each must record forty events"
        );
    }
}
