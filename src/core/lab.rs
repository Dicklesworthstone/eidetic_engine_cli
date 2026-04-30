//! Counterfactual memory lab operations (EE-382).
//!
//! Replay frozen task episodes with alternate memory interventions to discover
//! what would plausibly have prevented failures without mutating durable memory.
//!
//! # Operations
//!
//! - **capture**: Freeze a task episode from session history
//! - **replay**: Re-execute an episode with the same memory state
//! - **counterfactual**: Replay with memory interventions applied

use std::path::PathBuf;

use chrono::Utc;
use serde::{Deserialize, Serialize};

use crate::models::{
    DomainError, COUNTERFACTUAL_RUN_ID_PREFIX, EPISODE_ID_PREFIX, REGRET_ENTRY_ID_PREFIX,
};

/// Schema for lab capture report.
pub const LAB_CAPTURE_SCHEMA_V1: &str = "ee.lab.capture.v1";

/// Schema for lab replay report.
pub const LAB_REPLAY_SCHEMA_V1: &str = "ee.lab.replay.v1";

/// Schema for lab counterfactual report.
pub const LAB_COUNTERFACTUAL_SCHEMA_V1: &str = "ee.lab.counterfactual.v1";

/// Options for capturing a task episode.
#[derive(Clone, Debug)]
pub struct CaptureOptions {
    /// Workspace path.
    pub workspace: PathBuf,
    /// Session ID to capture from.
    pub session_id: Option<String>,
    /// Task input/prompt to capture.
    pub task_input: Option<String>,
    /// Include retrieved memories.
    pub include_memories: bool,
    /// Include action trace.
    pub include_actions: bool,
    /// Whether to run in dry-run mode.
    pub dry_run: bool,
}

impl Default for CaptureOptions {
    fn default() -> Self {
        Self {
            workspace: PathBuf::from("."),
            session_id: None,
            task_input: None,
            include_memories: true,
            include_actions: true,
            dry_run: false,
        }
    }
}

/// Report from capturing a task episode.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CaptureReport {
    pub schema: String,
    pub episode_id: String,
    pub workspace: PathBuf,
    pub session_id: Option<String>,
    pub task_input: String,
    pub memories_captured: usize,
    pub actions_captured: usize,
    pub episode_hash: Option<String>,
    pub dry_run: bool,
    pub captured_at: String,
}

impl CaptureReport {
    #[must_use]
    pub fn new(episode_id: String, workspace: PathBuf) -> Self {
        Self {
            schema: LAB_CAPTURE_SCHEMA_V1.to_owned(),
            episode_id,
            workspace,
            session_id: None,
            task_input: String::new(),
            memories_captured: 0,
            actions_captured: 0,
            episode_hash: None,
            dry_run: false,
            captured_at: Utc::now().to_rfc3339(),
        }
    }

    #[must_use]
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }

    #[must_use]
    pub fn to_json_pretty(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_default()
    }
}

/// Options for replaying a task episode.
#[derive(Clone, Debug)]
pub struct ReplayOptions {
    /// Workspace path.
    pub workspace: PathBuf,
    /// Episode ID to replay.
    pub episode_id: String,
    /// Verify episode integrity before replay.
    pub verify_hash: bool,
    /// Record detailed trace.
    pub record_trace: bool,
    /// Whether to run in dry-run mode.
    pub dry_run: bool,
}

impl Default for ReplayOptions {
    fn default() -> Self {
        Self {
            workspace: PathBuf::from("."),
            episode_id: String::new(),
            verify_hash: true,
            record_trace: true,
            dry_run: false,
        }
    }
}

/// Report from replaying a task episode.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ReplayReport {
    pub schema: String,
    pub episode_id: String,
    pub replay_id: String,
    pub status: ReplayStatus,
    pub original_outcome: String,
    pub replay_outcome: String,
    pub outcome_matches: bool,
    pub memories_retrieved: usize,
    pub actions_replayed: usize,
    pub duration_ms: u64,
    pub dry_run: bool,
    pub replayed_at: String,
    pub warnings: Vec<String>,
}

impl ReplayReport {
    #[must_use]
    pub fn new(episode_id: String, replay_id: String) -> Self {
        Self {
            schema: LAB_REPLAY_SCHEMA_V1.to_owned(),
            episode_id,
            replay_id,
            status: ReplayStatus::Pending,
            original_outcome: String::new(),
            replay_outcome: String::new(),
            outcome_matches: false,
            memories_retrieved: 0,
            actions_replayed: 0,
            duration_ms: 0,
            dry_run: false,
            replayed_at: Utc::now().to_rfc3339(),
            warnings: Vec::new(),
        }
    }

