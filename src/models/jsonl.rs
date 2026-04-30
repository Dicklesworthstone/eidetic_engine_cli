//! JSONL export schema types (EE-220).
//!
//! Defines the schema for JSONL export/import operations. Each JSONL file
//! contains a header record followed by data records, one per line.
//!
//! # File Structure
//!
//! ```text
//! {"schema": "ee.export.header.v1", ...}  // Header (first line)
//! {"schema": "ee.export.memory.v1", ...}  // Data record
//! {"schema": "ee.export.memory.v1", ...}  // Data record
//! ...
//! {"schema": "ee.export.footer.v1", ...}  // Footer (optional, last line)
//! ```

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

/// Schema identifier for export header records.
pub const EXPORT_HEADER_SCHEMA_V1: &str = "ee.export.header.v1";

/// Schema identifier for export memory records.
pub const EXPORT_MEMORY_SCHEMA_V1: &str = "ee.export.memory.v1";

/// Schema identifier for export footer records.
pub const EXPORT_FOOTER_SCHEMA_V1: &str = "ee.export.footer.v1";

/// Schema identifier for export audit records.
pub const EXPORT_AUDIT_SCHEMA_V1: &str = "ee.export.audit.v1";

/// Schema identifier for export link records.
pub const EXPORT_LINK_SCHEMA_V1: &str = "ee.export.link.v1";

/// Schema identifier for export tag records.
pub const EXPORT_TAG_SCHEMA_V1: &str = "ee.export.tag.v1";

/// Schema identifier for export agent records.
pub const EXPORT_AGENT_SCHEMA_V1: &str = "ee.export.agent.v1";

/// Schema identifier for export workspace records.
pub const EXPORT_WORKSPACE_SCHEMA_V1: &str = "ee.export.workspace.v1";

/// All JSONL export schema identifiers.
pub const ALL_EXPORT_SCHEMAS: &[&str] = &[
    EXPORT_HEADER_SCHEMA_V1,
    EXPORT_MEMORY_SCHEMA_V1,
    EXPORT_FOOTER_SCHEMA_V1,
    EXPORT_AUDIT_SCHEMA_V1,
    EXPORT_LINK_SCHEMA_V1,
    EXPORT_TAG_SCHEMA_V1,
    EXPORT_AGENT_SCHEMA_V1,
    EXPORT_WORKSPACE_SCHEMA_V1,
];

/// Export format version.
pub const EXPORT_FORMAT_VERSION: u32 = 1;

/// Export record type discriminator.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExportRecordType {
    Header,
    Memory,
    Link,
    Tag,
    Agent,
    Workspace,
    Audit,
    Footer,
}

impl ExportRecordType {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Header => "header",
            Self::Memory => "memory",
            Self::Link => "link",
            Self::Tag => "tag",
            Self::Agent => "agent",
            Self::Workspace => "workspace",
            Self::Audit => "audit",
            Self::Footer => "footer",
        }
    }

    #[must_use]
    pub const fn schema(self) -> &'static str {
        match self {
            Self::Header => EXPORT_HEADER_SCHEMA_V1,
            Self::Memory => EXPORT_MEMORY_SCHEMA_V1,
            Self::Link => EXPORT_LINK_SCHEMA_V1,
            Self::Tag => EXPORT_TAG_SCHEMA_V1,
            Self::Agent => EXPORT_AGENT_SCHEMA_V1,
            Self::Workspace => EXPORT_WORKSPACE_SCHEMA_V1,
            Self::Audit => EXPORT_AUDIT_SCHEMA_V1,
            Self::Footer => EXPORT_FOOTER_SCHEMA_V1,
        }
    }

    #[must_use]
    pub const fn all() -> &'static [Self] {
        &[
            Self::Header,
            Self::Memory,
            Self::Link,
            Self::Tag,
            Self::Agent,
            Self::Workspace,
            Self::Audit,
            Self::Footer,
        ]
    }
}

impl fmt::Display for ExportRecordType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParseExportRecordTypeError {
    pub invalid: String,
}

impl fmt::Display for ParseExportRecordTypeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "invalid export record type '{}'; expected one of: header, memory, link, tag, agent, workspace, audit, footer",
            self.invalid
        )
    }
}

impl std::error::Error for ParseExportRecordTypeError {}

impl FromStr for ExportRecordType {
    type Err = ParseExportRecordTypeError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "header" => Ok(Self::Header),
            "memory" => Ok(Self::Memory),
            "link" => Ok(Self::Link),
            "tag" => Ok(Self::Tag),
            "agent" => Ok(Self::Agent),
            "workspace" => Ok(Self::Workspace),
            "audit" => Ok(Self::Audit),
            "footer" => Ok(Self::Footer),
            _ => Err(ParseExportRecordTypeError {
                invalid: s.to_owned(),
            }),
        }
    }
}

/// Redaction level for exported data.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum RedactionLevel {
    /// No redaction applied.
    #[default]
    None,
    /// Minimal redaction: secrets and credentials only.
    Minimal,
    /// Standard redaction: secrets, paths, and identifiers.
    Standard,
    /// Full redaction: all potentially sensitive content.
    Full,
}

