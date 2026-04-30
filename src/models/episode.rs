//! Task episode and counterfactual memory schemas (EE-380).
//!
//! Replay frozen task episodes with alternate memory interventions to
//! discover what would plausibly have prevented failures without mutating
//! durable memory state.
//!
//! Core concepts:
//!
//! * **Task episode**: A frozen snapshot of a task execution including
//!   inputs, context, actions, and outcome. Episodes are immutable once
//!   captured.
//! * **Intervention**: A hypothetical memory mutation (add, remove,
//!   strengthen, weaken) to test counterfactual scenarios.
//! * **Counterfactual run**: Replay an episode with one or more
//!   interventions applied, producing a hypothetical outcome.
//! * **Regret ledger**: A record of interventions that would have
//!   plausibly changed outcomes, with confidence scores.

use std::fmt;
use std::str::FromStr;

/// Schema version for task episode.
pub const TASK_EPISODE_SCHEMA_V1: &str = "ee.task_episode.v1";

/// Schema version for intervention.
pub const INTERVENTION_SCHEMA_V1: &str = "ee.intervention.v1";

/// Schema version for counterfactual run.
pub const COUNTERFACTUAL_RUN_SCHEMA_V1: &str = "ee.counterfactual_run.v1";

/// Schema version for regret ledger.
pub const REGRET_LEDGER_SCHEMA_V1: &str = "ee.regret_ledger.v1";

/// Schema version for regret entry.
pub const REGRET_ENTRY_SCHEMA_V1: &str = "ee.regret_entry.v1";

/// ID prefix for task episodes.
pub const EPISODE_ID_PREFIX: &str = "ep_";

/// ID prefix for interventions.
pub const INTERVENTION_ID_PREFIX: &str = "int_";

/// ID prefix for counterfactual runs.
pub const COUNTERFACTUAL_RUN_ID_PREFIX: &str = "cfr_";

/// ID prefix for regret entries.
pub const REGRET_ENTRY_ID_PREFIX: &str = "reg_";

/// A frozen snapshot of a task execution.
///
/// Episodes capture the complete context of a task at execution time,
/// including the input prompt, retrieved memories, actions taken, and
/// final outcome. They are immutable once captured and serve as the
/// baseline for counterfactual analysis.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct TaskEpisode {
    /// Schema identifier.
    pub schema: &'static str,
    /// Unique episode ID.
    pub id: String,
    /// Workspace where this episode occurred.
    pub workspace_id: Option<String>,
    /// Session ID if imported from CASS.
    pub session_id: Option<String>,
    /// The original task input/prompt.
    pub task_input: String,
    /// IDs of memories retrieved during this task.
    pub retrieved_memory_ids: Vec<String>,
    /// Context pack ID if one was generated.
    pub context_pack_id: Option<String>,
    /// Sequence of actions taken during the task.
    pub actions: Vec<EpisodeAction>,
    /// Final outcome of the task.
    pub outcome: EpisodeOutcome,
    /// Timestamp when task started (RFC 3339).
    pub started_at: String,
    /// Timestamp when task ended (RFC 3339).
    pub ended_at: Option<String>,
    /// Duration in milliseconds.
    pub duration_ms: Option<u64>,
    /// Agent that executed this task.
    pub agent: Option<String>,
    /// Hash of the frozen episode for integrity.
    pub episode_hash: Option<String>,
}

impl TaskEpisode {
    /// Create a new task episode.
    #[must_use]
    pub fn new(
        id: impl Into<String>,
        task_input: impl Into<String>,
        started_at: impl Into<String>,
    ) -> Self {
        Self {
            schema: TASK_EPISODE_SCHEMA_V1,
            id: id.into(),
            task_input: task_input.into(),
            started_at: started_at.into(),
            outcome: EpisodeOutcome::Unknown,
            ..Default::default()
        }
    }

    /// Set the workspace ID.
    #[must_use]
    pub fn with_workspace_id(mut self, id: impl Into<String>) -> Self {
        self.workspace_id = Some(id.into());
        self
    }

    /// Set the session ID.
    #[must_use]
    pub fn with_session_id(mut self, id: impl Into<String>) -> Self {
        self.session_id = Some(id.into());
        self
    }

    /// Set the context pack ID.
    #[must_use]
    pub fn with_context_pack_id(mut self, id: impl Into<String>) -> Self {
        self.context_pack_id = Some(id.into());
        self
    }

