//! Version of the session format and its compatibility rule at restore: minor revisions only add optional fields, and only
//! an incompatible major version refuses the restore (`agent-sessions/session#FR-10`).

use std::fmt;

use serde::{Deserialize, Serialize};

use super::{SessionError, SessionResult};

/// Version of the session format, as `major.minor`. Serializes as `{"major": n, "minor": n}`; unknown fields are ignored
/// when read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct FormatVersion {
    major: u32,
    minor: u32,
}

impl FormatVersion {
    /// The format version this library reads and writes.
    pub const CURRENT: Self = Self { major: 1, minor: 0 };

    /// Builds a format version from its major and minor numbers.
    #[must_use]
    pub const fn new(major: u32, minor: u32) -> Self {
        Self { major, minor }
    }

    /// Returns the major number.
    #[must_use]
    pub const fn major(self) -> u32 {
        self.major
    }

    /// Returns the minor number.
    #[must_use]
    pub const fn minor(self) -> u32 {
        self.minor
    }

    /// Checks a saved version against [`FormatVersion::CURRENT`]. Any minor number is accepted when the major numbers
    /// are equal; a different major number is refused.
    ///
    /// # Errors
    ///
    /// Returns [`SessionError::IncompatibleFormat`], carrying both versions, when the major numbers differ.
    pub const fn check(self) -> SessionResult<()> {
        if self.major == Self::CURRENT.major {
            Ok(())
        } else {
            Err(SessionError::IncompatibleFormat {
                saved: self,
                current: Self::CURRENT,
            })
        }
    }
}

impl fmt::Display for FormatVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}", self.major, self.minor)
    }
}

#[cfg(test)]
mod tests {
    use super::FormatVersion;
    use crate::session::SessionError;

    #[test]
    fn same_version_accepted() {
        assert_eq!(
            FormatVersion::CURRENT,
            FormatVersion::new(1, 0),
            "the current format version must be 1.0 for the first milestone"
        );
        assert!(
            FormatVersion::CURRENT.check().is_ok(),
            "a saved version equal to the current one must be accepted"
        );
    }

    #[test]
    fn newer_minor_accepted() {
        let saved = FormatVersion::new(1, 7);
        assert!(
            saved.check().is_ok(),
            "a saved minor newer than the current one, with the same major, must be accepted"
        );
    }

    #[test]
    fn different_major_refused() {
        for saved in [FormatVersion::new(2, 0), FormatVersion::new(0, 9)] {
            let Err(SessionError::IncompatibleFormat {
                saved: reported,
                current,
            }) = saved.check()
            else {
                panic!(
                    "a saved major {} differing from the current one must be refused",
                    saved.major()
                );
            };
            assert_eq!(reported, saved, "the refusal must name the saved version {saved}");
            assert_eq!(
                current,
                FormatVersion::CURRENT,
                "the refusal must name the current version"
            );
        }
    }

    #[test]
    fn serializes_as_a_major_minor_object() {
        let json = serde_json::to_string(&FormatVersion::new(1, 7)).expect("a format version serializes");
        assert_eq!(
            json, r#"{"major":1,"minor":7}"#,
            "the format version must serialize as a major and minor object"
        );
    }

    #[test]
    fn unknown_field_ignored_when_read() {
        let version: FormatVersion = serde_json::from_str(r#"{"major":1,"minor":9,"extra":true}"#)
            .expect("an object with an unknown field must still be read");
        assert_eq!(
            version,
            FormatVersion::new(1, 9),
            "the unknown field must be ignored and the version read as 1.9"
        );
    }
}
