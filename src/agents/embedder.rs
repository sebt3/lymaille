//! Text embedding port, at the same level as the inference provider: the [`Embedder`] trait, its typed errors without
//! leak, the profile of an embedding model, the batch splitting, the signed truncation, and a deterministic test
//! implementation that needs no network (`agent-sessions/agents#FR-12`, `FR-13`, `FR-14`, `AC-6`, `AC-7`,
//! `agent-sessions#DEC-15`).
//!
//! The library stores no vector (`agent-sessions#CON-1`): the host persists each vector with the model identifier that
//! produced it, and invalidates its index when that identifier changes.

use std::fmt;
use std::num::NonZeroUsize;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use rig_core::ErrorKind;
use rig_core::ProviderError;
use rig_core::driver::{DynModel, Exchange, Local, Model, Opened, Opening, Step, Transport};
use rig_core::embeddings::{Embedding, EmbeddingResponse};
use rig_core::operation::Embedding as EmbeddingOperation;
use rig_core::providers::openai::wire::{Embeddings, LLAMACPP, OpenAIConfig};
use rig_core::wire::Capabilities;
use serde::{Deserialize, Serialize};

/// Port of text embedding. It is `Send + Sync` and used through a generic parameter only, never as a trait object.
///
/// Every implementation keeps the guarantees of the port: one vector per input, in the order of the inputs, all of the
/// announced dimension, the truncation of an input over the limit signalled by [`Embedded::truncated`], and errors that
/// show neither an address, nor a key, nor an embedded text.
pub trait Embedder: Send + Sync {
    /// Returns the identifier of the embedding model. A vector stored by the host carries it.
    fn model_id(&self) -> &str;

    /// Returns the dimension of the vectors this embedder produces.
    fn dimensions(&self) -> usize;

    /// Embeds `texts`, in order, and returns one [`Embedded`] per input.
    ///
    /// An empty batch returns an empty batch without any call to the service.
    ///
    /// # Errors
    ///
    /// Returns an [`EmbedderError`] carrying its [`EmbedderErrorKind`] when the batch cannot be embedded. No partial
    /// result is returned: a failure of one slice of a batch fails the whole batch.
    fn embed(&self, texts: &[String]) -> impl Future<Output = Result<Vec<Embedded>, EmbedderError>> + Send;
}

/// Any `Arc<E>` is an embedder when `E` is one: the calls delegate to `E`, with the same results and the same errors, so a
/// host can share one embedder between its indexing and its queries.
impl<E: Embedder + ?Sized> Embedder for Arc<E> {
    fn model_id(&self) -> &str {
        (**self).model_id()
    }

    fn dimensions(&self) -> usize {
        (**self).dimensions()
    }

    fn embed(&self, texts: &[String]) -> impl Future<Output = Result<Vec<Embedded>, EmbedderError>> + Send {
        (**self).embed(texts)
    }
}

/// The vector of one input, with the signal of its truncation.
///
/// `truncated` is `true` when the input was longer than the character limit of the embedder and was cut to that limit
/// before embedding. An input within the limit is never modified and carries `false`.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct Embedded {
    vector: Vec<f64>,
    truncated: bool,
}

impl Embedded {
    /// Builds the embedded input of a vector and of its truncation signal.
    #[must_use]
    pub const fn new(vector: Vec<f64>, truncated: bool) -> Self {
        Self { vector, truncated }
    }

    /// Returns the vector.
    #[must_use]
    pub fn vector(&self) -> &[f64] {
        &self.vector
    }

    /// Returns `true` when the input was truncated to the character limit before embedding.
    #[must_use]
    pub const fn truncated(&self) -> bool {
        self.truncated
    }

    /// Consumes the embedded input and returns its vector.
    #[must_use]
    pub fn into_vector(self) -> Vec<f64> {
        self.vector
    }
}

/// Class of an embedding error, which tells the caller what to do with it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum EmbedderErrorKind {
    /// The service is unreachable, timed out, or answered 429 or a 5xx status: the failure may go away, and the caller may
    /// retry. The embedder never retries by itself.
    Unavailable,
    /// The service refused the request with a permanent 4xx status, a 400 that signals an input too long for the service
    /// included.
    Rejected,
    /// The reply cannot be read: a body that is not JSON, an error envelope under a success status, or a missing `usage`
    /// the dialect requires.
    Malformed,
    /// A vector does not have the dimension of the profile. Carries the received and the expected dimensions.
    UnexpectedDimension {
        /// Dimension of the vector the service returned.
        received: usize,
        /// Dimension announced by the profile.
        expected: usize,
    },
    /// The service did not return one vector per input. Carries the received and the expected counts.
    WrongCount {
        /// Number of vectors the service returned.
        received: usize,
        /// Number of inputs of the batch.
        expected: usize,
    },
}

/// Error of an embedding call. Its message is in English and never carries an address, a key nor an embedded text.
#[derive(Debug, thiserror::Error)]
#[error("{message}")]
#[non_exhaustive]
pub struct EmbedderError {
    kind: EmbedderErrorKind,
    message: String,
}

impl EmbedderError {
    /// Builds an error of class `kind` with the text `message`.
    #[must_use]
    pub const fn new(kind: EmbedderErrorKind, message: String) -> Self {
        Self { kind, message }
    }

    /// Returns the class of the error.
    #[must_use]
    pub const fn kind(&self) -> EmbedderErrorKind {
        self.kind
    }

    /// Returns the message of the error.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }
}

/// Profile of an embedding model: the provider that serves it, the model identifier, the dimension of its vectors, the
/// maximum number of texts per request, and the input limit in characters.
///
/// It is a different type from the chat model profile: it has no context size and no sampling parameter, and it names
/// its provider the same way. It holds no address and no secret.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct EmbeddingProfile {
    name: String,
    provider: String,
    model: String,
    dimensions: NonZeroUsize,
    max_batch: NonZeroUsize,
    char_limit: NonZeroUsize,
}

