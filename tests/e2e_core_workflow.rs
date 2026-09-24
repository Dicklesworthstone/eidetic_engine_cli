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
use std::path::{Path, PathBuf};
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

fn run_ee_as_agent(args: &[&str]) -> Result<Output, String> {
    Command::new(env!("CARGO_BIN_EXE_ee"))
        .args(args)
        .env_remove("EE_WORKSPACE")
        .env_remove("EE_WORKSPACE_REGISTRY")
        .env("EE_AGENT_NAME", "GreenOsprey")
        .output()
        .map_err(|error| format!("failed to run ee {}: {error}", args.join(" ")))
}

fn run_ee_with_home(args: &[&str], home: &Path) -> Result<Output, String> {
    Command::new(env!("CARGO_BIN_EXE_ee"))
        .args(args)
        .env_remove("EE_WORKSPACE")
        .env_remove("EE_WORKSPACE_REGISTRY")
        .env_remove("EE_AGENT_NAME")
        .env("EE_EMBED_DOWNLOAD", "off")
        .env("HOME", home)
        .env("USERPROFILE", home)
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
fn remember_accepts_sec_identifiers_preserves_pii_refusal_and_loads_user_allow_regex() -> TestResult
{
    let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
    let workspace_path = tempdir.path().join("workspace");
    let home_path = tempdir.path().join("home");
    let user_config_dir = home_path.join(".config").join("ee");
    fs::create_dir_all(&workspace_path).map_err(|error| error.to_string())?;
    fs::create_dir_all(&user_config_dir).map_err(|error| error.to_string())?;
    fs::write(
        user_config_dir.join("config.toml"),
        "[policy.secret_detector]\nallow_regex = ['212-555-0199']\n",
    )
    .map_err(|error| error.to_string())?;
    let workspace = workspace_path.to_string_lossy().to_string();

    let init = run_ee_with_home(&["--workspace", &workspace, "init", "--json"], &home_path)?;
    ensure_equal(
        &init.status.code(),
        &Some(EXIT_SUCCESS),
        "SEC identifier workspace init",
    )?;
    assert_stderr_empty(&init, "SEC identifier workspace init")?;

    for (label, content) in [
        ("zero-padded CIK", "Fund Alpha SEC CIK 0001720116."),
        ("unpadded labeled CIK", "Fund Beta CIK 1720116."),
        (
            "SEC accession",
            "Issuer C 10-K accession 0001957132-26-000015 filed.",
        ),
        (
            "inline canonical CIK",
            "The inline canonical CIK is 0001957132 in this filing note.",
        ),
    ] {
        let remember = run_ee_with_home(
            &[
                "--workspace",
                &workspace,
                "remember",
                content,
                "--level",
                "semantic",
                "--kind",
                "fact",
                "--json",
            ],
            &home_path,
        )?;
        ensure_equal(
            &remember.status.code(),
            &Some(EXIT_SUCCESS),
            &format!("{label} remember exit"),
        )?;
        assert_stderr_empty(&remember, &format!("{label} remember"))?;
        let json = stdout_json(&remember)?;
        ensure_equal(
            &json.pointer("/data/content"),
            &Some(&serde_json::Value::String(content.to_owned())),
            &format!("{label} content round trip"),
        )?;
        ensure_equal(
            &json.pointer("/data/persisted"),
            &Some(&serde_json::Value::Bool(true)),
            &format!("{label} persisted"),
        )?;
    }

    for (label, content) in [
        ("dashed NANP", "Contact 646-555-0123."),
        ("dotted NANP", "Contact 646.555.0123."),
        ("bare NANP", "Contact 6465550123."),
        ("SSN", "Taxpayer SSN 123-45-6789."),
    ] {
        let remember = run_ee_with_home(
            &[
                "--workspace",
                &workspace,
                "remember",
                content,
                "--level",
                "semantic",
                "--kind",
                "fact",
                "--json",
            ],
            &home_path,
        )?;
        ensure(
            !remember.status.success(),
            format!("{label} must remain refused"),
        )?;
        let json = stdout_json(&remember)?;
        ensure_equal(
            &json.pointer("/error/code"),
            &Some(&serde_json::Value::String("policy_denied".to_owned())),
            &format!("{label} refusal code"),
        )?;
    }

    let user_allowed = run_ee_with_home(
        &[
            "--workspace",
            &workspace,
            "remember",
            "Configured documentation contact 212-555-0199.",
            "--level",
            "semantic",
            "--kind",
            "fact",
            "--json",
        ],
        &home_path,
    )?;
    ensure_equal(
        &user_allowed.status.code(),
        &Some(EXIT_SUCCESS),
        "user allow_regex remember exit",
    )?;
    let allowed_json = stdout_json(&user_allowed)?;
    ensure_equal(
        &allowed_json.pointer("/data/policy_bypass_used"),
        &Some(&serde_json::Value::Bool(true)),
        "user allow_regex bypass used",
    )?;
    ensure_equal(
        &allowed_json.pointer("/data/policy_bypass/kind"),
        &Some(&serde_json::Value::String("config_regex".to_owned())),
        "user allow_regex bypass kind",
    )?;

    fs::write(
        user_config_dir.join("config.toml"),
        "[policy.secret_detector]\nallow_regex = ['(']\n",
    )
    .map_err(|error| error.to_string())?;
    let malformed = run_ee_with_home(
        &[
            "--workspace",
            &workspace,
            "remember",
            "Malformed regex must not silently allow 212-555-0199.",
            "--level",
            "semantic",
            "--kind",
            "fact",
            "--json",
        ],
        &home_path,
    )?;
    ensure(
        !malformed.status.success(),
        "malformed user allow_regex must fail explicitly",
    )?;
    let malformed_json = stdout_json(&malformed)?;
    ensure_equal(
        &malformed_json.pointer("/error/code"),
        &Some(&serde_json::Value::String("configuration".to_owned())),
        "malformed user allow_regex error code",
    )?;
    let malformed_message = malformed_json
        .pointer("/error/message")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| format!("malformed allow_regex error lacks a message: {malformed_json}"))?;
    ensure(
        malformed_message.contains("policy.secret_detector.allow_regex")
            && malformed_message.contains("invalid value")
            && malformed_message.contains("valid regex"),
        format!("malformed allow_regex error must name the key and regex defect: {malformed_json}"),
    )
}

