//! Typed memory anchors extracted from durable memories.
//!
//! Anchors are intentionally metadata-only. Raw anchor values never become
//! durable payloads; callers store a domain-separated BLAKE3 hash plus a short
//! redacted display token.

use std::collections::BTreeMap;
use std::fmt;

pub const MEMORY_ANCHOR_SCHEMA_V1: &str = "ee.memory_anchor.v1";

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum MemoryAnchorKind {
    Path,
    Symbol,
    Command,
    EnvVar,
    Schema,
    DegradedCode,
    Dependency,
    ConfigKey,
}

impl MemoryAnchorKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Path => "path",
            Self::Symbol => "symbol",
            Self::Command => "command",
            Self::EnvVar => "env_var",
            Self::Schema => "schema",
            Self::DegradedCode => "degraded_code",
            Self::Dependency => "dependency",
            Self::ConfigKey => "config_key",
        }
    }

    #[must_use]
    pub fn parse(input: &str) -> Option<Self> {
        match input {
            "path" => Some(Self::Path),
            "symbol" => Some(Self::Symbol),
            "command" => Some(Self::Command),
            "env_var" => Some(Self::EnvVar),
            "schema" => Some(Self::Schema),
            "degraded_code" => Some(Self::DegradedCode),
            "dependency" => Some(Self::Dependency),
            "config_key" => Some(Self::ConfigKey),
            _ => None,
        }
    }
}

impl fmt::Display for MemoryAnchorKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum MemoryAnchorSource {
    Explicit,
    Remember,
    CassImport,
    CurateApply,
    IndexRebuild,
}

impl MemoryAnchorSource {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Explicit => "explicit",
            Self::Remember => "remember",
            Self::CassImport => "cass_import",
            Self::CurateApply => "curate_apply",
            Self::IndexRebuild => "index_rebuild",
        }
    }

    #[must_use]
    pub fn parse(input: &str) -> Option<Self> {
        match input {
            "explicit" => Some(Self::Explicit),
            "remember" => Some(Self::Remember),
            "cass_import" => Some(Self::CassImport),
            "curate_apply" => Some(Self::CurateApply),
            "index_rebuild" => Some(Self::IndexRebuild),
            _ => None,
        }
    }
}

impl fmt::Display for MemoryAnchorSource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum MemoryAnchorFreshnessState {
    Current,
    Suspect,
    Stale,
}

impl MemoryAnchorFreshnessState {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Current => "current",
            Self::Suspect => "suspect",
            Self::Stale => "stale",
        }
    }

    #[must_use]
    pub fn parse(input: &str) -> Option<Self> {
        match input {
            "current" => Some(Self::Current),
            "suspect" => Some(Self::Suspect),
            "stale" => Some(Self::Stale),
            _ => None,
        }
    }
}

impl fmt::Display for MemoryAnchorFreshnessState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct CreateMemoryAnchorInput {
    pub memory_id: String,
    pub anchor_kind: MemoryAnchorKind,
    pub anchor_value_hash: String,
    pub redacted_anchor_value: String,
    pub confidence: f32,
    pub source: MemoryAnchorSource,
    pub provenance: String,
    pub captured_span_hash: String,
    pub freshness_state: MemoryAnchorFreshnessState,
    pub generation: i64,
}

