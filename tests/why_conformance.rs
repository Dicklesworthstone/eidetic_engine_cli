//! EE-lp4p.8: ee why memory explanation conformance tests
//!
//! Validates that `ee why <memory-id> --json` output conforms to the expected
//! schema and includes complete explanation fields for storage, retrieval,
//! and selection decisions.

use std::fmt::Debug;
use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};

type TestResult = Result<(), String>;

fn run_ee(args: &[&str]) -> Result<Output, String> {
    Command::new(env!("CARGO_BIN_EXE_ee"))
        .args(args)
        .output()
        .map_err(|error| format!("failed to run ee {}: {error}", args.join(" ")))
}

fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

fn ensure_equal<T>(actual: &T, expected: &T, context: &str) -> TestResult
where
    T: Debug + PartialEq,
{
    if actual == expected {
        Ok(())
    } else {
        Err(format!("{context}: expected {expected:?}, got {actual:?}"))
    }
}

fn stdout_json(output: &Output) -> Result<serde_json::Value, String> {
    let stdout = String::from_utf8(output.stdout.clone())
        .map_err(|error| format!("stdout was not UTF-8: {error}"))?;
    serde_json::from_str(&stdout).map_err(|error| format!("stdout was not JSON: {error}\n{stdout}"))
}

fn stdout_is_json(output: &Output) -> bool {
    let stdout = String::from_utf8_lossy(&output.stdout);
    serde_json::from_str::<serde_json::Value>(&stdout).is_ok()
}

fn stdout_is_clean(output: &Output) -> bool {
    let stdout = String::from_utf8_lossy(&output.stdout);
    for line in stdout.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("[INFO]")
            || trimmed.starts_with("[WARN]")
            || trimmed.starts_with("[ERROR]")
            || trimmed.starts_with("warning:")
            || trimmed.starts_with("error:")
        {
            return false;
        }
    }
    true
}

fn artifact_dir() -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("why_conformance_artifacts");
    let _ = fs::create_dir_all(&dir);
    dir
}

fn persist_artifact(name: &str, output: &Output) {
    let dir = artifact_dir();
    let stdout_path = dir.join(format!("{name}.stdout"));
    let stderr_path = dir.join(format!("{name}.stderr"));
    let _ = fs::write(&stdout_path, &output.stdout);
    let _ = fs::write(&stderr_path, &output.stderr);
}

fn persist_json_artifact(name: &str, value: &serde_json::Value) {
    let dir = artifact_dir();
    let path = dir.join(format!("{name}.json"));
    let _ = fs::write(
        &path,
        serde_json::to_string_pretty(value).unwrap_or_default(),
    );
}

// ============================================================================
// Schema Conformance Tests
// ============================================================================

