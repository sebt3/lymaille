//! Skill: metadata and body compatible with `SKILL.md` (Agent Skills), named annex documents, and the render cap they are
//! checked against (`agent-sessions/toolsets#FR-7`, `FR-15`, `FR-18`). The frontmatter parser is not part of this lot: a
//! skill is built from its fields, and the toolset it belongs to is set by the toolset, not by the skill.

use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Deserializer, Serialize};

use super::{ToolsetsError, ToolsetsResult};

/// Largest number of characters of a skill name.
const NAME_MAX_CHARS: usize = 64;
/// Largest number of characters of a skill description.
const DESCRIPTION_MAX_CHARS: usize = 1024;

/// Skill: a body of instructions with its metadata and its annex documents. The body and the description are templates,
/// rendered by the template module; a skill grants no capability.
#[derive(Clone, PartialEq, Eq, Serialize)]
#[non_exhaustive]
pub struct Skill {
    name: String,
    description: String,
    body: String,
    license: Option<String>,
    compatibility: Option<String>,
    metadata: BTreeMap<String, String>,
    annexes: BTreeMap<String, String>,
}

impl Skill {
    /// Builds a skill from its name, its description and its body.
    ///
    /// # Errors
    ///
    /// Returns [`ToolsetsError::InvalidSkill`] when the name is not 1 to 64 lowercase ASCII letters, digits or hyphens,
    /// or when the description is blank or does not hold at most 1024 characters.
    pub fn new(name: String, description: String, body: String) -> ToolsetsResult<Self> {
        if !is_skill_name(&name) {
            return Err(ToolsetsError::InvalidSkill {
                name,
                reason: "the name must hold 1 to 64 lowercase ASCII letters, digits or hyphens",
            });
        }
        if description.trim().is_empty() || description.chars().count() > DESCRIPTION_MAX_CHARS {
            return Err(ToolsetsError::InvalidSkill {
                name,
                reason: "the description must hold 1 to 1024 characters and must not be blank",
            });
        }
        Ok(Self {
            name,
            description,
            body,
            license: None,
            compatibility: None,
            metadata: BTreeMap::new(),
            annexes: BTreeMap::new(),
        })
    }

    /// Sets the license of the skill.
    #[must_use]
    pub fn with_license(mut self, license: String) -> Self {
        self.license = Some(license);
        self
    }

    /// Sets the compatibility note of the skill.
    #[must_use]
    pub fn with_compatibility(mut self, compatibility: String) -> Self {
        self.compatibility = Some(compatibility);
        self
    }

    /// Adds a metadata entry.
    ///
    /// # Errors
    ///
    /// Returns [`ToolsetsError::InvalidSkill`] when an entry of the same key is already declared: a duplicate is refused,
    /// never replaced in silence.
    pub fn with_metadata(mut self, key: String, value: String) -> ToolsetsResult<Self> {
        if self.metadata.contains_key(&key) {
            return Err(ToolsetsError::InvalidSkill {
                name: self.name,
                reason: "a metadata key is declared twice",
            });
        }
        let _previous = self.metadata.insert(key, value);
        Ok(self)
    }

    /// Adds an annex document. It is read on demand and never preloaded into the context.
    ///
    /// # Errors
    ///
    /// Returns [`ToolsetsError::InvalidSkill`] when the annex name is empty or when an annex of this name is already
    /// declared.
    pub fn with_annex(mut self, name: String, text: String) -> ToolsetsResult<Self> {
        if name.is_empty() {
            return Err(ToolsetsError::InvalidSkill {
                name,
                reason: "the annex name is empty",
            });
        }
        if self.annexes.contains_key(&name) {
            return Err(ToolsetsError::InvalidSkill {
                name,
                reason: "an annex of this name is declared twice",
            });
        }
        let _previous = self.annexes.insert(name, text);
        Ok(self)
    }