impl RedactionLevel {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Minimal => "minimal",
            Self::Standard => "standard",
            Self::Full => "full",
        }
    }

    #[must_use]
    pub const fn all() -> &'static [Self] {
        &[Self::None, Self::Minimal, Self::Standard, Self::Full]
    }

    #[must_use]
    pub const fn redacts_secrets(self) -> bool {
        !matches!(self, Self::None)
    }

    #[must_use]
    pub const fn redacts_paths(self) -> bool {
        matches!(self, Self::Standard | Self::Full)
    }

    #[must_use]
    pub const fn redacts_identifiers(self) -> bool {
        matches!(self, Self::Standard | Self::Full)
    }

    #[must_use]
    pub const fn redacts_content(self) -> bool {
        matches!(self, Self::Full)
    }
}

impl fmt::Display for RedactionLevel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParseRedactionLevelError {
    pub invalid: String,
}

impl fmt::Display for ParseRedactionLevelError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "invalid redaction level '{}'; expected one of: none, minimal, standard, full",
            self.invalid
        )
    }
}

impl std::error::Error for ParseRedactionLevelError {}

impl FromStr for RedactionLevel {
    type Err = ParseRedactionLevelError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "none" => Ok(Self::None),
            "minimal" => Ok(Self::Minimal),
            "standard" => Ok(Self::Standard),
            "full" => Ok(Self::Full),
            _ => Err(ParseRedactionLevelError {
                invalid: s.to_owned(),
            }),
        }
    }
}

/// Export scope selector.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ExportScope {
    /// Export all data.
    #[default]
    All,
    /// Export only memories.
    Memories,
    /// Export only audit records.
    Audit,
    /// Export only links and relationships.
    Links,
    /// Export metadata only (no content).
    MetadataOnly,
}

impl ExportScope {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Memories => "memories",
            Self::Audit => "audit",
            Self::Links => "links",
            Self::MetadataOnly => "metadata_only",
        }
    }

    #[must_use]
    pub const fn all() -> &'static [Self] {
        &[
            Self::All,
            Self::Memories,
            Self::Audit,
            Self::Links,
            Self::MetadataOnly,
        ]
    }

    #[must_use]
    pub const fn includes_memories(self) -> bool {
        matches!(self, Self::All | Self::Memories | Self::MetadataOnly)
    }

    #[must_use]
    pub const fn includes_audit(self) -> bool {
        matches!(self, Self::All | Self::Audit)
    }

    #[must_use]
    pub const fn includes_links(self) -> bool {
        matches!(self, Self::All | Self::Links)
    }

    #[must_use]
    pub const fn includes_content(self) -> bool {
        !matches!(self, Self::MetadataOnly)
    }
}

impl fmt::Display for ExportScope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParseExportScopeError {
    pub invalid: String,
}

impl fmt::Display for ParseExportScopeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "invalid export scope '{}'; expected one of: all, memories, audit, links, metadata_only",
            self.invalid
        )
    }
}

impl std::error::Error for ParseExportScopeError {}

impl FromStr for ExportScope {
    type Err = ParseExportScopeError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "all" => Ok(Self::All),
            "memories" => Ok(Self::Memories),
            "audit" => Ok(Self::Audit),
            "links" => Ok(Self::Links),
            "metadata_only" => Ok(Self::MetadataOnly),
            _ => Err(ParseExportScopeError {
                invalid: s.to_owned(),
            }),
        }
    }
}

/// Export header record.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ExportHeader {
    pub schema: String,
    pub format_version: u32,
    pub created_at: String,
    pub workspace_id: Option<String>,
    pub workspace_path: Option<String>,
    pub export_scope: ExportScope,
    pub redaction_level: RedactionLevel,
    pub record_count: Option<u64>,
    pub ee_version: String,
    pub hostname: Option<String>,
    pub export_id: String,
}

impl ExportHeader {
    #[must_use]
    pub fn builder() -> ExportHeaderBuilder {
        ExportHeaderBuilder::default()
    }
}

#[derive(Clone, Debug, Default)]
pub struct ExportHeaderBuilder {
    created_at: Option<String>,
    workspace_id: Option<String>,
    workspace_path: Option<String>,
    export_scope: ExportScope,
    redaction_level: RedactionLevel,
    record_count: Option<u64>,
    ee_version: Option<String>,
    hostname: Option<String>,
    export_id: Option<String>,
}

impl ExportHeaderBuilder {
    #[must_use]
    pub fn created_at(mut self, created_at: impl Into<String>) -> Self {
        self.created_at = Some(created_at.into());
        self
    }

    #[must_use]
    pub fn workspace_id(mut self, workspace_id: impl Into<String>) -> Self {
        self.workspace_id = Some(workspace_id.into());
        self
    }

    #[must_use]
    pub fn workspace_path(mut self, workspace_path: impl Into<String>) -> Self {
        self.workspace_path = Some(workspace_path.into());
        self
    }

    #[must_use]
    pub fn export_scope(mut self, export_scope: ExportScope) -> Self {
        self.export_scope = export_scope;
        self
    }

    #[must_use]
    pub fn redaction_level(mut self, redaction_level: RedactionLevel) -> Self {
        self.redaction_level = redaction_level;
        self
    }

    #[must_use]
    pub fn record_count(mut self, record_count: u64) -> Self {
        self.record_count = Some(record_count);
        self
    }

    #[must_use]
    pub fn ee_version(mut self, ee_version: impl Into<String>) -> Self {
        self.ee_version = Some(ee_version.into());
        self
    }