    /// Add a retrieved memory ID.
    pub fn add_retrieved_memory(&mut self, id: impl Into<String>) {
        self.retrieved_memory_ids.push(id.into());
    }

    /// Add an action.
    pub fn add_action(&mut self, action: EpisodeAction) {
        self.actions.push(action);
    }

    /// Set the outcome.
    #[must_use]
    pub fn with_outcome(mut self, outcome: EpisodeOutcome) -> Self {
        self.outcome = outcome;
        self
    }

    /// Set the ended timestamp.
    #[must_use]
    pub fn with_ended_at(mut self, ts: impl Into<String>) -> Self {
        self.ended_at = Some(ts.into());
        self
    }

    /// Set the duration.
    #[must_use]
    pub fn with_duration_ms(mut self, ms: u64) -> Self {
        self.duration_ms = Some(ms);
        self
    }

    /// Set the agent.
    #[must_use]
    pub fn with_agent(mut self, agent: impl Into<String>) -> Self {
        self.agent = Some(agent.into());
        self
    }

    /// Set the episode hash.
    #[must_use]
    pub fn with_episode_hash(mut self, hash: impl Into<String>) -> Self {
        self.episode_hash = Some(hash.into());
        self
    }
}

/// An action taken during a task episode.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EpisodeAction {
    /// Action sequence number within the episode.
    pub sequence: u32,
    /// Type of action (tool_call, edit, command, etc.).
    pub action_type: ActionType,
    /// Brief description of the action.
    pub description: String,
    /// Timestamp (RFC 3339).
    pub timestamp: String,
    /// Whether the action succeeded.
    pub succeeded: bool,
    /// Error message if action failed.
    pub error: Option<String>,
}

impl EpisodeAction {
    /// Create a new action.
    #[must_use]
    pub fn new(
        sequence: u32,
        action_type: ActionType,
        description: impl Into<String>,
        timestamp: impl Into<String>,
    ) -> Self {
        Self {
            sequence,
            action_type,
            description: description.into(),
            timestamp: timestamp.into(),
            succeeded: true,
            error: None,
        }
    }

    /// Mark as failed with error.
    #[must_use]
    pub fn with_error(mut self, err: impl Into<String>) -> Self {
        self.succeeded = false;
        self.error = Some(err.into());
        self
    }
}

/// Type of action in a task episode.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub enum ActionType {
    /// A tool was called.
    ToolCall,
    /// A file was edited.
    Edit,
    /// A command was run.
    Command,
    /// A search was performed.
    Search,
    /// Memory was retrieved.
    Retrieval,
    /// Output was generated.
    Output,
    /// Unknown or other action.
    #[default]
    Other,
}

impl ActionType {
    /// Stable string representation.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ToolCall => "tool_call",
            Self::Edit => "edit",
            Self::Command => "command",
            Self::Search => "search",
            Self::Retrieval => "retrieval",
            Self::Output => "output",
            Self::Other => "other",
        }
    }
}

impl fmt::Display for ActionType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for ActionType {
    type Err = ParseActionTypeError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "tool_call" => Ok(Self::ToolCall),
            "edit" => Ok(Self::Edit),
            "command" => Ok(Self::Command),
            "search" => Ok(Self::Search),
            "retrieval" => Ok(Self::Retrieval),
            "output" => Ok(Self::Output),
            "other" => Ok(Self::Other),
            _ => Err(ParseActionTypeError {
                input: s.to_owned(),
            }),
        }
    }
}

/// Error parsing an action type.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParseActionTypeError {
    input: String,
}

impl fmt::Display for ParseActionTypeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "unknown action type `{}`; expected tool_call, edit, command, search, retrieval, output, or other",
            self.input
        )
    }
}

impl std::error::Error for ParseActionTypeError {}

/// Outcome of a task episode.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub enum EpisodeOutcome {
    /// Task completed successfully.
    Success,
    /// Task failed.
    Failure,
    /// Task was cancelled.
    Cancelled,
    /// Task timed out.
    Timeout,
    /// Task outcome is unknown.
    #[default]
    Unknown,
}

impl EpisodeOutcome {
    /// Stable string representation.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::Failure => "failure",
            Self::Cancelled => "cancelled",
            Self::Timeout => "timeout",
            Self::Unknown => "unknown",
        }
    }

    /// Whether the outcome is considered negative (failure, cancelled, timeout).
    #[must_use]
    pub const fn is_negative(self) -> bool {
        matches!(self, Self::Failure | Self::Cancelled | Self::Timeout)
    }
}