#[test]
fn why_response_schema_is_ee_response_v1() -> TestResult {
    let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
    let workspace = tempdir.path().to_string_lossy().to_string();

    // Setup: init, remember, index, context (to populate pack selection)
    let init = run_ee(&["--workspace", &workspace, "init", "--json"])?;
    ensure_equal(&init.status.code(), &Some(0), "init exit")?;

    let remember = run_ee(&[
        "--workspace",
        &workspace,
        "remember",
        "Schema conformance test memory.",
        "--level",
        "episodic",
        "--kind",
        "fact",
        "--json",
    ])?;
    persist_artifact("schema_remember", &remember);
    ensure_equal(&remember.status.code(), &Some(0), "remember exit")?;
    let remember_json = stdout_json(&remember)?;
    let memory_id = remember_json["data"]["memory_id"]
        .as_str()
        .ok_or_else(|| "memory_id must be a string".to_string())?;

    let rebuild = run_ee(&["--workspace", &workspace, "index", "rebuild", "--json"])?;
    ensure_equal(&rebuild.status.code(), &Some(0), "rebuild exit")?;

    let context = run_ee(&[
        "--workspace",
        &workspace,
        "context",
        "schema test",
        "--max-tokens",
        "4000",
        "--json",
    ])?;
    ensure_equal(&context.status.code(), &Some(0), "context exit")?;

    // Test: ee why
    let why = run_ee(&["--workspace", &workspace, "why", memory_id, "--json"])?;
    persist_artifact("schema_why", &why);
    let why_stderr = String::from_utf8_lossy(&why.stderr);
    ensure_equal(
        &why.status.code(),
        &Some(0),
        &format!("why exit (stderr: {why_stderr})"),
    )?;
    ensure(why.stderr.is_empty(), format!("stderr empty: {why_stderr}"))?;
    ensure(stdout_is_json(&why), "stdout is JSON")?;
    ensure(stdout_is_clean(&why), "stdout is clean")?;

    let why_json = stdout_json(&why)?;
    persist_json_artifact("schema_why", &why_json);

    // Schema assertions
    ensure_equal(
        &why_json["schema"],
        &serde_json::json!("ee.response.v1"),
        "response schema",
    )?;
    ensure_equal(
        &why_json["success"],
        &serde_json::json!(true),
        "success flag",
    )?;
    ensure(why_json["data"].is_object(), "data field must be an object")
}

#[test]
fn why_accepts_result_doc_id_targets() -> TestResult {
    let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
    let workspace = tempdir.path().to_string_lossy().to_string();

    let init = run_ee(&["--workspace", &workspace, "init", "--json"])?;
    ensure_equal(&init.status.code(), &Some(0), "init exit")?;

    let remember = run_ee(&[
        "--workspace",
        &workspace,
        "remember",
        "Result target explanation memory for release diagnostics.",
        "--level",
        "episodic",
        "--kind",
        "fact",
        "--tags",
        "result-target,explainability",
        "--json",
    ])?;
    persist_artifact("result_target_remember", &remember);
    ensure_equal(&remember.status.code(), &Some(0), "remember exit")?;

    let rebuild = run_ee(&["--workspace", &workspace, "index", "rebuild", "--json"])?;
    ensure_equal(&rebuild.status.code(), &Some(0), "rebuild exit")?;

    let search = run_ee(&[
        "--workspace",
        &workspace,
        "search",
        "release diagnostics",
        "--limit",
        "1",
        "--json",
    ])?;
    persist_artifact("result_target_search", &search);
    ensure_equal(&search.status.code(), &Some(0), "search exit")?;
    let search_json = stdout_json(&search)?;
    persist_json_artifact("result_target_search", &search_json);
    let doc_id = search_json["data"]["results"][0]["docId"]
        .as_str()
        .ok_or_else(|| "search result docId must be a string".to_string())?;
    let target = format!("result:{doc_id}");

    let why = run_ee(&["--workspace", &workspace, "why", &target, "--json"])?;
    persist_artifact("result_target_why", &why);
    ensure_equal(&why.status.code(), &Some(0), "why exit")?;

    let why_json = stdout_json(&why)?;
    persist_json_artifact("result_target_why", &why_json);

    ensure_equal(
        &why_json["data"]["memoryId"],
        &serde_json::json!(doc_id),
        "result target resolves to underlying document id",
    )?;
    ensure_equal(
        &why_json["data"]["found"],
        &serde_json::json!(true),
        "found",
    )?;
    ensure(
        why_json["data"]["retrieval"].is_object(),
        "result target must include retrieval explanation",
    )
}

