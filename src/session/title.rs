//! Title of a session: set by the model through the `session` module or by the host, bounded and masked, with a default
//! computed from the metadata when nobody set one (`agent-sessions/session#FR-2`, `agent-sessions#CON-3`). The last writer
//! wins.

use super::{Message, Redactor, SessionState};

/// Longest title, in characters, ellipsis included.
const MAX_TITLE_CHARS: usize = 120;

/// Ellipsis that ends a text cut to its bound.
const ELLIPSIS: char = '\u{2026}';

/// Separator of the sections of a default title: a middle dot between spaces.
const SECTION_SEPARATOR: &str = " \u{b7} ";

/// Separator of the first message excerpt in a default title: an em dash between spaces.
const EXCERPT_SEPARATOR: &str = " \u{2014} ";

/// Longest excerpt of the first user message in a default title, in characters.
const MAX_EXCERPT_CHARS: usize = 60;

/// Normalizes a title: one line, no control character, single spaces, trimmed, bounded to 120 characters with a final `…`
/// when cut. Returns `None` when nothing is left.
#[must_use]
pub fn normalize_title(text: &str) -> Option<String> {
    single_line(text, MAX_TITLE_CHARS)
}

/// Reduces `text` to one line of at most `max_chars` characters, `…` included when it was cut. Returns `None` when nothing
/// is left.
pub(in crate::session) fn single_line(text: &str, max_chars: usize) -> Option<String> {
    let spaced: String = text
        .chars()
        .map(|character| if character.is_control() { ' ' } else { character })
        .collect();
    let line = spaced.split_whitespace().collect::<Vec<_>>().join(" ");
    if line.is_empty() {
        return None;
    }
    if line.chars().count() <= max_chars {
        return Some(line);
    }
    let kept: String = line.chars().take(max_chars.saturating_sub(1)).collect();
    Some(format!("{}{ELLIPSIS}", kept.trim_end()))
}

/// Integer quotient of `dividend` by the non-zero constant `divisor`.
fn quotient(dividend: u64, divisor: u64) -> u64 {
    dividend.checked_div(divisor).unwrap_or(0)
}

/// Integer remainder of `dividend` by the non-zero constant `divisor`.
fn remainder(dividend: u64, divisor: u64) -> u64 {
    dividend.checked_rem(divisor).unwrap_or(0)
}

/// Civil date `YYYY-MM-DD` (UTC) of an instant given in milliseconds since the Unix epoch.
fn civil_date(millis: u64) -> String {
    let days = quotient(millis, 86_400_000);
    let shifted = days.saturating_add(719_468);
    let era = quotient(shifted, 146_097);
    let day_of_era = remainder(shifted, 146_097);
    let year_of_era = quotient(
        day_of_era
            .saturating_sub(quotient(day_of_era, 1_460))
            .saturating_add(quotient(day_of_era, 36_524))
            .saturating_sub(quotient(day_of_era, 146_096)),
        365,
    );
    let day_of_year = day_of_era.saturating_sub(
        year_of_era
            .saturating_mul(365)
            .saturating_add(quotient(year_of_era, 4))
            .saturating_sub(quotient(year_of_era, 100)),
    );
    let month_index = quotient(day_of_year.saturating_mul(5).saturating_add(2), 153);
    let day = day_of_year
        .saturating_sub(quotient(month_index.saturating_mul(153).saturating_add(2), 5))
        .saturating_add(1);
    let month = if month_index < 10 {
        month_index.saturating_add(3)
    } else {
        month_index.saturating_sub(9)
    };
    let year = year_of_era.saturating_add(era.saturating_mul(400));
    let year = if month <= 2 { year.saturating_add(1) } else { year };
    format!("{year:04}-{month:02}-{day:02}")
}

/// Sets `slot` to the normalized `text`. Returns the title stored, or `None` when nothing is left (`slot` unchanged).
pub(in crate::session) fn set<'slot>(slot: &'slot mut Option<String>, text: &str) -> Option<&'slot str> {
    let title = normalize_title(text)?;
    Some(slot.insert(title).as_str())
}

