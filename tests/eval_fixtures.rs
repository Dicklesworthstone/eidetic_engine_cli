use ee::eval::{
    CommandStep, DegradedBranch, EVAL_FIXTURE_SCHEMA_V1, EvaluationScenario, ExpectedOutput,
    RedactionClass, RedactionLeakDetector,
};
use ee::models::model_registry::{
    EmbeddingMetadataRecord, ModelDistanceMetric, ModelProvider, ModelPurpose, ModelRegistryStatus,
    SEMANTIC_MODEL_ADMISSIBILITY_SCHEMA_V1, SemanticModelAdmissibilityBudget,
    SemanticModelAdmissibilityReport, SemanticModelCandidate,
};
use ee::policy::detect_instruction_like_content;
use serde_json::Value;

const RELEASE_FAILURE_SCENARIO: &str = include_str!("fixtures/eval/release_failure/scenario.json");
const RELEASE_FAILURE_SOURCE: &str =
    include_str!("fixtures/eval/release_failure/source_memory.json");
const RELEASE_FAILURE_README: &str = include_str!("fixtures/eval/release_failure/README.md");

const ASYNC_MIGRATION_SCENARIO: &str = include_str!("fixtures/eval/async_migration/scenario.json");
const ASYNC_MIGRATION_SOURCE: &str =
    include_str!("fixtures/eval/async_migration/source_memory.json");
const ASYNC_MIGRATION_README: &str = include_str!("fixtures/eval/async_migration/README.md");

const DANGEROUS_CLEANUP_SCENARIO: &str =
    include_str!("fixtures/eval/dangerous_cleanup/scenario.json");
const DANGEROUS_CLEANUP_SOURCE: &str =
    include_str!("fixtures/eval/dangerous_cleanup/source_memory.json");
const DANGEROUS_CLEANUP_README: &str = include_str!("fixtures/eval/dangerous_cleanup/README.md");

const MEMORY_POISONING_SCENARIO: &str =
    include_str!("fixtures/eval/memory_poisoning/scenario.json");
const MEMORY_POISONING_SOURCE: &str =
    include_str!("fixtures/eval/memory_poisoning/source_memory.json");
const MEMORY_POISONING_README: &str = include_str!("fixtures/eval/memory_poisoning/README.md");

const DATA_SIZE_TIERS_SCENARIO: &str = include_str!("fixtures/eval/data_size_tiers/scenario.json");
const DATA_SIZE_TIERS_SOURCE: &str =
    include_str!("fixtures/eval/data_size_tiers/source_memory.json");
const DATA_SIZE_TIERS_README: &str = include_str!("fixtures/eval/data_size_tiers/README.md");

const SEMANTIC_MODEL_ADMISSIBILITY_SCENARIO: &str =
    include_str!("fixtures/eval/semantic_model_admissibility/scenario.json");
const SEMANTIC_MODEL_ADMISSIBILITY_SOURCE: &str =
    include_str!("fixtures/eval/semantic_model_admissibility/source_memory.json");
const SEMANTIC_MODEL_ADMISSIBILITY_README: &str =
    include_str!("fixtures/eval/semantic_model_admissibility/README.md");

const METAMORPHIC_EVALUATION_SCENARIO: &str =
    include_str!("fixtures/eval/metamorphic_evaluation/scenario.json");
const METAMORPHIC_EVALUATION_SOURCE: &str =
    include_str!("fixtures/eval/metamorphic_evaluation/source_memory.json");
const METAMORPHIC_EVALUATION_README: &str =
    include_str!("fixtures/eval/metamorphic_evaluation/README.md");

type TestResult = Result<(), String>;

fn parse_json(source: &str, label: &str) -> Result<Value, String> {
    serde_json::from_str(source).map_err(|error| format!("{label} must parse as JSON: {error}"))
}

fn field<'a>(value: &'a Value, name: &str) -> Result<&'a Value, String> {
    value
        .get(name)
        .ok_or_else(|| format!("missing field `{name}`"))
}

fn string_field<'a>(value: &'a Value, name: &str) -> Result<&'a str, String> {
    field(value, name)?
        .as_str()
        .ok_or_else(|| format!("field `{name}` must be a string"))
}

fn array_field<'a>(value: &'a Value, name: &str) -> Result<&'a Vec<Value>, String> {
    field(value, name)?
        .as_array()
        .ok_or_else(|| format!("field `{name}` must be an array"))
}

fn ensure(condition: bool, ctx: &str) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(ctx.to_string())
    }
}

fn ensure_equal<T: std::fmt::Debug + PartialEq>(actual: T, expected: T, ctx: &str) -> TestResult {
    if actual == expected {
        Ok(())
    } else {
        Err(format!("{ctx}: expected {expected:?}, got {actual:?}"))
    }
}

fn array_contains_string(values: &[Value], expected: &str) -> bool {
    values.iter().any(|value| value.as_str() == Some(expected))
}

fn u32_field(value: &Value, name: &str) -> Result<u32, String> {
    let raw = field(value, name)?
        .as_u64()
        .ok_or_else(|| format!("field `{name}` must be an unsigned integer"))?;
    u32::try_from(raw).map_err(|error| format!("field `{name}` is out of range: {error}"))
}

fn u64_field(value: &Value, name: &str) -> Result<u64, String> {
    field(value, name)?
        .as_u64()
        .ok_or_else(|| format!("field `{name}` must be an unsigned integer"))
}

fn bool_field(value: &Value, name: &str) -> Result<bool, String> {
    field(value, name)?
        .as_bool()
        .ok_or_else(|| format!("field `{name}` must be a boolean"))
}

fn string_array_field<'a>(value: &'a Value, name: &str) -> Result<Vec<&'a str>, String> {
    array_field(value, name)?
        .iter()
        .map(|item| {
            item.as_str()
                .ok_or_else(|| format!("field `{name}` must contain only strings"))
        })
        .collect()
}

fn case_by_id<'a>(cases: &'a [Value], case_id: &str) -> Result<&'a Value, String> {
    cases
        .iter()
        .find(|case| case.get("case_id").and_then(Value::as_str) == Some(case_id))
        .ok_or_else(|| format!("missing case `{case_id}`"))
}

fn score_field(value: &Value, name: &str) -> Result<u64, String> {
    u64_field(field(value, "scores")?, name)
}

#[test]
fn release_failure_scenario_contract_is_complete() -> TestResult {
    let scenario = parse_json(RELEASE_FAILURE_SCENARIO, "release_failure scenario")?;

    ensure_equal(
        string_field(&scenario, "schema")?,
        EVAL_FIXTURE_SCHEMA_V1,
        "scenario schema",
    )?;
    ensure_equal(
        string_field(&scenario, "fixture_id")?,
        "fx.release_failure.v1",
        "fixture id",
    )?;
    ensure_equal(
        string_field(&scenario, "fixture_family")?,
        "release_failure",
        "fixture family",
    )?;
    ensure_equal(
        string_field(&scenario, "coverage_state")?,
        "implemented",
        "coverage state",
    )?;
    ensure(
        array_contains_string(
            array_field(&scenario, "owning_bead_ids")?,
            "eidetic_engine_cli-eu92",
        ),
        "fixture must be owned by EE-247",
    )?;
    ensure(
        array_contains_string(
            array_field(&scenario, "scenario_ids")?,
            "usr_pre_task_brief",
        ),
        "fixture must cover usr_pre_task_brief",
    )?;

    let commands = array_field(&scenario, "command_sequence")?;
    ensure_equal(commands.len(), 4, "command count")?;
    for (index, command) in commands.iter().enumerate() {
        let argv = array_field(command, "argv")?;
        ensure_equal(
            argv.first().and_then(Value::as_str),
            Some("ee"),
            "argv starts with ee",
        )?;
        ensure(
            array_contains_string(argv, "--workspace"),
            "every command must pin --workspace",
        )?;
        ensure_equal(
            field(command, "expected_exit_code")?.as_i64(),
            Some(0),
            "expected exit code",
        )?;
        ensure_equal(
            string_field(command, "stdout_schema")?,
            "ee.response.v1",
            "stdout schema",
        )?;
        ensure(
            string_field(command, "stdout_artifact_path")?
                .starts_with("target/ee-e2e/usr_pre_task_brief/"),
            "stdout artifact path must use the scenario artifact root",
        )?;
        ensure_equal(
            field(command, "step")?.as_u64(),
            Some(u64::try_from(index + 1).map_err(|error| error.to_string())?),
            "step ordering",
        )?;
    }

    let degraded_codes: Vec<&str> = array_field(&scenario, "degraded_branches")?
        .iter()
        .filter_map(|branch| branch.get("code").and_then(Value::as_str))
        .collect();
    ensure(
        degraded_codes.contains(&"semantic_disabled"),
        "semantic_disabled branch",
    )?;
    ensure(
        degraded_codes.contains(&"graph_snapshot_stale"),
        "graph_snapshot_stale branch",
    )?;

    let redaction = field(&scenario, "redaction")?;
    ensure_equal(
        array_field(redaction, "classes_expected")?.len(),
        0,
        "secret-free fixture must not expect redactions",
    )
}

#[test]
fn release_failure_source_memory_is_specific_and_secret_free() -> TestResult {
    let source = parse_json(RELEASE_FAILURE_SOURCE, "release_failure source")?;

    ensure_equal(
        string_field(&source, "schema")?,
        "ee.eval_source_memory.v1",
        "source schema",
    )?;
    ensure_equal(
        string_field(&source, "fixture_id")?,
        "fx.release_failure.v1",
        "source fixture id",
    )?;

    let memories = array_field(&source, "memories")?;
    ensure_equal(memories.len(), 2, "source memory count")?;

    let joined_content = memories
        .iter()
        .map(|memory| string_field(memory, "content"))
        .collect::<Result<Vec<_>, _>>()?
        .join("\n");

    for required in [
        "cargo fmt --check",
        "cargo clippy --all-targets -- -D warnings",
        "cargo test",
        "release workflow",
        "unused import",
    ] {
        ensure(
            joined_content.contains(required),
            &format!("source content must contain `{required}`"),
        )?;
    }

    let all_fixture_text =
        format!("{RELEASE_FAILURE_SCENARIO}\n{RELEASE_FAILURE_SOURCE}\n{RELEASE_FAILURE_README}");
    for forbidden in ["sk-", "ghp_", "AKIA", "-----BEGIN PRIVATE KEY-----"] {
        ensure(
            !all_fixture_text.contains(forbidden),
            &format!("fixture files must not contain secret marker `{forbidden}`"),
        )?;
    }

    ensure(
        RELEASE_FAILURE_README.contains("fx.release_failure.v1"),
        "README must name fixture ID",
    )?;
    ensure(
        RELEASE_FAILURE_README.contains("usr_pre_task_brief"),
        "README must name scenario ID",
    )
}