    #[must_use]
    pub fn hostname(mut self, hostname: impl Into<String>) -> Self {
        self.hostname = Some(hostname.into());
        self
    }

    #[must_use]
    pub fn export_id(mut self, export_id: impl Into<String>) -> Self {
        self.export_id = Some(export_id.into());
        self
    }

    #[must_use]
    pub fn build(self) -> ExportHeader {
        ExportHeader {
            schema: EXPORT_HEADER_SCHEMA_V1.to_owned(),
            format_version: EXPORT_FORMAT_VERSION,
            created_at: self.created_at.unwrap_or_default(),
            workspace_id: self.workspace_id,
            workspace_path: self.workspace_path,
            export_scope: self.export_scope,
            redaction_level: self.redaction_level,
            record_count: self.record_count,
            ee_version: self.ee_version.unwrap_or_default(),
            hostname: self.hostname,
            export_id: self.export_id.unwrap_or_default(),
        }
    }
}

/// Export footer record.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ExportFooter {
    pub schema: String,
    pub export_id: String,
    pub completed_at: String,
    pub total_records: u64,
    pub memory_count: u64,
    pub link_count: u64,
    pub tag_count: u64,
    pub audit_count: u64,
    pub checksum: Option<String>,
    pub success: bool,
    pub error_message: Option<String>,
}

impl ExportFooter {
    #[must_use]
    pub fn builder() -> ExportFooterBuilder {
        ExportFooterBuilder::default()
    }
}

#[derive(Clone, Debug, Default)]
pub struct ExportFooterBuilder {
    export_id: Option<String>,
    completed_at: Option<String>,
    total_records: u64,
    memory_count: u64,
    link_count: u64,
    tag_count: u64,
    audit_count: u64,
    checksum: Option<String>,
    success: bool,
    error_message: Option<String>,
}

impl ExportFooterBuilder {
    #[must_use]
    pub fn export_id(mut self, export_id: impl Into<String>) -> Self {
        self.export_id = Some(export_id.into());
        self
    }

    #[must_use]
    pub fn completed_at(mut self, completed_at: impl Into<String>) -> Self {
        self.completed_at = Some(completed_at.into());
        self
    }

    #[must_use]
    pub fn total_records(mut self, total_records: u64) -> Self {
        self.total_records = total_records;
        self
    }

    #[must_use]
    pub fn memory_count(mut self, memory_count: u64) -> Self {
        self.memory_count = memory_count;
        self
    }

    #[must_use]
    pub fn link_count(mut self, link_count: u64) -> Self {
        self.link_count = link_count;
        self
    }

    #[must_use]
    pub fn tag_count(mut self, tag_count: u64) -> Self {
        self.tag_count = tag_count;
        self
    }

    #[must_use]
    pub fn audit_count(mut self, audit_count: u64) -> Self {
        self.audit_count = audit_count;
        self
    }

    #[must_use]
    pub fn checksum(mut self, checksum: impl Into<String>) -> Self {
        self.checksum = Some(checksum.into());
        self
    }

    #[must_use]
    pub fn success(mut self, success: bool) -> Self {
        self.success = success;
        self
    }

    #[must_use]
    pub fn error_message(mut self, error_message: impl Into<String>) -> Self {
        self.error_message = Some(error_message.into());
        self
    }

    #[must_use]
    pub fn build(self) -> ExportFooter {
        ExportFooter {
            schema: EXPORT_FOOTER_SCHEMA_V1.to_owned(),
            export_id: self.export_id.unwrap_or_default(),
            completed_at: self.completed_at.unwrap_or_default(),
            total_records: self.total_records,
            memory_count: self.memory_count,
            link_count: self.link_count,
            tag_count: self.tag_count,
            audit_count: self.audit_count,
            checksum: self.checksum,
            success: self.success,
            error_message: self.error_message,
        }
    }
}

/// Export memory record.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ExportMemoryRecord {
    pub schema: String,
    pub memory_id: String,
    pub workspace_id: String,
    pub level: String,
    pub kind: String,
    pub content: String,
    pub importance: Option<f64>,
    pub confidence: Option<f64>,
    pub utility: Option<f64>,
    pub created_at: String,
    pub updated_at: Option<String>,
    pub expires_at: Option<String>,
    pub source_agent: Option<String>,
    pub provenance_uri: Option<String>,
    pub superseded_by: Option<String>,
    pub supersedes: Option<String>,
    pub redacted: bool,
    pub redaction_reason: Option<String>,
}

impl ExportMemoryRecord {
    #[must_use]
    pub fn builder() -> ExportMemoryRecordBuilder {
        ExportMemoryRecordBuilder::default()
    }
}

#[derive(Clone, Debug, Default)]
pub struct ExportMemoryRecordBuilder {
    memory_id: Option<String>,
    workspace_id: Option<String>,
    level: Option<String>,
    kind: Option<String>,
    content: Option<String>,
    importance: Option<f64>,
    confidence: Option<f64>,
    utility: Option<f64>,
    created_at: Option<String>,
    updated_at: Option<String>,
    expires_at: Option<String>,
    source_agent: Option<String>,
    provenance_uri: Option<String>,
    superseded_by: Option<String>,
    supersedes: Option<String>,
    redacted: bool,
    redaction_reason: Option<String>,
}

