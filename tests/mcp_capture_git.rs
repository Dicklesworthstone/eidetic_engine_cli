//! Real MCP -> CLI -> Git -> database tests. No mocked CLI response or writer.
use super::*;
use ee::db::DbConnection;
use std::process::Command;

fn git(dir: &Path, args: &[&str]) -> Result<String, String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "commit.gpgsign=false"])
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .map_err(|e| e.to_string())?;
    // bd-w5bza: exit code and stdout belong here beside stderr. This printed
    // stderr alone, which is the `[code+stdout]` gap -- and for an `ee --json`
    // invocation it is the worst two to omit, because the ee.error.v2 envelope
    // goes to STDOUT while stderr stays empty, and exit 130 is
    // Outcome::Cancelled. Without them a cancellation reads as a rejection.
    ensure(
        output.status.success(),
        format!(
            "fixture git failed: exit: {:?}; stdout: {}; stderr: {}",
            output.status.code(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ),
    )?;
    String::from_utf8(output.stdout).map_err(|e| e.to_string())
}

fn fixture() -> Result<tempfile::TempDir, String> {
    let root = tempfile::tempdir().map_err(|e| e.to_string())?;
    git(root.path(), &["init", "--quiet", "--initial-branch=main"])?;
    git(root.path(), &["config", "user.name", "Capture Fixture"])?;
    git(
        root.path(),
        &["config", "user.email", "capture@example.invalid"],
    )?;
    fs::write(
        root.path().join("engine.rs"),
        "pub fn release_safety() { }\n",
    )
    .map_err(|e| e.to_string())?;
    git(root.path(), &["add", "engine.rs"])?;
    git(root.path(), &["commit", "-qm", "Record release safety"])?;
    init_workspace(root.path())?;
    Ok(root)
}

fn capture_args(dir: &Path, mode: &str) -> JsonValue {
    json!({"workspace": dir, "mode": mode, "noAutoLink": true, "noProposeCandidates": true})
}

fn capture(arguments: JsonValue) -> Result<JsonValue, String> {
    let response = run_mcp_tool_call("ee_capture_git", arguments)?;
    ensure(
        response.pointer("/result/isError") == Some(&json!(false)),
        format!("capture failed: {response}"),
    )?;
    serde_json::from_str(&extract_mcp_tool_text(&response)?).map_err(|e| e.to_string())
}

fn counts(dir: &Path) -> Result<Vec<(String, i64)>, String> {
    let db = DbConnection::open_file_read_only(dir.join(".ee/ee.db")).map_err(|e| e.to_string())?;
    let result = db
        .list_user_tables()
        .map_err(|e| e.to_string())?
        .into_iter()
        .map(|table| {
            db.count_table_rows(&table)
                .map(|count| (table, count))
                .map_err(|e| e.to_string())
        })
        .collect::<Result<Vec<_>, _>>()?;
    db.close().map_err(|e| e.to_string())?;
    Ok(result)
}

#[test]
fn mcp_capture_git_preview_matches_all_cli_modes_without_durable_writes() -> TestResult {
    let root = fixture()?;
    fs::write(
        root.path().join("engine.rs"),
        "pub fn release_safety() { verify_release(); }\n",
    )
    .map_err(|e| e.to_string())?;
    let before = counts(root.path())?;
    let index = fs::read(root.path().join(".git/index")).map_err(|e| e.to_string())?;
    for (mode, flags) in [
        ("commit", vec!["--from-commit", "HEAD"]),
        ("diff", vec!["--from-diff", "HEAD"]),
        ("worktree", vec!["--from-worktree"]),
    ] {
        let mut cli_args = vec![
            OsString::from("ee"),
            OsString::from("--json"),
            OsString::from("--workspace"),
            root.path().into(),
            OsString::from("remember"),
        ];
        cli_args.extend(flags.into_iter().map(OsString::from));
        cli_args
            .extend(["--dry-run", "--no-auto-link", "--no-propose-candidates"].map(OsString::from));
        let (exit, stdout, stderr) = run_cli(cli_args);
        ensure(
            exit == ee::models::ProcessExitCode::Success,
            format!("CLI capture failed: {stderr} {stdout}"),
        )?;
        let mut input = capture_args(root.path(), mode);
        input["allowWrite"] = json!(true); // permission alone must not apply
        if mode == "diff" {
            input["reference"] = json!("HEAD");
        }
        let result = capture(input)?;
        ensure(
            result.pointer("/data/dry_run") == Some(&json!(true)),
            "capture must preview",
        )?;
        ensure(
            result.pointer("/data/persisted") == Some(&json!(false)),
            "preview persisted",
        )?;
        assert_json_equal_modulo_timestamps(&stdout, &result.to_string(), mode)?;
    }
    assert_eq!(counts(root.path())?, before);
    assert_eq!(
        fs::read(root.path().join(".git/index")).map_err(|e| e.to_string())?,
        index
    );
    Ok(())
}

