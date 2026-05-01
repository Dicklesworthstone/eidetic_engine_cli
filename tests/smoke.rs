#[cfg(unix)]
use std::ffi::OsString;
use std::fmt::Debug;
#[cfg(unix)]
use std::fs;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
#[cfg(unix)]
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
#[cfg(unix)]
use std::time::{SystemTime, UNIX_EPOCH};

type TestResult = Result<(), String>;

fn run_ee(args: &[&str]) -> Result<Output, String> {
    Command::new(env!("CARGO_BIN_EXE_ee"))
        .args(args)
        .output()
        .map_err(|error| format!("failed to run ee {}: {error}", args.join(" ")))
}

#[cfg(unix)]
fn run_ee_with_env(args: &[&str], envs: &[(&str, OsString)]) -> Result<Output, String> {
    let mut command = Command::new(env!("CARGO_BIN_EXE_ee"));
    command.args(args);
    for (key, value) in envs {
        command.env(key, value);
    }
    command
        .output()
        .map_err(|error| format!("failed to run ee {}: {error}", args.join(" ")))
}

#[cfg(unix)]
fn unique_artifact_dir(prefix: &str) -> Result<PathBuf, String> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| format!("clock moved backwards: {error}"))?
        .as_nanos();
    Ok(PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join("ee-smoke-artifacts")
        .join(format!("{prefix}-{}-{now}", std::process::id())))
}

#[cfg(unix)]
fn path_with_fake_cass(fake_dir: &Path) -> Result<OsString, String> {
    let mut entries = vec![fake_dir.to_path_buf()];
    if let Some(existing) = std::env::var_os("PATH") {
        entries.extend(std::env::split_paths(&existing));
    }
    std::env::join_paths(entries).map_err(|error| error.to_string())
}

#[cfg(unix)]
fn write_fake_cass_binary(path: &Path) -> TestResult {
    let script = r#"#!/bin/sh
set -eu
cmd="${1:-}"
case "$cmd" in
  sessions)
    printf '{"sessions":[{"path":"%s","workspace":"%s","agent":"codex","started_at":"2026-04-30T00:00:00Z","message_count":2,"token_count":42,"content_hash":"hash-session-a"}]}\n' "$EE_FAKE_CASS_SESSION" "$EE_FAKE_CASS_WORKSPACE"
    ;;
  view)
    printf '{"lines":[{"line":1,"content":"{\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":\"remember this\"}}"}]}\n'
    ;;
  *)
    echo "unexpected cass command: $cmd" >&2
    exit 64
    ;;
esac
"#;
    fs::write(path, script).map_err(|error| error.to_string())?;
    let mut permissions = fs::metadata(path)
        .map_err(|error| error.to_string())?
        .permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).map_err(|error| error.to_string())
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

fn ensure_contains(haystack: &str, needle: &str, context: &str) -> TestResult {
    ensure(
        haystack.contains(needle),
        format!("{context}: expected output to contain {needle:?}, got {haystack:?}"),
    )
}

fn ensure_no_ansi(output: &str, context: &str) -> TestResult {
    ensure(
        !output.contains('\x1b'),
        format!("{context}: machine output must not contain ANSI escape bytes"),
    )
}

fn ensure_starts_with(haystack: &str, prefix: &str, context: &str) -> TestResult {
    ensure(
        haystack.starts_with(prefix),
        format!("{context}: expected output to start with {prefix:?}, got {haystack:?}"),
    )
}

fn ensure_ends_with(haystack: &str, suffix: char, context: &str) -> TestResult {
    ensure(
        haystack.ends_with(suffix),
        format!("{context}: expected output to end with {suffix:?}, got {haystack:?}"),
    )
}

#[test]
fn status_json_stdout_is_stable_machine_data() -> TestResult {
    let output = run_ee(&["status", "--json"])?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    ensure(
        output.status.success(),
        format!("status --json should succeed; stderr: {stderr}"),
    )?;
    ensure(
        stderr.is_empty(),
        "stderr must be empty for JSON status".to_string(),
    )?;
    ensure_starts_with(
        &stdout,
        "{\"schema\":\"ee.response.v1\"",
        "status JSON schema",
    )?;
    ensure_contains(&stdout, "\"success\":true", "status JSON success flag")?;
    ensure_contains(&stdout, "\"command\":\"status\"", "status JSON command")?;
    ensure_contains(
        &stdout,
        "\"runtime\":\"ready\"",
        "status JSON runtime state",
    )?;
    ensure_contains(
        &stdout,
        "\"engine\":\"asupersync\"",
        "status JSON runtime engine",
    )?;
    ensure_ends_with(&stdout, '\n', "status JSON trailing newline")
}

#[test]
fn global_json_flag_is_order_independent() -> TestResult {
    let before = run_ee(&["--json", "status"])?;
    let after = run_ee(&["status", "--json"])?;

    ensure(before.status.success(), "--json status should succeed")?;
    ensure(after.status.success(), "status --json should succeed")?;
    ensure_equal(
        &before.stdout,
        &after.stdout,
        "global --json output must be order independent",
    )?;
    ensure(
        before.stderr.is_empty(),
        "--json status stderr must be empty",
    )?;
    ensure(
        after.stderr.is_empty(),
        "status --json stderr must be empty",
    )
}

#[test]
fn format_json_global_selects_machine_output() -> TestResult {
    let output = run_ee(&["status", "--format", "json"])?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    ensure(
        output.status.success(),
        format!("status --format json should succeed; stderr: {stderr}"),
    )?;
    ensure(
        stderr.is_empty(),
        "stderr must be empty for JSON status".to_string(),
    )?;
    ensure_starts_with(
        &stdout,
        "{\"schema\":\"ee.response.v1\"",
        "format JSON schema",
    )
}

#[test]
fn robot_global_selects_machine_output() -> TestResult {
    let output = run_ee(&["status", "--robot"])?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    ensure(
        output.status.success(),
        format!("status --robot should succeed; stderr: {stderr}"),
    )?;
    ensure(
        stderr.is_empty(),
        "stderr must be empty for robot status".to_string(),
    )?;
    ensure_starts_with(
        &stdout,
        "{\"schema\":\"ee.response.v1\"",
        "robot JSON schema",
    )
}

#[test]
fn clap_help_keeps_stderr_clean() -> TestResult {
    let output = run_ee(&["--help"])?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    ensure(
        output.status.success(),
        format!("--help should succeed; stderr: {stderr}"),
    )?;
    ensure(
        stderr.is_empty(),
        "help must not write diagnostics".to_string(),
    )?;
    ensure_contains(&stdout, "Usage:", "help usage line")?;
    ensure_contains(&stdout, "status", "help status subcommand")
}

