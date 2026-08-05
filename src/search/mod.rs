use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use crate::cache::{CacheBudget, MemoryPressure, assess_pressure};
use crate::models::{
    CapabilityStatus, INDEX_MANIFEST_SCHEMA_V1, MEMORY_ANCHOR_SCHEMA_V1, RuleMaturity, RuleScope,
    SEARCH_DOCUMENT_SCHEMA_V1, SEARCH_MODULE_SCHEMA_V1, StoredMemoryAnchor,
};

pub mod bloom_prefilter;
pub mod bm25_simd;
pub mod hot_path;
pub mod lexical_ram_tier;
pub mod plan_cache;
pub mod query;
pub mod scoring;
pub mod simhash;
pub mod tag_bitmaps;

pub use frankensearch::core::types::IndexableDocument;
pub use frankensearch::{
    Embedder, EmbedderStack, HashEmbedder, IndexBuilder, NativeReranker, Reranker, ScoreSource,
    ScoredResult, TwoTierConfig, TwoTierIndex, TwoTierSearcher,
};
#[cfg(feature = "lexical-bm25")]
pub use frankensearch::{LexicalRead, LexicalWrite, TantivyIndex};
pub use query::{ParsedSearchQuery, SearchQueryClause, parse_search_query};
pub use scoring::{
    AnchorMatchCandidateSignals, AnchorMatchContext, AnchorMatchScore,
    BeadAffinityCandidateSignals, BeadAffinityContext, BeadAffinityScore,
    DEFAULT_ANCHOR_MATCH_BIAS_CAP, DEFAULT_BEAD_AFFINITY_BIAS_CAP, ParseSpeedModeError,
    RetrievalMaturity, SearchScoreComponents, SearchScoringConfig, SearchScoringSignals, SpeedMode,
    anchor_match_score, bead_affinity_score, final_score,
};

pub const SUBSYSTEM: &str = "search";
pub const CANONICAL_DOCUMENT_SCHEMA: &str = SEARCH_DOCUMENT_SCHEMA_V1;
pub(crate) const MEMORY_INDEX_PROJECTION_SCHEMA_V1: &str = "ee.memory_index_projection.v1";
pub(crate) const SESSION_INDEX_PROJECTION_SCHEMA_V1: &str = "ee.session_index_projection.v1";
pub(crate) const ARTIFACT_INDEX_PROJECTION_SCHEMA_V1: &str = "ee.artifact_index_projection.v1";
pub(crate) const RULE_INDEX_PROJECTION_SCHEMA_V1: &str = "ee.rule_index_projection.v1";
pub(crate) const EVIDENCE_INDEX_PROJECTION_SCHEMA_V1: &str = "ee.evidence_index_projection.v1";
pub const MEMORY_ANCHOR_SCHEMA_METADATA_KEY: &str = "memory_anchor_schema";
pub const MEMORY_ANCHOR_COUNT_METADATA_KEY: &str = "memory_anchor_count";
pub const MEMORY_ANCHOR_KINDS_METADATA_KEY: &str = "memory_anchor_kinds";
pub const MEMORY_ANCHOR_HASHES_METADATA_KEY: &str = "memory_anchor_hashes";
pub const MEMORY_ANCHOR_REDACTED_VALUES_METADATA_KEY: &str = "memory_anchor_redacted_values";
pub const MEMORY_ANCHOR_FRESHNESS_METADATA_KEY: &str = "memory_anchor_freshness";

/// Emit a standard tracing checkpoint for the radix-ULID tie-breaker surface.
///
/// The actual radix sorter lands under the owning `bd-3usjw.50` implementation
/// bead; this helper keeps the search callsite's Part II tracing contract ready
/// without changing current ranking behavior.
pub fn trace_radix_ulid_sort_checkpoint(
    phase: &'static str,
    elapsed_ms: u64,
    candidate_count: usize,
    degraded_codes: &[&str],
) {
    tracing::info!(
        workspace_id = "search",
        request_id = "radix_ulid_sort_request",
        bead_id = option_env!("EE_TRACE_BEAD_ID").unwrap_or("bd-3usjw.50"),
        surface = "radix_ulid_sort",
        phase,
        elapsed_ms,
        candidate_count,
        degraded_codes = ?degraded_codes,
        "radix ULID sort checkpoint"
    );
}

/// Source type for canonical search documents.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum DocumentSource {
    Memory,
    Session,
    Rule,
    Import,
    Artifact,
    CurationCandidate,
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
            Self::CurationCandidate => "curation_candidate",
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
    metadata: BTreeMap<String, String>,
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
            metadata: BTreeMap::new(),
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
        doc.metadata = metadata.into_iter().collect();
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
    anchors: Vec<StoredMemoryAnchor>,
    typed_fields_json: Option<String>,
}

impl MemoryDocumentBuilder {
    /// Create a new builder with no workspace path or tags.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            workspace_path: None,
            tags: Vec::new(),
            anchors: Vec::new(),
            typed_fields_json: None,
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

    /// Attach hash/redacted memory-anchor metadata to the document.
    #[must_use]
    pub fn with_anchors(mut self, anchors: impl IntoIterator<Item = StoredMemoryAnchor>) -> Self {
        self.anchors = anchors.into_iter().collect();
        self
    }

    /// Attach a validated typed-field sidecar for metadata indexing.
    #[must_use]
    pub fn with_typed_fields_json(mut self, typed_fields_json: impl Into<String>) -> Self {
        self.typed_fields_json = Some(typed_fields_json.into());
        self
    }

    /// Build a canonical search document from a stored memory.
    ///
    /// The content field is used as the primary searchable text.
    /// Memory metadata (level, kind, timestamps) are preserved as
    /// document metadata for filtering and scoring.
    #[must_use]
    pub fn build(self, memory: &crate::db::StoredMemory) -> CanonicalSearchDocument {
        let (content, content_truncated) = content_preview_with_flag(&memory.content);
        let mut doc =
            CanonicalSearchDocument::new(&memory.id, &memory.content, DocumentSource::Memory)
                .with_level(&memory.level)
                .with_kind(&memory.kind)
                .with_created_at(&memory.created_at)
                .with_metadata_entry("content", content)
                .with_metadata_entry("content_truncated", content_truncated.to_string())
                .with_metadata_entry(
                    "provenanceVerificationStatus",
                    &memory.provenance_verification_status,
                )
                .with_metadata_entry(
                    "validity_window_kind",
                    memory_validity_window_kind(
                        memory.valid_from.as_deref(),
                        memory.valid_to.as_deref(),
                    ),
                );
        if let Some(hash) = &memory.provenance_chain_hash {
            doc = doc.with_metadata_entry("provenanceChainHash", hash.as_str());
        }
        if let Some(verified_at) = &memory.provenance_verified_at {
            doc = doc.with_metadata_entry("provenanceVerifiedAt", verified_at.as_str());
        }

        if let Some(workspace) = self.workspace_path {
            doc = doc.with_workspace(workspace);
        }

        if let Some(valid_from) = &memory.valid_from {
            doc = doc.with_metadata_entry("valid_from", valid_from);
        }
        if let Some(valid_to) = &memory.valid_to {
            doc = doc.with_metadata_entry("valid_to", valid_to);
        }

        if let Some(typed_fields_json) = self.typed_fields_json.as_deref()
            && let Ok(kind) = crate::models::MemoryKind::from_str(&memory.kind)
            && let Ok(metadata) = crate::models::memory::typed_memory_index_metadata_from_json(
                &kind,
                typed_fields_json,
            )
        {
            for (key, value) in metadata {
                doc = doc.with_metadata_entry(key, value);
            }
        }

        if !self.tags.is_empty() {
            doc = doc.with_tags(self.tags);
        }

        doc = attach_memory_anchor_metadata(doc, &self.anchors);

        doc
    }
}

impl Default for MemoryDocumentBuilder {
    fn default() -> Self {
        Self::new()
    }
}

fn memory_validity_window_kind(valid_from: Option<&str>, valid_to: Option<&str>) -> &'static str {
    match (valid_from, valid_to) {
        (None, None) => "unbounded",
        (Some(from), Some(to)) if from == to => "instant",
        (Some(_), Some(_)) => "bounded",
        (Some(_), None) => "starts_at",
        (None, Some(_)) => "ends_at",
    }
}

fn content_preview_with_flag(content: &str) -> (String, bool) {
    const MAX_CHARS: usize = 240;
    let mut preview = String::new();
    for (index, ch) in content.chars().enumerate() {
        if index == MAX_CHARS {
            preview.push_str("...");
            return (preview, true);
        }
        preview.push(ch);
    }
    (preview, false)
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

/// Convert a stored memory with workspace, tags, and anchor metadata.
///
/// Anchor metadata is hash/redacted only and is deterministic, so derived
/// Frankensearch documents can be rebuilt without exposing raw code anchors.
#[must_use]
pub fn memory_to_document_with_context_and_anchors(
    memory: &crate::db::StoredMemory,
    workspace_path: Option<&str>,
    tags: &[String],
    anchors: &[StoredMemoryAnchor],
) -> CanonicalSearchDocument {
    memory_to_document_with_context_anchors_and_typed_fields(
        memory,
        workspace_path,
        tags,
        anchors,
        None,
    )
}

/// Convert a stored memory with workspace, tags, anchors, and typed fields.
#[must_use]
pub fn memory_to_document_with_context_anchors_and_typed_fields(
    memory: &crate::db::StoredMemory,
    workspace_path: Option<&str>,
    tags: &[String],
    anchors: &[StoredMemoryAnchor],
    typed_fields_json: Option<&str>,
) -> CanonicalSearchDocument {
    let mut builder = MemoryDocumentBuilder::new();

    if let Some(path) = workspace_path {
        builder = builder.with_workspace_path(path);
    }

    if !tags.is_empty() {
        builder = builder.with_tags(tags.iter().cloned());
    }

    if !anchors.is_empty() {
        builder = builder.with_anchors(anchors.iter().cloned());
    }

    if let Some(typed_fields_json) = typed_fields_json {
        builder = builder.with_typed_fields_json(typed_fields_json);
    }

    builder.build(memory)
}

fn attach_memory_anchor_metadata(
    mut doc: CanonicalSearchDocument,
    anchors: &[StoredMemoryAnchor],
) -> CanonicalSearchDocument {
    if anchors.is_empty() {
        return doc;
    }

    let mut anchors = anchors.to_vec();
    anchors.sort_by(|left, right| {
        left.anchor_kind
            .cmp(&right.anchor_kind)
            .then_with(|| left.anchor_value_hash.cmp(&right.anchor_value_hash))
    });

    let mut kinds = Vec::new();
    let mut hashes = Vec::new();
    let mut redacted_values = Vec::new();
    let mut freshness = Vec::new();
    let mut last_kind = None;

    for anchor in &anchors {
        let kind = anchor.anchor_kind.as_str();
        if last_kind != Some(kind) {
            kinds.push(kind.to_owned());
            last_kind = Some(kind);
        }
        hashes.push(format!("{kind}:{}", anchor.anchor_value_hash));
        redacted_values.push(anchor.redacted_anchor_value.clone());
        freshness.push(format!(
            "{kind}:{}:{}:{}",
            anchor.anchor_value_hash,
            anchor.freshness_state.as_str(),
            anchor.generation
        ));
    }

    doc = doc
        .with_metadata_entry(MEMORY_ANCHOR_SCHEMA_METADATA_KEY, MEMORY_ANCHOR_SCHEMA_V1)
        .with_metadata_entry(MEMORY_ANCHOR_COUNT_METADATA_KEY, anchors.len().to_string())
        .with_metadata_entry(MEMORY_ANCHOR_KINDS_METADATA_KEY, kinds.join(","))
        .with_metadata_entry(MEMORY_ANCHOR_HASHES_METADATA_KEY, hashes.join(","))
        .with_metadata_entry(
            MEMORY_ANCHOR_REDACTED_VALUES_METADATA_KEY,
            redacted_values.join(","),
        )
        .with_metadata_entry(MEMORY_ANCHOR_FRESHNESS_METADATA_KEY, freshness.join(","));
    doc
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
        let public_provenance = format!("cass-session://{}", session.id);
        let mut lines = vec![format!("CASS session: {}", session.id)];
        push_optional_labeled_line(&mut lines, "Agent", session.agent_name.as_deref());
        push_optional_labeled_line(&mut lines, "Model", session.model.as_deref());
        push_optional_labeled_line(&mut lines, "Started at", session.started_at.as_deref());
        push_optional_labeled_line(&mut lines, "Ended at", session.ended_at.as_deref());
        lines.push(format!("Messages: {}", session.message_count));
        if let Some(token_count) = session.token_count {
            lines.push(format!("Tokens: {token_count}"));
        }

        let created_at = session
            .started_at
            .as_deref()
            .unwrap_or(session.imported_at.as_str());

        let mut doc =
            CanonicalSearchDocument::new(&session.id, lines.join("\n"), DocumentSource::Session)
                .with_title(format!("CASS session {}", session.id))
                .with_kind("cass_session")
                .with_created_at(created_at)
                .with_metadata_entry("workspace_id", &session.workspace_id)
                .with_metadata_entry("provenance_uri", public_provenance)
                .with_metadata_entry("message_count", session.message_count.to_string())
                .with_metadata_entry("imported_at", &session.imported_at)
                .with_metadata_entry("updated_at", &session.updated_at);

        if let Some(workspace) = self.workspace_path {
            doc = doc.with_workspace(workspace);
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

/// Stable validation failure for a workspace-relative rule scope pattern.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RuleScopePatternError {
    MissingRequired,
    Unexpected,
    ContainsNul,
    Absolute,
    Traversal,
    WorkspaceUnavailable,
    SymlinkEscape,
    PathInspectionFailed,
}

impl RuleScopePatternError {
    /// Stable machine-facing code used in rule projection metadata and hashes.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::MissingRequired => "missing_required",
            Self::Unexpected => "unexpected",
            Self::ContainsNul => "contains_nul",
            Self::Absolute => "absolute",
            Self::Traversal => "traversal",
            Self::WorkspaceUnavailable => "workspace_unavailable",
            Self::SymlinkEscape => "symlink_escape",
            Self::PathInspectionFailed => "path_inspection_failed",
        }
    }
}

impl fmt::Display for RuleScopePatternError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::MissingRequired => "scope requires a non-empty workspace-relative pattern",
            Self::Unexpected => "scope does not accept a pattern",
            Self::ContainsNul => "scope pattern contains a NUL byte",
            Self::Absolute => "scope pattern must be workspace-relative",
            Self::Traversal => "scope pattern must not contain parent traversal",
            Self::WorkspaceUnavailable => "workspace root could not be resolved",
            Self::SymlinkEscape => "scope pattern escapes the workspace through a symbolic link",
            Self::PathInspectionFailed => "scope pattern path could not be inspected safely",
        };
        formatter.write_str(message)
    }
}

impl std::error::Error for RuleScopePatternError {}

