use std::process::{Command, Output};

use ee::models::ProcessExitCode;
use serde_json::{Value, json};

type TestResult = Result<(), String>;

const EXPECTED_FIXTURE_IDS: &[&str] = &[
    "ask_v1",
    "fx.async_migration.v1",
    "fx.bundled_embeddings.v1",
    "fx.dangerous_cleanup.v1",
    "fx.data_size_tiers.v1",
    "fx.memory_poisoning.v1",
    "fx.metamorphic_evaluation.v1",
    "fx.release_failure.v1",
    "fx.semantic_model_admissibility.v1",
    "fx.structural_recall.v1",
];

const GOLDEN_REPORTS: &[(&str, ProcessExitCode, &str)] = &[
    (
        "fx.async_migration.v1",
        ProcessExitCode::Success,
        include_str!("fixtures/golden/eval/fx.async_migration.v1/report.json.golden"),
    ),
    (
        "fx.dangerous_cleanup.v1",
        ProcessExitCode::Success,
        include_str!("fixtures/golden/eval/fx.dangerous_cleanup.v1/report.json.golden"),
    ),
    (
        "fx.release_failure.v1",
        ProcessExitCode::EvalFailure,
        include_str!("fixtures/golden/eval/fx.release_failure.v1/report.json.golden"),
    ),
];

fn run_ee(args: &[&str]) -> Result<Output, String> {
    Command::new(env!("CARGO_BIN_EXE_ee"))
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .args(args)
        .output()
        .map_err(|error| format!("failed to run ee {}: {error}", args.join(" ")))
}

fn command_json(args: &[&str]) -> Result<Value, String> {
    command_json_with_exit(args, ProcessExitCode::Success)
}

fn command_json_with_exit(args: &[&str], expected_exit: ProcessExitCode) -> Result<Value, String> {
    let output = run_ee(args)?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    if output.status.code() != Some(expected_exit as i32) {
        return Err(format!(
            "ee {} exited with status {:?}, expected {:?}\nstdout:\n{}\nstderr:\n{}",
            args.join(" "),
            output.status.code(),
            expected_exit,
            stdout,
            stderr
        ));
    }

    if !stderr.trim().is_empty() {
        return Err(format!(
            "ee {} wrote stderr during JSON success:\n{}",
            args.join(" "),
            stderr
        ));
    }

    serde_json::from_str(&stdout)
        .map_err(|error| format!("failed to parse JSON from ee {}: {error}", args.join(" ")))
}

fn ensure_equal<T>(actual: &T, expected: &T, context: &str) -> TestResult
where
    T: std::fmt::Debug + PartialEq,
{
    if actual == expected {
        Ok(())
    } else {
        Err(format!("{context}: expected {expected:?}, got {actual:?}"))
    }
}

fn string_field<'a>(value: &'a Value, pointer: &str) -> Result<&'a str, String> {
    value
        .pointer(pointer)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("missing string field at {pointer}"))
}

fn fixture_ids(value: &Value) -> Result<Vec<&str>, String> {
    let fixtures = value
        .pointer("/data/fixtures")
        .and_then(Value::as_array)
        .ok_or_else(|| "missing /data/fixtures array".to_string())?;

    fixtures
        .iter()
        .map(|fixture| {
            fixture
                .get("fixture_id")
                .and_then(Value::as_str)
                .ok_or_else(|| format!("fixture entry missing fixture_id: {fixture:?}"))
        })
        .collect()
}

fn report_value(value: &Value) -> Result<Value, String> {
    value
        .pointer("/data/report")
        .cloned()
        .ok_or_else(|| "missing /data/report object".to_string())
}

fn normalized_response_for_stability(mut value: Value) -> Result<String, String> {
    let report = value
        .pointer_mut("/data/report")
        .ok_or_else(|| "missing /data/report object".to_string())?;
    report["duration_ms"] = json!("[duration_ms]");
    serde_json::to_string(&value).map_err(|error| format!("serialize stability JSON: {error}"))
}

fn normalized_report_for_golden(value: &Value) -> Result<String, String> {
    let mut report = report_value(value)?;

    ensure_equal(
        &string_field(&report, "/schema")?,
        &"ee.eval.report.v1",
        "report schema",
    )?;

    report["duration_ms"] = json!("[duration_ms]");
    report["data_hash"] = json!("[data_hash]");

    let per_query = report
        .pointer_mut("/metrics/per_query")
        .and_then(Value::as_array_mut)
        .ok_or_else(|| "missing /metrics/per_query array".to_string())?;
    per_query.sort_by(|left, right| {
        let left_query = left
            .get("query")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let right_query = right
            .get("query")
            .and_then(Value::as_str)
            .unwrap_or_default();
        left_query.cmp(right_query)
    });

    let mut output = serde_json::to_string_pretty(&report)
        .map_err(|error| format!("serialize report golden JSON: {error}"))?;
    output.push('\n');
    Ok(output)
}

