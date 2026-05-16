//! Advisory QoS lane registry for foreground/background pressure.
//!
//! The registry is a workspace side-path artifact, not a scheduler and not a
//! source of truth. It stores only redaction-safe hashes and posture metadata so
//! readers can make cooperative throttling decisions without raw queries,
//! memory bodies, peer paths, or secrets.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize, Serializer};
use sha2::{Digest, Sha256};

use crate::core::degraded_aggregation::{DegradationAggregationInput, aggregate_degraded_entries};
use crate::models::DomainError;

pub const QOS_ACTIVE_LANE_REGISTRY_SCHEMA_V1: &str = "ee.qos.active_lane_registry.v1";
pub const QOS_ACTIVE_LANE_RECORD_SCHEMA_V1: &str = "ee.qos.active_lane_record.v1";
pub const QOS_ACTIVE_LANE_SUMMARY_SCHEMA_V1: &str = "ee.qos.active_lane_summary.v1";
pub const QOS_REGISTRY_UNAVAILABLE_CODE: &str = "qos_registry_unavailable";

const REGISTRY_RELATIVE_PATH: &[&str] = &[".ee", "qos", "active-lanes.json"];

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum QosLane {
    ForegroundRead,
    ForegroundWrite,
    BackgroundDerived,
    VerificationRch,
    MaintenanceSteward,
}

