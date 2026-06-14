//! Opt-in session-budget ledger recording.
//!
//! The recorder is deliberately inert unless a caller constructs the enabled
//! variant. That keeps the ordinary command path free of estimator, filesystem,
//! and retention work while still giving bd-1clqr.3 a real bounded ledger to
//! consume.

use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::num::{NonZeroU32, NonZeroUsize};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Duration as ChronoDuration, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::models::SESSION_BUDGET_SCHEMA_V1;

pub const SESSION_BUDGET_REDACTION_STATUS: &str = "paths_counts_hashes_no_content";
pub const SESSION_BUDGET_PATH_POLICY: &str = "workspace_relative_or_hashed";

#[derive(Clone, Debug)]
pub enum SessionBudgetRecorder {
    Disabled,
    Enabled(SessionBudgetRecorderConfig),
}

impl SessionBudgetRecorder {
    #[must_use]
    pub const fn disabled() -> Self {
        Self::Disabled
    }

    #[must_use]
    pub fn enabled(config: SessionBudgetRecorderConfig) -> Self {
        Self::Enabled(config)
    }

    pub fn record_with<F>(
        &self,
        estimate: F,
    ) -> Result<SessionBudgetRecordOutcome, SessionBudgetRecordError>
    where
        F: FnOnce() -> Result<SessionBudgetObservation, SessionBudgetRecordError>,
    {
        match self {
            Self::Disabled => Ok(SessionBudgetRecordOutcome::disabled()),
            Self::Enabled(config) => {
                let observation = estimate()?;
                record_enabled(config, observation)
            }
        }
    }
}

#[derive(Clone, Debug)]
pub struct SessionBudgetRecorderConfig {
    pub ledger_path: PathBuf,
    pub max_rows_per_workspace: NonZeroUsize,
    pub max_age_days: NonZeroU32,
    pub opt_in_source: SessionBudgetOptInSource,
    pub sampling_rate: f64,
}