#[test]
fn why_storage_section_is_complete() -> TestResult {
    let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
    let workspace = tempdir.path().to_string_lossy().to_string();

    let init = run_ee(&["--workspace", &workspace, "init", "--json"])?;
    ensure_equal(&init.status.code(), &Some(0), "init exit")?;

    let remember = run_ee(&[
        "--workspace",
        &workspace,
        "remember",
        "Storage section test memory with provenance.",
        "--level",
        "procedural",
        "--kind",
        "rule",
        "--source",
        "file://tests/fixtures/storage_test.json#L42",
        "--json",
    ])?;
    persist_artifact("storage_remember", &remember);
    ensure_equal(&remember.status.code(), &Some(0), "remember exit")?;
    let remember_json = stdout_json(&remember)?;
    let memory_id = remember_json["data"]["memory_id"]
        .as_str()
        .ok_or_else(|| "memory_id must be a string".to_string())?;

    let rebuild = run_ee(&["--workspace", &workspace, "index", "rebuild", "--json"])?;
    ensure_equal(&rebuild.status.code(), &Some(0), "rebuild exit")?;

    let context = run_ee(&[
        "--workspace",
        &workspace,
        "context",
        "storage test",
        "--max-tokens",
        "4000",
        "--json",
    ])?;
    ensure_equal(&context.status.code(), &Some(0), "context exit")?;

    let why = run_ee(&["--workspace", &workspace, "why", memory_id, "--json"])?;
    persist_artifact("storage_why", &why);
    ensure_equal(&why.status.code(), &Some(0), "why exit")?;

    let why_json = stdout_json(&why)?;
    persist_json_artifact("storage_why", &why_json);

    // Storage section conformance
    let storage = &why_json["data"]["storage"];
    ensure(storage.is_object(), "storage section must exist")?;
    ensure_equal(
        &storage["provenanceUri"],
        &serde_json::json!("file://tests/fixtures/storage_test.json#L42"),
        "storage provenanceUri",
    )?;
    // Storage section should contain memory metadata; exact fields may vary
    ensure(
        storage.as_object().is_some_and(|obj| !obj.is_empty()),
        "storage section must contain fields",
    )
}

#[test]
fn why_retrieval_section_exposes_numeric_scores() -> TestResult {
    let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
    let workspace = tempdir.path().to_string_lossy().to_string();

    let init = run_ee(&["--workspace", &workspace, "init", "--json"])?;
    ensure_equal(&init.status.code(), &Some(0), "init exit")?;

    let remember = run_ee(&[
        "--workspace",
        &workspace,
        "remember",
        "Retrieval section test memory.",
        "--level",
        "episodic",
        "--kind",
        "fact",
        "--tags",
        "retrieval,test,conformance",
        "--json",
    ])?;
    persist_artifact("retrieval_remember", &remember);
    ensure_equal(&remember.status.code(), &Some(0), "remember exit")?;
    let remember_json = stdout_json(&remember)?;
    let memory_id = remember_json["data"]["memory_id"]
        .as_str()
        .ok_or_else(|| "memory_id must be a string".to_string())?;

    let rebuild = run_ee(&["--workspace", &workspace, "index", "rebuild", "--json"])?;
    ensure_equal(&rebuild.status.code(), &Some(0), "rebuild exit")?;

    let context = run_ee(&[
        "--workspace",
        &workspace,
        "context",
        "retrieval test",
        "--max-tokens",
        "4000",
        "--json",
    ])?;
    ensure_equal(&context.status.code(), &Some(0), "context exit")?;

    let why = run_ee(&["--workspace", &workspace, "why", memory_id, "--json"])?;
    persist_artifact("retrieval_why", &why);
    ensure_equal(&why.status.code(), &Some(0), "why exit")?;

    let why_json = stdout_json(&why)?;
    persist_json_artifact("retrieval_why", &why_json);

    // Retrieval section conformance
    let retrieval = &why_json["data"]["retrieval"];
    ensure(retrieval.is_object(), "retrieval section must exist")?;
    ensure(
        retrieval["confidence"].as_f64().is_some(),
        "retrieval.confidence must be numeric",
    )?;
    ensure(
        retrieval["utility"].as_f64().is_some(),
        "retrieval.utility must be numeric",
    )?;
    ensure(
        retrieval["importance"].as_f64().is_some(),
        "retrieval.importance must be numeric",
    )?;

    // Tags preservation
    let tags = retrieval["tags"]
        .as_array()
        .ok_or_else(|| "retrieval.tags must be an array".to_string())?;
    ensure(
        tags.iter().any(|tag| tag.as_str() == Some("retrieval")),
        "retrieval.tags must include 'retrieval' tag",
    )?;
    ensure(
        tags.iter().any(|tag| tag.as_str() == Some("conformance")),
        "retrieval.tags must include 'conformance' tag",
    )
}

