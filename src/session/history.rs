//! Persisted history of a session: each message as the model received or produced it, with its reasoning and its
//! emitter, the tool calls, the outputs of `rhai_pad` and `skill` as capped, and the segment boundaries
//! (`agent-sessions/session#FR-12`, `agent-sessions/session#FR-14`).

use serde::{Deserialize, Serialize};

use super::{BoundaryKind, SessionError, SessionResult};

/// Reasoning produced by the model, with the identifier of the provider or model that alone can replay it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct Reasoning {
    emitter: String,
    text: String,
}

impl Reasoning {
    /// Builds reasoning from the identifier of its emitter and its text.
    #[must_use]
    pub fn new<E: Into<String>, T: Into<String>>(emitter: E, text: T) -> Self {
        Self {
            emitter: emitter.into(),
            text: text.into(),
        }
    }

    /// Returns the identifier of the provider or model that emitted the reasoning.
    #[must_use]
    pub fn emitter(&self) -> &str {
        &self.emitter
    }

    /// Returns the reasoning text, as produced.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }
}

/// A tool call of the assistant: its identifier, the tool name and its JSON arguments.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct ToolCall {
    id: String,
    name: String,
    arguments: serde_json::Value,
}

impl ToolCall {
    /// Builds a tool call.
    #[must_use]
    pub fn new<I: Into<String>, N: Into<String>>(id: I, name: N, arguments: serde_json::Value) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            arguments,
        }
    }

    /// Returns the identifier of the call.
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Returns the name of the tool.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns the JSON arguments of the call.
    #[must_use]
    pub const fn arguments(&self) -> &serde_json::Value {
        &self.arguments
    }
}

/// One message of the history. Contents are kept as received or produced, never rewritten.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "role", rename_all = "snake_case")]
#[non_exhaustive]
pub enum Message {
    /// A system message.
    System {
        /// The text of the message.
        content: String,
    },
    /// A message from the user.
    User {
        /// The text of the message.
        content: String,
    },
    /// A message from the assistant: text, optional reasoning, and the tool calls it made.
    Assistant {
        /// The text of the message.
        content: String,
        /// The reasoning emitted with the message, when there is one.
        reasoning: Option<Reasoning>,
        /// The tool calls made by the message, in order.
        tool_calls: Vec<ToolCall>,
    },
    /// The result of a tool call, as the model received it.
    ToolResult {
        /// Identifier of the tool call this result answers.
        call_id: String,
        /// Name of the tool.
        tool: String,
        /// Content as the model received it, truncated when it was too long.
        content: String,
        /// Length in characters of the output before truncation, when it was truncated.
        original_chars: Option<u64>,
    },
    /// A segment boundary: start, compaction or restore.
    Boundary {
        /// Why the segment starts.
        kind: BoundaryKind,
    },
}

impl Message {
    /// Builds a system message.
    #[must_use]
    pub fn system<C: Into<String>>(content: C) -> Self {
        Self::System {
            content: content.into(),
        }
    }

    /// Builds a user message.
    #[must_use]
    pub fn user<C: Into<String>>(content: C) -> Self {
        Self::User {
            content: content.into(),
        }
    }

    /// Builds an assistant message.
    #[must_use]
    pub fn assistant<C: Into<String>>(
        content: C,
        reasoning: Option<Reasoning>,
        tool_calls: Vec<ToolCall>,
    ) -> Self {
        Self::Assistant {
            content: content.into(),
            reasoning,
            tool_calls,
        }
    }

    /// Builds the result of a tool call. `original_chars` is the length of the output before truncation, when it was
    /// truncated.
    #[must_use]
    pub fn tool_result<I: Into<String>, T: Into<String>, C: Into<String>>(
        call_id: I,
        tool: T,
        content: C,
        original_chars: Option<u64>,
    ) -> Self {
        Self::ToolResult {
            call_id: call_id.into(),
            tool: tool.into(),
            content: content.into(),
            original_chars,
        }
    }

    /// Builds a segment boundary.
    #[must_use]
    pub const fn boundary(kind: BoundaryKind) -> Self {
        Self::Boundary { kind }
    }
}

/// Append-only history of one session. Pending tool calls are derived from the messages, never stored.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
#[non_exhaustive]
pub struct History {
    messages: Vec<Message>,
}

impl History {
    /// Builds an empty history.
    #[must_use]
    pub const fn new() -> Self {
        Self { messages: Vec::new() }
    }

    /// Appends a message. Nothing already in the history is changed.
    pub fn push(&mut self, message: Message) {
        self.messages.push(message);
    }

    /// Appends a user message received during a turn. It is refused, and nothing is appended, while a tool call waits
    /// for its result: the turn retries after the results (`agent-sessions/session#FR-14`).
    ///
    /// # Errors
    ///
    /// Returns [`SessionError::PendingToolCall`] when some tool call has no result yet.
    pub fn push_user_during_turn<C: Into<String>>(&mut self, content: C) -> SessionResult<()> {
        if !self.pending_tool_calls().is_empty() {
            return Err(SessionError::PendingToolCall);
        }
        self.messages.push(Message::user(content));
        Ok(())
    }