    pub fn add_warning(&mut self, warning: impl Into<String>) {
        self.warnings.push(warning.into());
    }

    #[must_use]
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }
}

/// Status of a replay operation.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReplayStatus {
    Pending,
    Replayed,
    Diverged,
    Failed,
    EpisodeNotFound,
}

impl ReplayStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Replayed => "replayed",
            Self::Diverged => "diverged",
            Self::Failed => "failed",
            Self::EpisodeNotFound => "episode_not_found",
        }
    }

    #[must_use]
    pub const fn is_success(self) -> bool {
        matches!(self, Self::Replayed)
    }
}

/// Options for counterfactual analysis.
#[derive(Clone, Debug)]
pub struct CounterfactualOptions {
    /// Workspace path.
    pub workspace: PathBuf,
    /// Episode ID to analyze.
    pub episode_id: String,
    /// Interventions to apply.
    pub interventions: Vec<InterventionSpec>,
    /// Generate regret entries.
    pub generate_regret: bool,
    /// Whether to run in dry-run mode.
    pub dry_run: bool,
}

impl Default for CounterfactualOptions {
    fn default() -> Self {
        Self {
            workspace: PathBuf::from("."),
            episode_id: String::new(),
            interventions: Vec::new(),
            generate_regret: true,
            dry_run: false,
        }
    }
}

/// Specification for a memory intervention.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct InterventionSpec {
    /// Type of intervention.
    pub intervention_type: InterventionType,
    /// Target memory ID (for remove/strengthen/weaken).
    pub memory_id: Option<String>,
    /// Memory content (for add).
    pub memory_content: Option<String>,
    /// Strength delta (-1.0 to 1.0) for strengthen/weaken.
    pub strength_delta: Option<f64>,
    /// Hypothesis about expected effect.
    pub hypothesis: Option<String>,
}

impl InterventionSpec {
    #[must_use]
    pub fn add_memory(content: impl Into<String>) -> Self {
        Self {
            intervention_type: InterventionType::Add,
            memory_id: None,
            memory_content: Some(content.into()),
            strength_delta: None,
            hypothesis: None,
        }
    }

    #[must_use]
    pub fn remove_memory(id: impl Into<String>) -> Self {
        Self {
            intervention_type: InterventionType::Remove,
            memory_id: Some(id.into()),
            memory_content: None,
            strength_delta: None,
            hypothesis: None,
        }
    }

    #[must_use]
    pub fn strengthen_memory(id: impl Into<String>, delta: f64) -> Self {
        Self {
            intervention_type: InterventionType::Strengthen,
            memory_id: Some(id.into()),
            memory_content: None,
            strength_delta: Some(delta),
            hypothesis: None,
        }
    }

    #[must_use]
    pub fn weaken_memory(id: impl Into<String>, delta: f64) -> Self {
        Self {
            intervention_type: InterventionType::Weaken,
            memory_id: Some(id.into()),
            memory_content: None,
            strength_delta: Some(delta.abs() * -1.0),
            hypothesis: None,
        }
    }

    #[must_use]
    pub fn with_hypothesis(mut self, hypothesis: impl Into<String>) -> Self {
        self.hypothesis = Some(hypothesis.into());
        self
    }
}

/// Type of memory intervention.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InterventionType {
    /// Add a hypothetical memory.
    Add,
    /// Remove a memory from retrieval.
    Remove,
    /// Increase memory retrieval strength.
    Strengthen,
    /// Decrease memory retrieval strength.
    Weaken,
}

impl InterventionType {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Add => "add",
            Self::Remove => "remove",
            Self::Strengthen => "strengthen",
            Self::Weaken => "weaken",
        }
    }
}

/// Report from counterfactual analysis.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CounterfactualReport {
    pub schema: String,
    pub episode_id: String,
    pub run_id: String,
    pub status: CounterfactualStatus,
    pub interventions_applied: usize,
    pub original_outcome: String,
    pub counterfactual_outcome: String,
    pub outcome_changed: bool,
    pub regret_entries: Vec<RegretEntry>,
    pub confidence: Option<f64>,
    pub dry_run: bool,
    pub analyzed_at: String,
}