/// Normalize and validate a rule scope pattern against its workspace root.
///
/// Stored and public patterns use `/` separators on every platform. Absolute
/// paths, Windows drive prefixes, parent traversal, and existing symlink
/// prefixes that resolve outside the workspace are rejected. A missing suffix
/// is allowed because patterns commonly describe files that do not exist yet.
pub fn normalize_rule_scope_pattern(
    workspace_root: &Path,
    scope: RuleScope,
    raw: Option<&str>,
) -> Result<Option<String>, RuleScopePatternError> {
    let pattern = raw.map(str::trim).filter(|value| !value.is_empty());
    if scope.requires_pattern() && pattern.is_none() {
        return Err(RuleScopePatternError::MissingRequired);
    }
    if !scope.requires_pattern() && pattern.is_some() {
        return Err(RuleScopePatternError::Unexpected);
    }
    let Some(pattern) = pattern else {
        return Ok(None);
    };
    if pattern.contains('\0') {
        return Err(RuleScopePatternError::ContainsNul);
    }

    let portable = pattern.replace('\\', "/");
    let first_segment = portable.split('/').next().unwrap_or_default();
    let has_windows_drive_prefix = first_segment.as_bytes().get(1) == Some(&b':')
        && first_segment
            .as_bytes()
            .first()
            .is_some_and(u8::is_ascii_alphabetic);
    if portable.starts_with('/') || has_windows_drive_prefix {
        return Err(RuleScopePatternError::Absolute);
    }

    let mut segments = Vec::new();
    for segment in portable.split('/') {
        match segment {
            "" | "." => {}
            ".." => return Err(RuleScopePatternError::Traversal),
            value => segments.push(value),
        }
    }
    if segments.is_empty() {
        return Err(RuleScopePatternError::MissingRequired);
    }

    let canonical_root = std::fs::canonicalize(workspace_root)
        .map_err(|_| RuleScopePatternError::WorkspaceUnavailable)?;
    let mut inspected = canonical_root.clone();
    for segment in segments
        .iter()
        .take_while(|segment| !contains_glob_metacharacter(segment))
    {
        inspected.push(segment);
        match std::fs::symlink_metadata(&inspected) {
            Ok(_) => {
                let resolved = std::fs::canonicalize(&inspected)
                    .map_err(|_| RuleScopePatternError::PathInspectionFailed)?;
                if !resolved.starts_with(&canonical_root) {
                    return Err(RuleScopePatternError::SymlinkEscape);
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => break,
            Err(_) => return Err(RuleScopePatternError::PathInspectionFailed),
        }
    }

    Ok(Some(segments.join("/")))
}

fn contains_glob_metacharacter(segment: &str) -> bool {
    segment
        .bytes()
        .any(|byte| matches!(byte, b'*' | b'?' | b'[' | b']' | b'{' | b'}'))
}

/// Complete, deterministic procedural-rule projection used by search and pack
/// admission.
///
/// The rule row is the searchable body. Tags are searchable filter metadata;
/// source-memory IDs and lifecycle counters are provenance-only metadata. The
/// revision covers every stored rule field plus both junction sets, so a
/// derived document can be compared with live storage without trusting search
/// metadata as authorization.
#[derive(Clone, Debug)]
pub struct RuleIndexProjection {
    rule: crate::db::StoredProceduralRule,
    workspace_path: PathBuf,
    tags: Vec<String>,
    source_memory_ids: Vec<String>,
    normalized_scope_pattern: Option<String>,
    scope_pattern_posture: &'static str,
    scope_pattern_error: Option<&'static str>,
    entity_revision: String,
}

impl RuleIndexProjection {
    /// Build a canonical rule projection from a row and its joined metadata.
    #[must_use]
    pub fn new(
        rule: crate::db::StoredProceduralRule,
        workspace_path: impl Into<PathBuf>,
        mut tags: Vec<String>,
        mut source_memory_ids: Vec<String>,
    ) -> Self {
        tags.sort();
        tags.dedup();
        source_memory_ids.sort();
        source_memory_ids.dedup();
        let workspace_path = workspace_path.into();
        let (normalized_scope_pattern, scope_pattern_posture, scope_pattern_error) =
            match RuleScope::from_str(&rule.scope) {
                Ok(scope) => match normalize_rule_scope_pattern(
                    &workspace_path,
                    scope,
                    rule.scope_pattern.as_deref(),
                ) {
                    Ok(pattern) => (
                        pattern,
                        if scope.requires_pattern() {
                            "valid"
                        } else {
                            "not_applicable"
                        },
                        None,
                    ),
                    Err(error) => (None, "invalid", Some(error.code())),
                },
                Err(_) => (None, "invalid", Some("invalid_scope")),
            };
        let entity_revision = rule_entity_revision(
            &rule,
            &tags,
            &source_memory_ids,
            normalized_scope_pattern.as_deref(),
            scope_pattern_posture,
            scope_pattern_error,
        );
        Self {
            rule,
            workspace_path,
            tags,
            source_memory_ids,
            normalized_scope_pattern,
            scope_pattern_posture,
            scope_pattern_error,
            entity_revision,
        }
    }

    #[must_use]
    pub const fn rule(&self) -> &crate::db::StoredProceduralRule {
        &self.rule
    }

    #[must_use]
    pub fn tags(&self) -> &[String] {
        &self.tags
    }

    #[must_use]
    pub fn source_memory_ids(&self) -> &[String] {
        &self.source_memory_ids
    }

    #[must_use]
    pub fn entity_revision(&self) -> &str {
        &self.entity_revision
    }

    #[must_use]
    pub fn normalized_scope_pattern(&self) -> Option<&str> {
        self.normalized_scope_pattern.as_deref()
    }

    #[must_use]
    pub const fn scope_pattern_posture(&self) -> &'static str {
        self.scope_pattern_posture
    }

    /// Whether this projection belongs in the derived search corpus.
    #[must_use]
    pub fn is_search_indexable(&self) -> bool {
        self.rule.tombstoned_at.is_none()
            && self.rule.superseded_by.is_none()
            && self.rule.maturity != RuleMaturity::Superseded.as_str()
    }

    /// Fail-closed rule eligibility at the current pack hydration boundary.
    #[must_use]
    pub fn is_pack_admissible(&self) -> bool {
        self.is_search_indexable()
            && matches!(
                RuleMaturity::from_str(&self.rule.maturity),
                Ok(RuleMaturity::Candidate | RuleMaturity::Validated)
            )
            && self.scope_pattern_posture != "invalid"
    }
}

fn rule_entity_revision(
    rule: &crate::db::StoredProceduralRule,
    tags: &[String],
    source_memory_ids: &[String],
    normalized_scope_pattern: Option<&str>,
    scope_pattern_posture: &str,
    scope_pattern_error: Option<&str>,
) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(RULE_INDEX_PROJECTION_SCHEMA_V1.as_bytes());
    hash_rule_bytes(&mut hasher, "id", rule.id.as_bytes());
    hash_rule_bytes(&mut hasher, "workspace_id", rule.workspace_id.as_bytes());
    hash_rule_bytes(&mut hasher, "content", rule.content.as_bytes());
    hash_rule_bytes(
        &mut hasher,
        "confidence",
        &rule.confidence.to_bits().to_le_bytes(),
    );
    hash_rule_bytes(
        &mut hasher,
        "utility",
        &rule.utility.to_bits().to_le_bytes(),
    );
    hash_rule_bytes(
        &mut hasher,
        "importance",
        &rule.importance.to_bits().to_le_bytes(),
    );
    hash_rule_bytes(&mut hasher, "trust_class", rule.trust_class.as_bytes());
    hash_rule_bytes(&mut hasher, "scope", rule.scope.as_bytes());
    hash_rule_optional_str(&mut hasher, "scope_pattern", rule.scope_pattern.as_deref());
    hash_rule_bytes(&mut hasher, "maturity", rule.maturity.as_bytes());
    hash_rule_bytes(&mut hasher, "protected", &[u8::from(rule.protected)]);
    hash_rule_bytes(
        &mut hasher,
        "positive_feedback_count",
        &rule.positive_feedback_count.to_le_bytes(),
    );
    hash_rule_bytes(
        &mut hasher,
        "negative_feedback_count",
        &rule.negative_feedback_count.to_le_bytes(),
    );
    hash_rule_bytes(
        &mut hasher,
        "validation_passes",
        &rule.validation_passes.to_le_bytes(),
    );
    hash_rule_bytes(
        &mut hasher,
        "validation_contradictions",
        &rule.validation_contradictions.to_le_bytes(),
    );
    hash_rule_optional_str(
        &mut hasher,
        "last_applied_at",
        rule.last_applied_at.as_deref(),
    );
    hash_rule_optional_str(
        &mut hasher,
        "last_validated_at",
        rule.last_validated_at.as_deref(),
    );
    hash_rule_optional_str(&mut hasher, "superseded_by", rule.superseded_by.as_deref());
    hash_rule_bytes(&mut hasher, "created_at", rule.created_at.as_bytes());
    hash_rule_bytes(&mut hasher, "updated_at", rule.updated_at.as_bytes());
    hash_rule_optional_str(&mut hasher, "tombstoned_at", rule.tombstoned_at.as_deref());
    hash_rule_optional_str(
        &mut hasher,
        "normalized_scope_pattern",
        normalized_scope_pattern,
    );
    hash_rule_bytes(
        &mut hasher,
        "scope_pattern_posture",
        scope_pattern_posture.as_bytes(),
    );
    hash_rule_optional_str(&mut hasher, "scope_pattern_error", scope_pattern_error);
    hash_rule_string_list(&mut hasher, "tags", tags);
    hash_rule_string_list(&mut hasher, "source_memory_ids", source_memory_ids);
    format!("blake3:{}", hasher.finalize().to_hex())
}

fn hash_rule_bytes(hasher: &mut blake3::Hasher, label: &str, value: &[u8]) {
    hasher.update(&(label.len() as u64).to_le_bytes());
    hasher.update(label.as_bytes());
    hasher.update(&(value.len() as u64).to_le_bytes());
    hasher.update(value);
}

fn hash_rule_optional_str(hasher: &mut blake3::Hasher, label: &str, value: Option<&str>) {
    match value {
        Some(value) => {
            hash_rule_bytes(hasher, label, &[1]);
            hash_rule_bytes(hasher, "value", value.as_bytes());
        }
        None => hash_rule_bytes(hasher, label, &[0]),
    }
}

fn hash_rule_string_list(hasher: &mut blake3::Hasher, label: &str, values: &[String]) {
    hash_rule_bytes(hasher, label, &(values.len() as u64).to_le_bytes());
    for value in values {
        hash_rule_bytes(hasher, "item", value.as_bytes());
    }
}

/// Convert a complete procedural-rule projection to a search document.
#[must_use]
pub fn rule_to_document(projection: &RuleIndexProjection) -> CanonicalSearchDocument {
    let rule = projection.rule();
    let mut doc = CanonicalSearchDocument::new(&rule.id, &rule.content, DocumentSource::Rule)
        .with_title(format!("Procedural rule {}", rule.id))
        .with_workspace(projection.workspace_path.display().to_string())
        .with_level("procedural")
        .with_kind("rule")
        .with_created_at(&rule.created_at)
        .with_tags(projection.tags().iter().cloned())
        .with_metadata_entry("workspace_id", &rule.workspace_id)
        .with_metadata_entry("maturity", &rule.maturity)
        .with_metadata_entry("scope", &rule.scope)
        .with_metadata_entry("trust_class", &rule.trust_class)
        .with_metadata_entry("confidence", rule.confidence.to_string())
        .with_metadata_entry("utility", rule.utility.to_string())
        .with_metadata_entry("importance", rule.importance.to_string())
        .with_metadata_entry("protected", rule.protected.to_string())
        .with_metadata_entry(
            "positive_feedback_count",
            rule.positive_feedback_count.to_string(),
        )
        .with_metadata_entry(
            "negative_feedback_count",
            rule.negative_feedback_count.to_string(),
        )
        .with_metadata_entry("validation_passes", rule.validation_passes.to_string())
        .with_metadata_entry(
            "validation_contradictions",
            rule.validation_contradictions.to_string(),
        )
        .with_metadata_entry("updated_at", &rule.updated_at)
        .with_metadata_entry("entity_revision", projection.entity_revision())
        .with_metadata_entry("projection_schema", RULE_INDEX_PROJECTION_SCHEMA_V1)
        .with_metadata_entry("scope_pattern_posture", projection.scope_pattern_posture())
        .with_metadata_entry(
            "source_memory_count",
            projection.source_memory_ids().len().to_string(),
        );
    if !projection.source_memory_ids().is_empty() {
        doc = doc.with_metadata_entry(
            "source_memory_ids",
            projection.source_memory_ids().join(","),
        );
    }
    if let Some(pattern) = projection.normalized_scope_pattern() {
        doc = doc.with_metadata_entry("scope_pattern", pattern);
    }
    if let Some(error) = projection.scope_pattern_error {
        doc = doc.with_metadata_entry("scope_pattern_error", error);
    }
    if let Some(last_applied_at) = rule.last_applied_at.as_deref() {
        doc = doc.with_metadata_entry("last_applied_at", last_applied_at);
    }
    if let Some(last_validated_at) = rule.last_validated_at.as_deref() {
        doc = doc.with_metadata_entry("last_validated_at", last_validated_at);
    }
    if let Some(superseded_by) = rule.superseded_by.as_deref() {
        doc = doc.with_metadata_entry("superseded_by", superseded_by);
    }
    if let Some(tombstoned_at) = rule.tombstoned_at.as_deref() {
        doc = doc.with_metadata_entry("tombstoned_at", tombstoned_at);
    }
    doc
}

/// Convert a stored imported evidence span to a canonical search document.
///
/// The caller must have positively admitted the live row through
/// `StoredEvidenceSpan::is_search_admitted_for_session`. This projection
/// still repeats the egress screen defensively and never includes a raw CASS
/// span id, source path, or upstream metadata.
#[must_use]
pub fn evidence_span_to_document(span: &crate::db::StoredEvidenceSpan) -> CanonicalSearchDocument {
    let egress = crate::policy::screen_external_text_for_ingestion(&span.excerpt);
    let withheld = egress.redacted
        || egress.instruction_like
        || !matches!(egress.instruction_risk, "none" | "low");
    let safe_excerpt = if withheld {
        "[EVIDENCE_WITHHELD]".to_owned()
    } else {
        egress.content
    };
    let mut doc = CanonicalSearchDocument::new(&span.id, safe_excerpt, DocumentSource::Import)
        .with_title(format!(
            "Imported evidence {} (session {}, lines {}-{})",
            span.id, span.session_id, span.start_line, span.end_line
        ))
        .with_kind("evidence_span")
        .with_created_at(&span.created_at)
        .with_metadata_entry("workspace_id", &span.workspace_id)
        .with_metadata_entry("session_id", &span.session_id)
        .with_metadata_entry("provenance_uri", span.canonical_provenance_uri())
        .with_metadata_entry("span_kind", &span.span_kind)
        .with_metadata_entry("start_line", span.start_line.to_string())
        .with_metadata_entry("end_line", span.end_line.to_string())
        .with_metadata_entry("producer_kind", &span.producer_kind)
        .with_metadata_entry("screening_version", span.screening_version.to_string())
        .with_metadata_entry(
            "security_policy_epoch",
            span.security_policy_epoch.to_string(),
        )
        .with_metadata_entry(
            "canonical_provenance_revision",
            span.canonical_provenance_revision.to_string(),
        )
        .with_metadata_entry("secret_redaction_status", &span.secret_redaction_status)
        .with_metadata_entry("instruction_risk", &span.instruction_risk)
        .with_metadata_entry("search_eligibility", &span.search_eligibility)
        .with_metadata_entry("pack_eligibility", &span.pack_eligibility);
    if !withheld {
        doc = doc.with_metadata_entry("content_hash", &span.content_hash);
    }
    if let Some(memory_id) = span.memory_id.as_deref() {
        doc = doc.with_metadata_entry("memory_id", memory_id);
    }
    if let Some(role) = span.role.as_deref() {
        doc = doc.with_metadata_entry("role", role);
    }
    doc
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
        let safe_original_path = artifact
            .original_path
            .as_deref()
            .map(redact_artifact_search_ref);
        let safe_external_ref = artifact
            .external_ref
            .as_deref()
            .map(redact_artifact_search_ref);
        let safe_provenance_uri = artifact
            .provenance_uri
            .as_deref()
            .map(redact_artifact_search_ref);
        let mut lines = vec![
            format!("Artifact: {}", artifact.id),
            format!("Artifact type: {}", artifact.artifact_type),
            format!("Source kind: {}", artifact.source_kind),
            format!("Media type: {}", artifact.media_type),
            format!("Redaction status: {}", artifact.redaction_status),
            format!("Content hash: {}", artifact.content_hash),
        ];
        if let Some(path) = &safe_original_path {
            push_labeled_line(&mut lines, "Path", path);
        }
        if let Some(external_ref) = &safe_external_ref {
            push_labeled_line(&mut lines, "External ref", external_ref);
        }
        if let Some(snippet) = &artifact.snippet {
            push_labeled_line(&mut lines, "Snippet", snippet);
        }

        let title = safe_original_path
            .as_deref()
            .or(safe_external_ref.as_deref())
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
        if let Some(path) = &safe_original_path {
            doc = doc.with_metadata_entry("path", path);
        }
        if let Some(external_ref) = &safe_external_ref {
            doc = doc.with_metadata_entry("external_ref", external_ref);
        }
        if let Some(provenance_uri) = &safe_provenance_uri {
            doc = doc.with_metadata_entry("provenance_uri", provenance_uri);
        }
        if let Some(snippet_hash) = &artifact.snippet_hash {
            doc = doc.with_metadata_entry("snippet_hash", snippet_hash);
        }

        doc
    }
}

fn redact_artifact_search_ref(value: &str) -> String {
    redact_search_projection_ref(value)
}

fn redact_curation_candidate_search_ref(value: &str) -> String {
    redact_search_projection_ref(value)
}

fn redact_search_projection_ref(value: &str) -> String {
    let secret_redacted = crate::policy::redact_secret_like_content(value).content;
    redact_search_projection_absolute_path_like_segments(&secret_redacted)
}

pub(crate) fn redact_search_projection_absolute_path_like_segments(input: &str) -> String {
    const REDACTED_PATH: &str = "[REDACTED_PATH]";
    const UNIX_PATH_PREFIXES: &[&str] = &[
        "/home/",
        "/Users/",
        "/data/",
        "/workspace/",
        "/workspaces/",
        "/Volumes/",
        "/var/run/",
        "/run/",
        "/var/lib/docker/",
        "/var/lib/kubelet/",
        "/var/folders/",
        "/var/log/",
        "/var/tmp/",
        "/proc/",
        "/sys/",
        "/dev/",
        "/etc/ssh/",
        "/etc/kubernetes/",
        "/etc/ssl/",
        "/etc/letsencrypt/",
        "/etc/secrets/",
        "/mnt/",
        "/media/",
        "/app/",
        "/github/workspace/",
        "/__w/",
        "/root/",
        "/tmp/",
        "/private/var/run/",
        "/private/var/log/",
        "/private/var/tmp/",
        "/private/var/folders/",
        "/private/etc/ssh/",
        "/private/etc/kubernetes/",
        "/private/etc/ssl/",
        "/private/etc/letsencrypt/",
        "/private/etc/secrets/",
        "/private/tmp/",
    ];

    let mut output = String::with_capacity(input.len());
    let mut cursor = 0usize;
    while cursor < input.len() {
        let remaining = &input[cursor..];
        if let Some(prefix_len) =
            search_projection_path_prefix_len(input, cursor, UNIX_PATH_PREFIXES)
        {
            output.push_str(REDACTED_PATH);
            cursor += prefix_len;
            while cursor < input.len() {
                let next = input[cursor..].chars().next().unwrap_or('\0');
                if next.is_whitespace() {
                    if whitespace_starts_search_path_continuation(input, cursor) {
                        cursor += next.len_utf8();
                        continue;
                    }
                    break;
                }
                if is_search_path_hard_delimiter(next) {
                    break;
                }
                cursor += next.len_utf8();
            }
            continue;
        }

        let next = remaining.chars().next().unwrap_or('\0');
        output.push(next);
        cursor += next.len_utf8();
    }

    output
}

fn search_projection_path_prefix_len(
    input: &str,
    cursor: usize,
    unix_path_prefixes: &[&str],
) -> Option<usize> {
    let remaining = &input[cursor..];
    if let Some(prefix) = unix_path_prefixes
        .iter()
        .find(|prefix| search_projection_unix_prefix_matches(remaining, prefix))
    {
        return Some(prefix.len());
    }

    if starts_with_search_projection_file_host_ref(remaining) {
        return Some("file://".len());
    }

    if let Some(prefix_len) = search_projection_env_path_prefix_len(remaining) {
        return Some(prefix_len);
    }
    if let Some(prefix_len) = sensitive_relative_path_prefix_len(remaining) {
        return Some(prefix_len);
    }

    if remaining.starts_with(r"\\?\") || remaining.starts_with(r"\\.\") {
        return Some(4);
    }
    if remaining.starts_with(r"\\") {
        return Some(2);
    }
    if starts_with_forward_slash_network_path(input, cursor) {
        return Some(2);
    }

    let bytes = remaining.as_bytes();
    if bytes.first().is_some_and(|byte| byte.is_ascii_alphabetic())
        && matches!(bytes.get(1), Some(b':'))
        && matches!(bytes.get(2), Some(b'\\' | b'/'))
        && search_projection_drive_prefix_is_bounded(input, cursor)
    {
        Some(3)
    } else {
        None
    }
}

fn search_projection_drive_prefix_is_bounded(input: &str, cursor: usize) -> bool {
    cursor == 0
        || input
            .as_bytes()
            .get(cursor.saturating_sub(1))
            .is_none_or(|byte| !byte.is_ascii_alphanumeric())
}

fn search_projection_unix_prefix_matches(remaining: &str, prefix: &str) -> bool {
    if is_case_insensitive_macos_search_path_prefix(prefix) {
        remaining
            .get(..prefix.len())
            .is_some_and(|head| head.eq_ignore_ascii_case(prefix))
    } else {
        remaining.starts_with(prefix)
    }
}

fn is_case_insensitive_macos_search_path_prefix(prefix: &str) -> bool {
    matches!(
        prefix,
        "/Users/"
            | "/Volumes/"
            | "/var/run/"
            | "/var/log/"
            | "/var/tmp/"
            | "/var/folders/"
            | "/private/var/run/"
            | "/private/var/log/"
            | "/private/var/tmp/"
            | "/private/var/folders/"
            | "/private/etc/ssh/"
            | "/private/etc/kubernetes/"
            | "/private/etc/ssl/"
            | "/private/etc/letsencrypt/"
            | "/private/etc/secrets/"
            | "/private/tmp/"
    )
}

fn starts_with_search_projection_file_host_ref(remaining: &str) -> bool {
    const FILE_SCHEME: &str = "file://";
    if !starts_with_ascii_case_insensitive(remaining, FILE_SCHEME) {
        return false;
    }

    let bytes = remaining.as_bytes();
    bytes
        .get(FILE_SCHEME.len())
        .is_some_and(|byte| !matches!(byte, b'/' | b'\\'))
        || matches!(
            (
                bytes.get(FILE_SCHEME.len()),
                bytes.get(FILE_SCHEME.len() + 1)
            ),
            (Some(b'/'), Some(b'/'))
        )
}

fn search_projection_env_path_prefix_len(remaining: &str) -> Option<usize> {
    let bytes = remaining.as_bytes();
    if bytes.first() == Some(&b'~') {
        return tilde_path_prefix_len(bytes);
    }
    if bytes.first() == Some(&b'%') {
        return percent_env_path_prefix_len(bytes);
    }
    if starts_with_ascii_case_insensitive(remaining, "$env:") {
        return dollar_env_path_prefix_len(bytes, "$env:".len());
    }
    if remaining.starts_with("${") {
        return braced_env_path_prefix_len(bytes);
    }
    if bytes.first() == Some(&b'$') {
        return dollar_env_path_prefix_len(bytes, 1);
    }
    None
}

fn tilde_path_prefix_len(bytes: &[u8]) -> Option<usize> {
    let separator = bytes.iter().position(|byte| matches!(byte, b'/' | b'\\'))?;
    if separator == 1
        || bytes[1..separator]
            .iter()
            .all(|byte| is_env_name_byte(*byte))
    {
        Some(separator + 1)
    } else {
        None
    }
}

