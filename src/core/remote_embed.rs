//! OpenAI-compatible remote embedding backend (GH #34).
//!
//! # Why ee owns this instead of reusing `frankensearch/api`
//!
//! frankensearch ships an `api` feature whose [`ApiEmbedder`] is the only type
//! there that implements [`Embedder`]. That type is deliberately fail-closed:
//! it refuses to construct without a [`PinnedRemoteAttesterV1`], and every
//! response must carry an HMAC producer attestation signed by a pinned gateway
//! key. A stock Ollama / llama.cpp / vLLM `/v1/embeddings` server signs
//! nothing, so that path can never be satisfied by the setup this backend
//! exists to support. The unattested sibling, `AssumedRemoteApi`, intentionally
//! does *not* implement [`Embedder`] and is documented as transient-exploration
//! only — explicitly not for persistent indexing, which is exactly what ee
//! needs. Both paths additionally require a caller-authored
//! `FrozenEmbeddingIdentityBundleV1` carrying artifact, tokenizer, vocabulary
//! and model-config SHA-256 fingerprints plus a golden-vector certificate;
//! none of those exist for an arbitrary remote endpoint, and filling them with
//! placeholder constants would be fabricating provenance.
//!
//! So ee implements the wire protocol itself, over the same `asupersync` HTTP
//! client frankensearch uses (no new dependency), and carries the honesty in
//! ee's own vocabulary instead of a forged attestation:
//!
//! * [`Embedder::identity`] is left at its fail-closed default, so these
//!   vectors can never masquerade as a verified embedding space.
//! * The backend, model and dimension are stamped into the index metadata, and
//!   an index built by a different backend/model/dimension is refused with a
//!   clear error rather than silently mixed.
//! * `ee model status` and `ee doctor` both name the active backend and, for
//!   `doctor`, probe the endpoint.
//!
//! # Configuration
//!
//! * `EE_EMBED_BACKEND` — `local` (default) or `remote`.
//! * `EE_EMBED_REMOTE_URL` — base URL (`http://127.0.0.1:11434/v1`) or the full
//!   endpoint (`.../v1/embeddings`). Required when the backend is `remote`.
//! * `EE_EMBED_REMOTE_MODEL` — model tag, e.g. `all-minilm`. Required when the
//!   backend is `remote`.
//! * `EE_EMBED_REMOTE_API_KEY` — optional bearer token.
//! * `EE_EMBED_REMOTE_DIMENSION` — optional. When unset, the dimension is
//!   discovered from the first response.

use std::fmt;
use std::pin::Pin;
use std::sync::OnceLock;
use std::time::Duration;

use asupersync::Cx;
use asupersync::bytes::Buf;
use asupersync::http::body::{Body, Frame};
use asupersync::http::h1::{HttpClient, HttpClientConfig, Method, RedirectPolicy};
use frankensearch::{Embedder, ModelCategory, SearchError, SearchFuture};

use crate::config::env_registry::{EnvVar, read_or_default};

/// Stable prefix for the embedder id recorded in index metadata.
///
/// The id is `remote-api:{model}`, which is what makes a backend switch or a
/// model switch a visible identity change in `meta.json` rather than a silent
/// re-use of vectors from a different embedding space.
pub const REMOTE_EMBEDDER_ID_PREFIX: &str = "remote-api";

/// Text sent when discovering the endpoint's dimension.
///
/// Deliberately short, ASCII, and content-free: it is never indexed, and a
/// server that charges per token should charge as little as possible for it.
const DIMENSION_PROBE_INPUT: &str = "ee";

/// Ceiling on one remote embedding HTTP exchange.
///
/// A remote embedder sits on the interactive search path, so this is far
/// tighter than the bundled-model download ceiling. It must still tolerate a
/// cold Ollama loading a model into memory on the first request.
pub const REMOTE_EMBED_REQUEST_TIMEOUT: Duration = Duration::from_secs(60);

/// Ceiling on the one-shot dimension/reachability probe.
///
/// `ee doctor` and embedder selection both use this. It is shorter than the
/// per-request ceiling because a probe that has not answered in this long is
/// an operational problem the user needs told about, not a slow batch.
pub const REMOTE_EMBED_PROBE_TIMEOUT: Duration = Duration::from_secs(15);

/// Hard cap on an accepted response body.
const MAX_RESPONSE_BYTES: usize = 64 * 1024 * 1024;

/// Inputs sent per HTTP request.
const MAX_BATCH_INPUTS: usize = 256;

/// Largest dimension ee will accept from a remote endpoint.
///
/// Guards against a malformed or hostile response allocating without bound;
/// no real embedding model exceeds this.
const MAX_ACCEPTED_DIMENSION: usize = 65_536;

/// Which embedding backend ee should use.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum EmbedBackendSelection {
    /// The bundled local model2vec model, with deterministic-hash fallback.
    #[default]
    Local,
    /// An OpenAI-compatible `/v1/embeddings` endpoint.
    Remote,
}

impl EmbedBackendSelection {
    /// Wire value, matching the accepted `EE_EMBED_BACKEND` spellings.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Local => "local",
            Self::Remote => "remote",
        }
    }
}

