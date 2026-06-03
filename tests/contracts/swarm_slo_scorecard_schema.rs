//! Contract coverage for `ee.swarm_slo.scorecard.v1`.
//!
//! Golden fixture validation proves representative scorecards still validate.
//! This contract pins the public schema structure directly so scorecard
//! producers cannot accidentally weaken required fields that downstream swarm
//! replay, budget, and support-bundle consumers rely on.

use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;

use serde_json::Value;

type TestResult = Result<(), String>;

const SCHEMA_PATH: &str = "docs/schemas/ee.swarm_slo.scorecard.v1.json";
const SCHEMA_ID: &str = "https://eidetic-engine/schemas/ee.swarm_slo.scorecard.v1.json";
const SCHEMA_NAME: &str = "ee.swarm_slo.scorecard.v1";

const REQUIRED_TOP_LEVEL: &[&str] = &[
    "schema",
    "workload",
    "sourceHealth",
    "budgets",
    "measurements",
    "stageAttribution",
    "verdict",
    "failureReasons",
    "budgetVerdicts",
    "regressionReasons",
    "determinism",
    "redaction",
];

const REQUIRED_WORKLOAD: &[&str] = &[
    "profileId",
    "scenario",
    "agentCount",
    "traceSchema",
    "expectedDegradationPosture",
    "concurrency",
    "commandMix",
    "provenance",
];

const REQUIRED_CONCURRENCY: &[&str] = &[
    "shape",
    "requestedAgents",
    "activeAgents",
    "maxParallelAgents",
];
const REQUIRED_COMMAND_COUNT: &[&str] = &["command", "count"];
const REQUIRED_PROVENANCE: &[&str] = &["kind", "recordedRows", "syntheticRows", "traceHash"];
const REQUIRED_SOURCE_HEALTH: &[&str] = &["agentMail", "beads", "bv", "rch", "workspace"];
const REQUIRED_SOURCE_POSTURE: &[&str] = &["status", "evidence"];

const REQUIRED_BUDGETS: &[&str] = &[
    "profile",
    "p50MsTarget",
    "p95MsTarget",
    "p99MsTarget",
    "errorCountMax",
    "degradedCountMax",
    "stageBudgets",
];

const REQUIRED_STAGE_BUDGET: &[&str] = &["stage", "p95MsTarget"];
const REQUIRED_MEASUREMENTS: &[&str] = &[
    "sampleCount",
    "commandCount",
    "errorCount",
    "degradedCount",
    "latency",
];
const REQUIRED_LATENCY: &[&str] = &["p50Ms", "p95Ms", "p99Ms", "maxMs"];
const REQUIRED_STAGE_ATTRIBUTION: &[&str] = &["stage", "elapsedMs", "shareBasisPoints", "verdict"];
const REQUIRED_VERDICT: &[&str] = &["status", "summary", "failingBudgetCount"];
const REQUIRED_FAILURE_REASON: &[&str] = &["code", "severity", "message", "source"];
const REQUIRED_BUDGET_VERDICT: &[&str] = &[
    "name",
    "category",
    "measurement",
    "target",
    "unit",
    "status",
    "reasonCode",
];
const REQUIRED_REGRESSION_REASON: &[&str] = &["code", "severity", "message", "repair", "evidence"];
const REQUIRED_DETERMINISM: &[&str] = &[
    "replayHash",
    "fixtureHash",
    "stable",
    "volatileFieldsStripped",
];
const REQUIRED_REDACTION: &[&str] = &[
    "rawMailBodiesPresent",
    "rawMemoryBodiesPresent",
    "rawCommandOutputPresent",
    "privatePathsPresent",
    "secretScanApplied",
];

const SCENARIOS: &[&str] = &[
    "healthy_small_checkout",
    "crowded_checkout",
    "agent_mail_unavailable",
    "bv_timeout_no_output",
    "rch_topology_blocked",
];

const DEGRADATION_POSTURES: &[&str] = &["none", "recoverable", "required", "blocked"];
const CONCURRENCY_SHAPES: &[&str] = &[
    "serial",
    "bounded_parallel",
    "wide_parallel",
    "resource_limited",
];
const PROVENANCE_KINDS: &[&str] = &["synthetic", "recorded", "mixed"];
const SOURCE_STATUSES: &[&str] = &[
    "ok",
    "degraded",
    "unavailable",
    "timeout",
    "blocked",
    "unknown",
];
const BUDGET_PROFILES: &[&str] = &[
    "ci_smoke",
    "developer_crowded_checkout",
    "swarm_heavy_64_agent",
    "stress_256gb_host",
];
const STAGES: &[&str] = &[
    "coordination",
    "context",
    "search",
    "why",
    "record",
    "agent_mail",
    "beads",
    "bv",
    "rch",
    "workspace",
    "other",
];
const RESULT_STATUSES: &[&str] = &["pass", "warn", "fail", "blocked"];
const SEVERITIES: &[&str] = &["info", "low", "warning", "medium", "high", "critical"];
const FAILURE_SOURCES: &[&str] = &["budget", "coordination", "runner", "rch", "workspace"];
const BUDGET_CATEGORIES: &[&str] = &["latency", "error", "degradation", "source", "stage"];
const BUDGET_UNITS: &[&str] = &["ms", "count"];
const REGRESSION_CODES: &[&str] = &[
    "context_p99_over_budget",
    "coordination_source_unavailable",
    "rch_topology_blocked",
    "budget_profile_missing",
    "replay_nondeterministic",
    "unsafe_mutation_in_replay",
];

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

