//! EE-u7vb: Pack determinism end-to-end test
//!
//! Validates that context packs are deterministic:
//! same DB + indexes + config + query → identical pack hash and JSON output.
//!
//! NO MOCKS. Real ee binary, real FrankenSQLite, real Frankensearch indexes.

use ee::db::{DbConnection, PACK_REPLAY_LEDGER_SCHEMA_V1};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

type TestResult = Result<(), String>;

const EXIT_SUCCESS: i32 = 0;

fn ee_binary_path() -> Result<PathBuf, String> {
    let cargo_path = PathBuf::from(env!("CARGO_BIN_EXE_ee"));
    if cargo_path.exists() {
        return Ok(cargo_path);
    }

    let current_exe = std::env::current_exe()
        .map_err(|error| format!("failed to resolve current test binary: {error}"))?;
    let debug_dir = current_exe.parent().and_then(Path::parent).ok_or_else(|| {
        format!(
            "failed to resolve debug directory from test binary {}",
            current_exe.display()
        )
    })?;
    let sibling = debug_dir.join("ee");
    if sibling.exists() {
        Ok(sibling)
    } else {
        Err(format!(
            "ee binary not found at {} or {}",
            cargo_path.display(),
            sibling.display()
        ))
    }
}

fn run_ee(args: &[&str]) -> Result<Output, String> {
    Command::new(ee_binary_path()?)
        .args(args)
        .env_remove("EE_WORKSPACE")
        .env_remove("EE_WORKSPACE_REGISTRY")
        .output()
        .map_err(|error| format!("failed to run ee {}: {error}", args.join(" ")))
}

fn run_ee_pack_query_file(workspace: &str, query_file: &Path) -> Result<Output, String> {
    let query_file_arg = query_file.to_string_lossy().into_owned();
    let build_output = run_ee(&[
        "--workspace",
        workspace,
        "pack",
        "build",
        "--query-file",
        &query_file_arg,
        "--json",
    ])?;
    if build_output.status.code() == Some(EXIT_SUCCESS) {
        return Ok(build_output);
    }

    run_ee(&[
        "--workspace",
        workspace,
        "pack",
        "--query-file",
        &query_file_arg,
        "--json",
    ])
}

fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

fn stdout_string(output: &Output) -> Result<String, String> {
    String::from_utf8(output.stdout.clone())
        .map_err(|error| format!("stdout was not UTF-8: {error}"))
}

fn stdout_json(output: &Output) -> Result<serde_json::Value, String> {
    let stdout = stdout_string(output)?;
    serde_json::from_str(&stdout)
        .map_err(|error| format!("stdout was not JSON: {error}\nstdout: {stdout}"))
}

fn ensure_stderr_empty(output: &Output, context: &str) -> TestResult {
    let stderr = String::from_utf8_lossy(&output.stderr);
    ensure(
        stderr.trim().is_empty(),
        format!("{context}: stderr should be empty in JSON mode, got: {stderr}"),
    )
}

fn extract_pack_hash(json: &serde_json::Value) -> Option<String> {
    json.pointer("/data/pack/hash")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

fn extract_item_ids(json: &serde_json::Value) -> Vec<String> {
    json.pointer("/data/pack/items")
        .and_then(|v| v.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.get("memoryId").and_then(|v| v.as_str()))
                .map(|s| s.to_string())
                .collect()
        })
        .unwrap_or_default()
}

fn pack_record_ids_for_selection(
    workspace: &Path,
    query: &str,
    pack_hash: &str,
    selected_item_ids: &[String],
) -> Result<Vec<String>, String> {
    let anchor_memory_id = selected_item_ids
        .first()
        .ok_or_else(|| "pack selected no anchor memory".to_owned())?;
    let database_path = workspace.join(".ee").join("ee.db");
    let connection = DbConnection::open_file(&database_path)
        .map_err(|error| format!("open pack ledger database: {error}"))?;
    let history = connection
        .list_pack_records_for_memory(anchor_memory_id, 20)
        .map_err(|error| format!("list persisted pack records: {error}"))?;

    let ids = history
        .iter()
        .filter(|(record, _item)| record.query == query && record.pack_hash == pack_hash)
        .map(|(record, _item)| record.id.clone())
        .collect();

    connection.close().map_err(|error| error.to_string())?;
    Ok(ids)
}