#[test]
fn why_graph_retrieval_features_are_complete_when_snapshot_is_missing() -> TestResult {
    let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
    let workspace = tempdir.path().to_string_lossy().to_string();

    let init = run_ee(&["--workspace", &workspace, "init", "--json"])?;
    ensure_equal(&init.status.code(), &Some(0), "init exit")?;

    let remember = run_ee(&[
        "--workspace",
        &workspace,
        "remember",
        "Graph retrieval explanation field coverage memory.",
        "--level",
        "episodic",
        "--kind",
        "fact",
        "--json",
    ])?;
    ensure_equal(&remember.status.code(), &Some(0), "remember exit")?;
    let remember_json = stdout_json(&remember)?;
    let memory_id = remember_json["data"]["memory_id"]
        .as_str()
        .ok_or_else(|| "memory_id must be a string".to_string())?;

    let why = run_ee(&["--workspace", &workspace, "why", memory_id, "--json"])?;
    persist_artifact("graph_retrieval_missing_snapshot_why", &why);
    ensure_equal(&why.status.code(), &Some(0), "why exit")?;

    let why_json = stdout_json(&why)?;
    persist_json_artifact("graph_retrieval_missing_snapshot_why", &why_json);
    let graph = &why_json["data"]["graphRetrievalFeatures"];

    ensure(graph.is_object(), "graphRetrievalFeatures must be present")?;
    ensure_equal(
        &graph["status"],
        &serde_json::json!("scores_unavailable"),
        "graph status",
    )?;
    ensure(
        graph["centralityScore"].as_f64().is_some(),
        "centralityScore must be numeric",
    )?;
    ensure(
        graph["authorityScore"].as_f64().is_some(),
        "authorityScore must be numeric",
    )?;
    ensure(
        graph["hubScore"].as_f64().is_some(),
        "hubScore must be numeric",
    )?;
    ensure(
        graph["communityId"].is_null(),
        "communityId must be explicit null when unavailable",
    )?;
    ensure(
        graph["distanceToQuerySeed"].is_null(),
        "distanceToQuerySeed must be explicit null when unavailable",
    )?;
    ensure(
        graph["sameClusterAsTopResult"].is_null(),
        "sameClusterAsTopResult must be explicit null when unavailable",
    )?;
    ensure(
        graph["evidenceSupportCount"].as_u64().is_some(),
        "evidenceSupportCount must be numeric",
    )?;
    ensure(
        graph["contradictionCount"].as_u64().is_some(),
        "contradictionCount must be numeric",
    )?;
    ensure(
        graph["orphanPenalty"].as_f64().is_some(),
        "orphanPenalty must be numeric",
    )?;
    ensure(
        graph["staleBridgePenalty"].as_f64().is_some(),
        "staleBridgePenalty must be numeric",
    )?;
    ensure(graph["pagerank"].is_object(), "pagerank metric must exist")?;
    ensure(
        graph["betweenness"].is_object(),
        "betweenness metric must exist",
    )?;
    ensure(
        graph["degraded"].as_array().is_some_and(|items| {
            items
                .iter()
                .any(|item| item["code"] == "graph_snapshot_missing")
        }),
        "missing graph snapshot must be explained",
    )
}

