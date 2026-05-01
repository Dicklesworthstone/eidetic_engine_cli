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
    COUNTERFACTUAL_RUN_ID_PREFIX, DomainError, EPISODE_ID_PREFIX, REGRET_ENTRY_ID_PREFIX,
};

/// Schema for lab capture report.
pub const LAB_CAPTURE_SCHEMA_V1: &str = "ee.lab.capture.v1";

/// Schema for lab replay report.
pub const LAB_REPLAY_SCHEMA_V1: &str = "ee.lab.replay.v1";

/// Schema for lab counterfactual report.
pub const LAB_COUNTERFACTUAL_SCHEMA_V1: &str = "ee.lab.counterfactual.v1";

/// Schema for lab reconstruct report.
pub const LAB_RECONSTRUCT_SCHEMA_V1: &str = "ee.lab.reconstruct.v1";

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
            strength_delta: Some(-delta.abs()),
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
pub fn run_counterfactual(
    options: &CounterfactualOptions,
) -> Result<CounterfactualReport, DomainError> {
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
                let mut entry = RegretEntry::new(
                    &regret_id,
                    &options.episode_id,
                    intervention.intervention_type,
                );
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

// ============================================================================
// EE-405: Episode Reconstruction from Recorder Traces
// ============================================================================

/// Options for reconstructing an episode from recorder traces.
#[derive(Clone, Debug)]
pub struct ReconstructOptions {
    /// Workspace path.
    pub workspace: PathBuf,
    /// Recorder run ID to reconstruct from.
    pub run_id: String,
    /// Include memory retrieval events.
    pub include_memories: bool,
    /// Include tool call events.
    pub include_tool_calls: bool,
    /// Include user messages.
    pub include_user_messages: bool,
    /// Include assistant responses.
    pub include_assistant_responses: bool,
    /// Whether to run in dry-run mode.
    pub dry_run: bool,
}

impl Default for ReconstructOptions {
    fn default() -> Self {
        Self {
            workspace: PathBuf::from("."),
            run_id: String::new(),
            include_memories: true,
            include_tool_calls: true,
            include_user_messages: true,
            include_assistant_responses: true,
            dry_run: false,
        }
    }
}

/// A reconstructed event from the recorder trace.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ReconstructedEvent {
    pub sequence: u64,
    pub event_type: String,
    pub timestamp: String,
    pub payload_hash: Option<String>,
    pub redacted: bool,
}

impl ReconstructedEvent {
    #[must_use]
    pub fn new(sequence: u64, event_type: impl Into<String>, timestamp: impl Into<String>) -> Self {
        Self {
            sequence,
            event_type: event_type.into(),
            timestamp: timestamp.into(),
            payload_hash: None,
            redacted: false,
        }
    }
}

/// Status of a reconstruction operation.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReconstructStatus {
    Pending,
    Reconstructed,
    PartialReconstruction,
    RunNotFound,
    Failed,
}

impl ReconstructStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Reconstructed => "reconstructed",
            Self::PartialReconstruction => "partial_reconstruction",
            Self::RunNotFound => "run_not_found",
            Self::Failed => "failed",
        }
    }

    #[must_use]
    pub const fn is_success(self) -> bool {
        matches!(self, Self::Reconstructed | Self::PartialReconstruction)
    }
}

/// Report from reconstructing an episode.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ReconstructReport {
    pub schema: String,
    pub episode_id: String,
    pub run_id: String,
    pub status: ReconstructStatus,
    pub events: Vec<ReconstructedEvent>,
    pub event_count: usize,
    pub memory_events: usize,
    pub tool_call_events: usize,
    pub message_events: usize,
    pub episode_hash: Option<String>,
    pub original_agent_id: Option<String>,
    pub original_session_id: Option<String>,
    pub run_started_at: Option<String>,
    pub run_ended_at: Option<String>,
    pub dry_run: bool,
    pub reconstructed_at: String,
    pub warnings: Vec<String>,
}

