//! System messages telling the LLM, at restore, what changed since the save: a different major version of a toolset, a changed
//! agent definition, a toolset older than the saved one, broken functions. The session always resumes with the current
//! definitions (`agent-sessions/session#FR-10`, `agent-sessions/session#FR-3`, `agent-sessions/session#AC-2`,
//! `agent-sessions/session#AC-6`).

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use vynil_core::Semver;

/// What the session was built with: the fingerprint of the agent definition and the versions of the toolsets, by name.
/// Versions are `major.minor.patch` texts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct Provenance {
    agent_fingerprint: String,
    toolsets: BTreeMap<String, String>,
}

impl Provenance {
    /// Builds a provenance from the agent definition fingerprint and the toolset versions, by name.
    #[must_use]
    pub fn new<F: Into<String>>(agent_fingerprint: F, toolsets: BTreeMap<String, String>) -> Self {
        Self {
            agent_fingerprint: agent_fingerprint.into(),
            toolsets,
        }
    }

    /// Returns the fingerprint of the agent definition.
    #[must_use]
    pub fn agent_fingerprint(&self) -> &str {
        &self.agent_fingerprint
    }

    /// Returns the toolset versions, by name.
    #[must_use]
    pub const fn toolsets(&self) -> &BTreeMap<String, String> {
        &self.toolsets
    }
}

/// Lists the changes between the saved provenance and the current one, as system message texts.
///
/// One text per change, in this order: the agent definition, then the toolsets by ascending name, then the broken
/// functions in the order received. The list is empty when nothing changed. Texts carry names, fingerprints and
/// versions only, never session content or secrets.
#[must_use]
pub fn restore_notices(saved: &Provenance, current: &Provenance, broken_functions: &[String]) -> Vec<String> {
    let mut notices = Vec::new();
    if saved.agent_fingerprint != current.agent_fingerprint {
        notices.push(
            "the agent definition changed since the session was saved; the session resumes with the current definition"
                .to_owned(),
        );
    }
    let names: BTreeSet<&String> = saved.toolsets.keys().chain(current.toolsets.keys()).collect();
    for name in names {
        match (saved.toolsets.get(name), current.toolsets.get(name)) {
            (Some(was), Some(now)) => notices.extend(toolset_notice(name, was, now)),
            (Some(_), None) => notices.push(format!(
                "toolset `{name}` was available when the session was saved and is not available now"
            )),
            (None, Some(_)) => notices.push(format!(
                "toolset `{name}` was not available when the session was saved"
            )),
            (None, None) => {}
        }
    }
    if !broken_functions.is_empty() {
        notices.push(format!(
            "functions failed to recompile and are unavailable: {}",
            broken_functions.join(", ")
        ));
    }
    notices
}

/// Reads a version with `vynil_core::Semver`, without its build metadata: the notice gives no opinion on build metadata
/// (decision of 2026-10-30, `notice.sdd` #6). The text after the first `+` is dropped before parsing.
fn read_version(text: &str) -> Option<Semver> {
    Semver::opt_parse(text.split_once('+').map_or(text, |(core, _)| core))
}

/// Message for one toolset present in both provenances, or `None` when the change needs no message. A different major
/// version is reported first; a strictly older version with the same major is a rollback; a newer minor or patch is
/// silent. Versions that `vynil_core::Semver` does not read are compared as text.
fn toolset_notice(name: &str, saved: &str, current: &str) -> Option<String> {
    if saved == current {
        return None;
    }
    match (read_version(saved), read_version(current)) {
        (Some(was), Some(now)) if was.version.major != now.version.major => Some(format!(
            "toolset `{name}` changed major version: saved {saved}, current {current}; the session resumes with the current definitions"
        )),
        (Some(was), Some(now)) if now < was => Some(format!(
            "toolset `{name}` is older than the saved one: saved {saved}, current {current}"
        )),
        (Some(_), Some(_)) => None,
        (None, _) | (_, None) => Some(format!(
            "toolset `{name}` changed version: saved {saved}, current {current}"
        )),
    }
}

