//! Preflight risk assessment operations (EE-391).
//!
//! Assess task-specific risks before work starts so memory changes
//! agent behavior at the moment of risk rather than only after a mistake.
//!
//! # Operations
//!
//! - **run**: Execute a preflight risk assessment for a task
//! - **show**: Display details of a preflight run
//! - **close**: Mark a preflight run as completed

use std::collections::BTreeMap;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::Instant;

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

use crate::core::feedback::{PreflightFeedbackKind, RecordFeedbackReport, TaskOutcome};
use crate::models::DomainError;
use crate::models::claims::{ClaimEntry, ClaimStatus};
use crate::models::episode::{RegretCategory, RegretEntry as LedgerRegretEntry};
use crate::models::preflight::{
    PREFLIGHT_RUN_ID_PREFIX, PreflightRun, PreflightStatus, RISK_BRIEF_ID_PREFIX, RiskBrief,
    RiskCategory, RiskItem, RiskLevel, TRIPWIRE_ID_PREFIX, Tripwire, TripwireAction, TripwireType,
};

/// Schema for preflight reports.
pub const PREFLIGHT_REPORT_SCHEMA_V1: &str = "ee.preflight.report.v1";

/// Schema for the read-only agent operating contract extracted from repo docs.
pub const AGENT_OPERATING_CONTRACT_SCHEMA_V1: &str = "ee.agent_operating_contract.v1";

/// Schema for the lightweight project-local preflight run store.
pub const PREFLIGHT_RUN_STORE_SCHEMA_V1: &str = "ee.preflight_run_store.v1";

/// Location of persisted preflight runs, relative to the workspace root.
pub const PREFLIGHT_RUN_STORE_RELATIVE_PATH: &str = ".ee/preflight_runs.json";

/// Default minimum score for turning evidence into a tripwire.
pub const DEFAULT_TRIPWIRE_SOURCE_SCORE: f64 = 0.5;

/// Default maximum number of generated tripwires per run.
pub const DEFAULT_MAX_GENERATED_TRIPWIRES: usize = 8;

/// Default age after which persisted preflight evidence must be refreshed.
pub const DEFAULT_STALE_EVIDENCE_DAYS: i64 = 14;

const TRAUMA_GUARD_PREFLIGHT_SURFACE: &str = "trauma_guard_preflight";

fn elapsed_ms_since(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

fn preflight_trace_workspace_id(workspace: &Path) -> String {
    let path = workspace.to_string_lossy();
    let digest = blake3::hash(path.as_bytes()).to_hex().to_string();
    format!("wsp_{}", &digest[..16])
}

fn trace_trauma_guard_preflight(
    workspace: &Path,
    phase: &'static str,
    elapsed_ms: u64,
    degraded_codes: &[&str],
) {
    tracing::info!(
        workspace_id = %preflight_trace_workspace_id(workspace),
        request_id = "preflight_run_request",
        bead_id = option_env!("EE_TRACE_BEAD_ID").unwrap_or("bd-3usjw.6"),
        surface = TRAUMA_GUARD_PREFLIGHT_SURFACE,
        phase,
        elapsed_ms,
        degraded_codes = ?degraded_codes,
        "trauma guard preflight risk checkpoint"
    );
}

/// Configuration for deterministic tripwire generation.
#[derive(Clone, Debug, PartialEq)]
pub struct TripwireGenerationConfig {
    /// Minimum normalized source score required for generation.
    pub min_source_score: f64,
    /// Maximum tripwires generated for a preflight run.
    pub max_tripwires: usize,
}

impl Default for TripwireGenerationConfig {
    fn default() -> Self {
        Self {
            min_source_score: DEFAULT_TRIPWIRE_SOURCE_SCORE,
            max_tripwires: DEFAULT_MAX_GENERATED_TRIPWIRES,
        }
    }
}

/// Evidence surface that can seed a preflight tripwire.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TripwireSourceKind {
    /// A high-utility memory already known to prevent mistakes.
    HighUtilityMemory,
    /// A regret ledger entry from counterfactual analysis.
    RegretLedgerEntry,
    /// An executable claim or claim manifest surface.
    Claim,
    /// A dependency contract or forbidden dependency gate.
    DependencyContract,
    /// A counterfactual candidate that has not yet become regret.
    CounterfactualCandidate,
}

impl TripwireSourceKind {
    /// Stable string representation.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::HighUtilityMemory => "high_utility_memory",
            Self::RegretLedgerEntry => "regret_ledger_entry",
            Self::Claim => "claim",
            Self::DependencyContract => "dependency_contract",
            Self::CounterfactualCandidate => "counterfactual_candidate",
        }
    }

    const fn rank(self) -> u8 {
        match self {
            Self::RegretLedgerEntry => 0,
            Self::DependencyContract => 1,
            Self::HighUtilityMemory => 2,
            Self::CounterfactualCandidate => 3,
            Self::Claim => 4,
        }
    }
}

/// A normalized source candidate for tripwire generation.
#[derive(Clone, Debug, PartialEq)]
pub struct TripwireSource {
    /// Source surface kind.
    pub kind: TripwireSourceKind,
    /// Stable source identifier.
    pub source_id: String,
    /// Human-readable evidence summary.
    pub summary: String,
    /// Normalized utility/regret/confidence score.
    pub score: f64,
    /// Risk category this source guards.
    pub risk_category: RiskCategory,
    /// Risk level this source implies.
    pub risk_level: RiskLevel,
    /// Lowercase task terms that make this source task-relevant.
    pub trigger_terms: Vec<String>,
    /// Action to take if the tripwire fires.
    pub action: TripwireAction,
    /// Tripwire type to create.
    pub tripwire_type: TripwireType,
}

impl TripwireSource {
    /// Build a source from a high-utility memory.
    #[must_use]
    pub fn high_utility_memory(
        memory_id: impl Into<String>,
        summary: impl Into<String>,
        utility: f64,
        trigger_terms: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        let score = normalized_score(utility);
        Self {
            kind: TripwireSourceKind::HighUtilityMemory,
            source_id: memory_id.into(),
            summary: summary.into(),
            score,
            risk_category: RiskCategory::Other,
            risk_level: if score >= 0.85 {
                RiskLevel::High
            } else {
                RiskLevel::Medium
            },
            trigger_terms: normalize_terms(trigger_terms),
            action: TripwireAction::Warn,
            tripwire_type: TripwireType::Custom,
        }
    }

    /// Build a source from a regret ledger entry.
    #[must_use]
    pub fn regret_entry(
        entry: &LedgerRegretEntry,
        summary: impl Into<String>,
        trigger_terms: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        let score = normalized_score(entry.regret_score * entry.confidence);
        let (risk_category, risk_level, action) = regret_profile(entry.category, score);
        Self {
            kind: TripwireSourceKind::RegretLedgerEntry,
            source_id: entry.id.clone(),
            summary: summary.into(),
            score,
            risk_category,
            risk_level,
            trigger_terms: normalize_terms(trigger_terms),
            action,
            tripwire_type: TripwireType::ErrorThreshold,
        }
    }

    /// Build a source from an executable claim.
    #[must_use]
    pub fn claim_entry(
        claim: &ClaimEntry,
        score: f64,
        trigger_terms: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        let score = normalized_score(score);
        let (risk_level, action) = claim_profile(claim.status, score);
        Self {
            kind: TripwireSourceKind::Claim,
            source_id: claim.id.to_string(),
            summary: claim.title.clone(),
            score,
            risk_category: RiskCategory::Compliance,
            risk_level,
            trigger_terms: normalize_terms(trigger_terms),
            action,
            tripwire_type: TripwireType::Custom,
        }
    }

    /// Build a source from a dependency contract.
    #[must_use]
    pub fn dependency_contract(
        contract_id: impl Into<String>,
        summary: impl Into<String>,
        critical: bool,
        trigger_terms: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        Self {
            kind: TripwireSourceKind::DependencyContract,
            source_id: contract_id.into(),
            summary: summary.into(),
            score: if critical { 1.0 } else { 0.7 },
            risk_category: RiskCategory::Compliance,
            risk_level: if critical {
                RiskLevel::Critical
            } else {
                RiskLevel::High
            },
            trigger_terms: normalize_terms(trigger_terms),
            action: if critical {
                TripwireAction::Halt
            } else {
                TripwireAction::Pause
            },
            tripwire_type: TripwireType::FileChange,
        }
    }

    /// Build a source from a counterfactual candidate.
    #[must_use]
    pub fn counterfactual_candidate(
        candidate_id: impl Into<String>,
        hypothesis: impl Into<String>,
        confidence: f64,
        trigger_terms: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        let score = normalized_score(confidence);
        Self {
            kind: TripwireSourceKind::CounterfactualCandidate,
            source_id: candidate_id.into(),
            summary: hypothesis.into(),
            score,
            risk_category: RiskCategory::Stability,
            risk_level: if score >= 0.75 {
                RiskLevel::High
            } else {
                RiskLevel::Medium
            },
            trigger_terms: normalize_terms(trigger_terms),
            action: if score >= 0.75 {
                TripwireAction::Pause
            } else {
                TripwireAction::Warn
            },
            tripwire_type: TripwireType::Custom,
        }
    }

    fn matches_task(&self, task_input: &str) -> bool {
        self.trigger_terms.is_empty()
            || self
                .trigger_terms
                .iter()
                .any(|term| task_input.contains(term.as_str()))
    }
}

/// A generated tripwire with source provenance.
#[derive(Clone, Debug, PartialEq)]
pub struct GeneratedTripwire {
    /// Generated tripwire domain object.
    pub tripwire: Tripwire,
    /// Evidence source kind.
    pub source_kind: TripwireSourceKind,
    /// Evidence source ID.
    pub source_id: String,
    /// Score used for ranking and thresholding.
    pub source_score: f64,
    /// Trigger terms copied from the evidence source.
    pub trigger_terms: Vec<String>,
    /// Human-readable provenance facts for the generated tripwire.
    pub provenance: Vec<String>,
    /// Risk level used for deterministic ordering.
    pub risk_level: RiskLevel,
}

/// Options for running a preflight assessment.
#[derive(Clone, Debug)]
pub struct RunOptions {
    /// Workspace path.
    pub workspace: PathBuf,
    /// Task input/prompt to assess.
    pub task_input: String,
    /// Check for similar past failures.
    pub check_history: bool,
    /// Check for related tripwires.
    pub check_tripwires: bool,
    /// Maximum risk level to auto-clear.
    pub auto_clear_threshold: Option<RiskLevel>,
    /// Whether to run in dry-run mode.
    pub dry_run: bool,
    /// Persist the completed run into the workspace-local preflight run store.
    pub persist_run: bool,
    /// Evidence sources available for deterministic tripwire generation.
    pub tripwire_sources: Vec<TripwireSource>,
    /// Tripwire generation thresholds.
    pub tripwire_generation: TripwireGenerationConfig,
}