impl CreateMemoryAnchorInput {
    #[must_use]
    pub fn from_raw(
        memory_id: &str,
        anchor_kind: MemoryAnchorKind,
        raw_value: &str,
        confidence: f32,
        source: MemoryAnchorSource,
        provenance: impl Into<String>,
        generation: i64,
    ) -> Option<Self> {
        let normalized = normalize_anchor_value(anchor_kind, raw_value)?;
        let anchor_value_hash = memory_anchor_value_hash(anchor_kind, &normalized);
        let captured_span_hash = memory_anchor_span_hash(anchor_kind, &normalized);
        let redacted_anchor_value = redacted_anchor_value(anchor_kind, &anchor_value_hash);
        Some(Self {
            memory_id: memory_id.to_owned(),
            anchor_kind,
            anchor_value_hash,
            redacted_anchor_value,
            confidence: bounded_confidence(confidence),
            source,
            provenance: provenance.into(),
            captured_span_hash,
            freshness_state: MemoryAnchorFreshnessState::Current,
            generation: generation.max(0),
        })
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct StoredMemoryAnchor {
    pub memory_id: String,
    pub anchor_kind: MemoryAnchorKind,
    pub anchor_value_hash: String,
    pub redacted_anchor_value: String,
    pub confidence: f32,
    pub source: MemoryAnchorSource,
    pub provenance: String,
    pub captured_span_hash: String,
    pub freshness_state: MemoryAnchorFreshnessState,
    pub generation: i64,
    pub created_at: String,
    pub updated_at: String,
}

#[must_use]
pub fn memory_anchor_value_hash(anchor_kind: MemoryAnchorKind, normalized_value: &str) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(MEMORY_ANCHOR_SCHEMA_V1.as_bytes());
    hasher.update(b"\0kind:");
    hasher.update(anchor_kind.as_str().as_bytes());
    hasher.update(b"\0value:");
    hasher.update(normalized_value.as_bytes());
    format!("blake3:{}", hasher.finalize().to_hex())
}

#[must_use]
pub fn extract_precision_memory_anchors(
    memory_id: &str,
    content: &str,
    source: MemoryAnchorSource,
    provenance: Option<&str>,
) -> Vec<CreateMemoryAnchorInput> {
    let mut anchors = BTreeMap::new();
    let provenance = provenance.unwrap_or("memory.content");

    extract_explicit_anchors(memory_id, content, provenance, &mut anchors);
    extract_schema_anchors(memory_id, content, source, provenance, &mut anchors);

    for fragment in code_fragments(content) {
        extract_code_fragment_anchors(memory_id, fragment, source, provenance, &mut anchors);
    }

    anchors.into_values().collect()
}

fn extract_explicit_anchors(
    memory_id: &str,
    content: &str,
    provenance: &str,
    anchors: &mut BTreeMap<(MemoryAnchorKind, String), CreateMemoryAnchorInput>,
) {
    for token in content.split_whitespace() {
        let cleaned = trim_token(token);
        let Some(rest) = cleaned
            .strip_prefix("ee-anchor:")
            .or_else(|| cleaned.strip_prefix("anchor:"))
        else {
            continue;
        };
        let Some((kind, value)) = rest.split_once(':') else {
            continue;
        };
        let Some(anchor_kind) = MemoryAnchorKind::parse(kind) else {
            continue;
        };
        push_raw_anchor(
            anchors,
            memory_id,
            anchor_kind,
            value,
            1.0,
            MemoryAnchorSource::Explicit,
            provenance,
            0,
        );
    }
}

fn extract_schema_anchors(
    memory_id: &str,
    content: &str,
    source: MemoryAnchorSource,
    provenance: &str,
    anchors: &mut BTreeMap<(MemoryAnchorKind, String), CreateMemoryAnchorInput>,
) {
    for token in content.split_whitespace() {
        let cleaned = trim_token(token);
        if looks_like_schema_id(cleaned) {
            push_raw_anchor(
                anchors,
                memory_id,
                MemoryAnchorKind::Schema,
                cleaned,
                0.95,
                source,
                provenance,
                0,
            );
        }
    }
}

fn extract_code_fragment_anchors(
    memory_id: &str,
    fragment: &str,
    source: MemoryAnchorSource,
    provenance: &str,
    anchors: &mut BTreeMap<(MemoryAnchorKind, String), CreateMemoryAnchorInput>,
) {
    for line in fragment
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
    {
        let line = trim_token(line);
        if looks_like_command(line) {
            push_raw_anchor(
                anchors,
                memory_id,
                MemoryAnchorKind::Command,
                line,
                0.9,
                source,
                provenance,
                0,
            );
        }
    }

    for token in fragment.split_whitespace() {
        let cleaned = trim_token(token);
        if cleaned.is_empty() {
            continue;
        }
        if looks_like_repo_path(cleaned) {
            push_raw_anchor(
                anchors,
                memory_id,
                MemoryAnchorKind::Path,
                cleaned,
                0.92,
                source,
                provenance,
                0,
            );
        }
        if looks_like_symbol(cleaned) {
            push_raw_anchor(
                anchors,
                memory_id,
                MemoryAnchorKind::Symbol,
                cleaned,
                0.82,
                source,
                provenance,
                0,
            );
        }
        if looks_like_env_var(cleaned) {
            push_raw_anchor(
                anchors,
                memory_id,
                MemoryAnchorKind::EnvVar,
                cleaned,
                0.95,
                source,
                provenance,
                0,
            );
        }
        if looks_like_schema_id(cleaned) {
            push_raw_anchor(
                anchors,
                memory_id,
                MemoryAnchorKind::Schema,
                cleaned,
                0.95,
                source,
                provenance,
                0,
            );
        }
        if looks_like_degraded_code(cleaned) {
            push_raw_anchor(
                anchors,
                memory_id,
                MemoryAnchorKind::DegradedCode,
                cleaned,
                0.88,
                source,
                provenance,
                0,
            );
        }
        if looks_like_dependency(cleaned) {
            push_raw_anchor(
                anchors,
                memory_id,
                MemoryAnchorKind::Dependency,
                cleaned,
                0.86,
                source,
                provenance,
                0,
            );
        }
        if looks_like_config_key(cleaned) {
            push_raw_anchor(
                anchors,
                memory_id,
                MemoryAnchorKind::ConfigKey,
                cleaned,
                0.78,
                source,
                provenance,
                0,
            );
        }
    }
}

fn push_raw_anchor(
    anchors: &mut BTreeMap<(MemoryAnchorKind, String), CreateMemoryAnchorInput>,
    memory_id: &str,
    anchor_kind: MemoryAnchorKind,
    raw_value: &str,
    confidence: f32,
    source: MemoryAnchorSource,
    provenance: &str,
    generation: i64,
) {
    let Some(anchor) = CreateMemoryAnchorInput::from_raw(
        memory_id,
        anchor_kind,
        raw_value,
        confidence,
        source,
        provenance,
        generation,
    ) else {
        return;
    };
    anchors
        .entry((anchor.anchor_kind, anchor.anchor_value_hash.clone()))
        .or_insert(anchor);
}

fn code_fragments(content: &str) -> Vec<&str> {
    let mut fragments = Vec::new();
    let mut offset = 0;
    while let Some(relative) = content[offset..].find('`') {
        let marker_start = offset + relative;
        let marker_len = content[marker_start..]
            .bytes()
            .take_while(|byte| *byte == b'`')
            .count();
        let marker = &content[marker_start..marker_start + marker_len];
        let fragment_start = marker_start + marker_len;
        let Some(relative_end) = content[fragment_start..].find(marker) else {
            break;
        };
        let fragment_end = fragment_start + relative_end;
        fragments.push(&content[fragment_start..fragment_end]);
        offset = fragment_end + marker_len;
    }
    fragments
}

fn normalize_anchor_value(anchor_kind: MemoryAnchorKind, raw_value: &str) -> Option<String> {
    let cleaned = trim_token(raw_value);
    if cleaned.is_empty() {
        return None;
    }
    let normalized = match anchor_kind {
        MemoryAnchorKind::Path => normalize_path(cleaned)?,
        MemoryAnchorKind::Symbol => cleaned.trim_end_matches("()").to_owned(),
        MemoryAnchorKind::Command => normalize_command(cleaned)?,
        MemoryAnchorKind::EnvVar => {
            if looks_like_env_var(cleaned) {
                cleaned.to_ascii_uppercase()
            } else {
                return None;
            }
        }
        MemoryAnchorKind::Schema => {
            if looks_like_schema_id(cleaned) {
                cleaned.to_ascii_lowercase()
            } else {
                return None;
            }
        }
        MemoryAnchorKind::DegradedCode => {
            if looks_like_degraded_code(cleaned) {
                cleaned.to_ascii_lowercase()
            } else {
                return None;
            }
        }
        MemoryAnchorKind::Dependency => {
            if looks_like_dependency(cleaned) {
                cleaned.to_ascii_lowercase()
            } else {
                return None;
            }
        }
        MemoryAnchorKind::ConfigKey => cleaned.to_ascii_lowercase(),
    };
    (!normalized.is_empty()).then_some(normalized)
}

fn normalize_path(raw: &str) -> Option<String> {
    let without_prefix = raw.strip_prefix("./").unwrap_or(raw);
    if looks_like_repo_path(without_prefix) {
        Some(without_prefix.to_owned())
    } else {
        None
    }
}

fn normalize_command(raw: &str) -> Option<String> {
    let mut normalized = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    while normalized.ends_with(';') || normalized.ends_with('.') {
        normalized.pop();
    }
    looks_like_command(&normalized).then_some(normalized)
}

fn memory_anchor_span_hash(anchor_kind: MemoryAnchorKind, normalized_value: &str) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(MEMORY_ANCHOR_SCHEMA_V1.as_bytes());
    hasher.update(b"\0span:");
    hasher.update(anchor_kind.as_str().as_bytes());
    hasher.update(b"\0");
    hasher.update(normalized_value.as_bytes());
    format!("blake3:{}", hasher.finalize().to_hex())
}

