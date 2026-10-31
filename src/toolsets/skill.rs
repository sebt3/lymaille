//! Skill: metadata and body compatible with `SKILL.md` (Agent Skills), named annex documents, and the render cap they are
//! checked against (`agent-sessions/toolsets#FR-7`, `FR-15`, `FR-18`). A skill is built from its fields, or read from a
//! `SKILL.md` text by [`Skill::parse`], which reads the frontmatter with `yaml_serde`. The toolset a skill belongs to is set
//! by the toolset, not by the skill.

use std::collections::BTreeMap;
use std::fmt;

use serde::de::{MapAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize};

use super::{ToolsetsError, ToolsetsResult};

/// Largest number of characters of a skill name.
const NAME_MAX_CHARS: usize = 64;
/// Largest number of characters of a skill description.
const DESCRIPTION_MAX_CHARS: usize = 1024;
/// Largest size in bytes of a `SKILL.md` text that is parsed, 256 KiB (decision of 2026-10-31, D-a). A larger text is refused
/// before any parsing.
const TEXT_MAX_BYTES: usize = 256 * 1024;
/// Line that opens and closes the frontmatter of a `SKILL.md` text.
const FENCE: &str = "---";
/// Name that a refusal of a parsed text carries in place of a value of the file.
const PARSED_NAME: &str = "<SKILL.md>";
/// Reason of a text over [`TEXT_MAX_BYTES`].
const TEXT_TOO_LARGE: &str = "the text is larger than 256 KiB";
/// Reason of a text without a closed frontmatter.
const FRONTMATTER_MISSING: &str = "the frontmatter is missing or is not closed by a `---` line";
/// Reason of a frontmatter that is not YAML with the expected fields.
const FRONTMATTER_UNREADABLE: &str =
    "the frontmatter is not readable YAML with a name, a description and string metadata";
