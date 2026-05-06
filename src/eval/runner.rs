//! Eval fixture runner for retrieval quality evaluation.
//!
//! Loads fixtures from `tests/fixtures/eval/`, seeds a deterministic workspace,
//! runs queries, and computes retrieval metrics (P@k, nDCG@k, MRR).

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::models::DomainError;

/// Schema version for eval fixture files.
pub const EVAL_FIXTURE_SCHEMA_V1: &str = "ee.eval_fixture.v1";

/// Schema version for eval source memory files.
pub const EVAL_SOURCE_MEMORY_SCHEMA_V1: &str = "ee.eval_source_memory.v1";

/// Schema version for eval run report.
pub const EVAL_REPORT_SCHEMA_V1: &str = "ee.eval.report.v1";

/// Default fixture directory relative to project root.
pub const DEFAULT_FIXTURE_DIR: &str = "tests/fixtures/eval";

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

/// Source memory definition (parsed from source_memory.json).
#[derive(Clone, Debug, Deserialize)]
pub struct SourceMemoryFile {
    pub schema: String,
    pub fixture_id: String,
    #[serde(default)]
    pub source_kind: String,
    #[serde(default)]
    pub fixed_clock: Option<String>,
    pub memories: Vec<SourceMemory>,
    #[serde(default)]
    pub secret_policy: SecretPolicy,
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
        if !path.is_dir() {
            continue;
        }

        let scenario_path = path.join("scenario.json");
        let source_memory_path = path.join("source_memory.json");

        if !scenario_path.exists() || !source_memory_path.exists() {
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
    let content = std::fs::read_to_string(path).map_err(|e| DomainError::Storage {
        message: format!("Failed to read scenario file {}: {e}", path.display()),
        repair: None,
    })?;

    serde_json::from_str(&content).map_err(|e| DomainError::Import {
        message: format!("Failed to parse scenario JSON: {e}"),
        repair: Some("Check scenario.json syntax".into()),
    })
}

/// Load source memories from JSON.
pub fn load_source_memories(path: &Path) -> Result<SourceMemoryFile, DomainError> {
    let content = std::fs::read_to_string(path).map_err(|e| DomainError::Storage {
        message: format!("Failed to read source memory file {}: {e}", path.display()),
        repair: None,
    })?;

    serde_json::from_str(&content).map_err(|e| DomainError::Import {
        message: format!("Failed to parse source memory JSON: {e}"),
        repair: Some("Check source_memory.json syntax".into()),
    })
}

/// List all available fixtures with metadata.
pub fn list_fixtures(fixture_dir: &Path) -> Result<Vec<FixtureListEntry>, DomainError> {
    let discovered = discover_fixtures(fixture_dir)?;
    let mut entries = Vec::with_capacity(discovered.len());

    for fixture in discovered {
        let scenario = load_scenario(&fixture.scenario_path)?;
        let source = load_source_memories(&fixture.source_memory_path)?;

        let query_count: usize = source
            .memories
            .iter()
            .flat_map(|m| &m.expected_query_match)
            .collect::<HashSet<_>>()
            .len();

        entries.push(FixtureListEntry {
            fixture_id: fixture.fixture_id,
            fixture_family: fixture.fixture_family,
            journey: scenario.journey,
            memory_count: source.memories.len(),
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
    let top_k: Vec<_> = retrieved.iter().take(k).collect();
    if top_k.is_empty() {
        return 0.0;
    }
    let hits = top_k.iter().filter(|id| relevant.contains(**id)).count();
    hits as f64 / top_k.len() as f64
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

    let dcg: f64 = retrieved
        .iter()
        .take(k)
        .enumerate()
        .filter(|(_, id)| relevant.contains(*id))
        .map(|(i, _)| 1.0 / (i as f64 + 2.0).log2())
        .sum();

    let ideal_count = relevant.len().min(k);
    let idcg: f64 = (0..ideal_count)
        .map(|i| 1.0 / (i as f64 + 2.0).log2())
        .sum();

    if idcg == 0.0 { 0.0 } else { dcg / idcg }
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
pub fn compute_fixture_metrics(fixture_id: &str, per_query: Vec<QueryMetrics>) -> FixtureMetrics {
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

/// Compute data hash for determinism verification.
pub fn compute_data_hash(report: &EvalRunReport) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    let mut hasher = DefaultHasher::new();
    report.fixture_id.hash(&mut hasher);
    report.fixture_family.hash(&mut hasher);
    report.metrics.queries_evaluated.hash(&mut hasher);

    for q in &report.metrics.per_query {
        q.query.hash(&mut hasher);
        for id in &q.expected_ids {
            id.hash(&mut hasher);
        }
        for id in &q.retrieved_ids {
            id.hash(&mut hasher);
        }
    }

    format!("{:016x}", hasher.finish())
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
        ensure(EVAL_REPORT_SCHEMA_V1, "ee.eval.report.v1", "report schema")
    }
}