impl EmbeddingProfile {
    /// Builds the profile of the embedding model `model`, served by the provider `provider`.
    ///
    /// `max_batch` is the maximum number of texts of one request, `char_limit` the maximum length of one input in
    /// characters.
    #[must_use]
    #[expect(
        clippy::too_many_arguments,
        reason = "the six fields of the profile are the contract of the spec (embedder.sdd, Accepts); a builder would hide them"
    )]
    pub const fn new(
        name: String,
        provider: String,
        model: String,
        dimensions: NonZeroUsize,
        max_batch: NonZeroUsize,
        char_limit: NonZeroUsize,
    ) -> Self {
        Self {
            name,
            provider,
            model,
            dimensions,
            max_batch,
            char_limit,
        }
    }

    /// Returns the name of the profile.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns the name of the provider that serves the model.
    #[must_use]
    pub fn provider(&self) -> &str {
        &self.provider
    }

    /// Returns the identifier of the embedding model.
    #[must_use]
    pub fn model(&self) -> &str {
        &self.model
    }

    /// Returns the expected dimension of the vectors.
    #[must_use]
    pub const fn dimensions(&self) -> NonZeroUsize {
        self.dimensions
    }

    /// Returns the maximum number of texts of one request.
    #[must_use]
    pub const fn max_batch(&self) -> NonZeroUsize {
        self.max_batch
    }

    /// Returns the input limit in characters.
    #[must_use]
    pub const fn char_limit(&self) -> NonZeroUsize {
        self.char_limit
    }
}

/// Model identifier of [`TestEmbedder`].
const TEST_MODEL_ID: &str = "test-embedder";

/// Deterministic embedder for tests: no network, the same vectors for the same inputs, and the same guarantees as the
/// production client (truncation signed by [`Embedded::truncated`], dimension).
///
/// The vector of a text is built from its words (case and punctuation ignored) and from the whole text, so two texts that
/// share words are closer than two texts that share none, and two different texts have different vectors. Collisions
/// are possible when the dimension is small, as for any hashing of a finite space.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct TestEmbedder {
    dimensions: NonZeroUsize,
    char_limit: NonZeroUsize,
}

impl TestEmbedder {
    /// Builds the embedder of vectors of dimension `dimensions`, with inputs cut to `char_limit` characters.
    #[must_use]
    pub const fn new(dimensions: NonZeroUsize, char_limit: NonZeroUsize) -> Self {
        Self {
            dimensions,
            char_limit,
        }
    }
}

impl Embedder for TestEmbedder {
    fn model_id(&self) -> &str {
        TEST_MODEL_ID
    }

    fn dimensions(&self) -> usize {
        self.dimensions.get()
    }

    fn embed(&self, texts: &[String]) -> impl Future<Output = Result<Vec<Embedded>, EmbedderError>> + Send {
        let embedded: Vec<Embedded> = texts
            .iter()
            .map(|text| {
                let (kept, truncated) = truncate(text, self.char_limit.get());
                Embedded::new(hashed_vector(&kept, self.dimensions.get()), truncated)
            })
            .collect();
        std::future::ready(Ok(embedded))
    }
}

/// Weight of the whole text in its vector, against a weight of 1 for each of its words.
const TEXT_WEIGHT: f64 = 0.5;

/// Cuts `text` to `limit` characters, on a character boundary, and tells whether it was cut.
fn truncate(text: &str, limit: usize) -> (String, bool) {
    let truncated = text.chars().nth(limit).is_some();
    (text.chars().take(limit).collect(), truncated)
}

/// Builds the vector of `text` in `dimensions` buckets: each word adds 1 to its bucket, the whole text adds
/// [`TEXT_WEIGHT`] to its own bucket, and the result is normalized to unit length. The weight of the text keeps the norm
/// non-zero, even for an empty text.
fn hashed_vector(text: &str, dimensions: usize) -> Vec<f64> {
    let mut vector = vec![0.0f64; dimensions];
    let lowered = text.to_lowercase();
    for word in lowered
        .split(|character: char| !character.is_alphanumeric())
        .filter(|word| !word.is_empty())
    {
        add_to_bucket(&mut vector, fnv1a(word.as_bytes()), 1.0);
    }
    add_to_bucket(&mut vector, fnv1a(text.as_bytes()), TEXT_WEIGHT);
    let norm = vector.iter().map(|value| value * value).sum::<f64>().sqrt();
    if norm > 0.0 {
        for value in &mut vector {
            *value /= norm;
        }
    }
    vector
}

/// Adds `weight` to the bucket that `hash` selects among the vector's buckets.
fn add_to_bucket(vector: &mut [f64], hash: u64, weight: f64) {
    let buckets = u64::try_from(vector.len()).unwrap_or(u64::MAX);
    let slot = hash
        .checked_rem(buckets)
        .and_then(|bucket| usize::try_from(bucket).ok())
        .and_then(|bucket| vector.get_mut(bucket));
    if let Some(value) = slot {
        *value += weight;
    }
}

/// FNV-1a, 64 bits: a hash that is the same on every run and every platform, unlike the default hasher of the standard
/// library.
fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325u64, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x0100_0000_01b3)
    })
}

/// Configuration of [`RigEmbedder`]: the profile of the model, and the address and the optional key of the provider,
/// both resolved by the host (plain text, never a reference, as for `RigConfig`).
///
/// The base address carries the version segment (`/v1`): a request goes to `POST <base>/embeddings`. The key is
/// optional: an empty key is no key. The `Debug` output shows neither the address nor the key.
#[derive(Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct RigEmbedderConfig {
    profile: EmbeddingProfile,
    base_url: String,
    api_key: Option<String>,
}

impl RigEmbedderConfig {
    /// Builds the configuration of the profile `profile`, served at `base_url`, with the optional key `api_key`.
    #[must_use]
    pub fn new(profile: EmbeddingProfile, base_url: String, api_key: Option<String>) -> Self {
        Self {
            profile,
            base_url,
            api_key: api_key.filter(|key| !key.is_empty()),
        }
    }

