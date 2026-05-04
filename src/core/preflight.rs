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

use std::path::PathBuf;

use chrono::Utc;
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

/// Default minimum score for turning evidence into a tripwire.
pub const DEFAULT_TRIPWIRE_SOURCE_SCORE: f64 = 0.5;

/// Default maximum number of generated tripwires per run.
pub const DEFAULT_MAX_GENERATED_TRIPWIRES: usize = 8;

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
        serde_json::to_string(self).unwrap_or_default()
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
        serde_json::to_string(self).unwrap_or_default()
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
        serde_json::to_string(self).unwrap_or_default()
    }
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
        ClaimStatus::Regressed => (RiskLevel::High, TripwireAction::Pause),
        ClaimStatus::Stale => (RiskLevel::High, TripwireAction::Warn),
        ClaimStatus::Draft => (RiskLevel::Medium, TripwireAction::Audit),
        ClaimStatus::Active | ClaimStatus::Verified => {
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

/// Run a preflight risk assessment.
pub fn run_preflight(options: &RunOptions) -> Result<RunReport, DomainError> {
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

    report.status = PreflightStatus::Completed.as_str().to_owned();
    report.completed_at = Some(Utc::now().to_rfc3339());

    Ok(report)
}

/// Show details of a preflight run.
pub fn show_preflight(options: &ShowOptions) -> Result<ShowReport, DomainError> {
    validate_preflight_run_id(&options.run_id)?;
    Err(preflight_run_not_found(&options.run_id))
}

/// Close a preflight run.
pub fn close_preflight(options: &CloseOptions) -> Result<CloseReport, DomainError> {
    validate_preflight_run_id(&options.run_id)?;
    Err(preflight_run_not_found(&options.run_id))
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
            "Persisted preflight run storage is not wired; run preflight with explicit evidence before show/close."
                .to_owned(),
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
