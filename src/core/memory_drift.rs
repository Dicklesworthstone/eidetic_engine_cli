//! Read-only provenance snapshots for memory drift checks.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde::{Deserialize, Serialize};

use super::workspace::stable_workspace_id;
use crate::db::{DbConnection, StoredMemory};
use crate::models::DomainError;

pub const MEMORY_DRIFT_SNAPSHOT_SCHEMA_V1: &str = "ee.memory_drift.snapshot.v1";
pub const MEMORY_DRIFT_QUEUE_SCHEMA_V1: &str = "ee.memory_drift.queue.v1";
pub const MEMORY_DRIFT_REPORT_SCHEMA_V1: &str = "ee.memory_drift.report.v1";
pub const MEMORY_DRIFT_SUPPORT_SUMMARY_SCHEMA_V1: &str =
    "ee.support_bundle.memory_drift_summary.v1";
pub const DEFAULT_MEMORY_DRIFT_SOURCE_WINDOW_BYTES: usize = 4096;
pub const MAX_MEMORY_DRIFT_SUPPORT_SUMMARY_ITEMS: usize = 8;

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
            Self::Current | Self::StaleAnchor | Self::Suppressed => None,
            Self::Changed => Some("memory_drift_source_changed"),
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
pub struct MemoryDriftSelectionHint {
    pub memory_id: String,
    pub drift_status: MemoryDriftStatus,
    pub top_reason: String,
    pub evidence_count: u32,
    pub revalidation_command: String,
    pub degraded_code: Option<String>,
    pub severity: String,
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
            top_reason: normalized_non_empty(Some(top_reason))
                .unwrap_or_else(|| drift_status.default_reason().to_owned()),
            evidence_count,
            degraded_code: drift_status.degraded_code().map(str::to_owned),
            severity: drift_status.report_severity().to_owned(),
        }
    }

    #[must_use]
    pub fn compact_json(&self) -> serde_json::Value {
        serde_json::json!({
            "driftStatus": self.drift_status.as_str(),
            "topReason": &self.top_reason,
            "evidenceCount": self.evidence_count,
            "revalidationCommand": &self.revalidation_command,
        })
    }

    fn sort_key(&self) -> (std::cmp::Reverse<u8>, String) {
        (
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
    let report_command = match mode {
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
    };

    vec![
        MemoryDriftRecoveryAction::new(
            1,
            "rerun_source_validation",
            Some(report_command),
            "Rerun the read-only drift report before reusing affected memories.",
        ),
        MemoryDriftRecoveryAction::new(
            2,
            "revise_memory",
            None,
            "Revise or re-remember affected memories after updating their source evidence.",
        ),
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
    serde_json::json!({
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
            "severity": "medium",
            "message": memory_drift_support_message(message),
        }],
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

#[derive(Clone, Debug)]
pub struct MemoryDriftReportOptions<'a> {
    pub database_path: &'a Path,
    pub workspace_path: &'a Path,
    pub mode: MemoryDriftReportMode,
    pub memory_id: Option<&'a str>,
    pub limit: u32,
    pub include_tombstoned: bool,
}

pub fn build_memory_drift_report(
    options: &MemoryDriftReportOptions<'_>,
) -> Result<MemoryDriftReport, DomainError> {
    let connection = open_memory_drift_database(options.database_path)?;
    build_memory_drift_report_with_connection(&connection, options)
}

pub fn build_memory_drift_report_read_only(
    options: &MemoryDriftReportOptions<'_>,
) -> Result<MemoryDriftReport, DomainError> {
    let connection =
        DbConnection::open_file(options.database_path).map_err(|error| DomainError::Storage {
            message: format!("Failed to open database read-only for memory drift report: {error}"),
            repair: Some("ee doctor --json".to_owned()),
        })?;
    build_memory_drift_report_with_connection(&connection, options)
}

fn build_memory_drift_report_with_connection(
    connection: &DbConnection,
    options: &MemoryDriftReportOptions<'_>,
) -> Result<MemoryDriftReport, DomainError> {
    let workspace_path = options
        .workspace_path
        .canonicalize()
        .unwrap_or_else(|_| options.workspace_path.to_path_buf());
    let workspace_id = stable_workspace_id(&workspace_path);
    let limit = options.limit.max(1);

    let items = match options.mode {
        MemoryDriftReportMode::AllMemories => memory_drift_report_all_memories(
            connection,
            &workspace_id,
            limit,
            options.include_tombstoned,
        )?,
        MemoryDriftReportMode::OneMemory => {
            let memory_id = options.memory_id.ok_or_else(|| DomainError::Usage {
                message: "memory drift --mode one requires MEMORY_ID".to_owned(),
                repair: Some("ee memory drift <MEMORY_ID> --json".to_owned()),
            })?;
            vec![memory_drift_report_one_memory(
                connection,
                memory_id,
                options.include_tombstoned,
            )?]
        }
        MemoryDriftReportMode::RecentPackItems => {
            memory_drift_report_recent_pack_items(connection, &workspace_id, limit)?
        }
    };

    Ok(MemoryDriftReport::new(options.mode, None, items))
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
    Ok(memories
        .iter()
        .take(limit as usize)
        .map(memory_drift_report_hint_from_memory)
        .collect())
}

fn memory_drift_report_one_memory(
    connection: &DbConnection,
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
    Ok(memory_drift_report_hint_from_memory(&memory))
}

fn memory_drift_report_recent_pack_items(
    connection: &DbConnection,
    workspace_id: &str,
    limit: u32,
) -> Result<Vec<MemoryDriftSelectionHint>, DomainError> {
    let pack_items = connection
        .list_recent_pack_items_for_workspace(workspace_id, limit)
        .map_err(|error| DomainError::Storage {
            message: format!("Failed to list recent pack items for drift report: {error}"),
            repair: Some("ee doctor --json".to_owned()),
        })?;
    let mut seen = BTreeSet::new();
    let mut hints = Vec::new();
    for (_record, item) in pack_items {
        if !seen.insert(item.memory_id.clone()) {
            continue;
        }
        match connection.get_memory(&item.memory_id) {
            Ok(Some(memory)) => hints.push(memory_drift_report_hint_from_memory(&memory)),
            Ok(None) => hints.push(MemoryDriftSelectionHint::new(
                &item.memory_id,
                MemoryDriftStatus::MissingSource,
                "pack_item_memory_row_missing",
                1,
            )),
            Err(error) => {
                return Err(DomainError::Storage {
                    message: format!(
                        "Failed to query selected pack memory {} for drift report: {error}",
                        item.memory_id
                    ),
                    repair: Some("ee doctor --json".to_owned()),
                });
            }
        }
    }
    Ok(hints)
}

fn memory_drift_report_hint_from_memory(memory: &StoredMemory) -> MemoryDriftSelectionHint {
    memory_drift_report_hint_from_provenance_status(
        &memory.id,
        &memory.provenance_verification_status,
        memory.provenance_chain_hash.as_deref(),
    )
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
    MemoryDriftSelectionHint::new(memory_id, status, reason, evidence_count)
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

    fn sample_snapshot(anchors: Vec<MemoryDriftAnchor>) -> MemoryDriftSnapshot {
        MemoryDriftSnapshot::new(
            "mem_123",
            Some("workspace_abc"),
            Some("2026-05-19T10:00:00Z"),
            16,
            anchors,
        )
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
                "topReason": "provenance_chain_mismatch",
                "evidenceCount": 1,
                "revalidationCommand": "ee memory drift mem_changed --json",
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
    fn selection_hint_compact_json_is_field_bounded() {
        let changed = memory_drift_report_hint_from_provenance_status(
            "mem_changed",
            "mismatch",
            Some("blake3:changed"),
        );
        let compact = changed.compact_json();
        let compact_object = compact.as_object().expect("compact hint is an object");
        assert_eq!(compact_object.len(), 4);
        assert!(compact_object.contains_key("driftStatus"));
        assert!(compact_object.contains_key("topReason"));
        assert!(compact_object.contains_key("evidenceCount"));
        assert!(compact_object.contains_key("revalidationCommand"));
        assert!(!compact_object.contains_key("memoryId"));
        assert!(!compact_object.contains_key("degradedCode"));
        assert!(!compact_object.contains_key("severity"));

        let rendered = serde_json::to_string(&compact).expect("compact hint serializes");
        assert!(
            rendered.len() <= 160,
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
    fn support_summary_unavailable_is_explicit_and_redaction_safe() {
        let summary = memory_drift_support_summary_unavailable(
            "database_unavailable",
            "memory_drift_source_unverifiable",
            "Database was unavailable at /Users/example/private/path",
        );
        let encoded = serde_json::to_string(&summary).expect("support summary serializes");

        assert_eq!(
            summary.get("status").and_then(serde_json::Value::as_str),
            Some("database_unavailable")
        );
        assert!(encoded.contains("memory_drift_source_unverifiable"));
        assert!(encoded.contains("\"rawSnippetsIncluded\":false"));
        assert!(!encoded.contains("/Users"));
        assert!(!encoded.contains('$'));
        assert!(!encoded.contains('`'));
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
}