/// Parse `EE_EMBED_BACKEND`.
///
/// Unset, empty, or `local` select [`EmbedBackendSelection::Local`]. An
/// unrecognised value also selects `Local`, but returns `false` in the second
/// tuple slot so callers can surface the typo instead of silently ignoring it.
#[must_use]
pub fn parse_embed_backend(raw: Option<&str>) -> (EmbedBackendSelection, bool) {
    let Some(value) = raw.map(str::trim).filter(|value| !value.is_empty()) else {
        return (EmbedBackendSelection::Local, true);
    };
    if value.eq_ignore_ascii_case("local") {
        return (EmbedBackendSelection::Local, true);
    }
    if value.eq_ignore_ascii_case("remote") {
        return (EmbedBackendSelection::Remote, true);
    }
    (EmbedBackendSelection::Local, false)
}

/// Read `EE_EMBED_BACKEND` from the environment.
#[must_use]
pub fn configured_embed_backend() -> EmbedBackendSelection {
    let raw = read_or_default(EnvVar::EmbedBackend);
    let (selection, recognised) = parse_embed_backend(raw.as_deref());
    if !recognised {
        tracing::warn!(
            target: "ee::index::embedder",
            "invalid EE_EMBED_BACKEND value; falling back to the local backend"
        );
    }
    selection
}

/// Why a remote backend could not be configured from the environment.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RemoteEmbedConfigError {
    /// `EE_EMBED_REMOTE_URL` is unset or blank.
    MissingUrl,
    /// `EE_EMBED_REMOTE_MODEL` is unset or blank.
    MissingModel,
    /// The URL has no scheme, or a scheme other than `http`/`https`.
    UnsupportedScheme,
    /// The URL has no host.
    MissingHost,
    /// `EE_EMBED_REMOTE_DIMENSION` is not a positive integer within bounds.
    InvalidDimension,
}

impl RemoteEmbedConfigError {
    /// Stable, redaction-safe code for structured output.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::MissingUrl => "remote_embed_missing_url",
            Self::MissingModel => "remote_embed_missing_model",
            Self::UnsupportedScheme => "remote_embed_unsupported_scheme",
            Self::MissingHost => "remote_embed_missing_host",
            Self::InvalidDimension => "remote_embed_invalid_dimension",
        }
    }

    /// Operator-facing repair guidance.
    #[must_use]
    pub const fn repair(self) -> &'static str {
        match self {
            Self::MissingUrl => {
                "Set EE_EMBED_REMOTE_URL to your endpoint, e.g. http://127.0.0.1:11434/v1"
            }
            Self::MissingModel => {
                "Set EE_EMBED_REMOTE_MODEL to the model tag your endpoint serves, e.g. all-minilm"
            }
            Self::UnsupportedScheme => "EE_EMBED_REMOTE_URL must start with http:// or https://",
            Self::MissingHost => "EE_EMBED_REMOTE_URL must include a host, e.g. 127.0.0.1:11434",
            Self::InvalidDimension => {
                "EE_EMBED_REMOTE_DIMENSION must be a positive integer, or unset to discover it from the first response"
            }
        }
    }
}

impl fmt::Display for RemoteEmbedConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::MissingUrl => "EE_EMBED_REMOTE_URL is not set",
            Self::MissingModel => "EE_EMBED_REMOTE_MODEL is not set",
            Self::UnsupportedScheme => "EE_EMBED_REMOTE_URL must use http:// or https://",
            Self::MissingHost => "EE_EMBED_REMOTE_URL has no host",
            Self::InvalidDimension => "EE_EMBED_REMOTE_DIMENSION is not a positive integer",
        };
        formatter.write_str(message)
    }
}

/// Fully resolved remote endpoint settings.
///
/// `api_key` is never rendered by [`fmt::Debug`]; see the manual impl below.
#[derive(Clone, Eq, PartialEq)]
pub struct RemoteEmbedSettings {
    /// Absolute POST target, always ending in `/embeddings`.
    pub endpoint: String,
    /// Model tag sent in the request body.
    pub model: String,
    /// Optional bearer token.
    pub api_key: Option<String>,
    /// Dimension when explicitly configured; `None` means "discover it".
    pub dimension: Option<usize>,
}

impl fmt::Debug for RemoteEmbedSettings {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RemoteEmbedSettings")
            .field("endpoint", &self.redacted_endpoint())
            .field("model", &self.model)
            .field("api_key", &self.api_key.as_ref().map(|_| "<redacted>"))
            .field("dimension", &self.dimension)
            .finish()
    }
}

