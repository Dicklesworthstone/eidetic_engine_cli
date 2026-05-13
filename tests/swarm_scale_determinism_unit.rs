//! S7 swarm-scale performance and determinism gates.
//!
//! The normal unit path validates the full gate matrix and materializes
//! deterministic measurement events without executing the heavy 10k/100k CLI
//! workloads. The shell E2E script is the opt-in execution hook.

use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;

use serde_json::{Value, json};

const BUDGETS_TEXT: &str = include_str!("swarm_scale_budgets.toml");
const CORPUS_MANIFEST_TEXT: &str = include_str!("fixtures/swarm_scale/corpus_manifest.json");
const EXPECTED_SCHEMA: &str = "ee.swarm_scale.budgets.v1";
const EXPECTED_EVENT_SCHEMA: &str = "ee.perf.v1";
const EXPECTED_EVENT_KIND: &str = "swarm_scale_measurement";
const EXPECTED_RUNS: u64 = 3;
const EXPECTED_OPS: &[&str] = &[
    "ee_init",
    "ee_remember",
    "ee_search",
    "ee_context",
    "ee_index_rebuild",
    "ee_why",
    "ee_export",
    "ee_handoff_create",
    "ee_graph_centrality_refresh",
];
const EXPECTED_SCALES: &[&str] = &["1k", "10k", "100k"];
const VOLATILE_FIELDS: &[&str] = &[
    "generatedAt",
    "generated_at",
    "elapsedMs",
    "elapsed_ms",
    "startedAt",
    "started_at",
    "endedAt",
    "ended_at",
    "ts",
    "timestamp",
    "runIndex",
    "run_index",
    "ee_binary_hash",
    "databasePath",
    "workspacePath",
    "indexDir",
];

type TestResult<T = ()> = Result<T, String>;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn budgets() -> TestResult<toml_edit::DocumentMut> {
    BUDGETS_TEXT
        .parse::<toml_edit::DocumentMut>()
        .map_err(|error| format!("budget TOML failed: {error}"))
}

fn corpus_manifest() -> TestResult<Value> {
    serde_json::from_str(CORPUS_MANIFEST_TEXT)
        .map_err(|error| format!("corpus manifest JSON failed: {error}"))
}

fn table<'a>(value: &'a toml_edit::DocumentMut, key: &str) -> TestResult<&'a toml_edit::Table> {
    value
        .get(key)
        .and_then(toml_edit::Item::as_table)
        .ok_or_else(|| format!("missing TOML table `{key}`"))
}

fn nested_table<'a>(
    table: &'a toml_edit::Table,
    key: &str,
    context: &str,
) -> TestResult<&'a toml_edit::Table> {
    table
        .get(key)
        .and_then(toml_edit::Item::as_table)
        .ok_or_else(|| format!("{context} missing table `{key}`"))
}

fn string_value<'a>(table: &'a toml_edit::Table, key: &str, context: &str) -> TestResult<&'a str> {
    table
        .get(key)
        .and_then(toml_edit::Item::as_str)
        .ok_or_else(|| format!("{context} missing string `{key}`"))
}

fn integer_value(table: &toml_edit::Table, key: &str, context: &str) -> TestResult<i64> {
    table
        .get(key)
        .and_then(toml_edit::Item::as_integer)
        .ok_or_else(|| format!("{context} missing integer `{key}`"))
}

fn bool_value(table: &toml_edit::Table, key: &str, context: &str) -> TestResult<bool> {
    table
        .get(key)
        .and_then(toml_edit::Item::as_bool)
        .ok_or_else(|| format!("{context} missing boolean `{key}`"))
}

fn ensure(condition: bool, context: &str) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(context.to_owned())
    }
}

fn ensure_eq<T: std::fmt::Debug + PartialEq>(actual: T, expected: T, context: &str) -> TestResult {
    if actual == expected {
        Ok(())
    } else {
        Err(format!("{context}: expected {expected:?}, got {actual:?}"))
    }
}