#[test]
fn unknown_command_keeps_stdout_clean() -> TestResult {
    let output = run_ee(&["unknown"])?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    ensure(
        !output.status.success(),
        "unknown command must fail with usage error",
    )?;
    ensure(
        stdout.is_empty(),
        "stdout must stay clean on usage errors".to_string(),
    )?;
    ensure_contains(
        &stderr,
        "error: unrecognized subcommand",
        "unknown command diagnostic",
    )
}

#[cfg(unix)]
#[test]
fn import_cass_json_uses_cass_robot_contract_and_is_idempotent() -> TestResult {
    let root = unique_artifact_dir("import-cass")?;
    let workspace = root.join("workspace");
    let fake_bin_dir = root.join("bin");
    fs::create_dir_all(&workspace).map_err(|error| error.to_string())?;
    fs::create_dir_all(&fake_bin_dir).map_err(|error| error.to_string())?;
    let cass_binary = fake_bin_dir.join("cass");
    write_fake_cass_binary(&cass_binary)?;

    let database = workspace.join(".ee").join("ee.db");
    let session_path = workspace.join("session-a.jsonl");
    let workspace_arg = workspace.to_string_lossy().into_owned();
    let database_arg = database.to_string_lossy().into_owned();
    let session_arg = session_path.to_string_lossy().into_owned();
    let path = path_with_fake_cass(&fake_bin_dir)?;
    let envs = [
        ("PATH", path),
        ("EE_FAKE_CASS_SESSION", OsString::from(session_arg)),
        (
            "EE_FAKE_CASS_WORKSPACE",
            OsString::from(workspace_arg.clone()),
        ),
    ];
    let args = [
        "--workspace",
        workspace_arg.as_str(),
        "--json",
        "import",
        "cass",
        "--database",
        database_arg.as_str(),
        "--limit",
        "1",
    ];

    let first = run_ee_with_env(&args, &envs)?;
    let first_stderr = String::from_utf8_lossy(&first.stderr);
    ensure(
        first.status.success(),
        format!("first import should succeed; stderr: {first_stderr}"),
    )?;
    ensure(
        first.stderr.is_empty(),
        "first import stderr must stay clean",
    )?;
    let first_json: serde_json::Value = serde_json::from_slice(&first.stdout)
        .map_err(|error| format!("first import stdout must be JSON: {error}"))?;
    ensure_equal(
        &first_json["schema"],
        &serde_json::json!("ee.response.v1"),
        "first envelope schema",
    )?;
    ensure_equal(
        &first_json["success"],
        &serde_json::json!(true),
        "first success",
    )?;
    ensure_equal(
        &first_json["data"]["command"],
        &serde_json::json!("import cass"),
        "first command",
    )?;
    ensure_equal(
        &first_json["data"]["status"],
        &serde_json::json!("completed"),
        "first import status",
    )?;
    ensure_equal(
        &first_json["data"]["sessionsImported"],
        &serde_json::json!(1),
        "first imported count",
    )?;
    ensure_equal(
        &first_json["data"]["spansImported"],
        &serde_json::json!(1),
        "first span count",
    )?;
    ensure(database.exists(), "import should create the database")?;

    let second = run_ee_with_env(&args, &envs)?;
    let second_stderr = String::from_utf8_lossy(&second.stderr);
    ensure(
        second.status.success(),
        format!("second import should succeed; stderr: {second_stderr}"),
    )?;
    ensure(
        second.stderr.is_empty(),
        "second import stderr must stay clean",
    )?;
    let second_json: serde_json::Value = serde_json::from_slice(&second.stdout)
        .map_err(|error| format!("second import stdout must be JSON: {error}"))?;
    ensure_equal(
        &second_json["data"]["sessionsImported"],
        &serde_json::json!(0),
        "second imported count",
    )?;
    ensure_equal(
        &second_json["data"]["sessionsSkipped"],
        &serde_json::json!(1),
        "second skipped count",
    )?;
    ensure_equal(
        &second_json["data"]["sessions"][0]["status"],
        &serde_json::json!("skipped"),
        "second session status",
    )
}

