//! Shared cell through which a Rhai module asks the session for a state change it cannot make itself: setting the title,
//! flagging the session for analysis. The turn loop empties it after each tool call and applies the changes
//! (`agent-sessions#CON-1`).

use std::fmt;
use std::sync::{Mutex, MutexGuard, PoisonError};

/// Changes requested since the last [`SessionEdits::take`]: the title asked for, if any, and the review requests in order.
/// The texts are raw: nothing is normalized nor masked yet.
#[derive(Default, Clone, PartialEq, Eq)]
pub struct PendingEdits {
    title: Option<String>,
    reviews: Vec<String>,
}

impl PendingEdits {
    /// Returns the title asked for, the last one when asked several times.
    #[must_use]
    pub fn title(&self) -> Option<&str> {
        self.title.as_deref()
    }

    /// Returns the reasons of the review requests, in the order they were made.
    #[must_use]
    pub const fn reviews(&self) -> &[String] {
        self.reviews.as_slice()
    }

    /// Tells whether nothing was requested.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.title.is_none() && self.reviews.is_empty()
    }
}

/// The Debug output shows what was requested, never the texts, which may hold a secret.
impl fmt::Debug for PendingEdits {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PendingEdits")
            .field("title_requested", &self.title.is_some())
            .field("reviews", &self.reviews.len())
            .finish()
    }
}

/// Cell of pending session edits, shared (`Arc`) between the `session` module and the turn loop.
#[derive(Default)]
pub struct SessionEdits {
    pending: Mutex<PendingEdits>,
}

impl SessionEdits {
    /// Builds an empty cell.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Locks the cell, recovering a poisoned lock.
    fn lock(&self) -> MutexGuard<'_, PendingEdits> {
        self.pending.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Asks for the title `text`, raw. The last request wins before the cell is emptied.
    pub fn set_title(&self, text: &str) {
        self.lock().title = Some(text.to_owned());
    }

    /// Asks for the session to be flagged for analysis, with a raw `reason` that may be empty.
    pub fn flag_for_review(&self, reason: &str) {
        self.lock().reviews.push(reason.to_owned());
    }

    /// Empties the cell and returns what it held: every change is returned once.
    #[must_use]
    pub fn take(&self) -> PendingEdits {
        std::mem::take(&mut *self.lock())
    }
}

impl fmt::Debug for SessionEdits {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SessionEdits").finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::thread;

    use super::{PendingEdits, SessionEdits};

    // Scenario: la cellule est vidée une fois
    #[test]
    fn the_cell_is_emptied_once() {
        let edits = SessionEdits::new();
        edits.set_title("first");
        edits.flag_for_review("slow");
        edits.set_title("second");
        edits.flag_for_review("wrong");
        let first = edits.take();
        assert_eq!(
            first.title(),
            Some("second"),
            "the last requested title must be returned"
        );
        assert_eq!(
            first.reviews(),
            ["slow".to_owned(), "wrong".to_owned()],
            "both review requests must be returned in order"
        );
        assert!(!first.is_empty(), "a cell that held changes is not empty");
        let second = edits.take();
        assert!(second.is_empty(), "the second take must find the cell empty");
        assert_eq!(
            second,
            PendingEdits::default(),
            "an emptied cell equals the default"
        );
    }

    #[test]
    fn an_empty_reason_is_still_a_request() {
        let edits = SessionEdits::new();
        edits.flag_for_review("");
        assert_eq!(
            edits.take().reviews(),
            [String::new()],
            "an empty reason is kept as a request"
        );
    }

    #[test]
    fn the_cell_is_shareable_and_survives_a_poisoned_lock() {
        let edits = Arc::new(SessionEdits::new());
        let worker = Arc::clone(&edits);
        let outcome = thread::spawn(move || {
            let _guard = worker.pending.lock();
            std::panic::resume_unwind(Box::new("poison the lock"));
        })
        .join();
        assert!(outcome.is_err(), "the worker thread panicked on purpose");
        edits.set_title("after poison");
        assert_eq!(
            edits.take().title(),
            Some("after poison"),
            "a poisoned lock must be recovered, never panic"
        );
    }

    #[test]
    fn debug_shows_no_text() {
        let edits = SessionEdits::new();
        edits.set_title("hidden-title-text");
        edits.flag_for_review("hidden-reason-text");
        let pending = edits.take();
        let shown = format!("{pending:?}{edits:?}");
        assert!(
            !shown.contains("hidden"),
            "the debug output must not show the texts: {shown}"
        );
    }
}
