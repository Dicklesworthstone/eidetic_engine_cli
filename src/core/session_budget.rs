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

// ── Planner (bd-1clqr.3) ───────────────────────────────────────────────────

pub const SESSION_BUDGET_PLAN_SCHEMA_V1: &str = "ee.session_budget.plan.v1";

const CARGO_REFUSAL_REASON: &str = "local cargo is structurally forbidden; \
    use `scripts/rch_verify.sh --skip-known-blocker -- cargo test <target>` for verification";
const CARGO_REFUSAL_ALTERNATIVE: &str =
    "scripts/rch_verify.sh --skip-known-blocker -- cargo test --lib";

const DEGRADED_PENALTY_MS: u64 = 10_000;
const PLAN_MAX_FALLBACKS: usize = 3;
const PROOF_POSTURE_ADVISORY_COST_MS: u64 = 175;

/// One scored command the planner might recommend.
#[derive(Clone, Debug, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct BudgetPlanEntry {
    pub rank: u32,
    pub surface: String,
    pub command: String,
    pub rationale: String,
    pub estimated_cost_ms: u64,
    pub estimated_output_tokens: u64,
    pub degraded_penalty: bool,
}

/// A refused input with explanation and alternative.
#[derive(Clone, Debug, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct BudgetPlanRefusal {
    pub input: String,
    pub reason: String,
    pub alternative: Option<String>,
}

/// Summary of ledger history surfaced alongside the plan.
#[derive(Clone, Debug, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct BudgetLedgerSummary {
    pub row_count: usize,
    pub total_wall_clock_ms: u64,
    pub most_recent_surface: Option<String>,
    pub degraded_event_count: u64,
}

/// The advisory plan emitted by `ee session-budget plan`.
#[derive(Clone, Debug, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct BudgetPlan {
    pub schema: &'static str,
    pub generated_at: DateTime<Utc>,
    pub workspace_fingerprint: String,
    pub advisory: bool,
    pub task_hint: Option<String>,
    pub recommendation: BudgetPlanEntry,
    pub fallbacks: Vec<BudgetPlanEntry>,
    pub refusals: Vec<BudgetPlanRefusal>,
    pub ledger_summary: BudgetLedgerSummary,
}

/// Input to `plan_cheapest_next_command`.
#[derive(Clone, Debug)]
pub struct BudgetPlannerInput {
    pub ledger_path: Option<PathBuf>,
    /// Names of currently degraded sources: "db", "rch", "agent_mail", "pack", "bv", "beads".
    pub degraded_sources: Vec<String>,
    /// Whether RCH is healthy (true = active verifications may be in progress).
    pub rch_healthy: bool,
    /// Free-text task hint from the caller (used for cargo refusal check).
    pub task_hint: Option<String>,
    pub workspace_fingerprint: String,
    pub generated_at: DateTime<Utc>,
}

// Internal row; never serialised.
struct CandidateRow {
    surface: &'static str,
    command: &'static str,
    base_cost_ms: u64,
    base_tokens: u64,
    rationale_clean: &'static str,
    rationale_degraded: &'static str,
    /// Source names that, if degraded, trigger a penalty.
    penalised_by: &'static [&'static str],
    /// true = only emit this row when rch_healthy is true.
    only_when_rch_healthy: bool,
    /// true = only emit this row when rch_healthy is false.
    only_when_rch_unhealthy: bool,
}