#[test]
fn release_failure_json_fixture_maps_to_eval_domain_types() -> TestResult {
    let raw = parse_json(RELEASE_FAILURE_SCENARIO, "release_failure scenario")?;
    let scenario_id = array_field(&raw, "scenario_ids")?
        .first()
        .and_then(Value::as_str)
        .ok_or_else(|| "scenario_ids must contain a string".to_string())?;
    let mut builder = EvaluationScenario::builder(scenario_id)
        .journey(string_field(&raw, "journey")?)
        .fixture_family(string_field(&raw, "fixture_family")?)
        .agent_success_signal(string_field(&raw, "agent_success_signal")?);

    for bead in array_field(&raw, "owning_bead_ids")? {
        builder = builder.owning_bead(
            bead.as_str()
                .ok_or_else(|| "owning bead must be a string".to_string())?,
        );
    }

    for gate in array_field(&raw, "owning_gate_ids")? {
        builder = builder.owning_gate(
            gate.as_str()
                .ok_or_else(|| "owning gate must be a string".to_string())?,
        );
    }

    for command in array_field(&raw, "command_sequence")? {
        let step = field(command, "step")?
            .as_u64()
            .ok_or_else(|| "command step must be an integer".to_string())?;
        let argv = array_field(command, "argv")?
            .iter()
            .map(|part| {
                part.as_str()
                    .map(str::to_string)
                    .ok_or_else(|| "argv part must be a string".to_string())
            })
            .collect::<Result<Vec<_>, _>>()?;
        let command_step = CommandStep::new(
            u32::try_from(step).map_err(|error| error.to_string())?,
            argv.join(" "),
        )
        .with_exit_code(
            i32::try_from(
                field(command, "expected_exit_code")?
                    .as_i64()
                    .ok_or_else(|| "expected exit code must be an integer".to_string())?,
            )
            .map_err(|error| error.to_string())?,
        )
        .with_schema(string_field(command, "stdout_schema")?);
        builder = builder.command(command_step);
    }

    for output in array_field(&raw, "expected_outputs")? {
        let step = field(output, "step")?
            .as_u64()
            .ok_or_else(|| "output step must be an integer".to_string())?;
        let mut expected = ExpectedOutput::new(
            u32::try_from(step).map_err(|error| error.to_string())?,
            string_field(output, "schema")?,
        );
        for required in array_field(output, "required_fields")? {
            expected = expected.require_field(
                required
                    .as_str()
                    .ok_or_else(|| "required field must be a string".to_string())?,
            );
        }
        for absent in array_field(output, "absent_fields")? {
            expected = expected.absent_field(
                absent
                    .as_str()
                    .ok_or_else(|| "absent field must be a string".to_string())?,
            );
        }
        builder = builder.expected_output(expected);
    }

    for branch in array_field(&raw, "degraded_branches")? {
        let mut degraded = DegradedBranch::new(
            string_field(branch, "code")?,
            string_field(branch, "description")?,
        );
        if let Some(repair) = branch.get("repair_action").and_then(Value::as_str) {
            degraded = degraded.with_repair(repair);
        }
        if branch
            .get("preserves_success_signal")
            .and_then(Value::as_bool)
            == Some(false)
        {
            degraded = degraded.signal_not_preserved();
        }
        builder = builder.degraded_branch(degraded);
    }

    let scenario = builder.build();
    ensure_equal(
        scenario.scenario_id,
        "usr_pre_task_brief".to_string(),
        "domain scenario id",
    )?;
    ensure_equal(scenario.command_sequence.len(), 4, "domain commands")?;
    ensure_equal(scenario.expected_outputs.len(), 4, "domain outputs")?;
    ensure_equal(
        scenario.degraded_branches.len(),
        2,
        "domain degraded branches",
    )?;
    ensure(
        scenario.agent_success_signal.contains("release"),
        "success signal remains agent-facing",
    )
}

#[test]
fn dangerous_cleanup_scenario_contract_is_complete() -> TestResult {
    let scenario = parse_json(DANGEROUS_CLEANUP_SCENARIO, "dangerous_cleanup scenario")?;

    ensure_equal(
        string_field(&scenario, "schema")?,
        EVAL_FIXTURE_SCHEMA_V1,
        "scenario schema",
    )?;
    ensure_equal(
        string_field(&scenario, "fixture_id")?,
        "fx.dangerous_cleanup.v1",
        "fixture id",
    )?;
    ensure_equal(
        string_field(&scenario, "fixture_family")?,
        "dangerous_cleanup",
        "fixture family",
    )?;
    ensure_equal(
        string_field(&scenario, "coverage_state")?,
        "implemented",
        "coverage state",
    )?;
    ensure(
        array_contains_string(
            array_field(&scenario, "owning_bead_ids")?,
            "eidetic_engine_cli-1kk2",
        ),
        "fixture must be owned by EE-249",
    )?;
    ensure(
        array_contains_string(
            array_field(&scenario, "scenario_ids")?,
            "usr_pre_cleanup_guard",
        ),
        "fixture must cover usr_pre_cleanup_guard",
    )?;

    let commands = array_field(&scenario, "command_sequence")?;
    ensure_equal(commands.len(), 4, "command count")?;
    for (index, command) in commands.iter().enumerate() {
        let argv = array_field(command, "argv")?;
        ensure_equal(
            argv.first().and_then(Value::as_str),
            Some("ee"),
            "argv starts with ee",
        )?;
        ensure(
            array_contains_string(argv, "--workspace"),
            "every command must pin --workspace",
        )?;
        ensure_equal(
            field(command, "expected_exit_code")?.as_i64(),
            Some(0),
            "expected exit code",
        )?;
        ensure_equal(
            string_field(command, "stdout_schema")?,
            "ee.response.v1",
            "stdout schema",
        )?;
        ensure(
            string_field(command, "stdout_artifact_path")?
                .starts_with("target/ee-e2e/usr_pre_cleanup_guard/"),
            "stdout artifact path must use the scenario artifact root",
        )?;
        ensure_equal(
            field(command, "step")?.as_u64(),
            Some(u64::try_from(index + 1).map_err(|error| error.to_string())?),
            "step ordering",
        )?;
    }

    let degraded_codes: Vec<&str> = array_field(&scenario, "degraded_branches")?
        .iter()
        .filter_map(|branch| branch.get("code").and_then(Value::as_str))
        .collect();
    ensure(
        degraded_codes.contains(&"semantic_disabled"),
        "semantic_disabled branch",
    )?;
    ensure(
        degraded_codes.contains(&"graph_snapshot_stale"),
        "graph_snapshot_stale branch",
    )?;
    ensure(
        degraded_codes.contains(&"policy_guard_unavailable"),
        "policy_guard_unavailable branch",
    )?;

    let redaction = field(&scenario, "redaction")?;
    ensure_equal(
        array_field(redaction, "classes_expected")?.len(),
        0,
        "secret-free fixture must not expect redactions",
    )
}

#[test]
fn dangerous_cleanup_source_memory_is_specific_and_secret_free() -> TestResult {
    let source = parse_json(DANGEROUS_CLEANUP_SOURCE, "dangerous_cleanup source")?;

    ensure_equal(
        string_field(&source, "schema")?,
        "ee.eval_source_memory.v1",
        "source schema",
    )?;
    ensure_equal(
        string_field(&source, "fixture_id")?,
        "fx.dangerous_cleanup.v1",
        "source fixture id",
    )?;

    let memories = array_field(&source, "memories")?;
    ensure_equal(memories.len(), 3, "source memory count")?;

    let joined_content = memories
        .iter()
        .map(|memory| string_field(memory, "content"))
        .collect::<Result<Vec<_>, _>>()?
        .join("\n");

    for required in [
        "git status --short --untracked-files=all",
        "git diff --stat",
        "explicit written permission",
        "git clean -fd",
        "git reset --hard",
        "rm -rf",
        "preserve unknown files",
    ] {
        ensure(
            joined_content.contains(required),
            &format!("source content must contain `{required}`"),
        )?;
    }

    let all_fixture_text = format!(
        "{DANGEROUS_CLEANUP_SCENARIO}\n{DANGEROUS_CLEANUP_SOURCE}\n{DANGEROUS_CLEANUP_README}"
    );
    for forbidden in ["sk-", "ghp_", "AKIA", "-----BEGIN PRIVATE KEY-----"] {
        ensure(
            !all_fixture_text.contains(forbidden),
            &format!("fixture files must not contain secret marker `{forbidden}`"),
        )?;
    }

    ensure(
        DANGEROUS_CLEANUP_README.contains("fx.dangerous_cleanup.v1"),
        "README must name fixture ID",
    )?;
    ensure(
        DANGEROUS_CLEANUP_README.contains("usr_pre_cleanup_guard"),
        "README must name scenario ID",
    )
}

