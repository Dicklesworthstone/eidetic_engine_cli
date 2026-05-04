use std::collections::HashMap;

use crate::models::{
    CapabilityStatus, INDEX_MANIFEST_SCHEMA_V1, SEARCH_DOCUMENT_SCHEMA_V1, SEARCH_MODULE_SCHEMA_V1,
};

pub use frankensearch::core::types::IndexableDocument;
pub use frankensearch::{
    Embedder, EmbedderStack, HashEmbedder, IndexBuilder, ScoreSource, ScoredResult, TwoTierConfig,
    TwoTierIndex, TwoTierSearcher,
};

pub const SUBSYSTEM: &str = "search";
pub const CANONICAL_DOCUMENT_SCHEMA: &str = SEARCH_DOCUMENT_SCHEMA_V1;

/// Source type for canonical search documents.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum DocumentSource {
    Memory,
    Session,
    Rule,
    Import,
    Artifact,
}

impl DocumentSource {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Memory => "memory",
            Self::Session => "session",
            Self::Rule => "rule",
            Self::Import => "import",
            Self::Artifact => "artifact",
        }
    }
}

/// Canonical search document for ee.
///
/// This is the unified document format that all indexable content
/// (memories, sessions, rules, imports) must produce before indexing.
/// It converts directly to frankensearch's [`IndexableDocument`].
#[derive(Clone, Debug)]
pub struct CanonicalSearchDocument {
    id: String,
    content: String,
    title: Option<String>,
    source: DocumentSource,
    workspace: Option<String>,
    level: Option<String>,
    kind: Option<String>,
    created_at: Option<String>,
    tags: Vec<String>,
    metadata: HashMap<String, String>,
}

impl CanonicalSearchDocument {
    /// Create a new canonical document with required fields.
    #[must_use]
    pub fn new(id: impl Into<String>, content: impl Into<String>, source: DocumentSource) -> Self {
        Self {
            id: id.into(),
            content: content.into(),
            title: None,
            source,
            workspace: None,
            level: None,
            kind: None,
            created_at: None,
            tags: Vec::new(),
            metadata: HashMap::new(),
        }
    }

    /// Set the document title (receives BM25 boost in lexical search).
    #[must_use]
    pub fn with_title(mut self, title: impl Into<String>) -> Self {
        self.title = Some(title.into());
        self
    }

    /// Set the workspace path.
    #[must_use]
    pub fn with_workspace(mut self, workspace: impl Into<String>) -> Self {
        self.workspace = Some(workspace.into());
        self
    }

    /// Set the memory level (working, episodic, semantic, procedural).
    #[must_use]
    pub fn with_level(mut self, level: impl Into<String>) -> Self {
        self.level = Some(level.into());
        self
    }

    /// Set the memory kind (rule, fact, decision, etc.).
    #[must_use]
    pub fn with_kind(mut self, kind: impl Into<String>) -> Self {
        self.kind = Some(kind.into());
        self
    }

    /// Set the creation timestamp (RFC 3339).
    #[must_use]
    pub fn with_created_at(mut self, timestamp: impl Into<String>) -> Self {
        self.created_at = Some(timestamp.into());
        self
    }

    /// Add tags for filtering.
    #[must_use]
    pub fn with_tags(mut self, tags: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.tags = tags.into_iter().map(Into::into).collect();
        self
    }

    /// Add a metadata field for filtering, provenance, or diagnostics.
    ///
    /// Canonical fields such as `source`, `schema`, `workspace`, `level`,
    /// `kind`, `created_at`, and `tags` are reserved and are written by
    /// [`Self::into_indexable`].
    #[must_use]
    pub fn with_metadata_entry(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.metadata.insert(key.into(), value.into());
        self
    }

    /// Return the document ID.
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Return the searchable content.
    #[must_use]
    pub fn content(&self) -> &str {
        &self.content
    }

    /// Return the document source type.
    #[must_use]
    pub const fn source(&self) -> DocumentSource {
        self.source
    }

    /// Convert to frankensearch's [`IndexableDocument`].
    #[must_use]
    pub fn into_indexable(self) -> IndexableDocument {
        let mut metadata = self.metadata;
        metadata.insert("source".to_owned(), self.source.as_str().to_owned());
        metadata.insert("schema".to_owned(), CANONICAL_DOCUMENT_SCHEMA.to_owned());

        if let Some(ref workspace) = self.workspace {
            metadata.insert("workspace".to_owned(), workspace.clone());
        }
        if let Some(ref level) = self.level {
            metadata.insert("level".to_owned(), level.clone());
        }
        if let Some(ref kind) = self.kind {
            metadata.insert("kind".to_owned(), kind.clone());
        }
        if let Some(ref created_at) = self.created_at {
            metadata.insert("created_at".to_owned(), created_at.clone());
        }
        if !self.tags.is_empty() {
            metadata.insert("tags".to_owned(), self.tags.join(","));
        }

        let mut doc = IndexableDocument::new(self.id, self.content);
        if let Some(title) = self.title {
            doc = doc.with_title(title);
        }
        doc.metadata = metadata;
        doc
    }
}

fn push_labeled_line(lines: &mut Vec<String>, label: &str, value: &str) {
    if !value.trim().is_empty() {
        lines.push(format!("{label}: {value}"));
    }
}

fn push_optional_labeled_line(lines: &mut Vec<String>, label: &str, value: Option<&str>) {
    if let Some(value) = value {
        push_labeled_line(lines, label, value);
    }
}

/// Builder for converting stored memories to canonical search documents.
///
/// This is the integration point between `ee-db` (StoredMemory) and
/// `ee-search` (CanonicalSearchDocument). It maps memory fields to
/// the unified document format for Frankensearch indexing.
pub struct MemoryDocumentBuilder {
    workspace_path: Option<String>,
    tags: Vec<String>,
}

impl MemoryDocumentBuilder {
    /// Create a new builder with no workspace path or tags.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            workspace_path: None,
            tags: Vec::new(),
        }
    }

    /// Set the workspace path for the document.
    #[must_use]
    pub fn with_workspace_path(mut self, path: impl Into<String>) -> Self {
        self.workspace_path = Some(path.into());
        self
    }

    /// Set the tags for the document.
    #[must_use]
    pub fn with_tags(mut self, tags: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.tags = tags.into_iter().map(Into::into).collect();
        self
    }

    /// Build a canonical search document from a stored memory.
    ///
    /// The content field is used as the primary searchable text.
    /// Memory metadata (level, kind, timestamps) are preserved as
    /// document metadata for filtering and scoring.
    #[must_use]
    pub fn build(self, memory: &crate::db::StoredMemory) -> CanonicalSearchDocument {
        let mut doc =
            CanonicalSearchDocument::new(&memory.id, &memory.content, DocumentSource::Memory)
                .with_level(&memory.level)
                .with_kind(&memory.kind)
                .with_created_at(&memory.created_at);

        if let Some(workspace) = self.workspace_path {
            doc = doc.with_workspace(workspace);
        }

        if !self.tags.is_empty() {
            doc = doc.with_tags(self.tags);
        }

        doc
    }
}

impl Default for MemoryDocumentBuilder {
    fn default() -> Self {
        Self::new()
    }
}

/// Convert a stored memory directly to a canonical search document.
///
/// This is a convenience function for simple cases where no additional
/// context (workspace path, tags) is needed. For full control, use
/// [`MemoryDocumentBuilder`].
#[must_use]
pub fn memory_to_document(memory: &crate::db::StoredMemory) -> CanonicalSearchDocument {
    MemoryDocumentBuilder::new().build(memory)
}

/// Convert a stored memory with full context to a canonical search document.
///
/// This function fetches tags from the database and includes the workspace
/// path in the document metadata.
#[must_use]
pub fn memory_to_document_with_context(
    memory: &crate::db::StoredMemory,
    workspace_path: Option<&str>,
    tags: &[String],
) -> CanonicalSearchDocument {
    let mut builder = MemoryDocumentBuilder::new();

    if let Some(path) = workspace_path {
        builder = builder.with_workspace_path(path);
    }

    if !tags.is_empty() {
        builder = builder.with_tags(tags.iter().cloned());
    }

    builder.build(memory)
}

/// Builder for converting imported CASS sessions to canonical search documents.
///
/// Sessions currently index their stable CASS metadata rather than raw transcript
/// content. Evidence span indexing can attach richer excerpts later without
/// changing the session document identity or metadata contract.
pub struct SessionDocumentBuilder {
    workspace_path: Option<String>,
    tags: Vec<String>,
}