impl QosLane {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ForegroundRead => "foreground_read",
            Self::ForegroundWrite => "foreground_write",
            Self::BackgroundDerived => "background_derived",
            Self::VerificationRch => "verification_rch",
            Self::MaintenanceSteward => "maintenance_steward",
        }
    }

    #[must_use]
    pub const fn is_foreground(self) -> bool {
        matches!(self, Self::ForegroundRead | Self::ForegroundWrite)
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum QosLaneStatus {
    Starting,
    Active,
    Yielding,
    Completing,
    Failed,
}

impl QosLaneStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Starting => "starting",
            Self::Active => "active",
            Self::Yielding => "yielding",
            Self::Completing => "completing",
            Self::Failed => "failed",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct QosLaneRecordInput<'a> {
    pub workspace_identity: &'a str,
    pub lane: QosLane,
    pub command_class: &'a str,
    pub process_id: Option<u32>,
    pub profile_label: Option<&'a str>,
    pub budget_label: Option<&'a str>,
    pub request_text: Option<&'a str>,
    pub request_hash: Option<&'a str>,
    pub started_at_epoch_ms: u64,
    pub ttl_ms: u64,
    pub status: QosLaneStatus,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QosLaneRecord {
    pub schema: String,
    pub record_id: String,
    pub workspace_hash: String,
    pub lane: QosLane,
    pub command_class: String,
    pub process_id: Option<u32>,
    pub profile_label: Option<String>,
    pub budget_label: Option<String>,
    pub request_hash: Option<String>,
    pub started_at_epoch_ms: u64,
    pub deadline_epoch_ms: u64,
    pub ttl_ms: u64,
    pub status: QosLaneStatus,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QosActiveLaneRegistry {
    pub schema: String,
    pub records: Vec<QosLaneRecord>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QosLaneSummary {
    pub schema: String,
    pub workspace_hash: String,
    pub active_records: Vec<QosLaneRecord>,
    pub foreground_active_count: u32,
    pub background_active_count: u32,
    pub verification_active_count: u32,
    pub maintenance_active_count: u32,
    pub stale_ignored_count: u32,
    #[serde(serialize_with = "serialize_qos_registry_degradations")]
    pub degraded: Vec<QosRegistryDegradation>,
}

#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QosRegistryDegradation {
    pub code: String,
    pub severity: String,
    pub message: String,
    pub repair: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum QosThrottleCheckpoint {
    BeforeExpensivePhase,
    CheckpointBoundary,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum QosBackgroundThrottleAction {
    Continue,
    ShrinkItemBudget,
    Yield,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QosBackgroundThrottleInput {
    pub lane: QosLane,
    pub checkpoint: QosThrottleCheckpoint,
    pub remaining_item_budget: u32,
    pub minimum_item_budget: u32,
    pub may_yield: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QosBackgroundThrottleDecision {
    pub action: QosBackgroundThrottleAction,
    pub foreground_pressure: bool,
    pub adjusted_item_budget: Option<u32>,
    pub reason: String,
}

impl QosBackgroundThrottleDecision {
    #[must_use]
    pub const fn behavior_changed(&self) -> bool {
        !matches!(self.action, QosBackgroundThrottleAction::Continue)
    }
}

impl QosRegistryDegradation {
    #[must_use]
    pub fn registry_unavailable(message: impl Into<String>) -> Self {
        Self {
            code: QOS_REGISTRY_UNAVAILABLE_CODE.to_owned(),
            severity: "medium".to_owned(),
            message: message.into(),
            repair: "Inspect workspace .ee/qos permissions or disable QoS registry reads."
                .to_owned(),
        }
    }
}

fn serialize_qos_registry_degradations<S>(
    degraded: &[QosRegistryDegradation],
    serializer: S,
) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    aggregate_qos_registry_degradations(degraded).serialize(serializer)
}

fn aggregate_qos_registry_degradations(
    degraded: &[QosRegistryDegradation],
) -> Vec<crate::core::degraded_aggregation::AggregatedDegradation> {
    aggregate_degraded_entries(degraded.iter().map(|entry| {
        DegradationAggregationInput::new(
            "qos_registry",
            entry.code.clone(),
            entry.severity.clone(),
            entry.message.clone(),
            entry.repair.clone(),
        )
    }))
}

#[must_use]
pub fn decide_background_throttle(
    summary: &QosLaneSummary,
    input: QosBackgroundThrottleInput,
) -> QosBackgroundThrottleDecision {
    let foreground_pressure = summary.foreground_active_count > 0;
    if !is_background_work_lane(input.lane) {
        return continue_decision(foreground_pressure, "lane_not_background_or_maintenance");
    }
    if !summary.degraded.is_empty() {
        return continue_decision(foreground_pressure, "qos_summary_degraded_fail_open");
    }
    if !foreground_pressure {
        return continue_decision(false, "no_foreground_pressure");
    }
    if input.may_yield && input.checkpoint == QosThrottleCheckpoint::CheckpointBoundary {
        return QosBackgroundThrottleDecision {
            action: QosBackgroundThrottleAction::Yield,
            foreground_pressure: true,
            adjusted_item_budget: Some(0),
            reason: "foreground_pressure_at_checkpoint".to_owned(),
        };
    }

    let floor = input.minimum_item_budget.min(input.remaining_item_budget);
    if input.remaining_item_budget > floor {
        let halved = input.remaining_item_budget / 2;
        let adjusted = halved.max(floor);
        return QosBackgroundThrottleDecision {
            action: QosBackgroundThrottleAction::ShrinkItemBudget,
            foreground_pressure: true,
            adjusted_item_budget: Some(adjusted),
            reason: "foreground_pressure_shrink_budget".to_owned(),
        };
    }

    continue_decision(true, "foreground_pressure_minimum_budget_reached")
}

impl QosLaneRecord {
    #[must_use]
    pub fn from_input(input: &QosLaneRecordInput<'_>) -> Self {
        let workspace_hash = redacted_hash("qos.workspace", input.workspace_identity);
        let request_hash = input
            .request_hash
            .and_then(non_empty)
            .map(ToOwned::to_owned)
            .or_else(|| {
                input
                    .request_text
                    .and_then(non_empty)
                    .map(|value| redacted_hash("qos.request", value))
            });
        let command_class = normalized_or(input.command_class, "unknown");
        let profile_label = input
            .profile_label
            .and_then(non_empty)
            .map(ToOwned::to_owned);
        let budget_label = input
            .budget_label
            .and_then(non_empty)
            .map(ToOwned::to_owned);
        let deadline_epoch_ms = input.started_at_epoch_ms.saturating_add(input.ttl_ms);
        let record_id = record_id(
            &workspace_hash,
            input.lane,
            &command_class,
            input.process_id,
            request_hash.as_deref(),
        );

        Self {
            schema: QOS_ACTIVE_LANE_RECORD_SCHEMA_V1.to_owned(),
            record_id,
            workspace_hash,
            lane: input.lane,
            command_class,
            process_id: input.process_id,
            profile_label,
            budget_label,
            request_hash,
            started_at_epoch_ms: input.started_at_epoch_ms,
            deadline_epoch_ms,
            ttl_ms: input.ttl_ms,
            status: input.status,
        }
    }

    #[must_use]
    pub const fn is_stale_at(&self, now_epoch_ms: u64) -> bool {
        self.deadline_epoch_ms <= now_epoch_ms
    }
}

impl QosActiveLaneRegistry {
    #[must_use]
    pub fn new(records: Vec<QosLaneRecord>) -> Self {
        let mut registry = Self {
            schema: QOS_ACTIVE_LANE_REGISTRY_SCHEMA_V1.to_owned(),
            records,
        };
        registry.normalize();
        registry
    }

    pub fn normalize(&mut self) {
        self.records.sort_by(compare_records);
        self.records
            .dedup_by(|left, right| left.record_id == right.record_id);
    }

    pub fn upsert(&mut self, record: QosLaneRecord) {
        self.records
            .retain(|existing| existing.record_id != record.record_id);
        self.records.push(record);
        self.normalize();
    }
}

#[must_use]
pub fn qos_registry_path(workspace: &Path) -> PathBuf {
    REGISTRY_RELATIVE_PATH
        .iter()
        .fold(workspace.to_path_buf(), |path, component| {
            path.join(component)
        })
}

pub fn publish_qos_lane_record(
    workspace: &Path,
    input: &QosLaneRecordInput<'_>,
) -> Result<QosLaneRecord, DomainError> {
    let record = QosLaneRecord::from_input(input);
    let path = qos_registry_path(workspace);
    let mut registry = read_registry_document(&path)?.unwrap_or_default();
    registry.upsert(record.clone());
    write_registry_document(&path, &registry)?;
    Ok(record)
}

pub fn summarize_qos_lane_registry(
    workspace: &Path,
    workspace_identity: &str,
    now_epoch_ms: u64,
) -> QosLaneSummary {
    let workspace_hash = redacted_hash("qos.workspace", workspace_identity);
    let path = qos_registry_path(workspace);
    match read_registry_document(&path) {
        Ok(Some(registry)) => summarize_qos_records(workspace_hash, registry.records, now_epoch_ms),
        Ok(None) => summarize_qos_records(workspace_hash, Vec::new(), now_epoch_ms),
        Err(error) => {
            let mut summary = summarize_qos_records(workspace_hash, Vec::new(), now_epoch_ms);
            summary
                .degraded
                .push(QosRegistryDegradation::registry_unavailable(
                    error.to_string(),
                ));
            summary
        }
    }
}

#[must_use]
pub fn summarize_qos_records(
    workspace_hash: String,
    records: Vec<QosLaneRecord>,
    now_epoch_ms: u64,
) -> QosLaneSummary {
    let mut stale_ignored_count = 0_u32;
    let mut active_records = Vec::new();
    for record in records {
        if record.is_stale_at(now_epoch_ms) {
            stale_ignored_count = stale_ignored_count.saturating_add(1);
        } else {
            active_records.push(record);
        }
    }
    active_records.sort_by(compare_records);

    let foreground_active_count = capped_count(
        active_records
            .iter()
            .filter(|record| record.lane.is_foreground())
            .count(),
    );
    let background_active_count = capped_count(
        active_records
            .iter()
            .filter(|record| record.lane == QosLane::BackgroundDerived)
            .count(),
    );
    let verification_active_count = capped_count(
        active_records
            .iter()
            .filter(|record| record.lane == QosLane::VerificationRch)
            .count(),
    );
    let maintenance_active_count = capped_count(
        active_records
            .iter()
            .filter(|record| record.lane == QosLane::MaintenanceSteward)
            .count(),
    );

    QosLaneSummary {
        schema: QOS_ACTIVE_LANE_SUMMARY_SCHEMA_V1.to_owned(),
        workspace_hash,
        active_records,
        foreground_active_count,
        background_active_count,
        verification_active_count,
        maintenance_active_count,
        stale_ignored_count,
        degraded: Vec::new(),
    }
}

fn read_registry_document(path: &Path) -> Result<Option<QosActiveLaneRegistry>, DomainError> {
    if let Some(symlink) = first_existing_symlink_component(path)? {
        return Err(DomainError::Storage {
            message: format!(
                "refusing to read QoS active-lane registry through symlink '{}'",
                symlink.display()
            ),
            repair: Some("replace the symlinked .ee/qos path with a real directory".to_owned()),
        });
    }

    match fs::read_to_string(path) {
        Ok(raw) => {
            let mut registry: QosActiveLaneRegistry =
                serde_json::from_str(&raw).map_err(|error| DomainError::Storage {
                    message: format!(
                        "failed to parse QoS active-lane registry '{}': {error}",
                        path.display()
                    ),
                    repair: Some("remove or repair malformed .ee/qos/active-lanes.json".to_owned()),
                })?;
            if registry.schema != QOS_ACTIVE_LANE_REGISTRY_SCHEMA_V1 {
                return Err(DomainError::Storage {
                    message: format!(
                        "unsupported QoS active-lane registry schema `{}`",
                        registry.schema
                    ),
                    repair: Some("regenerate the QoS active-lane registry".to_owned()),
                });
            }
            registry.normalize();
            Ok(Some(registry))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(DomainError::Storage {
            message: format!(
                "failed to read QoS active-lane registry '{}': {error}",
                path.display()
            ),
            repair: Some("check workspace .ee/qos permissions".to_owned()),
        }),
    }
}

fn write_registry_document(
    path: &Path,
    registry: &QosActiveLaneRegistry,
) -> Result<(), DomainError> {
    if let Some(symlink) = first_existing_symlink_component(path)? {
        return Err(DomainError::Storage {
            message: format!(
                "refusing to write QoS active-lane registry through symlink '{}'",
                symlink.display()
            ),
            repair: Some("replace the symlinked .ee/qos path with a real directory".to_owned()),
        });
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| DomainError::Storage {
            message: format!(
                "failed to create QoS active-lane registry directory '{}': {error}",
                parent.display()
            ),
            repair: Some("check workspace .ee/qos permissions".to_owned()),
        })?;
    }
    if let Some(symlink) = first_existing_symlink_component(path)? {
        return Err(DomainError::Storage {
            message: format!(
                "refusing to write QoS active-lane registry through symlink '{}'",
                symlink.display()
            ),
            repair: Some("replace the symlinked .ee/qos path with a real directory".to_owned()),
        });
    }

    let mut normalized = registry.clone();
    normalized.normalize();
    let json = serde_json::to_string_pretty(&normalized).map_err(|error| DomainError::Storage {
        message: format!("failed to serialize QoS active-lane registry: {error}"),
        repair: Some("report the QoS registry serialization failure".to_owned()),
    })?;
    let temp_path = path.with_extension("json.tmp");
    if let Some(symlink) = first_existing_symlink_component(&temp_path)? {
        return Err(DomainError::Storage {
            message: format!(
                "refusing to write QoS active-lane registry temp file through symlink '{}'",
                symlink.display()
            ),
            repair: Some("replace the symlinked .ee/qos temp path with a real file".to_owned()),
        });
    }
    fs::write(&temp_path, format!("{json}\n")).map_err(|error| DomainError::Storage {
        message: format!(
            "failed to write QoS active-lane registry temp file '{}': {error}",
            temp_path.display()
        ),
        repair: Some("check workspace .ee/qos permissions".to_owned()),
    })?;
    fs::rename(&temp_path, path).map_err(|error| DomainError::Storage {
        message: format!(
            "failed to publish QoS active-lane registry '{}': {error}",
            path.display()
        ),
        repair: Some("check workspace .ee/qos permissions and retry".to_owned()),
    })
}

fn first_existing_symlink_component(path: &Path) -> Result<Option<PathBuf>, DomainError> {
    let mut current = PathBuf::new();
    for component in path.components() {
        current.push(component.as_os_str());
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => return Ok(Some(current)),
            Ok(_) => {}
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
                ) =>
            {
                return Ok(None);
            }
            Err(error) => {
                return Err(DomainError::Storage {
                    message: format!(
                        "failed to inspect QoS registry path component '{}': {error}",
                        current.display()
                    ),
                    repair: Some("check workspace .ee/qos permissions".to_owned()),
                });
            }
        }
    }
    Ok(None)
}

fn compare_records(left: &QosLaneRecord, right: &QosLaneRecord) -> std::cmp::Ordering {
    left.lane
        .cmp(&right.lane)
        .then_with(|| left.command_class.cmp(&right.command_class))
        .then_with(|| left.started_at_epoch_ms.cmp(&right.started_at_epoch_ms))
        .then_with(|| left.record_id.cmp(&right.record_id))
}

fn record_id(
    workspace_hash: &str,
    lane: QosLane,
    command_class: &str,
    process_id: Option<u32>,
    request_hash: Option<&str>,
) -> String {
    let mut lines = Vec::with_capacity(5);
    lines.push(format!("workspaceHash={workspace_hash}"));
    lines.push(format!("lane={}", lane.as_str()));
    lines.push(format!("commandClass={command_class}"));
    lines.push(format!(
        "processId={}",
        process_id.map_or_else(|| "<none>".to_owned(), |value| value.to_string())
    ));
    lines.push(format!("requestHash={}", request_hash.unwrap_or("<none>")));
    redacted_hash("qos.active_lane.record", &lines.join("\n"))
}

fn redacted_hash(label: &str, value: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(label.as_bytes());
    hasher.update([0]);
    hasher.update(value.as_bytes());
    format!("sha256:{:x}", hasher.finalize())
}

fn non_empty(value: &str) -> Option<&str> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed)
    }
}