const ALL_CANDIDATES: &[CandidateRow] = &[
    CandidateRow {
        surface: "primer",
        command: "ee primer --json",
        base_cost_ms: 50,
        base_tokens: 2000,
        rationale_clean: "cheapest read-only command; establishes workspace context with minimal token cost",
        rationale_degraded: "db is degraded; primer may return cached or partial output",
        penalised_by: &["db"],
        only_when_rch_healthy: false,
        only_when_rch_unhealthy: false,
    },
    CandidateRow {
        surface: "recall",
        command: "ee recall --json",
        base_cost_ms: 100,
        base_tokens: 500,
        rationale_clean: "fast code-anchored reverse lookup; low token overhead for targeted queries",
        rationale_degraded: "db is degraded; recall may return stale or partial results",
        penalised_by: &["db"],
        only_when_rch_healthy: false,
        only_when_rch_unhealthy: false,
    },
    CandidateRow {
        surface: "ask",
        command: "ee ask --json",
        base_cost_ms: 150,
        base_tokens: 300,
        rationale_clean: "deterministic extractive QA with citations; no generation cost",
        rationale_degraded: "db is degraded; ask span retrieval may miss recent memories",
        penalised_by: &["db"],
        only_when_rch_healthy: false,
        only_when_rch_unhealthy: false,
    },
    CandidateRow {
        surface: "search",
        command: "ee search --json",
        base_cost_ms: 200,
        base_tokens: 1000,
        rationale_clean: "hybrid BM25+vector search; moderate cost for broad discovery",
        rationale_degraded: "db is degraded; search index may be stale or unavailable",
        penalised_by: &["db"],
        only_when_rch_healthy: false,
        only_when_rch_unhealthy: false,
    },
    CandidateRow {
        surface: "swarm-brief",
        command: "ee swarm brief --json",
        base_cost_ms: 300,
        base_tokens: 1500,
        rationale_clean: "coordination snapshot; shows peer state, RCH posture, and bead queue",
        rationale_degraded: "agent_mail or rch is degraded; swarm brief will have reduced signal",
        penalised_by: &["agent_mail", "rch"],
        only_when_rch_healthy: false,
        only_when_rch_unhealthy: false,
    },
    CandidateRow {
        surface: "pack",
        command: "ee pack --json",
        base_cost_ms: 500,
        base_tokens: 4000,
        rationale_clean: "full context pack assembly; highest token yield but highest cost",
        rationale_degraded: "db or pack source is degraded; pack may be incomplete",
        penalised_by: &["db", "pack"],
        only_when_rch_healthy: false,
        only_when_rch_unhealthy: false,
    },
    CandidateRow {
        surface: "proof-wait",
        command: "# wait for active RCH verification to complete before proceeding",
        base_cost_ms: PROOF_POSTURE_ADVISORY_COST_MS,
        base_tokens: 0,
        rationale_clean: "RCH is healthy; waiting for verification avoids retrying on a broken build",
        rationale_degraded: "",
        penalised_by: &[],
        only_when_rch_healthy: true,
        only_when_rch_unhealthy: false,
    },
    CandidateRow {
        surface: "proof-skip",
        command: "# skip RCH verification this round; proceed with cheaper read-only commands",
        base_cost_ms: PROOF_POSTURE_ADVISORY_COST_MS,
        base_tokens: 0,
        rationale_clean: "RCH is degraded; skipping verification prevents indefinite queue wait",
        rationale_degraded: "",
        penalised_by: &[],
        only_when_rch_healthy: false,
        only_when_rch_unhealthy: true,
    },
];

/// Produce an advisory, deterministic, explainable plan for the cheapest useful
/// next command given the current ledger and degraded-source posture.
///
/// This function is pure: it never writes to disk or opens network connections.
pub fn plan_cheapest_next_command(input: &BudgetPlannerInput) -> BudgetPlan {
    let ledger_summary = summarize_ledger(input.ledger_path.as_deref());
    let refusals = collect_cargo_refusals(input);
    let mut entries = score_all_candidates(input);

    // Sort by effective cost ascending, then by surface name for determinism.
    entries.sort_by(|a, b| {
        a.estimated_cost_ms
            .cmp(&b.estimated_cost_ms)
            .then_with(|| a.surface.cmp(&b.surface))
    });

    // Assign final ranks (1-based).
    for (i, entry) in entries.iter_mut().enumerate() {
        entry.rank = u32::try_from(i + 1).unwrap_or(u32::MAX);
    }

    let mut iter = entries.into_iter();
    let recommendation = iter.next().expect("always at least one candidate");
    let fallbacks = iter.take(PLAN_MAX_FALLBACKS).collect();

    BudgetPlan {
        schema: SESSION_BUDGET_PLAN_SCHEMA_V1,
        generated_at: input.generated_at,
        workspace_fingerprint: input.workspace_fingerprint.clone(),
        advisory: true,
        task_hint: input.task_hint.clone(),
        recommendation,
        fallbacks,
        refusals,
        ledger_summary,
    }
}

