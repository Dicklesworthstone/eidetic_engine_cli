use serde::{Deserialize, Serialize};

pub const SYMBOL_SNAPSHOT_SCHEMA_V1: &str = "ee.symbol_snapshot.v1";
pub const SYMBOL_EVIDENCE_LINKS_SCHEMA_V1: &str = "ee.symbol_evidence_links.v1";
pub const SYMBOL_ID_PREFIX: &str = "sym_v1_";
pub const SYMBOL_EVIDENCE_LINK_ID_PREFIX: &str = "sym_link_v1_";
pub const SYMBOL_INDEX_STALE_CODE: &str = "symbol_index_stale";

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SymbolSnapshot {
    pub schema: String,
    pub workspace_root: Option<String>,
    pub generated_by: String,
    pub files: Vec<SymbolSourceFile>,
    pub symbols: Vec<SymbolRecord>,
    pub degraded: Vec<SymbolGraphDegradation>,
    pub snapshot_hash: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SymbolEvidenceLinkSet {
    pub schema: String,
    pub snapshot_hash: String,
    pub generated_by: String,
    pub source_manifest_hash: String,
    pub links: Vec<SymbolEvidenceLink>,
    pub degraded: Vec<SymbolEvidenceLinkDegradation>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SymbolEvidenceLink {
    pub link_id: String,
    pub source_kind: SymbolEvidenceSourceKind,
    pub evidence_id: String,
    pub provenance_uri: String,
    pub target_path: String,
    pub target_range: SymbolSourceRange,
    pub symbol_id: Option<String>,
    pub canonical_name: Option<String>,
    pub symbol_kind: Option<SymbolKind>,
    pub symbol_range: Option<SymbolSourceRange>,
    pub confidence: f32,
    pub resolution: SymbolEvidenceResolution,
    pub reason: SymbolEvidenceReasonCode,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SymbolEvidenceSourceKind {
    Memory,
    CassEvidence,
    Failure,
    Rule,
    Decision,
}

impl SymbolEvidenceSourceKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Memory => "memory",
            Self::CassEvidence => "cass_evidence",
            Self::Failure => "failure",
            Self::Rule => "rule",
            Self::Decision => "decision",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SymbolEvidenceResolution {
    ExactSymbol,
    ContainingSymbol,
    FileLevel,
    StaleSpan,
    Ambiguous,
    RenamedSymbol,
    DeletedSymbol,
    SourceFileMissing,
}

impl SymbolEvidenceResolution {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ExactSymbol => "exact_symbol",
            Self::ContainingSymbol => "containing_symbol",
            Self::FileLevel => "file_level",
            Self::StaleSpan => "stale_span",
            Self::Ambiguous => "ambiguous",
            Self::RenamedSymbol => "renamed_symbol",
            Self::DeletedSymbol => "deleted_symbol",
            Self::SourceFileMissing => "source_file_missing",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SymbolEvidenceReasonCode {
    ExactSymbolSpan,
    ContainingSymbolSpan,
    FileLevelNoContainingSymbol,
    StaleLineSpan,
    SourceFileMissing,
    AmbiguousContainingSymbols,
    SymbolRenamedByFingerprint,
    SymbolDeleted,
}

impl SymbolEvidenceReasonCode {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ExactSymbolSpan => "exact_symbol_span",
            Self::ContainingSymbolSpan => "containing_symbol_span",
            Self::FileLevelNoContainingSymbol => "file_level_no_containing_symbol",
            Self::StaleLineSpan => "stale_line_span",
            Self::SourceFileMissing => "source_file_missing",
            Self::AmbiguousContainingSymbols => "ambiguous_containing_symbols",
            Self::SymbolRenamedByFingerprint => "symbol_renamed_by_fingerprint",
            Self::SymbolDeleted => "symbol_deleted",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SymbolEvidenceLinkDegradation {
    pub code: SymbolEvidenceLinkDegradationCode,
    pub severity: SymbolGraphDegradationSeverity,
    pub evidence_id: String,
    pub path: Option<String>,
    pub message: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SymbolEvidenceLinkDegradationCode {
    StaleLineSpan,
    SourceFileMissing,
    AmbiguousContainingSymbols,
    SymbolRenamed,
    SymbolDeleted,
}

impl SymbolEvidenceLinkDegradationCode {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::StaleLineSpan => "stale_line_span",
            Self::SourceFileMissing => "source_file_missing",
            Self::AmbiguousContainingSymbols => "ambiguous_containing_symbols",
            Self::SymbolRenamed => "symbol_renamed",
            Self::SymbolDeleted => "symbol_deleted",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SymbolSourceFile {
    pub path: String,
    pub language: SymbolSourceLanguage,
    pub parser: SymbolParserKind,
    pub source_hash: String,
    pub byte_len: u64,
    pub symbol_count: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SymbolSourceLanguage {
    Rust,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SymbolKind {
    Module,
    Function,
    Method,
    Struct,
    Enum,
    Trait,
    Impl,
    MacroInvocation,
    JsonSchemaConstant,
    CliCommandHandler,
}

impl SymbolKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Module => "module",
            Self::Function => "function",
            Self::Method => "method",
            Self::Struct => "struct",
            Self::Enum => "enum",
            Self::Trait => "trait",
            Self::Impl => "impl",
            Self::MacroInvocation => "macro_invocation",
            Self::JsonSchemaConstant => "json_schema_constant",
            Self::CliCommandHandler => "cli_command_handler",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SymbolVisibility {
    Public,
    Restricted,
    Private,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SymbolParserKind {
    RustLexicalScanner,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SymbolRecord {
    pub id: String,
    pub kind: SymbolKind,
    pub canonical_name: String,
    pub namespace: Vec<String>,
    pub path: String,
    pub range: SymbolSourceRange,
    pub visibility: SymbolVisibility,
    pub declaration_hash: String,
    pub rename_fingerprint: String,
    pub parser: SymbolParserKind,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SymbolSourceRange {
    pub start_line: u32,
    pub start_column: u32,
    pub end_line: u32,
    pub end_column: u32,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SymbolGraphDegradation {
    pub code: SymbolGraphDegradationCode,
    pub severity: SymbolGraphDegradationSeverity,
    pub path: Option<String>,
    pub message: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SymbolGraphDegradationCode {
    SourceMissing,
    SourceNonRegular,
    SourceTooLarge,
    SourceUnreadable,
    SourceUnparsable,
    SymbolIndexStale,
}

impl SymbolGraphDegradationCode {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SourceMissing => "source_missing",
            Self::SourceNonRegular => "source_non_regular",
            Self::SourceTooLarge => "source_too_large",
            Self::SourceUnreadable => "source_unreadable",
            Self::SourceUnparsable => "source_unparsable",
            Self::SymbolIndexStale => SYMBOL_INDEX_STALE_CODE,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SymbolGraphDegradationSeverity {
    Warning,
    Medium,
}