impl ExportMemoryRecordBuilder {
    #[must_use]
    pub fn memory_id(mut self, memory_id: impl Into<String>) -> Self {
        self.memory_id = Some(memory_id.into());
        self
    }

    #[must_use]
    pub fn workspace_id(mut self, workspace_id: impl Into<String>) -> Self {
        self.workspace_id = Some(workspace_id.into());
        self
    }

    #[must_use]
    pub fn level(mut self, level: impl Into<String>) -> Self {
        self.level = Some(level.into());
        self
    }

    #[must_use]
    pub fn kind(mut self, kind: impl Into<String>) -> Self {
        self.kind = Some(kind.into());
        self
    }

    #[must_use]
    pub fn content(mut self, content: impl Into<String>) -> Self {
        self.content = Some(content.into());
        self
    }

    #[must_use]
    pub fn importance(mut self, importance: f64) -> Self {
        self.importance = Some(importance);
        self
    }

    #[must_use]
    pub fn confidence(mut self, confidence: f64) -> Self {
        self.confidence = Some(confidence);
        self
    }

    #[must_use]
    pub fn utility(mut self, utility: f64) -> Self {
        self.utility = Some(utility);
        self
    }

    #[must_use]
    pub fn created_at(mut self, created_at: impl Into<String>) -> Self {
        self.created_at = Some(created_at.into());
        self
    }

    #[must_use]
    pub fn updated_at(mut self, updated_at: impl Into<String>) -> Self {
        self.updated_at = Some(updated_at.into());
        self
    }

    #[must_use]
    pub fn expires_at(mut self, expires_at: impl Into<String>) -> Self {
        self.expires_at = Some(expires_at.into());
        self
    }

    #[must_use]
    pub fn source_agent(mut self, source_agent: impl Into<String>) -> Self {
        self.source_agent = Some(source_agent.into());
        self
    }

    #[must_use]
    pub fn provenance_uri(mut self, provenance_uri: impl Into<String>) -> Self {
        self.provenance_uri = Some(provenance_uri.into());
        self
    }

    #[must_use]
    pub fn superseded_by(mut self, superseded_by: impl Into<String>) -> Self {
        self.superseded_by = Some(superseded_by.into());
        self
    }

    #[must_use]
    pub fn supersedes(mut self, supersedes: impl Into<String>) -> Self {
        self.supersedes = Some(supersedes.into());
        self
    }

    #[must_use]
    pub fn redacted(mut self, redacted: bool) -> Self {
        self.redacted = redacted;
        self
    }

    #[must_use]
    pub fn redaction_reason(mut self, redaction_reason: impl Into<String>) -> Self {
        self.redaction_reason = Some(redaction_reason.into());
        self
    }

    #[must_use]
    pub fn build(self) -> ExportMemoryRecord {
        ExportMemoryRecord {
            schema: EXPORT_MEMORY_SCHEMA_V1.to_owned(),
            memory_id: self.memory_id.unwrap_or_default(),
            workspace_id: self.workspace_id.unwrap_or_default(),
            level: self.level.unwrap_or_default(),
            kind: self.kind.unwrap_or_default(),
            content: self.content.unwrap_or_default(),
            importance: self.importance,
            confidence: self.confidence,
            utility: self.utility,
            created_at: self.created_at.unwrap_or_default(),
            updated_at: self.updated_at,
            expires_at: self.expires_at,
            source_agent: self.source_agent,
            provenance_uri: self.provenance_uri,
            superseded_by: self.superseded_by,
            supersedes: self.supersedes,
            redacted: self.redacted,
            redaction_reason: self.redaction_reason,
        }
    }
}

/// Export link record (memory relationships).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ExportLinkRecord {
    pub schema: String,
    pub link_id: String,
    pub source_memory_id: String,
    pub target_memory_id: String,
    pub link_type: String,
    pub weight: Option<f64>,
    pub created_at: String,
    pub metadata: Option<serde_json::Value>,
}

impl ExportLinkRecord {
    #[must_use]
    pub fn builder() -> ExportLinkRecordBuilder {
        ExportLinkRecordBuilder::default()
    }
}

#[derive(Clone, Debug, Default)]
pub struct ExportLinkRecordBuilder {
    link_id: Option<String>,
    source_memory_id: Option<String>,
    target_memory_id: Option<String>,
    link_type: Option<String>,
    weight: Option<f64>,
    created_at: Option<String>,
    metadata: Option<serde_json::Value>,
}

impl ExportLinkRecordBuilder {
    #[must_use]
    pub fn link_id(mut self, link_id: impl Into<String>) -> Self {
        self.link_id = Some(link_id.into());
        self
    }

    #[must_use]
    pub fn source_memory_id(mut self, source_memory_id: impl Into<String>) -> Self {
        self.source_memory_id = Some(source_memory_id.into());
        self
    }

    #[must_use]
    pub fn target_memory_id(mut self, target_memory_id: impl Into<String>) -> Self {
        self.target_memory_id = Some(target_memory_id.into());
        self
    }

    #[must_use]
    pub fn link_type(mut self, link_type: impl Into<String>) -> Self {
        self.link_type = Some(link_type.into());
        self
    }

    #[must_use]
    pub fn weight(mut self, weight: f64) -> Self {
        self.weight = Some(weight);
        self
    }