    /// Builds the `rig` embedding model of the configuration. No network call happens here.
    fn model(&self) -> Model<Embeddings> {
        OpenAIConfig::with_key(&LLAMACPP, self.api_key.clone().unwrap_or_default())
            .with_base_url(self.base_url.clone())
            .client()
            .embedding(self.profile.model(), Some(self.profile.dimensions().get()))
    }
}

impl fmt::Debug for RigEmbedderConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RigEmbedderConfig").finish_non_exhaustive()
    }
}

/// Production embedder: the embedding client of `rig` (OpenAI-compatible `POST <base>/embeddings`) behind the guarantees
/// of [`Embedder`].
///
/// It splits a batch in requests of the maximum size of the profile, and checks the count and the dimension of the
/// vectors that come back. It never relays the message of a `rig` error, which carries the address and the body: its
/// messages are built from the class of the error and from the HTTP status only.
///
/// The host must have installed a rustls crypto provider beforehand: `reqwest` refuses to build its client without one,
/// even for an `http://` address. The embedder sends no retry: the caller decides what an `Unavailable` error deserves.
///
/// Known limit: `rig` pairs each vector with its input by position and ignores the `index` field of the answer. The
/// order of the request is therefore the contract with the service; an out-of-order answer is not detectable here and was
/// not measured against a real server.
pub struct RigEmbedder {
    model: Model<Embeddings>,
    profile: EmbeddingProfile,
}

impl RigEmbedder {
    /// Builds the embedder of `config`, without any network call.
    #[must_use]
    pub fn new(config: &RigEmbedderConfig) -> Self {
        Self {
            model: config.model(),
            profile: config.profile.clone(),
        }
    }
}

impl fmt::Debug for RigEmbedder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RigEmbedder").finish_non_exhaustive()
    }
}

impl Embedder for RigEmbedder {
    fn model_id(&self) -> &str {
        self.profile.model()
    }

    fn dimensions(&self) -> usize {
        self.profile.dimensions().get()
    }

    async fn embed(&self, texts: &[String]) -> Result<Vec<Embedded>, EmbedderError> {
        let limit = self.profile.char_limit().get();
        let dimension = self.profile.dimensions().get();
        let prepared: Vec<(String, bool)> = texts.iter().map(|text| truncate(text, limit)).collect();
        let mut embedded = Vec::with_capacity(texts.len());
        for batch in prepared.chunks(self.profile.max_batch().get()) {
            let inputs: Vec<String> = batch.iter().map(|(text, _)| text.clone()).collect();
            let response = self
                .model
                .call(inputs)
                .await
                .map_err(|error| classify_provider(&error))?;
            let vectors = checked_vectors(response.embeddings, batch.len(), dimension)?;
            embedded.extend(
                vectors
                    .into_iter()
                    .zip(batch)
                    .map(|(vector, (_, truncated))| Embedded::new(vector, *truncated)),
            );
        }
        Ok(embedded)
    }
}

/// Message of a vector whose dimension is not the one of the profile. It names no value of the request.
const DIMENSION_MESSAGE: &str =
    "the embedding service returned vectors of another dimension than the profile";

/// Checks the count and the dimension of the vectors of one request, and returns them in the order of the answer.
fn checked_vectors(
    embeddings: Vec<Embedding>,
    expected: usize,
    dimension: usize,
) -> Result<Vec<Vec<f64>>, EmbedderError> {
    let received = embeddings.len();
    if received != expected {
        let message = format!("the embedding service returned {received} vectors for {expected} inputs");
        return Err(EmbedderError::new(
            EmbedderErrorKind::WrongCount { received, expected },
            message,
        ));
    }
    if let Some(found) = embeddings
        .iter()
        .map(|embedding| embedding.vec.len())
        .find(|length| *length != dimension)
    {
        return Err(EmbedderError::new(
            EmbedderErrorKind::UnexpectedDimension {
                received: found,
                expected: dimension,
            },
            DIMENSION_MESSAGE.to_owned(),
        ));
    }
    Ok(embeddings.into_iter().map(|embedding| embedding.vec).collect())
}

/// Reads the counts of the count mismatch that `rig` raises as text only, `provider returned N embeddings for M
/// documents` (`rig-core` 0.43.0, `operation/modality.rs`, pinned by `Cargo.toml`). Only the two numbers are kept: the
/// text itself is never relayed.
fn count_reported_by_rig(text: &str) -> Option<(usize, usize)> {
    let mut words = text.split_whitespace();
    if words.next()? != "provider" || words.next()? != "returned" {
        return None;
    }
    let received = words.next()?.parse().ok()?;
    if words.next()? != "embeddings" || words.next()? != "for" {
        return None;
    }
    let expected = words.next()?.parse().ok()?;
    (words.next()? == "documents").then_some((received, expected))
}

/// Classifies a failure of the `rig` client. The message is built from the class and the HTTP status only: the `Display`
/// of a `rig` error carries the address and the body, and is never relayed.
fn classify_provider(error: &ProviderError) -> EmbedderError {
    if let ProviderError::MismatchedDimensions {
        requested, returned, ..
    } = error
    {
        return EmbedderError::new(
            EmbedderErrorKind::UnexpectedDimension {
                received: *returned,
                expected: *requested,
            },
            DIMENSION_MESSAGE.to_owned(),
        );
    }
    if let Some(status) = error.provider_response_status() {
        let code = status.as_u16();
        return if code == 429 || status.is_server_error() {
            EmbedderError::new(
                EmbedderErrorKind::Unavailable,
                format!("the embedding service answered HTTP status {code}"),
            )
        } else {
            EmbedderError::new(
                EmbedderErrorKind::Rejected,
                format!("the embedding service rejected the request with HTTP status {code}"),
            )
        };
    }
    if let ProviderError::Response(text) = error
        && let Some((received, expected)) = count_reported_by_rig(text)
    {
        return EmbedderError::new(
            EmbedderErrorKind::WrongCount { received, expected },
            format!("the embedding service returned {received} vectors for {expected} inputs"),
        );
    }
    match error.kind() {
        ErrorKind::Http if error.is_retryable() => EmbedderError::new(
            EmbedderErrorKind::Unavailable,
            "the embedding service is unreachable or timed out".to_owned(),
        ),
        ErrorKind::Http | ErrorKind::Json | ErrorKind::Response | ErrorKind::ProviderResponse => {
            EmbedderError::new(
                EmbedderErrorKind::Malformed,
                "the embedding service answered an unreadable or incomplete response".to_owned(),
            )
        }
        ErrorKind::Url
        | ErrorKind::Request
        | ErrorKind::Provider
        | ErrorKind::Tool(_)
        | ErrorKind::MemoryBackend
        | ErrorKind::MemoryPolicy
        | ErrorKind::Internal
        | ErrorKind::Cancelled
        | ErrorKind::Timeout
        | ErrorKind::BusClosed
        | ErrorKind::HandlerUnavailable
        | ErrorKind::Divergence
        | ErrorKind::Denied
        | ErrorKind::Other => EmbedderError::new(
            EmbedderErrorKind::Rejected,
            "the embedding request failed".to_owned(),
        ),
    }
}

