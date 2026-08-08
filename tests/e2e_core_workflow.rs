//! EE-3ofv: Core memory workflow end-to-end integration test
//!
//! Validates the primary user flow: init → remember → search → context → why
//! using real FrankenSQLite database in a tempdir workspace.
//!
//! NO MOCKS. Real ee binary, real DB, real search indexes.

#[path = "support/test_tracing.rs"]
mod test_tracing;

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Debug;
use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};

type TestResult = Result<(), String>;

const EXIT_SUCCESS: i32 = 0;

struct RememberedMemory {
    level: String,
    kind: String,
    content: String,
    source_uri: String,
}

fn run_ee(args: &[&str]) -> Result<Output, String> {
    Command::new(env!("CARGO_BIN_EXE_ee"))
        .args(args)
        .env_remove("EE_WORKSPACE")
        .env_remove("EE_WORKSPACE_REGISTRY")
        .env_remove("EE_AGENT_NAME")
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
    serde_json::from_str(&stdout)
        .map_err(|error| format!("stdout was not JSON: {error}\nstdout: {stdout}"))
}

fn artifact_dir() -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("e2e_core_workflow_artifacts");
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

fn persist_json_artifact(name: &str, value: &serde_json::Value) -> TestResult {
    let dir = artifact_dir();
    let path = dir.join(format!("{name}.json"));
    let serialized = serde_json::to_string_pretty(value).map_err(|error| error.to_string())?;
    fs::write(&path, serialized).map_err(|error| error.to_string())
}

fn assert_schema(json: &serde_json::Value, expected: &str, context: &str) -> TestResult {
    let schema = json
        .get("schema")
        .and_then(|s| s.as_str())
        .ok_or_else(|| format!("{context}: missing schema field"))?;
    ensure_equal(&schema, &expected, &format!("{context} schema"))
}

fn assert_stderr_empty(output: &Output, context: &str) -> TestResult {
    let stderr = String::from_utf8_lossy(&output.stderr);
    ensure(
        stderr.trim().is_empty(),
        format!("{context}: stderr should be empty in JSON mode, got: {stderr}"),
    )
}

fn degraded_codes(json: &serde_json::Value) -> Vec<&str> {
    json.pointer("/data/degraded")
        .and_then(serde_json::Value::as_array)
        .map(|entries| {
            entries
                .iter()
                .filter_map(|entry| entry.get("code").and_then(serde_json::Value::as_str))
                .collect()
        })
        .unwrap_or_default()
}

fn json_array<'a>(
    value: &'a serde_json::Value,
    pointer: &str,
    context: &str,
) -> Result<&'a [serde_json::Value], String> {
    value
        .pointer(pointer)
        .and_then(serde_json::Value::as_array)
        .map(Vec::as_slice)
        .ok_or_else(|| format!("{context}: {pointer} must be an array"))
}

fn json_str<'a>(
    value: &'a serde_json::Value,
    pointer: &str,
    context: &str,
) -> Result<&'a str, String> {
    value
        .pointer(pointer)
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| format!("{context}: {pointer} must be a string"))
}