    #[must_use]
    pub fn created_at(mut self, created_at: impl Into<String>) -> Self {
        self.created_at = Some(created_at.into());
        self
    }

    #[must_use]
    pub fn metadata(mut self, metadata: serde_json::Value) -> Self {
        self.metadata = Some(metadata);
        self
    }

    #[must_use]
    pub fn build(self) -> ExportLinkRecord {
        ExportLinkRecord {
            schema: EXPORT_LINK_SCHEMA_V1.to_owned(),
            link_id: self.link_id.unwrap_or_default(),
            source_memory_id: self.source_memory_id.unwrap_or_default(),
            target_memory_id: self.target_memory_id.unwrap_or_default(),
            link_type: self.link_type.unwrap_or_default(),
            weight: self.weight,
            created_at: self.created_at.unwrap_or_default(),
            metadata: self.metadata,
        }
    }
}

/// Export tag record.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ExportTagRecord {
    pub schema: String,
    pub memory_id: String,
    pub tag: String,
    pub created_at: String,
}

impl ExportTagRecord {
    #[must_use]
    pub fn new(
        memory_id: impl Into<String>,
        tag: impl Into<String>,
        created_at: impl Into<String>,
    ) -> Self {
        Self {
            schema: EXPORT_TAG_SCHEMA_V1.to_owned(),
            memory_id: memory_id.into(),
            tag: tag.into(),
            created_at: created_at.into(),
        }
    }
}

/// Export audit record.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ExportAuditRecord {
    pub schema: String,
    pub audit_id: String,
    pub operation: String,
    pub target_type: String,
    pub target_id: String,
    pub performed_at: String,
    pub performed_by: Option<String>,
    pub details: Option<serde_json::Value>,
}

impl ExportAuditRecord {
    #[must_use]
    pub fn builder() -> ExportAuditRecordBuilder {
        ExportAuditRecordBuilder::default()
    }
}

#[derive(Clone, Debug, Default)]
pub struct ExportAuditRecordBuilder {
    audit_id: Option<String>,
    operation: Option<String>,
    target_type: Option<String>,
    target_id: Option<String>,
    performed_at: Option<String>,
    performed_by: Option<String>,
    details: Option<serde_json::Value>,
}

impl ExportAuditRecordBuilder {
    #[must_use]
    pub fn audit_id(mut self, audit_id: impl Into<String>) -> Self {
        self.audit_id = Some(audit_id.into());
        self
    }

    #[must_use]
    pub fn operation(mut self, operation: impl Into<String>) -> Self {
        self.operation = Some(operation.into());
        self
    }

    #[must_use]
    pub fn target_type(mut self, target_type: impl Into<String>) -> Self {
        self.target_type = Some(target_type.into());
        self
    }

    #[must_use]
    pub fn target_id(mut self, target_id: impl Into<String>) -> Self {
        self.target_id = Some(target_id.into());
        self
    }

    #[must_use]
    pub fn performed_at(mut self, performed_at: impl Into<String>) -> Self {
        self.performed_at = Some(performed_at.into());
        self
    }

    #[must_use]
    pub fn performed_by(mut self, performed_by: impl Into<String>) -> Self {
        self.performed_by = Some(performed_by.into());
        self
    }

    #[must_use]
    pub fn details(mut self, details: serde_json::Value) -> Self {
        self.details = Some(details);
        self
    }

    #[must_use]
    pub fn build(self) -> ExportAuditRecord {
        ExportAuditRecord {
            schema: EXPORT_AUDIT_SCHEMA_V1.to_owned(),
            audit_id: self.audit_id.unwrap_or_default(),
            operation: self.operation.unwrap_or_default(),
            target_type: self.target_type.unwrap_or_default(),
            target_id: self.target_id.unwrap_or_default(),
            performed_at: self.performed_at.unwrap_or_default(),
            performed_by: self.performed_by,
            details: self.details,
        }
    }
}

/// Export workspace record.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ExportWorkspaceRecord {
    pub schema: String,
    pub workspace_id: String,
    pub path: String,
    pub name: Option<String>,
    pub created_at: String,
    pub last_accessed: Option<String>,
}

impl ExportWorkspaceRecord {
    #[must_use]
    pub fn builder() -> ExportWorkspaceRecordBuilder {
        ExportWorkspaceRecordBuilder::default()
    }
}

#[derive(Clone, Debug, Default)]
pub struct ExportWorkspaceRecordBuilder {
    workspace_id: Option<String>,
    path: Option<String>,
    name: Option<String>,
    created_at: Option<String>,
    last_accessed: Option<String>,
}

impl ExportWorkspaceRecordBuilder {
    #[must_use]
    pub fn workspace_id(mut self, workspace_id: impl Into<String>) -> Self {
        self.workspace_id = Some(workspace_id.into());
        self
    }

    #[must_use]
    pub fn path(mut self, path: impl Into<String>) -> Self {
        self.path = Some(path.into());
        self
    }

    #[must_use]
    pub fn name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    #[must_use]
    pub fn created_at(mut self, created_at: impl Into<String>) -> Self {
        self.created_at = Some(created_at.into());
        self
    }

    #[must_use]
    pub fn last_accessed(mut self, last_accessed: impl Into<String>) -> Self {
        self.last_accessed = Some(last_accessed.into());
        self
    }