#[test]
fn remember_three_memories_keeps_search_index_fresh_without_manual_rebuild() -> TestResult {
    let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
    let workspace = tempdir.path().to_string_lossy().to_string();

    let init = run_ee(&["--workspace", &workspace, "init", "--json"])?;
    ensure_equal(&init.status.code(), &Some(EXIT_SUCCESS), "init exit code")?;
    assert_stderr_empty(&init, "init")?;

    let mut memory_ids = BTreeSet::new();
    for content in [
        "Freshness cohort marker alpha release evidence.",
        "Freshness cohort marker beta release evidence.",
        "Freshness cohort marker gamma release evidence.",
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
            "--no-auto-link",
            "--json",
        ])?;
        ensure_equal(
            &remember.status.code(),
            &Some(EXIT_SUCCESS),
            "remember exit code",
        )?;
        assert_stderr_empty(&remember, "remember")?;
        let remember_json = stdout_json(&remember)?;
        assert_schema(&remember_json, "ee.response.v2", "remember")?;
        ensure_equal(
            &remember_json.pointer("/data/index_status"),
            &Some(&serde_json::json!("indexed")),
            "remember synchronously indexes the stored memory",
        )?;
        memory_ids
            .insert(json_str(&remember_json, "/data/memory_id", "remember memory id")?.to_owned());
        if memory_ids.len() == 1 {
            let first_status = run_ee(&["--workspace", &workspace, "index", "status", "--json"])?;
            ensure_equal(
                &first_status.status.code(),
                &Some(EXIT_SUCCESS),
                "first-write index status exit code",
            )?;
            let first_status_json = stdout_json(&first_status)?;
            ensure_equal(
                &first_status_json.pointer("/data/indexGeneration"),
                &first_status_json.pointer("/data/dbGeneration"),
                "first remember keeps index generation equal to database generation",
            )?;
        }
    }
    ensure_equal(&memory_ids.len(), &3usize, "three distinct memories")?;

    let status = run_ee(&["--workspace", &workspace, "index", "status", "--json"])?;
    ensure_equal(
        &status.status.code(),
        &Some(EXIT_SUCCESS),
        "index status exit code",
    )?;
    assert_stderr_empty(&status, "index status")?;
    let status_json = stdout_json(&status)?;
    assert_schema(&status_json, "ee.response.v2", "index status")?;
    ensure_equal(
        &status_json.pointer("/data/health"),
        &Some(&serde_json::json!("ready")),
        "index health after remembers",
    )?;
    ensure_equal(
        &status_json.pointer("/data/indexDocumentCount"),
        &Some(&serde_json::Value::from(3)),
        "index contains all remembered documents",
    )?;
    ensure_equal(
        &status_json.pointer("/data/indexDocumentCounts/memories"),
        &Some(&serde_json::Value::from(3)),
        "memory index count after remembers",
    )?;
    ensure_equal(
        &status_json.pointer("/data/indexGeneration"),
        &status_json.pointer("/data/dbGeneration"),
        "remember keeps index generation equal to database generation",
    )?;
    ensure_equal(
        &status_json.pointer("/data/actualCorpusRevision"),
        &status_json.pointer("/data/expectedCorpusRevision"),
        "remember keeps the indexed corpus revision current",
    )?;

    let search = run_ee(&[
        "--workspace",
        &workspace,
        "search",
        "freshness cohort marker",
        "--source-mode",
        "lexical_only",
        "--limit",
        "10",
        "--json",
    ])?;
    ensure_equal(
        &search.status.code(),
        &Some(EXIT_SUCCESS),
        "search exit code",
    )?;
    assert_stderr_empty(&search, "search")?;
    let search_json = stdout_json(&search)?;
    assert_schema(&search_json, "ee.response.v2", "search")?;
    let result_ids = json_array(&search_json, "/data/results", "search results")?
        .iter()
        .filter_map(|result| result.get("docId").and_then(serde_json::Value::as_str))
        .collect::<BTreeSet<_>>();
    ensure(
        memory_ids
            .iter()
            .all(|memory_id| result_ids.contains(memory_id.as_str())),
        format!("search must return all three newly indexed memories: {search_json}"),
    )?;
    ensure(
        !degraded_codes(&search_json)
            .iter()
            .any(|code| *code == "search_index_stale" || *code == "index_stale"),
        format!("fresh common path must not report a stale index: {search_json}"),
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
fn memory_list_and_show_round_trip_local_provenance_uri() -> TestResult {
    let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
    let workspace = tempdir.path().to_string_lossy().to_string();
    let source_path = tempdir.path().join("SN__SYNTHESIZED_REPORT.md");
    fs::write(&source_path, "Local provenance round-trip evidence.")
        .map_err(|error| error.to_string())?;
    let source_uri = format!("file://{}", source_path.display());

    let init = run_ee(&["--workspace", &workspace, "init", "--json"])?;
    ensure_equal(&init.status.code(), &Some(EXIT_SUCCESS), "init")?;
    assert_stderr_empty(&init, "init")?;

    let remember = run_ee(&[
        "--workspace",
        &workspace,
        "remember",
        "Local provenance round-trip evidence.",
        "--level",
        "semantic",
        "--kind",
        "fact",
        "--source",
        &source_uri,
        "--json",
    ])?;
    ensure_equal(&remember.status.code(), &Some(EXIT_SUCCESS), "remember")?;
    assert_stderr_empty(&remember, "remember")?;
    let remember_json = stdout_json(&remember)?;
    let memory_id = json_str(&remember_json, "/data/memory_id", "remember")?;
    ensure_equal(
        &json_str(&remember_json, "/data/provenance_uri", "remember")?,
        &source_uri.as_str(),
        "remember provenance URI",
    )?;

    let list = run_ee(&["--workspace", &workspace, "memory", "list", "--json"])?;
    ensure_equal(&list.status.code(), &Some(EXIT_SUCCESS), "memory list")?;
    assert_stderr_empty(&list, "memory list")?;
    let list_json = stdout_json(&list)?;
    let listed = json_array(&list_json, "/data/memories", "memory list")?
        .iter()
        .find(|memory| memory.get("id").and_then(serde_json::Value::as_str) == Some(memory_id))
        .ok_or_else(|| format!("memory list omitted remembered memory {memory_id}"))?;
    ensure_equal(
        &listed
            .get("provenance_uri")
            .and_then(serde_json::Value::as_str),
        &Some(source_uri.as_str()),
        "memory list provenance URI",
    )?;

    let show = run_ee(&[
        "--workspace",
        &workspace,
        "memory",
        "show",
        memory_id,
        "--json",
    ])?;
    ensure_equal(&show.status.code(), &Some(EXIT_SUCCESS), "memory show")?;
    assert_stderr_empty(&show, "memory show")?;
    let show_json = stdout_json(&show)?;
    ensure_equal(
        &json_str(&show_json, "/data/memory/provenance_uri", "memory show")?,
        &source_uri.as_str(),
        "memory show provenance URI",
    )
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
fn memory_list_tag_filter_round_trips_mixed_case_without_reindex() -> TestResult {
    let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
    let workspace = tempdir.path().to_string_lossy().to_string();

    let init = run_ee(&["--workspace", &workspace, "init", "--json"])?;
    ensure_equal(&init.status.code(), &Some(EXIT_SUCCESS), "tag fixture init")?;

    let remember = run_ee(&[
        "--workspace",
        &workspace,
        "remember",
        "Mixed-case tag round-trip fixture",
        "--level",
        "semantic",
        "--kind",
        "fact",
        "--tags",
        "ticker:ZZZZ,screening-probe",
        "--json",
    ])?;
    ensure_equal(
        &remember.status.code(),
        &Some(EXIT_SUCCESS),
        "tag fixture remember",
    )?;
    let remember_json = stdout_json(&remember)?;
    let remembered_id =
        json_str(&remember_json, "/data/memory_id", "tag fixture remember")?.to_owned();

    let list_ids = |json: &serde_json::Value, context: &str| -> Result<BTreeSet<String>, String> {
        json_array(json, "/data/memories", context)?
            .iter()
            .map(|memory| {
                memory
                    .get("id")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_owned)
                    .ok_or_else(|| format!("{context}: memory entry must contain a string id"))
            })
            .collect()
    };

    let lower = run_ee(&[
        "--workspace",
        &workspace,
        "memory",
        "list",
        "--tag",
        "ticker:zzzz",
        "--json",
    ])?;
    ensure_equal(
        &lower.status.code(),
        &Some(EXIT_SUCCESS),
        "lowercase tag list",
    )?;
    let lower_json = stdout_json(&lower)?;
    let lower_ids = list_ids(&lower_json, "lowercase tag list")?;

    let upper = run_ee(&[
        "--workspace",
        &workspace,
        "memory",
        "list",
        "--tag",
        "TICKER:ZZZZ",
        "--json",
    ])?;
    ensure_equal(
        &upper.status.code(),
        &Some(EXIT_SUCCESS),
        "uppercase tag list",
    )?;
    let upper_json = stdout_json(&upper)?;
    let upper_ids = list_ids(&upper_json, "uppercase tag list")?;

    ensure_equal(
        &upper_ids,
        &lower_ids,
        "uppercase and lowercase tag filters return identical rows",
    )?;
    ensure_equal(
        &lower_ids,
        &BTreeSet::from([remembered_id.clone()]),
        "case-insensitive filter returns the remembered row",
    )?;

    let hyphenated = run_ee(&[
        "--workspace",
        &workspace,
        "memory",
        "list",
        "--tag",
        "SCREENING-PROBE",
        "--json",
    ])?;
    ensure_equal(
        &hyphenated.status.code(),
        &Some(EXIT_SUCCESS),
        "hyphenated tag list",
    )?;
    let hyphenated_json = stdout_json(&hyphenated)?;
    ensure_equal(
        &list_ids(&hyphenated_json, "hyphenated tag list")?,
        &BTreeSet::from([remembered_id]),
        "hyphenated tag round-trips through the same case rules",
    )?;

    let underscored = run_ee(&[
        "--workspace",
        &workspace,
        "memory",
        "list",
        "--tag",
        "screening_probe",
        "--json",
    ])?;
    ensure_equal(
        &underscored.status.code(),
        &Some(EXIT_SUCCESS),
        "underscore tag list",
    )?;
    let underscored_json = stdout_json(&underscored)?;
    ensure(
        list_ids(&underscored_json, "underscore tag list")?.is_empty(),
        "underscore tag must remain distinct from the stored hyphenated tag",
    )
}

#[test]
fn search_family_is_queryless_complete_scoped_and_redaction_safe() -> TestResult {
    let first = tempfile::tempdir().map_err(|error| error.to_string())?;
    let second = tempfile::tempdir().map_err(|error| error.to_string())?;
    let first_workspace = first.path().to_string_lossy().to_string();
    let second_workspace = second.path().to_string_lossy().to_string();
    let family_id = "AKIAIOSFODNN7EXAMPLE";

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
    persist_artifact("family_complete", &family);
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
    ensure(
        family_json
            .pointer("/data/familyAlias")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|alias| alias.starts_with("afm_") && alias.len() == 36),
        "family search exposes only a stable domain-separated alias",
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
            && !serialized_family.contains(family_id)
            && !serialized_family.contains("AKIA")
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
    )?;

    let missing = run_ee(&[
        "--workspace",
        &first_workspace,
        "search",
        "--family",
        "fam-no-recorded-attempts",
        "--json",
    ])?;
    persist_artifact("family_missing", &missing);
    ensure_equal(
        &missing.status.code(),
        &Some(EXIT_SUCCESS),
        "empty family read snapshot",
    )?;
    assert_stderr_empty(&missing, "empty family read snapshot")?;
    let missing_json = stdout_json(&missing)?;
    ensure_equal(
        &missing_json.pointer("/data/members"),
        &Some(&serde_json::json!([])),
        "unknown family cannot borrow another family's members",
    )?;
    ensure_equal(
        &missing_json.pointer("/data/promotionPosture"),
        &Some(&serde_json::json!("blocked_undeclared")),
        "unknown family cannot acquire declared promotion eligibility",
    )?;

    let repeated = run_ee(&[
        "--workspace",
        &first_workspace,
        "search",
        "--family",
        family_id,
        "--json",
    ])?;
    persist_artifact("family_repeated", &repeated);
    ensure_equal(
        &repeated.status.code(),
        &Some(EXIT_SUCCESS),
        "repeat family search after scoped and empty reads",
    )?;
    assert_stderr_empty(&repeated, "repeat family search")?;
    ensure_equal(
        &stdout_json(&repeated)?,
        &family_json,
        "independent read snapshots preserve the complete family response",
    )
}