#[test]
fn dangerous_cleanup_json_fixture_maps_to_eval_domain_types() -> TestResult {
    let raw = parse_json(DANGEROUS_CLEANUP_SCENARIO, "dangerous_cleanup scenario")?;
    let scenario_id = array_field(&raw, "scenario_ids")?
        .first()
        .and_then(Value::as_str)
        .ok_or_else(|| "scenario_ids must contain a string".to_string())?;
    let mut builder = EvaluationScenario::builder(scenario_id)
        .journey(string_field(&raw, "journey")?)
        .fixture_family(string_field(&raw, "fixture_family")?)
        .agent_success_signal(string_field(&raw, "agent_success_signal")?);

    for bead in array_field(&raw, "owning_bead_ids")? {
        builder = builder.owning_bead(
            bead.as_str()
                .ok_or_else(|| "owning bead must be a string".to_string())?,
        );
    }

    for gate in array_field(&raw, "owning_gate_ids")? {
        builder = builder.owning_gate(
            gate.as_str()
                .ok_or_else(|| "owning gate must be a string".to_string())?,
        );
    }

    for command in array_field(&raw, "command_sequence")? {
        let step = field(command, "step")?
            .as_u64()
            .ok_or_else(|| "command step must be an integer".to_string())?;
        let argv = array_field(command, "argv")?
            .iter()
            .map(|part| {
                part.as_str()
                    .map(str::to_string)
                    .ok_or_else(|| "argv part must be a string".to_string())
            })
            .collect::<Result<Vec<_>, _>>()?;
        let command_step = CommandStep::new(
            u32::try_from(step).map_err(|error| error.to_string())?,
            argv.join(" "),
        )
        .with_exit_code(
            i32::try_from(
                field(command, "expected_exit_code")?
                    .as_i64()
                    .ok_or_else(|| "expected exit code must be an integer".to_string())?,
            )
            .map_err(|error| error.to_string())?,
        )
        .with_schema(string_field(command, "stdout_schema")?);
        builder = builder.command(command_step);
    }

    for output in array_field(&raw, "expected_outputs")? {
        let step = field(output, "step")?
            .as_u64()
            .ok_or_else(|| "output step must be an integer".to_string())?;
        let mut expected = ExpectedOutput::new(
            u32::try_from(step).map_err(|error| error.to_string())?,
            string_field(output, "schema")?,
        );
        for required in array_field(output, "required_fields")? {
            expected = expected.require_field(
                required
                    .as_str()
                    .ok_or_else(|| "required field must be a string".to_string())?,
            );
        }
        for absent in array_field(output, "absent_fields")? {
            expected = expected.absent_field(
                absent
                    .as_str()
                    .ok_or_else(|| "absent field must be a string".to_string())?,
            );
        }
        builder = builder.expected_output(expected);
    }

    for branch in array_field(&raw, "degraded_branches")? {
        let mut degraded = DegradedBranch::new(
            string_field(branch, "code")?,
            string_field(branch, "description")?,
        );
        if let Some(repair) = branch.get("repair_action").and_then(Value::as_str) {
            degraded = degraded.with_repair(repair);
        }
        if branch
            .get("preserves_success_signal")
            .and_then(Value::as_bool)
            == Some(false)
        {
            degraded = degraded.signal_not_preserved();
        }
        builder = builder.degraded_branch(degraded);
    }

    let scenario = builder.build();
    ensure_equal(
        scenario.scenario_id,
        "usr_pre_cleanup_guard".to_string(),
        "domain scenario id",
    )?;
    ensure_equal(scenario.command_sequence.len(), 4, "domain commands")?;
    ensure_equal(scenario.expected_outputs.len(), 4, "domain outputs")?;
    ensure_equal(
        scenario.degraded_branches.len(),
        3,
        "domain degraded branches",
    )?;
    ensure(
        scenario.agent_success_signal.contains("clean up"),
        "success signal remains agent-facing",
    )
}

#[test]
fn memory_poisoning_scenario_contract_is_complete() -> TestResult {
    let scenario = parse_json(MEMORY_POISONING_SCENARIO, "memory_poisoning scenario")?;

    ensure_equal(
        string_field(&scenario, "schema")?,
        EVAL_FIXTURE_SCHEMA_V1,
        "scenario schema",
    )?;
    ensure_equal(
        string_field(&scenario, "fixture_id")?,
        "fx.memory_poisoning.v1",
        "fixture id",
    )?;
    ensure_equal(
        string_field(&scenario, "fixture_family")?,
        "memory_poisoning",
        "fixture family",
    )?;
    ensure_equal(
        string_field(&scenario, "coverage_state")?,
        "implemented",
        "coverage state",
    )?;
    ensure(
        array_contains_string(
            array_field(&scenario, "owning_bead_ids")?,
            "eidetic_engine_cli-73b0",
        ),
        "fixture must be owned by EE-262",
    )?;
    ensure(
        array_contains_string(
            array_field(&scenario, "scenario_ids")?,
            "usr_import_poisoned_memory_guard",
        ),
        "fixture must cover usr_import_poisoned_memory_guard",
    )?;

    let commands = array_field(&scenario, "command_sequence")?;
    ensure_equal(commands.len(), 4, "command count")?;
    for (index, command) in commands.iter().enumerate() {
        let argv = array_field(command, "argv")?;
        ensure_equal(
            argv.first().and_then(Value::as_str),
            Some("ee"),
            "argv starts with ee",
        )?;
        ensure(
            array_contains_string(argv, "--workspace"),
            "every command must pin --workspace",
        )?;
        ensure(
            string_field(command, "stdout_artifact_path")?
                .starts_with("target/ee-e2e/usr_import_poisoned_memory_guard/"),
            "stdout artifact path must use the scenario artifact root",
        )?;
        ensure_equal(
            field(command, "step")?.as_u64(),
            Some(u64::try_from(index + 1).map_err(|error| error.to_string())?),
            "step ordering",
        )?;
    }

    let poisoned_command = &commands[2];
    ensure_equal(
        field(poisoned_command, "expected_exit_code")?.as_i64(),
        Some(7),
        "poisoned memory is policy-denied",
    )?;
    ensure_equal(
        string_field(poisoned_command, "stdout_schema")?,
        "ee.error.v1",
        "poisoned memory returns JSON error",
    )?;

    let policy = field(&scenario, "policy_expectations")?;
    ensure_equal(
        string_field(policy, "required_error_code")?,
        "policy_instruction_like_content",
        "policy error code",
    )?;
    ensure_equal(
        string_field(policy, "required_risk")?,
        "high",
        "policy risk",
    )?;
    for signal in [
        "ignore_previous_instructions",
        "reveal_system_prompt",
        "send_credentials",
        "developer_role_markup",
        "must_obey_this_memory",
    ] {
        ensure(
            array_contains_string(array_field(policy, "required_signal_codes")?, signal),
            &format!("policy expectations must include `{signal}`"),
        )?;
    }

    let degraded_branches = array_field(&scenario, "degraded_branches")?;
    let degraded_codes: Vec<&str> = degraded_branches
        .iter()
        .filter_map(|branch| branch.get("code").and_then(Value::as_str))
        .collect();
    ensure(
        degraded_codes.contains(&"policy_guard_unavailable"),
        "policy_guard_unavailable branch",
    )?;
    ensure(
        degraded_codes.contains(&"semantic_disabled"),
        "semantic_disabled branch",
    )?;
    ensure(
        degraded_codes.contains(&"quarantine_store_unavailable"),
        "quarantine_store_unavailable branch",
    )?;
    ensure(
        degraded_branches.iter().any(|branch| {
            branch.get("code").and_then(Value::as_str) == Some("quarantine_store_unavailable")
                && branch
                    .get("preserves_success_signal")
                    .and_then(Value::as_bool)
                    == Some(false)
        }),
        "quarantine storage failure must be fail-closed",
    )?;

    let redaction = field(&scenario, "redaction")?;
    ensure_equal(
        array_field(redaction, "classes_expected")?.len(),
        0,
        "secret-free fixture must not expect redactions",
    )
}

#[test]
fn memory_poisoning_source_memory_policy_signals_are_deterministic_and_secret_free() -> TestResult {
    let source = parse_json(MEMORY_POISONING_SOURCE, "memory_poisoning source")?;

    ensure_equal(
        string_field(&source, "schema")?,
        "ee.eval_source_memory.v1",
        "source schema",
    )?;
    ensure_equal(
        string_field(&source, "fixture_id")?,
        "fx.memory_poisoning.v1",
        "source fixture id",
    )?;

    let memories = array_field(&source, "memories")?;
    ensure_equal(memories.len(), 3, "source memory count")?;

    let mut denied_count = 0usize;
    for memory in memories {
        let content = string_field(memory, "content")?;
        let expected_policy = field(memory, "expected_policy")?;
        let expected_instruction_like = field(expected_policy, "is_instruction_like")?
            .as_bool()
            .ok_or_else(|| "expected_policy.is_instruction_like must be bool".to_string())?;
        let expected_risk = string_field(expected_policy, "risk")?;

        let report = detect_instruction_like_content(content);
        ensure_equal(
            report.is_instruction_like,
            expected_instruction_like,
            "instruction-like classification",
        )?;
        ensure_equal(report.risk.as_str(), expected_risk, "instruction risk")?;

        let signal_codes: Vec<&str> = report.signals.iter().map(|signal| signal.code).collect();
        for signal in array_field(expected_policy, "signal_codes")? {
            let expected_signal = signal
                .as_str()
                .ok_or_else(|| "signal code must be a string".to_string())?;
            ensure(
                signal_codes.contains(&expected_signal),
                &format!("detector must report `{expected_signal}`"),
            )?;
        }

        if expected_instruction_like {
            denied_count += 1;
            let rejected_reasons = field(expected_policy, "rejected_reasons")?;
            for reason in rejected_reasons
                .as_array()
                .ok_or_else(|| "rejected_reasons must be an array".to_string())?
            {
                let expected_reason = reason
                    .as_str()
                    .ok_or_else(|| "rejected reason must be a string".to_string())?;
                ensure(
                    report.rejected_reasons.contains(&expected_reason),
                    &format!("detector must reject for `{expected_reason}`"),
                )?;
            }
        } else {
            ensure(
                report.rejected_reasons.is_empty(),
                "safe memory must not have rejection reasons",
            )?;
        }
    }
    ensure_equal(denied_count, 2, "two poisoned memories are denied")?;

    let all_fixture_text = format!(
        "{MEMORY_POISONING_SCENARIO}\n{MEMORY_POISONING_SOURCE}\n{MEMORY_POISONING_README}"
    );
    for forbidden in ["sk-", "ghp_", "AKIA", "-----BEGIN PRIVATE KEY-----"] {
        ensure(
            !all_fixture_text.contains(forbidden),
            &format!("fixture files must not contain secret marker `{forbidden}`"),
        )?;
    }

    ensure(
        MEMORY_POISONING_README.contains("fx.memory_poisoning.v1"),
        "README must name fixture ID",
    )?;
    ensure(
        MEMORY_POISONING_README.contains("usr_import_poisoned_memory_guard"),
        "README must name scenario ID",
    )
}