impl Default for RunOptions {
    fn default() -> Self {
        Self {
            workspace: PathBuf::from("."),
            task_input: String::new(),
            check_history: true,
            check_tripwires: true,
            auto_clear_threshold: Some(RiskLevel::Medium),
            dry_run: false,
            persist_run: false,
            tripwire_sources: Vec::new(),
            tripwire_generation: TripwireGenerationConfig::default(),
        }
    }
}

/// Options for showing a preflight run.
#[derive(Clone, Debug)]
pub struct ShowOptions {
    /// Workspace path.
    pub workspace: PathBuf,
    /// Preflight run ID to show.
    pub run_id: String,
    /// Include risk brief details.
    pub include_brief: bool,
    /// Include tripwire details.
    pub include_tripwires: bool,
}

impl Default for ShowOptions {
    fn default() -> Self {
        Self {
            workspace: PathBuf::from("."),
            run_id: String::new(),
            include_brief: true,
            include_tripwires: true,
        }
    }
}

/// Options for closing a preflight run.
#[derive(Clone, Debug)]
pub struct CloseOptions {
    /// Workspace path.
    pub workspace: PathBuf,
    /// Preflight run ID to close.
    pub run_id: String,
    /// Close as cleared for execution.
    pub cleared: bool,
    /// Reason for closing (especially if blocked).
    pub reason: Option<String>,
    /// Observed task outcome to feed into future scoring.
    pub task_outcome: Option<TaskOutcome>,
    /// Explicit feedback class for the warning, if known.
    pub feedback_kind: Option<PreflightFeedbackKind>,
    /// Whether to run in dry-run mode.
    pub dry_run: bool,
}

impl Default for CloseOptions {
    fn default() -> Self {
        Self {
            workspace: PathBuf::from("."),
            run_id: String::new(),
            cleared: false,
            reason: None,
            task_outcome: None,
            feedback_kind: None,
            dry_run: false,
        }
    }
}

/// Report from running a preflight assessment.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RunReport {
    pub schema: String,
    pub run_id: String,
    pub task_input: String,
    pub status: String,
    pub risk_level: String,
    pub cleared: bool,
    pub block_reason: Option<String>,
    pub risk_brief_id: Option<String>,
    pub top_risks: Vec<String>,
    pub ask_now_prompts: Vec<String>,
    pub must_verify_checks: Vec<String>,
    pub evidence_ids: Vec<String>,
    pub next_action: String,
    pub risks_identified: usize,
    pub tripwires_set: usize,
    pub tripwires: Vec<TripwireView>,
    pub degraded: Vec<PreflightDegradation>,
    pub dry_run: bool,
    pub started_at: String,
    pub completed_at: Option<String>,
}

impl RunReport {
    #[must_use]
    pub fn new(run_id: String, task_input: String) -> Self {
        Self {
            schema: PREFLIGHT_REPORT_SCHEMA_V1.to_owned(),
            run_id,
            task_input,
            status: PreflightStatus::Running.as_str().to_owned(),
            risk_level: RiskLevel::Unknown.as_str().to_owned(),
            cleared: false,
            block_reason: None,
            risk_brief_id: None,
            top_risks: Vec::new(),
            ask_now_prompts: Vec::new(),
            must_verify_checks: Vec::new(),
            evidence_ids: Vec::new(),
            next_action: "assess_risk".to_owned(),
            risks_identified: 0,
            tripwires_set: 0,
            tripwires: Vec::new(),
            degraded: Vec::new(),
            dry_run: false,
            started_at: Utc::now().to_rfc3339(),
            completed_at: None,
        }
    }

    #[must_use]
    pub fn to_json(&self) -> String {
        crate::core::serialize_or_error(self)
    }
}

/// Report from showing a preflight run.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ShowReport {
    pub schema: String,
    pub run: PreflightRunView,
    pub brief: Option<RiskBriefView>,
    pub tripwires: Vec<TripwireView>,
    pub degraded: Vec<PreflightDegradation>,
}

impl ShowReport {
    #[must_use]
    pub fn new(run: PreflightRunView) -> Self {
        Self {
            schema: PREFLIGHT_REPORT_SCHEMA_V1.to_owned(),
            run,
            brief: None,
            tripwires: Vec::new(),
            degraded: Vec::new(),
        }
    }

    #[must_use]
    pub fn to_json(&self) -> String {
        crate::core::serialize_or_error(self)
    }
}

/// View of a preflight run for display.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PreflightRunView {
    pub id: String,
    pub task_input: String,
    pub status: String,
    pub risk_level: String,
    pub cleared: bool,
    pub block_reason: Option<String>,
    pub started_at: String,
    pub completed_at: Option<String>,
    pub duration_ms: Option<u64>,
}

impl From<&PreflightRun> for PreflightRunView {
    fn from(run: &PreflightRun) -> Self {
        Self {
            id: run.id.clone(),
            task_input: run.task_input.clone(),
            status: run.status.as_str().to_owned(),
            risk_level: run.risk_level.as_str().to_owned(),
            cleared: run.cleared,
            block_reason: run.block_reason.clone(),
            started_at: run.started_at.clone(),
            completed_at: run.completed_at.clone(),
            duration_ms: run.duration_ms,
        }
    }
}

/// View of a risk brief for display.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RiskBriefView {
    pub id: String,
    pub risk_level: String,
    pub summary: Option<String>,
    pub risks: Vec<RiskItemView>,
    pub recommendations: Vec<String>,
}

impl From<&RiskBrief> for RiskBriefView {
    fn from(brief: &RiskBrief) -> Self {
        Self {
            id: brief.id.clone(),
            risk_level: brief.risk_level.as_str().to_owned(),
            summary: brief.summary.clone(),
            risks: brief.risks.iter().map(RiskItemView::from).collect(),
            recommendations: brief.recommendations.clone(),
        }
    }
}

/// View of a risk item for display.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RiskItemView {
    pub category: String,
    pub level: String,
    pub description: String,
    pub mitigation: Option<String>,
}

impl From<&RiskItem> for RiskItemView {
    fn from(item: &RiskItem) -> Self {
        Self {
            category: item.category.as_str().to_owned(),
            level: item.level.as_str().to_owned(),
            description: item.description.clone(),
            mitigation: item.mitigation.clone(),
        }
    }
}

/// View of a tripwire for display.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TripwireView {
    pub id: String,
    pub name: String,
    pub status: String,
    pub tripwire_type: String,
    pub action: String,
    pub condition: String,
    pub message: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_score: Option<f64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub trigger_terms: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub provenance: Vec<String>,
}

impl From<&Tripwire> for TripwireView {
    fn from(tripwire: &Tripwire) -> Self {
        Self {
            id: tripwire.id.clone(),
            name: tripwire
                .message
                .clone()
                .unwrap_or_else(|| tripwire.tripwire_type.as_str().to_owned()),
            status: tripwire.state.as_str().to_owned(),
            tripwire_type: tripwire.tripwire_type.as_str().to_owned(),
            action: tripwire.action.as_str().to_owned(),
            condition: tripwire.condition.clone(),
            message: tripwire.message.clone(),
            source_kind: None,
            source_id: None,
            source_score: None,
            trigger_terms: Vec::new(),
            provenance: Vec::new(),
        }
    }
}

impl From<&GeneratedTripwire> for TripwireView {
    fn from(generated: &GeneratedTripwire) -> Self {
        let mut view = Self::from(&generated.tripwire);
        view.source_kind = Some(generated.source_kind.as_str().to_owned());
        view.source_id = Some(generated.source_id.clone());
        view.source_score = Some(generated.source_score);
        view.trigger_terms = generated.trigger_terms.clone();
        view.provenance = generated.provenance.clone();
        view
    }
}

/// Report from closing a preflight run.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CloseReport {
    pub schema: String,
    pub run_id: String,
    pub previous_status: String,
    pub new_status: String,
    pub cleared: bool,
    pub reason: Option<String>,
    pub task_outcome: Option<String>,
    pub feedback: Option<RecordFeedbackReport>,
    pub dry_run: bool,
    pub closed_at: String,
}

impl CloseReport {
    #[must_use]
    pub fn new(run_id: String, previous_status: PreflightStatus) -> Self {
        Self {
            schema: PREFLIGHT_REPORT_SCHEMA_V1.to_owned(),
            run_id,
            previous_status: previous_status.as_str().to_owned(),
            new_status: PreflightStatus::Completed.as_str().to_owned(),
            cleared: false,
            reason: None,
            task_outcome: None,
            feedback: None,
            dry_run: false,
            closed_at: Utc::now().to_rfc3339(),
        }
    }