impl SessionDocumentBuilder {
    /// Create a new builder with no workspace path or tags.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            workspace_path: None,
            tags: Vec::new(),
        }
    }

    /// Set the workspace path for the document.
    #[must_use]
    pub fn with_workspace_path(mut self, path: impl Into<String>) -> Self {
        self.workspace_path = Some(path.into());
        self
    }

    /// Set the tags for the document.
    #[must_use]
    pub fn with_tags(mut self, tags: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.tags = tags.into_iter().map(Into::into).collect();
        self
    }

    /// Build a canonical search document from a stored CASS session row.
    #[must_use]
    pub fn build(self, session: &crate::db::StoredSession) -> CanonicalSearchDocument {
        let mut lines = vec![format!("CASS session: {}", session.cass_session_id)];
        push_optional_labeled_line(&mut lines, "Source path", session.source_path.as_deref());
        push_optional_labeled_line(&mut lines, "Agent", session.agent_name.as_deref());
        push_optional_labeled_line(&mut lines, "Model", session.model.as_deref());
        push_optional_labeled_line(&mut lines, "Started at", session.started_at.as_deref());
        push_optional_labeled_line(&mut lines, "Ended at", session.ended_at.as_deref());
        lines.push(format!("Messages: {}", session.message_count));
        if let Some(token_count) = session.token_count {
            lines.push(format!("Tokens: {token_count}"));
        }
        push_labeled_line(&mut lines, "Content hash", &session.content_hash);
        push_optional_labeled_line(&mut lines, "Metadata", session.metadata_json.as_deref());

        let created_at = session
            .started_at
            .as_deref()
            .unwrap_or(session.imported_at.as_str());

        let mut doc =
            CanonicalSearchDocument::new(&session.id, lines.join("\n"), DocumentSource::Session)
                .with_title(format!("CASS session {}", session.cass_session_id))
                .with_kind("cass_session")
                .with_created_at(created_at)
                .with_metadata_entry("workspace_id", &session.workspace_id)
                .with_metadata_entry("cass_session_id", &session.cass_session_id)
                .with_metadata_entry("message_count", session.message_count.to_string())
                .with_metadata_entry("content_hash", &session.content_hash)
                .with_metadata_entry("imported_at", &session.imported_at)
                .with_metadata_entry("updated_at", &session.updated_at);

        if let Some(workspace) = self.workspace_path {
            doc = doc.with_workspace(workspace);
        }
        if let Some(source_path) = &session.source_path {
            doc = doc.with_metadata_entry("source_path", source_path);
        }
        if let Some(agent_name) = &session.agent_name {
            doc = doc.with_metadata_entry("agent_name", agent_name);
        }
        if let Some(model) = &session.model {
            doc = doc.with_metadata_entry("model", model);
        }
        if let Some(started_at) = &session.started_at {
            doc = doc.with_metadata_entry("started_at", started_at);
        }
        if let Some(ended_at) = &session.ended_at {
            doc = doc.with_metadata_entry("ended_at", ended_at);
        }
        if let Some(token_count) = session.token_count {
            doc = doc.with_metadata_entry("token_count", token_count.to_string());
        }
        if let Some(metadata_json) = &session.metadata_json {
            doc = doc.with_metadata_entry("metadata_json", metadata_json);
        }
        if !self.tags.is_empty() {
            doc = doc.with_tags(self.tags);
        }

        doc
    }
}

impl Default for SessionDocumentBuilder {
    fn default() -> Self {
        Self::new()
    }
}

/// Convert a stored CASS session directly to a canonical search document.
#[must_use]
pub fn session_to_document(session: &crate::db::StoredSession) -> CanonicalSearchDocument {
    SessionDocumentBuilder::new().build(session)
}

/// Convert a stored CASS session with workspace and tags to a canonical document.
#[must_use]
pub fn session_to_document_with_context(
    session: &crate::db::StoredSession,
    workspace_path: Option<&str>,
    tags: &[String],
) -> CanonicalSearchDocument {
    let mut builder = SessionDocumentBuilder::new();

    if let Some(path) = workspace_path {
        builder = builder.with_workspace_path(path);
    }

    if !tags.is_empty() {
        builder = builder.with_tags(tags.iter().cloned());
    }

    builder.build(session)
}

/// Builder for converting registered coding artifacts to canonical documents.
///
/// Artifact rows are the source of truth; the search document is a derived,
/// rebuildable projection containing only safe metadata and optional snippets.
pub struct ArtifactDocumentBuilder {
    workspace_path: Option<String>,
}

impl ArtifactDocumentBuilder {
    /// Create a new artifact document builder.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            workspace_path: None,
        }
    }

    /// Set the workspace path for document metadata.
    #[must_use]
    pub fn with_workspace_path(mut self, path: impl Into<String>) -> Self {
        self.workspace_path = Some(path.into());
        self
    }

    /// Build a canonical search document from a stored artifact row.
    #[must_use]
    pub fn build(self, artifact: &crate::db::StoredArtifact) -> CanonicalSearchDocument {
        let mut lines = vec![
            format!("Artifact: {}", artifact.id),
            format!("Artifact type: {}", artifact.artifact_type),
            format!("Source kind: {}", artifact.source_kind),
            format!("Media type: {}", artifact.media_type),
            format!("Redaction status: {}", artifact.redaction_status),
            format!("Content hash: {}", artifact.content_hash),
        ];
        if let Some(path) = &artifact.original_path {
            push_labeled_line(&mut lines, "Path", path);
        }
        if let Some(external_ref) = &artifact.external_ref {
            push_labeled_line(&mut lines, "External ref", external_ref);
        }
        if let Some(snippet) = &artifact.snippet {
            push_labeled_line(&mut lines, "Snippet", snippet);
        }

        let title = artifact
            .original_path
            .as_deref()
            .or(artifact.external_ref.as_deref())
            .unwrap_or(artifact.id.as_str());

        let mut doc =
            CanonicalSearchDocument::new(&artifact.id, lines.join("\n"), DocumentSource::Artifact)
                .with_title(format!("Artifact {title}"))
                .with_kind(&artifact.artifact_type)
                .with_created_at(&artifact.created_at)
                .with_metadata_entry("workspace_id", &artifact.workspace_id)
                .with_metadata_entry("artifact_type", &artifact.artifact_type)
                .with_metadata_entry("source_kind", &artifact.source_kind)
                .with_metadata_entry("content_hash", &artifact.content_hash)
                .with_metadata_entry("media_type", &artifact.media_type)
                .with_metadata_entry("size_bytes", artifact.size_bytes.to_string())
                .with_metadata_entry("redaction_status", &artifact.redaction_status)
                .with_metadata_entry("updated_at", &artifact.updated_at);

        if let Some(workspace) = self.workspace_path {
            doc = doc.with_workspace(workspace);
        }
        if let Some(path) = &artifact.original_path {
            doc = doc.with_metadata_entry("path", path);
        }
        if let Some(external_ref) = &artifact.external_ref {
            doc = doc.with_metadata_entry("external_ref", external_ref);
        }
        if let Some(provenance_uri) = &artifact.provenance_uri {
            doc = doc.with_metadata_entry("provenance_uri", provenance_uri);
        }
        if let Some(snippet_hash) = &artifact.snippet_hash {
            doc = doc.with_metadata_entry("snippet_hash", snippet_hash);
        }

        doc
    }
}

impl Default for ArtifactDocumentBuilder {
    fn default() -> Self {
        Self::new()
    }
}

/// Convert a stored artifact directly to a canonical search document.
#[must_use]
pub fn artifact_to_document(artifact: &crate::db::StoredArtifact) -> CanonicalSearchDocument {
    ArtifactDocumentBuilder::new().build(artifact)
}

pub const MODULE_CONTRACT: &str = SEARCH_MODULE_SCHEMA_V1;
pub const REQUIRED_RETRIEVAL_ENGINE: &str = "frankensearch::TwoTierSearcher";
/// Frankensearch crate version selected by this package.
///
/// Keep this synchronized with the explicit `frankensearch` dependency version
/// in `Cargo.toml`; the local search contract test checks that drift.
pub const FRANKENSEARCH_VERSION: &str = "0.3.0";

static SEARCH_CAPABILITIES: [SearchCapability; 8] = [
    SearchCapability::ready(
        SearchCapabilityName::ModuleBoundary,
        SearchSurface::Status,
        "Search module is present.",
    ),
    SearchCapability::ready(
        SearchCapabilityName::FrankensearchDependency,
        SearchSurface::IndexAndQuery,
        "Frankensearch dependency is wired.",
    ),
    SearchCapability::ready(
        SearchCapabilityName::CanonicalDocument,
        SearchSurface::Indexing,
        "Canonical search document is defined.",
    ),
    SearchCapability::ready(
        SearchCapabilityName::IndexJobs,
        SearchSurface::Indexing,
        "Search index jobs table added (V005 migration).",
    ),
    SearchCapability::ready(
        SearchCapabilityName::IndexRebuild,
        SearchSurface::Indexing,
        "Index rebuild wired through Frankensearch.",
    ),
    SearchCapability::ready(
        SearchCapabilityName::JsonSearch,
        SearchSurface::Query,
        "Search results exposed through stable JSON response envelope.",
    ),
    SearchCapability::ready(
        SearchCapabilityName::RetrievalMetrics,
        SearchSurface::Evaluation,
        "Search JSON includes deterministic retrieval metrics.",
    ),
    SearchCapability::pending(
        SearchCapabilityName::ScoreExplanation,
        SearchSurface::Explanation,
        "Wire Frankensearch score components into ee search/context/why output renderers.",
    ),
];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SearchModuleReadiness {
    contract: &'static str,
    subsystem: &'static str,
    retrieval_engine: &'static str,
    capabilities: &'static [SearchCapability],
}