#[test]
fn memory_poisoning_json_fixture_maps_to_eval_domain_types() -> TestResult {
    let raw = parse_json(MEMORY_POISONING_SCENARIO, "memory_poisoning scenario")?;
    let scenario_id = array_field(&raw, "scenario_ids")?
        .first()
        .and_then(Value::as_str)
        .ok_or_else(|| "scenario_ids must contain a string".to_string())?;
    let mut builder = EvaluationScenario::builder(scenario_id)
        .journey(string_field(&raw, "journey")?)
        .fixture_family(string_field(&raw, "fixture_family")?)
        .agent_success_signal(string_field(&raw, "agent_success_signal")?);

    for bead in array_field(&raw, "owning_bead_ids")? {
        builder = builder.owning_bead(
            bead.as_str()
                .ok_or_else(|| "owning bead must be a string".to_string())?,
        );
    }

    for gate in array_field(&raw, "owning_gate_ids")? {
        builder = builder.owning_gate(
            gate.as_str()
                .ok_or_else(|| "owning gate must be a string".to_string())?,
        );
    }

    for command in array_field(&raw, "command_sequence")? {
        let step = field(command, "step")?
            .as_u64()
            .ok_or_else(|| "command step must be an integer".to_string())?;
        let argv = array_field(command, "argv")?
            .iter()
            .map(|part| {
                part.as_str()
                    .map(str::to_string)
                    .ok_or_else(|| "argv part must be a string".to_string())
            })
            .collect::<Result<Vec<_>, _>>()?;
        let command_step = CommandStep::new(
            u32::try_from(step).map_err(|error| error.to_string())?,
            argv.join(" "),
        )
        .with_exit_code(
            i32::try_from(
                field(command, "expected_exit_code")?
                    .as_i64()
                    .ok_or_else(|| "expected exit code must be an integer".to_string())?,
            )
            .map_err(|error| error.to_string())?,
        )
        .with_schema(string_field(command, "stdout_schema")?);
        builder = builder.command(command_step);
    }

    for output in array_field(&raw, "expected_outputs")? {
        let step = field(output, "step")?
            .as_u64()
            .ok_or_else(|| "output step must be an integer".to_string())?;
        let mut expected = ExpectedOutput::new(
            u32::try_from(step).map_err(|error| error.to_string())?,
            string_field(output, "schema")?,
        );
        for required in array_field(output, "required_fields")? {
            expected = expected.require_field(
                required
                    .as_str()
                    .ok_or_else(|| "required field must be a string".to_string())?,
            );
        }
        for absent in array_field(output, "absent_fields")? {
            expected = expected.absent_field(
                absent
                    .as_str()
                    .ok_or_else(|| "absent field must be a string".to_string())?,
            );
        }
        builder = builder.expected_output(expected);
    }

    for branch in array_field(&raw, "degraded_branches")? {
        let mut degraded = DegradedBranch::new(
            string_field(branch, "code")?,
            string_field(branch, "description")?,
        );
        if let Some(repair) = branch.get("repair_action").and_then(Value::as_str) {
            degraded = degraded.with_repair(repair);
        }
        if branch
            .get("preserves_success_signal")
            .and_then(Value::as_bool)
            == Some(false)
        {
            degraded = degraded.signal_not_preserved();
        }
        builder = builder.degraded_branch(degraded);
    }

    let scenario = builder.build();
    ensure_equal(
        scenario.scenario_id,
        "usr_import_poisoned_memory_guard".to_string(),
        "domain scenario id",
    )?;
    ensure_equal(scenario.command_sequence.len(), 4, "domain commands")?;
    ensure_equal(
        scenario.command_sequence[2].expected_exit_code,
        7,
        "poisoned memory command is policy denied",
    )?;
    ensure_equal(scenario.expected_outputs.len(), 4, "domain outputs")?;
    ensure_equal(
        scenario.degraded_branches.len(),
        3,
        "domain degraded branches",
    )?;
    ensure(
        scenario
            .degraded_branches
            .iter()
            .any(|branch| !branch.preserves_success_signal),
        "at least one degraded branch fails closed",
    )?;
    ensure(
        scenario.agent_success_signal.contains("poisoned"),
        "success signal remains agent-facing",
    )
}

#[test]
fn data_size_tiers_scenario_contract_is_complete() -> TestResult {
    let scenario = parse_json(DATA_SIZE_TIERS_SCENARIO, "data_size_tiers scenario")?;

    ensure_equal(
        string_field(&scenario, "schema")?,
        EVAL_FIXTURE_SCHEMA_V1,
        "scenario schema",
    )?;
    ensure_equal(
        string_field(&scenario, "fixture_id")?,
        "fx.data_size_tiers.v1",
        "fixture id",
    )?;
    ensure_equal(
        string_field(&scenario, "fixture_family")?,
        "data_size_tiers",
        "fixture family",
    )?;
    ensure_equal(
        string_field(&scenario, "coverage_state")?,
        "implemented",
        "coverage state",
    )?;
    ensure(
        array_contains_string(
            array_field(&scenario, "owning_bead_ids")?,
            "eidetic_engine_cli-cgqc",
        ),
        "fixture must be owned by EE-258",
    )?;
    for scenario_id in [
        "usr_context_small_workspace",
        "usr_context_medium_workspace",
        "usr_context_large_workspace",
    ] {
        ensure(
            array_contains_string(array_field(&scenario, "scenario_ids")?, scenario_id),
            &format!("fixture must cover `{scenario_id}`"),
        )?;
    }

    let expected_tiers = [
        (
            "small",
            "usr_context_small_workspace",
            6,
            800,
            "profile.data_size_tiers.small.v1",
            &[
                "all_task_relevant_memories_selected",
                "no_budget_truncation",
                "full_provenance_chain_present",
            ],
        ),
        (
            "medium",
            "usr_context_medium_workspace",
            60,
            2400,
            "profile.data_size_tiers.medium.v1",
            &[
                "section_quotas_applied",
                "redundant_memories_suppressed",
                "selection_explanations_present",
            ],
        ),
        (
            "large",
            "usr_context_large_workspace",
            600,
            4000,
            "profile.data_size_tiers.large.v1",
            &[
                "top_ranked_memories_selected",
                "budget_truncation_explained",
                "degraded_lexical_path_preserves_core_signal",
            ],
        ),
    ];
    let tiers = array_field(&scenario, "data_size_tiers")?;
    ensure_equal(tiers.len(), expected_tiers.len(), "tier count")?;

    let mut previous_memory_count = 0;
    let mut previous_token_budget = 0;
    for (tier, expected) in tiers.iter().zip(expected_tiers) {
        let (
            expected_name,
            expected_scenario_id,
            expected_memory_count,
            expected_max_tokens,
            expected_profile,
            expected_behaviors,
        ) = expected;
        ensure_equal(string_field(tier, "name")?, expected_name, "tier name")?;
        ensure_equal(
            string_field(tier, "scenario_id")?,
            expected_scenario_id,
            "tier scenario id",
        )?;
        ensure_equal(
            field(tier, "expected_memory_count")?.as_u64(),
            Some(expected_memory_count),
            "tier memory count",
        )?;
        ensure_equal(
            field(tier, "max_tokens")?.as_u64(),
            Some(expected_max_tokens),
            "tier token budget",
        )?;
        ensure_equal(
            string_field(tier, "generation_profile")?,
            expected_profile,
            "generation profile",
        )?;
        ensure(
            expected_memory_count > previous_memory_count,
            "tier memory counts must increase",
        )?;
        ensure(
            expected_max_tokens > previous_token_budget,
            "tier token budgets must increase",
        )?;
        previous_memory_count = expected_memory_count;
        previous_token_budget = expected_max_tokens;

        let behaviors = array_field(tier, "expected_pack_behavior")?;
        for expected_behavior in expected_behaviors {
            ensure(
                array_contains_string(behaviors, expected_behavior),
                &format!("tier must expect `{expected_behavior}`"),
            )?;
        }
    }

    let commands = array_field(&scenario, "command_sequence")?;
    ensure_equal(commands.len(), 5, "command count")?;
    for (index, command) in commands.iter().enumerate() {
        let argv = array_field(command, "argv")?;
        ensure_equal(
            argv.first().and_then(Value::as_str),
            Some("ee"),
            "argv starts with ee",
        )?;
        ensure(
            array_contains_string(argv, "--workspace"),
            "every command must pin --workspace",
        )?;
        ensure_equal(
            field(command, "expected_exit_code")?.as_i64(),
            Some(0),
            "expected exit code",
        )?;
        ensure_equal(
            string_field(command, "stdout_schema")?,
            "ee.response.v1",
            "stdout schema",
        )?;
        ensure(
            string_field(command, "stdout_artifact_path")?
                .starts_with("target/ee-e2e/data_size_tiers/"),
            "stdout artifact path must use the data-size tier artifact root",
        )?;
        ensure_equal(
            field(command, "step")?.as_u64(),
            Some(u64::try_from(index + 1).map_err(|error| error.to_string())?),
            "step ordering",
        )?;
    }

    for (command_index, expected_tier, expected_tokens) in [
        (2usize, "small", "800"),
        (3usize, "medium", "2400"),
        (4usize, "large", "4000"),
    ] {
        let command = &commands[command_index];
        let argv = array_field(command, "argv")?;
        ensure(
            array_contains_string(argv, "context"),
            "tier commands must exercise ee context",
        )?;
        ensure(
            array_contains_string(argv, "--max-tokens"),
            "tier context commands must set --max-tokens",
        )?;
        ensure(
            array_contains_string(argv, expected_tokens),
            &format!("tier command must set `{expected_tokens}` tokens"),
        )?;
        ensure_equal(
            string_field(command, "tier")?,
            expected_tier,
            "context command tier label",
        )?;
    }

    let degraded_codes: Vec<&str> = array_field(&scenario, "degraded_branches")?
        .iter()
        .filter_map(|branch| branch.get("code").and_then(Value::as_str))
        .collect();
    for required in [
        "semantic_disabled",
        "pack_budget_exceeded",
        "graph_snapshot_stale",
    ] {
        ensure(
            degraded_codes.contains(&required),
            &format!("degraded branches must include `{required}`"),
        )?;
    }

    let redaction = field(&scenario, "redaction")?;
    ensure_equal(
        array_field(redaction, "classes_expected")?.len(),
        0,
        "secret-free fixture must not expect redactions",
    )
}