fn normalized_or(value: &str, fallback: &str) -> String {
    non_empty(value).unwrap_or(fallback).to_owned()
}

fn capped_count(count: usize) -> u32 {
    u32::try_from(count).unwrap_or(u32::MAX)
}

const fn is_background_work_lane(lane: QosLane) -> bool {
    matches!(
        lane,
        QosLane::BackgroundDerived | QosLane::MaintenanceSteward
    )
}

fn continue_decision(
    foreground_pressure: bool,
    reason: &'static str,
) -> QosBackgroundThrottleDecision {
    QosBackgroundThrottleDecision {
        action: QosBackgroundThrottleAction::Continue,
        foreground_pressure,
        adjusted_item_budget: None,
        reason: reason.to_owned(),
    }
}

impl Default for QosActiveLaneRegistry {
    fn default() -> Self {
        Self {
            schema: QOS_ACTIVE_LANE_REGISTRY_SCHEMA_V1.to_owned(),
            records: Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn record_input<'a>(
        lane: QosLane,
        command_class: &'a str,
        request_text: &'a str,
        started_at_epoch_ms: u64,
    ) -> QosLaneRecordInput<'a> {
        QosLaneRecordInput {
            workspace_identity: "/workspace/secret-project",
            lane,
            command_class,
            process_id: Some(42),
            profile_label: Some("balanced"),
            budget_label: Some("interactive"),
            request_text: Some(request_text),
            request_hash: None,
            started_at_epoch_ms,
            ttl_ms: 1_000,
            status: QosLaneStatus::Active,
        }
    }