impl RemoteEmbedSettings {
    /// Build settings from explicit values, applying the same normalization the
    /// environment path uses.
    ///
    /// # Errors
    ///
    /// Returns [`RemoteEmbedConfigError`] when the URL, model, or dimension is
    /// missing or malformed.
    pub fn new(
        url: Option<&str>,
        model: Option<&str>,
        api_key: Option<&str>,
        dimension: Option<&str>,
    ) -> Result<Self, RemoteEmbedConfigError> {
        let url = url
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or(RemoteEmbedConfigError::MissingUrl)?;
        let model = model
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or(RemoteEmbedConfigError::MissingModel)?;
        let dimension = match dimension.map(str::trim).filter(|value| !value.is_empty()) {
            None => None,
            Some(raw) => Some(parse_remote_dimension(raw)?),
        };
        Ok(Self {
            endpoint: normalize_embeddings_endpoint(url)?,
            model: model.to_owned(),
            api_key: api_key
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_owned),
            dimension,
        })
    }

    /// Read settings from the environment registry.
    ///
    /// # Errors
    ///
    /// Returns [`RemoteEmbedConfigError`] when the URL, model, or dimension is
    /// missing or malformed.
    pub fn from_env() -> Result<Self, RemoteEmbedConfigError> {
        let url = read_or_default(EnvVar::EmbedRemoteUrl);
        let model = read_or_default(EnvVar::EmbedRemoteModel);
        let api_key = read_or_default(EnvVar::EmbedRemoteApiKey);
        let dimension = read_or_default(EnvVar::EmbedRemoteDimension);
        Self::new(
            url.as_deref(),
            model.as_deref(),
            api_key.as_deref(),
            dimension.as_deref(),
        )
    }

    /// Stable embedder id recorded in the index: `remote-api:{model}`.
    ///
    /// The dimension is deliberately *not* part of the id: the index records it
    /// as its own field, so a model that changed dimension underneath us
    /// surfaces as a dimension mismatch (the precise diagnosis) rather than as
    /// an unrelated-model mismatch.
    #[must_use]
    pub fn embedder_id(&self) -> String {
        format!("{REMOTE_EMBEDDER_ID_PREFIX}:{}", self.model)
    }

    /// Endpoint with any userinfo stripped, safe to print in diagnostics.
    #[must_use]
    pub fn redacted_endpoint(&self) -> String {
        redact_userinfo(&self.endpoint)
    }
}

fn parse_remote_dimension(raw: &str) -> Result<usize, RemoteEmbedConfigError> {
    let parsed: usize = raw
        .parse()
        .map_err(|_| RemoteEmbedConfigError::InvalidDimension)?;
    if parsed == 0 || parsed > MAX_ACCEPTED_DIMENSION {
        return Err(RemoteEmbedConfigError::InvalidDimension);
    }
    Ok(parsed)
}

/// Turn a base URL or a full endpoint into the absolute `/embeddings` target.
///
/// Accepts `…/v1`, `…/v1/`, and `…/v1/embeddings` so the operator can paste
/// whichever form their server's documentation shows.
fn normalize_embeddings_endpoint(url: &str) -> Result<String, RemoteEmbedConfigError> {
    let rest = if let Some(rest) = url.strip_prefix("http://") {
        rest
    } else if let Some(rest) = url.strip_prefix("https://") {
        rest
    } else {
        return Err(RemoteEmbedConfigError::UnsupportedScheme);
    };
    // Everything before the first '/', '?' or '#' is authority.
    let authority_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let authority = &rest[..authority_end];
    // Reject a bare scheme, and a URL whose host is empty after any userinfo.
    let host = authority.rsplit('@').next().unwrap_or(authority);
    if host.is_empty() {
        return Err(RemoteEmbedConfigError::MissingHost);
    }
    // Query strings and fragments are meaningless for this POST target.
    let path_end = rest.find(['?', '#']).unwrap_or(rest.len());
    let trimmed = url[..url.len() - (rest.len() - path_end)].trim_end_matches('/');
    if trimmed.ends_with("/embeddings") {
        return Ok(trimmed.to_owned());
    }
    Ok(format!("{trimmed}/embeddings"))
}

/// Strip `user:password@` from a URL's authority for safe display.
fn redact_userinfo(url: &str) -> String {
    let Some((scheme, rest)) = url.split_once("://") else {
        return url.to_owned();
    };
    let authority_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let (authority, tail) = rest.split_at(authority_end);
    match authority.rsplit_once('@') {
        Some((_userinfo, host)) => format!("{scheme}://<redacted>@{host}{tail}"),
        None => url.to_owned(),
    }
}

/// What went wrong talking to the remote endpoint.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RemoteEmbedError {
    /// The endpoint could not be reached, or the exchange timed out.
    Unreachable { detail: String },
    /// The endpoint answered with a non-2xx status.
    Status { status: u16 },
    /// The response was not the expected OpenAI embeddings shape.
    MalformedResponse { detail: String },
    /// The response carried a different dimension than the index expects.
    DimensionMismatch { expected: usize, actual: usize },
}

impl RemoteEmbedError {
    /// Stable, redaction-safe code for structured output.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Unreachable { .. } => "remote_embed_unreachable",
            Self::Status { .. } => "remote_embed_http_status",
            Self::MalformedResponse { .. } => "remote_embed_malformed_response",
            Self::DimensionMismatch { .. } => "remote_embed_dimension_mismatch",
        }
    }
}

impl fmt::Display for RemoteEmbedError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unreachable { detail } => {
                write!(formatter, "remote embedding endpoint unreachable: {detail}")
            }
            Self::Status { status } => {
                write!(
                    formatter,
                    "remote embedding endpoint returned HTTP {status}"
                )
            }
            Self::MalformedResponse { detail } => {
                write!(formatter, "remote embedding response malformed: {detail}")
            }
            Self::DimensionMismatch { expected, actual } => write!(
                formatter,
                "remote embedding endpoint returned {actual}d vectors but this index was built at {expected}d"
            ),
        }
    }
}