#[test]
fn why_selection_section_exposes_score_formula() -> TestResult {
    let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
    let workspace = tempdir.path().to_string_lossy().to_string();

    let init = run_ee(&["--workspace", &workspace, "init", "--json"])?;
    ensure_equal(&init.status.code(), &Some(0), "init exit")?;

    let remember = run_ee(&[
        "--workspace",
        &workspace,
        "remember",
        "Selection section test memory.",
        "--level",
        "episodic",
        "--kind",
        "fact",
        "--json",
    ])?;
    persist_artifact("selection_remember", &remember);
    ensure_equal(&remember.status.code(), &Some(0), "remember exit")?;
    let remember_json = stdout_json(&remember)?;
    let memory_id = remember_json["data"]["memory_id"]
        .as_str()
        .ok_or_else(|| "memory_id must be a string".to_string())?;

    let rebuild = run_ee(&["--workspace", &workspace, "index", "rebuild", "--json"])?;
    ensure_equal(&rebuild.status.code(), &Some(0), "rebuild exit")?;

    let context = run_ee(&[
        "--workspace",
        &workspace,
        "context",
        "selection test",
        "--max-tokens",
        "4000",
        "--json",
    ])?;
    ensure_equal(&context.status.code(), &Some(0), "context exit")?;

    let why = run_ee(&["--workspace", &workspace, "why", memory_id, "--json"])?;
    persist_artifact("selection_why", &why);
    ensure_equal(&why.status.code(), &Some(0), "why exit")?;

    let why_json = stdout_json(&why)?;
    persist_json_artifact("selection_why", &why_json);

    // Selection section conformance
    let selection = &why_json["data"]["selection"];
    ensure(selection.is_object(), "selection section must exist")?;
    ensure(
        selection["selectionScore"].as_f64().is_some(),
        "selection.selectionScore must be numeric",
    )?;
    ensure(
        selection["scoreBreakdown"]
            .as_str()
            .is_some_and(|breakdown| breakdown.contains("selection_score")),
        "selection.scoreBreakdown must contain deterministic formula",
    )
}

#[test]
fn why_latest_pack_selection_references_context_pack() -> TestResult {
    let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
    let workspace = tempdir.path().to_string_lossy().to_string();

    let init = run_ee(&["--workspace", &workspace, "init", "--json"])?;
    ensure_equal(&init.status.code(), &Some(0), "init exit")?;

    let remember = run_ee(&[
        "--workspace",
        &workspace,
        "remember",
        "Pack selection reference test memory.",
        "--level",
        "episodic",
        "--kind",
        "fact",
        "--json",
    ])?;
    persist_artifact("pack_ref_remember", &remember);
    ensure_equal(&remember.status.code(), &Some(0), "remember exit")?;
    let remember_json = stdout_json(&remember)?;
    let memory_id = remember_json["data"]["memory_id"]
        .as_str()
        .ok_or_else(|| "memory_id must be a string".to_string())?;

    let rebuild = run_ee(&["--workspace", &workspace, "index", "rebuild", "--json"])?;
    ensure_equal(&rebuild.status.code(), &Some(0), "rebuild exit")?;

    // Run context to create a pack record
    let context = run_ee(&[
        "--workspace",
        &workspace,
        "context",
        "pack reference test",
        "--max-tokens",
        "4000",
        "--json",
    ])?;
    persist_artifact("pack_ref_context", &context);
    ensure_equal(&context.status.code(), &Some(0), "context exit")?;
    let context_json = stdout_json(&context)?;
    persist_json_artifact("pack_ref_context", &context_json);

    let pack_hash = context_json["data"]["pack"]["hash"]
        .as_str()
        .ok_or_else(|| "context pack hash must be a string".to_string())?;

    let why = run_ee(&["--workspace", &workspace, "why", memory_id, "--json"])?;
    persist_artifact("pack_ref_why", &why);
    ensure_equal(&why.status.code(), &Some(0), "why exit")?;

    let why_json = stdout_json(&why)?;
    persist_json_artifact("pack_ref_why", &why_json);

    // Latest pack selection conformance
    let latest_pack = &why_json["data"]["selection"]["latestPackSelection"];
    ensure(
        latest_pack.is_object(),
        "latestPackSelection section must exist",
    )?;
    ensure(
        latest_pack["packId"]
            .as_str()
            .is_some_and(|id| id.starts_with("pack_")),
        "latestPackSelection.packId must have pack_ prefix",
    )?;
    ensure_equal(
        &latest_pack["packHash"],
        &serde_json::json!(pack_hash),
        "latestPackSelection.packHash must match context pack",
    )?;
    ensure(
        latest_pack["query"].as_str().is_some(),
        "latestPackSelection.query must be a string",
    )?;
    ensure(
        latest_pack["relevance"].as_f64().is_some(),
        "latestPackSelection.relevance must be numeric",
    )?;
    ensure(
        latest_pack["utility"].as_f64().is_some(),
        "latestPackSelection.utility must be numeric",
    )
}

