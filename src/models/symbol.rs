use serde::{Deserialize, Serialize};

pub const SYMBOL_SNAPSHOT_SCHEMA_V1: &str = "ee.symbol_snapshot.v1";
pub const SYMBOL_ID_PREFIX: &str = "sym_v1_";

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
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SymbolGraphDegradationSeverity {
    Warning,
    Medium,
}