fn score_all_candidates(input: &BudgetPlannerInput) -> Vec<BudgetPlanEntry> {
    let mut entries = Vec::with_capacity(ALL_CANDIDATES.len());
    for row in ALL_CANDIDATES {
        if row.only_when_rch_healthy && !input.rch_healthy {
            continue;
        }
        if row.only_when_rch_unhealthy && input.rch_healthy {
            continue;
        }
        let degraded = row
            .penalised_by
            .iter()
            .any(|src| input.degraded_sources.iter().any(|d| d.as_str() == *src));
        let effective_cost = if degraded {
            row.base_cost_ms.saturating_add(DEGRADED_PENALTY_MS)
        } else {
            row.base_cost_ms
        };
        let rationale = if degraded && !row.rationale_degraded.is_empty() {
            row.rationale_degraded.to_owned()
        } else {
            row.rationale_clean.to_owned()
        };
        entries.push(BudgetPlanEntry {
            rank: 0,
            surface: row.surface.to_owned(),
            command: row.command.to_owned(),
            rationale,
            estimated_cost_ms: effective_cost,
            estimated_output_tokens: row.base_tokens,
            degraded_penalty: degraded,
        });
    }
    entries
}

fn collect_cargo_refusals(input: &BudgetPlannerInput) -> Vec<BudgetPlanRefusal> {
    let hint = match input.task_hint.as_deref() {
        Some(h) if h.to_ascii_lowercase().contains("cargo") => h,
        _ => return Vec::new(),
    };
    vec![BudgetPlanRefusal {
        input: hint.to_owned(),
        reason: CARGO_REFUSAL_REASON.to_owned(),
        alternative: Some(CARGO_REFUSAL_ALTERNATIVE.to_owned()),
    }]
}