fn assert_pack_ledger_persisted(
    workspace: &Path,
    query: &str,
    pack_hash: &str,
    selected_item_ids: &[String],
) -> TestResult {
    let anchor_memory_id = selected_item_ids
        .first()
        .ok_or_else(|| "pack selected no anchor memory".to_owned())?;
    let database_path = workspace.join(".ee").join("ee.db");
    let connection = DbConnection::open_file(&database_path)
        .map_err(|error| format!("open pack ledger database: {error}"))?;
    let history = connection
        .list_pack_records_for_memory(anchor_memory_id, 10)
        .map_err(|error| format!("list persisted pack records: {error}"))?;

    let (record, _item) = history
        .iter()
        .find(|(record, _item)| record.query == query && record.pack_hash == pack_hash)
        .ok_or_else(|| {
            format!(
                "no persisted pack record for query {query:?}, hash {pack_hash:?}, anchor {anchor_memory_id}"
            )
        })?;
    let ledger_hash = record
        .ledger_hash
        .as_ref()
        .ok_or_else(|| format!("pack record {} missing ledger_hash", record.id))?;
    let ledger_json = record
        .ledger_json
        .as_ref()
        .ok_or_else(|| format!("pack record {} missing ledger_json", record.id))?;
    let ledger: serde_json::Value = serde_json::from_str(ledger_json)
        .map_err(|error| format!("pack ledger JSON malformed: {error}"))?;

    ensure(
        ledger_hash.starts_with("blake3:"),
        format!("ledger hash must be blake3-prefixed, got {ledger_hash}"),
    )?;
    ensure(
        ledger.pointer("/schema") == Some(&serde_json::json!(PACK_REPLAY_LEDGER_SCHEMA_V1)),
        "ledger schema must be pinned",
    )?;
    ensure(
        ledger.pointer("/ledgerHash") == Some(&serde_json::json!(ledger_hash.as_str())),
        "ledger hash field must match pack_records.ledger_hash",
    )?;
    ensure(
        ledger.pointer("/request/query/text") == Some(&serde_json::json!(query)),
        "ledger must record safe query text",
    )?;
    let ledger_item_ids = ledger
        .pointer("/selectedItems")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| "ledger selectedItems missing".to_owned())?
        .iter()
        .map(|item| {
            item.get("memoryId")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
                .ok_or_else(|| "ledger selected item missing memoryId".to_owned())
        })
        .collect::<Result<Vec<_>, _>>()?;
    ensure(
        ledger_item_ids == selected_item_ids,
        format!(
            "ledger selected item order mismatch: expected {selected_item_ids:?}, got {ledger_item_ids:?}"
        ),
    )?;

    connection.close().map_err(|error| error.to_string())
}

#[test]
fn pack_hash_is_deterministic_across_runs() -> TestResult {
    let tempdir = tempfile::tempdir().map_err(|e| e.to_string())?;
    let workspace = tempdir.path().to_string_lossy().to_string();

    // Step 1: Initialize workspace
    let init = run_ee(&["--workspace", &workspace, "init", "--json"])?;
    ensure(
        init.status.code() == Some(EXIT_SUCCESS),
        format!("init failed: {:?}", init.status.code()),
    )?;

    // Step 2: Add deterministic seed memories
    let memories = [
        "Always run cargo fmt before committing code changes",
        "Unit tests must pass before merging pull requests",
        "The CI pipeline uses GitHub Actions for automation",
        "Database migrations live in the migrations/ directory",
        "Use structured logging with tracing crate",
        "Error handling should use Result types not panics",
        "Configuration is loaded from config.toml",
        "The API follows RESTful conventions",
        "Authentication follows industry best practices",
        "Rate limiting is enforced for API stability",
    ];

    for content in &memories {
        let remember = run_ee(&[
            "--workspace",
            &workspace,
            "remember",
            content,
            "--kind",
            "rule",
            "--json",
        ])?;
        ensure(
            remember.status.code() == Some(EXIT_SUCCESS),
            format!("remember failed for '{content}'"),
        )?;
    }

    // Step 3: Run context query multiple times
    let query_args = [
        "--workspace",
        &workspace,
        "context",
        "code review and testing",
        "--max-tokens",
        "2000",
        "--json",
    ];

    let mut hashes = Vec::new();
    let mut outputs = Vec::new();

    for i in 0..5 {
        let output = run_ee(&query_args)?;
        ensure(
            output.status.code() == Some(EXIT_SUCCESS),
            format!("context run {i} failed"),
        )?;

        let stdout = stdout_string(&output)?;
        let json = stdout_json(&output)?;

        let hash =
            extract_pack_hash(&json).ok_or_else(|| format!("run {i}: missing pack.hash field"))?;

        hashes.push(hash);
        outputs.push(stdout);
    }

    // Step 4: Assert all hashes are identical
    let first_hash = &hashes[0];
    for (i, hash) in hashes.iter().enumerate().skip(1) {
        ensure(
            hash == first_hash,
            format!("pack hash mismatch: run 0 = {first_hash}, run {i} = {hash}"),
        )?;
    }

    // Step 5: Assert all JSON outputs are byte-identical
    let first_output = &outputs[0];
    for (i, output) in outputs.iter().enumerate().skip(1) {
        ensure(
            output == first_output,
            format!(
                "JSON output mismatch between run 0 and run {i}\n\
                 First 200 chars of run 0: {}\n\
                 First 200 chars of run {i}: {}",
                &first_output[..first_output.len().min(200)],
                &output[..output.len().min(200)]
            ),
        )?;
    }

    Ok(())
}