fn redacted_anchor_value(anchor_kind: MemoryAnchorKind, anchor_value_hash: &str) -> String {
    let short_hash = anchor_value_hash
        .strip_prefix("blake3:")
        .unwrap_or(anchor_value_hash)
        .chars()
        .take(12)
        .collect::<String>();
    format!("{}:blake3:{short_hash}", anchor_kind.as_str())
}

fn bounded_confidence(confidence: f32) -> f32 {
    if confidence.is_finite() {
        confidence.clamp(0.0, 1.0)
    } else {
        0.5
    }
}

fn trim_token(token: &str) -> &str {
    token.trim_matches(|character: char| {
        matches!(
            character,
            '"' | '\'' | '(' | ')' | '[' | ']' | '{' | '}' | '<' | '>' | ',' | ';' | '.'
        )
    })
}

fn looks_like_repo_path(token: &str) -> bool {
    let token = token.strip_prefix("./").unwrap_or(token);
    if token.contains("://") || token.contains("..") || token.starts_with('/') {
        return false;
    }
    if matches!(
        token,
        "AGENTS.md" | "README.md" | "Cargo.toml" | "Cargo.lock" | "rust-toolchain.toml"
    ) {
        return true;
    }
    [
        "src/",
        "tests/",
        "docs/",
        "scripts/",
        ".beads/",
        "benches/",
        "examples/",
        "fuzz/",
    ]
    .iter()
    .any(|prefix| token.starts_with(prefix))
}