    #[test]
    fn lane_record_serialization_is_redaction_safe() -> TestResult {
        let record = QosLaneRecord::from_input(&record_input(
            QosLane::ForegroundRead,
            "context",
            "raw query with secret token and /Users/name/project path",
            100,
        ));

        let json = serde_json::to_string_pretty(&record)?;
        assert!(!json.contains("raw query"));
        assert!(!json.contains("secret token"));
        assert!(!json.contains("/Users/name/project"));
        assert!(json.contains("\"requestHash\""));
        assert!(json.contains("\"workspaceHash\""));
        assert_eq!(record.deadline_epoch_ms, 1_100);
        Ok(())
    }

    #[test]
    fn registry_upsert_sorts_deduplicates_and_summarizes_ttl() -> TestResult {
        let foreground = QosLaneRecord::from_input(&record_input(
            QosLane::ForegroundRead,
            "context",
            "query a",
            100,
        ));
        let stale_background = QosLaneRecord::from_input(&record_input(
            QosLane::BackgroundDerived,
            "steward",
            "query b",
            1,
        ));
        let verification = QosLaneRecord::from_input(&record_input(
            QosLane::VerificationRch,
            "cargo-test",
            "query c",
            200,
        ));

        let mut registry = QosActiveLaneRegistry::default();
        registry.upsert(verification.clone());
        registry.upsert(stale_background);
        registry.upsert(foreground.clone());
        registry.upsert(foreground);

        assert_eq!(registry.records.len(), 3);
        let summary = summarize_qos_records("workspace-hash".to_owned(), registry.records, 1_050);
        assert_eq!(summary.active_records.len(), 2);
        assert_eq!(summary.foreground_active_count, 1);
        assert_eq!(summary.verification_active_count, 1);
        assert_eq!(summary.stale_ignored_count, 1);
        assert_eq!(summary.active_records[0].lane, QosLane::ForegroundRead);
        assert_eq!(summary.active_records[1].lane, QosLane::VerificationRch);
        Ok(())
    }