impl fmt::Display for EpisodeOutcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for EpisodeOutcome {
    type Err = ParseEpisodeOutcomeError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "success" => Ok(Self::Success),
            "failure" => Ok(Self::Failure),
            "cancelled" => Ok(Self::Cancelled),
            "timeout" => Ok(Self::Timeout),
            "unknown" => Ok(Self::Unknown),
            _ => Err(ParseEpisodeOutcomeError {
                input: s.to_owned(),
            }),
        }
    }
}

/// Error parsing an episode outcome.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParseEpisodeOutcomeError {
    input: String,
}

impl fmt::Display for ParseEpisodeOutcomeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "unknown episode outcome `{}`; expected success, failure, cancelled, timeout, or unknown",
            self.input
        )
    }
}

impl std::error::Error for ParseEpisodeOutcomeError {}

/// A hypothetical memory mutation for counterfactual analysis.
///
/// Interventions are applied to episodes to test "what if" scenarios
/// without mutating actual memory state.
#[derive(Clone, Debug, PartialEq)]
pub struct Intervention {
    /// Schema identifier.
    pub schema: &'static str,
    /// Unique intervention ID.
    pub id: String,
    /// Type of intervention.
    pub intervention_type: InterventionType,
    /// Target memory ID (for add/remove/modify).
    pub target_memory_id: Option<String>,
    /// Hypothetical memory content (for add/replace).
    pub hypothetical_content: Option<String>,
    /// Score adjustment (for strengthen/weaken).
    pub score_delta: Option<f64>,
    /// Brief description of the intervention.
    pub description: String,
    /// Rationale for this intervention.
    pub rationale: Option<String>,
    /// Timestamp when intervention was created (RFC 3339).
    pub created_at: String,
    /// Who created this intervention.
    pub created_by: Option<String>,
}

impl Intervention {
    /// Create a new intervention.
    #[must_use]
    pub fn new(
        id: impl Into<String>,
        intervention_type: InterventionType,
        description: impl Into<String>,
        created_at: impl Into<String>,
    ) -> Self {
        Self {
            schema: INTERVENTION_SCHEMA_V1,
            id: id.into(),
            intervention_type,
            description: description.into(),
            created_at: created_at.into(),
            target_memory_id: None,
            hypothetical_content: None,
            score_delta: None,
            rationale: None,
            created_by: None,
        }
    }

    /// Set the target memory ID.
    #[must_use]
    pub fn with_target_memory(mut self, id: impl Into<String>) -> Self {
        self.target_memory_id = Some(id.into());
        self
    }

    /// Set hypothetical content.
    #[must_use]
    pub fn with_hypothetical_content(mut self, content: impl Into<String>) -> Self {
        self.hypothetical_content = Some(content.into());
        self
    }

    /// Set score delta.
    #[must_use]
    pub fn with_score_delta(mut self, delta: f64) -> Self {
        self.score_delta = Some(delta);
        self
    }

    /// Set rationale.
    #[must_use]
    pub fn with_rationale(mut self, rationale: impl Into<String>) -> Self {
        self.rationale = Some(rationale.into());
        self
    }

    /// Set creator.
    #[must_use]
    pub fn with_created_by(mut self, by: impl Into<String>) -> Self {
        self.created_by = Some(by.into());
        self
    }
}

/// Type of memory intervention.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum InterventionType {
    /// Add a hypothetical memory that didn't exist.
    AddMemory,
    /// Remove a memory that was present.
    RemoveMemory,
    /// Replace memory content.
    ReplaceContent,
    /// Increase memory scores (utility, confidence, relevance).
    Strengthen,
    /// Decrease memory scores.
    Weaken,
    /// Change memory retrieval ranking.
    Rerank,
}

impl InterventionType {
    /// Stable string representation.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::AddMemory => "add_memory",
            Self::RemoveMemory => "remove_memory",
            Self::ReplaceContent => "replace_content",
            Self::Strengthen => "strengthen",
            Self::Weaken => "weaken",
            Self::Rerank => "rerank",
        }
    }
}