fn scale_from_manifest<'a>(manifest: &'a Value, scale: &str) -> TestResult<&'a Value> {
    let fixture_scale = match scale {
        "1k" => "smoke_1k",
        "10k" => "mid_10k",
        "100k" => "large_100k",
        _ => return Err(format!("unknown scale `{scale}`")),
    };
    manifest
        .get("scales")
        .and_then(Value::as_array)
        .and_then(|items| {
            items
                .iter()
                .find(|item| item.get("name").and_then(Value::as_str) == Some(fixture_scale))
        })
        .ok_or_else(|| format!("corpus manifest missing fixture scale `{fixture_scale}`"))
}

fn event_for(
    operation: &str,
    scale: &str,
    run_index: u64,
    budget_ms: u64,
    dominant_stage: &str,
) -> Value {
    json!({
        "schema": EXPECTED_EVENT_SCHEMA,
        "kind": EXPECTED_EVENT_KIND,
        "operation": operation,
        "scale": scale,
        "runIndex": run_index,
        "generatedAt": format!("2026-05-13T00:00:0{run_index}Z"),
        "elapsedMs": budget_ms.saturating_mul(3) / 5,
        "budgetMs": budget_ms,
        "withinBudget": true,
        "outputHash": stable_hash(&json!({
            "operation": operation,
            "scale": scale,
            "stable": true,
        })),
        "deterministic": true,
        "memoryBytesPeak": budget_ms.saturating_mul(1024),
        "dominantStage": dominant_stage,
        "degradationCodes": [],
        "ee_binary_hash": format!("sha256:volatile-build-{run_index}"),
    })
}

fn stable_hash(value: &Value) -> String {
    let bytes = serde_json::to_vec(value).unwrap_or_default();
    format!("blake3:{}", blake3::hash(&bytes).to_hex())
}

fn normalize_volatile(value: &mut Value) {
    match value {
        Value::Object(object) => {
            object.retain(|key, _| !VOLATILE_FIELDS.contains(&key.as_str()));
            for child in object.values_mut() {
                normalize_volatile(child);
            }
        }
        Value::Array(items) => {
            for item in items {
                normalize_volatile(item);
            }
        }
        _ => {}
    }
}

fn normalized_hash(value: &Value) -> String {
    let mut normalized = value.clone();
    normalize_volatile(&mut normalized);
    stable_hash(&normalized)
}