#[cfg(unix)]
#[test]
fn import_jsonl_json_validates_imports_and_skips_duplicates() -> TestResult {
    let root = unique_artifact_dir("import-jsonl")?;
    let workspace = root.join("workspace");
    fs::create_dir_all(&workspace).map_err(|error| error.to_string())?;

    let database = workspace.join(".ee").join("ee.db");
    let source = root.join("snapshot.jsonl");
    fs::write(
        &source,
        [
            r#"{"schema":"ee.export.header.v1","format_version":1,"created_at":"2026-04-30T00:00:00Z","workspace_id":"wsp_01234567890123456789012345","workspace_path":"/source","export_scope":"memories","redaction_level":"none","record_count":3,"ee_version":"0.1.0","hostname":null,"export_id":"exp-001","import_source":"native","trust_level":"validated","checksum":null,"signature":null,"source_schema_version":null}"#,
            r#"{"schema":"ee.export.memory.v1","memory_id":"mem_01234567890123456789012345","workspace_id":"wsp_01234567890123456789012345","level":"procedural","kind":"rule","content":"Run cargo fmt --check before release.","importance":0.8,"confidence":0.9,"utility":0.7,"created_at":"2026-04-30T00:00:00Z","updated_at":null,"expires_at":null,"source_agent":"MistySalmon","provenance_uri":"ee-export://fixture","superseded_by":null,"supersedes":null,"redacted":false,"redaction_reason":null}"#,
            r#"{"schema":"ee.export.tag.v1","memory_id":"mem_01234567890123456789012345","tag":"Release","created_at":"2026-04-30T00:00:00Z"}"#,
            r#"{"schema":"ee.export.footer.v1","export_id":"exp-001","completed_at":"2026-04-30T00:01:00Z","total_records":3,"memory_count":1,"link_count":0,"tag_count":1,"audit_count":0,"checksum":null,"success":true,"error_message":null}"#,
        ]
        .join("\n"),
    )
    .map_err(|error| error.to_string())?;

    let workspace_arg = workspace.to_string_lossy().into_owned();
    let database_arg = database.to_string_lossy().into_owned();
    let source_arg = source.to_string_lossy().into_owned();

    let dry_run = run_ee(&[
        "--workspace",
        workspace_arg.as_str(),
        "--json",
        "import",
        "jsonl",
        "--source",
        source_arg.as_str(),
        "--dry-run",
    ])?;
    let dry_stderr = String::from_utf8_lossy(&dry_run.stderr);
    ensure(
        dry_run.status.success(),
        format!("dry-run import should succeed; stderr: {dry_stderr}"),
    )?;
    ensure(dry_run.stderr.is_empty(), "dry-run stderr must stay clean")?;
    let dry_json: serde_json::Value = serde_json::from_slice(&dry_run.stdout)
        .map_err(|error| format!("dry-run stdout must be JSON: {error}"))?;
    ensure_equal(
        &dry_json["data"]["schema"],
        &serde_json::json!("ee.import.jsonl.v1"),
        "dry-run data schema",
    )?;
    ensure_equal(
        &dry_json["data"]["status"],
        &serde_json::json!("dry_run"),
        "dry-run status",
    )?;
    ensure_equal(
        &dry_json["data"]["memoryRecords"],
        &serde_json::json!(1),
        "dry-run memory record count",
    )?;
    ensure_equal(
        &dry_json["data"]["memoriesImported"],
        &serde_json::json!(0),
        "dry-run imported count",
    )?;

    let import_args = [
        "--workspace",
        workspace_arg.as_str(),
        "--json",
        "import",
        "jsonl",
        "--source",
        source_arg.as_str(),
        "--database",
        database_arg.as_str(),
    ];
    let first = run_ee(&import_args)?;
    let first_stderr = String::from_utf8_lossy(&first.stderr);
    ensure(
        first.status.success(),
        format!("first JSONL import should succeed; stderr: {first_stderr}"),
    )?;
    ensure(first.stderr.is_empty(), "first JSONL import stderr clean")?;
    let first_json: serde_json::Value = serde_json::from_slice(&first.stdout)
        .map_err(|error| format!("first import stdout must be JSON: {error}"))?;
    ensure_equal(
        &first_json["data"]["status"],
        &serde_json::json!("completed"),
        "first import status",
    )?;
    ensure_equal(
        &first_json["data"]["memoriesImported"],
        &serde_json::json!(1),
        "first imported memory count",
    )?;
    ensure_equal(
        &first_json["data"]["tagsImported"],
        &serde_json::json!(1),
        "first imported tag count",
    )?;
    ensure(database.exists(), "JSONL import should create the database")?;

    let second = run_ee(&import_args)?;
    let second_stderr = String::from_utf8_lossy(&second.stderr);
    ensure(
        second.status.success(),
        format!("second JSONL import should succeed; stderr: {second_stderr}"),
    )?;
    ensure(second.stderr.is_empty(), "second JSONL import stderr clean")?;
    let second_json: serde_json::Value = serde_json::from_slice(&second.stdout)
        .map_err(|error| format!("second import stdout must be JSON: {error}"))?;
    ensure_equal(
        &second_json["data"]["memoriesImported"],
        &serde_json::json!(0),
        "second imported memory count",
    )?;
    ensure_equal(
        &second_json["data"]["memoriesSkippedDuplicate"],
        &serde_json::json!(1),
        "second duplicate skip count",
    )?;

    let show = run_ee(&[
        "--workspace",
        workspace_arg.as_str(),
        "--json",
        "memory",
        "show",
        "mem_01234567890123456789012345",
        "--database",
        database_arg.as_str(),
    ])?;
    let show_stderr = String::from_utf8_lossy(&show.stderr);
    ensure(
        show.status.success(),
        format!("memory show should find imported memory; stderr: {show_stderr}"),
    )?;
    ensure(show.stderr.is_empty(), "memory show stderr clean")?;
    let show_json: serde_json::Value = serde_json::from_slice(&show.stdout)
        .map_err(|error| format!("memory show stdout must be JSON: {error}"))?;
    ensure_equal(
        &show_json["data"]["memory"]["content"],
        &serde_json::json!("Run cargo fmt --check before release."),
        "imported memory content",
    )?;
    ensure_equal(
        &show_json["data"]["memory"]["trust_class"],
        &serde_json::json!("agent_validated"),
        "imported memory trust class",
    )?;
    ensure_equal(
        &show_json["data"]["memory"]["tags"][0]["name"],
        &serde_json::json!("release"),
        "imported memory tag",
    )
}

