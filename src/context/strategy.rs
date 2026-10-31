//! Compaction strategies: the summarizer the host provides, the choice of what is compacted, the default strategy that
//! summarizes the whole session, and the digest of the state kept outside the window
//! (`agent-sessions/context-window#FR-3`, `FR-4`, `FR-7`, `AC-4`). The crate makes no model call: the summarizer belongs
//! to the host or to the `agents` domain.

use super::{ContextError, ContextResult, ReplayMode, replayed_reasoning};
use crate::session::{BoundaryKind, History, Message, Reasoning, ToolCall};

/// Digest of the state kept outside the window: the number of open tasks and the number of memories. It carries counts
/// only, never the text of a task or of a memory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct Digest {
    open_tasks: u64,
    memories: u64,
}

impl Digest {
    /// Builds a digest from the number of open tasks and the number of memories.
    #[must_use]
    pub const fn new(open_tasks: u64, memories: u64) -> Self {
        Self { open_tasks, memories }
    }

    /// Renders the digest as one line of English text that gives the two counts and nothing else.
    #[must_use]
    pub fn render(self) -> String {
        format!(
            "Session digest: {} open tasks, {} memories.",
            self.open_tasks, self.memories
        )
    }
}

/// Opening line of the message that carries the summary: it tells the model what the message is.
const SUMMARY_PREAMBLE: &str = "The earlier part of this conversation was compacted. Summary of that part:";

/// Renders the messages as the summarizer reads them: one `role: content` line per message, in order. The reasoning
/// selected by the replay mode is given on the line before its assistant message, and each tool call of an assistant
/// message on a line after it.
fn render_transcript(messages: &[Message], replayed: &[Option<&Reasoning>]) -> String {
    let mut lines: Vec<String> = Vec::new();
    for (message, reasoning) in messages.iter().zip(replayed) {
        if let Some(reasoning) = reasoning {
            lines.push(format!("reasoning: {}", reasoning.text()));
        }
        lines.push(render_message(message));
        if let Message::Assistant { tool_calls, .. } = message {
            lines.extend(tool_calls.iter().map(render_tool_call));
        }
    }
    lines.join("\n")
}

/// Renders one message as `role: content`. A tool result carries the name of its tool: `tool <name>: content`.
fn render_message(message: &Message) -> String {
    match message {
        Message::System { content } => format!("system: {content}"),
        Message::User { content } => format!("user: {content}"),
        Message::Assistant { content, .. } => format!("assistant: {content}"),
        Message::ToolResult { tool, content, .. } => format!("tool {tool}: {content}"),
        Message::Boundary { kind } => format!("boundary: {}", boundary_text(*kind)),
    }
}

/// Renders one tool call of an assistant message, on the line after that message: `assistant tool call <name>: <args>`.
fn render_tool_call(call: &ToolCall) -> String {
    format!("assistant tool call {}: {}", call.name(), call.arguments())
}

/// Text of a boundary kind, as the transcript shows it.
const fn boundary_text(kind: BoundaryKind) -> &'static str {
    match kind {
        BoundaryKind::Start => "start",
        BoundaryKind::Compaction => "compaction",
        BoundaryKind::Restore => "restore",
    }
}

/// Text of the message that opens the new segment: the preamble, the summary, then the digest when there is one.
fn summary_message(summary: &str, digest: Option<&Digest>) -> String {
    let mut parts = vec![SUMMARY_PREAMBLE.to_owned(), summary.to_owned()];
    if let Some(digest) = digest {
        parts.push(digest.render());
    }
    parts.join("\n\n")
}