#[test]
fn swarm_scale_budget_manifest_covers_all_scales_and_operations() -> TestResult {
    let budgets = budgets()?;
    let manifest = corpus_manifest()?;
    let meta = table(&budgets, "meta")?;
    ensure_eq(
        string_value(meta, "schema", "meta")?,
        EXPECTED_SCHEMA,
        "budget schema",
    )?;
    ensure_eq(
        string_value(meta, "event_schema", "meta")?,
        EXPECTED_EVENT_SCHEMA,
        "event schema",
    )?;
    ensure_eq(
        string_value(meta, "event_kind", "meta")?,
        EXPECTED_EVENT_KIND,
        "event kind",
    )?;
    ensure_eq(
        integer_value(meta, "runs_per_operation", "meta")?,
        i64::try_from(EXPECTED_RUNS).map_err(|error| error.to_string())?,
        "runs per operation",
    )?;

    let scales = table(&budgets, "scales")?;
    let actual_scales = scales
        .iter()
        .map(|(key, _)| key.to_owned())
        .collect::<BTreeSet<_>>();
    ensure_eq(
        actual_scales,
        EXPECTED_SCALES
            .iter()
            .map(|scale| (*scale).to_owned())
            .collect::<BTreeSet<_>>(),
        "scale inventory",
    )?;
    for scale_name in EXPECTED_SCALES {
        let scale_budget = nested_table(scales, scale_name, "scales")?;
        let fixture = scale_from_manifest(&manifest, scale_name)?;
        ensure_eq(
            u64::try_from(integer_value(scale_budget, "memory_count", scale_name)?)
                .map_err(|error| error.to_string())?,
            fixture
                .get("memoryCount")
                .and_then(Value::as_u64)
                .ok_or_else(|| format!("fixture scale {scale_name} missing memoryCount"))?,
            "memory count",
        )?;
        ensure_eq(
            u64::try_from(integer_value(scale_budget, "agent_count", scale_name)?)
                .map_err(|error| error.to_string())?,
            fixture
                .get("agentCount")
                .and_then(Value::as_u64)
                .ok_or_else(|| format!("fixture scale {scale_name} missing agentCount"))?,
            "agent count",
        )?;
    }

    let ops = table(&budgets, "operations")?;
    let actual_ops = ops
        .iter()
        .map(|(key, _)| key.to_owned())
        .collect::<BTreeSet<_>>();
    ensure_eq(
        actual_ops,
        EXPECTED_OPS
            .iter()
            .map(|op| (*op).to_owned())
            .collect::<BTreeSet<_>>(),
        "operation inventory",
    )?;
    for operation in EXPECTED_OPS {
        let op = nested_table(ops, operation, "operations")?;
        ensure(
            bool_value(op, "determinism_required", operation)?,
            "determinism must be required for every operation",
        )?;
        let budgets_by_scale = nested_table(op, "budgets", operation)?;
        for scale_name in EXPECTED_SCALES {
            let row = nested_table(budgets_by_scale, scale_name, operation)?;
            ensure(
                integer_value(row, "budget_ms", scale_name)? > 0,
                "budget_ms must be positive",
            )?;
            ensure(
                integer_value(row, "tolerance_pct", scale_name)? > 0,
                "tolerance_pct must be positive",
            )?;
        }
    }
    Ok(())
}

#[test]
fn smoke_scale_is_normal_ci_and_large_scales_are_opt_in() -> TestResult {
    let budgets = budgets()?;
    let scales = table(&budgets, "scales")?;
    let one_k = nested_table(scales, "1k", "scales")?;
    let ten_k = nested_table(scales, "10k", "scales")?;
    let hundred_k = nested_table(scales, "100k", "scales")?;
    ensure(
        bool_value(one_k, "normal_verification", "1k")?,
        "1k must run in normal verification",
    )?;
    ensure(
        !bool_value(one_k, "explicit_benchmark", "1k")?,
        "1k must not require explicit benchmark mode",
    )?;
    for (name, scale) in [("10k", ten_k), ("100k", hundred_k)] {
        ensure(
            !bool_value(scale, "normal_verification", name)?,
            "large scales must stay out of normal verification",
        )?;
        ensure(
            bool_value(scale, "explicit_benchmark", name)?,
            "large scales must be opt-in",
        )?;
    }
    Ok(())
}

#[test]
fn measurement_events_are_deterministic_after_volatile_normalization() -> TestResult {
    let budgets = budgets()?;
    let ops = table(&budgets, "operations")?;
    for operation in EXPECTED_OPS {
        let op = nested_table(ops, operation, "operations")?;
        let dominant_stage = string_value(op, "dominant_stage", operation)?;
        let by_scale = nested_table(op, "budgets", operation)?;
        for scale_name in EXPECTED_SCALES {
            let row = nested_table(by_scale, scale_name, operation)?;
            let budget_ms = u64::try_from(integer_value(row, "budget_ms", scale_name)?)
                .map_err(|error| error.to_string())?;
            let mut hashes = BTreeSet::new();
            for run_index in 1..=EXPECTED_RUNS {
                let event = event_for(operation, scale_name, run_index, budget_ms, dominant_stage);
                validate_measurement_event(&event, operation, scale_name)?;
                hashes.insert(normalized_hash(&event));
            }
            ensure_eq(
                hashes.len(),
                1,
                &format!("{operation}/{scale_name} must normalize to one stable hash"),
            )?;
        }
    }
    Ok(())
}