    #[must_use]
    pub fn build(self) -> ExportWorkspaceRecord {
        ExportWorkspaceRecord {
            schema: EXPORT_WORKSPACE_SCHEMA_V1.to_owned(),
            workspace_id: self.workspace_id.unwrap_or_default(),
            path: self.path.unwrap_or_default(),
            name: self.name,
            created_at: self.created_at.unwrap_or_default(),
            last_accessed: self.last_accessed,
        }
    }
}

/// Export agent record.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ExportAgentRecord {
    pub schema: String,
    pub agent_id: String,
    pub name: String,
    pub program: Option<String>,
    pub model: Option<String>,
    pub created_at: String,
    pub last_seen: Option<String>,
}

impl ExportAgentRecord {
    #[must_use]
    pub fn builder() -> ExportAgentRecordBuilder {
        ExportAgentRecordBuilder::default()
    }
}

#[derive(Clone, Debug, Default)]
pub struct ExportAgentRecordBuilder {
    agent_id: Option<String>,
    name: Option<String>,
    program: Option<String>,
    model: Option<String>,
    created_at: Option<String>,
    last_seen: Option<String>,
}

impl ExportAgentRecordBuilder {
    #[must_use]
    pub fn agent_id(mut self, agent_id: impl Into<String>) -> Self {
        self.agent_id = Some(agent_id.into());
        self
    }

    #[must_use]
    pub fn name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    #[must_use]
    pub fn program(mut self, program: impl Into<String>) -> Self {
        self.program = Some(program.into());
        self
    }

    #[must_use]
    pub fn model(mut self, model: impl Into<String>) -> Self {
        self.model = Some(model.into());
        self
    }

    #[must_use]
    pub fn created_at(mut self, created_at: impl Into<String>) -> Self {
        self.created_at = Some(created_at.into());
        self
    }

    #[must_use]
    pub fn last_seen(mut self, last_seen: impl Into<String>) -> Self {
        self.last_seen = Some(last_seen.into());
        self
    }

    #[must_use]
    pub fn build(self) -> ExportAgentRecord {
        ExportAgentRecord {
            schema: EXPORT_AGENT_SCHEMA_V1.to_owned(),
            agent_id: self.agent_id.unwrap_or_default(),
            name: self.name.unwrap_or_default(),
            program: self.program,
            model: self.model,
            created_at: self.created_at.unwrap_or_default(),
            last_seen: self.last_seen,
        }
    }
}

/// Typed union of all export record types.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ExportRecord {
    Header(ExportHeader),
    Memory(ExportMemoryRecord),
    Link(ExportLinkRecord),
    Tag(ExportTagRecord),
    Agent(ExportAgentRecord),
    Workspace(ExportWorkspaceRecord),
    Audit(ExportAuditRecord),
    Footer(ExportFooter),
}

impl ExportRecord {
    #[must_use]
    pub fn record_type(&self) -> ExportRecordType {
        match self {
            Self::Header(_) => ExportRecordType::Header,
            Self::Memory(_) => ExportRecordType::Memory,
            Self::Link(_) => ExportRecordType::Link,
            Self::Tag(_) => ExportRecordType::Tag,
            Self::Agent(_) => ExportRecordType::Agent,
            Self::Workspace(_) => ExportRecordType::Workspace,
            Self::Audit(_) => ExportRecordType::Audit,
            Self::Footer(_) => ExportRecordType::Footer,
        }
    }