impl fmt::Display for InterventionType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for InterventionType {
    type Err = ParseInterventionTypeError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "add_memory" => Ok(Self::AddMemory),
            "remove_memory" => Ok(Self::RemoveMemory),
            "replace_content" => Ok(Self::ReplaceContent),
            "strengthen" => Ok(Self::Strengthen),
            "weaken" => Ok(Self::Weaken),
            "rerank" => Ok(Self::Rerank),
            _ => Err(ParseInterventionTypeError {
                input: s.to_owned(),
            }),
        }
    }
}

/// Error parsing an intervention type.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParseInterventionTypeError {
    input: String,
}

impl fmt::Display for ParseInterventionTypeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "unknown intervention type `{}`; expected add_memory, remove_memory, replace_content, strengthen, weaken, or rerank",
            self.input
        )
    }
}

impl std::error::Error for ParseInterventionTypeError {}

/// A counterfactual replay of an episode with interventions applied.
#[derive(Clone, Debug, PartialEq)]
pub struct CounterfactualRun {
    /// Schema identifier.
    pub schema: &'static str,
    /// Unique run ID.
    pub id: String,
    /// Episode being replayed.
    pub episode_id: String,
    /// Interventions applied to this run.
    pub intervention_ids: Vec<String>,
    /// Hypothetical outcome after interventions.
    pub hypothetical_outcome: EpisodeOutcome,
    /// Confidence that the outcome would have changed (0.0-1.0).
    pub confidence: f64,
    /// Method used for counterfactual analysis.
    pub method: CounterfactualMethod,
    /// Analysis notes.
    pub analysis: Option<String>,
    /// Timestamp when run was executed (RFC 3339).
    pub executed_at: String,
    /// Duration of analysis in milliseconds.
    pub analysis_duration_ms: Option<u64>,
}

impl CounterfactualRun {
    /// Create a new counterfactual run.
    #[must_use]
    pub fn new(
        id: impl Into<String>,
        episode_id: impl Into<String>,
        hypothetical_outcome: EpisodeOutcome,
        confidence: f64,
        method: CounterfactualMethod,
        executed_at: impl Into<String>,
    ) -> Self {
        Self {
            schema: COUNTERFACTUAL_RUN_SCHEMA_V1,
            id: id.into(),
            episode_id: episode_id.into(),
            intervention_ids: Vec::new(),
            hypothetical_outcome,
            confidence,
            method,
            analysis: None,
            executed_at: executed_at.into(),
            analysis_duration_ms: None,
        }
    }

    /// Add an intervention.
    pub fn add_intervention(&mut self, id: impl Into<String>) {
        self.intervention_ids.push(id.into());
    }

    /// Set analysis notes.
    #[must_use]
    pub fn with_analysis(mut self, analysis: impl Into<String>) -> Self {
        self.analysis = Some(analysis.into());
        self
    }

    /// Set analysis duration.
    #[must_use]
    pub fn with_analysis_duration_ms(mut self, ms: u64) -> Self {
        self.analysis_duration_ms = Some(ms);
        self
    }
}

/// Method used for counterfactual analysis.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub enum CounterfactualMethod {
    /// Deterministic replay with modified inputs.
    DeterministicReplay,
    /// Heuristic estimation based on memory impact.
    HeuristicEstimate,
    /// LLM-based what-if reasoning.
    LlmReasoning,
    /// Human expert judgment.
    HumanJudgment,
    /// Method unknown or not specified.
    #[default]
    Unknown,
}

impl CounterfactualMethod {
    /// Stable string representation.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::DeterministicReplay => "deterministic_replay",
            Self::HeuristicEstimate => "heuristic_estimate",
            Self::LlmReasoning => "llm_reasoning",
            Self::HumanJudgment => "human_judgment",
            Self::Unknown => "unknown",
        }
    }
}

impl fmt::Display for CounterfactualMethod {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for CounterfactualMethod {
    type Err = ParseCounterfactualMethodError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "deterministic_replay" => Ok(Self::DeterministicReplay),
            "heuristic_estimate" => Ok(Self::HeuristicEstimate),
            "llm_reasoning" => Ok(Self::LlmReasoning),
            "human_judgment" => Ok(Self::HumanJudgment),
            "unknown" => Ok(Self::Unknown),
            _ => Err(ParseCounterfactualMethodError {
                input: s.to_owned(),
            }),
        }
    }
}

/// Error parsing a counterfactual method.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParseCounterfactualMethodError {
    input: String,
}

impl fmt::Display for ParseCounterfactualMethodError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "unknown counterfactual method `{}`; expected deterministic_replay, heuristic_estimate, llm_reasoning, human_judgment, or unknown",
            self.input
        )
    }
}

