use std::collections::BTreeSet;
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

// Independent workload oracle: deleting an expectation from a source fixture
// must fail even if the evaluator faithfully executes the remaining queries.
//
// ITS VALUE DEPENDS ON THE TWO DECLARATIONS MOVING FOR INDEPENDENT REASONS. If
// every fixture edit reflexively drags this list along, the oracle degrades
// into a formality that always agrees and catches nothing -- which is how a
// guard dies without anyone deleting it.
//
// So when this disagrees with a fixture, that red is INFORMATION. Establish why
// the fixture changed and whether the change was justified on its own evidence
// BEFORE touching this list. Updating it is sometimes correct (see 543cf14ef,
// where four declared queries could never retrieve against a non-stemming index
// and were re-anchored); updating it reflexively never is.
const RETRIEVAL_WORKLOADS: &[(&str, &[&str])] = &[
    (
        "ask_v1",
        &[
            "How should Project Zephyr ask answers cite evidence?",
            "Is remote cache delta enabled for Project Zephyr?",
            "What Rust toolchain does Project Zephyr use?",
            "What is the current retention window for Project Zephyr traces?",
            "What was the old Project Zephyr trace retention window?",
            "Where does Project Zephyr store trace metadata?",
            "Which command must run before every release tag?",
            "Which release readiness gate must Project Zephyr pass before deploy?",
        ],
    ),
    (
        "fx.async_migration.v1",
        &[
            "background job queue health and capacity",
            "production backfill timeout and stuck migration",
            "rollback plan and pre-migration schema checkpoint",
        ],
    ),
    (
        "fx.bundled_embeddings.v1",
        &[
            "RBLX bookings FCF watchlist",
            "Roblox Robux creator marketplace",
        ],
    ),
    (
        "fx.dangerous_cleanup.v1",
        &[
            "before deleting",
            "clean up generated build artifacts safely",
            "cleanup failure",
            "dangerous cleanup",
            "dry-run",
            "explicit written permission",
            "preserve unknown files",
            "safe cleanup plan",
            "untracked fixtures",
        ],
    ),
    (
        "fx.data_size_tiers.v1",
        &[
            "budget truncation",
            "deterministic priority bucket",
            "large workspace history",
            "medium workspace history",
            "redundant memories suppressed",
            "release memory",
            "release note verification",
            "repeated checks",
            "small workspace history",
        ],
    ),
    (
        "fx.memory_poisoning.v1",
        &[
            "authority claim",
            "ignore previous instructions",
            "imported memories are evidence",
            "legacy memories",
            "new system prompt",
            "policy denial",
            "prompt injection",
            "role markup",
            "source trust",
        ],
    ),
    (
        "fx.release_failure.v1",
        &[
            "clippy before release",
            "failing release workflow",
            "prepare release",
            "release failure",
            "unused import",
        ],
    ),
    (
        "fx.structural_recall.v1",
        &[
            "RCH queue backpressure authority",
            "completion audit dominance alias",
            "fresh workspace init status",
            "local cargo fallback is acceptable when RCH is busy",
            "revision lineage aggregate dominance",
            "thin-thread concurrency release compass",
        ],
    ),
];