/// Lists the calls that a restored function makes to names no longer available, as system message texts.
///
/// One text per function, in ascending key order, with the calls sorted and without duplicates. `calls` holds pairs of
/// a function key (`name/arity`) and a called name, given by the caller: this crate does not know the types of
/// `scratchpad`. The list is empty when nothing is unresolved. Texts carry names only, never session content.
#[must_use]
pub fn unresolved_call_notices(calls: &[(String, String)]) -> Vec<String> {
    let mut by_function: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for (function, call) in calls {
        by_function
            .entry(function.as_str())
            .or_default()
            .push(call.as_str());
    }
    by_function
        .into_iter()
        .map(|(function, mut names)| {
            names.sort_unstable();
            names.dedup();
            let verb = if names.len() == 1 { "which is" } else { "which are" };
            let list = names.iter().map(|name| format!("`{name}`")).collect::<Vec<_>>().join(", ");
            format!("the function `{function}` calls {list} {verb} not available in this session; fix or redefine it")
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::{Provenance, restore_notices, unresolved_call_notices};

    // Scenario: une API disparue est signalée par fonction (notice.sdd #P6)
    #[test]
    fn unresolved_calls_are_reported_once_per_function_in_key_order() {
        let calls = [
            ("g/0", "ops::b"),
            ("f/1", "ops::a"),
            ("f/1", "ops::a"),
            ("f/1", "ops::c"),
        ]
        .map(|(function, call)| (function.to_owned(), call.to_owned()));
        let notices = unresolved_call_notices(&calls);
        assert_eq!(notices.len(), 2, "one text per function");
        assert_eq!(
            notices[0],
            "the function `f/1` calls `ops::a`, `ops::c` which are not available in this session; fix or redefine it",
            "f/1 comes first, its calls are listed once each"
        );
        assert_eq!(
            notices[1],
            "the function `g/0` calls `ops::b` which is not available in this session; fix or redefine it",
            "g/0 comes second, with the singular verb for one call"
        );
    }

    // Scenario: rien d'irrésolu, rien de dit (notice.sdd #P6)
    #[test]
    fn no_unresolved_call_says_nothing() {
        assert!(
            unresolved_call_notices(&[]).is_empty(),
            "an empty list of calls gives no system message"
        );
    }

    fn toolsets(entries: &[(&str, &str)]) -> BTreeMap<String, String> {
        entries
            .iter()
            .map(|(name, version)| ((*name).to_owned(), (*version).to_owned()))
            .collect()
    }

    fn provenance(fingerprint: &str, entries: &[(&str, &str)]) -> Provenance {
        Provenance::new(fingerprint, toolsets(entries))
    }

    #[test]
    fn nothing_changed() {
        let saved = provenance("sha-1", &[("k8s", "1.4.0")]);
        let current = provenance("sha-1", &[("k8s", "1.4.0")]);
        let notices = restore_notices(&saved, &current, &[]);
        assert!(
            notices.is_empty(),
            "identical provenances and no broken function must produce no message"
        );
    }

    #[test]
    fn agent_definition_changed() {
        let saved = provenance("sha-1", &[]);
        let current = provenance("sha-2", &[]);
        let notices = restore_notices(&saved, &current, &[]);
        let [notice] = notices.as_slice() else {
            panic!("a changed agent fingerprint must produce exactly one message, got {notices:?}");
        };
        assert!(
            notice.contains("agent definition changed"),
            "the message must state that the agent definition changed, got {notice:?}"
        );
    }

    #[test]
    fn agent_definition_same_fingerprint_is_silent() {
        let saved = provenance("sha-1", &[]);
        let current = provenance("sha-1", &[]);
        assert!(
            restore_notices(&saved, &current, &[]).is_empty(),
            "an unchanged agent fingerprint must not produce a message about the agent definition"
        );
    }

    #[test]
    fn major_toolset_change() {
        let saved = provenance("sha-1", &[("k8s", "1.4.0")]);
        let current = provenance("sha-1", &[("k8s", "2.0.0")]);
        let notices = restore_notices(&saved, &current, &[]);
        let [notice] = notices.as_slice() else {
            panic!("a major toolset change must produce exactly one message, got {notices:?}");
        };
        assert!(
            notice.contains("k8s") && notice.contains("1.4.0") && notice.contains("2.0.0"),
            "the message must name the toolset and both versions, got {notice:?}"
        );
    }

    #[test]
    fn older_toolset() {
        let saved = provenance("sha-1", &[("k8s", "1.4.0")]);
        let current = provenance("sha-1", &[("k8s", "1.2.0")]);
        let notices = restore_notices(&saved, &current, &[]);
        let [notice] = notices.as_slice() else {
            panic!("an older toolset must produce exactly one message, got {notices:?}");
        };
        assert!(
            notice.contains("k8s")
                && notice.contains("older")
                && notice.contains("1.4.0")
                && notice.contains("1.2.0"),
            "the message must state that the toolset is older than the saved one, naming both versions, got {notice:?}"
        );
    }

    #[test]
    fn newer_minor_is_silent() {
        let saved = provenance("sha-1", &[("k8s", "1.4.0")]);
        let current = provenance("sha-1", &[("k8s", "1.6.1")]);
        let notices = restore_notices(&saved, &current, &[]);
        assert!(
            notices.is_empty(),
            "a newer minor and patch with the same major must produce no message, got {notices:?}"
        );
    }

    #[test]
    fn toolset_missing_or_added_is_named() {
        let saved = provenance("sha-1", &[("k8s", "1.4.0")]);
        let current = provenance("sha-1", &[("git", "0.3.0")]);
        let notices = restore_notices(&saved, &current, &[]);
        assert!(
            notices.iter().any(|notice| notice.contains("k8s")),
            "a toolset present at save and absent now must be named, got {notices:?}"
        );
        assert!(
            notices.iter().any(|notice| notice.contains("git")),
            "a toolset absent at save and present now must be named, got {notices:?}"
        );
    }

    #[test]
    fn broken_functions_listed_once() {
        let saved = provenance("sha-1", &[]);
        let current = provenance("sha-1", &[]);
        let broken = vec!["list_pods".to_owned(), "scale".to_owned()];
        let notices = restore_notices(&saved, &current, &broken);
        let [notice] = notices.as_slice() else {
            panic!("broken functions must produce exactly one message, got {notices:?}");
        };
        let first = notice.find("list_pods");
        let second = notice.find("scale");
        assert!(
            matches!((first, second), (Some(a), Some(b)) if a < b),
            "the single message must list both names, in the order received, got {notice:?}"
        );
    }

    // Decisions of 2026-10-30 on the Semver semantics (notice.sdd, #6): one test per divergence.

    #[test]
    fn prerelease_to_release_is_not_a_rollback() {
        let saved = provenance("sha-1", &[("k8s", "1.4.0-rc.1")]);
        let current = provenance("sha-1", &[("k8s", "1.4.0")]);
        assert!(
            restore_notices(&saved, &current, &[]).is_empty(),
            "a pre-release followed by its release is a newer version: no message"
        );
        let saved_back = provenance("sha-1", &[("k8s", "1.4.0")]);
        let current_back = provenance("sha-1", &[("k8s", "1.4.0-rc.1")]);
        let notices = restore_notices(&saved_back, &current_back, &[]);
        assert!(
            notices.len() == 1 && notices[0].contains("older"),
            "a release replaced by its pre-release is older under Semver precedence, got {notices:?}"
        );
    }

    #[test]
    fn v_prefix_is_read_as_the_version() {
        let saved = provenance("sha-1", &[("k8s", "1.4.0")]);
        let current = provenance("sha-1", &[("k8s", "v1.4.0")]);
        assert!(
            restore_notices(&saved, &current, &[]).is_empty(),
            "v1.4.0 and 1.4.0 are the same version: no message"
        );
    }

    #[test]
    fn build_metadata_is_not_a_change() {
        let saved = provenance("sha-1", &[("k8s", "1.4.0+a")]);
        let current = provenance("sha-1", &[("k8s", "1.4.0+b")]);
        assert!(
            restore_notices(&saved, &current, &[]).is_empty(),
            "two versions equal under the notice's reading but with different text: no message"
        );
        let saved_reverse = provenance("sha-1", &[("k8s", "1.4.0+b")]);
        let current_reverse = provenance("sha-1", &[("k8s", "1.4.0+a")]);
        assert!(
            restore_notices(&saved_reverse, &current_reverse, &[]).is_empty(),
            "build metadata has no opinion either way: the reverse order gives no message"
        );
    }

    #[test]
    fn leading_zero_is_compared_as_text() {
        let saved = provenance("sha-1", &[("k8s", "01.4.0")]);
        let current = provenance("sha-1", &[("k8s", "1.4.0")]);
        let notices = restore_notices(&saved, &current, &[]);
        assert!(
            notices.len() == 1 && notices[0].contains("changed version"),
            "a version Semver does not read is compared as text, so a difference gives a message, got {notices:?}"
        );
    }

    #[test]
    fn unparseable_version_never_panics() {
        let saved = provenance("sha-1", &[("k8s", "nightly")]);
        let current = provenance("sha-1", &[("k8s", "1.0.0")]);
        let notices = restore_notices(&saved, &current, &[]);
        assert!(
            !notices.is_empty(),
            "a version that is not major.minor.patch must be compared as text, and a difference must produce a message"
        );
    }

    #[test]
    fn messages_follow_the_global_order() {
        let saved = provenance("sha-1", &[("k8s", "1.4.0"), ("git", "0.1.0")]);
        let current = provenance("sha-2", &[("k8s", "2.0.0"), ("vault", "3.0.0")]);
        let broken = vec!["scale".to_owned()];
        let notices = restore_notices(&saved, &current, &broken);
        let [agent, git, k8s, vault_added, functions] = notices.as_slice() else {
            panic!(
                "the agent, three toolset changes and the broken functions must give five messages, got {notices:?}"
            );
        };
        assert!(
            agent.contains("agent definition"),
            "the agent definition message must come first, got {agent:?}"
        );
        assert!(
            git.contains("`git`") && git.contains("not available now"),
            "the toolset git, absent now, must come first among the toolsets (ascending name), got {git:?}"
        );
        assert!(
            k8s.contains("`k8s`") && k8s.contains("major version"),
            "the toolset k8s, with a major change, must follow git, got {k8s:?}"
        );
        assert!(
            vault_added.contains("`vault`")
                && vault_added.contains("not available when the session was saved"),
            "the toolset vault, added since the save, must follow k8s, got {vault_added:?}"
        );
        assert!(
            functions.contains("scale"),
            "the broken functions message must come last, got {functions:?}"
        );
    }

    #[test]
    fn major_change_and_older_gives_one_message() {
        let saved = provenance("sha-1", &[("k8s", "2.0.0")]);
        let current = provenance("sha-1", &[("k8s", "1.9.0")]);
        let notices = restore_notices(&saved, &current, &[]);
        let [notice] = notices.as_slice() else {
            panic!(
                "a toolset both of a different major and older must give exactly one message, got {notices:?}"
            );
        };
        assert!(
            notice.contains("major version") && notice.contains("2.0.0") && notice.contains("1.9.0"),
            "the single message must be the major change, naming both versions (decision #4), got {notice:?}"
        );
        assert!(
            !notice.contains("older"),
            "no rollback message may be added when the major change already speaks (decision #4), got {notice:?}"
        );
    }
}