    #[test]
    fn throttle_decision_continues_without_foreground_pressure() -> TestResult {
        let background = QosLaneRecord::from_input(&record_input(
            QosLane::BackgroundDerived,
            "index",
            "background work",
            100,
        ));
        let summary = summarize_qos_records("workspace-hash".to_owned(), vec![background], 500);

        let decision = decide_background_throttle(
            &summary,
            QosBackgroundThrottleInput {
                lane: QosLane::BackgroundDerived,
                checkpoint: QosThrottleCheckpoint::BeforeExpensivePhase,
                remaining_item_budget: 100,
                minimum_item_budget: 10,
                may_yield: true,
            },
        );

        assert_eq!(decision.action, QosBackgroundThrottleAction::Continue);
        assert!(!decision.foreground_pressure);
        assert!(!decision.behavior_changed());
        assert_eq!(decision.reason, "no_foreground_pressure");
        Ok(())
    }

    #[test]
    fn throttle_decision_shrinks_background_budget_under_foreground_pressure() -> TestResult {
        let foreground = QosLaneRecord::from_input(&record_input(
            QosLane::ForegroundRead,
            "context",
            "foreground query",
            100,
        ));
        let background = QosLaneRecord::from_input(&record_input(
            QosLane::MaintenanceSteward,
            "steward",
            "maintenance work",
            120,
        ));
        let summary = summarize_qos_records(
            "workspace-hash".to_owned(),
            vec![foreground, background],
            500,
        );

        let decision = decide_background_throttle(
            &summary,
            QosBackgroundThrottleInput {
                lane: QosLane::MaintenanceSteward,
                checkpoint: QosThrottleCheckpoint::BeforeExpensivePhase,
                remaining_item_budget: 99,
                minimum_item_budget: 30,
                may_yield: false,
            },
        );

        assert_eq!(
            decision.action,
            QosBackgroundThrottleAction::ShrinkItemBudget
        );
        assert!(decision.foreground_pressure);
        assert_eq!(decision.adjusted_item_budget, Some(49));
        assert!(decision.behavior_changed());
        assert_eq!(decision.reason, "foreground_pressure_shrink_budget");
        Ok(())
    }