/// Adapter that makes an [`Embedder`] a model of `rig`, for the index of `rig` the host chooses.
///
/// The index passes its queries and its documents through the guarantees of the embedder.
/// The model names `model_id` as its identifier and the dimension of the embedder as its `ndims`. Its `max_documents` is
/// `usize::MAX`: every [`Embedder`] already splits its batches (`agent-sessions/agents#FR-14`), so `rig` never has to.
/// The vectors pass through the model as they are; the texts are not relayed to `rig`.
///
/// The constructor returns the `rig` model itself, which is what an index takes. It does not store any vector.
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub struct EmbedderModel;

impl EmbedderModel {
    /// Builds the `rig` model of the shared embedder `embedder`. No network call happens here.
    #[must_use]
    #[expect(
        clippy::new_ret_no_self,
        reason = "the spec fixes the constructor's result to the `rig` model the index takes (embedder.sdd, Accepts and Returns), not a value of this type"
    )]
    pub fn new<E: Embedder + 'static>(embedder: Arc<E>) -> DynModel<EmbeddingOperation> {
        let capabilities = Capabilities::embedding(usize::MAX, embedder.dimensions());
        let wire = Local::<EmbeddingOperation>::new("embedder")
            .with_id(embedder.model_id().to_owned())
            .with_capabilities(capabilities);
        Model::new(wire, EmbedderTransport { embedder }).erase()
    }
}

/// The transport that `rig` drives in place of a network: each call embeds its texts with the embedder and answers with
/// the vectors only.
struct EmbedderTransport<E> {
    embedder: Arc<E>,
}

impl<E> Clone for EmbedderTransport<E> {
    fn clone(&self) -> Self {
        Self {
            embedder: Arc::clone(&self.embedder),
        }
    }
}

impl<E: Embedder + 'static> Transport<Local<EmbeddingOperation>> for EmbedderTransport<E> {
    fn send(&self, payload: Vec<String>, exchange: Exchange) -> Opening<Step<EmbeddingOperation>> {
        let _exchange = exchange;
        let shared = Arc::clone(&self.embedder);
        Opening::new(async move {
            let embedded = shared
                .embed(&payload)
                .await
                .map_err(|error| ProviderError::Provider(error.message().to_owned()))?;
            // The answer carries the vectors only: the texts never leave the embedder.
            let embeddings = embedded
                .into_iter()
                .map(|one| Embedding {
                    document: String::new(),
                    vec: one.into_vector(),
                })
                .collect();
            let response = EmbeddingResponse::new(embeddings);
            Ok(Opened::new(OnceFrame(Some(Ok(Step::End(response))))))
        })
    }
}

/// Stream of the single frame of an embedding reply, which is always whole.
struct OnceFrame<T>(Option<T>);

impl<T: Unpin> futures_core::Stream for OnceFrame<T> {
    type Item = T;