fn looks_like_symbol(token: &str) -> bool {
    let symbol = token.trim_end_matches("()");
    symbol.contains("::")
        && symbol
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '_' | ':'))
        && symbol
            .chars()
            .any(|character| character.is_ascii_alphabetic())
}

fn looks_like_command(line: &str) -> bool {
    let first = line.split_whitespace().next().unwrap_or_default();
    matches!(
        first,
        "ee" | "cargo" | "rustfmt" | "br" | "bv" | "git" | "jq" | "shellcheck"
    ) || first.starts_with("scripts/")
}

fn looks_like_env_var(token: &str) -> bool {
    token.starts_with("EE_")
        && token.len() > 3
        && token.chars().all(|character| {
            character.is_ascii_uppercase() || character.is_ascii_digit() || character == '_'
        })
}

fn looks_like_schema_id(token: &str) -> bool {
    let Some((prefix, version)) = token.rsplit_once(".v") else {
        return false;
    };
    prefix.starts_with("ee.")
        && version.chars().all(|character| character.is_ascii_digit())
        && prefix.chars().all(|character| {
            character.is_ascii_lowercase()
                || character.is_ascii_digit()
                || matches!(character, '.' | '_')
        })
}

fn looks_like_degraded_code(token: &str) -> bool {
    token.contains('_')
        && token.chars().all(|character| {
            character.is_ascii_lowercase() || character.is_ascii_digit() || character == '_'
        })
        && [
            "_unavailable",
            "_stale",
            "_blocked",
            "_required",
            "_mismatch",
            "_drift",
            "_missing",
            "_failed",
        ]
        .iter()
        .any(|suffix| token.ends_with(suffix))
}