fn percent_env_path_prefix_len(bytes: &[u8]) -> Option<usize> {
    let closing = bytes.iter().skip(1).position(|byte| *byte == b'%')? + 1;
    if closing == 1 || !bytes[1..closing].iter().all(|byte| is_env_name_byte(*byte)) {
        return None;
    }
    let after = closing + 1;
    if bytes
        .get(after)
        .is_some_and(|byte| matches!(byte, b'/' | b'\\' | b'%'))
    {
        Some(after)
    } else {
        None
    }
}

fn braced_env_path_prefix_len(bytes: &[u8]) -> Option<usize> {
    let closing = bytes.iter().skip(2).position(|byte| *byte == b'}')? + 2;
    if closing == 2 || !bytes[2..closing].iter().all(|byte| is_env_name_byte(*byte)) {
        return None;
    }
    let after = closing + 1;
    if bytes
        .get(after)
        .is_some_and(|byte| matches!(byte, b'/' | b'\\'))
    {
        Some(after + 1)
    } else {
        None
    }
}

fn dollar_env_path_prefix_len(bytes: &[u8], name_start: usize) -> Option<usize> {
    let mut cursor = name_start;
    while bytes
        .get(cursor)
        .is_some_and(|byte| is_env_name_byte(*byte))
    {
        cursor += 1;
    }
    if cursor == name_start {
        return None;
    }
    if bytes
        .get(cursor)
        .is_some_and(|byte| matches!(byte, b'/' | b'\\'))
    {
        Some(cursor + 1)
    } else {
        None
    }
}

fn is_env_name_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'(' | b')')
}

fn sensitive_relative_path_prefix_len(remaining: &str) -> Option<usize> {
    const SENSITIVE_RELATIVE_PATH_PREFIXES: &[&str] = &[
        ".ssh/",
        ".ssh\\",
        "./.ssh/",
        r#".\.ssh\"#,
        "../.ssh/",
        r#"..\.ssh\"#,
        ".aws/",
        ".aws\\",
        "./.aws/",
        r#".\.aws\"#,
        "../.aws/",
        r#"..\.aws\"#,
        ".kube/",
        ".kube\\",
        "./.kube/",
        r#".\.kube\"#,
        "../.kube/",
        r#"..\.kube\"#,
        ".config/gcloud/",
        ".config\\gcloud\\",
        "./.config/gcloud/",
        r#".\.config\gcloud\"#,
        "../.config/gcloud/",
        r#"..\.config\gcloud\"#,
        ".config/gh/",
        ".config\\gh\\",
        "./.config/gh/",
        r#".\.config\gh\"#,
        "../.config/gh/",
        r#"..\.config\gh\"#,
        ".azure/",
        ".azure\\",
        "./.azure/",
        r#".\.azure\"#,
        "../.azure/",
        r#"..\.azure\"#,
        ".docker/",
        ".docker\\",
        "./.docker/",
        r#".\.docker\"#,
        "../.docker/",
        r#"..\.docker\"#,
        ".gnupg/",
        ".gnupg\\",
        "./.gnupg/",
        r#".\.gnupg\"#,
        "../.gnupg/",
        r#"..\.gnupg\"#,
        ".cargo/credentials",
        ".cargo\\credentials",
        "./.cargo/credentials",
        r#".\.cargo\credentials"#,
        "../.cargo/credentials",
        r#"..\.cargo\credentials"#,
        ".netrc",
        "./.netrc",
        r#".\.netrc"#,
        "../.netrc",
        r#"..\.netrc"#,
        ".npmrc",
        "./.npmrc",
        r#".\.npmrc"#,
        "../.npmrc",
        r#"..\.npmrc"#,
        ".yarnrc",
        "./.yarnrc",
        r#".\.yarnrc"#,
        "../.yarnrc",
        r#"..\.yarnrc"#,
        ".pnpmrc",
        "./.pnpmrc",
        r#".\.pnpmrc"#,
        "../.pnpmrc",
        r#"..\.pnpmrc"#,
        ".pypirc",
        "./.pypirc",
        r#".\.pypirc"#,
        "../.pypirc",
        r#"..\.pypirc"#,
        ".config/pip/pip.conf",
        ".config\\pip\\pip.conf",
        "./.config/pip/pip.conf",
        r#".\.config\pip\pip.conf"#,
        "../.config/pip/pip.conf",
        r#"..\.config\pip\pip.conf"#,
        ".pip/pip.conf",
        ".pip\\pip.conf",
        "./.pip/pip.conf",
        r#".\.pip\pip.conf"#,
        "../.pip/pip.conf",
        r#"..\.pip\pip.conf"#,
        ".pip/pip.ini",
        ".pip\\pip.ini",
        "./.pip/pip.ini",
        r#".\.pip\pip.ini"#,
        "../.pip/pip.ini",
        r#"..\.pip\pip.ini"#,
        ".composer/auth.json",
        ".composer\\auth.json",
        "./.composer/auth.json",
        r#".\.composer\auth.json"#,
        "../.composer/auth.json",
        r#"..\.composer\auth.json"#,
        ".gradle/gradle.properties",
        ".gradle\\gradle.properties",
        "./.gradle/gradle.properties",
        r#".\.gradle\gradle.properties"#,
        "../.gradle/gradle.properties",
        r#"..\.gradle\gradle.properties"#,
        ".m2/settings.xml",
        ".m2\\settings.xml",
        "./.m2/settings.xml",
        r#".\.m2\settings.xml"#,
        "../.m2/settings.xml",
        r#"..\.m2\settings.xml"#,
        ".nuget/NuGet/NuGet.Config",
        ".nuget\\NuGet\\NuGet.Config",
        "./.nuget/NuGet/NuGet.Config",
        r#".\.nuget\NuGet\NuGet.Config"#,
        "../.nuget/NuGet/NuGet.Config",
        r#"..\.nuget\NuGet\NuGet.Config"#,
        ".gem/credentials",
        ".gem\\credentials",
        "./.gem/credentials",
        r#".\.gem\credentials"#,
        "../.gem/credentials",
        r#"..\.gem\credentials"#,
        ".git-credentials",
        "./.git-credentials",
        r#".\.git-credentials"#,
        "../.git-credentials",
        r#"..\.git-credentials"#,
    ];

    for prefix in SENSITIVE_RELATIVE_PATH_PREFIXES {
        if remaining
            .get(..prefix.len())
            .is_some_and(|head| head.eq_ignore_ascii_case(prefix))
        {
            return Some(prefix.len());
        }
    }
    None
}

fn starts_with_forward_slash_network_path(input: &str, cursor: usize) -> bool {
    let remaining = &input[cursor..];
    if !remaining.starts_with("//")
        || input
            .as_bytes()
            .get(cursor.saturating_sub(1))
            .is_some_and(|byte| *byte == b':')
        || input
            .as_bytes()
            .get(cursor.saturating_sub(2)..cursor)
            .is_some_and(|prefix| prefix == b":/")
    {
        return false;
    }

    let bytes = remaining.as_bytes();
    if !bytes
        .get(2)
        .is_some_and(|byte| is_network_path_component_byte(*byte))
    {
        return false;
    }
    bytes[2..]
        .iter()
        .position(|byte| matches!(byte, b'/' | b'\\'))
        .is_some_and(|offset| offset > 0 && bytes.get(2 + offset + 1).is_some())
}

fn is_network_path_component_byte(byte: u8) -> bool {
    !matches!(
        byte,
        b'\0'
            | b'\t'
            | b'\n'
            | b'\r'
            | b' '
            | b'/'
            | b'\\'
            | b'"'
            | b'\''
            | b'`'
            | b'<'
            | b'>'
            | b')'
            | b']'
            | b'}'
            | b','
            | b';'
            | b'|'
            | b'?'
            | b'#'
    )
}

fn starts_with_ascii_case_insensitive(value: &str, prefix: &str) -> bool {
    value
        .get(..prefix.len())
        .is_some_and(|head| head.eq_ignore_ascii_case(prefix))
}

fn is_search_path_hard_delimiter(character: char) -> bool {
    matches!(
        character,
        '"' | '\'' | '`' | '<' | '>' | ')' | ']' | '}' | ',' | ';' | '|' | '?' | '#'
    )
}

fn whitespace_starts_search_path_continuation(input: &str, cursor: usize) -> bool {
    let mut offset = cursor;
    let mut consumed_horizontal_space = false;
    while offset < input.len() {
        let next = input[offset..].chars().next().unwrap_or('\0');
        if !is_horizontal_search_path_space(next) {
            break;
        }
        consumed_horizontal_space = true;
        offset += next.len_utf8();
    }
    if !consumed_horizontal_space {
        return false;
    }

    let mut saw_component_character = false;
    while offset < input.len() {
        let next = input[offset..].chars().next().unwrap_or('\0');
        if next.is_whitespace() || is_search_path_hard_delimiter(next) {
            break;
        }
        if next == '=' {
            return false;
        }
        saw_component_character = true;
        offset += next.len_utf8();
    }

    saw_component_character
}

fn is_horizontal_search_path_space(character: char) -> bool {
    matches!(character, ' ' | '\t')
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

/// Builder for converting curation candidates to canonical search documents.
///
/// Candidate clustering uses this projection before embedding so the science
/// path sees the same stable text that search indexing would consume.
pub struct CurationCandidateDocumentBuilder {
    workspace_path: Option<String>,
    target_memory_content: Option<String>,
}

impl CurationCandidateDocumentBuilder {
    /// Create a new builder with no workspace or target-memory context.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            workspace_path: None,
            target_memory_content: None,
        }
    }

    /// Set workspace path metadata for the document.
    #[must_use]
    pub fn with_workspace_path(mut self, path: impl Into<String>) -> Self {
        self.workspace_path = Some(path.into());
        self
    }

    /// Include target memory content in the clustering text.
    #[must_use]
    pub fn with_target_memory_content(mut self, content: impl Into<String>) -> Self {
        self.target_memory_content = Some(content.into());
        self
    }

    /// Build a canonical search document from a curation candidate row.
    #[must_use]
    pub fn build(self, candidate: &crate::db::StoredCurationCandidate) -> CanonicalSearchDocument {
        let safe_source_id = candidate
            .source_id
            .as_deref()
            .map(redact_curation_candidate_search_ref);
        let content = crate::curate::candidate_embedding_text(
            &crate::curate::CurationCandidateEmbeddingText {
                id: &candidate.id,
                candidate_type: &candidate.candidate_type,
                target_memory_id: candidate.target_memory_id.as_deref().unwrap_or(""),
                target_memory_content: self.target_memory_content.as_deref(),
                proposed_content: candidate.proposed_content.as_deref(),
                proposed_confidence: candidate.proposed_confidence,
                proposed_trust_class: candidate.proposed_trust_class.as_deref(),
                source_type: &candidate.source_type,
                source_id: safe_source_id.as_deref(),
                reason: &candidate.reason,
                confidence: candidate.confidence,
                status: &candidate.status,
                review_state: &candidate.review_state,
            },
        );

        let mut doc =
            CanonicalSearchDocument::new(&candidate.id, content, DocumentSource::CurationCandidate)
                .with_title(format!("Curation candidate {}", candidate.id))
                .with_kind(&candidate.candidate_type)
                .with_created_at(&candidate.created_at)
                .with_metadata_entry("workspace_id", &candidate.workspace_id)
                .with_metadata_entry("candidate_type", &candidate.candidate_type)
                .with_metadata_entry("source_type", &candidate.source_type)
                .with_metadata_entry("confidence", format!("{:.3}", candidate.confidence))
                .with_metadata_entry("status", &candidate.status)
                .with_metadata_entry("review_state", &candidate.review_state);
        if let Some(target_memory_id) = candidate.target_memory_id.as_deref() {
            doc = doc.with_metadata_entry("target_memory_id", target_memory_id);
        }

        if let Some(workspace) = self.workspace_path {
            doc = doc.with_workspace(workspace);
        }
        if let Some(source_id) = &safe_source_id {
            doc = doc.with_metadata_entry("source_id", source_id);
        }
        if let Some(proposed_confidence) = candidate.proposed_confidence {
            doc =
                doc.with_metadata_entry("proposed_confidence", format!("{proposed_confidence:.3}"));
        }
        if let Some(proposed_trust_class) = &candidate.proposed_trust_class {
            doc = doc.with_metadata_entry("proposed_trust_class", proposed_trust_class);
        }
        if let Some(ttl_policy_id) = &candidate.ttl_policy_id {
            doc = doc.with_metadata_entry("ttl_policy_id", ttl_policy_id);
        }

        doc
    }
}

impl Default for CurationCandidateDocumentBuilder {
    fn default() -> Self {
        Self::new()
    }
}

/// Deterministic search embedding for one curation candidate.
#[derive(Clone, Debug, PartialEq)]
pub struct CurationCandidateEmbedding {
    pub candidate_id: String,
    pub embedding: Vec<f32>,
}

/// Convert a curation candidate directly to a canonical search document.
#[must_use]
pub fn curation_candidate_to_document(
    candidate: &crate::db::StoredCurationCandidate,
) -> CanonicalSearchDocument {
    CurationCandidateDocumentBuilder::new().build(candidate)
}

/// Embed a curation candidate using the search module's deterministic fallback.
#[must_use]
pub fn curation_candidate_embedding(
    candidate: &crate::db::StoredCurationCandidate,
    target_memory: Option<&crate::db::StoredMemory>,
    workspace_path: Option<&str>,
) -> CurationCandidateEmbedding {
    let mut builder = CurationCandidateDocumentBuilder::new();
    if let Some(path) = workspace_path {
        builder = builder.with_workspace_path(path);
    }
    if let Some(memory) = target_memory {
        builder = builder.with_target_memory_content(&memory.content);
    }

    let document = builder.build(candidate);
    let embedder = HashEmbedder::default_256();
    CurationCandidateEmbedding {
        candidate_id: candidate.id.clone(),
        embedding: embedder.embed_sync(document.content()),
    }
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
    SearchCapability::ready(
        SearchCapabilityName::ScoreExplanation,
        SearchSurface::Explanation,
        "Score explanation and deterministic retrieval multipliers are wired.",
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
        let final_score = finite_explanation_score(result.score);
        components.push(SearchScoreComponent::new(
            "primary_score",
            final_score,
            ScoreComponentSource::Structural,
        ));
        push_optional_score_component(
            &mut components,
            "lexical_score",
            result.lexical_score,
            ScoreComponentSource::Lexical,
        );
        push_optional_score_component(
            &mut components,
            "semantic_fast_score",
            result.fast_score,
            ScoreComponentSource::Semantic,
        );
        push_optional_score_component(
            &mut components,
            "semantic_quality_score",
            result.quality_score,
            ScoreComponentSource::Semantic,
        );
        push_optional_score_component(
            &mut components,
            "rerank_score",
            result.rerank_score,
            ScoreComponentSource::Structural,
        );

        Self {
            doc_id: result.doc_id.to_string(),
            source: score_source_name(result.source),
            final_score,
            components,
            frankensearch_explanation_available: result.explanation.is_some(),
            metadata_available: result.metadata.is_some(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SearchScoreComponent {
    pub name: &'static str,
    pub source: &'static str,
    pub value: f32,
}

impl SearchScoreComponent {
    #[must_use]
    pub const fn new(name: &'static str, value: f32, source: ScoreComponentSource) -> Self {
        Self {
            name,
            source: source.as_str(),
            value,
        }
    }
}

/// Stable source tags for individual score components.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ScoreComponentSource {
    Lexical,
    Semantic,
    Freshness,
    Structural,
}

impl ScoreComponentSource {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Lexical => "lexical",
            Self::Semantic => "semantic",
            Self::Freshness => "freshness",
            Self::Structural => "structural",
        }
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
    source: ScoreComponentSource,
) {
    if let Some(value) = value {
        components.push(SearchScoreComponent::new(
            name,
            finite_explanation_score(value),
            source,
        ));
    }
}

fn finite_explanation_score(value: f32) -> f32 {
    if value.is_finite() {
        value.max(0.0)
    } else {
        0.0
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
    pub fn hash_256() -> Self {
        Self::new("hash-256", 256, true)
    }

    /// Stable 256-bit content hash over the embedding configuration fields.
    #[must_use]
    pub fn content_hash(&self) -> String {
        compute_embedding_config_hash(self)
    }
}

impl Default for EmbeddingConfig {
    fn default() -> Self {
        Self::hash_256()
    }
}

const EMBEDDING_CONFIG_HASH_DOMAIN: &[u8] = b"ee.search.embedding_config.v1";

fn compute_embedding_config_hash(config: &EmbeddingConfig) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(EMBEDDING_CONFIG_HASH_DOMAIN);
    hash_embedding_config_str_field(&mut hasher, "model_id", &config.model_id);
    hash_embedding_config_usize_field(&mut hasher, "dimension", config.dimension);
    hash_embedding_config_bool_field(&mut hasher, "deterministic", config.deterministic);
    format!("blake3:{}", hasher.finalize().to_hex())
}

fn hash_embedding_config_str_field(hasher: &mut blake3::Hasher, field: &str, value: &str) {
    hash_embedding_config_str(hasher, field);
    hash_embedding_config_str(hasher, value);
}

fn hash_embedding_config_usize_field(hasher: &mut blake3::Hasher, field: &str, value: usize) {
    hash_embedding_config_str(hasher, field);
    let value = u64::try_from(value).unwrap_or(u64::MAX);
    hasher.update(&value.to_le_bytes());
}

fn hash_embedding_config_bool_field(hasher: &mut blake3::Hasher, field: &str, value: bool) {
    hash_embedding_config_str(hasher, field);
    hasher.update(&[u8::from(value)]);
}

fn hash_embedding_config_str(hasher: &mut blake3::Hasher, value: &str) {
    let len = u64::try_from(value.len()).unwrap_or(u64::MAX);
    hasher.update(&len.to_le_bytes());
    hasher.update(value.as_bytes());
}

/// Mesh search-surrogate shape pinned by `ee.mesh.surrogate.v1`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SearchSurrogateType {
    Embedding,
    Summary,
    Minhash,
    LexicalMetadata,
    QueryFingerprint,
}

impl SearchSurrogateType {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Embedding => "embedding",
            Self::Summary => "summary",
            Self::Minhash => "minhash",
            Self::LexicalMetadata => "lexical_metadata",
            Self::QueryFingerprint => "query_fingerprint",
        }
    }
}

/// Structured degraded codes for mesh surrogate audit decisions.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SearchSurrogateDegradedCode {
    Denied,
    Incompatible,
    Recomputed,
    LexicalFallbackUsed,
}

impl SearchSurrogateDegradedCode {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Denied => "surrogate_denied",
            Self::Incompatible => "surrogate_incompatible",
            Self::Recomputed => "surrogate_recomputed",
            Self::LexicalFallbackUsed => "lexical_fallback_used",
        }
    }
}

/// Model and feature fingerprint used to decide whether a remote surrogate can
/// be indexed without local recomputation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SearchSurrogateModelFingerprint {
    pub model_id: String,
    pub model_version: String,
    pub feature_flags: Vec<String>,
}

impl SearchSurrogateModelFingerprint {
    #[must_use]
    pub fn new(
        model_id: impl Into<String>,
        model_version: impl Into<String>,
        feature_flags: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        let mut feature_flags: Vec<String> = feature_flags.into_iter().map(Into::into).collect();
        feature_flags.sort();
        feature_flags.dedup();
        Self {
            model_id: model_id.into(),
            model_version: model_version.into(),
            feature_flags,
        }
    }