#[test]
fn search_family_exposes_incomplete_discounts_and_unslotted_legacy_posture() -> TestResult {
    let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
    let workspace = temp.path().to_string_lossy().to_string();
    let family_id = "AKIAIOSFODNN7EXAMPLE";
    let init = run_ee(&["--workspace", &workspace, "init", "--json"])?;
    ensure_equal(&init.status.code(), &Some(EXIT_SUCCESS), "family init")?;
    assert_stderr_empty(&init, "family init")?;

    let human_remember = run_ee_as_agent(&[
        "--workspace",
        &workspace,
        "remember",
        "Dry-run selected family member",
        "--level",
        "semantic",
        "--kind",
        "fact",
        "--family",
        family_id,
        "--of-n",
        "3",
        "--attempt",
        "1",
        "--attempt-outcome",
        "selected",
        "--dry-run",
    ])?;
    ensure_equal(
        &human_remember.status.code(),
        &Some(EXIT_SUCCESS),
        "human dry-run family remember",
    )?;
    assert_stderr_empty(&human_remember, "human dry-run family remember")?;
    let human_stdout =
        String::from_utf8(human_remember.stdout).map_err(|error| error.to_string())?;
    ensure(
        human_stdout.contains("afm_") && !human_stdout.contains(family_id),
        "human remember output exposes only an opaque family alias",
    )?;

    let mut selected_memory_id = None;
    let mut rejected_memory_id = None;
    for (attempt, disposition, content) in [
        ("1", "selected", "Selected partial family member"),
        ("2", "rejected", "Rejected partial family evidence"),
    ] {
        let remember = run_ee_as_agent(&[
            "--workspace",
            &workspace,
            "remember",
            content,
            "--level",
            "semantic",
            "--kind",
            "fact",
            "--family",
            family_id,
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
        let remember_json = stdout_json(&remember)?;
        ensure(
            remember_json
                .pointer("/data/attemptFamily/familyAlias")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|alias| alias.starts_with("afm_") && alias.len() == 36)
                && remember_json
                    .pointer("/data/attemptFamily/familyId")
                    .is_none()
                && !serde_json::to_string(&remember_json)
                    .map_err(|error| error.to_string())?
                    .contains(family_id),
            "public remember output exposes only the family alias, never the raw secret-shaped id",
        )?;
        let memory_id = json_str(&remember_json, "/data/memory_id", "partial remember")?;
        if disposition == "selected" {
            selected_memory_id = Some(memory_id.to_owned());
        } else {
            rejected_memory_id = Some(memory_id.to_owned());
        }
    }
    let partial = run_ee(&[
        "--workspace",
        &workspace,
        "search",
        "--family",
        family_id,
        "--json",
    ])?;
    persist_artifact("family_incomplete", &partial);
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

    let trust = run_ee(&["--workspace", &workspace, "trust", "report", "--json"])?;
    persist_artifact("family_incomplete_trust", &trust);
    ensure_equal(
        &trust.status.code(),
        &Some(EXIT_SUCCESS),
        &format!(
            "multiplicity-aware trust report; stdout={}; stderr={}",
            String::from_utf8_lossy(&trust.stdout),
            String::from_utf8_lossy(&trust.stderr),
        ),
    )?;
    assert_stderr_empty(&trust, "multiplicity-aware trust report")?;
    let trust_json = stdout_json(&trust)?;
    let family_rows = json_array(
        &trust_json,
        "/data/attemptFamilies/families",
        "multiplicity-aware trust report",
    )?;
    let family_row = family_rows
        .iter()
        .find(|row| row.get("declaredSize").and_then(serde_json::Value::as_u64) == Some(3))
        .ok_or_else(|| "trust report omitted partial attempt family".to_owned())?;
    ensure_equal(
        &family_row
            .get("recordedSlots")
            .and_then(serde_json::Value::as_u64),
        &Some(2),
        "trust report recorded slots",
    )?;
    ensure_equal(
        &family_row
            .get("unrecordedCount")
            .and_then(serde_json::Value::as_u64),
        &Some(1),
        "trust report unrecorded siblings",
    )?;
    ensure(
        family_row
            .get("selectedDiscountFactor")
            .and_then(serde_json::Value::as_f64)
            .is_some_and(|factor| (factor - 1.0 / 3.0).abs() < 1.0e-6),
        "trust report exposes the exact selected 1/N discount",
    )?;
    ensure(
        !serde_json::to_string(&trust_json)
            .map_err(|error| error.to_string())?
            .contains(family_id),
        "trust report must not expose the raw family id",
    )?;
    // Reuse the real process read pool as well as the public CLI. Each report
    // must finish its own snapshot without the membership reader nesting one.
    let read_pool = ee::db::read_pool::registered_process_read_pool(
        ee::db::DatabaseConfig::file(PathBuf::from(&workspace).join(".ee").join("ee.db")),
        ee::db::read_pool::PoolConfig::default_single(),
    );
    let options = ee::core::trust_report::TrustReportOptions::new(PathBuf::from(&workspace));
    let first_report = ee::core::trust_report::generate_trust_report(options.clone())
        .map_err(|error| format!("first pooled family trust report: {error}"))?;
    ensure(
        read_pool.active_snapshot_pins().is_empty(),
        "first trust report must release its read snapshot",
    )?;
    let repeated_report = ee::core::trust_report::generate_trust_report(options)
        .map_err(|error| format!("repeated pooled family trust report: {error}"))?;
    ensure(
        read_pool.active_snapshot_pins().is_empty(),
        "repeated trust report must release its read snapshot",
    )?;
    ensure_equal(
        &repeated_report,
        &first_report,
        "repeated pooled trust snapshots preserve every report field",
    )?;
    ensure_equal(
        &first_report.memory_count,
        &2,
        "pooled trust report reads the real recorded memories",
    )?;
    ensure_equal(
        &first_report.data_json()["attemptFamilies"],
        &trust_json["data"]["attemptFamilies"],
        "pooled trust read preserves the full public family evidence",
    )?;
    drop(read_pool);
    let trust_human = run_ee(&["--workspace", &workspace, "trust", "report"])?;
    ensure_equal(
        &trust_human.status.code(),
        &Some(EXIT_SUCCESS),
        "multiplicity-aware human trust report",
    )?;
    let trust_human_stdout =
        String::from_utf8(trust_human.stdout).map_err(|error| error.to_string())?;
    ensure(
        trust_human_stdout.contains("2 of 3 attempt slots recorded; 1 unrecorded")
            && trust_human_stdout.contains("selected discount 0.333333")
            && trust_human_stdout.contains("afm_")
            && !trust_human_stdout.contains(family_id),
        "public human trust report surfaces recorded-vs-declared posture and only the family alias",
    )?;
    let create_audit = run_ee(&[
        "--workspace",
        &workspace,
        "audit",
        "timeline",
        "--action",
        "memory.create",
        "--json",
    ])?;
    let create_audit_json = stdout_json(&create_audit)?;
    let serialized_create_audit =
        serde_json::to_string(&create_audit_json).map_err(|error| error.to_string())?;
    ensure(
        serialized_create_audit.contains("familyAlias")
            && !serialized_create_audit.contains("familyId")
            && !serialized_create_audit.contains(family_id),
        "memory.create audit stores only a domain-separated family alias",
    )?;

    let mut selected_memory_id =
        selected_memory_id.ok_or_else(|| "selected partial-family memory id missing".to_owned())?;
    let mut rejected_memory_id =
        rejected_memory_id.ok_or_else(|| "rejected partial-family memory id missing".to_owned())?;

    // Semantic facts use the artifacts section. Compact reserves only 5% of
    // the unchanged 60-token budget for it, so the selected body must fit three
    // tokens independently of the much larger overall pack budget.
    let selected_revision_content = "Frozen ledger";
    let selected_revision_tokens = ee::pack::estimate_tokens_default(selected_revision_content);
    let artifact_quota = ee::pack::SectionQuotas::compact(60).get(ee::pack::PackSection::Artifacts);
    ensure_equal(
        &artifact_quota.max_tokens,
        &3,
        "compact family artifact quota",
    )?;
    ensure(
        selected_revision_tokens > 0 && selected_revision_tokens <= artifact_quota.max_tokens,
        "short selected fixture must actually fit its compact section quota",
    )?;
    let revise = run_ee(&[
        "--workspace",
        &workspace,
        "memory",
        "revise",
        &selected_memory_id,
        "--content",
        selected_revision_content,
        "--actor",
        "literal-closure-auditor",
        "--reason",
        "Exercise attempt-family inheritance across revisions.",
        "--json",
    ])?;
    ensure_equal(
        &revise.status.code(),
        &Some(EXIT_SUCCESS),
        "revise selected family member",
    )?;
    assert_stderr_empty(&revise, "revise selected family member")?;
    let revise_json = stdout_json(&revise)?;
    let revised_memory_id = json_str(&revise_json, "/data/new_id", "family revision")?;
    ensure(
        revised_memory_id != selected_memory_id,
        "family revision must create a distinct immutable row",
    )?;
    selected_memory_id = revised_memory_id.to_owned();

    let revised_family = run_ee(&[
        "--workspace",
        &workspace,
        "search",
        "--family",
        family_id,
        "--json",
    ])?;
    let revised_family_json = stdout_json(&revised_family)?;
    let revised_members = json_array(
        &revised_family_json,
        "/data/members",
        "revised family search",
    )?;
    ensure(
        revised_members.iter().any(|member| {
            member.get("memoryId").and_then(serde_json::Value::as_str)
                == Some(selected_memory_id.as_str())
                && member
                    .get("attemptIndex")
                    .and_then(serde_json::Value::as_u64)
                    == Some(1)
        }),
        "public family search must resolve the inherited slot through the current revision",
    )?;

    let rejected_revision_content = "Frozen multiplicity ledger rejected evidence carries a deliberately oversized body so the public pack selector records it in the omitted ledger while the short selected sibling still fits. This planted negative is intentionally repetitive: frozen multiplicity ledger rejected evidence must remain visible, preserve its unchanged rejection discount, and stay attached to the same attempt slot across revision, backup, restore, replay, and why inspection even after later siblings change the live family posture.";
    ensure(
        ee::pack::estimate_tokens_default(rejected_revision_content) > 60,
        "rejected fixture must exceed even the entire pack budget",
    )?;
    let revise_rejected = run_ee(&[
        "--workspace",
        &workspace,
        "memory",
        "revise",
        &rejected_memory_id,
        "--content",
        rejected_revision_content,
        "--actor",
        "literal-closure-auditor",
        "--reason",
        "Exercise omitted family evidence across revisions.",
        "--json",
    ])?;
    ensure_equal(
        &revise_rejected.status.code(),
        &Some(EXIT_SUCCESS),
        "revise rejected family member",
    )?;
    let revise_rejected_json = stdout_json(&revise_rejected)?;
    rejected_memory_id = json_str(
        &revise_rejected_json,
        "/data/new_id",
        "rejected family revision",
    )?
    .to_owned();

    let backup = run_ee(&[
        "--workspace",
        &workspace,
        "backup",
        "create",
        "--redaction",
        "none",
        "--label",
        "multiplicity-revision-roundtrip",
        "--json",
    ])?;
    ensure_equal(
        &backup.status.code(),
        &Some(EXIT_SUCCESS),
        "backup revised family",
    )?;
    assert_stderr_empty(&backup, "backup revised family")?;
    let backup_json = stdout_json(&backup)?;
    let backup_id = json_str(&backup_json, "/data/backupId", "family backup")?;
    let restore_root = tempfile::tempdir().map_err(|error| error.to_string())?;
    let restore_workspace = restore_root.path().join("restored-family");
    let restore_workspace_arg = restore_workspace.to_string_lossy().into_owned();
    let restore = run_ee(&[
        "--workspace",
        &workspace,
        "backup",
        "restore",
        backup_id,
        "--side-path",
        &restore_workspace_arg,
        "--json",
    ])?;
    ensure_equal(
        &restore.status.code(),
        &Some(EXIT_SUCCESS),
        "restore revised family",
    )?;
    assert_stderr_empty(&restore, "restore revised family")?;
    let restored_family = run_ee(&[
        "--workspace",
        &restore_workspace_arg,
        "search",
        "--family",
        family_id,
        "--json",
    ])?;
    ensure_equal(
        &restored_family.status.code(),
        &Some(EXIT_SUCCESS),
        "search restored revised family",
    )?;
    let restored_family_json = stdout_json(&restored_family)?;
    ensure_equal(
        &restored_family_json
            .pointer("/data/recordedSlots")
            .and_then(serde_json::Value::as_u64),
        &Some(2),
        "backup restore retains each family slot exactly once",
    )?;
    let restored_members = json_array(
        &restored_family_json,
        "/data/members",
        "restored revised family search",
    )?;
    ensure_equal(
        &restored_members.len(),
        &2_usize,
        "backup restore does not duplicate a slot for historical revisions",
    )?;
    ensure(
        restored_members.iter().any(|member| {
            member.get("content").and_then(serde_json::Value::as_str)
                == Some(selected_revision_content)
                && member
                    .get("attemptIndex")
                    .and_then(serde_json::Value::as_u64)
                    == Some(1)
        }),
        "restored family retains the revised selected head and its slot",
    )?;

    let rebuild = run_ee(&["--workspace", &workspace, "index", "rebuild", "--json"])?;
    ensure_equal(
        &rebuild.status.code(),
        &Some(EXIT_SUCCESS),
        "family pack index rebuild",
    )?;
    // Persisted calls populate the derived cache; read-only consumers share
    // this explicit temporal snapshot without skipping any ledger/audit work.
    let pack_as_of = chrono::Utc::now().to_rfc3339();
    let pack_cache_dir = temp.path().join("pack-cache");
    let run_pack = |args: &[&str], cache_disabled: bool| {
        Command::new(env!("CARGO_BIN_EXE_ee"))
            .args(args)
            .env_remove("EE_WORKSPACE")
            .env_remove("EE_WORKSPACE_REGISTRY")
            .env_remove("EE_AGENT_NAME")
            .env("EE_L2_PACK_CACHE_DIR", &pack_cache_dir)
            .env(
                "EE_L2_PACK_CACHE_DISABLE",
                if cache_disabled { "true" } else { "false" },
            )
            .output()
            .map_err(|error| format!("family pack command failed: {error}"))
    };
    let pack_args = [
        "--workspace",
        workspace.as_str(),
        "pack",
        "frozen multiplicity ledger",
        "--max-tokens",
        "60",
        "--candidate-pool",
        "2",
        "--profile",
        "compact",
        "--source-mode",
        "lexical-only",
        "--as-of",
        pack_as_of.as_str(),
        "--json",
    ];
    let frozen_pack = run_pack(&pack_args, false)?;
    persist_artifact("family_frozen_pack", &frozen_pack);
    ensure_equal(
        &frozen_pack.status.code(),
        &Some(EXIT_SUCCESS),
        "selected and omitted frozen family pack",
    )?;
    let frozen_pack_json = stdout_json(&frozen_pack)?;
    let frozen_items = json_array(&frozen_pack_json, "/data/pack/items", "frozen pack items")?;
    ensure(
        frozen_items.iter().any(|item| {
            item.get("memoryId").and_then(serde_json::Value::as_str)
                == Some(selected_memory_id.as_str())
        }),
        format!(
            "short selected sibling must be persisted as a selected pack item: {frozen_pack_json}"
        ),
    )?;
    let selected_item = frozen_items
        .iter()
        .find(|item| {
            item.get("memoryId").and_then(serde_json::Value::as_str)
                == Some(selected_memory_id.as_str())
        })
        .ok_or_else(|| "selected family pack item missing after inclusion check".to_owned())?;
    ensure_equal(
        &selected_item
            .get("content")
            .and_then(serde_json::Value::as_str),
        &Some(selected_revision_content),
        "selected family body is retained without truncation",
    )?;
    ensure_equal(
        &selected_item
            .get("section")
            .and_then(serde_json::Value::as_str),
        &Some("artifacts"),
        "semantic family fact keeps its actual section assignment",
    )?;
    ensure_equal(
        &selected_item
            .get("estimatedTokens")
            .and_then(serde_json::Value::as_u64),
        &Some(u64::from(selected_revision_tokens)),
        "selected family token cost matches the independent tokenizer",
    )?;
    let frozen_skipped = json_array(
        &frozen_pack_json,
        "/data/pack/skipped",
        "frozen pack skipped items",
    )?;
    ensure(
        frozen_skipped.iter().any(|item| {
            item.get("memoryId").and_then(serde_json::Value::as_str)
                == Some(rejected_memory_id.as_str())
                && item.get("reason").and_then(serde_json::Value::as_str)
                    == Some("token_budget_exceeded")
                && item
                    .get("tokens")
                    .and_then(serde_json::Value::as_u64)
                    .is_some_and(|tokens| tokens > 60)
        }),
        "oversized rejected sibling must be persisted as an omitted pack item with its budget reason and oversized cost",
    )?;

    let mut performance_args = pack_args.to_vec();
    performance_args.insert(performance_args.len() - 1, "--explain-performance");
    performance_args.insert(performance_args.len() - 1, "--read-only");
    let warm_performance = run_pack(&performance_args, false)?;
    persist_artifact("family_warm_cache_performance", &warm_performance);
    let warm_performance_json = stdout_json(&warm_performance)?;
    let expected_cache_status = if cfg!(unix) { "hit" } else { "fallback" };
    ensure_equal(
        &warm_performance_json
            .pointer("/data/cache/status")
            .and_then(serde_json::Value::as_str),
        &Some(expected_cache_status),
        "matching fixed-time read-only request hits the persisted producer's L2 entry",
    )?;
    if !cfg!(unix) {
        ensure(
            json_array(
                &warm_performance_json,
                "/data/fallbacks",
                "unsupported platform cache fallback",
            )?
            .iter()
            .all(|entry| {
                entry.get("code").and_then(serde_json::Value::as_str)
                    != Some("l2_pack_cache_unavailable")
            }),
            "unsupported file identity is a conservative bypass, not a storage failure",
        )?;
    }

    let cache_snapshot =
        || -> Result<BTreeMap<PathBuf, (Vec<u8>, std::time::SystemTime)>, String> {
            let mut pending = vec![pack_cache_dir.clone()];
            let mut files = BTreeMap::new();
            while let Some(directory) = pending.pop() {
                for entry in fs::read_dir(directory).map_err(|error| error.to_string())? {
                    let path = entry.map_err(|error| error.to_string())?.path();
                    let metadata =
                        fs::symlink_metadata(&path).map_err(|error| error.to_string())?;
                    if metadata.is_dir() {
                        pending.push(path);
                    } else {
                        ensure(
                            metadata.is_file(),
                            "cache fixture contains only regular files",
                        )?;
                        files.insert(
                            path.clone(),
                            (
                                fs::read(&path).map_err(|error| error.to_string())?,
                                metadata.modified().map_err(|error| error.to_string())?,
                            ),
                        );
                    }
                }
            }
            Ok(files)
        };
    let pack_database_snapshot = || -> Result<Vec<i64>, String> {
        let connection = ee::db::DbConnection::open_file_read_only(temp.path().join(".ee/ee.db"))
            .map_err(|error| error.to_string())?;
        ["pack_records", "pack_items", "pack_omissions", "audit_log"]
            .into_iter()
            .map(|table| {
                connection
                    .count_table_rows(table)
                    .map_err(|error| error.to_string())
            })
            .collect()
    };
    let normalize_pack_elapsed =
        |mut value: serde_json::Value| -> Result<serde_json::Value, String> {
            ensure_equal(
                &value
                    .pointer("/data/pack/slo/resourceStatus")
                    .and_then(serde_json::Value::as_str),
                &Some("within_budget"),
                "cache equivalence requires a real within-budget resource decision",
            )?;
            ensure_equal(
                &value
                    .pointer("/data/pack/slo/admission/outcome")
                    .and_then(serde_json::Value::as_str),
                &Some("admitted"),
                "cache equivalence retains successful pack admission",
            )?;
            ensure(
                json_array(
                    &value,
                    "/data/pack/slo/degradations",
                    "pack SLO degradations",
                )?
                .is_empty(),
                "cache equivalence cannot erase an SLO degradation",
            )?;
            ensure(
                ee::obs::normalize_pack_slo_measurements(&mut value)?,
                "cache equivalence requires a validated producer SLO measurement",
            )?;
            Ok(value)
        };
    let mut readonly_args = pack_args.to_vec();
    readonly_args.insert(readonly_args.len() - 1, "--read-only");
    if cfg!(unix) {
        let cache_before_readonly = cache_snapshot()?;
        ensure_equal(
            &cache_before_readonly.len(),
            &1,
            "one real persisted producer cache entry",
        )?;
        let database_before_readonly = pack_database_snapshot()?;
        let cached_pack = run_pack(&readonly_args, false)?;
        let fresh_readonly_pack = run_pack(&readonly_args, true)?;
        for (label, output) in [
            ("cached read-only pack", &cached_pack),
            ("fresh read-only pack", &fresh_readonly_pack),
        ] {
            ensure_equal(&output.status.code(), &Some(EXIT_SUCCESS), label)?;
            assert_stderr_empty(output, label)?;
        }
        ensure_equal(
            &normalize_pack_elapsed(stdout_json(&cached_pack)?)?,
            &normalize_pack_elapsed(stdout_json(&fresh_readonly_pack)?)?,
            "cache hit preserves every fresh read-only field except the three validated producer SLO measurements",
        )?;
        ensure_equal(
            &pack_database_snapshot()?,
            &database_before_readonly,
            "read-only hit and miss preserve all pack/item/omission/audit rows",
        )?;
        ensure_equal(
            &cache_snapshot()?,
            &cache_before_readonly,
            "read-only hit and miss preserve cache bytes, names and mtimes",
        )?;

        let mut strict_floor_args = readonly_args.clone();
        strict_floor_args.insert(strict_floor_args.len() - 1, "--relevance-floor");
        strict_floor_args.insert(strict_floor_args.len() - 1, "1");
        let strict_floor_pack = run_pack(&strict_floor_args, false)?;
        ensure_equal(
            &strict_floor_pack.status.code(),
            &Some(EXIT_SUCCESS),
            "strict relevance floor pack",
        )?;
        assert_stderr_empty(&strict_floor_pack, "strict relevance floor pack")?;
        let strict_floor_json = stdout_json(&strict_floor_pack)?;
        ensure(
            json_array(&strict_floor_json, "/data/pack/items", "strict floor items")?
                .iter()
                .all(|item| {
                    item.get("memoryId").and_then(serde_json::Value::as_str)
                        != Some(selected_memory_id.as_str())
                }),
            "changed relevance floor must not replay the cached low-score selected memory",
        )?;
        ensure_equal(
            &cache_snapshot()?,
            &cache_before_readonly,
            "stricter read-only request cannot populate another cache key",
        )?;

        let cache_entry = cache_before_readonly
            .keys()
            .next()
            .ok_or_else(|| "producer cache entry missing".to_owned())?;
        fs::write(cache_entry, b"planted corrupt cache payload")
            .map_err(|error| error.to_string())?;
        let corrupt_cache = cache_snapshot()?;
        let corrupt_lookup = run_pack(&performance_args, false)?;
        ensure_equal(
            &corrupt_lookup.status.code(),
            &Some(EXIT_SUCCESS),
            "corrupt cache retains positive fresh pack fallback",
        )?;
        assert_stderr_empty(&corrupt_lookup, "corrupt cache fallback")?;
        let corrupt_json = stdout_json(&corrupt_lookup)?;
        ensure_equal(
            &corrupt_json
                .pointer("/data/cache/status")
                .and_then(serde_json::Value::as_str),
            &Some("fallback"),
            "corrupt cache must not be a hit",
        )?;
        ensure(
            json_array(
                &corrupt_json,
                "/data/fallbacks",
                "corrupt cache degradations",
            )?
            .iter()
            .any(|entry| {
                entry.get("code").and_then(serde_json::Value::as_str)
                    == Some("l2_pack_cache_corruption")
            }),
            "cache corruption remains an explicit typed degradation",
        )?;
        ensure_equal(
            &cache_snapshot()?,
            &corrupt_cache,
            "read-only corruption rejection cannot delete, rewrite or touch the bad cache entry",
        )?;
        ensure_equal(
            &pack_database_snapshot()?,
            &database_before_readonly,
            "read-only corruption fallback cannot append pack or audit rows",
        )?;
        let repair_producer = run_pack(&pack_args, false)?;
        ensure_equal(
            &repair_producer.status.code(),
            &Some(EXIT_SUCCESS),
            "persisted producer refreshes the corrupted cache",
        )?;
        assert_stderr_empty(&repair_producer, "persisted cache repair producer")?;
        let repaired_hit = stdout_json(&run_pack(&performance_args, false)?)?;
        ensure_equal(
            &repaired_hit
                .pointer("/data/cache/status")
                .and_then(serde_json::Value::as_str),
            &Some("hit"),
            "real writable producer restores the read-only hit",
        )?;

        let metadata_path = temp.path().join(".ee/index/meta.json");
        let metadata_original = fs::read(&metadata_path).map_err(|error| error.to_string())?;
        let metadata_json: serde_json::Value =
            serde_json::from_slice(&metadata_original).map_err(|error| error.to_string())?;
        let timestamp = json_str(
            &metadata_json,
            "/lastRebuildAt",
            "index publication timestamp",
        )?;
        let metadata_text =
            std::str::from_utf8(&metadata_original).map_err(|error| error.to_string())?;
        let second_digit = metadata_text
            .find(timestamp)
            .ok_or_else(|| "index timestamp bytes missing".to_owned())?
            + timestamp.len()
            - 2;
        ensure(
            metadata_original[second_digit].is_ascii_digit(),
            "index fixture timestamp ends in a seconds digit and Z",
        )?;
        let metadata_modified = fs::metadata(&metadata_path)
            .and_then(|metadata| metadata.modified())
            .map_err(|error| error.to_string())?;
        let mut metadata_changed = metadata_original.clone();
        metadata_changed[second_digit] = if metadata_changed[second_digit] == b'0' {
            b'1'
        } else {
            b'0'
        };
        fs::write(&metadata_path, &metadata_changed).map_err(|error| error.to_string())?;
        fs::File::options()
            .write(true)
            .open(&metadata_path)
            .and_then(|file| file.set_times(fs::FileTimes::new().set_modified(metadata_modified)))
            .map_err(|error| error.to_string())?;
        ensure_equal(
            &fs::metadata(&metadata_path)
                .and_then(|metadata| metadata.modified())
                .map_err(|error| error.to_string())?,
            &metadata_modified,
            "index mutation restores the exact original mtime",
        )?;
        let index_change_cache = cache_snapshot()?;
        let changed_index = stdout_json(&run_pack(&performance_args, false)?)?;
        ensure_equal(
            &changed_index
                .pointer("/data/cache/status")
                .and_then(serde_json::Value::as_str),
            &Some("fallback"),
            "same-generation same-length restored-mtime index bytes invalidate the cache",
        )?;
        ensure_equal(
            &cache_snapshot()?,
            &index_change_cache,
            "index-invalidated read-only request cannot populate cache",
        )?;
        fs::write(&metadata_path, &metadata_original).map_err(|error| error.to_string())?;
        fs::File::options()
            .write(true)
            .open(&metadata_path)
            .and_then(|file| file.set_times(fs::FileTimes::new().set_modified(metadata_modified)))
            .map_err(|error| error.to_string())?;
        let original_index = stdout_json(&run_pack(&performance_args, false)?)?;
        ensure_equal(
            &original_index
                .pointer("/data/cache/status")
                .and_then(serde_json::Value::as_str),
            &Some("hit"),
            "restoring identical published bytes makes the original complete cache entry usable",
        )?;
    }

    for index in 0..10 {
        let source_id = format!("public-family-promotion-{index}");
        let outcome = run_ee(&[
            "--workspace",
            &workspace,
            "outcome",
            &selected_memory_id,
            "--signal",
            "helpful",
            "--source-id",
            &source_id,
            "--json",
        ])?;
        ensure_equal(
            &outcome.status.code(),
            &Some(EXIT_SUCCESS),
            "ordinary helpful family outcome",
        )?;
        assert_stderr_empty(&outcome, "ordinary helpful family outcome")?;
    }
    let blocked_show = run_ee(&[
        "--workspace",
        &workspace,
        "memory",
        "show",
        &selected_memory_id,
        "--json",
    ])?;
    let blocked_show_json = stdout_json(&blocked_show)?;
    ensure_equal(
        &blocked_show_json
            .pointer("/data/memory/trust_class")
            .and_then(serde_json::Value::as_str),
        &Some("agent_assertion"),
        "incomplete family refuses ordinary promotion",
    )?;

    let override_reason = "Operator reviewed the missing attempt and deliberately accepts risk.";
    let override_outcome = run_ee(&[
        "--workspace",
        &workspace,
        "outcome",
        &selected_memory_id,
        "--signal",
        "helpful",
        "--source-type",
        "human_explicit",
        "--source-id",
        "operator-family-override",
        "--actor",
        "literal-closure-auditor",
        "--reason",
        override_reason,
        "--json",
    ])?;
    ensure_equal(
        &override_outcome.status.code(),
        &Some(EXIT_SUCCESS),
        "explicit family promotion override",
    )?;
    assert_stderr_empty(&override_outcome, "explicit family promotion override")?;
    let promoted_show = run_ee(&[
        "--workspace",
        &workspace,
        "memory",
        "show",
        &selected_memory_id,
        "--json",
    ])?;
    let promoted_show_json = stdout_json(&promoted_show)?;
    ensure_equal(
        &promoted_show_json
            .pointer("/data/memory/trust_class")
            .and_then(serde_json::Value::as_str),
        &Some("agent_validated"),
        "deliberate human override promotes incomplete-family survivor",
    )?;
    let override_audit = run_ee(&[
        "--workspace",
        &workspace,
        "audit",
        "timeline",
        "--action",
        "trust_class.promotion_override",
        "--target",
        &selected_memory_id,
        "--json",
    ])?;
    let override_audit_json = stdout_json(&override_audit)?;
    let override_entries = json_array(
        &override_audit_json,
        "/data/entries",
        "family promotion override audit",
    )?;
    ensure_equal(
        &override_entries.len(),
        &1_usize,
        "one deliberate override audit row",
    )?;
    let serialized_override =
        serde_json::to_string(&override_audit_json).map_err(|error| error.to_string())?;
    ensure(
        serialized_override.contains(override_reason) && !serialized_override.contains(family_id),
        "override audit keeps the reason and aliases the family id",
    )?;

    // Feedback and trust promotion advance source generation without queuing
    // document-index work. Preserve that invalidation before establishing a
    // fresh index for the independent family-sidecar cache control below.
    let outcome_index = run_ee(&["--workspace", &workspace, "index", "status", "--json"])?;
    ensure_equal(
        &outcome_index.status.code(),
        &Some(EXIT_SUCCESS),
        "post-outcome index status",
    )?;
    let outcome_index_json = stdout_json(&outcome_index)?;
    ensure_equal(
        &outcome_index_json
            .pointer("/data/health")
            .and_then(serde_json::Value::as_str),
        &Some("stale"),
        "feedback and promotion invalidate the previously ready index",
    )?;
    let outcome_generation = outcome_index_json
        .pointer("/data/dbGeneration")
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| "post-outcome database generation missing".to_owned())?;
    let stale_generation = outcome_index_json
        .pointer("/data/indexGeneration")
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| "post-outcome index generation missing".to_owned())?;
    ensure(
        outcome_generation > stale_generation,
        "post-outcome source generation exceeds the index",
    )?;
    let stale_cache_before = cfg!(unix).then(&cache_snapshot).transpose()?;
    let stale_database_before = pack_database_snapshot()?;
    let stale_after_outcomes = run_pack(&performance_args, false)?;
    persist_artifact("family_after_outcomes_stale_cache", &stale_after_outcomes);
    ensure_equal(
        &stale_after_outcomes.status.code(),
        &Some(EXIT_SUCCESS),
        "stale post-outcome pack remains usable",
    )?;
    assert_stderr_empty(&stale_after_outcomes, "stale post-outcome pack")?;
    let stale_after_outcomes_json = stdout_json(&stale_after_outcomes)?;
    ensure_equal(
        &stale_after_outcomes_json
            .pointer("/data/cache/status")
            .and_then(serde_json::Value::as_str),
        &Some("fallback"),
        "post-outcome stale index cannot replay the old cache entry",
    )?;
    ensure(
        json_array(
            &stale_after_outcomes_json,
            "/data/fallbacks",
            "post-outcome fallbacks",
        )?
        .iter()
        .any(|entry| entry["code"] == "search_index_stale"),
        "post-outcome fallback retains the actual stale-index diagnosis",
    )?;
    ensure_equal(
        &pack_database_snapshot()?,
        &stale_database_before,
        "post-outcome readonly fallback leaves pack and audit rows unchanged",
    )?;
    if let Some(stale_cache_before) = stale_cache_before {
        ensure_equal(
            &cache_snapshot()?,
            &stale_cache_before,
            "post-outcome readonly fallback leaves cache files unchanged",
        )?;
    }
    let outcome_rebuild = run_ee(&["--workspace", &workspace, "index", "rebuild", "--json"])?;
    ensure_equal(
        &outcome_rebuild.status.code(),
        &Some(EXIT_SUCCESS),
        "rebuild after feedback and promotion",
    )?;
    assert_stderr_empty(&outcome_rebuild, "post-outcome index rebuild")?;
    let ready_index = run_ee(&["--workspace", &workspace, "index", "status", "--json"])?;
    ensure_equal(
        &ready_index.status.code(),
        &Some(EXIT_SUCCESS),
        "rebuilt post-outcome index status",
    )?;
    let ready_index_json = stdout_json(&ready_index)?;
    ensure_equal(
        &ready_index_json
            .pointer("/data/health")
            .and_then(serde_json::Value::as_str),
        &Some("ready"),
        "family-sidecar cache control starts with a genuinely ready index",
    )?;
    for pointer in ["/data/dbGeneration", "/data/indexGeneration"] {
        ensure_equal(
            &ready_index_json
                .pointer(pointer)
                .and_then(serde_json::Value::as_u64),
            &Some(outcome_generation),
            "rebuild catches up to the exact post-outcome source generation",
        )?;
    }

    let rewarm_after_outcomes = run_pack(&pack_args, false)?;
    persist_artifact(
        "family_after_outcomes_cache_producer",
        &rewarm_after_outcomes,
    );
    ensure_equal(
        &rewarm_after_outcomes.status.code(),
        &Some(EXIT_SUCCESS),
        "rewarm pack after outcome generation changes",
    )?;
    assert_stderr_empty(&rewarm_after_outcomes, "post-outcome cache producer")?;
    let hot_immediately_before_family_write = run_pack(&performance_args, false)?;
    persist_artifact(
        "family_before_sidecar_cache_hit",
        &hot_immediately_before_family_write,
    );
    ensure_equal(
        &hot_immediately_before_family_write.status.code(),
        &Some(EXIT_SUCCESS),
        "read-only hit before family sidecar write",
    )?;
    assert_stderr_empty(
        &hot_immediately_before_family_write,
        "read-only hit before family sidecar write",
    )?;
    let hot_before_family_json = stdout_json(&hot_immediately_before_family_write)?;
    ensure_equal(
        &hot_before_family_json
            .pointer("/data/cache/status")
            .and_then(serde_json::Value::as_str),
        &Some(expected_cache_status),
        "L2 entry is confirmed hot immediately before the family-sidecar write",
    )?;

    let third_sibling = run_ee_as_agent(&[
        "--workspace",
        &workspace,
        "remember",
        "Frozen multiplicity ledger final rejected sibling",
        "--level",
        "semantic",
        "--kind",
        "fact",
        "--family",
        family_id,
        "--of-n",
        "3",
        "--attempt",
        "3",
        "--attempt-outcome",
        "rejected",
        "--json",
    ])?;
    ensure_equal(
        &third_sibling.status.code(),
        &Some(EXIT_SUCCESS),
        "record final canonical sibling",
    )?;
    let canonical_family = run_ee(&[
        "--workspace",
        &workspace,
        "search",
        "--family",
        family_id,
        "--json",
    ])?;
    let canonical_family_json = stdout_json(&canonical_family)?;
    ensure_equal(
        &canonical_family_json
            .pointer("/data/promotionEligible")
            .and_then(serde_json::Value::as_bool),
        &Some(true),
        "one selected plus N-1 rejected siblings is canonical",
    )?;

    let frozen_why = run_ee(&[
        "--workspace",
        &workspace,
        "why",
        &selected_memory_id,
        "--json",
    ])?;
    persist_artifact("family_frozen_why", &frozen_why);
    let frozen_why_json = stdout_json(&frozen_why)?;
    let frozen_selection = frozen_why_json
        .pointer("/data/selection/latestPackSelection")
        .ok_or_else(|| "why omitted latest frozen pack selection".to_owned())?;
    ensure_equal(
        &frozen_selection
            .pointer("/attemptFamilyMultiplicity/memberships/0/recordedSlots")
            .and_then(serde_json::Value::as_u64),
        &Some(2),
        "why uses selected-item multiplicity frozen before the sibling changed",
    )?;
    ensure_equal(
        &frozen_selection
            .pointer("/attemptFamilyMultiplicity/memberships/0/unrecordedCount")
            .and_then(serde_json::Value::as_u64),
        &Some(1),
        "why retains the frozen missing-sibling count",
    )?;
    ensure(
        frozen_selection
            .pointer("/attemptFamilyMultiplicity/effectiveDiscountFactor")
            .and_then(serde_json::Value::as_f64)
            .is_some_and(|factor| (factor - 1.0 / 3.0).abs() < 1.0e-6),
        "why retains the exact frozen selected 1/N discount",
    )?;
    ensure(
        !serde_json::to_string(&frozen_why_json)
            .map_err(|error| error.to_string())?
            .contains(family_id),
        "why must never expose the raw secret-shaped family id",
    )?;
    let pack_id = json_str(frozen_selection, "/packId", "frozen pack selection")?;
    let replay = run_ee(&[
        "--workspace",
        &workspace,
        "pack",
        "replay",
        pack_id,
        "--json",
    ])?;
    let replay_json = stdout_json(&replay)?;
    let replay_selected = json_array(
        &replay_json,
        "/data/replay/selectedItems",
        "frozen replay selected items",
    )?;
    let replay_omitted = json_array(
        &replay_json,
        "/data/replay/omittedItems",
        "frozen replay omitted items",
    )?;
    let selected_snapshot = replay_selected
        .iter()
        .find(|item| {
            item.get("memoryId").and_then(serde_json::Value::as_str)
                == Some(selected_memory_id.as_str())
        })
        .and_then(|item| item.get("attemptFamilyMultiplicity"))
        .ok_or_else(|| "selected replay ledger omitted multiplicity snapshot".to_owned())?;
    let omitted_snapshot = replay_omitted
        .iter()
        .find(|item| {
            item.get("memoryId").and_then(serde_json::Value::as_str)
                == Some(rejected_memory_id.as_str())
        })
        .and_then(|item| item.get("attemptFamilyMultiplicity"))
        .ok_or_else(|| "omitted replay ledger omitted multiplicity snapshot".to_owned())?;
    ensure_equal(
        &selected_snapshot.pointer("/memberships/0/recordedSlots"),
        &Some(&serde_json::json!(2)),
        "selected replay snapshot stays frozen",
    )?;
    ensure_equal(
        &omitted_snapshot.pointer("/memberships/0/memberDiscountFactor"),
        &Some(&serde_json::json!(1.0)),
        "rejected omitted snapshot keeps its undiscounted factor",
    )?;
    let public_replay = serde_json::to_string(&replay_json).map_err(|error| error.to_string())?;
    ensure(
        !public_replay.contains(family_id),
        "pack replay must never expose the raw secret-shaped family id",
    )?;

    let invalidated_performance = run_pack(&performance_args, false)?;
    let invalidated_performance_json = stdout_json(&invalidated_performance)?;
    ensure_equal(
        &invalidated_performance_json
            .pointer("/data/cache/status")
            .and_then(serde_json::Value::as_str),
        &Some("fallback"),
        "family-sidecar generation change invalidates the warm L2 entry",
    )?;

    let tombstone = run_ee(&[
        "--workspace",
        &workspace,
        "curate",
        "tombstone",
        &rejected_memory_id,
        "--actor",
        "literal-closure-auditor",
        "--reason",
        "Planted negative: deleted sibling must not count as live evidence.",
        "--json",
    ])?;
    ensure_equal(
        &tombstone.status.code(),
        &Some(EXIT_SUCCESS),
        "tombstone rejected sibling",
    )?;
    let after_tombstone = run_ee(&[
        "--workspace",
        &workspace,
        "search",
        "--family",
        family_id,
        "--json",
    ])?;
    let after_tombstone_json = stdout_json(&after_tombstone)?;
    ensure_equal(
        &after_tombstone_json
            .pointer("/data/recordedSlots")
            .and_then(serde_json::Value::as_u64),
        &Some(2),
        "tombstoned rejected sibling no longer counts as live family evidence",
    )?;
    ensure_equal(
        &after_tombstone_json
            .pointer("/data/missingCurrentRevisionCount")
            .and_then(serde_json::Value::as_u64),
        &Some(1),
        "tombstoned sibling remains visible as missing-current forensic evidence",
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
    )?;

    if cfg!(unix) {
        let final_producer = run_pack(&pack_args, false)?;
        ensure_equal(
            &final_producer.status.code(),
            &Some(EXIT_SUCCESS),
            "config invalidation producer",
        )?;
        let final_hit = stdout_json(&run_pack(&performance_args, false)?)?;
        ensure_equal(
            &final_hit
                .pointer("/data/cache/status")
                .and_then(serde_json::Value::as_str),
            &Some("hit"),
            "config negative starts from a real hot entry",
        )?;
        let before_config_cache = cache_snapshot()?;
        let before_config_database = pack_database_snapshot()?;
        let config_path = temp.path().join(".ee/config.toml");
        ensure(
            !config_path.exists(),
            "config fixture starts with the supported absent config",
        )?;
        fs::write(&config_path, "[search]\nrerank = \"off\"\n")
            .map_err(|error| error.to_string())?;
        let configured_pack = run_pack(&performance_args, false)?;
        ensure_equal(
            &configured_pack.status.code(),
            &Some(EXIT_SUCCESS),
            "new config still permits real fresh pack assembly",
        )?;
        assert_stderr_empty(&configured_pack, "configured fresh pack")?;
        let configured_json = stdout_json(&configured_pack)?;
        ensure_equal(
            &configured_json
                .pointer("/data/cache/status")
                .and_then(serde_json::Value::as_str),
            &Some("fallback"),
            "present config bypasses the old hot cache",
        )?;
        fs::write(&config_path, "[policy.workspace_memory\nenabled = false\n")
            .map_err(|error| error.to_string())?;
        let malformed_pack = run_pack(&readonly_args, false)?;
        ensure_equal(
            &malformed_pack.status.code(),
            &Some(2),
            "malformed policy cannot be bypassed by a cached pack",
        )?;
        assert_stderr_empty(&malformed_pack, "malformed policy typed response")?;
        let malformed_json = stdout_json(&malformed_pack)?;
        ensure_equal(
            &malformed_json
                .pointer("/error/code")
                .and_then(serde_json::Value::as_str),
            &Some("configuration"),
            "cached pack preserves typed policy failure",
        )?;
        ensure(
            malformed_json.get("data").is_none(),
            "malformed policy cannot emit cached memory data",
        )?;
        ensure_equal(
            &cache_snapshot()?,
            &before_config_cache,
            "config bypass/error leaves cache bytes and mtimes unchanged",
        )?;
        ensure_equal(
            &pack_database_snapshot()?,
            &before_config_database,
            "config bypass/error leaves pack and audit rows unchanged",
        )?;
    }
    Ok(())
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
        let source_name = format!("memory-source-{index}.md");
        let source_path = tempdir.path().join(&source_name);
        fs::write(&source_path, content).map_err(|error| error.to_string())?;
        // Relative sources resolve inside the real workspace and remain
        // distinguishable after the pack renderer redacts private absolute paths.
        let source_uri = format!("file://{source_name}#L1");

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
            ensure(
                !provenance.iter().any(|entry| {
                    entry
                        .get("uri")
                        .and_then(serde_json::Value::as_str)
                        .is_some_and(|uri| {
                            remembered.iter().any(|(other_id, other)| {
                                other_id != memory_id && uri == other.source_uri
                            })
                        })
                }),
                format!("context item {memory_id} must not cite another memory's source"),
            )?;
            let expected_note =
                format!("Memory {memory_id} selected for context pack; evidenceFreshness=fresh");
            ensure(
                provenance.iter().any(|entry| {
                    entry.get("uri").and_then(serde_json::Value::as_str)
                        == Some(stored.source_uri.as_str())
                        && entry.get("note").and_then(serde_json::Value::as_str)
                            == Some(expected_note.as_str())
                }),
                format!("context item {memory_id} must verify its real source as fresh"),
            )?;
            ensure(
                !serde_json::to_string(provenance)
                    .map_err(|error| error.to_string())?
                    .contains(&workspace),
                format!("context item {memory_id} must not disclose the private workspace path"),
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
            // The search/why privacy policy treats file://<non-slash> as
            // host-like, so even this local relative spelling is redacted.
            &"[REDACTED_PATH]#L1",
            &format!("why {memory_id} public provenanceUri"),
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

    let connection =
        ee::db::DbConnection::open_file_read_only(tempdir.path().join(".ee").join("ee.db"))
            .map_err(|error| error.to_string())?;
    for (memory_id, expected) in &remembered {
        let stored = connection
            .get_memory(memory_id)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| format!("source-backed memory {memory_id} disappeared"))?;
        ensure_equal(
            &stored.provenance_uri.as_deref(),
            &Some(expected.source_uri.as_str()),
            &format!("why must not mutate {memory_id}'s stored source URI"),
        )?;
    }

    Ok(())
}