#[test]
fn data_size_tiers_source_memory_profiles_are_deterministic_and_secret_free() -> TestResult {
    let scenario = parse_json(DATA_SIZE_TIERS_SCENARIO, "data_size_tiers scenario")?;
    let source = parse_json(DATA_SIZE_TIERS_SOURCE, "data_size_tiers source")?;

    ensure_equal(
        string_field(&source, "schema")?,
        "ee.eval_source_memory.v1",
        "source schema",
    )?;
    ensure_equal(
        string_field(&source, "fixture_id")?,
        "fx.data_size_tiers.v1",
        "source fixture id",
    )?;
    ensure_equal(
        string_field(&source, "source_kind")?,
        "synthetic",
        "source kind",
    )?;

    let seed_memory = field(&source, "seed_memory")?;
    ensure_equal(
        string_field(seed_memory, "id")?,
        "mem_00000000000000000000000501",
        "seed memory id",
    )?;
    ensure(
        string_field(seed_memory, "content")?.contains("token budget usage"),
        "seed memory must describe token-budget comparison",
    )?;
    ensure(
        array_contains_string(array_field(seed_memory, "tags")?, "data-size-tier"),
        "seed memory must carry data-size-tier tag",
    )?;

    let expected_tiers = [
        (
            "small",
            6,
            "profile.data_size_tiers.small.v1",
            1,
            0,
            "small workspace history",
        ),
        (
            "medium",
            60,
            "profile.data_size_tiers.medium.v1",
            2,
            5,
            "medium workspace history",
        ),
        (
            "large",
            600,
            "profile.data_size_tiers.large.v1",
            3,
            7,
            "large workspace history",
        ),
    ];
    let source_tiers = array_field(&source, "tiers")?;
    let scenario_tiers = array_field(&scenario, "data_size_tiers")?;
    ensure_equal(
        source_tiers.len(),
        expected_tiers.len(),
        "source tier count",
    )?;
    ensure_equal(
        scenario_tiers.len(),
        expected_tiers.len(),
        "scenario tier count",
    )?;

    for ((source_tier, scenario_tier), expected) in source_tiers
        .iter()
        .zip(scenario_tiers.iter())
        .zip(expected_tiers)
    {
        let (
            expected_name,
            expected_count,
            expected_profile,
            expected_relevant_every,
            expected_distractor_every,
            expected_query_marker,
        ) = expected;
        ensure_equal(
            string_field(source_tier, "name")?,
            expected_name,
            "source tier name",
        )?;
        ensure_equal(
            field(source_tier, "expected_memory_count")?.as_u64(),
            Some(expected_count),
            "source tier count",
        )?;
        ensure_equal(
            field(scenario_tier, "expected_memory_count")?.as_u64(),
            Some(expected_count),
            "scenario tier count",
        )?;

        let generator = field(source_tier, "generator")?;
        ensure_equal(
            string_field(generator, "profile")?,
            expected_profile,
            "generator profile",
        )?;
        ensure_equal(
            string_field(scenario_tier, "generation_profile")?,
            expected_profile,
            "scenario generation profile",
        )?;
        ensure_equal(
            field(generator, "relevant_every")?.as_u64(),
            Some(expected_relevant_every),
            "generator relevant cadence",
        )?;
        ensure_equal(
            field(generator, "distractor_every")?.as_u64(),
            Some(expected_distractor_every),
            "generator distractor cadence",
        )?;
        ensure(
            array_contains_string(
                array_field(source_tier, "expected_query_match")?,
                expected_query_marker,
            ),
            &format!("source tier must expose query marker `{expected_query_marker}`"),
        )?;
    }

    let secret_policy = field(&source, "secret_policy")?;
    ensure_equal(
        field(secret_policy, "secret_like_values_present")?.as_bool(),
        Some(false),
        "source fixture must be secret-free",
    )?;

    let all_fixture_text =
        format!("{DATA_SIZE_TIERS_SCENARIO}\n{DATA_SIZE_TIERS_SOURCE}\n{DATA_SIZE_TIERS_README}");
    for forbidden in ["sk-", "ghp_", "AKIA", "-----BEGIN PRIVATE KEY-----"] {
        ensure(
            !all_fixture_text.contains(forbidden),
            &format!("fixture files must not contain secret marker `{forbidden}`"),
        )?;
    }

    ensure(
        DATA_SIZE_TIERS_README.contains("fx.data_size_tiers.v1"),
        "README must name fixture ID",
    )?;
    for scenario_id in [
        "usr_context_small_workspace",
        "usr_context_medium_workspace",
        "usr_context_large_workspace",
    ] {
        ensure(
            DATA_SIZE_TIERS_README.contains(scenario_id),
            &format!("README must name scenario ID `{scenario_id}`"),
        )?;
    }
    Ok(())
}

#[test]
fn data_size_tiers_json_fixture_maps_to_eval_domain_types() -> TestResult {
    let raw = parse_json(DATA_SIZE_TIERS_SCENARIO, "data_size_tiers scenario")?;
    let scenario_id = array_field(&raw, "scenario_ids")?
        .first()
        .and_then(Value::as_str)
        .ok_or_else(|| "scenario_ids must contain a string".to_string())?;
    let mut builder = EvaluationScenario::builder(scenario_id)
        .journey(string_field(&raw, "journey")?)
        .fixture_family(string_field(&raw, "fixture_family")?)
        .agent_success_signal(string_field(&raw, "agent_success_signal")?);

    for bead in array_field(&raw, "owning_bead_ids")? {
        builder = builder.owning_bead(
            bead.as_str()
                .ok_or_else(|| "owning bead must be a string".to_string())?,
        );
    }

    for gate in array_field(&raw, "owning_gate_ids")? {
        builder = builder.owning_gate(
            gate.as_str()
                .ok_or_else(|| "owning gate must be a string".to_string())?,
        );
    }

    for command in array_field(&raw, "command_sequence")? {
        let step = field(command, "step")?
            .as_u64()
            .ok_or_else(|| "command step must be an integer".to_string())?;
        let argv = array_field(command, "argv")?
            .iter()
            .map(|part| {
                part.as_str()
                    .map(str::to_string)
                    .ok_or_else(|| "argv part must be a string".to_string())
            })
            .collect::<Result<Vec<_>, _>>()?;
        let command_step = CommandStep::new(
            u32::try_from(step).map_err(|error| error.to_string())?,
            argv.join(" "),
        )
        .with_exit_code(
            i32::try_from(
                field(command, "expected_exit_code")?
                    .as_i64()
                    .ok_or_else(|| "expected exit code must be an integer".to_string())?,
            )
            .map_err(|error| error.to_string())?,
        )
        .with_schema(string_field(command, "stdout_schema")?);
        builder = builder.command(command_step);
    }

    for output in array_field(&raw, "expected_outputs")? {
        let step = field(output, "step")?
            .as_u64()
            .ok_or_else(|| "output step must be an integer".to_string())?;
        let mut expected = ExpectedOutput::new(
            u32::try_from(step).map_err(|error| error.to_string())?,
            string_field(output, "schema")?,
        );
        for required in array_field(output, "required_fields")? {
            expected = expected.require_field(
                required
                    .as_str()
                    .ok_or_else(|| "required field must be a string".to_string())?,
            );
        }
        for absent in array_field(output, "absent_fields")? {
            expected = expected.absent_field(
                absent
                    .as_str()
                    .ok_or_else(|| "absent field must be a string".to_string())?,
            );
        }
        builder = builder.expected_output(expected);
    }

    for branch in array_field(&raw, "degraded_branches")? {
        let mut degraded = DegradedBranch::new(
            string_field(branch, "code")?,
            string_field(branch, "description")?,
        );
        if let Some(repair) = branch.get("repair_action").and_then(Value::as_str) {
            degraded = degraded.with_repair(repair);
        }
        if branch
            .get("preserves_success_signal")
            .and_then(Value::as_bool)
            == Some(false)
        {
            degraded = degraded.signal_not_preserved();
        }
        builder = builder.degraded_branch(degraded);
    }

    let scenario = builder.build();
    ensure_equal(
        scenario.scenario_id,
        "usr_context_small_workspace".to_string(),
        "domain scenario id",
    )?;
    ensure_equal(scenario.command_sequence.len(), 5, "domain commands")?;
    ensure(
        scenario.command_sequence[4]
            .command_template
            .contains("--max-tokens 4000"),
        "large-tier command keeps its token budget",
    )?;
    ensure_equal(scenario.expected_outputs.len(), 5, "domain outputs")?;
    ensure_equal(
        scenario.degraded_branches.len(),
        3,
        "domain degraded branches",
    )?;
    ensure(
        scenario.agent_success_signal.contains("large workspace"),
        "success signal remains agent-facing",
    )
}

#[test]
fn async_migration_scenario_contract_is_complete() -> TestResult {
    let scenario = parse_json(ASYNC_MIGRATION_SCENARIO, "async_migration scenario")?;

    ensure_equal(
        string_field(&scenario, "schema")?,
        EVAL_FIXTURE_SCHEMA_V1,
        "scenario schema",
    )?;
    ensure_equal(
        string_field(&scenario, "fixture_id")?,
        "fx.async_migration.v1",
        "fixture id",
    )?;
    ensure_equal(
        string_field(&scenario, "fixture_family")?,
        "async_migration",
        "fixture family",
    )?;
    ensure_equal(
        string_field(&scenario, "coverage_state")?,
        "implemented",
        "coverage state",
    )?;
    ensure(
        array_contains_string(
            array_field(&scenario, "owning_bead_ids")?,
            "eidetic_engine_cli-h7g3",
        ),
        "fixture must be owned by EE-248",
    )?;
    ensure(
        array_contains_string(
            array_field(&scenario, "scenario_ids")?,
            "usr_pre_migration_context",
        ),
        "fixture must cover usr_pre_migration_context",
    )?;

    let commands = array_field(&scenario, "command_sequence")?;
    ensure_equal(commands.len(), 5, "command count")?;
    for (index, command) in commands.iter().enumerate() {
        let argv = array_field(command, "argv")?;
        ensure_equal(
            argv.first().and_then(Value::as_str),
            Some("ee"),
            "argv starts with ee",
        )?;
        ensure(
            array_contains_string(argv, "--workspace"),
            "every command must pin --workspace",
        )?;
        ensure_equal(
            field(command, "expected_exit_code")?.as_i64(),
            Some(0),
            "expected exit code",
        )?;
        ensure_equal(
            string_field(command, "stdout_schema")?,
            "ee.response.v1",
            "stdout schema",
        )?;
        ensure(
            string_field(command, "stdout_artifact_path")?
                .starts_with("target/ee-e2e/usr_pre_migration_context/"),
            "stdout artifact path must use the scenario artifact root",
        )?;
        ensure_equal(
            field(command, "step")?.as_u64(),
            Some(u64::try_from(index + 1).map_err(|error| error.to_string())?),
            "step ordering",
        )?;
    }

    let degraded_codes: Vec<&str> = array_field(&scenario, "degraded_branches")?
        .iter()
        .filter_map(|branch| branch.get("code").and_then(Value::as_str))
        .collect();
    ensure(
        degraded_codes.contains(&"semantic_disabled"),
        "semantic_disabled branch",
    )?;
    ensure(
        degraded_codes.contains(&"graph_snapshot_stale"),
        "graph_snapshot_stale branch",
    )?;
    ensure(
        degraded_codes.contains(&"migration_queue_degraded"),
        "migration_queue_degraded branch",
    )?;

    let redaction = field(&scenario, "redaction")?;
    ensure_equal(
        array_field(redaction, "classes_expected")?.len(),
        0,
        "secret-free fixture must not expect redactions",
    )
}