#[test]
fn mcp_capture_git_applies_once_and_retains_sanitized_audited_source() -> TestResult {
    let root = fixture()?;
    let token = format!("ghp_{}", "Q".repeat(36));
    fs::write(
        root.path().join("engine.rs"),
        format!("pub fn release_safety() {{ verify_release(); }}\n// label-{token}\n"),
    )
    .map_err(|e| e.to_string())?;
    let mut input = capture_args(root.path(), "worktree");
    input["dryRun"] = json!(false);
    let before = counts(root.path())?;
    let denied = run_mcp_tool_call("ee_capture_git", input.clone())?;
    ensure(
        denied.pointer("/error/code") == Some(&json!(-32602)),
        "missing write permission was accepted",
    )?;
    assert_eq!(counts(root.path())?, before);
    input["allowWrite"] = json!(true);
    input["idempotencyKey"] = json!("release-capture");
    input["tags"] = json!("release-probe");
    let result = capture(input.clone())?;
    ensure(
        result.pointer("/data/persisted") == Some(&json!(true)),
        "capture did not persist",
    )?;
    let id = result
        .pointer("/data/memory_id")
        .and_then(JsonValue::as_str)
        .ok_or("capture has no memory identity")?;
    let audit = result
        .pointer("/data/audit_id")
        .and_then(JsonValue::as_str)
        .ok_or("capture has no audit identity")?;
    let db = DbConnection::open_file_read_only(root.path().join(".ee/ee.db"))
        .map_err(|e| e.to_string())?;
    let memory = db
        .get_memory(id)
        .map_err(|e| e.to_string())?
        .ok_or("captured memory missing")?;
    assert!(memory.content.contains("verify_release"));
    assert!(!memory.content.contains(&token));
    assert!(
        db.get_memory_tags(id)
            .map_err(|e| e.to_string())?
            .contains(&"release-probe".to_owned())
    );
    assert!(
        db.list_audit_by_target("memory", id, None)
            .map_err(|e| e.to_string())?
            .iter()
            .any(|row| row.id == audit)
    );
    db.close().map_err(|e| e.to_string())?;
    let after_apply = counts(root.path())?;
    let replay = capture(input.clone())?;
    assert_eq!(
        replay.pointer("/data/status").and_then(JsonValue::as_str),
        Some("already_recorded")
    );
    assert_eq!(
        replay.pointer("/data/memoryId").and_then(JsonValue::as_str),
        Some(id)
    );
    assert_eq!(counts(root.path())?, after_apply);
    // New evidence under an old retry key must not be silently discarded or
    // create another memory. The core remember conflict survives the adapter.
    fs::write(
        root.path().join("engine.rs"),
        "pub fn release_safety() { verify_signatures(); }\n",
    )
    .map_err(|e| e.to_string())?;
    let conflict = run_mcp_tool_call("ee_capture_git", input)?;
    assert_eq!(conflict.pointer("/result/isError"), Some(&json!(true)));
    assert_eq!(counts(root.path())?, after_apply);
    let search = run_mcp_tool_call(
        "ee_search",
        json!({"workspace": root.path(), "query": "verify_release", "limit": 5}),
    )?;
    ensure(
        search.pointer("/result/isError") == Some(&json!(false)),
        "captured memory search failed",
    )?;
    let found: JsonValue =
        serde_json::from_str(&extract_mcp_tool_text(&search)?).map_err(|e| e.to_string())?;
    assert!(
        found
            .pointer("/data/results")
            .and_then(JsonValue::as_array)
            .is_some_and(|rows| rows.iter().any(
                |row| row["memoryId"].as_str() == Some(id) && row["docId"].as_str() == Some(id)
            )),
        "the MCP capture must be searchable through its ordinary index job: {found}"
    );
    assert!(!found.to_string().contains(&token));
    Ok(())
}