    fn poll_next(mut self: Pin<&mut Self>, _context: &mut Context<'_>) -> Poll<Option<T>> {
        Poll::Ready(self.0.take())
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroUsize;
    use std::sync::Arc;

    use super::{Embedded, Embedder, EmbeddingProfile, TestEmbedder};

    fn size(value: usize) -> NonZeroUsize {
        NonZeroUsize::new(value).expect("a test size is not zero")
    }

    fn texts(items: &[&str]) -> Vec<String> {
        items.iter().map(|item| (*item).to_owned()).collect()
    }

    fn cosine(left: &[f64], right: &[f64]) -> f64 {
        let dot: f64 = left.iter().zip(right).map(|(a, b)| a * b).sum();
        let norm_left = left.iter().map(|a| a * a).sum::<f64>().sqrt();
        let norm_right = right.iter().map(|b| b * b).sum::<f64>().sqrt();
        dot / (norm_left * norm_right)
    }

    #[tokio::test]
    async fn nominal_batch_gives_one_vector_per_input_in_order() {
        let embedder = TestEmbedder::new(size(8), size(100));
        let inputs = texts(&["alpha beta", "gamma", "delta epsilon zeta"]);
        let batch = embedder
            .embed(&inputs)
            .await
            .expect("the test embedder does not fail");
        assert_eq!(batch.len(), 3);
        for (input, embedded) in inputs.iter().zip(&batch) {
            assert_eq!(embedded.vector().len(), 8);
            assert!(!embedded.truncated());
            let alone = embedder
                .embed(std::slice::from_ref(input))
                .await
                .expect("the test embedder does not fail");
            assert_eq!(alone.first().map(Embedded::vector), Some(embedded.vector()));
        }
    }

    #[tokio::test]
    async fn same_text_gives_same_vector_and_other_text_another() {
        let embedder = TestEmbedder::new(size(16), size(100));
        let batch = embedder
            .embed(&texts(&["a plain text", "a plain text", "another text"]))
            .await
            .expect("the test embedder does not fail");
        assert_eq!(batch.len(), 3);
        assert_eq!(batch[0].vector(), batch[1].vector());
        assert_ne!(batch[0].vector(), batch[2].vector());
    }

    #[tokio::test]
    async fn shared_words_bring_vectors_closer_than_no_shared_word() {
        let embedder = TestEmbedder::new(size(64), size(200));
        let batch = embedder
            .embed(&texts(&[
                "the quick brown fox",
                "the quick red fox jumps",
                "zebra galaxy orbit nebula",
            ]))
            .await
            .expect("the test embedder does not fail");
        let shared = cosine(batch[0].vector(), batch[1].vector());
        assert!(shared > cosine(batch[0].vector(), batch[2].vector()));
        assert!(shared > cosine(batch[1].vector(), batch[2].vector()));
    }

    #[tokio::test]
    async fn empty_batch_gives_empty_batch() {
        let embedder = TestEmbedder::new(size(8), size(100));
        let batch = embedder
            .embed(&[])
            .await
            .expect("the test embedder does not fail");
        assert_eq!(batch, Vec::<Embedded>::new());
    }

    #[tokio::test]
    async fn long_input_is_cut_on_a_character_boundary_and_signalled() {
        let embedder = TestEmbedder::new(size(32), size(10));
        let long = "\u{e9}".repeat(25);
        let short = "h\u{e9}llo";
        let prefix = "\u{e9}".repeat(10);
        let batch = embedder
            .embed(&texts(&[&long, short]))
            .await
            .expect("the test embedder does not fail");
        assert_eq!(batch.len(), 2);
        assert!(batch[0].truncated());
        assert!(!batch[1].truncated());
        let cut = embedder
            .embed(&texts(&[&prefix]))
            .await
            .expect("the test embedder does not fail");
        assert_eq!(batch[0].vector(), cut[0].vector());
        let whole = embedder
            .embed(&texts(&[short]))
            .await
            .expect("the test embedder does not fail");
        assert_eq!(batch[1].vector(), whole[0].vector());
    }

    #[tokio::test]
    async fn input_of_exactly_the_limit_is_unchanged_and_one_more_is_cut() {
        let limited = TestEmbedder::new(size(32), size(10));
        let exact = "\u{e9}".repeat(10);
        let over = "\u{e9}".repeat(11);
        let batch = limited
            .embed(&texts(&[&exact, &over]))
            .await
            .expect("the test embedder does not fail");
        assert!(!batch[0].truncated());
        assert!(batch[1].truncated());
        let unlimited = TestEmbedder::new(size(32), size(1000));
        let whole = unlimited
            .embed(&texts(&[&exact]))
            .await
            .expect("the test embedder does not fail");
        assert_eq!(batch[0].vector(), whole[0].vector());
    }

    #[tokio::test]
    async fn shared_embedder_behaves_as_the_embedder() {
        let embedder = Arc::new(TestEmbedder::new(size(8), size(100)));
        let inputs = texts(&["one", "two"]);
        assert_eq!(embedder.model_id(), "test-embedder");
        assert_eq!(Embedder::dimensions(&embedder), 8);
        let through_arc = embedder
            .embed(&inputs)
            .await
            .expect("the test embedder does not fail");
        let direct = embedder
            .as_ref()
            .embed(&inputs)
            .await
            .expect("the test embedder does not fail");
        assert_eq!(through_arc.len(), 2);
        assert_eq!(through_arc, direct);
    }

    #[test]
    fn profile_round_trips_and_refuses_a_zero_batch() {
        let profile = EmbeddingProfile::new(
            "embed".to_owned(),
            "local".to_owned(),
            "bge-small".to_owned(),
            size(384),
            size(32),
            size(8000),
        );
        assert_eq!(profile.model(), "bge-small");
        assert_eq!(profile.max_batch().get(), 32);
        let text = serde_json::to_string(&profile).expect("a profile serializes");
        let back: EmbeddingProfile = serde_json::from_str(&text).expect("a profile deserializes");
        assert_eq!(back, profile);
        let zero = text.replace("\"max_batch\":32", "\"max_batch\":0");
        let refused = serde_json::from_str::<EmbeddingProfile>(&zero).is_err();
        assert!(refused, "a zero batch is refused");
    }
}

#[cfg(test)]
mod rig_tests {
    use std::num::NonZeroUsize;
    use std::sync::{Arc, Mutex, PoisonError};

    use serde_json::{Value, json};
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
    use tokio::net::{TcpListener, TcpStream};

    use rig_core::embeddings::Embedding;

    use super::{
        Embedded, Embedder as _, EmbedderError, EmbedderErrorKind, EmbeddingProfile, RigEmbedder,
        RigEmbedderConfig, checked_vectors,
    };

    /// Answers the inputs of one request with a status and a body.
    type Handler = dyn Fn(&[String]) -> (u16, String) + Send + Sync;

    /// A request the fake server received: its head and its body.
    #[derive(Clone)]
    struct Seen {
        head: String,
        body: String,
    }

    impl Seen {
        /// The texts of the request, in order.
        fn inputs(&self) -> Vec<String> {
            serde_json::from_str::<Value>(&self.body)
                .ok()
                .and_then(|document| document.get("input").and_then(Value::as_array).cloned())
                .unwrap_or_default()
                .iter()
                .filter_map(|input| input.as_str().map(str::to_owned))
                .collect()
        }
    }

    /// A local OpenAI-compatible server that records every request it receives.
    struct Fake {
        base: String,
        seen: Arc<Mutex<Vec<Seen>>>,
    }

    impl Fake {
        fn requests(&self) -> Vec<Seen> {
            self.seen.lock().unwrap_or_else(PoisonError::into_inner).clone()
        }
    }

    /// Installs the TLS provider that the host owns in production. An error means one is already installed.
    fn install_tls_provider() {
        let _installed = rustls::crypto::ring::default_provider().install_default();
    }

    /// Starts a fake server that answers every request with `handler`.
    async fn fake(handler: impl Fn(&[String]) -> (u16, String) + Send + Sync + 'static) -> Fake {
        install_tls_provider();
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("a local port can be bound");
        let address = listener.local_addr().expect("a bound listener has an address");
        let seen = Arc::new(Mutex::new(Vec::new()));
        let log = Arc::clone(&seen);
        let handler: Arc<Handler> = Arc::new(handler);
        let _acceptor = tokio::spawn(async move {
            while let Ok((stream, _peer)) = listener.accept().await {
                let _connection = tokio::spawn(serve(stream, Arc::clone(&handler), Arc::clone(&log)));
            }
        });
        Fake {
            base: format!("http://{address}/v1"),
            seen,
        }
    }