    #[test]
    fn throttle_decision_yields_at_checkpoint_when_allowed() -> TestResult {
        let foreground = QosLaneRecord::from_input(&record_input(
            QosLane::ForegroundWrite,
            "remember",
            "foreground write",
            100,
        ));
        let background = QosLaneRecord::from_input(&record_input(
            QosLane::BackgroundDerived,
            "graph-refresh",
            "derived work",
            120,
        ));
        let summary = summarize_qos_records(
            "workspace-hash".to_owned(),
            vec![foreground, background],
            500,
        );

        let decision = decide_background_throttle(
            &summary,
            QosBackgroundThrottleInput {
                lane: QosLane::BackgroundDerived,
                checkpoint: QosThrottleCheckpoint::CheckpointBoundary,
                remaining_item_budget: 100,
                minimum_item_budget: 20,
                may_yield: true,
            },
        );

        assert_eq!(decision.action, QosBackgroundThrottleAction::Yield);
        assert_eq!(decision.adjusted_item_budget, Some(0));
        assert_eq!(decision.reason, "foreground_pressure_at_checkpoint");
        Ok(())
    }

    #[test]
    fn throttle_decision_fails_open_when_registry_summary_is_degraded() -> TestResult {
        let foreground = QosLaneRecord::from_input(&record_input(
            QosLane::ForegroundRead,
            "context",
            "foreground query",
            100,
        ));
        let mut summary = summarize_qos_records("workspace-hash".to_owned(), vec![foreground], 500);
        summary
            .degraded
            .push(QosRegistryDegradation::registry_unavailable("synthetic"));

        let decision = decide_background_throttle(
            &summary,
            QosBackgroundThrottleInput {
                lane: QosLane::BackgroundDerived,
                checkpoint: QosThrottleCheckpoint::CheckpointBoundary,
                remaining_item_budget: 100,
                minimum_item_budget: 10,
                may_yield: true,
            },
        );

        assert_eq!(decision.action, QosBackgroundThrottleAction::Continue);
        assert!(decision.foreground_pressure);
        assert_eq!(decision.reason, "qos_summary_degraded_fail_open");
        Ok(())
    }

