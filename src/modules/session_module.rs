//! Rhai module `session`: the model sets the title of its own session and may flag the session for analysis by the
//! configuration agent (`agent-sessions/session#FR-2`, `agent-sessions#BR-8`). The module only fills a
//! [`SessionEdits`] cell; the turn loop normalizes, masks and persists what it holds.

use std::sync::Arc;

use rhai::{Dynamic, EvalAltResult, FuncRegistration, ImmutableString, Module, Position};

use crate::session::{SessionEdits, normalize_title};

/// Builds the Rhai module `session`: `set_title(text)`, `title()` and `flag(reason)`.
///
/// `edits` receives the requests; `current_title` reads the title the session has now, supplied by the host. Grant it to a
/// work or configuration agent as the module `session`; a sub-agent never receives it.
#[must_use]
pub fn session_module<T>(edits: Arc<SessionEdits>, current_title: T) -> Module
where
    T: Fn() -> Option<String> + Send + Sync + 'static,
{
    let mut module = Module::new();
    let title_edits = Arc::clone(&edits);
    let _set_title = FuncRegistration::new("set_title")
        .with_params_info(["text: string", "()"])
        .with_comments([
            "/// Sets the title of this session: a short line that tells a person what the session is about.",
            "/// `text` is cut to one line of at most 120 characters. The last call wins. Fails when `text` is blank.",
        ])
        .set_into_module(
            &mut module,
            move |text: ImmutableString| -> Result<(), Box<EvalAltResult>> {
                if normalize_title(text.as_str()).is_none() {
                    return Err(Box::new(EvalAltResult::ErrorRuntime(
                        Dynamic::from("session::set_title: the title must not be blank".to_owned()),
                        Position::NONE,
                    )));
                }
                title_edits.set_title(text.as_str());
                Ok(())
            },
        );
    let _title = FuncRegistration::new("title")
        .with_params_info(["Dynamic"])
        .with_comments(["/// The current title of this session, or `()` when none is set."])
        .set_into_module(&mut module, move || -> Dynamic {
            current_title().map_or(Dynamic::UNIT, Dynamic::from)
        });
    let _flag = FuncRegistration::new("flag")
        .with_params_info(["reason: string", "()"])
        .with_comments([
            "/// Flags this session for analysis by the configuration agent, with a short `reason` (cut to 200 characters).",
            "/// The flag cannot be withdrawn.",
        ])
        .set_into_module(&mut module, move |reason: ImmutableString| {
            edits.flag_for_review(reason.as_str());
        });
    module
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use rhai::Shared;

    use super::session_module;
    use crate::clock::{ManualClock, Timestamp};
    use crate::scratchpad::{
        GrantedModule, Grants, RunEnv, RunOutcome, RunStatus, Scratchpad, ScratchpadEngine, ScratchpadLimits,
    };
    use crate::session::{CancelHandle, Redactor, SessionEdits};

    /// A pad granted the module `session` over `edits`, whose current title is `current`.
    fn pad(edits: &Arc<SessionEdits>, current: Option<&str>) -> Scratchpad {
        let current = current.map(str::to_owned);
        let module = session_module(Arc::clone(edits), move || current.clone());
        let grants = Grants::new()
            .with_module(GrantedModule::new("session", Shared::new(module)).expect("a valid module name"))
            .expect("a distinct module name");
        let engine = ScratchpadEngine::builder(ScratchpadLimits::new(), grants)
            .build()
            .expect("the engine builds");
        Scratchpad::new(Arc::new(engine))
    }

    async fn run(pad: &Scratchpad, script: &str) -> RunOutcome {
        let redactor = Redactor::new();
        let clock = ManualClock::new(Timestamp::from_millis(0));
        let cancel = CancelHandle::new();
        let env = RunEnv::new("agent", &redactor, &clock);
        pad.run(script, &cancel, &env).await
    }

    // Scenario: le LLM pose le titre et le relit
    #[tokio::test]
    async fn the_model_sets_the_title_and_reads_it_back() {
        let edits = Arc::new(SessionEdits::new());
        let pad = pad(&edits, Some("Current title"));
        let outcome = run(&pad, "session::set_title(\"Migration loki\"); session::title()").await;
        assert_eq!(
            outcome.status(),
            RunStatus::Ok,
            "the script must run: {}",
            outcome.text()
        );
        assert!(
            outcome.text().contains("Current title"),
            "title() must return the current title of the reader: {}",
            outcome.text()
        );
        assert_eq!(
            edits.take().title(),
            Some("Migration loki"),
            "the cell must carry the requested title"
        );
    }

    #[tokio::test]
    async fn title_is_unit_when_none_is_set() {
        let edits = Arc::new(SessionEdits::new());
        let pad = pad(&edits, None);
        let outcome = run(&pad, "type_of(session::title())").await;
        assert!(
            outcome.text().contains("()"),
            "no title must read as the unit value: {}",
            outcome.text()
        );
    }

    // Scenario: un titre vide est une erreur de script
    #[tokio::test]
    async fn a_blank_title_is_a_script_error() {
        let edits = Arc::new(SessionEdits::new());
        let pad = pad(&edits, None);
        for script in ["session::set_title(\"   \")", "session::set_title(\"\")"] {
            let outcome = run(&pad, script).await;
            assert!(
                matches!(outcome.status(), RunStatus::Failed(_)),
                "`{script}` must fail, got {:?}",
                outcome.status()
            );
            assert!(
                outcome.text().contains("title"),
                "the error must tell the title is blank: {}",
                outcome.text()
            );
        }
        assert!(edits.take().is_empty(), "a refused title must not reach the cell");
    }

    // Scenario: le signalement porte une raison
    #[tokio::test]
    async fn a_flag_carries_its_reason() {
        let edits = Arc::new(SessionEdits::new());
        let pad = pad(&edits, None);
        let outcome = run(&pad, "session::flag(\"r\u{e9}ponse fausse sur le pod loki\")").await;
        assert_eq!(
            outcome.status(),
            RunStatus::Ok,
            "the script must run: {}",
            outcome.text()
        );
        assert_eq!(
            edits.take().reviews(),
            ["r\u{e9}ponse fausse sur le pod loki".to_owned()],
            "the cell must carry the flag with its reason"
        );
    }

    // Scenario: le module se découvre
    #[tokio::test]
    async fn the_module_is_discoverable() {
        let edits = Arc::new(SessionEdits::new());
        let pad = pad(&edits, None);
        let found = run(
            &pad,
            r#"let ok = false; for r in find("title") { if r.path == "session::set_title" { ok = true; } } ok"#,
        )
        .await;
        assert!(
            found.text().contains("true"),
            "find(\"title\") must list session::set_title: {}",
            found.text()
        );
        let listed = run(
            &pad,
            r#"let paths = []; let documented = true;
               for f in functions("session") { paths.push(f.path); if f.doc == () || f.doc.len() == 0 { documented = false; } }
               paths.sort(); `${paths} ${documented}`"#,
        )
        .await;
        assert!(
            listed.text().contains("session::set_title")
                && listed.text().contains("session::title")
                && listed.text().contains("session::flag")
                && listed.text().contains("true"),
            "functions(\"session\") must list the three documented functions: {}",
            listed.text()
        );
    }
}