impl RemoteEmbedError {
    fn into_search_error(self, model: &str) -> SearchError {
        SearchError::EmbeddingFailed {
            model: model.to_owned(),
            source: self.to_string().into(),
        }
    }
}

/// An OpenAI-compatible remote embedder.
///
/// [`Embedder::identity`] is intentionally left at its fail-closed default: the
/// vectors are trusted because the operator pointed ee at this endpoint, not
/// because the endpoint proved anything.
pub struct RemoteApiEmbedder {
    settings: RemoteEmbedSettings,
    client: HttpClient,
    id: String,
    dimension: usize,
    request_timeout: Duration,
}

impl fmt::Debug for RemoteApiEmbedder {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RemoteApiEmbedder")
            .field("id", &self.id)
            .field("endpoint", &self.settings.redacted_endpoint())
            .field("dimension", &self.dimension)
            .finish()
    }
}

impl RemoteApiEmbedder {
    /// Build an embedder for an already-known dimension.
    #[must_use]
    pub fn with_dimension(settings: RemoteEmbedSettings, dimension: usize) -> Self {
        let id = settings.embedder_id();
        Self {
            client: build_client(REMOTE_EMBED_REQUEST_TIMEOUT),
            id,
            dimension,
            settings,
            request_timeout: REMOTE_EMBED_REQUEST_TIMEOUT,
        }
    }

    /// Tighten the per-request budget.
    ///
    /// Used by tests to exercise the timeout path in milliseconds rather than
    /// minutes, and available to callers that want a stricter interactive
    /// bound than [`REMOTE_EMBED_REQUEST_TIMEOUT`].
    #[must_use]
    pub fn with_request_timeout(mut self, request_timeout: Duration) -> Self {
        self.client = build_client(request_timeout);
        self.request_timeout = request_timeout;
        self
    }

    /// Build an embedder, discovering the dimension from the endpoint when it
    /// was not configured.
    ///
    /// # Errors
    ///
    /// Returns [`RemoteEmbedError`] when the endpoint is unreachable, answers
    /// with a non-2xx status, or returns a malformed body.
    pub async fn resolve(cx: &Cx, settings: RemoteEmbedSettings) -> Result<Self, RemoteEmbedError> {
        if let Some(dimension) = settings.dimension {
            return Ok(Self::with_dimension(settings, dimension));
        }
        let dimension = probe_dimension(cx, &settings, REMOTE_EMBED_PROBE_TIMEOUT).await?;
        Ok(Self::with_dimension(settings, dimension))
    }

    /// Endpoint settings this embedder was built from.
    #[must_use]
    pub const fn settings(&self) -> &RemoteEmbedSettings {
        &self.settings
    }

    async fn embed_chunk(
        &self,
        cx: &Cx,
        texts: &[&str],
    ) -> Result<Vec<Vec<f32>>, RemoteEmbedError> {
        let body = serialize_request(&self.settings.model, texts);
        let payload =
            post_json(cx, &self.client, &self.settings, body, self.request_timeout).await?;
        let vectors = parse_embeddings_response(&payload, texts.len())?;
        for vector in &vectors {
            if vector.len() != self.dimension {
                return Err(RemoteEmbedError::DimensionMismatch {
                    expected: self.dimension,
                    actual: vector.len(),
                });
            }
        }
        Ok(vectors)
    }
}

impl Embedder for RemoteApiEmbedder {
    fn embed<'a>(&'a self, cx: &'a Cx, text: &'a str) -> SearchFuture<'a, Vec<f32>> {
        Box::pin(async move {
            let texts = [text];
            let mut vectors = self
                .embed_chunk(cx, &texts)
                .await
                .map_err(|error| error.into_search_error(&self.id))?;
            vectors.pop().ok_or_else(|| {
                RemoteEmbedError::MalformedResponse {
                    detail: "response carried no embedding".to_owned(),
                }
                .into_search_error(&self.id)
            })
        })
    }

    fn embed_batch<'a>(
        &'a self,
        cx: &'a Cx,
        texts: &'a [&'a str],
    ) -> SearchFuture<'a, Vec<Vec<f32>>> {
        Box::pin(async move {
            let mut out = Vec::with_capacity(texts.len());
            for chunk in texts.chunks(MAX_BATCH_INPUTS) {
                let vectors = self
                    .embed_chunk(cx, chunk)
                    .await
                    .map_err(|error| error.into_search_error(&self.id))?;
                out.extend(vectors);
            }
            Ok(out)
        })
    }

    fn dimension(&self) -> usize {
        self.dimension
    }

    fn id(&self) -> &str {
        &self.id
    }

    fn model_name(&self) -> &str {
        &self.settings.model
    }

    fn is_semantic(&self) -> bool {
        true
    }

    fn category(&self) -> ModelCategory {
        ModelCategory::ApiEmbedder
    }
}