    /// Returns the messages, in the order they were appended.
    #[must_use]
    pub fn messages(&self) -> &[Message] {
        &self.messages
    }

    /// Returns the identifiers of the tool calls of the assistant that have no result yet, in call order.
    #[must_use]
    pub fn pending_tool_calls(&self) -> Vec<&str> {
        let mut pending: Vec<&str> = Vec::new();
        for message in &self.messages {
            match message {
                Message::Assistant { tool_calls, .. } => pending.extend(tool_calls.iter().map(ToolCall::id)),
                Message::ToolResult { call_id, .. } => pending.retain(|id| *id != call_id.as_str()),
                Message::System { .. } | Message::User { .. } | Message::Boundary { .. } => {}
            }
        }
        pending
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{History, Message, Reasoning, ToolCall};
    use crate::session::{SessionError, SessionResult};

    fn read_file_call(id: &str) -> ToolCall {
        ToolCall::new(id, "read_file", json!({"path": "notes.md"}))
    }

    #[test]
    fn messages_keep_their_order_and_content() {
        let mut history = History::new();
        history.push(Message::user("summarize the notes"));
        history.push(Message::assistant(
            "I read the file first.",
            Some(Reasoning::new("model-x/replay", "the user wants a summary")),
            vec![read_file_call("call-1")],
        ));
        history.push(Message::tool_result(
            "call-1",
            "read_file",
            "# Notes\nbuy milk",
            None,
        ));

        let json = serde_json::to_string(&history).expect("a history serializes");
        let back: History = serde_json::from_str(&json).expect("a serialized history deserializes");
        assert_eq!(back, history, "the history must be identical after a round trip");

        let [user, assistant, result] = back.messages() else {
            panic!("the history must hold its three messages in order");
        };
        assert_eq!(
            user,
            &Message::user("summarize the notes"),
            "the user message must come first and be unchanged"
        );
        let Message::Assistant {
            reasoning: Some(reasoning),
            ..
        } = assistant
        else {
            panic!("the assistant message must keep its reasoning");
        };
        assert_eq!(
            reasoning.emitter(),
            "model-x/replay",
            "the reasoning emitter must be kept as given"
        );
        assert_eq!(
            reasoning.text(),
            "the user wants a summary",
            "the reasoning text must be kept as given"
        );
        let Message::ToolResult { content, .. } = result else {
            panic!("the tool result must come last");
        };
        assert_eq!(
            content, "# Notes\nbuy milk",
            "the tool output must be kept as the model received it"
        );
    }

    #[test]
    fn user_message_waits_for_tool_results() {
        let mut history = History::new();
        history.push(Message::assistant("", None, vec![read_file_call("call-1")]));
        let before = history.clone();

        let result: SessionResult<()> = history.push_user_during_turn("and also this");

        assert!(
            matches!(result, Err(SessionError::PendingToolCall)),
            "a user message during a pending tool call must fail with PendingToolCall"
        );
        assert_eq!(
            history, before,
            "a refused user message must leave the history unchanged"
        );
    }

    #[test]
    fn user_message_accepted_after_the_result() {
        let mut history = History::new();
        history.push(Message::assistant("", None, vec![read_file_call("call-1")]));
        history.push(Message::tool_result("call-1", "read_file", "content", None));

        let result = history.push_user_during_turn("and also this");

        assert!(
            result.is_ok(),
            "a user message once every tool call has its result must be accepted"
        );
        let last = history.messages().last();
        assert_eq!(
            last,
            Some(&Message::user("and also this")),
            "the accepted user message must be appended after the result"
        );
    }

    #[test]
    fn pending_calls_derive_from_messages() {
        let mut history = History::new();
        history.push(Message::assistant(
            "two reads",
            None,
            vec![read_file_call("call-1"), read_file_call("call-2")],
        ));
        history.push(Message::tool_result("call-1", "read_file", "content", None));

        assert_eq!(
            history.pending_tool_calls(),
            vec!["call-2"],
            "only the call without a result must be listed"
        );

        let json = serde_json::to_string(&history).expect("a history serializes");
        let back: History = serde_json::from_str(&json).expect("a serialized history deserializes");
        assert_eq!(
            back.pending_tool_calls(),
            vec!["call-2"],
            "after a round trip the pending calls must be the same"
        );
    }

    #[test]
    fn truncated_output_keeps_the_original_size() {
        let mut history = History::new();
        history.push(Message::tool_result(
            "call-1",
            "rhai_pad",
            "first 1000 characters...",
            Some(5000),
        ));

        let json = serde_json::to_string(&history).expect("a history serializes");
        let back: History = serde_json::from_str(&json).expect("a serialized history deserializes");

        let [only] = back.messages() else {
            panic!("the history must hold the single tool result");
        };
        let Message::ToolResult {
            content,
            original_chars,
            ..
        } = only
        else {
            panic!("the message must be a tool result");
        };
        assert_eq!(
            content, "first 1000 characters...",
            "the content must be the truncated text"
        );
        assert_eq!(
            *original_chars,
            Some(5000),
            "the original size of 5000 characters must be kept"
        );
    }
}