#[cfg(unix)]
#[test]
fn remember_persists_and_feeds_search_context_flow() -> TestResult {
    let workspace = unique_artifact_dir("remember-flow")?;
    fs::create_dir_all(&workspace).map_err(|error| error.to_string())?;
    let workspace_arg = workspace.to_string_lossy().into_owned();

    let init = run_ee(&["--workspace", workspace_arg.as_str(), "--json", "init"])?;
    let init_stderr = String::from_utf8_lossy(&init.stderr);
    ensure(
        init.status.success(),
        format!("init should succeed; stderr: {init_stderr}"),
    )?;
    ensure(init.stderr.is_empty(), "init stderr clean")?;
    let _: serde_json::Value = serde_json::from_slice(&init.stdout)
        .map_err(|error| format!("init stdout must be JSON: {error}"))?;

    let remember = run_ee(&[
        "--workspace",
        workspace_arg.as_str(),
        "--json",
        "remember",
        "--level",
        "procedural",
        "--kind",
        "rule",
        "--tags",
        "release,checks",
        "--confidence",
        "0.9",
        "--source",
        "file://README.md#L74-77",
        "Store release checks as durable memory.",
    ])?;
    let remember_stdout = String::from_utf8_lossy(&remember.stdout);
    let remember_stderr = String::from_utf8_lossy(&remember.stderr);
    ensure(
        remember.status.success(),
        format!("remember should succeed; stdout: {remember_stdout}; stderr: {remember_stderr}"),
    )?;
    ensure(remember.stderr.is_empty(), "remember stderr clean")?;
    ensure(
        !remember_stdout.contains("storage_not_implemented"),
        "remember must not report storage_not_implemented after persistence",
    )?;
    let remember_json: serde_json::Value = serde_json::from_slice(&remember.stdout)
        .map_err(|error| format!("remember stdout must be JSON: {error}"))?;
    ensure_equal(
        &remember_json["data"]["persisted"],
        &serde_json::json!(true),
        "remember persisted flag",
    )?;
    ensure_equal(
        &remember_json["data"]["revision_number"],
        &serde_json::json!(1),
        "remember revision number",
    )?;
    ensure_equal(
        &remember_json["data"]["index_status"],
        &serde_json::json!("queued"),
        "remember index status",
    )?;
    ensure_equal(
        &remember_json["data"]["effect_ids"],
        &serde_json::json!([]),
        "remember effect ids placeholder",
    )?;
    ensure_equal(
        &remember_json["data"]["suggested_links"],
        &serde_json::json!([]),
        "remember suggested links placeholder",
    )?;
    ensure_equal(
        &remember_json["data"]["redaction_status"],
        &serde_json::json!("checked"),
        "remember redaction status",
    )?;
    let memory_id = remember_json["data"]["memory_id"]
        .as_str()
        .ok_or_else(|| "remember memory_id must be a string".to_string())?;
    let database_path = workspace.join(".ee").join("ee.db");
    ensure(database_path.exists(), "remember should create database")?;

    let show = run_ee(&[
        "--workspace",
        workspace_arg.as_str(),
        "--json",
        "memory",
        "show",
        memory_id,
    ])?;
    let show_stderr = String::from_utf8_lossy(&show.stderr);
    ensure(
        show.status.success(),
        format!("memory show should find remembered memory; stderr: {show_stderr}"),
    )?;
    ensure(show.stderr.is_empty(), "memory show stderr clean")?;
    let show_json: serde_json::Value = serde_json::from_slice(&show.stdout)
        .map_err(|error| format!("memory show stdout must be JSON: {error}"))?;
    ensure_equal(
        &show_json["data"]["memory"]["content"],
        &serde_json::json!("Store release checks as durable memory."),
        "remembered memory content",
    )?;
    ensure_equal(
        &show_json["data"]["memory"]["trust_class"],
        &serde_json::json!("human_explicit"),
        "remembered memory trust class",
    )?;

    let rebuild = run_ee(&[
        "--workspace",
        workspace_arg.as_str(),
        "--json",
        "index",
        "rebuild",
    ])?;
    let rebuild_stderr = String::from_utf8_lossy(&rebuild.stderr);
    ensure(
        rebuild.status.success(),
        format!("index rebuild should succeed; stderr: {rebuild_stderr}"),
    )?;
    ensure(rebuild.stderr.is_empty(), "index rebuild stderr clean")?;
    let rebuild_json: serde_json::Value = serde_json::from_slice(&rebuild.stdout)
        .map_err(|error| format!("index rebuild stdout must be JSON: {error}"))?;
    ensure_equal(
        &rebuild_json["data"]["memories_indexed"],
        &serde_json::json!(1),
        "index rebuild memory count",
    )?;

    let search = run_ee(&[
        "--workspace",
        workspace_arg.as_str(),
        "--json",
        "search",
        "release checks",
    ])?;
    let search_stderr = String::from_utf8_lossy(&search.stderr);
    ensure(
        search.status.success(),
        format!("search should succeed; stderr: {search_stderr}"),
    )?;
    ensure(search.stderr.is_empty(), "search stderr clean")?;
    let search_json: serde_json::Value = serde_json::from_slice(&search.stdout)
        .map_err(|error| format!("search stdout must be JSON: {error}"))?;
    ensure(
        search_json["data"]["results"]
            .as_array()
            .is_some_and(|results| results.iter().any(|hit| hit["doc_id"] == memory_id)),
        "search results should include remembered memory",
    )?;

    let context = run_ee(&[
        "--workspace",
        workspace_arg.as_str(),
        "--json",
        "context",
        "prepare release",
    ])?;
    let context_stderr = String::from_utf8_lossy(&context.stderr);
    ensure(
        context.status.success(),
        format!("context should succeed; stderr: {context_stderr}"),
    )?;
    ensure(context.stderr.is_empty(), "context stderr clean")?;
    let context_json: serde_json::Value = serde_json::from_slice(&context.stdout)
        .map_err(|error| format!("context stdout must be JSON: {error}"))?;
    ensure_equal(
        &context_json["schema"],
        &serde_json::json!("ee.response.v1"),
        "context schema",
    )?;

    let query_file = workspace.join("task.eeq.json");
    fs::write(
        &query_file,
        r#"{
          "version": "ee.query.v1",
          "query": {"text": "prepare release", "mode": "hybrid"},
          "budget": {"maxTokens": 3000, "candidatePool": 25},
          "output": {"format": "json", "profile": "balanced"}
        }"#,
    )
    .map_err(|error| error.to_string())?;
    let query_file_arg = query_file.to_string_lossy().into_owned();
    let pack = run_ee(&[
        "--workspace",
        workspace_arg.as_str(),
        "pack",
        "--query-file",
        query_file_arg.as_str(),
    ])?;
    let pack_stderr = String::from_utf8_lossy(&pack.stderr);
    ensure(
        pack.status.success(),
        format!("pack query-file should succeed; stderr: {pack_stderr}"),
    )?;
    ensure(pack.stderr.is_empty(), "pack query-file stderr clean")?;
    let pack_json: serde_json::Value = serde_json::from_slice(&pack.stdout)
        .map_err(|error| format!("pack query-file stdout must be JSON: {error}"))?;
    ensure_equal(
        &pack_json["schema"],
        &serde_json::json!("ee.response.v1"),
        "pack query-file schema",
    )?;
    ensure_equal(
        &pack_json["data"]["request"]["query"],
        &serde_json::json!("prepare release"),
        "pack query-file request query",
    )?;
    ensure_no_ansi(&String::from_utf8_lossy(&pack.stdout), "pack JSON stdout")?;

    let unknown_field_query_file = workspace.join("task-unknown-field.eeq.json");
    fs::write(
        &unknown_field_query_file,
        r#"{
          "version": "ee.query.v1",
          "query": {"text": "prepare release"},
          "futureField": true,
          "output": {"format": "json"}
        }"#,
    )
    .map_err(|error| error.to_string())?;
    let unknown_field_query_file_arg = unknown_field_query_file.to_string_lossy().into_owned();
    let unknown_field_pack = run_ee(&[
        "--workspace",
        workspace_arg.as_str(),
        "--json",
        "pack",
        "--query-file",
        unknown_field_query_file_arg.as_str(),
    ])?;
    let unknown_field_stdout = String::from_utf8_lossy(&unknown_field_pack.stdout);
    let unknown_field_stderr = String::from_utf8_lossy(&unknown_field_pack.stderr);
    ensure(
        unknown_field_pack.status.success(),
        format!(
            "pack query-file with unknown field should succeed; stderr: {unknown_field_stderr}"
        ),
    )?;
    ensure(
        unknown_field_pack.stderr.is_empty(),
        "unknown-field pack stderr clean",
    )?;
    ensure_no_ansi(&unknown_field_stdout, "unknown-field pack JSON stdout")?;
    let unknown_field_json: serde_json::Value = serde_json::from_slice(&unknown_field_pack.stdout)
        .map_err(|error| format!("unknown-field pack stdout must be JSON: {error}"))?;
    ensure(
        unknown_field_json["data"]["degraded"]
            .as_array()
            .is_some_and(|items| {
                items
                    .iter()
                    .any(|item| item["code"] == "query_unknown_field")
            }),
        "unknown query-file field should be reported as degraded metadata",
    )
}

#[cfg(unix)]
#[test]
fn pack_query_file_invalid_json_uses_stable_machine_error() -> TestResult {
    let workspace = unique_artifact_dir("pack-invalid-json")?;
    fs::create_dir_all(&workspace).map_err(|error| error.to_string())?;
    let workspace_arg = workspace.to_string_lossy().into_owned();
    let query_file = workspace.join("invalid.eeq.json");
    fs::write(&query_file, "{").map_err(|error| error.to_string())?;
    let query_file_arg = query_file.to_string_lossy().into_owned();

    let output = run_ee(&[
        "--workspace",
        workspace_arg.as_str(),
        "--json",
        "pack",
        "--query-file",
        query_file_arg.as_str(),
    ])?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    ensure(
        !output.status.success(),
        "invalid JSON query-file should fail",
    )?;
    ensure(output.stderr.is_empty(), "invalid JSON stderr clean")?;
    ensure_no_ansi(&stdout, "invalid JSON error stdout")?;
    let json: serde_json::Value = serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("invalid JSON error stdout must be JSON: {error}"))?;
    ensure_equal(
        &json["schema"],
        &serde_json::json!("ee.error.v1"),
        "invalid JSON error schema",
    )?;
    ensure_equal(
        &json["error"]["code"],
        &serde_json::json!("ERR_MALFORMED_JSON"),
        "invalid JSON error code",
    )
}