#[test]
fn init_publishes_ready_empty_search_index() -> TestResult {
    let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
    let workspace = tempdir.path().to_string_lossy().to_string();

    let init = run_ee(&["--workspace", &workspace, "init", "--json"])?;
    ensure_equal(
        &init.status.code(),
        &Some(EXIT_SUCCESS),
        "fresh init exit code",
    )?;
    assert_stderr_empty(&init, "fresh init")?;
    let init_json = stdout_json(&init)?;
    assert_schema(&init_json, "ee.response.v2", "fresh init")?;
    ensure(
        init_json
            .pointer("/data/actions")
            .and_then(serde_json::Value::as_array)
            .is_some_and(|actions| {
                actions.iter().any(|action| {
                    action.get("action").and_then(serde_json::Value::as_str)
                        == Some("initialize_index")
                        && action.get("status").and_then(serde_json::Value::as_str) == Some("ready")
                })
            }),
        format!("fresh init must report a ready search index: {init_json}"),
    )?;

    let status = run_ee(&["--workspace", &workspace, "index", "status", "--json"])?;
    ensure_equal(
        &status.status.code(),
        &Some(EXIT_SUCCESS),
        "fresh index status exit code",
    )?;
    assert_stderr_empty(&status, "fresh index status")?;
    let status_json = stdout_json(&status)?;
    assert_schema(&status_json, "ee.response.v2", "fresh index status")?;
    ensure_equal(
        &status_json.pointer("/data/health"),
        &Some(&serde_json::Value::String("ready".to_owned())),
        "fresh index health",
    )?;
    ensure_equal(
        &status_json.pointer("/data/indexDocumentCount"),
        &Some(&serde_json::Value::from(0)),
        "fresh index document count",
    )?;
    ensure_equal(
        &status_json.pointer("/data/indexDocumentCounts"),
        &Some(&serde_json::json!({
            "memories": 0,
            "sessions": 0,
            "artifacts": 0,
            "rules": 0,
            "evidence": 0,
        })),
        "fresh index per-source document counts",
    )?;
    ensure_equal(
        &status_json.pointer("/data/actualCorpusRevision"),
        &status_json.pointer("/data/expectedCorpusRevision"),
        "fresh index corpus revision",
    )?;
    ensure_equal(
        &status_json.pointer("/data/repairHint"),
        &Some(&serde_json::Value::Null),
        "fresh index repair hint",
    )?;
    let db_generation = status_json
        .pointer("/data/dbGeneration")
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| format!("fresh index status missing dbGeneration: {status_json}"))?;
    let index_generation = status_json
        .pointer("/data/indexGeneration")
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| format!("fresh index status missing indexGeneration: {status_json}"))?;
    ensure_equal(
        &index_generation,
        &db_generation,
        "fresh index generation matches database",
    )?;

    let search = run_ee(&[
        "--workspace",
        &workspace,
        "search",
        "cold start",
        "--source-mode",
        "lexical_only",
        "--json",
    ])?;
    ensure_equal(
        &search.status.code(),
        &Some(EXIT_SUCCESS),
        "empty-index search exit code",
    )?;
    assert_stderr_empty(&search, "empty-index search")?;
    let search_json = stdout_json(&search)?;
    assert_schema(&search_json, "ee.response.v2", "empty-index search")?;
    ensure_equal(
        &search_json.pointer("/data/status"),
        &Some(&serde_json::Value::String("no_results".to_owned())),
        "empty-index search status",
    )?;
    ensure_equal(
        &search_json.pointer("/data/results"),
        &Some(&serde_json::json!([])),
        "empty-index search results",
    )?;
    ensure_equal(
        &search_json.pointer("/data/errors"),
        &Some(&serde_json::json!([])),
        "empty-index search errors",
    )
}