fn load_schema() -> Result<Value, String> {
    let path = repo_root().join(SCHEMA_PATH);
    let text =
        fs::read_to_string(&path).map_err(|error| format!("read {}: {error}", path.display()))?;
    serde_json::from_str(&text).map_err(|error| format!("parse {}: {error}", path.display()))
}

fn collect_string_set(node: &Value, ctx: &str) -> Result<BTreeSet<String>, String> {
    let array = node
        .as_array()
        .ok_or_else(|| format!("{ctx}: expected array, got {node}"))?;
    array
        .iter()
        .map(|value| {
            value
                .as_str()
                .map(str::to_owned)
                .ok_or_else(|| format!("{ctx}: non-string entry {value}"))
        })
        .collect()
}

fn expected_string_set(values: &[&str]) -> BTreeSet<String> {
    values.iter().map(|value| (*value).to_owned()).collect()
}

fn require_exact_strings(
    schema: &Value,
    pointer: &str,
    expected: &[&str],
    label: &str,
) -> TestResult {
    let actual = collect_string_set(schema.pointer(pointer).unwrap_or(&Value::Null), label)?;
    let expected = expected_string_set(expected);
    ensure(
        actual == expected,
        format!("{label} drifted from exact set; expected {expected:?}, got {actual:?}"),
    )
}

fn require_schema_identity(schema: &Value) -> TestResult {
    ensure(
        schema.pointer("/$id").and_then(Value::as_str) == Some(SCHEMA_ID),
        format!("scorecard schema $id must stay {SCHEMA_ID}"),
    )?;
    ensure(
        schema.pointer("/title").and_then(Value::as_str) == Some(SCHEMA_NAME),
        format!("scorecard schema title must stay {SCHEMA_NAME}"),
    )?;
    ensure(
        schema
            .pointer("/properties/schema/const")
            .and_then(Value::as_str)
            == Some(SCHEMA_NAME),
        format!("scorecard schema const must stay {SCHEMA_NAME}"),
    )?;
    ensure(
        schema
            .pointer("/additionalProperties")
            .and_then(Value::as_bool)
            == Some(false),
        "scorecard top-level schema must remain closed",
    )
}

fn require_example_fields(schema: &Value, required: &[&str]) -> TestResult {
    let example = schema
        .pointer("/examples/0")
        .and_then(Value::as_object)
        .ok_or_else(|| "scorecard schema must include an object example".to_string())?;
    for field in required {
        ensure(
            example.contains_key(*field),
            format!("scorecard example missing required field `{field}`"),
        )?;
    }
    Ok(())
}

fn require_no_raw_sensitive_example_values(schema: &Value) -> TestResult {
    let example = schema
        .pointer("/examples/0")
        .ok_or_else(|| "scorecard schema must include an example".to_string())?
        .to_string();
    for forbidden in ["PinkOriole", "api_key", "/tmp", "id_ed25519"] {
        ensure(
            !example.contains(forbidden),
            format!("scorecard example leaks raw sensitive fixture value `{forbidden}`"),
        )?;
    }
    Ok(())
}

fn require_bool_const(schema: &Value, pointer: &str, expected: bool, label: &str) -> TestResult {
    ensure(
        schema.pointer(pointer).and_then(Value::as_bool) == Some(expected),
        format!("{label} must stay const {expected}"),
    )
}

#[test]
fn swarm_slo_scorecard_schema_identity_and_top_level_required_fields_are_pinned() -> TestResult {
    let schema = load_schema()?;
    require_schema_identity(&schema)?;
    require_exact_strings(
        &schema,
        "/required",
        REQUIRED_TOP_LEVEL,
        "scorecard top-level required fields",
    )?;
    require_example_fields(&schema, REQUIRED_TOP_LEVEL)?;
    require_no_raw_sensitive_example_values(&schema)
}