    #[must_use]
    pub fn from_embedding_config(
        embedding: &EmbeddingConfig,
        model_version: impl Into<String>,
        feature_flags: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        Self::new(embedding.model_id.clone(), model_version, feature_flags)
    }

    #[must_use]
    pub fn is_compatible_with(&self, local: &Self) -> bool {
        self == local
    }
}

/// Privacy and rebuild policy attached to a mesh search surrogate.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SearchSurrogatePolicy {
    pub export_allowed: bool,
    pub requires_local_recompute: bool,
    pub requires_compatibility_check: bool,
    pub lexical_fallback: bool,
}

impl SearchSurrogatePolicy {
    #[must_use]
    pub const fn metadata_only_for(surrogate_type: SearchSurrogateType) -> Self {
        match surrogate_type {
            SearchSurrogateType::LexicalMetadata => Self {
                export_allowed: true,
                requires_local_recompute: false,
                requires_compatibility_check: true,
                lexical_fallback: true,
            },
            SearchSurrogateType::Embedding
            | SearchSurrogateType::Summary
            | SearchSurrogateType::Minhash
            | SearchSurrogateType::QueryFingerprint => Self {
                export_allowed: false,
                requires_local_recompute: true,
                requires_compatibility_check: true,
                lexical_fallback: true,
            },
        }
    }

    #[must_use]
    pub const fn allow_reuse_after_compatibility_check() -> Self {
        Self {
            export_allowed: true,
            requires_local_recompute: false,
            requires_compatibility_check: true,
            lexical_fallback: true,
        }
    }

    #[must_use]
    pub const fn requires_local_recompute() -> Self {
        Self {
            export_allowed: true,
            requires_local_recompute: true,
            requires_compatibility_check: true,
            lexical_fallback: true,
        }
    }
}

impl Default for SearchSurrogatePolicy {
    fn default() -> Self {
        Self {
            export_allowed: false,
            requires_local_recompute: true,
            requires_compatibility_check: true,
            lexical_fallback: true,
        }
    }
}

/// Metadata needed to audit one incoming mesh search surrogate.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SearchSurrogateDescriptor {
    pub surrogate_type: SearchSurrogateType,
    pub model_fingerprint: SearchSurrogateModelFingerprint,
    pub content_hash: String,
    pub valid_until: Option<String>,
}

/// Search-side decision for an incoming mesh surrogate.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SearchSurrogateAuditDecision {
    ReuseRemote,
    RecomputeLocal,
    LexicalFallback,
    Denied,
}

impl SearchSurrogateAuditDecision {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ReuseRemote => "reuse_remote",
            Self::RecomputeLocal => "recompute_local",
            Self::LexicalFallback => "lexical_fallback",
            Self::Denied => "denied",
        }
    }
}

/// Input to the deterministic search-surrogate audit.
#[derive(Clone, Debug)]
pub struct SearchSurrogateAuditInput<'a> {
    pub surrogate: &'a SearchSurrogateDescriptor,
    pub policy: &'a SearchSurrogatePolicy,
    pub local_model: &'a SearchSurrogateModelFingerprint,
    pub local_content_hash: Option<&'a str>,
    pub observed_at: &'a str,
    pub local_body_available: bool,
}

/// Outcome of a mesh search-surrogate audit. The JSON is deliberately limited
/// to hashes, types, model fingerprints, and stable degraded codes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SearchSurrogateAuditOutcome {
    pub decision: SearchSurrogateAuditDecision,
    pub degraded_codes: Vec<SearchSurrogateDegradedCode>,
}

impl SearchSurrogateAuditOutcome {
    #[must_use]
    pub fn data_json(&self, input: &SearchSurrogateAuditInput<'_>) -> serde_json::Value {
        serde_json::json!({
            "schema": "ee.mesh.surrogate_audit.v1",
            "surrogateType": input.surrogate.surrogate_type.as_str(),
            "decision": self.decision.as_str(),
            "degradedCodes": self
                .degraded_codes
                .iter()
                .map(|code| code.as_str())
                .collect::<Vec<_>>(),
            "modelFingerprint": {
                "modelId": input.surrogate.model_fingerprint.model_id,
                "modelVersion": input.surrogate.model_fingerprint.model_version,
                "featureFlags": input.surrogate.model_fingerprint.feature_flags,
            },
            "contentHash": input.surrogate.content_hash,
            "localContentHashMatched": input
                .local_content_hash
                .is_some_and(|hash| hash == input.surrogate.content_hash),
        })
    }
}

/// Audit whether a mesh search surrogate may be reused, must be recomputed, or
/// should fall back to lexical metadata.
#[must_use]
pub fn audit_search_surrogate(
    input: &SearchSurrogateAuditInput<'_>,
) -> SearchSurrogateAuditOutcome {
    if !input.policy.export_allowed {
        return finish_denied_or_fallback(vec![SearchSurrogateDegradedCode::Denied], input);
    }

    let mut degraded_codes = Vec::new();
    if input.policy.requires_compatibility_check
        && !input
            .surrogate
            .model_fingerprint
            .is_compatible_with(input.local_model)
    {
        degraded_codes.push(SearchSurrogateDegradedCode::Incompatible);
        return finish_recompute_or_fallback(degraded_codes, input);
    }

    let content_hash_matches = input
        .local_content_hash
        .is_some_and(|hash| hash == input.surrogate.content_hash);
    let surrogate_expired = input
        .surrogate
        .valid_until
        .as_deref()
        .is_some_and(|valid_until| surrogate_valid_until_expired(valid_until, input.observed_at));

    if input.policy.requires_local_recompute || !content_hash_matches || surrogate_expired {
        return finish_recompute_or_fallback(degraded_codes, input);
    }

    SearchSurrogateAuditOutcome {
        decision: SearchSurrogateAuditDecision::ReuseRemote,
        degraded_codes,
    }
}

fn finish_denied_or_fallback(
    mut degraded_codes: Vec<SearchSurrogateDegradedCode>,
    input: &SearchSurrogateAuditInput<'_>,
) -> SearchSurrogateAuditOutcome {
    if input.policy.lexical_fallback {
        degraded_codes.push(SearchSurrogateDegradedCode::LexicalFallbackUsed);
        SearchSurrogateAuditOutcome {
            decision: SearchSurrogateAuditDecision::LexicalFallback,
            degraded_codes,
        }
    } else {
        SearchSurrogateAuditOutcome {
            decision: SearchSurrogateAuditDecision::Denied,
            degraded_codes,
        }
    }
}

fn finish_recompute_or_fallback(
    mut degraded_codes: Vec<SearchSurrogateDegradedCode>,
    input: &SearchSurrogateAuditInput<'_>,
) -> SearchSurrogateAuditOutcome {
    if input.local_body_available {
        degraded_codes.push(SearchSurrogateDegradedCode::Recomputed);
        SearchSurrogateAuditOutcome {
            decision: SearchSurrogateAuditDecision::RecomputeLocal,
            degraded_codes,
        }
    } else if input.policy.lexical_fallback {
        degraded_codes.push(SearchSurrogateDegradedCode::LexicalFallbackUsed);
        SearchSurrogateAuditOutcome {
            decision: SearchSurrogateAuditDecision::LexicalFallback,
            degraded_codes,
        }
    } else {
        SearchSurrogateAuditOutcome {
            decision: SearchSurrogateAuditDecision::Denied,
            degraded_codes,
        }
    }
}

fn surrogate_valid_until_expired(valid_until: &str, observed_at: &str) -> bool {
    let Ok(valid_until) = chrono::DateTime::parse_from_rfc3339(valid_until) else {
        return true;
    };
    let Ok(observed_at) = chrono::DateTime::parse_from_rfc3339(observed_at) else {
        return true;
    };
    observed_at >= valid_until
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
    EmbeddingDeterministicMismatch { expected: bool, actual: bool },
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
                "content_hash": self.embedding.content_hash(),
            },
        });

        if let Some(value_map) = value.as_object_mut() {
            if let Some(path) = &self.lexical_index_path {
                value_map.insert("lexical_index_path".to_string(), serde_json::json!(path));
            }
            if let Some(path) = &self.vector_index_path {
                value_map.insert("vector_index_path".to_string(), serde_json::json!(path));
            }
        }

        value
    }
}

impl Default for IndexManifest {
    fn default() -> Self {
        Self::new(0, "1970-01-01T00:00:00Z", 0, 0, EmbeddingConfig::default())
    }
}

/// Search-side hotset entry types for derived cache prewarming.
///
/// These entries model the reusable shape of expensive search work without
/// storing raw memory text, query text, or graph payloads.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum SearchHotsetEntryKind {
    Memory,
    QueryShape,
    SearchDocument,
    GraphNeighborhood,
}

impl SearchHotsetEntryKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Memory => "memory",
            Self::QueryShape => "query_shape",
            Self::SearchDocument => "search_document",
            Self::GraphNeighborhood => "graph_neighborhood",
        }
    }
}

/// Redaction-safe search cache hotset entry.
///
/// The `key` is a BLAKE3 digest over stable identifiers or normalized query
/// shape. It is safe to include in JSON reports because raw user content is
/// never stored in the entry.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SearchHotsetEntry {
    pub key: String,
    pub kind: SearchHotsetEntryKind,
    pub generation: u64,
    pub estimated_bytes: usize,
    pub hit_count: u64,
    pub redaction_status: &'static str,
}

impl SearchHotsetEntry {
    #[must_use]
    pub fn memory(memory_id: impl AsRef<str>, generation: u64, hit_count: u64) -> Self {
        Self {
            key: cache_key("search:memory", memory_id.as_ref()),
            kind: SearchHotsetEntryKind::Memory,
            generation,
            estimated_bytes: 96_usize.saturating_add(memory_id.as_ref().len()),
            hit_count,
            redaction_status: "content_not_stored",
        }
    }

    #[must_use]
    pub fn query_shape(query: impl AsRef<str>, generation: u64, hit_count: u64) -> Option<Self> {
        let normalized = normalized_query_shape(query.as_ref())?;
        let token_count = normalized
            .split(' ')
            .filter(|part| !part.is_empty())
            .count();
        Some(Self {
            key: cache_key("search:query_shape", &normalized),
            kind: SearchHotsetEntryKind::QueryShape,
            generation,
            estimated_bytes: 128_usize.saturating_add(token_count.saturating_mul(16)),
            hit_count,
            redaction_status: "content_not_stored",
        })
    }

    #[must_use]
    pub fn search_document(
        document: &CanonicalSearchDocument,
        generation: u64,
        hit_count: u64,
    ) -> Self {
        Self {
            key: cache_key(
                "search:document",
                &format!("{}:{}", document.source().as_str(), document.id()),
            ),
            kind: SearchHotsetEntryKind::SearchDocument,
            generation,
            estimated_bytes: 160_usize
                .saturating_add(document.id().len())
                .saturating_add(document.content().len().min(4096)),
            hit_count,
            redaction_status: "content_not_stored",
        }
    }

    #[must_use]
    pub fn graph_neighborhood(
        root_id: impl AsRef<str>,
        depth: u8,
        generation: u64,
        hit_count: u64,
    ) -> Self {
        Self {
            key: cache_key(
                "search:graph_neighborhood",
                &format!("{}:{depth}", root_id.as_ref()),
            ),
            kind: SearchHotsetEntryKind::GraphNeighborhood,
            generation,
            estimated_bytes: 192_usize
                .saturating_add(root_id.as_ref().len())
                .saturating_add(usize::from(depth).saturating_mul(64)),
            hit_count,
            redaction_status: "content_not_stored",
        }
    }

    #[must_use]
    pub fn is_redaction_safe(&self) -> bool {
        self.redaction_status == "content_not_stored"
    }

    #[must_use]
    pub fn data_json(&self) -> serde_json::Value {
        serde_json::json!({
            "key": self.key,
            "kind": self.kind.as_str(),
            "generation": self.generation,
            "estimatedBytes": self.estimated_bytes,
            "hitCount": self.hit_count,
            "redactionStatus": self.redaction_status,
        })
    }
}

/// Deterministic search hotset assembled from frequent read shapes.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SearchHotset {
    entries: Vec<SearchHotsetEntry>,
}

impl SearchHotset {
    #[must_use]
    pub fn new(entries: impl IntoIterator<Item = SearchHotsetEntry>) -> Self {
        let mut merged: BTreeMap<(SearchHotsetEntryKind, String), SearchHotsetEntry> =
            BTreeMap::new();
        for entry in entries {
            let key = (entry.kind, entry.key.clone());
            merged
                .entry(key)
                .and_modify(|existing| {
                    existing.hit_count = existing.hit_count.saturating_add(entry.hit_count);
                    existing.estimated_bytes = existing.estimated_bytes.max(entry.estimated_bytes);
                    existing.generation = existing.generation.max(entry.generation);
                })
                .or_insert(entry);
        }
        Self {
            entries: merged.into_values().collect(),
        }
    }

    #[must_use]
    pub fn from_queries_and_documents<'a, Q, D>(queries: Q, documents: D, generation: u64) -> Self
    where
        Q: IntoIterator,
        Q::Item: AsRef<str>,
        D: IntoIterator<Item = &'a CanonicalSearchDocument>,
    {
        let mut entries = Vec::new();
        for query in queries {
            if let Some(entry) = SearchHotsetEntry::query_shape(query, generation, 1) {
                entries.push(entry);
            }
        }
        for document in documents {
            entries.push(SearchHotsetEntry::search_document(document, generation, 1));
        }
        Self::new(entries)
    }

    #[must_use]
    pub fn entries(&self) -> &[SearchHotsetEntry] {
        &self.entries
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    #[must_use]
    pub fn total_estimated_bytes(&self) -> usize {
        self.entries.iter().fold(0usize, |total, entry| {
            total.saturating_add(entry.estimated_bytes)
        })
    }

    #[must_use]
    pub fn total_hit_count(&self) -> u64 {
        self.entries
            .iter()
            .fold(0u64, |total, entry| total.saturating_add(entry.hit_count))
    }
}

/// Cache-governor status for search hotset prewarming.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SearchCacheStatus {
    Warm,
    StaleGeneration,
    PressureFallback,
    Bypassed,
}

impl SearchCacheStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Warm => "warm",
            Self::StaleGeneration => "stale_generation",
            Self::PressureFallback => "pressure_fallback",
            Self::Bypassed => "bypassed",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SearchCacheGovernor {
    pub budget: CacheBudget,
    pub current_generation: u64,
    pub current_entries: usize,
    pub current_bytes: usize,
}

impl SearchCacheGovernor {
    #[must_use]
    pub fn new(current_generation: u64, budget: CacheBudget) -> Self {
        Self {
            budget,
            current_generation,
            current_entries: 0,
            current_bytes: 0,
        }
    }

    #[must_use]
    pub const fn with_current_usage(mut self, entries: usize, bytes: usize) -> Self {
        self.current_entries = entries;
        self.current_bytes = bytes;
        self
    }