#[test]
fn core_workflow_init_remember_search_context_why() -> TestResult {
    let tempdir = tempfile::tempdir().map_err(|e| e.to_string())?;
    let workspace = tempdir.path().to_string_lossy().to_string();
    let trace = test_tracing::init_test_tracing(
        "bd-3usjw.55",
        "core_workflow_init_remember_search_context_why",
    );
    trace.setup("core_workflow", "created temporary workspace");

    // Step 1: ee init
    let init_output = run_ee(&["--workspace", &workspace, "init", "--json"])?;
    trace.exercise("core_workflow", "ee init --json", "ran init command");
    ensure_equal(
        &init_output.status.code(),
        &Some(EXIT_SUCCESS),
        "init exit code",
    )?;
    let init_json = stdout_json(&init_output)?;
    assert_schema(&init_json, "ee.response.v2", "init")?;

    // Step 2: ee remember (add 3 memories)
    let memories = [
        ("Run cargo fmt before release", "rule"),
        ("Check all tests pass before merge", "rule"),
        ("The release workflow uses GitHub Actions", "fact"),
    ];

    let mut memory_ids = Vec::new();

    for (content, kind) in &memories {
        let remember_output = run_ee(&[
            "--workspace",
            &workspace,
            "remember",
            content,
            "--level",
            "procedural",
            "--kind",
            kind,
            "--json",
        ])?;
        ensure_equal(
            &remember_output.status.code(),
            &Some(EXIT_SUCCESS),
            &format!("remember '{content}' exit code"),
        )?;
        let remember_json = stdout_json(&remember_output)?;
        assert_schema(
            &remember_json,
            "ee.response.v2",
            &format!("remember '{content}'"),
        )?;

        // Extract memory_id from response
        if let Some(id) = remember_json
            .pointer("/data/memory_id")
            .and_then(|v| v.as_str())
        {
            memory_ids.push(id.to_string());
        }
    }

    ensure(
        !memory_ids.is_empty(),
        "at least one memory_id should be captured",
    )?;

    // Step 3: ee search
    let search_output = run_ee(&[
        "--workspace",
        &workspace,
        "search",
        "cargo fmt release",
        "--json",
    ])?;
    ensure_equal(
        &search_output.status.code(),
        &Some(EXIT_SUCCESS),
        "search exit code",
    )?;
    let search_json = stdout_json(&search_output)?;
    assert_schema(&search_json, "ee.response.v2", "search")?;
    trace.verify(
        "core_workflow",
        "ee.response.v2",
        "ee.response.v2",
        "search schema matched",
    );

    // Verify search returns results
    let results = search_json
        .pointer("/data/results")
        .or_else(|| search_json.pointer("/data/hits"))
        .and_then(|r| r.as_array());
    ensure(
        results.map(|r| !r.is_empty()).unwrap_or(false),
        "search should return at least one result",
    )?;

    // Step 4: ee context
    let context_output = run_ee(&[
        "--workspace",
        &workspace,
        "pack",
        "release preparation",
        "--max-tokens",
        "2000",
        "--json",
    ])?;
    ensure_equal(
        &context_output.status.code(),
        &Some(EXIT_SUCCESS),
        "context exit code",
    )?;
    let context_json = stdout_json(&context_output)?;
    assert_schema(&context_json, "ee.response.v2", "context")?;

    // Verify context pack has items
    let pack_items = context_json
        .pointer("/data/pack/items")
        .or_else(|| context_json.pointer("/data/items"))
        .and_then(|p| p.as_array());
    ensure(
        pack_items.map(|p| !p.is_empty()).unwrap_or(false),
        "context pack should include at least one item",
    )?;

    // Step 5: ee why (if we have a memory_id)
    if let Some(memory_id) = memory_ids.first() {
        let why_output = run_ee(&["--workspace", &workspace, "why", memory_id, "--json"])?;
        ensure_equal(
            &why_output.status.code(),
            &Some(EXIT_SUCCESS),
            "why exit code",
        )?;
        let why_json = stdout_json(&why_output)?;
        assert_schema(&why_json, "ee.response.v2", "why")?;

        // Verify why has explanation data
        let has_storage = why_json.pointer("/data/storage").is_some();
        let has_report = why_json.pointer("/data/report").is_some();
        let has_data = why_json.get("data").is_some();
        ensure(
            has_storage || has_report || has_data,
            "why should return explanation data",
        )?;
    }
    trace.teardown("core_workflow", "temporary workspace dropped");

    Ok(())
}

#[test]
fn context_and_why_report_changed_file_provenance() -> TestResult {
    let tempdir = tempfile::tempdir().map_err(|e| e.to_string())?;
    let workspace = tempdir.path().to_string_lossy().to_string();
    let source_path = tempdir.path().join("freshness-source.md");
    let remembered_content = "Freshness source release evidence line";
    fs::write(&source_path, remembered_content).map_err(|error| error.to_string())?;
    let source_uri = format!("file://{}#L1", source_path.display());

    let init = run_ee(&["--workspace", &workspace, "init", "--json"])?;
    ensure_equal(&init.status.code(), &Some(EXIT_SUCCESS), "init")?;
    assert_stderr_empty(&init, "init")?;

    let remember = run_ee(&[
        "--workspace",
        &workspace,
        "remember",
        remembered_content,
        "--level",
        "procedural",
        "--kind",
        "rule",
        "--source",
        &source_uri,
        "--json",
    ])?;
    ensure_equal(&remember.status.code(), &Some(EXIT_SUCCESS), "remember")?;
    assert_stderr_empty(&remember, "remember")?;
    let remember_json = stdout_json(&remember)?;
    let memory_id = remember_json["data"]["memory_id"]
        .as_str()
        .ok_or_else(|| "remember response missing memory_id".to_string())?;

    let rebuild = run_ee(&["--workspace", &workspace, "index", "rebuild", "--json"])?;
    ensure_equal(&rebuild.status.code(), &Some(EXIT_SUCCESS), "index rebuild")?;
    assert_stderr_empty(&rebuild, "index rebuild")?;

    fs::write(&source_path, "Freshness source release evidence changed")
        .map_err(|error| error.to_string())?;

    let context = run_ee(&[
        "--workspace",
        &workspace,
        "pack",
        "freshness source release",
        "--max-tokens",
        "2000",
        "--json",
    ])?;
    ensure_equal(&context.status.code(), &Some(EXIT_SUCCESS), "context")?;
    assert_stderr_empty(&context, "context")?;
    let context_json = stdout_json(&context)?;
    assert_schema(&context_json, "ee.response.v2", "context")?;
    ensure(
        degraded_codes(&context_json).contains(&"context_evidence_freshness_changed_source"),
        "context should report changed source evidence freshness",
    )?;

    let why = run_ee(&["--workspace", &workspace, "why", memory_id, "--json"])?;
    ensure_equal(&why.status.code(), &Some(EXIT_SUCCESS), "why")?;
    assert_stderr_empty(&why, "why")?;
    let why_json = stdout_json(&why)?;
    assert_schema(&why_json, "ee.response.v2", "why")?;
    ensure(
        degraded_codes(&why_json).contains(&"why_evidence_freshness_changed_source"),
        "why should report changed source evidence freshness",
    )
}

