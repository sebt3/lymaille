//! Output contract of a toolset, extracted from the AST of its Rhai module: the signatures of its public functions. The
//! contract is compared with the contract of an earlier version to tell the nature of a change, and it is checked against
//! the declared version (`agent-sessions/toolsets#FR-10`, `FR-11`, `FR-12`). Extraction compiles the source and reads the
//! AST; it never runs it.

use std::collections::BTreeMap;
use std::fmt;

use vynil_core::Semver;

use super::{Bump, ToolsetsError, ToolsetsResult};

/// Output contract of a toolset: the public functions, each with the names of its parameters in order.
///
/// A function is keyed `name/arity`, so two functions of the same name and different arities are two entries. A body, a
/// doc-comment or a private function is not part of the contract.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct Contract {
    functions: BTreeMap<String, Vec<String>>,
}

/// Divergence between the contract extracted from a toolset and its declared version. It is a warning, never a refusal
/// (`agent-sessions/toolsets#FR-12`).
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum VersionWarning {
    /// The public signatures changed by `bump`, but the declared version did not change.
    SignatureChangedWithoutVersionChange {
        /// Nature of the signature change, as computed by [`Contract::bump_since`].
        bump: Bump,
        /// The declared version, which stayed the same.
        declared: String,
    },
    /// The declared version changed, but the public signatures did not.
    VersionChangedWithoutSignatureChange {
        /// The declared version before the change.
        previous: String,
        /// The declared version after the change.
        current: String,
    },
    /// The public signatures changed by `bump`, but the declared version changed by less than that: a break declared as
    /// a minor change, for instance.
    UnderDeclaredVersion {
        /// Nature of the signature change, as computed by [`Contract::bump_since`].
        bump: Bump,
        /// The declared version before the change.
        previous: String,
        /// The declared version after the change.
        current: String,
    },
}

impl Contract {
    /// Returns the public functions by `name/arity`, each with the names of its parameters in order.
    #[must_use]
    pub const fn functions(&self) -> &BTreeMap<String, Vec<String>> {
        &self.functions
    }

    /// Computes the nature of the change from `previous`, an earlier contract of the same toolset: a removed function, or
    /// a function whose parameter list changed, is a major change; an added function alone is a minor change; no change
    /// is `None`.
    #[must_use]
    pub fn bump_since(&self, previous: &Self) -> Bump {
        // A function of the previous contract that is absent, or whose parameters changed, is a break.
        let broken = previous
            .functions
            .iter()
            .any(|(key, params)| self.functions.get(key) != Some(params));
        if broken {
            return Bump::Major;
        }
        let added = self
            .functions
            .keys()
            .any(|key| !previous.functions.contains_key(key));
        if added { Bump::Minor } else { Bump::None }
    }
}

impl fmt::Display for VersionWarning {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SignatureChangedWithoutVersionChange { bump, declared } => write!(
                f,
                "the public signatures changed ({}) but the declared version stays {declared}",
                bump_text(*bump)
            ),
            Self::VersionChangedWithoutSignatureChange { previous, current } => write!(
                f,
                "the declared version changed from {previous} to {current} but the public signatures did not"
            ),
            Self::UnderDeclaredVersion {
                bump,
                previous,
                current,
            } => write!(
                f,
                "the public signatures changed ({}) but the declared version changed only from {previous} to {current}",
                bump_text(*bump)
            ),
        }
    }
}

/// The detail of a compilation error: the line and the column that Rhai reports, and the kind of the error. The text of
/// the error is never copied: Rhai puts tokens and literals of the source in it.
fn compile_detail(error: &rhai::ParseError) -> String {
    let position = error.position();
    let place = match (position.line(), position.position()) {
        (Some(line), Some(column)) => format!("line {line}, column {column}"),
        (Some(line), None) => format!("line {line}"),
        _ => "unknown position".to_owned(),
    };
    format!("{place}, {}", error_kind(error.err_type()))
}

/// The kind of a compilation error, named without any text of the source.
const fn error_kind(kind: &rhai::ParseErrorType) -> &'static str {
    if matches!(kind, rhai::ParseErrorType::UnexpectedEOF) {
        "unexpected end of input"
    } else if matches!(kind, rhai::ParseErrorType::BadInput(_)) {
        "bad input"
    } else {
        "syntax error"
    }
}