#[test]
fn remember_level_kind_cross_wire_guard_public_cli_contract() -> TestResult {
    let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
    let workspace = tempdir.path().to_string_lossy().to_string();

    let init = run_ee(&["--workspace", &workspace, "init", "--json"])?;
    ensure_equal(
        &init.status.code(),
        &Some(EXIT_SUCCESS),
        "cross-wire guard init exit code",
    )?;

    // Cross-wire direction 1: a known level token passed as --kind.
    let kind_as_level = run_ee(&[
        "--workspace",
        &workspace,
        "remember",
        "cross-wired kind attempt",
        "--kind",
        "episodic",
        "--json",
    ])?;
    ensure_equal(
        &kind_as_level.status.code(),
        &Some(1),
        "level token as --kind must exit with the usage code",
    )?;
    let error_json = stdout_json(&kind_as_level)?;
    assert_schema(&error_json, "ee.error.v2", "kind-as-level error envelope")?;
    ensure_equal(
        &error_json
            .pointer("/error/code")
            .and_then(serde_json::Value::as_str),
        &Some("remember_kind_is_level"),
        "kind-as-level error code",
    )?;
    ensure(
        error_json
            .pointer("/error/message")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|message| message.contains("did you mean `--level episodic`")),
        "kind-as-level message must carry the did-you-mean guidance",
    )?;
    ensure_equal(
        &error_json
            .pointer("/error/details/didYouMean/argument")
            .and_then(serde_json::Value::as_str),
        &Some("--level"),
        "kind-as-level didYouMean argument",
    )?;
    ensure_equal(
        &error_json
            .pointer("/error/details/didYouMean/value")
            .and_then(serde_json::Value::as_str),
        &Some("episodic"),
        "kind-as-level didYouMean value",
    )?;
    ensure_equal(
        &error_json
            .pointer("/error/details/provided")
            .and_then(serde_json::Value::as_str),
        &Some("episodic"),
        "kind-as-level provided token",
    )?;
    ensure_equal(
        &error_json
            .pointer("/error/details/providedTruncated")
            .and_then(serde_json::Value::as_bool),
        &Some(false),
        "kind-as-level provided truncation",
    )?;
    ensure_equal(
        &error_json
            .pointer("/error/severity")
            .and_then(serde_json::Value::as_str),
        &Some("low"),
        "kind-as-level severity",
    )?;
    ensure_equal(
        &error_json
            .pointer("/error/repairKind")
            .and_then(serde_json::Value::as_str),
        &Some("template"),
        "kind-as-level repairKind",
    )?;
    ensure(
        error_json
            .pointer("/error/repair")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|repair| repair.contains("--level episodic")),
        "kind-as-level repair must template the corrected flag",
    )?;
    ensure_equal(
        &error_json
            .pointer("/error/details/recovery/0/kind")
            .and_then(serde_json::Value::as_str),
        &Some("flag"),
        "kind-as-level recovery kind",
    )?;
    ensure_equal(
        &error_json
            .pointer("/error/details/recovery/0/flagName")
            .and_then(serde_json::Value::as_str),
        &Some("--level"),
        "kind-as-level recovery flag",
    )?;
    ensure_equal(
        &error_json
            .pointer("/error/details/recovery/0/valueHint")
            .and_then(serde_json::Value::as_str),
        &Some("episodic"),
        "kind-as-level recovery value",
    )?;
    ensure_equal(
        &error_json
            .pointer("/error/details/recovery/0/riskClass")
            .and_then(serde_json::Value::as_str),
        &Some("mutating_local_repair"),
        "kind-as-level recovery risk class",
    )?;
    for field in [
        "requiresHumanApproval",
        "mutatesExternalState",
        "mutatesTrackerState",
    ] {
        ensure_equal(
            &error_json
                .pointer(&format!("/error/details/recovery/0/{field}"))
                .and_then(serde_json::Value::as_bool),
            &Some(false),
            &format!("kind-as-level recovery {field}"),
        )?;
    }
    ensure_equal(
        &error_json
            .pointer("/error/details/recovery/0/privacyClass")
            .and_then(serde_json::Value::as_str),
        &Some("bounded_command_no_raw_state"),
        "kind-as-level recovery privacy class",
    )?;

    // The same guard is reachable through the real `ee note` CLI surface.
    let note_cross_wire = run_ee(&[
        "--workspace",
        &workspace,
        "note",
        "cross-wired note attempt",
        "--kind",
        "episodic",
        "--json",
    ])?;
    ensure_equal(
        &note_cross_wire.status.code(),
        &Some(1),
        "level token as note --kind must exit with the usage code",
    )?;
    let note_json = stdout_json(&note_cross_wire)?;
    assert_schema(
        &note_json,
        "ee.error.v2",
        "note kind-as-level error envelope",
    )?;
    ensure_equal(
        &note_json
            .pointer("/error/code")
            .and_then(serde_json::Value::as_str),
        &Some("remember_kind_is_level"),
        "note kind-as-level error code",
    )?;

    // Cross-wire direction 2: a canonical kind token passed as --level.
    let level_as_kind = run_ee(&[
        "--workspace",
        &workspace,
        "remember",
        "cross-wired level attempt",
        "--level",
        "rule",
        "--json",
    ])?;
    ensure_equal(
        &level_as_kind.status.code(),
        &Some(1),
        "kind token as --level must exit with the usage code",
    )?;
    let inverse_json = stdout_json(&level_as_kind)?;
    assert_schema(&inverse_json, "ee.error.v2", "level-as-kind error envelope")?;
    ensure_equal(
        &inverse_json
            .pointer("/error/code")
            .and_then(serde_json::Value::as_str),
        &Some("remember_level_is_kind"),
        "level-as-kind error code",
    )?;
    ensure_equal(
        &inverse_json
            .pointer("/error/details/didYouMean/argument")
            .and_then(serde_json::Value::as_str),
        &Some("--kind"),
        "level-as-kind didYouMean argument",
    )?;
    ensure_equal(
        &inverse_json
            .pointer("/error/details/didYouMean/value")
            .and_then(serde_json::Value::as_str),
        &Some("rule"),
        "level-as-kind didYouMean value",
    )?;
    ensure_equal(
        &inverse_json
            .pointer("/error/details/recovery/0/riskClass")
            .and_then(serde_json::Value::as_str),
        &Some("mutating_local_repair"),
        "level-as-kind recovery risk class",
    )?;

    // Planted negative: a noncanonical lookalike custom kind sharing a level
    // prefix must be accepted and follow the pre-existing canonicalization
    // contract, never prefix-rejected by the cross-wire guard.
    let custom_kind = run_ee(&[
        "--workspace",
        &workspace,
        "remember",
        "custom kind lookalike stays accepted",
        "--kind",
        "EpisodicNote",
        "--json",
    ])?;
    ensure_equal(
        &custom_kind.status.code(),
        &Some(EXIT_SUCCESS),
        "custom kind lookalike exit code",
    )?;
    assert_stderr_empty(&custom_kind, "custom kind lookalike")?;
    let custom_json = stdout_json(&custom_kind)?;
    assert_schema(&custom_json, "ee.response.v2", "custom kind lookalike")?;
    ensure_equal(
        &custom_json
            .pointer("/data/kind")
            .and_then(serde_json::Value::as_str),
        &Some("episodic-note"),
        "custom kind must use the established canonical form",
    )?;
    let custom_memory_id = custom_json
        .pointer("/data/memory_id")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| "custom kind remember must return a memory_id".to_owned())?
        .to_owned();

    // Read back the persisted row: the guard must not reject the lookalike,
    // while the stored kind still follows the established canonical form.
    let show = run_ee(&[
        "--workspace",
        &workspace,
        "show",
        &custom_memory_id,
        "--json",
    ])?;
    ensure_equal(
        &show.status.code(),
        &Some(EXIT_SUCCESS),
        "custom kind show exit code",
    )?;
    let show_json = stdout_json(&show)?;
    ensure_equal(
        &show_json
            .pointer("/data/memory/kind")
            .and_then(serde_json::Value::as_str),
        &Some("episodic-note"),
        "persisted custom kind read-back must stay canonical",
    )?;

    // Canonical control: valid level plus canonical kind still succeeds.
    let canonical = run_ee(&[
        "--workspace",
        &workspace,
        "remember",
        "canonical pair control",
        "--level",
        "semantic",
        "--kind",
        "decision",
        "--json",
    ])?;
    ensure_equal(
        &canonical.status.code(),
        &Some(EXIT_SUCCESS),
        "canonical pair exit code",
    )?;

    Ok(())
}