impl std::error::Error for ParseCounterfactualMethodError {}

/// A ledger of regret entries recording what interventions would have helped.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RegretLedger {
    /// Schema identifier.
    pub schema: &'static str,
    /// Workspace this ledger belongs to.
    pub workspace_id: Option<String>,
    /// All regret entries.
    pub entries: Vec<RegretEntry>,
    /// Summary statistics.
    pub summary: Option<RegretSummary>,
    /// Timestamp when ledger was last updated (RFC 3339).
    pub updated_at: String,
}

impl RegretLedger {
    /// Create a new regret ledger.
    #[must_use]
    pub fn new(updated_at: impl Into<String>) -> Self {
        Self {
            schema: REGRET_LEDGER_SCHEMA_V1,
            updated_at: updated_at.into(),
            ..Default::default()
        }
    }

    /// Set the workspace ID.
    #[must_use]
    pub fn with_workspace_id(mut self, id: impl Into<String>) -> Self {
        self.workspace_id = Some(id.into());
        self
    }

    /// Add a regret entry.
    pub fn add_entry(&mut self, entry: RegretEntry) {
        self.entries.push(entry);
    }

    /// Set summary statistics.
    #[must_use]
    pub fn with_summary(mut self, summary: RegretSummary) -> Self {
        self.summary = Some(summary);
        self
    }
}

/// A single entry in the regret ledger.
#[derive(Clone, Debug, PartialEq)]
pub struct RegretEntry {
    /// Schema identifier.
    pub schema: &'static str,
    /// Unique entry ID.
    pub id: String,
    /// Episode that experienced regret.
    pub episode_id: String,
    /// Counterfactual run that identified this regret.
    pub counterfactual_run_id: String,
    /// Intervention that would have helped.
    pub intervention_id: String,
    /// Estimated regret (impact of not having the intervention).
    pub regret_score: f64,
    /// Confidence in the regret estimate.
    pub confidence: f64,
    /// Category of regret.
    pub category: RegretCategory,
    /// Whether this regret led to an actual memory promotion.
    pub promoted: bool,
    /// Memory ID if a promotion occurred.
    pub promoted_memory_id: Option<String>,
    /// Timestamp when entry was created (RFC 3339).
    pub created_at: String,
}

impl RegretEntry {
    /// Create a new regret entry.
    #[must_use]
    pub fn new(
        id: impl Into<String>,
        episode_id: impl Into<String>,
        counterfactual_run_id: impl Into<String>,
        intervention_id: impl Into<String>,
        regret_score: f64,
        confidence: f64,
        category: RegretCategory,
        created_at: impl Into<String>,
    ) -> Self {
        Self {
            schema: REGRET_ENTRY_SCHEMA_V1,
            id: id.into(),
            episode_id: episode_id.into(),
            counterfactual_run_id: counterfactual_run_id.into(),
            intervention_id: intervention_id.into(),
            regret_score,
            confidence,
            category,
            promoted: false,
            promoted_memory_id: None,
            created_at: created_at.into(),
        }
    }

    /// Mark as promoted with memory ID.
    #[must_use]
    pub fn with_promotion(mut self, memory_id: impl Into<String>) -> Self {
        self.promoted = true;
        self.promoted_memory_id = Some(memory_id.into());
        self
    }
}

/// Category of regret.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub enum RegretCategory {
    /// Missing knowledge that would have helped.
    MissingKnowledge,
    /// Stale or outdated information was used.
    StaleInformation,
    /// Relevant memory was not retrieved.
    RetrievalFailure,
    /// Retrieved but not used effectively.
    UnderutilizedMemory,
    /// Wrong information was used.
    Misinformation,
    /// Uncategorized regret.
    #[default]
    Other,
}

impl RegretCategory {
    /// Stable string representation.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::MissingKnowledge => "missing_knowledge",
            Self::StaleInformation => "stale_information",
            Self::RetrievalFailure => "retrieval_failure",
            Self::UnderutilizedMemory => "underutilized_memory",
            Self::Misinformation => "misinformation",
            Self::Other => "other",
        }
    }
}

impl fmt::Display for RegretCategory {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for RegretCategory {
    type Err = ParseRegretCategoryError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "missing_knowledge" => Ok(Self::MissingKnowledge),
            "stale_information" => Ok(Self::StaleInformation),
            "retrieval_failure" => Ok(Self::RetrievalFailure),
            "underutilized_memory" => Ok(Self::UnderutilizedMemory),
            "misinformation" => Ok(Self::Misinformation),
            "other" => Ok(Self::Other),
            _ => Err(ParseRegretCategoryError {
                input: s.to_owned(),
            }),
        }
    }
}