impl SearchModuleReadiness {
    #[must_use]
    pub const fn contract(&self) -> &'static str {
        self.contract
    }

    #[must_use]
    pub const fn subsystem(&self) -> &'static str {
        self.subsystem
    }

    #[must_use]
    pub const fn retrieval_engine(&self) -> &'static str {
        self.retrieval_engine
    }

    #[must_use]
    pub const fn capabilities(&self) -> &'static [SearchCapability] {
        self.capabilities
    }

    #[must_use]
    pub fn status(&self) -> CapabilityStatus {
        if self
            .capabilities
            .iter()
            .all(|capability| capability.status() == CapabilityStatus::Ready)
        {
            CapabilityStatus::Ready
        } else {
            CapabilityStatus::Pending
        }
    }

    pub fn missing_capabilities(&self) -> impl Iterator<Item = SearchCapability> + '_ {
        self.capabilities
            .iter()
            .copied()
            .filter(|capability| capability.status() != CapabilityStatus::Ready)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SearchCapability {
    name: SearchCapabilityName,
    surface: SearchSurface,
    status: CapabilityStatus,
    repair: &'static str,
}

impl SearchCapability {
    const fn ready(
        name: SearchCapabilityName,
        surface: SearchSurface,
        repair: &'static str,
    ) -> Self {
        Self {
            name,
            surface,
            status: CapabilityStatus::Ready,
            repair,
        }
    }

    const fn pending(
        name: SearchCapabilityName,
        surface: SearchSurface,
        repair: &'static str,
    ) -> Self {
        Self {
            name,
            surface,
            status: CapabilityStatus::Pending,
            repair,
        }
    }

    #[must_use]
    pub const fn name(self) -> SearchCapabilityName {
        self.name
    }

    #[must_use]
    pub const fn surface(self) -> SearchSurface {
        self.surface
    }

    #[must_use]
    pub const fn status(self) -> CapabilityStatus {
        self.status
    }

    #[must_use]
    pub const fn repair(self) -> &'static str {
        self.repair
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SearchCapabilityName {
    ModuleBoundary,
    FrankensearchDependency,
    CanonicalDocument,
    IndexJobs,
    IndexRebuild,
    JsonSearch,
    RetrievalMetrics,
    ScoreExplanation,
}

impl SearchCapabilityName {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ModuleBoundary => "module_boundary",
            Self::FrankensearchDependency => "frankensearch_dependency",
            Self::CanonicalDocument => "canonical_document",
            Self::IndexJobs => "index_jobs",
            Self::IndexRebuild => "index_rebuild",
            Self::JsonSearch => "json_search",
            Self::RetrievalMetrics => "retrieval_metrics",
            Self::ScoreExplanation => "score_explanation",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SearchSurface {
    Status,
    Indexing,
    Query,
    Evaluation,
    Explanation,
    IndexAndQuery,
}

impl SearchSurface {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Status => "status",
            Self::Indexing => "indexing",
            Self::Query => "query",
            Self::Evaluation => "evaluation",
            Self::Explanation => "explanation",
            Self::IndexAndQuery => "index_and_query",
        }
    }
}

#[must_use]
pub const fn subsystem_name() -> &'static str {
    SUBSYSTEM
}

#[must_use]
pub const fn module_readiness() -> SearchModuleReadiness {
    SearchModuleReadiness {
        contract: MODULE_CONTRACT,
        subsystem: SUBSYSTEM,
        retrieval_engine: REQUIRED_RETRIEVAL_ENGINE,
        capabilities: &SEARCH_CAPABILITIES,
    }
}

/// Deterministic score explanation for one Frankensearch result.
///
/// This is an ee-owned bridge type, not a second ranking system. It only
/// carries score values already produced by Frankensearch so higher-level
/// `search`, `context`, and `why` renderers can explain retrieval without
/// inventing narrative reasons.
#[derive(Clone, Debug, PartialEq)]
pub struct SearchScoreExplanation {
    pub doc_id: String,
    pub source: &'static str,
    pub final_score: f32,
    pub components: Vec<SearchScoreComponent>,
    pub frankensearch_explanation_available: bool,
    pub metadata_available: bool,
}

impl SearchScoreExplanation {
    #[must_use]
    pub fn from_scored_result(result: &ScoredResult) -> Self {
        let mut components = Vec::with_capacity(5);
        components.push(SearchScoreComponent::new("primary_score", result.score));
        push_optional_score_component(&mut components, "lexical_score", result.lexical_score);
        push_optional_score_component(&mut components, "semantic_fast_score", result.fast_score);
        push_optional_score_component(
            &mut components,
            "semantic_quality_score",
            result.quality_score,
        );
        push_optional_score_component(&mut components, "rerank_score", result.rerank_score);

        Self {
            doc_id: result.doc_id.clone(),
            source: score_source_name(result.source),
            final_score: result.score,
            components,
            frankensearch_explanation_available: result.explanation.is_some(),
            metadata_available: result.metadata.is_some(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SearchScoreComponent {
    pub name: &'static str,
    pub value: f32,
}

impl SearchScoreComponent {
    #[must_use]
    pub const fn new(name: &'static str, value: f32) -> Self {
        Self { name, value }
    }
}

#[must_use]
pub fn explain_scored_result(result: &ScoredResult) -> SearchScoreExplanation {
    SearchScoreExplanation::from_scored_result(result)
}

fn push_optional_score_component(
    components: &mut Vec<SearchScoreComponent>,
    name: &'static str,
    value: Option<f32>,
) {
    if let Some(value) = value {
        components.push(SearchScoreComponent::new(name, value));
    }
}

#[must_use]
pub const fn score_source_name(source: ScoreSource) -> &'static str {
    match source {
        ScoreSource::Lexical => "lexical",
        ScoreSource::SemanticFast => "semantic_fast",
        ScoreSource::SemanticQuality => "semantic_quality",
        ScoreSource::Hybrid => "hybrid",
        ScoreSource::Reranked => "reranked",
    }
}

// ============================================================================
// Index Manifest (EE-267)
//
// The index manifest tracks metadata about the search index state, enabling
// staleness detection and rebuild decisions without reading the full index.
// ============================================================================

/// Embedding model configuration stored in the manifest.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EmbeddingConfig {
    /// Model identifier (e.g., "hash-256", "model2vec-base").
    pub model_id: String,
    /// Embedding dimension.
    pub dimension: usize,
    /// Whether this is a deterministic hash-based embedder.
    pub deterministic: bool,
}

impl EmbeddingConfig {
    /// Create a new embedding configuration.
    #[must_use]
    pub fn new(model_id: impl Into<String>, dimension: usize, deterministic: bool) -> Self {
        Self {
            model_id: model_id.into(),
            dimension,
            deterministic,
        }
    }

    /// Create config for the default hash embedder.
    #[must_use]
    pub const fn hash_256() -> Self {
        Self {
            model_id: String::new(), // Will be set below
            dimension: 256,
            deterministic: true,
        }
    }
}

impl Default for EmbeddingConfig {
    fn default() -> Self {
        Self {
            model_id: "hash-256".to_owned(),
            dimension: 256,
            deterministic: true,
        }
    }
}

/// Index staleness status after validation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IndexStaleness {
    /// Index is current with the database.
    Current,
    /// Index is behind the database (needs rebuild).
    Stale,
    /// Index generation is ahead of database (corrupted or from different DB).
    Ahead,
    /// Database generation unknown (cannot determine staleness).
    Unknown,
}

impl IndexStaleness {
    /// Return a stable string representation.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Current => "current",
            Self::Stale => "stale",
            Self::Ahead => "ahead",
            Self::Unknown => "unknown",
        }
    }

    /// True if a rebuild is recommended.
    #[must_use]
    pub const fn needs_rebuild(self) -> bool {
        matches!(self, Self::Stale | Self::Ahead | Self::Unknown)
    }
}