#[test]
fn swarm_slo_scorecard_schema_nested_required_fields_are_pinned() -> TestResult {
    let schema = load_schema()?;
    for (pointer, expected, label) in [
        (
            "/$defs/workload/required",
            REQUIRED_WORKLOAD,
            "scorecard workload required fields",
        ),
        (
            "/$defs/concurrency/required",
            REQUIRED_CONCURRENCY,
            "scorecard concurrency required fields",
        ),
        (
            "/$defs/commandCount/required",
            REQUIRED_COMMAND_COUNT,
            "scorecard command count required fields",
        ),
        (
            "/$defs/provenance/required",
            REQUIRED_PROVENANCE,
            "scorecard provenance required fields",
        ),
        (
            "/$defs/sourceHealth/required",
            REQUIRED_SOURCE_HEALTH,
            "scorecard source health required fields",
        ),
        (
            "/$defs/sourcePosture/required",
            REQUIRED_SOURCE_POSTURE,
            "scorecard source posture required fields",
        ),
        (
            "/$defs/budgets/required",
            REQUIRED_BUDGETS,
            "scorecard budgets required fields",
        ),
        (
            "/$defs/stageBudget/required",
            REQUIRED_STAGE_BUDGET,
            "scorecard stage budget required fields",
        ),
        (
            "/$defs/measurements/required",
            REQUIRED_MEASUREMENTS,
            "scorecard measurements required fields",
        ),
        (
            "/$defs/latency/required",
            REQUIRED_LATENCY,
            "scorecard latency required fields",
        ),
        (
            "/$defs/stageAttribution/required",
            REQUIRED_STAGE_ATTRIBUTION,
            "scorecard stage attribution required fields",
        ),
        (
            "/$defs/verdict/required",
            REQUIRED_VERDICT,
            "scorecard verdict required fields",
        ),
        (
            "/$defs/failureReason/required",
            REQUIRED_FAILURE_REASON,
            "scorecard failure reason required fields",
        ),
        (
            "/$defs/budgetVerdict/required",
            REQUIRED_BUDGET_VERDICT,
            "scorecard budget verdict required fields",
        ),
        (
            "/$defs/regressionReason/required",
            REQUIRED_REGRESSION_REASON,
            "scorecard regression reason required fields",
        ),
        (
            "/$defs/determinism/required",
            REQUIRED_DETERMINISM,
            "scorecard determinism required fields",
        ),
        (
            "/$defs/redaction/required",
            REQUIRED_REDACTION,
            "scorecard redaction required fields",
        ),
    ] {
        require_exact_strings(&schema, pointer, expected, label)?;
    }
    Ok(())
}

#[test]
fn swarm_slo_scorecard_schema_enum_vocabularies_are_pinned() -> TestResult {
    let schema = load_schema()?;
    for (pointer, expected, label) in [
        (
            "/$defs/workload/properties/scenario/enum",
            SCENARIOS,
            "scorecard scenario enum",
        ),
        (
            "/$defs/workload/properties/expectedDegradationPosture/enum",
            DEGRADATION_POSTURES,
            "scorecard expected degradation posture enum",
        ),
        (
            "/$defs/concurrency/properties/shape/enum",
            CONCURRENCY_SHAPES,
            "scorecard concurrency shape enum",
        ),
        (
            "/$defs/provenance/properties/kind/enum",
            PROVENANCE_KINDS,
            "scorecard provenance kind enum",
        ),
        (
            "/$defs/sourcePosture/properties/status/enum",
            SOURCE_STATUSES,
            "scorecard source status enum",
        ),
        (
            "/$defs/budgets/properties/profile/enum",
            BUDGET_PROFILES,
            "scorecard budget profile enum",
        ),
        ("/$defs/stage/enum", STAGES, "scorecard stage enum"),
        (
            "/$defs/stageAttribution/properties/verdict/enum",
            RESULT_STATUSES,
            "scorecard stage verdict enum",
        ),
        (
            "/$defs/verdict/properties/status/enum",
            RESULT_STATUSES,
            "scorecard verdict status enum",
        ),
        (
            "/$defs/failureReason/properties/severity/enum",
            SEVERITIES,
            "scorecard failure severity enum",
        ),
        (
            "/$defs/failureReason/properties/source/enum",
            FAILURE_SOURCES,
            "scorecard failure source enum",
        ),
        (
            "/$defs/budgetVerdict/properties/category/enum",
            BUDGET_CATEGORIES,
            "scorecard budget category enum",
        ),
        (
            "/$defs/budgetVerdict/properties/unit/enum",
            BUDGET_UNITS,
            "scorecard budget unit enum",
        ),
        (
            "/$defs/budgetVerdict/properties/status/enum",
            RESULT_STATUSES,
            "scorecard budget verdict status enum",
        ),
        (
            "/$defs/regressionReason/properties/code/enum",
            REGRESSION_CODES,
            "scorecard regression code enum",
        ),
        (
            "/$defs/regressionReason/properties/severity/enum",
            SEVERITIES,
            "scorecard regression severity enum",
        ),
    ] {
        require_exact_strings(&schema, pointer, expected, label)?;
    }
    Ok(())
}

#[test]
fn swarm_slo_scorecard_schema_redaction_booleans_are_safe_consts() -> TestResult {
    let schema = load_schema()?;
    for field in [
        "rawMailBodiesPresent",
        "rawMemoryBodiesPresent",
        "rawCommandOutputPresent",
        "privatePathsPresent",
    ] {
        require_bool_const(
            &schema,
            &format!("/$defs/redaction/properties/{field}/const"),
            false,
            &format!("scorecard redaction {field}"),
        )?;
    }
    require_bool_const(
        &schema,
        "/$defs/redaction/properties/secretScanApplied/const",
        true,
        "scorecard redaction secretScanApplied",
    )
}