/// Error parsing a regret category.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParseRegretCategoryError {
    input: String,
}

impl fmt::Display for ParseRegretCategoryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "unknown regret category `{}`; expected missing_knowledge, stale_information, retrieval_failure, underutilized_memory, misinformation, or other",
            self.input
        )
    }
}

impl std::error::Error for ParseRegretCategoryError {}

/// Summary statistics for a regret ledger.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RegretSummary {
    /// Total number of entries.
    pub total_entries: u32,
    /// Number that led to promotions.
    pub promoted_count: u32,
    /// Entries by category.
    pub by_category: Vec<(RegretCategory, u32)>,
    /// Average regret score.
    pub average_regret: Option<String>,
    /// Average confidence.
    pub average_confidence: Option<String>,
}

impl RegretSummary {
    /// Create a new summary.
    #[must_use]
    pub fn new(total_entries: u32, promoted_count: u32) -> Self {
        Self {
            total_entries,
            promoted_count,
            ..Default::default()
        }
    }

    /// Add a category count.
    pub fn add_category_count(&mut self, category: RegretCategory, count: u32) {
        self.by_category.push((category, count));
    }

    /// Set average regret (as string to avoid float comparison issues).
    #[must_use]
    pub fn with_average_regret(mut self, avg: impl Into<String>) -> Self {
        self.average_regret = Some(avg.into());
        self
    }

    /// Set average confidence.
    #[must_use]
    pub fn with_average_confidence(mut self, avg: impl Into<String>) -> Self {
        self.average_confidence = Some(avg.into());
        self
    }
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
    fn episode_schema_versions_are_stable() -> TestResult {
        ensure(TASK_EPISODE_SCHEMA_V1, "ee.task_episode.v1", "episode")?;
        ensure(INTERVENTION_SCHEMA_V1, "ee.intervention.v1", "intervention")?;
        ensure(
            COUNTERFACTUAL_RUN_SCHEMA_V1,
            "ee.counterfactual_run.v1",
            "cfr",
        )?;
        ensure(REGRET_LEDGER_SCHEMA_V1, "ee.regret_ledger.v1", "ledger")?;
        ensure(REGRET_ENTRY_SCHEMA_V1, "ee.regret_entry.v1", "entry")
    }

    #[test]
    fn task_episode_builder() -> TestResult {
        let mut ep = TaskEpisode::new("ep_001", "Fix the bug", "2026-04-30T12:00:00Z")
            .with_workspace_id("ws_001")
            .with_session_id("sess_001")
            .with_context_pack_id("pack_001")
            .with_outcome(EpisodeOutcome::Success)
            .with_ended_at("2026-04-30T12:05:00Z")
            .with_duration_ms(300000)
            .with_agent("claude-code");

        ep.add_retrieved_memory("mem_001");
        ep.add_action(EpisodeAction::new(
            1,
            ActionType::Edit,
            "Edit file",
            "2026-04-30T12:01:00Z",
        ));

        ensure(ep.schema, TASK_EPISODE_SCHEMA_V1, "schema")?;
        ensure(ep.task_input, "Fix the bug".to_string(), "task")?;
        ensure(ep.outcome, EpisodeOutcome::Success, "outcome")?;
        ensure(ep.retrieved_memory_ids.len(), 1, "memories")?;
        ensure(ep.actions.len(), 1, "actions")
    }

    #[test]
    fn action_type_strings_are_stable() -> TestResult {
        ensure(ActionType::ToolCall.as_str(), "tool_call", "tool_call")?;
        ensure(ActionType::Edit.as_str(), "edit", "edit")?;
        ensure(ActionType::Command.as_str(), "command", "command")?;
        ensure(ActionType::Search.as_str(), "search", "search")?;
        ensure(ActionType::Retrieval.as_str(), "retrieval", "retrieval")?;
        ensure(ActionType::Output.as_str(), "output", "output")?;
        ensure(ActionType::Other.as_str(), "other", "other")
    }

