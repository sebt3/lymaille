//! Public API of the crate seen from outside its modules: what a host can actually call. Integration tests are the
//! only place where a `pub(in crate::toolsets)` accessor is visibly not public, so the visibility tasks of the
//! accessors land here.

// The harness denies `unused_crate_dependencies`, which also checks the externs of this test crate: every direct
// dependency is named here, the way `src/lib.rs` does for the library. Optional ones follow the same feature gates.
use base64 as _;
use handlebars as _;
use jsonschema as _;
use rhai as _;
use rustls as _;
use serde as _;
use thiserror as _;
use tokio as _;
use tokio_util as _;
use yaml_serde as _;

#[cfg(feature = "mcp-http")]
use http as _;
#[cfg(feature = "rig")]
use rig_core as _;
#[cfg(feature = "mcp-http")]
use rmcp as _;
#[cfg(feature = "ts-rs")]
use ts_rs as _;

#[cfg(test)]
mod tests {
    // Scenario: le nom d'un type se lit (src/toolsets/context_type.sdd)
    #[test]
    fn context_type_name_is_readable() {
        let context_type = lymaille::ContextType::new(
            "G_cluster-2".to_owned(),
            vynil_core::Semver::parse("1.0.0").expect("the sample version must parse"),
            serde_json::json!({ "type": "object" }),
        )
        .expect("the sample type must build");
        assert_eq!(
            context_type.name(),
            "G_cluster-2",
            "the name is read back as it was set at construction, without going through serialization"
        );
    }

    // Scenario: le nom d'un contexte se lit (src/toolsets/toolset_context.sdd)
    #[test]
    fn toolset_context_name_is_readable() {
        let context = lymaille::ToolsetContext::new("prod".to_owned(), lymaille::ContextLevel::Shared)
            .expect("the sample context must build");
        assert_eq!(
            context.name(),
            "prod",
            "the name is read back as it was set at construction, without going through serialization"
        );
    }

    // Scenario: la version minimale se lit (src/toolsets/version.sdd)
    #[test]
    fn type_requirement_minimum_version_is_readable() {
        let minor =
            lymaille::TypeRequirement::parse("cluster@1.2").expect("a two-part requirement must parse");
        let patch =
            lymaille::TypeRequirement::parse("cluster@1.2.3").expect("a three-part requirement must parse");
        assert_eq!(
            minor.minimum(),
            &sample_version("1.2.0"),
            "a two-part requirement reads with patch 0"
        );
        assert_eq!(
            patch.minimum(),
            &sample_version("1.2.3"),
            "a three-part requirement reads exactly"
        );
    }

    /// Builds a sample version; `vynil_core::Semver` is not re-exported by the crate, so a host reads it from the
    /// dependency it pins itself.
    fn sample_version(text: &str) -> vynil_core::Semver {
        vynil_core::Semver::parse(text).expect("a sample version must parse")
    }
}