/// A success assertion that explains a failure: exit code AND both streams.
fn ensure_success(output: &Output, label: &str) -> TestResult {
    if output.status.success() {
        Ok(())
    } else {
        Err(format!(
            "{label}: ee exited {:?}; stderr: {}; stdout: {}",
            output.status.code(),
            String::from_utf8_lossy(&output.stderr).trim_end(),
            String::from_utf8_lossy(&output.stdout).trim_end()
        ))
    }
}

/// JSON pointers where `after` differs from `before`: removed keys, changed
/// scalars, and arrays whose length changed (reported as the array itself).
fn changed_json_paths(before: &serde_json::Value, after: &serde_json::Value) -> Vec<String> {
    fn walk(
        before: &serde_json::Value,
        after: &serde_json::Value,
        path: &str,
        out: &mut Vec<String>,
    ) {
        match (before, after) {
            (serde_json::Value::Object(old), serde_json::Value::Object(new)) => {
                for (key, old_child) in old {
                    let child_path = format!("{path}/{key}");
                    match new.get(key) {
                        Some(new_child) => walk(old_child, new_child, &child_path, out),
                        None => out.push(child_path),
                    }
                }
                for key in new.keys().filter(|key| !old.contains_key(*key)) {
                    out.push(format!("{path}/{key}"));
                }
            }
            (serde_json::Value::Array(old), serde_json::Value::Array(new))
                if old.len() == new.len() =>
            {
                for (index, (old_item, new_item)) in old.iter().zip(new).enumerate() {
                    walk(old_item, new_item, &format!("{path}/{index}"), out);
                }
            }
            _ if before == after => {}
            _ => out.push(path.to_owned()),
        }
    }
    let mut out = Vec::new();
    walk(before, after, "", &mut out);
    out
}