#[cfg(unix)]
#[test]
fn pack_query_file_unsupported_version_uses_stable_machine_error() -> TestResult {
    let workspace = unique_artifact_dir("pack-unsupported-version")?;
    fs::create_dir_all(&workspace).map_err(|error| error.to_string())?;
    let workspace_arg = workspace.to_string_lossy().into_owned();
    let query_file = workspace.join("future.eeq.json");
    fs::write(
        &query_file,
        r#"{
          "version": "ee.query.v2",
          "query": {"text": "prepare release"}
        }"#,
    )
    .map_err(|error| error.to_string())?;
    let query_file_arg = query_file.to_string_lossy().into_owned();

    let output = run_ee(&[
        "--workspace",
        workspace_arg.as_str(),
        "--json",
        "pack",
        "--query-file",
        query_file_arg.as_str(),
    ])?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    ensure(
        !output.status.success(),
        "unsupported query-file version should fail",
    )?;
    ensure(output.stderr.is_empty(), "unsupported version stderr clean")?;
    ensure_no_ansi(&stdout, "unsupported version error stdout")?;
    let json: serde_json::Value = serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("unsupported version error stdout must be JSON: {error}"))?;
    ensure_equal(
        &json["schema"],
        &serde_json::json!("ee.error.v1"),
        "unsupported version error schema",
    )?;
    ensure_equal(
        &json["error"]["code"],
        &serde_json::json!("ERR_UNKNOWN_VERSION"),
        "unsupported version error code",
    )
}

#[test]
fn learn_experiment_propose_json_exposes_decision_ready_fields() -> TestResult {
    let output = run_ee(&[
        "--json",
        "learn",
        "experiment",
        "propose",
        "--limit",
        "1",
        "--min-expected-value",
        "0.2",
        "--max-attention-tokens",
        "650",
        "--max-runtime-seconds",
        "180",
        "--safety-boundary",
        "human_review",
    ])?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    ensure(
        output.status.success(),
        format!("learn experiment propose should succeed; stderr: {stderr}"),
    )?;
    ensure(
        output.stderr.is_empty(),
        "learn experiment propose stderr clean",
    )?;
    ensure_no_ansi(&stdout, "learn experiment propose JSON stdout")?;
    let json: serde_json::Value = serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("learn experiment propose stdout must be JSON: {error}"))?;
    ensure_equal(
        &json["schema"],
        &serde_json::json!("ee.learn.experiment_proposal.v1"),
        "learn experiment proposal schema",
    )?;
    ensure_equal(&json["success"], &serde_json::json!(true), "success flag")?;
    ensure_equal(&json["returned"], &serde_json::json!(1), "returned count")?;
    ensure(
        json["proposals"]
            .as_array()
            .is_some_and(|proposals| proposals.len() == 1),
        "learn experiment propose should return one proposal",
    )?;
    let proposal = &json["proposals"][0];
    ensure(
        proposal["expectedValue"].is_number(),
        "proposal expectedValue must be numeric",
    )?;
    ensure_equal(
        &proposal["budget"]["attentionTokens"],
        &serde_json::json!(650),
        "proposal attention budget",
    )?;
    ensure_equal(
        &proposal["budget"]["maxRuntimeSeconds"],
        &serde_json::json!(180),
        "proposal runtime budget",
    )?;
    ensure_equal(
        &proposal["safety"]["boundary"],
        &serde_json::json!("human_review"),
        "proposal safety boundary",
    )?;
    ensure_equal(
        &proposal["safety"]["mutationAllowed"],
        &serde_json::json!(false),
        "proposal must not allow mutation",
    )?;
    ensure(
        proposal["decisionImpact"]["decisionId"].is_string(),
        "proposal decision impact must identify the affected decision",
    )
}

// =============================================================================
// Integration Foundation Smoke Tests (EE-313)
//
// These tests verify that the foundational integrations are working:
// - Response envelope schema (ee.response.v1)
// - Asupersync runtime bootstrap
// - SQLModel/FrankenSQLite repository shape (reports degraded until wired)
// - Frankensearch persistent index (reports degraded until wired)
// - CLI runtime boundary and exit codes
// =============================================================================

#[test]
fn integration_foundation_response_envelope_schema() -> TestResult {
    let output = run_ee(&["status", "--json"])?;
    let stdout = String::from_utf8_lossy(&output.stdout);

    ensure(
        output.status.success(),
        "status --json must succeed for envelope test",
    )?;

    let json: serde_json::Value = serde_json::from_str(&stdout)
        .map_err(|e| format!("response envelope must be valid JSON: {e}"))?;

    ensure_equal(
        &json["schema"],
        &serde_json::json!("ee.response.v1"),
        "response envelope schema must be ee.response.v1",
    )?;
    ensure(
        json["success"].as_bool().unwrap_or(false),
        "response envelope success must be true",
    )?;
    ensure(
        json["data"].is_object(),
        "response envelope must have data object",
    )
}

#[test]
fn integration_foundation_asupersync_runtime_reports_correctly() -> TestResult {
    let output = run_ee(&["status", "--json"])?;
    let stdout = String::from_utf8_lossy(&output.stdout);

    ensure(
        output.status.success(),
        "status must succeed for runtime test",
    )?;

    let json: serde_json::Value = serde_json::from_str(&stdout)
        .map_err(|e| format!("status output must be valid JSON: {e}"))?;

    let runtime = &json["data"]["runtime"];
    ensure(runtime.is_object(), "status must include runtime object")?;
    ensure_equal(
        &runtime["engine"],
        &serde_json::json!("asupersync"),
        "runtime engine must be asupersync",
    )?;
    ensure(
        runtime["profile"].is_string(),
        "runtime profile must be a string",
    )?;
    ensure(
        runtime["workerThreads"].is_number(),
        "runtime workerThreads must be a number",
    )
}

#[test]
fn integration_foundation_capability_status_structure() -> TestResult {
    let output = run_ee(&["status", "--json"])?;
    let stdout = String::from_utf8_lossy(&output.stdout);

    ensure(
        output.status.success(),
        "status must succeed for capability test",
    )?;

    let json: serde_json::Value = serde_json::from_str(&stdout)
        .map_err(|e| format!("status output must be valid JSON: {e}"))?;

    let capabilities = &json["data"]["capabilities"];
    ensure(
        capabilities.is_object(),
        "status must include capabilities object",
    )?;
    ensure_equal(
        &capabilities["runtime"],
        &serde_json::json!("ready"),
        "runtime capability must be ready",
    )?;
    ensure(
        capabilities["storage"].is_string(),
        "storage capability status must be a string",
    )?;
    ensure(
        capabilities["search"].is_string(),
        "search capability status must be a string",
    )
}