fn expected_corpus_ids(fixture_id: &str) -> Result<BTreeSet<String>, String> {
    let numeric_range = match fixture_id {
        "fx.bundled_embeddings.v1" => Some(901..=903),
        "fx.dangerous_cleanup.v1" => Some(301..=303),
        "fx.data_size_tiers.v1" => Some(501..=1100),
        "fx.memory_poisoning.v1" => Some(401..=403),
        "fx.release_failure.v1" => Some(101..=102),
        "fx.structural_recall.v1" => Some(1101..=1110),
        _ => None,
    };
    if let Some(range) = numeric_range {
        return Ok(range.map(|id| format!("mem_{id:026}")).collect());
    }
    // bd-90yt4. These two fixtures used to label their corpora `memory-1` and
    // `mem_ask_*`, which the memories table cannot store: src/db/mod.rs:3416
    // declares CHECK (id GLOB 'mem_*' AND length(id) = 30), preserved through
    // the V090 rebuild at :8480. They only ever "passed" because the evaluator
    // searched a store-less workspace, so nothing reconciled them against the
    // schema. Seeding a real store made the violation reachable and they now
    // carry storable IDs.
    //
    // The oracle keeps BUILDING ids from its own rule rather than importing the
    // fixture's, so it still fails when a fixture silently drops or renames a
    // record. Padding right with '0' is what keeps the names readable at a
    // glance while satisfying the exact-30 requirement.
    if fixture_id == "fx.async_migration.v1" {
        return Ok((1..=3)
            .map(|id| format!("mem_asyncmigration{id:012}"))
            .collect());
    }
    if fixture_id == "ask_v1" {
        let mut ids: BTreeSet<_> = [
            "releasetagformat",
            "directtoolchain",
            "directdatabase",
            "multireleaseprimary",
            "multireleasesupport",
            "conflictaffirm",
            "conflictnegate",
            "versioncurrent",
            "versionold",
            "boundaryrule",
            "lowtrustnoise",
            "unrelatedbilling",
            "unrelatedui",
        ]
        .into_iter()
        .map(|suffix| {
            let body = format!("ask{suffix}");
            format!("mem_{body:0<26}")
        })
        .collect();
        ids.extend((1..=48).map(|id| format!("mem_asknoise{id:018}")));
        return Ok(ids);
    }
    Err(format!("unclassified retrieval fixture {fixture_id}"))
}