#[test]
fn remember_creates_searchable_memory() -> TestResult {
    let tempdir = tempfile::tempdir().map_err(|e| e.to_string())?;
    let workspace = tempdir.path().to_string_lossy().to_string();

    // Init workspace
    let init = run_ee(&["--workspace", &workspace, "init", "--json"])?;
    ensure_equal(&init.status.code(), &Some(EXIT_SUCCESS), "init")?;

    // Remember a unique fact
    let unique_content = "Xylophone zebra quantum 12345 unique test phrase";
    let remember = run_ee(&[
        "--workspace",
        &workspace,
        "remember",
        unique_content,
        "--kind",
        "fact",
        "--json",
    ])?;
    ensure_equal(&remember.status.code(), &Some(EXIT_SUCCESS), "remember")?;

    // Search for the unique phrase
    let search = run_ee(&[
        "--workspace",
        &workspace,
        "search",
        "xylophone zebra quantum",
        "--json",
    ])?;
    ensure_equal(&search.status.code(), &Some(EXIT_SUCCESS), "search")?;

    let search_json = stdout_json(&search)?;
    let results = search_json
        .pointer("/data/results")
        .or_else(|| search_json.pointer("/data/hits"))
        .and_then(|r| r.as_array());

    ensure(
        results.map(|r| !r.is_empty()).unwrap_or(false),
        "search for unique content should find the remembered memory",
    )
}