    #[must_use]
    pub fn to_json(&self) -> String {
        crate::core::serialize_or_error(self)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct PreflightRunStoreDocument {
    schema: String,
    runs: Vec<StoredPreflightRun>,
}

impl Default for PreflightRunStoreDocument {
    fn default() -> Self {
        Self {
            schema: PREFLIGHT_RUN_STORE_SCHEMA_V1.to_owned(),
            runs: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct StoredPreflightRun {
    report: RunReport,
    close_report: Option<CloseReport>,
}

/// Honest degraded-mode marker for preflight readiness contracts.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PreflightDegradation {
    pub code: String,
    pub severity: String,
    pub message: String,
    pub repair: Option<String>,
}

impl PreflightDegradation {
    #[must_use]
    pub fn evidence_unavailable(message: impl Into<String>) -> Self {
        Self {
            code: "preflight_evidence_unavailable".to_owned(),
            severity: "medium".to_owned(),
            message: message.into(),
            repair: Some("Provide explicit preflight evidence sources or run a project-local preflight risk-review skill.".to_owned()),
        }
    }

    #[must_use]
    pub fn evidence_stale(message: impl Into<String>) -> Self {
        Self {
            code: "preflight_evidence_stale".to_owned(),
            severity: "warning".to_owned(),
            message: message.into(),
            repair: Some("ee preflight run <task> --json".to_owned()),
        }
    }
}

fn generate_id() -> String {
    uuid::Uuid::now_v7().to_string()
}

/// Path for the workspace-local persisted preflight run store.
#[must_use]
pub fn preflight_run_store_path(workspace: &Path) -> PathBuf {
    workspace.join(PREFLIGHT_RUN_STORE_RELATIVE_PATH)
}

fn read_preflight_run_store(store_path: &Path) -> Result<PreflightRunStoreDocument, DomainError> {
    ensure_no_symlink_components(store_path, "read")?;
    match fs::symlink_metadata(store_path) {
        Ok(metadata) if metadata.file_type().is_file() => {}
        Ok(_) => {
            return Err(DomainError::Storage {
                message: format!(
                    "Refusing to read preflight run store `{}` because it is not a regular file.",
                    store_path.display()
                ),
                repair: Some(
                    "Replace the preflight run store with a regular JSON file.".to_owned(),
                ),
            });
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(PreflightRunStoreDocument::default());
        }
        Err(error) => {
            return Err(DomainError::Storage {
                message: format!(
                    "Failed to stat preflight run store `{}`: {error}",
                    store_path.display()
                ),
                repair: Some("Check workspace .ee permissions.".to_owned()),
            });
        }
    }

    let text = read_preflight_run_store_file_no_follow(store_path).map_err(|error| {
        DomainError::Storage {
            message: format!(
                "Failed to read preflight run store `{}`: {error}",
                store_path.display()
            ),
            repair: Some("Check workspace .ee permissions.".to_owned()),
        }
    })?;

    let document: PreflightRunStoreDocument =
        serde_json::from_str(&text).map_err(|error| DomainError::Storage {
            message: format!(
                "Failed to parse preflight run store `{}`: {error}",
                store_path.display()
            ),
            repair: Some("Repair or remove the malformed preflight run store.".to_owned()),
        })?;

    if document.schema == PREFLIGHT_RUN_STORE_SCHEMA_V1 {
        Ok(document)
    } else {
        Err(DomainError::Storage {
            message: format!(
                "Unsupported preflight run store schema `{}` in `{}`.",
                document.schema,
                store_path.display()
            ),
            repair: Some(format!(
                "Expected schema `{PREFLIGHT_RUN_STORE_SCHEMA_V1}`; migrate or rebuild the store."
            )),
        })
    }
}

fn read_preflight_run_store_file_no_follow(store_path: &Path) -> Result<String, std::io::Error> {
    let mut file = open_preflight_run_store_file_for_read(store_path)?;
    let mut text = String::new();
    file.read_to_string(&mut text)?;
    Ok(text)
}

fn open_preflight_run_store_file_for_read(store_path: &Path) -> Result<fs::File, std::io::Error> {
    let mut options = fs::OpenOptions::new();
    options.read(true);
    configure_preflight_run_store_open_no_follow(&mut options);
    options.open(store_path)
}

#[cfg(all(unix, not(any(target_os = "espidf", target_os = "horizon"))))]
fn configure_preflight_run_store_open_no_follow(options: &mut fs::OpenOptions) {
    use std::os::unix::fs::OpenOptionsExt;

    options.custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32);
}

#[cfg(not(all(unix, not(any(target_os = "espidf", target_os = "horizon")))))]
fn configure_preflight_run_store_open_no_follow(_options: &mut fs::OpenOptions) {}

fn write_preflight_run_store(
    store_path: &Path,
    store: &mut PreflightRunStoreDocument,
) -> Result<(), DomainError> {
    ensure_no_symlink_components(store_path, "write")?;
    if let Some(parent) = store_path.parent() {
        fs::create_dir_all(parent).map_err(|error| DomainError::Storage {
            message: format!(
                "Failed to create preflight run store directory `{}`: {error}",
                parent.display()
            ),
            repair: Some("Check workspace .ee permissions.".to_owned()),
        })?;
    }
    ensure_no_symlink_components(store_path, "write")?;
    ensure_preflight_run_store_final_path_for_write(store_path)?;

    store.runs.sort_by(|left, right| {
        left.report
            .started_at
            .cmp(&right.report.started_at)
            .then_with(|| left.report.run_id.cmp(&right.report.run_id))
    });

    let text = serde_json::to_string_pretty(store).map_err(|error| DomainError::Storage {
        message: format!("Failed to serialize preflight run store: {error}"),
        repair: Some("Report the invalid preflight run payload.".to_owned()),
    })?;

    let temp_path = store_path.with_extension("json.tmp");
    ensure_no_symlink_components(&temp_path, "write")?;
    ensure_preflight_run_store_temp_path_for_write(&temp_path)?;
    write_preflight_run_store_temp_file(&temp_path, &format!("{text}\n"))?;
    publish_preflight_run_store_temp_file(&temp_path, store_path)
}

fn publish_preflight_run_store_temp_file(
    temp_path: &Path,
    store_path: &Path,
) -> Result<(), DomainError> {
    ensure_no_symlink_components(temp_path, "publish")?;
    ensure_preflight_run_store_temp_path_is_regular(temp_path)?;
    ensure_no_symlink_components(store_path, "publish")?;
    ensure_preflight_run_store_final_path_for_write(store_path)?;
    fs::rename(temp_path, store_path).map_err(|error| DomainError::Storage {
        message: format!(
            "Failed to publish preflight run store `{}` from temp file `{}`: {error}",
            store_path.display(),
            temp_path.display()
        ),
        repair: Some("Check workspace .ee permissions.".to_owned()),
    })
}

fn ensure_preflight_run_store_temp_path_is_regular(temp_path: &Path) -> Result<(), DomainError> {
    match fs::symlink_metadata(temp_path) {
        Ok(metadata) if metadata.file_type().is_file() => Ok(()),
        Ok(_) => Err(DomainError::Storage {
            message: format!(
                "Refusing to publish preflight run store temp file `{}` because it is not a regular file.",
                temp_path.display()
            ),
            repair: Some("Replace .ee/preflight_runs.json.tmp with a regular file.".to_owned()),
        }),
        Err(error) => Err(DomainError::Storage {
            message: format!(
                "Failed to stat preflight run store temp file `{}` before publish: {error}",
                temp_path.display()
            ),
            repair: Some("Check workspace .ee permissions.".to_owned()),
        }),
    }
}

fn ensure_preflight_run_store_final_path_for_write(store_path: &Path) -> Result<(), DomainError> {
    match fs::symlink_metadata(store_path) {
        Ok(metadata) if metadata.file_type().is_file() => Ok(()),
        Ok(_) => Err(DomainError::Storage {
            message: format!(
                "Refusing to write preflight run store `{}` because it is not a regular file.",
                store_path.display()
            ),
            repair: Some("Replace the preflight run store with a regular JSON file.".to_owned()),
        }),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(DomainError::Storage {
            message: format!(
                "Failed to stat preflight run store `{}` before write: {error}",
                store_path.display()
            ),
            repair: Some("Check workspace .ee permissions.".to_owned()),
        }),
    }
}

fn ensure_preflight_run_store_temp_path_for_write(temp_path: &Path) -> Result<(), DomainError> {
    match fs::symlink_metadata(temp_path) {
        Ok(metadata) if metadata.file_type().is_file() => Err(DomainError::Storage {
            message: format!(
                "Refusing to write preflight run store temp file `{}` because it already exists.",
                temp_path.display()
            ),
            repair: Some("Remove stale .ee/preflight_runs.json.tmp and retry.".to_owned()),
        }),
        Ok(_) => Err(DomainError::Storage {
            message: format!(
                "Refusing to write preflight run store temp file `{}` because it is not a regular file.",
                temp_path.display()
            ),
            repair: Some("Replace .ee/preflight_runs.json.tmp with a regular file.".to_owned()),
        }),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(DomainError::Storage {
            message: format!(
                "Failed to stat preflight run store temp file `{}` before write: {error}",
                temp_path.display()
            ),
            repair: Some("Check workspace .ee permissions.".to_owned()),
        }),
    }
}

fn write_preflight_run_store_temp_file(temp_path: &Path, text: &str) -> Result<(), DomainError> {
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(temp_path)
        .map_err(|error| DomainError::Storage {
            message: format!(
                "Failed to create preflight run store temp file `{}`: {error}",
                temp_path.display()
            ),
            repair: Some("Check workspace .ee permissions.".to_owned()),
        })?;
    file.write_all(text.as_bytes())
        .map_err(|error| DomainError::Storage {
            message: format!(
                "Failed to write preflight run store temp file `{}`: {error}",
                temp_path.display()
            ),
            repair: Some("Check workspace .ee permissions.".to_owned()),
        })?;
    file.sync_all().map_err(|error| DomainError::Storage {
        message: format!(
            "Failed to sync preflight run store temp file `{}`: {error}",
            temp_path.display()
        ),
        repair: Some("Check workspace .ee permissions.".to_owned()),
    })
}

fn ensure_no_symlink_components(path: &Path, operation: &'static str) -> Result<(), DomainError> {
    let mut current = PathBuf::new();
    for component in path.components() {
        current.push(component.as_os_str());
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(DomainError::Storage {
                    message: format!(
                        "Refusing to {operation} preflight run store `{}` through symlinked path component `{}`.",
                        path.display(),
                        current.display()
                    ),
                    repair: Some(
                        "Replace the symlink with a real workspace .ee path before retrying."
                            .to_owned(),
                    ),
                });
            }
            Ok(_) => {}
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
                ) =>
            {
                return Ok(());
            }
            Err(error) => {
                return Err(DomainError::Storage {
                    message: format!(
                        "Failed to inspect preflight run store path component `{}` before {operation}: {error}",
                        current.display()
                    ),
                    repair: Some("Check workspace .ee permissions.".to_owned()),
                });
            }
        }
    }
    Ok(())
}

fn persist_preflight_run(workspace: &Path, report: &RunReport) -> Result<(), DomainError> {
    let store_path = preflight_run_store_path(workspace);
    let mut store = read_preflight_run_store(&store_path)?;
    match store
        .runs
        .iter_mut()
        .find(|stored| stored.report.run_id == report.run_id)
    {
        Some(stored) => stored.report = report.clone(),
        None => store.runs.push(StoredPreflightRun {
            report: report.clone(),
            close_report: None,
        }),
    }
    write_preflight_run_store(&store_path, &mut store)
}

fn preflight_run_view_from_report(report: &RunReport) -> PreflightRunView {
    PreflightRunView {
        id: report.run_id.clone(),
        task_input: report.task_input.clone(),
        status: report.status.clone(),
        risk_level: report.risk_level.clone(),
        cleared: report.cleared,
        block_reason: report.block_reason.clone(),
        started_at: report.started_at.clone(),
        completed_at: report.completed_at.clone(),
        duration_ms: None,
    }
}

/// Generate deterministic tripwires from already-selected evidence sources.
#[must_use]
pub fn generate_tripwires_from_sources(
    preflight_run_id: &str,
    task_input: &str,
    created_at: &str,
    sources: &[TripwireSource],
    config: &TripwireGenerationConfig,
) -> Vec<GeneratedTripwire> {
    if config.max_tripwires == 0 {
        return Vec::new();
    }

    let task_input = task_input.to_lowercase();
    let mut generated: Vec<_> = sources
        .iter()
        .filter(|source| source.score >= config.min_source_score)
        .filter(|source| source.matches_task(&task_input))
        .map(|source| build_generated_tripwire(preflight_run_id, created_at, source))
        .collect();

    generated.sort_by(|left, right| {
        risk_rank(right.risk_level)
            .cmp(&risk_rank(left.risk_level))
            .then_with(|| right.source_score.total_cmp(&left.source_score))
            .then_with(|| left.source_kind.rank().cmp(&right.source_kind.rank()))
            .then_with(|| left.source_id.cmp(&right.source_id))
    });
    generated.truncate(config.max_tripwires);
    generated
}