#[test]
fn pack_item_ordering_is_deterministic() -> TestResult {
    let tempdir = tempfile::tempdir().map_err(|e| e.to_string())?;
    let workspace = tempdir.path().to_string_lossy().to_string();

    // Initialize
    let init = run_ee(&["--workspace", &workspace, "init", "--json"])?;
    ensure(init.status.code() == Some(EXIT_SUCCESS), "init failed")?;

    // Add memories with similar relevance to stress tie-breaking
    let memories = [
        "Testing rule alpha: verify inputs",
        "Testing rule beta: validate outputs",
        "Testing rule gamma: check boundaries",
        "Testing rule delta: assert invariants",
        "Testing rule epsilon: confirm idempotency",
    ];

    for content in &memories {
        let remember = run_ee(&[
            "--workspace",
            &workspace,
            "remember",
            content,
            "--kind",
            "rule",
            "--json",
        ])?;
        ensure(
            remember.status.code() == Some(EXIT_SUCCESS),
            format!("remember failed: {content}"),
        )?;
    }

    // Run context multiple times and collect item orderings
    let query_args = [
        "--workspace",
        &workspace,
        "context",
        "testing rules",
        "--max-tokens",
        "4000",
        "--json",
    ];

    let mut orderings: Vec<Vec<String>> = Vec::new();

    for i in 0..5 {
        let output = run_ee(&query_args)?;
        ensure(
            output.status.code() == Some(EXIT_SUCCESS),
            format!("context run {i} failed"),
        )?;

        let json = stdout_json(&output)?;
        let item_ids = extract_item_ids(&json);
        orderings.push(item_ids);
    }

    // Assert all orderings are identical
    let first_ordering = &orderings[0];
    for (i, ordering) in orderings.iter().enumerate().skip(1) {
        ensure(
            ordering == first_ordering,
            format!(
                "item ordering mismatch: run 0 = {:?}, run {i} = {:?}",
                first_ordering, ordering
            ),
        )?;
    }

    Ok(())
}

#[test]
fn pack_query_file_persists_selection_ledger() -> TestResult {
    let tempdir = tempfile::tempdir().map_err(|e| e.to_string())?;
    let workspace = tempdir.path().to_string_lossy().to_string();

    let init = run_ee(&["--workspace", &workspace, "init", "--json"])?;
    ensure(init.status.code() == Some(EXIT_SUCCESS), "init failed")?;
    ensure_stderr_empty(&init, "init")?;

    let memories = [
        "Pack ledger release rule: run cargo fmt before tagging",
        "Pack ledger release failure: clippy warning blocked release",
    ];
    for content in &memories {
        let remember = run_ee(&[
            "--workspace",
            &workspace,
            "remember",
            content,
            "--kind",
            "rule",
            "--json",
        ])?;
        ensure(
            remember.status.code() == Some(EXIT_SUCCESS),
            format!("remember failed for '{content}'"),
        )?;
        ensure_stderr_empty(&remember, "remember")?;
    }

    let index = run_ee(&["--workspace", &workspace, "index", "rebuild", "--json"])?;
    ensure(
        index.status.code() == Some(EXIT_SUCCESS),
        "index rebuild failed",
    )?;
    ensure_stderr_empty(&index, "index rebuild")?;

    let query_file = tempdir.path().join("pack-ledger-query.json");
    let query_text = "pack ledger release";
    fs::write(
        &query_file,
        format!(
            r#"{{
  "version": "ee.query.v1",
  "query": {{"text": "{query_text}"}},
  "budget": {{"maxTokens": 2000, "candidatePool": 20}},
  "output": {{"profile": "compact"}}
}}"#
        ),
    )
    .map_err(|error| format!("failed to write query file: {error}"))?;

    let output = run_ee_pack_query_file(&workspace, &query_file)?;
    ensure(
        output.status.code() == Some(EXIT_SUCCESS),
        format!("pack query-file failed: {:?}", output.status.code()),
    )?;
    ensure_stderr_empty(&output, "pack query-file")?;
    let json = stdout_json(&output)?;
    let pack_hash =
        extract_pack_hash(&json).ok_or_else(|| "pack query-file missing pack hash".to_owned())?;
    let item_ids = extract_item_ids(&json);
    ensure(
        !item_ids.is_empty(),
        "pack query-file should select at least one memory",
    )?;

    assert_pack_ledger_persisted(tempdir.path(), query_text, &pack_hash, &item_ids)
}

