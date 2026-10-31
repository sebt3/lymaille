//! Version requirement of a context type (`cluster@1.2`) and the nature of a version change
//! (`agent-sessions/toolsets#FR-10`, `FR-13`, `FR-14`).

use serde::{Deserialize, Serialize};
use vynil_core::Semver;

use super::{ToolsetsError, ToolsetsResult};

/// Serializes a [`Semver`] as its `major.minor.patch` text and reads it back with [`Semver::parse`]. `vynil_core::Semver`
/// does not implement serde itself.
pub(in crate::toolsets) mod semver_text {
    use serde::Deserialize as _;
    use serde::{Deserializer, Serializer};
    use vynil_core::Semver;

    /// Writes the version as text.
    pub(in crate::toolsets) fn serialize<S: Serializer>(
        value: &Semver,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&value.version.to_string())
    }

    /// Reads a version text; a text that is not a version is refused.
    pub(in crate::toolsets) fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Semver, D::Error> {
        let text = String::deserialize(deserializer)?;
        Semver::opt_parse(&text).ok_or_else(|| <D::Error as serde::de::Error>::custom("invalid version"))
    }
}

/// Requirement on a context type: a type name and a minimal version. A requirement `cluster@1.2` is satisfied by any
/// version of the same major that is greater than or equal to the minimum.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct TypeRequirement {
    name: String,
    #[serde(with = "semver_text")]
    minimum: Semver,
}

impl TypeRequirement {
    /// Builds a requirement from a type name and a minimal version.
    ///
    /// # Errors
    ///
    /// Returns [`ToolsetsError::InvalidToolset`] when the name is empty.
    pub fn new(name: String, minimum: Semver) -> ToolsetsResult<Self> {
        if name.is_empty() {
            return Err(ToolsetsError::InvalidToolset {
                name,
                reason: "the required type name is empty",
                detail: String::new(),
            });
        }
        Ok(Self { name, minimum })
    }

    /// Reads a requirement from its text `name@major.minor` or `name@major.minor.patch`. A two-part version means
    /// `.0` for the patch.
    ///
    /// # Errors
    ///
    /// Returns [`ToolsetsError::InvalidToolset`] when the text has no `@`, when the name is empty, or when the version
    /// is not readable.
    pub fn parse(text: &str) -> ToolsetsResult<Self> {
        let Some((name, version)) = text.split_once('@') else {
            return Err(ToolsetsError::InvalidToolset {
                name: text.to_owned(),
                reason: "the requirement has no `@` between the type name and the version",
                detail: String::new(),
            });
        };
        // A two-part version `major.minor` means `major.minor.0`.
        let full = if version.matches('.').count() == 1 {
            format!("{version}.0")
        } else {
            version.to_owned()
        };
        let minimum = Semver::opt_parse(&full).ok_or_else(|| ToolsetsError::InvalidToolset {
            name: name.to_owned(),
            reason: "the required version is not readable",
            detail: format!("version `{version}`"),
        })?;
        Self::new(name.to_owned(), minimum)
    }

    /// Returns the name of the required type.
    pub(in crate::toolsets) fn name(&self) -> &str {
        &self.name
    }

    /// Returns the minimal version of the requirement. Read by the compatibility check, to name the version in a gap.
    pub(in crate::toolsets) const fn minimum(&self) -> &Semver {
        &self.minimum
    }

    /// Tells whether `version` satisfies the requirement: same major, greater than or equal to the minimum. A
    /// prerelease version never satisfies a requirement.
    #[must_use]
    pub fn is_satisfied_by(&self, version: &Semver) -> bool {
        let candidate = &version.version;
        candidate.pre.is_empty()
            && candidate.major == self.minimum.version.major
            && *candidate >= self.minimum.version
    }
}

/// Nature of a change of version, in increasing order: `None` (no change), `Minor` (an addition that does not break
/// consumers) or `Major` (a breaking change).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[non_exhaustive]
pub enum Bump {
    /// No change of the public contract.
    None,
    /// An addition that breaks no consumer.
    Minor,
    /// A breaking change.
    Major,
}