/// Error returned when index manifest validation fails.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum IndexManifestError {
    /// Manifest file not found.
    NotFound { path: String },
    /// Manifest has invalid JSON format.
    InvalidFormat { message: String },
    /// Manifest schema version is not supported.
    UnsupportedSchema { schema: String, expected: String },
    /// Manifest is missing required fields.
    MissingField { field: String },
    /// Index generation mismatch with database.
    GenerationMismatch {
        index_generation: u64,
        db_generation: u64,
    },
    /// Embedding config mismatch (rebuild required).
    EmbeddingMismatch {
        expected_model: String,
        actual_model: String,
    },
    /// Embedding dimension mismatch (rebuild required).
    EmbeddingDimensionMismatch {
        expected_dimension: usize,
        actual_dimension: usize,
    },
    /// Embedding deterministic flag mismatch (rebuild required).
    EmbeddingDeterministicMismatch {
        expected: bool,
        actual: bool,
    },
    /// Document schema mismatch (rebuild required).
    DocumentSchemaMismatch {
        expected_schema: String,
        actual_schema: String,
    },
}

impl std::fmt::Display for IndexManifestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound { path } => {
                write!(f, "index manifest not found: {path}")
            }
            Self::InvalidFormat { message } => {
                write!(f, "invalid index manifest format: {message}")
            }
            Self::UnsupportedSchema { schema, expected } => {
                write!(
                    f,
                    "unsupported index manifest schema: {schema} (expected {expected})"
                )
            }
            Self::MissingField { field } => {
                write!(f, "index manifest missing required field: {field}")
            }
            Self::GenerationMismatch {
                index_generation,
                db_generation,
            } => {
                write!(
                    f,
                    "index generation {index_generation} does not match database generation {db_generation}"
                )
            }
            Self::EmbeddingMismatch {
                expected_model,
                actual_model,
            } => {
                write!(
                    f,
                    "index embedding model '{actual_model}' does not match expected '{expected_model}'"
                )
            }
            Self::EmbeddingDimensionMismatch {
                expected_dimension,
                actual_dimension,
            } => {
                write!(
                    f,
                    "index embedding dimension {actual_dimension} does not match expected {expected_dimension}"
                )
            }
            Self::EmbeddingDeterministicMismatch { expected, actual } => {
                write!(
                    f,
                    "index embedding deterministic flag {actual} does not match expected {expected}"
                )
            }
            Self::DocumentSchemaMismatch {
                expected_schema,
                actual_schema,
            } => {
                write!(
                    f,
                    "index document schema '{actual_schema}' does not match expected '{expected_schema}'"
                )
            }
        }
    }
}

impl std::error::Error for IndexManifestError {}

impl IndexManifestError {
    /// Return a repair suggestion for this error.
    #[must_use]
    pub fn repair(&self) -> &'static str {
        match self {
            Self::NotFound { .. } => "Run `ee index build` to create the index.",
            Self::InvalidFormat { .. } => "Delete the corrupted manifest and run `ee index build`.",
            Self::UnsupportedSchema { .. } => {
                "Upgrade ee or rebuild the index with `ee index build`."
            }
            Self::MissingField { .. } => "Run `ee index build` to regenerate the manifest.",
            Self::GenerationMismatch { .. } => "Run `ee index rebuild` to sync with the database.",
            Self::EmbeddingMismatch { .. } => {
                "Run `ee index rebuild` with the correct embedding model."
            }
            Self::EmbeddingDimensionMismatch { .. } => {
                "Run `ee index rebuild` to regenerate with correct embedding dimensions."
            }
            Self::EmbeddingDeterministicMismatch { .. } => {
                "Run `ee index rebuild` to regenerate with correct embedding configuration."
            }
            Self::DocumentSchemaMismatch { .. } => {
                "Run `ee index rebuild` to regenerate with current document schema."
            }
        }
    }

    /// Return the error code for JSON output.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::NotFound { .. } => "index_manifest_not_found",
            Self::InvalidFormat { .. } => "index_manifest_invalid",
            Self::UnsupportedSchema { .. } => "index_manifest_unsupported_schema",
            Self::MissingField { .. } => "index_manifest_missing_field",
            Self::GenerationMismatch { .. } => "index_generation_mismatch",
            Self::EmbeddingMismatch { .. } => "index_embedding_mismatch",
            Self::EmbeddingDimensionMismatch { .. } => "index_embedding_dimension_mismatch",
            Self::EmbeddingDeterministicMismatch { .. } => "index_embedding_deterministic_mismatch",
            Self::DocumentSchemaMismatch { .. } => "index_document_schema_mismatch",
        }
    }
}

/// Index manifest tracking index state and staleness.
#[derive(Clone, Debug)]
pub struct IndexManifest {
    /// Schema version for the manifest.
    pub schema: String,
    /// Index generation (incremented on each rebuild).
    pub generation: u64,
    /// Canonical document schema used to populate this index.
    pub document_schema: String,
    /// Frankensearch crate version used to build the index artifacts.
    pub frankensearch_version: String,
    /// RFC 3339 timestamp when the index was created.
    pub created_at: String,
    /// RFC 3339 timestamp when the index was last updated.
    pub updated_at: String,
    /// Number of documents in the index.
    pub document_count: u64,
    /// Database generation the index was built from.
    pub db_generation: u64,
    /// Embedding configuration used for the index.
    pub embedding: EmbeddingConfig,
    /// Path to the lexical index file (relative to manifest).
    pub lexical_index_path: Option<String>,
    /// Path to the vector index file (relative to manifest).
    pub vector_index_path: Option<String>,
}

impl IndexManifest {
    /// Create a new manifest with the given generation.
    #[must_use]
    pub fn new(
        generation: u64,
        created_at: impl Into<String>,
        document_count: u64,
        db_generation: u64,
        embedding: EmbeddingConfig,
    ) -> Self {
        let created = created_at.into();
        Self {
            schema: INDEX_MANIFEST_SCHEMA_V1.to_owned(),
            generation,
            document_schema: CANONICAL_DOCUMENT_SCHEMA.to_owned(),
            frankensearch_version: FRANKENSEARCH_VERSION.to_owned(),
            created_at: created.clone(),
            updated_at: created,
            document_count,
            db_generation,
            embedding,
            lexical_index_path: None,
            vector_index_path: None,
        }
    }

    /// Set the lexical index path.
    #[must_use]
    pub fn with_lexical_path(mut self, path: impl Into<String>) -> Self {
        self.lexical_index_path = Some(path.into());
        self
    }

    /// Set the vector index path.
    #[must_use]
    pub fn with_vector_path(mut self, path: impl Into<String>) -> Self {
        self.vector_index_path = Some(path.into());
        self
    }

    /// Check staleness against the current database generation.
    #[must_use]
    pub fn check_staleness(&self, current_db_generation: u64) -> IndexStaleness {
        match self.db_generation.cmp(&current_db_generation) {
            std::cmp::Ordering::Equal => IndexStaleness::Current,
            std::cmp::Ordering::Less => IndexStaleness::Stale,
            std::cmp::Ordering::Greater => IndexStaleness::Ahead,
        }
    }

    /// Validate the manifest schema version.
    ///
    /// # Errors
    ///
    /// Returns [`IndexManifestError::UnsupportedSchema`] if the schema
    /// doesn't match the expected version.
    pub fn validate_schema(&self) -> Result<(), IndexManifestError> {
        if self.schema == INDEX_MANIFEST_SCHEMA_V1 {
            Ok(())
        } else {
            Err(IndexManifestError::UnsupportedSchema {
                schema: self.schema.clone(),
                expected: INDEX_MANIFEST_SCHEMA_V1.to_owned(),
            })
        }
    }

    /// Validate the embedding configuration matches expected.
    ///
    /// Checks model ID, dimension, and deterministic flag.
    ///
    /// # Errors
    ///
    /// Returns an error if any embedding field mismatches.
    pub fn validate_embedding(&self, expected: &EmbeddingConfig) -> Result<(), IndexManifestError> {
        if self.embedding.model_id != expected.model_id {
            return Err(IndexManifestError::EmbeddingMismatch {
                expected_model: expected.model_id.clone(),
                actual_model: self.embedding.model_id.clone(),
            });
        }
        if self.embedding.dimension != expected.dimension {
            return Err(IndexManifestError::EmbeddingDimensionMismatch {
                expected_dimension: expected.dimension,
                actual_dimension: self.embedding.dimension,
            });
        }
        if self.embedding.deterministic != expected.deterministic {
            return Err(IndexManifestError::EmbeddingDeterministicMismatch {
                expected: expected.deterministic,
                actual: self.embedding.deterministic,
            });
        }
        Ok(())
    }

    /// Validate the document schema matches the current canonical schema.
    ///
    /// # Errors
    ///
    /// Returns [`IndexManifestError::DocumentSchemaMismatch`] if the schema
    /// doesn't match the current canonical document schema.
    pub fn validate_document_schema(&self) -> Result<(), IndexManifestError> {
        if self.document_schema == CANONICAL_DOCUMENT_SCHEMA {
            Ok(())
        } else {
            Err(IndexManifestError::DocumentSchemaMismatch {
                expected_schema: CANONICAL_DOCUMENT_SCHEMA.to_owned(),
                actual_schema: self.document_schema.clone(),
            })
        }
    }