/// bd-ndzfg.4 clause (e): a REAL L2 hit is byte-identical to a fresh
/// read-only pack once the REGISTERED volatile channel is removed, and only
/// that channel.
///
/// The path is real end to end: a persisted producer assembles and publishes
/// the entry, a read-only request reads it back through the cache, and the
/// same read-only request with the cache disabled assembles fresh. The
/// comparison uses only the shared normalizers in src/obs/volatile_fields.rs,
/// asserts exactly which fields they touched, and asserts pack.hash equal on
/// the raw responses.
#[test]
fn context_pack_l2_hit_is_byte_identical_to_fresh_after_registered_volatile_fields() -> TestResult {
    let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
    let workspace_dir = temp.path().join("workspace");
    let home = temp.path().join("home");
    for dir in [&workspace_dir, &home] {
        fs::create_dir_all(dir).map_err(|error| error.to_string())?;
    }
    let workspace = workspace_dir.to_string_lossy().to_string();
    let pack_cache_dir = temp.path().join("pack-cache");
    // A private HOME keeps a global store on this host from bypassing L2, so
    // whether the request hits cannot depend on the worker.
    let run = |args: &[&str], cache_disabled: bool| -> Result<Output, String> {
        Command::new(env!("CARGO_BIN_EXE_ee"))
            .args(args)
            .env_remove("EE_WORKSPACE")
            .env_remove("EE_WORKSPACE_REGISTRY")
            .env_remove("EE_AGENT_NAME")
            .env("EE_EMBED_DOWNLOAD", "off")
            .env("HOME", &home)
            .env("USERPROFILE", &home)
            .env("EE_L2_PACK_CACHE_DIR", &pack_cache_dir)
            .env(
                "EE_L2_PACK_CACHE_DISABLE",
                if cache_disabled { "true" } else { "false" },
            )
            .output()
            .map_err(|error| format!("failed to run ee {}: {error}", args.join(" ")))
    };

    let init = run(&["--workspace", &workspace, "init", "--json"], false)?;
    ensure_success(&init, "init")?;
    // No file: provenance: file-backed evidence bypasses L2 by design.
    for (index, content) in [
        "Release verification runs the full checklist before tagging.",
        "The release checklist requires a clean formatting check.",
    ]
    .into_iter()
    .enumerate()
    {
        let remember = run(
            &[
                "--workspace",
                &workspace,
                "remember",
                content,
                "--level",
                "procedural",
                "--kind",
                "rule",
                "--json",
            ],
            false,
        )?;
        ensure_success(&remember, &format!("remember {index}"))?;
    }
    let rebuild = run(
        &["--workspace", &workspace, "index", "rebuild", "--json"],
        false,
    )?;
    ensure_success(&rebuild, "index rebuild")?;

    let as_of = chrono::Utc::now().to_rfc3339();
    let pack_args = [
        "--workspace",
        workspace.as_str(),
        "pack",
        "release checklist verification",
        "--source-mode",
        "lexical-only",
        "--as-of",
        as_of.as_str(),
        "--json",
    ];
    let producer = run(&pack_args, false)?;
    persist_artifact("l2_identity_producer", &producer);
    ensure_success(&producer, "persisted producer pack")?;

    let mut readonly_args = pack_args.to_vec();
    readonly_args.insert(readonly_args.len() - 1, "--read-only");
    // The hit is observed, not assumed: the same read-only key with
    // --explain-performance reports the cache status.
    let mut probe_args = readonly_args.clone();
    probe_args.insert(probe_args.len() - 1, "--explain-performance");
    let probe = run(&probe_args, false)?;
    persist_artifact("l2_identity_probe", &probe);
    ensure_success(&probe, "read-only cache probe")?;
    ensure_equal(
        &stdout_json(&probe)?
            .pointer("/data/cache/status")
            .and_then(serde_json::Value::as_str),
        &Some("hit"),
        "the read-only request must be served from the producer's L2 entry",
    )?;

    let hit = run(&readonly_args, false)?;
    persist_artifact("l2_identity_hit", &hit);
    ensure_success(&hit, "read-only L2 hit")?;
    assert_stderr_empty(&hit, "read-only L2 hit")?;
    let fresh = run(&readonly_args, true)?;
    persist_artifact("l2_identity_fresh", &fresh);
    ensure_success(&fresh, "read-only fresh pack")?;
    assert_stderr_empty(&fresh, "read-only fresh pack")?;

    let hit_json = stdout_json(&hit)?;
    let fresh_json = stdout_json(&fresh)?;
    let hit_hash = hit_json.pointer("/data/pack/hash").cloned();
    ensure(hit_hash.is_some(), "the hit response must carry pack.hash")?;
    ensure_equal(
        &hit_hash,
        &fresh_json.pointer("/data/pack/hash").cloned(),
        "pack.hash of the L2 hit and of the fresh pack",
    )?;

    // Only the registered channel: the SLO timing measurements, then the
    // wall-clock degradation and the counts derived from it.
    let normalize = |label: &str,
                     value: &serde_json::Value|
     -> Result<(serde_json::Value, Vec<String>, usize), String> {
        let mut normalized = value.clone();
        ensure(
            ee::obs::normalize_pack_slo_measurements(&mut normalized)?,
            format!("{label}: the response must carry a validated pack SLO"),
        )?;
        let slo_paths = changed_json_paths(value, &normalized);
        let before_timing = normalized.clone();
        let timing_dropped = ee::obs::normalize_pack_timing_degradations(&mut normalized);
        let timing_paths = changed_json_paths(&before_timing, &normalized);
        ensure_equal(
            &timing_paths.is_empty(),
            &(timing_dropped == 0),
            &format!("{label}: timing normalizer changes {timing_paths:?}"),
        )?;
        Ok((normalized, slo_paths, timing_dropped))
    };
    let expected_slo_paths = vec![
        "/data/pack/slo/actuals/elapsedMs".to_owned(),
        "/data/pack/slo/elapsedStatus".to_owned(),
        "/data/pack/slo/status".to_owned(),
    ];
    let (hit_normalized, hit_slo_paths, hit_timing) = normalize("hit", &hit_json)?;
    let (fresh_normalized, fresh_slo_paths, fresh_timing) = normalize("fresh", &fresh_json)?;
    for (label, paths) in [("hit", &hit_slo_paths), ("fresh", &fresh_slo_paths)] {
        let mut sorted = paths.clone();
        sorted.sort();
        ensure_equal(
            &sorted,
            &expected_slo_paths,
            &format!("{label}: the SLO normalizer touched exactly these 3 registered fields"),
        )?;
    }
    println!(
        "l2 identity: SLO fields touched hit={} fresh={}; timing degradations dropped hit={hit_timing} fresh={fresh_timing}",
        hit_slo_paths.len(),
        fresh_slo_paths.len()
    );

    let hit_bytes = serde_json::to_string(&hit_normalized).map_err(|error| error.to_string())?;
    let fresh_bytes =
        serde_json::to_string(&fresh_normalized).map_err(|error| error.to_string())?;
    ensure(
        hit_bytes == fresh_bytes,
        format!(
            "the L2 hit and the fresh pack differ outside the registered volatile channel at {:?}",
            changed_json_paths(&fresh_normalized, &hit_normalized)
        ),
    )
}
