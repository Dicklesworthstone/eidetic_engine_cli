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

/// Schema version for pack-quality expectations embedded in eval fixtures.
pub const PACK_QUALITY_EXPECTATIONS_SCHEMA_V1: &str = "ee.eval.pack_quality_expectations.v1";

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
    pub pack_quality_expectations: Option<PackQualityExpectations>,
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

#[derive(Clone, Debug, Deserialize)]
pub struct SourceMemoryTier {
    pub name: String,
    #[serde(default)]
    pub expected_memory_count: u32,
    #[serde(default)]
    pub id_range: Option<SourceMemoryIdRange>,
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

    validate_pack_quality_expectations(scenario, source)
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

    let count = end_number - start_number + 1;
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
        .map_or(0, |index| index + 1);
    let digits = value.get(split_at..)?;
    if digits.is_empty() {
        return None;
    }
    let number = digits.parse().ok()?;
    Some((&value[..split_at], number, digits.len()))
}

fn source_memory_counts(path: &Path) -> Result<(usize, usize), DomainError> {
    let content = std::fs::read_to_string(path).map_err(|e| DomainError::Storage {
        message: format!("Failed to read source memory file {}: {e}", path.display()),
        repair: None,
    })?;

    let value: serde_json::Value =
        serde_json::from_str(&content).map_err(|e| DomainError::Import {
            message: format!("Failed to parse source memory JSON: {e}"),
            repair: Some("Check source_memory.json syntax".into()),
        })?;

    let Some(memories) = value.get("memories").and_then(serde_json::Value::as_array) else {
        return Ok((0, 0));
    };

    let query_count = memories
        .iter()
        .flat_map(|memory| {
            memory
                .get("expected_query_match")
                .and_then(serde_json::Value::as_array)
                .into_iter()
                .flatten()
        })
        .filter_map(serde_json::Value::as_str)
        .collect::<HashSet<_>>()
        .len();

    Ok((memories.len(), query_count))
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
