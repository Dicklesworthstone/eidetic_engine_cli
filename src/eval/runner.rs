//! Eval fixture runner for retrieval quality evaluation.
//!
//! Loads fixtures from `tests/fixtures/eval/`, seeds a deterministic workspace,
//! runs queries, and computes retrieval metrics (P@k, nDCG@k, MRR).

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::io::Read;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::models::DomainError;

/// Schema version for eval fixture files.
pub const EVAL_FIXTURE_SCHEMA_V1: &str = "ee.eval_fixture.v1";

/// Schema version for eval source memory files.
pub const EVAL_SOURCE_MEMORY_SCHEMA_V1: &str = "ee.eval_source_memory.v1";

/// Schema version for eval run report.
pub const EVAL_REPORT_SCHEMA_V1: &str = "ee.eval.report.v1";

/// Schema version for pack-quality expectations embedded in eval fixtures.
pub const PACK_QUALITY_EXPECTATIONS_SCHEMA_V1: &str = "ee.eval.pack_quality_expectations.v1";

/// Schema version for outcome-backed pack-quality feedback summaries.
pub const PACK_QUALITY_OUTCOME_FEEDBACK_SCHEMA_V1: &str =
    "ee.eval.pack_quality_outcome_feedback.v1";

/// Schema version for structural-recall expectations embedded in eval fixtures.
pub const STRUCTURAL_RECALL_EXPECTATIONS_SCHEMA_V1: &str =
    "ee.eval.structural_recall_expectations.v1";

/// Schema version for ask-quality expectations embedded in eval fixtures.
pub const ASK_QUALITY_EXPECTATIONS_SCHEMA_V1: &str = "ee.eval.ask_quality_expectations.v1";

/// Schema version for ask-quality reports emitted by the eval harness.
pub const ASK_REPORT_SCHEMA_V1: &str = "ee.eval.ask_report.v1";

/// Schema version for bundled-embedding semantic recall expectations.
pub const SEMANTIC_RECALL_EXPECTATIONS_SCHEMA_V1: &str = "ee.eval.semantic_recall_expectations.v1";

/// Schema version for bundled-embedding semantic recall reports.
pub const SEMANTIC_RECALL_REPORT_SCHEMA_V1: &str = "ee.eval.semantic_recall_report.v1";

/// Default fixture directory relative to project root.
pub const DEFAULT_FIXTURE_DIR: &str = "tests/fixtures/eval";

const EVAL_FIXTURE_FILE_MAX_BYTES: u64 = 8 * 1024 * 1024;

/// A discovered fixture with its metadata.
#[derive(Clone, Debug)]
pub struct DiscoveredFixture {
    pub fixture_id: String,
    pub fixture_family: String,
    pub path: PathBuf,
    pub scenario_path: PathBuf,
    pub source_memory_path: PathBuf,
}