fn looks_like_dependency(token: &str) -> bool {
    matches!(
        token,
        "asupersync"
            | "fsqlite"
            | "fsqlite-core"
            | "fsqlite-types"
            | "fsqlite-error"
            | "sqlmodel"
            | "frankensearch"
            | "fnx-runtime"
            | "fnx-classes"
            | "fnx-algorithms"
            | "tokio"
            | "tokio-util"
            | "rusqlite"
            | "sqlx"
            | "diesel"
            | "sea-orm"
            | "petgraph"
            | "hyper"
            | "axum"
            | "tower"
            | "reqwest"
    )
}

fn looks_like_config_key(token: &str) -> bool {
    token.contains('.')
        && !looks_like_schema_id(token)
        && token.chars().all(|character| {
            character.is_ascii_lowercase()
                || character.is_ascii_digit()
                || matches!(character, '.' | '_' | '-')
        })
        && token.chars().any(|character| character == '.')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extractor_finds_precise_code_and_schema_anchors() {
        let anchors = extract_precision_memory_anchors(
            "mem_01234567890123456789012345",
            "Run `cargo fmt --check` before touching `src/db/mod.rs`; envelope `ee.response.v2`; use `EE_CONTEXT_MAX_TOKENS`; watch `index_stale`.",
            MemoryAnchorSource::Remember,
            Some("test://anchor"),
        );
        let kinds = anchors
            .iter()
            .map(|anchor| anchor.anchor_kind)
            .collect::<Vec<_>>();
        assert!(kinds.contains(&MemoryAnchorKind::Command));
        assert!(kinds.contains(&MemoryAnchorKind::Path));
        assert!(kinds.contains(&MemoryAnchorKind::Schema));
        assert!(kinds.contains(&MemoryAnchorKind::EnvVar));
        assert!(kinds.contains(&MemoryAnchorKind::DegradedCode));
        assert!(
            anchors
                .iter()
                .all(|anchor| !anchor.redacted_anchor_value.contains("src/db/mod.rs"))
        );
    }

    #[test]
    fn extractor_rejects_prose_lookalikes() {
        let anchors = extract_precision_memory_anchors(
            "mem_01234567890123456789012345",
            "This prose says src slash db slash mod dot rs, cargo fmt words, and EE underscore TOKEN without exact code syntax.",
            MemoryAnchorSource::Remember,
            None,
        );
        assert!(anchors.is_empty());
    }

    #[test]
    fn explicit_anchor_syntax_works_outside_code_spans() {
        let anchors = extract_precision_memory_anchors(
            "mem_01234567890123456789012345",
            "Known durable anchor:path:src/models/memory.rs and ee-anchor:env_var:EE_PACK_TRACE.",
            MemoryAnchorSource::Remember,
            None,
        );
        let kinds = anchors
            .iter()
            .map(|anchor| anchor.anchor_kind)
            .collect::<Vec<_>>();
        assert_eq!(
            kinds,
            vec![MemoryAnchorKind::Path, MemoryAnchorKind::EnvVar]
        );
    }
}