#[cfg(test)]
mod tests {
    use vynil_core::Semver;

    use super::{Bump, TypeRequirement};
    use crate::toolsets::ToolsetsError;

    fn version(text: &str) -> Semver {
        Semver::parse(text).expect("a sample version must parse")
    }

    fn cluster_1_2() -> TypeRequirement {
        TypeRequirement::parse("cluster@1.2").expect("the sample requirement must parse")
    }

    // Scenario: même majeure, mineure supérieure ou égale
    #[test]
    fn same_major_higher_or_equal_minor_is_satisfied() {
        let requirement = cluster_1_2();
        assert!(
            requirement.is_satisfied_by(&version("1.2.0")),
            "1.2.0 satisfies cluster@1.2"
        );
        assert!(
            requirement.is_satisfied_by(&version("1.3.0")),
            "1.3.0 satisfies cluster@1.2"
        );
    }

    // Scenario: mineure inférieure ou autre majeure
    #[test]
    fn lower_minor_or_other_major_is_not_satisfied() {
        let requirement = cluster_1_2();
        assert!(
            !requirement.is_satisfied_by(&version("1.1.0")),
            "1.1.0 does not satisfy cluster@1.2"
        );
        assert!(
            !requirement.is_satisfied_by(&version("2.0.0")),
            "2.0.0 does not satisfy cluster@1.2"
        );
    }

    // Scenario: exigence mal formée
    #[test]
    fn malformed_requirement_fails_with_invalid_toolset() {
        for text in ["cluster", "@1.2", "cluster@x"] {
            let result = TypeRequirement::parse(text);
            assert!(
                matches!(result, Err(ToolsetsError::InvalidToolset { .. })),
                "the requirement `{text}` must fail with InvalidToolset"
            );
        }
    }

    // Task #1 of error.sdd: an unreadable version is named in the detail, and the type name stays the name.
    #[test]
    fn unreadable_version_is_named_in_the_detail() {
        match TypeRequirement::parse("cluster@x") {
            Err(error @ ToolsetsError::InvalidToolset { .. }) => {
                let text = error.to_string();
                assert!(text.contains("`cluster`"), "the type name is named: {text}");
                assert!(
                    text.contains("(version `x`)"),
                    "the version is the detail: {text}"
                );
            }
            other => panic!("expected InvalidToolset, got {other:?}"),
        }
    }

    #[test]
    fn two_part_requirement_means_patch_zero() {
        let requirement = TypeRequirement::parse("cluster@1.2").expect("a two-part version parses");
        assert!(
            requirement.is_satisfied_by(&version("1.2.0")),
            "1.2 is 1.2.0: that version satisfies it"
        );
        assert!(
            !requirement.is_satisfied_by(&version("1.1.9")),
            "1.1.9 is below 1.2.0 and does not satisfy it"
        );
    }

    #[test]
    fn prerelease_never_satisfies() {
        let requirement = cluster_1_2();
        assert!(
            !requirement.is_satisfied_by(&version("1.3.0-rc.1")),
            "a prerelease never satisfies a requirement"
        );
    }

    #[test]
    fn bump_order_is_none_minor_major() {
        assert!(Bump::None < Bump::Minor, "a minor change is above no change");
        assert!(Bump::Minor < Bump::Major, "a major change is above a minor one");
    }

    #[test]
    fn requirement_round_trips_through_serde() {
        let requirement = cluster_1_2();
        let text = serde_json::to_string(&requirement).expect("a requirement serializes");
        assert!(
            text.contains("\"1.2.0\""),
            "the version is written as a major.minor.patch text: {text}"
        );
        let back: TypeRequirement = serde_json::from_str(&text).expect("a requirement deserializes");
        assert_eq!(back, requirement, "the round trip must be identical");
    }
}