fn build_client(request_timeout: Duration) -> HttpClient {
    let mut config = HttpClientConfig::default();
    config.redirect_policy = RedirectPolicy::Limited(5);
    config.user_agent = Some(format!("ee/{} (remote-embed)", env!("CARGO_PKG_VERSION")));
    config.max_body_size = Some(MAX_RESPONSE_BYTES);
    config.request_timeout = Some(request_timeout);
    HttpClient::with_config(config)
}

/// Serialize an OpenAI `/v1/embeddings` request body.
///
/// `encoding_format: "float"` is explicit so a server that would otherwise
/// default to base64 returns numbers we can parse.
fn serialize_request(model: &str, texts: &[&str]) -> Vec<u8> {
    let payload = serde_json::json!({
        "model": model,
        "input": texts,
        "encoding_format": "float",
    });
    serde_json::to_vec(&payload).unwrap_or_else(|_| b"{}".to_vec())
}

fn request_headers(settings: &RemoteEmbedSettings) -> Vec<(String, String)> {
    let mut headers = vec![
        ("content-type".to_owned(), "application/json".to_owned()),
        ("accept".to_owned(), "application/json".to_owned()),
    ];
    if let Some(api_key) = settings.api_key.as_deref() {
        headers.push(("authorization".to_owned(), format!("Bearer {api_key}")));
    }
    headers
}

async fn post_json(
    cx: &Cx,
    client: &HttpClient,
    settings: &RemoteEmbedSettings,
    body: Vec<u8>,
    request_timeout: Duration,
) -> Result<Vec<u8>, RemoteEmbedError> {
    let exchange = async {
        let mut response = client
            .request_streaming(
                cx,
                Method::Post,
                &settings.endpoint,
                request_headers(settings),
                body,
            )
            .await
            .map_err(|error| RemoteEmbedError::Unreachable {
                detail: bounded_detail(&error.to_string()),
            })?;

        let status = response.head.status;
        let mut payload = Vec::new();
        while let Some(frame) =
            std::future::poll_fn(|task_cx| Pin::new(&mut response.body).poll_frame(task_cx)).await
        {
            match frame {
                Ok(Frame::Data(mut chunk)) => {
                    while chunk.has_remaining() {
                        let bytes = chunk.chunk();
                        if bytes.is_empty() {
                            break;
                        }
                        if payload.len().saturating_add(bytes.len()) > MAX_RESPONSE_BYTES {
                            return Err(RemoteEmbedError::MalformedResponse {
                                detail: "response exceeded the accepted byte budget".to_owned(),
                            });
                        }
                        payload.extend_from_slice(bytes);
                        chunk.advance(bytes.len());
                    }
                }
                Ok(Frame::Trailers(_)) => {}
                Err(error) => {
                    return Err(RemoteEmbedError::Unreachable {
                        detail: bounded_detail(&error.to_string()),
                    });
                }
            }
        }

        if !(200..300).contains(&status) {
            return Err(RemoteEmbedError::Status { status });
        }
        Ok(payload)
    };
    // request_streaming's timeout ends after the headers. Keep one deadline
    // around the complete exchange so a stalled or trickling body cannot
    // outlive the configured request budget.
    match asupersync::time::TimeoutFuture::after(cx.now(), request_timeout, exchange).await {
        Ok(result) => result,
        Err(_elapsed) => Err(RemoteEmbedError::Unreachable {
            detail: format!(
                "no complete response within {}ms",
                request_timeout.as_millis()
            ),
        }),
    }
}

/// Bound and sanitise a transport error string before it reaches a log or a
/// doctor message.
fn bounded_detail(detail: &str) -> String {
    let cleaned: String = detail
        .chars()
        .map(|character| {
            if character.is_control() {
                ' '
            } else {
                character
            }
        })
        .take(200)
        .collect();
    let trimmed = cleaned.trim();
    if trimmed.is_empty() {
        "transport error".to_owned()
    } else {
        trimmed.to_owned()
    }
}