    /// Checks that the body and every annex document hold at most `cap_chars` characters, the render cap of the scratchpad.
    /// The body is checked first, then the annexes in name order.
    ///
    /// # Errors
    ///
    /// Returns [`ToolsetsError::SkillTooLarge`] for the first text that exceeds the cap, naming the skill or the annex.
    pub fn check_size(&self, cap_chars: u64) -> ToolsetsResult<()> {
        check_text_size(&self.name, &self.body, cap_chars)?;
        for (name, text) in &self.annexes {
            check_text_size(name, text, cap_chars)?;
        }
        Ok(())
    }

    /// Returns the name of the skill.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns the description of the skill.
    #[must_use]
    pub fn description(&self) -> &str {
        &self.description
    }

    /// Returns the body of the skill, as written (a template, not yet rendered).
    #[must_use]
    pub fn body(&self) -> &str {
        &self.body
    }

    /// Returns the license, when one is declared.
    #[must_use]
    pub fn license(&self) -> Option<&str> {
        self.license.as_deref()
    }

    /// Returns the compatibility note, when one is declared.
    #[must_use]
    pub fn compatibility(&self) -> Option<&str> {
        self.compatibility.as_deref()
    }

    /// Returns the metadata entries, by key.
    #[must_use]
    pub const fn metadata(&self) -> &BTreeMap<String, String> {
        &self.metadata
    }

    /// Returns the annex documents, by name.
    #[must_use]
    pub const fn annexes(&self) -> &BTreeMap<String, String> {
        &self.annexes
    }

    /// Returns the text of one annex document, when it exists.
    #[must_use]
    pub fn annex(&self, name: &str) -> Option<&str> {
        self.annexes.get(name).map(String::as_str)
    }
}

impl fmt::Debug for Skill {
    /// Shows the name, the size in bytes of the body and of each annex, and the annex names. Never the text of the body,
    /// of the annexes or of the description: a debug output of a toolset would spread them.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let annexes: BTreeMap<&str, usize> = self
            .annexes
            .iter()
            .map(|(name, text)| (name.as_str(), text.len()))
            .collect();
        f.debug_struct("Skill")
            .field("name", &self.name)
            .field("body_bytes", &self.body.len())
            .field("annexes", &annexes)
            .finish_non_exhaustive()
    }
}

impl<'de> Deserialize<'de> for Skill {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct Record {
            name: String,
            description: String,
            body: String,
            license: Option<String>,
            compatibility: Option<String>,
            metadata: BTreeMap<String, String>,
            annexes: BTreeMap<String, String>,
        }
        let record = Record::deserialize(deserializer)?;
        let fail = |error: ToolsetsError| <D::Error as serde::de::Error>::custom(error);
        let mut built = Self::new(record.name, record.description, record.body).map_err(fail)?;
        if let Some(license) = record.license {
            built = built.with_license(license);
        }
        if let Some(compatibility) = record.compatibility {
            built = built.with_compatibility(compatibility);
        }
        for (key, value) in record.metadata {
            built = built.with_metadata(key, value).map_err(fail)?;
        }
        for (name, text) in record.annexes {
            built = built.with_annex(name, text).map_err(fail)?;
        }
        Ok(built)
    }
}