    #[test]
    fn qos_summary_json_aggregates_duplicate_degraded_entries() -> TestResult {
        let mut summary = summarize_qos_records("workspace-hash".to_owned(), Vec::new(), 500);
        summary.degraded = vec![
            QosRegistryDegradation {
                code: QOS_REGISTRY_UNAVAILABLE_CODE.to_owned(),
                severity: "low".to_owned(),
                message: "low duplicate".to_owned(),
                repair: "low repair".to_owned(),
            },
            QosRegistryDegradation {
                code: QOS_REGISTRY_UNAVAILABLE_CODE.to_owned(),
                severity: "high".to_owned(),
                message: "high duplicate".to_owned(),
                repair: "high repair".to_owned(),
            },
        ];

        let value = serde_json::to_value(&summary)?;
        let degraded = value
            .get("degraded")
            .and_then(serde_json::Value::as_array)
            .ok_or_else(|| format!("degraded array missing: {value}"))?;

        assert_eq!(degraded.len(), 1);
        assert_eq!(
            degraded[0]["code"].as_str(),
            Some(QOS_REGISTRY_UNAVAILABLE_CODE)
        );
        assert_eq!(degraded[0]["severity"].as_str(), Some("high"));
        assert_eq!(degraded[0]["repair"].as_str(), Some("high repair"));
        assert_eq!(
            degraded[0]["sources"].clone(),
            serde_json::json!(["qos_registry"])
        );
        Ok(())
    }

    #[test]
    fn publish_and_read_registry_round_trips_deterministically() -> TestResult {
        let tempdir = tempfile::tempdir()?;
        let workspace = tempdir.path();
        let first = record_input(QosLane::ForegroundRead, "context", "query a", 100);
        let second = record_input(QosLane::MaintenanceSteward, "steward", "query b", 200);

        publish_qos_lane_record(workspace, &second)?;
        publish_qos_lane_record(workspace, &first)?;

        let path = qos_registry_path(workspace);
        let first_json = fs::read_to_string(&path)?;
        let summary = summarize_qos_lane_registry(workspace, "/workspace/secret-project", 500);
        let second_json = fs::read_to_string(&path)?;

        assert_eq!(first_json, second_json);
        assert_eq!(summary.active_records.len(), 2);
        assert!(summary.degraded.is_empty());
        assert!(!first_json.contains("query a"));
        assert!(!first_json.contains("query b"));
        Ok(())
    }

