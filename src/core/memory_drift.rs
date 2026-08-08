//! Read-only provenance snapshots for memory drift checks.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Component, Path};
use std::process::Command;

use chrono::{DateTime, Duration as ChronoDuration, Utc};
use serde::{Deserialize, Serialize};

#[cfg(test)]
use super::workspace::stable_workspace_id;
use crate::db::{
    DbConnection, DbError, PackLedgerStatus, StoredMemory, StoredPackRecord,
    parse_stored_pack_ledger,
};
use crate::models::memory_anchor::MemoryAnchorFreshnessTransition;
use crate::models::{
    DomainError, MemoryAnchorFreshnessState, MemoryAnchorKind, MemoryAnchorSource,
    StoredMemoryAnchor, extract_memory_anchor_surfaces,
};

pub const MEMORY_DRIFT_SNAPSHOT_SCHEMA_V1: &str = "ee.memory_drift.snapshot.v1";
pub const MEMORY_DRIFT_QUEUE_SCHEMA_V1: &str = "ee.memory_drift.queue.v1";
pub const MEMORY_DRIFT_REPORT_SCHEMA_V1: &str = "ee.memory_drift.report.v1";
pub const MEMORY_DRIFT_SUPPORT_SUMMARY_SCHEMA_V1: &str =
    "ee.support_bundle.memory_drift_summary.v1";
pub const DEFAULT_MEMORY_DRIFT_SOURCE_WINDOW_BYTES: usize = 4096;
pub const MAX_MEMORY_DRIFT_SUPPORT_SUMMARY_ITEMS: usize = 8;
/// Fixed claim-authority horizon for persisted pack selections.
///
/// A selection exactly seven days old remains in scope. Only selections
/// strictly older than this horizon are excluded from claim authority.
pub const RECENT_PACK_MEMORY_DRIFT_HORIZON_DAYS: i64 = 7;
/// Maximum pack-record/ledger selection units inspected for one claim-authority report.
/// Reaching this cap emits an unverifiable finding instead of silently
/// treating an incomplete scan as authoritative.
pub const RECENT_PACK_MEMORY_DRIFT_SCAN_CAP: u32 = 10_000;

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryDriftAnchorKind {
    FilePath,
    Symbol,
    LineSpanHash,
    CommandHash,
    SchemaId,
    BeadId,
    CommitHash,
    SessionFragmentHash,
}

impl MemoryDriftAnchorKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::FilePath => "file_path",
            Self::Symbol => "symbol",
            Self::LineSpanHash => "line_span_hash",
            Self::CommandHash => "command_hash",
            Self::SchemaId => "schema_id",
            Self::BeadId => "bead_id",
            Self::CommitHash => "commit_hash",
            Self::SessionFragmentHash => "session_fragment_hash",
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryDriftAnchorStatus {
    Captured,
    MissingSource,
    BinarySource,
    TooLarge,
    Unavailable,
    Redacted,
}