#[test]
fn pack_replay_and_diff_work_for_real_pack_records() -> TestResult {
    let tempdir = tempfile::tempdir().map_err(|e| e.to_string())?;
    let workspace = tempdir.path().to_string_lossy().to_string();

    let init = run_ee(&["--json", "--workspace", &workspace, "init"])?;
    ensure(init.status.code() == Some(EXIT_SUCCESS), "init failed")?;
    ensure_stderr_empty(&init, "init")?;

    for content in [
        "Pack replay rule: run cargo fmt before release",
        "Pack replay rule: run cargo clippy before release",
    ] {
        let remember = run_ee(&[
            "--json",
            "--workspace",
            &workspace,
            "remember",
            content,
            "--kind",
            "rule",
        ])?;
        ensure(
            remember.status.code() == Some(EXIT_SUCCESS),
            format!("remember failed for '{content}'"),
        )?;
        ensure_stderr_empty(&remember, "remember")?;
    }

    let index = run_ee(&["--json", "--workspace", &workspace, "index", "rebuild"])?;
    ensure(
        index.status.code() == Some(EXIT_SUCCESS),
        "index rebuild failed",
    )?;
    ensure_stderr_empty(&index, "index rebuild")?;

    let query_file = tempdir.path().join("pack-replay-query.json");
    let query_text = "pack replay release";
    fs::write(
        &query_file,
        format!(
            r#"{{
  "version": "ee.query.v1",
  "query": {{"text": "{query_text}"}},
  "budget": {{"maxTokens": 2000, "candidatePool": 20}},
  "output": {{"profile": "compact"}}
}}"#
        ),
    )
    .map_err(|error| format!("failed to write query file: {error}"))?;

    let first_output = run_ee_pack_query_file(&workspace, &query_file)?;
    ensure(
        first_output.status.code() == Some(EXIT_SUCCESS),
        format!(
            "first pack query-file failed: {:?}",
            first_output.status.code()
        ),
    )?;
    ensure_stderr_empty(&first_output, "first pack query-file")?;
    let first_json = stdout_json(&first_output)?;
    let first_hash =
        extract_pack_hash(&first_json).ok_or_else(|| "first pack missing hash".to_owned())?;
    let first_item_ids = extract_item_ids(&first_json);
    assert_pack_ledger_persisted(tempdir.path(), query_text, &first_hash, &first_item_ids)?;
    let before_ids =
        pack_record_ids_for_selection(tempdir.path(), query_text, &first_hash, &first_item_ids)?;
    let first_pack_id = before_ids
        .first()
        .ok_or_else(|| "first pack record id missing".to_owned())?
        .clone();

    let replay = run_ee(&[
        "--json",
        "--workspace",
        &workspace,
        "pack",
        "replay",
        &first_pack_id,
    ])?;
    ensure(
        replay.status.code() == Some(EXIT_SUCCESS),
        format!("pack replay failed: {:?}", replay.status.code()),
    )?;
    ensure_stderr_empty(&replay, "pack replay")?;
    let replay_json = stdout_json(&replay)?;
    ensure(
        replay_json.pointer("/schema") == Some(&serde_json::json!("ee.pack.replay.v1")),
        "pack replay schema mismatch",
    )?;
    ensure(
        replay_json.pointer("/data/pack/id") == Some(&serde_json::json!(first_pack_id.as_str())),
        "pack replay should identify the requested pack",
    )?;
    ensure(
        replay_json.pointer("/data/replay/status") == Some(&serde_json::json!("available")),
        "pack replay should report an available persisted ledger",
    )?;
    let replay_ledger_pack_id = replay_json
        .pointer("/data/replay/ledger/core/packId")
        .or_else(|| replay_json.pointer("/data/replay/ledger/packId"));
    ensure(
        replay_ledger_pack_id == Some(&serde_json::json!(first_pack_id.as_str())),
        "pack replay ledger should match requested pack id",
    )?;

    let second_output = run_ee_pack_query_file(&workspace, &query_file)?;
    ensure(
        second_output.status.code() == Some(EXIT_SUCCESS),
        format!(
            "second pack query-file failed: {:?}",
            second_output.status.code()
        ),
    )?;
    ensure_stderr_empty(&second_output, "second pack query-file")?;
    let second_json = stdout_json(&second_output)?;
    let second_hash =
        extract_pack_hash(&second_json).ok_or_else(|| "second pack missing hash".to_owned())?;
    let second_item_ids = extract_item_ids(&second_json);
    ensure(
        second_hash == first_hash,
        format!("same input should produce same pack hash: {first_hash} vs {second_hash}"),
    )?;
    ensure(
        second_item_ids == first_item_ids,
        "same input should select the same pack item order",
    )?;
    assert_pack_ledger_persisted(tempdir.path(), query_text, &second_hash, &second_item_ids)?;
    let after_ids =
        pack_record_ids_for_selection(tempdir.path(), query_text, &second_hash, &second_item_ids)?;
    let second_pack_id = after_ids
        .iter()
        .find(|id| !before_ids.contains(id))
        .ok_or_else(|| {
            format!("second pack record id missing; before={before_ids:?}, after={after_ids:?}")
        })?
        .clone();

    let diff = run_ee(&[
        "--json",
        "--workspace",
        &workspace,
        "pack",
        "diff",
        &first_pack_id,
        &second_pack_id,
    ])?;
    ensure(
        diff.status.code() == Some(EXIT_SUCCESS),
        format!("pack diff failed: {:?}", diff.status.code()),
    )?;
    ensure_stderr_empty(&diff, "pack diff")?;
    let diff_json = stdout_json(&diff)?;
    ensure(
        diff_json.pointer("/schema") == Some(&serde_json::json!("ee.pack.diff.v1")),
        "pack diff schema mismatch",
    )?;
    ensure(
        diff_json.pointer("/data/diff/summary/replayable") == Some(&serde_json::json!(true)),
        "pack diff should be replayable when both ledgers exist",
    )?;
    ensure(
        diff_json.pointer("/data/diff/summary/hashMatch") == Some(&serde_json::json!(true)),
        "pack diff should report matching pack hashes for repeated input",
    )?;
    ensure(
        diff_json.pointer("/data/diff/summary/changedCount") == Some(&serde_json::json!(0)),
        "pack diff should report no changed items for repeated input",
    )?;
    ensure(
        diff_json.pointer("/data/diff/likelyCauses") == Some(&serde_json::json!(["no_change"])),
        "pack diff should explain repeated packs as no_change",
    )
}

