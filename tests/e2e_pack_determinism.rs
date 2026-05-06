//! EE-u7vb: Pack determinism end-to-end test
//!
//! Validates that context packs are deterministic:
//! same DB + indexes + config + query → identical pack hash and JSON output.
//!
//! NO MOCKS. Real ee binary, real FrankenSQLite, real Frankensearch indexes.

use std::process::{Command, Output};

type TestResult = Result<(), String>;

const EXIT_SUCCESS: i32 = 0;

fn run_ee(args: &[&str]) -> Result<Output, String> {
    Command::new(env!("CARGO_BIN_EXE_ee"))
        .args(args)
        .env_remove("EE_WORKSPACE")
        .env_remove("EE_WORKSPACE_REGISTRY")
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

fn stdout_string(output: &Output) -> Result<String, String> {
    String::from_utf8(output.stdout.clone())
        .map_err(|error| format!("stdout was not UTF-8: {error}"))
}

fn stdout_json(output: &Output) -> Result<serde_json::Value, String> {
    let stdout = stdout_string(output)?;
    serde_json::from_str(&stdout)
        .map_err(|error| format!("stdout was not JSON: {error}\nstdout: {stdout}"))
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