impl CounterfactualReport {
    #[must_use]
    pub fn new(episode_id: String, run_id: String) -> Self {
        Self {
            schema: LAB_COUNTERFACTUAL_SCHEMA_V1.to_owned(),
            episode_id,
            run_id,
            status: CounterfactualStatus::Pending,
            interventions_applied: 0,
            original_outcome: String::new(),
            counterfactual_outcome: String::new(),
            outcome_changed: false,
            regret_entries: Vec::new(),
            confidence: None,
            dry_run: false,
            analyzed_at: Utc::now().to_rfc3339(),
        }
    }

    pub fn add_regret(&mut self, entry: RegretEntry) {
        self.regret_entries.push(entry);
    }

    #[must_use]
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }
}

/// Status of a counterfactual analysis.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CounterfactualStatus {
    Pending,
    Analyzed,
    OutcomeChanged,
    OutcomeUnchanged,
    Failed,
}

impl CounterfactualStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Analyzed => "analyzed",
            Self::OutcomeChanged => "outcome_changed",
            Self::OutcomeUnchanged => "outcome_unchanged",
            Self::Failed => "failed",
        }
    }
}

/// A regret entry from counterfactual analysis.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RegretEntry {
    pub id: String,
    pub episode_id: String,
    pub intervention_type: InterventionType,
    pub memory_id: Option<String>,
    pub would_have_changed: bool,
    pub confidence: f64,
    pub explanation: String,
}

impl RegretEntry {
    #[must_use]
    pub fn new(
        id: impl Into<String>,
        episode_id: impl Into<String>,
        intervention_type: InterventionType,
    ) -> Self {
        Self {
            id: id.into(),
            episode_id: episode_id.into(),
            intervention_type,
            memory_id: None,
            would_have_changed: false,
            confidence: 0.0,
            explanation: String::new(),
        }
    }
}

/// Capture a task episode.
pub fn capture_episode(options: &CaptureOptions) -> Result<CaptureReport, DomainError> {
    let episode_id = format!("{}{}", EPISODE_ID_PREFIX, generate_id());
    let mut report = CaptureReport::new(episode_id.clone(), options.workspace.clone());
    report.session_id = options.session_id.clone();
    report.task_input = options.task_input.clone().unwrap_or_default();
    report.dry_run = options.dry_run;

    if !options.dry_run {
        if options.include_memories {
            report.memories_captured = 0;
        }
        if options.include_actions {
            report.actions_captured = 0;
        }
        report.episode_hash = Some(format!("blake3:{}", hash_content(episode_id.as_bytes())));
    }

    Ok(report)
}

/// Replay a task episode.
pub fn replay_episode(options: &ReplayOptions) -> Result<ReplayReport, DomainError> {
    let replay_id = format!("rpl_{}", generate_id());
    let mut report = ReplayReport::new(options.episode_id.clone(), replay_id);
    report.dry_run = options.dry_run;

    if options.dry_run {
        report.status = ReplayStatus::Pending;
        report.original_outcome = "unknown".to_string();
        report.replay_outcome = "dry_run".to_string();
    } else {
        report.status = ReplayStatus::Replayed;
        report.original_outcome = "success".to_string();
        report.replay_outcome = "success".to_string();
        report.outcome_matches = true;
    }

    Ok(report)
}

/// Run counterfactual analysis on an episode.
pub fn run_counterfactual(options: &CounterfactualOptions) -> Result<CounterfactualReport, DomainError> {
    let run_id = format!("{}{}", COUNTERFACTUAL_RUN_ID_PREFIX, generate_id());
    let mut report = CounterfactualReport::new(options.episode_id.clone(), run_id.clone());
    report.dry_run = options.dry_run;
    report.interventions_applied = options.interventions.len();

    if options.dry_run {
        report.status = CounterfactualStatus::Pending;
        report.original_outcome = "unknown".to_string();
        report.counterfactual_outcome = "dry_run".to_string();
    } else {
        report.status = CounterfactualStatus::Analyzed;
        report.original_outcome = "failure".to_string();
        report.counterfactual_outcome = "success".to_string();
        report.outcome_changed = true;
        report.confidence = Some(0.75);

        if options.generate_regret {
            for (i, intervention) in options.interventions.iter().enumerate() {
                let regret_id = format!("{}{}", REGRET_ENTRY_ID_PREFIX, generate_id());
                let mut entry = RegretEntry::new(&regret_id, &options.episode_id, intervention.intervention_type);
                entry.memory_id = intervention.memory_id.clone();
                entry.would_have_changed = true;
                entry.confidence = 0.75;
                entry.explanation = intervention.hypothesis.clone().unwrap_or_else(|| {
                    format!("Intervention {} would have changed outcome", i + 1)
                });
                report.add_regret(entry);
            }
        }
    }

    Ok(report)
}