/// Reason of a `name` or a `description` that is missing, or a YAML null: a null is not text.
const FIELD_NOT_TEXT: &str = "the name and the description must be text, not missing and not a YAML null";

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
                known: Vec::new(),
            });
        }
        if description.trim().is_empty() || description.chars().count() > DESCRIPTION_MAX_CHARS {
            return Err(ToolsetsError::InvalidSkill {
                name,
                reason: "the description must hold 1 to 1024 characters and must not be blank",
                known: Vec::new(),
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

    /// Reads a `SKILL.md` text: a frontmatter of YAML between two `---` lines, then the body, which is the text after
    /// the closing line (that line and its line break removed, nothing else changed). The fields `name`, `description`,
    /// `license`, `compatibility` and `metadata` (a table of strings) are read; every other field is ignored, `allowed-tools`
    /// included.
    ///
    /// # Errors
    ///
    /// Returns [`ToolsetsError::InvalidSkill`] when the text is over 256 KiB (refused before any parsing), when the frontmatter
    /// is missing or not closed, when it is not readable YAML with those fields, when `name` or `description` is missing or a
    /// YAML null, when a metadata key is declared twice, or when the name or the description is refused. A refusal names the
    /// file (`<SKILL.md>`), never a value of the text.
    pub fn parse(text: &str) -> ToolsetsResult<Self> {
        if text.len() > TEXT_MAX_BYTES {
            return Err(parsed_refusal(TEXT_TOO_LARGE));
        }
        let (frontmatter, body) =
            split_frontmatter(text).ok_or_else(|| parsed_refusal(FRONTMATTER_MISSING))?;
        let record: Frontmatter = yaml_serde::from_str(&frontmatter)
            .ok()
            .ok_or_else(|| parsed_refusal(FRONTMATTER_UNREADABLE))?;
        let name = record.name.ok_or_else(|| parsed_refusal(FIELD_NOT_TEXT))?;
        let description = record.description.ok_or_else(|| parsed_refusal(FIELD_NOT_TEXT))?;
        let mut skill = Self::new(name, description, body).map_err(hide_name)?;
        if let Some(license) = record.license {
            skill = skill.with_license(license);
        }
        if let Some(compatibility) = record.compatibility {
            skill = skill.with_compatibility(compatibility);
        }
        for (key, value) in record.metadata.map(|pairs| pairs.0).unwrap_or_default() {
            skill = skill.with_metadata(key, value).map_err(hide_name)?;
        }
        Ok(skill)
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
                known: Vec::new(),
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
                known: Vec::new(),
            });
        }
        if self.annexes.contains_key(&name) {
            return Err(ToolsetsError::InvalidSkill {
                name,
                reason: "an annex of this name is declared twice",
                known: Vec::new(),
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

/// The frontmatter of a `SKILL.md` text. Unknown fields are ignored: `allowed-tools` grants nothing.
#[derive(Deserialize)]
struct Frontmatter {
    /// Optional so that a YAML null reads as absent, which is refused, and not as the text `null`.
    name: Option<String>,
    /// Optional so that a YAML null reads as absent, which is refused, and not as the text `null`.
    description: Option<String>,
    license: Option<String>,
    compatibility: Option<String>,
    metadata: Option<MetadataPairs>,
}

/// The metadata of a frontmatter, in the order of the file, with a key declared twice kept as twice, so that
/// [`Skill::with_metadata`] refuses it instead of replacing it in silence.
struct MetadataPairs(Vec<(String, String)>);

impl<'de> Deserialize<'de> for MetadataPairs {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_map(PairsVisitor)
    }
}

/// Visitor that reads every entry of a metadata table, duplicates included.
struct PairsVisitor;

impl<'de> Visitor<'de> for PairsVisitor {
    type Value = MetadataPairs;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a table of strings")
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
        let mut pairs = Vec::new();
        while let Some(pair) = map.next_entry::<String, String>()? {
            pairs.push(pair);
        }
        Ok(MetadataPairs(pairs))
    }
}

/// Splits a `SKILL.md` text into its frontmatter and its body. The opening `---` line must be the first line; the
/// frontmatter runs to the first following `---` line, and the body is what follows that line. CRLF line endings are
/// accepted. Returns `None` when the frontmatter is missing or not closed.
fn split_frontmatter(text: &str) -> Option<(String, String)> {
    let lines: Vec<&str> = text.split_inclusive('\n').collect();
    let (opening, rest) = lines.split_first()?;
    if line_text(opening) != FENCE {
        return None;
    }
    let closing = rest.iter().position(|line| line_text(line) == FENCE)?;
    let (frontmatter, after) = rest.split_at_checked(closing)?;
    let body: String = after.iter().skip(1).copied().collect();
    Some((frontmatter.concat(), body))
}

/// The text of a line without its line break, LF or CRLF.
fn line_text(line: &str) -> &str {
    let without_newline = line.strip_suffix('\n').unwrap_or(line);
    without_newline.strip_suffix('\r').unwrap_or(without_newline)
}

/// A refusal raised while reading a parsed text names the file, not the skill: the name of the skill may be the very value
/// that is refused, and a message never carries a value of the text.
fn hide_name(error: ToolsetsError) -> ToolsetsError {
    if let ToolsetsError::InvalidSkill { reason, .. } = error {
        parsed_refusal(reason)
    } else {
        error
    }
}

/// The refusal of a parsed text: an invalid skill named after the file, with a fixed reason.
fn parsed_refusal(reason: &'static str) -> ToolsetsError {
    ToolsetsError::InvalidSkill {
        name: PARSED_NAME.to_owned(),
        reason,
        known: Vec::new(),
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

    // Scenario: nom invalide refusé
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

    /// A complete `SKILL.md`: the frontmatter between two `---` lines, then the body.
    const SKILL_FILE: &str = "---\nname: grafana\ndescription: Query the dashboards\nlicense: BSD-3-Clause\ncompatibility: any Rhai scratchpad\nmetadata:\n  owner: team\n---\nUse the dashboards.\n\nSecond paragraph.\n";

    // Scenario: un fichier de skill valide est analysé
    #[test]
    fn valid_skill_file_is_parsed() {
        let parsed = Skill::parse(SKILL_FILE).expect("a valid SKILL.md must parse");
        assert_eq!(parsed.name(), "grafana");
        assert_eq!(parsed.description(), "Query the dashboards");
        assert_eq!(parsed.license(), Some("BSD-3-Clause"));
        assert_eq!(parsed.compatibility(), Some("any Rhai scratchpad"));
        assert_eq!(parsed.metadata().get("owner").map(String::as_str), Some("team"));
        assert_eq!(
            parsed.body(),
            "Use the dashboards.\n\nSecond paragraph.\n",
            "the body is the text after the frontmatter"
        );
    }

    #[test]
    fn body_keeps_later_rule_lines_and_crlf_endings() {
        let rule = Skill::parse("---\nname: grafana\ndescription: d\n---\nabove\n---\nbelow\n")
            .expect("a body may hold a `---` line after the closing one");
        assert_eq!(rule.body(), "above\n---\nbelow\n");
        let crlf = Skill::parse("---\r\nname: grafana\r\ndescription: d\r\n---\r\nbody\r\n")
            .expect("CRLF line endings parse");
        assert_eq!(
            crlf.body(),
            "body\r\n",
            "the closing line and its line break are removed"
        );
        let empty = Skill::parse("---\nname: grafana\ndescription: d\n---").expect("an empty body parses");
        assert_eq!(
            empty.body(),
            "",
            "a closing line at the end of the text leaves an empty body"
        );
    }

    // Scenario: allowed-tools est ignoré
    #[test]
    fn allowed_tools_is_ignored() {
        let plain = Skill::parse("---\nname: grafana\ndescription: d\n---\nbody\n").expect("parses");
        let granted =
            Skill::parse("---\nname: grafana\ndescription: d\nallowed-tools: [rhai_pad, shell]\n---\nbody\n")
                .expect("the frontmatter with allowed-tools parses");
        assert_eq!(
            granted, plain,
            "allowed-tools grants no capability: the skill is the same"
        );
    }

    #[test]
    fn unknown_frontmatter_fields_are_tolerated() {
        let plain = Skill::parse("---\nname: grafana\ndescription: d\n---\nbody\n").expect("parses");
        let extra = Skill::parse("---\nname: grafana\ndescription: d\nauthor: someone\n---\nbody\n")
            .expect("an unknown field parses");
        assert_eq!(extra, plain, "an unknown field changes nothing");
    }

    // Scenario: nom invalide refusé
    #[test]
    fn invalid_name_refused_when_parsed() {
        for name in ["Grafana", "a b", ""] {
            let text = format!("---\nname: \"{name}\"\ndescription: d\n---\nbody\n");
            assert!(
                matches!(Skill::parse(&text), Err(ToolsetsError::InvalidSkill { .. })),
                "the parsed name `{name}` must fail with InvalidSkill"
            );
        }
    }

    #[test]
    fn blank_description_refused_when_parsed() {
        assert!(
            matches!(
                Skill::parse("---\nname: grafana\ndescription: \"  \"\n---\nbody\n"),
                Err(ToolsetsError::InvalidSkill { .. })
            ),
            "a blank description is refused when parsed"
        );
    }

    #[test]
    fn missing_or_unclosed_frontmatter_refused() {
        for text in [
            "name: grafana\ndescription: d\nbody\n",
            "---\nname: grafana\ndescription: d\nbody\n",
        ] {
            assert!(
                matches!(Skill::parse(text), Err(ToolsetsError::InvalidSkill { .. })),
                "a text without a closed frontmatter must fail with InvalidSkill: {text:?}"
            );
        }
    }

    #[test]
    fn frontmatter_must_open_the_text() {
        let text = "intro line\nname: grafana\ndescription: d\n---\nbody\n";
        assert!(
            matches!(Skill::parse(text), Err(ToolsetsError::InvalidSkill { .. })),
            "a `---` line that is not the first line does not open a frontmatter"
        );
    }

    // Scenario: les refus ne portent jamais le contenu du fichier
    #[test]
    fn malformed_frontmatter_refused_without_its_content() {
        let text = "---\nname: [FILE-CONTENT-SECRET\ndescription: d\n---\nbody\n";
        match Skill::parse(text) {
            Err(error @ ToolsetsError::InvalidSkill { .. }) => {
                assert!(
                    !error.to_string().contains("FILE-CONTENT-SECRET"),
                    "the refusal must not carry the file: {error}"
                );
            }
            other => panic!("malformed YAML must fail with InvalidSkill, got {other:?}"),
        }
    }

    #[test]
    fn invalid_name_refusal_never_echoes_the_name() {
        match Skill::parse("---\nname: Grafana-NAME-SECRET\ndescription: d\n---\nbody\n") {
            Err(error @ ToolsetsError::InvalidSkill { .. }) => assert!(
                !error.to_string().contains("NAME-SECRET"),
                "the refusal of a file must not echo a value of the file: {error}"
            ),
            other => panic!("an invalid name must fail with InvalidSkill, got {other:?}"),
        }
    }

    // Task #2 decision: metadata is a table of strings; a duplicate key is refused, never replaced in silence.
    #[test]
    fn duplicate_metadata_key_refused_when_parsed() {
        assert!(
            matches!(
                Skill::parse(
                    "---\nname: grafana\ndescription: d\nmetadata:\n  owner: a\n  owner: b\n---\nbody\n"
                ),
                Err(ToolsetsError::InvalidSkill { .. })
            ),
            "a metadata key declared twice is refused"
        );
    }

    #[test]
    fn metadata_value_that_is_a_list_or_a_table_refused() {
        for value in ["[a, b]", "{ nested: x }"] {
            let text = format!("---\nname: grafana\ndescription: d\nmetadata:\n  tags: {value}\n---\nbody\n");
            assert!(
                matches!(Skill::parse(&text), Err(ToolsetsError::InvalidSkill { .. })),
                "metadata is a table of strings, `{value}` is refused"
            );
        }
    }

    #[test]
    fn metadata_scalar_is_read_as_its_text() {
        let parsed =
            Skill::parse("---\nname: grafana\ndescription: d\nmetadata:\n  version: 1.0\n---\nbody\n")
                .expect("a scalar parses as its text");
        assert_eq!(parsed.metadata().get("version").map(String::as_str), Some("1.0"));
    }

    /// Size of a `SKILL.md` text above which it is refused before any parsing (decision of 2026-10-31, D-a).
    const TEXT_LIMIT_BYTES: usize = 256 * 1024;

    // Scenario: texte SKILL.md de plus de 256 Kio refusé avant toute analyse
    #[test]
    fn text_over_256_kib_is_refused_before_parsing() {
        let text = format!("---\nname: [broken\n---\n{}", "x".repeat(TEXT_LIMIT_BYTES));
        match Skill::parse(&text) {
            Err(ToolsetsError::InvalidSkill { reason, name, .. }) => {
                assert!(reason.contains("256 KiB"), "the size is the reason: {reason}");
                assert_eq!(name, "<SKILL.md>", "the refusal names the file");
            }
            other => panic!("an oversized text must fail with InvalidSkill, got {other:?}"),
        }
    }

    // Scenario: texte SKILL.md de 256 Kio exactement lu
    #[test]
    fn text_of_exactly_256_kib_is_read() {
        let head = "---\nname: grafana\ndescription: d\n---\n";
        let body = "x".repeat(TEXT_LIMIT_BYTES.saturating_sub(head.len()));
        let text = format!("{head}{body}");
        assert_eq!(text.len(), TEXT_LIMIT_BYTES, "the sample is exactly the limit");
        assert!(Skill::parse(&text).is_ok(), "a text of exactly 256 KiB is read");
    }

    // Scenario: name ou description en null YAML refusé
    #[test]
    fn yaml_null_name_or_description_is_refused() {
        for frontmatter in [
            "name: ~\ndescription: d",
            "name: null\ndescription: d",
            "name:\ndescription: d",
            "name: grafana\ndescription: ~",
            "name: grafana\ndescription: null",
            "name: grafana\ndescription:",
        ] {
            let text = format!("---\n{frontmatter}\n---\nbody\n");
            assert!(
                matches!(Skill::parse(&text), Err(ToolsetsError::InvalidSkill { .. })),
                "a YAML null is not text, refused: {frontmatter:?}"
            );
        }
    }

    // Scenario: licence null lue comme absente
    #[test]
    fn yaml_null_license_reads_as_none() {
        let parsed = Skill::parse("---\nname: grafana\ndescription: d\nlicense: ~\n---\nbody\n")
            .expect("a null license parses");
        assert_eq!(parsed.license(), None, "a null license is no license");
    }

    // Scenario: corps trop volumineux refusé
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