fn build_generated_tripwire(
    preflight_run_id: &str,
    created_at: &str,
    source: &TripwireSource,
) -> GeneratedTripwire {
    let id = stable_tripwire_id(preflight_run_id, source);
    let condition = tripwire_condition(source);
    let message = format!(
        "{} [{}:{}]: {}",
        source.risk_level.as_str(),
        source.kind.as_str(),
        source.source_id,
        source.summary
    );
    let tripwire = Tripwire::new(
        id,
        preflight_run_id,
        source.tripwire_type,
        condition,
        source.action,
        created_at,
    )
    .with_message(message);

    GeneratedTripwire {
        tripwire,
        source_kind: source.kind,
        source_id: source.source_id.clone(),
        source_score: source.score,
        trigger_terms: source.trigger_terms.clone(),
        provenance: vec![
            format!("source_kind={}", source.kind.as_str()),
            format!("source_id={}", source.source_id),
            format!("source_score={:.3}", source.score),
        ],
        risk_level: source.risk_level,
    }
}

fn stable_tripwire_id(preflight_run_id: &str, source: &TripwireSource) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(preflight_run_id.as_bytes());
    hasher.update(b"\0");
    hasher.update(source.kind.as_str().as_bytes());
    hasher.update(b"\0");
    hasher.update(source.source_id.as_bytes());
    hasher.update(b"\0");
    hasher.update(source.summary.as_bytes());
    let digest = hasher.finalize().to_hex().to_string();
    format!("{TRIPWIRE_ID_PREFIX}{}", &digest[..26])
}

fn tripwire_condition(source: &TripwireSource) -> String {
    if source.trigger_terms.is_empty() {
        return format!(
            "source:{}:{} remains relevant",
            source.kind.as_str(),
            source.source_id
        );
    }

    format!(
        "task_contains_any({})",
        source
            .trigger_terms
            .iter()
            .map(|term| format!("\"{term}\""))
            .collect::<Vec<_>>()
            .join(", ")
    )
}

fn normalize_terms<I, S>(terms: I) -> Vec<String>
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
{
    let mut normalized: Vec<_> = terms
        .into_iter()
        .map(Into::into)
        .map(|term| term.trim().to_lowercase())
        .filter(|term| !term.is_empty())
        .collect();
    normalized.sort();
    normalized.dedup();
    normalized
}

fn normalized_score(score: f64) -> f64 {
    if score.is_finite() {
        score.clamp(0.0, 1.0)
    } else {
        0.0
    }
}

fn regret_profile(
    category: RegretCategory,
    score: f64,
) -> (RiskCategory, RiskLevel, TripwireAction) {
    match category {
        RegretCategory::Misinformation => (
            RiskCategory::Stability,
            if score >= 0.8 {
                RiskLevel::Critical
            } else {
                RiskLevel::High
            },
            if score >= 0.8 {
                TripwireAction::Halt
            } else {
                TripwireAction::Pause
            },
        ),
        RegretCategory::StaleInformation => (
            RiskCategory::Compliance,
            RiskLevel::High,
            TripwireAction::Pause,
        ),
        RegretCategory::MissingKnowledge | RegretCategory::RetrievalFailure => (
            RiskCategory::Stability,
            RiskLevel::High,
            TripwireAction::Pause,
        ),
        RegretCategory::UnderutilizedMemory | RegretCategory::Other => {
            (RiskCategory::Other, RiskLevel::Medium, TripwireAction::Warn)
        }
    }
}

fn claim_profile(status: ClaimStatus, score: f64) -> (RiskLevel, TripwireAction) {
    match status {
        ClaimStatus::Invalid | ClaimStatus::Regressed => (RiskLevel::High, TripwireAction::Pause),
        ClaimStatus::Expired | ClaimStatus::Stale => (RiskLevel::High, TripwireAction::Warn),
        ClaimStatus::Unverified | ClaimStatus::Draft => (RiskLevel::Medium, TripwireAction::Audit),
        ClaimStatus::Valid | ClaimStatus::Active | ClaimStatus::Verified => {
            if score >= 0.8 {
                (RiskLevel::Medium, TripwireAction::Warn)
            } else {
                (RiskLevel::Low, TripwireAction::Audit)
            }
        }
        ClaimStatus::Retired => (RiskLevel::Low, TripwireAction::Audit),
    }
}

fn risk_rank(level: RiskLevel) -> u8 {
    match level {
        RiskLevel::Critical => 5,
        RiskLevel::High => 4,
        RiskLevel::Medium => 3,
        RiskLevel::Low => 2,
        RiskLevel::None => 1,
        RiskLevel::Unknown => 0,
    }
}

/// Options for extracting an agent operating contract from repository docs.
#[derive(Clone, Debug)]
pub struct AgentOperatingContractOptions {
    /// Workspace path whose AGENTS.md and README.md should be inspected.
    pub workspace: PathBuf,
}

impl Default for AgentOperatingContractOptions {
    fn default() -> Self {
        Self {
            workspace: PathBuf::from("."),
        }
    }
}

/// Machine-readable rule report for agent operating obligations.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AgentOperatingContractReport {
    pub schema: String,
    pub rules: Vec<AgentOperatingContractRule>,
    pub degraded: Vec<PreflightDegradation>,
}

impl AgentOperatingContractReport {
    #[must_use]
    pub fn new() -> Self {
        Self {
            schema: AGENT_OPERATING_CONTRACT_SCHEMA_V1.to_owned(),
            rules: Vec::new(),
            degraded: Vec::new(),
        }
    }

    #[must_use]
    pub fn to_json(&self) -> String {
        crate::core::serialize_or_error(self)
    }
}

impl Default for AgentOperatingContractReport {
    fn default() -> Self {
        Self::new()
    }
}

/// One deterministic operating rule extracted from repository-local docs.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AgentOperatingContractRule {
    pub id: String,
    pub severity: String,
    pub category: String,
    pub source_file: String,
    pub source_heading: String,
    pub line_start: usize,
    pub line_end: usize,
    pub excerpt_hash: String,
    pub instruction: String,
}

#[derive(Clone, Copy, Debug)]
struct AgentContractRulePattern {
    id: &'static str,
    severity: &'static str,
    category: &'static str,
    instruction: &'static str,
    needles: &'static [&'static str],
}

const AGENT_CONTRACT_RULE_PATTERNS: &[AgentContractRulePattern] = &[
    AgentContractRulePattern {
        id: "agent.no_file_deletion",
        severity: "critical",
        category: "hard_denial",
        instruction: "Do not delete files or folders without explicit written human permission.",
        needles: &["no file deletion", "never allowed to delete a file"],
    },
    AgentContractRulePattern {
        id: "agent.no_worktrees",
        severity: "critical",
        category: "hard_denial",
        instruction: "Do not create or use git worktrees for this repository.",
        needles: &["no worktrees", "git worktree add"],
    },
    AgentContractRulePattern {
        id: "agent.no_git_reset_hard",
        severity: "critical",
        category: "hard_denial",
        instruction: "Do not run git reset --hard without exact explicit human authorization.",
        needles: &["git reset --hard"],
    },
    AgentContractRulePattern {
        id: "agent.no_git_stash",
        severity: "critical",
        category: "hard_denial",
        instruction: "Do not use git stash to park repository changes.",
        needles: &["never run `git stash`", "never run git stash"],
    },
    AgentContractRulePattern {
        id: "agent.no_git_checkout_other_ref",
        severity: "critical",
        category: "hard_denial",
        instruction: "Do not use git checkout to move off main or detach HEAD.",
        needles: &[
            "never run `git checkout <other-ref>`",
            "never run git checkout <other-ref>",
        ],
    },
    AgentContractRulePattern {
        id: "agent.no_script_based_code_changes",
        severity: "high",
        category: "hard_denial",
        instruction: "Make code edits manually; do not run scripts that transform code files.",
        needles: &[
            "no script-based changes",
            "never run a script that processes/changes code",
        ],
    },
    AgentContractRulePattern {
        id: "agent.main_branch_only",
        severity: "high",
        category: "required_before_edit",
        instruction: "Keep repository work on the main branch and do not introduce master references.",
        needles: &["only use `main`", "default branch is `main`"],
    },
    AgentContractRulePattern {
        id: "agent.rch_remote_verification",
        severity: "critical",
        category: "required_before_verify",
        instruction: "Run Cargo builds, tests, clippy, and other CPU-heavy Rust verification through RCH only.",
        needles: &[
            "must be done using $rch",
            "rch-only",
            "no local cargo fallback",
        ],
    },
    AgentContractRulePattern {
        id: "agent.external_build_drive",
        severity: "high",
        category: "environment_fact",
        instruction: "Preserve the Mac external USB-NVMe Cargo target and temporary build routing.",
        needles: &[
            "external usb-nvme",
            "cargo_target_dir",
            "/volumes/usbnvme16tb",
        ],
    },
    AgentContractRulePattern {
        id: "agent.no_tokio_runtime",
        severity: "critical",
        category: "hard_denial",
        instruction: "Use Asupersync as the runtime; do not introduce Tokio.",
        needles: &["no tokio", "runtime is `/dp/asupersync`"],
    },
    AgentContractRulePattern {
        id: "agent.no_rusqlite_sqlx_diesel",
        severity: "critical",
        category: "hard_denial",
        instruction: "Use FrankenSQLite through SQLModel; do not introduce rusqlite, SQLx, Diesel, or SeaORM.",
        needles: &["no `rusqlite`", "no sqlx", "no diesel", "no seaorm"],
    },
    AgentContractRulePattern {
        id: "agent.no_petgraph",
        severity: "critical",
        category: "hard_denial",
        instruction: "Use FrankenNetworkX for graph analytics; do not introduce petgraph.",
        needles: &["no `petgraph`", "no petgraph"],
    },
    AgentContractRulePattern {
        id: "agent.stable_json",
        severity: "high",
        category: "reporting_required",
        instruction: "Machine-facing commands must keep stable versioned JSON output.",
        needles: &["stable json output", "stable json contract"],
    },
    AgentContractRulePattern {
        id: "agent.context_provenance",
        severity: "high",
        category: "reporting_required",
        instruction: "Generated context must include provenance and score or selection explanations.",
        needles: &[
            "provenance and score explanation",
            "provenance-tagged context",
        ],
    },
    AgentContractRulePattern {
        id: "agent.agent_mail_coordination",
        severity: "high",
        category: "coordination_required",
        instruction: "Coordinate with active agents through Agent Mail when it is available.",
        needles: &["agent mail", "file reservations"],
    },
    AgentContractRulePattern {
        id: "agent.beads_bv_triage",
        severity: "medium",
        category: "coordination_required",
        instruction: "Use Beads and BV for task tracking and prioritization instead of ad hoc selection.",
        needles: &["beads", "bv"],
    },
];