#[test]
fn async_migration_source_memory_is_specific_and_secret_free() -> TestResult {
    let source = parse_json(ASYNC_MIGRATION_SOURCE, "async_migration source")?;

    ensure_equal(
        string_field(&source, "schema")?,
        "ee.eval_source_memory.v1",
        "source schema",
    )?;
    ensure_equal(
        string_field(&source, "fixture_id")?,
        "fx.async_migration.v1",
        "source fixture id",
    )?;

    let memories = array_field(&source, "memories")?;
    ensure_equal(memories.len(), 3, "source memory count")?;

    let joined_content = memories
        .iter()
        .map(|memory| string_field(memory, "content"))
        .collect::<Result<Vec<_>, _>>()?
        .join("\n");

    for required in [
        "async migration",
        "job queue",
        "rollback",
        "checkpoint",
        "timeout",
    ] {
        ensure(
            joined_content.to_lowercase().contains(required),
            &format!("source content must contain `{required}`"),
        )?;
    }

    let all_fixture_text =
        format!("{ASYNC_MIGRATION_SCENARIO}\n{ASYNC_MIGRATION_SOURCE}\n{ASYNC_MIGRATION_README}");
    for forbidden in ["sk-", "ghp_", "AKIA", "-----BEGIN PRIVATE KEY-----"] {
        ensure(
            !all_fixture_text.contains(forbidden),
            &format!("fixture files must not contain secret marker `{forbidden}`"),
        )?;
    }

    ensure(
        ASYNC_MIGRATION_README.contains("fx.async_migration.v1"),
        "README must name fixture ID",
    )?;
    ensure(
        ASYNC_MIGRATION_README.contains("usr_pre_migration_context"),
        "README must name scenario ID",
    )
}

#[test]
fn async_migration_json_fixture_maps_to_eval_domain_types() -> TestResult {
    let raw = parse_json(ASYNC_MIGRATION_SCENARIO, "async_migration scenario")?;
    let scenario_id = array_field(&raw, "scenario_ids")?
        .first()
        .and_then(Value::as_str)
        .ok_or_else(|| "scenario_ids must contain a string".to_string())?;
    let mut builder = EvaluationScenario::builder(scenario_id)
        .journey(string_field(&raw, "journey")?)
        .fixture_family(string_field(&raw, "fixture_family")?)
        .agent_success_signal(string_field(&raw, "agent_success_signal")?);

    for bead in array_field(&raw, "owning_bead_ids")? {
        builder = builder.owning_bead(
            bead.as_str()
                .ok_or_else(|| "owning bead must be a string".to_string())?,
        );
    }

    for gate in array_field(&raw, "owning_gate_ids")? {
        builder = builder.owning_gate(
            gate.as_str()
                .ok_or_else(|| "owning gate must be a string".to_string())?,
        );
    }

    for command in array_field(&raw, "command_sequence")? {
        let step = field(command, "step")?
            .as_u64()
            .ok_or_else(|| "command step must be an integer".to_string())?;
        let argv = array_field(command, "argv")?
            .iter()
            .map(|part| {
                part.as_str()
                    .map(str::to_string)
                    .ok_or_else(|| "argv part must be a string".to_string())
            })
            .collect::<Result<Vec<_>, _>>()?;
        let command_step = CommandStep::new(
            u32::try_from(step).map_err(|error| error.to_string())?,
            argv.join(" "),
        )
        .with_exit_code(
            i32::try_from(
                field(command, "expected_exit_code")?
                    .as_i64()
                    .ok_or_else(|| "expected exit code must be an integer".to_string())?,
            )
            .map_err(|error| error.to_string())?,
        )
        .with_schema(string_field(command, "stdout_schema")?);
        builder = builder.command(command_step);
    }

    for output in array_field(&raw, "expected_outputs")? {
        let step = field(output, "step")?
            .as_u64()
            .ok_or_else(|| "output step must be an integer".to_string())?;
        let mut expected = ExpectedOutput::new(
            u32::try_from(step).map_err(|error| error.to_string())?,
            string_field(output, "schema")?,
        );
        for required in array_field(output, "required_fields")? {
            expected = expected.require_field(
                required
                    .as_str()
                    .ok_or_else(|| "required field must be a string".to_string())?,
            );
        }
        for absent in array_field(output, "absent_fields")? {
            expected = expected.absent_field(
                absent
                    .as_str()
                    .ok_or_else(|| "absent field must be a string".to_string())?,
            );
        }
        builder = builder.expected_output(expected);
    }

    for branch in array_field(&raw, "degraded_branches")? {
        let mut degraded = DegradedBranch::new(
            string_field(branch, "code")?,
            string_field(branch, "description")?,
        );
        if let Some(repair) = branch.get("repair_action").and_then(Value::as_str) {
            degraded = degraded.with_repair(repair);
        }
        if branch
            .get("preserves_success_signal")
            .and_then(Value::as_bool)
            == Some(false)
        {
            degraded = degraded.signal_not_preserved();
        }
        builder = builder.degraded_branch(degraded);
    }

    let scenario = builder.build();
    ensure_equal(
        scenario.scenario_id,
        "usr_pre_migration_context".to_string(),
        "domain scenario id",
    )?;
    ensure_equal(scenario.command_sequence.len(), 5, "domain commands")?;
    ensure_equal(scenario.expected_outputs.len(), 5, "domain outputs")?;
    ensure_equal(
        scenario.degraded_branches.len(),
        3,
        "domain degraded branches",
    )?;
    ensure(
        scenario.agent_success_signal.contains("migration"),
        "success signal remains agent-facing",
    )
}

#[test]
fn semantic_model_admissibility_scenario_contract_is_complete() -> TestResult {
    let scenario = parse_json(
        SEMANTIC_MODEL_ADMISSIBILITY_SCENARIO,
        "semantic_model_admissibility scenario",
    )?;

    ensure_equal(
        string_field(&scenario, "schema")?,
        EVAL_FIXTURE_SCHEMA_V1,
        "scenario schema",
    )?;
    ensure_equal(
        string_field(&scenario, "fixture_id")?,
        "fx.semantic_model_admissibility.v1",
        "fixture id",
    )?;
    ensure_equal(
        string_field(&scenario, "coverage_state")?,
        "implemented",
        "coverage state",
    )?;
    ensure(
        array_contains_string(
            array_field(&scenario, "owning_bead_ids")?,
            "eidetic_engine_cli-dsx4",
        ),
        "fixture must be owned by EE-315",
    )?;
    ensure(
        array_contains_string(
            array_field(&scenario, "scenario_ids")?,
            "usr_semantic_model_budget_guard",
        ),
        "fixture must cover usr_semantic_model_budget_guard",
    )?;

    let budget = field(&scenario, "semantic_budget")?;
    ensure_equal(
        string_field(budget, "schema")?,
        SEMANTIC_MODEL_ADMISSIBILITY_SCHEMA_V1,
        "budget schema",
    )?;
    ensure_equal(u32_field(budget, "maxDimension")?, 4096, "max dimension")?;
    ensure_equal(
        u32_field(budget, "maxQueryTokens")?,
        4096,
        "max query tokens",
    )?;
    ensure_equal(
        bool_field(budget, "allowRemoteModels")?,
        false,
        "remote models disabled",
    )?;
    ensure_equal(
        bool_field(budget, "requireDeterministic")?,
        true,
        "deterministic mode required",
    )?;

    let commands = array_field(&scenario, "command_sequence")?;
    ensure_equal(commands.len(), 3, "command count")?;
    for command in commands {
        ensure_equal(
            string_field(command, "stdout_schema")?,
            "ee.response.v1",
            "stdout schema",
        )?;
        ensure_equal(
            string_field(command, "stderr_policy")?,
            "diagnostics_only",
            "stderr policy",
        )?;
        ensure(
            string_field(command, "stdout_artifact_path")?
                .starts_with("target/ee-e2e/semantic_model_admissibility/"),
            "stdout artifact path must use fixture root",
        )?;
    }

    let degraded_codes: Vec<&str> = array_field(&scenario, "degraded_branches")?
        .iter()
        .filter_map(|branch| branch.get("code").and_then(Value::as_str))
        .collect();
    for required in [
        "semantic_disabled",
        "semantic_dimension_exceeds_budget",
        "remote_semantic_model_denied",
    ] {
        ensure(
            degraded_codes.contains(&required),
            &format!("degraded branch must contain `{required}`"),
        )?;
    }

    let all_fixture_text = format!(
        "{SEMANTIC_MODEL_ADMISSIBILITY_SCENARIO}\n{SEMANTIC_MODEL_ADMISSIBILITY_SOURCE}\n{SEMANTIC_MODEL_ADMISSIBILITY_README}"
    );
    for forbidden in ["sk-", "ghp_", "AKIA", "-----BEGIN PRIVATE KEY-----"] {
        ensure(
            !all_fixture_text.contains(forbidden),
            &format!("fixture files must not contain secret marker `{forbidden}`"),
        )?;
    }

    ensure(
        SEMANTIC_MODEL_ADMISSIBILITY_README.contains("fx.semantic_model_admissibility.v1"),
        "README must name fixture ID",
    )?;
    ensure(
        SEMANTIC_MODEL_ADMISSIBILITY_README.contains("usr_semantic_model_budget_guard"),
        "README must name scenario ID",
    )
}