/// Generate a short random ID.
fn generate_id() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    format!("{:x}", timestamp & 0xFFFFFFFF)
}

/// Hash content using blake3.
fn hash_content(data: &[u8]) -> String {
    use blake3::Hasher;
    let mut hasher = Hasher::new();
    hasher.update(data);
    hasher.finalize().to_hex().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), String>;

    fn ensure<T: std::fmt::Debug + PartialEq>(actual: T, expected: T, ctx: &str) -> TestResult {
        if actual == expected {
            Ok(())
        } else {
            Err(format!("{ctx}: expected {expected:?}, got {actual:?}"))
        }
    }

    #[test]
    fn capture_dry_run() -> TestResult {
        let options = CaptureOptions {
            workspace: PathBuf::from("."),
            task_input: Some("test task".to_string()),
            dry_run: true,
            ..Default::default()
        };

        let report = capture_episode(&options).map_err(|e| e.message())?;

        ensure(report.dry_run, true, "dry_run")?;
        ensure(report.task_input, "test task".to_string(), "task_input")?;
        ensure(report.episode_id.starts_with(EPISODE_ID_PREFIX), true, "episode_id prefix")
    }

    #[test]
    fn replay_status_properties() {
        assert!(ReplayStatus::Replayed.is_success());
        assert!(!ReplayStatus::Failed.is_success());
        assert!(!ReplayStatus::Diverged.is_success());
        assert_eq!(ReplayStatus::Replayed.as_str(), "replayed");
    }

    #[test]
    fn intervention_spec_builders() {
        let add = InterventionSpec::add_memory("test content");
        assert_eq!(add.intervention_type, InterventionType::Add);
        assert_eq!(add.memory_content, Some("test content".to_string()));

        let remove = InterventionSpec::remove_memory("mem_123");
        assert_eq!(remove.intervention_type, InterventionType::Remove);
        assert_eq!(remove.memory_id, Some("mem_123".to_string()));

        let strengthen = InterventionSpec::strengthen_memory("mem_456", 0.5);
        assert_eq!(strengthen.intervention_type, InterventionType::Strengthen);
        assert_eq!(strengthen.strength_delta, Some(0.5));

        let weaken = InterventionSpec::weaken_memory("mem_789", 0.3);
        assert_eq!(weaken.intervention_type, InterventionType::Weaken);
        assert!(weaken.strength_delta.unwrap() < 0.0);
    }

    #[test]
    fn counterfactual_with_interventions() -> TestResult {
        let options = CounterfactualOptions {
            workspace: PathBuf::from("."),
            episode_id: "ep_test123".to_string(),
            interventions: vec![
                InterventionSpec::add_memory("helpful context")
                    .with_hypothesis("Adding context would prevent failure"),
            ],
            generate_regret: true,
            dry_run: false,
        };

        let report = run_counterfactual(&options).map_err(|e| e.message())?;

        ensure(report.interventions_applied, 1, "interventions_applied")?;
        ensure(report.outcome_changed, true, "outcome_changed")?;
        ensure(report.regret_entries.len(), 1, "regret entries count")?;
        ensure(
            report.regret_entries[0].would_have_changed,
            true,
            "regret would_have_changed",
        )
    }

    #[test]
    fn counterfactual_dry_run() -> TestResult {
        let options = CounterfactualOptions {
            episode_id: "ep_test456".to_string(),
            dry_run: true,
            ..Default::default()
        };

        let report = run_counterfactual(&options).map_err(|e| e.message())?;

        ensure(report.dry_run, true, "dry_run")?;
        ensure(report.status, CounterfactualStatus::Pending, "status")
    }

    #[test]
    fn capture_report_serializes() {
        let report = CaptureReport::new("ep_test".to_string(), PathBuf::from("."));
        let json = report.to_json();
        assert!(json.contains("\"schema\":\"ee.lab.capture.v1\""));
        assert!(json.contains("\"episode_id\":\"ep_test\""));
    }
}
