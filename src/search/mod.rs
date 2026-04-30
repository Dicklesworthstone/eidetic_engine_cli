use std::collections::HashMap;

use crate::models::{CapabilityStatus, SEARCH_DOCUMENT_SCHEMA_V1, SEARCH_MODULE_SCHEMA_V1};

pub use frankensearch::core::types::IndexableDocument;
pub use frankensearch::{
    Embedder, EmbedderStack, HashEmbedder, IndexBuilder, ScoreSource, TwoTierConfig, TwoTierIndex,
    TwoTierSearcher,
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
}

impl DocumentSource {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Memory => "memory",
            Self::Session => "session",
            Self::Rule => "rule",
            Self::Import => "import",
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

pub const MODULE_CONTRACT: &str = SEARCH_MODULE_SCHEMA_V1;
pub const REQUIRED_RETRIEVAL_ENGINE: &str = "frankensearch::TwoTierSearcher";
pub const FRANKENSEARCH_VERSION: &str = env!("CARGO_PKG_VERSION");

static SEARCH_CAPABILITIES: [SearchCapability; 7] = [
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
    SearchCapability::pending(
        SearchCapabilityName::ScoreExplanation,
        SearchSurface::Explanation,
        "Carry Frankensearch score metadata into ee explanations.",
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
            Self::ScoreExplanation => "score_explanation",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SearchSurface {
    Status,
    Indexing,
    Query,
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

#[cfg(test)]
mod tests {
    use crate::models::CapabilityStatus;

    use super::{
        CanonicalSearchDocument, DocumentSource, Embedder, HashEmbedder, REQUIRED_RETRIEVAL_ENGINE,
        SearchCapabilityName, SearchSurface, module_readiness, subsystem_name,
    };

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
            created_at: "2026-04-29T12:00:00Z".to_string(),
            updated_at: "2026-04-29T12:00:00Z".to_string(),
            tombstoned_at: None,
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
}