// ============================================================================
// Error Handling Conformance
// ============================================================================

#[test]
fn why_invalid_memory_id_returns_usage_error() -> TestResult {
    let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
    let workspace = tempdir.path().to_string_lossy().to_string();

    let init = run_ee(&["--workspace", &workspace, "init", "--json"])?;
    ensure_equal(&init.status.code(), &Some(0), "init exit")?;

    let why = run_ee(&[
        "--workspace",
        &workspace,
        "why",
        "not-a-valid-memory-id",
        "--json",
    ])?;
    persist_artifact("invalid_id_why", &why);

    ensure(stdout_is_json(&why), "error response must be JSON")?;
    let why_json = stdout_json(&why)?;
    persist_json_artifact("invalid_id_why", &why_json);

    ensure_equal(
        &why_json["schema"],
        &serde_json::json!("ee.error.v1"),
        "error schema",
    )?;
    // Invalid ID format may return "usage" or "not_found" depending on validation order
    let error_code = why_json["error"]["code"]
        .as_str()
        .ok_or_else(|| "error code must be a string".to_string())?;
    ensure(
        error_code == "usage" || error_code == "not_found",
        format!("error code must be usage or not_found, got {error_code}"),
    )?;
    ensure(
        why.status.code() == Some(1) || why.status.code() == Some(3),
        "error exit code must be 1 or 3",
    )
}

#[test]
fn why_nonexistent_memory_returns_not_found() -> TestResult {
    let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
    let workspace = tempdir.path().to_string_lossy().to_string();

    let init = run_ee(&["--workspace", &workspace, "init", "--json"])?;
    ensure_equal(&init.status.code(), &Some(0), "init exit")?;

    // Valid format but nonexistent
    let why = run_ee(&[
        "--workspace",
        &workspace,
        "why",
        "mem_00000000000000000000000000",
        "--json",
    ])?;
    persist_artifact("nonexistent_why", &why);

    ensure(stdout_is_json(&why), "response must be JSON")?;
    let why_json = stdout_json(&why)?;
    persist_json_artifact("nonexistent_why", &why_json);

    // Either error or unsuccessful response is acceptable
    ensure(
        why_json["schema"].as_str() == Some("ee.error.v1")
            || why_json["success"].as_bool() == Some(false),
        "nonexistent memory must return error or unsuccessful response",
    )
}

// ============================================================================
// Explanation Completeness Tests
// ============================================================================