#[test]
fn integration_foundation_degradation_codes_present_when_unimplemented() -> TestResult {
    let output = run_ee(&["status", "--json"])?;
    let stdout = String::from_utf8_lossy(&output.stdout);

    ensure(
        output.status.success(),
        "status must succeed for degradation test",
    )?;

    let json: serde_json::Value = serde_json::from_str(&stdout)
        .map_err(|e| format!("status output must be valid JSON: {e}"))?;

    let capabilities = &json["data"]["capabilities"];
    let degradations = &json["data"]["degraded"];

    ensure(
        degradations.is_array(),
        "status must include degraded array",
    )?;

    let storage_status = capabilities["storage"].as_str().unwrap_or("");
    let search_status = capabilities["search"].as_str().unwrap_or("");

    if storage_status == "unimplemented" {
        let has_storage_degradation = degradations
            .as_array()
            .map(|arr| {
                arr.iter()
                    .any(|d| d["code"].as_str() == Some("storage_not_implemented"))
            })
            .unwrap_or(false);
        ensure(
            has_storage_degradation,
            "storage_not_implemented degradation must be present when storage is unimplemented",
        )?;
    }

    if search_status == "unimplemented" {
        let has_search_degradation = degradations
            .as_array()
            .map(|arr| {
                arr.iter()
                    .any(|d| d["code"].as_str() == Some("search_not_implemented"))
            })
            .unwrap_or(false);
        ensure(
            has_search_degradation,
            "search_not_implemented degradation must be present when search is unimplemented",
        )?;
    }

    Ok(())
}

#[test]
fn integration_foundation_degradation_includes_repair_hints() -> TestResult {
    let output = run_ee(&["status", "--json"])?;
    let stdout = String::from_utf8_lossy(&output.stdout);

    ensure(
        output.status.success(),
        "status must succeed for repair hint test",
    )?;

    let json: serde_json::Value = serde_json::from_str(&stdout)
        .map_err(|e| format!("status output must be valid JSON: {e}"))?;

    let degradations = &json["data"]["degraded"];
    let arr = degradations.as_array().ok_or("degraded must be an array")?;

    for degradation in arr {
        ensure(
            degradation["code"].is_string(),
            "each degradation must have a code string",
        )?;
        ensure(
            degradation["severity"].is_string(),
            "each degradation must have a severity string",
        )?;
        ensure(
            degradation["message"].is_string(),
            "each degradation must have a message string",
        )?;
        ensure(
            degradation["repair"].is_string(),
            "each degradation must have a repair hint string",
        )?;
    }

    Ok(())
}

#[test]
fn integration_foundation_exit_code_zero_on_success() -> TestResult {
    let output = run_ee(&["status"])?;

    ensure(
        output.status.success(),
        "status command must exit with code 0",
    )?;
    ensure_equal(
        &output.status.code(),
        &Some(0),
        "exit code must be exactly 0 for successful status",
    )
}

#[test]
fn integration_foundation_version_reported_in_status() -> TestResult {
    let output = run_ee(&["status", "--json"])?;
    let stdout = String::from_utf8_lossy(&output.stdout);

    ensure(
        output.status.success(),
        "status must succeed for version test",
    )?;

    let json: serde_json::Value = serde_json::from_str(&stdout)
        .map_err(|e| format!("status output must be valid JSON: {e}"))?;

    let version = json["data"]["version"]
        .as_str()
        .ok_or("version must be a string in status data")?;
    ensure(!version.is_empty(), "version string must not be empty")?;
    ensure(
        version
            .chars()
            .next()
            .map(|c| c.is_ascii_digit())
            .unwrap_or(false),
        "version must start with a digit (semantic versioning)",
    )
}

#[test]
fn integration_foundation_memory_health_structure() -> TestResult {
    let output = run_ee(&["status", "--json"])?;
    let stdout = String::from_utf8_lossy(&output.stdout);

    ensure(
        output.status.success(),
        "status must succeed for memory health test",
    )?;

    let json: serde_json::Value = serde_json::from_str(&stdout)
        .map_err(|e| format!("status output must be valid JSON: {e}"))?;

    let memory_health = &json["data"]["memoryHealth"];
    ensure(
        memory_health.is_object(),
        "status must include memoryHealth object",
    )?;
    ensure(
        memory_health["status"].is_string(),
        "memoryHealth must have status string",
    )?;
    ensure(
        memory_health["totalCount"].is_number(),
        "memoryHealth must have totalCount number",
    )?;
    ensure(
        memory_health["activeCount"].is_number(),
        "memoryHealth must have activeCount number",
    )?;
    ensure(
        memory_health.get("healthScore").is_some(),
        "memoryHealth must include healthScore",
    )?;
    ensure(
        memory_health["scoreComponents"].is_object(),
        "memoryHealth must include scoreComponents object",
    )
}

// =============================================================================
// Schema Contract Drift Tests (EE-306)
//
// These tests verify that public JSON output adheres to the schema contract.
// They detect drift when schemas change without updating the KNOWN_SCHEMAS
// constant or when output fields don't match expected schemas.
// =============================================================================

#[test]
fn contract_drift_response_schema_is_used() -> TestResult {
    // Verify that successful commands use the response schema
    let output = run_ee(&["status", "--json"])?;
    let stdout = String::from_utf8_lossy(&output.stdout);

    let json: serde_json::Value = serde_json::from_str(&stdout)
        .map_err(|e| format!("status output must be valid JSON: {e}"))?;

    let schema = json["schema"].as_str();
    ensure(schema.is_some(), "JSON output must have schema field")?;
    ensure(
        schema == Some("ee.response.v1"),
        format!(
            "successful command must use ee.response.v1, got {:?}",
            schema
        ),
    )
}

#[test]
fn contract_drift_schema_format_is_valid() -> TestResult {
    // Verify schema format follows ee.<namespace>.v<n> pattern
    let output = run_ee(&["status", "--json"])?;
    let stdout = String::from_utf8_lossy(&output.stdout);

    let json: serde_json::Value = serde_json::from_str(&stdout)
        .map_err(|e| format!("status output must be valid JSON: {e}"))?;

    let schema = json["schema"]
        .as_str()
        .ok_or("schema field must be a string")?;

    // Must start with "ee."
    ensure(schema.starts_with("ee."), "schema must start with 'ee.'")?;

    // Must end with ".v" followed by digits
    let parts: Vec<&str> = schema.split('.').collect();
    ensure(parts.len() >= 3, "schema must have at least 3 parts")?;

    let version_part = parts.last().ok_or("schema must have version part")?;
    ensure(
        version_part.starts_with('v'),
        "version part must start with 'v'",
    )?;

    let version_num = &version_part[1..];
    ensure(
        !version_num.is_empty() && version_num.chars().all(|c| c.is_ascii_digit()),
        "version part must be v followed by digits",
    )
}