/// Parse an OpenAI `/v1/embeddings` response into per-input vectors.
///
/// The `data` array is ordered by its `index` field, because the OpenAI
/// contract permits a server to return entries out of order and Ollama has
/// historically done so under concurrency.
fn parse_embeddings_response(
    payload: &[u8],
    expected_count: usize,
) -> Result<Vec<Vec<f32>>, RemoteEmbedError> {
    let parsed: serde_json::Value =
        serde_json::from_slice(payload).map_err(|error| RemoteEmbedError::MalformedResponse {
            detail: bounded_detail(&error.to_string()),
        })?;
    let data = parsed
        .get("data")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| RemoteEmbedError::MalformedResponse {
            detail: "response has no `data` array".to_owned(),
        })?;
    if data.len() != expected_count {
        return Err(RemoteEmbedError::MalformedResponse {
            detail: format!(
                "response returned {} embeddings for {expected_count} inputs",
                data.len()
            ),
        });
    }

    let mut indexed = Vec::with_capacity(data.len());
    for (position, entry) in data.iter().enumerate() {
        let values = entry
            .get("embedding")
            .and_then(serde_json::Value::as_array)
            .ok_or_else(|| RemoteEmbedError::MalformedResponse {
                detail: "response entry has no `embedding` array".to_owned(),
            })?;
        if values.is_empty() {
            return Err(RemoteEmbedError::MalformedResponse {
                detail: "response entry has an empty `embedding`".to_owned(),
            });
        }
        if values.len() > MAX_ACCEPTED_DIMENSION {
            return Err(RemoteEmbedError::MalformedResponse {
                detail: format!(
                    "response dimension {} exceeds the accepted ceiling",
                    values.len()
                ),
            });
        }
        let mut vector = Vec::with_capacity(values.len());
        for value in values {
            let number = value
                .as_f64()
                .ok_or_else(|| RemoteEmbedError::MalformedResponse {
                    detail: "response embedding contains a non-numeric value".to_owned(),
                })?;
            #[allow(clippy::cast_possible_truncation)]
            let component = number as f32;
            if !component.is_finite() {
                return Err(RemoteEmbedError::MalformedResponse {
                    detail: "response embedding contains a non-finite value".to_owned(),
                });
            }
            vector.push(component);
        }
        // A missing `index` falls back to array position, which is what a
        // server that always answers in order effectively means.
        let index = entry
            .get("index")
            .and_then(serde_json::Value::as_u64)
            .and_then(|index| usize::try_from(index).ok())
            .unwrap_or(position);
        indexed.push((index, vector));
    }

    indexed.sort_by_key(|(index, _)| *index);
    // Reject duplicate or out-of-range indices rather than silently reordering
    // a response we do not actually understand.
    for (position, (index, _)) in indexed.iter().enumerate() {
        if *index != position {
            return Err(RemoteEmbedError::MalformedResponse {
                detail: "response embeddings do not cover each input exactly once".to_owned(),
            });
        }
    }

    let vectors: Vec<Vec<f32>> = indexed.into_iter().map(|(_, vector)| vector).collect();
    let first_len = vectors[0].len();
    if vectors.iter().any(|vector| vector.len() != first_len) {
        return Err(RemoteEmbedError::MalformedResponse {
            detail: "response embeddings have inconsistent dimensions".to_owned(),
        });
    }
    Ok(vectors)
}

/// One-shot probe: reach the endpoint and report the dimension it produces.
///
/// # Errors
///
/// Returns [`RemoteEmbedError`] when the endpoint is unreachable, answers with
/// a non-2xx status, or returns a malformed body.
pub async fn probe_dimension(
    cx: &Cx,
    settings: &RemoteEmbedSettings,
    request_timeout: Duration,
) -> Result<usize, RemoteEmbedError> {
    let client = build_client(request_timeout);
    let body = serialize_request(&settings.model, &[DIMENSION_PROBE_INPUT]);
    let payload = post_json(cx, &client, settings, body, request_timeout).await?;
    let vectors = parse_embeddings_response(&payload, 1)?;
    Ok(vectors[0].len())
}

/// Run a bounded probe from synchronous code.
///
/// The probe runs on its own thread with its own current-thread runtime, so it
/// is safe to call from inside an existing async context (embedder selection
/// can be reached from both) without nesting `block_on`.
///
/// # Errors
///
/// Returns [`RemoteEmbedError`] when the endpoint is unreachable, answers with
/// a non-2xx status, or returns a malformed body.
pub fn probe_dimension_blocking(settings: &RemoteEmbedSettings) -> Result<usize, RemoteEmbedError> {
    probe_dimension_blocking_with_timeout(settings, REMOTE_EMBED_PROBE_TIMEOUT)
}

/// [`probe_dimension_blocking`] with an explicit budget.
///
/// # Errors
///
/// Returns [`RemoteEmbedError`] when the endpoint is unreachable, answers with
/// a non-2xx status, or returns a malformed body.
pub fn probe_dimension_blocking_with_timeout(
    settings: &RemoteEmbedSettings,
    request_timeout: Duration,
) -> Result<usize, RemoteEmbedError> {
    let settings = settings.clone();
    // `run_cli_with_cx` builds its own current-thread runtime, so the probe
    // gets its own thread: embedder selection is reachable from inside an
    // existing runtime, and nesting `block_on` there would deadlock.
    let joined = std::thread::Builder::new()
        .name("ee-remote-embed-probe".to_owned())
        .spawn(move || {
            crate::core::run_cli_with_cx(request_timeout, |cx| async move {
                probe_dimension(&cx, &settings, request_timeout).await
            })
        })
        .map_err(|error| RemoteEmbedError::Unreachable {
            detail: bounded_detail(&error.to_string()),
        })?
        .join();
    match joined {
        Ok(Ok(result)) => result,
        Ok(Err(error)) => Err(RemoteEmbedError::Unreachable {
            detail: bounded_detail(&error.to_string()),
        }),
        Err(_panic) => Err(RemoteEmbedError::Unreachable {
            detail: "probe thread panicked".to_owned(),
        }),
    }
}

/// Process-wide cache of the resolved remote embedder dimension.
///
/// Discovery costs one HTTP round trip; every ee invocation that needs the
/// dimension in the same process reuses it.
static RESOLVED_REMOTE_DIMENSION: OnceLock<Option<usize>> = OnceLock::new();