    #[test]
    fn malformed_registry_reports_unavailable_degradation() -> TestResult {
        let tempdir = tempfile::tempdir()?;
        let workspace = tempdir.path();
        let path = qos_registry_path(workspace);
        fs::create_dir_all(path.parent().expect("registry parent"))?;
        fs::write(&path, "{not-json\n")?;

        let summary = summarize_qos_lane_registry(workspace, "/workspace/secret-project", 500);

        assert_eq!(summary.active_records.len(), 0);
        assert_eq!(summary.degraded.len(), 1);
        assert_eq!(summary.degraded[0].code, QOS_REGISTRY_UNAVAILABLE_CODE);
        assert_eq!(summary.degraded[0].severity, "medium");
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn registry_read_rejects_symlinked_registry_file() -> TestResult {
        use std::os::unix::fs::symlink;

        let tempdir = tempfile::tempdir()?;
        let workspace = tempdir.path().join("workspace");
        let outside = tempdir.path().join("outside-active-lanes.json");
        let path = qos_registry_path(&workspace);
        fs::create_dir_all(path.parent().expect("registry parent"))?;
        let registry = QosActiveLaneRegistry::new(vec![QosLaneRecord::from_input(&record_input(
            QosLane::ForegroundRead,
            "context",
            "outside query",
            100,
        ))]);
        fs::write(&outside, serde_json::to_string_pretty(&registry)?)?;
        symlink(&outside, &path)?;

        let summary = summarize_qos_lane_registry(&workspace, "/workspace/secret-project", 500);

        assert!(summary.active_records.is_empty());
        assert_eq!(summary.degraded.len(), 1);
        assert_eq!(summary.degraded[0].code, QOS_REGISTRY_UNAVAILABLE_CODE);
        assert!(summary.degraded[0].message.contains("symlink"));
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn registry_read_rejects_symlinked_registry_parent() -> TestResult {
        use std::os::unix::fs::symlink;

        let tempdir = tempfile::tempdir()?;
        let workspace = tempdir.path().join("workspace");
        let ee_dir = workspace.join(".ee");
        let outside_qos = tempdir.path().join("outside-qos");
        fs::create_dir_all(&ee_dir)?;
        fs::create_dir_all(&outside_qos)?;
        let registry = QosActiveLaneRegistry::new(vec![QosLaneRecord::from_input(&record_input(
            QosLane::VerificationRch,
            "cargo-test",
            "outside query",
            100,
        ))]);
        fs::write(
            outside_qos.join("active-lanes.json"),
            serde_json::to_string_pretty(&registry)?,
        )?;
        symlink(&outside_qos, ee_dir.join("qos"))?;

        let summary = summarize_qos_lane_registry(&workspace, "/workspace/secret-project", 500);

        assert!(summary.active_records.is_empty());
        assert_eq!(summary.degraded.len(), 1);
        assert_eq!(summary.degraded[0].code, QOS_REGISTRY_UNAVAILABLE_CODE);
        assert!(summary.degraded[0].message.contains("symlink"));
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn registry_write_rejects_symlinked_temp_file() -> TestResult {
        use std::os::unix::fs::symlink;

        let tempdir = tempfile::tempdir()?;
        let workspace = tempdir.path().join("workspace");
        let path = qos_registry_path(&workspace);
        let outside = tempdir.path().join("outside-temp-target.json");
        fs::create_dir_all(path.parent().expect("registry parent"))?;
        fs::write(&outside, "outside sentinel")?;
        symlink(&outside, path.with_extension("json.tmp"))?;

        let error = publish_qos_lane_record(
            &workspace,
            &record_input(QosLane::VerificationRch, "cargo-test", "query", 100),
        )
        .expect_err("symlinked temp file should reject registry write");

        assert!(
            error.message().contains("temp file through symlink"),
            "unexpected symlink temp error: {}",
            error.message()
        );
        assert_eq!(fs::read_to_string(&outside)?, "outside sentinel");
        Ok(())
    }

    #[test]
    fn registry_readers_never_require_stale_record_deletion() -> TestResult {
        let stale = QosLaneRecord::from_input(&record_input(
            QosLane::ForegroundWrite,
            "remember",
            "query stale",
            1,
        ));
        let current = QosLaneRecord::from_input(&record_input(
            QosLane::BackgroundDerived,
            "index",
            "query current",
            10_000,
        ));

        let summary =
            summarize_qos_records("workspace-hash".to_owned(), vec![stale, current], 5000);

        assert_eq!(summary.active_records.len(), 1);
        assert_eq!(summary.background_active_count, 1);
        assert_eq!(summary.stale_ignored_count, 1);
        Ok(())
    }
}