#[test]
fn contract_drift_agent_docs_all_topics_valid() -> TestResult {
    // Verify that all agent docs topics produce valid output
    let topics = [
        "guide",
        "commands",
        "contracts",
        "schemas",
        "paths",
        "env",
        "exit-codes",
        "fields",
        "errors",
        "formats",
        "examples",
    ];

    for topic in topics {
        let output = run_ee(&["agent-docs", topic, "--json"])?;
        ensure(
            output.status.success(),
            format!("agent-docs {topic} must succeed"),
        )?;

        let stdout = String::from_utf8_lossy(&output.stdout);
        let json: serde_json::Value = serde_json::from_str(&stdout)
            .map_err(|e| format!("agent-docs {topic} must be valid JSON: {e}"))?;

        ensure(
            json["schema"].is_string(),
            format!("agent-docs {topic} must have schema field"),
        )?;
        ensure(
            json["success"].as_bool() == Some(true),
            format!("agent-docs {topic} must have success: true"),
        )?;
    }

    Ok(())
}

#[test]
fn contract_drift_agent_docs_without_topic_valid() -> TestResult {
    // Verify agent-docs without topic lists all topics
    let output = run_ee(&["agent-docs", "--json"])?;
    ensure(output.status.success(), "agent-docs must succeed")?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let json: serde_json::Value =
        serde_json::from_str(&stdout).map_err(|e| format!("agent-docs must be valid JSON: {e}"))?;

    ensure(json["schema"].is_string(), "must have schema field")?;
    ensure(
        json["success"].as_bool() == Some(true),
        "must have success: true",
    )?;
    ensure(json["data"].is_object(), "must have data object")?;

    // Should have topics list
    let topics = &json["data"]["topics"];
    ensure(topics.is_array(), "data.topics must be an array")?;
    ensure(
        topics.as_array().map(|t| t.len()).unwrap_or(0) >= 10,
        "should have at least 10 topics",
    )
}

#[test]
fn contract_drift_success_field_is_boolean() -> TestResult {
    // Verify that success field is always a proper boolean
    let output = run_ee(&["status", "--json"])?;
    let stdout = String::from_utf8_lossy(&output.stdout);

    let json: serde_json::Value =
        serde_json::from_str(&stdout).map_err(|e| format!("status must be valid JSON: {e}"))?;

    let success = &json["success"];
    ensure(
        success.is_boolean(),
        "success must be a boolean, not null or absent",
    )?;

    // If command succeeded, success must be true
    if output.status.success() {
        ensure(
            success.as_bool() == Some(true),
            "successful command must have success: true",
        )?;
    }

    Ok(())
}

#[test]
fn contract_drift_data_field_present_on_success() -> TestResult {
    // Verify that successful commands always have a data field
    let output = run_ee(&["status", "--json"])?;
    ensure(output.status.success(), "status must succeed")?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let json: serde_json::Value =
        serde_json::from_str(&stdout).map_err(|e| format!("status must be valid JSON: {e}"))?;

    ensure(
        json["data"].is_object(),
        "successful command must have data object",
    )
}