/// Default summary prompt, given to the summarizer of the host.
///
/// It asks for the user's requests to be quoted verbatim, since they cannot be regenerated, and for the quotes of a
/// previous summary to be kept.
pub const DEFAULT_SUMMARY_PROMPT: &str = "You are summarizing the earlier part of a conversation between a user and an AI agent. The agent will continue working from your summary alone: it will not see the original messages.\n\n\
Write a summary that lets the agent resume without loss, in these sections:\n\n\
1. User requests \u{2014} Quote every request, instruction, correction and constraint the user gave, VERBATIM, in order, inside quotation marks. Never paraphrase them: they cannot be regenerated. Keep decisions the user made and anything they said to remember or avoid.\n\
2. Current state \u{2014} What has been done, what is in progress, and the exact next step.\n\
3. Findings \u{2014} Facts established (results, identifiers, names, values) that later steps depend on. Keep exact values; drop exploration that led nowhere.\n\
4. Open items \u{2014} Unresolved questions, failures not yet understood, pending decisions.\n\n\
Rules:\n\
- Facts only. Do not invent, infer intent, or add advice.\n\
- Keep tool results only as far as they changed the state or a decision; do not copy raw output.\n\
- If a previous summary appears in the text, fold it in and keep its verbatim user quotes.\n\
- Be concise; omit pleasantries and narration. Respond with the summary only.";

/// Prompt to give to the summarizer: the override of the agent or of the host when it is not blank, otherwise
/// [`DEFAULT_SUMMARY_PROMPT`].
#[must_use]
pub fn summary_prompt(custom: Option<&str>) -> &str {
    match custom {
        Some(text) if !text.trim().is_empty() => text,
        _ => DEFAULT_SUMMARY_PROMPT,
    }
}

/// Produces the summary of a session. The host implements it, usually with a model call: the crate provides no
/// concrete summarizer. The returned future is `Send`.
pub trait Summarizer: Send + Sync {
    /// Summarizes `transcript`, which holds one `role: content` line per message, and returns the text of the summary.
    /// `digest` is given when the caller keeps one.
    ///
    /// # Errors
    ///
    /// Returns [`ContextError::Summarizer`](super::ContextError::Summarizer) when the summary cannot be produced.
    fn summarize(
        &self,
        transcript: &str,
        digest: Option<&Digest>,
    ) -> impl Future<Output = ContextResult<String>> + Send;
}

/// Chooses what is compacted and produces the messages that open the new segment. The returned future is `Send`.
pub trait CompactionStrategy: Send + Sync {
    /// Compacts `history`, and returns the messages that open the new segment in order: a `Compaction` boundary, then
    /// the summary as a user message. `digest` is appended to the summary when it is given. The history is not changed.
    ///
    /// # Errors
    ///
    /// Returns [`ContextError::PendingToolCall`](super::ContextError::PendingToolCall) when a tool call waits for its
    /// result, [`ContextError::EmptySummary`](super::ContextError::EmptySummary) when the summary is blank, and the
    /// error of the summarizer otherwise.
    fn compact(
        &self,
        history: &History,
        digest: Option<&Digest>,
    ) -> impl Future<Output = ContextResult<Vec<Message>>> + Send;
}

/// Default strategy: the whole session is sent to the summarizer, without pruning and without kept excerpts.
#[derive(Debug)]
#[non_exhaustive]
pub struct DefaultStrategy<S> {
    summarizer: S,
    replay: ReplayMode,
}

impl<S> DefaultStrategy<S> {
    /// Builds the default strategy over `summarizer`. The transcript carries the reasoning selected by `replay`.
    #[must_use]
    pub const fn new(summarizer: S, replay: ReplayMode) -> Self {
        Self { summarizer, replay }
    }
}