    /// Full validation including schema, embedding, document schema, and staleness check.
    ///
    /// # Errors
    ///
    /// Returns the first validation error encountered.
    pub fn validate(
        &self,
        expected_embedding: &EmbeddingConfig,
        current_db_generation: u64,
    ) -> Result<IndexStaleness, IndexManifestError> {
        self.validate_schema()?;
        self.validate_document_schema()?;
        self.validate_embedding(expected_embedding)?;
        Ok(self.check_staleness(current_db_generation))
    }

    /// Stable JSON representation for index-manifest contract tests and
    /// future machine-facing output.
    #[must_use]
    pub fn data_json(&self) -> serde_json::Value {
        let mut value = serde_json::json!({
            "schema": self.schema,
            "generation": self.generation,
            "document_schema": self.document_schema,
            "frankensearch_version": self.frankensearch_version,
            "created_at": self.created_at,
            "updated_at": self.updated_at,
            "document_count": self.document_count,
            "db_generation": self.db_generation,
            "embedding": {
                "model_id": self.embedding.model_id,
                "dimension": self.embedding.dimension,
                "deterministic": self.embedding.deterministic,
            },
        });

        if let Some(path) = &self.lexical_index_path {
            value["lexical_index_path"] = serde_json::json!(path);
        }
        if let Some(path) = &self.vector_index_path {
            value["vector_index_path"] = serde_json::json!(path);
        }

        value
    }
}

impl Default for IndexManifest {
    fn default() -> Self {
        Self::new(0, "1970-01-01T00:00:00Z", 0, 0, EmbeddingConfig::default())
    }
}

#[cfg(test)]
mod tests {
    use super::{
        CanonicalSearchDocument, DocumentSource, Embedder, HashEmbedder, REQUIRED_RETRIEVAL_ENGINE,
        ScoreSource, ScoredResult, SearchCapabilityName, SearchSurface, explain_scored_result,
        module_readiness, score_source_name, subsystem_name,
    };
    use crate::models::CapabilityStatus;
    use serde_json::json;

    #[test]
    fn subsystem_name_is_stable() {
        assert_eq!(subsystem_name(), "search");
    }

    #[test]
    fn module_contract_names_frankensearch_boundary() {
        let readiness = module_readiness();

        assert_eq!(readiness.contract(), "ee.search.module.v1");
        assert_eq!(readiness.subsystem(), "search");
        assert_eq!(readiness.retrieval_engine(), REQUIRED_RETRIEVAL_ENGINE);
        assert_eq!(
            readiness.retrieval_engine(),
            "frankensearch::TwoTierSearcher"
        );
    }

    #[test]
    fn readiness_reports_pending_until_integration_lands() {
        let readiness = module_readiness();

        assert_eq!(readiness.status(), CapabilityStatus::Pending);
        assert_eq!(
            readiness
                .capabilities()
                .first()
                .map(|capability| capability.status()),
            Some(CapabilityStatus::Ready)
        );
        assert_eq!(readiness.missing_capabilities().count(), 1);
    }

    #[test]
    fn capabilities_are_in_dependency_order() {
        let names: Vec<&str> = module_readiness()
            .capabilities()
            .iter()
            .map(|capability| capability.name().as_str())
            .collect();

        assert_eq!(
            names,
            vec![
                "module_boundary",
                "frankensearch_dependency",
                "canonical_document",
                "index_jobs",
                "index_rebuild",
                "json_search",
                "retrieval_metrics",
                "score_explanation",
            ]
        );
    }

    #[test]
    fn capability_surfaces_are_stable() {
        let surfaces: Vec<&str> = module_readiness()
            .capabilities()
            .iter()
            .map(|capability| capability.surface().as_str())
            .collect();

        assert_eq!(
            surfaces,
            vec![
                "status",
                "index_and_query",
                "indexing",
                "indexing",
                "indexing",
                "query",
                "evaluation",
                "explanation",
            ]
        );
    }

    #[test]
    fn missing_capabilities_keep_repair_metadata() {
        let missing: Vec<_> = module_readiness().missing_capabilities().collect();

        assert_eq!(
            missing.first().map(|capability| capability.name()),
            Some(SearchCapabilityName::ScoreExplanation)
        );
        assert_eq!(
            missing.first().map(|capability| capability.surface()),
            Some(SearchSurface::Explanation)
        );
        assert!(
            missing
                .first()
                .map(|capability| capability.repair().contains("score"))
                .unwrap_or(false)
        );
    }

    #[test]
    fn score_source_names_are_stable() {
        assert_eq!(score_source_name(ScoreSource::Lexical), "lexical");
        assert_eq!(
            score_source_name(ScoreSource::SemanticFast),
            "semantic_fast"
        );
        assert_eq!(
            score_source_name(ScoreSource::SemanticQuality),
            "semantic_quality"
        );
        assert_eq!(score_source_name(ScoreSource::Hybrid), "hybrid");
        assert_eq!(score_source_name(ScoreSource::Reranked), "reranked");
    }

    #[test]
    fn scored_result_explanation_preserves_components_in_stable_order() {
        let result = ScoredResult {
            doc_id: "mem-release-rule".to_owned(),
            score: 0.875,
            source: ScoreSource::Hybrid,
            index: Some(17),
            fast_score: Some(0.82),
            quality_score: None,
            lexical_score: Some(3.5),
            rerank_score: Some(0.91),
            explanation: None,
            metadata: Some(json!({
                "source": "memory",
                "schema": super::CANONICAL_DOCUMENT_SCHEMA,
            })),
        };

        let explanation = explain_scored_result(&result);
        let components: Vec<(&str, String)> = explanation
            .components
            .iter()
            .map(|component| (component.name, format!("{:.3}", component.value)))
            .collect();

        assert_eq!(explanation.doc_id, "mem-release-rule");
        assert_eq!(explanation.source, "hybrid");
        assert_eq!(format!("{:.3}", explanation.final_score), "0.875");
        assert_eq!(
            components,
            vec![
                ("primary_score", "0.875".to_owned()),
                ("lexical_score", "3.500".to_owned()),
                ("semantic_fast_score", "0.820".to_owned()),
                ("rerank_score", "0.910".to_owned()),
            ]
        );
        assert!(!explanation.frankensearch_explanation_available);
        assert!(explanation.metadata_available);
    }

    #[test]
    fn scored_result_explanation_omits_absent_optional_scores() {
        let result = ScoredResult {
            doc_id: "mem-lexical-only".to_owned(),
            score: 1.25,
            source: ScoreSource::Lexical,
            index: None,
            fast_score: None,
            quality_score: None,
            lexical_score: None,
            rerank_score: None,
            explanation: None,
            metadata: None,
        };

        let explanation = explain_scored_result(&result);
        let component_names: Vec<&str> = explanation
            .components
            .iter()
            .map(|component| component.name)
            .collect();

        assert_eq!(explanation.source, "lexical");
        assert_eq!(component_names, vec!["primary_score"]);
        assert!(!explanation.frankensearch_explanation_available);
        assert!(!explanation.metadata_available);
    }

    #[test]
    fn frankensearch_hash_embedder_produces_deterministic_vectors() {
        let embedder = HashEmbedder::default_256();

        let text = "Rust ownership and borrowing";
        let embedding_a = embedder.embed_sync(text);
        let embedding_b = embedder.embed_sync(text);

        assert_eq!(embedding_a.len(), 256);
        assert_eq!(
            embedding_a, embedding_b,
            "hash embedder must be deterministic"
        );
    }

    #[test]
    fn frankensearch_hash_embedder_dimension_matches_config() {
        let embedder_256 = HashEmbedder::default_256();
        let embedder_384 = HashEmbedder::default_384();

        let text = "test document";
        assert_eq!(embedder_256.embed_sync(text).len(), 256);
        assert_eq!(embedder_384.embed_sync(text).len(), 384);
        assert_eq!(embedder_256.dimension(), 256);
        assert_eq!(embedder_384.dimension(), 384);
    }

    #[test]
    fn canonical_document_converts_to_indexable() {
        let doc = CanonicalSearchDocument::new(
            "mem-001",
            "Always run tests before commit",
            DocumentSource::Memory,
        )
        .with_title("pre-commit rule")
        .with_workspace("/home/user/project")
        .with_level("procedural")
        .with_kind("rule")
        .with_created_at("2026-04-29T12:00:00Z")
        .with_tags(["ci", "testing"]);

        let indexable = doc.into_indexable();

        assert_eq!(indexable.id, "mem-001");
        assert_eq!(indexable.content, "Always run tests before commit");
        assert_eq!(indexable.title.as_deref(), Some("pre-commit rule"));
        assert_eq!(indexable.metadata.get("source"), Some(&"memory".to_owned()));
        assert_eq!(
            indexable.metadata.get("workspace"),
            Some(&"/home/user/project".to_owned())
        );
        assert_eq!(
            indexable.metadata.get("level"),
            Some(&"procedural".to_owned())
        );
        assert_eq!(indexable.metadata.get("kind"), Some(&"rule".to_owned()));
        assert_eq!(
            indexable.metadata.get("tags"),
            Some(&"ci,testing".to_owned())
        );
    }