fn validate_measurement_event(event: &Value, operation: &str, scale: &str) -> TestResult {
    ensure_eq(
        event.get("schema").and_then(Value::as_str),
        Some(EXPECTED_EVENT_SCHEMA),
        "measurement schema",
    )?;
    ensure_eq(
        event.get("kind").and_then(Value::as_str),
        Some(EXPECTED_EVENT_KIND),
        "measurement kind",
    )?;
    ensure_eq(
        event.get("operation").and_then(Value::as_str),
        Some(operation),
        "measurement operation",
    )?;
    ensure_eq(
        event.get("scale").and_then(Value::as_str),
        Some(scale),
        "measurement scale",
    )?;
    ensure(
        event
            .get("outputHash")
            .and_then(Value::as_str)
            .is_some_and(|hash| hash.starts_with("blake3:")),
        "measurement outputHash must be a blake3 hash",
    )?;
    ensure(
        event
            .get("withinBudget")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        "synthetic measurement must be within budget",
    )?;
    ensure(
        event
            .get("deterministic")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        "synthetic measurement must be deterministic",
    )
}

#[test]
fn e2e_swarm_scale_script_emits_required_event_fields() -> TestResult {
    let source = fs::read_to_string(repo_root().join("scripts/e2e_overhaul/swarm_scale.sh"))
        .map_err(|error| format!("read swarm_scale.sh: {error}"))?;
    for required in [
        "EE_SWARM_BENCH",
        EXPECTED_EVENT_SCHEMA,
        EXPECTED_EVENT_KIND,
        "operation",
        "elapsedMs",
        "budgetMs",
        "withinBudget",
        "outputHash",
        "deterministic",
        "memoryBytesPeak",
        "dominantStage",
    ] {
        ensure(
            source.contains(required),
            &format!("swarm_scale.sh missing `{required}`"),
        )?;
    }
    Ok(())
}

#[test]
fn failure_mode_fixtures_exist_for_s7_codes() -> TestResult {
    let driver = fs::read_to_string(repo_root().join("scripts/e2e_overhaul/failure_modes.sh"))
        .map_err(|error| error.to_string())?;
    let budget = fs::read_to_string(
        repo_root()
            .join("tests/fixtures/failure_modes")
            .join("swarm_scale_budget_exceeded.json"),
    )
    .map_err(|error| error.to_string())?;
    let nondeterminism = fs::read_to_string(
        repo_root()
            .join("tests/fixtures/failure_modes")
            .join("swarm_scale_nondeterminism.json"),
    )
    .map_err(|error| error.to_string())?;
    for (name, text) in [
        ("swarm_scale_budget_exceeded", budget),
        ("swarm_scale_nondeterminism", nondeterminism),
    ] {
        ensure(
            driver.contains(&format!("{name})")),
            &format!("failure-mode driver missing branch for {name}"),
        )?;
        let fixture: Value = serde_json::from_str(&text).map_err(|error| error.to_string())?;
        ensure_eq(
            fixture.get("code").and_then(Value::as_str),
            Some(name),
            "fixture code",
        )?;
        ensure_eq(
            fixture
                .pointer("/introduced_by/bead")
                .and_then(Value::as_str),
            Some("bd-1zb7k.8"),
            "fixture bead",
        )?;
    }
    Ok(())
}

#[test]
fn volatile_registry_mentions_fields_used_by_scale_gate() -> TestResult {
    let registry = fs::read_to_string(repo_root().join("docs/volatile_field_registry.md"))
        .map_err(|error| error.to_string())?;
    for field in ["generatedAt", "elapsedMs", "runIndex", "ee_binary_hash"] {
        ensure(
            registry.contains(field),
            &format!("volatile registry must mention {field}"),
        )?;
    }
    Ok(())
}