/// Names the nature of a change, for a warning text.
const fn bump_text(bump: Bump) -> &'static str {
    match bump {
        Bump::None => "no change",
        Bump::Minor => "minor",
        Bump::Major => "major",
    }
}

/// Extracts the output contract of a toolset from the source text of its Rhai module. The source is compiled with a raw
/// engine and its AST is read; nothing is run.
///
/// # Errors
///
/// Returns [`ToolsetsError::InvalidToolset`] when the source does not compile. The detail carries the line and the column
/// that Rhai reports, and the kind of the error; never a fragment of the source.
pub fn extract_contract(source: &str) -> ToolsetsResult<Contract> {
    let engine = rhai::Engine::new_raw();
    let ast = engine
        .compile(source)
        .map_err(|error| ToolsetsError::InvalidToolset {
            name: "<source>".to_owned(),
            reason: "the module does not compile",
            detail: compile_detail(&error),
        })?;
    let mut functions: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for metadata in ast.iter_functions() {
        if metadata.access != rhai::FnAccess::Public {
            continue;
        }
        let params: Vec<String> = metadata.params.iter().map(|param| (*param).to_owned()).collect();
        let name = metadata.name;
        let arity = params.len();
        let _previous = functions.insert(format!("{name}/{arity}"), params);
    }
    Ok(Contract { functions })
}

/// Checks the declared version of a toolset against the change of its contract.
///
/// Returns a warning when the signatures changed without a change of the declared version, or when the declared version
/// changed without a change of the signatures, or when the declared version changed by less than the signatures did (an
/// under-declared version). Returns nothing when both agree.
#[must_use]
pub fn check_declared_version(
    extracted: &Contract,
    previous: &Contract,
    declared_previous: &Semver,
    declared_current: &Semver,
) -> Option<VersionWarning> {
    let bump = extracted.bump_since(previous);
    let version_changed = declared_previous != declared_current;
    match (bump, version_changed) {
        // A fix (patch) that leaves the contract unchanged is not a divergence: only a minor or major declared change is.
        (Bump::None, true) if declared_bump(declared_previous, declared_current) != Bump::None => {
            Some(VersionWarning::VersionChangedWithoutSignatureChange {
                previous: declared_previous.version.to_string(),
                current: declared_current.version.to_string(),
            })
        }
        (Bump::Minor | Bump::Major, false) => Some(VersionWarning::SignatureChangedWithoutVersionChange {
            bump,
            declared: declared_current.version.to_string(),
        }),
        (Bump::Minor | Bump::Major, true) if declared_bump(declared_previous, declared_current) < bump => {
            Some(VersionWarning::UnderDeclaredVersion {
                bump,
                previous: declared_previous.version.to_string(),
                current: declared_current.version.to_string(),
            })
        }
        (Bump::None, _) | (Bump::Minor | Bump::Major, true) => None,
    }
}

/// The nature of the change of the declared version from `previous` to `current`: major when the major number changed,
/// minor when only the minor number did, none otherwise. A patch change is below a minor change.
const fn declared_bump(previous: &Semver, current: &Semver) -> Bump {
    if previous.version.major != current.version.major {
        Bump::Major
    } else if previous.version.minor != current.version.minor {
        Bump::Minor
    } else {
        Bump::None
    }
}

#[cfg(test)]
mod tests {
    use vynil_core::Semver;

    use super::{Contract, VersionWarning, check_declared_version, extract_contract};
    use crate::toolsets::{Bump, ToolsetsError};

    fn contract(source: &str) -> Contract {
        extract_contract(source).expect("the sample module compiles")
    }

    fn version(text: &str) -> Semver {
        Semver::parse(text).expect("a sample version must parse")
    }

    // Measurement, not a Scenario: with the feature set of this crate, the AST exposes the access and the parameter names
    // of each function, and extraction relies on it.
    #[test]
    fn rhai_ast_exposes_access_and_parameter_names() {
        type Seen = (String, rhai::FnAccess, Vec<String>);
        let engine = rhai::Engine::new_raw();
        let ast = engine
            .compile("private fn p(x, y) { x }\nfn q(z) { z }")
            .expect("the sample compiles");
        let mut seen: Vec<Seen> = ast
            .iter_functions()
            .map(|meta| {
                (
                    meta.name.to_owned(),
                    meta.access,
                    meta.params.iter().map(|param| (*param).to_owned()).collect(),
                )
            })
            .collect();
        seen.sort_by(|left, right| left.0.cmp(&right.0));
        assert_eq!(
            seen,
            vec![
                (
                    "p".to_owned(),
                    rhai::FnAccess::Private,
                    vec!["x".to_owned(), "y".to_owned()]
                ),
                ("q".to_owned(), rhai::FnAccess::Public, vec!["z".to_owned()]),
            ],
            "the AST reports the access and the parameter names"
        );
    }