#[cfg(unix)]
#[test]
fn walking_skeleton_durability_scenario() -> TestResult {
    use std::time::Instant;

    let workspace = unique_artifact_dir("walking-skeleton")?;
    fs::create_dir_all(&workspace).map_err(|error| error.to_string())?;
    let workspace_arg = workspace.to_string_lossy().into_owned();

    let start = Instant::now();

    // Step 1: Initialize workspace
    let init = run_ee(&["--workspace", workspace_arg.as_str(), "--json", "init"])?;
    let init_stdout = String::from_utf8_lossy(&init.stdout);
    let init_stderr = String::from_utf8_lossy(&init.stderr);
    ensure(
        init.status.success(),
        format!("init should succeed; stderr: {init_stderr}"),
    )?;
    ensure(init.stderr.is_empty(), "init stderr must be empty")?;
    let init_json: serde_json::Value = serde_json::from_slice(&init.stdout)
        .map_err(|error| format!("init stdout must be valid JSON: {error}"))?;
    ensure_equal(
        &init_json["schema"],
        &serde_json::json!("ee.response.v1"),
        "init schema",
    )?;
    ensure_no_ansi(&init_stdout, "init stdout")?;

    // Step 2: Remember first memory (procedural rule)
    let remember1 = run_ee(&[
        "--workspace",
        workspace_arg.as_str(),
        "--json",
        "remember",
        "--level",
        "procedural",
        "--kind",
        "rule",
        "--tags",
        "cargo,release,checks",
        "--confidence",
        "0.95",
        "--source",
        "file://AGENTS.md#L164-L173",
        "Run cargo fmt --check before every release.",
    ])?;
    let remember1_stdout = String::from_utf8_lossy(&remember1.stdout);
    let remember1_stderr = String::from_utf8_lossy(&remember1.stderr);
    ensure(
        remember1.status.success(),
        format!("remember1 should succeed; stderr: {remember1_stderr}"),
    )?;
    ensure(
        remember1.stderr.is_empty(),
        "remember1 stderr must be empty",
    )?;
    let remember1_json: serde_json::Value = serde_json::from_slice(&remember1.stdout)
        .map_err(|error| format!("remember1 stdout must be valid JSON: {error}"))?;
    ensure_equal(
        &remember1_json["data"]["persisted"],
        &serde_json::json!(true),
        "remember1 persisted flag",
    )?;
    let memory1_id = remember1_json["data"]["memory_id"]
        .as_str()
        .ok_or_else(|| "remember1 memory_id must be a string".to_string())?
        .to_string();
    ensure_no_ansi(&remember1_stdout, "remember1 stdout")?;

    // Step 3: Remember second memory (semantic fact)
    let remember2 = run_ee(&[
        "--workspace",
        workspace_arg.as_str(),
        "--json",
        "remember",
        "--level",
        "semantic",
        "--kind",
        "fact",
        "--tags",
        "architecture,async",
        "--confidence",
        "0.85",
        "The runtime uses asupersync, not tokio.",
    ])?;
    let remember2_stderr = String::from_utf8_lossy(&remember2.stderr);
    ensure(
        remember2.status.success(),
        format!("remember2 should succeed; stderr: {remember2_stderr}"),
    )?;
    ensure(
        remember2.stderr.is_empty(),
        "remember2 stderr must be empty",
    )?;
    let remember2_json: serde_json::Value = serde_json::from_slice(&remember2.stdout)
        .map_err(|error| format!("remember2 stdout must be valid JSON: {error}"))?;
    let memory2_id = remember2_json["data"]["memory_id"]
        .as_str()
        .ok_or_else(|| "remember2 memory_id must be a string".to_string())?
        .to_string();
    ensure(
        memory2_id != memory1_id,
        "second remembered memory should get a distinct ID",
    )?;

    // Step 4: Verify database exists
    let database_path = workspace.join(".ee").join("ee.db");
    ensure(database_path.exists(), "database must exist after remember")?;

    // Step 5: Memory show
    let show = run_ee(&[
        "--workspace",
        workspace_arg.as_str(),
        "--json",
        "memory",
        "show",
        &memory1_id,
    ])?;
    let show_stderr = String::from_utf8_lossy(&show.stderr);
    ensure(
        show.status.success(),
        format!("memory show should succeed; stderr: {show_stderr}"),
    )?;
    ensure(show.stderr.is_empty(), "memory show stderr must be empty")?;
    let show_json: serde_json::Value = serde_json::from_slice(&show.stdout)
        .map_err(|error| format!("memory show stdout must be valid JSON: {error}"))?;
    ensure_equal(
        &show_json["data"]["memory"]["content"],
        &serde_json::json!("Run cargo fmt --check before every release."),
        "memory show content",
    )?;
    ensure_equal(
        &show_json["data"]["memory"]["level"],
        &serde_json::json!("procedural"),
        "memory show level",
    )?;
    ensure_equal(
        &show_json["data"]["memory"]["trust_class"],
        &serde_json::json!("human_explicit"),
        "memory show trust_class",
    )?;

    // Step 6: Memory list
    let list = run_ee(&[
        "--workspace",
        workspace_arg.as_str(),
        "--json",
        "memory",
        "list",
    ])?;
    let list_stderr = String::from_utf8_lossy(&list.stderr);
    ensure(
        list.status.success(),
        format!("memory list should succeed; stderr: {list_stderr}"),
    )?;
    ensure(list.stderr.is_empty(), "memory list stderr must be empty")?;
    let list_json: serde_json::Value = serde_json::from_slice(&list.stdout)
        .map_err(|error| format!("memory list stdout must be valid JSON: {error}"))?;
    let memories = list_json["data"]["memories"]
        .as_array()
        .ok_or_else(|| "memory list must have memories array".to_string())?;
    ensure_equal(&memories.len(), &2, "memory list should show 2 memories")?;

    // Step 7: Index rebuild
    let rebuild = run_ee(&[
        "--workspace",
        workspace_arg.as_str(),
        "--json",
        "index",
        "rebuild",
    ])?;
    let rebuild_stderr = String::from_utf8_lossy(&rebuild.stderr);
    ensure(
        rebuild.status.success(),
        format!("index rebuild should succeed; stderr: {rebuild_stderr}"),
    )?;
    ensure(
        rebuild.stderr.is_empty(),
        "index rebuild stderr must be empty",
    )?;
    let rebuild_json: serde_json::Value = serde_json::from_slice(&rebuild.stdout)
        .map_err(|error| format!("index rebuild stdout must be valid JSON: {error}"))?;
    ensure_equal(
        &rebuild_json["data"]["memories_indexed"],
        &serde_json::json!(2),
        "index rebuild should index 2 memories",
    )?;

    // Step 8: Search
    let search = run_ee(&[
        "--workspace",
        workspace_arg.as_str(),
        "--json",
        "search",
        "cargo release checks",
    ])?;
    let search_stderr = String::from_utf8_lossy(&search.stderr);
    ensure(
        search.status.success(),
        format!("search should succeed; stderr: {search_stderr}"),
    )?;
    ensure(search.stderr.is_empty(), "search stderr must be empty")?;
    let search_json: serde_json::Value = serde_json::from_slice(&search.stdout)
        .map_err(|error| format!("search stdout must be valid JSON: {error}"))?;
    ensure(
        search_json["data"]["results"]
            .as_array()
            .is_some_and(|results| !results.is_empty()),
        "search should return results",
    )?;

    // Step 9: Context JSON
    let context_json = run_ee(&[
        "--workspace",
        workspace_arg.as_str(),
        "--json",
        "context",
        "prepare release",
    ])?;
    let context_json_stderr = String::from_utf8_lossy(&context_json.stderr);
    ensure(
        context_json.status.success(),
        format!("context --json should succeed; stderr: {context_json_stderr}"),
    )?;
    ensure(
        context_json.stderr.is_empty(),
        "context --json stderr must be empty",
    )?;
    let context_json_parsed: serde_json::Value = serde_json::from_slice(&context_json.stdout)
        .map_err(|error| format!("context --json stdout must be valid JSON: {error}"))?;
    ensure_equal(
        &context_json_parsed["schema"],
        &serde_json::json!("ee.response.v1"),
        "context --json schema",
    )?;
    ensure_no_ansi(
        &String::from_utf8_lossy(&context_json.stdout),
        "context --json stdout",
    )?;

    // Step 10: Context Markdown
    let context_md = run_ee(&[
        "--workspace",
        workspace_arg.as_str(),
        "--format",
        "markdown",
        "context",
        "prepare release",
    ])?;
    let context_md_stdout = String::from_utf8_lossy(&context_md.stdout);
    let context_md_stderr = String::from_utf8_lossy(&context_md.stderr);
    ensure(
        context_md.status.success(),
        format!("context --format markdown should succeed; stderr: {context_md_stderr}"),
    )?;
    ensure(
        context_md.stderr.is_empty(),
        "context markdown stderr must be empty",
    )?;
    ensure_contains(
        &context_md_stdout,
        "# ",
        "context markdown should have header",
    )?;
    ensure_no_ansi(&context_md_stdout, "context markdown stdout")?;

    // Step 11: Why command
    let why = run_ee(&[
        "--workspace",
        workspace_arg.as_str(),
        "--json",
        "why",
        &memory1_id,
    ])?;
    let why_stdout = String::from_utf8_lossy(&why.stdout);
    let why_stderr = String::from_utf8_lossy(&why.stderr);
    ensure(
        why.status.success(),
        format!("why should succeed; stderr: {why_stderr}"),
    )?;
    ensure(why.stderr.is_empty(), "why stderr must be empty")?;
    let why_json: serde_json::Value = serde_json::from_slice(&why.stdout)
        .map_err(|error| format!("why stdout must be valid JSON: {error}"))?;
    ensure_equal(
        &why_json["schema"],
        &serde_json::json!("ee.response.v1"),
        "why schema",
    )?;
    ensure(
        why_json["data"]["storage"].is_object(),
        "why should include storage explanation",
    )?;
    ensure_no_ansi(&why_stdout, "why stdout")?;

    // Scenario timing
    let elapsed = start.elapsed();
    ensure(
        elapsed.as_secs() < 60,
        format!(
            "walking skeleton scenario should complete in under 60s, took {}s",
            elapsed.as_secs()
        ),
    )?;

    Ok(())
}