/// Extract the agent operating contract from the repository's AGENTS.md and README.md.
pub fn extract_agent_operating_contract(
    options: &AgentOperatingContractOptions,
) -> Result<AgentOperatingContractReport, DomainError> {
    let mut docs = Vec::new();
    let mut report = AgentOperatingContractReport::new();
    for file_name in ["AGENTS.md", "README.md"] {
        let path = options.workspace.join(file_name);
        match fs::read_to_string(&path) {
            Ok(text) => docs.push((file_name.to_owned(), text)),
            Err(error) => report
                .degraded
                .push(agent_contract_source_unavailable(file_name, error)),
        }
    }

    let doc_refs = docs
        .iter()
        .map(|(source_file, text)| (source_file.as_str(), text.as_str()))
        .collect::<Vec<_>>();
    report.rules = extract_agent_operating_contract_rules(&doc_refs);
    Ok(report)
}

/// Extract operating rules from already-loaded markdown documents.
#[must_use]
pub fn extract_agent_operating_contract_rules(
    docs: &[(&str, &str)],
) -> Vec<AgentOperatingContractRule> {
    let mut rules_by_id = BTreeMap::new();
    for (source_file, text) in docs {
        extract_agent_operating_contract_rules_from_doc(source_file, text, &mut rules_by_id);
    }
    rules_by_id.into_values().collect()
}

fn extract_agent_operating_contract_rules_from_doc(
    source_file: &str,
    text: &str,
    rules_by_id: &mut BTreeMap<String, AgentOperatingContractRule>,
) {
    let mut current_heading = "document".to_owned();
    for (line_index, raw_line) in text.lines().enumerate() {
        let trimmed = raw_line.trim();
        if let Some(heading) = markdown_heading_text(trimmed) {
            current_heading = heading.to_owned();
        }
        if trimmed.is_empty() {
            continue;
        }
        let normalized = trimmed.to_lowercase();
        for pattern in AGENT_CONTRACT_RULE_PATTERNS {
            if !pattern
                .needles
                .iter()
                .any(|needle| normalized.contains(needle))
            {
                continue;
            }
            rules_by_id.entry(pattern.id.to_owned()).or_insert_with(|| {
                AgentOperatingContractRule {
                    id: pattern.id.to_owned(),
                    severity: pattern.severity.to_owned(),
                    category: pattern.category.to_owned(),
                    source_file: source_file.to_owned(),
                    source_heading: current_heading.clone(),
                    line_start: line_index + 1,
                    line_end: line_index + 1,
                    excerpt_hash: excerpt_hash(trimmed),
                    instruction: pattern.instruction.to_owned(),
                }
            });
        }
    }
}

fn markdown_heading_text(line: &str) -> Option<&str> {
    let marker_count = line.chars().take_while(|ch| *ch == '#').count();
    if marker_count == 0 || marker_count > 6 {
        return None;
    }
    line.get(marker_count..)?
        .trim()
        .strip_prefix(' ')
        .or_else(|| {
            let rest = line.get(marker_count..)?.trim();
            (!rest.is_empty()).then_some(rest)
        })
}

fn excerpt_hash(excerpt: &str) -> String {
    let digest = blake3::hash(excerpt.as_bytes()).to_hex().to_string();
    format!("blake3:{}", &digest[..16])
}

fn agent_contract_source_unavailable(
    source_file: &str,
    error: std::io::Error,
) -> PreflightDegradation {
    PreflightDegradation {
        code: "agent_contract_source_unavailable".to_owned(),
        severity: "warning".to_owned(),
        message: format!(
            "Could not read {source_file} while extracting the agent operating contract: {error}"
        ),
        repair: Some(format!(
            "Restore readable {source_file} documentation or pass an explicit contract fixture."
        )),
    }
}

/// Run a preflight risk assessment.
pub fn run_preflight(options: &RunOptions) -> Result<RunReport, DomainError> {
    let started = Instant::now();
    trace_trauma_guard_preflight(&options.workspace, "input", 0, &[]);

    let run_id = format!("{}{}", PREFLIGHT_RUN_ID_PREFIX, generate_id());
    let mut report = RunReport::new(run_id.clone(), options.task_input.clone());
    report.dry_run = options.dry_run;

    let mut generated_tripwires = Vec::new();
    if options.check_tripwires {
        generated_tripwires = generate_tripwires_from_sources(
            &run_id,
            &options.task_input,
            &report.started_at,
            &options.tripwire_sources,
            &options.tripwire_generation,
        );
        report.tripwires_set = generated_tripwires.len();
        report.tripwires = generated_tripwires.iter().map(TripwireView::from).collect();
        report.evidence_ids = generated_tripwires
            .iter()
            .map(|generated| generated.source_id.clone())
            .collect();
    }

    if generated_tripwires.is_empty() {
        report.risk_level = RiskLevel::Unknown.as_str().to_owned();
        report.cleared = false;
        report.block_reason = Some(
            "No persisted preflight evidence matched the task; task-text heuristics are not enough to clear execution.".to_owned(),
        );
        report.next_action = "collect_preflight_evidence_or_use_risk_review_skill".to_owned();
        report.degraded.push(preflight_unavailable_degradation(
            &options.task_input,
            options.tripwire_sources.len(),
        ));
    } else {
        let risk_level = evidence_risk_level(&generated_tripwires);
        report.risk_level = risk_level.as_str().to_owned();
        report.risk_brief_id = Some(format!("{}{}", RISK_BRIEF_ID_PREFIX, generate_id()));

        let readiness = evidence_brief_fields(&generated_tripwires);
        report.top_risks = readiness.top_risks;
        report.must_verify_checks = readiness.must_verify_checks;
        report.risks_identified = report.top_risks.len();

        let auto_clear_threshold = options.auto_clear_threshold.unwrap_or(RiskLevel::Medium);
        if risk_level <= auto_clear_threshold {
            report.cleared = true;
        } else {
            report.cleared = false;
            report.block_reason = Some(format!(
                "Evidence-backed risk level {} exceeds auto-clear threshold {}",
                risk_level.as_str(),
                auto_clear_threshold.as_str()
            ));
        }
        report.next_action = if report.cleared {
            "proceed_after_evidence_review".to_owned()
        } else {
            "review_evidence_matches_before_proceeding".to_owned()
        };
    }
    if let Some(degradation) = stale_preflight_evidence_degradation(&options.workspace, &report)? {
        report.degraded.push(degradation);
        if report.next_action == "proceed_after_evidence_review" {
            report.next_action = "refresh_stale_preflight_evidence_before_proceeding".to_owned();
        }
    }

    report.status = PreflightStatus::Completed.as_str().to_owned();
    report.completed_at = Some(Utc::now().to_rfc3339());

    if options.persist_run && !options.dry_run {
        let degraded_codes = report
            .degraded
            .iter()
            .map(|degraded| degraded.code.as_str())
            .collect::<Vec<_>>();
        trace_trauma_guard_preflight(
            &options.workspace,
            "persistence",
            elapsed_ms_since(started),
            &degraded_codes,
        );
        persist_preflight_run(&options.workspace, &report)?;
    }

    let degraded_codes = report
        .degraded
        .iter()
        .map(|degraded| degraded.code.as_str())
        .collect::<Vec<_>>();
    trace_trauma_guard_preflight(
        &options.workspace,
        "response",
        elapsed_ms_since(started),
        &degraded_codes,
    );
    Ok(report)
}

/// Show details of a preflight run.
pub fn show_preflight(options: &ShowOptions) -> Result<ShowReport, DomainError> {
    validate_preflight_run_id(&options.run_id)?;
    let store_path = preflight_run_store_path(&options.workspace);
    let store = read_preflight_run_store(&store_path)?;
    let stored = store
        .runs
        .iter()
        .find(|stored| stored.report.run_id == options.run_id)
        .ok_or_else(|| preflight_run_not_found(&options.run_id))?;

    let mut report = ShowReport::new(preflight_run_view_from_report(&stored.report));
    if options.include_tripwires {
        report.tripwires = stored.report.tripwires.clone();
    }
    report.degraded = stored.report.degraded.clone();
    Ok(report)
}

/// Close a preflight run.
pub fn close_preflight(options: &CloseOptions) -> Result<CloseReport, DomainError> {
    validate_preflight_run_id(&options.run_id)?;
    let store_path = preflight_run_store_path(&options.workspace);
    let mut store = read_preflight_run_store(&store_path)?;
    let stored = store
        .runs
        .iter_mut()
        .find(|stored| stored.report.run_id == options.run_id)
        .ok_or_else(|| preflight_run_not_found(&options.run_id))?;

    let previous_status = stored
        .report
        .status
        .parse::<PreflightStatus>()
        .unwrap_or(PreflightStatus::Completed);
    let mut report = CloseReport::new(options.run_id.clone(), previous_status);
    report.cleared = options.cleared;
    report.reason = options.reason.clone();
    report.task_outcome = options
        .task_outcome
        .map(|outcome| outcome.as_str().to_owned());
    report.dry_run = options.dry_run;

    if !options.dry_run {
        stored.report.cleared = options.cleared;
        stored.report.block_reason = if options.cleared {
            None
        } else {
            options.reason.clone()
        };
        stored.report.status = PreflightStatus::Completed.as_str().to_owned();
        stored.close_report = Some(report.clone());
        write_preflight_run_store(&store_path, &mut store)?;
    }

    Ok(report)
}

fn validate_preflight_run_id(run_id: &str) -> Result<(), DomainError> {
    if run_id.starts_with(PREFLIGHT_RUN_ID_PREFIX) {
        Ok(())
    } else {
        Err(DomainError::Usage {
            message: format!(
                "Invalid preflight run ID: expected prefix '{}', got '{}'",
                PREFLIGHT_RUN_ID_PREFIX,
                &run_id[..run_id.len().min(3)]
            ),
            repair: Some("Provide a valid preflight run ID (format: pf_<uuid>)".to_owned()),
        })
    }
}

fn preflight_run_not_found(run_id: &str) -> DomainError {
    DomainError::NotFound {
        resource: "preflight run".to_owned(),
        id: run_id.to_owned(),
        repair: Some(
            "Run `ee preflight run <task>` in the same workspace before show/close.".to_owned(),
        ),
    }
}

/// Assess risk level from task input text.
fn assess_task_risk(task_input: &str) -> RiskLevel {
    let lower = task_input.to_lowercase();

    // Critical risk patterns
    if lower.contains("delete")
        || lower.contains("rm -rf")
        || lower.contains("drop table")
        || lower.contains("truncate")
    {
        return RiskLevel::Critical;
    }

    // High risk patterns
    if lower.contains("production")
        || lower.contains("deploy")
        || lower.contains("migrate")
        || lower.contains("force")
    {
        return RiskLevel::High;
    }

    // Medium risk patterns
    if lower.contains("update")
        || lower.contains("modify")
        || lower.contains("change")
        || lower.contains("refactor")
    {
        return RiskLevel::Medium;
    }

    // Low risk patterns
    if lower.contains("read")
        || lower.contains("list")
        || lower.contains("show")
        || lower.contains("search")
    {
        return RiskLevel::Low;
    }

    RiskLevel::None
}