impl<S: Summarizer> CompactionStrategy for DefaultStrategy<S> {
    async fn compact(&self, history: &History, digest: Option<&Digest>) -> ContextResult<Vec<Message>> {
        if !history.pending_tool_calls().is_empty() {
            return Err(ContextError::PendingToolCall);
        }
        let replayed = replayed_reasoning(history.messages(), self.replay);
        let transcript = render_transcript(history.messages(), &replayed);
        let summary = self.summarizer.summarize(&transcript, digest).await?;
        if summary.trim().is_empty() {
            return Err(ContextError::EmptySummary);
        }
        Ok(vec![
            Message::boundary(BoundaryKind::Compaction),
            Message::user(summary_message(&summary, digest)),
        ])
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use serde_json::json;

    use super::{
        CompactionStrategy as _, DEFAULT_SUMMARY_PROMPT, DefaultStrategy, Digest, Summarizer, summary_prompt,
    };
    use crate::context::{ContextError, ContextResult, ReplayMode};
    use crate::session::{BoundaryKind, History, Message, Reasoning, ToolCall};

    /// One call received by the recording summarizer: the transcript and the digest.
    type Call = (String, Option<Digest>);

    /// Summarizer that records every transcript and digest it receives, and answers with a fixed text or fails.
    struct RecordingSummarizer {
        reply: Option<String>,
        received: Mutex<Vec<Call>>,
    }

    impl RecordingSummarizer {
        fn answering(text: &str) -> Self {
            Self {
                reply: Some(text.to_owned()),
                received: Mutex::new(Vec::new()),
            }
        }

        fn failing() -> Self {
            Self {
                reply: None,
                received: Mutex::new(Vec::new()),
            }
        }

        fn calls(&self) -> Vec<Call> {
            self.received.lock().expect("the lock is not poisoned").clone()
        }

        /// The transcript of the first call, or an empty text when the summarizer was never called.
        fn first_transcript(&self) -> String {
            self.calls()
                .first()
                .map_or_else(String::new, |call| call.0.clone())
        }
    }

    impl Summarizer for RecordingSummarizer {
        fn summarize(
            &self,
            transcript: &str,
            digest: Option<&Digest>,
        ) -> impl Future<Output = ContextResult<String>> + Send {
            self.received
                .lock()
                .expect("the lock is not poisoned")
                .push((transcript.to_owned(), digest.copied()));
            std::future::ready(self.reply.as_ref().map_or_else(
                || Err(ContextError::Summarizer("the model is unavailable".to_owned())),
                |text| Ok(text.clone()),
            ))
        }
    }

    fn assert_send<F: Future + Send>(_: &F) {}

    fn four_messages() -> History {
        let mut history = History::new();
        history.push(Message::user("q1"));
        history.push(Message::assistant("a1", None, Vec::new()));
        history.push(Message::user("q2"));
        history.push(Message::assistant("a2", None, Vec::new()));
        history
    }

    fn summary_content(messages: &[Message]) -> Option<&str> {
        match messages.get(1) {
            Some(Message::User { content }) => Some(content.as_str()),
            _ => None,
        }
    }

    #[tokio::test]
    async fn default_strategy_sends_the_whole_session() {
        let strategy = DefaultStrategy::new(
            RecordingSummarizer::answering("the summary"),
            ReplayMode::AllTurns,
        );
        let history = four_messages();
        let future = strategy.compact(&history, None);
        assert_send(&future);
        let messages = future.await;
        assert_eq!(
            strategy.summarizer.first_transcript(),
            "user: q1\nassistant: a1\nuser: q2\nassistant: a2",
            "the summarizer receives the four messages, in order, one role-prefixed line each"
        );
        let messages = messages.unwrap_or_default();
        assert!(
            matches!(
                messages.first(),
                Some(Message::Boundary {
                    kind: BoundaryKind::Compaction
                })
            ),
            "the new segment opens with a Compaction boundary, got {messages:?}"
        );
        assert_eq!(
            messages.len(),
            2,
            "the boundary is followed by one summary message"
        );
        assert!(
            summary_content(&messages).is_some_and(|content| content.contains("the summary")),
            "the summary is a user message that carries the summary text"
        );
    }

    #[tokio::test]
    async fn reasoning_follows_the_replay_mode() {
        let mut history = History::new();
        history.push(Message::user("question"));
        history.push(Message::assistant(
            "answer",
            Some(Reasoning::new("provider-a", "thinking")),
            Vec::new(),
        ));
        let with_replay =
            DefaultStrategy::new(RecordingSummarizer::answering("summary"), ReplayMode::AllTurns);
        let without_replay =
            DefaultStrategy::new(RecordingSummarizer::answering("summary"), ReplayMode::None);
        let replayed = with_replay.compact(&history, None).await;
        let not_replayed = without_replay.compact(&history, None).await;
        assert!(
            replayed.is_ok() && not_replayed.is_ok(),
            "both compactions succeed"
        );
        assert!(
            with_replay.summarizer.first_transcript().contains("thinking"),
            "the reasoning is in the transcript with AllTurns"
        );
        assert!(
            !without_replay.summarizer.first_transcript().contains("thinking"),
            "the reasoning is absent from the transcript with None"
        );
    }

    #[tokio::test]
    async fn tool_calls_and_tool_names_are_in_the_transcript() {
        let mut history = History::new();
        history.push(Message::user("run it"));
        history.push(Message::assistant(
            "",
            None,
            vec![ToolCall::new("call-1", "rhai_pad", json!({"script": "1+1"}))],
        ));
        history.push(Message::tool_result("call-1", "rhai_pad", "2", None));
        let strategy = DefaultStrategy::new(RecordingSummarizer::answering("summary"), ReplayMode::AllTurns);
        let result = strategy.compact(&history, None).await;
        assert!(result.is_ok(), "the compaction succeeds, got {result:?}");
        assert_eq!(
            strategy.summarizer.first_transcript(),
            "user: run it\nassistant: \nassistant tool call rhai_pad: {\"script\":\"1+1\"}\ntool rhai_pad: 2",
            "the transcript gives the tool call with its name and arguments, then the result with the tool name, in order"
        );
    }

    #[tokio::test]
    async fn no_compaction_between_a_tool_call_and_its_result() {
        let mut history = History::new();
        history.push(Message::user("read the notes"));
        history.push(Message::assistant(
            "",
            None,
            vec![ToolCall::new("call-1", "read_file", json!({"path": "notes.md"}))],
        ));
        let strategy = DefaultStrategy::new(RecordingSummarizer::answering("summary"), ReplayMode::AllTurns);
        let result = strategy.compact(&history, None).await;
        assert!(
            matches!(result, Err(ContextError::PendingToolCall)),
            "a history ending with a tool call without result is refused, got {result:?}"
        );
        assert!(
            strategy.summarizer.calls().is_empty(),
            "the summarizer must not be called for a refused compaction"
        );
    }

    #[tokio::test]
    async fn digest_carries_counts_only() {
        let digest = Digest::new(3, 7);
        let strategy = DefaultStrategy::new(
            RecordingSummarizer::answering("the summary"),
            ReplayMode::AllTurns,
        );
        let history = four_messages();
        let messages = strategy
            .compact(&history, Some(&digest))
            .await
            .unwrap_or_default();
        let rendered = digest.render();
        assert!(
            rendered.contains('3') && rendered.contains('7'),
            "the rendered digest gives the 3 open tasks and the 7 memories, got {rendered:?}"
        );
        assert!(!rendered.contains('\n'), "the digest is one line");
        assert!(
            summary_content(&messages).is_some_and(|content| content.ends_with(rendered.as_str())),
            "the rendered digest is appended to the end of the summary message"
        );
        assert_eq!(
            strategy.summarizer.calls().first().and_then(|call| call.1),
            Some(digest),
            "the summarizer receives the digest"
        );
    }

    #[tokio::test]
    async fn empty_summary_refused() {
        let strategy = DefaultStrategy::new(RecordingSummarizer::answering(" \n\t "), ReplayMode::AllTurns);
        let history = four_messages();
        let result = strategy.compact(&history, None).await;
        assert!(
            matches!(result, Err(ContextError::EmptySummary)),
            "a blank summary is refused, got {result:?}"
        );
    }

    #[tokio::test]
    async fn summarizer_failure_is_reported() {
        let strategy = DefaultStrategy::new(RecordingSummarizer::failing(), ReplayMode::AllTurns);
        let history = four_messages();
        let before = history.clone();
        let result = strategy.compact(&history, None).await;
        assert!(
            matches!(result, Err(ContextError::Summarizer(_))),
            "the failure of the summarizer is reported, got {result:?}"
        );
        assert_eq!(history, before, "the history given is unchanged");
    }

    // Must: multiline content is rendered on several lines without escaping, each line kept as written.
    #[tokio::test]
    async fn multiline_content_stays_readable() {
        let mut history = History::new();
        history.push(Message::user("line one\nline two"));
        history.push(Message::assistant("first\nsecond\n\nfourth", None, Vec::new()));
        let strategy = DefaultStrategy::new(RecordingSummarizer::answering("summary"), ReplayMode::AllTurns);
        let result = strategy.compact(&history, None).await;
        assert!(result.is_ok(), "the compaction succeeds, got {result:?}");
        assert_eq!(
            strategy.summarizer.first_transcript(),
            "user: line one\nline two\nassistant: first\nsecond\n\nfourth",
            "the multiline contents are given as written, each message still opening with its role line"
        );
    }

    // Must: the reasoning is given on the line before its assistant message, in the exact form `reasoning: <text>`.
    #[tokio::test]
    async fn reasoning_line_has_the_exact_form() {
        let mut history = History::new();
        history.push(Message::user("question"));
        history.push(Message::assistant(
            "answer",
            Some(Reasoning::new("provider-a", "thinking")),
            Vec::new(),
        ));
        let strategy = DefaultStrategy::new(RecordingSummarizer::answering("summary"), ReplayMode::AllTurns);
        let result = strategy.compact(&history, None).await;
        assert!(result.is_ok(), "the compaction succeeds, got {result:?}");
        assert_eq!(
            strategy.summarizer.first_transcript(),
            "user: question\nreasoning: thinking\nassistant: answer",
            "the reasoning line is `reasoning: ` followed by the text, and comes before the assistant line"
        );
    }

    // Must: @DefaultStrategy does not change the history it receives, on the success path too.
    #[tokio::test]
    async fn history_is_unchanged_on_success() {
        let history = four_messages();
        let before = history.clone();
        let strategy = DefaultStrategy::new(
            RecordingSummarizer::answering("the summary"),
            ReplayMode::AllTurns,
        );
        let result = strategy.compact(&history, None).await;
        assert!(result.is_ok(), "the compaction succeeds, got {result:?}");
        assert_eq!(
            history, before,
            "the history given is unchanged after a successful compaction"
        );
    }

    // Scenario: the default prompt demands verbatim quotation.
    #[test]
    fn default_prompt_demands_verbatim_quotation() {
        let prompt = DEFAULT_SUMMARY_PROMPT;
        assert!(
            prompt.contains("VERBATIM"),
            "the prompt demands verbatim quotation"
        );
        for section in ["User requests", "Current state", "Findings", "Open items"] {
            assert!(
                prompt.contains(section),
                "the prompt names the section `{section}`"
            );
        }
        for separation in [
            "original messages.\n\nWrite a summary",
            "in these sections:\n\n1. User requests",
            "pending decisions.\n\nRules:",
        ] {
            assert!(
                prompt.contains(separation),
                "the prompt has a blank line at {separation:?}"
            );
        }
        for consecutive in [
            "\n2. Current state",
            "\n3. Findings",
            "\n4. Open items",
            "\n- Facts only",
        ] {
            assert!(
                prompt.contains(consecutive) && !prompt.contains(&format!("\n{consecutive}")),
                "the line of {consecutive:?} follows the previous line without a blank line"
            );
        }
        assert!(
            prompt.contains(
                "If a previous summary appears in the text, fold it in and keep its verbatim user quotes."
            ),
            "the prompt tells the summarizer to keep the verbatim quotes of a previous summary"
        );
    }

    // Scenario: the override replaces the default, a blank override does not.
    #[test]
    fn override_replaces_the_default_but_a_blank_one_does_not() {
        assert_eq!(
            summary_prompt(Some("custom")),
            "custom",
            "a non-blank override is used"
        );
        assert_eq!(
            summary_prompt(Some("")),
            DEFAULT_SUMMARY_PROMPT,
            "an empty override falls back to the default prompt"
        );
        assert_eq!(
            summary_prompt(Some("   ")),
            DEFAULT_SUMMARY_PROMPT,
            "a blank override falls back to the default prompt"
        );
        assert_eq!(
            summary_prompt(None),
            DEFAULT_SUMMARY_PROMPT,
            "no override gives the default prompt"
        );
    }
}