fn stable_data_hash(value: &Value) -> Result<&str, String> {
    let data_hash = string_field(value, "/data/report/data_hash")?;
    let is_hex = data_hash
        .strip_prefix("blake3:")
        .is_some_and(|digest| digest.len() == 64 && digest.chars().all(|c| c.is_ascii_hexdigit()));
    if is_hex {
        Ok(data_hash)
    } else {
        Err(format!(
            "data_hash is not a full BLAKE3 digest: {data_hash}"
        ))
    }
}

#[test]
fn eval_list_json_enumerates_all_fixture_directories() -> TestResult {
    let value = command_json(&["--json", "eval", "list"])?;

    ensure_equal(
        &string_field(&value, "/schema")?,
        &"ee.response.v2",
        "response schema",
    )?;
    ensure_equal(
        &value.pointer("/success").and_then(Value::as_bool),
        &Some(true),
        "success",
    )?;
    ensure_equal(
        &string_field(&value, "/data/command")?,
        &"eval list",
        "command",
    )?;
    ensure_equal(
        &value.pointer("/data/fixtureCount").and_then(Value::as_u64),
        &Some(EXPECTED_FIXTURE_IDS.len() as u64),
        "fixtureCount",
    )?;

    let actual_ids = fixture_ids(&value)?;
    let expected_ids = EXPECTED_FIXTURE_IDS.to_vec();
    ensure_equal(&actual_ids, &expected_ids, "fixture IDs")?;

    let fixtures = value
        .pointer("/data/fixtures")
        .and_then(Value::as_array)
        .ok_or_else(|| "missing fixture inventory".to_string())?;
    for (fixture_id, memory_count, query_count) in [
        ("ask_v1", 61, 8),
        ("fx.async_migration.v1", 3, 3),
        ("fx.data_size_tiers.v1", 600, 9),
    ] {
        let fixture = fixtures
            .iter()
            .find(|fixture| fixture["fixture_id"].as_str() == Some(fixture_id))
            .ok_or_else(|| format!("missing fixture {fixture_id}"))?;
        ensure_equal(
            &fixture["memory_count"].as_u64(),
            &Some(memory_count),
            &format!("{fixture_id} materialized memory count"),
        )?;
        ensure_equal(
            &fixture["query_count"].as_u64(),
            &Some(query_count),
            &format!("{fixture_id} materialized query count"),
        )?;
    }
    Ok(())
}

#[test]
fn eval_run_reports_are_stable_and_match_golden_snapshots() -> TestResult {
    for &(fixture_id, expected_exit, golden) in GOLDEN_REPORTS {
        let first = command_json_with_exit(&["--json", "eval", "run", fixture_id], expected_exit)?;
        let second = command_json_with_exit(&["--json", "eval", "run", fixture_id], expected_exit)?;

        ensure_equal(
            &stable_data_hash(&first)?,
            &stable_data_hash(&second)?,
            &format!("{fixture_id} data_hash stability"),
        )?;
        ensure_equal(
            &normalized_response_for_stability(first.clone())?,
            &normalized_response_for_stability(second)?,
            &format!("{fixture_id} JSON stability"),
        )?;

        let actual = normalized_report_for_golden(&first)?;
        ensure_equal(
            &actual.as_str(),
            &golden,
            &format!("{fixture_id} golden report"),
        )?;
    }

    Ok(())
}

#[test]
fn eval_async_migration_retrieves_the_exact_complete_query_inventory() -> TestResult {
    let value = command_json(&["--json", "eval", "run", "fx.async_migration.v1"])?;
    let metrics = &value["data"]["report"]["metrics"];
    ensure_equal(
        &metrics["queries_evaluated"],
        &json!(3),
        "async query count",
    )?;
    let queries = metrics["per_query"]
        .as_array()
        .ok_or_else(|| "missing async query results".to_owned())?;
    ensure_equal(&queries.len(), &3, "async per-query result count")?;
    for (query, (expected_query, expected_id)) in queries.iter().zip([
        ("background job queue health and capacity", "memory-1"),
        (
            "production backfill timeout and stuck migration",
            "memory-2",
        ),
        (
            "rollback plan and pre-migration schema checkpoint",
            "memory-3",
        ),
    ]) {
        ensure_equal(&query["query"], &json!(expected_query), "exact async query")?;
        ensure_equal(
            &query["expected_ids"],
            &json!([expected_id]),
            "exact async expected entity",
        )?;
        ensure_equal(
            &query["retrieved_ids"][0],
            &json!(expected_id),
            "async top result is the required source memory",
        )?;
        ensure_equal(&query["precision_at_1"], &json!(1.0), "async P@1")?;
        ensure_equal(&query["recall_at_5"], &json!(1.0), "async recall@5")?;
    }
    Ok(())
}