fn check_source_workload(
    fixture_id: &str,
    expected_queries: &[&str],
    memories: &[ee::eval::SourceMemory],
) -> TestResult {
    let ids: BTreeSet<_> = memories.iter().map(|memory| memory.id.clone()).collect();
    ensure_equal(
        &ids,
        &expected_corpus_ids(fixture_id)?,
        "exact materialized corpus IDs",
    )?;
    ensure_equal(&ids.len(), &memories.len(), "no duplicate corpus IDs")?;
    let queries: BTreeSet<_> = memories
        .iter()
        .flat_map(|memory| memory.expected_query_match.iter().map(String::as_str))
        .collect();
    ensure_equal(
        &queries,
        &expected_queries.iter().copied().collect(),
        "exact declared query inventory",
    )
}

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
        &"ee.eval.report.v2",
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
        (
            "background job queue health and capacity",
            "mem_asyncmigration000000000001",
        ),
        (
            "production backfill timeout and stuck migration",
            "mem_asyncmigration000000000002",
        ),
        (
            "rollback plan and pre-migration schema checkpoint",
            "mem_asyncmigration000000000003",
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

/// Executes one retrieval family and records every empty retrieval it
/// observes. An `Err` means the family did not produce a complete
/// measurement; empty retrievals already pushed for it remain recorded.
fn execute_retrieval_workload(
    fixtures: &[ee::eval::DiscoveredFixture],
    fixture_id: &str,
    expected_queries: &[&str],
    artifacts: &std::path::Path,
    empty_retrievals: &mut Vec<String>,
) -> TestResult {
    let fixture = fixtures
        .iter()
        .find(|fixture| fixture.fixture_id == fixture_id)
        .ok_or_else(|| format!("missing fixture {fixture_id}"))?;
    let source = ee::eval::load_source_memories(&fixture.source_memory_path)
        .map_err(|error| error.to_string())?;
    let memories =
        ee::eval::materialize_source_memories(&source).map_err(|error| error.to_string())?;
    check_source_workload(fixture_id, expected_queries, &memories)?;
    let output = run_ee(&["--json", "eval", "run", fixture_id])?;
    std::fs::write(artifacts.join(format!("{fixture_id}.json")), &output.stdout)
        .map_err(|error| error.to_string())?;
    std::fs::write(
        artifacts.join(format!("{fixture_id}.stderr")),
        &output.stderr,
    )
    .map_err(|error| error.to_string())?;
    if !matches!(output.status.code(), Some(0 | 9)) {
        return Err(format!(
            "did not execute retrieval: {:?}\n{}\n{}",
            output.status.code(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    ensure_equal(&output.stderr, &Vec::<u8>::new(), "evaluation stderr")?;
    let value: Value = serde_json::from_slice(&output.stdout).map_err(|error| error.to_string())?;
    let metrics = &value["data"]["report"]["metrics"];
    ensure_equal(
        &metrics["queries_evaluated"],
        &json!(expected_queries.len()),
        "complete executed query count",
    )?;
    let queries = metrics["per_query"]
        .as_array()
        .ok_or("missing per-query measurements")?;
    ensure_equal(
        &queries.len(),
        &expected_queries.len(),
        "one result per declared query",
    )?;
    let actual_queries = queries
        .iter()
        .map(|query| string_field(query, "/query"))
        .collect::<Result<BTreeSet<_>, _>>()?;
    ensure_equal(
        &actual_queries,
        &expected_queries.iter().copied().collect(),
        "complete executed query inventory",
    )?;
    for query in queries {
        let text = string_field(query, "/query")?;
        let expected_ids: BTreeSet<_> = memories
            .iter()
            .filter(|memory| {
                memory
                    .expected_query_match
                    .iter()
                    .any(|expected| expected == text)
            })
            .map(|memory| memory.id.as_str())
            .collect();
        let observed_ids = query["expected_ids"]
            .as_array()
            .ok_or("missing expected IDs")?
            .iter()
            .map(|id| id.as_str().ok_or("non-string expected ID"))
            .collect::<Result<BTreeSet<_>, _>>()?;
        ensure_equal(
            &observed_ids,
            &expected_ids,
            "every declared expected entity is measured",
        )?;
        let retrieved = query["retrieved_ids"]
            .as_array()
            .ok_or("missing retrieved IDs")?;
        if retrieved.is_empty() {
            empty_retrievals.push(format!(
                "{fixture_id} executed an empty retrieval for {text:?}"
            ));
        }
    }
    Ok(())
}

#[test]
fn eval_all_retrieval_families_execute_their_complete_workloads() -> TestResult {
    let artifacts = tempfile::Builder::new()
        .prefix("ee-eval-workloads-")
        .tempdir()
        .map_err(|error| error.to_string())?
        .keep();
    let fixtures = ee::eval::discover_fixtures(std::path::Path::new("tests/fixtures/eval"))
        .map_err(|error| error.to_string())?;
    // One family failing must not hide another family's empty retrievals:
    // every family executes, and both lists are reported together.
    let mut empty_retrievals = Vec::new();
    let mut family_failures = Vec::new();
    for &(fixture_id, expected_queries) in RETRIEVAL_WORKLOADS {
        if let Err(error) = execute_retrieval_workload(
            &fixtures,
            fixture_id,
            expected_queries,
            &artifacts,
            &mut empty_retrievals,
        ) {
            family_failures.push(format!("{fixture_id}: {error}"));
        }
    }
    let classified: BTreeSet<_> = RETRIEVAL_WORKLOADS
        .iter()
        .map(|(id, _)| *id)
        .chain([
            "fx.metamorphic_evaluation.v1",
            "fx.semantic_model_admissibility.v1",
        ])
        .collect();
    if let Err(error) = ensure_equal(
        &classified,
        &EXPECTED_FIXTURE_IDS.iter().copied().collect(),
        "every fixture has an explicit execution classification",
    ) {
        family_failures.push(error);
    }
    if !family_failures.is_empty() || !empty_retrievals.is_empty() {
        return Err(format!(
            "{} family failure(s):\n{}\n{} empty retrieval(s):\n{}\ncomplete workload artifacts: {}",
            family_failures.len(),
            family_failures.join("\n"),
            empty_retrievals.len(),
            empty_retrievals.join("\n"),
            artifacts.display()
        ));
    }
    Ok(())
}

#[test]
fn pack_quality_executes_real_packs_independently_of_expected_answers() -> TestResult {
    let root = tempfile::Builder::new()
        .prefix("ee-pack-quality-controls-")
        .tempdir()
        .map_err(|error| error.to_string())?
        .keep();
    let root = root.canonicalize().map_err(|error| error.to_string())?;
    let fixture_dir = root.join("fixtures");
    std::fs::create_dir(&fixture_dir).map_err(|error| error.to_string())?;
    let fixture = fixture_dir.join("release_failure");
    std::fs::create_dir(&fixture).map_err(|error| error.to_string())?;
    std::fs::copy(
        "tests/fixtures/eval/release_failure/source_memory.json",
        fixture.join("source_memory.json"),
    )
    .map_err(|error| error.to_string())?;
    let mut scenario: Value =
        serde_json::from_str(include_str!("fixtures/eval/release_failure/scenario.json"))
            .map_err(|error| error.to_string())?;
    let baseline = scenario["pack_quality_expectations"]["cases"][0].clone();
    let mut wrong_oracle = baseline.clone();
    wrong_oracle["case_id"] = json!("pq.oracle_mutation");
    wrong_oracle["command_step"] = json!(5);
    wrong_oracle["expected_selected_memory_ids"] = json!(["mem_00000000000000000000000101"]);
    wrong_oracle["allowed_degradation_codes"] = json!(["unobserved_fixture_branch"]);
    let mut budget_pressure = baseline.clone();
    budget_pressure["case_id"] = json!("pq.budget_pressure");
    budget_pressure["command_step"] = json!(6);
    budget_pressure["token_budget"] =
        json!({"max_tokens":1,"expected_used_tokens_max":1,"expect_truncation":true});
    scenario["pack_quality_expectations"]["cases"] =
        json!([baseline, wrong_oracle, budget_pressure]);
    let steps = scenario["command_sequence"]
        .as_array_mut()
        .ok_or("missing steps")?;
    let pack_step = steps[3].clone();
    for step_number in [5, 6] {
        let mut step = pack_step.clone();
        step["step"] = json!(step_number);
        steps.push(step);
    }
    scenario["degraded_branches"].as_array_mut().ok_or("missing branches")?.push(json!({
        "code":"unobserved_fixture_branch", "description":"Planted declaration without a runtime trigger.",
        "preserves_success_signal":true
    }));
    let data_home = root.join("data");
    let global = Command::new(env!("CARGO_BIN_EXE_ee"))
        .current_dir(&root)
        .env("XDG_DATA_HOME", &data_home)
        .env("EE_EMBED_DOWNLOAD", "off")
        .args([
            "remember",
            "--global",
            "Prepare release using the unrelated global canary checklist.",
            "--level",
            "semantic",
            "--kind",
            "rule",
            "--json",
        ])
        .output()
        .map_err(|error| error.to_string())?;
    std::fs::write(root.join("global.stdout.json"), &global.stdout)
        .map_err(|error| error.to_string())?;
    std::fs::write(root.join("global.stderr.txt"), &global.stderr)
        .map_err(|error| error.to_string())?;
    ensure_equal(
        &global.status.code(),
        &Some(0),
        "real unrelated global memory planted",
    )?;
    let global_response: Value =
        serde_json::from_slice(&global.stdout).map_err(|error| error.to_string())?;
    let global_id = global_response
        .pointer("/data/memory_id")
        .and_then(Value::as_str)
        .ok_or("missing global canary ID")?;
    // Freeze this control after the canary exists: an April as-of timestamp
    // would exclude it by time and fail to exercise global-store isolation.
    scenario["deterministic"]["fixed_clock"] = json!(chrono::Utc::now().to_rfc3339());
    std::fs::write(
        fixture.join("scenario.json"),
        serde_json::to_vec_pretty(&scenario).map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?;
    let output = Command::new(env!("CARGO_BIN_EXE_ee"))
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .env("XDG_DATA_HOME", &data_home)
        .env("EE_EMBED_DOWNLOAD", "off")
        .args([
            "--json",
            "eval",
            "run",
            "fx.release_failure.v1",
            "--pack-quality",
            "--fixture-dir",
            fixture_dir.to_str().ok_or("non-UTF-8 fixture path")?,
        ])
        .output()
        .map_err(|error| error.to_string())?;
    std::fs::write(root.join("eval.stdout.json"), &output.stdout)
        .map_err(|error| error.to_string())?;
    std::fs::write(root.join("eval.stderr.txt"), &output.stderr)
        .map_err(|error| error.to_string())?;
    let value: Value = serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("{error}; evidence {}", root.display()))?;
    ensure_equal(
        &output.status.code(),
        &Some(ProcessExitCode::EvalFailure as i32),
        "wrong oracle and missing required budget items fail the public evaluator",
    )?;
    ensure_equal(
        &value["success"],
        &json!(false),
        "failed expectations cannot report success",
    )?;
    let comparisons = value
        .pointer("/data/report/comparisons")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            format!(
                "missing actual pack comparisons: {value}; evidence {}",
                root.display()
            )
        })?;
    ensure_equal(&comparisons.len(), &3, "all controls executed")?;
    let baseline = &comparisons[0];
    let wrong = &comparisons[1];
    let budget = &comparisons[2];
    let baseline_ids: BTreeSet<_> = baseline["actual_selected_ids"]
        .as_array()
        .ok_or("missing selected IDs")?
        .iter()
        .map(|id| id.as_str().ok_or("non-string ID"))
        .collect::<Result<_, _>>()?;
    if baseline_ids.contains(global_id) {
        return Err(format!(
            "unrelated global memory contaminated eval: {global_id}"
        ));
    }
    ensure_equal(
        &baseline_ids,
        &BTreeSet::from([
            "mem_00000000000000000000000101",
            "mem_00000000000000000000000102",
        ]),
        "positive pack retains both original release memories",
    )?;
    for field in [
        "actual_selected_ids",
        "actual_tokens_used",
        "provenance_density",
        "actual_degradation_codes",
        "actual_redaction_leaks",
    ] {
        ensure_equal(
            &baseline[field],
            &wrong[field],
            &format!("expected answers cannot affect {field}"),
        )?;
    }
    ensure_equal(
        &wrong["unexpected_ids"],
        &json!(["mem_00000000000000000000000102"]),
        "wrong oracle is detected",
    )?;
    ensure_equal(
        &wrong["verdict"],
        &json!("regression"),
        "wrong oracle fails",
    )?;
    ensure_equal(
        &budget["verdict"],
        &json!("regression"),
        "missing required memories fail",
    )?;
    ensure_equal(
        &budget["actual_selected_ids"],
        &json!([]),
        "one token cannot carry either full memory",
    )?;
    ensure_equal(
        &budget["actual_tokens_used"],
        &json!(0),
        "real empty pack token cost",
    )?;
    let branches = value["data"]["degradedBranches"]
        .as_array()
        .ok_or("missing branch reports")?;
    let phantom = branches
        .iter()
        .find(|entry| entry["code"] == "unobserved_fixture_branch")
        .ok_or("missing planted branch")?;
    ensure_equal(
        &phantom["executed"],
        &json!(false),
        "declaration is not execution",
    )?;
    ensure_equal(
        &phantom["observedCaseIds"],
        &json!([]),
        "no invented branch evidence",
    )?;
    let artifacts = value["data"]["artifactPaths"]
        .as_array()
        .ok_or("missing actual artifacts")?;
    ensure_equal(&artifacts.len(), &3, "one actual artifact per pack")?;
    for (artifact, comparison) in artifacts.iter().zip(comparisons) {
        let path = artifact["stdout"]
            .as_str()
            .ok_or("missing actual pack path")?;
        let pack: Value =
            serde_json::from_slice(&std::fs::read(path).map_err(|error| error.to_string())?)
                .map_err(|error| error.to_string())?;
        ensure_equal(
            &pack["schema"],
            &json!("ee.response.v2"),
            "actual production response envelope",
        )?;
        ensure_equal(
            &pack["data"]["pack"]["budget"]["usedTokens"],
            &comparison["actual_tokens_used"],
            "token cost comes from retained pack",
        )?;
        ensure_equal(
            &artifact["memoryCount"],
            &json!(2),
            "entire source corpus stored",
        )?;
    }
    Ok(())
}

#[test]
fn eval_workload_oracle_rejects_an_omitted_query_or_corpus_record() -> TestResult {
    let source: ee::eval::SourceMemoryFile = serde_json::from_str(include_str!(
        "fixtures/eval/async_migration/source_memory.json"
    ))
    .map_err(|error| error.to_string())?;
    let memories =
        ee::eval::materialize_source_memories(&source).map_err(|error| error.to_string())?;
    let (_, queries) = RETRIEVAL_WORKLOADS
        .iter()
        .find(|(id, _)| *id == "fx.async_migration.v1")
        .ok_or("missing independent async oracle")?;
    check_source_workload("fx.async_migration.v1", queries, &memories)?;
    let mut omitted = memories.clone();
    omitted[0].expected_query_match.clear();
    if check_source_workload("fx.async_migration.v1", queries, &omitted).is_ok() {
        return Err("omitted query passed the independent workload oracle".into());
    }
    if check_source_workload("fx.async_migration.v1", queries, &memories[1..]).is_ok() {
        return Err("omitted corpus record passed the independent workload oracle".into());
    }
    Ok(())
}

#[test]
fn eval_retrieval_is_independent_of_host_model_selection() -> TestResult {
    let baseline = command_json(&["--json", "eval", "run", "fx.async_migration.v1"])?;
    let scratch = tempfile::Builder::new()
        .prefix("ee-eval-host-model-")
        .tempdir()
        .map_err(|error| format!("create host-model fixture: {error}"))?
        .keep();
    let output = Command::new(env!("CARGO_BIN_EXE_ee"))
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .args(["--json", "eval", "run", "fx.async_migration.v1"])
        .env("EE_EMBED_DOWNLOAD", "auto")
        .env("EE_EMBED_MODEL_DIR", scratch.join("missing-host-model"))
        .env("FRANKENSEARCH_API_PROVIDER", "openai")
        .env("FRANKENSEARCH_API_MODEL", "ambient-model-must-not-select")
        .output()
        .map_err(|error| format!("run eval with hostile host-model selection: {error}"))?;
    std::fs::write(scratch.join("stdout.json"), &output.stdout)
        .map_err(|error| format!("retain model-selection stdout: {error}"))?;
    std::fs::write(scratch.join("stderr.txt"), &output.stderr)
        .map_err(|error| format!("retain model-selection stderr: {error}"))?;
    ensure_equal(
        &output.status.code(),
        &Some(0),
        "host-independent eval exit",
    )?;
    ensure_equal(
        &output.stderr,
        &Vec::<u8>::new(),
        "host-independent eval stderr",
    )?;
    let observed: Value = serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("parse host-independent eval: {error}"))?;
    ensure_equal(
        &observed["data"]["report"]["metrics"]["queries_evaluated"],
        &json!(3),
        "model-selection comparison must execute all three queries",
    )?;
    ensure_equal(
        &normalized_response_for_stability(observed)?,
        &normalized_response_for_stability(baseline)?,
        "host model settings cannot change fixture results or their hash",
    )
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
        &json!(["mem_askreleasetagformat0000000"]),
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
    let root = root.canonicalize().map_err(|error| error.to_string())?;
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
    direct_case["expected_cited_memory_ids"] = json!(["mem_askunrelatedui000000000000"]);
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

/// bd-1azkt.33. A declared query that appears verbatim inside the memory it is
/// expected to retrieve is a LEXICAL DIRECT HIT: it cannot miss. Measured
/// against the frozen goldens, every such query scores precision_at_1 = 1.0 at
/// rank 1 -- six of six, no exceptions -- while paraphrase queries in the same
/// two families average 0.250 and several land at 0.0 on rank 2-3. Same
/// evaluator, same corpus, same run; the only difference is whether the query
/// text occurs in the target.
///
/// They are averaged into the same `mean_precision_at_1` that decides the
/// release-quality verdict in `src/cli/mod.rs` (`>= 0.5`), so they do not
/// merely fail to measure retrieval -- they push the verdict toward passing.
/// Today `fx.dangerous_cleanup.v1` reports 0.667 and PASSES; across its four
/// paraphrase queries alone it scores 0.250 and would FAIL the same gate. Five
/// direct hits are carrying that pass. It executes every query it declares, so
/// executed equals expected and no coverage count can see it.
///
/// WHAT THIS CANNOT CATCH. It pins the direct-hit COUNT, not the verdict. A
/// family can still pass on direct hits -- `fx.dangerous_cleanup.v1` does, and
/// this test is green while it does. Excluding direct hits from the quality
/// mean is .12's threshold call and relabeling them is .30's; both change a
/// passing fixture to failing, which is not this bead's to decide.
///
/// Substring is deliberately the weakest reading of "direct hit" -- it counts
/// only exact occurrences, so stemmed and reordered near-hits score as
/// paraphrase and the 17 is a FLOOR on contamination, never an overstatement.
///
/// Ratcheted both directions, because each direction means something
/// different. UP means a new query was authored that cannot fail. DOWN means
/// the repair landed, or a query was re-anchored -- update the constant then,
/// and only after establishing which.
const LEXICAL_DIRECT_HIT_QUERIES: usize = 17;

/// Below this, the census read nothing and a `0` would be a broken instrument
/// reporting a clean suite.
///
/// The floor is not ceremony: reading the raw JSON instead of materializing it
/// undercounts by four. `fx.data_size_tiers.v1` declares a generator and an id
/// range and carries no `content` key at all, so parsing the file scores every
/// one of its nine queries a false paraphrase -- yet once its 600 memories are
/// materialized, four of its queries turn out to sit verbatim in the templates
/// that generate them (`release memory` inside "Small tier release memory
/// {n}", `deterministic priority bucket` inside the large-tier template). A
/// clean zero from that family was the instrument failing to read, not the
/// family being clean.
const MIN_CENSUSED_QUERIES: usize = 40;

#[test]
fn declared_eval_queries_do_not_drift_toward_lexical_direct_hits() -> TestResult {
    let fixtures = ee::eval::discover_fixtures(std::path::Path::new("tests/fixtures/eval"))
        .map_err(|error| error.to_string())?;
    let mut direct_hits: BTreeSet<(&str, String)> = BTreeSet::new();
    let mut paraphrases: BTreeSet<(&str, String)> = BTreeSet::new();

    for &(fixture_id, _) in RETRIEVAL_WORKLOADS {
        let fixture = fixtures
            .iter()
            .find(|fixture| fixture.fixture_id == fixture_id)
            .ok_or_else(|| format!("missing fixture {fixture_id}"))?;
        let source = ee::eval::load_source_memories(&fixture.source_memory_path)
            .map_err(|error| error.to_string())?;
        let memories =
            ee::eval::materialize_source_memories(&source).map_err(|error| error.to_string())?;
        for memory in &memories {
            let content = memory.content.to_lowercase();
            for query in &memory.expected_query_match {
                let key = (fixture_id, query.clone());
                if content.contains(&query.to_lowercase()) {
                    paraphrases.remove(&key);
                    direct_hits.insert(key);
                } else if !direct_hits.contains(&key) {
                    paraphrases.insert(key);
                }
            }
        }
    }

    let censused = direct_hits.len() + paraphrases.len();
    // Print the population before judging it: a bare count cannot say which
    // query newly became unmissable, and that is the only actionable part.
    println!(
        "lexical direct-hit census: {} direct / {censused} declared across {} families",
        direct_hits.len(),
        RETRIEVAL_WORKLOADS.len()
    );
    for (fixture_id, query) in &direct_hits {
        println!("  DIRECT HIT  {fixture_id}  {query:?}");
    }

    if censused < MIN_CENSUSED_QUERIES {
        return Err(format!(
            "censused only {censused} declared queries (floor {MIN_CENSUSED_QUERIES}): \
             the corpus did not materialize, so this measured nothing"
        ));
    }
    // A matcher stuck at always-true or always-false cannot discriminate, and
    // would sail through a count it happened to match. Both arms must be
    // populated for the count above to carry any information.
    if direct_hits.is_empty() || paraphrases.is_empty() {
        return Err(format!(
            "predicate does not discriminate: {} direct, {} paraphrase -- \
             one arm is empty, so the classifier is broken, not the suite clean",
            direct_hits.len(),
            paraphrases.len()
        ));
    }
    ensure_equal(
        &direct_hits.len(),
        &LEXICAL_DIRECT_HIT_QUERIES,
        "declared queries that appear verbatim in the memory they must retrieve",
    )
}

/// bd-j09rg. The evaluator used to keep `report.results` and discard the rest
/// of the SearchReport, so every diagnostic search raised was dropped at the
/// boundary -- including the orphan filtering that made 40 queries across six
/// families return nothing while the reports still rendered as ordinary
/// failures. The carry-through now exists, but its only other coverage is
/// in-crate unit tests over SYNTHETIC reports, which pass whether or not the
/// real binary emits anything at all.
///
/// So this asserts on OBSERVED OUTPUT of the actual CLI. A test built on a
/// constructed fixture proves the fixture; this exists solely to go red if the
/// wire is cut again.
///
/// THE LOAD-BEARING ASSERTION IS NOT "degradations exist". It is that they
/// arrive ATTRIBUTED PER QUERY and cover the fixture's declared inventory
/// exactly. A severed channel cannot produce that, and neither can a single
/// collapsed entry -- src/eval/runner.rs states the intent as "Do not collapse
/// equal codes across queries: that would hide which workload was affected."
/// Presence alone would still pass if attribution were dropped.
///
/// WHEN THIS GOES RED AND THE PLUMBING IS FINE. Every eval search currently
/// runs on a non-semantic hash embedder stack, so `embed_model_unavailable`
/// and `rerank_model_unavailable` are raised for every query. If the evaluator
/// ever gains a real semantic embedder those stop and an empty `degraded` is
/// legitimate. The failure text names both readings rather than asserting the
/// wire broke, because an assertion that reds when the feature works is worse
/// than none if the next reader cannot tell which happened.
///
/// This test deliberately does NOT fail the run because degradations exist.
/// They exist on every run today, so that rule would pin this gate red from
/// the first minute and teach everyone to ignore it.
#[test]
fn eval_run_surfaces_search_degradations_attributed_to_each_query() -> TestResult {
    const FIXTURE: &str = "fx.dangerous_cleanup.v1";
    let (_, declared) = RETRIEVAL_WORKLOADS
        .iter()
        .find(|(id, _)| *id == FIXTURE)
        .ok_or_else(|| format!("{FIXTURE} is not a declared retrieval workload"))?;

    let value = command_json(&["--json", "eval", "run", FIXTURE])?;
    let degraded = value["degraded"]
        .as_array()
        .ok_or_else(|| format!("eval run emitted no degraded array at all: {value}"))?;

    if degraded.is_empty() {
        return Err(format!(
            "eval run reported zero search degradations for {FIXTURE}. Two readings, \
             and they need different responses: (1) the per-query carry-through in \
             compute_search_query_metrics / retrieval_degradations was severed again, \
             which is the regression this test exists to catch; or (2) the evaluator \
             gained a real semantic embedder, so embed_model_unavailable and \
             rerank_model_unavailable legitimately stopped. Check \
             sourceModeApplied on a manual `ee --json eval run {FIXTURE}` before \
             assuming the first."
        ));
    }

    let mut attributed = BTreeSet::new();
    let mut codes = BTreeSet::new();
    for entry in degraded {
        let code = entry["code"]
            .as_str()
            .ok_or_else(|| format!("degradation without a code: {entry}"))?;
        codes.insert(code.to_owned());
        let details = &entry["details"];
        ensure_equal(
            &details["fixtureId"].as_str(),
            &Some(FIXTURE),
            "degradation attributed to its fixture",
        )?;
        for field in ["sourceModeRequested", "sourceModeApplied", "searchStatus"] {
            if !details[field].is_string() {
                return Err(format!(
                    "degradation {code} lost {field}; the search layer's own \
                     classification is what makes a degraded measurement legible: {entry}"
                ));
            }
        }
        let query = details["query"]
            .as_str()
            .ok_or_else(|| format!("degradation {code} is not attributed to a query: {entry}"))?;
        attributed.insert(query.to_owned());
    }

    // Printed, not asserted: pinning the exact code set would red on any
    // honest change to the embedder stack, which is not what this guards.
    println!(
        "observed degradation codes for {FIXTURE}: {codes:?} across {} entries",
        degraded.len()
    );

    ensure_equal(
        &attributed,
        &declared.iter().copied().map(str::to_owned).collect(),
        "every declared query carries its own search degradation",
    )
}