/// Resolve the remote embedder for the current environment.
///
/// Returns `Ok(None)` when the configured backend is not `remote`.
///
/// # Errors
///
/// Returns `Err` with either a configuration error or an endpoint error, so
/// callers can report the precise reason rather than falling back silently.
pub fn resolve_configured_remote_embedder()
-> Result<Option<RemoteApiEmbedder>, RemoteEmbedResolution> {
    if configured_embed_backend() != EmbedBackendSelection::Remote {
        return Ok(None);
    }
    let settings = RemoteEmbedSettings::from_env().map_err(RemoteEmbedResolution::Config)?;
    if let Some(dimension) = settings.dimension {
        return Ok(Some(RemoteApiEmbedder::with_dimension(settings, dimension)));
    }
    let cached =
        *RESOLVED_REMOTE_DIMENSION.get_or_init(|| probe_dimension_blocking(&settings).ok());
    let dimension = cached.ok_or_else(|| {
        RemoteEmbedResolution::Endpoint(RemoteEmbedError::Unreachable {
            detail: "dimension probe failed; set EE_EMBED_REMOTE_DIMENSION to skip discovery"
                .to_owned(),
        })
    })?;
    Ok(Some(RemoteApiEmbedder::with_dimension(settings, dimension)))
}

/// Why the remote backend could not be brought up.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RemoteEmbedResolution {
    /// The environment configuration is incomplete or malformed.
    Config(RemoteEmbedConfigError),
    /// The endpoint itself could not serve a usable response.
    Endpoint(RemoteEmbedError),
}

impl RemoteEmbedResolution {
    /// Stable, redaction-safe code for structured output.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Config(error) => error.code(),
            Self::Endpoint(error) => error.code(),
        }
    }
}

impl fmt::Display for RemoteEmbedResolution {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Config(error) => write!(formatter, "{error}"),
            Self::Endpoint(error) => write!(formatter, "{error}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backend_parsing_accepts_the_documented_spellings() {
        assert_eq!(
            parse_embed_backend(None),
            (EmbedBackendSelection::Local, true)
        );
        assert_eq!(
            parse_embed_backend(Some("   ")),
            (EmbedBackendSelection::Local, true)
        );
        assert_eq!(
            parse_embed_backend(Some("local")),
            (EmbedBackendSelection::Local, true)
        );
        assert_eq!(
            parse_embed_backend(Some(" REMOTE ")),
            (EmbedBackendSelection::Remote, true)
        );
    }

    #[test]
    fn backend_parsing_reports_an_unrecognised_value_instead_of_hiding_it() {
        assert_eq!(
            parse_embed_backend(Some("remot")),
            (EmbedBackendSelection::Local, false)
        );
    }

    #[test]
    fn endpoint_normalization_accepts_base_and_full_urls() {
        for input in [
            "http://127.0.0.1:11434/v1",
            "http://127.0.0.1:11434/v1/",
            "http://127.0.0.1:11434/v1/embeddings",
            "http://127.0.0.1:11434/v1/embeddings/",
        ] {
            assert_eq!(
                normalize_embeddings_endpoint(input).expect("valid url"),
                "http://127.0.0.1:11434/v1/embeddings",
                "input {input}"
            );
        }
    }

    #[test]
    fn endpoint_normalization_drops_query_and_fragment() {
        assert_eq!(
            normalize_embeddings_endpoint("http://host:1/v1?debug=1").expect("valid url"),
            "http://host:1/v1/embeddings"
        );
        assert_eq!(
            normalize_embeddings_endpoint("https://host/v1#frag").expect("valid url"),
            "https://host/v1/embeddings"
        );
    }

    #[test]
    fn endpoint_normalization_rejects_bad_urls() {
        assert_eq!(
            normalize_embeddings_endpoint("ftp://host/v1"),
            Err(RemoteEmbedConfigError::UnsupportedScheme)
        );
        assert_eq!(
            normalize_embeddings_endpoint("127.0.0.1:11434/v1"),
            Err(RemoteEmbedConfigError::UnsupportedScheme)
        );
        assert_eq!(
            normalize_embeddings_endpoint("http:///v1"),
            Err(RemoteEmbedConfigError::MissingHost)
        );
    }

    #[test]
    fn settings_require_url_and_model() {
        assert_eq!(
            RemoteEmbedSettings::new(None, Some("all-minilm"), None, None),
            Err(RemoteEmbedConfigError::MissingUrl)
        );
        assert_eq!(
            RemoteEmbedSettings::new(Some("http://h/v1"), Some("  "), None, None),
            Err(RemoteEmbedConfigError::MissingModel)
        );
    }

    #[test]
    fn settings_reject_a_nonsense_dimension() {
        for raw in ["0", "-1", "abc", "99999999"] {
            assert_eq!(
                RemoteEmbedSettings::new(Some("http://h/v1"), Some("m"), None, Some(raw)),
                Err(RemoteEmbedConfigError::InvalidDimension),
                "raw {raw}"
            );
        }
    }