#[test]
fn semantic_model_admissibility_source_maps_to_domain_reports() -> TestResult {
    let source = parse_json(
        SEMANTIC_MODEL_ADMISSIBILITY_SOURCE,
        "semantic_model_admissibility source",
    )?;

    ensure_equal(
        string_field(&source, "schema")?,
        "ee.eval_source_memory.v1",
        "source schema",
    )?;
    ensure_equal(
        string_field(&source, "fixture_id")?,
        "fx.semantic_model_admissibility.v1",
        "source fixture id",
    )?;

    let budget_json = field(&source, "budget")?;
    let mut budget = SemanticModelAdmissibilityBudget::local_default();
    budget.max_dimension = u32_field(budget_json, "maxDimension")?;
    budget.max_query_tokens = u32_field(budget_json, "maxQueryTokens")?;
    budget.max_model_bytes = Some(u64_field(budget_json, "maxModelBytes")?);
    budget.max_p95_latency_ms = Some(u32_field(budget_json, "maxP95LatencyMs")?);
    budget.allow_remote_models = bool_field(budget_json, "allowRemoteModels")?;
    budget.require_deterministic = bool_field(budget_json, "requireDeterministic")?;
    budget.require_known_token_limit = bool_field(budget_json, "requireKnownTokenLimit")?;
    budget.validate().map_err(|error| error.to_string())?;

    let candidates = array_field(&source, "candidates")?;
    ensure_equal(candidates.len(), 4, "candidate count")?;

    let mut observed_modes = Vec::new();
    for candidate_json in candidates {
        let mut metadata = EmbeddingMetadataRecord::new(
            u32_field(candidate_json, "dimension")?,
            ModelDistanceMetric::Cosine,
        );
        metadata.max_input_tokens = Some(u32_field(candidate_json, "max_input_tokens")?);
        metadata.tokenizer = Some("fixture:semantic_model_admissibility".to_string());
        metadata.model_revision = Some("fixture.v1".to_string());
        metadata.deterministic = bool_field(candidate_json, "deterministic")?;

        let provider = string_field(candidate_json, "provider")?
            .parse::<ModelProvider>()
            .map_err(|error| error.to_string())?;
        let purpose = string_field(candidate_json, "purpose")?
            .parse::<ModelPurpose>()
            .map_err(|error| error.to_string())?;
        let status = string_field(candidate_json, "status")?
            .parse::<ModelRegistryStatus>()
            .map_err(|error| error.to_string())?;

        let candidate = SemanticModelCandidate::new(
            string_field(candidate_json, "model_id")?,
            provider,
            metadata,
        )
        .with_purpose(purpose)
        .with_status(status)
        .with_model_bytes(u64_field(candidate_json, "model_bytes")?)
        .with_p95_latency_ms(u32_field(candidate_json, "p95_latency_ms")?)
        .with_remote(bool_field(candidate_json, "remote")?);

        let report = SemanticModelAdmissibilityReport::evaluate(&candidate, &budget)
            .map_err(|error| error.to_string())?;
        ensure_equal(
            report.mode.as_str(),
            string_field(candidate_json, "expected_mode")?,
            "expected mode",
        )?;

        let actual_codes: Vec<&str> = report
            .degradation_codes
            .iter()
            .chain(report.rejection_codes.iter())
            .map(String::as_str)
            .collect();
        let expected_codes: Vec<&str> = array_field(candidate_json, "expected_codes")?
            .iter()
            .map(|code| {
                code.as_str()
                    .ok_or_else(|| "expected code must be a string".to_string())
            })
            .collect::<Result<Vec<_>, _>>()?;
        ensure_equal(actual_codes, expected_codes, "expected codes")?;

        observed_modes.push(report.mode.as_str());
    }

    ensure_equal(
        observed_modes,
        vec![
            "semantic",
            "lexical_fallback",
            "lexical_fallback",
            "rejected",
        ],
        "mode coverage",
    )
}

#[test]
fn semantic_model_admissibility_privacy_surfaces_stay_clean() -> TestResult {
    let scenario = parse_json(
        SEMANTIC_MODEL_ADMISSIBILITY_SCENARIO,
        "semantic_model_admissibility scenario",
    )?;
    let source = parse_json(
        SEMANTIC_MODEL_ADMISSIBILITY_SOURCE,
        "semantic_model_admissibility source",
    )?;
    let detector = RedactionLeakDetector::new();
    let privacy_classes = [
        RedactionClass::Secret,
        RedactionClass::Pii,
        RedactionClass::InternalPath,
    ];

    let all_fixture_text = format!(
        "{SEMANTIC_MODEL_ADMISSIBILITY_SCENARIO}\n{SEMANTIC_MODEL_ADMISSIBILITY_SOURCE}\n{SEMANTIC_MODEL_ADMISSIBILITY_README}"
    );
    let fixture_leaks = detector.detect_leaks_in_classes(&all_fixture_text, &privacy_classes);
    ensure(
        fixture_leaks.is_empty(),
        &format!(
            "semantic admissibility fixture must not leak privacy-sensitive values: {}",
            fixture_leaks
                .iter()
                .map(|leak| leak.display())
                .collect::<Vec<_>>()
                .join("; ")
        ),
    )?;

    let secret_policy = field(&source, "secret_policy")?;
    ensure_equal(
        string_field(secret_policy, "synthetic_secret_policy")?,
        "none",
        "semantic fixture secret policy",
    )?;
    ensure_equal(
        bool_field(secret_policy, "secret_like_values_present")?,
        false,
        "semantic fixture secret-like value flag",
    )?;
    for blocked in [
        "api_token",
        "github_token",
        "cloud_access_key",
        "private_key",
    ] {
        ensure(
            array_contains_string(array_field(secret_policy, "blocked_classes")?, blocked),
            &format!("semantic fixture must block `{blocked}`"),
        )?;
    }

    for output in array_field(&scenario, "expected_outputs")? {
        let absent = array_field(output, "absent_fields")?;
        ensure(
            array_contains_string(absent, "secret"),
            "semantic outputs must declare secret absence",
        )?;
        ensure(
            array_contains_string(absent, "token"),
            "semantic outputs must declare token absence",
        )?;
    }

    let budget_json = field(&source, "budget")?;
    let mut budget = SemanticModelAdmissibilityBudget::local_default();
    budget.max_dimension = u32_field(budget_json, "maxDimension")?;
    budget.max_query_tokens = u32_field(budget_json, "maxQueryTokens")?;
    budget.max_model_bytes = Some(u64_field(budget_json, "maxModelBytes")?);
    budget.max_p95_latency_ms = Some(u32_field(budget_json, "maxP95LatencyMs")?);
    budget.allow_remote_models = bool_field(budget_json, "allowRemoteModels")?;
    budget.require_deterministic = bool_field(budget_json, "requireDeterministic")?;
    budget.require_known_token_limit = bool_field(budget_json, "requireKnownTokenLimit")?;
    budget.validate().map_err(|error| error.to_string())?;

    for candidate_json in array_field(&source, "candidates")? {
        let mut metadata = EmbeddingMetadataRecord::new(
            u32_field(candidate_json, "dimension")?,
            ModelDistanceMetric::Cosine,
        );
        metadata.max_input_tokens = Some(u32_field(candidate_json, "max_input_tokens")?);
        metadata.tokenizer = Some("fixture:semantic_model_admissibility".to_string());
        metadata.model_revision = Some("fixture.v1".to_string());
        metadata.deterministic = bool_field(candidate_json, "deterministic")?;

        let provider = string_field(candidate_json, "provider")?
            .parse::<ModelProvider>()
            .map_err(|error| error.to_string())?;
        let purpose = string_field(candidate_json, "purpose")?
            .parse::<ModelPurpose>()
            .map_err(|error| error.to_string())?;
        let status = string_field(candidate_json, "status")?
            .parse::<ModelRegistryStatus>()
            .map_err(|error| error.to_string())?;

        let candidate = SemanticModelCandidate::new(
            string_field(candidate_json, "model_id")?,
            provider,
            metadata,
        )
        .with_purpose(purpose)
        .with_status(status)
        .with_model_bytes(u64_field(candidate_json, "model_bytes")?)
        .with_p95_latency_ms(u32_field(candidate_json, "p95_latency_ms")?)
        .with_remote(bool_field(candidate_json, "remote")?);

        let report = SemanticModelAdmissibilityReport::evaluate(&candidate, &budget)
            .map_err(|error| error.to_string())?;
        let report_json = serde_json::to_string(&report).map_err(|error| error.to_string())?;
        let report_leaks = detector.detect_leaks_in_classes(&report_json, &privacy_classes);
        ensure(
            report_leaks.is_empty(),
            &format!(
                "semantic report for `{}` must not leak privacy-sensitive values: {}",
                report.model_id,
                report_leaks
                    .iter()
                    .map(|leak| leak.display())
                    .collect::<Vec<_>>()
                    .join("; ")
            ),
        )?;
    }

    Ok(())
}