impl MemoryDriftAnchorStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Captured => "captured",
            Self::MissingSource => "missing_source",
            Self::BinarySource => "binary_source",
            Self::TooLarge => "too_large",
            Self::Unavailable => "unavailable",
            Self::Redacted => "redacted",
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryDriftSnapshotRedactionStatus {
    RedactionSafe,
    Redacted,
    Mixed,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MemoryDriftSourceWindow {
    pub start_byte: u64,
    pub end_byte: u64,
    pub byte_len: u64,
    pub truncated: bool,
}

impl MemoryDriftSourceWindow {
    #[must_use]
    pub fn new(start_byte: usize, end_byte: usize, total_byte_len: usize) -> Self {
        Self {
            start_byte: start_byte as u64,
            end_byte: end_byte as u64,
            byte_len: total_byte_len as u64,
            truncated: end_byte < total_byte_len,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MemoryDriftWindowDigest {
    pub content_hash: String,
    pub source_window: MemoryDriftSourceWindow,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MemoryDriftFreshness {
    pub observed_at: Option<String>,
    pub source_modified_at: Option<String>,
    pub source_byte_len: Option<u64>,
    pub unavailable_reason: Option<String>,
}

impl MemoryDriftFreshness {
    #[must_use]
    pub fn observed(observed_at: Option<&str>) -> Self {
        Self {
            observed_at: normalized_non_empty(observed_at),
            source_modified_at: None,
            source_byte_len: None,
            unavailable_reason: None,
        }
    }

    #[must_use]
    pub fn for_source(
        observed_at: Option<&str>,
        source_modified_at: Option<&str>,
        source_byte_len: usize,
    ) -> Self {
        Self {
            observed_at: normalized_non_empty(observed_at),
            source_modified_at: normalized_non_empty(source_modified_at),
            source_byte_len: Some(source_byte_len as u64),
            unavailable_reason: None,
        }
    }

    #[must_use]
    pub fn unavailable(observed_at: Option<&str>, reason: &str) -> Self {
        Self {
            observed_at: normalized_non_empty(observed_at),
            source_modified_at: None,
            source_byte_len: None,
            unavailable_reason: normalized_non_empty(Some(reason)),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MemoryDriftAnchor {
    pub kind: MemoryDriftAnchorKind,
    pub label: String,
    pub status: MemoryDriftAnchorStatus,
    pub content_hash: Option<String>,
    pub metadata_hash: String,
    pub source_window: Option<MemoryDriftSourceWindow>,
    pub freshness: MemoryDriftFreshness,
    pub metadata: BTreeMap<String, String>,
}

impl MemoryDriftAnchor {
    #[must_use]
    pub fn new(
        kind: MemoryDriftAnchorKind,
        label: &str,
        status: MemoryDriftAnchorStatus,
        content_hash: Option<String>,
        source_window: Option<MemoryDriftSourceWindow>,
        freshness: MemoryDriftFreshness,
        metadata: BTreeMap<String, String>,
    ) -> Self {
        let label = normalized_non_empty(Some(label)).unwrap_or_else(|| "unknown".to_owned());
        let metadata_hash =
            memory_drift_metadata_hash(kind, &label, status, &source_window, &freshness, &metadata);
        Self {
            kind,
            label,
            status,
            content_hash,
            metadata_hash,
            source_window,
            freshness,
            metadata,
        }
    }

    #[must_use]
    pub fn identifier(kind: MemoryDriftAnchorKind, value: &str, observed_at: Option<&str>) -> Self {
        let label = redacted_anchor_label(kind, value);
        let value_hash = memory_drift_content_hash(value.as_bytes());
        let mut metadata = BTreeMap::new();
        metadata.insert("valueHash".to_owned(), value_hash.clone());
        Self::new(
            kind,
            &label,
            MemoryDriftAnchorStatus::Captured,
            Some(value_hash),
            None,
            MemoryDriftFreshness::observed(observed_at),
            metadata,
        )
    }

    #[must_use]
    pub fn source_bytes(
        path: &Path,
        bytes: &[u8],
        max_window_bytes: usize,
        observed_at: Option<&str>,
        source_modified_at: Option<&str>,
    ) -> Self {
        let digest = hash_bounded_source_window(bytes, max_window_bytes);
        let status = classify_source_bytes(bytes, max_window_bytes);
        let mut freshness =
            MemoryDriftFreshness::for_source(observed_at, source_modified_at, bytes.len());
        if status != MemoryDriftAnchorStatus::Captured {
            freshness.unavailable_reason = Some(status.as_str().to_owned());
        }

        let mut metadata = BTreeMap::new();
        metadata.insert("pathHash".to_owned(), memory_drift_path_hash(path));
        metadata.insert("sourceKind".to_owned(), "file_path".to_owned());

        Self::new(
            MemoryDriftAnchorKind::FilePath,
            &redacted_path_label(path),
            status,
            Some(digest.content_hash),
            Some(digest.source_window),
            freshness,
            metadata,
        )
    }

    #[must_use]
    pub fn missing_file(path: &Path, reason: &str, observed_at: Option<&str>) -> Self {
        let mut metadata = BTreeMap::new();
        metadata.insert("pathHash".to_owned(), memory_drift_path_hash(path));
        metadata.insert("sourceKind".to_owned(), "file_path".to_owned());
        Self::new(
            MemoryDriftAnchorKind::FilePath,
            &redacted_path_label(path),
            MemoryDriftAnchorStatus::MissingSource,
            None,
            None,
            MemoryDriftFreshness::unavailable(observed_at, reason),
            metadata,
        )
    }

    #[must_use]
    pub fn line_span(
        path: &Path,
        source: &str,
        start_line: usize,
        end_line: usize,
        max_window_bytes: usize,
        observed_at: Option<&str>,
    ) -> Self {
        let selected = select_line_span(source, start_line, end_line);
        let digest = hash_bounded_source_window(selected.as_bytes(), max_window_bytes);
        let normalized_start = start_line.max(1);
        let normalized_end = end_line.max(normalized_start);
        let mut metadata = BTreeMap::new();
        metadata.insert("lineEnd".to_owned(), normalized_end.to_string());
        metadata.insert("lineStart".to_owned(), normalized_start.to_string());
        metadata.insert("pathHash".to_owned(), memory_drift_path_hash(path));
        Self::new(
            MemoryDriftAnchorKind::LineSpanHash,
            &format!(
                "{}:{normalized_start}-{normalized_end}",
                redacted_path_label(path)
            ),
            MemoryDriftAnchorStatus::Captured,
            Some(digest.content_hash),
            Some(digest.source_window),
            MemoryDriftFreshness::for_source(observed_at, None, selected.len()),
            metadata,
        )
    }

    fn sort_key(&self) -> String {
        format!(
            "{}|{}|{}|{}",
            self.kind.as_str(),
            self.label,
            self.status.as_str(),
            self.metadata_hash
        )
    }

    fn identity_key(&self) -> String {
        format!("{}|{}", self.kind.as_str(), self.label)
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MemoryDriftDegradation {
    pub code: String,
    pub severity: String,
    pub message: String,
}

impl MemoryDriftDegradation {
    #[must_use]
    pub fn new(code: &str, severity: &str, message: &str) -> Self {
        Self {
            code: code.to_owned(),
            severity: severity.to_owned(),
            message: message.to_owned(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MemoryDriftSnapshot {
    pub schema: String,
    pub memory_id: String,
    pub workspace_id: Option<String>,
    pub captured_at: Option<String>,
    pub redaction_status: MemoryDriftSnapshotRedactionStatus,
    pub source_window_bytes: usize,
    pub anchors: Vec<MemoryDriftAnchor>,
    pub degraded: Vec<MemoryDriftDegradation>,
}

impl MemoryDriftSnapshot {
    #[must_use]
    pub fn new(
        memory_id: &str,
        workspace_id: Option<&str>,
        captured_at: Option<&str>,
        source_window_bytes: usize,
        anchors: Vec<MemoryDriftAnchor>,
    ) -> Self {
        let mut snapshot = Self {
            schema: MEMORY_DRIFT_SNAPSHOT_SCHEMA_V1.to_owned(),
            memory_id: memory_id.to_owned(),
            workspace_id: normalized_non_empty(workspace_id),
            captured_at: normalized_non_empty(captured_at),
            redaction_status: MemoryDriftSnapshotRedactionStatus::RedactionSafe,
            source_window_bytes,
            anchors,
            degraded: Vec::new(),
        };
        snapshot.sort_anchors();
        snapshot
    }

    #[must_use]
    pub fn with_degraded(mut self, degraded: Vec<MemoryDriftDegradation>) -> Self {
        self.degraded = degraded;
        self
    }

    #[must_use]
    pub fn volatile_scrubbed(&self) -> Self {
        let mut scrubbed = self.clone();
        scrubbed.captured_at = None;
        for anchor in &mut scrubbed.anchors {
            anchor.freshness.observed_at = None;
            anchor.freshness.source_modified_at = None;
            anchor.metadata_hash = memory_drift_metadata_hash(
                anchor.kind,
                &anchor.label,
                anchor.status,
                &anchor.source_window,
                &anchor.freshness,
                &anchor.metadata,
            );
        }
        scrubbed
    }

    pub fn stable_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(self)
    }

    fn sort_anchors(&mut self) {
        self.anchors.sort_by_key(MemoryDriftAnchor::sort_key);
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryDriftStatus {
    Current,
    Changed,
    MissingSource,
    StaleAnchor,
    Unverifiable,
    Suppressed,
}

impl MemoryDriftStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Current => "current",
            Self::Changed => "changed",
            Self::MissingSource => "missing_source",
            Self::StaleAnchor => "stale_anchor",
            Self::Unverifiable => "unverifiable",
            Self::Suppressed => "suppressed",
        }
    }

    pub const fn severity_rank(self) -> u8 {
        match self {
            Self::Suppressed | Self::Current => 0,
            Self::StaleAnchor => 1,
            Self::Unverifiable => 2,
            Self::Changed => 3,
            Self::MissingSource => 4,
        }
    }

    #[must_use]
    pub const fn report_severity(self) -> &'static str {
        match self {
            Self::Current | Self::Suppressed => "info",
            Self::StaleAnchor => "low",
            Self::Unverifiable => "medium",
            Self::Changed => "medium",
            Self::MissingSource => "high",
        }
    }

    #[must_use]
    pub const fn default_reason(self) -> &'static str {
        match self {
            Self::Current => "source_evidence_current",
            Self::Changed => "source_evidence_changed",
            Self::MissingSource => "source_missing",
            Self::StaleAnchor => "anchor_stale",
            Self::Unverifiable => "source_unverifiable",
            Self::Suppressed => "suppressed_by_policy",
        }
    }

    #[must_use]
    pub const fn degraded_code(self) -> Option<&'static str> {
        match self {
            Self::Current | Self::Suppressed => None,
            Self::Changed => Some("memory_drift_source_changed"),
            Self::StaleAnchor => Some("memory_drift_source_changed"),
            Self::MissingSource => Some("memory_drift_source_missing"),
            Self::Unverifiable => Some("memory_drift_source_unverifiable"),
        }
    }

    const fn base_score_micros(self) -> u64 {
        match self {
            Self::Suppressed | Self::Current => 0,
            Self::StaleAnchor => 250_000,
            Self::Unverifiable => 350_000,
            Self::Changed => 550_000,
            Self::MissingSource => 700_000,
        }
    }
}

#[must_use]
pub const fn memory_drift_freshness_label(status: MemoryDriftStatus) -> &'static str {
    match status {
        MemoryDriftStatus::Current => "fresh",
        MemoryDriftStatus::Changed | MemoryDriftStatus::StaleAnchor => "drifted",
        MemoryDriftStatus::MissingSource => "missing",
        MemoryDriftStatus::Unverifiable => "unknown",
        MemoryDriftStatus::Suppressed => "suppressed",
    }
}

/// Map a drift observation to the freshness state an anchored memory should
/// hold once that drift is applied.
///
/// Conservatism rule (ADR 0056, part B): only a resolved symbol's content
/// change (`Changed`), exact disappearance (`MissingSource`), or already-stale
/// anchor (`StaleAnchor`) becomes `Stale`. An `Unverifiable` result — including
/// refactor ambiguity such as a rename/move that `src/core/symbol_graph.rs`
/// could not resolve exactly — is only `Suspect` (advisory), never silently
/// `Stale`. `Current` and `Suppressed` keep the anchor `Current`.
#[must_use]
pub const fn freshness_state_for_drift(status: MemoryDriftStatus) -> MemoryAnchorFreshnessState {
    match status {
        MemoryDriftStatus::Current | MemoryDriftStatus::Suppressed => {
            MemoryAnchorFreshnessState::Current
        }
        MemoryDriftStatus::Unverifiable => MemoryAnchorFreshnessState::Suspect,
        MemoryDriftStatus::Changed
        | MemoryDriftStatus::MissingSource
        | MemoryDriftStatus::StaleAnchor => MemoryAnchorFreshnessState::Stale,
    }
}

/// Build the audited `memory.freshness_transition` row for an anchor whose
/// bounded drift check produced `status`.
///
/// Returns `None` when the resulting freshness state equals `previous_state`
/// (no transition to record). The drift `status` itself is produced upstream by
/// the symbol-graph-backed resolution (`src/core/symbol_graph.rs`); this bridges
/// that resolution to the durable, redaction-safe audit model. `file_line`
/// carries the live symbol location when it resolved, and stays `None` under
/// ambiguity.
#[must_use]
pub fn freshness_transition_for_drift(
    anchor: &StoredMemoryAnchor,
    status: MemoryDriftStatus,
    previous_state: MemoryAnchorFreshnessState,
    file_line: Option<String>,
    detected_at: impl Into<String>,
) -> Option<MemoryAnchorFreshnessTransition> {
    let new_state = freshness_state_for_drift(status);
    if new_state == previous_state {
        return None;
    }
    Some(MemoryAnchorFreshnessTransition {
        memory_id: anchor.memory_id.clone(),
        anchor_kind: anchor.anchor_kind,
        anchor_value_hash: anchor.anchor_value_hash.clone(),
        previous_state,
        new_state,
        drift_code: status.degraded_code().map(str::to_owned),
        file_line,
        reason: status.default_reason().to_owned(),
        automatic: true,
        detected_at: detected_at.into(),
    })
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MemoryDriftMemoryFactors {
    pub memory_id: String,
    pub importance: f64,
    pub trust_class: String,
    pub trust_score: f64,
    pub recent_pack_inclusions: u32,
    pub downstream_graph_refs: u32,
    pub days_since_validation: u32,
    pub suppressed: bool,
}

impl Default for MemoryDriftMemoryFactors {
    fn default() -> Self {
        Self {
            memory_id: String::new(),
            importance: 0.0,
            trust_class: "agent_assertion".to_owned(),
            trust_score: 0.0,
            recent_pack_inclusions: 0,
            downstream_graph_refs: 0,
            days_since_validation: 0,
            suppressed: false,
        }
    }
}

impl MemoryDriftMemoryFactors {
    #[must_use]
    pub fn new(memory_id: &str) -> Self {
        Self {
            memory_id: memory_id.to_owned(),
            ..Self::default()
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MemoryDriftAnchorComparison {
    pub anchor_label: String,
    pub anchor_kind: MemoryDriftAnchorKind,
    pub drift_status: MemoryDriftStatus,
    pub previous_content_hash: Option<String>,
    pub current_content_hash: Option<String>,
    pub previous_metadata_hash: String,
    pub current_metadata_hash: Option<String>,
    pub reason_codes: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MemoryDriftQueueItem {
    pub memory_id: String,
    pub drift_status: MemoryDriftStatus,
    pub priority_score: f64,
    pub priority_score_micros: u64,
    pub affected_anchor_count: usize,
    pub total_anchor_count: usize,
    pub change_magnitude: f64,
    pub factors: MemoryDriftMemoryFactors,
    pub reason_codes: Vec<String>,
    pub comparisons: Vec<MemoryDriftAnchorComparison>,
}

impl MemoryDriftQueueItem {
    fn sort_key(&self) -> (std::cmp::Reverse<u64>, String) {
        (
            std::cmp::Reverse(self.priority_score_micros),
            self.memory_id.clone(),
        )
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MemoryDriftQueue {
    pub schema: String,
    pub generated_at: Option<String>,
    pub queue_truncated: bool,
    pub items: Vec<MemoryDriftQueueItem>,
    pub degraded: Vec<MemoryDriftDegradation>,
}

impl MemoryDriftQueue {
    #[must_use]
    pub fn new(
        generated_at: Option<&str>,
        mut items: Vec<MemoryDriftQueueItem>,
        limit: Option<usize>,
    ) -> Self {
        items.sort_by_key(MemoryDriftQueueItem::sort_key);
        let queue_truncated = limit.is_some_and(|limit| items.len() > limit);
        if let Some(limit) = limit {
            items.truncate(limit);
        }
        Self {
            schema: MEMORY_DRIFT_QUEUE_SCHEMA_V1.to_owned(),
            generated_at: normalized_non_empty(generated_at),
            queue_truncated,
            items,
            degraded: Vec::new(),
        }
    }

    #[must_use]
    pub fn with_degraded(mut self, degraded: Vec<MemoryDriftDegradation>) -> Self {
        self.degraded = degraded;
        self
    }

    #[must_use]
    pub fn volatile_scrubbed(&self) -> Self {
        let mut scrubbed = self.clone();
        scrubbed.generated_at = None;
        scrubbed
    }

    pub fn stable_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(self)
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryDriftReportMode {
    AllMemories,
    OneMemory,
    RecentPackItems,
}

impl MemoryDriftReportMode {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::AllMemories => "all_memories",
            Self::OneMemory => "one_memory",
            Self::RecentPackItems => "recent_pack_items",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MemoryDriftCodeAnchor {
    pub anchor_kind: String,
    pub anchor_value_hash: String,
    pub redacted_anchor_value: String,
    pub captured_span_hash: String,
    pub freshness_state: String,
    pub freshness: String,
    pub generation: i64,
    pub stale_anchor: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MemoryDriftSelectionHint {
    pub memory_id: String,
    pub drift_status: MemoryDriftStatus,
    pub freshness: String,
    pub top_reason: String,
    pub evidence_count: u32,
    pub revalidation_command: String,
    pub degraded_code: Option<String>,
    pub severity: String,
    pub stale_anchor: bool,
    pub captured_at_commit: Option<String>,
    pub current_commit: Option<String>,
    pub commit_distance: Option<u32>,
    pub changed_regions: Vec<String>,
    pub anchors: Vec<MemoryDriftCodeAnchor>,
}

impl MemoryDriftSelectionHint {
    #[must_use]
    pub fn new(
        memory_id: &str,
        drift_status: MemoryDriftStatus,
        top_reason: &str,
        evidence_count: u32,
    ) -> Self {
        let memory_id = normalized_non_empty(Some(memory_id)).unwrap_or_else(|| "unknown".into());
        Self {
            revalidation_command: format!("ee memory drift {memory_id} --json"),
            memory_id,
            drift_status,
            freshness: memory_drift_freshness_label(drift_status).to_owned(),
            top_reason: normalized_non_empty(Some(top_reason))
                .unwrap_or_else(|| drift_status.default_reason().to_owned()),
            evidence_count,
            degraded_code: drift_status.degraded_code().map(str::to_owned),
            severity: drift_status.report_severity().to_owned(),
            stale_anchor: drift_status == MemoryDriftStatus::StaleAnchor,
            captured_at_commit: None,
            current_commit: None,
            commit_distance: None,
            changed_regions: Vec::new(),
            anchors: Vec::new(),
        }
    }

    #[must_use]
    pub fn with_code_anchor_context(
        mut self,
        anchors: Vec<MemoryDriftCodeAnchor>,
        captured_at_commit: Option<String>,
        current_commit: Option<String>,
        commit_distance: Option<u32>,
        changed_regions: Vec<String>,
        stale_anchor: bool,
    ) -> Self {
        self.evidence_count = self
            .evidence_count
            .saturating_add(u32::try_from(anchors.len()).unwrap_or(u32::MAX));
        self.stale_anchor = self.stale_anchor || stale_anchor;
        if self.stale_anchor && self.drift_status == MemoryDriftStatus::Current {
            self.drift_status = MemoryDriftStatus::StaleAnchor;
            self.freshness = memory_drift_freshness_label(self.drift_status).to_owned();
            self.top_reason = "code_anchor_stale".to_owned();
            self.degraded_code = self.drift_status.degraded_code().map(str::to_owned);
            self.severity = self.drift_status.report_severity().to_owned();
        }
        if self.stale_anchor && self.degraded_code.is_none() {
            self.degraded_code = MemoryDriftStatus::StaleAnchor
                .degraded_code()
                .map(str::to_owned);
        }
        self.captured_at_commit = captured_at_commit;
        self.current_commit = current_commit;
        self.commit_distance = commit_distance;
        self.changed_regions = changed_regions;
        self.anchors = anchors;
        self
    }

    #[must_use]
    pub fn compact_json(&self) -> serde_json::Value {
        serde_json::json!({
            "driftStatus": self.drift_status.as_str(),
            "freshness": &self.freshness,
            "topReason": &self.top_reason,
            "evidenceCount": self.evidence_count,
            "revalidationCommand": &self.revalidation_command,
            "staleAnchor": self.stale_anchor,
        })
    }

    fn sort_key(&self) -> (std::cmp::Reverse<u8>, std::cmp::Reverse<u8>, String) {
        (
            std::cmp::Reverse(u8::from(
                self.degraded_code.as_deref() == Some("memory_drift_source_unverifiable"),
            )),
            std::cmp::Reverse(self.drift_status.severity_rank()),
            self.memory_id.clone(),
        )
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MemoryDriftRecoveryAction {
    pub priority: u8,
    pub kind: String,
    pub command: Option<String>,
    pub description: String,
}

impl MemoryDriftRecoveryAction {
    #[must_use]
    pub fn new(priority: u8, kind: &str, command: Option<String>, description: &str) -> Self {
        Self {
            priority,
            kind: kind.to_owned(),
            command,
            description: description.to_owned(),
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MemoryDriftReportSummary {
    pub total_memories: u32,
    pub current: u32,
    pub changed: u32,
    pub missing_source: u32,
    pub stale_anchor: u32,
    pub unverifiable: u32,
    pub suppressed: u32,
}

impl MemoryDriftReportSummary {
    #[must_use]
    pub fn from_items(items: &[MemoryDriftSelectionHint]) -> Self {
        let mut summary = Self {
            total_memories: u32::try_from(items.len()).unwrap_or(u32::MAX),
            ..Self::default()
        };
        for item in items {
            match item.drift_status {
                MemoryDriftStatus::Current => summary.current += 1,
                MemoryDriftStatus::Changed => summary.changed += 1,
                MemoryDriftStatus::MissingSource => summary.missing_source += 1,
                MemoryDriftStatus::StaleAnchor => summary.stale_anchor += 1,
                MemoryDriftStatus::Unverifiable => summary.unverifiable += 1,
                MemoryDriftStatus::Suppressed => summary.suppressed += 1,
            }
        }
        summary
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MemoryDriftReport {
    pub schema: String,
    pub mode: MemoryDriftReportMode,
    pub generated_at: Option<String>,
    pub summary: MemoryDriftReportSummary,
    pub items: Vec<MemoryDriftSelectionHint>,
    pub recovery_actions: Vec<MemoryDriftRecoveryAction>,
    pub degraded: Vec<MemoryDriftDegradation>,
}

impl MemoryDriftReport {
    #[must_use]
    pub fn new(
        mode: MemoryDriftReportMode,
        generated_at: Option<&str>,
        mut items: Vec<MemoryDriftSelectionHint>,
    ) -> Self {
        items.sort_by_key(MemoryDriftSelectionHint::sort_key);
        let summary = MemoryDriftReportSummary::from_items(&items);
        let recovery_actions = memory_drift_recovery_actions(mode, &items);
        Self {
            schema: MEMORY_DRIFT_REPORT_SCHEMA_V1.to_owned(),
            mode,
            generated_at: normalized_non_empty(generated_at),
            summary,
            items,
            recovery_actions,
            degraded: Vec::new(),
        }
    }

    #[must_use]
    pub fn with_degraded(mut self, degraded: Vec<MemoryDriftDegradation>) -> Self {
        self.degraded = degraded;
        self
    }

    #[must_use]
    pub fn volatile_scrubbed(&self) -> Self {
        let mut scrubbed = self.clone();
        scrubbed.generated_at = None;
        scrubbed
    }

    pub fn stable_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(self)
    }
}

fn memory_drift_recovery_actions(
    mode: MemoryDriftReportMode,
    items: &[MemoryDriftSelectionHint],
) -> Vec<MemoryDriftRecoveryAction> {
    let superseded_pack_selection = items
        .iter()
        .any(|item| item.top_reason == "pack_item_superseded_since_selection");
    let pack_integrity_failure = items.iter().any(|item| {
        item.top_reason.starts_with("pack_item_")
            && item.top_reason != "pack_item_superseded_since_selection"
    });
    let report_command = if pack_integrity_failure {
        "ee doctor --json".to_owned()
    } else if superseded_pack_selection {
        "ee pack \"<TASK>\" --workspace . --json".to_owned()
    } else {
        match mode {
            MemoryDriftReportMode::AllMemories => {
                "ee memory drift --mode all-memories --json".to_owned()
            }
            MemoryDriftReportMode::RecentPackItems => {
                "ee memory drift --mode recent-pack-items --json".to_owned()
            }
            MemoryDriftReportMode::OneMemory => items
                .first()
                .map(|item| format!("ee memory drift {} --json", item.memory_id))
                .unwrap_or_else(|| "ee memory drift <MEMORY_ID> --json".to_owned()),
        }
    };

    let rerun_description = if pack_integrity_failure {
        "Inspect pack-record, revision-lineage, and database integrity before retrying; rerunning drift alone cannot repair malformed or incomplete authority evidence."
    } else if superseded_pack_selection {
        "Run a fresh pack for the same task so persisted evidence selects the unique live revision, then rerun the read-only drift report; rerunning drift alone cannot rewrite an old pack selection."
    } else {
        "Rerun the read-only drift report before reusing affected memories."
    };
    let revise_command = superseded_pack_selection.then(|| {
        "ee memory revise <CURRENT_MEMORY_ID> --provenance-uri <URI> --reason correction --json"
            .to_owned()
    });
    let revise_description = if superseded_pack_selection {
        "If the live revision itself has stale source evidence, revise that current ID immutably first; after the revision succeeds, run a fresh pack so the new live ID replaces historical pack evidence."
    } else {
        "Revise or re-remember affected memories after updating their source evidence."
    };

    vec![
        MemoryDriftRecoveryAction::new(
            1,
            "rerun_source_validation",
            Some(report_command),
            rerun_description,
        ),
        MemoryDriftRecoveryAction::new(2, "revise_memory", revise_command, revise_description),
        MemoryDriftRecoveryAction::new(
            3,
            "mark_source_unavailable",
            None,
            "Record source-unavailable status only through a later explicit audited mutation flow.",
        ),
        MemoryDriftRecoveryAction::new(
            4,
            "ignore_with_audit",
            None,
            "Ignore a drift warning only with a later audited decision; this report is read-only.",
        ),
    ]
}

#[must_use]
pub fn memory_drift_support_summary_from_report(report: &MemoryDriftReport) -> serde_json::Value {
    let top_affected = report
        .items
        .iter()
        .filter(|item| item.drift_status != MemoryDriftStatus::Current)
        .take(MAX_MEMORY_DRIFT_SUPPORT_SUMMARY_ITEMS)
        .map(memory_drift_support_summary_item)
        .collect::<Vec<_>>();
    let degraded_codes = memory_drift_support_degraded_codes(report);
    let status = if !degraded_codes.is_empty() {
        "degraded"
    } else if top_affected.is_empty() {
        "empty_queue"
    } else {
        "available"
    };

    serde_json::json!({
        "schema": MEMORY_DRIFT_SUPPORT_SUMMARY_SCHEMA_V1,
        "sourceSchema": MEMORY_DRIFT_REPORT_SCHEMA_V1,
        "source": "memory_drift_recent_pack_items_report",
        "status": status,
        "redactionStatus": "ids_status_counts_only_no_raw_snippets_no_command_bodies",
        "reportMode": report.mode.as_str(),
        "counts": {
            "totalMemories": report.summary.total_memories,
            "current": report.summary.current,
            "changed": report.summary.changed,
            "missingSource": report.summary.missing_source,
            "staleAnchor": report.summary.stale_anchor,
            "unverifiable": report.summary.unverifiable,
            "suppressed": report.summary.suppressed,
            "affected": report.summary.changed
                .saturating_add(report.summary.missing_source)
                .saturating_add(report.summary.stale_anchor)
                .saturating_add(report.summary.unverifiable),
            "topAffectedCount": top_affected.len(),
        },
        "driftStatusCounts": memory_drift_support_status_counts(&report.items),
        "sourceKindCounts": memory_drift_support_source_kind_counts(&report.items),
        "degradedCodes": degraded_codes,
        "topAffected": top_affected,
        "limits": {
            "maxTopAffected": MAX_MEMORY_DRIFT_SUPPORT_SUMMARY_ITEMS,
        },
        "provenance": {
            "sideEffectFree": true,
            "rawSnippetsIncluded": false,
            "rawCommandBodiesIncluded": false,
            "fullListingsIncluded": false,
            "revalidationCommandIncluded": false,
        },
    })
}

#[must_use]
pub fn memory_drift_support_summary_unavailable(
    status: &str,
    degraded_code: &str,
    message: &str,
) -> serde_json::Value {
    let severity = if degraded_code == MEMORY_DRIFT_LOCK_CONTENTION_CODE
        || degraded_code == MEMORY_DRIFT_REPORT_UNAVAILABLE_CODE
    {
        "warning"
    } else {
        "medium"
    };
    let mut summary = serde_json::json!({
        "schema": MEMORY_DRIFT_SUPPORT_SUMMARY_SCHEMA_V1,
        "sourceSchema": MEMORY_DRIFT_REPORT_SCHEMA_V1,
        "source": "memory_drift_recent_pack_items_report",
        "status": memory_drift_support_label(status),
        "redactionStatus": "ids_status_counts_only_no_raw_snippets_no_command_bodies",
        "counts": {
            "totalMemories": 0,
            "current": 0,
            "changed": 0,
            "missingSource": 0,
            "staleAnchor": 0,
            "unverifiable": 0,
            "suppressed": 0,
            "affected": 0,
            "topAffectedCount": 0,
        },
        "driftStatusCounts": serde_json::json!({}),
        "sourceKindCounts": serde_json::json!({}),
        "degradedCodes": [memory_drift_support_label(degraded_code)],
        "topAffected": [],
        "degraded": [{
            "code": memory_drift_support_label(degraded_code),
            "severity": severity,
            "message": memory_drift_support_message(message),
        }],
        "evidenceInspection": memory_drift_support_unavailable_evidence_inspection(degraded_code),
        "limits": {
            "maxTopAffected": MAX_MEMORY_DRIFT_SUPPORT_SUMMARY_ITEMS,
        },
        "provenance": {
            "sideEffectFree": true,
            "rawSnippetsIncluded": false,
            "rawCommandBodiesIncluded": false,
            "fullListingsIncluded": false,
            "revalidationCommandIncluded": false,
        },
    });
    if degraded_code == MEMORY_DRIFT_REPORT_UNAVAILABLE_CODE
        || degraded_code == MEMORY_DRIFT_LOCK_CONTENTION_CODE
    {
        summary["degraded"][0]["repair"] = serde_json::json!("ee doctor --json");
    }
    summary
}

fn memory_drift_support_unavailable_evidence_inspection(degraded_code: &str) -> serde_json::Value {
    if degraded_code == MEMORY_DRIFT_LOCK_CONTENTION_CODE {
        return serde_json::json!({
            "status": "not_inspected",
            "memoryEvidenceInspected": false,
            "sourceFreshness": "not_inspected",
            "degradationClass": "database_read_snapshot_contention",
            "lockAcquisitionClass": MEMORY_DRIFT_READ_ONLY_COLLECTOR_LOCK_CLASS,
            "supportBundleMeaning": "collector_blocked_before_evidence",
            "advisoryOnly": true,
            "mutatesState": false,
            "recoverySuggestions": [
                "retry_read_only_drift_report",
                "inspect_database_health_read_only",
                "use_source_authority_snapshot",
                "continue_plan_space",
            ],
        });
    }

    if degraded_code == MEMORY_DRIFT_REPORT_UNAVAILABLE_CODE {
        return serde_json::json!({
            "status": "not_inspected",
            "memoryEvidenceInspected": false,
            "sourceFreshness": "not_inspected",
            "degradationClass": "memory_drift_report_unavailable",
            "supportBundleMeaning": "collector_failed_before_evidence",
            "advisoryOnly": true,
            "mutatesState": false,
            "recoverySuggestions": [
                "retry_read_only_drift_report",
                "inspect_database_health_read_only",
                "continue_plan_space",
            ],
        });
    }

    serde_json::json!({
        "status": "unknown",
        "memoryEvidenceInspected": null,
        "sourceFreshness": "unknown",
        "degradationClass": "report_unavailable",
        "advisoryOnly": true,
        "mutatesState": false,
        "recoverySuggestions": [
            "rerun_read_only_drift_report",
            "inspect_doctor",
        ],
    })
}

#[derive(Clone, Debug)]
pub struct MemoryDriftReportOptions<'a> {
    pub database_path: &'a Path,
    pub workspace_path: &'a Path,
    pub mode: MemoryDriftReportMode,
    pub memory_id: Option<&'a str>,
    pub limit: u32,
    pub include_tombstoned: bool,
    /// Injected collector clock for deterministic horizon checks. Public CLI
    /// callers leave this unset and use one captured `Utc::now()` value.
    pub as_of: Option<DateTime<Utc>>,
}

/// Current swarm memory-drift collector strategy after bd-3sh42 landed a
/// genuinely read-only fsqlite/sqlmodel open path. Workspace write-owner
/// flocks no longer block collection; the database read snapshot supplies
/// cross-query consistency without joining the write-owner gate.
pub const MEMORY_DRIFT_READ_ONLY_COLLECTOR_STRATEGY_NAME: &str = "true_read_only_snapshot";
pub const MEMORY_DRIFT_READ_ONLY_COLLECTOR_LOCK_CLASS: &str = "database_read_snapshot";
pub const MEMORY_DRIFT_TRUE_READ_ONLY_DATABASE_OPEN_BEAD: &str = "bd-3sh42";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MemoryDriftReadOnlyCollectorStrategy {
    pub name: &'static str,
    pub lock_acquisition_class: &'static str,
    pub true_read_only_database_open_available: bool,
    pub memory_evidence_inspected_on_lock_contention: bool,
    pub unblocks_true_read_only_bead: &'static str,
}

#[must_use]
pub const fn memory_drift_read_only_collector_strategy() -> MemoryDriftReadOnlyCollectorStrategy {
    MemoryDriftReadOnlyCollectorStrategy {
        name: MEMORY_DRIFT_READ_ONLY_COLLECTOR_STRATEGY_NAME,
        lock_acquisition_class: MEMORY_DRIFT_READ_ONLY_COLLECTOR_LOCK_CLASS,
        true_read_only_database_open_available: true,
        memory_evidence_inspected_on_lock_contention: false,
        unblocks_true_read_only_bead: MEMORY_DRIFT_TRUE_READ_ONLY_DATABASE_OPEN_BEAD,
    }
}

pub fn build_memory_drift_report(
    options: &MemoryDriftReportOptions<'_>,
) -> Result<MemoryDriftReport, DomainError> {
    let connection = open_memory_drift_database(options.database_path)?;
    build_memory_drift_report_with_connection(&connection, options)
}

/// Degraded code for a non-contention memory-drift collection failure before
/// any evidence was inspected. Missing/unsafe database paths and generic
/// read-only report failures use this code; structurally inspected evidence
/// that cannot be verified uses `memory_drift_source_unverifiable` instead.
pub const MEMORY_DRIFT_REPORT_UNAVAILABLE_CODE: &str = "memory_drift_report_unavailable";

/// bd-1xpq9: degraded code for memory-drift COLLECTION being blocked by
/// database open or read-snapshot contention. Distinct from both
/// `memory_drift_report_unavailable` (non-contention failure before inspection)
/// and `memory_drift_source_unverifiable` (inspected evidence that could not be
/// verified).
pub const MEMORY_DRIFT_LOCK_CONTENTION_CODE: &str = "memory_drift_lock_contention";

/// Detail schema for lock-contention emissions (bd-1xpq9).
pub const MEMORY_DRIFT_LOCK_CONTENTION_SCHEMA: &str = "ee.memory_drift.lock_contention.v1";

/// True when a memory-drift collection error is database open or read-snapshot
/// contention. The genuinely read-only collector does not acquire the
/// workspace write-owner flock, so write-lock acquisition errors deliberately
/// do not match this classifier.
#[must_use]
pub fn memory_drift_error_is_lock_contention(error: &DomainError) -> bool {
    memory_drift_error_text_is_lock_contention(&error.message())
}

fn memory_drift_error_text_is_lock_contention(text: &str) -> bool {
    let text = text.to_ascii_lowercase();
    text.contains("database is busy")
        || text.contains("database is locked")
        || text.contains("database table is locked")
        || text.contains("snapshot conflict")
        || text.contains("database read-snapshot contention")
        || text.contains("database read snapshot contention")
}

/// Redaction-safe structured details for a lock-contention emission:
/// names the collector surface and lock class, and states explicitly
/// that NO memory evidence was inspected — so consumers can never
/// confuse this with stale provenance. No lock-holder internals, no
/// host-private absolute paths.
#[must_use]
pub fn memory_drift_lock_contention_details(collector_surface: &str) -> serde_json::Value {
    serde_json::json!({
        "schema": MEMORY_DRIFT_LOCK_CONTENTION_SCHEMA,
        "collectorSurface": collector_surface,
        "lockAcquisitionClass": MEMORY_DRIFT_READ_ONLY_COLLECTOR_LOCK_CLASS,
        "sourceFreshness": "not_inspected",
        "memoryEvidenceInspected": false,
    })
}

/// Canonical agent-facing message for the lock-contention code.
#[must_use]
pub fn memory_drift_lock_contention_message(collector_surface: &str) -> String {
    format!(
        "Memory drift collection on {collector_surface} was blocked by database read-snapshot \
         contention before any evidence inspection; memory evidence was NOT inspected and this \
         does not indicate stale provenance."
    )
}

/// Canonical non-mutating repair guidance for the lock-contention code.
pub const MEMORY_DRIFT_LOCK_CONTENTION_REPAIR: &str = "Retry the read-only drift report; if contention persists, inspect database health with `ee doctor --json`.";

pub fn build_memory_drift_report_read_only(
    options: &MemoryDriftReportOptions<'_>,
) -> Result<MemoryDriftReport, DomainError> {
    let connection = open_memory_drift_database_for_read_only_report(options.database_path)?;
    build_memory_drift_report_with_connection(&connection, options)
}

fn open_memory_drift_database_for_read_only_report(
    database_path: &Path,
) -> Result<DbConnection, DomainError> {
    let strategy = memory_drift_read_only_collector_strategy();
    debug_assert_eq!(
        strategy.name,
        MEMORY_DRIFT_READ_ONLY_COLLECTOR_STRATEGY_NAME
    );
    DbConnection::open_file_read_only(database_path).map_err(memory_drift_read_only_open_error)
}

fn memory_drift_read_only_open_error(error: DbError) -> DomainError {
    let error_text = error.to_string();
    if memory_drift_error_text_is_lock_contention(&error_text) {
        return DomainError::Storage {
            message: format!(
                "Memory drift read-only collector strategy `{}` was blocked by database \
                 read-snapshot contention before any evidence inspection.",
                MEMORY_DRIFT_READ_ONLY_COLLECTOR_STRATEGY_NAME
            ),
            repair: Some(MEMORY_DRIFT_LOCK_CONTENTION_REPAIR.to_owned()),
        };
    }

    DomainError::Storage {
        message: format!("Failed to open database read-only for memory drift report: {error_text}"),
        repair: Some("ee doctor --json".to_owned()),
    }
}

fn build_memory_drift_report_with_connection(
    connection: &DbConnection,
    options: &MemoryDriftReportOptions<'_>,
) -> Result<MemoryDriftReport, DomainError> {
    connection
        .begin_read_snapshot()
        .map_err(|error| DomainError::Storage {
            message: format!("Failed to begin memory drift read snapshot: {error}"),
            repair: Some("ee doctor --json".to_owned()),
        })?;
    let result = build_memory_drift_report_in_snapshot(connection, options);
    match result {
        Ok(report) => {
            if let Err(commit_error) = connection.commit_read_snapshot() {
                let rollback_detail = connection
                    .rollback_read_snapshot()
                    .err()
                    .map(|error| format!("; rollback error: {error}"))
                    .unwrap_or_default();
                return Err(DomainError::Storage {
                    message: format!(
                        "Failed to commit memory drift read snapshot: {commit_error}{rollback_detail}"
                    ),
                    repair: Some("ee doctor --json".to_owned()),
                });
            }
            Ok(report)
        }
        Err(error) => {
            if let Err(rollback_error) = connection.rollback_read_snapshot() {
                return Err(DomainError::Storage {
                    message: format!(
                        "Memory drift collection failed and its read snapshot could not be rolled back: {}; rollback error: {rollback_error}",
                        error.message()
                    ),
                    repair: Some("ee doctor --json".to_owned()),
                });
            }
            Err(error)
        }
    }
}

fn build_memory_drift_report_in_snapshot(
    connection: &DbConnection,
    options: &MemoryDriftReportOptions<'_>,
) -> Result<MemoryDriftReport, DomainError> {
    let workspace_path = options
        .workspace_path
        .canonicalize()
        .unwrap_or_else(|_| options.workspace_path.to_path_buf());
    // GH#23: prefer the DB's own path-keyed workspace row (user-global store
    // support), falling back to the canonical-path hash.
    let workspace_id = super::memory::workspace_id_for_database(connection, options.workspace_path);
    let limit = options.limit.max(1);

    let (items, generated_at) = match options.mode {
        MemoryDriftReportMode::AllMemories => (
            memory_drift_report_all_memories(
                connection,
                &workspace_path,
                &workspace_id,
                limit,
                options.include_tombstoned,
            )?,
            None,
        ),
        MemoryDriftReportMode::OneMemory => {
            let memory_id = options.memory_id.ok_or_else(|| DomainError::Usage {
                message: "memory drift --mode one requires MEMORY_ID".to_owned(),
                repair: Some("ee memory drift --help".to_owned()),
            })?;
            (
                vec![memory_drift_report_one_memory(
                    connection,
                    &workspace_path,
                    memory_id,
                    options.include_tombstoned,
                )?],
                None,
            )
        }
        MemoryDriftReportMode::RecentPackItems => memory_drift_report_recent_pack_items(
            connection,
            &workspace_path,
            &workspace_id,
            limit,
            options.as_of,
        )?,
    };

    Ok(MemoryDriftReport::new(
        options.mode,
        generated_at.as_deref(),
        items,
    ))
}

fn open_memory_drift_database(database_path: &Path) -> Result<DbConnection, DomainError> {
    let connection =
        DbConnection::open_file(database_path).map_err(|error| DomainError::Storage {
            message: format!("Failed to open database: {error}"),
            repair: Some("ee init --workspace .".to_owned()),
        })?;
    connection.migrate().map_err(|error| DomainError::Storage {
        message: format!("Failed to migrate database: {error}"),
        repair: Some("ee migrate run --workspace .".to_owned()),
    })?;
    Ok(connection)
}

fn memory_drift_report_all_memories(
    connection: &DbConnection,
    workspace_path: &Path,
    workspace_id: &str,
    limit: u32,
    include_tombstoned: bool,
) -> Result<Vec<MemoryDriftSelectionHint>, DomainError> {
    let memories = connection
        .list_memories(workspace_id, None, include_tombstoned)
        .map_err(|error| DomainError::Storage {
            message: format!("Failed to list memories for drift report: {error}"),
            repair: Some("ee doctor --json".to_owned()),
        })?;
    memories
        .iter()
        .take(limit as usize)
        .map(|memory| memory_drift_report_hint_from_memory(connection, workspace_path, memory))
        .collect()
}

fn memory_drift_report_one_memory(
    connection: &DbConnection,
    workspace_path: &Path,
    memory_id: &str,
    include_tombstoned: bool,
) -> Result<MemoryDriftSelectionHint, DomainError> {
    let memory = connection
        .get_memory(memory_id)
        .map_err(|error| DomainError::Storage {
            message: format!("Failed to query memory for drift report: {error}"),
            repair: Some("ee doctor --json".to_owned()),
        })?
        .ok_or_else(|| DomainError::NotFound {
            resource: "memory".to_owned(),
            id: memory_id.to_owned(),
            repair: Some("ee memory list --json".to_owned()),
        })?;
    if memory.tombstoned_at.is_some() && !include_tombstoned {
        return Err(DomainError::NotFound {
            resource: "memory".to_owned(),
            id: memory_id.to_owned(),
            repair: Some("rerun with --include-tombstoned or list active memories".to_owned()),
        });
    }
    memory_drift_report_hint_from_memory(connection, workspace_path, &memory)
}

fn memory_drift_report_recent_pack_items(
    connection: &DbConnection,
    workspace_path: &Path,
    workspace_id: &str,
    limit: u32,
    requested_as_of: Option<DateTime<Utc>>,
) -> Result<(Vec<MemoryDriftSelectionHint>, Option<String>), DomainError> {
    memory_drift_report_recent_pack_items_with_scan_cap(
        connection,
        workspace_path,
        workspace_id,
        limit,
        requested_as_of,
        RECENT_PACK_MEMORY_DRIFT_SCAN_CAP,
    )
}

fn memory_drift_report_recent_pack_items_with_scan_cap(
    connection: &DbConnection,
    workspace_path: &Path,
    workspace_id: &str,
    limit: u32,
    requested_as_of: Option<DateTime<Utc>>,
    scan_cap: u32,
) -> Result<(Vec<MemoryDriftSelectionHint>, Option<String>), DomainError> {
    let scan_cap = scan_cap.max(1);
    let scan_limit = scan_cap.saturating_add(1);
    let mut pack_record_ids = connection
        .list_pack_record_ids_for_memory_drift(workspace_id, scan_limit)
        .map_err(|error| DomainError::Storage {
            message: format!("Failed to list recent pack identities for drift report: {error}"),
            repair: Some("ee doctor --json".to_owned()),
        })?;

    // The first SELECT above binds the deferred read transaction. Capture the
    // decision clock only after that bind so a concurrent pack cannot appear
    // in this snapshot with a timestamp later than a pre-query wall clock.
    let as_of = requested_as_of.unwrap_or_else(Utc::now);
    let horizon_start = as_of
        .checked_sub_signed(ChronoDuration::days(RECENT_PACK_MEMORY_DRIFT_HORIZON_DAYS))
        .ok_or_else(|| DomainError::Usage {
            message: "Memory drift decision clock cannot represent the seven-day horizon"
                .to_owned(),
            repair: Some(
                "Use an RFC 3339 --as-of value within Chrono's supported range.".to_owned(),
            ),
        })?;
    let records_truncated = pack_record_ids.len() > scan_cap as usize;
    let mut scan_truncated = records_truncated;
    pack_record_ids.truncate(scan_cap as usize);
    let mut findings_by_chain = BTreeMap::<String, MemoryDriftSelectionHint>::new();
    let mut selections_by_chain = BTreeMap::<String, Vec<RecentPackRevisionSelection>>::new();
    let mut admitted_items = Vec::<RecentPackLedgerSelection>::new();
    let mut scanned_units = 0_u32;

    for (selection_order, record_id) in pack_record_ids {
        if scanned_units >= scan_cap {
            scan_truncated = true;
            break;
        }
        let record = connection
            .get_pack_record_for_memory_drift(&record_id)
            .map_err(|error| DomainError::Storage {
                message: format!(
                    "Failed to load recent pack record {} for drift report: {error}",
                    crate::models::public_pack_id(&record_id)
                ),
                repair: Some("ee doctor --json".to_owned()),
            })?;
        let Some(record) = record else {
            findings_by_chain.insert(
                format!("pack:{record_id}"),
                recent_pack_integrity_finding(&record_id, "pack_record_missing", 1),
            );
            scanned_units = scanned_units.saturating_add(1);
            continue;
        };
        if record.workspace_id != workspace_id {
            findings_by_chain.insert(
                format!("pack:{record_id}"),
                recent_pack_integrity_finding(
                    &record_id,
                    "pack_record_workspace_mismatch",
                    record.item_count.max(1),
                ),
            );
            scanned_units = scanned_units.saturating_add(1);
            continue;
        }
        let validated = match validated_recent_pack_record(&record) {
            Ok(validated) => validated,
            Err((reason, finding_id)) => {
                let finding_id = finding_id.as_deref().unwrap_or(&record.id);
                findings_by_chain
                    .entry(format!("pack:{}", record.id))
                    .or_insert_with(|| {
                        recent_pack_integrity_finding(finding_id, reason, record.item_count.max(1))
                    });
                scanned_units = scanned_units.saturating_add(1);
                continue;
            }
        };
        let pack_units = u32::try_from(validated.memory_ids.len())
            .unwrap_or(u32::MAX)
            .max(1);
        let remaining = scan_cap.saturating_sub(scanned_units);
        let admitted_count = pack_units.min(remaining);
        scanned_units = scanned_units.saturating_add(admitted_count);
        let pack_overflowed = pack_units > remaining;
        if pack_overflowed {
            scan_truncated = true;
        }

        let finding_id = validated
            .memory_ids
            .first()
            .map(String::as_str)
            .unwrap_or(record.id.as_str());
        if validated.selected_at > as_of {
            findings_by_chain
                .entry(format!("pack:{}", record.id))
                .or_insert_with(|| {
                    recent_pack_integrity_finding(
                        finding_id,
                        "pack_item_selected_at_future",
                        pack_units,
                    )
                });
            continue;
        }

        // The boundary is intentionally inclusive: exactly seven days old is
        // still recent enough to affect a claim. Only strictly older rows are
        // historical diagnostics rather than claim authority.
        if validated.selected_at < horizon_start {
            continue;
        }

        admitted_items.extend(
            validated
                .memory_ids
                .into_iter()
                .take(admitted_count as usize)
                .map(|memory_id| RecentPackLedgerSelection {
                    selection_order,
                    selected_at: validated.selected_at,
                    memory_id,
                }),
        );
        if pack_overflowed {
            break;
        }
    }

    if scan_truncated {
        findings_by_chain.insert(
            "scan:truncated".to_owned(),
            recent_pack_integrity_finding(
                "recent-pack-authority-scan",
                "pack_item_authority_scan_truncated",
                scan_cap,
            ),
        );
    }

    for admitted in admitted_items {
        let memory_id = admitted.memory_id;
        let selected_memory =
            connection
                .get_memory(&memory_id)
                .map_err(|error| DomainError::Storage {
                    message: format!(
                        "Failed to query selected pack memory {} for drift report: {error}",
                        memory_id
                    ),
                    repair: Some("ee doctor --json".to_owned()),
                })?;
        let Some(selected_memory) = selected_memory else {
            findings_by_chain
                .entry(format!("missing:{memory_id}"))
                .or_insert_with(|| {
                    recent_pack_integrity_finding(&memory_id, "pack_item_memory_row_missing", 1)
                });
            continue;
        };
        if selected_memory.workspace_id != workspace_id {
            findings_by_chain
                .entry(format!("workspace_mismatch:{memory_id}"))
                .or_insert_with(|| {
                    recent_pack_integrity_finding(
                        &memory_id,
                        "pack_item_memory_workspace_mismatch",
                        1,
                    )
                });
            continue;
        }

        let logical_id = connection
            .get_memory_logical_id(&memory_id)
            .map_err(|error| DomainError::Storage {
                message: format!(
                    "Failed to query revision lineage for selected pack memory {}: {error}",
                    memory_id
                ),
                repair: Some("ee doctor --json".to_owned()),
            })?
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty());
        let Some(logical_id) = logical_id else {
            findings_by_chain
                .entry(format!("lineage_missing:{memory_id}"))
                .or_insert_with(|| {
                    recent_pack_integrity_finding(
                        &memory_id,
                        "pack_item_revision_lineage_missing",
                        1,
                    )
                });
            continue;
        };

        let live_revisions = connection
            .list_live_memory_revisions_for_logical_id(workspace_id, &logical_id)
            .map_err(|error| DomainError::Storage {
                message: format!(
                    "Failed to resolve live revision for selected pack memory {}: {error}",
                    memory_id
                ),
                repair: Some("ee doctor --json".to_owned()),
            })?;
        let [live_memory] = live_revisions.as_slice() else {
            let reason = if live_revisions.is_empty() {
                "pack_item_live_revision_missing"
            } else {
                "pack_item_live_revision_ambiguous"
            };
            findings_by_chain.entry(logical_id).or_insert_with(|| {
                recent_pack_integrity_finding(
                    &memory_id,
                    reason,
                    u32::try_from(live_revisions.len()).unwrap_or(u32::MAX),
                )
            });
            continue;
        };

        selections_by_chain
            .entry(logical_id.clone())
            .or_default()
            .push(RecentPackRevisionSelection {
                selected_memory_id: memory_id,
                selected_at: admitted.selected_at,
                selection_order: admitted.selection_order,
                live_memory: live_memory.clone(),
            });
    }

    for (logical_id, selections) in selections_by_chain {
        if findings_by_chain.contains_key(&logical_id) {
            continue;
        }
        let Some(latest_selection) = selections
            .iter()
            .map(|selection| (selection.selected_at, selection.selection_order))
            .max()
        else {
            continue;
        };
        let latest_selected_ids = selections
            .iter()
            .filter(|selection| {
                (selection.selected_at, selection.selection_order) == latest_selection
            })
            .map(|selection| selection.selected_memory_id.clone())
            .collect::<BTreeSet<_>>();
        let live_ids = selections
            .iter()
            .map(|selection| selection.live_memory.id.clone())
            .collect::<BTreeSet<_>>();

        if live_ids.len() != 1 || latest_selected_ids.len() != 1 {
            let memory_id = latest_selected_ids
                .iter()
                .next()
                .or_else(|| live_ids.iter().next())
                .cloned()
                .unwrap_or_else(|| logical_id.clone());
            findings_by_chain.insert(
                logical_id,
                recent_pack_integrity_finding(
                    &memory_id,
                    "pack_item_revision_selection_ambiguous",
                    u32::try_from(latest_selected_ids.len()).unwrap_or(u32::MAX),
                ),
            );
            continue;
        }

        let Some(live_memory) = selections.first().map(|selection| &selection.live_memory) else {
            continue;
        };
        let Some(latest_selected_id) = latest_selected_ids.iter().next() else {
            continue;
        };
        if latest_selected_id != &live_memory.id {
            findings_by_chain.insert(
                logical_id,
                MemoryDriftSelectionHint::new(
                    latest_selected_id,
                    MemoryDriftStatus::Unverifiable,
                    "pack_item_superseded_since_selection",
                    1,
                ),
            );
            continue;
        }

        findings_by_chain.insert(
            logical_id,
            memory_drift_report_hint_from_memory(connection, workspace_path, live_memory)?,
        );
    }

    let mut hints = findings_by_chain.into_values().collect::<Vec<_>>();
    hints.sort_by_key(MemoryDriftSelectionHint::sort_key);
    hints.truncate(limit as usize);
    Ok((hints, Some(as_of.to_rfc3339())))
}

#[derive(Clone, Debug)]
struct RecentPackRevisionSelection {
    selected_memory_id: String,
    selected_at: DateTime<Utc>,
    selection_order: i64,
    live_memory: StoredMemory,
}

#[derive(Clone, Debug)]
struct RecentPackLedgerSelection {
    selection_order: i64,
    selected_at: DateTime<Utc>,
    memory_id: String,
}

#[derive(Clone, Debug)]
struct ValidatedRecentPackRecord {
    selected_at: DateTime<Utc>,
    memory_ids: Vec<String>,
}

fn validated_recent_pack_record(
    record: &StoredPackRecord,
) -> Result<ValidatedRecentPackRecord, (&'static str, Option<String>)> {
    let parsed = parse_stored_pack_ledger(record);
    let reason = match parsed.status {
        PackLedgerStatus::Available => None,
        PackLedgerStatus::Missing => Some("pack_item_ledger_missing"),
        PackLedgerStatus::Malformed => Some("pack_item_ledger_malformed"),
        PackLedgerStatus::HashMismatch => Some("pack_item_ledger_hash_mismatch"),
    };
    if let Some(reason) = reason {
        return Err((reason, None));
    }
    let ledger = parsed
        .available_ledger()
        .ok_or(("pack_item_ledger_missing", None))?;
    if ledger.get("packId").and_then(serde_json::Value::as_str) != Some(record.id.as_str())
        || ledger
            .get("workspaceId")
            .and_then(serde_json::Value::as_str)
            != Some(record.workspace_id.as_str())
    {
        return Err(("pack_item_ledger_identity_mismatch", None));
    }
    if ledger.get("packHash").and_then(serde_json::Value::as_str) != Some(record.pack_hash.as_str())
    {
        return Err(("pack_item_ledger_record_mismatch", None));
    }
    let selected_items = ledger
        .get("selectedItems")
        .and_then(serde_json::Value::as_array)
        .ok_or(("pack_item_ledger_selected_items_missing", None))?;
    let finding_id = selected_items
        .first()
        .and_then(|item| item.get("memoryId"))
        .and_then(serde_json::Value::as_str)
        .filter(|memory_id| !memory_id.trim().is_empty())
        .map(str::to_owned);
    let ledger_created_at = ledger
        .get("createdAt")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| ("pack_item_ledger_timestamp_missing", finding_id.clone()))?;
    if ledger_created_at != record.created_at {
        return Err(("pack_item_ledger_timestamp_mismatch", finding_id.clone()));
    }
    let selected_at = parse_recent_pack_selection_time(ledger_created_at)
        .map_err(|()| ("pack_item_selected_at_malformed", finding_id.clone()))?;
    let omitted_items = ledger
        .get("omittedItems")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| ("pack_item_ledger_omitted_items_missing", finding_id.clone()))?;
    let candidate_counts = ledger
        .get("candidateCounts")
        .and_then(serde_json::Value::as_object)
        .ok_or_else(|| {
            (
                "pack_item_ledger_candidate_counts_missing",
                finding_id.clone(),
            )
        })?;
    if selected_items.len() != record.item_count as usize
        || omitted_items.len() != record.omitted_count as usize
        || candidate_counts
            .get("selected")
            .and_then(serde_json::Value::as_u64)
            != Some(u64::from(record.item_count))
        || candidate_counts
            .get("omitted")
            .and_then(serde_json::Value::as_u64)
            != Some(u64::from(record.omitted_count))
        || candidate_counts
            .get("candidatePool")
            .and_then(serde_json::Value::as_u64)
            != Some(u64::from(record.item_count) + u64::from(record.omitted_count))
    {
        return Err(("pack_item_ledger_count_mismatch", finding_id.clone()));
    }

    let mut ranked_ids = Vec::with_capacity(selected_items.len());
    for item in selected_items {
        let memory_id = item
            .get("memoryId")
            .and_then(serde_json::Value::as_str)
            .filter(|memory_id| !memory_id.trim().is_empty())
            .ok_or_else(|| {
                (
                    "pack_item_ledger_selected_item_malformed",
                    finding_id.clone(),
                )
            })?;
        let rank = item
            .get("rank")
            .and_then(serde_json::Value::as_u64)
            .and_then(|rank| u32::try_from(rank).ok())
            .ok_or_else(|| {
                (
                    "pack_item_ledger_selected_item_malformed",
                    finding_id.clone(),
                )
            })?;
        ranked_ids.push((rank, memory_id.to_owned()));
    }
    ranked_ids.sort();
    let distinct_ranks = ranked_ids
        .iter()
        .map(|(rank, _)| *rank)
        .collect::<BTreeSet<_>>();
    let distinct_ids = ranked_ids
        .iter()
        .map(|(_, memory_id)| memory_id.as_str())
        .collect::<BTreeSet<_>>();
    if distinct_ranks.len() != ranked_ids.len() || distinct_ids.len() != ranked_ids.len() {
        return Err(("pack_item_ledger_selected_item_ambiguous", finding_id));
    }

    Ok(ValidatedRecentPackRecord {
        selected_at,
        memory_ids: ranked_ids
            .into_iter()
            .map(|(_, memory_id)| memory_id)
            .collect(),
    })
}

fn recent_pack_integrity_finding(
    memory_id: &str,
    reason: &str,
    evidence_count: u32,
) -> MemoryDriftSelectionHint {
    let public_memory_id = crate::models::public_memory_id(memory_id);
    let mut finding = MemoryDriftSelectionHint::new(
        &public_memory_id,
        MemoryDriftStatus::Unverifiable,
        reason,
        evidence_count,
    );
    finding.revalidation_command = "ee doctor --json".to_owned();
    finding
}

fn parse_recent_pack_selection_time(raw: &str) -> Result<DateTime<Utc>, ()> {
    if raw.is_empty() || raw != raw.trim() {
        return Err(());
    }
    DateTime::parse_from_rfc3339(raw)
        .map(|timestamp| timestamp.with_timezone(&Utc))
        .map_err(|_| ())
}

pub fn memory_drift_selection_hint_for_memory(
    connection: &DbConnection,
    workspace_path: &Path,
    memory: &StoredMemory,
) -> Result<Option<MemoryDriftSelectionHint>, DomainError> {
    let hint = memory_drift_report_hint_from_memory(connection, workspace_path, memory)?;
    let provenance_selection = matches!(
        memory.provenance_verification_status.trim(),
        "mismatch" | "missing" | "skipped"
    );
    let anchor_selection = hint.stale_anchor
        || hint
            .anchors
            .iter()
            .any(|anchor| anchor.freshness_state != "current");
    if provenance_selection || anchor_selection {
        Ok(Some(hint))
    } else {
        Ok(None)
    }
}

fn memory_drift_report_hint_from_memory(
    connection: &DbConnection,
    workspace_path: &Path,
    memory: &StoredMemory,
) -> Result<MemoryDriftSelectionHint, DomainError> {
    let anchors = connection
        .list_memory_anchors(&memory.id)
        .map_err(|error| DomainError::Storage {
            message: format!("Failed to list memory anchors for drift report: {error}"),
            repair: Some("ee doctor --json".to_owned()),
        })?;
    Ok(memory_drift_report_hint_from_memory_and_anchors(
        workspace_path,
        memory,
        anchors,
    ))
}

fn memory_drift_report_hint_from_memory_and_anchors(
    workspace_path: &Path,
    memory: &StoredMemory,
    anchors: Vec<StoredMemoryAnchor>,
) -> MemoryDriftSelectionHint {
    let captured_commit = captured_commit_for_memory(memory, &anchors);
    let live_resolution = resolve_live_code_anchor_freshness(
        workspace_path,
        memory,
        anchors,
        captured_commit.as_deref(),
    );
    let anchors = live_resolution.anchors;
    let mut hint = memory_drift_report_hint_from_provenance_status(
        &memory.id,
        &memory.provenance_verification_status,
        memory.provenance_chain_hash.as_deref(),
    );
    let anchor_status = live_resolution
        .status
        .or_else(|| memory_drift_status_from_stored_anchors(&anchors));
    let routine_unverified_is_advisory =
        hint.top_reason == "provenance_not_yet_verified" && hint.degraded_code.is_none();
    if let Some((status, reason)) = anchor_status
        && (status.severity_rank() > hint.drift_status.severity_rank()
            || routine_unverified_is_advisory)
    {
        hint = MemoryDriftSelectionHint::new(&memory.id, status, reason, 0);
    }
    let stale_anchor = !anchors.is_empty()
        && (anchor_status.is_some()
            || matches!(
                hint.drift_status,
                MemoryDriftStatus::Changed
                    | MemoryDriftStatus::MissingSource
                    | MemoryDriftStatus::StaleAnchor
            ));
    let current_commit = if anchors.is_empty() && captured_commit.is_none() {
        None
    } else {
        current_git_commit(workspace_path)
    };
    let commit_distance = captured_commit
        .as_deref()
        .zip(current_commit.as_deref())
        .and_then(|(captured, current)| git_commit_distance(workspace_path, captured, current));
    let code_anchors = memory_drift_code_anchors(&anchors, hint.drift_status);
    let mut changed_regions = if stale_anchor {
        code_anchors
            .iter()
            .map(|anchor| anchor.redacted_anchor_value.clone())
            .collect::<Vec<_>>()
    } else {
        Vec::new()
    };
    changed_regions.sort();
    changed_regions.dedup();

    hint.with_code_anchor_context(
        code_anchors,
        captured_commit,
        current_commit,
        commit_distance,
        changed_regions,
        stale_anchor,
    )
}

struct LiveCodeAnchorResolution {
    anchors: Vec<StoredMemoryAnchor>,
    status: Option<(MemoryDriftStatus, &'static str)>,
}

fn resolve_live_code_anchor_freshness(
    workspace_path: &Path,
    memory: &StoredMemory,
    mut anchors: Vec<StoredMemoryAnchor>,
    captured_commit: Option<&str>,
) -> LiveCodeAnchorResolution {
    let normalized_values = normalized_code_anchor_values(memory);
    let mut status = None;

    for anchor in &mut anchors {
        if anchor.anchor_kind != MemoryAnchorKind::Path {
            continue;
        }
        let Some(normalized_path) =
            normalized_values.get(&(anchor.anchor_kind, anchor.anchor_value_hash.clone()))
        else {
            continue;
        };
        let observed =
            observe_path_anchor_freshness(workspace_path, normalized_path, anchor, captured_commit);
        anchor.freshness_state = observed.freshness_state;
        if observed.status != MemoryDriftStatus::Current {
            status = max_memory_drift_status(status, Some((observed.status, observed.reason)));
        }
    }

    LiveCodeAnchorResolution { anchors, status }
}

fn normalized_code_anchor_values(
    memory: &StoredMemory,
) -> BTreeMap<(MemoryAnchorKind, String), String> {
    extract_memory_anchor_surfaces(
        &memory.id,
        &memory.content,
        MemoryAnchorSource::Remember,
        memory.provenance_uri.as_deref(),
    )
    .into_iter()
    .map(|surface| {
        (
            (
                surface.anchor.anchor_kind,
                surface.anchor.anchor_value_hash.clone(),
            ),
            surface.normalized_value,
        )
    })
    .collect()
}

struct ObservedPathAnchorFreshness {
    status: MemoryDriftStatus,
    reason: &'static str,
    freshness_state: MemoryAnchorFreshnessState,
}

fn observe_path_anchor_freshness(
    workspace_path: &Path,
    normalized_path: &str,
    anchor: &StoredMemoryAnchor,
    captured_commit: Option<&str>,
) -> ObservedPathAnchorFreshness {
    let relative_path = Path::new(normalized_path);
    if !is_safe_workspace_relative_path(relative_path) {
        return observed_path_anchor_status(
            MemoryDriftStatus::Unverifiable,
            "code_anchor_path_unverifiable",
        );
    }

    let source_path = workspace_path.join(relative_path);
    let metadata = match fs::metadata(&source_path) {
        Ok(metadata) if metadata.is_file() => metadata,
        Ok(_) => {
            return observed_path_anchor_status(
                MemoryDriftStatus::Unverifiable,
                "code_anchor_not_file",
            );
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return observed_path_anchor_status(
                MemoryDriftStatus::MissingSource,
                "code_anchor_file_missing",
            );
        }
        Err(_) => {
            return observed_path_anchor_status(
                MemoryDriftStatus::Unverifiable,
                "code_anchor_file_unreadable",
            );
        }
    };

    let current_bytes = match fs::read(&source_path) {
        Ok(bytes) => bytes,
        Err(_) => {
            return observed_path_anchor_status(
                MemoryDriftStatus::Unverifiable,
                "code_anchor_file_unreadable",
            );
        }
    };
    let current_hash = memory_drift_content_hash(&current_bytes);

    if let Some(commit) = captured_commit {
        if let Some(captured_bytes) = git_blob_at_commit(workspace_path, commit, normalized_path) {
            let captured_hash = memory_drift_content_hash(&captured_bytes);
            if captured_hash != current_hash {
                return observed_path_anchor_status(
                    MemoryDriftStatus::Changed,
                    "code_anchor_hash_changed",
                );
            }
            return observed_path_anchor_status(
                MemoryDriftStatus::Current,
                "code_anchor_hash_current",
            );
        }
        return observed_path_anchor_status(
            MemoryDriftStatus::Unverifiable,
            "code_anchor_capture_unavailable",
        );
    }

    let source_modified_at = metadata.modified().ok().map(DateTime::<Utc>::from);
    let anchor_updated_at = DateTime::parse_from_rfc3339(&anchor.updated_at)
        .ok()
        .map(|timestamp| timestamp.with_timezone(&Utc));
    if let (Some(source_modified_at), Some(anchor_updated_at)) =
        (source_modified_at, anchor_updated_at)
        && source_modified_at > anchor_updated_at
    {
        return observed_path_anchor_status(
            MemoryDriftStatus::Unverifiable,
            "code_anchor_mtime_newer_than_anchor",
        );
    }

    observed_path_anchor_status(MemoryDriftStatus::Current, "code_anchor_mtime_current")
}

fn observed_path_anchor_status(
    status: MemoryDriftStatus,
    reason: &'static str,
) -> ObservedPathAnchorFreshness {
    ObservedPathAnchorFreshness {
        status,
        reason,
        freshness_state: freshness_state_for_drift(status),
    }
}

fn max_memory_drift_status(
    left: Option<(MemoryDriftStatus, &'static str)>,
    right: Option<(MemoryDriftStatus, &'static str)>,
) -> Option<(MemoryDriftStatus, &'static str)> {
    match (left, right) {
        (None, None) => None,
        (Some(status), None) | (None, Some(status)) => Some(status),
        (Some(left), Some(right)) => {
            if right.0.severity_rank() > left.0.severity_rank() {
                Some(right)
            } else {
                Some(left)
            }
        }
    }
}

fn is_safe_workspace_relative_path(path: &Path) -> bool {
    !path.as_os_str().is_empty()
        && !path.is_absolute()
        && path
            .components()
            .all(|component| matches!(component, Component::Normal(_) | Component::CurDir))
}

fn memory_drift_status_from_stored_anchors(
    anchors: &[StoredMemoryAnchor],
) -> Option<(MemoryDriftStatus, &'static str)> {
    if anchors
        .iter()
        .any(|anchor| anchor.freshness_state == MemoryAnchorFreshnessState::Stale)
    {
        return Some((MemoryDriftStatus::StaleAnchor, "code_anchor_stale"));
    }
    if anchors
        .iter()
        .any(|anchor| anchor.freshness_state == MemoryAnchorFreshnessState::Suspect)
    {
        return Some((MemoryDriftStatus::Unverifiable, "code_anchor_suspect"));
    }
    None
}

fn memory_drift_code_anchors(
    anchors: &[StoredMemoryAnchor],
    drift_status: MemoryDriftStatus,
) -> Vec<MemoryDriftCodeAnchor> {
    let mut output = anchors
        .iter()
        .map(|anchor| {
            let freshness = memory_drift_anchor_freshness_label(anchor, drift_status);
            let stale_anchor = matches!(
                anchor.freshness_state,
                MemoryAnchorFreshnessState::Stale | MemoryAnchorFreshnessState::Suspect
            ) || matches!(
                drift_status,
                MemoryDriftStatus::Changed
                    | MemoryDriftStatus::MissingSource
                    | MemoryDriftStatus::StaleAnchor
            );
            MemoryDriftCodeAnchor {
                anchor_kind: anchor.anchor_kind.as_str().to_owned(),
                anchor_value_hash: anchor.anchor_value_hash.clone(),
                redacted_anchor_value: anchor.redacted_anchor_value.clone(),
                captured_span_hash: anchor.captured_span_hash.clone(),
                freshness_state: anchor.freshness_state.as_str().to_owned(),
                freshness: freshness.to_owned(),
                generation: anchor.generation,
                stale_anchor,
            }
        })
        .collect::<Vec<_>>();
    output.sort_by(|left, right| {
        left.anchor_kind
            .cmp(&right.anchor_kind)
            .then_with(|| left.anchor_value_hash.cmp(&right.anchor_value_hash))
    });
    output
}

fn memory_drift_anchor_freshness_label(
    anchor: &StoredMemoryAnchor,
    drift_status: MemoryDriftStatus,
) -> &'static str {
    match anchor.freshness_state {
        MemoryAnchorFreshnessState::Stale => "drifted",
        MemoryAnchorFreshnessState::Suspect => "unknown",
        MemoryAnchorFreshnessState::Current => memory_drift_freshness_label(drift_status),
    }
}

fn captured_commit_for_memory(
    memory: &StoredMemory,
    anchors: &[StoredMemoryAnchor],
) -> Option<String> {
    memory
        .provenance_uri
        .as_deref()
        .and_then(first_hex_commit)
        .or_else(|| first_hex_commit(&memory.content))
        .or_else(|| {
            anchors
                .iter()
                .find_map(|anchor| first_hex_commit(&anchor.provenance))
        })
}

fn current_git_commit(workspace_path: &Path) -> Option<String> {
    git_output(workspace_path, &["rev-parse", "--verify", "HEAD"])
        .and_then(|output| first_hex_commit(&output))
}

fn git_commit_distance(workspace_path: &Path, captured: &str, current: &str) -> Option<u32> {
    if captured == current {
        return Some(0);
    }
    if !is_hex_commit(captured) || !is_hex_commit(current) {
        return None;
    }
    git_output(
        workspace_path,
        &["rev-list", "--count", &format!("{captured}..{current}")],
    )
    .and_then(|output| output.trim().parse::<u32>().ok())
}

fn git_output(workspace_path: &Path, args: &[&str]) -> Option<String> {
    String::from_utf8(git_output_bytes(workspace_path, args)?).ok()
}

fn git_output_bytes(workspace_path: &Path, args: &[&str]) -> Option<Vec<u8>> {
    let output = Command::new("git")
        .arg("-C")
        .arg(workspace_path)
        .args(args)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    Some(output.stdout)
}

fn git_blob_at_commit(
    workspace_path: &Path,
    commit: &str,
    normalized_path: &str,
) -> Option<Vec<u8>> {
    if !is_hex_commit(commit) || !is_safe_workspace_relative_path(Path::new(normalized_path)) {
        return None;
    }
    let object = format!("{commit}:{normalized_path}");
    git_output_bytes(workspace_path, &["show", &object])
}

fn first_hex_commit(input: &str) -> Option<String> {
    let mut current = String::new();
    for ch in input.chars() {
        if ch.is_ascii_hexdigit() {
            current.push(ch.to_ascii_lowercase());
            if current.len() == 40 {
                return Some(current);
            }
        } else if let Some(commit) = finish_hex_commit(&current) {
            return Some(commit);
        } else {
            current.clear();
        }
    }
    finish_hex_commit(&current)
}

fn finish_hex_commit(candidate: &str) -> Option<String> {
    if is_hex_commit(candidate) {
        Some(candidate.to_owned())
    } else {
        None
    }
}

fn is_hex_commit(candidate: &str) -> bool {
    (7..=40).contains(&candidate.len()) && candidate.chars().all(|ch| ch.is_ascii_hexdigit())
}

fn memory_drift_support_summary_item(item: &MemoryDriftSelectionHint) -> serde_json::Value {
    serde_json::json!({
        "memoryId": memory_drift_support_label(&item.memory_id),
        "driftStatus": item.drift_status.as_str(),
        "severity": memory_drift_support_label(&item.severity),
        "topReason": memory_drift_support_label(&item.top_reason),
        "sourceKind": memory_drift_support_source_kind(item),
        "evidenceCount": item.evidence_count,
        "degradedCode": item.degraded_code.as_deref().map(memory_drift_support_label),
    })
}

fn memory_drift_support_status_counts(
    items: &[MemoryDriftSelectionHint],
) -> BTreeMap<String, usize> {
    let mut counts = BTreeMap::new();
    for item in items {
        *counts
            .entry(item.drift_status.as_str().to_owned())
            .or_default() += 1;
    }
    counts
}

fn memory_drift_support_source_kind_counts(
    items: &[MemoryDriftSelectionHint],
) -> BTreeMap<String, usize> {
    let mut counts = BTreeMap::new();
    for item in items {
        *counts
            .entry(memory_drift_support_source_kind(item).to_owned())
            .or_default() += 1;
    }
    counts
}

fn memory_drift_support_source_kind(item: &MemoryDriftSelectionHint) -> &'static str {
    let reason = item.top_reason.as_str();
    if reason.starts_with("provenance_chain_") || reason.starts_with("provenance_") {
        "provenance_chain"
    } else if reason.starts_with("code_anchor_") || item.stale_anchor {
        "code_anchor"
    } else if reason.starts_with("pack_item_") {
        "pack_record"
    } else if reason.contains("schema") {
        "schema"
    } else {
        "memory_record"
    }
}

fn memory_drift_support_degraded_codes(report: &MemoryDriftReport) -> Vec<String> {
    let mut codes = report
        .degraded
        .iter()
        .map(|degradation| memory_drift_support_label(&degradation.code))
        .chain(
            report
                .items
                .iter()
                .filter_map(|item| item.degraded_code.as_deref())
                .map(memory_drift_support_label),
        )
        .collect::<Vec<_>>();
    codes.sort();
    codes.dedup();
    codes
}

fn memory_drift_support_label(value: &str) -> String {
    let mut output: String = value
        .trim()
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, ':' | '_' | '-' | '.' | '/') {
                ch
            } else {
                '_'
            }
        })
        .take(120)
        .collect();
    if output.is_empty() {
        output.push_str("unknown");
    }
    output
}

fn memory_drift_support_message(value: &str) -> String {
    if value.contains('/') || value.contains('\\') || value.contains('$') || value.contains('`') {
        return "Memory drift summary is unavailable; see degraded code for recovery.".to_owned();
    }
    let mut output = value
        .chars()
        .map(|ch| {
            if ch.is_ascii_graphic() || ch == ' ' {
                ch
            } else {
                ' '
            }
        })
        .collect::<String>();
    output.truncate(180);
    if output.trim().is_empty() {
        "Memory drift summary is unavailable.".to_owned()
    } else {
        output
    }
}

#[must_use]
pub fn memory_drift_report_hint_from_provenance_status(
    memory_id: &str,
    provenance_verification_status: &str,
    provenance_chain_hash: Option<&str>,
) -> MemoryDriftSelectionHint {
    let (status, reason) = match provenance_verification_status.trim() {
        "verified" => (MemoryDriftStatus::Current, "provenance_chain_verified"),
        "mismatch" => (MemoryDriftStatus::Changed, "provenance_chain_mismatch"),
        "missing" => (MemoryDriftStatus::MissingSource, "provenance_chain_missing"),
        "skipped" => (
            MemoryDriftStatus::Unverifiable,
            "provenance_verification_skipped",
        ),
        "unverified" | "" => (
            MemoryDriftStatus::Unverifiable,
            "provenance_not_yet_verified",
        ),
        _ => (
            MemoryDriftStatus::Unverifiable,
            "provenance_verification_status_unknown",
        ),
    };
    let evidence_count = u32::from(
        provenance_chain_hash
            .map(str::trim)
            .is_some_and(|hash| !hash.is_empty()),
    );
    let mut hint = MemoryDriftSelectionHint::new(memory_id, status, reason, evidence_count);
    // `unverified` is the normal initial state for every freshly remembered or
    // revised memory. Keep it visible in drift reports, but do not conflate it
    // with structural pack/lineage corruption that must block claim authority.
    if matches!(provenance_verification_status.trim(), "unverified" | "") {
        hint.degraded_code = None;
    }
    hint
}

#[must_use]
pub fn memory_drift_selection_hint_from_provenance_status(
    memory_id: &str,
    provenance_verification_status: &str,
    provenance_chain_hash: Option<&str>,
) -> Option<MemoryDriftSelectionHint> {
    match provenance_verification_status.trim() {
        "mismatch" | "missing" | "skipped" => {
            Some(memory_drift_report_hint_from_provenance_status(
                memory_id,
                provenance_verification_status,
                provenance_chain_hash,
            ))
        }
        _ => None,
    }
}

#[must_use]
pub fn score_memory_drift(
    previous: &MemoryDriftSnapshot,
    current: &MemoryDriftSnapshot,
    mut factors: MemoryDriftMemoryFactors,
) -> MemoryDriftQueueItem {
    let memory_id = normalized_non_empty(Some(factors.memory_id.as_str()))
        .unwrap_or_else(|| previous.memory_id.clone());
    factors.memory_id.clone_from(&memory_id);
    if factors.suppressed {
        return MemoryDriftQueueItem {
            memory_id,
            drift_status: MemoryDriftStatus::Suppressed,
            priority_score: 0.0,
            priority_score_micros: 0,
            affected_anchor_count: 0,
            total_anchor_count: previous.anchors.len(),
            change_magnitude: 0.0,
            factors,
            reason_codes: vec!["suppressed_by_policy".to_owned()],
            comparisons: Vec::new(),
        };
    }

    let mut current_by_key = BTreeMap::<String, &MemoryDriftAnchor>::new();
    for anchor in &current.anchors {
        current_by_key.insert(anchor.identity_key(), anchor);
    }

    let mut comparisons = Vec::new();
    let mut aggregate_status = if previous.anchors.is_empty() {
        MemoryDriftStatus::Unverifiable
    } else {
        MemoryDriftStatus::Current
    };

    for previous_anchor in &previous.anchors {
        let comparison = compare_memory_drift_anchor(previous_anchor, &current_by_key);
        if comparison.drift_status.severity_rank() > aggregate_status.severity_rank() {
            aggregate_status = comparison.drift_status;
        }
        comparisons.push(comparison);
    }
    comparisons.sort_by(|left, right| {
        left.anchor_kind
            .cmp(&right.anchor_kind)
            .then_with(|| left.anchor_label.cmp(&right.anchor_label))
    });

    let affected_anchor_count = comparisons
        .iter()
        .filter(|comparison| comparison.drift_status != MemoryDriftStatus::Current)
        .count();
    let total_anchor_count = previous.anchors.len();
    let change_magnitude = if total_anchor_count == 0 {
        1.0
    } else {
        affected_anchor_count as f64 / total_anchor_count as f64
    };
    let mut reason_codes = comparisons
        .iter()
        .flat_map(|comparison| comparison.reason_codes.iter().cloned())
        .collect::<Vec<_>>();
    if comparisons.is_empty() {
        reason_codes.push("no_previous_evidence".to_owned());
    }
    reason_codes.sort();
    reason_codes.dedup();

    let priority_score_micros = drift_priority_score_micros(
        aggregate_status,
        affected_anchor_count,
        total_anchor_count,
        &factors,
    );
    MemoryDriftQueueItem {
        memory_id,
        drift_status: aggregate_status,
        priority_score: priority_score_micros as f64 / 1_000_000.0,
        priority_score_micros,
        affected_anchor_count,
        total_anchor_count,
        change_magnitude,
        factors,
        reason_codes,
        comparisons,
    }
}

#[must_use]
pub fn memory_drift_content_hash(bytes: &[u8]) -> String {
    format!("blake3:{}", blake3::hash(bytes).to_hex())
}

#[must_use]
pub fn memory_drift_path_hash(path: &Path) -> String {
    memory_drift_content_hash(normalized_path_string(path).as_bytes())
}

#[must_use]
pub fn redacted_path_label(path: &Path) -> String {
    let path_hash = blake3_prefix(normalized_path_string(path).as_bytes(), 16);
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.trim().is_empty())
        .unwrap_or("path");
    let file_name = if contains_sensitive_label(file_name) {
        "redacted".to_owned()
    } else {
        sanitized_label_segment(file_name)
    };
    format!("path:{file_name}:{path_hash}")
}

#[must_use]
pub fn hash_bounded_source_window(
    bytes: &[u8],
    max_window_bytes: usize,
) -> MemoryDriftWindowDigest {
    let end_byte = bytes.len().min(max_window_bytes);
    let window = &bytes[..end_byte];
    MemoryDriftWindowDigest {
        content_hash: memory_drift_content_hash(window),
        source_window: MemoryDriftSourceWindow::new(0, end_byte, bytes.len()),
    }
}

#[must_use]
pub fn classify_source_bytes(bytes: &[u8], max_window_bytes: usize) -> MemoryDriftAnchorStatus {
    if bytes.contains(&0) {
        MemoryDriftAnchorStatus::BinarySource
    } else if bytes.len() > max_window_bytes {
        MemoryDriftAnchorStatus::TooLarge
    } else {
        MemoryDriftAnchorStatus::Captured
    }
}

fn compare_memory_drift_anchor(
    previous: &MemoryDriftAnchor,
    current_by_key: &BTreeMap<String, &MemoryDriftAnchor>,
) -> MemoryDriftAnchorComparison {
    let current = current_by_key.get(&previous.identity_key()).copied();
    let (drift_status, current_content_hash, current_metadata_hash, mut reason_codes) =
        match current {
            None => (
                MemoryDriftStatus::StaleAnchor,
                None,
                None,
                vec!["anchor_not_in_current_snapshot".to_owned()],
            ),
            Some(current_anchor) => {
                let current_content_hash = current_anchor.content_hash.clone();
                let current_metadata_hash = Some(current_anchor.metadata_hash.clone());
                let (status, reason) = match current_anchor.status {
                    MemoryDriftAnchorStatus::MissingSource => {
                        (MemoryDriftStatus::MissingSource, "source_missing")
                    }
                    MemoryDriftAnchorStatus::Unavailable => {
                        (MemoryDriftStatus::Unverifiable, "source_unavailable")
                    }
                    MemoryDriftAnchorStatus::BinarySource => {
                        (MemoryDriftStatus::Unverifiable, "binary_source")
                    }
                    MemoryDriftAnchorStatus::Redacted => {
                        (MemoryDriftStatus::Unverifiable, "source_redacted")
                    }
                    MemoryDriftAnchorStatus::TooLarge => {
                        if previous.content_hash.as_deref()
                            != current_anchor.content_hash.as_deref()
                        {
                            (MemoryDriftStatus::Changed, "bounded_hash_changed")
                        } else if previous.metadata_hash.as_str()
                            != current_anchor.metadata_hash.as_str()
                        {
                            (MemoryDriftStatus::StaleAnchor, "metadata_changed")
                        } else {
                            (MemoryDriftStatus::Current, "anchor_current")
                        }
                    }
                    MemoryDriftAnchorStatus::Captured => {
                        if previous.content_hash.is_none() || current_anchor.content_hash.is_none()
                        {
                            (MemoryDriftStatus::Unverifiable, "content_hash_unavailable")
                        } else if previous.content_hash.as_deref()
                            != current_anchor.content_hash.as_deref()
                        {
                            (MemoryDriftStatus::Changed, "content_hash_changed")
                        } else if previous.metadata_hash.as_str()
                            != current_anchor.metadata_hash.as_str()
                        {
                            (MemoryDriftStatus::StaleAnchor, "metadata_changed")
                        } else {
                            (MemoryDriftStatus::Current, "anchor_current")
                        }
                    }
                };
                (
                    status,
                    current_content_hash,
                    current_metadata_hash,
                    vec![reason.to_owned()],
                )
            }
        };
    if drift_status != MemoryDriftStatus::Current {
        reason_codes.push(format!("kind:{}", previous.kind.as_str()));
    }
    reason_codes.sort();
    reason_codes.dedup();

    MemoryDriftAnchorComparison {
        anchor_label: previous.label.clone(),
        anchor_kind: previous.kind,
        drift_status,
        previous_content_hash: previous.content_hash.clone(),
        current_content_hash,
        previous_metadata_hash: previous.metadata_hash.clone(),
        current_metadata_hash,
        reason_codes,
    }
}

fn drift_priority_score_micros(
    drift_status: MemoryDriftStatus,
    affected_anchor_count: usize,
    total_anchor_count: usize,
    factors: &MemoryDriftMemoryFactors,
) -> u64 {
    let mut score = drift_status.base_score_micros();
    let magnitude_micros = if total_anchor_count == 0 {
        150_000
    } else {
        ((affected_anchor_count as u64).saturating_mul(150_000) / total_anchor_count as u64)
            .min(150_000)
    };
    score = score.saturating_add(magnitude_micros);
    score = score
        .saturating_add(unit_score_micros(factors.importance).saturating_mul(100_000) / 1_000_000);
    let trust_micros =
        unit_score_micros(factors.trust_score).max(trust_class_micros(&factors.trust_class));
    score = score.saturating_add(trust_micros.saturating_mul(70_000) / 1_000_000);
    score = score.saturating_add(u64::from(factors.recent_pack_inclusions.min(5)) * 20_000);
    score = score.saturating_add(u64::from(factors.downstream_graph_refs.min(20)) * 5_000);
    score = score.saturating_add(u64::from(factors.days_since_validation.min(365)) * 80_000 / 365);
    score.min(1_000_000)
}

fn unit_score_micros(value: f64) -> u64 {
    if !value.is_finite() || value <= 0.0 {
        0
    } else if value >= 1.0 {
        1_000_000
    } else {
        (value * 1_000_000.0).round() as u64
    }
}

fn trust_class_micros(trust_class: &str) -> u64 {
    match trust_class.trim() {
        "human_explicit" => 850_000,
        "peer_human_attested" => 750_000,
        "agent_validated" => 650_000,
        "agent_assertion" => 500_000,
        "cass_evidence" => 450_000,
        "legacy_import" => 300_000,
        _ => 250_000,
    }
}

fn memory_drift_metadata_hash(
    kind: MemoryDriftAnchorKind,
    label: &str,
    status: MemoryDriftAnchorStatus,
    source_window: &Option<MemoryDriftSourceWindow>,
    freshness: &MemoryDriftFreshness,
    metadata: &BTreeMap<String, String>,
) -> String {
    #[derive(Serialize)]
    #[serde(rename_all = "camelCase")]
    struct MetadataHashInput<'a> {
        kind: MemoryDriftAnchorKind,
        label: &'a str,
        status: MemoryDriftAnchorStatus,
        source_window: &'a Option<MemoryDriftSourceWindow>,
        freshness: &'a MemoryDriftFreshness,
        metadata: &'a BTreeMap<String, String>,
    }

    let input = MetadataHashInput {
        kind,
        label,
        status,
        source_window,
        freshness,
        metadata,
    };
    let bytes = serde_json::to_vec(&input).unwrap_or_else(|_| label.as_bytes().to_vec());
    memory_drift_content_hash(&bytes)
}

fn redacted_anchor_label(kind: MemoryDriftAnchorKind, value: &str) -> String {
    match kind {
        MemoryDriftAnchorKind::FilePath => redacted_path_label(Path::new(value)),
        MemoryDriftAnchorKind::CommandHash => {
            format!("command:{}", blake3_prefix(value.as_bytes(), 16))
        }
        MemoryDriftAnchorKind::LineSpanHash => {
            format!("line_span:{}", blake3_prefix(value.as_bytes(), 16))
        }
        MemoryDriftAnchorKind::SessionFragmentHash => {
            format!("session_fragment:{}", blake3_prefix(value.as_bytes(), 16))
        }
        MemoryDriftAnchorKind::SchemaId
        | MemoryDriftAnchorKind::BeadId
        | MemoryDriftAnchorKind::CommitHash
        | MemoryDriftAnchorKind::Symbol => sanitized_label_segment(value),
    }
}

fn select_line_span(source: &str, start_line: usize, end_line: usize) -> String {
    let normalized_start = start_line.max(1);
    let normalized_end = end_line.max(normalized_start);
    source
        .lines()
        .enumerate()
        .filter_map(|(index, line)| {
            let line_number = index + 1;
            if (normalized_start..=normalized_end).contains(&line_number) {
                Some(line)
            } else {
                None
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn normalized_non_empty(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn normalized_path_string(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

fn sanitized_label_segment(value: &str) -> String {
    let mut output: String = value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-') {
                ch
            } else {
                '_'
            }
        })
        .take(80)
        .collect();
    if output.is_empty() {
        output.push_str("unknown");
    }
    output
}

fn contains_sensitive_label(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    [
        "password",
        "secret",
        "api_key",
        "apikey",
        "token",
        "credential",
        "private_key",
    ]
    .iter()
    .any(|pattern| lower.contains(pattern))
}

fn blake3_prefix(bytes: &[u8], chars: usize) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let digest = blake3::hash(bytes);
    let mut output = String::with_capacity(chars);
    for byte in digest.as_bytes() {
        if output.len() >= chars {
            break;
        }
        output.push(HEX[(byte >> 4) as usize] as char);
        if output.len() >= chars {
            break;
        }
        output.push(HEX[(byte & 0x0F) as usize] as char);
    }
    output
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;
    use crate::db::{
        CreateMemoryInput, CreatePackItemInput, CreatePackRecordInput, CreateWorkspaceInput,
    };

    #[test]
    fn peer_human_attested_drift_weight_matches_initial_confidence_order() {
        assert_eq!(trust_class_micros("peer_human_attested"), 750_000);
        assert!(trust_class_micros("human_explicit") > trust_class_micros("peer_human_attested"));
        assert!(trust_class_micros("peer_human_attested") > trust_class_micros("agent_validated"));
    }

    fn sample_snapshot(anchors: Vec<MemoryDriftAnchor>) -> MemoryDriftSnapshot {
        MemoryDriftSnapshot::new(
            "mem_123",
            Some("workspace_abc"),
            Some("2026-05-19T10:00:00Z"),
            16,
            anchors,
        )
    }

    fn recent_pack_test_connection(
        workspace_path: &Path,
    ) -> Result<(DbConnection, String), String> {
        let connection = DbConnection::open_memory().map_err(|error| error.to_string())?;
        connection.migrate().map_err(|error| error.to_string())?;
        let workspace_id = stable_workspace_id(workspace_path);
        connection
            .insert_workspace(
                &workspace_id,
                &CreateWorkspaceInput {
                    path: workspace_path.display().to_string(),
                    name: Some("recent pack drift test".to_owned()),
                },
            )
            .map_err(|error| error.to_string())?;
        Ok((connection, workspace_id))
    }

    fn recent_pack_test_memory_input(workspace_id: &str, content: &str) -> CreateMemoryInput {
        CreateMemoryInput {
            workspace_id: workspace_id.to_owned(),
            level: "procedural".to_owned(),
            kind: "rule".to_owned(),
            content: content.to_owned(),
            workflow_id: None,
            confidence: 0.9,
            utility: 0.8,
            importance: 0.7,
            provenance_uri: None,
            trust_class: "agent_assertion".to_owned(),
            trust_subclass: None,
            tags: Vec::new(),
            valid_from: None,
            valid_to: None,
        }
    }

    fn insert_recent_pack_test_memory(
        connection: &DbConnection,
        workspace_id: &str,
        memory_id: &str,
        verified: bool,
    ) -> Result<(), String> {
        connection
            .insert_memory(
                memory_id,
                &recent_pack_test_memory_input(workspace_id, "Bounded recent-pack drift rule."),
            )
            .map_err(|error| error.to_string())?;
        if verified {
            connection
                .execute_raw(&format!(
                    "UPDATE memories SET provenance_verification_status = 'verified', provenance_chain_hash = 'blake3:verified' WHERE id = '{memory_id}'"
                ))
                .map_err(|error| error.to_string())?;
        }
        Ok(())
    }

    fn insert_recent_pack_test_record(
        connection: &DbConnection,
        workspace_id: &str,
        pack_seed: &str,
        memory_id: &str,
        selected_at: &str,
    ) -> Result<(), String> {
        let pack_seed_hash = blake3::hash(pack_seed.as_bytes()).to_hex().to_string();
        let pack_id = format!("pack_{}", &pack_seed_hash[..26]);
        let persisted_at = if DateTime::parse_from_rfc3339(selected_at).is_ok() {
            selected_at
        } else {
            "2030-01-07T00:00:00Z"
        };
        connection
            .insert_pack_record_at(
                &pack_id,
                &CreatePackRecordInput {
                    workspace_id: workspace_id.to_owned(),
                    query: "recent pack drift test".to_owned(),
                    profile: "balanced".to_owned(),
                    max_tokens: 512,
                    used_tokens: 32,
                    item_count: 1,
                    omitted_count: 0,
                    pack_hash: format!("blake3:{pack_seed_hash}"),
                    degraded_json: None,
                    created_by: Some("memory-drift-test".to_owned()),
                },
                &[CreatePackItemInput {
                    pack_id: pack_id.clone(),
                    memory_id: memory_id.to_owned(),
                    rank: 1,
                    section: "procedural_rules".to_owned(),
                    estimated_tokens: 32,
                    relevance: 0.8,
                    utility: 0.7,
                    combined_score: None,
                    attempt_family_multiplicity: None,
                    why: "test selection".to_owned(),
                    diversity_key: None,
                    provenance_json: "{}".to_owned(),
                    trust_class: "agent_assertion".to_owned(),
                    trust_subclass: None,
                }],
                &[],
                persisted_at,
            )
            .map_err(|error| error.to_string())?;
        if persisted_at != selected_at {
            connection
                .execute_raw(&format!(
                    "UPDATE pack_records SET created_at = '{selected_at}' WHERE id = '{pack_id}'"
                ))
                .map_err(|error| error.to_string())?;
        }
        Ok(())
    }

    fn recent_pack_test_report(
        connection: &DbConnection,
        workspace_path: &Path,
        workspace_id: &str,
        as_of: &str,
    ) -> Result<Vec<MemoryDriftSelectionHint>, String> {
        let as_of = DateTime::parse_from_rfc3339(as_of)
            .map_err(|error| error.to_string())?
            .with_timezone(&Utc);
        memory_drift_report_recent_pack_items(
            connection,
            workspace_path,
            workspace_id,
            50,
            Some(as_of),
        )
        .map(|(items, _)| items)
        .map_err(|error| error.to_string())
    }

    #[test]
    fn recent_pack_horizon_includes_exact_boundary_and_excludes_strictly_older_rows()
    -> Result<(), String> {
        let workspace_path = Path::new("/tmp/ee-memory-drift-horizon");
        let (connection, workspace_id) = recent_pack_test_connection(workspace_path)?;
        let boundary_id = crate::testing::mem("driftboundary");
        let older_id = crate::testing::mem("driftolder");
        insert_recent_pack_test_memory(&connection, &workspace_id, &boundary_id, false)?;
        insert_recent_pack_test_memory(&connection, &workspace_id, &older_id, false)?;
        insert_recent_pack_test_record(
            &connection,
            &workspace_id,
            "pack_drift_boundary",
            &boundary_id,
            "2030-01-01T00:00:00Z",
        )?;
        insert_recent_pack_test_record(
            &connection,
            &workspace_id,
            "pack_drift_older",
            &older_id,
            "2029-12-31T23:59:59Z",
        )?;

        let report = recent_pack_test_report(
            &connection,
            workspace_path,
            &workspace_id,
            "2030-01-08T00:00:00Z",
        )?;
        assert_eq!(report.len(), 1);
        assert_eq!(report[0].memory_id, boundary_id);
        assert_eq!(report[0].drift_status, MemoryDriftStatus::Unverifiable);
        assert!(report.iter().all(|item| item.memory_id != older_id));
        Ok(())
    }

    #[test]
    fn recent_pack_old_only_affected_selection_yields_empty_claim_window() -> Result<(), String> {
        let workspace_path = Path::new("/tmp/ee-memory-drift-old-only");
        let (connection, workspace_id) = recent_pack_test_connection(workspace_path)?;
        let old_id = crate::testing::mem("driftoldonly");
        insert_recent_pack_test_memory(&connection, &workspace_id, &old_id, false)?;
        insert_recent_pack_test_record(
            &connection,
            &workspace_id,
            "pack_drift_old_only",
            &old_id,
            "2029-12-31T23:59:59Z",
        )?;

        let report = recent_pack_test_report(
            &connection,
            workspace_path,
            &workspace_id,
            "2030-01-08T00:00:00Z",
        )?;
        assert!(
            report.is_empty(),
            "strictly pre-horizon drift must not retain claim authority: {report:?}"
        );
        Ok(())
    }

    #[test]
    fn recent_pack_dangling_memory_row_fails_closed_as_unverifiable() -> Result<(), String> {
        let workspace_path = Path::new("/tmp/ee-memory-drift-dangling-pack-item");
        let (connection, workspace_id) = recent_pack_test_connection(workspace_path)?;
        let memory_id = crate::testing::mem("driftdanglingselection");
        let pack_seed = "pack_drift_dangling_selection";
        let pack_seed_hash = blake3::hash(pack_seed.as_bytes()).to_hex().to_string();
        let pack_id = format!("pack_{}", &pack_seed_hash[..26]);
        insert_recent_pack_test_memory(&connection, &workspace_id, &memory_id, true)?;
        insert_recent_pack_test_record(
            &connection,
            &workspace_id,
            pack_seed,
            &memory_id,
            "2030-01-07T00:00:00Z",
        )?;

        // A normal delete cascades to pack_items. Disable FK enforcement only
        // long enough to simulate an imported/corrupt database whose persisted
        // pack selection points at a memory row that cannot be resolved.
        connection
            .execute_raw("PRAGMA foreign_keys = OFF")
            .map_err(|error| error.to_string())?;
        connection
            .execute_raw(&format!("DELETE FROM memories WHERE id = '{memory_id}'"))
            .map_err(|error| error.to_string())?;
        connection
            .execute_raw("PRAGMA foreign_keys = ON")
            .map_err(|error| error.to_string())?;

        let persisted_items = connection
            .list_pack_items_for_memory_drift(&workspace_id, 50)
            .map_err(|error| error.to_string())?;
        assert_eq!(
            persisted_items.len(),
            1,
            "dangling selection fixture must persist"
        );

        // The integrity-bound ledger, not the denormalized pack_items table,
        // is the authoritative selection source. Simulate a normal cascade or
        // partial import that removes the projection row while retaining the
        // pack record and prove the missing memory still cannot false-clear.
        connection
            .execute_raw(&format!(
                "DELETE FROM pack_items WHERE pack_id = '{pack_id}'"
            ))
            .map_err(|error| error.to_string())?;
        let projected_items = connection
            .list_pack_items_for_memory_drift(&workspace_id, 50)
            .map_err(|error| error.to_string())?;
        assert!(
            projected_items.is_empty(),
            "test must remove the denormalized pack-item projection"
        );

        let report = recent_pack_test_report(
            &connection,
            workspace_path,
            &workspace_id,
            "2030-01-08T00:00:00Z",
        )?;
        assert_eq!(report.len(), 1);
        assert_eq!(report[0].memory_id, memory_id);
        assert_eq!(report[0].top_reason, "pack_item_memory_row_missing");
        assert_eq!(report[0].drift_status, MemoryDriftStatus::Unverifiable);
        assert_eq!(
            report[0].degraded_code.as_deref(),
            Some("memory_drift_source_unverifiable")
        );
        Ok(())
    }

    #[test]
    fn recent_pack_malformed_and_future_timestamps_fail_closed() -> Result<(), String> {
        let workspace_path = Path::new("/tmp/ee-memory-drift-timestamps");
        let (connection, workspace_id) = recent_pack_test_connection(workspace_path)?;
        let malformed_id = crate::testing::mem("driftmalformed");
        let future_id = crate::testing::mem("driftfuture");
        insert_recent_pack_test_memory(&connection, &workspace_id, &malformed_id, true)?;
        insert_recent_pack_test_memory(&connection, &workspace_id, &future_id, true)?;
        insert_recent_pack_test_record(
            &connection,
            &workspace_id,
            "pack_drift_malformed",
            &malformed_id,
            "not-a-timestamp",
        )?;
        insert_recent_pack_test_record(
            &connection,
            &workspace_id,
            "pack_drift_future",
            &future_id,
            "2030-01-08T00:00:01Z",
        )?;

        let report = recent_pack_test_report(
            &connection,
            workspace_path,
            &workspace_id,
            "2030-01-08T00:00:00Z",
        )?;
        let reasons = report
            .iter()
            .map(|item| item.top_reason.as_str())
            .collect::<BTreeSet<_>>();
        assert!(reasons.contains("pack_item_ledger_timestamp_mismatch"));
        assert!(reasons.contains("pack_item_selected_at_future"));
        assert!(report.iter().all(|item| {
            item.drift_status == MemoryDriftStatus::Unverifiable
                && item.degraded_code.as_deref() == Some("memory_drift_source_unverifiable")
        }));
        Ok(())
    }

    #[test]
    fn recent_pack_insertion_admission_cannot_hide_low_sorting_malformed_timestamp()
    -> Result<(), String> {
        let workspace_path = Path::new("/tmp/ee-memory-drift-malformed-admission");
        let (connection, workspace_id) = recent_pack_test_connection(workspace_path)?;
        for index in 0..50 {
            let memory_id = crate::testing::mem(&format!("driftvalid{index:02}"));
            let pack_id = format!("pack_drift_valid_{index:02}");
            insert_recent_pack_test_memory(&connection, &workspace_id, &memory_id, true)?;
            insert_recent_pack_test_record(
                &connection,
                &workspace_id,
                &pack_id,
                &memory_id,
                "2030-01-07T00:00:00Z",
            )?;
        }
        let malformed_id = crate::testing::mem("driftlowsortmalformed");
        insert_recent_pack_test_memory(&connection, &workspace_id, &malformed_id, true)?;
        insert_recent_pack_test_record(
            &connection,
            &workspace_id,
            "pack_drift_low_sort_malformed",
            &malformed_id,
            "!",
        )?;

        let report = recent_pack_test_report(
            &connection,
            workspace_path,
            &workspace_id,
            "2030-01-08T00:00:00Z",
        )?;
        let malformed = report
            .iter()
            .find(|item| item.memory_id == malformed_id)
            .ok_or_else(|| {
                "newest inserted malformed timestamp was hidden by bounded admission".to_owned()
            })?;
        assert_eq!(malformed.top_reason, "pack_item_ledger_timestamp_mismatch");
        assert_eq!(
            malformed.degraded_code.as_deref(),
            Some("memory_drift_source_unverifiable")
        );
        Ok(())
    }

    #[test]
    fn recent_pack_output_limit_does_not_hide_later_ranked_drift() -> Result<(), String> {
        let workspace_path = Path::new("/tmp/ee-memory-drift-pack-output-limit");
        let (connection, workspace_id) = recent_pack_test_connection(workspace_path)?;
        let pack_id = "pack_01j00000000000000000000000";
        let mut items = Vec::new();
        let mut drifted_id = String::new();
        for rank in 1..=17_u32 {
            let memory_id = crate::testing::mem(&format!("driftpackrank{rank:02}"));
            insert_recent_pack_test_memory(&connection, &workspace_id, &memory_id, true)?;
            if rank == 17 {
                connection
                    .execute_raw(&format!(
                        "UPDATE memories SET provenance_verification_status = 'mismatch' WHERE id = '{memory_id}'"
                    ))
                    .map_err(|error| error.to_string())?;
                drifted_id = memory_id.clone();
            }
            items.push(CreatePackItemInput {
                pack_id: pack_id.to_owned(),
                memory_id,
                rank,
                section: "procedural_rules".to_owned(),
                estimated_tokens: 16,
                relevance: 0.8,
                utility: 0.7,
                combined_score: None,
                attempt_family_multiplicity: None,
                why: "output limit regression".to_owned(),
                diversity_key: None,
                provenance_json: "{}".to_owned(),
                trust_class: "agent_assertion".to_owned(),
                trust_subclass: None,
            });
        }
        connection
            .insert_pack_record_at(
                pack_id,
                &CreatePackRecordInput {
                    workspace_id: workspace_id.clone(),
                    query: "output limit regression".to_owned(),
                    profile: "balanced".to_owned(),
                    max_tokens: 1024,
                    used_tokens: 272,
                    item_count: 17,
                    omitted_count: 0,
                    pack_hash: format!(
                        "blake3:{}",
                        blake3::hash(b"output-limit-regression").to_hex()
                    ),
                    degraded_json: None,
                    created_by: Some("memory-drift-test".to_owned()),
                },
                &items,
                &[],
                "2030-01-07T00:00:00Z",
            )
            .map_err(|error| error.to_string())?;

        let (report, generated_at) = memory_drift_report_recent_pack_items(
            &connection,
            workspace_path,
            &workspace_id,
            16,
            Some(
                DateTime::parse_from_rfc3339("2030-01-08T00:00:00Z")
                    .map_err(|error| error.to_string())?
                    .with_timezone(&Utc),
            ),
        )
        .map_err(|error| error.to_string())?;
        assert_eq!(generated_at.as_deref(), Some("2030-01-08T00:00:00+00:00"));
        assert!(report.iter().any(|item| {
            item.memory_id == drifted_id
                && item.degraded_code.as_deref() == Some("memory_drift_source_changed")
        }));

        let (truncated, _) = memory_drift_report_recent_pack_items_with_scan_cap(
            &connection,
            workspace_path,
            &workspace_id,
            16,
            Some(
                DateTime::parse_from_rfc3339("2030-01-08T00:00:00Z")
                    .map_err(|error| error.to_string())?
                    .with_timezone(&Utc),
            ),
            16,
        )
        .map_err(|error| error.to_string())?;
        assert!(truncated.iter().any(|item| {
            item.top_reason == "pack_item_authority_scan_truncated"
                && item.degraded_code.as_deref() == Some("memory_drift_source_unverifiable")
        }));
        Ok(())
    }

    #[test]
    fn recent_pack_minimum_clock_returns_error_instead_of_panicking() -> Result<(), String> {
        let workspace_path = Path::new("/tmp/ee-memory-drift-minimum-clock");
        let (connection, workspace_id) = recent_pack_test_connection(workspace_path)?;
        let error = memory_drift_report_recent_pack_items(
            &connection,
            workspace_path,
            &workspace_id,
            16,
            Some(DateTime::<Utc>::MIN_UTC),
        )
        .expect_err("minimum clock cannot represent the seven-day horizon");
        assert!(error.message().contains("seven-day horizon"));
        Ok(())
    }

    #[test]
    fn recent_pack_newest_revision_selection_controls_chain_authority() -> Result<(), String> {
        let workspace_path = Path::new("/tmp/ee-memory-drift-revisions");
        let (connection, workspace_id) = recent_pack_test_connection(workspace_path)?;

        let blocked_old = crate::testing::mem("driftblockedold");
        let blocked_live = crate::testing::mem("driftblockedlive");
        insert_recent_pack_test_memory(&connection, &workspace_id, &blocked_old, true)?;
        connection
            .expire_memory_valid_to(&blocked_old, "2030-01-07T00:00:00Z")
            .map_err(|error| error.to_string())?;
        connection
            .insert_memory_revision(
                &blocked_live,
                &blocked_old,
                &recent_pack_test_memory_input(&workspace_id, "Current blocked-chain revision."),
            )
            .map_err(|error| error.to_string())?;
        connection
            .execute_raw(&format!(
                "UPDATE memories SET provenance_verification_status = 'verified', provenance_chain_hash = 'blake3:verified' WHERE id = '{blocked_live}'"
            ))
            .map_err(|error| error.to_string())?;
        insert_recent_pack_test_record(
            &connection,
            &workspace_id,
            "pack_drift_blocked_old",
            &blocked_old,
            "2030-01-07T12:00:00Z",
        )?;

        let cleared_old = crate::testing::mem("driftclearedold");
        let cleared_live = crate::testing::mem("driftclearedlive");
        insert_recent_pack_test_memory(&connection, &workspace_id, &cleared_old, true)?;
        connection
            .expire_memory_valid_to(&cleared_old, "2030-01-06T00:00:00Z")
            .map_err(|error| error.to_string())?;
        connection
            .insert_memory_revision(
                &cleared_live,
                &cleared_old,
                &recent_pack_test_memory_input(&workspace_id, "Current cleared-chain revision."),
            )
            .map_err(|error| error.to_string())?;
        connection
            .execute_raw(&format!(
                "UPDATE memories SET provenance_verification_status = 'verified', provenance_chain_hash = 'blake3:verified' WHERE id = '{cleared_live}'"
            ))
            .map_err(|error| error.to_string())?;
        insert_recent_pack_test_record(
            &connection,
            &workspace_id,
            "pack_drift_cleared_old",
            &cleared_old,
            "2030-01-06T12:00:00Z",
        )?;
        insert_recent_pack_test_record(
            &connection,
            &workspace_id,
            "pack_drift_cleared_live",
            &cleared_live,
            "2030-01-07T12:00:01Z",
        )?;

        let report = recent_pack_test_report(
            &connection,
            workspace_path,
            &workspace_id,
            "2030-01-08T00:00:00Z",
        )?;
        let blocked = report
            .iter()
            .find(|item| item.memory_id == blocked_old)
            .ok_or_else(|| "superseded newest selection must remain in report".to_owned())?;
        assert_eq!(blocked.top_reason, "pack_item_superseded_since_selection");
        assert_eq!(blocked.drift_status, MemoryDriftStatus::Unverifiable);
        assert!(report.iter().any(|item| {
            item.memory_id == cleared_live && item.drift_status == MemoryDriftStatus::Current
        }));
        assert!(report.iter().all(|item| item.memory_id != cleared_old));

        let recovery = MemoryDriftReport::new(MemoryDriftReportMode::RecentPackItems, None, report)
            .recovery_actions;
        assert!(recovery.iter().any(|action| {
            action.kind == "rerun_source_validation"
                && action.command.as_deref() == Some("ee pack \"<TASK>\" --workspace . --json")
                && action
                    .description
                    .contains("cannot rewrite an old pack selection")
        }));
        assert!(recovery.iter().any(|action| {
            action.kind == "revise_memory"
                && action.command.as_deref()
                    == Some("ee memory revise <CURRENT_MEMORY_ID> --provenance-uri <URI> --reason correction --json")
                && action.description.contains("run a fresh pack")
        }));
        Ok(())
    }

    #[test]
    fn recent_pack_requires_exactly_one_live_revision_head() -> Result<(), String> {
        let workspace_path = Path::new("/tmp/ee-memory-drift-live-heads");
        let (connection, workspace_id) = recent_pack_test_connection(workspace_path)?;

        let missing_head = crate::testing::mem("driftmissinghead");
        insert_recent_pack_test_memory(&connection, &workspace_id, &missing_head, true)?;
        insert_recent_pack_test_record(
            &connection,
            &workspace_id,
            "pack_drift_missing_head",
            &missing_head,
            "2030-01-07T00:00:00Z",
        )?;
        connection
            .expire_memory_valid_to(&missing_head, "2030-01-07T00:00:01Z")
            .map_err(|error| error.to_string())?;

        let ambiguous_first = crate::testing::mem("driftambiguousfirst");
        let ambiguous_second = crate::testing::mem("driftambiguousecond");
        insert_recent_pack_test_memory(&connection, &workspace_id, &ambiguous_first, true)?;
        connection
            .insert_memory_revision(
                &ambiguous_second,
                &ambiguous_first,
                &recent_pack_test_memory_input(&workspace_id, "Second live head."),
            )
            .map_err(|error| error.to_string())?;
        insert_recent_pack_test_record(
            &connection,
            &workspace_id,
            "pack_drift_ambiguous_head",
            &ambiguous_first,
            "2030-01-07T00:00:02Z",
        )?;

        let report = recent_pack_test_report(
            &connection,
            workspace_path,
            &workspace_id,
            "2030-01-08T00:00:00Z",
        )?;
        let reasons = report
            .iter()
            .map(|item| item.top_reason.as_str())
            .collect::<BTreeSet<_>>();
        assert!(reasons.contains("pack_item_live_revision_missing"));
        assert!(reasons.contains("pack_item_live_revision_ambiguous"));

        assert!(
            connection
                .list_live_memory_revisions_for_logical_id(&workspace_id, &missing_head)
                .map_err(|error| error.to_string())?
                .is_empty()
        );
        assert_eq!(
            connection
                .list_live_memory_revisions_for_logical_id(&workspace_id, &ambiguous_first)
                .map_err(|error| error.to_string())?
                .len(),
            2
        );
        assert!(
            connection
                .list_live_memory_revisions_for_logical_id("wsp_other", &ambiguous_first)
                .map_err(|error| error.to_string())?
                .is_empty()
        );

        let tombstoned = crate::testing::mem("drifttombstonedhead");
        insert_recent_pack_test_memory(&connection, &workspace_id, &tombstoned, true)?;
        assert!(
            connection
                .tombstone_memory(&tombstoned)
                .map_err(|error| error.to_string())?
        );
        assert!(
            connection
                .list_live_memory_revisions_for_logical_id(&workspace_id, &tombstoned)
                .map_err(|error| error.to_string())?
                .is_empty()
        );
        Ok(())
    }

    #[test]
    fn stable_hashing_is_deterministic() {
        let left = memory_drift_content_hash(b"same source window");
        let right = memory_drift_content_hash(b"same source window");
        assert_eq!(left, right);
        assert!(left.starts_with("blake3:"));
    }

    #[test]
    fn path_redaction_removes_parent_directories() {
        let label = redacted_path_label(Path::new(
            "/Users/jemanuel/projects/eidetic_engine_cli/src/core/memory_drift.rs",
        ));
        assert!(label.starts_with("path:memory_drift.rs:"));
        assert!(!label.contains("/Users"));
        assert!(!label.contains("projects/eidetic_engine_cli"));
    }

    #[test]
    fn anchors_redact_sensitive_paths_and_command_text() {
        let path_anchor = MemoryDriftAnchor::source_bytes(
            Path::new("/Users/example/project/private/api_key_secret.rs"),
            b"token = \"do-not-render\"\n",
            64,
            None,
            None,
        );
        let path_json = serde_json::to_string(&path_anchor).expect("path anchor serializes");
        assert!(path_anchor.label.starts_with("path:redacted:"));
        assert!(!path_json.contains("/Users/example"));
        assert!(!path_json.contains("api_key_secret.rs"));
        assert!(!path_json.contains("do-not-render"));

        let command_anchor = MemoryDriftAnchor::identifier(
            MemoryDriftAnchorKind::CommandHash,
            "cargo test --lib memory_drift -- password=secret",
            Some("2026-05-19T10:00:01Z"),
        );
        let command_json =
            serde_json::to_string(&command_anchor).expect("command anchor serializes");
        assert!(command_anchor.label.starts_with("command:"));
        assert!(!command_json.contains("cargo test"));
        assert!(!command_json.contains("password=secret"));
    }

    #[test]
    fn source_windows_are_bounded_for_large_files() {
        let bytes = b"abcdefghijklmnopqrstuvwxyz";
        let anchor = MemoryDriftAnchor::source_bytes(
            Path::new("/Users/example/project/src/lib.rs"),
            bytes,
            8,
            None,
            None,
        );
        assert_eq!(anchor.status, MemoryDriftAnchorStatus::TooLarge);
        assert_eq!(
            anchor.source_window,
            Some(MemoryDriftSourceWindow {
                start_byte: 0,
                end_byte: 8,
                byte_len: 26,
                truncated: true,
            })
        );
        assert_eq!(
            anchor.content_hash,
            Some(memory_drift_content_hash(b"abcdefgh"))
        );
    }

    #[test]
    fn binary_sources_are_classified_without_raw_content() {
        let anchor = MemoryDriftAnchor::source_bytes(
            Path::new("/Users/example/project/blob.bin"),
            b"abc\0def",
            32,
            None,
            None,
        );
        assert_eq!(anchor.status, MemoryDriftAnchorStatus::BinarySource);
        assert_eq!(
            anchor.freshness.unavailable_reason,
            Some("binary_source".to_owned())
        );
    }

    #[test]
    fn missing_sources_record_unavailable_reason() {
        let anchor = MemoryDriftAnchor::missing_file(
            Path::new("/Users/example/project/missing.rs"),
            "not_found",
            None,
        );
        assert_eq!(anchor.status, MemoryDriftAnchorStatus::MissingSource);
        assert_eq!(
            anchor.freshness.unavailable_reason,
            Some("not_found".to_owned())
        );
        assert!(anchor.content_hash.is_none());
    }

    #[test]
    fn reordered_evidence_serializes_identically_after_volatile_scrub() {
        let file_anchor = MemoryDriftAnchor::source_bytes(
            Path::new("/Users/example/project/src/lib.rs"),
            b"pub fn alpha() {}\n",
            64,
            Some("2026-05-19T10:00:00Z"),
            Some("2026-05-19T09:00:00Z"),
        );
        let command_anchor = MemoryDriftAnchor::identifier(
            MemoryDriftAnchorKind::CommandHash,
            "cargo test --lib memory_drift -- --nocapture",
            Some("2026-05-19T10:00:01Z"),
        );

        let left = sample_snapshot(vec![file_anchor.clone(), command_anchor.clone()])
            .volatile_scrubbed()
            .stable_json()
            .expect("snapshot serializes");
        let right = sample_snapshot(vec![command_anchor, file_anchor])
            .volatile_scrubbed()
            .stable_json()
            .expect("snapshot serializes");
        assert_eq!(left, right);
    }

    #[test]
    fn line_span_hash_uses_selected_lines_only() {
        let source = "one\ntwo\nthree\nfour\n";
        let anchor = MemoryDriftAnchor::line_span(
            Path::new("/Users/example/project/src/lib.rs"),
            source,
            2,
            3,
            64,
            None,
        );
        assert_eq!(anchor.kind, MemoryDriftAnchorKind::LineSpanHash);
        assert_eq!(
            anchor.content_hash,
            Some(memory_drift_content_hash(b"two\nthree"))
        );
        assert_eq!(
            anchor.metadata.get("lineStart").map(String::as_str),
            Some("2")
        );
        assert_eq!(
            anchor.metadata.get("lineEnd").map(String::as_str),
            Some("3")
        );
    }

    #[test]
    fn changed_snapshot_scores_above_current_snapshot() {
        let previous = sample_snapshot(vec![MemoryDriftAnchor::source_bytes(
            Path::new("/Users/example/project/src/lib.rs"),
            b"pub fn alpha() {}\n",
            64,
            None,
            None,
        )]);
        let current = sample_snapshot(vec![MemoryDriftAnchor::source_bytes(
            Path::new("/Users/example/project/src/lib.rs"),
            b"pub fn beta() {}\n",
            64,
            None,
            None,
        )]);
        let unchanged = score_memory_drift(
            &previous,
            &previous,
            MemoryDriftMemoryFactors {
                memory_id: "mem_unchanged".to_owned(),
                importance: 0.5,
                trust_score: 0.5,
                ..MemoryDriftMemoryFactors::default()
            },
        );
        let changed = score_memory_drift(
            &previous,
            &current,
            MemoryDriftMemoryFactors {
                memory_id: "mem_changed".to_owned(),
                importance: 0.5,
                trust_score: 0.5,
                ..MemoryDriftMemoryFactors::default()
            },
        );
        assert_eq!(changed.drift_status, MemoryDriftStatus::Changed);
        assert!(changed.priority_score_micros > unchanged.priority_score_micros);
        assert!(
            changed
                .reason_codes
                .contains(&"content_hash_changed".to_owned())
        );
    }

    #[test]
    fn missing_source_and_unverifiable_evidence_are_classified() {
        let previous_anchor = MemoryDriftAnchor::source_bytes(
            Path::new("/Users/example/project/src/lib.rs"),
            b"pub fn alpha() {}\n",
            64,
            None,
            None,
        );
        let previous = sample_snapshot(vec![previous_anchor.clone()]);
        let missing = sample_snapshot(vec![MemoryDriftAnchor::missing_file(
            Path::new("/Users/example/project/src/lib.rs"),
            "not_found",
            None,
        )]);
        let redacted = sample_snapshot(vec![MemoryDriftAnchor::new(
            MemoryDriftAnchorKind::FilePath,
            &previous_anchor.label,
            MemoryDriftAnchorStatus::Redacted,
            None,
            None,
            MemoryDriftFreshness::unavailable(None, "redacted"),
            BTreeMap::new(),
        )]);

        let missing_item = score_memory_drift(
            &previous,
            &missing,
            MemoryDriftMemoryFactors::new("mem_missing"),
        );
        let redacted_item = score_memory_drift(
            &previous,
            &redacted,
            MemoryDriftMemoryFactors::new("mem_redacted"),
        );

        assert_eq!(missing_item.drift_status, MemoryDriftStatus::MissingSource);
        assert_eq!(redacted_item.drift_status, MemoryDriftStatus::Unverifiable);
        assert!(
            missing_item
                .reason_codes
                .contains(&"source_missing".to_owned())
        );
    }

    #[test]
    fn no_previous_evidence_is_unverifiable() {
        let empty = sample_snapshot(Vec::new());
        let item = score_memory_drift(
            &empty,
            &empty,
            MemoryDriftMemoryFactors::new("mem_no_evidence"),
        );
        assert_eq!(item.drift_status, MemoryDriftStatus::Unverifiable);
        assert!(
            item.reason_codes
                .contains(&"no_previous_evidence".to_owned())
        );
        assert_eq!(item.change_magnitude, 1.0);
    }

    #[test]
    fn suppressed_memory_overrides_changed_evidence() {
        let previous = sample_snapshot(vec![MemoryDriftAnchor::identifier(
            MemoryDriftAnchorKind::SchemaId,
            "ee.memory_drift.snapshot.v1",
            None,
        )]);
        let current = sample_snapshot(vec![MemoryDriftAnchor::identifier(
            MemoryDriftAnchorKind::SchemaId,
            "ee.memory_drift.queue.v1",
            None,
        )]);
        let item = score_memory_drift(
            &previous,
            &current,
            MemoryDriftMemoryFactors {
                memory_id: "mem_suppressed".to_owned(),
                suppressed: true,
                importance: 1.0,
                trust_class: "human_explicit".to_owned(),
                trust_score: 1.0,
                recent_pack_inclusions: 5,
                downstream_graph_refs: 20,
                days_since_validation: 365,
            },
        );
        assert_eq!(item.drift_status, MemoryDriftStatus::Suppressed);
        assert_eq!(item.priority_score_micros, 0);
        assert!(item.comparisons.is_empty());
    }

    #[test]
    fn symbol_moved_becomes_stale_anchor() {
        let previous = sample_snapshot(vec![MemoryDriftAnchor::identifier(
            MemoryDriftAnchorKind::Symbol,
            "old_module::Widget::render",
            None,
        )]);
        let current = sample_snapshot(vec![MemoryDriftAnchor::identifier(
            MemoryDriftAnchorKind::Symbol,
            "new_module::Widget::render",
            None,
        )]);
        let item = score_memory_drift(
            &previous,
            &current,
            MemoryDriftMemoryFactors::new("mem_symbol"),
        );

        assert_eq!(item.drift_status, MemoryDriftStatus::StaleAnchor);
        assert!(
            item.reason_codes
                .contains(&"anchor_not_in_current_snapshot".to_owned())
        );
        assert!(item.reason_codes.contains(&"kind:symbol".to_owned()));
    }

    #[test]
    fn more_affected_anchors_do_not_lower_score() {
        let anchor_a = MemoryDriftAnchor::source_bytes(
            Path::new("/Users/example/project/src/a.rs"),
            b"pub fn a() {}\n",
            64,
            None,
            None,
        );
        let anchor_b = MemoryDriftAnchor::source_bytes(
            Path::new("/Users/example/project/src/b.rs"),
            b"pub fn b() {}\n",
            64,
            None,
            None,
        );
        let previous = sample_snapshot(vec![anchor_a.clone(), anchor_b.clone()]);
        let one_changed = sample_snapshot(vec![
            MemoryDriftAnchor::source_bytes(
                Path::new("/Users/example/project/src/a.rs"),
                b"pub fn changed_a() {}\n",
                64,
                None,
                None,
            ),
            anchor_b.clone(),
        ]);
        let two_changed = sample_snapshot(vec![
            MemoryDriftAnchor::source_bytes(
                Path::new("/Users/example/project/src/a.rs"),
                b"pub fn changed_a() {}\n",
                64,
                None,
                None,
            ),
            MemoryDriftAnchor::source_bytes(
                Path::new("/Users/example/project/src/b.rs"),
                b"pub fn changed_b() {}\n",
                64,
                None,
                None,
            ),
        ]);

        let one = score_memory_drift(
            &previous,
            &one_changed,
            MemoryDriftMemoryFactors::new("mem_one"),
        );
        let two = score_memory_drift(
            &previous,
            &two_changed,
            MemoryDriftMemoryFactors::new("mem_two"),
        );

        assert_eq!(one.affected_anchor_count, 1);
        assert_eq!(two.affected_anchor_count, 2);
        assert!(two.priority_score_micros >= one.priority_score_micros);
    }

    #[test]
    fn queue_order_is_stable_by_score_then_memory_id() {
        let previous = sample_snapshot(vec![MemoryDriftAnchor::identifier(
            MemoryDriftAnchorKind::BeadId,
            "bd-1z1fd.1",
            None,
        )]);
        let current = sample_snapshot(vec![MemoryDriftAnchor::identifier(
            MemoryDriftAnchorKind::BeadId,
            "bd-1z1fd.2",
            None,
        )]);
        let beta = score_memory_drift(
            &previous,
            &current,
            MemoryDriftMemoryFactors::new("mem_beta"),
        );
        let alpha = score_memory_drift(
            &previous,
            &current,
            MemoryDriftMemoryFactors::new("mem_alpha"),
        );
        let queue =
            MemoryDriftQueue::new(Some("2026-05-19T11:00:00Z"), vec![beta, alpha], Some(10))
                .volatile_scrubbed();
        let ids = queue
            .items
            .iter()
            .map(|item| item.memory_id.as_str())
            .collect::<Vec<_>>();
        assert_eq!(ids, vec!["mem_alpha", "mem_beta"]);
        assert_eq!(
            queue.stable_json().expect("queue serializes"),
            MemoryDriftQueue::new(None, queue.items.clone(), Some(10))
                .stable_json()
                .expect("queue serializes")
        );
    }

    #[test]
    fn golden_fixture_deserializes() {
        let fixture =
            include_str!("../../tests/fixtures/golden/memory_drift_snapshot_v1.json.golden");
        let snapshot: MemoryDriftSnapshot =
            serde_json::from_str(fixture).expect("golden fixture remains valid JSON");
        assert_eq!(snapshot.schema, MEMORY_DRIFT_SNAPSHOT_SCHEMA_V1);
        assert_eq!(snapshot.anchors.len(), 4);
        assert!(snapshot.anchors.iter().any(|anchor| {
            anchor.kind == MemoryDriftAnchorKind::CommandHash
                && !anchor.label.contains("cargo test")
        }));
    }

    #[test]
    fn queue_golden_fixture_deserializes() {
        let fixture = include_str!("../../tests/fixtures/golden/memory_drift_queue_v1.json.golden");
        let queue: MemoryDriftQueue =
            serde_json::from_str(fixture).expect("queue golden fixture remains valid JSON");
        assert_eq!(queue.schema, MEMORY_DRIFT_QUEUE_SCHEMA_V1);
        assert_eq!(queue.items.len(), 2);
        assert_eq!(queue.items[0].memory_id, "mem_changed");
    }

    #[test]
    fn provenance_status_maps_to_report_and_selection_hints() {
        let changed = memory_drift_report_hint_from_provenance_status(
            "mem_changed",
            "mismatch",
            Some("blake3:changed"),
        );
        assert_eq!(changed.drift_status, MemoryDriftStatus::Changed);
        assert_eq!(changed.top_reason, "provenance_chain_mismatch");
        assert_eq!(changed.evidence_count, 1);
        assert_eq!(
            changed.degraded_code.as_deref(),
            Some("memory_drift_source_changed")
        );
        assert_eq!(changed.severity, "medium");
        assert_eq!(
            changed.compact_json(),
            serde_json::json!({
                "driftStatus": "changed",
                "freshness": "drifted",
                "topReason": "provenance_chain_mismatch",
                "evidenceCount": 1,
                "revalidationCommand": "ee memory drift mem_changed --json",
                "staleAnchor": false,
            })
        );

        let missing =
            memory_drift_selection_hint_from_provenance_status("mem_missing", "missing", None)
                .expect("missing provenance status should be surfaced as a selection hint");
        assert_eq!(missing.drift_status, MemoryDriftStatus::MissingSource);
        assert_eq!(missing.evidence_count, 0);
        assert_eq!(
            missing.degraded_code.as_deref(),
            Some("memory_drift_source_missing")
        );

        assert!(
            memory_drift_selection_hint_from_provenance_status(
                "mem_current",
                "verified",
                Some("blake3:current"),
            )
            .is_none(),
            "verified memories should not add per-selection degraded hints"
        );
    }

    #[test]
    fn code_anchor_report_rechecks_live_file_hash_even_when_stored_anchor_is_current()
    -> Result<(), String> {
        let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
        let repo = tempdir.path();
        std::fs::create_dir_all(repo.join("src")).map_err(|error| error.to_string())?;
        run_memory_drift_git(repo, &["init", "-q", "-b", "main"])?;
        run_memory_drift_git(repo, &["config", "user.email", "ee-test@example.test"])?;
        run_memory_drift_git(repo, &["config", "user.name", "ee test"])?;
        std::fs::write(repo.join("src/lib.rs"), "pub fn trust_probe() {}\n")
            .map_err(|error| error.to_string())?;
        run_memory_drift_git(repo, &["add", "src/lib.rs"])?;
        run_memory_drift_git(
            repo,
            &[
                "-c",
                "commit.gpgsign=false",
                "commit",
                "-q",
                "-m",
                "baseline",
            ],
        )?;
        let captured = git_output(repo, &["rev-parse", "--verify", "HEAD"])
            .and_then(|output| first_hex_commit(&output))
            .ok_or_else(|| "captured commit should be available".to_owned())?;
        std::fs::write(repo.join("src/lib.rs"), "pub fn trust_probe_changed() {}\n")
            .map_err(|error| error.to_string())?;
        run_memory_drift_git(repo, &["add", "src/lib.rs"])?;
        run_memory_drift_git(
            repo,
            &["-c", "commit.gpgsign=false", "commit", "-q", "-m", "change"],
        )?;
        let current = git_output(repo, &["rev-parse", "--verify", "HEAD"])
            .and_then(|output| first_hex_commit(&output))
            .ok_or_else(|| "current commit should be available".to_owned())?;
        assert_ne!(captured, current);

        let anchor_hash = crate::models::memory_anchor_value_hash(
            crate::models::MemoryAnchorKind::Path,
            "src/lib.rs",
        );
        let anchor = StoredMemoryAnchor {
            memory_id: "mem_anchor".to_owned(),
            anchor_kind: crate::models::MemoryAnchorKind::Path,
            anchor_value_hash: anchor_hash.clone(),
            redacted_anchor_value: "path:lib.rs:abcdef12".to_owned(),
            confidence: 1.0,
            source: crate::models::MemoryAnchorSource::Explicit,
            provenance: format!("git-sha://{captured}"),
            captured_span_hash: anchor_hash,
            freshness_state: MemoryAnchorFreshnessState::Current,
            generation: 7,
            created_at: "2026-06-18T00:00:00Z".to_owned(),
            updated_at: "2026-06-18T00:00:00Z".to_owned(),
        };
        let memory = StoredMemory {
            id: "mem_anchor".to_owned(),
            workspace_id: "wsp_anchor".to_owned(),
            level: "episodic".to_owned(),
            kind: "fact".to_owned(),
            content: format!("Memory captured at commit {captured} for ee-anchor:path:src/lib.rs."),
            workflow_id: None,
            confidence: 0.8,
            utility: 0.5,
            importance: 0.5,
            provenance_uri: Some(format!("git-sha://{captured}")),
            trust_class: "agent_assertion".to_owned(),
            trust_subclass: None,
            provenance_chain_hash: Some("blake3:verified".to_owned()),
            provenance_chain_hash_version: "v1".to_owned(),
            provenance_verification_status: "verified".to_owned(),
            provenance_verified_at: None,
            provenance_verification_note: None,
            created_at: "2026-06-18T00:00:00Z".to_owned(),
            updated_at: "2026-06-18T00:00:00Z".to_owned(),
            tombstoned_at: None,
            valid_from: None,
            valid_to: None,
        };

        let hint = memory_drift_report_hint_from_memory_and_anchors(repo, &memory, vec![anchor]);
        assert_eq!(hint.drift_status, MemoryDriftStatus::Changed);
        assert_eq!(hint.top_reason, "code_anchor_hash_changed");
        assert_eq!(hint.freshness, "drifted");
        assert!(hint.stale_anchor);
        assert_eq!(hint.captured_at_commit.as_deref(), Some(captured.as_str()));
        assert_eq!(hint.current_commit.as_deref(), Some(current.as_str()));
        assert_eq!(hint.commit_distance, Some(1));
        assert_eq!(hint.changed_regions, vec!["path:lib.rs:abcdef12"]);
        assert_eq!(hint.anchors.len(), 1);
        assert_eq!(hint.anchors[0].freshness_state, "stale");
        assert_eq!(hint.anchors[0].freshness, "drifted");
        assert!(hint.anchors[0].stale_anchor);
        assert_eq!(
            hint.compact_json()
                .pointer("/staleAnchor")
                .and_then(serde_json::Value::as_bool),
            Some(true)
        );
        Ok(())
    }

    #[test]
    fn code_anchor_report_outranks_routine_unverified_provenance() -> Result<(), String> {
        let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
        let workspace = tempdir.path();
        std::fs::create_dir_all(workspace.join("src")).map_err(|error| error.to_string())?;
        std::fs::write(workspace.join("src/lib.rs"), "pub fn trust_probe() {}\n")
            .map_err(|error| error.to_string())?;

        let anchor_hash = crate::models::memory_anchor_value_hash(
            crate::models::MemoryAnchorKind::Path,
            "src/lib.rs",
        );
        let anchor = StoredMemoryAnchor {
            memory_id: "mem_anchor".to_owned(),
            anchor_kind: crate::models::MemoryAnchorKind::Path,
            anchor_value_hash: anchor_hash.clone(),
            redacted_anchor_value: "path:lib.rs:abcdef12".to_owned(),
            confidence: 1.0,
            source: crate::models::MemoryAnchorSource::Explicit,
            provenance: "memory.content".to_owned(),
            captured_span_hash: anchor_hash,
            freshness_state: MemoryAnchorFreshnessState::Current,
            generation: 7,
            created_at: "1970-01-01T00:00:00Z".to_owned(),
            updated_at: "1970-01-01T00:00:00Z".to_owned(),
        };
        let memory = StoredMemory {
            id: "mem_anchor".to_owned(),
            workspace_id: "wsp_anchor".to_owned(),
            level: "episodic".to_owned(),
            kind: "fact".to_owned(),
            content: "Memory with ee-anchor:path:src/lib.rs but no git baseline.".to_owned(),
            workflow_id: None,
            confidence: 0.8,
            utility: 0.5,
            importance: 0.5,
            provenance_uri: None,
            trust_class: "agent_assertion".to_owned(),
            trust_subclass: None,
            provenance_chain_hash: None,
            provenance_chain_hash_version: "v1".to_owned(),
            provenance_verification_status: "unverified".to_owned(),
            provenance_verified_at: None,
            provenance_verification_note: None,
            created_at: "1970-01-01T00:00:00Z".to_owned(),
            updated_at: "1970-01-01T00:00:00Z".to_owned(),
            tombstoned_at: None,
            valid_from: None,
            valid_to: None,
        };

        let hint =
            memory_drift_report_hint_from_memory_and_anchors(workspace, &memory, vec![anchor]);
        assert_eq!(hint.drift_status, MemoryDriftStatus::Unverifiable);
        assert_eq!(hint.top_reason, "code_anchor_mtime_newer_than_anchor");
        assert_eq!(hint.freshness, "unknown");
        assert_eq!(
            hint.degraded_code.as_deref(),
            Some("memory_drift_source_unverifiable")
        );
        assert!(hint.stale_anchor);
        assert_eq!(hint.anchors.len(), 1);
        assert_eq!(hint.anchors[0].freshness_state, "suspect");
        assert_eq!(hint.anchors[0].freshness, "unknown");
        assert!(hint.anchors[0].stale_anchor);
        Ok(())
    }

    fn run_memory_drift_git(repo: &Path, args: &[&str]) -> Result<(), String> {
        let output = Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(args)
            .output()
            .map_err(|error| error.to_string())?;
        if output.status.success() {
            Ok(())
        } else {
            Err(String::from_utf8_lossy(&output.stderr).to_string())
        }
    }

    #[test]
    fn selection_hint_compact_json_is_field_bounded() {
        let changed = memory_drift_report_hint_from_provenance_status(
            "mem_changed",
            "mismatch",
            Some("blake3:changed"),
        );
        let compact = changed.compact_json();
        let compact_object = compact.as_object().expect("compact hint is an object");
        assert_eq!(compact_object.len(), 6);
        assert!(compact_object.contains_key("driftStatus"));
        assert!(compact_object.contains_key("freshness"));
        assert!(compact_object.contains_key("topReason"));
        assert!(compact_object.contains_key("evidenceCount"));
        assert!(compact_object.contains_key("revalidationCommand"));
        assert!(compact_object.contains_key("staleAnchor"));
        assert!(!compact_object.contains_key("memoryId"));
        assert!(!compact_object.contains_key("degradedCode"));
        assert!(!compact_object.contains_key("severity"));

        let rendered = serde_json::to_string(&compact).expect("compact hint serializes");
        assert!(
            rendered.len() <= 210,
            "compact drift hint exceeded budget: {rendered}"
        );
        assert!(!rendered.contains("blake3:changed"));
        assert!(!rendered.contains("memory_drift_source_changed"));
        assert!(!rendered.contains("medium"));
    }

    #[test]
    fn report_golden_fixture_matches_stable_order_and_summary() {
        let report = MemoryDriftReport::new(
            MemoryDriftReportMode::RecentPackItems,
            Some("2026-05-19T11:00:00Z"),
            vec![
                memory_drift_report_hint_from_provenance_status(
                    "mem_current",
                    "verified",
                    Some("blake3:current"),
                ),
                memory_drift_report_hint_from_provenance_status("mem_missing", "missing", None),
                memory_drift_report_hint_from_provenance_status(
                    "mem_changed",
                    "mismatch",
                    Some("blake3:changed"),
                ),
            ],
        )
        .with_degraded(vec![MemoryDriftDegradation::new(
            "memory_drift_report_partial",
            "low",
            "Fixture report omits unavailable source bodies.",
        )])
        .volatile_scrubbed();

        let fixture =
            include_str!("../../tests/fixtures/golden/memory_drift_report_v1.json.golden");
        let expected: MemoryDriftReport =
            serde_json::from_str(fixture).expect("report golden fixture remains valid JSON");
        assert_eq!(report, expected);
        assert_eq!(report.summary.total_memories, 3);
        assert_eq!(
            report
                .items
                .iter()
                .map(|item| item.memory_id.as_str())
                .collect::<Vec<_>>(),
            vec!["mem_missing", "mem_changed", "mem_current"]
        );
    }

    #[test]
    fn support_summary_counts_status_source_and_degraded_without_raw_payloads() {
        let report = MemoryDriftReport::new(
            MemoryDriftReportMode::RecentPackItems,
            Some("2026-05-19T11:30:00Z"),
            vec![
                memory_drift_report_hint_from_provenance_status(
                    "mem_changed`unsafe`",
                    "mismatch",
                    Some("blake3:changed"),
                ),
                memory_drift_report_hint_from_provenance_status(
                    "mem_missing$(unsafe)",
                    "missing",
                    None,
                ),
                memory_drift_report_hint_from_provenance_status(
                    "mem_current",
                    "verified",
                    Some("blake3:current"),
                ),
            ],
        )
        .with_degraded(vec![MemoryDriftDegradation::new(
            "memory_drift_report_partial",
            "low",
            "Fixture report omits unavailable source bodies.",
        )]);

        let summary = memory_drift_support_summary_from_report(&report);
        let encoded = serde_json::to_string(&summary).expect("support summary serializes");

        assert_eq!(
            summary.get("schema").and_then(serde_json::Value::as_str),
            Some(MEMORY_DRIFT_SUPPORT_SUMMARY_SCHEMA_V1)
        );
        assert_eq!(
            summary
                .pointer("/counts/affected")
                .and_then(serde_json::Value::as_u64),
            Some(2)
        );
        assert_eq!(
            summary
                .pointer("/sourceKindCounts/provenance_chain")
                .and_then(serde_json::Value::as_u64),
            Some(3)
        );
        assert!(encoded.contains("memory_drift_source_changed"));
        assert!(encoded.contains("memory_drift_source_missing"));
        assert!(encoded.contains("memory_drift_report_partial"));
        assert!(encoded.contains("mem_changed_unsafe_"));
        assert!(encoded.contains("mem_missing__unsafe_"));
        assert!(!encoded.contains("blake3:changed"));
        assert!(!encoded.contains("ee memory drift"));
        assert!(!encoded.contains('$'));
        assert!(!encoded.contains('`'));
    }

    #[test]
    fn support_summary_report_unavailable_is_explicit_and_redaction_safe() {
        let summary = memory_drift_support_summary_unavailable(
            "database_unavailable",
            MEMORY_DRIFT_REPORT_UNAVAILABLE_CODE,
            "Database was unavailable at /Users/example/private/path",
        );
        let encoded = serde_json::to_string(&summary).expect("support summary serializes");

        assert_eq!(
            summary.get("status").and_then(serde_json::Value::as_str),
            Some("database_unavailable")
        );
        assert!(encoded.contains(MEMORY_DRIFT_REPORT_UNAVAILABLE_CODE));
        assert_eq!(
            summary
                .pointer("/evidenceInspection/memoryEvidenceInspected")
                .and_then(serde_json::Value::as_bool),
            Some(false)
        );
        assert_eq!(
            summary
                .pointer("/evidenceInspection/sourceFreshness")
                .and_then(serde_json::Value::as_str),
            Some("not_inspected")
        );
        assert_eq!(
            summary
                .pointer("/degraded/0/severity")
                .and_then(serde_json::Value::as_str),
            Some("warning")
        );
        assert_eq!(
            summary
                .pointer("/degraded/0/repair")
                .and_then(serde_json::Value::as_str),
            Some("ee doctor --json")
        );
        assert!(encoded.contains("\"rawSnippetsIncluded\":false"));
        assert!(!encoded.contains("/Users"));
        assert!(!encoded.contains('$'));
        assert!(!encoded.contains('`'));
    }

    #[test]
    fn support_summary_lock_contention_projects_not_inspected_recovery() {
        let summary = memory_drift_support_summary_unavailable(
            "lock_contention",
            MEMORY_DRIFT_LOCK_CONTENTION_CODE,
            &memory_drift_lock_contention_message("support_bundle"),
        );
        let encoded = serde_json::to_string(&summary).expect("support summary serializes");

        assert_eq!(
            summary.get("status").and_then(serde_json::Value::as_str),
            Some("lock_contention")
        );
        assert_eq!(
            summary
                .pointer("/evidenceInspection/memoryEvidenceInspected")
                .and_then(serde_json::Value::as_bool),
            Some(false)
        );
        assert_eq!(
            summary
                .pointer("/evidenceInspection/sourceFreshness")
                .and_then(serde_json::Value::as_str),
            Some("not_inspected")
        );
        assert_eq!(
            summary
                .pointer("/evidenceInspection/lockAcquisitionClass")
                .and_then(serde_json::Value::as_str),
            Some(MEMORY_DRIFT_READ_ONLY_COLLECTOR_LOCK_CLASS)
        );
        assert_eq!(
            summary
                .pointer("/degraded/0/repair")
                .and_then(serde_json::Value::as_str),
            Some("ee doctor --json")
        );
        assert_eq!(
            summary
                .pointer("/evidenceInspection/supportBundleMeaning")
                .and_then(serde_json::Value::as_str),
            Some("collector_blocked_before_evidence")
        );
        assert_eq!(
            summary
                .pointer("/degraded/0/severity")
                .and_then(serde_json::Value::as_str),
            Some("warning")
        );
        assert!(
            summary
                .pointer("/evidenceInspection/recoverySuggestions")
                .and_then(serde_json::Value::as_array)
                .is_some_and(|actions| {
                    actions
                        .iter()
                        .any(|action| action.as_str() == Some("retry_read_only_drift_report"))
                        && actions
                            .iter()
                            .any(|action| action.as_str() == Some("use_source_authority_snapshot"))
                })
        );
        assert!(encoded.contains(MEMORY_DRIFT_LOCK_CONTENTION_CODE));
        assert!(!encoded.contains("/Users"));
        assert!(!encoded.contains("stdout"));
        assert!(!encoded.contains("stderr"));
    }

    #[test]
    fn read_only_collector_strategy_uses_true_read_only_open() {
        let strategy = memory_drift_read_only_collector_strategy();

        assert_eq!(
            strategy.name,
            MEMORY_DRIFT_READ_ONLY_COLLECTOR_STRATEGY_NAME
        );
        assert_eq!(
            strategy.lock_acquisition_class,
            MEMORY_DRIFT_READ_ONLY_COLLECTOR_LOCK_CLASS
        );
        assert!(strategy.true_read_only_database_open_available);
        assert!(
            !strategy.memory_evidence_inspected_on_lock_contention,
            "lock contention must mean the collector never inspected memory evidence"
        );
        assert_eq!(
            strategy.unblocks_true_read_only_bead,
            MEMORY_DRIFT_TRUE_READ_ONLY_DATABASE_OPEN_BEAD
        );
    }

    #[test]
    fn read_only_collector_lock_contention_error_is_bounded_and_redaction_safe() {
        let db_error = DbError::InvalidPath {
            operation: crate::db::DbOperation::BeginTransaction,
            path: std::path::PathBuf::from("/Users/example/private/ee.sqlite3"),
            message: "database is busy (snapshot conflict)".to_owned(),
        };

        let error = memory_drift_read_only_open_error(db_error);
        let message = error.message();

        assert!(memory_drift_error_is_lock_contention(&error));
        assert!(message.contains(MEMORY_DRIFT_READ_ONLY_COLLECTOR_STRATEGY_NAME));
        assert!(message.contains("before any evidence inspection"));
        assert!(!message.contains("/Users"));
        assert!(!message.contains("private"));
        match &error {
            DomainError::Storage { repair, .. } => {
                assert_eq!(repair.as_deref(), Some(MEMORY_DRIFT_LOCK_CONTENTION_REPAIR))
            }
            other => panic!("expected storage error, got {other:?}"),
        }
    }

    #[test]
    fn all_and_one_report_golden_fixtures_match() {
        let all_report = MemoryDriftReport::new(
            MemoryDriftReportMode::AllMemories,
            Some("2026-05-19T11:05:00Z"),
            vec![
                memory_drift_report_hint_from_provenance_status(
                    "mem_current",
                    "verified",
                    Some("blake3:current"),
                ),
                memory_drift_report_hint_from_provenance_status(
                    "mem_unverified",
                    "unverified",
                    None,
                ),
                memory_drift_report_hint_from_provenance_status("mem_missing", "missing", None),
                memory_drift_report_hint_from_provenance_status(
                    "mem_changed",
                    "mismatch",
                    Some("blake3:changed"),
                ),
            ],
        )
        .volatile_scrubbed();
        let all_fixture =
            include_str!("../../tests/fixtures/golden/memory_drift_report_all_v1.json.golden");
        let all_expected: MemoryDriftReport =
            serde_json::from_str(all_fixture).expect("all-memory report golden remains valid JSON");
        assert_eq!(all_report, all_expected);

        let one_report = MemoryDriftReport::new(
            MemoryDriftReportMode::OneMemory,
            Some("2026-05-19T11:10:00Z"),
            vec![memory_drift_report_hint_from_provenance_status(
                "mem_changed",
                "mismatch",
                Some("blake3:changed"),
            )],
        )
        .volatile_scrubbed();
        let one_fixture =
            include_str!("../../tests/fixtures/golden/memory_drift_report_one_v1.json.golden");
        let one_expected: MemoryDriftReport =
            serde_json::from_str(one_fixture).expect("one-memory report golden remains valid JSON");
        assert_eq!(one_report, one_expected);
    }

    #[test]
    fn report_builder_is_read_only_over_existing_database() -> Result<(), String> {
        let connection = DbConnection::open_memory().map_err(|error| error.to_string())?;
        connection.migrate().map_err(|error| error.to_string())?;
        let workspace_path = Path::new("/tmp/ee-memory-drift-read-only");
        let workspace_id = stable_workspace_id(workspace_path);
        connection
            .insert_workspace(
                &workspace_id,
                &CreateWorkspaceInput {
                    path: workspace_path.display().to_string(),
                    name: Some("memory drift read only".to_owned()),
                },
            )
            .map_err(|error| error.to_string())?;
        connection
            .insert_memory(
                "mem_driftreadonly0000000000001",
                &CreateMemoryInput {
                    workspace_id: workspace_id.clone(),
                    level: "procedural".to_owned(),
                    kind: "rule".to_owned(),
                    content: "Check memory drift before reusing stale source-grounded memories."
                        .to_owned(),
                    workflow_id: None,
                    confidence: 0.9,
                    utility: 0.7,
                    importance: 0.8,
                    provenance_uri: Some("file://AGENTS.md#L1".to_owned()),
                    trust_class: "agent_assertion".to_owned(),
                    trust_subclass: None,
                    tags: Vec::new(),
                    valid_from: None,
                    valid_to: None,
                },
            )
            .map_err(|error| error.to_string())?;
        connection
            .execute_raw(
                "UPDATE memories SET provenance_verification_status = 'mismatch', provenance_chain_hash = 'blake3:changed' WHERE id = 'mem_driftreadonly0000000000001'",
            )
            .map_err(|error| error.to_string())?;

        let audit_before = connection
            .list_audit_entries(Some(&workspace_id), None)
            .map_err(|error| error.to_string())?
            .len();
        let options = MemoryDriftReportOptions {
            database_path: Path::new(":memory:"),
            workspace_path,
            mode: MemoryDriftReportMode::AllMemories,
            memory_id: None,
            limit: 10,
            include_tombstoned: false,
            as_of: None,
        };
        let report = build_memory_drift_report_with_connection(&connection, &options)
            .map_err(|error| error.to_string())?;
        let audit_after = connection
            .list_audit_entries(Some(&workspace_id), None)
            .map_err(|error| error.to_string())?
            .len();

        assert_eq!(report.summary.changed, 1);
        assert_eq!(
            audit_after, audit_before,
            "memory drift report must not emit audit rows"
        );
        connection.close().map_err(|error| error.to_string())?;
        Ok(())
    }

    #[test]
    fn read_only_report_builder_preserves_audit_rows_on_file_database() -> Result<(), String> {
        let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
        let db_path = tempdir.path().join("ee.db");
        let workspace_path = tempdir.path().join("workspace");
        std::fs::create_dir_all(&workspace_path).map_err(|error| error.to_string())?;
        let canonical_workspace = workspace_path
            .canonicalize()
            .map_err(|error| error.to_string())?;
        let workspace_id = stable_workspace_id(&canonical_workspace);

        {
            let connection =
                DbConnection::open_file(&db_path).map_err(|error| error.to_string())?;
            connection.migrate().map_err(|error| error.to_string())?;
            connection
                .insert_workspace(
                    &workspace_id,
                    &CreateWorkspaceInput {
                        path: canonical_workspace.display().to_string(),
                        name: Some("memory drift read only file".to_owned()),
                    },
                )
                .map_err(|error| error.to_string())?;
            let memory_id = crate::testing::mem("driftreadonlyfile");
            connection
                .insert_memory(
                    &memory_id,
                    &CreateMemoryInput {
                        workspace_id: workspace_id.clone(),
                        level: "procedural".to_owned(),
                        kind: "rule".to_owned(),
                        content: "Read-only swarm collectors must not append audit rows."
                            .to_owned(),
                        workflow_id: None,
                        confidence: 0.9,
                        utility: 0.7,
                        importance: 0.8,
                        provenance_uri: Some("file://AGENTS.md#L1".to_owned()),
                        trust_class: "agent_assertion".to_owned(),
                        trust_subclass: None,
                        tags: Vec::new(),
                        valid_from: None,
                        valid_to: None,
                    },
                )
                .map_err(|error| error.to_string())?;
            connection
                .execute_raw(&format!(
                    "UPDATE memories SET provenance_verification_status = 'mismatch', \
                     provenance_chain_hash = 'blake3:changed' \
                     WHERE id = '{memory_id}'"
                ))
                .map_err(|error| error.to_string())?;
            connection.close().map_err(|error| error.to_string())?;
        }

        let audit_before = {
            let connection =
                DbConnection::open_file(&db_path).map_err(|error| error.to_string())?;
            let count = connection
                .list_audit_entries(Some(&workspace_id), None)
                .map_err(|error| error.to_string())?
                .len();
            connection.close().map_err(|error| error.to_string())?;
            count
        };
        let options = MemoryDriftReportOptions {
            database_path: &db_path,
            workspace_path: &canonical_workspace,
            mode: MemoryDriftReportMode::AllMemories,
            memory_id: None,
            limit: 10,
            include_tombstoned: false,
            as_of: None,
        };

        let report =
            build_memory_drift_report_read_only(&options).map_err(|error| error.to_string())?;

        let audit_after = {
            let connection =
                DbConnection::open_file(&db_path).map_err(|error| error.to_string())?;
            let count = connection
                .list_audit_entries(Some(&workspace_id), None)
                .map_err(|error| error.to_string())?
                .len();
            connection.close().map_err(|error| error.to_string())?;
            count
        };

        assert_eq!(report.summary.changed, 1);
        assert_eq!(
            audit_after, audit_before,
            "memory drift read-only collector must not emit audit rows"
        );
        Ok(())
    }

    fn sample_stored_anchor() -> StoredMemoryAnchor {
        StoredMemoryAnchor {
            memory_id: "mem_01234567890123456789012345".to_owned(),
            anchor_kind: crate::models::MemoryAnchorKind::Symbol,
            anchor_value_hash:
                "blake3:0000000000000000000000000000000000000000000000000000000000000000".to_owned(),
            redacted_anchor_value: "symbol:blake3:000000000000".to_owned(),
            confidence: 0.9,
            source: crate::models::MemoryAnchorSource::Remember,
            provenance: "test".to_owned(),
            captured_span_hash:
                "blake3:1111111111111111111111111111111111111111111111111111111111111111".to_owned(),
            freshness_state: MemoryAnchorFreshnessState::Current,
            generation: 0,
            created_at: "2026-06-07T00:00:00Z".to_owned(),
            updated_at: "2026-06-07T00:00:00Z".to_owned(),
        }
    }

    #[test]
    fn freshness_state_for_drift_is_conservative() {
        assert_eq!(
            freshness_state_for_drift(MemoryDriftStatus::Current),
            MemoryAnchorFreshnessState::Current
        );
        assert_eq!(
            freshness_state_for_drift(MemoryDriftStatus::Suppressed),
            MemoryAnchorFreshnessState::Current
        );
        // Ambiguity (incl. unresolved rename/move) -> advisory Suspect, never Stale.
        assert_eq!(
            freshness_state_for_drift(MemoryDriftStatus::Unverifiable),
            MemoryAnchorFreshnessState::Suspect
        );
        // Resolved content change / disappearance -> Stale.
        assert_eq!(
            freshness_state_for_drift(MemoryDriftStatus::Changed),
            MemoryAnchorFreshnessState::Stale
        );
        assert_eq!(
            freshness_state_for_drift(MemoryDriftStatus::MissingSource),
            MemoryAnchorFreshnessState::Stale
        );
        assert_eq!(
            freshness_state_for_drift(MemoryDriftStatus::StaleAnchor),
            MemoryAnchorFreshnessState::Stale
        );
    }

    #[test]
    fn freshness_transition_for_drift_records_only_real_changes() {
        let anchor = sample_stored_anchor();
        // No state change -> no audit row.
        assert!(
            freshness_transition_for_drift(
                &anchor,
                MemoryDriftStatus::Current,
                MemoryAnchorFreshnessState::Current,
                None,
                "2026-06-07T00:00:00Z",
            )
            .is_none()
        );
        // Resolved content change -> Current -> Stale, carrying drift code + file:line.
        let transition = freshness_transition_for_drift(
            &anchor,
            MemoryDriftStatus::Changed,
            MemoryAnchorFreshnessState::Current,
            Some("src/db/mod.rs:42".to_owned()),
            "2026-06-07T00:00:00Z",
        )
        .expect("content change must record a transition");
        assert_eq!(
            transition.previous_state,
            MemoryAnchorFreshnessState::Current
        );
        assert_eq!(transition.new_state, MemoryAnchorFreshnessState::Stale);
        assert_eq!(
            transition.drift_code.as_deref(),
            Some("memory_drift_source_changed")
        );
        assert_eq!(transition.file_line.as_deref(), Some("src/db/mod.rs:42"));
        assert!(transition.is_degradation());
        assert_eq!(transition.anchor_value_hash, anchor.anchor_value_hash);
    }
    /// bd-1xpq9: lock-contention classification matches database
    /// read-snapshot errors, but never the workspace write-owner flock.
    #[test]
    fn lock_contention_classification_matches_stable_lock_errors() {
        let contended = DomainError::Storage {
            message: "Failed to begin memory drift read snapshot: database is busy \
                      (snapshot conflict on pages: 4)"
                .to_owned(),
            repair: None,
        };
        assert!(memory_drift_error_is_lock_contention(&contended));
        let workspace_write_lock = DomainError::Storage {
            message: "could not acquire database write lock: contention timeout".to_owned(),
            repair: None,
        };
        assert!(!memory_drift_error_is_lock_contention(
            &workspace_write_lock
        ));
        let open_failed = DomainError::Storage {
            message: "Failed to open database read-only for memory drift report: permission denied"
                .to_owned(),
            repair: None,
        };
        assert!(!memory_drift_error_is_lock_contention(&open_failed));
        let unrelated = DomainError::Storage {
            message: "Failed to open database read-only for memory drift report: disk I/O error"
                .to_owned(),
            repair: None,
        };
        assert!(!memory_drift_error_is_lock_contention(&unrelated));
    }

    /// bd-1xpq9: the structured details state explicitly that no memory
    /// evidence was inspected, with redaction-safe fields only.
    #[test]
    fn lock_contention_details_are_explicit_and_redaction_safe() {
        let details = memory_drift_lock_contention_details("swarm_brief");
        assert_eq!(
            details["schema"],
            serde_json::json!(MEMORY_DRIFT_LOCK_CONTENTION_SCHEMA)
        );
        assert_eq!(
            details["collectorSurface"],
            serde_json::json!("swarm_brief")
        );
        assert_eq!(
            details["lockAcquisitionClass"],
            serde_json::json!(MEMORY_DRIFT_READ_ONLY_COLLECTOR_LOCK_CLASS)
        );
        assert!(details.get("lockPath").is_none());
        assert_eq!(details["memoryEvidenceInspected"], serde_json::json!(false));
        assert_eq!(
            details["sourceFreshness"],
            serde_json::json!("not_inspected")
        );
        let rendered = details.to_string();
        assert!(
            !rendered.contains("/Users") && !rendered.contains("/private"),
            "details must not leak host-private absolute paths"
        );
        let message = memory_drift_lock_contention_message("swarm_brief");
        assert!(
            message.contains("database read-snapshot contention")
                && message.contains("NOT inspected"),
            "the message must identify database snapshot contention and state evidence was not inspected"
        );
        assert!(!message.contains("workspace write-lock"));
        assert!(MEMORY_DRIFT_LOCK_CONTENTION_REPAIR.contains("ee doctor --json"));
        assert!(!MEMORY_DRIFT_LOCK_CONTENTION_REPAIR.contains("advisory-lock"));
    }

    #[test]
    fn recent_pack_integrity_finding_projects_secret_shaped_memory_id() {
        const SECRET: &str = "AKIAIOSFODNN7EXAMPLE";
        let hostile_id = format!("mem_{SECRET}000000");
        let finding = recent_pack_integrity_finding(&hostile_id, "pack_item_memory_row_missing", 1);

        assert_eq!(
            finding.memory_id,
            crate::models::public_memory_id(&hostile_id)
        );
        let rendered = serde_json::to_string(&finding).expect("serialize public drift finding");
        assert!(!rendered.contains(SECRET));
        assert!(!rendered.contains(&hostile_id));
    }
}