    #[must_use]
    pub fn pressure(self) -> MemoryPressure {
        max_pressure(
            assess_pressure(self.current_entries, &self.budget),
            byte_pressure(self.current_bytes, &self.budget),
        )
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct SearchCachePrewarmEvidence {
    pub operations: usize,
    pub requested_entries: usize,
    pub admitted_entries: usize,
    pub rejected_entries: usize,
    pub requested_bytes: usize,
    pub admitted_bytes: usize,
    pub rejected_bytes: usize,
    pub requested_hit_count: u64,
    pub admitted_hit_count: u64,
    pub rejected_hit_count: u64,
    pub hit_coverage_ratio: f64,
    pub byte_coverage_ratio: f64,
}

impl SearchCachePrewarmEvidence {
    #[must_use]
    pub fn from_prewarm_entries(
        requested: &[SearchHotsetEntry],
        admitted: &[SearchHotsetEntry],
    ) -> Self {
        let requested_entries = requested.len();
        let admitted_entries = admitted.len();
        let requested_bytes = entries_estimated_bytes(requested);
        let admitted_bytes = entries_estimated_bytes(admitted);
        let requested_hit_count = entries_hit_count(requested);
        let admitted_hit_count = entries_hit_count(admitted);
        let rejected_entries = requested_entries.saturating_sub(admitted_entries);
        let rejected_bytes = requested_bytes.saturating_sub(admitted_bytes);
        let rejected_hit_count = requested_hit_count.saturating_sub(admitted_hit_count);
        let hit_coverage_ratio = if requested_hit_count == 0 {
            0.0
        } else {
            admitted_hit_count as f64 / requested_hit_count as f64
        };
        let byte_coverage_ratio = if requested_bytes == 0 {
            0.0
        } else {
            admitted_bytes as f64 / requested_bytes as f64
        };
        Self {
            operations: requested_entries,
            requested_entries,
            admitted_entries,
            rejected_entries,
            requested_bytes,
            admitted_bytes,
            rejected_bytes,
            requested_hit_count,
            admitted_hit_count,
            rejected_hit_count,
            hit_coverage_ratio,
            byte_coverage_ratio,
        }
    }

    #[must_use]
    pub fn data_json(&self) -> serde_json::Value {
        serde_json::json!({
            "evidenceKind": "search_hotset_admission",
            "operations": self.operations,
            "requestedEntries": self.requested_entries,
            "admittedEntries": self.admitted_entries,
            "rejectedEntries": self.rejected_entries,
            "requestedBytes": self.requested_bytes,
            "admittedBytes": self.admitted_bytes,
            "rejectedBytes": self.rejected_bytes,
            "requestedHitCount": self.requested_hit_count,
            "admittedHitCount": self.admitted_hit_count,
            "rejectedHitCount": self.rejected_hit_count,
            "hitCoverageRatio": rounded_f64(self.hit_coverage_ratio),
            "byteCoverageRatio": rounded_f64(self.byte_coverage_ratio),
        })
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct SearchCachePrewarmReport {
    pub status: SearchCacheStatus,
    pub source_generation: Option<u64>,
    pub current_generation: u64,
    pub requested_entries: usize,
    pub admitted_entries: usize,
    pub rejected_entries: usize,
    pub estimated_bytes: usize,
    pub budget_max_entries: usize,
    pub budget_max_bytes: usize,
    pub memory_pressure: MemoryPressure,
    pub hit_rate: f64,
    pub fallback_reason: Option<&'static str>,
    pub prewarm_evidence: SearchCachePrewarmEvidence,
    pub admitted: Vec<SearchHotsetEntry>,
}

impl SearchCachePrewarmReport {
    #[must_use]
    pub fn data_json(&self) -> serde_json::Value {
        serde_json::json!({
            "schema": "ee.search.cache_prewarm.v1",
            "status": self.status.as_str(),
            "sourceGeneration": self.source_generation,
            "currentGeneration": self.current_generation,
            "requestedEntries": self.requested_entries,
            "admittedEntries": self.admitted_entries,
            "rejectedEntries": self.rejected_entries,
            "estimatedBytes": self.estimated_bytes,
            "budget": {
                "maxEntries": self.budget_max_entries,
                "maxBytes": self.budget_max_bytes,
            },
            "memoryPressure": self.memory_pressure.as_str(),
            "hitRate": rounded_f64(self.hit_rate),
            "fallbackReason": self.fallback_reason,
            "prewarmEvidence": self.prewarm_evidence.data_json(),
            "admitted": self.admitted.iter().map(SearchHotsetEntry::data_json).collect::<Vec<_>>(),
        })
    }
}

#[must_use]
pub fn prewarm_search_hotset(
    hotset: &SearchHotset,
    governor: SearchCacheGovernor,
) -> SearchCachePrewarmReport {
    let source_generation = hotset.entries().first().map(|entry| entry.generation);
    let requested_entries = hotset.len();
    let pressure = governor.pressure();

    let stale_generation = hotset
        .entries()
        .iter()
        .any(|entry| entry.generation != governor.current_generation);
    if stale_generation {
        return search_cache_report(
            SearchCacheStatus::StaleGeneration,
            source_generation,
            governor,
            hotset.entries(),
            Vec::new(),
            Some("generation_mismatch"),
        );
    }

    if pressure == MemoryPressure::Critical {
        return search_cache_report(
            SearchCacheStatus::Bypassed,
            source_generation,
            governor,
            hotset.entries(),
            Vec::new(),
            Some("memory_pressure_critical"),
        );
    }

    let mut admitted = Vec::new();
    let mut projected_entries = governor.current_entries;
    let mut projected_bytes = governor.current_bytes;
    for entry in hotset.entries() {
        let next_entries = projected_entries.saturating_add(1);
        let next_bytes = projected_bytes.saturating_add(entry.estimated_bytes);
        if next_entries > governor.budget.max_entries || next_bytes > governor.budget.max_bytes {
            continue;
        }
        if entry.is_redaction_safe() {
            projected_entries = next_entries;
            projected_bytes = next_bytes;
            admitted.push(entry.clone());
        }
    }

    let status = if admitted.len() == requested_entries {
        SearchCacheStatus::Warm
    } else {
        SearchCacheStatus::PressureFallback
    };
    let fallback_reason = if status == SearchCacheStatus::PressureFallback {
        Some("budget_trimmed")
    } else {
        None
    };
    search_cache_report(
        status,
        source_generation,
        governor,
        hotset.entries(),
        admitted,
        fallback_reason,
    )
}

fn search_cache_report(
    status: SearchCacheStatus,
    source_generation: Option<u64>,
    governor: SearchCacheGovernor,
    requested: &[SearchHotsetEntry],
    admitted: Vec<SearchHotsetEntry>,
    fallback_reason: Option<&'static str>,
) -> SearchCachePrewarmReport {
    let requested_entries = requested.len();
    let total_hit_count = entries_hit_count(requested);
    let admitted_hit_count = entries_hit_count(&admitted);
    let hit_rate = if total_hit_count == 0 {
        0.0
    } else {
        admitted_hit_count as f64 / total_hit_count as f64
    };
    let admitted_entries = admitted.len();
    SearchCachePrewarmReport {
        status,
        source_generation,
        current_generation: governor.current_generation,
        requested_entries,
        admitted_entries,
        rejected_entries: requested_entries.saturating_sub(admitted_entries),
        estimated_bytes: admitted.iter().fold(0usize, |total, entry| {
            total.saturating_add(entry.estimated_bytes)
        }),
        budget_max_entries: governor.budget.max_entries,
        budget_max_bytes: governor.budget.max_bytes,
        memory_pressure: governor.pressure(),
        hit_rate,
        fallback_reason,
        prewarm_evidence: SearchCachePrewarmEvidence::from_prewarm_entries(requested, &admitted),
        admitted,
    }
}

fn entries_estimated_bytes(entries: &[SearchHotsetEntry]) -> usize {
    entries.iter().fold(0usize, |total, entry| {
        total.saturating_add(entry.estimated_bytes)
    })
}

fn entries_hit_count(entries: &[SearchHotsetEntry]) -> u64 {
    entries
        .iter()
        .fold(0u64, |total, entry| total.saturating_add(entry.hit_count))
}

fn normalized_query_shape(query: &str) -> Option<String> {
    let mut terms: Vec<String> = query
        .split(|character: char| !character.is_alphanumeric() && character != '_')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .map(str::to_ascii_lowercase)
        .collect();
    if terms.is_empty() {
        return None;
    }
    terms.sort();
    terms.dedup();
    Some(terms.join(" "))
}

fn cache_key(namespace: &str, payload: &str) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(namespace.as_bytes());
    hasher.update(&[0]);
    hasher.update(payload.as_bytes());
    format!("blake3:{}", hasher.finalize().to_hex())
}

fn byte_pressure(current_bytes: usize, budget: &CacheBudget) -> MemoryPressure {
    if budget.max_bytes == 0
        || current_bytes >= watermark_bytes(budget.max_bytes, budget.critical_watermark)
    {
        MemoryPressure::Critical
    } else if current_bytes >= watermark_bytes(budget.max_bytes, budget.high_watermark) {
        MemoryPressure::High
    } else {
        MemoryPressure::Normal
    }
}

fn watermark_bytes(max_bytes: usize, watermark: f64) -> usize {
    ((max_bytes as f64) * watermark).floor() as usize
}

const fn max_pressure(left: MemoryPressure, right: MemoryPressure) -> MemoryPressure {
    if left as u8 >= right as u8 {
        left
    } else {
        right
    }
}

fn rounded_f64(value: f64) -> f64 {
    (value * 10_000.0).round() / 10_000.0
}

#[cfg(test)]
// Search unit tests use expect for static fixture construction and JSON assertions.
#[allow(clippy::expect_used)]
mod tests {
    use super::{
        CanonicalSearchDocument, DocumentSource, Embedder, HashEmbedder,
        MEMORY_ANCHOR_COUNT_METADATA_KEY, MEMORY_ANCHOR_FRESHNESS_METADATA_KEY,
        MEMORY_ANCHOR_HASHES_METADATA_KEY, MEMORY_ANCHOR_KINDS_METADATA_KEY,
        MEMORY_ANCHOR_REDACTED_VALUES_METADATA_KEY, MEMORY_ANCHOR_SCHEMA_METADATA_KEY,
        REQUIRED_RETRIEVAL_ENGINE, RuleIndexProjection, RuleScopePatternError,
        ScoreComponentSource, ScoreSource, ScoredResult, SearchCacheGovernor, SearchCacheStatus,
        SearchCapabilityName, SearchHotset, SearchHotsetEntry, SearchHotsetEntryKind,
        SearchSurface, explain_scored_result, module_readiness, normalize_rule_scope_pattern,
        prewarm_search_hotset, rule_to_document, score_source_name, subsystem_name,
    };
    use crate::cache::{CacheBudget, MemoryPressure};
    use crate::db::StoredProceduralRule;
    use crate::models::{
        CapabilityStatus, MEMORY_ANCHOR_SCHEMA_V1, MemoryAnchorFreshnessState, MemoryAnchorKind,
        MemoryAnchorSource, RuleScope, StoredMemoryAnchor,
    };
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
    fn readiness_reports_ready_when_search_contract_is_wired() {
        let readiness = module_readiness();

        assert_eq!(readiness.status(), CapabilityStatus::Ready);
        assert_eq!(
            readiness
                .capabilities()
                .first()
                .map(|capability| capability.status()),
            Some(CapabilityStatus::Ready)
        );
        assert_eq!(readiness.missing_capabilities().count(), 0);
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
    fn score_explanation_capability_reports_repair_metadata() {
        let missing: Vec<_> = module_readiness().missing_capabilities().collect();

        assert!(missing.is_empty());
        let readiness = module_readiness();
        let capability = readiness
            .capabilities()
            .iter()
            .find(|capability| capability.name() == SearchCapabilityName::ScoreExplanation)
            .copied();
        assert_eq!(
            capability.map(|capability| capability.surface()),
            Some(SearchSurface::Explanation)
        );
        assert_eq!(
            capability.map(|capability| capability.status()),
            Some(CapabilityStatus::Ready)
        );
        assert_eq!(
            capability.map(|capability| capability.repair().contains("Score explanation")),
            Some(true)
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
    fn score_component_source_tags_are_stable() {
        assert_eq!(ScoreComponentSource::Lexical.as_str(), "lexical");
        assert_eq!(ScoreComponentSource::Semantic.as_str(), "semantic");
        assert_eq!(ScoreComponentSource::Freshness.as_str(), "freshness");
        assert_eq!(ScoreComponentSource::Structural.as_str(), "structural");
    }

    #[test]
    fn scored_result_explanation_preserves_components_in_stable_order() {
        let result = ScoredResult {
            doc_id: "mem-release-rule".into(),
            score: 0.875,
            source: ScoreSource::Hybrid,
            index: Some(17),
            fast_score: Some(0.82),
            quality_score: None,
            lexical_score: Some(3.5),
            rerank_score: Some(0.91),
            explanation: None,
            metadata: Some(std::sync::Arc::new(json!({
                "source": "memory",
                "schema": super::CANONICAL_DOCUMENT_SCHEMA,
            }))),
        };

        let explanation = explain_scored_result(&result);
        let components: Vec<(&str, &str, String)> = explanation
            .components
            .iter()
            .map(|component| {
                (
                    component.name,
                    component.source,
                    format!("{:.3}", component.value),
                )
            })
            .collect();

        assert_eq!(explanation.doc_id, "mem-release-rule");
        assert_eq!(explanation.source, "hybrid");
        assert_eq!(format!("{:.3}", explanation.final_score), "0.875");
        assert_eq!(
            components,
            vec![
                ("primary_score", "structural", "0.875".to_owned()),
                ("lexical_score", "lexical", "3.500".to_owned()),
                ("semantic_fast_score", "semantic", "0.820".to_owned()),
                ("rerank_score", "structural", "0.910".to_owned()),
            ]
        );
        assert!(!explanation.frankensearch_explanation_available);
        assert!(explanation.metadata_available);
    }

    #[test]
    fn scored_result_explanation_tags_semantic_quality_component_source() {
        let result = ScoredResult {
            doc_id: "mem-quality-rule".into(),
            score: 0.93,
            source: ScoreSource::SemanticQuality,
            index: Some(3),
            fast_score: None,
            quality_score: Some(0.93),
            lexical_score: None,
            rerank_score: None,
            explanation: None,
            metadata: None,
        };

        let explanation = explain_scored_result(&result);
        assert_eq!(
            explanation
                .components
                .iter()
                .find(|component| component.name == "semantic_quality_score")
                .map(|component| component.source),
            Some("semantic")
        );
    }

    #[test]
    fn scored_result_explanation_omits_absent_optional_scores() {
        let result = ScoredResult {
            doc_id: "mem-lexical-only".into(),
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
        let component_sources: Vec<&str> = explanation
            .components
            .iter()
            .map(|component| component.source)
            .collect();

        assert_eq!(explanation.source, "lexical");
        assert_eq!(component_names, vec!["primary_score"]);
        assert_eq!(component_sources, vec!["structural"]);
        assert!(!explanation.frankensearch_explanation_available);
        assert!(!explanation.metadata_available);
    }

    #[test]
    fn scored_result_explanation_normalizes_non_finite_scores() {
        let result = ScoredResult {
            doc_id: "mem-non-finite-score".into(),
            score: f32::NAN,
            source: ScoreSource::Hybrid,
            index: None,
            fast_score: Some(f32::INFINITY),
            quality_score: Some(-1.0),
            lexical_score: Some(f32::NEG_INFINITY),
            rerank_score: Some(0.25),
            explanation: None,
            metadata: None,
        };

        let explanation = explain_scored_result(&result);
        let components: Vec<(&str, f32)> = explanation
            .components
            .iter()
            .map(|component| (component.name, component.value))
            .collect();

        assert_eq!(explanation.final_score, 0.0);
        assert_eq!(
            components,
            vec![
                ("primary_score", 0.0),
                ("lexical_score", 0.0),
                ("semantic_fast_score", 0.0),
                ("semantic_quality_score", 0.0),
                ("rerank_score", 0.25),
            ]
        );
        assert!(
            explanation
                .components
                .iter()
                .all(|component| component.value.is_finite() && component.value >= 0.0)
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
        assert_eq!(DocumentSource::Artifact.as_str(), "artifact");
        assert_eq!(
            DocumentSource::CurationCandidate.as_str(),
            "curation_candidate"
        );
    }

    fn stored_rule_fixture() -> StoredProceduralRule {
        StoredProceduralRule {
            id: "rule_01234567890123456789012345".to_owned(),
            workspace_id: "wsp_01234567890123456789012345".to_owned(),
            content: "Run the exact release verifier before publishing.".to_owned(),
            confidence: 0.812_345,
            utility: 0.623_456,
            importance: 0.734_567,
            trust_class: "human_explicit".to_owned(),
            scope: "workspace".to_owned(),
            scope_pattern: None,
            maturity: "candidate".to_owned(),
            protected: false,
            positive_feedback_count: 7,
            negative_feedback_count: 2,
            validation_passes: 3,
            validation_contradictions: 1,
            last_applied_at: Some("2026-07-27T10:00:00Z".to_owned()),
            last_validated_at: Some("2026-07-27T11:00:00Z".to_owned()),
            superseded_by: None,
            created_at: "2026-07-27T09:00:00Z".to_owned(),
            updated_at: "2026-07-27T11:00:00Z".to_owned(),
            tombstoned_at: None,
        }
    }

    #[test]
    fn rule_projection_preserves_exact_metadata_and_revision_inputs() {
        let workspace = tempfile::tempdir().expect("workspace tempdir");
        let rule = stored_rule_fixture();
        let projection = RuleIndexProjection::new(
            rule.clone(),
            workspace.path(),
            vec!["zeta".to_owned(), "alpha".to_owned(), "alpha".to_owned()],
            vec![
                "mem_22222222222222222222222222".to_owned(),
                "mem_11111111111111111111111111".to_owned(),
                "mem_11111111111111111111111111".to_owned(),
            ],
        );
        assert!(projection.is_search_indexable());
        assert!(projection.is_pack_admissible());
        assert_eq!(projection.tags(), &["alpha".to_owned(), "zeta".to_owned()]);
        assert_eq!(
            projection.source_memory_ids(),
            &[
                "mem_11111111111111111111111111".to_owned(),
                "mem_22222222222222222222222222".to_owned(),
            ]
        );
        assert_eq!(projection.entity_revision().len(), 71);
        assert!(projection.entity_revision().starts_with("blake3:"));

        let indexable = rule_to_document(&projection).into_indexable();
        assert_eq!(
            indexable.metadata.get("confidence"),
            Some(&rule.confidence.to_string())
        );
        assert_eq!(
            indexable.metadata.get("utility"),
            Some(&rule.utility.to_string())
        );
        assert_eq!(
            indexable.metadata.get("importance"),
            Some(&rule.importance.to_string())
        );
        assert_eq!(
            indexable.metadata.get("tags"),
            Some(&"alpha,zeta".to_owned())
        );
        assert_eq!(
            indexable.metadata.get("source_memory_ids"),
            Some(&"mem_11111111111111111111111111,mem_22222222222222222222222222".to_owned())
        );
        assert_eq!(
            indexable.metadata.get("entity_revision"),
            Some(&projection.entity_revision().to_owned())
        );

        let reordered = RuleIndexProjection::new(
            rule.clone(),
            workspace.path(),
            vec!["alpha".to_owned(), "zeta".to_owned()],
            vec![
                "mem_11111111111111111111111111".to_owned(),
                "mem_22222222222222222222222222".to_owned(),
            ],
        );
        assert_eq!(
            projection.entity_revision(),
            reordered.entity_revision(),
            "junction input order must not change the canonical revision"
        );

        let mut protected_rule = rule;
        protected_rule.protected = true;
        let protected = RuleIndexProjection::new(
            protected_rule,
            workspace.path(),
            reordered.tags().to_vec(),
            reordered.source_memory_ids().to_vec(),
        );
        assert_ne!(
            projection.entity_revision(),
            protected.entity_revision(),
            "an indexed rule field mutation must change the entity revision"
        );
    }

    #[test]
    fn rule_scope_patterns_are_normalized_and_fail_closed() {
        let workspace = tempfile::tempdir().expect("workspace tempdir");
        assert_eq!(
            normalize_rule_scope_pattern(
                workspace.path(),
                RuleScope::FilePattern,
                Some("src//./**/*.rs"),
            ),
            Ok(Some("src/**/*.rs".to_owned()))
        );
        assert_eq!(
            normalize_rule_scope_pattern(
                workspace.path(),
                RuleScope::Directory,
                Some("../outside"),
            ),
            Err(RuleScopePatternError::Traversal)
        );
        assert_eq!(
            normalize_rule_scope_pattern(
                workspace.path(),
                RuleScope::FilePattern,
                Some("/tmp/*.rs"),
            ),
            Err(RuleScopePatternError::Absolute)
        );

        let mut legacy_rule = stored_rule_fixture();
        legacy_rule.scope = "file_pattern".to_owned();
        legacy_rule.scope_pattern = Some("../outside/*.rs".to_owned());
        let projection =
            RuleIndexProjection::new(legacy_rule, workspace.path(), Vec::new(), Vec::new());
        assert!(projection.is_search_indexable());
        assert!(!projection.is_pack_admissible());
        let indexable = rule_to_document(&projection).into_indexable();
        assert_eq!(
            indexable.metadata.get("scope_pattern_posture"),
            Some(&"invalid".to_owned())
        );
        assert_eq!(
            indexable.metadata.get("scope_pattern_error"),
            Some(&"traversal".to_owned())
        );
        assert!(
            !indexable.metadata.contains_key("scope_pattern"),
            "unsafe legacy patterns must not leave the database projection"
        );
    }

    #[cfg(unix)]
    #[test]
    fn rule_scope_pattern_rejects_existing_symlink_escape() {
        let workspace = tempfile::tempdir().expect("workspace tempdir");
        let outside = tempfile::tempdir().expect("outside tempdir");
        std::os::unix::fs::symlink(outside.path(), workspace.path().join("escape"))
            .expect("symlink fixture");
        assert_eq!(
            normalize_rule_scope_pattern(
                workspace.path(),
                RuleScope::FilePattern,
                Some("escape/*.rs"),
            ),
            Err(RuleScopePatternError::SymlinkEscape)
        );
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
            workflow_id: None,
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

    fn make_test_anchor(kind: MemoryAnchorKind, hash: &str, redacted: &str) -> StoredMemoryAnchor {
        StoredMemoryAnchor {
            memory_id: "mem_01234567890123456789012345".to_string(),
            anchor_kind: kind,
            anchor_value_hash: hash.to_string(),
            redacted_anchor_value: redacted.to_string(),
            confidence: 0.95,
            source: MemoryAnchorSource::IndexRebuild,
            provenance: "index_rebuild".to_string(),
            captured_span_hash: "blake3:captured-span".to_string(),
            freshness_state: MemoryAnchorFreshnessState::Current,
            generation: 66,
            created_at: "2026-06-07T16:00:00Z".to_string(),
            updated_at: "2026-06-07T16:00:00Z".to_string(),
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

    fn make_test_evidence_span(excerpt: &str) -> crate::db::StoredEvidenceSpan {
        let content_hash = format!("blake3:{}", blake3::hash(excerpt.as_bytes()).to_hex());
        crate::db::StoredEvidenceSpan {
            id: "ev_01234567890123456789012345".to_owned(),
            workspace_id: "wsp_01234567890123456789012345".to_owned(),
            session_id: "sess_01234567890123456789012345".to_owned(),
            memory_id: Some("mem_01234567890123456789012345".to_owned()),
            cass_span_id: "/Users/alice/private/session.jsonl:42".to_owned(),
            span_kind: "message".to_owned(),
            start_line: 42,
            end_line: 42,
            start_byte: None,
            end_byte: None,
            role: Some("assistant".to_owned()),
            excerpt: excerpt.to_owned(),
            content_hash: content_hash.clone(),
            metadata_json: Some(
                r#"{"sourcePath":"/Users/alice/private/session.jsonl","upstreamId":"raw-42"}"#
                    .to_owned(),
            ),
            producer_kind: "cass_import".to_owned(),
            screening_version: crate::db::EVIDENCE_SCREENING_VERSION,
            secret_redaction_status: "clean".to_owned(),
            redaction_classes_json: "[]".to_owned(),
            instruction_risk: "none".to_owned(),
            search_eligibility: "admitted".to_owned(),
            pack_eligibility: "admitted".to_owned(),
            canonical_provenance_revision: crate::db::EVIDENCE_CANONICAL_PROVENANCE_REVISION,
            canonical_excerpt_hash: Some(content_hash),
            security_policy_epoch: crate::db::EVIDENCE_SECURITY_POLICY_EPOCH,
            upstream_ref_hash: Some(
                "blake3:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
                    .to_owned(),
            ),
            created_at: "2026-07-28T00:00:00Z".to_owned(),
            updated_at: "2026-07-28T00:00:00Z".to_owned(),
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

    fn make_test_candidate() -> crate::db::StoredCurationCandidate {
        crate::db::StoredCurationCandidate {
            id: "curate_01234567890123456789012345".to_string(),
            workspace_id: "wsp_01234567890123456789012345".to_string(),
            candidate_type: "consolidate".to_string(),
            target_memory_id: Some("mem_01234567890123456789012345".to_string()),
            proposed_content: Some(
                "Run cargo fmt --check before release verification.".to_string(),
            ),
            proposed_confidence: Some(0.91),
            proposed_trust_class: Some("validated".to_string()),
            source_type: "science_test".to_string(),
            source_id: Some("eval-run-001".to_string()),
            reason: "Repeated release failures cite missing format checks.".to_string(),
            confidence: 0.84,
            status: "pending".to_string(),
            created_at: "2026-04-29T12:02:00Z".to_string(),
            reviewed_at: None,
            reviewed_by: None,
            applied_at: None,
            ttl_expires_at: None,
            review_state: "new".to_string(),
            snoozed_until: None,
            merged_into_candidate_id: None,
            state_entered_at: Some("2026-04-29T12:02:00Z".to_string()),
            last_action_at: Some("2026-04-29T12:02:00Z".to_string()),
            ttl_policy_id: Some("curation.proposed.default".to_string()),
            derivation_source_refs_json: None,
            derivation_metadata_json: None,
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
        assert_eq!(
            indexable.metadata.get("content"),
            Some(&"Always run cargo fmt before commit.".to_owned())
        );
        assert_eq!(
            indexable.metadata.get("content_truncated"),
            Some(&"false".to_owned())
        );
        assert!(!indexable.metadata.contains_key("contentPreview"));
        assert!(!indexable.metadata.contains_key("workspace"));
    }

    #[test]
    fn memory_document_builder_bounds_content_and_marks_truncation() {
        let mut memory = make_test_memory();
        memory.content = "a".repeat(300);

        let indexable = super::memory_to_document(&memory).into_indexable();

        assert_eq!(
            indexable
                .metadata
                .get("content")
                .map(std::string::String::len),
            Some(243)
        );
        assert!(
            indexable
                .metadata
                .get("content")
                .is_some_and(|content| content.ends_with("..."))
        );
        assert_eq!(
            indexable.metadata.get("content_truncated"),
            Some(&"true".to_owned())
        );
    }

    #[test]
    fn memory_document_builder_indexes_validity_window_metadata() {
        let mut memory = make_test_memory();
        memory.valid_from = Some("2026-05-01T00:00:00Z".to_owned());
        memory.valid_to = Some("2026-05-31T23:59:59Z".to_owned());

        let indexable = super::memory_to_document(&memory).into_indexable();

        assert_eq!(
            indexable.metadata.get("valid_from"),
            Some(&"2026-05-01T00:00:00Z".to_owned())
        );
        assert_eq!(
            indexable.metadata.get("valid_to"),
            Some(&"2026-05-31T23:59:59Z".to_owned())
        );
        assert_eq!(
            indexable.metadata.get("validity_window_kind"),
            Some(&"bounded".to_owned())
        );
    }

    #[test]
    fn memory_document_builder_indexes_registry_typed_field_metadata() {
        let mut memory = make_test_memory();
        memory.kind = "decision".to_owned();
        let indexable = super::MemoryDocumentBuilder::new()
            .with_typed_fields_json(
                r#"{"chosen":"RCH remote","rationale":"avoid local cargo","supersedes":"mem_old","revisit_by":"2026-07-01T12:00:00Z"}"#,
            )
            .build(&memory)
            .into_indexable();

        assert_eq!(
            indexable.metadata.get("typed_field.chosen"),
            Some(&"RCH remote".to_owned())
        );
        assert_eq!(
            indexable.metadata.get("typed_field.supersedes"),
            Some(&"mem_old".to_owned())
        );
        assert!(!indexable.metadata.contains_key("typed_field.rationale"));
        assert!(!indexable.metadata.contains_key("typed_field.revisit_by"));
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
    fn memory_document_builder_attaches_hash_redacted_anchor_metadata() {
        let memory = make_test_memory();
        let anchors = vec![
            make_test_anchor(
                MemoryAnchorKind::Schema,
                "blake3:schemahash",
                "schema:blake3:schemahash",
            ),
            make_test_anchor(
                MemoryAnchorKind::Path,
                "blake3:pathhash",
                "path:blake3:pathhash",
            ),
        ];

        let indexable = super::memory_to_document_with_context_and_anchors(
            &memory,
            Some("/home/user/project"),
            &[],
            &anchors,
        )
        .into_indexable();

        assert_eq!(
            indexable.metadata.get(MEMORY_ANCHOR_SCHEMA_METADATA_KEY),
            Some(&MEMORY_ANCHOR_SCHEMA_V1.to_owned())
        );
        assert_eq!(
            indexable.metadata.get(MEMORY_ANCHOR_COUNT_METADATA_KEY),
            Some(&"2".to_owned())
        );
        assert_eq!(
            indexable.metadata.get(MEMORY_ANCHOR_KINDS_METADATA_KEY),
            Some(&"path,schema".to_owned())
        );
        assert_eq!(
            indexable.metadata.get(MEMORY_ANCHOR_HASHES_METADATA_KEY),
            Some(&"path:blake3:pathhash,schema:blake3:schemahash".to_owned())
        );
        assert_eq!(
            indexable
                .metadata
                .get(MEMORY_ANCHOR_REDACTED_VALUES_METADATA_KEY),
            Some(&"path:blake3:pathhash,schema:blake3:schemahash".to_owned())
        );
        assert_eq!(
            indexable.metadata.get(MEMORY_ANCHOR_FRESHNESS_METADATA_KEY),
            Some(&"path:blake3:pathhash:current:66,schema:blake3:schemahash:current:66".to_owned())
        );
    }

    #[test]
    fn memory_anchor_metadata_does_not_include_raw_anchor_values() {
        let memory = make_test_memory();
        let anchors = [make_test_anchor(
            MemoryAnchorKind::Path,
            "blake3:srcdbhash",
            "path:blake3:srcdbhash",
        )];

        let indexable =
            super::memory_to_document_with_context_and_anchors(&memory, None, &[], &anchors)
                .into_indexable();
        let metadata_text = indexable
            .metadata
            .values()
            .cloned()
            .collect::<Vec<_>>()
            .join("\n");

        assert!(!metadata_text.contains("src/db/mod.rs"));
        assert!(!metadata_text.contains("ee.search.v1"));
        assert!(metadata_text.contains("blake3:srcdbhash"));
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
                .contains("CASS session: sess_01234567890123456789012345")
        );
        assert!(doc.content().contains("Messages: 42"));
        assert!(!doc.content().contains("Content hash:"));

        let indexable = doc.into_indexable();
        assert_eq!(
            indexable.title.as_deref(),
            Some("CASS session sess_01234567890123456789012345")
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
            indexable.metadata.get("provenance_uri"),
            Some(&"cass-session://sess_01234567890123456789012345".to_owned())
        );
        assert!(!indexable.metadata.contains_key("cass_session_id"));
        assert_eq!(
            indexable.metadata.get("message_count"),
            Some(&"42".to_owned())
        );
        assert!(!indexable.metadata.contains_key("content_hash"));
        assert!(!indexable.metadata.contains_key("workspace"));
        assert!(!indexable.metadata.contains_key("token_count"));
    }

    #[test]
    fn evidence_document_projection_uses_only_canonical_provenance() {
        let span = make_test_evidence_span("Release verification completed successfully.");
        let doc = super::evidence_span_to_document(&span);
        let content = doc.content().to_owned();
        let indexable = doc.into_indexable();
        let rendered = format!("{content}\n{:?}", indexable.metadata);

        assert!(content.contains("Release verification completed successfully."));
        assert_eq!(
            indexable.metadata.get("provenance_uri"),
            Some(&"cass-session://sess_01234567890123456789012345#L42-42".to_owned())
        );
        assert!(!indexable.metadata.contains_key("cass_span_id"));
        assert!(!indexable.metadata.contains_key("source_path"));
        assert!(!indexable.metadata.contains_key("metadata_json"));
        assert!(!rendered.contains("/Users/alice"));
        assert!(!rendered.contains("raw-42"));
    }

    #[test]
    fn evidence_document_defensive_withhold_has_no_raw_derived_hash() {
        let raw = "api_key=low-entropy-secret";
        let raw_hash = format!("blake3:{}", blake3::hash(raw.as_bytes()).to_hex());
        let span = make_test_evidence_span(raw);
        let doc = super::evidence_span_to_document(&span);
        assert_eq!(doc.content(), "[EVIDENCE_WITHHELD]");
        let indexable = doc.into_indexable();
        let rendered = format!("{:?}", indexable.metadata);

        assert!(!rendered.contains(raw));
        assert!(!rendered.contains(&raw_hash));
        assert!(!indexable.metadata.contains_key("content_hash"));
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
        assert!(!doc.content().contains("Metadata:"));
        assert!(!doc.content().contains("Source path:"));
        assert!(
            !doc.content()
                .contains("/home/user/.cass/sessions/session.jsonl"),
            "session search content leaked raw source path: {}",
            doc.content()
        );

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
        assert!(!indexable.metadata.contains_key("metadata_json"));
        assert!(!indexable.metadata.contains_key("source_path"));
        assert_eq!(
            indexable.metadata.get("tags"),
            Some(&"cass,session".to_owned())
        );
    }

    #[test]
    fn session_document_builder_omits_sensitive_source_path() {
        let mut session = make_test_session();
        session.source_path = Some(
            "file:///Volumes/USBNVME16TB/private/session.jsonl?api_key=redaction-fixture"
                .to_string(),
        );

        let doc = super::session_to_document(&session);
        let content = doc.content().to_string();
        let indexable = doc.into_indexable();
        let rendered = format!("{}\n{:?}", content, indexable.metadata);

        assert!(!rendered.contains("source_path"));
        assert!(
            !rendered.contains("/Volumes/USBNVME16TB/private/session.jsonl"),
            "session search document leaked source path: {rendered}"
        );
        assert!(
            !rendered.contains("redaction-fixture"),
            "session search document leaked secret-like source path: {rendered}"
        );
        assert!(!indexable.metadata.contains_key("source_path"));
    }

    #[test]
    fn search_projection_redacts_malformed_delimited_path_segments() {
        let redacted = super::redact_search_projection_absolute_path_like_segments(
            r#"source=file:///Users/alice/private/session.jsonl]; other=/data/projects/ee); win=C:\Users\alice\secret?next"#,
        );

        assert_eq!(
            redacted,
            r#"source=file://[REDACTED_PATH]]; other=[REDACTED_PATH]); win=[REDACTED_PATH]?next"#
        );
        for leaked in [
            "/Users/alice",
            "private/session.jsonl",
            "/data/projects/ee",
            r#"C:\Users\alice\secret"#,
        ] {
            assert!(
                !redacted.contains(leaked),
                "search projection redaction leaked {leaked}: {redacted}"
            );
        }
    }

    #[test]
    fn search_projection_redacts_mixed_case_macos_private_path_roots() {
        // bd-89312: On case-insensitive macOS filesystems, indexed projection
        // content can carry uppercase or mixed-case private/var/folders
        // roots. `is_case_insensitive_macos_search_path_prefix` widened the
        // case-insensitive list past /Users/ and /Volumes/; this regression
        // test pins every macOS runtime/secret root the redactor MUST treat
        // case-insensitively, plus an ordinary relative reference that must
        // stay visible.
        let redacted = super::redact_search_projection_absolute_path_like_segments(
            r#"users=/USERS/alice/Notes.md volumes=/VOLUMES/Backup/session.jsonl run=/PRIVATE/VAR/RUN/agent.sock log=/Private/Var/Log/system.log tmp=/PRIVATE/VAR/TMP/scratch.txt folders=/PRIVATE/VAR/FOLDERS/zz/T/spool ssh=/PRIVATE/ETC/SSH/sshd_config kube=/PRIVATE/ETC/KUBERNETES/admin.conf ssl=/PRIVATE/ETC/SSL/cert.pem le=/PRIVATE/ETC/LETSENCRYPT/live/example.com secrets=/PRIVATE/ETC/SECRETS/api.json pt=/PRIVATE/TMP/agent.sock varrun=/VAR/RUN/docker.sock varlog=/VAR/LOG/system.log vartmp=/VAR/TMP/cache varfolders=/VAR/FOLDERS/zz/T/scratch ordinary=docs/notes.md next=done"#,
        );

        assert_eq!(
            redacted,
            r#"users=[REDACTED_PATH] volumes=[REDACTED_PATH] run=[REDACTED_PATH] log=[REDACTED_PATH] tmp=[REDACTED_PATH] folders=[REDACTED_PATH] ssh=[REDACTED_PATH] kube=[REDACTED_PATH] ssl=[REDACTED_PATH] le=[REDACTED_PATH] secrets=[REDACTED_PATH] pt=[REDACTED_PATH] varrun=[REDACTED_PATH] varlog=[REDACTED_PATH] vartmp=[REDACTED_PATH] varfolders=[REDACTED_PATH] ordinary=docs/notes.md next=done"#,
        );
        for leaked in [
            "/USERS/alice",
            "/VOLUMES/Backup",
            "/PRIVATE/VAR/RUN/agent.sock",
            "/Private/Var/Log/system.log",
            "/PRIVATE/VAR/TMP/scratch.txt",
            "/PRIVATE/VAR/FOLDERS/zz/T/spool",
            "/PRIVATE/ETC/SSH/sshd_config",
            "/PRIVATE/ETC/KUBERNETES/admin.conf",
            "/PRIVATE/ETC/SSL/cert.pem",
            "/PRIVATE/ETC/LETSENCRYPT/live",
            "/PRIVATE/ETC/SECRETS/api.json",
            "/PRIVATE/TMP/agent.sock",
            "/VAR/RUN/docker.sock",
            "/VAR/LOG/system.log",
            "/VAR/TMP/cache",
            "/VAR/FOLDERS/zz/T/scratch",
        ] {
            assert!(
                !redacted.contains(leaked),
                "search projection redaction leaked mixed-case macOS path {leaked}: {redacted}"
            );
        }
        assert!(
            redacted.contains("ordinary=docs/notes.md"),
            "ordinary relative paths should remain visible: {redacted}"
        );
    }

    #[test]
    fn search_projection_redacts_windows_drive_prefix_casing_and_separator_variants() {
        let redacted = super::redact_search_projection_absolute_path_like_segments(
            r#"upper=C:\Users\alice\secret lower=c:\Users\alice\secret slash=Z:/Users/alice/secret next=done"#,
        );

        assert_eq!(
            redacted,
            r#"upper=[REDACTED_PATH] lower=[REDACTED_PATH] slash=[REDACTED_PATH] next=done"#
        );
        for leaked in [
            r#"C:\Users\alice\secret"#,
            r#"c:\Users\alice\secret"#,
            "Z:/Users/alice/secret",
        ] {
            assert!(
                !redacted.contains(leaked),
                "search projection redaction leaked Windows drive path {leaked}: {redacted}"
            );
        }
    }

    #[test]
    fn search_projection_redacts_unc_file_host_and_env_rooted_paths() {
        let redacted = super::redact_search_projection_absolute_path_like_segments(
            r#"unc=\\server\share\alice\session.jsonl extended=\\?\C:\Users\alice\secret file=file://build-server/share/private/log.txt env=%userprofile%\Secrets\tokens.json app=%LOCALAPPDATA%/Temp/cache.json home=$HOME/.ssh/config next=done"#,
        );

        assert_eq!(
            redacted,
            r#"unc=[REDACTED_PATH] extended=[REDACTED_PATH] file=[REDACTED_PATH] env=[REDACTED_PATH] app=[REDACTED_PATH] home=[REDACTED_PATH] next=done"#
        );
        for leaked in [
            r#"\\server\share"#,
            r#"\\?\C:\Users\alice"#,
            "build-server/share/private",
            r#"%userprofile%\Secrets"#,
            "%LOCALAPPDATA%/Temp",
            "$HOME/.ssh",
        ] {
            assert!(
                !redacted.contains(leaked),
                "search projection redaction leaked path variant {leaked}: {redacted}"
            );
        }
    }

    #[test]
    fn search_projection_redacts_generic_env_and_slash_unc_path_roots() {
        let redacted = super::redact_search_projection_absolute_path_like_segments(
            r#"slash_unc=//server/share/alice/session.jsonl file_unc=file:////server/share/alice/log.txt dollar=$USERPROFILE\Secrets\tokens.json braced=${HOME}/.ssh/config powershell=$env:APPDATA\Roaming\state tilde=~alice/.ssh/config percent=%ONEDRIVE%/Private/file.txt url=https://example.invalid/artifacts/path next=done"#,
        );

        assert_eq!(
            redacted,
            r#"slash_unc=[REDACTED_PATH] file_unc=[REDACTED_PATH] dollar=[REDACTED_PATH] braced=[REDACTED_PATH] powershell=[REDACTED_PATH] tilde=[REDACTED_PATH] percent=[REDACTED_PATH] url=https://example.invalid/artifacts/path next=done"#
        );
        for leaked in [
            "//server/share",
            "file:////server/share",
            r#"$USERPROFILE\Secrets"#,
            "${HOME}/.ssh",
            r#"$env:APPDATA\Roaming"#,
            "~alice/.ssh",
            "%ONEDRIVE%/Private",
        ] {
            assert!(
                !redacted.contains(leaked),
                "search projection redaction leaked generic path root {leaked}: {redacted}"
            );
        }
        assert!(
            redacted.contains("https://example.invalid/artifacts/path"),
            "ordinary HTTPS refs should not be treated as slash-UNC paths: {redacted}"
        );
    }

    #[test]
    fn search_projection_redacts_runtime_container_and_relative_ssh_paths() {
        let redacted = super::redact_search_projection_absolute_path_like_segments(
            r#"sock=/var/run/docker.sock run=/run/user/501/agent.sock mount=/mnt/data/.ssh/id_ed25519 app=/app/.ssh/config work=/workspaces/project/.ssh/config github=/github/workspace/.ssh/config root=/root/.ssh/config tmp=/tmp/agent/.ssh/config rel=.ssh/id_ed25519 parent=../.ssh/config winrel=.\.ssh\config next=done"#,
        );

        assert_eq!(
            redacted,
            r#"sock=[REDACTED_PATH] run=[REDACTED_PATH] mount=[REDACTED_PATH] app=[REDACTED_PATH] work=[REDACTED_PATH] github=[REDACTED_PATH] root=[REDACTED_PATH] tmp=[REDACTED_PATH] rel=[REDACTED_PATH] parent=[REDACTED_PATH] winrel=[REDACTED_PATH] next=done"#
        );
        for leaked in [
            "/var/run/docker.sock",
            "/run/user/501",
            "/mnt/data/.ssh",
            "/app/.ssh",
            "/workspaces/project/.ssh",
            "/github/workspace/.ssh",
            "/root/.ssh",
            "/tmp/agent/.ssh",
            ".ssh/id_ed25519",
            "../.ssh/config",
            r#".\.ssh\config"#,
        ] {
            assert!(
                !redacted.contains(leaked),
                "search projection redaction leaked runtime or ssh path {leaked}: {redacted}"
            );
        }
    }

    #[test]
    fn search_projection_redacts_relative_credential_path_refs() {
        let redacted = super::redact_search_projection_absolute_path_like_segments(
            r#"aws=.aws/credentials kube=./.kube/config docker=../.docker/config.json gcloud=.config/gcloud/application_default_credentials.json cargo=.cargo/credentials.toml gnupg=.\.gnupg\private-keys-v1.d npm=.npmrc yarn=./.yarnrc.yml pnpm=../.pnpmrc netrc=../.netrc pypi=.\.pypirc ordinary=docs/config.json next=done"#,
        );

        assert_eq!(
            redacted,
            r#"aws=[REDACTED_PATH] kube=[REDACTED_PATH] docker=[REDACTED_PATH] gcloud=[REDACTED_PATH] cargo=[REDACTED_PATH] gnupg=[REDACTED_PATH] npm=[REDACTED_PATH] yarn=[REDACTED_PATH] pnpm=[REDACTED_PATH] netrc=[REDACTED_PATH] pypi=[REDACTED_PATH] ordinary=docs/config.json next=done"#
        );
        for leaked in [
            ".aws/credentials",
            "./.kube/config",
            "../.docker/config.json",
            ".config/gcloud/application_default_credentials.json",
            ".cargo/credentials.toml",
            r#".\.gnupg\private-keys-v1.d"#,
            ".npmrc",
            "./.yarnrc.yml",
            "../.pnpmrc",
            "../.netrc",
            r#".\.pypirc"#,
        ] {
            assert!(
                !redacted.contains(leaked),
                "search projection redaction leaked relative credential path {leaked}: {redacted}"
            );
        }
        assert!(
            redacted.contains("ordinary=docs/config.json"),
            "ordinary relative paths should remain visible: {redacted}"
        );
    }

    #[test]
    fn search_projection_redacts_relative_package_manager_credential_paths() {
        let redacted = super::redact_search_projection_absolute_path_like_segments(
            r#"pip=.config/pip/pip.conf old_pip=./.pip/pip.ini composer=../.composer/auth.json gradle=.\.gradle\gradle.properties maven=.m2/settings.xml nuget=../.nuget/NuGet/NuGet.Config ordinary=docs/settings.xml next=done"#,
        );

        assert_eq!(
            redacted,
            r#"pip=[REDACTED_PATH] old_pip=[REDACTED_PATH] composer=[REDACTED_PATH] gradle=[REDACTED_PATH] maven=[REDACTED_PATH] nuget=[REDACTED_PATH] ordinary=docs/settings.xml next=done"#
        );
        for leaked in [
            ".config/pip/pip.conf",
            "./.pip/pip.ini",
            "../.composer/auth.json",
            r#".\.gradle\gradle.properties"#,
            ".m2/settings.xml",
            "../.nuget/NuGet/NuGet.Config",
        ] {
            assert!(
                !redacted.contains(leaked),
                "search projection redaction leaked package-manager credential path {leaked}: {redacted}"
            );
        }
        assert!(
            redacted.contains("ordinary=docs/settings.xml"),
            "ordinary relative paths should remain visible: {redacted}"
        );
    }

    #[test]
    fn search_projection_redacts_relative_cli_credential_stores() {
        let redacted = super::redact_search_projection_absolute_path_like_segments(
            r#"gh=.config/gh/hosts.yml win_gh=.\.config\gh\hosts.yml azure=.azure/accessTokens.json win_azure=..\.azure\azureProfile.json gem=.gem/credentials git=../.git-credentials ordinary=docs/hosts.yml next=done"#,
        );

        assert_eq!(
            redacted,
            r#"gh=[REDACTED_PATH] win_gh=[REDACTED_PATH] azure=[REDACTED_PATH] win_azure=[REDACTED_PATH] gem=[REDACTED_PATH] git=[REDACTED_PATH] ordinary=docs/hosts.yml next=done"#
        );
        for leaked in [
            ".config/gh/hosts.yml",
            r#".\.config\gh\hosts.yml"#,
            ".azure/accessTokens.json",
            r#"..\.azure\azureProfile.json"#,
            ".gem/credentials",
            "../.git-credentials",
        ] {
            assert!(
                !redacted.contains(leaked),
                "search projection redaction leaked CLI credential path {leaked}: {redacted}"
            );
        }
        assert!(
            redacted.contains("ordinary=docs/hosts.yml"),
            "ordinary relative paths should remain visible: {redacted}"
        );
    }

    #[test]
    fn search_projection_redacts_case_insensitive_relative_credential_paths() {
        let redacted = super::redact_search_projection_absolute_path_like_segments(
            r#"ssh=.SSH/id_ed25519 gh=.\.Config\GH\hosts.yml azure=../.AZURE/accessTokens.json gem=.Gem/Credentials git=./.GIT-CREDENTIALS ordinary=docs/Hosts.yml next=done"#,
        );

        assert_eq!(
            redacted,
            r#"ssh=[REDACTED_PATH] gh=[REDACTED_PATH] azure=[REDACTED_PATH] gem=[REDACTED_PATH] git=[REDACTED_PATH] ordinary=docs/Hosts.yml next=done"#
        );
        for leaked in [
            ".SSH/id_ed25519",
            r#".\.Config\GH\hosts.yml"#,
            "../.AZURE/accessTokens.json",
            ".Gem/Credentials",
            "./.GIT-CREDENTIALS",
        ] {
            assert!(
                !redacted.contains(leaked),
                "search projection redaction leaked case-insensitive credential path {leaked}: {redacted}"
            );
        }
        assert!(
            redacted.contains("ordinary=docs/Hosts.yml"),
            "ordinary relative paths should remain visible: {redacted}"
        );
    }

    #[test]
    fn search_projection_redacts_case_insensitive_macos_absolute_roots() {
        let redacted = super::redact_search_projection_absolute_path_like_segments(
            r#"home=/USERS/alice/private/session.jsonl file=file:///VOLUMES/USB/private/log.txt ordinary=notes/USERS.md next=done"#,
        );

        assert_eq!(
            redacted,
            r#"home=[REDACTED_PATH] file=file://[REDACTED_PATH] ordinary=notes/USERS.md next=done"#
        );
        for leaked in [
            "/USERS/alice",
            "private/session.jsonl",
            "/VOLUMES/USB",
            "private/log.txt",
        ] {
            assert!(
                !redacted.contains(leaked),
                "search projection redaction leaked case-insensitive macOS path {leaked}: {redacted}"
            );
        }
        assert!(
            redacted.contains("ordinary=notes/USERS.md"),
            "ordinary relative text should remain visible: {redacted}"
        );
    }

    #[test]
    fn search_projection_redacts_proc_sys_dev_and_secret_config_roots() {
        let redacted = super::redact_search_projection_absolute_path_like_segments(
            r#"proc=/proc/self/environ sys=/sys/kernel/security dev=/dev/shm/agent.sock log=/var/log/agent/secrets.log vartmp=/var/tmp/ee/session.json etcssh=/etc/ssh/ssh_host_ed25519_key kube=/etc/kubernetes/admin.conf ssl=/etc/ssl/private/server.key le=/etc/letsencrypt/live/example/privkey.pem secrets=/etc/secrets/token next=done"#,
        );

        assert_eq!(
            redacted,
            r#"proc=[REDACTED_PATH] sys=[REDACTED_PATH] dev=[REDACTED_PATH] log=[REDACTED_PATH] vartmp=[REDACTED_PATH] etcssh=[REDACTED_PATH] kube=[REDACTED_PATH] ssl=[REDACTED_PATH] le=[REDACTED_PATH] secrets=[REDACTED_PATH] next=done"#
        );
        for leaked in [
            "/proc/self/environ",
            "/sys/kernel/security",
            "/dev/shm/agent.sock",
            "/var/log/agent",
            "/var/tmp/ee",
            "/etc/ssh/ssh_host_ed25519_key",
            "/etc/kubernetes/admin.conf",
            "/etc/ssl/private",
            "/etc/letsencrypt/live",
            "/etc/secrets/token",
        ] {
            assert!(
                !redacted.contains(leaked),
                "search projection redaction leaked OS or secret config root {leaked}: {redacted}"
            );
        }
    }

    #[test]
    fn search_projection_redacts_macos_private_runtime_and_secret_roots() {
        let redacted = super::redact_search_projection_absolute_path_like_segments(
            r#"run=/private/var/run/agent.sock log=/private/var/log/agent/secrets.log vartmp=/private/var/tmp/ee/session.json folders=/var/folders/vt/n2xyn/T/tmp.stderr pfolders=/private/var/folders/vt/n2xyn/T/tmp.stdout etcssh=/private/etc/ssh/ssh_host_ed25519_key kube=/private/etc/kubernetes/admin.conf ssl=/private/etc/ssl/private/server.key le=/private/etc/letsencrypt/live/example/privkey.pem secrets=/private/etc/secrets/token next=done"#,
        );

        assert_eq!(
            redacted,
            r#"run=[REDACTED_PATH] log=[REDACTED_PATH] vartmp=[REDACTED_PATH] folders=[REDACTED_PATH] pfolders=[REDACTED_PATH] etcssh=[REDACTED_PATH] kube=[REDACTED_PATH] ssl=[REDACTED_PATH] le=[REDACTED_PATH] secrets=[REDACTED_PATH] next=done"#
        );
        for leaked in [
            "/private/var/run/agent.sock",
            "/private/var/log/agent",
            "/private/var/tmp/ee",
            "/var/folders/vt",
            "/private/var/folders/vt",
            "/private/etc/ssh/ssh_host_ed25519_key",
            "/private/etc/kubernetes/admin.conf",
            "/private/etc/ssl/private",
            "/private/etc/letsencrypt/live",
            "/private/etc/secrets/token",
        ] {
            assert!(
                !redacted.contains(leaked),
                "search projection redaction leaked macOS private root {leaked}: {redacted}"
            );
        }
    }

    #[test]
    fn search_projection_redacts_case_insensitive_macos_private_roots() {
        let redacted = super::redact_search_projection_absolute_path_like_segments(
            r#"run=/PRIVATE/VAR/RUN/agent.sock log=/PRIVATE/VAR/LOG/agent/secrets.log vartmp=/PRIVATE/VAR/TMP/ee/session.json folders=/VAR/FOLDERS/vt/n2xyn/T/tmp.stderr pfolders=/PRIVATE/VAR/FOLDERS/vt/n2xyn/T/tmp.stdout etcssh=/PRIVATE/ETC/SSH/ssh_host_ed25519_key kube=/PRIVATE/ETC/KUBERNETES/admin.conf ssl=/PRIVATE/ETC/SSL/private/server.key le=/PRIVATE/ETC/LETSENCRYPT/live/example/privkey.pem secrets=/PRIVATE/ETC/SECRETS/token ptmp=/PRIVATE/TMP/ee.sock ordinary=notes/PRIVATE.md next=done"#,
        );

        assert_eq!(
            redacted,
            r#"run=[REDACTED_PATH] log=[REDACTED_PATH] vartmp=[REDACTED_PATH] folders=[REDACTED_PATH] pfolders=[REDACTED_PATH] etcssh=[REDACTED_PATH] kube=[REDACTED_PATH] ssl=[REDACTED_PATH] le=[REDACTED_PATH] secrets=[REDACTED_PATH] ptmp=[REDACTED_PATH] ordinary=notes/PRIVATE.md next=done"#
        );
        for leaked in [
            "/PRIVATE/VAR/RUN/agent.sock",
            "/PRIVATE/VAR/LOG/agent",
            "/PRIVATE/VAR/TMP/ee",
            "/VAR/FOLDERS/vt",
            "/PRIVATE/VAR/FOLDERS/vt",
            "/PRIVATE/ETC/SSH/ssh_host_ed25519_key",
            "/PRIVATE/ETC/KUBERNETES/admin.conf",
            "/PRIVATE/ETC/SSL/private",
            "/PRIVATE/ETC/LETSENCRYPT/live",
            "/PRIVATE/ETC/SECRETS/token",
            "/PRIVATE/TMP/ee.sock",
        ] {
            assert!(
                !redacted.contains(leaked),
                "search projection redaction leaked mixed-case macOS private root {leaked}: {redacted}"
            );
        }
        assert!(
            redacted.contains("ordinary=notes/PRIVATE.md"),
            "ordinary relative text should remain visible: {redacted}"
        );
    }

    #[test]
    fn search_projection_preserves_file_url_authority_slashes_for_absolute_paths() {
        let redacted = super::redact_search_projection_absolute_path_like_segments(
            r#"home=file:///home/alice/.ssh/config etc=file:///etc/ssh/ssh_host_ed25519_key proc=file:///proc/self/environ volumes=file:///Volumes/USBNVME16TB/private/session.jsonl next=done"#,
        );

        assert_eq!(
            redacted,
            r#"home=file://[REDACTED_PATH] etc=file://[REDACTED_PATH] proc=file://[REDACTED_PATH] volumes=file://[REDACTED_PATH] next=done"#
        );
        for leaked in [
            "/home/alice/.ssh",
            "/etc/ssh/ssh_host_ed25519_key",
            "/proc/self/environ",
            "/Volumes/USBNVME16TB/private",
        ] {
            assert!(
                !redacted.contains(leaked),
                "search projection redaction leaked file URL path {leaked}: {redacted}"
            );
        }
    }

    #[test]
    fn search_projection_redacts_path_segments_with_spaces() {
        let redacted = super::redact_search_projection_absolute_path_like_segments(
            r#"source=file:///Users/alice/My Project/session.jsonl label=/workspace/Plain Path/log.txt win=C:\Users\alice\My Project\session.jsonl note=done"#,
        );

        assert_eq!(
            redacted,
            r#"source=file://[REDACTED_PATH] label=[REDACTED_PATH] win=[REDACTED_PATH] note=done"#
        );
        for leaked in [
            "/Users/alice",
            "My Project/session.jsonl",
            "/workspace/Plain Path/log.txt",
            r#"C:\Users\alice\My Project\session.jsonl"#,
        ] {
            assert!(
                !redacted.contains(leaked),
                "search projection redaction leaked {leaked}: {redacted}"
            );
        }
    }

    #[test]
    fn search_projection_redacts_terminal_path_components_with_spaces() {
        let redacted = super::redact_search_projection_absolute_path_like_segments(
            r#"source=/Users/alice/My Project label=/data/private/Release Notes win=C:\Users\alice\Draft Folder note=done"#,
        );

        assert_eq!(
            redacted,
            r#"source=[REDACTED_PATH] label=[REDACTED_PATH] win=[REDACTED_PATH] note=done"#
        );
        for leaked in [
            "/Users/alice",
            "My Project",
            "/data/private/Release Notes",
            r#"C:\Users\alice\Draft Folder"#,
        ] {
            assert!(
                !redacted.contains(leaked),
                "search projection redaction leaked terminal component {leaked}: {redacted}"
            );
        }
    }

    #[test]
    fn search_projection_does_not_cross_line_boundaries_after_paths() {
        let redacted = super::redact_search_projection_absolute_path_like_segments(
            "source=/Users/alice/My Project\nAgent: cod-search",
        );

        assert_eq!(redacted, "source=[REDACTED_PATH]\nAgent: cod-search");
        assert!(
            !redacted.contains("My Project"),
            "search projection leaked terminal component across newline: {redacted}"
        );
    }

    #[test]
    fn session_document_builder_omits_sensitive_metadata_json() {
        let mut session = make_test_session();
        session.metadata_json = Some(
            r#"{"source":"cass","sourcePath":"file:///Users/alice/private/session.jsonl?api_key=redaction-fixture"}"#.to_owned(),
        );
        session.content_hash =
            "file:///C:/Users/Alice/private/hash.txt?api_key=hash-redaction-fixture".to_owned();

        let doc = super::session_to_document(&session);
        let content = doc.content().to_string();
        let indexable = doc.into_indexable();
        let rendered = format!("{}\n{:?}", content, indexable.metadata);

        assert!(!rendered.contains("metadata_json"));
        assert!(
            !rendered.contains("/Users/alice/private/session.jsonl"),
            "session search document leaked metadata path: {rendered}"
        );
        assert!(
            !rendered.contains("redaction-fixture"),
            "session search document leaked secret-like metadata: {rendered}"
        );
        assert!(
            !rendered.contains("C:/Users/Alice") && !rendered.contains("hash-redaction-fixture"),
            "session search document leaked untrusted upstream content_hash: {rendered}"
        );
        assert!(!indexable.metadata.contains_key("metadata_json"));
        assert!(!indexable.metadata.contains_key("content_hash"));
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
    fn artifact_document_builder_redacts_sensitive_registry_refs() {
        let mut artifact = make_test_artifact();
        artifact.original_path = Some("/Users/alice/private/logs/build.log".to_string());
        artifact.external_ref =
            Some("https://example.invalid/artifacts?api_key=redaction-fixture".to_string());
        artifact.provenance_uri =
            Some("file:///workspace/project/logs/build.log?api_key=redaction-fixture".to_string());

        let doc = super::artifact_to_document(&artifact);
        assert!(doc.content().contains("Path: [REDACTED_PATH]"));
        assert!(
            doc.content()
                .contains("External ref: https://example.invalid/artifacts?")
        );

        let content = doc.content().to_string();
        let indexable = doc.into_indexable();
        let rendered = format!(
            "{}\n{:?}\n{:?}",
            content, indexable.title, indexable.metadata
        );

        assert!(
            rendered.contains("[REDACTED_PATH]"),
            "redacted artifact refs should retain path placeholders"
        );
        assert!(
            rendered.contains("[REDACTED:api_key]"),
            "redacted artifact refs should retain secret placeholders"
        );
        assert!(
            !rendered.contains("/Users/alice/private/logs/build.log"),
            "artifact search document leaked original path: {rendered}"
        );
        assert!(
            !rendered.contains("/workspace/project/logs/build.log"),
            "artifact search document leaked provenance path: {rendered}"
        );
        assert!(
            !rendered.contains("redaction-fixture"),
            "artifact search document leaked secret-like external ref: {rendered}"
        );
        assert_eq!(indexable.title.as_deref(), Some("Artifact [REDACTED_PATH]"));
        assert_eq!(
            indexable.metadata.get("path"),
            Some(&"[REDACTED_PATH]".to_string())
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

    #[test]
    fn curation_candidate_document_builder_projects_embedding_text() {
        let candidate = make_test_candidate();
        let target = make_test_memory();
        let doc = super::CurationCandidateDocumentBuilder::new()
            .with_workspace_path("/workspace/project")
            .with_target_memory_content(&target.content)
            .build(&candidate);

        assert_eq!(doc.id(), "curate_01234567890123456789012345");
        assert_eq!(doc.source(), DocumentSource::CurationCandidate);
        assert!(
            doc.content()
                .contains("Proposed content: Run cargo fmt --check")
        );
        assert!(
            doc.content()
                .contains("Target memory content: Always run cargo fmt")
        );
        assert!(doc.content().contains("Source id: eval-run-001"));

        let indexable = doc.into_indexable();
        assert_eq!(
            indexable.metadata.get("source"),
            Some(&"curation_candidate".to_owned())
        );
        assert_eq!(
            indexable.metadata.get("workspace"),
            Some(&"/workspace/project".to_owned())
        );
        assert_eq!(
            indexable.metadata.get("candidate_type"),
            Some(&"consolidate".to_owned())
        );
        assert_eq!(
            indexable.metadata.get("target_memory_id"),
            Some(&"mem_01234567890123456789012345".to_owned())
        );
        assert_eq!(
            indexable.metadata.get("source_id"),
            Some(&"eval-run-001".to_owned())
        );
    }

    #[test]
    fn curation_candidate_document_builder_redacts_sensitive_source_id() {
        let mut candidate = make_test_candidate();
        candidate.source_id =
            Some("file:///Users/alice/private/review.jsonl?api_key=redaction-fixture".to_string());

        let doc = super::CurationCandidateDocumentBuilder::new().build(&candidate);
        let content = doc.content().to_string();
        let indexable = doc.into_indexable();
        let rendered = format!("{}\n{:?}", content, indexable.metadata);

        assert!(
            rendered.contains("[REDACTED_PATH]"),
            "redacted curation source should retain path placeholders"
        );
        assert!(
            rendered.contains("[REDACTED:api_key]"),
            "redacted curation source should retain secret placeholders"
        );
        assert!(
            !rendered.contains("/Users/alice/private/review.jsonl"),
            "curation candidate search document leaked source path: {rendered}"
        );
        assert!(
            !rendered.contains("redaction-fixture"),
            "curation candidate search document leaked secret-like source id: {rendered}"
        );
        assert_eq!(
            indexable.metadata.get("source_id"),
            Some(&"file://[REDACTED_PATH]?api_key=[REDACTED:api_key]".to_string())
        );
    }

    #[test]
    fn curation_candidate_embedding_is_deterministic_search_vector() {
        let candidate = make_test_candidate();
        let target = make_test_memory();

        let first = super::curation_candidate_embedding(
            &candidate,
            Some(&target),
            Some("/workspace/project"),
        );
        let second = super::curation_candidate_embedding(
            &candidate,
            Some(&target),
            Some("/workspace/project"),
        );

        assert_eq!(first.candidate_id, candidate.id);
        assert_eq!(first.embedding.len(), 256);
        assert_eq!(first, second);
        assert!(
            first
                .embedding
                .iter()
                .any(|value| value.is_finite() && value.abs() > f32::EPSILON)
        );
    }

    // =========================================================================
    // Index Manifest Tests (EE-267)
    // =========================================================================

    use super::{
        CANONICAL_DOCUMENT_SCHEMA, EmbeddingConfig, FRANKENSEARCH_VERSION,
        INDEX_MANIFEST_SCHEMA_V1, IndexManifest, IndexManifestError, IndexStaleness,
        SearchSurrogateAuditDecision, SearchSurrogateAuditInput, SearchSurrogateDegradedCode,
        SearchSurrogateDescriptor, SearchSurrogateModelFingerprint, SearchSurrogatePolicy,
        SearchSurrogateType, audit_search_surrogate,
    };

    #[test]
    fn index_manifest_schema_constant_is_stable() {
        assert_eq!(INDEX_MANIFEST_SCHEMA_V1, "ee.index_manifest.v1");
    }

    #[test]
    fn embedding_config_default_is_hash_256() {
        let config = EmbeddingConfig::default();
        assert_eq!(config, EmbeddingConfig::hash_256());
        assert_eq!(config.model_id, "hash-256");
        assert_eq!(config.dimension, 256);
        assert!(config.deterministic);
    }

    #[test]
    fn embedding_config_hash_is_deterministic_and_field_sensitive() {
        let config = EmbeddingConfig::hash_256();
        let hash = config.content_hash();

        assert_eq!(hash, config.content_hash());
        assert_eq!(
            hash,
            EmbeddingConfig::new("hash-256", 256, true).content_hash()
        );
        assert!(hash.starts_with("blake3:"));
        assert_eq!(hash.len(), "blake3:".len() + 64);
        assert_ne!(
            hash,
            EmbeddingConfig::new("model2vec-base", 256, true).content_hash()
        );
        assert_ne!(
            hash,
            EmbeddingConfig::new("hash-256", 384, true).content_hash()
        );
        assert_ne!(
            hash,
            EmbeddingConfig::new("hash-256", 256, false).content_hash()
        );
    }

    fn local_surrogate_model() -> SearchSurrogateModelFingerprint {
        SearchSurrogateModelFingerprint::new("hash-256", "2026-05-19", ["normalize_l2", "utf8"])
    }

    fn embedding_surrogate() -> SearchSurrogateDescriptor {
        SearchSurrogateDescriptor {
            surrogate_type: SearchSurrogateType::Embedding,
            model_fingerprint: SearchSurrogateModelFingerprint::new(
                "hash-256",
                "2026-05-19",
                ["utf8", "normalize_l2", "utf8"],
            ),
            content_hash: "blake3:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_owned(),
            valid_until: Some("2026-05-20T00:00:00Z".to_owned()),
        }
    }

    #[test]
    fn search_surrogate_type_tokens_match_schema_contract() {
        assert_eq!(SearchSurrogateType::Embedding.as_str(), "embedding");
        assert_eq!(SearchSurrogateType::Summary.as_str(), "summary");
        assert_eq!(SearchSurrogateType::Minhash.as_str(), "minhash");
        assert_eq!(
            SearchSurrogateType::LexicalMetadata.as_str(),
            "lexical_metadata"
        );
        assert_eq!(
            SearchSurrogateType::QueryFingerprint.as_str(),
            "query_fingerprint"
        );
    }

    #[test]
    fn search_surrogate_model_fingerprint_normalizes_feature_flags_for_matching() {
        let remote = SearchSurrogateModelFingerprint::new(
            "hash-256",
            "2026-05-19",
            ["utf8", "normalize_l2", "utf8"],
        );
        let local = SearchSurrogateModelFingerprint::new(
            "hash-256",
            "2026-05-19",
            ["normalize_l2", "utf8"],
        );

        assert!(remote.is_compatible_with(&local));
        assert_eq!(
            remote.feature_flags,
            vec!["normalize_l2".to_owned(), "utf8".to_owned()]
        );
    }

    #[test]
    fn search_surrogate_default_policy_denies_embedding_export_and_uses_lexical_fallback() {
        let surrogate = embedding_surrogate();
        let local_model = local_surrogate_model();
        let policy = SearchSurrogatePolicy::default();
        let input = SearchSurrogateAuditInput {
            surrogate: &surrogate,
            policy: &policy,
            local_model: &local_model,
            local_content_hash: Some("blake3:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
            observed_at: "2026-05-19T23:00:00Z",
            local_body_available: true,
        };

        let outcome = audit_search_surrogate(&input);

        assert_eq!(
            outcome.decision,
            SearchSurrogateAuditDecision::LexicalFallback
        );
        assert_eq!(
            outcome.degraded_codes,
            vec![
                SearchSurrogateDegradedCode::Denied,
                SearchSurrogateDegradedCode::LexicalFallbackUsed,
            ]
        );
    }

    #[test]
    fn search_surrogate_metadata_only_policy_denies_embeddings_but_allows_lexical_metadata() {
        let embedding = embedding_surrogate();
        let lexical = SearchSurrogateDescriptor {
            surrogate_type: SearchSurrogateType::LexicalMetadata,
            ..embedding_surrogate()
        };
        let local_model = local_surrogate_model();
        let embedding_policy =
            SearchSurrogatePolicy::metadata_only_for(SearchSurrogateType::Embedding);
        let lexical_policy =
            SearchSurrogatePolicy::metadata_only_for(SearchSurrogateType::LexicalMetadata);

        let embedding_outcome = audit_search_surrogate(&SearchSurrogateAuditInput {
            surrogate: &embedding,
            policy: &embedding_policy,
            local_model: &local_model,
            local_content_hash: Some("blake3:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
            observed_at: "2026-05-19T23:00:00Z",
            local_body_available: true,
        });
        let lexical_outcome = audit_search_surrogate(&SearchSurrogateAuditInput {
            surrogate: &lexical,
            policy: &lexical_policy,
            local_model: &local_model,
            local_content_hash: Some("blake3:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
            observed_at: "2026-05-19T23:00:00Z",
            local_body_available: false,
        });

        assert_eq!(
            embedding_outcome.decision,
            SearchSurrogateAuditDecision::LexicalFallback
        );
        assert_eq!(
            embedding_outcome.degraded_codes,
            vec![
                SearchSurrogateDegradedCode::Denied,
                SearchSurrogateDegradedCode::LexicalFallbackUsed,
            ]
        );
        assert_eq!(
            lexical_outcome.decision,
            SearchSurrogateAuditDecision::ReuseRemote
        );
        assert!(lexical_outcome.degraded_codes.is_empty());
    }

    #[test]
    fn search_surrogate_incompatible_model_recomputes_when_body_is_available() {
        let surrogate = SearchSurrogateDescriptor {
            model_fingerprint: SearchSurrogateModelFingerprint::new(
                "model2vec-base",
                "2026-05-19",
                ["normalize_l2"],
            ),
            ..embedding_surrogate()
        };
        let local_model = local_surrogate_model();
        let policy = SearchSurrogatePolicy::allow_reuse_after_compatibility_check();
        let input = SearchSurrogateAuditInput {
            surrogate: &surrogate,
            policy: &policy,
            local_model: &local_model,
            local_content_hash: Some("blake3:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
            observed_at: "2026-05-19T23:00:00Z",
            local_body_available: true,
        };

        let outcome = audit_search_surrogate(&input);

        assert_eq!(
            outcome.decision,
            SearchSurrogateAuditDecision::RecomputeLocal
        );
        assert_eq!(
            outcome.degraded_codes,
            vec![
                SearchSurrogateDegradedCode::Incompatible,
                SearchSurrogateDegradedCode::Recomputed,
            ]
        );
    }

    #[test]
    fn search_surrogate_incompatible_model_falls_back_when_body_is_unavailable() {
        let surrogate = SearchSurrogateDescriptor {
            model_fingerprint: SearchSurrogateModelFingerprint::new(
                "hash-256",
                "older-version",
                ["normalize_l2", "utf8"],
            ),
            ..embedding_surrogate()
        };
        let local_model = local_surrogate_model();
        let policy = SearchSurrogatePolicy::allow_reuse_after_compatibility_check();
        let input = SearchSurrogateAuditInput {
            surrogate: &surrogate,
            policy: &policy,
            local_model: &local_model,
            local_content_hash: Some("blake3:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
            observed_at: "2026-05-19T23:00:00Z",
            local_body_available: false,
        };

        let outcome = audit_search_surrogate(&input);

        assert_eq!(
            outcome.decision,
            SearchSurrogateAuditDecision::LexicalFallback
        );
        assert_eq!(
            outcome.degraded_codes,
            vec![
                SearchSurrogateDegradedCode::Incompatible,
                SearchSurrogateDegradedCode::LexicalFallbackUsed,
            ]
        );
    }

    #[test]
    fn search_surrogate_content_hash_mismatch_invalidates_and_recomputes() {
        let surrogate = embedding_surrogate();
        let local_model = local_surrogate_model();
        let policy = SearchSurrogatePolicy::allow_reuse_after_compatibility_check();
        let input = SearchSurrogateAuditInput {
            surrogate: &surrogate,
            policy: &policy,
            local_model: &local_model,
            local_content_hash: Some("blake3:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"),
            observed_at: "2026-05-19T23:00:00Z",
            local_body_available: true,
        };

        let outcome = audit_search_surrogate(&input);

        assert_eq!(
            outcome.decision,
            SearchSurrogateAuditDecision::RecomputeLocal
        );
        assert_eq!(
            outcome.degraded_codes,
            vec![SearchSurrogateDegradedCode::Recomputed]
        );
    }

    #[test]
    fn search_surrogate_valid_until_expiry_invalidates_and_falls_back_without_body() {
        let surrogate = embedding_surrogate();
        let local_model = local_surrogate_model();
        let policy = SearchSurrogatePolicy::allow_reuse_after_compatibility_check();
        let input = SearchSurrogateAuditInput {
            surrogate: &surrogate,
            policy: &policy,
            local_model: &local_model,
            local_content_hash: Some("blake3:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
            observed_at: "2026-05-20T00:00:01Z",
            local_body_available: false,
        };

        let outcome = audit_search_surrogate(&input);

        assert_eq!(
            outcome.decision,
            SearchSurrogateAuditDecision::LexicalFallback
        );
        assert_eq!(
            outcome.degraded_codes,
            vec![SearchSurrogateDegradedCode::LexicalFallbackUsed]
        );
    }

    #[test]
    fn search_surrogate_compatible_fresh_surrogate_can_be_reused() {
        let surrogate = embedding_surrogate();
        let local_model = local_surrogate_model();
        let policy = SearchSurrogatePolicy::allow_reuse_after_compatibility_check();
        let input = SearchSurrogateAuditInput {
            surrogate: &surrogate,
            policy: &policy,
            local_model: &local_model,
            local_content_hash: Some("blake3:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
            observed_at: "2026-05-19T23:00:00Z",
            local_body_available: false,
        };

        let outcome = audit_search_surrogate(&input);

        assert_eq!(outcome.decision, SearchSurrogateAuditDecision::ReuseRemote);
        assert!(outcome.degraded_codes.is_empty());
    }

    #[test]
    fn search_surrogate_audit_json_uses_structured_codes_without_raw_content() {
        let surrogate = embedding_surrogate();
        let local_model = local_surrogate_model();
        let policy = SearchSurrogatePolicy::default();
        let input = SearchSurrogateAuditInput {
            surrogate: &surrogate,
            policy: &policy,
            local_model: &local_model,
            local_content_hash: Some("blake3:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
            observed_at: "2026-05-19T23:00:00Z",
            local_body_available: true,
        };
        let outcome = audit_search_surrogate(&input);

        let audit_json = outcome.data_json(&input);

        assert_eq!(audit_json["schema"], "ee.mesh.surrogate_audit.v1");
        assert_eq!(audit_json["surrogateType"], "embedding");
        assert_eq!(
            audit_json["degradedCodes"],
            json!(["surrogate_denied", "lexical_fallback_used"])
        );
        assert!(!audit_json.to_string().contains("raw private memory body"));
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
            5,                                           // Same db_generation as we'll check
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
        let embedding_hash = json["embedding"]["content_hash"]
            .as_str()
            .expect("embedding content hash");
        assert!(embedding_hash.starts_with("blake3:"));
        assert_eq!(embedding_hash.len(), "blake3:".len() + 64);
        assert_eq!(json["lexical_index_path"], "lexical");
        assert_eq!(json["vector_index_path"], "vector.fast.idx");
    }

    #[test]
    fn search_cache_hotset_prewarm_enforces_generation_and_budget() {
        let entries = vec![
            SearchHotsetEntry::memory("mem-1", 4, 3),
            SearchHotsetEntry::memory("mem-2", 4, 2),
            SearchHotsetEntry::graph_neighborhood("mem-1", 2, 4, 1),
        ];
        let hotset = SearchHotset::new(entries);
        let governor =
            SearchCacheGovernor::new(4, CacheBudget::new(2, 10_000)).with_current_usage(0, 0);

        let report = prewarm_search_hotset(&hotset, governor);

        assert_eq!(report.status, SearchCacheStatus::PressureFallback);
        assert_eq!(report.admitted_entries, 2);
        assert_eq!(report.rejected_entries, 1);
        assert_eq!(report.fallback_reason, Some("budget_trimmed"));
        assert_eq!(report.prewarm_evidence.operations, 3);
        assert_eq!(report.prewarm_evidence.requested_entries, 3);
        assert_eq!(report.prewarm_evidence.admitted_entries, 2);
        assert_eq!(report.prewarm_evidence.rejected_entries, 1);
        assert_eq!(report.prewarm_evidence.requested_hit_count, 6);
        assert_eq!(report.prewarm_evidence.admitted_hit_count, 5);
        assert_eq!(report.prewarm_evidence.rejected_hit_count, 1);
        assert!((report.prewarm_evidence.hit_coverage_ratio - (5.0 / 6.0)).abs() < f64::EPSILON);

        let report_json = report.data_json();
        let report_object = report_json.as_object().expect("report JSON object");
        assert!(!report_object.contains_key("benchmarkEvidence"));
        assert_eq!(
            report_json["prewarmEvidence"]["evidenceKind"],
            "search_hotset_admission"
        );
        assert!(
            report_json["prewarmEvidence"]
                .as_object()
                .expect("evidence JSON object")
                .get("coldLatencyUs")
                .is_none()
        );

        let stale = prewarm_search_hotset(
            &hotset,
            SearchCacheGovernor::new(5, CacheBudget::new(8, 10_000)),
        );
        assert_eq!(stale.status, SearchCacheStatus::StaleGeneration);
        assert_eq!(stale.admitted_entries, 0);
        assert_eq!(stale.prewarm_evidence.rejected_entries, 3);
        assert_eq!(stale.prewarm_evidence.requested_hit_count, 6);
        assert_eq!(stale.prewarm_evidence.admitted_hit_count, 0);
        assert_eq!(stale.fallback_reason, Some("generation_mismatch"));
    }

    #[test]
    fn search_cache_governor_bypasses_at_critical_pressure() {
        let hotset = SearchHotset::new(vec![SearchHotsetEntry::memory("mem-1", 1, 1)]);
        let budget = CacheBudget::new(10, 1_000).with_watermarks(0.5, 0.8);
        let governor = SearchCacheGovernor::new(1, budget).with_current_usage(9, 900);

        let report = prewarm_search_hotset(&hotset, governor);

        assert_eq!(report.status, SearchCacheStatus::Bypassed);
        assert_eq!(report.memory_pressure, MemoryPressure::Critical);
        assert_eq!(report.fallback_reason, Some("memory_pressure_critical"));
        assert_eq!(report.admitted_entries, 0);
    }

    #[test]
    fn search_cache_hotset_aggregate_totals_saturate() {
        let hotset = SearchHotset::new(vec![
            SearchHotsetEntry {
                key: "key-a".to_string(),
                kind: SearchHotsetEntryKind::Memory,
                generation: 7,
                estimated_bytes: usize::MAX,
                hit_count: u64::MAX,
                redaction_status: "content_not_stored",
            },
            SearchHotsetEntry {
                key: "key-b".to_string(),
                kind: SearchHotsetEntryKind::QueryShape,
                generation: 7,
                estimated_bytes: 1,
                hit_count: 1,
                redaction_status: "content_not_stored",
            },
        ]);

        assert_eq!(hotset.total_estimated_bytes(), usize::MAX);
        assert_eq!(hotset.total_hit_count(), u64::MAX);

        let report = prewarm_search_hotset(
            &hotset,
            SearchCacheGovernor::new(7, CacheBudget::new(4, usize::MAX)),
        );

        assert_eq!(report.status, SearchCacheStatus::Warm);
        assert_eq!(report.estimated_bytes, usize::MAX);
        assert_eq!(report.hit_rate, 1.0);
    }

    #[test]
    fn search_cache_entries_are_redaction_safe_and_stable_json() {
        let secret_query = "rotate sk-ant-api03-secret before release";
        let query_entry = SearchHotsetEntry::query_shape(secret_query, 3, 4)
            .expect("query shape should be cacheable");
        let document = CanonicalSearchDocument::new(
            "doc-1",
            "contains AWS_SECRET_ACCESS_KEY=abcdef but content must not enter cache",
            DocumentSource::Memory,
        );
        let hotset = SearchHotset::from_queries_and_documents([secret_query], [&document], 3);
        let report = prewarm_search_hotset(
            &hotset,
            SearchCacheGovernor::new(3, CacheBudget::new(8, 64_000)),
        );
        let json = report.data_json().to_string();

        assert_eq!(query_entry.kind, SearchHotsetEntryKind::QueryShape);
        assert!(query_entry.is_redaction_safe());
        assert!(!query_entry.key.contains("sk-ant"));
        assert!(!json.contains("AWS_SECRET_ACCESS_KEY"));
        assert!(!json.contains("sk-ant-api03-secret"));
        assert_eq!(report.status, SearchCacheStatus::Warm);
        assert_eq!(report.hit_rate, 1.0);
    }
}