#[test]
fn low_match_pack_is_deterministic() -> TestResult {
    let tempdir = tempfile::tempdir().map_err(|e| e.to_string())?;
    let workspace = tempdir.path().to_string_lossy().to_string();

    // Initialize and add one memory to create indexes
    let init = run_ee(&["--workspace", &workspace, "init", "--json"])?;
    ensure(init.status.code() == Some(EXIT_SUCCESS), "init failed")?;

    // Add a memory about something unrelated to our query
    let remember = run_ee(&[
        "--workspace",
        &workspace,
        "remember",
        "The database uses PostgreSQL for persistence",
        "--kind",
        "fact",
        "--json",
    ])?;
    ensure(
        remember.status.code() == Some(EXIT_SUCCESS),
        "remember failed",
    )?;

    // Run context query for unrelated topic
    let query_args = [
        "--workspace",
        &workspace,
        "context",
        "quantum physics formulas xyz123",
        "--json",
    ];

    let mut hashes = Vec::new();

    for i in 0..3 {
        let output = run_ee(&query_args)?;
        ensure(
            output.status.code() == Some(EXIT_SUCCESS),
            format!("context run {i} failed"),
        )?;

        let json = stdout_json(&output)?;
        if let Some(hash) = extract_pack_hash(&json) {
            hashes.push(hash);
        }
    }

    // If hashes exist, they should be identical
    if hashes.len() > 1 {
        let first = &hashes[0];
        for (i, hash) in hashes.iter().enumerate().skip(1) {
            ensure(
                hash == first,
                format!("low-match pack hash mismatch: run 0 = {first}, run {i} = {hash}"),
            )?;
        }
    }

    Ok(())
}