    #[test]
    fn action_type_round_trip() -> TestResult {
        for at in [
            ActionType::ToolCall,
            ActionType::Edit,
            ActionType::Command,
            ActionType::Search,
            ActionType::Retrieval,
            ActionType::Output,
            ActionType::Other,
        ] {
            let parsed = ActionType::from_str(at.as_str());
            ensure(parsed, Ok(at), at.as_str())?;
        }
        Ok(())
    }

    #[test]
    fn action_type_rejects_invalid() {
        assert!(ActionType::from_str("invalid").is_err());
    }

    #[test]
    fn episode_outcome_strings_are_stable() -> TestResult {
        ensure(EpisodeOutcome::Success.as_str(), "success", "success")?;
        ensure(EpisodeOutcome::Failure.as_str(), "failure", "failure")?;
        ensure(EpisodeOutcome::Cancelled.as_str(), "cancelled", "cancelled")?;
        ensure(EpisodeOutcome::Timeout.as_str(), "timeout", "timeout")?;
        ensure(EpisodeOutcome::Unknown.as_str(), "unknown", "unknown")
    }

    #[test]
    fn episode_outcome_round_trip() -> TestResult {
        for eo in [
            EpisodeOutcome::Success,
            EpisodeOutcome::Failure,
            EpisodeOutcome::Cancelled,
            EpisodeOutcome::Timeout,
            EpisodeOutcome::Unknown,
        ] {
            let parsed = EpisodeOutcome::from_str(eo.as_str());
            ensure(parsed, Ok(eo), eo.as_str())?;
        }
        Ok(())
    }

    #[test]
    fn episode_outcome_is_negative() -> TestResult {
        ensure(EpisodeOutcome::Success.is_negative(), false, "success")?;
        ensure(EpisodeOutcome::Failure.is_negative(), true, "failure")?;
        ensure(EpisodeOutcome::Cancelled.is_negative(), true, "cancelled")?;
        ensure(EpisodeOutcome::Timeout.is_negative(), true, "timeout")?;
        ensure(EpisodeOutcome::Unknown.is_negative(), false, "unknown")
    }

    #[test]
    fn intervention_builder() -> TestResult {
        let int = Intervention::new(
            "int_001",
            InterventionType::AddMemory,
            "Add missing rule",
            "2026-04-30T12:00:00Z",
        )
        .with_target_memory("mem_001")
        .with_hypothetical_content("Always run tests")
        .with_rationale("Would have prevented test failure")
        .with_created_by("analyst");

        ensure(int.schema, INTERVENTION_SCHEMA_V1, "schema")?;
        ensure(int.intervention_type, InterventionType::AddMemory, "type")?;
        ensure(int.target_memory_id, Some("mem_001".to_string()), "target")
    }

    #[test]
    fn intervention_type_strings_are_stable() -> TestResult {
        ensure(InterventionType::AddMemory.as_str(), "add_memory", "add")?;
        ensure(
            InterventionType::RemoveMemory.as_str(),
            "remove_memory",
            "remove",
        )?;
        ensure(
            InterventionType::ReplaceContent.as_str(),
            "replace_content",
            "replace",
        )?;
        ensure(
            InterventionType::Strengthen.as_str(),
            "strengthen",
            "strengthen",
        )?;
        ensure(InterventionType::Weaken.as_str(), "weaken", "weaken")?;
        ensure(InterventionType::Rerank.as_str(), "rerank", "rerank")
    }

    #[test]
    fn intervention_type_round_trip() -> TestResult {
        for it in [
            InterventionType::AddMemory,
            InterventionType::RemoveMemory,
            InterventionType::ReplaceContent,
            InterventionType::Strengthen,
            InterventionType::Weaken,
            InterventionType::Rerank,
        ] {
            let parsed = InterventionType::from_str(it.as_str());
            ensure(parsed, Ok(it), it.as_str())?;
        }
        Ok(())
    }

    #[test]
    fn counterfactual_run_builder() -> TestResult {
        let mut cfr = CounterfactualRun::new(
            "cfr_001",
            "ep_001",
            EpisodeOutcome::Success,
            0.85,
            CounterfactualMethod::DeterministicReplay,
            "2026-04-30T12:00:00Z",
        )
        .with_analysis("Adding the rule would have prevented failure")
        .with_analysis_duration_ms(500);

        cfr.add_intervention("int_001");

        ensure(cfr.schema, COUNTERFACTUAL_RUN_SCHEMA_V1, "schema")?;
        ensure(cfr.hypothetical_outcome, EpisodeOutcome::Success, "outcome")?;
        ensure(cfr.intervention_ids.len(), 1, "interventions")
    }