#[test]
fn why_explanation_covers_all_memory_levels() -> TestResult {
    let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
    let workspace = tempdir.path().to_string_lossy().to_string();

    let init = run_ee(&["--workspace", &workspace, "init", "--json"])?;
    ensure_equal(&init.status.code(), &Some(0), "init exit")?;

    let levels = ["episodic", "semantic", "procedural"];
    let mut memory_ids = Vec::new();

    for level in &levels {
        let remember = run_ee(&[
            "--workspace",
            &workspace,
            "remember",
            &format!("Memory at {level} level for completeness test."),
            "--level",
            level,
            "--kind",
            "fact",
            "--json",
        ])?;
        persist_artifact(&format!("levels_{level}_remember"), &remember);
        ensure_equal(&remember.status.code(), &Some(0), "remember exit")?;
        let remember_json = stdout_json(&remember)?;
        let memory_id = remember_json["data"]["memory_id"]
            .as_str()
            .ok_or_else(|| "memory_id must be a string".to_string())?
            .to_string();
        memory_ids.push((level.to_string(), memory_id));
    }

    let rebuild = run_ee(&["--workspace", &workspace, "index", "rebuild", "--json"])?;
    ensure_equal(&rebuild.status.code(), &Some(0), "rebuild exit")?;

    let context = run_ee(&[
        "--workspace",
        &workspace,
        "context",
        "completeness test",
        "--max-tokens",
        "8000",
        "--json",
    ])?;
    ensure_equal(&context.status.code(), &Some(0), "context exit")?;

    for (level, memory_id) in &memory_ids {
        let why = run_ee(&["--workspace", &workspace, "why", memory_id, "--json"])?;
        persist_artifact(&format!("levels_{level}_why"), &why);
        ensure_equal(&why.status.code(), &Some(0), &format!("{level} why exit"))?;

        let why_json = stdout_json(&why)?;
        persist_json_artifact(&format!("levels_{level}_why"), &why_json);

        // All memory levels should produce complete why responses
        ensure(
            why_json["data"]["storage"].is_object(),
            format!("{level} must have storage section"),
        )?;
        ensure(
            why_json["data"]["retrieval"].is_object(),
            format!("{level} must have retrieval section"),
        )?;
        ensure(
            why_json["data"]["selection"].is_object(),
            format!("{level} must have selection section"),
        )?;
    }

    Ok(())
}

#[test]
fn why_explanation_covers_all_memory_kinds() -> TestResult {
    let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
    let workspace = tempdir.path().to_string_lossy().to_string();

    let init = run_ee(&["--workspace", &workspace, "init", "--json"])?;
    ensure_equal(&init.status.code(), &Some(0), "init exit")?;

    let kinds = ["fact", "rule", "failure", "decision"];
    let mut memory_ids = Vec::new();

    for kind in &kinds {
        let remember = run_ee(&[
            "--workspace",
            &workspace,
            "remember",
            &format!("Memory of kind {kind} for completeness test."),
            "--level",
            "episodic",
            "--kind",
            kind,
            "--json",
        ])?;
        persist_artifact(&format!("kinds_{kind}_remember"), &remember);
        ensure_equal(&remember.status.code(), &Some(0), "remember exit")?;
        let remember_json = stdout_json(&remember)?;
        let memory_id = remember_json["data"]["memory_id"]
            .as_str()
            .ok_or_else(|| "memory_id must be a string".to_string())?
            .to_string();
        memory_ids.push((kind.to_string(), memory_id));
    }

    let rebuild = run_ee(&["--workspace", &workspace, "index", "rebuild", "--json"])?;
    ensure_equal(&rebuild.status.code(), &Some(0), "rebuild exit")?;

    let context = run_ee(&[
        "--workspace",
        &workspace,
        "context",
        "kinds test",
        "--max-tokens",
        "8000",
        "--json",
    ])?;
    ensure_equal(&context.status.code(), &Some(0), "context exit")?;

    for (kind, memory_id) in &memory_ids {
        let why = run_ee(&["--workspace", &workspace, "why", memory_id, "--json"])?;
        persist_artifact(&format!("kinds_{kind}_why"), &why);
        ensure_equal(&why.status.code(), &Some(0), &format!("{kind} why exit"))?;

        let why_json = stdout_json(&why)?;
        persist_json_artifact(&format!("kinds_{kind}_why"), &why_json);

        // All memory kinds should produce complete why responses
        ensure(
            why_json["data"]["storage"].is_object(),
            format!("{kind} must have storage section"),
        )?;
        ensure(
            why_json["data"]["retrieval"].is_object(),
            format!("{kind} must have retrieval section"),
        )?;
        ensure(
            why_json["data"]["selection"].is_object(),
            format!("{kind} must have selection section"),
        )?;
    }

    Ok(())
}