#[test]
fn search_family_is_queryless_complete_scoped_and_redaction_safe() -> TestResult {
    let first = tempfile::tempdir().map_err(|error| error.to_string())?;
    let second = tempfile::tempdir().map_err(|error| error.to_string())?;
    let first_workspace = first.path().to_string_lossy().to_string();
    let second_workspace = second.path().to_string_lossy().to_string();
    let family_id = "release-matrix-2026-08";

    for workspace in [&first_workspace, &second_workspace] {
        let init = run_ee(&["--workspace", workspace, "init", "--json"])?;
        ensure_equal(
            &init.status.code(),
            &Some(EXIT_SUCCESS),
            "family workspace init",
        )?;
        assert_stderr_empty(&init, "family workspace init")?;
    }

    let source = "file:///Users/alice/private/attempt.md?api_key=redaction-fixture#L1";
    let raw_secret = "ghp_abcdefghijklmnopqrstuvwxyz1234567890";
    let attempts = [
        (1_u32, "selected", "Selected safe release procedure", false),
        (
            2_u32,
            "rejected",
            "Rejected attempt timed out safely",
            false,
        ),
        (3_u32, "rejected", raw_secret, true),
    ];
    let mut expected_memory_ids = Vec::new();
    for (attempt_index, disposition, content, allow_secret_mention) in attempts {
        let mut owned_args = vec![
            "--workspace".to_owned(),
            first_workspace.clone(),
            "remember".to_owned(),
            content.to_owned(),
            "--level".to_owned(),
            "semantic".to_owned(),
            "--kind".to_owned(),
            "fact".to_owned(),
            "--source".to_owned(),
            source.to_owned(),
            "--family".to_owned(),
            family_id.to_owned(),
            "--of-n".to_owned(),
            "3".to_owned(),
            "--attempt".to_owned(),
            attempt_index.to_string(),
            "--attempt-outcome".to_owned(),
            disposition.to_owned(),
            "--json".to_owned(),
        ];
        if allow_secret_mention {
            owned_args.push("--allow-secret-mention".to_owned());
        }
        let borrowed_args = owned_args.iter().map(String::as_str).collect::<Vec<_>>();
        let remember = run_ee(&borrowed_args)?;
        ensure_equal(
            &remember.status.code(),
            &Some(EXIT_SUCCESS),
            &format!("family remember slot {attempt_index}"),
        )?;
        assert_stderr_empty(&remember, &format!("family remember slot {attempt_index}"))?;
        let remember_json = stdout_json(&remember)?;
        expected_memory_ids
            .push(json_str(&remember_json, "/data/memory_id", "family remember")?.to_owned());
    }

    let second_remember = run_ee(&[
        "--workspace",
        &second_workspace,
        "remember",
        "Same family id in a different workspace",
        "--level",
        "semantic",
        "--kind",
        "fact",
        "--family",
        family_id,
        "--of-n",
        "1",
        "--attempt",
        "1",
        "--attempt-outcome",
        "selected",
        "--json",
    ])?;
    ensure_equal(
        &second_remember.status.code(),
        &Some(EXIT_SUCCESS),
        "second workspace family remember",
    )?;

    let family = run_ee(&[
        "--workspace",
        &first_workspace,
        "search",
        "--family",
        family_id,
        "--json",
    ])?;
    ensure_equal(
        &family.status.code(),
        &Some(EXIT_SUCCESS),
        "queryless family search",
    )?;
    assert_stderr_empty(&family, "queryless family search")?;
    let family_json = stdout_json(&family)?;
    assert_schema(&family_json, "ee.response.v2", "queryless family search")?;
    ensure_equal(
        &family_json.pointer("/data/schema"),
        &Some(&serde_json::json!("ee.search.family.v1")),
        "family payload schema",
    )?;
    for (pointer, expected) in [
        ("/data/declaredSize", 3_u64),
        ("/data/recordedSlots", 3),
        ("/data/selectedCount", 1),
        ("/data/rejectedCount", 2),
        ("/data/unrecordedCount", 0),
        ("/data/scopeFilteredCount", 0),
    ] {
        ensure_equal(
            &family_json
                .pointer(pointer)
                .and_then(serde_json::Value::as_u64),
            &Some(expected),
            pointer,
        )?;
    }
    ensure_equal(
        &family_json
            .pointer("/data/promotionEligible")
            .and_then(serde_json::Value::as_bool),
        &Some(true),
        "complete family promotion posture",
    )?;
    ensure_equal(
        &family_json
            .pointer("/data/promotionPosture")
            .and_then(serde_json::Value::as_str),
        &Some("eligible"),
        "complete family typed promotion posture",
    )?;
    let members = json_array(&family_json, "/data/members", "family search")?;
    ensure_equal(&members.len(), &3_usize, "family member count")?;
    ensure(
        members.iter().all(|member| {
            member
                .get("discountFactor")
                .and_then(serde_json::Value::as_f64)
                == Some(1.0)
        }),
        "canonical family members are undiscounted",
    )?;
    let observed_slots = members
        .iter()
        .filter_map(|member| {
            member
                .get("attemptIndex")
                .and_then(serde_json::Value::as_u64)
        })
        .collect::<Vec<_>>();
    ensure_equal(
        &observed_slots,
        &vec![1_u64, 2, 3],
        "deterministic family slot order",
    )?;
    let observed_ids = members
        .iter()
        .filter_map(|member| member.get("memoryId").and_then(serde_json::Value::as_str))
        .map(str::to_owned)
        .collect::<Vec<_>>();
    ensure_equal(
        &observed_ids,
        &expected_memory_ids,
        "workspace-local family member ids",
    )?;
    ensure(
        members.iter().all(|member| {
            member
                .get("logicalId")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|logical_id| !logical_id.is_empty())
        }),
        "every family member must expose revision-stable lineage",
    )?;
    ensure_equal(
        &members[2]
            .get("contentRedacted")
            .and_then(serde_json::Value::as_bool),
        &Some(true),
        "secret-like rejected member redaction posture",
    )?;
    ensure(
        members[2]
            .get("content")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|content| content.contains("[REDACTED:")),
        "secret-like rejected member must render a redaction marker",
    )?;
    let serialized_family =
        serde_json::to_string(&family_json).map_err(|error| error.to_string())?;
    ensure(
        !serialized_family.contains(raw_secret)
            && !serialized_family.contains("redaction-fixture")
            && !serialized_family.contains("/Users/alice"),
        "family output must not project raw content secrets or local provenance paths",
    )?;
    ensure(
        members.iter().all(|member| {
            member
                .get("provenanceRedacted")
                .and_then(serde_json::Value::as_bool)
                == Some(true)
        }),
        "every local secret-bearing provenance URI must report redaction",
    )?;

    let strict = run_ee(&[
        "--workspace",
        &first_workspace,
        "search",
        "--family",
        family_id,
        "--memory-scope",
        "self",
        "--strict-scope",
        "--json",
    ])?;
    ensure_equal(
        &strict.status.code(),
        &Some(EXIT_SUCCESS),
        "strict family search",
    )?;
    let strict_json = stdout_json(&strict)?;
    ensure_equal(
        &strict_json.pointer("/data/members"),
        &Some(&serde_json::json!([])),
        "strict family search fails closed",
    )?;
    ensure_equal(
        &strict_json
            .pointer("/data/scopeFilteredCount")
            .and_then(serde_json::Value::as_u64),
        &Some(3),
        "strict family excluded count",
    )?;

    let isolated = run_ee(&[
        "--workspace",
        &second_workspace,
        "search",
        "--family",
        family_id,
        "--json",
    ])?;
    ensure_equal(
        &isolated.status.code(),
        &Some(EXIT_SUCCESS),
        "second workspace family search",
    )?;
    let isolated_json = stdout_json(&isolated)?;
    ensure_equal(
        &isolated_json
            .pointer("/data/members")
            .and_then(serde_json::Value::as_array)
            .map(Vec::len),
        &Some(1),
        "same family id remains workspace-isolated",
    )
}