    /// Reads one request, records it, and answers it.
    async fn serve(mut stream: TcpStream, handler: Arc<Handler>, log: Arc<Mutex<Vec<Seen>>>) {
        let mut buffer = Vec::new();
        let mut chunk = [0u8; 4096];
        let head_end = loop {
            if let Some(position) = buffer.windows(4).position(|window| window == b"\r\n\r\n") {
                break position;
            }
            let Ok(read) = stream.read(&mut chunk).await else {
                return;
            };
            if read == 0 {
                return;
            }
            buffer.extend_from_slice(&chunk[..read]);
        };
        let head = String::from_utf8_lossy(&buffer[..head_end]).into_owned();
        let length: usize = head
            .lines()
            .find_map(|line| {
                let lower = line.to_ascii_lowercase();
                lower
                    .strip_prefix("content-length:")
                    .and_then(|value| value.trim().parse().ok())
            })
            .unwrap_or(0);
        let mut body = buffer
            .get(head_end.saturating_add(4)..)
            .unwrap_or_default()
            .to_vec();
        while body.len() < length {
            let Ok(read) = stream.read(&mut chunk).await else {
                return;
            };
            if read == 0 {
                return;
            }
            body.extend_from_slice(&chunk[..read]);
        }
        let seen = Seen {
            head,
            body: String::from_utf8_lossy(&body).into_owned(),
        };
        let (status, answer) = handler(&seen.inputs());
        log.lock().unwrap_or_else(PoisonError::into_inner).push(seen);
        let reply = format!(
            "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{answer}",
            answer.len()
        );
        let _written = stream.write_all(reply.as_bytes()).await;
        let _closed = stream.shutdown().await;
    }

    /// The answer of a well-formed embeddings request: one vector per input, in order.
    fn answer(vectors: &[Vec<f64>]) -> String {
        let data: Vec<Value> = vectors
            .iter()
            .enumerate()
            .map(|(index, vector)| json!({"object": "embedding", "index": index, "embedding": vector}))
            .collect();
        json!({"object": "list", "model": "fake", "data": data, "usage": {"prompt_tokens": 1, "total_tokens": 1}})
            .to_string()
    }

    /// The number written after the `t` of an input such as `t3`, as the first component of its vector.
    fn number_of(text: &str) -> f64 {
        text.trim_start_matches('t').parse::<f64>().unwrap_or(-1.0)
    }

    /// Answers each input `tN` with the vector `[N, 0, 0]`.
    fn echo(inputs: &[String]) -> (u16, String) {
        let vectors: Vec<Vec<f64>> = inputs
            .iter()
            .map(|text| vec![number_of(text), 0.0, 0.0])
            .collect();
        (200, answer(&vectors))
    }

    fn size(value: usize) -> NonZeroUsize {
        NonZeroUsize::new(value).expect("a test size is not zero")
    }

    fn profile(max_batch: usize, dimensions: usize, char_limit: usize) -> EmbeddingProfile {
        EmbeddingProfile::new(
            "embed".to_owned(),
            "local".to_owned(),
            "fake-embed".to_owned(),
            size(dimensions),
            size(max_batch),
            size(char_limit),
        )
    }

    fn embedder(base: &str, key: Option<&str>, profile: EmbeddingProfile) -> RigEmbedder {
        RigEmbedder::new(&RigEmbedderConfig::new(
            profile,
            base.to_owned(),
            key.map(str::to_owned),
        ))
    }

    fn texts(items: &[&str]) -> Vec<String> {
        items.iter().map(|item| (*item).to_owned()).collect()
    }

    /// An address that refuses connections: a port that was bound and released.
    async fn refused_base() -> String {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("a local port can be bound");
        let address = listener.local_addr().expect("a bound listener has an address");
        drop(listener);
        format!("http://{address}/v1")
    }

    /// Checks that an error message shows neither the address, nor the key, nor the embedded text.
    fn assert_hidden(error: &EmbedderError, hidden: &[&str]) {
        for secret in hidden {
            assert!(
                !error.message().contains(secret),
                "the message `{}` leaks `{secret}`",
                error.message()
            );
        }
    }

    #[test]
    fn checked_vectors_counts_then_dimensions_and_keeps_order() {
        let vector = |length: usize, first: f64| Embedding {
            document: String::new(),
            vec: std::iter::once(first)
                .chain(std::iter::repeat_n(0.0, length.saturating_sub(1)))
                .collect(),
        };
        let kept = checked_vectors(vec![vector(3, 1.0), vector(3, 2.0)], 2, 3).expect("two vectors of three");
        assert_eq!(
            kept.iter().map(|v| v.first().copied()).collect::<Vec<_>>(),
            vec![Some(1.0), Some(2.0)]
        );
        let short = checked_vectors(vec![vector(3, 1.0)], 2, 3).expect_err("one vector for two inputs");
        assert_eq!(
            short.kind(),
            EmbedderErrorKind::WrongCount {
                received: 1,
                expected: 2
            }
        );
        let wide = checked_vectors(vec![vector(3, 1.0), vector(4, 2.0)], 2, 3).expect_err("a vector of four");
        assert_eq!(
            wide.kind(),
            EmbedderErrorKind::UnexpectedDimension {
                received: 4,
                expected: 3
            }
        );
    }

    #[test]
    fn embedder_is_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<RigEmbedder>();
    }