impl SessionBudgetRecorderConfig {
    pub fn new(
        ledger_path: impl Into<PathBuf>,
        max_rows_per_workspace: NonZeroUsize,
        max_age_days: NonZeroU32,
        opt_in_source: SessionBudgetOptInSource,
        sampling_rate: f64,
    ) -> Result<Self, SessionBudgetRecordError> {
        if !(0.0..=1.0).contains(&sampling_rate) || !sampling_rate.is_finite() {
            return Err(SessionBudgetRecordError::invalid_config(
                "session budget sampling_rate must be finite and within 0.0..=1.0",
            ));
        }
        Ok(Self {
            ledger_path: ledger_path.into(),
            max_rows_per_workspace,
            max_age_days,
            opt_in_source,
            sampling_rate,
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SessionBudgetRecordStatus {
    Disabled,
    Recorded,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionBudgetRecordOutcome {
    pub status: SessionBudgetRecordStatus,
    pub ledger_path: Option<PathBuf>,
    pub event_id: Option<String>,
    pub rows_before: usize,
    pub rows_after: usize,
    pub evicted_rows: u64,
}

impl SessionBudgetRecordOutcome {
    #[must_use]
    pub const fn disabled() -> Self {
        Self {
            status: SessionBudgetRecordStatus::Disabled,
            ledger_path: None,
            event_id: None,
            rows_before: 0,
            rows_after: 0,
            evicted_rows: 0,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum SessionBudgetOptInSource {
    CliFlag,
    Env,
    Config,
    TestFixture,
}

#[derive(Clone, Debug, Serialize, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum SessionBudgetCommandSurface {
    Primer,
    Recall,
    Search,
    Pack,
    Ask,
    SwarmBrief,
    WorkPacket,
    AgentMailCoordination,
    VerificationProof,
    ProofWait,
    Other,
}

#[derive(Clone, Debug, Serialize, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum SessionBudgetCommandClass {
    ReadOnly,
    DurableWrite,
    DerivedAsset,
    Coordination,
    Verification,
    Planning,
}

#[derive(Clone, Debug, Serialize, Deserialize, Eq, PartialEq)]
pub enum SessionBudgetNormalizedCommand {
    #[serde(rename = "ee primer")]
    EePrimer,
    #[serde(rename = "ee recall")]
    EeRecall,
    #[serde(rename = "ee search")]
    EeSearch,
    #[serde(rename = "ee pack")]
    EePack,
    #[serde(rename = "ee ask")]
    EeAsk,
    #[serde(rename = "ee swarm brief")]
    EeSwarmBrief,
    #[serde(rename = "ee swarm work-packet")]
    EeSwarmWorkPacket,
    #[serde(rename = "agent_mail snapshot")]
    AgentMailSnapshot,
    #[serde(rename = "rch cargo verification")]
    RchCargoVerification,
    #[serde(rename = "proof wait")]
    ProofWait,
    #[serde(rename = "other")]
    Other,
}

#[derive(Clone, Debug, Serialize, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum SessionBudgetDegradedSource {
    Output,
    Pack,
    Rch,
    Db,
    DerivedAsset,
    AgentMail,
    Beads,
    Bv,
    Memory,
    Unknown,
}

#[derive(Clone, Debug, Serialize, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum SessionBudgetSeverity {
    Info,
    Low,
    Warning,
    Medium,
    High,
    Critical,
}

#[derive(Clone, Debug, Serialize, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum SessionBudgetStaleSource {
    SearchIndex,
    GraphSnapshot,
    CassImport,
    PackCache,
    None,
}

#[derive(Clone, Debug, Serialize, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum SessionBudgetEvidenceKind {
    ResponseEnvelope,
    RchQueue,
    AgentMailSnapshot,
    BeadsRow,
    Timer,
    None,
}

#[derive(Clone, Debug, Serialize, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SessionBudgetCorrelation {
    pub session_id: String,
    pub command_id: String,
    pub parent_command_id: Option<String>,
    pub task_hash: String,
    pub pack_id: Option<String>,
    pub rch_job_id: Option<String>,
    pub agent_mail_thread_id: Option<String>,
    pub bead_id: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SessionBudgetCommand {
    pub surface: SessionBudgetCommandSurface,
    pub command_class: SessionBudgetCommandClass,
    pub read_only: bool,
    pub durable_mutation: bool,
    pub normalized_command: SessionBudgetNormalizedCommand,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SessionBudgetRchCost {
    pub slots_requested: u64,
    pub slots_used: u64,
    pub blocked_ms: u64,
    pub queue_depth: Option<u64>,
    pub workers_healthy: Option<u64>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SessionBudgetDbCost {
    pub lock_wait_ms: u64,
    pub read_pool_acquire_ms: u64,
    pub write_attempt_count: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SessionBudgetDerivedAssetCost {
    pub freshness_penalty_ms: u64,
    pub stale_sources: Vec<SessionBudgetStaleSource>,
}

impl Default for SessionBudgetDerivedAssetCost {
    fn default() -> Self {
        Self {
            freshness_penalty_ms: 0,
            stale_sources: vec![SessionBudgetStaleSource::None],
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SessionBudgetCost {
    pub wall_clock_ms: u64,
    pub output_tokens_estimated: u64,
    pub output_tokens_returned: u64,
    pub output_bytes: u64,
    pub pack_tokens_requested: u64,
    pub pack_tokens_used: u64,
    pub rch: SessionBudgetRchCost,
    pub db: SessionBudgetDbCost,
    pub derived_assets: SessionBudgetDerivedAssetCost,
}

#[derive(Clone, Debug, Serialize, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SessionBudgetDegradedGroup {
    pub code: String,
    pub source: SessionBudgetDegradedSource,
    pub severity: SessionBudgetSeverity,
    pub count: NonZeroU32,
}

#[derive(Clone, Debug, Serialize, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SessionBudgetEvidenceRef {
    pub kind: SessionBudgetEvidenceKind,
    pub r#ref: Option<String>,
    pub hash: Option<String>,
}

#[derive(Clone, Debug, Serialize, Eq, PartialEq)]
pub struct SessionBudgetPrivacy {
    #[serde(rename = "redactionStatus")]
    pub redaction_status: &'static str,
    #[serde(rename = "rawCommandStored")]
    pub raw_command_stored: bool,
    #[serde(rename = "rawOutputStored")]
    pub raw_output_stored: bool,
    #[serde(rename = "contentStored")]
    pub content_stored: bool,
    #[serde(rename = "pathPolicy")]
    pub path_policy: &'static str,
}

impl Default for SessionBudgetPrivacy {
    fn default() -> Self {
        Self {
            redaction_status: SESSION_BUDGET_REDACTION_STATUS,
            raw_command_stored: false,
            raw_output_stored: false,
            content_stored: false,
            path_policy: SESSION_BUDGET_PATH_POLICY,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SessionBudgetRetentionSnapshot {
    pub max_rows_per_workspace: usize,
    pub max_age_days: u32,
    pub evicted_rows: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SessionBudgetObservation {
    pub recorded_at: DateTime<Utc>,
    pub workspace_fingerprint: String,
    pub correlation: SessionBudgetCorrelation,
    pub command: SessionBudgetCommand,
    pub cost: SessionBudgetCost,
    pub degraded_groups: Vec<SessionBudgetDegradedGroup>,
    pub evidence: Vec<SessionBudgetEvidenceRef>,
}

#[derive(Clone, Debug, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SessionBudgetLedgerRow {
    pub schema: &'static str,
    pub event_id: String,
    pub recorded_at: DateTime<Utc>,
    pub workspace_fingerprint: String,
    pub opt_in: SessionBudgetOptIn,
    pub correlation: SessionBudgetCorrelation,
    pub command: SessionBudgetCommand,
    pub cost: SessionBudgetCost,
    pub degraded_groups: Vec<SessionBudgetDegradedGroup>,
    pub privacy: SessionBudgetPrivacy,
    pub retention: SessionBudgetRetentionSnapshot,
    pub evidence: Vec<SessionBudgetEvidenceRef>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SessionBudgetOptIn {
    pub enabled: bool,
    pub source: SessionBudgetOptInSource,
    pub sampling_rate: f64,
}

pub fn session_budget_hash(bytes: impl AsRef<[u8]>) -> String {
    format!("blake3:{}", blake3::hash(bytes.as_ref()).to_hex())
}

fn record_enabled(
    config: &SessionBudgetRecorderConfig,
    observation: SessionBudgetObservation,
) -> Result<SessionBudgetRecordOutcome, SessionBudgetRecordError> {
    let mut rows = load_ledger_rows(&config.ledger_path)?;
    let rows_before = rows.len();
    let evicted_rows = apply_retention(&mut rows, observation.recorded_at, config);
    let row = SessionBudgetLedgerRow::from_observation(config, observation, evicted_rows);
    let event_id = row.event_id.clone();
    rows.push(serde_json::to_value(row).map_err(SessionBudgetRecordError::json_value)?);
    write_ledger_rows(&config.ledger_path, &rows)?;

    Ok(SessionBudgetRecordOutcome {
        status: SessionBudgetRecordStatus::Recorded,
        ledger_path: Some(config.ledger_path.clone()),
        event_id: Some(event_id),
        rows_before,
        rows_after: rows.len(),
        evicted_rows,
    })
}

impl SessionBudgetLedgerRow {
    fn from_observation(
        config: &SessionBudgetRecorderConfig,
        observation: SessionBudgetObservation,
        evicted_rows: u64,
    ) -> Self {
        let event_id = event_id_for(&observation);
        Self {
            schema: SESSION_BUDGET_SCHEMA_V1,
            event_id,
            recorded_at: observation.recorded_at,
            workspace_fingerprint: observation.workspace_fingerprint,
            opt_in: SessionBudgetOptIn {
                enabled: true,
                source: config.opt_in_source.clone(),
                sampling_rate: config.sampling_rate,
            },
            correlation: observation.correlation,
            command: observation.command,
            cost: observation.cost,
            degraded_groups: observation.degraded_groups,
            privacy: SessionBudgetPrivacy::default(),
            retention: SessionBudgetRetentionSnapshot {
                max_rows_per_workspace: config.max_rows_per_workspace.get(),
                max_age_days: config.max_age_days.get(),
                evicted_rows,
            },
            evidence: observation.evidence,
        }
    }
}

fn event_id_for(observation: &SessionBudgetObservation) -> String {
    let seed = format!(
        "{}\n{}\n{}\n{}\n{}",
        observation.recorded_at.to_rfc3339(),
        observation.workspace_fingerprint,
        observation.correlation.session_id,
        observation.correlation.command_id,
        observation.correlation.task_hash
    );
    let hash = blake3::hash(seed.as_bytes()).to_hex().to_string();
    format!("sbud_{}", &hash[..24])
}

fn load_ledger_rows(path: &Path) -> Result<Vec<Value>, SessionBudgetRecordError> {
    let content = match fs::read_to_string(path) {
        Ok(content) => content,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(SessionBudgetRecordError::io(path, error)),
    };
    let mut rows = Vec::new();
    for (index, line) in content.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let value = serde_json::from_str::<Value>(line).map_err(|source| {
            SessionBudgetRecordError::json_line(path, index.saturating_add(1), source)
        })?;
        rows.push(value);
    }
    Ok(rows)
}

fn apply_retention(
    rows: &mut Vec<Value>,
    recorded_at: DateTime<Utc>,
    config: &SessionBudgetRecorderConfig,
) -> u64 {
    let cutoff = recorded_at - ChronoDuration::days(i64::from(config.max_age_days.get()));
    let before_age = rows.len();
    rows.retain(|row| {
        row.get("recordedAt")
            .and_then(Value::as_str)
            .and_then(|timestamp| DateTime::parse_from_rfc3339(timestamp).ok())
            .map(|timestamp| timestamp.with_timezone(&Utc) >= cutoff)
            .unwrap_or(false)
    });

    let max_existing = config.max_rows_per_workspace.get().saturating_sub(1);
    let mut evicted = before_age.saturating_sub(rows.len());
    if rows.len() > max_existing {
        let overflow = rows.len().saturating_sub(max_existing);
        rows.drain(0..overflow);
        evicted = evicted.saturating_add(overflow);
    }
    u64::try_from(evicted).unwrap_or(u64::MAX)
}

fn write_ledger_rows(path: &Path, rows: &[Value]) -> Result<(), SessionBudgetRecordError> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent)
            .map_err(|source| SessionBudgetRecordError::io(parent, source))?;
    }
    let mut file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(path)
        .map_err(|source| SessionBudgetRecordError::io(path, source))?;
    for row in rows {
        serde_json::to_writer(&mut file, row).map_err(SessionBudgetRecordError::json_value)?;
        file.write_all(b"\n")
            .map_err(|source| SessionBudgetRecordError::io(path, source))?;
    }
    file.flush()
        .map_err(|source| SessionBudgetRecordError::io(path, source))
}

#[derive(Debug)]
pub struct SessionBudgetRecordError {
    message: String,
}

impl SessionBudgetRecordError {
    fn invalid_config(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    fn io(path: &Path, source: std::io::Error) -> Self {
        Self {
            message: format!(
                "session budget ledger I/O failed at {}: {source}",
                path.display()
            ),
        }
    }

    fn json_line(path: &Path, line: usize, source: serde_json::Error) -> Self {
        Self {
            message: format!(
                "session budget ledger JSON parse failed at {}:{line}: {source}",
                path.display()
            ),
        }
    }

    fn json_value(source: serde_json::Error) -> Self {
        Self {
            message: format!("session budget ledger JSON serialization failed: {source}"),
        }
    }
}

impl fmt::Display for SessionBudgetRecordError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for SessionBudgetRecordError {}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::num::{NonZeroU32, NonZeroUsize};
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    use chrono::{TimeZone, Utc};
    use serde_json::Value;

    use super::*;

    static NEXT_TEST_ID: AtomicU64 = AtomicU64::new(1);

    type TestResult = Result<(), String>;

    fn test_ledger_path(name: &str) -> PathBuf {
        let id = NEXT_TEST_ID.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "ee-session-budget-{name}-{}-{id}.jsonl",
            std::process::id()
        ))
    }

    fn test_config(
        path: PathBuf,
        max_rows: usize,
        max_age_days: u32,
    ) -> SessionBudgetRecorderConfig {
        SessionBudgetRecorderConfig::new(
            path,
            NonZeroUsize::new(max_rows).expect("max rows"),
            NonZeroU32::new(max_age_days).expect("max age days"),
            SessionBudgetOptInSource::TestFixture,
            1.0,
        )
        .expect("valid config")
    }

    fn observation(sequence: u32, recorded_at: DateTime<Utc>) -> SessionBudgetObservation {
        SessionBudgetObservation {
            recorded_at,
            workspace_fingerprint: "a1b2c3d4e5f6".to_owned(),
            correlation: SessionBudgetCorrelation {
                session_id: "sess_session_budget_unit".to_owned(),
                command_id: format!("cmd_session_budget_{sequence:04}"),
                parent_command_id: None,
                task_hash: session_budget_hash(format!("task-{sequence}")),
                pack_id: None,
                rch_job_id: None,
                agent_mail_thread_id: Some("bd-1clqr.2".to_owned()),
                bead_id: Some("bd-1clqr.2".to_owned()),
            },
            command: SessionBudgetCommand {
                surface: SessionBudgetCommandSurface::Recall,
                command_class: SessionBudgetCommandClass::ReadOnly,
                read_only: true,
                durable_mutation: false,
                normalized_command: SessionBudgetNormalizedCommand::EeRecall,
            },
            cost: SessionBudgetCost {
                wall_clock_ms: u64::from(sequence) * 10,
                output_tokens_estimated: 12,
                output_tokens_returned: 10,
                output_bytes: 128,
                pack_tokens_requested: 0,
                pack_tokens_used: 0,
                rch: SessionBudgetRchCost::default(),
                db: SessionBudgetDbCost {
                    lock_wait_ms: 0,
                    read_pool_acquire_ms: 0,
                    write_attempt_count: 1,
                },
                derived_assets: SessionBudgetDerivedAssetCost::default(),
            },
            degraded_groups: Vec::new(),
            evidence: vec![SessionBudgetEvidenceRef {
                kind: SessionBudgetEvidenceKind::Timer,
                r#ref: Some(format!("timer-{sequence}")),
                hash: Some(session_budget_hash(format!("timer-{sequence}"))),
            }],
        }
    }

    fn read_rows(path: &Path) -> Result<Vec<Value>, String> {
        let content = fs::read_to_string(path).map_err(|error| error.to_string())?;
        content
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).map_err(|error| error.to_string()))
            .collect()
    }

    #[test]
    fn disabled_recorder_skips_estimator_and_ledger_work() -> TestResult {
        let path = test_ledger_path("disabled");
        let recorder = SessionBudgetRecorder::disabled();
        let mut estimator_called = false;

        let outcome = recorder
            .record_with(|| {
                estimator_called = true;
                Ok(observation(
                    1,
                    Utc.with_ymd_and_hms(2026, 6, 14, 12, 0, 0).unwrap(),
                ))
            })
            .map_err(|error| error.to_string())?;

        assert_eq!(outcome, SessionBudgetRecordOutcome::disabled());
        assert!(!estimator_called, "disabled recorder must not estimate");
        assert!(
            !path.exists(),
            "disabled recorder must not touch ledger path"
        );
        Ok(())
    }

    #[test]
    fn enabled_recorder_writes_schema_shaped_bounded_rows() -> TestResult {
        let path = test_ledger_path("enabled");
        let config = test_config(path.clone(), 2, 30);
        let recorder = SessionBudgetRecorder::enabled(config);
        let base = Utc.with_ymd_and_hms(2026, 6, 14, 12, 0, 0).unwrap();

        let first = recorder
            .record_with(|| Ok(observation(1, base)))
            .map_err(|error| error.to_string())?;
        let second = recorder
            .record_with(|| Ok(observation(2, base + ChronoDuration::seconds(1))))
            .map_err(|error| error.to_string())?;
        let third = recorder
            .record_with(|| Ok(observation(3, base + ChronoDuration::seconds(2))))
            .map_err(|error| error.to_string())?;

        assert_eq!(first.rows_after, 1);
        assert_eq!(second.rows_after, 2);
        assert_eq!(third.rows_after, 2);
        assert_eq!(third.evicted_rows, 1);

        let rows = read_rows(&path)?;
        assert_eq!(rows.len(), 2, "retention must cap rows");
        assert_eq!(
            rows[0]["correlation"]["commandId"],
            "cmd_session_budget_0002"
        );
        assert_eq!(
            rows[1]["correlation"]["commandId"],
            "cmd_session_budget_0003"
        );
        assert_eq!(rows[1]["schema"], SESSION_BUDGET_SCHEMA_V1);
        assert_eq!(
            rows[1]["privacy"]["redactionStatus"],
            SESSION_BUDGET_REDACTION_STATUS
        );
        assert_eq!(rows[1]["privacy"]["rawCommandStored"], false);
        assert_eq!(rows[1]["privacy"]["rawOutputStored"], false);
        assert_eq!(rows[1]["privacy"]["contentStored"], false);
        assert_eq!(rows[1]["retention"]["maxRowsPerWorkspace"], 2);
        assert_eq!(rows[1]["retention"]["evictedRows"], 1);
        Ok(())
    }

    #[test]
    fn retention_prunes_expired_rows_before_append() -> TestResult {
        let path = test_ledger_path("age");
        let config = test_config(path.clone(), 8, 1);
        let recorder = SessionBudgetRecorder::enabled(config);
        let old = Utc.with_ymd_and_hms(2026, 6, 10, 12, 0, 0).unwrap();
        let fresh = Utc.with_ymd_and_hms(2026, 6, 14, 12, 0, 0).unwrap();

        recorder
            .record_with(|| Ok(observation(1, old)))
            .map_err(|error| error.to_string())?;
        let outcome = recorder
            .record_with(|| Ok(observation(2, fresh)))
            .map_err(|error| error.to_string())?;

        assert_eq!(outcome.evicted_rows, 1);
        let rows = read_rows(&path)?;
        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0]["correlation"]["commandId"],
            "cmd_session_budget_0002"
        );
        assert_eq!(rows[0]["retention"]["maxAgeDays"], 1);
        Ok(())
    }
}