#[test]
fn search_family_exposes_incomplete_discounts_and_unslotted_legacy_posture() -> TestResult {
    let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
    let workspace = temp.path().to_string_lossy().to_string();
    let init = run_ee(&["--workspace", &workspace, "init", "--json"])?;
    ensure_equal(&init.status.code(), &Some(EXIT_SUCCESS), "family init")?;
    assert_stderr_empty(&init, "family init")?;

    for (attempt, disposition, content) in [
        ("1", "selected", "Selected partial family member"),
        ("2", "rejected", "Rejected partial family evidence"),
    ] {
        let remember = run_ee(&[
            "--workspace",
            &workspace,
            "remember",
            content,
            "--level",
            "semantic",
            "--kind",
            "fact",
            "--family",
            "fam-partial-discount",
            "--of-n",
            "3",
            "--attempt",
            attempt,
            "--attempt-outcome",
            disposition,
            "--json",
        ])?;
        ensure_equal(
            &remember.status.code(),
            &Some(EXIT_SUCCESS),
            "partial family remember",
        )?;
        assert_stderr_empty(&remember, "partial family remember")?;
    }
    let partial = run_ee(&[
        "--workspace",
        &workspace,
        "search",
        "--family",
        "fam-partial-discount",
        "--json",
    ])?;
    ensure_equal(
        &partial.status.code(),
        &Some(EXIT_SUCCESS),
        "partial family search",
    )?;
    assert_stderr_empty(&partial, "partial family search")?;
    let partial_json = stdout_json(&partial)?;
    ensure_equal(
        &partial_json
            .pointer("/data/promotionPosture")
            .and_then(serde_json::Value::as_str),
        &Some("blocked_incomplete"),
        "partial family posture",
    )?;
    let partial_members = json_array(&partial_json, "/data/members", "partial family")?;
    let selected_discount = partial_members
        .iter()
        .find(|member| {
            member
                .get("disposition")
                .and_then(serde_json::Value::as_str)
                == Some("selected")
        })
        .and_then(|member| member.get("discountFactor"))
        .and_then(serde_json::Value::as_f64);
    ensure(
        selected_discount.is_some_and(|factor| (factor - 1.0 / 3.0).abs() < 1.0e-7),
        "selected incomplete member remains exactly 1/N",
    )?;
    let rejected_discount = partial_members
        .iter()
        .find(|member| {
            member
                .get("disposition")
                .and_then(serde_json::Value::as_str)
                == Some("rejected")
        })
        .and_then(|member| member.get("discountFactor"))
        .and_then(serde_json::Value::as_f64);
    ensure_equal(
        &rejected_discount,
        &Some(1.0),
        "rejected evidence is never discounted",
    )?;

    let unslotted = run_ee(&[
        "--workspace",
        &workspace,
        "remember",
        "Pointer-only legacy family member",
        "--level",
        "semantic",
        "--kind",
        "fact",
        "--family",
        "fam-unslotted-search",
        "--of-n",
        "3",
        "--json",
    ])?;
    ensure_equal(
        &unslotted.status.code(),
        &Some(EXIT_SUCCESS),
        "unslotted family remember",
    )?;
    assert_stderr_empty(&unslotted, "unslotted family remember")?;
    let search = run_ee(&[
        "--workspace",
        &workspace,
        "search",
        "--family",
        "fam-unslotted-search",
        "--json",
    ])?;
    ensure_equal(
        &search.status.code(),
        &Some(EXIT_SUCCESS),
        "unslotted family search",
    )?;
    assert_stderr_empty(&search, "unslotted family search")?;
    let search_json = stdout_json(&search)?;
    ensure_equal(
        &search_json
            .pointer("/data/promotionPosture")
            .and_then(serde_json::Value::as_str),
        &Some("blocked_unslotted_members"),
        "unslotted family fails closed",
    )?;
    ensure_equal(
        &search_json
            .pointer("/data/unslottedCount")
            .and_then(serde_json::Value::as_u64),
        &Some(1),
        "unslotted family count",
    )?;
    ensure_equal(
        &search_json.pointer("/data/members/0/attemptIndex"),
        &Some(&serde_json::Value::Null),
        "unslotted member does not invent a slot",
    )?;
    ensure_equal(
        &search_json.pointer("/data/members/0/disposition"),
        &Some(&serde_json::Value::Null),
        "unslotted member does not invent a disposition",
    )
}