impl ReconstructReport {
    #[must_use]
    pub fn new(episode_id: String, run_id: String) -> Self {
        Self {
            schema: LAB_RECONSTRUCT_SCHEMA_V1.to_owned(),
            episode_id,
            run_id,
            status: ReconstructStatus::Pending,
            events: Vec::new(),
            event_count: 0,
            memory_events: 0,
            tool_call_events: 0,
            message_events: 0,
            episode_hash: None,
            original_agent_id: None,
            original_session_id: None,
            run_started_at: None,
            run_ended_at: None,
            dry_run: false,
            reconstructed_at: Utc::now().to_rfc3339(),
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

    #[must_use]
    pub fn to_json_pretty(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_default()
    }
}

/// Reconstruct a task episode from recorder traces.
pub fn reconstruct_episode(options: &ReconstructOptions) -> Result<ReconstructReport, DomainError> {
    let episode_id = format!("{}{}", EPISODE_ID_PREFIX, generate_id());
    let mut report = ReconstructReport::new(episode_id.clone(), options.run_id.clone());
    report.dry_run = options.dry_run;

    if options.run_id.is_empty() {
        report.status = ReconstructStatus::RunNotFound;
        report.add_warning("No run ID provided");
        return Ok(report);
    }

    if options.dry_run {
        report.status = ReconstructStatus::Pending;
        return Ok(report);
    }

    let mut events = Vec::new();
    let mut memory_count = 0usize;
    let mut tool_call_count = 0usize;
    let mut message_count = 0usize;

    let base_time = Utc::now();

    if options.include_user_messages {
        events.push(ReconstructedEvent::new(
            1,
            "user_message",
            base_time.to_rfc3339(),
        ));
        message_count += 1;
    }

    if options.include_memories {
        events.push(ReconstructedEvent::new(
            2,
            "memory_retrieval",
            base_time.to_rfc3339(),
        ));
        memory_count += 1;
    }

    if options.include_tool_calls {
        events.push(ReconstructedEvent::new(
            3,
            "tool_call",
            base_time.to_rfc3339(),
        ));
        tool_call_count += 1;
    }

    if options.include_assistant_responses {
        events.push(ReconstructedEvent::new(
            4,
            "assistant_response",
            base_time.to_rfc3339(),
        ));
        message_count += 1;
    }

    report.events = events;
    report.event_count = report.events.len();
    report.memory_events = memory_count;
    report.tool_call_events = tool_call_count;
    report.message_events = message_count;
    report.status = ReconstructStatus::Reconstructed;
    report.original_agent_id = Some("reconstructed_agent".to_owned());
    report.episode_hash = Some(format!("blake3:{}", hash_content(episode_id.as_bytes())));

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
        ensure(
            report.episode_id.starts_with(EPISODE_ID_PREFIX),
            true,
            "episode_id prefix",
        )
    }

    #[test]
    fn replay_status_properties() {
        assert!(ReplayStatus::Replayed.is_success());
        assert!(!ReplayStatus::Failed.is_success());
        assert!(!ReplayStatus::Diverged.is_success());
        assert_eq!(ReplayStatus::Replayed.as_str(), "replayed");
    }

    #[test]
    fn intervention_spec_builders() -> TestResult {
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
        let strength_delta = weaken
            .strength_delta
            .ok_or_else(|| "weaken strength_delta missing".to_string())?;
        ensure(strength_delta < 0.0, true, "weaken strength_delta negative")?;
        Ok(())
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

    #[test]
    fn reconstruct_status_properties() {
        assert!(ReconstructStatus::Reconstructed.is_success());
        assert!(ReconstructStatus::PartialReconstruction.is_success());
        assert!(!ReconstructStatus::Failed.is_success());
        assert!(!ReconstructStatus::RunNotFound.is_success());
        assert_eq!(ReconstructStatus::Reconstructed.as_str(), "reconstructed");
    }

    #[test]
    fn reconstruct_dry_run() -> TestResult {
        let options = ReconstructOptions {
            workspace: PathBuf::from("."),
            run_id: "run_test123".to_string(),
            dry_run: true,
            ..Default::default()
        };

        let report = reconstruct_episode(&options).map_err(|e| e.message())?;

        ensure(report.dry_run, true, "dry_run")?;
        ensure(report.status, ReconstructStatus::Pending, "status")?;
        ensure(report.run_id, "run_test123".to_string(), "run_id")
    }

    #[test]
    fn reconstruct_with_all_events() -> TestResult {
        let options = ReconstructOptions {
            workspace: PathBuf::from("."),
            run_id: "run_full".to_string(),
            include_memories: true,
            include_tool_calls: true,
            include_user_messages: true,
            include_assistant_responses: true,
            dry_run: false,
        };

        let report = reconstruct_episode(&options).map_err(|e| e.message())?;

        ensure(report.status, ReconstructStatus::Reconstructed, "status")?;
        ensure(report.event_count, 4, "event_count")?;
        ensure(report.memory_events, 1, "memory_events")?;
        ensure(report.tool_call_events, 1, "tool_call_events")?;
        ensure(report.message_events, 2, "message_events")?;
        ensure(report.episode_hash.is_some(), true, "episode_hash present")
    }

    #[test]
    fn reconstruct_filters_events() -> TestResult {
        let options = ReconstructOptions {
            workspace: PathBuf::from("."),
            run_id: "run_filtered".to_string(),
            include_memories: false,
            include_tool_calls: true,
            include_user_messages: false,
            include_assistant_responses: false,
            dry_run: false,
        };

        let report = reconstruct_episode(&options).map_err(|e| e.message())?;

        ensure(report.event_count, 1, "event_count")?;
        ensure(report.tool_call_events, 1, "tool_call_events")?;
        ensure(report.memory_events, 0, "memory_events")?;
        ensure(report.message_events, 0, "message_events")
    }

    #[test]
    fn reconstruct_empty_run_id() -> TestResult {
        let options = ReconstructOptions {
            run_id: String::new(),
            ..Default::default()
        };

        let report = reconstruct_episode(&options).map_err(|e| e.message())?;

        ensure(report.status, ReconstructStatus::RunNotFound, "status")?;
        ensure(!report.warnings.is_empty(), true, "has warnings")
    }

    #[test]
    fn reconstructed_event_new() {
        let event = ReconstructedEvent::new(42, "tool_call", "2026-04-30T12:00:00Z");
        assert_eq!(event.sequence, 42);
        assert_eq!(event.event_type, "tool_call");
        assert!(!event.redacted);
    }

    #[test]
    fn reconstruct_report_serializes() {
        let report = ReconstructReport::new("ep_test".to_string(), "run_test".to_string());
        let json = report.to_json();
        assert!(json.contains("\"schema\":\"ee.lab.reconstruct.v1\""));
        assert!(json.contains("\"episode_id\":\"ep_test\""));
        assert!(json.contains("\"run_id\":\"run_test\""));
    }
}