#[test]
fn metamorphic_evaluation_scenario_contract_is_complete() -> TestResult {
    let scenario = parse_json(
        METAMORPHIC_EVALUATION_SCENARIO,
        "metamorphic_evaluation scenario",
    )?;

    ensure_equal(
        string_field(&scenario, "schema")?,
        EVAL_FIXTURE_SCHEMA_V1,
        "scenario schema",
    )?;
    ensure_equal(
        string_field(&scenario, "fixture_id")?,
        "fx.metamorphic_evaluation.v1",
        "fixture id",
    )?;
    ensure_equal(
        string_field(&scenario, "fixture_family")?,
        "metamorphic_evaluation",
        "fixture family",
    )?;
    ensure_equal(
        string_field(&scenario, "coverage_state")?,
        "implemented",
        "coverage state",
    )?;
    ensure(
        array_contains_string(
            array_field(&scenario, "owning_bead_ids")?,
            "eidetic_engine_cli-e46v",
        ),
        "fixture must be owned by EE-316",
    )?;
    ensure(
        array_contains_string(
            array_field(&scenario, "scenario_ids")?,
            "usr_eval_metamorphic_memory_regressions",
        ),
        "fixture must cover usr_eval_metamorphic_memory_regressions",
    )?;

    let commands = array_field(&scenario, "command_sequence")?;
    ensure_equal(commands.len(), 6, "command count")?;
    for (index, command) in commands.iter().enumerate() {
        ensure_equal(
            field(command, "step")?.as_u64(),
            Some(u64::try_from(index + 1).map_err(|error| error.to_string())?),
            "step ordering",
        )?;
        ensure_equal(
            string_field(command, "stdout_schema")?,
            "ee.response.v1",
            "stdout schema",
        )?;
        ensure_equal(
            string_field(command, "stderr_policy")?,
            "diagnostics_only",
            "stderr policy",
        )?;
        ensure(
            string_field(command, "stdout_artifact_path")?
                .starts_with("target/ee-e2e/metamorphic_evaluation/"),
            "stdout artifact path must use fixture root",
        )?;
    }

    let relation_ids: Vec<&str> = array_field(&scenario, "metamorphic_relations")?
        .iter()
        .map(|relation| string_field(relation, "relation_id"))
        .collect::<Result<Vec<_>, _>>()?;
    for required in [
        "mr_feedback_strengthens_rule",
        "mr_contradiction_requires_review",
        "mr_supersession_prefers_latest",
        "mr_budget_shrink_preserves_priority",
        "mr_semantic_fallback_preserves_lexical",
    ] {
        ensure(
            relation_ids.contains(&required),
            &format!("metamorphic relation `{required}` must be present"),
        )?;
    }
    for relation in array_field(&scenario, "metamorphic_relations")? {
        ensure_equal(
            bool_field(relation, "must_hold")?,
            true,
            "metamorphic relation must hold",
        )?;
        ensure(
            !array_field(relation, "required_codes")?.is_empty(),
            "metamorphic relation must declare required codes",
        )?;
    }

    let degraded_codes: Vec<&str> = array_field(&scenario, "degraded_branches")?
        .iter()
        .filter_map(|branch| branch.get("code").and_then(Value::as_str))
        .collect();
    for required in [
        "semantic_disabled",
        "token_budget_exhausted",
        "contradiction_requires_review",
    ] {
        ensure(
            degraded_codes.contains(&required),
            &format!("degraded branch must contain `{required}`"),
        )?;
    }

    let all_fixture_text = format!(
        "{METAMORPHIC_EVALUATION_SCENARIO}\n{METAMORPHIC_EVALUATION_SOURCE}\n{METAMORPHIC_EVALUATION_README}"
    );
    for forbidden in ["sk-", "ghp_", "AKIA", "-----BEGIN PRIVATE KEY-----"] {
        ensure(
            !all_fixture_text.contains(forbidden),
            &format!("fixture files must not contain secret marker `{forbidden}`"),
        )?;
    }

    ensure(
        METAMORPHIC_EVALUATION_README.contains("fx.metamorphic_evaluation.v1"),
        "README must name fixture ID",
    )?;
    ensure(
        METAMORPHIC_EVALUATION_README.contains("usr_eval_metamorphic_memory_regressions"),
        "README must name scenario ID",
    )
}

#[test]
fn metamorphic_evaluation_source_relations_are_self_consistent() -> TestResult {
    let source = parse_json(
        METAMORPHIC_EVALUATION_SOURCE,
        "metamorphic_evaluation source",
    )?;

    ensure_equal(
        string_field(&source, "schema")?,
        "ee.eval_source_memory.v1",
        "source schema",
    )?;
    ensure_equal(
        string_field(&source, "fixture_id")?,
        "fx.metamorphic_evaluation.v1",
        "source fixture id",
    )?;

    let cases = array_field(&source, "cases")?;
    ensure_equal(cases.len(), 8, "case count")?;

    let base = case_by_id(cases, "base_rule_only")?;
    let feedback = case_by_id(cases, "positive_feedback_added")?;
    ensure(
        string_array_field(base, "selected_memory_ids")?.contains(&"mem_release_verification_rule"),
        "baseline selects the release verification rule",
    )?;
    ensure(
        string_array_field(feedback, "selected_memory_ids")?
            .contains(&"mem_release_verification_rule"),
        "feedback variant preserves the release verification rule",
    )?;
    ensure(
        score_field(feedback, "rule_confidence")? > score_field(base, "rule_confidence")?,
        "positive feedback must increase rule confidence",
    )?;
    ensure(
        string_array_field(feedback, "metamorphic_codes")?
            .contains(&"positive_feedback_reinforces_rule"),
        "feedback variant records reinforcement code",
    )?;

    let contradiction = case_by_id(cases, "contradictory_evidence_added")?;
    ensure(
        string_array_field(contradiction, "review_codes")?.contains(&"contradiction_detected"),
        "contradiction variant requires review",
    )?;
    ensure(
        score_field(contradiction, "contradiction_risk")?
            > score_field(base, "contradiction_risk")?,
        "contradiction risk must increase",
    )?;
    ensure(
        string_array_field(contradiction, "excluded_memory_ids")?
            .contains(&"mem_skip_verification_counterclaim"),
        "unsafe counterclaim is not packed as authoritative context",
    )?;

    let supersession = case_by_id(cases, "supersession_chain_added")?;
    ensure(
        string_array_field(supersession, "selected_memory_ids")?
            .contains(&"mem_release_procedure_latest"),
        "supersession variant selects latest procedure",
    )?;
    ensure(
        string_array_field(supersession, "excluded_memory_ids")?
            .contains(&"mem_release_procedure_old"),
        "supersession variant excludes older procedure",
    )?;
    ensure(
        string_array_field(supersession, "metamorphic_codes")?
            .contains(&"supersession_prefers_latest"),
        "supersession variant records latest-wins code",
    )?;

    let full_budget = case_by_id(cases, "budget_full")?;
    let tight_budget = case_by_id(cases, "budget_tight")?;
    ensure(
        score_field(tight_budget, "selected_tokens")? <= u64_field(tight_budget, "max_tokens")?,
        "tight budget must respect max token budget",
    )?;
    ensure(
        score_field(tight_budget, "selected_tokens")?
            <= score_field(full_budget, "selected_tokens")?,
        "tight budget must not use more tokens than full budget",
    )?;
    ensure(
        string_array_field(tight_budget, "selected_memory_ids")?
            .contains(&"mem_release_verification_rule"),
        "tight budget preserves the highest-priority release rule",
    )?;
    ensure(
        string_array_field(tight_budget, "excluded_memory_ids")?
            .contains(&"mem_release_background_detail"),
        "tight budget drops lower-priority background detail",
    )?;

    let semantic = case_by_id(cases, "semantic_available")?;
    let fallback = case_by_id(cases, "semantic_disabled_fallback")?;
    ensure_equal(
        string_array_field(fallback, "selected_memory_ids")?,
        string_array_field(semantic, "selected_memory_ids")?,
        "lexical fallback preserves selected memory IDs",
    )?;
    ensure(
        string_array_field(fallback, "degradation_codes")?.contains(&"semantic_disabled"),
        "fallback records semantic_disabled degradation",
    )?;
    ensure_equal(
        string_field(fallback, "semantic_mode")?,
        "lexical_fallback",
        "fallback semantic mode",
    )
}

#[test]
fn metamorphic_evaluation_json_fixture_maps_to_eval_domain_types() -> TestResult {
    let raw = parse_json(
        METAMORPHIC_EVALUATION_SCENARIO,
        "metamorphic_evaluation scenario",
    )?;
    let scenario_id = array_field(&raw, "scenario_ids")?
        .first()
        .and_then(Value::as_str)
        .ok_or_else(|| "scenario_ids must contain a string".to_string())?;
    let mut builder = EvaluationScenario::builder(scenario_id)
        .journey(string_field(&raw, "journey")?)
        .fixture_family(string_field(&raw, "fixture_family")?)
        .agent_success_signal(string_field(&raw, "agent_success_signal")?);

    for bead in array_field(&raw, "owning_bead_ids")? {
        builder = builder.owning_bead(
            bead.as_str()
                .ok_or_else(|| "owning bead must be a string".to_string())?,
        );
    }

    for gate in array_field(&raw, "owning_gate_ids")? {
        builder = builder.owning_gate(
            gate.as_str()
                .ok_or_else(|| "owning gate must be a string".to_string())?,
        );
    }

    for command in array_field(&raw, "command_sequence")? {
        let step = field(command, "step")?
            .as_u64()
            .ok_or_else(|| "command step must be an integer".to_string())?;
        let argv = array_field(command, "argv")?
            .iter()
            .map(|part| {
                part.as_str()
                    .map(str::to_string)
                    .ok_or_else(|| "argv part must be a string".to_string())
            })
            .collect::<Result<Vec<_>, _>>()?;
        let command_step = CommandStep::new(
            u32::try_from(step).map_err(|error| error.to_string())?,
            argv.join(" "),
        )
        .with_exit_code(
            i32::try_from(
                field(command, "expected_exit_code")?
                    .as_i64()
                    .ok_or_else(|| "expected exit code must be an integer".to_string())?,
            )
            .map_err(|error| error.to_string())?,
        )
        .with_schema(string_field(command, "stdout_schema")?);
        builder = builder.command(command_step);
    }

    for output in array_field(&raw, "expected_outputs")? {
        let step = field(output, "step")?
            .as_u64()
            .ok_or_else(|| "output step must be an integer".to_string())?;
        let mut expected = ExpectedOutput::new(
            u32::try_from(step).map_err(|error| error.to_string())?,
            string_field(output, "schema")?,
        );
        for required in array_field(output, "required_fields")? {
            expected = expected.require_field(
                required
                    .as_str()
                    .ok_or_else(|| "required field must be a string".to_string())?,
            );
        }
        for absent in array_field(output, "absent_fields")? {
            expected = expected.absent_field(
                absent
                    .as_str()
                    .ok_or_else(|| "absent field must be a string".to_string())?,
            );
        }
        builder = builder.expected_output(expected);
    }

    for branch in array_field(&raw, "degraded_branches")? {
        let mut degraded = DegradedBranch::new(
            string_field(branch, "code")?,
            string_field(branch, "description")?,
        );
        if let Some(repair) = branch.get("repair_action").and_then(Value::as_str) {
            degraded = degraded.with_repair(repair);
        }
        if branch
            .get("preserves_success_signal")
            .and_then(Value::as_bool)
            == Some(false)
        {
            degraded = degraded.signal_not_preserved();
        }
        builder = builder.degraded_branch(degraded);
    }

    let scenario = builder.build();
    ensure_equal(
        scenario.scenario_id,
        "usr_eval_metamorphic_memory_regressions".to_string(),
        "domain scenario id",
    )?;
    ensure_equal(scenario.command_sequence.len(), 6, "domain commands")?;
    ensure_equal(scenario.expected_outputs.len(), 6, "domain outputs")?;
    ensure_equal(
        scenario.degraded_branches.len(),
        3,
        "domain degraded branches",
    )?;
    ensure(
        scenario.agent_success_signal.contains("metamorphic"),
        "success signal remains agent-facing",
    )
}