    #[tokio::test]
    async fn batch_is_split_in_requests_of_the_profile_size_in_order() {
        let fake = fake(echo).await;
        let rig = embedder(&fake.base, None, profile(2, 3, 100));
        let inputs = texts(&["t0", "t1", "t2", "t3", "t4"]);
        let batch = rig.embed(&inputs).await.expect("the fake service answers");
        assert_eq!(batch.len(), 5);
        let firsts: Vec<f64> = batch
            .iter()
            .map(|embedded| embedded.vector().first().copied().unwrap_or(-1.0))
            .collect();
        assert_eq!(firsts, vec![0.0, 1.0, 2.0, 3.0, 4.0]);
        assert!(batch.iter().all(|embedded| !embedded.truncated()));
        let sizes: Vec<usize> = fake
            .requests()
            .iter()
            .map(|request| request.inputs().len())
            .collect();
        assert_eq!(sizes, vec![2, 2, 1]);
        let last = fake.requests().last().map(Seen::inputs).unwrap_or_default();
        assert_eq!(last, texts(&["t4"]));
    }

    #[tokio::test]
    async fn empty_batch_sends_no_request() {
        let fake = fake(echo).await;
        let rig = embedder(&fake.base, None, profile(2, 3, 100));
        let batch = rig.embed(&[]).await.expect("an empty batch is not an error");
        assert_eq!(batch, Vec::<Embedded>::new());
        assert!(fake.requests().is_empty());
    }

    #[tokio::test]
    async fn vector_of_another_dimension_is_unexpected_dimension() {
        let fake = fake(|inputs| {
            let vectors: Vec<Vec<f64>> = inputs.iter().map(|_| vec![1.0, 2.0, 3.0, 4.0]).collect();
            (200, answer(&vectors))
        })
        .await;
        let rig = embedder(&fake.base, Some("sk-secret-key"), profile(8, 3, 100));
        let error = rig
            .embed(&texts(&["secret text"]))
            .await
            .expect_err("a vector of dimension 4 is refused for a profile of 3");
        assert_eq!(
            error.kind(),
            EmbedderErrorKind::UnexpectedDimension {
                received: 4,
                expected: 3
            }
        );
        assert_hidden(&error, &["127.0.0.1", "sk-secret-key", "secret text"]);
    }

    #[tokio::test]
    async fn wrong_vector_count_is_refused_both_ways() {
        let one_short = fake(|inputs| {
            let vectors: Vec<Vec<f64>> = inputs.iter().skip(1).map(|_| vec![0.0, 0.0, 0.0]).collect();
            (200, answer(&vectors))
        })
        .await;
        let short_error = embedder(&one_short.base, None, profile(8, 3, 100))
            .embed(&texts(&["secret-a", "secret-b", "secret-c"]))
            .await
            .expect_err("two vectors for three inputs are refused");
        assert_eq!(
            short_error.kind(),
            EmbedderErrorKind::WrongCount {
                received: 2,
                expected: 3
            }
        );
        assert_hidden(&short_error, &["127.0.0.1", "secret-"]);

        let one_more = fake(|inputs| {
            let mut vectors: Vec<Vec<f64>> = inputs.iter().map(|_| vec![0.0, 0.0, 0.0]).collect();
            vectors.push(vec![0.0, 0.0, 0.0]);
            (200, answer(&vectors))
        })
        .await;
        let long_error = embedder(&one_more.base, None, profile(8, 3, 100))
            .embed(&texts(&["secret-a", "secret-b"]))
            .await
            .expect_err("three vectors for two inputs are refused");
        assert_eq!(
            long_error.kind(),
            EmbedderErrorKind::WrongCount {
                received: 3,
                expected: 2
            }
        );
        assert_hidden(&long_error, &["127.0.0.1", "secret-"]);
    }

    #[tokio::test]
    async fn unreadable_answer_is_malformed_without_leak() {
        let not_json = fake(|_| (200, "pas du json".to_owned())).await;
        let body_error = embedder(&not_json.base, None, profile(8, 3, 100))
            .embed(&texts(&["x"]))
            .await
            .expect_err("a body that is not JSON is refused");
        assert_eq!(body_error.kind(), EmbedderErrorKind::Malformed);
        assert_hidden(&body_error, &["127.0.0.1", "pas du json"]);

        let envelope = fake(|_| (200, json!({"error": {"message": "enveloppe"}}).to_string())).await;
        let envelope_error = embedder(&envelope.base, None, profile(8, 3, 100))
            .embed(&texts(&["secret-x"]))
            .await
            .expect_err("an error envelope under a success status is refused");
        assert_eq!(envelope_error.kind(), EmbedderErrorKind::Malformed);
        assert_hidden(&envelope_error, &["127.0.0.1", "enveloppe", "secret-"]);

        let no_usage = fake(|_| {
            (
                200,
                json!({"object": "list", "data": [{"index": 0, "embedding": [1.0, 2.0, 3.0]}]}).to_string(),
            )
        })
        .await;
        let usage_error = embedder(&no_usage.base, None, profile(8, 3, 100))
            .embed(&texts(&["secret-x"]))
            .await
            .expect_err("an answer without usage is refused");
        assert_eq!(usage_error.kind(), EmbedderErrorKind::Malformed);
        assert_hidden(&usage_error, &["127.0.0.1", "secret-"]);
    }

    #[tokio::test]
    async fn unreachable_or_failing_service_is_classed_by_status() {
        let base = refused_base().await;
        let refused = embedder(&base, Some("sk-secret-key"), profile(8, 3, 100))
            .embed(&texts(&["x"]))
            .await
            .expect_err("a refused connection is an error");
        assert_eq!(refused.kind(), EmbedderErrorKind::Unavailable);
        assert_hidden(&refused, &["127.0.0.1", "sk-secret-key"]);

        for (status, expected) in [
            (429u16, EmbedderErrorKind::Unavailable),
            (500, EmbedderErrorKind::Unavailable),
            (401, EmbedderErrorKind::Rejected),
        ] {
            let fake = fake(move |_| (status, "boom".to_owned())).await;
            let status_error = embedder(&fake.base, Some("sk-secret-key"), profile(8, 3, 100))
                .embed(&texts(&["x"]))
                .await
                .expect_err("an HTTP error status is an error");
            assert_eq!(status_error.kind(), expected, "status {status}");
            assert_hidden(&status_error, &["127.0.0.1", "sk-secret-key", "boom"]);
        }
    }