    #[test]
    fn counterfactual_method_strings_are_stable() -> TestResult {
        ensure(
            CounterfactualMethod::DeterministicReplay.as_str(),
            "deterministic_replay",
            "replay",
        )?;
        ensure(
            CounterfactualMethod::HeuristicEstimate.as_str(),
            "heuristic_estimate",
            "heuristic",
        )?;
        ensure(
            CounterfactualMethod::LlmReasoning.as_str(),
            "llm_reasoning",
            "llm",
        )?;
        ensure(
            CounterfactualMethod::HumanJudgment.as_str(),
            "human_judgment",
            "human",
        )?;
        ensure(CounterfactualMethod::Unknown.as_str(), "unknown", "unknown")
    }

    #[test]
    fn counterfactual_method_round_trip() -> TestResult {
        for cm in [
            CounterfactualMethod::DeterministicReplay,
            CounterfactualMethod::HeuristicEstimate,
            CounterfactualMethod::LlmReasoning,
            CounterfactualMethod::HumanJudgment,
            CounterfactualMethod::Unknown,
        ] {
            let parsed = CounterfactualMethod::from_str(cm.as_str());
            ensure(parsed, Ok(cm), cm.as_str())?;
        }
        Ok(())
    }

    #[test]
    fn regret_ledger_builder() -> TestResult {
        let mut ledger = RegretLedger::new("2026-04-30T12:00:00Z")
            .with_workspace_id("ws_001")
            .with_summary(RegretSummary::new(10, 3));

        ledger.add_entry(RegretEntry::new(
            "reg_001",
            "ep_001",
            "cfr_001",
            "int_001",
            0.7,
            0.85,
            RegretCategory::MissingKnowledge,
            "2026-04-30T12:00:00Z",
        ));

        ensure(ledger.schema, REGRET_LEDGER_SCHEMA_V1, "schema")?;
        ensure(ledger.entries.len(), 1, "entries")
    }

    #[test]
    fn regret_entry_builder() -> TestResult {
        let entry = RegretEntry::new(
            "reg_001",
            "ep_001",
            "cfr_001",
            "int_001",
            0.7,
            0.85,
            RegretCategory::RetrievalFailure,
            "2026-04-30T12:00:00Z",
        )
        .with_promotion("mem_new_001");

        ensure(entry.schema, REGRET_ENTRY_SCHEMA_V1, "schema")?;
        ensure(entry.promoted, true, "promoted")?;
        ensure(entry.category, RegretCategory::RetrievalFailure, "category")
    }

    #[test]
    fn regret_category_strings_are_stable() -> TestResult {
        ensure(
            RegretCategory::MissingKnowledge.as_str(),
            "missing_knowledge",
            "missing",
        )?;
        ensure(
            RegretCategory::StaleInformation.as_str(),
            "stale_information",
            "stale",
        )?;
        ensure(
            RegretCategory::RetrievalFailure.as_str(),
            "retrieval_failure",
            "retrieval",
        )?;
        ensure(
            RegretCategory::UnderutilizedMemory.as_str(),
            "underutilized_memory",
            "underutilized",
        )?;
        ensure(
            RegretCategory::Misinformation.as_str(),
            "misinformation",
            "misinformation",
        )?;
        ensure(RegretCategory::Other.as_str(), "other", "other")
    }

    #[test]
    fn regret_category_round_trip() -> TestResult {
        for rc in [
            RegretCategory::MissingKnowledge,
            RegretCategory::StaleInformation,
            RegretCategory::RetrievalFailure,
            RegretCategory::UnderutilizedMemory,
            RegretCategory::Misinformation,
            RegretCategory::Other,
        ] {
            let parsed = RegretCategory::from_str(rc.as_str());
            ensure(parsed, Ok(rc), rc.as_str())?;
        }
        Ok(())
    }

    #[test]
    fn regret_summary_builder() -> TestResult {
        let mut summary = RegretSummary::new(100, 25)
            .with_average_regret("0.65")
            .with_average_confidence("0.80");

        summary.add_category_count(RegretCategory::MissingKnowledge, 40);
        summary.add_category_count(RegretCategory::RetrievalFailure, 30);

        ensure(summary.total_entries, 100, "total")?;
        ensure(summary.promoted_count, 25, "promoted")?;
        ensure(summary.by_category.len(), 2, "categories")
    }
}
