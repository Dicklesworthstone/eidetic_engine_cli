use std::collections::HashMap;

use crate::models::CapabilityStatus;

pub use frankensearch::core::types::IndexableDocument;
pub use frankensearch::{
    Embedder, EmbedderStack, HashEmbedder, IndexBuilder, TwoTierConfig, TwoTierIndex,
    TwoTierSearcher,
};

pub const SUBSYSTEM: &str = "search";
pub const CANONICAL_DOCUMENT_SCHEMA: &str = "ee.search.document.v1";

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
        let mut metadata = HashMap::new();
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
pub const MODULE_CONTRACT: &str = "ee.search.module.v1";
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
    SearchCapability::pending(
        SearchCapabilityName::IndexRebuild,
        SearchSurface::Indexing,
        "Wire index rebuild through Frankensearch.",
    ),
    SearchCapability::pending(
        SearchCapabilityName::JsonSearch,
        SearchSurface::Query,
        "Expose search results through the stable JSON response envelope.",
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
        assert_eq!(readiness.missing_capabilities().count(), 3);
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
            Some(SearchCapabilityName::IndexRebuild)
        );
        assert_eq!(
            missing.first().map(|capability| capability.surface()),
            Some(SearchSurface::Indexing)
        );
        assert!(
            missing
                .first()
                .map(|capability| capability.repair().contains("index"))
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
}