    #[test]
    fn canonical_document_source_types_are_stable() {
        assert_eq!(DocumentSource::Memory.as_str(), "memory");
        assert_eq!(DocumentSource::Session.as_str(), "session");
        assert_eq!(DocumentSource::Rule.as_str(), "rule");
        assert_eq!(DocumentSource::Import.as_str(), "import");
        assert_eq!(DocumentSource::Artifact.as_str(), "artifact");
    }

    #[test]
    fn canonical_document_minimal_conversion() {
        let doc = CanonicalSearchDocument::new("doc-1", "content only", DocumentSource::Session);
        let indexable = doc.into_indexable();

        assert_eq!(indexable.id, "doc-1");
        assert_eq!(indexable.content, "content only");
        assert!(indexable.title.is_none());
        assert_eq!(
            indexable.metadata.get("source"),
            Some(&"session".to_owned())
        );
        assert!(!indexable.metadata.contains_key("workspace"));
    }

    fn make_test_memory() -> crate::db::StoredMemory {
        crate::db::StoredMemory {
            id: "mem_01234567890123456789012345".to_string(),
            workspace_id: "wsp_01234567890123456789012345".to_string(),
            level: "procedural".to_string(),
            kind: "rule".to_string(),
            content: "Always run cargo fmt before commit.".to_string(),
            confidence: 0.9,
            utility: 0.7,
            importance: 0.8,
            provenance_uri: Some("file://AGENTS.md#L42".to_string()),
            trust_class: "human_explicit".to_string(),
            trust_subclass: Some("project-rule".to_string()),
            provenance_chain_hash: Some("blake3:test-provenance-chain".to_string()),
            provenance_chain_hash_version: crate::db::PROVENANCE_CHAIN_HASH_VERSION.to_string(),
            provenance_verification_status: crate::db::PROVENANCE_STATUS_UNVERIFIED.to_string(),
            provenance_verified_at: None,
            provenance_verification_note: None,
            created_at: "2026-04-29T12:00:00Z".to_string(),
            updated_at: "2026-04-29T12:00:00Z".to_string(),
            tombstoned_at: None,
            valid_from: None,
            valid_to: None,
        }
    }