#[test]
fn context_pack_includes_relevant_memories() -> TestResult {
    let tempdir = tempfile::tempdir().map_err(|e| e.to_string())?;
    let workspace = tempdir.path().to_string_lossy().to_string();

    let init = run_ee(&["--workspace", &workspace, "init", "--json"])?;
    persist_artifact("pack_context_init", &init);
    ensure_equal(&init.status.code(), &Some(EXIT_SUCCESS), "init")?;

    let memories = [
        (
            "procedural",
            "rule",
            "Always run unit tests before committing code.",
        ),
        (
            "procedural",
            "rule",
            "Run integration tests for happy path and edge cases.",
        ),
        (
            "procedural",
            "command",
            "Use cargo test --all-targets when validating release readiness.",
        ),
        (
            "semantic",
            "fact",
            "The database schema is defined by the ee migration layer.",
        ),
        (
            "semantic",
            "convention",
            "Testing output must keep JSON stdout clean and diagnostics on stderr.",
        ),
        (
            "semantic",
            "decision",
            "Context packs must include provenance for every selected memory.",
        ),
        (
            "episodic",
            "failure",
            "A prior release failed because formatting checks were skipped.",
        ),
        (
            "episodic",
            "fact",
            "A search regression once hid relevant testing guidance behind low scores.",
        ),
        (
            "working",
            "fact",
            "Current test work is strengthening pack and context evidence checks.",
        ),
        (
            "working",
            "risk",
            "Small token budgets may omit lower utility memories but must explain omissions.",
        ),
    ];

    let mut remembered = BTreeMap::new();
    for (index, (level, kind, content)) in memories.iter().copied().enumerate() {
        let source_path = tempdir.path().join(format!("memory-source-{index}.md"));
        fs::write(&source_path, content).map_err(|error| error.to_string())?;
        let source_uri = format!("file://{}#L1", source_path.display());

        let remember = run_ee(&[
            "--workspace",
            &workspace,
            "remember",
            content,
            "--level",
            level,
            "--kind",
            kind,
            "--source",
            &source_uri,
            "--json",
        ])?;
        persist_artifact(&format!("pack_context_remember_{index}"), &remember);
        ensure_equal(
            &remember.status.code(),
            &Some(EXIT_SUCCESS),
            &format!("remember {index}"),
        )?;
        assert_stderr_empty(&remember, &format!("remember {index}"))?;
        let remember_json = stdout_json(&remember)?;
        persist_json_artifact(&format!("pack_context_remember_{index}"), &remember_json)?;
        let memory_id = json_str(&remember_json, "/data/memory_id", "remember")?.to_owned();
        remembered.insert(
            memory_id,
            RememberedMemory {
                level: level.to_string(),
                kind: kind.to_string(),
                content: content.to_string(),
                source_uri,
            },
        );
    }

    ensure_equal(&remembered.len(), &10_usize, "remembered memory count")?;

    let mut selected_memory_ids = BTreeSet::new();

    for max_tokens in ["800", "4000"] {
        let context = run_ee(&[
            "--workspace",
            &workspace,
            "pack",
            "testing release readiness provenance",
            "--max-tokens",
            max_tokens,
            "--json",
        ])?;
        persist_artifact(&format!("pack_context_context_{max_tokens}"), &context);
        ensure_equal(
            &context.status.code(),
            &Some(EXIT_SUCCESS),
            &format!("context {max_tokens} exit"),
        )?;
        assert_stderr_empty(&context, &format!("context {max_tokens}"))?;
        let context_json = stdout_json(&context)?;
        persist_json_artifact(&format!("pack_context_context_{max_tokens}"), &context_json)?;
        assert_schema(&context_json, "ee.response.v2", "context")?;
        let requested_tokens = max_tokens
            .parse::<u64>()
            .map_err(|error| error.to_string())?;
        let budget_max_tokens = context_json
            .pointer("/data/pack/budget/maxTokens")
            .and_then(serde_json::Value::as_u64)
            .ok_or_else(|| format!("context {max_tokens}: maxTokens must be an integer"))?;
        ensure_equal(
            &budget_max_tokens,
            &requested_tokens,
            &format!("context {max_tokens} budget maxTokens"),
        )?;
        let used_tokens = context_json
            .pointer("/data/pack/budget/usedTokens")
            .and_then(serde_json::Value::as_u64)
            .ok_or_else(|| format!("context {max_tokens}: usedTokens must be an integer"))?;
        ensure(
            used_tokens <= requested_tokens,
            format!("context {max_tokens} usedTokens must not exceed maxTokens"),
        )?;

        let items = json_array(&context_json, "/data/pack/items", "context")?;
        ensure(
            !items.is_empty(),
            format!("context {max_tokens} should select at least one item"),
        )?;

        for item in items {
            let memory_id = json_str(item, "/memoryId", "context item")?;
            let stored = remembered.get(memory_id).ok_or_else(|| {
                format!("context selected unknown memory id {memory_id}; item={item:?}")
            })?;
            selected_memory_ids.insert(memory_id.to_string());

            ensure(
                item.get("content")
                    .and_then(serde_json::Value::as_str)
                    .is_some_and(|packed| packed == stored.content),
                format!("packed content for {memory_id} must match stored memory"),
            )?;
            ensure(
                item.get("why")
                    .and_then(serde_json::Value::as_str)
                    .is_some_and(|why| !why.trim().is_empty()),
                format!("context item {memory_id} must include non-empty why"),
            )?;

            let provenance = item
                .get("provenance")
                .and_then(serde_json::Value::as_array)
                .ok_or_else(|| format!("context item {memory_id} missing provenance[]"))?;
            ensure(
                provenance.iter().any(|entry| {
                    entry
                        .get("uri")
                        .and_then(serde_json::Value::as_str)
                        .is_some_and(|uri| uri == stored.source_uri)
                }),
                format!(
                    "context item {memory_id} provenance must include {}",
                    stored.source_uri
                ),
            )?;
        }
    }

    ensure(
        !selected_memory_ids.is_empty(),
        "context should select at least one remembered memory across budgets",
    )?;

    for memory_id in selected_memory_ids {
        let stored = remembered
            .get(&memory_id)
            .ok_or_else(|| format!("selected memory {memory_id} was not remembered"))?;
        let why = run_ee(&["--workspace", &workspace, "why", &memory_id, "--json"])?;
        persist_artifact(&format!("pack_context_why_{memory_id}"), &why);
        ensure_equal(
            &why.status.code(),
            &Some(EXIT_SUCCESS),
            &format!("why {memory_id} exit"),
        )?;
        assert_stderr_empty(&why, &format!("why {memory_id}"))?;
        let why_json = stdout_json(&why)?;
        persist_json_artifact(&format!("pack_context_why_{memory_id}"), &why_json)?;
        assert_schema(&why_json, "ee.response.v2", "why")?;
        ensure_equal(
            &json_str(&why_json, "/data/memoryId", "why")?,
            &memory_id.as_str(),
            &format!("why {memory_id} memoryId"),
        )?;
        ensure_equal(
            &why_json
                .pointer("/data/found")
                .and_then(serde_json::Value::as_bool),
            &Some(true),
            &format!("why {memory_id} found"),
        )?;
        ensure_equal(
            &json_str(&why_json, "/data/storage/provenanceUri", "why")?,
            &stored.source_uri.as_str(),
            &format!("why {memory_id} provenanceUri"),
        )?;
        ensure_equal(
            &json_str(&why_json, "/data/retrieval/level", "why")?,
            &stored.level.as_str(),
            &format!("why {memory_id} level"),
        )?;
        ensure_equal(
            &json_str(&why_json, "/data/retrieval/kind", "why")?,
            &stored.kind.as_str(),
            &format!("why {memory_id} kind"),
        )?;
        ensure(
            json_str(&why_json, "/data/selection/latestPackSelection/why", "why")
                .is_ok_and(|why| !why.trim().is_empty()),
            format!("why {memory_id} should include latest pack selection rationale"),
        )?;
    }

    Ok(())
}