/// Fixture scenario definition (parsed from scenario.json).
#[derive(Clone, Debug, Deserialize)]
pub struct FixtureScenario {
    pub schema: String,
    pub fixture_id: String,
    #[serde(default)]
    pub scenario_ids: Vec<String>,
    pub fixture_family: String,
    #[serde(default)]
    pub coverage_state: String,
    pub journey: String,
    #[serde(default)]
    pub owning_bead_ids: Vec<String>,
    #[serde(default)]
    pub owning_gate_ids: Vec<String>,
    #[serde(default)]
    pub deterministic: DeterministicConfig,
    #[serde(default)]
    pub source: SourceConfig,
    #[serde(default)]
    pub redaction: RedactionConfig,
    #[serde(default)]
    pub command_sequence: Vec<CommandStep>,
    #[serde(default)]
    pub expected_outputs: Vec<ExpectedOutput>,
    #[serde(default)]
    pub pack_quality_expectations: Option<PackQualityExpectations>,
    #[serde(default)]
    pub ask_quality_expectations: Option<AskQualityExpectations>,
    #[serde(default)]
    pub structural_recall_expectations: Option<StructuralRecallExpectations>,
    #[serde(default)]
    pub semantic_recall_expectations: Option<SemanticRecallExpectations>,
    #[serde(default)]
    pub degraded_branches: Vec<DegradedBranch>,
    pub agent_success_signal: String,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct DeterministicConfig {
    #[serde(default)]
    pub fixed_clock: Option<String>,
    #[serde(default)]
    pub deterministic_seed: Option<String>,
    #[serde(default)]
    pub stable_ids: Vec<String>,
    #[serde(default)]
    pub workspace_fingerprints: Vec<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct SourceConfig {
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub path: String,
    #[serde(default)]
    pub source_hash: String,
    #[serde(default)]
    pub synthetic_secret_policy: String,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct RedactionConfig {
    #[serde(default)]
    pub classes_expected: Vec<String>,
    #[serde(default)]
    pub secret_leak_assertions: Vec<String>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct CommandStep {
    pub step: u32,
    pub argv: Vec<String>,
    #[serde(default)]
    pub cwd: String,
    #[serde(default)]
    pub expected_exit_code: i32,
    #[serde(default)]
    pub stdout_schema: Option<String>,
    #[serde(default)]
    pub stderr_policy: Option<String>,
    #[serde(default)]
    pub stdout_artifact_path: Option<String>,
    #[serde(default)]
    pub stderr_artifact_path: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ExpectedOutput {
    pub step: u32,
    #[serde(default)]
    pub schema: String,
    #[serde(default)]
    pub required_fields: Vec<String>,
    #[serde(default)]
    pub absent_fields: Vec<String>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct DegradedBranch {
    pub code: String,
    pub description: String,
    #[serde(default)]
    pub repair_action: Option<String>,
    #[serde(default = "default_true")]
    pub preserves_success_signal: bool,
}

fn default_true() -> bool {
    true
}

#[derive(Clone, Debug, Deserialize)]
pub struct PackQualityExpectations {
    pub schema: String,
    #[serde(default)]
    pub cases: Vec<PackQualityCase>,
    #[serde(default)]
    pub outcome_events: Vec<PackQualityOutcomeEvent>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct PackQualityCase {
    pub case_id: String,
    pub scenario_id: String,
    pub command_step: u32,
    pub query_surface: PackQualityQuerySurface,
    #[serde(default)]
    pub expected_selected_memory_ids: Vec<String>,
    #[serde(default)]
    pub critical_omitted_memory_ids: Vec<String>,
    pub min_provenance_density: f64,
    #[serde(default)]
    pub allowed_degradation_codes: Vec<String>,
    #[serde(default)]
    pub forbidden_redaction_leaks: Vec<String>,
    pub token_budget: PackQualityTokenBudget,
    pub stable_first_failure_label: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct PackQualityQuerySurface {
    pub kind: String,
    #[serde(default)]
    pub query: Option<String>,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub schema: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct PackQualityTokenBudget {
    pub max_tokens: u32,
    pub expected_used_tokens_max: u32,
    #[serde(default)]
    pub expect_truncation: bool,
}

#[derive(Clone, Debug, Deserialize)]
pub struct PackQualityOutcomeEvent {
    pub event_id: String,
    pub case_id: String,
    #[serde(default)]
    pub pack_profile: String,
    #[serde(default)]
    pub feature_set: Vec<String>,
    pub signal: String,
    #[serde(default)]
    pub memory_ids: Vec<String>,
    #[serde(default)]
    pub unused_memory_ids: Vec<String>,
    #[serde(default)]
    pub duplicated_memory_ids: Vec<String>,
    #[serde(default)]
    pub counterfactual_memory_ids: Vec<String>,
    #[serde(default)]
    pub token_waste_estimate: u32,
    #[serde(default)]
    pub evidence_backed: bool,
    #[serde(default)]
    pub rationale: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct StructuralRecallExpectations {
    pub schema: String,
    pub baseline_report_path: String,
    pub post_g1_report_path: String,
    pub ndcg_at_10_baseline: f64,
    pub allowed_ndcg_drop_pp: f64,
    pub structural_recall_at_10_min: f64,
    #[serde(default)]
    pub required_scenario_ids: Vec<String>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct SemanticRecallExpectations {
    pub schema: String,
    pub deterministic_seed: String,
    #[serde(default = "default_semantic_recall_k")]
    pub recall_at_k: u32,
    pub hash_baseline_recall_at_k_max: f64,
    pub semantic_recall_at_k_min: f64,
    pub minimum_recall_gain: f64,
    #[serde(default)]
    pub cases: Vec<SemanticRecallCase>,
}

fn default_semantic_recall_k() -> u32 {
    5
}

#[derive(Clone, Debug, Deserialize)]
pub struct SemanticRecallCase {
    pub case_id: String,
    pub query: String,
    #[serde(default)]
    pub expected_memory_ids: Vec<String>,
    #[serde(default)]
    pub hash_retrieved_ids: Vec<String>,
    #[serde(default)]
    pub semantic_retrieved_ids: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct SemanticRecallCaseReport {
    pub case_id: String,
    pub query: String,
    pub expected_memory_ids: Vec<String>,
    pub hash_retrieved_ids: Vec<String>,
    pub semantic_retrieved_ids: Vec<String>,
    pub hash_recall_at_k: f64,
    pub semantic_recall_at_k: f64,
    pub recall_gain: f64,
    pub first_semantic_rank: Option<u32>,
}

#[derive(Clone, Debug, Serialize)]
pub struct SemanticRecallReport {
    pub schema: &'static str,
    pub fixture_id: String,
    pub deterministic_seed: String,
    pub recall_at_k: u32,
    pub case_count: u32,
    pub hash_baseline_recall_at_k: f64,
    pub semantic_recall_at_k: f64,
    pub recall_gain: f64,
    pub thresholds: SemanticRecallThresholds,
    pub passed: bool,
    pub cases: Vec<SemanticRecallCaseReport>,
}

#[derive(Clone, Debug, Serialize)]
pub struct SemanticRecallThresholds {
    pub hash_baseline_recall_at_k_max: f64,
    pub semantic_recall_at_k_min: f64,
    pub minimum_recall_gain: f64,
}

#[derive(Clone, Debug, Deserialize)]
pub struct AskQualityExpectations {
    pub schema: String,
    #[serde(default)]
    pub thresholds: AskQualityThresholds,
    #[serde(default)]
    pub cases: Vec<AskQualityCase>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AskQualityGateMode {
    #[default]
    Advisory,
    Blocking,
}

impl AskQualityGateMode {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Advisory => "advisory",
            Self::Blocking => "blocking",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct AskQualityThresholds {
    #[serde(default = "default_ask_quality_threshold")]
    pub citation_precision_min: f64,
    #[serde(default = "default_ask_quality_threshold")]
    pub answer_exactness_min: f64,
    #[serde(default = "default_ask_quality_threshold")]
    pub abstention_calibration_min: f64,
    #[serde(default = "default_ask_quality_threshold")]
    pub conflict_recall_min: f64,
    #[serde(default)]
    pub gate_mode: AskQualityGateMode,
}

impl Default for AskQualityThresholds {
    fn default() -> Self {
        Self {
            citation_precision_min: default_ask_quality_threshold(),
            answer_exactness_min: default_ask_quality_threshold(),
            abstention_calibration_min: default_ask_quality_threshold(),
            conflict_recall_min: default_ask_quality_threshold(),
            gate_mode: AskQualityGateMode::Advisory,
        }
    }
}

const fn default_ask_quality_threshold() -> f64 {
    1.0
}

#[derive(Clone, Debug, Deserialize)]
pub struct AskQualityCase {
    pub case_id: String,
    pub scenario_id: String,
    pub command_step: u32,
    pub question: String,
    #[serde(default)]
    pub expected_cited_memory_ids: Vec<String>,
    #[serde(default)]
    pub expected_answer_terms: Vec<String>,
    #[serde(default)]
    pub expect_abstention: bool,
    #[serde(default)]
    pub expect_conflict: bool,
    #[serde(default)]
    pub expected_sides: Vec<AskQualityExpectedSide>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct AskQualityExpectedSide {
    pub label: String,
    #[serde(default)]
    pub cited_memory_ids: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct AskQualityActual {
    pub case_id: String,
    pub answer_text: Option<String>,
    pub abstained: bool,
    pub citations: Vec<AskQualityCitationActual>,
    pub sides: Vec<AskQualitySideActual>,
}

#[derive(Clone, Debug)]
pub struct AskQualityCitationActual {
    pub memory_id: String,
    pub text: String,
}

#[derive(Clone, Debug)]
pub struct AskQualitySideActual {
    pub label: String,
    pub cited_memory_ids: Vec<String>,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct AskQualityMetrics {
    pub citation_precision: f64,
    pub answer_exactness: f64,
    pub abstention_calibration: f64,
    pub conflict_recall: f64,
}

#[derive(Clone, Debug, Serialize)]
pub struct AskQualityCaseScores {
    pub citation_precision: f64,
    pub answer_exactness: f64,
    pub abstention_calibration: f64,
    pub conflict_recall: f64,
}

#[derive(Clone, Debug, Serialize)]
pub struct AskQualityComparison {
    pub case_id: String,
    pub scenario_id: String,
    pub verdict: PackQualityVerdict,
    pub scores: AskQualityCaseScores,
    pub expected_cited_memory_ids: Vec<String>,
    pub actual_cited_memory_ids: Vec<String>,
    pub expected_answer_terms: Vec<String>,
    pub actual_answer_text: Option<String>,
    pub expected_abstention: bool,
    pub actual_abstained: bool,
    pub expected_conflict: bool,
    pub actual_conflict: bool,
    pub failure_reasons: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct AskQualityReport {
    pub schema: &'static str,
    pub fixture_id: String,
    pub aggregate_verdict: PackQualityVerdict,
    pub gate_mode: AskQualityGateMode,
    pub cases_total: usize,
    pub cases_within: usize,
    pub cases_drift: usize,
    pub cases_regression: usize,
    pub cases_inconclusive: usize,
    pub metrics: AskQualityMetrics,
    pub thresholds: AskQualityThresholds,
    pub threshold_failures: Vec<String>,
    pub comparisons: Vec<AskQualityComparison>,
}

impl AskQualityReport {
    #[must_use]
    pub fn new(fixture_id: String, thresholds: AskQualityThresholds) -> Self {
        Self {
            schema: ASK_REPORT_SCHEMA_V1,
            fixture_id,
            aggregate_verdict: PackQualityVerdict::Within,
            gate_mode: thresholds.gate_mode,
            cases_total: 0,
            cases_within: 0,
            cases_drift: 0,
            cases_regression: 0,
            cases_inconclusive: 0,
            metrics: AskQualityMetrics::default(),
            thresholds,
            threshold_failures: Vec::new(),
            comparisons: Vec::new(),
        }
    }

    pub fn add_comparison(&mut self, comparison: AskQualityComparison) {
        match comparison.verdict {
            PackQualityVerdict::Within => self.cases_within += 1,
            PackQualityVerdict::Drift => self.cases_drift += 1,
            PackQualityVerdict::Regression => self.cases_regression += 1,
            PackQualityVerdict::Inconclusive => self.cases_inconclusive += 1,
        }
        self.cases_total += 1;
        self.comparisons.push(comparison);
        self.recompute_case_aggregate();
    }

    fn recompute_case_aggregate(&mut self) {
        if self.cases_regression > 0 {
            self.aggregate_verdict = PackQualityVerdict::Regression;
        } else if self.cases_inconclusive > 0 {
            self.aggregate_verdict = PackQualityVerdict::Inconclusive;
        } else if self.cases_drift > 0 {
            self.aggregate_verdict = PackQualityVerdict::Drift;
        } else {
            self.aggregate_verdict = PackQualityVerdict::Within;
        }
    }
}

/// Source memory definition (parsed from source_memory.json).
#[derive(Clone, Debug, Deserialize)]
pub struct SourceMemoryFile {
    pub schema: String,
    pub fixture_id: String,
    #[serde(default)]
    pub source_kind: String,
    #[serde(default)]
    pub fixed_clock: Option<String>,
    #[serde(default)]
    pub memories: Vec<SourceMemory>,
    #[serde(default)]
    pub seed_memory: Option<SourceMemory>,
    #[serde(default)]
    pub tiers: Vec<SourceMemoryTier>,
    #[serde(default)]
    pub structural_edges: Vec<StructuralEdge>,
    #[serde(default)]
    pub secret_policy: SecretPolicy,
}

#[derive(Clone, Debug, Deserialize)]
pub struct StructuralEdge {
    pub source_id: String,
    pub target_id: String,
    pub relation: String,
    pub weight: f64,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct SecretPolicy {
    #[serde(default)]
    pub synthetic_secret_policy: String,
    #[serde(default)]
    pub secret_like_values_present: bool,
    #[serde(default)]
    pub blocked_classes: Vec<String>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct SourceMemory {
    pub id: String,
    pub level: String,
    pub kind: String,
    #[serde(default)]
    pub trust_class: String,
    #[serde(default)]
    pub confidence: f64,
    #[serde(default)]
    pub utility: f64,
    #[serde(default)]
    pub importance: f64,
    #[serde(default)]
    pub tags: Vec<String>,
    pub content: String,
    #[serde(default)]
    pub provenance_uri: Option<String>,
    #[serde(default)]
    pub expected_query_match: Vec<String>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct SourceMemoryTier {
    pub name: String,
    #[serde(default)]
    pub expected_memory_count: u32,
    #[serde(default)]
    pub id_range: Option<SourceMemoryIdRange>,
    #[serde(default)]
    pub expected_query_match: Vec<String>,
    #[serde(default)]
    pub generator: Option<SourceMemoryTierGenerator>,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct SourceMemoryTierGenerator {
    #[serde(default)]
    pub profile: String,
    #[serde(default)]
    pub template: String,
    #[serde(default)]
    pub relevant_every: u32,
    #[serde(default)]
    pub distractor_every: u32,
}

#[derive(Clone, Debug, Deserialize)]
pub struct SourceMemoryIdRange {
    pub start: String,
    pub end: String,
}

/// Retrieval metrics for a single query.
#[derive(Clone, Debug, Default, Serialize)]
pub struct QueryMetrics {
    pub query: String,
    pub expected_ids: Vec<String>,
    pub retrieved_ids: Vec<String>,
    pub precision_at_1: f64,
    pub precision_at_3: f64,
    pub precision_at_5: f64,
    pub recall_at_5: f64,
    pub ndcg_at_5: f64,
    pub mrr: f64,
    pub first_relevant_rank: Option<u32>,
}

/// Aggregate metrics for a fixture evaluation.
#[derive(Clone, Debug, Default, Serialize)]
pub struct FixtureMetrics {
    pub fixture_id: String,
    pub queries_evaluated: u32,
    pub mean_precision_at_1: f64,
    pub mean_precision_at_3: f64,
    pub mean_precision_at_5: f64,
    pub mean_recall_at_5: f64,
    pub mean_ndcg_at_5: f64,
    pub mean_mrr: f64,
    pub per_query: Vec<QueryMetrics>,
}

/// Full evaluation report.
#[derive(Clone, Debug, Serialize)]
pub struct EvalRunReport {
    pub schema: &'static str,
    pub fixture_id: String,
    pub fixture_family: String,
    pub status: EvalRunStatus,
    pub metrics: FixtureMetrics,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub semantic_recall: Option<SemanticRecallReport>,
    pub duration_ms: f64,
    pub data_hash: String,
}

impl EvalRunReport {
    pub fn new(fixture_id: String, fixture_family: String) -> Self {
        Self {
            schema: EVAL_REPORT_SCHEMA_V1,
            fixture_id,
            fixture_family,
            status: EvalRunStatus::Pending,
            metrics: FixtureMetrics::default(),
            semantic_recall: None,
            duration_ms: 0.0,
            data_hash: String::new(),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EvalRunStatus {
    #[default]
    Pending,
    Running,
    Passed,
    Failed,
    Error,
}

impl EvalRunStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Running => "running",
            Self::Passed => "passed",
            Self::Failed => "failed",
            Self::Error => "error",
        }
    }
}

/// Fixture listing entry for `ee eval list`.
#[derive(Clone, Debug, Serialize)]
pub struct FixtureListEntry {
    pub fixture_id: String,
    pub fixture_family: String,
    pub journey: String,
    pub memory_count: usize,
    pub query_count: usize,
    pub path: String,
}

/// Discover all fixtures in the given directory.
pub fn discover_fixtures(fixture_dir: &Path) -> Result<Vec<DiscoveredFixture>, DomainError> {
    let mut fixtures = Vec::new();

    ensure_eval_fixture_path_has_no_symlink_components(fixture_dir, "read fixture directory")?;

    if !fixture_dir.exists() {
        return Err(DomainError::Configuration {
            message: format!(
                "Fixture directory does not exist: {}",
                fixture_dir.display()
            ),
            repair: Some("Ensure tests/fixtures/eval/ exists".into()),
        });
    }

    let entries = std::fs::read_dir(fixture_dir).map_err(|e| DomainError::Storage {
        message: format!("Failed to read fixture directory: {e}"),
        repair: Some("Check directory permissions".into()),
    })?;

    for entry in entries {
        let entry = entry.map_err(|e| DomainError::Storage {
            message: format!("Failed to read directory entry: {e}"),
            repair: None,
        })?;

        let path = entry.path();
        let file_type = entry.file_type().map_err(|e| DomainError::Storage {
            message: format!("Failed to inspect fixture entry {}: {e}", path.display()),
            repair: None,
        })?;
        if file_type.is_symlink() {
            return Err(eval_fixture_symlink_error(
                &path,
                &path,
                "inspect fixture entry",
            ));
        }
        if !file_type.is_dir() {
            continue;
        }

        let scenario_path = path.join("scenario.json");
        let source_memory_path = path.join("source_memory.json");

        ensure_eval_fixture_path_has_no_symlink_components(
            &scenario_path,
            "inspect fixture scenario",
        )?;
        ensure_eval_fixture_path_has_no_symlink_components(
            &source_memory_path,
            "inspect fixture source memories",
        )?;

        if !path_is_regular_file_no_follow(&scenario_path)?
            || !path_is_regular_file_no_follow(&source_memory_path)?
        {
            continue;
        }

        let scenario = load_scenario(&scenario_path)?;

        fixtures.push(DiscoveredFixture {
            fixture_id: scenario.fixture_id,
            fixture_family: scenario.fixture_family,
            path,
            scenario_path,
            source_memory_path,
        });
    }

    fixtures.sort_by(|a, b| a.fixture_id.cmp(&b.fixture_id));
    Ok(fixtures)
}

/// Load a fixture scenario from JSON.
pub fn load_scenario(path: &Path) -> Result<FixtureScenario, DomainError> {
    let content = read_eval_fixture_file(path, "read fixture scenario", "scenario file")?;

    serde_json::from_str(&content).map_err(|e| DomainError::Import {
        message: format!("Failed to parse scenario JSON: {e}"),
        repair: Some("Check scenario.json syntax".into()),
    })
}

/// Load source memories from JSON.
pub fn load_source_memories(path: &Path) -> Result<SourceMemoryFile, DomainError> {
    let content =
        read_eval_fixture_file(path, "read fixture source memories", "source memory file")?;

    serde_json::from_str(&content).map_err(|e| DomainError::Import {
        message: format!("Failed to parse source memory JSON: {e}"),
        repair: Some("Check source_memory.json syntax".into()),
    })
}

/// Expand fixture memory sources into concrete records for deterministic eval runs.
pub fn materialize_source_memories(
    source: &SourceMemoryFile,
) -> Result<Vec<SourceMemory>, DomainError> {
    let mut memories = Vec::new();
    let mut seen = HashSet::new();

    for memory in &source.memories {
        if seen.insert(memory.id.clone()) {
            memories.push(memory.clone());
        }
    }

    if let Some(seed_memory) = &source.seed_memory
        && seen.insert(seed_memory.id.clone())
    {
        memories.push(seed_memory.clone());
    }

    for tier in &source.tiers {
        let Some(range) = &tier.id_range else {
            return Err(fixture_validation_error(format!(
                "source tier `{}` is missing id_range",
                tier.name
            )));
        };
        let ids = expand_source_id_range(range, &tier.name)?;
        let take_count = if tier.expected_memory_count == 0 {
            ids.len()
        } else {
            let expected = usize::try_from(tier.expected_memory_count).unwrap_or(usize::MAX);
            if expected > ids.len() {
                return Err(fixture_validation_error(format!(
                    "source tier `{}` declares {expected} memories but id_range supplies only {}",
                    tier.name,
                    ids.len()
                )));
            }
            expected
        };

        for (offset, id) in ids.into_iter().take(take_count).enumerate() {
            if !seen.insert(id.clone()) {
                continue;
            }
            let ordinal = offset + 1;
            memories.push(SourceMemory {
                id: id.clone(),
                level: "episodic".to_string(),
                kind: "generated_fixture_memory".to_string(),
                trust_class: "synthetic_generated".to_string(),
                confidence: 0.8,
                utility: 0.7,
                importance: 0.6,
                tags: vec![
                    "eval".to_string(),
                    "synthetic".to_string(),
                    tier.name.clone(),
                ],
                content: tier_generated_content(tier, &id, ordinal),
                provenance_uri: Some(format!(
                    "fixture://{}/tiers/{}#{}",
                    source.fixture_id, tier.name, id
                )),
                expected_query_match: tier.expected_query_match.clone(),
            });
        }
    }

    memories.sort_by(|left, right| left.id.cmp(&right.id));
    Ok(memories)
}

fn tier_generated_content(tier: &SourceMemoryTier, id: &str, ordinal: usize) -> String {
    let template = tier
        .generator
        .as_ref()
        .map(|generator| generator.template.as_str())
        .filter(|template| !template.trim().is_empty())
        .unwrap_or("Synthetic {tier} eval memory {n}: deterministic source record {id}.");
    let module = format!("module_{}", ordinal % 7);
    let bucket = format!("{}", ordinal % 11);

    template
        .replace("{n}", &ordinal.to_string())
        .replace("{id}", id)
        .replace("{tier}", &tier.name)
        .replace("{module}", &module)
        .replace("{bucket}", &bucket)
}

/// Validate cross-file fixture expectations that cannot be checked by JSON syntax alone.
pub fn validate_fixture_scenario(
    scenario: &FixtureScenario,
    source: &SourceMemoryFile,
) -> Result<(), DomainError> {
    if scenario.fixture_id != source.fixture_id {
        return Err(fixture_validation_error(format!(
            "fixture_id mismatch: scenario `{}` uses source `{}`",
            scenario.fixture_id, source.fixture_id
        )));
    }

    validate_structural_edges(source)?;
    validate_pack_quality_expectations(scenario, source)?;
    validate_ask_quality_expectations(scenario, source)?;
    validate_structural_recall_expectations(scenario, source)?;
    validate_semantic_recall_expectations(scenario, source)
}

fn validate_semantic_recall_expectations(
    scenario: &FixtureScenario,
    source: &SourceMemoryFile,
) -> Result<(), DomainError> {
    let Some(expectations) = &scenario.semantic_recall_expectations else {
        return Ok(());
    };

    if expectations.schema != SEMANTIC_RECALL_EXPECTATIONS_SCHEMA_V1 {
        return Err(fixture_validation_error(format!(
            "semantic_recall_expectations schema `{}` must be `{}`",
            expectations.schema, SEMANTIC_RECALL_EXPECTATIONS_SCHEMA_V1
        )));
    }
    if expectations.deterministic_seed.trim().is_empty() {
        return Err(fixture_validation_error(
            "semantic_recall_expectations deterministic_seed must not be empty",
        ));
    }
    if expectations.recall_at_k == 0 {
        return Err(fixture_validation_error(
            "semantic_recall_expectations recall_at_k must be positive",
        ));
    }
    validate_score01(
        expectations.hash_baseline_recall_at_k_max,
        "semantic_recall_expectations.hash_baseline_recall_at_k_max",
    )?;
    validate_score01(
        expectations.semantic_recall_at_k_min,
        "semantic_recall_expectations.semantic_recall_at_k_min",
    )?;
    validate_score01(
        expectations.minimum_recall_gain,
        "semantic_recall_expectations.minimum_recall_gain",
    )?;
    if expectations.semantic_recall_at_k_min <= expectations.hash_baseline_recall_at_k_max {
        return Err(fixture_validation_error(
            "semantic_recall_expectations semantic_recall_at_k_min must exceed hash_baseline_recall_at_k_max",
        ));
    }
    if expectations.cases.is_empty() {
        return Err(fixture_validation_error(
            "semantic_recall_expectations cases must not be empty",
        ));
    }

    let source_ids = source_memory_ids(source)?;
    let mut case_ids = HashSet::new();
    for case in &expectations.cases {
        validate_required_label(&case.case_id, "case_id", "<semantic_recall>")?;
        if !case_ids.insert(case.case_id.as_str()) {
            return Err(fixture_validation_error(format!(
                "duplicate semantic recall case_id `{}`",
                case.case_id
            )));
        }
        validate_required_label(&case.query, "query", &case.case_id)?;
        validate_semantic_memory_id_list(
            &case.expected_memory_ids,
            "expected_memory_ids",
            &case.case_id,
        )?;
        validate_semantic_memory_id_list(
            &case.hash_retrieved_ids,
            "hash_retrieved_ids",
            &case.case_id,
        )?;
        validate_semantic_memory_id_list(
            &case.semantic_retrieved_ids,
            "semantic_retrieved_ids",
            &case.case_id,
        )?;
        validate_known_semantic_memory_ids(
            &case.expected_memory_ids,
            &source_ids,
            "expected_memory_ids",
            &case.case_id,
        )?;
        validate_known_semantic_memory_ids(
            &case.hash_retrieved_ids,
            &source_ids,
            "hash_retrieved_ids",
            &case.case_id,
        )?;
        validate_known_semantic_memory_ids(
            &case.semantic_retrieved_ids,
            &source_ids,
            "semantic_retrieved_ids",
            &case.case_id,
        )?;
    }

    Ok(())
}

fn validate_semantic_memory_id_list(
    ids: &[String],
    field: &str,
    case_id: &str,
) -> Result<(), DomainError> {
    if ids.is_empty() {
        return Err(fixture_validation_error(format!(
            "semantic recall case `{case_id}` field `{field}` must not be empty"
        )));
    }
    let mut seen = HashSet::new();
    for id in ids {
        validate_required_label(id, field, case_id)?;
        if !seen.insert(id.as_str()) {
            return Err(fixture_validation_error(format!(
                "semantic recall case `{case_id}` field `{field}` contains duplicate id `{id}`"
            )));
        }
    }
    Ok(())
}

fn validate_known_semantic_memory_ids(
    ids: &[String],
    source_ids: &HashSet<String>,
    field: &str,
    case_id: &str,
) -> Result<(), DomainError> {
    for id in ids {
        if !source_ids.contains(id) {
            return Err(fixture_validation_error(format!(
                "semantic recall case `{case_id}` field `{field}` references unknown memory id `{id}`"
            )));
        }
    }
    Ok(())
}

fn validate_structural_edges(source: &SourceMemoryFile) -> Result<(), DomainError> {
    if source.structural_edges.is_empty() {
        return Ok(());
    }

    let source_ids = source_memory_ids(source)?;
    let mut seen = HashSet::new();
    for edge in &source.structural_edges {
        validate_required_structural_field(&edge.source_id, "source_id")?;
        validate_required_structural_field(&edge.target_id, "target_id")?;
        validate_required_structural_field(&edge.relation, "relation")?;

        if edge.source_id == edge.target_id {
            return Err(fixture_validation_error(format!(
                "structural edge `{}` -> `{}` must not be a self-edge",
                edge.source_id, edge.target_id
            )));
        }
        if !source_ids.contains(&edge.source_id) {
            return Err(fixture_validation_error(format!(
                "structural edge references unknown source_id `{}`",
                edge.source_id
            )));
        }
        if !source_ids.contains(&edge.target_id) {
            return Err(fixture_validation_error(format!(
                "structural edge references unknown target_id `{}`",
                edge.target_id
            )));
        }
        if !edge.weight.is_finite() || !(0.0..=1.0).contains(&edge.weight) || edge.weight == 0.0 {
            return Err(fixture_validation_error(format!(
                "structural edge `{}` -> `{}` weight must be > 0.0 and <= 1.0",
                edge.source_id, edge.target_id
            )));
        }

        let key = format!("{}:{}:{}", edge.source_id, edge.target_id, edge.relation);
        if !seen.insert(key) {
            return Err(fixture_validation_error(format!(
                "duplicate structural edge `{}` -> `{}` relation `{}`",
                edge.source_id, edge.target_id, edge.relation
            )));
        }
    }

    Ok(())
}

fn validate_required_structural_field(value: &str, field: &str) -> Result<(), DomainError> {
    if value.trim().is_empty() {
        return Err(fixture_validation_error(format!(
            "structural edge field `{field}` must not be empty"
        )));
    }
    Ok(())
}

fn validate_structural_recall_expectations(
    scenario: &FixtureScenario,
    source: &SourceMemoryFile,
) -> Result<(), DomainError> {
    let Some(expectations) = &scenario.structural_recall_expectations else {
        return Ok(());
    };

    if expectations.schema != STRUCTURAL_RECALL_EXPECTATIONS_SCHEMA_V1 {
        return Err(fixture_validation_error(format!(
            "structural_recall_expectations schema `{}` must be `{}`",
            expectations.schema, STRUCTURAL_RECALL_EXPECTATIONS_SCHEMA_V1
        )));
    }

    if expectations.required_scenario_ids.is_empty() {
        return Err(fixture_validation_error(
            "structural_recall_expectations required_scenario_ids must not be empty",
        ));
    }

    let scenario_ids: HashSet<&str> = scenario.scenario_ids.iter().map(String::as_str).collect();
    let mut seen = HashSet::new();
    for scenario_id in &expectations.required_scenario_ids {
        validate_required_label(scenario_id, "required_scenario_ids", "<structural_recall>")?;
        if !seen.insert(scenario_id.as_str()) {
            return Err(fixture_validation_error(format!(
                "duplicate structural recall scenario_id `{scenario_id}`"
            )));
        }
        if !scenario_ids.contains(scenario_id.as_str()) {
            return Err(fixture_validation_error(format!(
                "structural recall expects unknown scenario_id `{scenario_id}`"
            )));
        }
    }

    validate_score01(
        expectations.ndcg_at_10_baseline,
        "structural_recall_expectations.ndcg_at_10_baseline",
    )?;
    validate_score01(
        expectations.structural_recall_at_10_min,
        "structural_recall_expectations.structural_recall_at_10_min",
    )?;
    if expectations.structural_recall_at_10_min <= 0.0 {
        return Err(fixture_validation_error(
            "structural_recall_expectations.structural_recall_at_10_min must be positive",
        ));
    }
    if !expectations.allowed_ndcg_drop_pp.is_finite()
        || !(0.0..=100.0).contains(&expectations.allowed_ndcg_drop_pp)
    {
        return Err(fixture_validation_error(
            "structural_recall_expectations.allowed_ndcg_drop_pp must be between 0.0 and 100.0",
        ));
    }

    validate_snapshot_report_path(&expectations.baseline_report_path)?;
    validate_snapshot_report_path(&expectations.post_g1_report_path)?;

    if scenario.fixture_family == "structural_recall"
        || scenario
            .owning_bead_ids
            .iter()
            .any(|bead| bead == "bd-bife.11")
    {
        validate_bd_bife_11_structural_recall_contract(scenario, source, expectations)?;
    }

    Ok(())
}

fn validate_score01(value: f64, field: &str) -> Result<(), DomainError> {
    if !value.is_finite() || !(0.0..=1.0).contains(&value) {
        return Err(fixture_validation_error(format!(
            "{field} must be between 0.0 and 1.0"
        )));
    }
    Ok(())
}

fn validate_snapshot_report_path(path: &str) -> Result<(), DomainError> {
    if path.trim().is_empty() {
        return Err(fixture_validation_error(
            "structural recall report paths must not be empty",
        ));
    }
    let report_path = Path::new(path);
    if report_path.is_absolute() {
        return Err(fixture_validation_error(format!(
            "structural recall report path `{path}` must be relative"
        )));
    }
    if report_path
        .components()
        .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        return Err(fixture_validation_error(format!(
            "structural recall report path `{path}` must not escape the repository"
        )));
    }
    if !path.starts_with("tests/snapshots/") || !path.ends_with(".snap") {
        return Err(fixture_validation_error(format!(
            "structural recall report path `{path}` must live under tests/snapshots/*.snap"
        )));
    }
    Ok(())
}

const BD_BIFE_11_STRUCTURAL_SCENARIOS: &[&str] = &[
    "orphan_query",
    "over_grounding",
    "related_concept",
    "contradicted_belief",
    "fresh_workspace",
    "derived_revision",
];

fn validate_bd_bife_11_structural_recall_contract(
    scenario: &FixtureScenario,
    source: &SourceMemoryFile,
    expectations: &StructuralRecallExpectations,
) -> Result<(), DomainError> {
    let scenario_ids: HashSet<&str> = scenario.scenario_ids.iter().map(String::as_str).collect();
    let required_ids: HashSet<&str> = expectations
        .required_scenario_ids
        .iter()
        .map(String::as_str)
        .collect();
    let pack_quality_ids: HashSet<&str> = scenario
        .pack_quality_expectations
        .as_ref()
        .map(|expectations| {
            expectations
                .cases
                .iter()
                .map(|case| case.scenario_id.as_str())
                .collect()
        })
        .unwrap_or_default();

    for required in BD_BIFE_11_STRUCTURAL_SCENARIOS {
        if !scenario_ids.contains(required) {
            return Err(fixture_validation_error(format!(
                "bd-bife.11 structural recall fixture must include scenario_id `{required}`"
            )));
        }
        if !required_ids.contains(required) {
            return Err(fixture_validation_error(format!(
                "bd-bife.11 structural recall expectations must require scenario_id `{required}`"
            )));
        }
        if !pack_quality_ids.contains(required) {
            return Err(fixture_validation_error(format!(
                "bd-bife.11 structural recall fixture must include a pack-quality case for `{required}`"
            )));
        }
    }

    if expectations.structural_recall_at_10_min < 0.75 {
        return Err(fixture_validation_error(
            "bd-bife.11 structural_recall_at_10_min must be at least 0.75",
        ));
    }
    if expectations.allowed_ndcg_drop_pp > 2.0 {
        return Err(fixture_validation_error(
            "bd-bife.11 allowed_ndcg_drop_pp must be no more than 2.0",
        ));
    }
    if source.structural_edges.is_empty() {
        return Err(fixture_validation_error(
            "bd-bife.11 structural recall fixture must declare structural_edges",
        ));
    }

    Ok(())
}

fn validate_pack_quality_expectations(
    scenario: &FixtureScenario,
    source: &SourceMemoryFile,
) -> Result<(), DomainError> {
    let Some(expectations) = &scenario.pack_quality_expectations else {
        return Ok(());
    };

    if expectations.schema != PACK_QUALITY_EXPECTATIONS_SCHEMA_V1 {
        return Err(fixture_validation_error(format!(
            "pack_quality_expectations schema `{}` must be `{}`",
            expectations.schema, PACK_QUALITY_EXPECTATIONS_SCHEMA_V1
        )));
    }

    if expectations.cases.is_empty() {
        return Err(fixture_validation_error(
            "pack_quality_expectations cases must not be empty",
        ));
    }

    let source_ids = source_memory_ids(source)?;
    let scenario_ids: HashSet<&str> = scenario.scenario_ids.iter().map(String::as_str).collect();
    let command_steps: HashSet<u32> = scenario
        .command_sequence
        .iter()
        .map(|command| command.step)
        .collect();
    let degradation_codes: HashSet<&str> = scenario
        .degraded_branches
        .iter()
        .map(|branch| branch.code.as_str())
        .collect();
    let mut case_ids = HashSet::new();
    let mut scenario_refs = HashSet::new();

    for case in &expectations.cases {
        validate_required_label(&case.case_id, "case_id", "<pack_quality>")?;
        if !case_ids.insert(case.case_id.as_str()) {
            return Err(fixture_validation_error(format!(
                "duplicate pack-quality case_id `{}`",
                case.case_id
            )));
        }

        validate_required_label(&case.scenario_id, "scenario_id", &case.case_id)?;
        if !scenario_ids.contains(case.scenario_id.as_str()) {
            return Err(fixture_validation_error(format!(
                "pack-quality case `{}` references unknown scenario_id `{}`",
                case.case_id, case.scenario_id
            )));
        }

        if !command_steps.contains(&case.command_step) {
            return Err(fixture_validation_error(format!(
                "pack-quality case `{}` references unknown command step {}",
                case.case_id, case.command_step
            )));
        }

        let scenario_ref = format!(
            "{}:{}:{}",
            case.scenario_id, case.command_step, case.query_surface.kind
        );
        if !scenario_refs.insert(scenario_ref) {
            return Err(fixture_validation_error(format!(
                "pack-quality case `{}` duplicates an ambiguous scenario/query reference",
                case.case_id
            )));
        }

        validate_query_surface(case)?;
        validate_memory_id_list(
            &case.expected_selected_memory_ids,
            "expected_selected_memory_ids",
            case,
        )?;
        validate_memory_id_list(
            &case.critical_omitted_memory_ids,
            "critical_omitted_memory_ids",
            case,
        )?;
        validate_known_memory_ids(
            &case.expected_selected_memory_ids,
            &source_ids,
            "expected_selected_memory_ids",
            case,
        )?;
        validate_known_memory_ids(
            &case.critical_omitted_memory_ids,
            &source_ids,
            "critical_omitted_memory_ids",
            case,
        )?;
        validate_no_selection_overlap(case)?;
        validate_provenance_density(case)?;
        validate_degradation_codes(case, &degradation_codes)?;
        validate_forbidden_redaction_leaks(case)?;
        validate_token_budget(case)?;
        validate_failure_label(case)?;
    }

    Ok(())
}

fn validate_ask_quality_expectations(
    scenario: &FixtureScenario,
    source: &SourceMemoryFile,
) -> Result<(), DomainError> {
    let Some(expectations) = &scenario.ask_quality_expectations else {
        return Ok(());
    };

    if expectations.schema != ASK_QUALITY_EXPECTATIONS_SCHEMA_V1 {
        return Err(fixture_validation_error(format!(
            "ask_quality_expectations schema `{}` must be `{}`",
            expectations.schema, ASK_QUALITY_EXPECTATIONS_SCHEMA_V1
        )));
    }

    if expectations.cases.is_empty() {
        return Err(fixture_validation_error(
            "ask_quality_expectations cases must not be empty",
        ));
    }

    validate_score01(
        expectations.thresholds.citation_precision_min,
        "ask_quality_expectations.thresholds.citation_precision_min",
    )?;
    validate_score01(
        expectations.thresholds.answer_exactness_min,
        "ask_quality_expectations.thresholds.answer_exactness_min",
    )?;
    validate_score01(
        expectations.thresholds.abstention_calibration_min,
        "ask_quality_expectations.thresholds.abstention_calibration_min",
    )?;
    validate_score01(
        expectations.thresholds.conflict_recall_min,
        "ask_quality_expectations.thresholds.conflict_recall_min",
    )?;

    let source_ids = source_memory_ids(source)?;
    let scenario_ids: HashSet<&str> = scenario.scenario_ids.iter().map(String::as_str).collect();
    let command_steps: HashSet<u32> = scenario
        .command_sequence
        .iter()
        .map(|command| command.step)
        .collect();
    let mut case_ids = HashSet::new();

    for case in &expectations.cases {
        validate_required_label(&case.case_id, "case_id", "<ask_quality>")?;
        if !case_ids.insert(case.case_id.as_str()) {
            return Err(fixture_validation_error(format!(
                "duplicate ask-quality case_id `{}`",
                case.case_id
            )));
        }

        validate_required_label(&case.scenario_id, "scenario_id", &case.case_id)?;
        if !scenario_ids.contains(case.scenario_id.as_str()) {
            return Err(fixture_validation_error(format!(
                "ask-quality case `{}` references unknown scenario_id `{}`",
                case.case_id, case.scenario_id
            )));
        }

        if !command_steps.contains(&case.command_step) {
            return Err(fixture_validation_error(format!(
                "ask-quality case `{}` references unknown command step {}",
                case.case_id, case.command_step
            )));
        }

        validate_required_label(&case.question, "question", &case.case_id)?;
        validate_ask_memory_id_list(
            &case.expected_cited_memory_ids,
            "expected_cited_memory_ids",
            &case.case_id,
        )?;
        validate_known_ask_memory_ids(
            &case.expected_cited_memory_ids,
            &source_ids,
            "expected_cited_memory_ids",
            &case.case_id,
        )?;

        if case.expect_abstention {
            if !case.expected_cited_memory_ids.is_empty() {
                return Err(fixture_validation_error(format!(
                    "ask-quality case `{}` expects abstention but declares expected cited memories",
                    case.case_id
                )));
            }
        } else {
            if case.expected_cited_memory_ids.is_empty() {
                return Err(fixture_validation_error(format!(
                    "ask-quality case `{}` must declare expected cited memories unless it expects abstention",
                    case.case_id
                )));
            }
            if case.expected_answer_terms.is_empty() {
                return Err(fixture_validation_error(format!(
                    "ask-quality case `{}` must declare expected_answer_terms unless it expects abstention",
                    case.case_id
                )));
            }
            for term in &case.expected_answer_terms {
                validate_required_label(term, "expected_answer_terms", &case.case_id)?;
            }
        }

        if case.expect_conflict {
            if case.expected_sides.len() < 2 {
                return Err(fixture_validation_error(format!(
                    "ask-quality case `{}` expects conflict but declares fewer than two expected_sides",
                    case.case_id
                )));
            }
            for side in &case.expected_sides {
                validate_required_label(&side.label, "expected_sides.label", &case.case_id)?;
                if side.cited_memory_ids.is_empty() {
                    return Err(fixture_validation_error(format!(
                        "ask-quality case `{}` side `{}` must cite at least one memory",
                        case.case_id, side.label
                    )));
                }
                validate_known_ask_memory_ids(
                    &side.cited_memory_ids,
                    &source_ids,
                    "expected_sides.cited_memory_ids",
                    &case.case_id,
                )?;
            }
        } else if !case.expected_sides.is_empty() {
            return Err(fixture_validation_error(format!(
                "ask-quality case `{}` declares expected_sides without expect_conflict",
                case.case_id
            )));
        }
    }

    Ok(())
}

fn validate_ask_memory_id_list(
    ids: &[String],
    field: &str,
    case_id: &str,
) -> Result<(), DomainError> {
    let mut seen = HashSet::new();
    for id in ids {
        validate_required_label(id, field, case_id)?;
        if !seen.insert(id.as_str()) {
            return Err(fixture_validation_error(format!(
                "ask-quality case `{case_id}` has duplicate memory ID `{id}` in `{field}`"
            )));
        }
    }
    Ok(())
}

fn validate_known_ask_memory_ids(
    ids: &[String],
    source_ids: &HashSet<String>,
    field: &str,
    case_id: &str,
) -> Result<(), DomainError> {
    for id in ids {
        if !source_ids.contains(id) {
            return Err(fixture_validation_error(format!(
                "ask-quality case `{case_id}` has unknown memory ID `{id}` in `{field}`"
            )));
        }
    }
    Ok(())
}

fn fixture_validation_error(message: impl Into<String>) -> DomainError {
    DomainError::Configuration {
        message: message.into(),
        repair: Some("Fix the eval fixture scenario/source memory contract.".to_string()),
    }
}

fn validate_required_label(value: &str, field: &str, case_id: &str) -> Result<(), DomainError> {
    if value.trim().is_empty() {
        return Err(fixture_validation_error(format!(
            "pack-quality case `{case_id}` field `{field}` must not be empty"
        )));
    }
    Ok(())
}

fn validate_query_surface(case: &PackQualityCase) -> Result<(), DomainError> {
    match case.query_surface.kind.as_str() {
        "inline_query" => {
            validate_required_option(
                case.query_surface.query.as_deref(),
                "query_surface.query",
                case,
            )?;
            if case.query_surface.path.is_some() {
                return Err(fixture_validation_error(format!(
                    "pack-quality case `{}` inline_query must not set query_surface.path",
                    case.case_id
                )));
            }
        }
        "query_file" => {
            validate_required_option(
                case.query_surface.path.as_deref(),
                "query_surface.path",
                case,
            )?;
            if case.query_surface.schema.as_deref() != Some("ee.query.v1") {
                return Err(fixture_validation_error(format!(
                    "pack-quality case `{}` query_file must declare schema `ee.query.v1`",
                    case.case_id
                )));
            }
        }
        other => {
            return Err(fixture_validation_error(format!(
                "pack-quality case `{}` has invalid query_surface.kind `{other}`",
                case.case_id
            )));
        }
    }

    Ok(())
}

fn validate_required_option(
    value: Option<&str>,
    field: &str,
    case: &PackQualityCase,
) -> Result<(), DomainError> {
    match value {
        Some(value) if !value.trim().is_empty() => Ok(()),
        _ => Err(fixture_validation_error(format!(
            "pack-quality case `{}` field `{field}` must not be empty",
            case.case_id
        ))),
    }
}

fn validate_memory_id_list(
    ids: &[String],
    field: &str,
    case: &PackQualityCase,
) -> Result<(), DomainError> {
    let mut seen = HashSet::new();
    for id in ids {
        validate_required_label(id, field, &case.case_id)?;
        if !seen.insert(id.as_str()) {
            return Err(fixture_validation_error(format!(
                "pack-quality case `{}` has duplicate memory ID `{id}` in `{field}`",
                case.case_id
            )));
        }
    }
    Ok(())
}

fn validate_known_memory_ids(
    ids: &[String],
    source_ids: &HashSet<String>,
    field: &str,
    case: &PackQualityCase,
) -> Result<(), DomainError> {
    for id in ids {
        if !source_ids.contains(id) {
            return Err(fixture_validation_error(format!(
                "pack-quality case `{}` has unknown memory ID `{id}` in `{field}`",
                case.case_id
            )));
        }
    }
    Ok(())
}

fn validate_no_selection_overlap(case: &PackQualityCase) -> Result<(), DomainError> {
    let selected: HashSet<&str> = case
        .expected_selected_memory_ids
        .iter()
        .map(String::as_str)
        .collect();
    for omitted in &case.critical_omitted_memory_ids {
        if selected.contains(omitted.as_str()) {
            return Err(fixture_validation_error(format!(
                "pack-quality case `{}` memory ID `{omitted}` is both selected and omitted",
                case.case_id
            )));
        }
    }
    Ok(())
}

fn validate_provenance_density(case: &PackQualityCase) -> Result<(), DomainError> {
    if !case.min_provenance_density.is_finite()
        || !(0.0..=1.0).contains(&case.min_provenance_density)
    {
        return Err(fixture_validation_error(format!(
            "pack-quality case `{}` min_provenance_density must be between 0.0 and 1.0",
            case.case_id
        )));
    }
    Ok(())
}

fn validate_degradation_codes(
    case: &PackQualityCase,
    fixture_codes: &HashSet<&str>,
) -> Result<(), DomainError> {
    let mut seen = HashSet::new();
    for code in &case.allowed_degradation_codes {
        validate_required_label(code, "allowed_degradation_codes", &case.case_id)?;
        if !seen.insert(code.as_str()) {
            return Err(fixture_validation_error(format!(
                "pack-quality case `{}` has duplicate degradation code `{code}`",
                case.case_id
            )));
        }
        if !fixture_codes.contains(code.as_str()) {
            return Err(fixture_validation_error(format!(
                "pack-quality case `{}` has invalid degradation code `{code}`",
                case.case_id
            )));
        }
    }
    Ok(())
}

fn validate_forbidden_redaction_leaks(case: &PackQualityCase) -> Result<(), DomainError> {
    if case.forbidden_redaction_leaks.is_empty() {
        return Err(fixture_validation_error(format!(
            "pack-quality case `{}` forbidden_redaction_leaks must not be empty",
            case.case_id
        )));
    }
    validate_memory_id_list(
        &case.forbidden_redaction_leaks,
        "forbidden_redaction_leaks",
        case,
    )
}

fn validate_token_budget(case: &PackQualityCase) -> Result<(), DomainError> {
    let budget = &case.token_budget;
    if budget.max_tokens == 0 {
        return Err(fixture_validation_error(format!(
            "pack-quality case `{}` token_budget.max_tokens must be positive",
            case.case_id
        )));
    }
    if budget.expected_used_tokens_max == 0 {
        return Err(fixture_validation_error(format!(
            "pack-quality case `{}` token_budget.expected_used_tokens_max must be positive",
            case.case_id
        )));
    }
    if budget.expected_used_tokens_max > budget.max_tokens {
        return Err(fixture_validation_error(format!(
            "pack-quality case `{}` token_budget expected usage exceeds max_tokens",
            case.case_id
        )));
    }
    Ok(())
}

fn validate_failure_label(case: &PackQualityCase) -> Result<(), DomainError> {
    let label = case.stable_first_failure_label.as_str();
    validate_required_label(label, "stable_first_failure_label", &case.case_id)?;
    let mut chars = label.chars();
    let Some(first) = chars.next() else {
        return Err(fixture_validation_error(format!(
            "pack-quality case `{}` stable_first_failure_label must not be empty",
            case.case_id
        )));
    };
    if !first.is_ascii_lowercase()
        || !chars.all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '_')
    {
        return Err(fixture_validation_error(format!(
            "pack-quality case `{}` stable_first_failure_label must be snake_case",
            case.case_id
        )));
    }
    Ok(())
}

fn source_memory_ids(source: &SourceMemoryFile) -> Result<HashSet<String>, DomainError> {
    let mut ids: HashSet<String> = source
        .memories
        .iter()
        .map(|memory| memory.id.clone())
        .collect();
    if let Some(seed_memory) = &source.seed_memory {
        ids.insert(seed_memory.id.clone());
    }
    for tier in &source.tiers {
        if let Some(range) = &tier.id_range {
            ids.extend(expand_source_id_range(range, &tier.name)?);
        }
    }
    Ok(ids)
}

fn expand_source_id_range(
    range: &SourceMemoryIdRange,
    tier_name: &str,
) -> Result<Vec<String>, DomainError> {
    let Some((start_prefix, start_number, start_width)) = stable_numeric_suffix(&range.start)
    else {
        return Err(fixture_validation_error(format!(
            "source tier `{tier_name}` id_range.start `{}` has no numeric suffix",
            range.start
        )));
    };
    let Some((end_prefix, end_number, end_width)) = stable_numeric_suffix(&range.end) else {
        return Err(fixture_validation_error(format!(
            "source tier `{tier_name}` id_range.end `{}` has no numeric suffix",
            range.end
        )));
    };

    if start_prefix != end_prefix || start_width != end_width || end_number < start_number {
        return Err(fixture_validation_error(format!(
            "source tier `{tier_name}` id_range must use one ordered stable ID prefix"
        )));
    }

    let count = end_number.saturating_sub(start_number).saturating_add(1);
    if count > 10_000 {
        return Err(fixture_validation_error(format!(
            "source tier `{tier_name}` id_range is too large for fixture validation"
        )));
    }

    Ok((start_number..=end_number)
        .map(|number| format!("{start_prefix}{number:0start_width$}"))
        .collect())
}

fn stable_numeric_suffix(value: &str) -> Option<(&str, u64, usize)> {
    let split_at = value
        .rfind(|ch: char| !ch.is_ascii_digit())
        .map_or(0, |index| {
            index + value[index..].chars().next().map_or(0, |ch| ch.len_utf8())
        });
    let digits = value.get(split_at..)?;
    if digits.is_empty() {
        return None;
    }
    let number = digits.parse().ok()?;
    Some((&value[..split_at], number, digits.len()))
}

fn source_memory_counts(path: &Path) -> Result<(usize, usize), DomainError> {
    let source = load_source_memories(path)?;
    let memories = materialize_source_memories(&source)?;

    let query_count = memories
        .iter()
        .flat_map(|memory| &memory.expected_query_match)
        .collect::<HashSet<_>>()
        .len();

    Ok((memories.len(), query_count))
}

fn path_is_regular_file_no_follow(path: &Path) -> Result<bool, DomainError> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) => Ok(metadata.file_type().is_file()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(DomainError::Storage {
            message: format!("Failed to inspect fixture file {}: {error}", path.display()),
            repair: None,
        }),
    }
}

fn read_eval_fixture_file(
    path: &Path,
    operation: &'static str,
    label: &'static str,
) -> Result<String, DomainError> {
    ensure_eval_fixture_regular_file(path, operation)?;
    let file = std::fs::File::open(path).map_err(|e| DomainError::Storage {
        message: format!("Failed to read {label} {}: {e}", path.display()),
        repair: None,
    })?;
    let mut limited = file.take(EVAL_FIXTURE_FILE_MAX_BYTES.saturating_add(1));
    let mut bytes = Vec::with_capacity(8 * 1024);
    limited
        .read_to_end(&mut bytes)
        .map_err(|e| DomainError::Storage {
            message: format!("Failed to read {label} {}: {e}", path.display()),
            repair: None,
        })?;
    if bytes.len() as u64 > EVAL_FIXTURE_FILE_MAX_BYTES {
        return Err(eval_fixture_file_too_large_error(
            path,
            operation,
            bytes.len() as u64,
        ));
    }
    String::from_utf8(bytes).map_err(|e| DomainError::Storage {
        message: format!("Failed to read {label} {} as UTF-8: {e}", path.display()),
        repair: None,
    })
}

fn ensure_eval_fixture_regular_file(
    path: &Path,
    operation: &'static str,
) -> Result<(), DomainError> {
    ensure_eval_fixture_path_has_no_symlink_components(path, operation)?;
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_file() => {
            if metadata.len() > EVAL_FIXTURE_FILE_MAX_BYTES {
                return Err(eval_fixture_file_too_large_error(
                    path,
                    operation,
                    metadata.len(),
                ));
            }
            Ok(())
        }
        Ok(_) => Err(DomainError::Storage {
            message: format!(
                "Refusing to {operation} {} because it is not a regular file.",
                path.display()
            ),
            repair: Some("Replace eval fixture paths with regular JSON files.".into()),
        }),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(DomainError::Storage {
            message: format!("Failed to inspect fixture file {}: {error}", path.display()),
            repair: None,
        }),
    }
}

fn eval_fixture_file_too_large_error(
    path: &Path,
    operation: &'static str,
    len: u64,
) -> DomainError {
    DomainError::Storage {
        message: format!(
            "Refusing to {operation} {} because it is {len} bytes, above the {EVAL_FIXTURE_FILE_MAX_BYTES}-byte cap.",
            path.display()
        ),
        repair: Some("Reduce the eval fixture JSON size or split it into smaller fixtures.".into()),
    }
}

fn ensure_eval_fixture_path_has_no_symlink_components(
    path: &Path,
    operation: &'static str,
) -> Result<(), DomainError> {
    let mut current = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::Prefix(_) | std::path::Component::RootDir => {
                current.push(component.as_os_str());
                continue;
            }
            std::path::Component::CurDir => continue,
            std::path::Component::ParentDir | std::path::Component::Normal(_) => {
                current.push(component.as_os_str());
            }
        }

        match std::fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(eval_fixture_symlink_error(path, &current, operation));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => {
                return Err(DomainError::Storage {
                    message: format!(
                        "Failed to inspect eval fixture path component {}: {error}",
                        current.display()
                    ),
                    repair: None,
                });
            }
        }
    }
    Ok(())
}

fn eval_fixture_symlink_error(
    path: &Path,
    symlink_path: &Path,
    operation: &'static str,
) -> DomainError {
    DomainError::Storage {
        message: format!(
            "Refusing to {operation} {} through symlinked path component {}.",
            path.display(),
            symlink_path.display()
        ),
        repair: Some(
            "Replace symlinked eval fixture paths with regular directories and files.".into(),
        ),
    }
}

/// List all available fixtures with metadata.
pub fn list_fixtures(fixture_dir: &Path) -> Result<Vec<FixtureListEntry>, DomainError> {
    let discovered = discover_fixtures(fixture_dir)?;
    let mut entries = Vec::with_capacity(discovered.len());

    for fixture in discovered {
        let scenario = load_scenario(&fixture.scenario_path)?;
        let (memory_count, query_count) = source_memory_counts(&fixture.source_memory_path)?;

        entries.push(FixtureListEntry {
            fixture_id: fixture.fixture_id,
            fixture_family: fixture.fixture_family,
            journey: scenario.journey,
            memory_count,
            query_count,
            path: fixture.path.display().to_string(),
        });
    }

    Ok(entries)
}

/// Compute precision at k.
fn precision_at_k(retrieved: &[String], relevant: &HashSet<String>, k: usize) -> f64 {
    if k == 0 {
        return 0.0;
    }
    let mut seen = HashSet::new();
    let mut considered = 0_usize;
    let mut hits = 0_usize;
    for id in retrieved.iter().take(k) {
        considered += 1;
        if seen.insert(id) && relevant.contains(id) {
            hits += 1;
        }
    }
    if considered == 0 {
        return 0.0;
    }
    hits as f64 / considered as f64
}

/// Compute recall at k.
fn recall_at_k(retrieved: &[String], relevant: &HashSet<String>, k: usize) -> f64 {
    if relevant.is_empty() {
        return 1.0;
    }
    let top_k: HashSet<_> = retrieved.iter().take(k).collect();
    let hits = relevant.iter().filter(|id| top_k.contains(id)).count();
    hits as f64 / relevant.len() as f64
}

/// Compute nDCG at k.
fn ndcg_at_k(retrieved: &[String], relevant: &HashSet<String>, k: usize) -> f64 {
    if k == 0 || relevant.is_empty() {
        return 0.0;
    }

    let mut seen = HashSet::new();
    let mut dcg = 0.0;
    for (i, id) in retrieved.iter().take(k).enumerate() {
        if seen.insert(id) && relevant.contains(id) {
            dcg += 1.0 / (i as f64 + 2.0).log2();
        }
    }

    let ideal_count = relevant.len().min(k);
    let idcg: f64 = (0..ideal_count)
        .map(|i| 1.0 / (i as f64 + 2.0).log2())
        .sum();

    if idcg < f64::EPSILON { 0.0 } else { dcg / idcg }
}

/// Compute mean reciprocal rank.
fn mrr(retrieved: &[String], relevant: &HashSet<String>) -> f64 {
    for (i, id) in retrieved.iter().enumerate() {
        if relevant.contains(id) {
            return 1.0 / (i as f64 + 1.0);
        }
    }
    0.0
}

/// Find the rank of first relevant result (1-based).
fn first_relevant_rank(retrieved: &[String], relevant: &HashSet<String>) -> Option<u32> {
    for (i, id) in retrieved.iter().enumerate() {
        if relevant.contains(id) {
            return Some((i + 1) as u32);
        }
    }
    None
}

/// Compute metrics for a single query.
pub fn compute_query_metrics(
    query: &str,
    expected_ids: &[String],
    retrieved_ids: &[String],
) -> QueryMetrics {
    let relevant: HashSet<String> = expected_ids.iter().cloned().collect();

    QueryMetrics {
        query: query.to_string(),
        expected_ids: expected_ids.to_vec(),
        retrieved_ids: retrieved_ids.to_vec(),
        precision_at_1: precision_at_k(retrieved_ids, &relevant, 1),
        precision_at_3: precision_at_k(retrieved_ids, &relevant, 3),
        precision_at_5: precision_at_k(retrieved_ids, &relevant, 5),
        recall_at_5: recall_at_k(retrieved_ids, &relevant, 5),
        ndcg_at_5: ndcg_at_k(retrieved_ids, &relevant, 5),
        mrr: mrr(retrieved_ids, &relevant),
        first_relevant_rank: first_relevant_rank(retrieved_ids, &relevant),
    }
}

/// Compute aggregate metrics from per-query metrics.
pub fn compute_fixture_metrics(
    fixture_id: &str,
    mut per_query: Vec<QueryMetrics>,
) -> FixtureMetrics {
    per_query.sort_by(|left, right| left.query.cmp(&right.query));

    let n = per_query.len();
    if n == 0 {
        return FixtureMetrics {
            fixture_id: fixture_id.to_string(),
            ..Default::default()
        };
    }

    let sum_p1: f64 = per_query.iter().map(|q| q.precision_at_1).sum();
    let sum_p3: f64 = per_query.iter().map(|q| q.precision_at_3).sum();
    let sum_p5: f64 = per_query.iter().map(|q| q.precision_at_5).sum();
    let sum_r5: f64 = per_query.iter().map(|q| q.recall_at_5).sum();
    let sum_ndcg: f64 = per_query.iter().map(|q| q.ndcg_at_5).sum();
    let sum_mrr: f64 = per_query.iter().map(|q| q.mrr).sum();

    let n_f64 = n as f64;

    FixtureMetrics {
        fixture_id: fixture_id.to_string(),
        queries_evaluated: n as u32,
        mean_precision_at_1: sum_p1 / n_f64,
        mean_precision_at_3: sum_p3 / n_f64,
        mean_precision_at_5: sum_p5 / n_f64,
        mean_recall_at_5: sum_r5 / n_f64,
        mean_ndcg_at_5: sum_ndcg / n_f64,
        mean_mrr: sum_mrr / n_f64,
        per_query,
    }
}

pub fn evaluate_semantic_recall_expectations(
    fixture_id: &str,
    expectations: &SemanticRecallExpectations,
) -> SemanticRecallReport {
    let k = usize::try_from(expectations.recall_at_k).unwrap_or(usize::MAX);
    let cases = expectations
        .cases
        .iter()
        .map(|case| {
            let relevant = case
                .expected_memory_ids
                .iter()
                .cloned()
                .collect::<HashSet<_>>();
            let hash_recall = recall_at_k(&case.hash_retrieved_ids, &relevant, k);
            let semantic_recall = recall_at_k(&case.semantic_retrieved_ids, &relevant, k);
            SemanticRecallCaseReport {
                case_id: case.case_id.clone(),
                query: case.query.clone(),
                expected_memory_ids: case.expected_memory_ids.clone(),
                hash_retrieved_ids: case.hash_retrieved_ids.clone(),
                semantic_retrieved_ids: case.semantic_retrieved_ids.clone(),
                hash_recall_at_k: hash_recall,
                semantic_recall_at_k: semantic_recall,
                recall_gain: semantic_recall - hash_recall,
                first_semantic_rank: first_relevant_rank(&case.semantic_retrieved_ids, &relevant),
            }
        })
        .collect::<Vec<_>>();

    let case_count = cases.len();
    let divisor = if case_count == 0 {
        1.0
    } else {
        case_count as f64
    };
    let hash_baseline_recall_at_k =
        cases.iter().map(|case| case.hash_recall_at_k).sum::<f64>() / divisor;
    let semantic_recall_at_k = cases
        .iter()
        .map(|case| case.semantic_recall_at_k)
        .sum::<f64>()
        / divisor;
    let recall_gain = semantic_recall_at_k - hash_baseline_recall_at_k;
    let thresholds = SemanticRecallThresholds {
        hash_baseline_recall_at_k_max: expectations.hash_baseline_recall_at_k_max,
        semantic_recall_at_k_min: expectations.semantic_recall_at_k_min,
        minimum_recall_gain: expectations.minimum_recall_gain,
    };
    let passed = case_count > 0
        && hash_baseline_recall_at_k <= thresholds.hash_baseline_recall_at_k_max
        && semantic_recall_at_k >= thresholds.semantic_recall_at_k_min
        && recall_gain >= thresholds.minimum_recall_gain;

    SemanticRecallReport {
        schema: SEMANTIC_RECALL_REPORT_SCHEMA_V1,
        fixture_id: fixture_id.to_owned(),
        deterministic_seed: expectations.deterministic_seed.clone(),
        recall_at_k: expectations.recall_at_k,
        case_count: u32::try_from(case_count).unwrap_or(u32::MAX),
        hash_baseline_recall_at_k,
        semantic_recall_at_k,
        recall_gain,
        thresholds,
        passed,
        cases,
    }
}

/// Compute data hash for determinism verification.
pub fn compute_data_hash(report: &EvalRunReport) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(report.fixture_id.as_bytes());
    hasher.update(report.fixture_family.as_bytes());
    hasher.update(&report.metrics.queries_evaluated.to_le_bytes());

    for q in &report.metrics.per_query {
        hasher.update(q.query.as_bytes());
        for id in &q.expected_ids {
            hasher.update(id.as_bytes());
        }
        for id in &q.retrieved_ids {
            hasher.update(id.as_bytes());
        }
    }
    if let Some(semantic_recall) = &report.semantic_recall {
        hasher.update(semantic_recall.schema.as_bytes());
        hasher.update(semantic_recall.deterministic_seed.as_bytes());
        hasher.update(&semantic_recall.recall_at_k.to_le_bytes());
        for case in &semantic_recall.cases {
            hasher.update(case.case_id.as_bytes());
            hasher.update(case.query.as_bytes());
            for id in &case.expected_memory_ids {
                hasher.update(id.as_bytes());
            }
            for id in &case.hash_retrieved_ids {
                hasher.update(id.as_bytes());
            }
            for id in &case.semantic_retrieved_ids {
                hasher.update(id.as_bytes());
            }
        }
    }

    format!("blake3:{}", hasher.finalize().to_hex())
}

pub use crate::models::PACK_QUALITY_REPORT_SCHEMA_V1;

/// Deterministic verdict for a pack-quality comparison.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PackQualityVerdict {
    /// All expectations matched exactly.
    Within,
    /// Minor deviations within tolerance (e.g., rank order differs but all IDs present).
    Drift,
    /// Critical expectations failed (e.g., critical memory omitted, forbidden leak).
    Regression,
    /// Could not evaluate due to missing data or infrastructure.
    Inconclusive,
}

impl PackQualityVerdict {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Within => "within",
            Self::Drift => "drift",
            Self::Regression => "regression",
            Self::Inconclusive => "inconclusive",
        }
    }

    #[must_use]
    pub const fn is_passing(self) -> bool {
        matches!(self, Self::Within | Self::Drift)
    }
}

/// Result of comparing one pack-quality case.
#[derive(Clone, Debug, Serialize)]
pub struct PackQualityComparison {
    pub case_id: String,
    pub scenario_id: String,
    pub verdict: PackQualityVerdict,
    pub expected_selected_ids: Vec<String>,
    pub actual_selected_ids: Vec<String>,
    pub missing_expected_ids: Vec<String>,
    pub unexpected_ids: Vec<String>,
    pub critical_omitted_ids: Vec<String>,
    pub omitted_critical_found: Vec<String>,
    pub provenance_density: f64,
    pub min_provenance_density: f64,
    pub provenance_density_passed: bool,
    pub expected_degradation_codes: Vec<String>,
    pub actual_degradation_codes: Vec<String>,
    pub unexpected_degradation_codes: Vec<String>,
    pub forbidden_redaction_leaks: Vec<String>,
    pub actual_redaction_leaks: Vec<String>,
    pub token_budget_max: u32,
    pub actual_tokens_used: u32,
    pub token_budget_passed: bool,
    pub failure_reasons: Vec<String>,
}

/// Evidence-backed usefulness metrics derived from observed pack outcomes.
#[derive(Clone, Debug, Serialize)]
pub struct PackQualityOutcomeFeedbackReport {
    pub schema: &'static str,
    pub evidence_backed_event_count: usize,
    pub hypothesis_event_count: usize,
    pub helped_outcome_rate_by_profile: Vec<PackQualityOutcomeRateBucket>,
    pub harmful_or_ignored_memory_rate: f64,
    pub risk_memory_surfaced_before_destructive_action: bool,
    pub verification_saved_by_pack_evidence: bool,
    pub token_waste_estimate: u32,
    pub counterfactual_candidates: Vec<PackQualityCounterfactualCandidate>,
    pub hypotheses: Vec<PackQualityOutcomeHypothesis>,
}

#[derive(Clone, Debug, Serialize)]
pub struct PackQualityOutcomeRateBucket {
    pub pack_profile: String,
    pub feature_set: Vec<String>,
    pub event_count: usize,
    pub helped_count: usize,
    pub helped_outcome_rate: f64,
}

#[derive(Clone, Debug, Serialize)]
pub struct PackQualityCounterfactualCandidate {
    pub memory_id: String,
    pub case_ids: Vec<String>,
    pub evidence_backed: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct PackQualityOutcomeHypothesis {
    pub event_id: String,
    pub case_id: String,
    pub signal: String,
    pub rationale: Option<String>,
}

/// Aggregate pack-quality report for all cases in a fixture.
#[derive(Clone, Debug, Serialize)]
pub struct PackQualityReport {
    pub schema: &'static str,
    pub fixture_id: String,
    pub aggregate_verdict: PackQualityVerdict,
    pub cases_total: usize,
    pub cases_within: usize,
    pub cases_drift: usize,
    pub cases_regression: usize,
    pub cases_inconclusive: usize,
    pub comparisons: Vec<PackQualityComparison>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub outcome_feedback: Option<PackQualityOutcomeFeedbackReport>,
}

impl PackQualityReport {
    #[must_use]
    pub fn new(fixture_id: String) -> Self {
        Self {
            schema: PACK_QUALITY_REPORT_SCHEMA_V1,
            fixture_id,
            aggregate_verdict: PackQualityVerdict::Within,
            cases_total: 0,
            cases_within: 0,
            cases_drift: 0,
            cases_regression: 0,
            cases_inconclusive: 0,
            comparisons: Vec::new(),
            outcome_feedback: None,
        }
    }

    pub fn add_comparison(&mut self, comparison: PackQualityComparison) {
        match comparison.verdict {
            PackQualityVerdict::Within => self.cases_within += 1,
            PackQualityVerdict::Drift => self.cases_drift += 1,
            PackQualityVerdict::Regression => self.cases_regression += 1,
            PackQualityVerdict::Inconclusive => self.cases_inconclusive += 1,
        }
        self.cases_total += 1;
        self.comparisons.push(comparison);
        self.recompute_aggregate();
    }

    fn recompute_aggregate(&mut self) {
        if self.cases_regression > 0 {
            self.aggregate_verdict = PackQualityVerdict::Regression;
        } else if self.cases_inconclusive > 0 {
            self.aggregate_verdict = PackQualityVerdict::Inconclusive;
        } else if self.cases_drift > 0 {
            self.aggregate_verdict = PackQualityVerdict::Drift;
        } else {
            self.aggregate_verdict = PackQualityVerdict::Within;
        }
    }
}

/// Input for comparing pack quality against expectations.
#[derive(Clone, Debug)]
pub struct PackQualityActual {
    pub selected_memory_ids: Vec<String>,
    pub degradation_codes: Vec<String>,
    pub redaction_leaks: Vec<String>,
    pub tokens_used: u32,
    pub provenance_density: f64,
}

#[derive(Default)]
struct PackQualityOutcomeRateAccumulator {
    event_count: usize,
    helped_count: usize,
}

#[derive(Default)]
struct PackQualityCounterfactualAccumulator {
    case_ids: BTreeSet<String>,
    evidence_backed: bool,
}

/// Compare actual pack output against a single pack-quality case expectation.
///
/// Returns a deterministic verdict: within, drift, or regression.
#[must_use]
pub fn compare_pack_quality(
    case: &PackQualityCase,
    actual: &PackQualityActual,
) -> PackQualityComparison {
    let expected_set: HashSet<_> = case.expected_selected_memory_ids.iter().collect();
    let actual_set: HashSet<_> = actual.selected_memory_ids.iter().collect();
    let critical_set: HashSet<_> = case.critical_omitted_memory_ids.iter().collect();

    let missing_expected: Vec<_> = case
        .expected_selected_memory_ids
        .iter()
        .filter(|id| !actual_set.contains(id))
        .cloned()
        .collect();

    let unexpected: Vec<_> = actual
        .selected_memory_ids
        .iter()
        .filter(|id| !expected_set.contains(id))
        .cloned()
        .collect();

    let omitted_critical_found: Vec<_> = actual
        .selected_memory_ids
        .iter()
        .filter(|id| critical_set.contains(id))
        .cloned()
        .collect();

    let provenance_density_passed = actual.provenance_density >= case.min_provenance_density;

    let allowed_degradation_set: HashSet<_> = case.allowed_degradation_codes.iter().collect();
    let unexpected_degradation: Vec<_> = actual
        .degradation_codes
        .iter()
        .filter(|code| !allowed_degradation_set.contains(code))
        .cloned()
        .collect();

    let forbidden_leak_set: HashSet<_> = case.forbidden_redaction_leaks.iter().collect();
    let actual_leaks: Vec<_> = actual
        .redaction_leaks
        .iter()
        .filter(|leak| forbidden_leak_set.contains(leak))
        .cloned()
        .collect();

    let token_budget_passed = actual.tokens_used <= case.token_budget.expected_used_tokens_max;

    let mut failure_reasons = Vec::new();

    if !omitted_critical_found.is_empty() {
        failure_reasons.push(format!(
            "Critical omitted memory found in pack: {:?}",
            omitted_critical_found
        ));
    }

    if !actual_leaks.is_empty() {
        failure_reasons.push(format!("Forbidden redaction leaks: {:?}", actual_leaks));
    }

    if !missing_expected.is_empty() {
        failure_reasons.push(format!("Missing expected memories: {:?}", missing_expected));
    }

    if !unexpected.is_empty() {
        failure_reasons.push(format!("Unexpected memories selected: {:?}", unexpected));
    }

    if !unexpected_degradation.is_empty() {
        failure_reasons.push(format!(
            "Unexpected degradation codes: {:?}",
            unexpected_degradation
        ));
    }

    if !provenance_density_passed {
        failure_reasons.push(format!(
            "Provenance density {:.2} below minimum {:.2}",
            actual.provenance_density, case.min_provenance_density
        ));
    }

    if !token_budget_passed {
        failure_reasons.push(format!(
            "Token usage {} exceeds budget {}",
            actual.tokens_used, case.token_budget.expected_used_tokens_max
        ));
    }

    let verdict = if !omitted_critical_found.is_empty() || !actual_leaks.is_empty() {
        PackQualityVerdict::Regression
    } else if !missing_expected.is_empty()
        || !unexpected.is_empty()
        || !unexpected_degradation.is_empty()
        || !provenance_density_passed
        || !token_budget_passed
    {
        PackQualityVerdict::Drift
    } else {
        PackQualityVerdict::Within
    };

    PackQualityComparison {
        case_id: case.case_id.clone(),
        scenario_id: case.scenario_id.clone(),
        verdict,
        expected_selected_ids: case.expected_selected_memory_ids.clone(),
        actual_selected_ids: actual.selected_memory_ids.clone(),
        missing_expected_ids: missing_expected,
        unexpected_ids: unexpected,
        critical_omitted_ids: case.critical_omitted_memory_ids.clone(),
        omitted_critical_found,
        provenance_density: actual.provenance_density,
        min_provenance_density: case.min_provenance_density,
        provenance_density_passed,
        expected_degradation_codes: case.allowed_degradation_codes.clone(),
        actual_degradation_codes: actual.degradation_codes.clone(),
        unexpected_degradation_codes: unexpected_degradation,
        forbidden_redaction_leaks: case.forbidden_redaction_leaks.clone(),
        actual_redaction_leaks: actual_leaks,
        token_budget_max: case.token_budget.expected_used_tokens_max,
        actual_tokens_used: actual.tokens_used,
        token_budget_passed,
        failure_reasons,
    }
}

/// Evaluate all pack-quality cases from a fixture against actual results.
#[must_use]
pub fn evaluate_pack_quality(
    fixture_id: &str,
    cases: &[PackQualityCase],
    actuals: &[PackQualityActual],
) -> PackQualityReport {
    let mut report = PackQualityReport::new(fixture_id.to_string());

    for (case, actual) in cases.iter().zip(actuals.iter()) {
        let comparison = compare_pack_quality(case, actual);
        report.add_comparison(comparison);
    }

    for case in cases.iter().skip(actuals.len()) {
        let inconclusive = PackQualityComparison {
            case_id: case.case_id.clone(),
            scenario_id: case.scenario_id.clone(),
            verdict: PackQualityVerdict::Inconclusive,
            expected_selected_ids: case.expected_selected_memory_ids.clone(),
            actual_selected_ids: Vec::new(),
            missing_expected_ids: case.expected_selected_memory_ids.clone(),
            unexpected_ids: Vec::new(),
            critical_omitted_ids: case.critical_omitted_memory_ids.clone(),
            omitted_critical_found: Vec::new(),
            provenance_density: 0.0,
            min_provenance_density: case.min_provenance_density,
            provenance_density_passed: false,
            expected_degradation_codes: case.allowed_degradation_codes.clone(),
            actual_degradation_codes: Vec::new(),
            unexpected_degradation_codes: Vec::new(),
            forbidden_redaction_leaks: case.forbidden_redaction_leaks.clone(),
            actual_redaction_leaks: Vec::new(),
            token_budget_max: case.token_budget.expected_used_tokens_max,
            actual_tokens_used: 0,
            token_budget_passed: false,
            failure_reasons: vec!["No actual result provided for this case".to_string()],
        };
        report.add_comparison(inconclusive);
    }

    report
}

/// Evaluate pack quality and attach outcome-backed usefulness metrics.
#[must_use]
pub fn evaluate_pack_quality_with_outcomes(
    fixture_id: &str,
    cases: &[PackQualityCase],
    actuals: &[PackQualityActual],
    outcome_events: &[PackQualityOutcomeEvent],
) -> PackQualityReport {
    let mut report = evaluate_pack_quality(fixture_id, cases, actuals);
    report.outcome_feedback = Some(summarize_pack_quality_outcomes(cases, outcome_events));
    report
}

/// Compare actual ask output against one ask-quality case.
#[must_use]
pub fn compare_ask_quality(
    case: &AskQualityCase,
    actual: &AskQualityActual,
) -> AskQualityComparison {
    let actual_cited_memory_ids = stable_actual_cited_memory_ids(actual);
    let citation_precision = ask_citation_precision(case, actual);
    let answer_exactness = ask_answer_exactness(case, &actual_cited_memory_ids, actual);
    let abstention_calibration = if case.expect_abstention == actual.abstained {
        1.0
    } else {
        0.0
    };
    let conflict_recall = ask_conflict_recall(case, actual);
    let actual_conflict = !actual.sides.is_empty();

    let mut failure_reasons = Vec::new();
    if case.expect_abstention != actual.abstained {
        failure_reasons.push(format!(
            "abstention mismatch: expected {}, got {}",
            case.expect_abstention, actual.abstained
        ));
    }
    if case.expect_conflict != actual_conflict {
        failure_reasons.push(format!(
            "conflict mismatch: expected {}, got {}",
            case.expect_conflict, actual_conflict
        ));
    }
    if citation_precision < 1.0 {
        failure_reasons.push(format!(
            "citation precision {:.3} below perfect citation grounding",
            citation_precision
        ));
    }
    if answer_exactness < 1.0 {
        failure_reasons.push(format!(
            "answer exactness {:.3} below expected cited memory match",
            answer_exactness
        ));
    }
    if conflict_recall < 1.0 {
        failure_reasons.push(format!(
            "conflict recall {:.3} below expected side coverage",
            conflict_recall
        ));
    }

    let verdict = if actual.case_id != case.case_id {
        failure_reasons.push(format!(
            "actual case_id `{}` did not match expected `{}`",
            actual.case_id, case.case_id
        ));
        PackQualityVerdict::Inconclusive
    } else if abstention_calibration < 1.0 || conflict_recall < 1.0 {
        PackQualityVerdict::Regression
    } else if citation_precision < 1.0 || answer_exactness < 1.0 {
        PackQualityVerdict::Drift
    } else {
        PackQualityVerdict::Within
    };

    AskQualityComparison {
        case_id: case.case_id.clone(),
        scenario_id: case.scenario_id.clone(),
        verdict,
        scores: AskQualityCaseScores {
            citation_precision,
            answer_exactness,
            abstention_calibration,
            conflict_recall,
        },
        expected_cited_memory_ids: case.expected_cited_memory_ids.clone(),
        actual_cited_memory_ids,
        expected_answer_terms: case.expected_answer_terms.clone(),
        actual_answer_text: actual.answer_text.clone(),
        expected_abstention: case.expect_abstention,
        actual_abstained: actual.abstained,
        expected_conflict: case.expect_conflict,
        actual_conflict,
        failure_reasons,
    }
}

/// Evaluate all ask-quality cases and apply fixture thresholds.
#[must_use]
pub fn evaluate_ask_quality(
    fixture_id: &str,
    expectations: &AskQualityExpectations,
    actuals: &[AskQualityActual],
) -> AskQualityReport {
    let mut report = AskQualityReport::new(fixture_id.to_string(), expectations.thresholds.clone());
    let actual_by_case: BTreeMap<&str, &AskQualityActual> = actuals
        .iter()
        .map(|actual| (actual.case_id.as_str(), actual))
        .collect();

    for case in &expectations.cases {
        if let Some(actual) = actual_by_case.get(case.case_id.as_str()) {
            report.add_comparison(compare_ask_quality(case, actual));
        } else {
            report.add_comparison(missing_ask_quality_actual(case));
        }
    }

    recompute_ask_quality_metrics(&mut report);
    apply_ask_quality_thresholds(&mut report);
    report
}

fn missing_ask_quality_actual(case: &AskQualityCase) -> AskQualityComparison {
    AskQualityComparison {
        case_id: case.case_id.clone(),
        scenario_id: case.scenario_id.clone(),
        verdict: PackQualityVerdict::Inconclusive,
        scores: AskQualityCaseScores {
            citation_precision: 0.0,
            answer_exactness: 0.0,
            abstention_calibration: 0.0,
            conflict_recall: 0.0,
        },
        expected_cited_memory_ids: case.expected_cited_memory_ids.clone(),
        actual_cited_memory_ids: Vec::new(),
        expected_answer_terms: case.expected_answer_terms.clone(),
        actual_answer_text: None,
        expected_abstention: case.expect_abstention,
        actual_abstained: false,
        expected_conflict: case.expect_conflict,
        actual_conflict: false,
        failure_reasons: vec!["No actual ask result provided for this case".to_string()],
    }
}

fn stable_actual_cited_memory_ids(actual: &AskQualityActual) -> Vec<String> {
    let mut ids = actual
        .citations
        .iter()
        .map(|citation| citation.memory_id.clone())
        .collect::<Vec<_>>();
    for side in &actual.sides {
        ids.extend(side.cited_memory_ids.iter().cloned());
    }
    ids.sort();
    ids.dedup();
    ids
}

fn ask_citation_precision(case: &AskQualityCase, actual: &AskQualityActual) -> f64 {
    if case.expect_abstention {
        return if actual.citations.is_empty() && actual.sides.is_empty() {
            1.0
        } else {
            0.0
        };
    }

    let citations = actual
        .citations
        .iter()
        .map(|citation| (citation.memory_id.as_str(), citation.text.as_str()))
        .collect::<Vec<_>>();
    if citations.is_empty() {
        return 0.0;
    }

    let matching = citations
        .iter()
        .filter(|(memory_id, text)| {
            case.expected_cited_memory_ids
                .iter()
                .any(|expected| expected.as_str() == *memory_id)
                && (case.expected_answer_terms.is_empty()
                    || case
                        .expected_answer_terms
                        .iter()
                        .any(|term| contains_case_insensitive(*text, term.as_str())))
        })
        .count();
    rate(matching, citations.len())
}

fn ask_answer_exactness(
    case: &AskQualityCase,
    actual_cited_memory_ids: &[String],
    actual: &AskQualityActual,
) -> f64 {
    if case.expect_abstention {
        return if actual.abstained { 1.0 } else { 0.0 };
    }

    let expected: BTreeSet<_> = case.expected_cited_memory_ids.iter().collect();
    let actual: BTreeSet<_> = actual_cited_memory_ids.iter().collect();
    if expected == actual { 1.0 } else { 0.0 }
}

fn ask_conflict_recall(case: &AskQualityCase, actual: &AskQualityActual) -> f64 {
    if !case.expect_conflict {
        return if actual.sides.is_empty() { 1.0 } else { 0.0 };
    }
    if case.expected_sides.is_empty() {
        return 0.0;
    }

    let matched = case
        .expected_sides
        .iter()
        .filter(|expected_side| {
            actual.sides.iter().any(|actual_side| {
                actual_side.label == expected_side.label
                    && expected_side.cited_memory_ids.iter().all(|id| {
                        actual_side
                            .cited_memory_ids
                            .iter()
                            .any(|actual| actual == id)
                    })
            })
        })
        .count();
    rate(matched, case.expected_sides.len())
}

fn contains_case_insensitive(haystack: &str, needle: &str) -> bool {
    haystack
        .to_ascii_lowercase()
        .contains(&needle.to_ascii_lowercase())
}

fn recompute_ask_quality_metrics(report: &mut AskQualityReport) {
    let total = report.comparisons.len();
    if total == 0 {
        report.metrics = AskQualityMetrics::default();
        return;
    }

    let citation_precision = report
        .comparisons
        .iter()
        .map(|comparison| comparison.scores.citation_precision)
        .sum::<f64>();
    let answer_exactness = report
        .comparisons
        .iter()
        .map(|comparison| comparison.scores.answer_exactness)
        .sum::<f64>();
    let abstention_calibration = report
        .comparisons
        .iter()
        .map(|comparison| comparison.scores.abstention_calibration)
        .sum::<f64>();
    let conflict_recall = report
        .comparisons
        .iter()
        .map(|comparison| comparison.scores.conflict_recall)
        .sum::<f64>();
    let denominator = total as f64;

    report.metrics = AskQualityMetrics {
        citation_precision: citation_precision / denominator,
        answer_exactness: answer_exactness / denominator,
        abstention_calibration: abstention_calibration / denominator,
        conflict_recall: conflict_recall / denominator,
    };
}

fn apply_ask_quality_thresholds(report: &mut AskQualityReport) {
    report.threshold_failures.clear();
    push_threshold_failure(
        &mut report.threshold_failures,
        "citation_precision",
        report.metrics.citation_precision,
        report.thresholds.citation_precision_min,
    );
    push_threshold_failure(
        &mut report.threshold_failures,
        "answer_exactness",
        report.metrics.answer_exactness,
        report.thresholds.answer_exactness_min,
    );
    push_threshold_failure(
        &mut report.threshold_failures,
        "abstention_calibration",
        report.metrics.abstention_calibration,
        report.thresholds.abstention_calibration_min,
    );
    push_threshold_failure(
        &mut report.threshold_failures,
        "conflict_recall",
        report.metrics.conflict_recall,
        report.thresholds.conflict_recall_min,
    );

    if report.threshold_failures.is_empty() {
        return;
    }

    report.aggregate_verdict = match report.thresholds.gate_mode {
        AskQualityGateMode::Advisory => {
            if matches!(report.aggregate_verdict, PackQualityVerdict::Within) {
                PackQualityVerdict::Drift
            } else {
                report.aggregate_verdict
            }
        }
        AskQualityGateMode::Blocking => PackQualityVerdict::Regression,
    };
}

fn push_threshold_failure(failures: &mut Vec<String>, metric: &str, actual: f64, minimum: f64) {
    if actual < minimum {
        failures.push(format!("{metric} {actual:.3} below threshold {minimum:.3}"));
    }
}

/// Summarize observed outcome events without inferring success from pack presence alone.
#[must_use]
pub fn summarize_pack_quality_outcomes(
    cases: &[PackQualityCase],
    outcome_events: &[PackQualityOutcomeEvent],
) -> PackQualityOutcomeFeedbackReport {
    let case_ids: BTreeSet<_> = cases.iter().map(|case| case.case_id.as_str()).collect();
    let mut buckets: BTreeMap<(String, Vec<String>), PackQualityOutcomeRateAccumulator> =
        BTreeMap::new();
    let mut counterfactuals: BTreeMap<String, PackQualityCounterfactualAccumulator> =
        BTreeMap::new();
    let mut hypotheses = Vec::new();
    let mut evidence_backed_event_count = 0_usize;
    let mut hypothesis_event_count = 0_usize;
    let mut memory_signal_count = 0_usize;
    let mut harmful_or_ignored_memory_count = 0_usize;
    let mut risk_memory_surfaced_before_destructive_action = false;
    let mut verification_saved_by_pack_evidence = false;
    let mut token_waste_estimate = 0_u32;

    for event in outcome_events
        .iter()
        .filter(|event| case_ids.contains(event.case_id.as_str()))
    {
        token_waste_estimate = token_waste_estimate.saturating_add(event.token_waste_estimate);
        for memory_id in &event.counterfactual_memory_ids {
            let entry = counterfactuals.entry(memory_id.clone()).or_default();
            entry.case_ids.insert(event.case_id.clone());
            entry.evidence_backed |= event.evidence_backed;
        }

        if !event.evidence_backed {
            hypothesis_event_count += 1;
            hypotheses.push(PackQualityOutcomeHypothesis {
                event_id: event.event_id.clone(),
                case_id: event.case_id.clone(),
                signal: event.signal.clone(),
                rationale: event.rationale.clone(),
            });
            continue;
        }

        evidence_backed_event_count += 1;
        let signal = event.signal.as_str();
        let key = (
            normalize_pack_profile(&event.pack_profile),
            stable_feature_set(&event.feature_set),
        );
        let bucket = buckets.entry(key).or_default();
        bucket.event_count += 1;

        if is_pack_quality_helpful_signal(signal) {
            bucket.helped_count += 1;
        }
        if signal == "risk_memory_surfaced_before_destructive_action" {
            risk_memory_surfaced_before_destructive_action = true;
        }
        if signal == "verification_saved_by_pack_evidence" {
            verification_saved_by_pack_evidence = true;
        }
        if !event.memory_ids.is_empty()
            || !event.unused_memory_ids.is_empty()
            || !event.duplicated_memory_ids.is_empty()
        {
            memory_signal_count += 1;
            if matches!(signal, "harmful" | "ignored") {
                harmful_or_ignored_memory_count += 1;
            }
        }
    }

    hypotheses.sort_by(|left, right| {
        (
            left.case_id.as_str(),
            left.event_id.as_str(),
            left.signal.as_str(),
        )
            .cmp(&(
                right.case_id.as_str(),
                right.event_id.as_str(),
                right.signal.as_str(),
            ))
    });

    PackQualityOutcomeFeedbackReport {
        schema: PACK_QUALITY_OUTCOME_FEEDBACK_SCHEMA_V1,
        evidence_backed_event_count,
        hypothesis_event_count,
        helped_outcome_rate_by_profile: buckets
            .into_iter()
            .map(
                |((pack_profile, feature_set), accumulator)| PackQualityOutcomeRateBucket {
                    pack_profile,
                    feature_set,
                    event_count: accumulator.event_count,
                    helped_count: accumulator.helped_count,
                    helped_outcome_rate: rate(accumulator.helped_count, accumulator.event_count),
                },
            )
            .collect(),
        harmful_or_ignored_memory_rate: rate(harmful_or_ignored_memory_count, memory_signal_count),
        risk_memory_surfaced_before_destructive_action,
        verification_saved_by_pack_evidence,
        token_waste_estimate,
        counterfactual_candidates: counterfactuals
            .into_iter()
            .map(
                |(memory_id, accumulator)| PackQualityCounterfactualCandidate {
                    memory_id,
                    case_ids: accumulator.case_ids.into_iter().collect(),
                    evidence_backed: accumulator.evidence_backed,
                },
            )
            .collect(),
        hypotheses,
    }
}

fn normalize_pack_profile(pack_profile: &str) -> String {
    let trimmed = pack_profile.trim();
    if trimmed.is_empty() {
        "default".to_string()
    } else {
        trimmed.to_string()
    }
}

fn stable_feature_set(feature_set: &[String]) -> Vec<String> {
    let mut features = feature_set
        .iter()
        .map(|feature| feature.trim())
        .filter(|feature| !feature.is_empty())
        .map(str::to_string)
        .collect::<Vec<_>>();
    features.sort();
    features.dedup();
    features
}

fn is_pack_quality_helpful_signal(signal: &str) -> bool {
    matches!(
        signal,
        "helpful"
            | "risk_memory_surfaced_before_destructive_action"
            | "verification_saved_by_pack_evidence"
    )
}

fn rate(numerator: usize, denominator: usize) -> f64 {
    if denominator == 0 {
        0.0
    } else {
        numerator as f64 / denominator as f64
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

    fn ensure_close(actual: f64, expected: f64, epsilon: f64, ctx: &str) -> TestResult {
        if (actual - expected).abs() <= epsilon {
            Ok(())
        } else {
            Err(format!("{ctx}: expected {expected:?}, got {actual:?}"))
        }
    }

    fn write_minimal_fixture(dir: &Path, fixture_id: &str) -> TestResult {
        std::fs::create_dir_all(dir).map_err(|error| error.to_string())?;
        std::fs::write(
            dir.join("scenario.json"),
            format!(
                r#"{{
  "schema": "{EVAL_FIXTURE_SCHEMA_V1}",
  "fixture_id": "{fixture_id}",
  "fixture_family": "symlink-hardening",
  "journey": "fixture discovery",
  "agent_success_signal": "listed"
}}"#
            ),
        )
        .map_err(|error| error.to_string())?;
        std::fs::write(
            dir.join("source_memory.json"),
            format!(r#"{{"schema":"{EVAL_SOURCE_MEMORY_SCHEMA_V1}","memories":[]}}"#),
        )
        .map_err(|error| error.to_string())
    }

    #[cfg(unix)]
    #[test]
    fn discover_fixtures_rejects_symlinked_fixture_root() -> TestResult {
        use std::os::unix::fs::symlink;

        let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
        let real_root = tempdir.path().join("real-fixtures");
        let linked_root = tempdir.path().join("linked-fixtures");
        std::fs::create_dir_all(&real_root).map_err(|error| error.to_string())?;
        symlink(&real_root, &linked_root).map_err(|error| error.to_string())?;

        let error = discover_fixtures(&linked_root)
            .map(|fixtures| format!("unexpected fixtures: {fixtures:?}"))
            .expect_err("symlinked fixture root should reject");

        ensure(
            error.to_string().contains("symlinked path component"),
            true,
            "symlinked fixture root error",
        )
    }

    #[cfg(unix)]
    #[test]
    fn discover_fixtures_rejects_symlinked_fixture_entry() -> TestResult {
        use std::os::unix::fs::symlink;

        let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
        let fixture_root = tempdir.path().join("fixtures");
        let outside_fixture = tempdir.path().join("outside-fixture");
        let linked_fixture = fixture_root.join("linked-fixture");
        std::fs::create_dir_all(&fixture_root).map_err(|error| error.to_string())?;
        write_minimal_fixture(&outside_fixture, "outside")?;
        symlink(&outside_fixture, &linked_fixture).map_err(|error| error.to_string())?;

        let error = discover_fixtures(&fixture_root)
            .map(|fixtures| format!("unexpected fixtures: {fixtures:?}"))
            .expect_err("symlinked fixture entry should reject");

        ensure(
            error.to_string().contains("symlinked path component"),
            true,
            "symlinked fixture entry error",
        )
    }

    #[cfg(unix)]
    #[test]
    fn load_scenario_rejects_symlinked_scenario_file() -> TestResult {
        use std::os::unix::fs::symlink;

        let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
        let real_scenario = tempdir.path().join("outside-scenario.json");
        let linked_scenario = tempdir.path().join("scenario.json");
        std::fs::write(&real_scenario, b"{not-json").map_err(|error| error.to_string())?;
        symlink(&real_scenario, &linked_scenario).map_err(|error| error.to_string())?;

        let error = load_scenario(&linked_scenario)
            .map(|scenario| format!("unexpected scenario: {scenario:?}"))
            .expect_err("symlinked scenario file should reject before parse");

        ensure(
            error.to_string().contains("symlinked path component"),
            true,
            "symlinked scenario file error",
        )
    }

    #[test]
    fn load_scenario_rejects_non_regular_scenario_file() -> TestResult {
        let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
        let scenario_dir = tempdir.path().join("scenario.json");
        std::fs::create_dir(&scenario_dir).map_err(|error| error.to_string())?;

        let error = load_scenario(&scenario_dir)
            .map(|scenario| format!("unexpected scenario: {scenario:?}"))
            .expect_err("scenario directory should reject before read");

        ensure(
            error.to_string().contains("not a regular file"),
            true,
            "non-regular scenario file error",
        )
    }

    #[test]
    fn load_scenario_rejects_oversized_scenario_before_parse() -> TestResult {
        let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
        let scenario_path = tempdir.path().join("scenario.json");
        std::fs::write(
            &scenario_path,
            vec![b'{'; EVAL_FIXTURE_FILE_MAX_BYTES as usize + 1],
        )
        .map_err(|error| error.to_string())?;

        let error = load_scenario(&scenario_path)
            .map(|scenario| format!("unexpected scenario: {scenario:?}"))
            .expect_err("oversized scenario file should reject before parse");

        ensure(
            error.to_string().contains("byte cap"),
            true,
            "oversized scenario file error",
        )
    }

    #[cfg(unix)]
    #[test]
    fn source_memory_counts_rejects_symlinked_source_file() -> TestResult {
        use std::os::unix::fs::symlink;

        let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
        let real_source = tempdir.path().join("outside-source-memory.json");
        let linked_source = tempdir.path().join("source_memory.json");
        std::fs::write(&real_source, b"{not-json").map_err(|error| error.to_string())?;
        symlink(&real_source, &linked_source).map_err(|error| error.to_string())?;

        let error = source_memory_counts(&linked_source)
            .map(|counts| format!("unexpected counts: {counts:?}"))
            .expect_err("symlinked source memory file should reject before parse");

        ensure(
            error.to_string().contains("symlinked path component"),
            true,
            "symlinked source memory file error",
        )
    }

    #[test]
    fn source_memory_counts_rejects_non_regular_source_file() -> TestResult {
        let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
        let source_dir = tempdir.path().join("source_memory.json");
        std::fs::create_dir(&source_dir).map_err(|error| error.to_string())?;

        let error = source_memory_counts(&source_dir)
            .map(|counts| format!("unexpected counts: {counts:?}"))
            .expect_err("source memory directory should reject before read");

        ensure(
            error.to_string().contains("not a regular file"),
            true,
            "non-regular source memory file error",
        )
    }

    #[test]
    fn source_memory_counts_rejects_oversized_source_before_parse() -> TestResult {
        let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
        let source_path = tempdir.path().join("source_memory.json");
        std::fs::write(
            &source_path,
            vec![b'{'; EVAL_FIXTURE_FILE_MAX_BYTES as usize + 1],
        )
        .map_err(|error| error.to_string())?;

        let error = source_memory_counts(&source_path)
            .map(|counts| format!("unexpected counts: {counts:?}"))
            .expect_err("oversized source memory file should reject before parse");

        ensure(
            error.to_string().contains("byte cap"),
            true,
            "oversized source memory file error",
        )
    }

    #[test]
    fn precision_at_k_empty_retrieved() -> TestResult {
        let retrieved: Vec<String> = vec![];
        let relevant: HashSet<String> = ["a".into()].into_iter().collect();
        ensure(
            precision_at_k(&retrieved, &relevant, 5),
            0.0,
            "empty retrieved",
        )
    }

    #[test]
    fn precision_at_k_all_relevant() -> TestResult {
        let retrieved: Vec<String> = vec!["a".into(), "b".into(), "c".into()];
        let relevant: HashSet<String> = ["a".into(), "b".into(), "c".into()].into_iter().collect();
        ensure_close(
            precision_at_k(&retrieved, &relevant, 3),
            1.0,
            1e-9,
            "all relevant",
        )
    }

    #[test]
    fn precision_at_k_partial() -> TestResult {
        let retrieved: Vec<String> = vec!["a".into(), "x".into(), "b".into()];
        let relevant: HashSet<String> = ["a".into(), "b".into()].into_iter().collect();
        ensure_close(
            precision_at_k(&retrieved, &relevant, 3),
            2.0 / 3.0,
            1e-9,
            "partial",
        )
    }

    #[test]
    fn precision_at_k_does_not_count_duplicate_relevant_ids() -> TestResult {
        let retrieved: Vec<String> = vec!["a".into(), "a".into()];
        let relevant: HashSet<String> = ["a".into()].into_iter().collect();
        ensure_close(
            precision_at_k(&retrieved, &relevant, 2),
            0.5,
            1e-9,
            "duplicate relevant id should count once",
        )
    }

    #[test]
    fn recall_at_k_all_retrieved() -> TestResult {
        let retrieved: Vec<String> = vec!["a".into(), "b".into()];
        let relevant: HashSet<String> = ["a".into(), "b".into()].into_iter().collect();
        ensure_close(
            recall_at_k(&retrieved, &relevant, 5),
            1.0,
            1e-9,
            "all retrieved",
        )
    }

    #[test]
    fn recall_at_k_partial() -> TestResult {
        let retrieved: Vec<String> = vec!["a".into(), "x".into()];
        let relevant: HashSet<String> = ["a".into(), "b".into()].into_iter().collect();
        ensure_close(
            recall_at_k(&retrieved, &relevant, 5),
            0.5,
            1e-9,
            "partial recall",
        )
    }

    #[test]
    fn mrr_first_position() -> TestResult {
        let retrieved: Vec<String> = vec!["a".into(), "b".into()];
        let relevant: HashSet<String> = ["a".into()].into_iter().collect();
        ensure_close(mrr(&retrieved, &relevant), 1.0, 1e-9, "first position")
    }

    #[test]
    fn mrr_second_position() -> TestResult {
        let retrieved: Vec<String> = vec!["x".into(), "a".into()];
        let relevant: HashSet<String> = ["a".into()].into_iter().collect();
        ensure_close(mrr(&retrieved, &relevant), 0.5, 1e-9, "second position")
    }

    #[test]
    fn mrr_not_found() -> TestResult {
        let retrieved: Vec<String> = vec!["x".into(), "y".into()];
        let relevant: HashSet<String> = ["a".into()].into_iter().collect();
        ensure_close(mrr(&retrieved, &relevant), 0.0, 1e-9, "not found")
    }

    #[test]
    fn ndcg_at_k_perfect() -> TestResult {
        let retrieved: Vec<String> = vec!["a".into(), "b".into()];
        let relevant: HashSet<String> = ["a".into(), "b".into()].into_iter().collect();
        ensure_close(
            ndcg_at_k(&retrieved, &relevant, 2),
            1.0,
            1e-9,
            "perfect ndcg",
        )
    }

    #[test]
    fn ndcg_at_k_duplicate_relevant_ids_stays_bounded() -> TestResult {
        let retrieved: Vec<String> = vec!["a".into(), "a".into()];
        let relevant: HashSet<String> = ["a".into()].into_iter().collect();
        ensure_close(
            ndcg_at_k(&retrieved, &relevant, 2),
            1.0,
            1e-9,
            "duplicate relevant id should not push nDCG above 1",
        )
    }

    #[test]
    fn first_relevant_rank_finds_first() -> TestResult {
        let retrieved: Vec<String> = vec!["x".into(), "a".into(), "b".into()];
        let relevant: HashSet<String> = ["a".into(), "b".into()].into_iter().collect();
        ensure(
            first_relevant_rank(&retrieved, &relevant),
            Some(2),
            "rank 2",
        )
    }

    #[test]
    fn compute_query_metrics_integration() -> TestResult {
        let expected = vec!["mem_001".into(), "mem_002".into()];
        let retrieved = vec!["mem_001".into(), "mem_003".into(), "mem_002".into()];
        let metrics = compute_query_metrics("test query", &expected, &retrieved);

        ensure_close(metrics.precision_at_1, 1.0, 1e-9, "p@1")?;
        ensure_close(metrics.precision_at_3, 2.0 / 3.0, 1e-9, "p@3")?;
        ensure_close(metrics.mrr, 1.0, 1e-9, "mrr")?;
        ensure(metrics.first_relevant_rank, Some(1), "first rank")
    }

    #[test]
    fn compute_fixture_metrics_averages() -> TestResult {
        let q1 = QueryMetrics {
            query: "q1".into(),
            precision_at_1: 1.0,
            precision_at_3: 0.5,
            precision_at_5: 0.4,
            recall_at_5: 1.0,
            ndcg_at_5: 0.8,
            mrr: 1.0,
            ..Default::default()
        };
        let q2 = QueryMetrics {
            query: "q2".into(),
            precision_at_1: 0.0,
            precision_at_3: 0.5,
            precision_at_5: 0.6,
            recall_at_5: 0.5,
            ndcg_at_5: 0.6,
            mrr: 0.5,
            ..Default::default()
        };

        let metrics = compute_fixture_metrics("test", vec![q1, q2]);

        ensure(metrics.queries_evaluated, 2, "query count")?;
        ensure_close(metrics.mean_precision_at_1, 0.5, 1e-9, "mean p@1")?;
        ensure_close(metrics.mean_mrr, 0.75, 1e-9, "mean mrr")
    }

    #[test]
    fn semantic_recall_expectations_measure_neural_gain() -> TestResult {
        let expectations = SemanticRecallExpectations {
            schema: SEMANTIC_RECALL_EXPECTATIONS_SCHEMA_V1.to_owned(),
            deterministic_seed: "seed.bundled_embeddings.semantic_recall.v1".to_owned(),
            recall_at_k: 3,
            hash_baseline_recall_at_k_max: 0.0,
            semantic_recall_at_k_min: 1.0,
            minimum_recall_gain: 1.0,
            cases: vec![SemanticRecallCase {
                case_id: "analyst_paraphrase".to_owned(),
                query: "video game virtual currency platform owner cash generation".to_owned(),
                expected_memory_ids: vec!["mem_rblx".to_owned()],
                hash_retrieved_ids: vec!["mem_snow".to_owned(), "mem_nke".to_owned()],
                semantic_retrieved_ids: vec!["mem_rblx".to_owned(), "mem_snow".to_owned()],
            }],
        };

        let report =
            evaluate_semantic_recall_expectations("fx.bundled_embeddings.v1", &expectations);

        ensure(report.schema, SEMANTIC_RECALL_REPORT_SCHEMA_V1, "schema")?;
        ensure(report.passed, true, "report should pass")?;
        ensure_close(
            report.hash_baseline_recall_at_k,
            0.0,
            1e-9,
            "hash baseline recall",
        )?;
        ensure_close(report.semantic_recall_at_k, 1.0, 1e-9, "semantic recall")?;
        ensure_close(report.recall_gain, 1.0, 1e-9, "recall gain")?;
        ensure(
            report.cases[0].first_semantic_rank,
            Some(1),
            "semantic first rank",
        )
    }

    #[test]
    fn semantic_recall_expectations_fail_when_hash_is_not_beaten() -> TestResult {
        let expectations = SemanticRecallExpectations {
            schema: SEMANTIC_RECALL_EXPECTATIONS_SCHEMA_V1.to_owned(),
            deterministic_seed: "seed.bundled_embeddings.semantic_recall.v1".to_owned(),
            recall_at_k: 3,
            hash_baseline_recall_at_k_max: 0.0,
            semantic_recall_at_k_min: 1.0,
            minimum_recall_gain: 1.0,
            cases: vec![SemanticRecallCase {
                case_id: "analyst_paraphrase".to_owned(),
                query: "video game virtual currency platform owner cash generation".to_owned(),
                expected_memory_ids: vec!["mem_rblx".to_owned()],
                hash_retrieved_ids: vec!["mem_rblx".to_owned()],
                semantic_retrieved_ids: vec!["mem_rblx".to_owned()],
            }],
        };

        let report =
            evaluate_semantic_recall_expectations("fx.bundled_embeddings.v1", &expectations);

        ensure(report.passed, false, "report should fail without gain")?;
        ensure_close(report.recall_gain, 0.0, 1e-9, "recall gain")
    }

    #[test]
    fn eval_run_status_strings_stable() -> TestResult {
        ensure(EvalRunStatus::Pending.as_str(), "pending", "pending")?;
        ensure(EvalRunStatus::Running.as_str(), "running", "running")?;
        ensure(EvalRunStatus::Passed.as_str(), "passed", "passed")?;
        ensure(EvalRunStatus::Failed.as_str(), "failed", "failed")?;
        ensure(EvalRunStatus::Error.as_str(), "error", "error")
    }

    #[test]
    fn schema_versions_stable() -> TestResult {
        ensure(
            EVAL_FIXTURE_SCHEMA_V1,
            "ee.eval_fixture.v1",
            "fixture schema",
        )?;
        ensure(
            EVAL_SOURCE_MEMORY_SCHEMA_V1,
            "ee.eval_source_memory.v1",
            "source schema",
        )?;
        ensure(EVAL_REPORT_SCHEMA_V1, "ee.eval.report.v1", "report schema")?;
        ensure(
            SEMANTIC_RECALL_EXPECTATIONS_SCHEMA_V1,
            "ee.eval.semantic_recall_expectations.v1",
            "semantic recall expectations schema",
        )?;
        ensure(
            SEMANTIC_RECALL_REPORT_SCHEMA_V1,
            "ee.eval.semantic_recall_report.v1",
            "semantic recall report schema",
        )
    }

    // ========================================================================
    // Pack Quality Comparison Tests (ADR-0026)
    // ========================================================================

    fn make_pack_quality_case(
        case_id: &str,
        expected_ids: Vec<&str>,
        critical_omitted: Vec<&str>,
    ) -> PackQualityCase {
        PackQualityCase {
            case_id: case_id.into(),
            scenario_id: "test_scenario".into(),
            command_step: 1,
            query_surface: PackQualityQuerySurface {
                kind: "inline".into(),
                query: Some("test query".into()),
                path: None,
                schema: None,
            },
            expected_selected_memory_ids: expected_ids.into_iter().map(String::from).collect(),
            critical_omitted_memory_ids: critical_omitted.into_iter().map(String::from).collect(),
            min_provenance_density: 0.5,
            allowed_degradation_codes: vec![],
            forbidden_redaction_leaks: vec!["secret".into(), "pii".into()],
            token_budget: PackQualityTokenBudget {
                max_tokens: 4000,
                expected_used_tokens_max: 3500,
                expect_truncation: false,
            },
            stable_first_failure_label: "test_failure".into(),
        }
    }

    fn make_pack_quality_actual(
        selected_ids: Vec<&str>,
        tokens: u32,
        provenance_density: f64,
    ) -> PackQualityActual {
        PackQualityActual {
            selected_memory_ids: selected_ids.into_iter().map(String::from).collect(),
            degradation_codes: vec![],
            redaction_leaks: vec![],
            tokens_used: tokens,
            provenance_density,
        }
    }

    fn make_pack_quality_outcome_event(
        event_id: &str,
        signal: &str,
        evidence_backed: bool,
    ) -> PackQualityOutcomeEvent {
        PackQualityOutcomeEvent {
            event_id: event_id.into(),
            case_id: "case1".into(),
            pack_profile: "balanced".into(),
            feature_set: vec!["ppr".into(), "pack_dna".into(), "ppr".into()],
            signal: signal.into(),
            memory_ids: vec!["mem_001".into()],
            unused_memory_ids: Vec::new(),
            duplicated_memory_ids: Vec::new(),
            counterfactual_memory_ids: Vec::new(),
            token_waste_estimate: 0,
            evidence_backed,
            rationale: Some("fixture outcome event".into()),
        }
    }

    #[test]
    fn pack_quality_verdict_strings_stable() -> TestResult {
        ensure(PackQualityVerdict::Within.as_str(), "within", "within")?;
        ensure(PackQualityVerdict::Drift.as_str(), "drift", "drift")?;
        ensure(
            PackQualityVerdict::Regression.as_str(),
            "regression",
            "regression",
        )?;
        ensure(
            PackQualityVerdict::Inconclusive.as_str(),
            "inconclusive",
            "inconclusive",
        )
    }

    #[test]
    fn pack_quality_verdict_is_passing() -> TestResult {
        ensure(PackQualityVerdict::Within.is_passing(), true, "within")?;
        ensure(PackQualityVerdict::Drift.is_passing(), true, "drift")?;
        ensure(
            PackQualityVerdict::Regression.is_passing(),
            false,
            "regression",
        )?;
        ensure(
            PackQualityVerdict::Inconclusive.is_passing(),
            false,
            "inconclusive",
        )
    }

    #[test]
    fn pack_quality_compare_perfect_match() -> TestResult {
        let case = make_pack_quality_case("perfect", vec!["mem_001", "mem_002"], vec![]);
        let actual = make_pack_quality_actual(vec!["mem_001", "mem_002"], 2000, 0.8);

        let result = compare_pack_quality(&case, &actual);

        ensure(result.verdict, PackQualityVerdict::Within, "verdict")?;
        ensure(result.missing_expected_ids.is_empty(), true, "no missing")?;
        ensure(result.unexpected_ids.is_empty(), true, "no unexpected")?;
        ensure(result.provenance_density_passed, true, "provenance ok")?;
        ensure(result.token_budget_passed, true, "tokens ok")?;
        ensure(result.failure_reasons.is_empty(), true, "no failures")
    }

    #[test]
    fn pack_quality_compare_missing_expected_id() -> TestResult {
        let case = make_pack_quality_case("missing", vec!["mem_001", "mem_002", "mem_003"], vec![]);
        let actual = make_pack_quality_actual(vec!["mem_001", "mem_002"], 2000, 0.8);

        let result = compare_pack_quality(&case, &actual);

        ensure(result.verdict, PackQualityVerdict::Drift, "verdict")?;
        ensure(result.missing_expected_ids.len(), 1, "one missing")?;
        ensure(
            result.missing_expected_ids[0].as_str(),
            "mem_003",
            "missing id",
        )
    }

    #[test]
    fn pack_quality_compare_unexpected_id() -> TestResult {
        let case = make_pack_quality_case("unexpected", vec!["mem_001"], vec![]);
        let actual = make_pack_quality_actual(vec!["mem_001", "mem_extra"], 2000, 0.8);

        let result = compare_pack_quality(&case, &actual);

        ensure(result.verdict, PackQualityVerdict::Drift, "verdict")?;
        ensure(result.unexpected_ids.len(), 1, "one unexpected")?;
        ensure(
            result.unexpected_ids[0].as_str(),
            "mem_extra",
            "unexpected id",
        )?;
        ensure(
            result
                .failure_reasons
                .iter()
                .any(|reason| reason.contains("Unexpected memories selected")),
            true,
            "unexpected id should explain drift",
        )
    }

    #[test]
    fn pack_quality_compare_critical_omitted_found() -> TestResult {
        let case = make_pack_quality_case(
            "critical_omitted",
            vec!["mem_001"],
            vec!["mem_secret"], // This should NOT be in pack
        );
        // But it IS in the pack - regression
        let actual = make_pack_quality_actual(vec!["mem_001", "mem_secret"], 2000, 0.8);

        let result = compare_pack_quality(&case, &actual);

        ensure(result.verdict, PackQualityVerdict::Regression, "verdict")?;
        ensure(result.omitted_critical_found.len(), 1, "one critical found")?;
        ensure(
            result.omitted_critical_found[0].as_str(),
            "mem_secret",
            "critical id",
        )
    }

    #[test]
    fn pack_quality_compare_provenance_density_below_min() -> TestResult {
        let case = make_pack_quality_case("low_provenance", vec!["mem_001"], vec![]);
        let actual = make_pack_quality_actual(vec!["mem_001"], 2000, 0.3); // Below 0.5 min

        let result = compare_pack_quality(&case, &actual);

        ensure(result.verdict, PackQualityVerdict::Drift, "verdict")?;
        ensure(result.provenance_density_passed, false, "provenance failed")
    }

    #[test]
    fn pack_quality_compare_token_budget_exceeded() -> TestResult {
        let case = make_pack_quality_case("over_budget", vec!["mem_001"], vec![]);
        let actual = make_pack_quality_actual(vec!["mem_001"], 4000, 0.8); // Exceeds 3500 max

        let result = compare_pack_quality(&case, &actual);

        ensure(result.verdict, PackQualityVerdict::Drift, "verdict")?;
        ensure(result.token_budget_passed, false, "budget failed")
    }

    #[test]
    fn pack_quality_compare_redaction_leak() -> TestResult {
        let case = make_pack_quality_case("leak", vec!["mem_001"], vec![]);
        let mut actual = make_pack_quality_actual(vec!["mem_001"], 2000, 0.8);
        actual.redaction_leaks = vec!["secret".into()]; // Forbidden leak

        let result = compare_pack_quality(&case, &actual);

        ensure(result.verdict, PackQualityVerdict::Regression, "verdict")?;
        ensure(result.actual_redaction_leaks.len(), 1, "one leak")?;
        ensure(
            result.actual_redaction_leaks[0].as_str(),
            "secret",
            "leak class",
        )
    }

    #[test]
    fn pack_quality_compare_unexpected_degradation() -> TestResult {
        let case = make_pack_quality_case("degraded", vec!["mem_001"], vec![]);
        let mut actual = make_pack_quality_actual(vec!["mem_001"], 2000, 0.8);
        actual.degradation_codes = vec!["semantic_unavailable".into()]; // Not allowed

        let result = compare_pack_quality(&case, &actual);

        ensure(result.verdict, PackQualityVerdict::Drift, "verdict")?;
        ensure(
            result.unexpected_degradation_codes.len(),
            1,
            "one unexpected",
        )?;
        ensure(
            result
                .failure_reasons
                .iter()
                .any(|reason| reason.contains("Unexpected degradation codes")),
            true,
            "unexpected degradation should explain drift",
        )
    }

    #[test]
    fn pack_quality_report_aggregates_correctly() -> TestResult {
        let case1 = make_pack_quality_case("case1", vec!["mem_001"], vec![]);
        let case2 = make_pack_quality_case("case2", vec!["mem_002"], vec![]);
        let case3 = make_pack_quality_case("case3", vec!["mem_003"], vec!["mem_secret"]);

        let actual1 = make_pack_quality_actual(vec!["mem_001"], 2000, 0.8); // Within
        let actual2 = make_pack_quality_actual(vec!["mem_002", "extra"], 2000, 0.8); // Drift
        let actual3 = make_pack_quality_actual(vec!["mem_003", "mem_secret"], 2000, 0.8); // Regression

        let report = evaluate_pack_quality(
            "test_fixture",
            &[case1, case2, case3],
            &[actual1, actual2, actual3],
        );

        ensure(report.cases_total, 3, "total cases")?;
        ensure(report.cases_within, 1, "within count")?;
        ensure(report.cases_drift, 1, "drift count")?;
        ensure(report.cases_regression, 1, "regression count")?;
        ensure(
            report.aggregate_verdict,
            PackQualityVerdict::Regression,
            "aggregate",
        )
    }

    #[test]
    fn pack_quality_report_missing_actuals_inconclusive() -> TestResult {
        let case1 = make_pack_quality_case("case1", vec!["mem_001"], vec![]);
        let case2 = make_pack_quality_case("case2", vec!["mem_002"], vec![]);

        let actual1 = make_pack_quality_actual(vec!["mem_001"], 2000, 0.8);
        // No actual2 provided

        let report = evaluate_pack_quality("test_fixture", &[case1, case2], &[actual1]);

        ensure(report.cases_total, 2, "total cases")?;
        ensure(report.cases_within, 1, "within count")?;
        ensure(report.cases_inconclusive, 1, "inconclusive count")?;
        ensure(
            report.aggregate_verdict,
            PackQualityVerdict::Inconclusive,
            "aggregate",
        )
    }

    #[test]
    fn pack_quality_report_all_within() -> TestResult {
        let case1 = make_pack_quality_case("case1", vec!["mem_001"], vec![]);
        let case2 = make_pack_quality_case("case2", vec!["mem_002"], vec![]);

        let actual1 = make_pack_quality_actual(vec!["mem_001"], 2000, 0.8);
        let actual2 = make_pack_quality_actual(vec!["mem_002"], 2000, 0.8);

        let report = evaluate_pack_quality("test_fixture", &[case1, case2], &[actual1, actual2]);

        ensure(report.cases_total, 2, "total cases")?;
        ensure(report.cases_within, 2, "all within")?;
        ensure(
            report.aggregate_verdict,
            PackQualityVerdict::Within,
            "aggregate",
        )
    }

    #[test]
    fn pack_quality_outcome_feedback_counts_only_evidence_backed_events() -> TestResult {
        let case = make_pack_quality_case("case1", vec!["mem_001"], vec![]);
        let mut helpful = make_pack_quality_outcome_event("out_001", "helpful", true);
        helpful.feature_set = vec!["pack_dna".into(), "ppr".into(), "pack_dna".into()];
        let risk = make_pack_quality_outcome_event(
            "out_002",
            "risk_memory_surfaced_before_destructive_action",
            true,
        );
        let mut ignored = make_pack_quality_outcome_event("out_003", "ignored", true);
        ignored.unused_memory_ids = vec!["mem_unused".into()];
        ignored.token_waste_estimate = 64;
        let mut hypothesis =
            make_pack_quality_outcome_event("out_004", "counterfactual_candidate", false);
        hypothesis.memory_ids.clear();
        hypothesis.counterfactual_memory_ids = vec!["mem_missing".into()];

        let feedback =
            summarize_pack_quality_outcomes(&[case], &[helpful, risk, ignored, hypothesis]);

        ensure(
            feedback.evidence_backed_event_count,
            3,
            "evidence-backed events",
        )?;
        ensure(feedback.hypothesis_event_count, 1, "hypotheses")?;
        ensure(
            feedback.risk_memory_surfaced_before_destructive_action,
            true,
            "risk memory surfaced",
        )?;
        ensure(feedback.token_waste_estimate, 64, "token waste")?;
        ensure(feedback.hypotheses.len(), 1, "hypothesis count")?;
        ensure(
            feedback.counterfactual_candidates.len(),
            1,
            "counterfactual candidates",
        )?;
        ensure_close(
            feedback.helped_outcome_rate_by_profile[0].helped_outcome_rate,
            2.0 / 3.0,
            0.000_001,
            "helped rate",
        )?;
        ensure_close(
            feedback.harmful_or_ignored_memory_rate,
            1.0 / 3.0,
            0.000_001,
            "harmful or ignored rate",
        )?;
        ensure(
            feedback.helped_outcome_rate_by_profile[0]
                .feature_set
                .clone(),
            vec!["pack_dna".to_string(), "ppr".to_string()],
            "stable feature set",
        )
    }

    #[test]
    fn pack_quality_report_attaches_outcome_feedback_when_requested() -> TestResult {
        let case = make_pack_quality_case("case1", vec!["mem_001"], vec![]);
        let actual = make_pack_quality_actual(vec!["mem_001"], 2000, 0.8);
        let event =
            make_pack_quality_outcome_event("out_001", "verification_saved_by_pack_evidence", true);

        let report =
            evaluate_pack_quality_with_outcomes("test_fixture", &[case], &[actual], &[event]);
        let feedback = report
            .outcome_feedback
            .ok_or("missing outcome feedback report")?;

        ensure(
            feedback.verification_saved_by_pack_evidence,
            true,
            "verification saved",
        )?;
        ensure(
            feedback.schema,
            PACK_QUALITY_OUTCOME_FEEDBACK_SCHEMA_V1,
            "feedback schema",
        )
    }

    #[test]
    fn pack_quality_schema_version_stable() -> TestResult {
        ensure(
            PACK_QUALITY_REPORT_SCHEMA_V1,
            "ee.eval.pack_quality_report.v1",
            "pack quality report schema",
        )
    }
}