/// Sets `slot` from a raw text: masked, normalized, masked again (cutting a long text must not leave the start of a
/// secret). Returns the title stored, or `None` when nothing is left (`slot` unchanged).
pub(in crate::session) fn set_masked<'slot>(
    slot: &'slot mut Option<String>,
    text: &str,
    redactor: &Redactor,
) -> Option<&'slot str> {
    let title = normalize_title(&redactor.redact(text)).map(|title| redactor.redact(&title))?;
    set(slot, &title)
}

/// The title to show for `state`: the one set, or a default `<agent> · <context> · <date> — <start of the first user
/// message>`. Masked by `redactor`.
pub(in crate::session) fn display(state: &SessionState, redactor: &Redactor) -> String {
    if let Some(title) = state.title() {
        return redactor.redact(title);
    }
    let mut sections = vec![state.agent().name().to_owned()];
    if !state.toolset_context().is_empty() {
        sections.push(state.toolset_context().to_owned());
    }
    sections.push(civil_date(state.created_at().as_millis()));
    let mut title = sections.join(SECTION_SEPARATOR);
    let first_message = state.history().messages().iter().find_map(|message| {
        if let Message::User { content } = message {
            Some(content.as_str())
        } else {
            None
        }
    });
    if let Some(excerpt) =
        first_message.and_then(|content| single_line(&redactor.redact(content), MAX_EXCERPT_CHARS))
    {
        title.push_str(EXCERPT_SEPARATOR);
        title.push_str(&excerpt);
    }
    let masked = redactor.redact(&title);
    single_line(&masked, MAX_TITLE_CHARS).map_or_else(String::new, |bounded| redactor.redact(&bounded))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use serde_json::json;

    use super::normalize_title;
    use crate::clock::Timestamp;
    use crate::session::{AgentRef, Message, Redactor, SessionId, SessionState};

    fn state_at(millis: u64) -> SessionState {
        SessionState::new(
            SessionId::new("s-1").expect("a non-empty text makes a session identifier"),
            AgentRef::new("ops-agent", "sha-1"),
            "prod-k8s",
            BTreeMap::new(),
            Timestamp::from_millis(millis),
        )
    }

    // Scenario: un titre est normalisé et borné
    #[test]
    fn title_is_normalized_and_bounded() {
        assert_eq!(
            normalize_title("  Plan\nde   migration\t").as_deref(),
            Some("Plan de migration"),
            "newlines, tabs and runs of spaces must become single spaces, edges trimmed"
        );
        let long = "a".repeat(300);
        let bounded = normalize_title(&long).expect("a long text makes a title");
        assert_eq!(
            bounded.chars().count(),
            120,
            "a title is bounded to 120 characters"
        );
        assert!(bounded.ends_with('\u{2026}'), "a cut title ends with an ellipsis");
        assert_eq!(normalize_title(" \n\t "), None, "a blank text makes no title");
        assert_eq!(normalize_title(""), None, "an empty text makes no title");
        assert_eq!(
            normalize_title("a\u{7}b\u{0}c").as_deref(),
            Some("a b c"),
            "control characters become spaces"
        );
        let exact = "\u{e9}".repeat(120);
        assert_eq!(
            normalize_title(&exact).as_deref(),
            Some(exact.as_str()),
            "a title of exactly 120 characters (not bytes) is kept whole"
        );
    }

    // Scenario: le dernier écrivain gagne
    #[test]
    fn last_writer_wins() {
        let mut state = state_at(0);
        assert_eq!(state.title(), None, "a new session has no title");
        assert_eq!(state.set_title("from the model"), Some("from the model"));
        assert_eq!(state.set_title("from the host"), Some("from the host"));
        assert_eq!(state.set_title("model again"), Some("model again"));
        assert_eq!(state.title(), Some("model again"), "the last title set must win");
        assert_eq!(state.set_title("  \n "), None, "a blank title is refused");
        assert_eq!(
            state.title(),
            Some("model again"),
            "a refused title leaves the state unchanged"
        );
    }

    // Scenario: un secret n'entre jamais dans un titre
    #[test]
    fn display_title_masks_a_secret() {
        let mut redactor = Redactor::new();
        redactor
            .add_secret("s3cr3t-token")
            .expect("a long enough secret is accepted");
        let mut state = state_at(0);
        let _title = state.set_title("deploy with s3cr3t-token now");
        let shown = state.display_title(&redactor);
        assert!(
            !shown.contains("s3cr3t-token"),
            "the displayed title must not hold the secret: {shown}"
        );
        assert!(
            shown.contains("*redacted secret*"),
            "the secret must be masked, not dropped: {shown}"
        );

        let mut by_message = state_at(0);
        by_message.push_message(Message::user("why does s3cr3t-token fail"));
        let default = by_message.display_title(&redactor);
        assert!(
            !default.contains("s3cr3t-token"),
            "the default title must not hold the secret: {default}"
        );
        assert!(
            !format!("{state:?}{by_message:?}").contains("s3cr3t-token"),
            "the debug output never shows the title"
        );
    }

    // Scenario: titre par défaut sans titre posé
    #[test]
    fn default_title_from_metadata() {
        let mut state = state_at(1_700_000_000_000);
        assert_eq!(
            state.display_title(&Redactor::new()),
            "ops-agent \u{b7} prod-k8s \u{b7} 2023-11-14",
            "without a message the last section is omitted"
        );
        state.push_message(Message::user("pourquoi loki red\u{e9}marre"));
        state.push_message(Message::user("second message ignored"));
        assert_eq!(
            state.display_title(&Redactor::new()),
            "ops-agent \u{b7} prod-k8s \u{b7} 2023-11-14 \u{2014} pourquoi loki red\u{e9}marre",
            "the default title names agent, context, start date and the first user message"
        );
        assert_eq!(
            state.title(),
            None,
            "reading the display title does not set the title"
        );
    }

    #[test]
    fn default_title_excerpt_and_total_are_bounded() {
        let mut state = state_at(0);
        state.push_message(Message::user(format!("line one\n{}", "x".repeat(200))));
        let shown = state.display_title(&Redactor::new());
        let (head, excerpt) = shown
            .split_once(" \u{2014} ")
            .expect("the excerpt follows the date");
        assert_eq!(
            head, "ops-agent \u{b7} prod-k8s \u{b7} 1970-01-01",
            "the epoch dates 1970-01-01"
        );
        assert_eq!(
            excerpt.chars().count(),
            60,
            "the excerpt is bounded to 60 characters"
        );
        assert!(!excerpt.contains('\n'), "the excerpt is on one line");
        assert!(
            shown.chars().count() <= 120,
            "the whole default title is bounded to 120 characters"
        );
    }

    #[test]
    fn default_title_dates_a_leap_day() {
        // 2024-02-29T12:00:00Z
        let state = state_at(1_709_208_000_000);
        assert!(
            state
                .display_title(&Redactor::new())
                .ends_with("\u{b7} 2024-02-29"),
            "the start date must be the civil date of the creation instant"
        );
    }

    #[test]
    fn a_set_title_is_shown_as_is() {
        let mut state = state_at(1_700_000_000_000);
        state.push_message(Message::user("hello"));
        let _title = state.set_title("My title");
        assert_eq!(
            state.display_title(&Redactor::new()),
            "My title",
            "a set title wins over the default"
        );
    }

    // Scenario: un état ancien sans titre se relit
    #[test]
    fn title_persists_and_an_old_state_reads_without_title() {
        let mut state = state_at(5);
        let old = state.to_json().expect("the state serializes");
        assert!(
            !old.contains("title"),
            "a state without title writes no title field: {old}"
        );
        let back = SessionState::from_json(&old).expect("an old state loads");
        assert_eq!(back.title(), None, "an old state has no title");
        assert_eq!(back, state, "the rest of the state is identical");

        let _title = state.set_title("Migration loki");
        let text = state.to_json().expect("the state serializes");
        let reread = SessionState::from_json(&text).expect("the state loads");
        assert_eq!(
            reread.title(),
            Some("Migration loki"),
            "the title survives a round trip"
        );
        assert_eq!(reread, state, "the whole state is identical");

        let document = json!({
            "id": "s-1",
            "agent": {"name": "a", "fingerprint": "f"},
            "format": {"major": 1, "minor": 0},
        })
        .to_string();
        assert_eq!(
            SessionState::from_json(&document)
                .expect("a minimal document loads")
                .title(),
            None,
            "a document without the field reads without title"
        );
    }

    /// Fails when `text` holds a piece of `secret` of six characters or more: a cut secret is still a partial secret.
    fn assert_no_fragment(text: &str, secret: &str) {
        let chars: Vec<char> = secret.chars().collect();
        for window in chars.windows(6) {
            let piece: String = window.iter().collect();
            assert!(
                !text.contains(&piece),
                "the piece `{piece}` of the secret appears in: {text}"
            );
        }
    }

    fn redactor_with(secret: &str) -> Redactor {
        let mut redactor = Redactor::new();
        redactor.add_secret(secret).expect("a long enough secret");
        redactor
    }

    // Mutant: the first masking removed from set_masked. The secret straddles the 120th character.
    #[test]
    fn a_secret_straddling_the_title_bound_leaves_no_piece() {
        let secret = "s3cr3t-token";
        let redactor = redactor_with(secret);
        let mut state = state_at(0);
        let raw = format!("{}{secret} and more words after it", "x".repeat(110));
        let stored = state
            .set_title_masked(&raw, &redactor)
            .map(str::to_owned)
            .expect("a title is set");
        assert_no_fragment(&stored, secret);
        assert_no_fragment(state.title().unwrap_or_default(), secret);
        assert_no_fragment(&state.display_title(&redactor), secret);
    }

    // Mutant: the second masking removed from set_masked. The normalization rebuilds the secret `ab cd ef`.
    #[test]
    fn a_secret_rebuilt_by_the_normalization_is_masked_in_a_title() {
        let redactor = redactor_with("ab cd ef");
        for raw in ["ab\ncd ef", "ab  cd   ef", "ab\tcd\n\nef"] {
            let mut state = state_at(0);
            let stored = state
                .set_title_masked(raw, &redactor)
                .map(str::to_owned)
                .expect("a title is set");
            assert!(
                !stored.contains("ab cd ef"),
                "the rebuilt secret is masked in `{stored}`"
            );
            assert_eq!(stored, "*redacted secret*", "only the mask is left");
        }
    }

    // Mutant: the first masking of the excerpt removed from display. The secret straddles the 60th character.
    #[test]
    fn a_secret_straddling_the_excerpt_bound_leaves_no_piece() {
        let secret = "s3cr3t-token";
        let redactor = redactor_with(secret);
        let mut state = state_at(0);
        state.push_message(Message::user(format!("{}{secret} tail", "y".repeat(50))));
        assert_no_fragment(&state.display_title(&redactor), secret);
    }

    // Mutant: the second masking removed from display. The first message rebuilds the secret once normalized.
    #[test]
    fn a_secret_rebuilt_in_the_first_message_is_masked_in_the_default_title() {
        let redactor = redactor_with("ab cd ef");
        for raw in ["ab\ncd ef", "ab  cd  ef"] {
            let mut state = state_at(0);
            state.push_message(Message::user(raw));
            let shown = state.display_title(&redactor);
            assert!(
                !shown.contains("ab cd ef"),
                "the rebuilt secret is masked in `{shown}`"
            );
        }
    }

    // Mutant: the masking of the whole default title removed before the cut at 120 characters. The secret is in the agent name.
    #[test]
    fn a_secret_straddling_the_default_title_bound_leaves_no_piece() {
        let secret = "s3cr3t-token";
        let redactor = redactor_with(secret);
        let state = SessionState::new(
            SessionId::new("s-1").expect("a non-empty text makes a session identifier"),
            AgentRef::new(format!("{}{secret}", "n".repeat(112)), "sha-1"),
            "prod-k8s",
            BTreeMap::new(),
            Timestamp::from_millis(0),
        );
        assert_no_fragment(&state.display_title(&redactor), secret);
    }
}