fn summarize_ledger(path: Option<&Path>) -> BudgetLedgerSummary {
    let path = match path {
        Some(p) => p,
        None => {
            return BudgetLedgerSummary {
                row_count: 0,
                total_wall_clock_ms: 0,
                most_recent_surface: None,
                degraded_event_count: 0,
            };
        }
    };
    let rows = match load_ledger_rows(path) {
        Ok(r) => r,
        Err(_) => {
            return BudgetLedgerSummary {
                row_count: 0,
                total_wall_clock_ms: 0,
                most_recent_surface: None,
                degraded_event_count: 0,
            };
        }
    };
    let mut total_wall_clock_ms: u64 = 0;
    let mut degraded_event_count: u64 = 0;
    let mut most_recent_surface: Option<String> = None;

    for row in &rows {
        if let Some(ms) = row
            .get("cost")
            .and_then(|c| c.get("wallClockMs"))
            .and_then(Value::as_u64)
        {
            total_wall_clock_ms = total_wall_clock_ms.saturating_add(ms);
        }
        if let Some(groups) = row.get("degradedGroups").and_then(Value::as_array) {
            if !groups.is_empty() {
                degraded_event_count = degraded_event_count.saturating_add(1);
            }
        }
        if let Some(surface) = row
            .get("command")
            .and_then(|c| c.get("surface"))
            .and_then(Value::as_str)
        {
            most_recent_surface = Some(surface.to_owned());
        }
    }

    BudgetLedgerSummary {
        row_count: rows.len(),
        total_wall_clock_ms,
        most_recent_surface,
        degraded_event_count,
    }
}

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

    // ── Planner tests ────────────────────────────────────────────────────────

    fn plan_input_clean() -> BudgetPlannerInput {
        BudgetPlannerInput {
            ledger_path: None,
            degraded_sources: Vec::new(),
            rch_healthy: false,
            task_hint: None,
            workspace_fingerprint: "aabbccddeeff".to_owned(),
            generated_at: Utc.with_ymd_and_hms(2026, 6, 14, 12, 0, 0).unwrap(),
        }
    }

    #[test]
    fn plan_no_degradation_recommends_primer_first() -> TestResult {
        let plan = plan_cheapest_next_command(&plan_input_clean());

        assert_eq!(plan.schema, SESSION_BUDGET_PLAN_SCHEMA_V1);
        assert!(plan.advisory, "plan must be advisory");
        assert_eq!(
            plan.recommendation.surface, "primer",
            "cheapest surface is primer"
        );
        assert_eq!(plan.recommendation.rank, 1);
        assert!(
            !plan.recommendation.degraded_penalty,
            "no penalty without degraded sources"
        );
        assert!(plan.refusals.is_empty(), "no refusals without cargo hint");
        Ok(())
    }

    #[test]
    fn plan_db_degraded_adds_penalty_to_db_surfaces() -> TestResult {
        let mut input = plan_input_clean();
        input.degraded_sources = vec!["db".to_owned()];
        let plan = plan_cheapest_next_command(&input);

        // With db degraded: proof-skip wins because it is not db-dependent.
        assert_eq!(
            plan.recommendation.surface, "proof-skip",
            "proof-skip should win when db is degraded and rch is unhealthy"
        );
        // All entries that ARE db-dependent should carry the penalty flag
        let all_entries: Vec<&BudgetPlanEntry> = std::iter::once(&plan.recommendation)
            .chain(plan.fallbacks.iter())
            .collect();
        for entry in all_entries {
            let is_db_dependent =
                ["primer", "recall", "ask", "search", "pack"].contains(&entry.surface.as_str());
            if is_db_dependent {
                assert!(
                    entry.degraded_penalty,
                    "db-dependent surface '{}' must carry degraded_penalty=true",
                    entry.surface
                );
            }
        }
        Ok(())
    }

    #[test]
    fn plan_cargo_hint_produces_refusal() -> TestResult {
        let mut input = plan_input_clean();
        input.task_hint = Some("cargo test --lib".to_owned());
        let plan = plan_cheapest_next_command(&input);

        assert_eq!(plan.refusals.len(), 1, "must produce exactly one refusal");
        let refusal = &plan.refusals[0];
        assert_eq!(refusal.input, "cargo test --lib");
        assert!(
            refusal.reason.contains("structurally forbidden"),
            "reason must mention forbidden: {}",
            refusal.reason
        );
        assert!(
            refusal
                .alternative
                .as_deref()
                .unwrap_or("")
                .contains("rch_verify"),
            "alternative must reference rch_verify: {:?}",
            refusal.alternative
        );
        Ok(())
    }

    #[test]
    fn plan_non_cargo_hint_no_refusal() -> TestResult {
        let mut input = plan_input_clean();
        input.task_hint = Some("search for memories about authentication".to_owned());
        let plan = plan_cheapest_next_command(&input);

        assert!(
            plan.refusals.is_empty(),
            "non-cargo hint must not produce refusals"
        );
        Ok(())
    }

    #[test]
    fn plan_rch_healthy_includes_proof_wait_not_proof_skip() -> TestResult {
        let mut input = plan_input_clean();
        input.rch_healthy = true;
        let plan = plan_cheapest_next_command(&input);

        let all_surfaces: Vec<&str> = std::iter::once(&plan.recommendation)
            .chain(plan.fallbacks.iter())
            .map(|e| e.surface.as_str())
            .collect();
        assert!(
            all_surfaces.contains(&"proof-wait"),
            "rch_healthy=true must include proof-wait"
        );
        assert!(
            !all_surfaces.contains(&"proof-skip"),
            "rch_healthy=true must exclude proof-skip"
        );
        Ok(())
    }

    #[test]
    fn plan_rch_unhealthy_includes_proof_skip_not_proof_wait() -> TestResult {
        let input = plan_input_clean(); // rch_healthy=false by default
        let plan = plan_cheapest_next_command(&input);

        let all_surfaces: Vec<&str> = std::iter::once(&plan.recommendation)
            .chain(plan.fallbacks.iter())
            .map(|e| e.surface.as_str())
            .collect();
        assert!(
            all_surfaces.contains(&"proof-skip"),
            "rch_healthy=false must include proof-skip"
        );
        assert!(
            !all_surfaces.contains(&"proof-wait"),
            "rch_healthy=false must exclude proof-wait"
        );
        Ok(())
    }

    #[test]
    fn plan_is_deterministic_across_calls() -> TestResult {
        let input = plan_input_clean();
        let plan_a = plan_cheapest_next_command(&input);
        let plan_b = plan_cheapest_next_command(&input);

        assert_eq!(
            plan_a.recommendation.surface, plan_b.recommendation.surface,
            "same input must produce same recommendation"
        );
        assert_eq!(
            plan_a.fallbacks.len(),
            plan_b.fallbacks.len(),
            "same input must produce same fallback count"
        );
        for (a, b) in plan_a.fallbacks.iter().zip(plan_b.fallbacks.iter()) {
            assert_eq!(a.surface, b.surface, "fallback order must be deterministic");
        }
        Ok(())
    }

    #[test]
    fn plan_ledger_summary_empty_when_no_path() -> TestResult {
        let plan = plan_cheapest_next_command(&plan_input_clean());

        assert_eq!(plan.ledger_summary.row_count, 0);
        assert_eq!(plan.ledger_summary.total_wall_clock_ms, 0);
        assert_eq!(plan.ledger_summary.most_recent_surface, None);
        assert_eq!(plan.ledger_summary.degraded_event_count, 0);
        Ok(())
    }

    #[test]
    fn plan_ledger_summary_reads_existing_ledger() -> TestResult {
        let path = test_ledger_path("plan-ledger");
        let config = test_config(path.clone(), 10, 30);
        let recorder = SessionBudgetRecorder::enabled(config);
        let base = Utc.with_ymd_and_hms(2026, 6, 14, 12, 0, 0).unwrap();

        recorder
            .record_with(|| Ok(observation(1, base)))
            .map_err(|error| error.to_string())?;
        recorder
            .record_with(|| Ok(observation(2, base + ChronoDuration::seconds(1))))
            .map_err(|error| error.to_string())?;

        let mut input = plan_input_clean();
        input.ledger_path = Some(path);
        let plan = plan_cheapest_next_command(&input);

        assert_eq!(plan.ledger_summary.row_count, 2, "must read both rows");
        assert!(
            plan.ledger_summary.total_wall_clock_ms > 0,
            "must sum wall_clock_ms"
        );
        Ok(())
    }

    #[test]
    fn plan_ranks_are_sequential_from_one() -> TestResult {
        let plan = plan_cheapest_next_command(&plan_input_clean());

        assert_eq!(plan.recommendation.rank, 1);
        for (i, entry) in plan.fallbacks.iter().enumerate() {
            assert_eq!(
                entry.rank,
                u32::try_from(i + 2).unwrap(),
                "fallback ranks must be sequential: got {} at position {}",
                entry.rank,
                i
            );
        }
        Ok(())
    }

    #[test]
    fn plan_serialises_to_valid_json() -> TestResult {
        let plan = plan_cheapest_next_command(&plan_input_clean());
        let json = serde_json::to_string(&plan).map_err(|e| e.to_string())?;
        let parsed: serde_json::Value = serde_json::from_str(&json).map_err(|e| e.to_string())?;

        assert_eq!(parsed["schema"], SESSION_BUDGET_PLAN_SCHEMA_V1);
        assert_eq!(parsed["advisory"], true);
        assert!(
            parsed["recommendation"].is_object(),
            "recommendation must be object"
        );
        assert!(parsed["fallbacks"].is_array(), "fallbacks must be array");
        assert!(parsed["refusals"].is_array(), "refusals must be array");
        assert!(
            parsed["ledgerSummary"].is_object(),
            "ledgerSummary must be object"
        );
        Ok(())
    }
}
