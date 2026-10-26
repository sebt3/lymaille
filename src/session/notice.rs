//! System messages telling the LLM, at restore, what changed since the save: a different major version of a toolset, a changed
//! agent definition, a toolset older than the saved one, broken functions. The session always resumes with the current
//! definitions (`agent-sessions/session#FR-10`, `agent-sessions/session#FR-3`, `agent-sessions/session#AC-2`,
//! `agent-sessions/session#AC-6`).

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

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

/// A `major.minor.patch` version read as numbers, compared lexicographically.
type Version = (u64, u64, u64);

/// Reads a version as `major.minor.patch`. Any other text yields `None`.
fn parse_version(text: &str) -> Option<Version> {
    let mut parts = text.split('.');
    let major: u64 = parts.next()?.parse().ok()?;
    let minor: u64 = parts.next()?.parse().ok()?;
    let patch: u64 = parts.next()?.parse().ok()?;
    parts.next().is_none().then_some((major, minor, patch))
}

/// Message for one toolset present in both provenances, or `None` when the change needs no message. A different major
/// version is reported first; a strictly older version with the same major is a rollback; a newer minor or patch is
/// silent. Versions that are not `major.minor.patch` are compared as text.
fn toolset_notice(name: &str, saved: &str, current: &str) -> Option<String> {
    if saved == current {
        return None;
    }
    match (parse_version(saved), parse_version(current)) {
        (Some(was), Some(now)) if was.0 != now.0 => Some(format!(
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

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::{Provenance, restore_notices};

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
            notices.iter().all(|notice| !notice.contains("k8s")),
            "a newer minor and patch with the same major must produce no message for the toolset, got {notices:?}"
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
