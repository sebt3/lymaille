//! Agents, providers and model profiles.
//!
//! An agent definition names its model by a profile; a profile names a provider and overrides what the provider detects.
//! The typed error of the module is also here. Pure data and logic, no I/O
//! (`agent-sessions/agents#FR-1`, `FR-6`, `FR-7`, `FR-8`).

mod catalog;
mod definition;
#[cfg(feature = "embedder")]
mod embedder;
mod error;
mod inference;
mod manifest;
mod profile;
mod provider;
#[cfg(feature = "rig")]
mod rig;
mod summarizer;

pub use catalog::{AgentStore, InMemoryCatalog, Layered, ProfileStore, ProviderStore};
pub use definition::{AgentDefinition, AgentKind, SandboxLimits};
#[cfg(feature = "embedder")]
pub use embedder::{
    Embedded, Embedder, EmbedderError, EmbedderErrorKind, EmbedderModel, EmbeddingProfile, RigEmbedder,
    RigEmbedderConfig, TestEmbedder,
};
pub use error::{AgentsError, AgentsResult};
pub use inference::{
    ErrorClass, FinishReason, InferenceError, InferenceProvider, InferenceRequest, InferenceResponse,
    ReportedUsage, ScriptedProvider, classify_status,
};
pub use manifest::{ManifestGap, ManifestView, ProjectManifest};
pub use profile::{DetectedModel, ModelProfile, ResolvedProfile, SamplingParams};
pub use provider::{Provider, ProviderLevel};
#[cfg(feature = "rig")]
pub use rig::{RigConfig, RigProvider, detect_models};
pub use summarizer::ProviderSummarizer;
