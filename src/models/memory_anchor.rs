//! Typed memory anchors extracted from durable memories.
//!
//! Anchors are intentionally metadata-only. Raw anchor values never become
//! durable payloads; callers store a domain-separated BLAKE3 hash plus a short
//! redacted display token.
//!
//! One scoped exception (ADR 0064): the derived, rebuildable
//! `memory_anchor_index` reverse index persists the *normalized* value for
//! `Path` and `Symbol` anchors only — workspace-relative repo paths and code
//! identifiers, never commands, env vars, or free text — because glob/exact
//! reverse lookup cannot run against hashes. [`extract_memory_anchor_surfaces`]
//! is the single extraction walk both consumers share, so the search-document
//! metadata and the reverse index cannot drift from each other.

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

    /// Ordinal staleness rank: `Current` (0) < `Suspect` (1) < `Stale` (2).
    /// A higher rank means the anchored memory is less fresh.
    #[must_use]
    pub const fn rank(self) -> u8 {
        match self {
            Self::Current => 0,
            Self::Suspect => 1,
            Self::Stale => 2,
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
        Self::from_raw_with_normalized(
            memory_id,
            anchor_kind,
            raw_value,
            confidence,
            source,
            provenance,
            generation,
        )
        .map(|(anchor, _normalized)| anchor)
    }

    /// Like [`Self::from_raw`], but also returns the normalized anchor value
    /// the hash was computed over. The normalized value is what the ADR 0064
    /// reverse index persists for `Path`/`Symbol` anchors; it never enters the
    /// `memory_anchors` row itself.
    #[must_use]
    pub fn from_raw_with_normalized(
        memory_id: &str,
        anchor_kind: MemoryAnchorKind,
        raw_value: &str,
        confidence: f32,
        source: MemoryAnchorSource,
        provenance: impl Into<String>,
        generation: i64,
    ) -> Option<(Self, String)> {
        let normalized = normalize_anchor_value(anchor_kind, raw_value)?;
        let anchor_value_hash = memory_anchor_value_hash(anchor_kind, &normalized);
        let captured_span_hash = memory_anchor_span_hash(anchor_kind, &normalized);
        let redacted_anchor_value = redacted_anchor_value(anchor_kind, &anchor_value_hash);
        Some((
            Self {
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
            },
            normalized,
        ))
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

/// Canonical details schema for `memory.freshness_transition` audit rows,
/// mirroring `ee.audit.memory_level_transition.v1`. Every freshness change a
/// drift check applies is durably explained by one of these payloads.
pub const MEMORY_ANCHOR_FRESHNESS_TRANSITION_SCHEMA_V1: &str =
    "ee.audit.memory_anchor_freshness_transition.v1";

/// One audited memory-anchor freshness transition.
///
/// This is the freshness-side mirror of the `memory.level_transition` audit:
/// when a code-coupled drift check (reusing `src/core/symbol_graph.rs`)
/// observes that an anchored symbol changed, disappeared, or could not be
/// resolved, it records the freshness-state transition rather than silently
/// re-ranking. It is **redaction-safe**: it carries only the anchor's
/// domain-separated `anchor_value_hash`, never the raw anchor value.
#[derive(Clone, Debug, PartialEq)]
pub struct MemoryAnchorFreshnessTransition {
    pub memory_id: String,
    pub anchor_kind: MemoryAnchorKind,
    pub anchor_value_hash: String,
    pub previous_state: MemoryAnchorFreshnessState,
    pub new_state: MemoryAnchorFreshnessState,
    /// Drift classification when drift-driven, e.g. `memory_drift_source_changed`,
    /// `memory_drift_source_missing`, or `memory_drift_source_unverifiable`.
    /// `None` for non-drift transitions such as an explicit revalidation.
    pub drift_code: Option<String>,
    /// Live `file:line` of the resolved symbol at detection time, when known.
    /// Refactor ambiguity (rename/move) stays `None` — never asserted as drift.
    pub file_line: Option<String>,
    pub reason: String,
    pub automatic: bool,
    pub detected_at: String,
}

impl MemoryAnchorFreshnessTransition {
    /// Whether this transition moves the anchor toward a less-fresh state.
    /// Conservatism rule: drift only ever ranks a memory down, so durable
    /// degradations are the auditable signal callers act on.
    #[must_use]
    pub fn is_degradation(&self) -> bool {
        self.new_state.rank() > self.previous_state.rank()
    }

    /// Agent-facing freshness label for surfacing (bd-1n0np.3.8): `ee memory
    /// show`, the per-pack `symbol_drift` facet, and revalidate candidates.
    ///
    /// Maps the transition's resulting state + drift code to one of
    /// `fresh | symbol_changed | symbol_missing | unknown`. Conservatism rule:
    /// an ambiguous (`Suspect`) result — e.g. an unresolved rename/move —
    /// surfaces as `unknown` (advisory), NEVER as a hard stale label.
    #[must_use]
    pub fn surface_label(&self) -> &'static str {
        match (self.new_state, self.drift_code.as_deref()) {
            (MemoryAnchorFreshnessState::Current, _) => "fresh",
            (MemoryAnchorFreshnessState::Suspect, _) => "unknown",
            (MemoryAnchorFreshnessState::Stale, Some("memory_drift_source_missing")) => {
                "symbol_missing"
            }
            (MemoryAnchorFreshnessState::Stale, _) => "symbol_changed",
        }
    }

    /// Canonical, deterministic audit-details JSON with a trailing
    /// `detailsHash` over the redaction-safe payload, mirroring
    /// `memory.level_transition` audit rows. The same transition always
    /// serializes to a byte-identical string.
    #[must_use]
    pub fn audit_details_json(&self) -> String {
        let payload = serde_json::json!({
            "schema": MEMORY_ANCHOR_FRESHNESS_TRANSITION_SCHEMA_V1,
            "memoryId": self.memory_id.as_str(),
            "anchorKind": self.anchor_kind.as_str(),
            "anchorValueHash": self.anchor_value_hash.as_str(),
            "previousState": self.previous_state.as_str(),
            "newState": self.new_state.as_str(),
            "driftCode": self.drift_code.as_deref(),
            "fileLine": self.file_line.as_deref(),
            "reason": self.reason.as_str(),
            "automatic": self.automatic,
            "detectedAt": self.detected_at.as_str(),
        });
        let details_hash = format!(
            "blake3:{}",
            blake3::hash(payload.to_string().as_bytes()).to_hex()
        );
        let mut payload_with_hash = payload;
        payload_with_hash["detailsHash"] = serde_json::json!(details_hash);
        payload_with_hash.to_string()
    }

    /// Parse a `memory.freshness_transition` audit details payload (as produced
    /// by [`Self::audit_details_json`]) back into a transition.
    ///
    /// Returns `None` if the payload is malformed, missing a required field, or
    /// carries an unknown enum value. The trailing `detailsHash` is
    /// informational and is not required to reconstruct the transition. This is
    /// the read-back inverse the steward uses to avoid re-recording an unchanged
    /// transition and that freshness surfacing uses to render prior drift.
    #[must_use]
    pub fn from_audit_details_json(details: &str) -> Option<Self> {
        let value: serde_json::Value = serde_json::from_str(details).ok()?;
        let object = value.as_object()?;
        let required = |key: &str| object.get(key).and_then(serde_json::Value::as_str);
        let optional = |key: &str| match object.get(key) {
            None | Some(serde_json::Value::Null) => Some(None),
            Some(serde_json::Value::String(text)) => Some(Some(text.clone())),
            Some(_) => None,
        };
        Some(Self {
            memory_id: required("memoryId")?.to_owned(),
            anchor_kind: MemoryAnchorKind::parse(required("anchorKind")?)?,
            anchor_value_hash: required("anchorValueHash")?.to_owned(),
            previous_state: MemoryAnchorFreshnessState::parse(required("previousState")?)?,
            new_state: MemoryAnchorFreshnessState::parse(required("newState")?)?,
            drift_code: optional("driftCode")?,
            file_line: optional("fileLine")?,
            reason: required("reason")?.to_owned(),
            automatic: object
                .get("automatic")
                .and_then(serde_json::Value::as_bool)?,
            detected_at: required("detectedAt")?.to_owned(),
        })
    }
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

/// One extracted anchor together with the normalized value its hash was
/// computed over. The `anchor` half is the durable, redaction-safe
/// `memory_anchors` payload; `normalized_value` is consumed only by the
/// ADR 0064 derived reverse index (and only for `Path`/`Symbol` kinds).
#[derive(Clone, Debug, PartialEq)]
pub struct ExtractedAnchorSurface {
    pub anchor: CreateMemoryAnchorInput,
    pub normalized_value: String,
}

/// Single anchor-extraction walk shared by the search-document builder and the
/// ADR 0064 reverse index ("single extractor, two consumers"). Returns each
/// anchor with its normalized value so reverse-index maintenance can persist
/// `normalized_path`/`symbol` without re-deriving (and drifting from) the
/// normalization rules.
#[must_use]
pub fn extract_memory_anchor_surfaces(
    memory_id: &str,
    content: &str,
    source: MemoryAnchorSource,
    provenance: Option<&str>,
) -> Vec<ExtractedAnchorSurface> {
    let mut anchors = BTreeMap::new();
    let provenance = provenance.unwrap_or("memory.content");

    extract_explicit_anchors(memory_id, content, provenance, &mut anchors);
    extract_schema_anchors(memory_id, content, source, provenance, &mut anchors);

    for fragment in code_fragments(content) {
        extract_code_fragment_anchors(memory_id, fragment, source, provenance, &mut anchors);
    }

    anchors.into_values().collect()
}

#[must_use]
pub fn extract_precision_memory_anchors(
    memory_id: &str,
    content: &str,
    source: MemoryAnchorSource,
    provenance: Option<&str>,
) -> Vec<CreateMemoryAnchorInput> {
    extract_memory_anchor_surfaces(memory_id, content, source, provenance)
        .into_iter()
        .map(|surface| surface.anchor)
        .collect()
}

fn extract_explicit_anchors(
    memory_id: &str,
    content: &str,
    provenance: &str,
    anchors: &mut BTreeMap<(MemoryAnchorKind, String), ExtractedAnchorSurface>,
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
    anchors: &mut BTreeMap<(MemoryAnchorKind, String), ExtractedAnchorSurface>,
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
    anchors: &mut BTreeMap<(MemoryAnchorKind, String), ExtractedAnchorSurface>,
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
    anchors: &mut BTreeMap<(MemoryAnchorKind, String), ExtractedAnchorSurface>,
    memory_id: &str,
    anchor_kind: MemoryAnchorKind,
    raw_value: &str,
    confidence: f32,
    source: MemoryAnchorSource,
    provenance: &str,
    generation: i64,
) {
    let Some((anchor, normalized_value)) = CreateMemoryAnchorInput::from_raw_with_normalized(
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
        .or_insert(ExtractedAnchorSurface {
            anchor,
            normalized_value,
        });
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
    let Some(schema_name) = prefix.strip_prefix("ee.") else {
        return false;
    };
    !schema_name.is_empty()
        && schema_name.split('.').all(|segment| {
            !segment.is_empty()
                && segment.chars().all(|character| {
                    character.is_ascii_lowercase() || character.is_ascii_digit() || character == '_'
                })
        })
        && !version.is_empty()
        && version.chars().all(|character| character.is_ascii_digit())
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

/// Canonical schema id for the `ee pack --surface` coverage facet.
pub const SURFACE_COVERAGE_FACET_SCHEMA_V1: &str = "ee.pack.surface_coverage.v1";

/// Anchor coverage of one code surface named by an `ee pack --surface` hint
/// (bd-1n0np.3.6).
///
/// Redaction-safe: the surface is identified by its domain-separated anchor
/// hash plus a short redacted display token, never its raw value.
#[derive(Clone, Debug, PartialEq)]
pub struct SurfaceCoverage {
    pub anchor_kind: MemoryAnchorKind,
    pub anchor_value_hash: String,
    pub redacted_surface: String,
    pub anchored_memory_count: usize,
}

impl SurfaceCoverage {
    /// A surface is covered when at least one durable memory is anchored to it.
    #[must_use]
    pub const fn is_covered(&self) -> bool {
        self.anchored_memory_count > 0
    }
}

/// Deterministic coverage facet for a set of `ee pack --surface` hints
/// (bd-1n0np.3.6), consumed by the gap-honesty epic to surface uncovered
/// (blind-spot) surfaces.
///
/// The pack path resolves each hint to its anchor and counts anchored memories
/// (`query_memory_anchors`); this pure builder shapes those counts into a stable
/// facet. It performs no I/O and is side-effect free.
#[derive(Clone, Debug, PartialEq)]
pub struct SurfaceCoverageFacet {
    pub schema: &'static str,
    pub surfaces: Vec<SurfaceCoverage>,
    pub covered_count: usize,
    pub uncovered_count: usize,
}

impl SurfaceCoverageFacet {
    /// Build a deterministic facet from per-surface anchored-memory counts.
    /// Surfaces are ordered by `(kind, hash)` for byte-stable output.
    #[must_use]
    pub fn from_surfaces(surfaces: impl IntoIterator<Item = SurfaceCoverage>) -> Self {
        let mut surfaces: Vec<SurfaceCoverage> = surfaces.into_iter().collect();
        surfaces.sort_by(|left, right| {
            left.anchor_kind
                .cmp(&right.anchor_kind)
                .then_with(|| left.anchor_value_hash.cmp(&right.anchor_value_hash))
        });
        let covered_count = surfaces
            .iter()
            .filter(|surface| surface.is_covered())
            .count();
        let uncovered_count = surfaces.len() - covered_count;
        Self {
            schema: SURFACE_COVERAGE_FACET_SCHEMA_V1,
            surfaces,
            covered_count,
            uncovered_count,
        }
    }

    /// Fraction of requested surfaces with at least one anchored memory.
    /// An empty facet (no surfaces requested) has full coverage `1.0`.
    #[must_use]
    pub fn coverage_ratio(&self) -> f32 {
        if self.surfaces.is_empty() {
            return 1.0;
        }
        self.covered_count as f32 / self.surfaces.len() as f32
    }
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
    fn surface_extractor_carries_normalized_values_and_matches_precision_anchors() {
        let memory_id = "mem_01234567890123456789012345";
        let content =
            "Touch `src/core/recall.rs` near `DbConnection::open_memory()` before `cargo check`.";
        let surfaces = extract_memory_anchor_surfaces(
            memory_id,
            content,
            MemoryAnchorSource::Remember,
            Some("test://surface"),
        );
        let anchors = extract_precision_memory_anchors(
            memory_id,
            content,
            MemoryAnchorSource::Remember,
            Some("test://surface"),
        );
        // Same walk, same anchors: the precision extractor is exactly the
        // surface extractor minus the normalized values.
        assert_eq!(
            surfaces
                .iter()
                .map(|surface| surface.anchor.clone())
                .collect::<Vec<_>>(),
            anchors
        );
        let path_surface = surfaces
            .iter()
            .find(|surface| surface.anchor.anchor_kind == MemoryAnchorKind::Path)
            .expect("path anchor extracted");
        assert_eq!(path_surface.normalized_value, "src/core/recall.rs");
        // The durable anchor payload stays redacted even though the surface
        // carries the raw normalized value for the reverse index.
        assert!(
            !path_surface
                .anchor
                .redacted_anchor_value
                .contains("recall.rs")
        );
        let symbol_surface = surfaces
            .iter()
            .find(|surface| surface.anchor.anchor_kind == MemoryAnchorKind::Symbol)
            .expect("symbol anchor extracted");
        assert_eq!(symbol_surface.normalized_value, "DbConnection::open_memory");
        // Normalized hash agreement: hashing the carried value reproduces the
        // anchor's stored hash, so reverse-index rows can be re-derived.
        assert_eq!(
            memory_anchor_value_hash(MemoryAnchorKind::Path, &path_surface.normalized_value),
            path_surface.anchor.anchor_value_hash
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

    #[test]
    fn schema_anchor_rejects_empty_version_or_name_segments() {
        assert!(
            CreateMemoryAnchorInput::from_raw(
                "mem_01234567890123456789012345",
                MemoryAnchorKind::Schema,
                "ee.response.v",
                0.95,
                MemoryAnchorSource::Remember,
                "test://schema",
                0,
            )
            .is_none()
        );
        assert!(
            CreateMemoryAnchorInput::from_raw(
                "mem_01234567890123456789012345",
                MemoryAnchorKind::Schema,
                "ee..v2",
                0.95,
                MemoryAnchorSource::Remember,
                "test://schema",
                0,
            )
            .is_none()
        );
        assert!(
            CreateMemoryAnchorInput::from_raw(
                "mem_01234567890123456789012345",
                MemoryAnchorKind::Schema,
                "ee.response..v2",
                0.95,
                MemoryAnchorSource::Remember,
                "test://schema",
                0,
            )
            .is_none()
        );
        assert!(
            CreateMemoryAnchorInput::from_raw(
                "mem_01234567890123456789012345",
                MemoryAnchorKind::Schema,
                "ee.response.v2",
                0.95,
                MemoryAnchorSource::Remember,
                "test://schema",
                0,
            )
            .is_some()
        );

        let anchors = extract_precision_memory_anchors(
            "mem_01234567890123456789012345",
            "Malformed schema id ee.response.v should stay out of schema anchors.",
            MemoryAnchorSource::Remember,
            Some("test://schema"),
        );
        assert!(
            !anchors
                .iter()
                .any(|anchor| anchor.anchor_kind == MemoryAnchorKind::Schema),
            "malformed schema ids must not produce schema anchors"
        );
    }

    fn sample_freshness_transition(
        previous: MemoryAnchorFreshnessState,
        new: MemoryAnchorFreshnessState,
    ) -> MemoryAnchorFreshnessTransition {
        MemoryAnchorFreshnessTransition {
            memory_id: "mem_01234567890123456789012345".to_owned(),
            anchor_kind: MemoryAnchorKind::Path,
            anchor_value_hash: memory_anchor_value_hash(MemoryAnchorKind::Path, "src/db/mod.rs"),
            previous_state: previous,
            new_state: new,
            drift_code: Some("memory_drift_source_changed".to_owned()),
            file_line: Some("src/db/mod.rs:42".to_owned()),
            reason: "symbol content hash changed".to_owned(),
            automatic: true,
            detected_at: "2026-06-07T00:00:00Z".to_owned(),
        }
    }

    #[test]
    fn freshness_state_rank_orders_current_below_suspect_below_stale() {
        assert!(
            MemoryAnchorFreshnessState::Current.rank() < MemoryAnchorFreshnessState::Suspect.rank()
        );
        assert!(
            MemoryAnchorFreshnessState::Suspect.rank() < MemoryAnchorFreshnessState::Stale.rank()
        );
    }

    #[test]
    fn freshness_transition_degradation_is_directional() {
        assert!(
            sample_freshness_transition(
                MemoryAnchorFreshnessState::Current,
                MemoryAnchorFreshnessState::Stale,
            )
            .is_degradation()
        );
        assert!(
            !sample_freshness_transition(
                MemoryAnchorFreshnessState::Stale,
                MemoryAnchorFreshnessState::Current,
            )
            .is_degradation()
        );
        assert!(
            !sample_freshness_transition(
                MemoryAnchorFreshnessState::Current,
                MemoryAnchorFreshnessState::Current,
            )
            .is_degradation()
        );
    }

    #[test]
    fn freshness_transition_audit_details_are_hashed_and_deterministic() {
        let transition = sample_freshness_transition(
            MemoryAnchorFreshnessState::Current,
            MemoryAnchorFreshnessState::Stale,
        );
        let details = transition.audit_details_json();
        assert!(details.contains(MEMORY_ANCHOR_FRESHNESS_TRANSITION_SCHEMA_V1));
        assert!(details.contains("\"detailsHash\":\"blake3:"));
        // Anchor identity is carried as a domain-separated hash, never raw.
        assert!(details.contains("\"anchorValueHash\":\"blake3:"));
        assert!(details.contains(&transition.anchor_value_hash));
        // Live file:line provenance surfaces when the symbol resolved.
        assert!(details.contains("\"fileLine\":\"src/db/mod.rs:42\""));
        // Deterministic: identical transition -> byte-identical payload.
        assert_eq!(details, transition.audit_details_json());
    }

    #[test]
    fn freshness_transition_audit_details_round_trip() {
        let original = sample_freshness_transition(
            MemoryAnchorFreshnessState::Current,
            MemoryAnchorFreshnessState::Stale,
        );
        let parsed = MemoryAnchorFreshnessTransition::from_audit_details_json(
            &original.audit_details_json(),
        )
        .expect("audit details must round-trip back to a transition");
        assert_eq!(parsed, original);

        // A transition with no drift code / file line also round-trips.
        let mut sparse = original.clone();
        sparse.drift_code = None;
        sparse.file_line = None;
        let sparse_round =
            MemoryAnchorFreshnessTransition::from_audit_details_json(&sparse.audit_details_json())
                .expect("sparse transition must round-trip");
        assert_eq!(sparse_round, sparse);

        // Malformed input is rejected, not panicked.
        assert!(MemoryAnchorFreshnessTransition::from_audit_details_json("not json").is_none());
        assert!(MemoryAnchorFreshnessTransition::from_audit_details_json("{}").is_none());
    }

    #[test]
    fn surface_coverage_facet_is_deterministic_and_counts_gaps() {
        let covered = SurfaceCoverage {
            anchor_kind: MemoryAnchorKind::Path,
            anchor_value_hash: memory_anchor_value_hash(MemoryAnchorKind::Path, "src/db/mod.rs"),
            redacted_surface: "path:blake3:000000000000".to_owned(),
            anchored_memory_count: 3,
        };
        let uncovered = SurfaceCoverage {
            anchor_kind: MemoryAnchorKind::Command,
            anchor_value_hash: memory_anchor_value_hash(
                MemoryAnchorKind::Command,
                "cargo fmt --check",
            ),
            redacted_surface: "command:blake3:111111111111".to_owned(),
            anchored_memory_count: 0,
        };
        assert!(covered.is_covered());
        assert!(!uncovered.is_covered());

        // Insertion order does not change the facet (deterministic sort).
        let facet = SurfaceCoverageFacet::from_surfaces([covered.clone(), uncovered.clone()]);
        let reversed = SurfaceCoverageFacet::from_surfaces([uncovered, covered]);
        assert_eq!(facet, reversed);
        assert_eq!(facet.schema, SURFACE_COVERAGE_FACET_SCHEMA_V1);
        assert_eq!(facet.covered_count, 1);
        assert_eq!(facet.uncovered_count, 1);
        assert!((facet.coverage_ratio() - 0.5).abs() < f32::EPSILON);

        // An empty facet (no surfaces requested) reports full coverage, no gap.
        let empty = SurfaceCoverageFacet::from_surfaces(Vec::new());
        assert_eq!(empty.uncovered_count, 0);
        assert_eq!(empty.coverage_ratio(), 1.0);
    }

    #[test]
    fn freshness_transition_surface_label_maps_states_conservatively() {
        let mut t = sample_freshness_transition(
            MemoryAnchorFreshnessState::Current,
            MemoryAnchorFreshnessState::Current,
        );
        t.drift_code = None;
        assert_eq!(t.surface_label(), "fresh");

        // Stale + content change -> symbol_changed.
        let mut changed = t.clone();
        changed.new_state = MemoryAnchorFreshnessState::Stale;
        changed.drift_code = Some("memory_drift_source_changed".to_owned());
        assert_eq!(changed.surface_label(), "symbol_changed");

        // Stale + disappearance -> symbol_missing.
        let mut missing = changed.clone();
        missing.drift_code = Some("memory_drift_source_missing".to_owned());
        assert_eq!(missing.surface_label(), "symbol_missing");

        // Ambiguity (Suspect) -> unknown, never a hard stale label.
        let mut suspect = changed.clone();
        suspect.new_state = MemoryAnchorFreshnessState::Suspect;
        suspect.drift_code = Some("memory_drift_source_unverifiable".to_owned());
        assert_eq!(suspect.surface_label(), "unknown");
    }
}