    #[test]
    fn settings_never_debug_print_the_api_key() {
        let settings = RemoteEmbedSettings::new(
            Some("http://user:endpoint-secret@127.0.0.1:11434/v1"),
            Some("all-minilm"),
            Some("sk-super-secret"),
            Some("384"),
        )
        .expect("valid settings");
        let rendered = format!("{settings:?}");
        assert!(
            !rendered.contains("sk-super-secret"),
            "api key leaked into Debug output: {rendered}"
        );
        assert!(rendered.contains("<redacted>"), "{rendered}");
        assert!(
            !rendered.contains("endpoint-secret"),
            "endpoint credentials leaked into Debug output: {rendered}"
        );
    }

    #[test]
    fn embedder_id_names_the_backend_and_model_but_not_the_dimension() {
        let settings = RemoteEmbedSettings::new(
            Some("http://127.0.0.1:11434/v1"),
            Some("all-minilm"),
            None,
            Some("384"),
        )
        .expect("valid settings");
        assert_eq!(settings.embedder_id(), "remote-api:all-minilm");
    }

    #[test]
    fn userinfo_is_redacted_from_a_displayed_endpoint() {
        let settings = RemoteEmbedSettings::new(
            Some("https://user:pass@example.test/v1"),
            Some("m"),
            None,
            None,
        )
        .expect("valid settings");
        let rendered = settings.redacted_endpoint();
        assert!(!rendered.contains("pass"), "{rendered}");
        assert_eq!(rendered, "https://<redacted>@example.test/v1/embeddings");
    }

    #[test]
    fn authorization_header_is_present_only_with_a_key() {
        let without = RemoteEmbedSettings::new(Some("http://h/v1"), Some("m"), None, None)
            .expect("valid settings");
        assert!(
            !request_headers(&without)
                .iter()
                .any(|(name, _)| name == "authorization")
        );
        let with = RemoteEmbedSettings::new(Some("http://h/v1"), Some("m"), Some("tok"), None)
            .expect("valid settings");
        let header = request_headers(&with)
            .into_iter()
            .find(|(name, _)| name == "authorization")
            .expect("authorization header");
        assert_eq!(header.1, "Bearer tok");
    }

    #[test]
    fn request_body_pins_float_encoding() {
        let body = serialize_request("all-minilm", &["a", "b"]);
        let parsed: serde_json::Value = serde_json::from_slice(&body).expect("valid json");
        assert_eq!(parsed["model"], "all-minilm");
        assert_eq!(parsed["encoding_format"], "float");
        assert_eq!(parsed["input"][0], "a");
        assert_eq!(parsed["input"][1], "b");
    }

    #[test]
    fn response_parsing_orders_by_index() {
        let payload = br#"{"data":[
            {"index":1,"embedding":[0.0,1.0]},
            {"index":0,"embedding":[1.0,0.0]}
        ]}"#;
        let vectors = parse_embeddings_response(payload, 2).expect("parsed");
        assert_eq!(vectors[0], vec![1.0, 0.0]);
        assert_eq!(vectors[1], vec![0.0, 1.0]);
    }

    #[test]
    fn response_parsing_falls_back_to_array_position_without_index() {
        let payload = br#"{"data":[{"embedding":[1.0]},{"embedding":[2.0]}]}"#;
        let vectors = parse_embeddings_response(payload, 2).expect("parsed");
        assert_eq!(vectors, vec![vec![1.0], vec![2.0]]);
    }

    #[test]
    fn response_parsing_rejects_a_count_mismatch() {
        let payload = br#"{"data":[{"index":0,"embedding":[1.0]}]}"#;
        let error = parse_embeddings_response(payload, 2).expect_err("count mismatch");
        assert_eq!(error.code(), "remote_embed_malformed_response");
    }

    #[test]
    fn response_parsing_rejects_duplicate_indices() {
        let payload = br#"{"data":[
            {"index":0,"embedding":[1.0]},
            {"index":0,"embedding":[2.0]}
        ]}"#;
        let error = parse_embeddings_response(payload, 2).expect_err("duplicate index");
        assert_eq!(error.code(), "remote_embed_malformed_response");
    }

    #[test]
    fn response_parsing_rejects_ragged_and_nonfinite_vectors() {
        let ragged = br#"{"data":[
            {"index":0,"embedding":[1.0,2.0]},
            {"index":1,"embedding":[1.0]}
        ]}"#;
        assert!(parse_embeddings_response(ragged, 2).is_err());

        let nonfinite = br#"{"data":[{"index":0,"embedding":[1e400]}]}"#;
        assert!(parse_embeddings_response(nonfinite, 1).is_err());
    }

    #[test]
    fn response_parsing_rejects_a_missing_data_array() {
        let payload = br#"{"error":{"message":"model not found"}}"#;
        let error = parse_embeddings_response(payload, 1).expect_err("no data array");
        assert_eq!(error.code(), "remote_embed_malformed_response");
    }

    #[test]
    fn bounded_detail_strips_control_characters_and_caps_length() {
        let detail = bounded_detail(&format!("a\nb{}", "x".repeat(500)));
        assert!(!detail.contains('\n'));
        assert!(detail.chars().count() <= 200);
        assert_eq!(bounded_detail("   "), "transport error");
    }
}