struct ReadinessBriefFields {
    top_risks: Vec<String>,
    must_verify_checks: Vec<String>,
}

fn evidence_brief_fields(generated_tripwires: &[GeneratedTripwire]) -> ReadinessBriefFields {
    let top_risks = generated_tripwires
        .iter()
        .map(|generated| {
            generated.tripwire.message.clone().unwrap_or_else(|| {
                format!(
                    "{} [{}:{}]",
                    generated.risk_level.as_str(),
                    generated.source_kind.as_str(),
                    generated.source_id
                )
            })
        })
        .collect();
    let must_verify_checks = generated_tripwires
        .iter()
        .map(|generated| {
            format!(
                "Review evidence source {}:{} before proceeding.",
                generated.source_kind.as_str(),
                generated.source_id
            )
        })
        .collect();
    ReadinessBriefFields {
        top_risks,
        must_verify_checks,
    }
}

fn evidence_risk_level(generated_tripwires: &[GeneratedTripwire]) -> RiskLevel {
    generated_tripwires
        .iter()
        .map(|generated| generated.risk_level)
        .max_by_key(|level| risk_rank(*level))
        .unwrap_or(RiskLevel::Unknown)
}

fn preflight_unavailable_degradation(
    task_input: &str,
    source_count: usize,
) -> PreflightDegradation {
    let heuristic_level = assess_task_risk(task_input);
    let source_message = if source_count == 0 {
        "No persisted evidence sources were provided."
    } else {
        "Persisted evidence sources were provided, but none matched the task and threshold."
    };
    let heuristic_message = if matches!(heuristic_level, RiskLevel::None | RiskLevel::Unknown) {
        "No task-text heuristic is treated as an evidence-backed risk."
    } else {
        "Task text matched heuristic risk language, but heuristics are not treated as evidence-backed risks."
    };
    PreflightDegradation::evidence_unavailable(format!("{source_message} {heuristic_message}"))
}

fn stale_preflight_evidence_degradation(
    workspace: &Path,
    report: &RunReport,
) -> Result<Option<PreflightDegradation>, DomainError> {
    let store_path = preflight_run_store_path(workspace);
    let store = read_preflight_run_store(&store_path)?;
    let now = Utc::now();
    let stale_before = now - Duration::days(DEFAULT_STALE_EVIDENCE_DAYS);
    let latest_matching = store
        .runs
        .iter()
        .filter(|stored| stored.report.task_input == report.task_input)
        .filter(|stored| stored.report.run_id != report.run_id)
        .filter_map(|stored| {
            preflight_report_observed_at(&stored.report).map(|observed_at| (stored, observed_at))
        })
        .max_by(|(_, left), (_, right)| left.cmp(right));

    let Some((stored, observed_at)) = latest_matching else {
        return Ok(None);
    };
    if observed_at >= stale_before {
        return Ok(None);
    }

    Ok(Some(PreflightDegradation::evidence_stale(format!(
        "Persisted preflight evidence for this task is stale: previous run {} was observed at {}.",
        stored.report.run_id,
        observed_at.to_rfc3339()
    ))))
}