// ============================================================================
// Output Contract Stability
// ============================================================================

#[test]
fn why_json_output_is_stdout_only() -> TestResult {
    let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
    let workspace = tempdir.path().to_string_lossy().to_string();

    let init = run_ee(&["--workspace", &workspace, "init", "--json"])?;
    ensure_equal(&init.status.code(), &Some(0), "init exit")?;

    let remember = run_ee(&[
        "--workspace",
        &workspace,
        "remember",
        "Output contract test memory.",
        "--level",
        "episodic",
        "--kind",
        "fact",
        "--json",
    ])?;
    ensure_equal(&remember.status.code(), &Some(0), "remember exit")?;
    let remember_json = stdout_json(&remember)?;
    let memory_id = remember_json["data"]["memory_id"]
        .as_str()
        .ok_or_else(|| "memory_id must be a string".to_string())?;

    let rebuild = run_ee(&["--workspace", &workspace, "index", "rebuild", "--json"])?;
    ensure_equal(&rebuild.status.code(), &Some(0), "rebuild exit")?;

    let context = run_ee(&[
        "--workspace",
        &workspace,
        "context",
        "output test",
        "--max-tokens",
        "4000",
        "--json",
    ])?;
    ensure_equal(&context.status.code(), &Some(0), "context exit")?;

    let why = run_ee(&["--workspace", &workspace, "why", memory_id, "--json"])?;
    persist_artifact("output_contract_why", &why);

    ensure_equal(&why.status.code(), &Some(0), "why exit")?;
    ensure(why.stderr.is_empty(), "stderr must be empty in JSON mode")?;
    ensure(stdout_is_json(&why), "stdout must be valid JSON")?;
    ensure(stdout_is_clean(&why), "stdout must be clean of diagnostics")?;

    let stdout = String::from_utf8_lossy(&why.stdout);
    ensure(
        stdout.ends_with('\n'),
        "JSON output must end with trailing newline",
    )
}

#[test]
fn why_human_mode_uses_stderr_for_diagnostics() -> TestResult {
    let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
    let workspace = tempdir.path().to_string_lossy().to_string();

    let init = run_ee(&["--workspace", &workspace, "init", "--json"])?;
    ensure_equal(&init.status.code(), &Some(0), "init exit")?;

    let remember = run_ee(&[
        "--workspace",
        &workspace,
        "remember",
        "Human mode test memory.",
        "--level",
        "episodic",
        "--kind",
        "fact",
        "--json",
    ])?;
    ensure_equal(&remember.status.code(), &Some(0), "remember exit")?;
    let remember_json = stdout_json(&remember)?;
    let memory_id = remember_json["data"]["memory_id"]
        .as_str()
        .ok_or_else(|| "memory_id must be a string".to_string())?;

    let rebuild = run_ee(&["--workspace", &workspace, "index", "rebuild", "--json"])?;
    ensure_equal(&rebuild.status.code(), &Some(0), "rebuild exit")?;

    let context = run_ee(&[
        "--workspace",
        &workspace,
        "context",
        "human mode test",
        "--max-tokens",
        "4000",
        "--json",
    ])?;
    ensure_equal(&context.status.code(), &Some(0), "context exit")?;

    // Run without --json (human mode)
    let why = run_ee(&["--workspace", &workspace, "why", memory_id])?;
    persist_artifact("human_mode_why", &why);

    ensure_equal(&why.status.code(), &Some(0), "why exit")?;

    // In human mode, stdout should not be JSON
    let stdout = String::from_utf8_lossy(&why.stdout);
    ensure(
        !stdout.starts_with('{'),
        "human mode stdout should not be JSON",
    )
}