    #[tokio::test]
    async fn input_of_exactly_the_limit_is_sent_unchanged_and_one_more_is_cut() {
        let fake = fake(echo).await;
        let rig = embedder(&fake.base, None, profile(8, 3, 10));
        let exact = "\u{e9}".repeat(10);
        let over = "\u{e9}".repeat(11);
        let batch = rig
            .embed(&texts(&[&exact, &over]))
            .await
            .expect("the fake service answers");
        assert!(!batch[0].truncated());
        assert!(batch[1].truncated());
        let sent = fake.requests().first().map(Seen::inputs).unwrap_or_default();
        assert_eq!(sent, texts(&[exact.as_str(), "\u{e9}".repeat(10).as_str()]));
    }

    #[tokio::test]
    async fn too_long_input_is_cut_and_signalled_before_sending() {
        let fake = fake(echo).await;
        let rig = embedder(&fake.base, None, profile(8, 3, 10));
        let long = "\u{e9}".repeat(25);
        let batch = rig
            .embed(&texts(&[&long, "h\u{e9}llo"]))
            .await
            .expect("the fake service answers");
        assert_eq!(batch.len(), 2);
        assert!(batch[0].truncated());
        assert!(!batch[1].truncated());
        let sent = fake.requests().first().map(Seen::inputs).unwrap_or_default();
        assert_eq!(sent, texts(&["\u{e9}".repeat(10).as_str(), "h\u{e9}llo"]));
    }

    #[tokio::test]
    async fn key_is_sent_as_bearer_and_absent_when_empty() {
        let fake = fake(echo).await;
        let with_key = embedder(&fake.base, Some("sk-secret-key"), profile(8, 3, 100));
        let _first = with_key
            .embed(&texts(&["t1"]))
            .await
            .expect("the fake service answers");
        let without_key = embedder(&fake.base, Some(""), profile(8, 3, 100));
        let _second = without_key
            .embed(&texts(&["t2"]))
            .await
            .expect("the fake service answers");
        let requests = fake.requests();
        let first = requests
            .first()
            .map(|request| request.head.to_ascii_lowercase())
            .unwrap_or_default();
        let second = requests
            .get(1)
            .map(|request| request.head.to_ascii_lowercase())
            .unwrap_or_default();
        assert!(first.contains("authorization: bearer sk-secret-key"));
        assert!(!second.contains("authorization"));
    }

    #[tokio::test]
    async fn debug_and_identity_hide_the_key_and_the_address() {
        install_tls_provider();
        let config = RigEmbedderConfig::new(
            profile(8, 3, 100),
            "http://10.20.30.40:8080/v1".to_owned(),
            Some("sk-secret-key".to_owned()),
        );
        let rig = RigEmbedder::new(&config);
        let shown = format!("{config:?} {rig:?}");
        assert!(!shown.contains("sk-secret-key"));
        assert!(!shown.contains("10.20.30.40"));
        assert_eq!(rig.model_id(), "fake-embed");
        assert_eq!(rig.dimensions(), 3);
    }
}

#[cfg(test)]
mod model_tests {
    use std::num::NonZeroUsize;
    use std::sync::Arc;

    use rig_core::vector_store::in_memory_store::InMemoryVectorStore;
    use rig_core::vector_store::{VectorSearchRequest, VectorStoreIndex as _};

    use super::{Embedded, Embedder, EmbedderError, EmbedderErrorKind, EmbedderModel, TestEmbedder};

    fn size(value: usize) -> NonZeroUsize {
        NonZeroUsize::new(value).expect("a test size is not zero")
    }

    fn texts(items: &[&str]) -> Vec<String> {
        items.iter().map(|item| (*item).to_owned()).collect()
    }

    /// An embedder that is always unavailable, to check that its error reaches the caller without its text.
    struct Unavailable;

    impl Embedder for Unavailable {
        fn model_id(&self) -> &'static str {
            "unavailable"
        }

        fn dimensions(&self) -> usize {
            3
        }

        fn embed(
            &self,
            _texts: &[String],
        ) -> impl Future<Output = Result<Vec<Embedded>, EmbedderError>> + Send {
            std::future::ready(Err(EmbedderError::new(
                EmbedderErrorKind::Unavailable,
                "the embedding service is unreachable".to_owned(),
            )))
        }
    }

    #[tokio::test]
    async fn index_passes_through_the_guards_and_ranks_the_close_document_first() {
        let embedder = Arc::new(TestEmbedder::new(size(64), size(100)));
        let model = EmbedderModel::new(Arc::clone(&embedder));
        assert_eq!(model.id(), Some("test-embedder"));
        assert_eq!(model.capabilities().ndims, 64);
        assert_eq!(model.capabilities().max_documents, usize::MAX);
        let documents = texts(&["alpha beta", "gamma delta", "omega zeta"]);
        let response = model
            .call(documents.clone())
            .await
            .expect("the test embedder does not fail");
        assert_eq!(response.embeddings.len(), 3);
        let pairs: Vec<(String, Vec<_>)> = response
            .embeddings
            .into_iter()
            .map(|embedding| (embedding.document.clone(), vec![embedding]))
            .collect();
        let store = InMemoryVectorStore::from_documents(pairs);
        let index = store.index(model.clone());
        let request = VectorSearchRequest::builder()
            .query("alpha".to_owned())
            .samples(2)
            .build();
        let hits = index
            .top_n::<String>(request)
            .await
            .expect("the index answers the query");
        let best = hits.first().expect("the index holds three documents");
        assert_eq!(best.2, "alpha beta");
        assert!(best.0 > 0.0);
    }

    #[tokio::test]
    async fn embedder_error_reaches_the_caller_without_the_text() {
        let model = EmbedderModel::new(Arc::new(Unavailable));
        let error = model
            .call(texts(&["secret text"]))
            .await
            .expect_err("the embedder is unavailable");
        let shown = error.to_string();
        assert!(shown.contains("unreachable"), "{shown}");
        assert!(!shown.contains("secret text"), "{shown}");
    }
}