    // Scenario: adding a function is a minor change
    #[test]
    fn adding_a_function_is_a_minor_change() {
        let before = contract("fn first(a) { a }");
        let after = contract("fn first(a) { a }\nfn second() { 2 }");
        assert_eq!(
            after.bump_since(&before),
            Bump::Minor,
            "adding a function is minor"
        );
    }

    // Scenario: body change leaves the contract
    #[test]
    fn body_change_leaves_the_contract() {
        let before = contract("fn f(a) { a + 1 }");
        let after = contract("/// Doubles the input.\nfn f(a) { a * 2 }");
        assert!(
            before.functions().contains_key("f/1"),
            "the contract holds the function, so equality means something"
        );
        assert_eq!(
            after, before,
            "a body and a doc-comment change leave the contract equal"
        );
        assert_eq!(
            after.bump_since(&before),
            Bump::None,
            "no change of the contract is no bump"
        );
    }

    // Scenario: removal is a major change
    #[test]
    fn removal_is_a_major_change() {
        let before = contract("fn a() { 1 }\nfn b() { 2 }");
        let after = contract("fn a() { 1 }");
        assert_eq!(
            after.bump_since(&before),
            Bump::Major,
            "removing a function is major"
        );
    }

    // Scenario: parameter change is a major change
    #[test]
    fn parameter_change_is_a_major_change() {
        let before = contract("fn f(a) { a }");
        let after = contract("fn f(a, b) { a + b }");
        assert_eq!(
            after.bump_since(&before),
            Bump::Major,
            "changing the parameters is major"
        );
    }

    // Task #2 of contract.sdd (posed): a renamed parameter, without a change of count, is a major change.
    #[test]
    fn renamed_parameter_is_a_major_change() {
        let before = contract("fn f(a) { a }");
        let after = contract("fn f(x) { x }");
        assert_eq!(
            after.bump_since(&before),
            Bump::Major,
            "renaming a parameter is major"
        );
    }

    // Task #3 of contract.sdd (posed): two functions of one name and different arities are two entries, keyed name/arity.
    #[test]
    fn overloads_by_arity_are_two_entries() {
        let both = contract("fn f(a) { a }\nfn f(a, b) { a + b }");
        assert!(
            both.functions().contains_key("f/1"),
            "the arity-1 overload is keyed f/1"
        );
        assert!(
            both.functions().contains_key("f/2"),
            "the arity-2 overload is keyed f/2"
        );
        assert_eq!(both.functions().len(), 2, "two entries for the two overloads");
    }

    #[test]
    fn parameter_names_are_kept_in_order() {
        let found = contract("fn f(b, a) { b }");
        assert_eq!(
            found.functions().get("f/2").map(Vec::as_slice),
            Some(["b".to_owned(), "a".to_owned()].as_slice()),
            "the parameter names are listed in their declaration order"
        );
    }

    // Scenario: private functions are outside the contract
    #[test]
    fn private_functions_are_outside_the_contract() {
        let found = contract("private fn hidden() { 1 }\nfn shown() { 2 }");
        assert!(
            found.functions().contains_key("shown/0"),
            "the public function is listed"
        );
        assert!(
            !found.functions().contains_key("hidden/0"),
            "the private function is absent"
        );
    }

    // Must of contract.sdd: extraction compiles the source and never runs it.
    #[test]
    fn extraction_never_runs_the_source() {
        let found = extract_contract("throw \"the module was run\";\nfn f() { 1 }");
        let found = found.expect("a module that would throw if it ran still compiles");
        assert!(found.functions().contains_key("f/0"), "the function is extracted");
    }

    // Raises of contract.sdd: a source that does not compile fails with the position in the message.
    #[test]
    fn uncompilable_source_fails_with_position() {
        let result = extract_contract("fn broken( {");
        match result {
            Err(error @ ToolsetsError::InvalidToolset { .. }) => assert!(
                error.to_string().contains("line 1, column"),
                "the message carries the line and the column reported by Rhai: {error}"
            ),
            other => panic!("expected InvalidToolset, got {other:?}"),
        }
    }