    fn make_test_session() -> crate::db::StoredSession {
        crate::db::StoredSession {
            id: "sess_01234567890123456789012345".to_string(),
            workspace_id: "wsp_01234567890123456789012345".to_string(),
            cass_session_id: "cass-session-2026-04-29".to_string(),
            source_path: Some("/home/user/.cass/sessions/session.jsonl".to_string()),
            agent_name: Some("codex".to_string()),
            model: Some("gpt-5".to_string()),
            started_at: Some("2026-04-29T12:00:00Z".to_string()),
            ended_at: Some("2026-04-29T12:30:00Z".to_string()),
            message_count: 42,
            token_count: Some(12_345),
            content_hash: "blake3:session-content".to_string(),
            metadata_json: Some(r#"{"source":"cass","schema":"cass.session.v1"}"#.to_string()),
            imported_at: "2026-04-29T12:31:00Z".to_string(),
            updated_at: "2026-04-29T12:31:00Z".to_string(),
        }
    }

    fn make_test_artifact() -> crate::db::StoredArtifact {
        crate::db::StoredArtifact {
            id: "art_01234567890123456789012345".to_string(),
            workspace_id: "wsp_01234567890123456789012345".to_string(),
            source_kind: "file".to_string(),
            artifact_type: "log".to_string(),
            original_path: Some("logs/build.log".to_string()),
            canonical_path: Some("/workspace/project/logs/build.log".to_string()),
            external_ref: None,
            content_hash: "blake3:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
                .to_string(),
            media_type: "text/plain".to_string(),
            size_bytes: 256,
            redaction_status: "checked".to_string(),
            snippet: Some("cargo fmt passed".to_string()),
            snippet_hash: Some(
                "blake3:abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789"
                    .to_string(),
            ),
            provenance_uri: Some("file:///workspace/project/logs/build.log".to_string()),
            metadata_json: r#"{"title":"build log"}"#.to_string(),
            created_at: "2026-04-29T12:00:00Z".to_string(),
            updated_at: "2026-04-29T12:01:00Z".to_string(),
        }
    }

    #[test]
    fn memory_document_builder_minimal() {
        let memory = make_test_memory();
        let doc = super::memory_to_document(&memory);

        assert_eq!(doc.id(), "mem_01234567890123456789012345");
        assert_eq!(doc.content(), "Always run cargo fmt before commit.");
        assert_eq!(doc.source(), DocumentSource::Memory);

        let indexable = doc.into_indexable();
        assert_eq!(
            indexable.metadata.get("level"),
            Some(&"procedural".to_owned())
        );
        assert_eq!(indexable.metadata.get("kind"), Some(&"rule".to_owned()));
        assert_eq!(
            indexable.metadata.get("created_at"),
            Some(&"2026-04-29T12:00:00Z".to_owned())
        );
        assert!(!indexable.metadata.contains_key("workspace"));
    }

    #[test]
    fn memory_document_builder_with_context() {
        let memory = make_test_memory();
        let tags = vec!["cargo".to_string(), "formatting".to_string()];
        let doc =
            super::memory_to_document_with_context(&memory, Some("/home/user/project"), &tags);

        assert_eq!(doc.id(), "mem_01234567890123456789012345");
        assert_eq!(doc.source(), DocumentSource::Memory);

        let indexable = doc.into_indexable();
        assert_eq!(
            indexable.metadata.get("workspace"),
            Some(&"/home/user/project".to_owned())
        );
        assert_eq!(
            indexable.metadata.get("tags"),
            Some(&"cargo,formatting".to_owned())
        );
    }

    #[test]
    fn memory_document_builder_fluent_api() {
        let memory = make_test_memory();
        let doc = super::MemoryDocumentBuilder::new()
            .with_workspace_path("/data/projects/test")
            .with_tags(["ci", "testing"])
            .build(&memory);

        let indexable = doc.into_indexable();
        assert_eq!(
            indexable.metadata.get("workspace"),
            Some(&"/data/projects/test".to_owned())
        );
        assert_eq!(
            indexable.metadata.get("tags"),
            Some(&"ci,testing".to_owned())
        );
        assert_eq!(indexable.metadata.get("source"), Some(&"memory".to_owned()));
    }

    #[test]
    fn memory_document_builder_default() {
        let builder = super::MemoryDocumentBuilder::default();
        let memory = make_test_memory();
        let doc = builder.build(&memory);

        assert_eq!(doc.id(), memory.id);
        assert_eq!(doc.content(), memory.content);
    }

    #[test]
    fn session_document_builder_minimal() {
        let mut session = make_test_session();
        session.source_path = None;
        session.agent_name = None;
        session.model = None;
        session.started_at = None;
        session.ended_at = None;
        session.token_count = None;
        session.metadata_json = None;

        let doc = super::session_to_document(&session);

        assert_eq!(doc.id(), "sess_01234567890123456789012345");
        assert_eq!(doc.source(), DocumentSource::Session);
        assert!(
            doc.content()
                .contains("CASS session: cass-session-2026-04-29")
        );
        assert!(doc.content().contains("Messages: 42"));
        assert!(
            doc.content()
                .contains("Content hash: blake3:session-content")
        );

        let indexable = doc.into_indexable();
        assert_eq!(
            indexable.title.as_deref(),
            Some("CASS session cass-session-2026-04-29")
        );
        assert_eq!(
            indexable.metadata.get("source"),
            Some(&"session".to_owned())
        );
        assert_eq!(
            indexable.metadata.get("schema"),
            Some(&super::CANONICAL_DOCUMENT_SCHEMA.to_owned())
        );
        assert_eq!(
            indexable.metadata.get("kind"),
            Some(&"cass_session".to_owned())
        );
        assert_eq!(
            indexable.metadata.get("created_at"),
            Some(&"2026-04-29T12:31:00Z".to_owned())
        );
        assert_eq!(
            indexable.metadata.get("cass_session_id"),
            Some(&"cass-session-2026-04-29".to_owned())
        );
        assert_eq!(
            indexable.metadata.get("message_count"),
            Some(&"42".to_owned())
        );
        assert!(!indexable.metadata.contains_key("workspace"));
        assert!(!indexable.metadata.contains_key("token_count"));
    }

    #[test]
    fn session_document_builder_with_context() {
        let session = make_test_session();
        let tags = vec!["cass".to_string(), "session".to_string()];
        let doc = super::session_to_document_with_context(
            &session,
            Some("/data/projects/eidetic_engine_cli"),
            &tags,
        );

        assert_eq!(doc.id(), "sess_01234567890123456789012345");
        assert_eq!(doc.source(), DocumentSource::Session);
        assert!(doc.content().contains("Agent: codex"));
        assert!(doc.content().contains("Model: gpt-5"));
        assert!(doc.content().contains("Tokens: 12345"));
        assert!(doc.content().contains("Metadata: {\"source\":\"cass\""));

        let indexable = doc.into_indexable();
        assert_eq!(
            indexable.metadata.get("workspace"),
            Some(&"/data/projects/eidetic_engine_cli".to_owned())
        );
        assert_eq!(
            indexable.metadata.get("workspace_id"),
            Some(&"wsp_01234567890123456789012345".to_owned())
        );
        assert_eq!(
            indexable.metadata.get("agent_name"),
            Some(&"codex".to_owned())
        );
        assert_eq!(indexable.metadata.get("model"), Some(&"gpt-5".to_owned()));
        assert_eq!(
            indexable.metadata.get("started_at"),
            Some(&"2026-04-29T12:00:00Z".to_owned())
        );
        assert_eq!(
            indexable.metadata.get("ended_at"),
            Some(&"2026-04-29T12:30:00Z".to_owned())
        );
        assert_eq!(
            indexable.metadata.get("token_count"),
            Some(&"12345".to_owned())
        );
        assert_eq!(
            indexable.metadata.get("metadata_json"),
            Some(&r#"{"source":"cass","schema":"cass.session.v1"}"#.to_owned())
        );
        assert_eq!(
            indexable.metadata.get("tags"),
            Some(&"cass,session".to_owned())
        );
    }

    #[test]
    fn session_document_builder_fluent_api_and_reserved_metadata() {
        let session = make_test_session();
        let doc = super::SessionDocumentBuilder::new()
            .with_workspace_path("/workspace")
            .with_tags(["one", "two"])
            .build(&session)
            .with_metadata_entry("source", "caller-cannot-override-source");

        let indexable = doc.into_indexable();
        assert_eq!(
            indexable.metadata.get("source"),
            Some(&"session".to_owned())
        );
        assert_eq!(
            indexable.metadata.get("workspace"),
            Some(&"/workspace".to_owned())
        );
        assert_eq!(indexable.metadata.get("tags"), Some(&"one,two".to_owned()));
    }

    #[test]
    fn session_document_builder_default() {
        let builder = super::SessionDocumentBuilder::default();
        let session = make_test_session();
        let doc = builder.build(&session);

        assert_eq!(doc.id(), session.id);
        assert_eq!(doc.source(), DocumentSource::Session);
    }

    #[test]
    fn artifact_document_builder_indexes_safe_registry_projection() {
        let artifact = make_test_artifact();
        let doc = super::artifact_to_document(&artifact);

        assert_eq!(doc.id(), "art_01234567890123456789012345");
        assert_eq!(doc.source(), DocumentSource::Artifact);
        assert!(doc.content().contains("Artifact type: log"));
        assert!(doc.content().contains("Path: logs/build.log"));
        assert!(doc.content().contains("Snippet: cargo fmt passed"));

        let indexable = doc.into_indexable();
        assert_eq!(indexable.title.as_deref(), Some("Artifact logs/build.log"));
        assert_eq!(
            indexable.metadata.get("source"),
            Some(&"artifact".to_owned())
        );
        assert_eq!(
            indexable.metadata.get("workspace_id"),
            Some(&"wsp_01234567890123456789012345".to_owned())
        );
        assert_eq!(
            indexable.metadata.get("artifact_type"),
            Some(&"log".to_owned())
        );
        assert_eq!(
            indexable.metadata.get("redaction_status"),
            Some(&"checked".to_owned())
        );
        assert_eq!(
            indexable.metadata.get("path"),
            Some(&"logs/build.log".to_owned())
        );
        assert_eq!(
            indexable.metadata.get("snippet_hash"),
            Some(
                &"blake3:abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789"
                    .to_owned()
            )
        );
    }

    #[test]
    fn artifact_document_builder_with_workspace_context() {
        let artifact = make_test_artifact();
        let doc = super::ArtifactDocumentBuilder::new()
            .with_workspace_path("/workspace/project")
            .build(&artifact);

        let indexable = doc.into_indexable();
        assert_eq!(
            indexable.metadata.get("workspace"),
            Some(&"/workspace/project".to_owned())
        );
        assert_eq!(
            indexable.metadata.get("content_hash"),
            Some(
                &"blake3:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
                    .to_owned()
            )
        );
    }

    // =========================================================================
    // Index Manifest Tests (EE-267)
    // =========================================================================

    use super::{
        CANONICAL_DOCUMENT_SCHEMA, EmbeddingConfig, FRANKENSEARCH_VERSION,
        INDEX_MANIFEST_SCHEMA_V1, IndexManifest, IndexManifestError, IndexStaleness,
    };

    #[test]
    fn index_manifest_schema_constant_is_stable() {
        assert_eq!(INDEX_MANIFEST_SCHEMA_V1, "ee.index_manifest.v1");
    }

    #[test]
    fn embedding_config_default_is_hash_256() {
        let config = EmbeddingConfig::default();
        assert_eq!(config.model_id, "hash-256");
        assert_eq!(config.dimension, 256);
        assert!(config.deterministic);
    }

    #[test]
    fn index_staleness_as_str_is_stable() {
        assert_eq!(IndexStaleness::Current.as_str(), "current");
        assert_eq!(IndexStaleness::Stale.as_str(), "stale");
        assert_eq!(IndexStaleness::Ahead.as_str(), "ahead");
        assert_eq!(IndexStaleness::Unknown.as_str(), "unknown");
    }

    #[test]
    fn index_staleness_needs_rebuild() {
        assert!(!IndexStaleness::Current.needs_rebuild());
        assert!(IndexStaleness::Stale.needs_rebuild());
        assert!(IndexStaleness::Ahead.needs_rebuild());
        assert!(IndexStaleness::Unknown.needs_rebuild());
    }

    #[test]
    fn index_manifest_check_staleness_current() {
        let manifest = IndexManifest::new(
            1,
            "2026-04-30T12:00:00Z",
            100,
            5,
            EmbeddingConfig::default(),
        );
        assert_eq!(manifest.document_schema, CANONICAL_DOCUMENT_SCHEMA);
        assert_eq!(manifest.frankensearch_version, FRANKENSEARCH_VERSION);
        assert_eq!(manifest.check_staleness(5), IndexStaleness::Current);
    }

    #[test]
    fn index_manifest_check_staleness_stale() {
        let manifest = IndexManifest::new(
            1,
            "2026-04-30T12:00:00Z",
            100,
            5,
            EmbeddingConfig::default(),
        );
        assert_eq!(manifest.check_staleness(10), IndexStaleness::Stale);
    }

    #[test]
    fn index_manifest_check_staleness_ahead() {
        let manifest = IndexManifest::new(
            1,
            "2026-04-30T12:00:00Z",
            100,
            10,
            EmbeddingConfig::default(),
        );
        assert_eq!(manifest.check_staleness(5), IndexStaleness::Ahead);
    }

    #[test]
    fn index_manifest_validate_schema_success() {
        let manifest = IndexManifest::new(
            1,
            "2026-04-30T12:00:00Z",
            100,
            5,
            EmbeddingConfig::default(),
        );
        assert!(manifest.validate_schema().is_ok());
    }

    #[test]
    fn index_manifest_validate_schema_failure() {
        let manifest = IndexManifest {
            schema: "ee.index_manifest.v2".to_owned(),
            ..Default::default()
        };

        let result = manifest.validate_schema();
        assert_eq!(
            result,
            Err(IndexManifestError::UnsupportedSchema {
                schema: "ee.index_manifest.v2".to_owned(),
                expected: INDEX_MANIFEST_SCHEMA_V1.to_owned(),
            })
        );
    }

    #[test]
    fn index_manifest_validate_embedding_success() {
        let manifest = IndexManifest::new(
            1,
            "2026-04-30T12:00:00Z",
            100,
            5,
            EmbeddingConfig::default(),
        );
        let expected = EmbeddingConfig::default();
        assert!(manifest.validate_embedding(&expected).is_ok());
    }

    #[test]
    fn index_manifest_validate_embedding_failure() {
        let manifest = IndexManifest::new(
            1,
            "2026-04-30T12:00:00Z",
            100,
            5,
            EmbeddingConfig::new("model2vec-base", 384, false),
        );
        let expected = EmbeddingConfig::default();

        let result = manifest.validate_embedding(&expected);
        assert_eq!(
            result,
            Err(IndexManifestError::EmbeddingMismatch {
                expected_model: "hash-256".to_owned(),
                actual_model: "model2vec-base".to_owned(),
            })
        );
    }

    #[test]
    fn index_manifest_full_validate_returns_staleness() {
        let manifest = IndexManifest::new(
            1,
            "2026-04-30T12:00:00Z",
            100,
            5,
            EmbeddingConfig::default(),
        );
        let expected = EmbeddingConfig::default();

        let result = manifest.validate(&expected, 5);
        assert_eq!(result, Ok(IndexStaleness::Current));

        let result_stale = manifest.validate(&expected, 10);
        assert_eq!(result_stale, Ok(IndexStaleness::Stale));
    }

    #[test]
    fn index_manifest_validate_embedding_dimension_mismatch() {
        let manifest = IndexManifest::new(
            1,
            "2026-04-30T12:00:00Z",
            100,
            5,
            EmbeddingConfig::new("hash-256", 512, true), // Wrong dimension
        );
        let expected = EmbeddingConfig::default(); // dimension=256

        let result = manifest.validate_embedding(&expected);
        assert_eq!(
            result,
            Err(IndexManifestError::EmbeddingDimensionMismatch {
                expected_dimension: 256,
                actual_dimension: 512,
            })
        );
    }

    #[test]
    fn index_manifest_validate_embedding_deterministic_mismatch() {
        let manifest = IndexManifest::new(
            1,
            "2026-04-30T12:00:00Z",
            100,
            5,
            EmbeddingConfig::new("hash-256", 256, false), // Wrong deterministic flag
        );
        let expected = EmbeddingConfig::default(); // deterministic=true

        let result = manifest.validate_embedding(&expected);
        assert_eq!(
            result,
            Err(IndexManifestError::EmbeddingDeterministicMismatch {
                expected: true,
                actual: false,
            })
        );
    }

    #[test]
    fn index_manifest_validate_document_schema_success() {
        let manifest = IndexManifest::new(
            1,
            "2026-04-30T12:00:00Z",
            100,
            5,
            EmbeddingConfig::default(),
        );
        assert!(manifest.validate_document_schema().is_ok());
    }

    #[test]
    fn index_manifest_validate_document_schema_mismatch() {
        let mut manifest = IndexManifest::new(
            1,
            "2026-04-30T12:00:00Z",
            100,
            5,
            EmbeddingConfig::default(),
        );
        manifest.document_schema = "ee.search.document.v0".to_owned();

        let result = manifest.validate_document_schema();
        assert_eq!(
            result,
            Err(IndexManifestError::DocumentSchemaMismatch {
                expected_schema: CANONICAL_DOCUMENT_SCHEMA.to_owned(),
                actual_schema: "ee.search.document.v0".to_owned(),
            })
        );
    }

    #[test]
    fn index_manifest_stale_but_reachable_reports_stale_not_current() {
        // Bug: eidetic_engine_cli-86mw
        // A manifest with matching generation but incompatible artifacts should
        // fail validation, not report Current.

        // Case 1: Matching db_generation but wrong embedding dimension
        let manifest = IndexManifest::new(
            1,
            "2026-04-30T12:00:00Z",
            100,
            5, // Same db_generation as we'll check
            EmbeddingConfig::new("hash-256", 512, true), // Wrong dimension
        );
        let expected = EmbeddingConfig::default();

        // check_staleness alone would say Current (same generation)
        assert_eq!(manifest.check_staleness(5), IndexStaleness::Current);

        // But full validate should catch the incompatibility
        let result = manifest.validate(&expected, 5);
        assert!(result.is_err());
        assert_eq!(
            result,
            Err(IndexManifestError::EmbeddingDimensionMismatch {
                expected_dimension: 256,
                actual_dimension: 512,
            })
        );
    }

    #[test]
    fn index_manifest_full_validate_checks_document_schema() {
        let mut manifest = IndexManifest::new(
            1,
            "2026-04-30T12:00:00Z",
            100,
            5,
            EmbeddingConfig::default(),
        );
        manifest.document_schema = "ee.search.document.v0".to_owned();
        let expected = EmbeddingConfig::default();

        // check_staleness alone would say Current
        assert_eq!(manifest.check_staleness(5), IndexStaleness::Current);

        // But full validate should catch document schema mismatch
        let result = manifest.validate(&expected, 5);
        assert!(result.is_err());
        assert_eq!(
            result,
            Err(IndexManifestError::DocumentSchemaMismatch {
                expected_schema: CANONICAL_DOCUMENT_SCHEMA.to_owned(),
                actual_schema: "ee.search.document.v0".to_owned(),
            })
        );
    }

    #[test]
    fn index_manifest_error_codes_are_stable() {
        assert_eq!(
            IndexManifestError::NotFound {
                path: "x".to_owned()
            }
            .code(),
            "index_manifest_not_found"
        );
        assert_eq!(
            IndexManifestError::InvalidFormat {
                message: "x".to_owned()
            }
            .code(),
            "index_manifest_invalid"
        );
        assert_eq!(
            IndexManifestError::UnsupportedSchema {
                schema: "x".to_owned(),
                expected: "y".to_owned()
            }
            .code(),
            "index_manifest_unsupported_schema"
        );
        assert_eq!(
            IndexManifestError::MissingField {
                field: "x".to_owned()
            }
            .code(),
            "index_manifest_missing_field"
        );
        assert_eq!(
            IndexManifestError::GenerationMismatch {
                index_generation: 1,
                db_generation: 2
            }
            .code(),
            "index_generation_mismatch"
        );
        assert_eq!(
            IndexManifestError::EmbeddingMismatch {
                expected_model: "a".to_owned(),
                actual_model: "b".to_owned()
            }
            .code(),
            "index_embedding_mismatch"
        );
        assert_eq!(
            IndexManifestError::EmbeddingDimensionMismatch {
                expected_dimension: 256,
                actual_dimension: 512
            }
            .code(),
            "index_embedding_dimension_mismatch"
        );
        assert_eq!(
            IndexManifestError::EmbeddingDeterministicMismatch {
                expected: true,
                actual: false
            }
            .code(),
            "index_embedding_deterministic_mismatch"
        );
        assert_eq!(
            IndexManifestError::DocumentSchemaMismatch {
                expected_schema: "a".to_owned(),
                actual_schema: "b".to_owned()
            }
            .code(),
            "index_document_schema_mismatch"
        );
    }

    #[test]
    fn index_manifest_error_repair_suggestions_exist() {
        let errors = [
            IndexManifestError::NotFound {
                path: "x".to_owned(),
            },
            IndexManifestError::InvalidFormat {
                message: "x".to_owned(),
            },
            IndexManifestError::UnsupportedSchema {
                schema: "x".to_owned(),
                expected: "y".to_owned(),
            },
            IndexManifestError::MissingField {
                field: "x".to_owned(),
            },
            IndexManifestError::GenerationMismatch {
                index_generation: 1,
                db_generation: 2,
            },
            IndexManifestError::EmbeddingMismatch {
                expected_model: "a".to_owned(),
                actual_model: "b".to_owned(),
            },
            IndexManifestError::EmbeddingDimensionMismatch {
                expected_dimension: 256,
                actual_dimension: 512,
            },
            IndexManifestError::EmbeddingDeterministicMismatch {
                expected: true,
                actual: false,
            },
            IndexManifestError::DocumentSchemaMismatch {
                expected_schema: "a".to_owned(),
                actual_schema: "b".to_owned(),
            },
        ];
        for error in errors {
            assert!(
                !error.repair().is_empty(),
                "Repair for {:?} should not be empty",
                error
            );
        }
    }

    #[test]
    fn index_manifest_with_paths() {
        let manifest = IndexManifest::new(
            1,
            "2026-04-30T12:00:00Z",
            100,
            5,
            EmbeddingConfig::default(),
        )
        .with_lexical_path("lexical.idx")
        .with_vector_path("vector.idx");

        assert_eq!(manifest.lexical_index_path, Some("lexical.idx".to_owned()));
        assert_eq!(manifest.vector_index_path, Some("vector.idx".to_owned()));
    }

    #[test]
    fn index_manifest_data_json_includes_contract_metadata() {
        let manifest =
            IndexManifest::new(7, "2026-04-30T12:00:00Z", 3, 7, EmbeddingConfig::default())
                .with_lexical_path("lexical")
                .with_vector_path("vector.fast.idx");

        let json = manifest.data_json();

        assert_eq!(json["schema"], INDEX_MANIFEST_SCHEMA_V1);
        assert_eq!(json["generation"], 7);
        assert_eq!(json["document_schema"], CANONICAL_DOCUMENT_SCHEMA);
        assert_eq!(json["frankensearch_version"], FRANKENSEARCH_VERSION);
        assert_eq!(json["document_count"], 3);
        assert_eq!(json["db_generation"], 7);
        assert_eq!(json["embedding"]["model_id"], "hash-256");
        assert_eq!(json["embedding"]["dimension"], 256);
        assert_eq!(json["embedding"]["deterministic"], true);
        assert_eq!(json["lexical_index_path"], "lexical");
        assert_eq!(json["vector_index_path"], "vector.fast.idx");
    }
}