fn preflight_report_observed_at(report: &RunReport) -> Option<DateTime<Utc>> {
    let timestamp = report.completed_at.as_deref().unwrap_or(&report.started_at);
    DateTime::parse_from_rfc3339(timestamp)
        .ok()
        .map(|parsed| parsed.with_timezone(&Utc))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    type TestResult = Result<(), String>;

    fn ensure<T: std::fmt::Debug + PartialEq>(actual: T, expected: T, ctx: &str) -> TestResult {
        if actual == expected {
            Ok(())
        } else {
            Err(format!("{ctx}: expected {expected:?}, got {actual:?}"))
        }
    }

    fn temp_workspace() -> Result<tempfile::TempDir, String> {
        tempfile::tempdir().map_err(|error| format!("tempdir: {error}"))
    }

    #[test]
    fn run_dry_run_completes_immediately() -> TestResult {
        let options = RunOptions {
            task_input: "test task".to_owned(),
            dry_run: true,
            ..Default::default()
        };

        let report = run_preflight(&options).map_err(|e| e.message())?;
        ensure(report.dry_run, true, "dry_run")?;
        ensure(
            report.status,
            PreflightStatus::Completed.as_str().to_owned(),
            "status",
        )?;
        ensure(
            report.run_id.starts_with(PREFLIGHT_RUN_ID_PREFIX),
            true,
            "run_id prefix",
        )
    }

    #[test]
    fn run_without_evidence_does_not_promote_task_text_to_risk() -> TestResult {
        let options = RunOptions {
            task_input: "delete all production data".to_owned(),
            dry_run: false,
            ..Default::default()
        };

        let report = run_preflight(&options).map_err(|e| e.message())?;
        ensure(
            report.risk_level,
            RiskLevel::Unknown.as_str().to_owned(),
            "task-text-only risk level",
        )?;
        ensure(report.cleared, false, "should not be cleared")?;
        ensure(report.risk_brief_id.is_none(), true, "no fake risk brief")?;
        ensure(report.top_risks.is_empty(), true, "no heuristic risks")?;
        ensure(
            report.ask_now_prompts.is_empty(),
            true,
            "no ask-now prompts",
        )?;
        ensure(
            report
                .degraded
                .iter()
                .any(|entry| entry.code == "preflight_evidence_unavailable"),
            true,
            "evidence unavailable degradation",
        )
    }

    #[test]
    fn run_with_no_evidence_does_not_auto_clear_low_risk_text() -> TestResult {
        let options = RunOptions {
            task_input: "list all files".to_owned(),
            dry_run: false,
            ..Default::default()
        };

        let report = run_preflight(&options).map_err(|e| e.message())?;
        ensure(
            report.risk_level,
            RiskLevel::Unknown.as_str().to_owned(),
            "risk_level",
        )?;
        ensure(report.cleared, false, "no evidence means not cleared")
    }

    #[test]
    fn show_rejects_invalid_run_id() -> TestResult {
        let options = ShowOptions {
            run_id: "invalid_id".to_owned(),
            ..Default::default()
        };

        let result = show_preflight(&options);
        ensure(result.is_err(), true, "should reject invalid ID")
    }

    #[test]
    fn close_returns_not_found_without_persisted_run() -> TestResult {
        let options = CloseOptions {
            run_id: format!("{}test", PREFLIGHT_RUN_ID_PREFIX),
            cleared: true,
            ..Default::default()
        };

        let Err(error) = close_preflight(&options) else {
            return Err("close should not invent a persisted preflight run".to_owned());
        };
        ensure(error.code(), "not_found", "error code")
    }

    #[test]
    fn persisted_run_can_be_shown_with_tripwire_provenance() -> TestResult {
        let workspace = temp_workspace()?;
        let source = TripwireSource::dependency_contract(
            "dep_forbidden_runtime",
            "Forbidden runtime dependency must not be introduced",
            true,
            ["release"],
        );
        let run = run_preflight(&RunOptions {
            workspace: workspace.path().to_path_buf(),
            task_input: "prepare release".to_owned(),
            persist_run: true,
            tripwire_sources: vec![source],
            ..Default::default()
        })
        .map_err(|error| error.message())?;

        ensure(
            preflight_run_store_path(workspace.path()).exists(),
            true,
            "run store exists",
        )?;

        let shown = show_preflight(&ShowOptions {
            workspace: workspace.path().to_path_buf(),
            run_id: run.run_id.clone(),
            include_tripwires: true,
            ..Default::default()
        })
        .map_err(|error| error.message())?;

        ensure(shown.run.id, run.run_id, "shown run id")?;
        ensure(shown.tripwires.len(), 1, "shown tripwire count")?;
        ensure(
            shown.tripwires[0].source_id.clone(),
            Some("dep_forbidden_runtime".to_owned()),
            "shown source id",
        )?;
        ensure(shown.degraded.is_empty(), true, "no degraded evidence")
    }

    #[cfg(unix)]
    #[test]
    fn run_preflight_rejects_symlinked_metadata_parent() -> TestResult {
        use std::os::unix::fs::symlink;

        let workspace = temp_workspace()?;
        let real_metadata = workspace.path().join("real-ee");
        std::fs::create_dir_all(&real_metadata).map_err(|error| error.to_string())?;
        symlink(&real_metadata, workspace.path().join(".ee")).map_err(|error| error.to_string())?;

        let source = TripwireSource::dependency_contract(
            "dep_forbidden_runtime",
            "Forbidden runtime dependency must not be introduced",
            true,
            ["release"],
        );
        let result = run_preflight(&RunOptions {
            workspace: workspace.path().to_path_buf(),
            task_input: "prepare release".to_owned(),
            persist_run: true,
            tripwire_sources: vec![source],
            ..Default::default()
        });
        let error = result.expect_err("symlinked .ee parent should be rejected");
        ensure(
            error.message().contains("symlinked path component"),
            true,
            "symlinked .ee error message",
        )?;
        ensure(
            real_metadata.join("preflight_runs.json").exists(),
            false,
            "preflight store must not be written through symlinked .ee parent",
        )
    }

    #[cfg(unix)]
    #[test]
    fn show_preflight_rejects_symlinked_run_store_file() -> TestResult {
        use std::os::unix::fs::symlink;

        let workspace = temp_workspace()?;
        let ee_dir = workspace.path().join(".ee");
        std::fs::create_dir_all(&ee_dir).map_err(|error| error.to_string())?;

        let outside_store = workspace.path().join("outside-preflight-runs.json");
        let run = RunReport::new(
            "pf_symlink000000000000000000000".to_owned(),
            "prepare release".to_owned(),
        );
        let mut store = PreflightRunStoreDocument {
            schema: PREFLIGHT_RUN_STORE_SCHEMA_V1.to_owned(),
            runs: vec![StoredPreflightRun {
                report: run,
                close_report: None,
            }],
        };
        write_preflight_run_store(&outside_store, &mut store).map_err(|error| error.message())?;
        symlink(&outside_store, preflight_run_store_path(workspace.path()))
            .map_err(|error| error.to_string())?;

        let result = show_preflight(&ShowOptions {
            workspace: workspace.path().to_path_buf(),
            run_id: "pf_symlink000000000000000000000".to_owned(),
            ..Default::default()
        });
        let error = result.expect_err("symlinked preflight run store should be rejected");
        ensure(
            error.message().contains("symlinked path component"),
            true,
            "symlinked store error message",
        )
    }

    #[cfg(unix)]
    #[test]
    fn preflight_run_store_final_read_open_rejects_swapped_symlink_file() -> TestResult {
        use std::os::unix::fs::symlink;

        let workspace = temp_workspace()?;
        let ee_dir = workspace.path().join(".ee");
        std::fs::create_dir_all(&ee_dir).map_err(|error| error.to_string())?;
        let outside_store = workspace.path().join("outside-preflight-runs.json");
        let outside_text =
            format!("{{\"schema\":\"{PREFLIGHT_RUN_STORE_SCHEMA_V1}\",\"runs\":[]}}\n");
        std::fs::write(&outside_store, &outside_text).map_err(|error| error.to_string())?;
        let store_path = preflight_run_store_path(workspace.path());
        symlink(&outside_store, &store_path).map_err(|error| error.to_string())?;

        let error = open_preflight_run_store_file_for_read(&store_path)
            .expect_err("final preflight run-store read open must reject symlinks");

        ensure(
            error.kind() != std::io::ErrorKind::NotFound,
            true,
            "final symlink read should fail because the path is a symlink",
        )?;
        ensure(
            std::fs::read_to_string(&outside_store).map_err(|error| error.to_string())?,
            outside_text,
            "preflight run-store read helper must not follow the symlink target",
        )
    }

    #[test]
    fn show_preflight_rejects_non_regular_run_store_file() -> TestResult {
        let workspace = temp_workspace()?;
        std::fs::create_dir_all(preflight_run_store_path(workspace.path()))
            .map_err(|error| error.to_string())?;

        let result = show_preflight(&ShowOptions {
            workspace: workspace.path().to_path_buf(),
            run_id: "pf_directory000000000000000000".to_owned(),
            ..Default::default()
        });
        let error = result.expect_err("directory preflight run store should be rejected");
        ensure(
            error.message().contains("not a regular file"),
            true,
            "non-regular store error message",
        )
    }

    #[test]
    fn write_preflight_run_store_rejects_non_regular_final_path() -> TestResult {
        let workspace = temp_workspace()?;
        let store_path = preflight_run_store_path(workspace.path());
        std::fs::create_dir_all(&store_path).map_err(|error| error.to_string())?;

        let mut store = PreflightRunStoreDocument::default();
        let result = write_preflight_run_store(&store_path, &mut store);
        let error = result.expect_err("directory preflight run store should be rejected on write");
        ensure(
            error.message().contains("not a regular file"),
            true,
            "non-regular write error message",
        )?;
        ensure(
            store_path.is_dir(),
            true,
            "non-regular store path remains a directory",
        )
    }

    #[test]
    fn write_preflight_run_store_rejects_existing_regular_temp_file_without_truncating()
    -> TestResult {
        let workspace = temp_workspace()?;
        let store_path = preflight_run_store_path(workspace.path());
        let temp_path = store_path.with_extension("json.tmp");
        std::fs::create_dir_all(temp_path.parent().expect("preflight temp parent"))
            .map_err(|error| error.to_string())?;
        std::fs::write(&temp_path, "stale preflight temp").map_err(|error| error.to_string())?;

        let mut store = PreflightRunStoreDocument::default();
        let result = write_preflight_run_store(&store_path, &mut store);
        let error =
            result.expect_err("existing regular temp file should reject preflight store write");
        ensure(
            error.message().contains("already exists"),
            true,
            "existing temp error message",
        )?;
        ensure(
            std::fs::read_to_string(&temp_path).map_err(|error| error.to_string())?,
            "stale preflight temp".to_owned(),
            "existing temp content remains unchanged",
        )?;
        ensure(
            store_path.exists(),
            false,
            "final preflight store must not be published when temp exists",
        )
    }

    #[cfg(unix)]
    #[test]
    fn write_preflight_run_store_rechecks_final_symlink_before_publish() -> TestResult {
        use std::os::unix::fs::symlink;

        let workspace = temp_workspace()?;
        let store_path = preflight_run_store_path(workspace.path());
        let temp_path = store_path.with_extension("json.tmp");
        std::fs::create_dir_all(temp_path.parent().expect("preflight temp parent"))
            .map_err(|error| error.to_string())?;
        write_preflight_run_store_temp_file(&temp_path, "{\"schema\":\"sentinel\"}\n")
            .map_err(|error| error.message())?;

        let outside_store = workspace.path().join("outside-preflight-runs.json");
        std::fs::write(&outside_store, "outside sentinel").map_err(|error| error.to_string())?;
        symlink(&outside_store, &store_path).map_err(|error| error.to_string())?;

        let result = publish_preflight_run_store_temp_file(&temp_path, &store_path);
        let error = result.expect_err("final symlink must be rejected before publish");
        ensure(
            error.message().contains("symlinked path component"),
            true,
            "final symlink publish error message",
        )?;
        ensure(
            std::fs::read_to_string(&outside_store).map_err(|error| error.to_string())?,
            "outside sentinel".to_owned(),
            "outside symlink target remains unchanged",
        )?;
        ensure(
            std::fs::read_to_string(&temp_path).map_err(|error| error.to_string())?,
            "{\"schema\":\"sentinel\"}\n".to_owned(),
            "temp store remains available after rejected publish",
        )
    }

    #[cfg(unix)]
    #[test]
    fn write_preflight_run_store_rechecks_temp_symlink_before_publish() -> TestResult {
        use std::os::unix::fs::symlink;

        let workspace = temp_workspace()?;
        let store_path = preflight_run_store_path(workspace.path());
        let temp_path = store_path.with_extension("json.tmp");
        let preserved_temp = store_path.with_extension("json.tmp.preserved");
        std::fs::create_dir_all(temp_path.parent().expect("preflight temp parent"))
            .map_err(|error| error.to_string())?;
        write_preflight_run_store_temp_file(&temp_path, "{\"schema\":\"sentinel\"}\n")
            .map_err(|error| error.message())?;
        std::fs::rename(&temp_path, &preserved_temp).map_err(|error| error.to_string())?;

        let outside_store = workspace.path().join("outside-preflight-runs.json");
        std::fs::write(&outside_store, "outside sentinel").map_err(|error| error.to_string())?;
        symlink(&outside_store, &temp_path).map_err(|error| error.to_string())?;

        let result = publish_preflight_run_store_temp_file(&temp_path, &store_path);
        let error = result.expect_err("temp symlink must be rejected before publish");
        ensure(
            error.message().contains("symlinked path component")
                || error.message().contains("not a regular file"),
            true,
            "temp symlink publish error message",
        )?;
        ensure(
            store_path.exists(),
            false,
            "final preflight store must not be published through swapped temp symlink",
        )?;
        ensure(
            std::fs::read_to_string(&outside_store).map_err(|error| error.to_string())?,
            "outside sentinel".to_owned(),
            "outside symlink target remains unchanged",
        )?;
        ensure(
            std::fs::symlink_metadata(&temp_path)
                .map_err(|error| error.to_string())?
                .file_type()
                .is_symlink(),
            true,
            "rejected temp symlink remains for inspection",
        )?;
        ensure(
            std::fs::read_to_string(&preserved_temp).map_err(|error| error.to_string())?,
            "{\"schema\":\"sentinel\"}\n".to_owned(),
            "preserved temp store remains available after simulated swap",
        )
    }

    #[test]
    fn run_reports_matching_stale_persisted_preflight_evidence() -> TestResult {
        let workspace = temp_workspace()?;
        let mut stale_report = RunReport::new(
            "pf_stale000000000000000000000000".to_owned(),
            "prepare release".to_owned(),
        );
        stale_report.status = PreflightStatus::Completed.as_str().to_owned();
        stale_report.started_at = "2000-01-01T00:00:00Z".to_owned();
        stale_report.completed_at = Some("2000-01-01T00:00:01Z".to_owned());

        let mut store = PreflightRunStoreDocument::default();
        store.runs.push(StoredPreflightRun {
            report: stale_report,
            close_report: None,
        });
        write_preflight_run_store(&preflight_run_store_path(workspace.path()), &mut store)
            .map_err(|error| error.message())?;

        let report = run_preflight(&RunOptions {
            workspace: workspace.path().to_path_buf(),
            task_input: "prepare release".to_owned(),
            persist_run: false,
            ..Default::default()
        })
        .map_err(|error| error.message())?;

        ensure(
            report
                .degraded
                .iter()
                .any(|entry| entry.code == "preflight_evidence_stale"),
            true,
            "stale evidence degradation present",
        )
    }

    #[test]
    fn agent_operating_contract_extraction_is_stable_and_deduped() -> TestResult {
        let docs = [(
            "AGENTS.md",
            r#"# Root Rules

## Git Branch: ONLY Use `main`, NEVER `master`

The default branch is `main`.

## RULE NUMBER 2: NO WORKTREES. EVER. NO EXCEPTIONS.

Never run `git worktree add`.
Never run `git worktree add`.

## Compiler Checks

All cargo builds and tests and other CPU intensive operations MUST be done using $rch.
"#,
        )];

        let first = extract_agent_operating_contract_rules(&docs);
        let second = extract_agent_operating_contract_rules(&docs);

        ensure(first.clone(), second, "stable extraction")?;
        ensure(
            first
                .iter()
                .filter(|rule| rule.id == "agent.no_worktrees")
                .count(),
            1,
            "duplicate rule id collapsed",
        )?;
        let ids = first
            .iter()
            .map(|rule| rule.id.as_str())
            .collect::<Vec<_>>();
        ensure(
            ids,
            vec![
                "agent.main_branch_only",
                "agent.no_worktrees",
                "agent.rch_remote_verification",
            ],
            "deterministic sorted rule ids",
        )?;
        let no_worktrees = first
            .iter()
            .find(|rule| rule.id == "agent.no_worktrees")
            .ok_or_else(|| "missing no-worktrees rule".to_owned())?;
        ensure(
            no_worktrees.source_heading.clone(),
            "RULE NUMBER 2: NO WORKTREES. EVER. NO EXCEPTIONS.".to_owned(),
            "source heading",
        )?;
        ensure(no_worktrees.line_start, 9, "line_start")
    }

    #[test]
    fn agent_operating_contract_extracts_readme_hard_requirements() -> TestResult {
        let docs = [(
            "README.md",
            r#"# Eidetic Engine

## Hard Requirements

- Runtime is `/dp/asupersync`. **No Tokio.** Anywhere. Ever.
- Database is `/dp/frankensqlite` through `/dp/sqlmodel_rust`. **No `rusqlite`, no SQLx, no Diesel, no SeaORM.**
- Graph is `/dp/franken_networkx`. **No `petgraph`.**
- Every machine-facing command supports stable JSON output.
- Every generated context includes provenance and score explanation.
"#,
        )];

        let rules = extract_agent_operating_contract_rules(&docs);
        let ids = rules
            .iter()
            .map(|rule| rule.id.as_str())
            .collect::<Vec<_>>();
        ensure(
            ids,
            vec![
                "agent.context_provenance",
                "agent.no_petgraph",
                "agent.no_rusqlite_sqlx_diesel",
                "agent.no_tokio_runtime",
                "agent.stable_json",
            ],
            "README hard requirement rule ids",
        )?;
        ensure(
            rules
                .iter()
                .all(|rule| rule.source_heading == "Hard Requirements"),
            true,
            "README source headings",
        )
    }

    #[test]
    fn agent_operating_contract_reports_missing_docs_as_degraded() -> TestResult {
        let workspace = temp_workspace()?;
        std::fs::write(
            workspace.path().join("AGENTS.md"),
            "# AGENTS\n\nNo Tokio.\n",
        )
        .map_err(|error| error.to_string())?;

        let report = extract_agent_operating_contract(&AgentOperatingContractOptions {
            workspace: workspace.path().to_path_buf(),
        })
        .map_err(|error| error.message())?;

        ensure(
            report.schema.clone(),
            AGENT_OPERATING_CONTRACT_SCHEMA_V1.to_owned(),
            "schema",
        )?;
        ensure(
            report
                .degraded
                .iter()
                .any(|entry| entry.code == "agent_contract_source_unavailable"),
            true,
            "missing README degradation",
        )?;
        ensure(
            report
                .rules
                .iter()
                .any(|rule| rule.id == "agent.no_tokio_runtime"),
            true,
            "extracts available AGENTS rule",
        )
    }

    #[test]
    fn close_updates_persisted_run_state() -> TestResult {
        let workspace = temp_workspace()?;
        let source = TripwireSource::dependency_contract(
            "dep_forbidden_runtime",
            "Forbidden runtime dependency must not be introduced",
            true,
            ["release"],
        );
        let run = run_preflight(&RunOptions {
            workspace: workspace.path().to_path_buf(),
            task_input: "prepare release".to_owned(),
            persist_run: true,
            tripwire_sources: vec![source],
            ..Default::default()
        })
        .map_err(|error| error.message())?;

        let close = close_preflight(&CloseOptions {
            workspace: workspace.path().to_path_buf(),
            run_id: run.run_id.clone(),
            cleared: false,
            reason: Some("manual review still required".to_owned()),
            task_outcome: Some(TaskOutcome::Failure),
            feedback_kind: Some(PreflightFeedbackKind::Missed),
            ..Default::default()
        })
        .map_err(|error| error.message())?;

        ensure(close.run_id, run.run_id.clone(), "close run id")?;
        ensure(close.cleared, false, "close cleared")?;
        ensure(
            close.task_outcome,
            Some("failure".to_owned()),
            "close task outcome",
        )?;

        let shown = show_preflight(&ShowOptions {
            workspace: workspace.path().to_path_buf(),
            run_id: run.run_id,
            ..Default::default()
        })
        .map_err(|error| error.message())?;

        ensure(shown.run.cleared, false, "stored cleared")?;
        ensure(
            shown.run.block_reason,
            Some("manual review still required".to_owned()),
            "stored close reason",
        )
    }

    #[test]
    fn report_serializes_to_json() -> TestResult {
        let report = RunReport::new("pf_test".to_owned(), "test task".to_owned());
        let json = report.to_json();
        ensure(json.contains("pf_test"), true, "json contains run_id")?;
        ensure(
            json.contains(PREFLIGHT_REPORT_SCHEMA_V1),
            true,
            "json contains schema",
        )
    }

    #[test]
    fn assess_task_risk_patterns() -> TestResult {
        ensure(assess_task_risk("rm -rf /"), RiskLevel::Critical, "rm -rf")?;
        ensure(
            assess_task_risk("deploy to production"),
            RiskLevel::High,
            "production deploy",
        )?;
        ensure(
            assess_task_risk("refactor the module"),
            RiskLevel::Medium,
            "refactor",
        )?;
        ensure(
            assess_task_risk("search for files"),
            RiskLevel::Low,
            "search",
        )?;
        ensure(
            assess_task_risk("hello world"),
            RiskLevel::None,
            "no pattern",
        )
    }

    #[test]
    fn tripwire_source_normalizes_terms_and_scores() -> TestResult {
        let source = TripwireSource::high_utility_memory(
            "mem_release_rule",
            "Run release checks before publishing",
            f64::NAN,
            [" Release ", "", "release"],
        );

        ensure(source.score, 0.0, "non-finite score clamps to zero")?;
        ensure(
            source.trigger_terms,
            vec!["release".to_string()],
            "normalized terms",
        )
    }

    #[test]
    fn generate_tripwires_filters_orders_and_stabilizes_ids() -> TestResult {
        let sources = vec![
            TripwireSource::high_utility_memory(
                "mem_release_rule",
                "Run release checks before publishing",
                0.95,
                ["release"],
            ),
            TripwireSource::counterfactual_candidate(
                "cf_billing_only",
                "Billing recovery candidate should not match release tasks",
                0.9,
                ["billing"],
            ),
            TripwireSource::dependency_contract(
                "dep_no_tokio",
                "Forbidden async runtime dependency must not appear",
                true,
                ["release"],
            ),
            TripwireSource::high_utility_memory(
                "mem_low_signal",
                "Low confidence reminder should stay below threshold",
                0.2,
                ["release"],
            ),
        ];

        let generated = generate_tripwires_from_sources(
            "pf_fixed",
            "prepare release",
            "2026-04-30T12:00:00Z",
            &sources,
            &TripwireGenerationConfig::default(),
        );
        let repeated = generate_tripwires_from_sources(
            "pf_fixed",
            "prepare release",
            "2026-04-30T12:00:00Z",
            &sources,
            &TripwireGenerationConfig::default(),
        );

        ensure(generated.len(), 2, "eligible tripwire count")?;
        ensure(
            generated[0].source_id.clone(),
            "dep_no_tokio".to_string(),
            "critical dependency contract first",
        )?;
        ensure(
            generated[1].source_id.clone(),
            "mem_release_rule".to_string(),
            "high utility memory second",
        )?;
        ensure(
            generated[0].tripwire.id.clone(),
            repeated[0].tripwire.id.clone(),
            "stable generated id",
        )?;
        ensure(
            generated[0].tripwire.condition.clone(),
            "task_contains_any(\"release\")".to_string(),
            "condition",
        )?;
        ensure(
            generated[0].trigger_terms.clone(),
            vec!["release".to_string()],
            "generated trigger terms",
        )?;
        ensure(
            generated[0].provenance.clone(),
            vec![
                "source_kind=dependency_contract".to_string(),
                "source_id=dep_no_tokio".to_string(),
                "source_score=1.000".to_string(),
            ],
            "generated provenance",
        )
    }

    #[test]
    fn regret_entries_generate_halting_tripwires_for_harmful_regret() -> TestResult {
        let entry = LedgerRegretEntry::new(
            "reg_bad_cleanup",
            "ep_cleanup",
            "cfr_cleanup",
            "int_missing_warning",
            0.9,
            0.95,
            RegretCategory::Misinformation,
            "2026-04-30T12:00:00Z",
        );
        let source = TripwireSource::regret_entry(
            &entry,
            "Wrong cleanup guidance would have caused data loss",
            ["cleanup"],
        );
        let generated = generate_tripwires_from_sources(
            "pf_cleanup",
            "perform cleanup",
            "2026-04-30T12:00:00Z",
            &[source],
            &TripwireGenerationConfig::default(),
        );

        ensure(generated.len(), 1, "generated count")?;
        ensure(generated[0].risk_level, RiskLevel::Critical, "risk level")?;
        ensure(
            generated[0].tripwire.action,
            TripwireAction::Halt,
            "halt action",
        )?;
        ensure(
            generated[0]
                .tripwire
                .message
                .as_ref()
                .is_some_and(|message| message.contains("regret_ledger_entry")),
            true,
            "source provenance in message",
        )
    }

    #[test]
    fn claim_entries_generate_pause_tripwires_for_regressed_claims() -> TestResult {
        let claim_id = crate::models::ClaimId::from_str("claim_00000000000000000000000000")
            .map_err(|err| err.to_string())?;
        let mut claim = ClaimEntry::new(
            claim_id,
            "Release workflow remains reproducible".to_string(),
            "Release artifacts should be generated from the documented workflow".to_string(),
        );
        claim.status = ClaimStatus::Regressed;

        let source = TripwireSource::claim_entry(&claim, 0.9, ["release"]);

        ensure(source.kind, TripwireSourceKind::Claim, "kind")?;
        ensure(source.risk_level, RiskLevel::High, "risk")?;
        ensure(source.action, TripwireAction::Pause, "action")
    }

    #[test]
    fn run_preflight_counts_generated_tripwires_from_sources() -> TestResult {
        let source = TripwireSource::dependency_contract(
            "dep_forbidden_runtime",
            "Forbidden runtime dependency must not be introduced",
            true,
            ["release"],
        );
        let options = RunOptions {
            task_input: "prepare release".to_string(),
            tripwire_sources: vec![source],
            ..Default::default()
        };

        let report = run_preflight(&options).map_err(|err| err.message())?;

        ensure(report.tripwires_set, 1, "tripwires_set")?;
        ensure(report.tripwires.len(), 1, "tripwire views")?;
        ensure(
            report.ask_now_prompts.is_empty(),
            true,
            "no generated ask-now prompts",
        )?;
        ensure(report.risks_identified, 1, "risk count from evidence")?;
        ensure(
            report.tripwires[0].tripwire_type.clone(),
            TripwireType::FileChange.as_str().to_string(),
            "tripwire type",
        )?;
        ensure(
            report.tripwires[0].source_kind.clone(),
            Some("dependency_contract".to_string()),
            "tripwire source kind",
        )?;
        ensure(
            report.tripwires[0].source_id.clone(),
            Some("dep_forbidden_runtime".to_string()),
            "tripwire source id",
        )?;
        ensure(
            report.tripwires[0].source_score,
            Some(1.0),
            "tripwire source score",
        )?;
        ensure(
            report.tripwires[0].trigger_terms.clone(),
            vec!["release".to_string()],
            "tripwire trigger terms",
        )?;
        ensure(
            report.tripwires[0].provenance.clone(),
            vec![
                "source_kind=dependency_contract".to_string(),
                "source_id=dep_forbidden_runtime".to_string(),
                "source_score=1.000".to_string(),
            ],
            "tripwire provenance",
        )
    }

    #[test]
    fn run_preflight_respects_disabled_tripwire_checks() -> TestResult {
        let source = TripwireSource::dependency_contract(
            "dep_forbidden_runtime",
            "Forbidden runtime dependency must not be introduced",
            true,
            ["release"],
        );
        let options = RunOptions {
            task_input: "prepare release".to_string(),
            check_tripwires: false,
            tripwire_sources: vec![source],
            ..Default::default()
        };

        let report = run_preflight(&options).map_err(|err| err.message())?;

        ensure(report.tripwires_set, 0, "tripwires disabled")?;
        ensure(report.tripwires.is_empty(), true, "no tripwire views")
    }
}