/// Tells whether `name` is a valid skill name: 1 to 64 lowercase ASCII letters, digits or hyphens.
fn is_skill_name(name: &str) -> bool {
    (1..=NAME_MAX_CHARS).contains(&name.len())
        && name
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

/// Checks one text against the render cap, counted in characters. The error names the skill or the annex.
fn check_text_size(name: &str, text: &str, cap_chars: u64) -> ToolsetsResult<()> {
    let length = u64::try_from(text.chars().count()).unwrap_or(u64::MAX);
    if length > cap_chars {
        return Err(ToolsetsError::SkillTooLarge {
            name: name.to_owned(),
            cap_chars,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::Skill;
    use crate::toolsets::ToolsetsError;

    fn skill(name: &str) -> Result<Skill, ToolsetsError> {
        Skill::new(
            name.to_owned(),
            "a short description".to_owned(),
            "the body".to_owned(),
        )
    }

    // Scenario: invalid name refused
    #[test]
    fn invalid_name_refused() {
        for name in ["Grafana", "a b", ""] {
            assert!(
                matches!(skill(name), Err(ToolsetsError::InvalidSkill { .. })),
                "the name `{name}` must fail with InvalidSkill"
            );
        }
    }

    #[test]
    fn name_bounds_are_64_characters() {
        assert!(skill(&"a".repeat(64)).is_ok(), "a 64-character name is accepted");
        assert!(
            matches!(skill(&"a".repeat(65)), Err(ToolsetsError::InvalidSkill { .. })),
            "a 65-character name is refused"
        );
    }

    #[test]
    fn description_bounds_are_1_to_1024_characters() {
        let build = |description: String| Skill::new("grafana".to_owned(), description, "body".to_owned());
        assert!(
            matches!(build(String::new()), Err(ToolsetsError::InvalidSkill { .. })),
            "an empty description is refused"
        );
        assert!(
            build("d".repeat(1024)).is_ok(),
            "a 1024-character description is accepted"
        );
        assert!(
            matches!(build("d".repeat(1025)), Err(ToolsetsError::InvalidSkill { .. })),
            "a 1025-character description is refused"
        );
    }

    // Task #4 of skill.sdd (Must): a blank description is refused, not only an empty one.
    #[test]
    fn blank_description_refused() {
        for description in ["   ", "\t", " \n "] {
            let result = Skill::new("grafana".to_owned(), description.to_owned(), "body".to_owned());
            assert!(
                matches!(result, Err(ToolsetsError::InvalidSkill { .. })),
                "the blank description {description:?} must fail with InvalidSkill"
            );
        }
    }

    // Task #4 of skill.sdd (Must): a duplicate metadata key is refused, never replaced in silence.
    #[test]
    fn duplicate_metadata_key_refused() {
        let result = skill("grafana")
            .expect("a valid skill builds")
            .with_metadata("owner".to_owned(), "team".to_owned())
            .expect("the first entry builds")
            .with_metadata("owner".to_owned(), "other".to_owned());
        assert!(
            matches!(result, Err(ToolsetsError::InvalidSkill { .. })),
            "a metadata key declared twice must fail with InvalidSkill"
        );
    }

    #[test]
    fn accessors_return_what_was_given() {
        let built = skill("grafana")
            .expect("a valid skill builds")
            .with_license("BSD-3-Clause".to_owned())
            .with_compatibility("any Rhai scratchpad".to_owned())
            .with_metadata("owner".to_owned(), "team".to_owned())
            .expect("a metadata entry builds")
            .with_annex("queries".to_owned(), "PromQL examples".to_owned())
            .expect("an annex builds");
        assert_eq!(built.name(), "grafana");
        assert_eq!(built.description(), "a short description");
        assert_eq!(built.body(), "the body");
        assert_eq!(built.license(), Some("BSD-3-Clause"));
        assert_eq!(built.compatibility(), Some("any Rhai scratchpad"));
        assert_eq!(built.metadata().get("owner").map(String::as_str), Some("team"));
        assert_eq!(built.annex("queries"), Some("PromQL examples"));
        assert_eq!(built.annex("missing"), None, "an absent annex is not found");
        assert_eq!(built.annexes().len(), 1, "one annex is listed");
    }

    #[test]
    fn absent_optional_fields_read_as_none() {
        let built = skill("grafana").expect("a valid skill builds");
        assert_eq!(built.license(), None, "no license by default");
        assert_eq!(built.compatibility(), None, "no compatibility by default");
        assert!(built.metadata().is_empty(), "no metadata by default");
    }

    #[test]
    fn duplicate_annex_refused() {
        let result = skill("grafana")
            .expect("a valid skill builds")
            .with_annex("notes".to_owned(), "first".to_owned())
            .expect("the first annex builds")
            .with_annex("notes".to_owned(), "second".to_owned());
        assert!(
            matches!(result, Err(ToolsetsError::InvalidSkill { .. })),
            "an annex declared twice is refused"
        );
    }

    #[test]
    fn empty_annex_name_refused() {
        let result = skill("grafana")
            .expect("a valid skill builds")
            .with_annex(String::new(), "text".to_owned());
        assert!(
            matches!(result, Err(ToolsetsError::InvalidSkill { .. })),
            "an annex without a name is refused"
        );
    }

    // Scenario: oversized body refused
    #[test]
    fn oversized_body_refused() {
        let built =
            Skill::new("grafana".to_owned(), "d".to_owned(), "x".repeat(101)).expect("a valid skill builds");
        assert!(
            matches!(
                built.check_size(100),
                Err(ToolsetsError::SkillTooLarge { cap_chars: 100, .. })
            ),
            "a body longer than the cap fails with SkillTooLarge"
        );
    }

    #[test]
    fn body_at_cap_is_accepted_and_counted_in_characters() {
        let built = Skill::new("grafana".to_owned(), "d".to_owned(), "\u{e9}".repeat(100))
            .expect("a valid skill builds");
        assert!(
            built.check_size(100).is_ok(),
            "a body of 100 characters fits a cap of 100 characters, though it is longer in bytes"
        );
    }

    #[test]
    fn oversized_annex_refused() {
        let built = skill("grafana")
            .expect("a valid skill builds")
            .with_annex("big".to_owned(), "x".repeat(101))
            .expect("the annex builds");
        assert!(
            matches!(
                built.check_size(100),
                Err(ToolsetsError::SkillTooLarge { name, cap_chars: 100 }) if name == "big"
            ),
            "an annex longer than the cap fails with SkillTooLarge naming the annex"
        );
    }

    #[test]
    fn serialization_round_trip_is_identical() {
        let built = skill("grafana")
            .expect("a valid skill builds")
            .with_metadata("owner".to_owned(), "team".to_owned())
            .expect("a metadata entry builds")
            .with_annex("queries".to_owned(), "PromQL".to_owned())
            .expect("the annex builds");
        let text = serde_json::to_string(&built).expect("a skill serializes");
        let back: Skill = serde_json::from_str(&text).expect("a skill deserializes");
        assert_eq!(back, built, "the round trip must be identical");
    }

    #[test]
    fn deserialization_refuses_an_invalid_name() {
        let text = r#"{"name":"Grafana","description":"d","body":"b","license":null,"compatibility":null,"metadata":{},"annexes":{}}"#;
        assert!(
            serde_json::from_str::<Skill>(text).is_err(),
            "an invalid name must fail to deserialize"
        );
    }

    // Task #5 of skill.sdd: the debug output shows the name and the sizes of the body and of the annexes, never their text.
    #[test]
    fn debug_shows_sizes_and_never_the_text() {
        let built = Skill::new(
            "grafana".to_owned(),
            "DESCRIPTION-TEXT".to_owned(),
            "BODY-TEXT-SECRET".to_owned(),
        )
        .expect("a valid skill builds")
        .with_annex("queries".to_owned(), "ANNEX-TEXT-SECRET".to_owned())
        .expect("the annex builds");
        let shown = format!("{built:?}");
        assert!(shown.contains("grafana"), "the name must be shown: {shown}");
        assert!(
            shown.contains("body_bytes: 16"),
            "the body size in bytes must be shown: {shown}"
        );
        assert!(
            shown.contains("\"queries\": 17"),
            "the annex name and its size in bytes must be shown: {shown}"
        );
        for text in ["DESCRIPTION-TEXT", "BODY-TEXT-SECRET", "ANNEX-TEXT-SECRET"] {
            assert!(
                !shown.contains(text),
                "the text of the skill never appears in a debug output: {shown}"
            );
        }
    }
}