    // Scenario: undeclared change is warned
    #[test]
    fn removed_function_with_unchanged_version_is_warned() {
        let before = contract("fn a() { 1 }\nfn b() { 2 }");
        let after = contract("fn a() { 1 }");
        let warning = check_declared_version(&after, &before, &version("1.0.0"), &version("1.0.0"));
        assert!(
            matches!(
                warning,
                Some(VersionWarning::SignatureChangedWithoutVersionChange {
                    bump: Bump::Major,
                    ..
                })
            ),
            "a removal with the same declared version is warned, got {warning:?}"
        );
    }

    #[test]
    fn version_change_without_signature_change_is_warned() {
        let before = contract("fn a() { 1 }");
        let after = contract("fn a() { 2 }");
        let warning = check_declared_version(&after, &before, &version("1.0.0"), &version("1.1.0"));
        assert!(
            matches!(
                warning,
                Some(VersionWarning::VersionChangedWithoutSignatureChange { .. })
            ),
            "a minor version change without a signature change is warned, got {warning:?}"
        );
    }

    // Task #6 of contract.sdd: a patch change of the declared version (a fix) without a change of the contract is not
    // warned.
    #[test]
    fn patch_change_without_signature_change_is_silent() {
        let before = contract("fn a() { 1 }");
        let after = contract("fn a() { 2 }");
        let warning = check_declared_version(&after, &before, &version("1.0.0"), &version("1.0.1"));
        assert_eq!(
            warning, None,
            "a fix (patch) that leaves the contract unchanged is not warned"
        );
    }

    // Task #6 of contract.sdd: a major version declared for a minor change is over-declared, not under-declared: it is not
    // warned, the rule is only about versions that are too low.
    #[test]
    fn over_declared_major_for_a_minor_change_is_silent() {
        let before = contract("fn a() { 1 }");
        let after = contract("fn a() { 1 }\nfn b() { 2 }");
        let warning = check_declared_version(&after, &before, &version("1.0.0"), &version("2.0.0"));
        assert_eq!(
            warning, None,
            "a major version declared for an added function is over-declared and not warned"
        );
    }

    #[test]
    fn consistent_change_gives_no_warning() {
        let before = contract("fn a() { 1 }");
        let after = contract("fn a() { 1 }\nfn b() { 2 }");
        let warning = check_declared_version(&after, &before, &version("1.0.0"), &version("1.1.0"));
        assert_eq!(
            warning, None,
            "an added function with a minor version is consistent"
        );
    }

    // Task #4 of contract.sdd (posed, no Scenario yet): a removal, a major break, declared as a minor version change is an
    // under-declared version. It is warned, never refused.
    #[test]
    fn under_declared_version_is_warned() {
        let before = contract("fn a() { 1 }\nfn b() { 2 }");
        let after = contract("fn a() { 1 }");
        let warning = check_declared_version(&after, &before, &version("1.0.0"), &version("1.1.0"));
        assert!(
            matches!(
                warning,
                Some(VersionWarning::UnderDeclaredVersion {
                    bump: Bump::Major,
                    ..
                })
            ),
            "a major break declared as a minor change is warned, got {warning:?}"
        );
    }

    // Task #1 of error.sdd: a compile error carries the position and the kind of the error, never a fragment of the source.
    // The precondition checks that the raw Rhai message does carry the fake secret, so the test measures the masking.
    #[test]
    fn compile_error_never_carries_a_source_fragment() {
        let source = "let a = 1;\nlet key = 'sk-FAKE-SECRET-1234';";
        let raw = match rhai::Engine::new_raw().compile(source) {
            Err(error) => error.to_string(),
            Ok(_) => String::new(),
        };
        assert!(
            raw.contains("sk-FAKE-SECRET-1234"),
            "precondition: the raw Rhai message carries the fragment: {raw}"
        );
        match extract_contract(source) {
            Err(error @ ToolsetsError::InvalidToolset { .. }) => {
                let text = error.to_string();
                assert!(
                    !text.contains("sk-FAKE"),
                    "no fragment of the source in the message: {text}"
                );
                assert!(text.contains("line 2"), "the line is named: {text}");
                assert!(text.contains("column"), "the column is named: {text}");
            }
            other => panic!("expected InvalidToolset, got {other:?}"),
        }
    }
}