    #[must_use]
    pub fn schema(&self) -> &str {
        match self {
            Self::Header(h) => &h.schema,
            Self::Memory(m) => &m.schema,
            Self::Link(l) => &l.schema,
            Self::Tag(t) => &t.schema,
            Self::Agent(a) => &a.schema,
            Self::Workspace(w) => &w.schema,
            Self::Audit(a) => &a.schema,
            Self::Footer(f) => &f.schema,
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    type TestResult = Result<(), String>;

    fn ensure<T: std::fmt::Debug + PartialEq>(actual: T, expected: T, ctx: &str) -> TestResult {
        if actual == expected {
            Ok(())
        } else {
            Err(format!("{ctx}: expected {expected:?}, got {actual:?}"))
        }
    }

    #[test]
    fn export_record_type_roundtrip() -> TestResult {
        for rt in ExportRecordType::all() {
            let s = rt.as_str();
            let parsed: ExportRecordType = s
                .parse()
                .map_err(|e: ParseExportRecordTypeError| e.to_string())?;
            ensure(parsed, *rt, &format!("roundtrip {s}"))?;
        }
        Ok(())
    }

    #[test]
    fn export_record_type_display() {
        assert_eq!(ExportRecordType::Header.to_string(), "header");
        assert_eq!(ExportRecordType::Memory.to_string(), "memory");
        assert_eq!(ExportRecordType::Footer.to_string(), "footer");
    }

    #[test]
    fn export_record_type_schema_mapping() {
        assert_eq!(ExportRecordType::Header.schema(), EXPORT_HEADER_SCHEMA_V1);
        assert_eq!(ExportRecordType::Memory.schema(), EXPORT_MEMORY_SCHEMA_V1);
        assert_eq!(ExportRecordType::Footer.schema(), EXPORT_FOOTER_SCHEMA_V1);
        assert_eq!(ExportRecordType::Audit.schema(), EXPORT_AUDIT_SCHEMA_V1);
        assert_eq!(ExportRecordType::Link.schema(), EXPORT_LINK_SCHEMA_V1);
        assert_eq!(ExportRecordType::Tag.schema(), EXPORT_TAG_SCHEMA_V1);
        assert_eq!(ExportRecordType::Agent.schema(), EXPORT_AGENT_SCHEMA_V1);
        assert_eq!(
            ExportRecordType::Workspace.schema(),
            EXPORT_WORKSPACE_SCHEMA_V1
        );
    }

    #[test]
    fn redaction_level_roundtrip() -> TestResult {
        for level in RedactionLevel::all() {
            let s = level.as_str();
            let parsed: RedactionLevel = s
                .parse()
                .map_err(|e: ParseRedactionLevelError| e.to_string())?;
            ensure(parsed, *level, &format!("roundtrip {s}"))?;
        }
        Ok(())
    }

    #[test]
    fn redaction_level_capabilities() {
        assert!(!RedactionLevel::None.redacts_secrets());
        assert!(RedactionLevel::Minimal.redacts_secrets());
        assert!(!RedactionLevel::Minimal.redacts_paths());
        assert!(RedactionLevel::Standard.redacts_paths());
        assert!(RedactionLevel::Standard.redacts_identifiers());
        assert!(!RedactionLevel::Standard.redacts_content());
        assert!(RedactionLevel::Full.redacts_content());
    }

    #[test]
    fn export_scope_roundtrip() -> TestResult {
        for scope in ExportScope::all() {
            let s = scope.as_str();
            let parsed: ExportScope = s
                .parse()
                .map_err(|e: ParseExportScopeError| e.to_string())?;
            ensure(parsed, *scope, &format!("roundtrip {s}"))?;
        }
        Ok(())
    }

    #[test]
    fn export_scope_includes_checks() {
        assert!(ExportScope::All.includes_memories());
        assert!(ExportScope::All.includes_audit());
        assert!(ExportScope::All.includes_links());
        assert!(ExportScope::All.includes_content());

        assert!(ExportScope::Memories.includes_memories());
        assert!(!ExportScope::Memories.includes_audit());
        assert!(!ExportScope::Memories.includes_links());

        assert!(!ExportScope::Audit.includes_memories());
        assert!(ExportScope::Audit.includes_audit());

        assert!(ExportScope::MetadataOnly.includes_memories());
        assert!(!ExportScope::MetadataOnly.includes_content());
    }

    #[test]
    fn export_header_builder() {
        let header = ExportHeader::builder()
            .created_at("2026-04-30T12:00:00Z")
            .workspace_id("ws-123")
            .export_scope(ExportScope::Memories)
            .redaction_level(RedactionLevel::Standard)
            .record_count(42)
            .ee_version("0.1.0")
            .export_id("exp-001")
            .build();

        assert_eq!(header.schema, EXPORT_HEADER_SCHEMA_V1);
        assert_eq!(header.format_version, EXPORT_FORMAT_VERSION);
        assert_eq!(header.created_at, "2026-04-30T12:00:00Z");
        assert_eq!(header.workspace_id, Some("ws-123".to_owned()));
        assert_eq!(header.export_scope, ExportScope::Memories);
        assert_eq!(header.redaction_level, RedactionLevel::Standard);
        assert_eq!(header.record_count, Some(42));
        assert_eq!(header.ee_version, "0.1.0");
        assert_eq!(header.export_id, "exp-001");
    }

    #[test]
    fn export_footer_builder() {
        let footer = ExportFooter::builder()
            .export_id("exp-001")
            .completed_at("2026-04-30T12:01:00Z")
            .total_records(100)
            .memory_count(50)
            .link_count(20)
            .tag_count(25)
            .audit_count(5)
            .checksum("abc123")
            .success(true)
            .build();

        assert_eq!(footer.schema, EXPORT_FOOTER_SCHEMA_V1);
        assert_eq!(footer.export_id, "exp-001");
        assert_eq!(footer.total_records, 100);
        assert_eq!(footer.memory_count, 50);
        assert!(footer.success);
        assert_eq!(footer.checksum, Some("abc123".to_owned()));
    }

    #[test]
    fn export_memory_record_builder() {
        let memory = ExportMemoryRecord::builder()
            .memory_id("mem-001")
            .workspace_id("ws-123")
            .level("procedural")
            .kind("rule")
            .content("Always run tests before commit")
            .importance(0.8)
            .confidence(0.9)
            .created_at("2026-04-30T12:00:00Z")
            .source_agent("claude-code")
            .redacted(false)
            .build();

        assert_eq!(memory.schema, EXPORT_MEMORY_SCHEMA_V1);
        assert_eq!(memory.memory_id, "mem-001");
        assert_eq!(memory.level, "procedural");
        assert_eq!(memory.kind, "rule");
        assert_eq!(memory.importance, Some(0.8));
        assert!(!memory.redacted);
    }

    #[test]
    fn export_link_record_builder() {
        let link = ExportLinkRecord::builder()
            .link_id("lnk-001")
            .source_memory_id("mem-001")
            .target_memory_id("mem-002")
            .link_type("supports")
            .weight(0.7)
            .created_at("2026-04-30T12:00:00Z")
            .build();

        assert_eq!(link.schema, EXPORT_LINK_SCHEMA_V1);
        assert_eq!(link.link_id, "lnk-001");
        assert_eq!(link.link_type, "supports");
        assert_eq!(link.weight, Some(0.7));
    }

    #[test]
    fn export_tag_record_new() {
        let tag = ExportTagRecord::new("mem-001", "important", "2026-04-30T12:00:00Z");
        assert_eq!(tag.schema, EXPORT_TAG_SCHEMA_V1);
        assert_eq!(tag.memory_id, "mem-001");
        assert_eq!(tag.tag, "important");
    }

    #[test]
    fn export_audit_record_builder() {
        let audit = ExportAuditRecord::builder()
            .audit_id("aud-001")
            .operation("create")
            .target_type("memory")
            .target_id("mem-001")
            .performed_at("2026-04-30T12:00:00Z")
            .performed_by("claude-code")
            .build();

        assert_eq!(audit.schema, EXPORT_AUDIT_SCHEMA_V1);
        assert_eq!(audit.audit_id, "aud-001");
        assert_eq!(audit.operation, "create");
        assert_eq!(audit.performed_by, Some("claude-code".to_owned()));
    }

    #[test]
    fn export_workspace_record_builder() {
        let workspace = ExportWorkspaceRecord::builder()
            .workspace_id("ws-123")
            .path("/home/user/project")
            .name("My Project")
            .created_at("2026-04-30T12:00:00Z")
            .build();

        assert_eq!(workspace.schema, EXPORT_WORKSPACE_SCHEMA_V1);
        assert_eq!(workspace.workspace_id, "ws-123");
        assert_eq!(workspace.path, "/home/user/project");
        assert_eq!(workspace.name, Some("My Project".to_owned()));
    }

    #[test]
    fn export_agent_record_builder() {
        let agent = ExportAgentRecord::builder()
            .agent_id("agt-001")
            .name("claude-code")
            .program("Claude Code")
            .model("claude-opus-4-5-20251101")
            .created_at("2026-04-30T12:00:00Z")
            .build();

        assert_eq!(agent.schema, EXPORT_AGENT_SCHEMA_V1);
        assert_eq!(agent.agent_id, "agt-001");
        assert_eq!(agent.name, "claude-code");
        assert_eq!(agent.program, Some("Claude Code".to_owned()));
    }

    #[test]
    fn export_record_union_type_detection() {
        let header = ExportRecord::Header(ExportHeader::builder().build());
        assert_eq!(header.record_type(), ExportRecordType::Header);
        assert_eq!(header.schema(), EXPORT_HEADER_SCHEMA_V1);

        let memory = ExportRecord::Memory(ExportMemoryRecord::builder().build());
        assert_eq!(memory.record_type(), ExportRecordType::Memory);
        assert_eq!(memory.schema(), EXPORT_MEMORY_SCHEMA_V1);

        let footer = ExportRecord::Footer(ExportFooter::builder().build());
        assert_eq!(footer.record_type(), ExportRecordType::Footer);
        assert_eq!(footer.schema(), EXPORT_FOOTER_SCHEMA_V1);
    }

    #[test]
    fn header_serializes_to_json() {
        let header = ExportHeader::builder()
            .created_at("2026-04-30T12:00:00Z")
            .ee_version("0.1.0")
            .export_id("test-export")
            .build();

        let json = serde_json::to_string(&header).expect("serialize");
        assert!(json.contains(r#""schema":"ee.export.header.v1""#));
        assert!(json.contains(r#""format_version":1"#));
        assert!(json.contains(r#""created_at":"2026-04-30T12:00:00Z""#));
    }

    #[test]
    fn memory_record_deserializes_from_json() {
        let json = r#"{
            "schema": "ee.export.memory.v1",
            "memory_id": "mem-001",
            "workspace_id": "ws-123",
            "level": "procedural",
            "kind": "rule",
            "content": "Test content",
            "importance": 0.8,
            "confidence": 0.9,
            "utility": 0.7,
            "created_at": "2026-04-30T12:00:00Z",
            "redacted": false
        }"#;

        let memory: ExportMemoryRecord = serde_json::from_str(json).expect("deserialize");
        assert_eq!(memory.schema, EXPORT_MEMORY_SCHEMA_V1);
        assert_eq!(memory.memory_id, "mem-001");
        assert_eq!(memory.importance, Some(0.8));
        assert!(!memory.redacted);
    }

    #[test]
    fn all_export_schemas_follow_naming_convention() {
        for schema in ALL_EXPORT_SCHEMAS {
            assert!(
                schema.starts_with("ee.export.") && schema.ends_with(".v1"),
                "schema {schema} should follow ee.export.<type>.v1 pattern"
            );
        }
    }

    #[test]
    fn parse_invalid_export_record_type_error() {
        let result: Result<ExportRecordType, _> = "invalid".parse();
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("invalid export record type"));
        assert!(err.to_string().contains("'invalid'"));
    }

    #[test]
    fn parse_invalid_redaction_level_error() {
        let result: Result<RedactionLevel, _> = "invalid".parse();
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("invalid redaction level"));
    }

    #[test]
    fn parse_invalid_export_scope_error() {
        let result: Result<ExportScope, _> = "invalid".parse();
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("invalid export scope"));
    }
}