#[test]
fn eval_run_distinguishes_non_retrieval_contracts_from_poor_retrieval() -> TestResult {
    // These families describe model admission and metamorphic transitions;
    // neither declares a retrieval query. They cannot count as quality runs.
    for fixture in [
        "fx.semantic_model_admissibility.v1",
        "fx.metamorphic_evaluation.v1",
    ] {
        let value = command_json_with_exit(
            &["--json", "eval", "run", fixture],
            ProcessExitCode::Configuration,
        )?;
        ensure_equal(
            &value["data"]["report"]["status"],
            &json!("error"),
            "unconfigured retrieval is an error",
        )?;
        ensure_equal(
            &value["data"]["report"]["metrics"]["queries_evaluated"],
            &json!(0),
            "non-retrieval contract cannot invent query measurements",
        )?;
    }
    Ok(())
}

#[test]
fn eval_run_executes_every_ask_quality_case_and_rejects_wrong_citations() -> TestResult {
    let value = command_json(&["--json", "eval", "run", "ask_v1"])?;
    let quality = value
        .pointer("/data/report/ask_quality")
        .ok_or_else(|| "eval run omitted the actual ask-quality report".to_owned())?;
    ensure_equal(&quality["cases_total"], &json!(7), "all ask cases executed")?;
    ensure_equal(&quality["cases_within"], &json!(7), "all ask cases passed")?;
    let comparisons = quality["comparisons"]
        .as_array()
        .ok_or_else(|| "missing ask comparisons".to_owned())?;
    let mut ids = comparisons
        .iter()
        .map(|case| string_field(case, "/case_id"))
        .collect::<Result<Vec<_>, _>>()?;
    ids.sort();
    ensure_equal(
        &ids,
        &vec![
            "ask.conflict_remote_cache",
            "ask.direct_toolchain",
            "ask.lexical_release_tag",
            "ask.multi_release_gate",
            "ask.unanswerable_lunar_invoice",
            "ask.unrelated_dashboard",
            "ask.version_current_retention",
        ],
        "exact ask case inventory",
    )?;
    let direct = comparisons
        .iter()
        .find(|case| case["case_id"] == "ask.lexical_release_tag")
        .ok_or_else(|| "missing lexical release-tag result".to_owned())?;
    ensure_equal(
        &direct["actual_cited_memory_ids"],
        &json!(["mem_ask_release_tag_format"]),
        "real lexical answer citation",
    )?;
    ensure_equal(
        &direct["actual_abstained"],
        &json!(false),
        "answer-bearing span must answer",
    )?;

    // Preserve the actual corpus and question; plant a wrong, existing entity
    // in the oracle. A scorer wired to declared answers instead of real engine
    // output would incorrectly pass this mutation.
    let root = tempfile::Builder::new()
        .prefix("ee-ask-quality-negative-")
        .tempdir()
        .map_err(|error| error.to_string())?
        .keep();
    let fixture = root.join("ask_v1");
    std::fs::create_dir(&fixture).map_err(|error| error.to_string())?;
    std::fs::copy(
        "tests/fixtures/eval/ask_v1/source_memory.json",
        fixture.join("source_memory.json"),
    )
    .map_err(|error| error.to_string())?;
    let mut scenario: Value =
        serde_json::from_str(include_str!("fixtures/eval/ask_v1/scenario.json"))
            .map_err(|error| error.to_string())?;
    let cases = scenario["ask_quality_expectations"]["cases"]
        .as_array_mut()
        .ok_or_else(|| "missing scenario cases".to_owned())?;
    let direct_case = cases
        .iter_mut()
        .find(|case| case["case_id"] == "ask.lexical_release_tag")
        .ok_or_else(|| "missing direct case".to_owned())?;
    direct_case["expected_cited_memory_ids"] = json!(["mem_ask_unrelated_ui"]);
    std::fs::write(
        fixture.join("scenario.json"),
        serde_json::to_vec_pretty(&scenario).map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?;
    let negative = command_json_with_exit(
        &[
            "--json",
            "eval",
            "run",
            "ask_v1",
            "--fixture-dir",
            root.to_str()
                .ok_or_else(|| "non-UTF-8 fixture path".to_owned())?,
        ],
        ProcessExitCode::EvalFailure,
    )?;
    ensure_equal(
        &negative["data"]["report"]["status"],
        &json!("failed"),
        "wrong expected citation blocks public evaluator",
    )?;
    let negative_cases = negative["data"]["report"]["ask_quality"]["comparisons"]
        .as_array()
        .ok_or_else(|| "negative run omitted comparisons".to_owned())?;
    let rejected = negative_cases
        .iter()
        .find(|case| case["case_id"] == "ask.lexical_release_tag")
        .ok_or_else(|| "negative run omitted direct case".to_owned())?;
    ensure_equal(
        &rejected["actual_cited_memory_ids"],
        &direct["actual_cited_memory_ids"],
        "oracle mutation cannot change generated answer",
    )?;
    if rejected["verdict"] == "within" {
        return Err(format!("wrong-citation mutation passed: {rejected}"));
    }
    if negative["data"]["report"]["data_hash"] == value["data"]["report"]["data_hash"] {
        return Err("ask-quality regression did not change report data hash".to_owned());
    }
    Ok(())
}
