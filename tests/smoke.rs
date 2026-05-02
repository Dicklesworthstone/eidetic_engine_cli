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
use std::time::{Instant, SystemTime, UNIX_EPOCH};

#[cfg(unix)]
use ee::db::{
    CreateCurationCandidateInput, CreateMemoryLinkInput, DatabaseConfig, DbConnection,
    MemoryLinkRelation, MemoryLinkSource,
};
#[cfg(unix)]
use ee::graph::{
    AutolinkCandidateOptions, AutolinkExistingEdge, AutolinkMemoryInput,
    generate_autolink_candidates,
};

type TestResult = Result<(), String>;

fn run_ee(args: &[&str]) -> Result<Output, String> {
    Command::new(env!("CARGO_BIN_EXE_ee"))
        .args(args)
        .output()
        .map_err(|error| format!("failed to run ee {}: {error}", args.join(" ")))
}

#[cfg(unix)]
fn run_ee_with_env(args: &[&str], envs: &[(&str, OsString)]) -> Result<Output, String> {
    let cwd = std::env::current_dir().map_err(|error| error.to_string())?;
    run_ee_with_env_in_dir(args, envs, &cwd)
}

#[cfg(unix)]
fn run_ee_in_dir(args: &[&str], cwd: &Path) -> Result<Output, String> {
    Command::new(env!("CARGO_BIN_EXE_ee"))
        .current_dir(cwd)
        .args(args)
        .output()
        .map_err(|error| {
            format!(
                "failed to run ee {} from {}: {error}",
                args.join(" "),
                cwd.display()
            )
        })
}

#[cfg(unix)]
fn run_ee_with_env_in_dir(
    args: &[&str],
    envs: &[(&str, OsString)],
    cwd: &Path,
) -> Result<Output, String> {
    let mut command = Command::new(env!("CARGO_BIN_EXE_ee"));
    command.current_dir(cwd).args(args);
    for (key, value) in envs {
        command.env(key, value);
    }
    command.output().map_err(|error| {
        format!(
            "failed to run ee {} from {}: {error}",
            args.join(" "),
            cwd.display()
        )
    })
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
struct LoggedEeRun {
    output: Output,
    dossier_dir: PathBuf,
}

#[cfg(unix)]
fn unique_e2e_dossier_dir(scenario: &str) -> Result<PathBuf, String> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| format!("clock moved backwards: {error}"))?
        .as_nanos();
    Ok(PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join("ee-e2e")
        .join(scenario)
        .join(format!("{}-{now}", std::process::id())))
}

#[cfg(unix)]
fn write_json_artifact(path: &Path, value: &serde_json::Value) -> TestResult {
    let mut content = serde_json::to_string_pretty(value).map_err(|error| error.to_string())?;
    content.push('\n');
    fs::write(path, content).map_err(|error| error.to_string())
}

#[cfg(unix)]
fn run_ee_logged(
    scenario: &str,
    args: &[&str],
    workspace: &Path,
    query_file: Option<&Path>,
    fixture_id: &str,
    schema: &str,
    golden_path: Option<&str>,
) -> Result<LoggedEeRun, String> {
    run_ee_logged_with_env_in_dir(
        scenario,
        args,
        workspace,
        query_file,
        fixture_id,
        schema,
        golden_path,
        &[],
        None,
    )
}

#[cfg(unix)]
#[allow(clippy::too_many_arguments)]
fn run_ee_logged_with_env(
    scenario: &str,
    args: &[&str],
    workspace: &Path,
    query_file: Option<&Path>,
    fixture_id: &str,
    schema: &str,
    golden_path: Option<&str>,
    envs: &[(&str, OsString)],
) -> Result<LoggedEeRun, String> {
    run_ee_logged_with_env_in_dir(
        scenario,
        args,
        workspace,
        query_file,
        fixture_id,
        schema,
        golden_path,
        envs,
        None,
    )
}

#[cfg(unix)]
#[allow(clippy::too_many_arguments)]
fn run_ee_logged_with_env_in_dir(
    scenario: &str,
    args: &[&str],
    workspace: &Path,
    query_file: Option<&Path>,
    fixture_id: &str,
    schema: &str,
    golden_path: Option<&str>,
    envs: &[(&str, OsString)],
    cwd_override: Option<&Path>,
) -> Result<LoggedEeRun, String> {
    let dossier_dir = unique_e2e_dossier_dir(scenario)?;
    fs::create_dir_all(&dossier_dir).map_err(|error| error.to_string())?;

    fs::write(
        dossier_dir.join("command.txt"),
        format!("ee {}\n", args.join(" ")),
    )
    .map_err(|error| error.to_string())?;
    let cwd = match cwd_override {
        Some(path) => path.to_path_buf(),
        None => std::env::current_dir().map_err(|error| error.to_string())?,
    };
    fs::write(dossier_dir.join("cwd.txt"), format!("{}\n", cwd.display()))
        .map_err(|error| error.to_string())?;
    fs::write(
        dossier_dir.join("workspace.txt"),
        format!("{}\n", workspace.display()),
    )
    .map_err(|error| error.to_string())?;
    let env_overrides = sanitized_env_overrides(envs);
    write_json_artifact(
        &dossier_dir.join("env.sanitized.json"),
        &serde_json::json!({
            "overrides": env_overrides,
            "sensitiveEnvOmitted": true,
            "toolchain": "cargo-test",
            "featureProfile": "default"
        }),
    )?;

    if let Some(query_file) = query_file {
        fs::write(
            dossier_dir.join("query-file.txt"),
            format!("{}\n", query_file.display()),
        )
        .map_err(|error| error.to_string())?;
        let query_bytes = fs::read(query_file).map_err(|error| error.to_string())?;
        fs::write(
            dossier_dir.join("query-file.blake3.txt"),
            format!("blake3:{}\n", blake3::hash(&query_bytes).to_hex()),
        )
        .map_err(|error| error.to_string())?;
    }

    let started = Instant::now();
    let output = run_ee_with_env_in_dir(args, envs, &cwd)?;
    let elapsed_ms = started.elapsed().as_millis();

    fs::write(
        dossier_dir.join("exit-code.txt"),
        format!("{}\n", output.status.code().unwrap_or(-1)),
    )
    .map_err(|error| error.to_string())?;
    fs::write(
        dossier_dir.join("elapsed-ms.txt"),
        format!("{elapsed_ms}\n"),
    )
    .map_err(|error| error.to_string())?;
    fs::write(dossier_dir.join("stdout"), &output.stdout).map_err(|error| error.to_string())?;
    fs::write(dossier_dir.join("stderr"), &output.stderr).map_err(|error| error.to_string())?;
    fs::write(dossier_dir.join("stderr.events.jsonl"), "").map_err(|error| error.to_string())?;

    let stdout_text = String::from_utf8_lossy(&output.stdout);
    let stdout_json = serde_json::from_slice::<serde_json::Value>(&output.stdout).ok();
    let schema_status = stdout_json
        .as_ref()
        .and_then(|value| value.get("schema"))
        .and_then(serde_json::Value::as_str)
        .map_or("missing", |actual| {
            if actual == schema {
                "matched"
            } else {
                "mismatched"
            }
        });
    write_json_artifact(
        &dossier_dir.join("stdout.schema.json"),
        &serde_json::json!({
            "fixtureId": fixture_id,
            "schema": schema,
            "parseStatus": if stdout_json.is_some() { "parsed" } else { "not_json" },
            "schemaStatus": schema_status,
            "stdoutPath": dossier_dir.join("stdout").display().to_string(),
            "stderrPath": dossier_dir.join("stderr").display().to_string(),
            "goldenPath": golden_path,
            "goldenStatus": if golden_path.is_some() { "covered_by_contract" } else { "not_applicable" }
        }),
    )?;

    let degraded_codes = stdout_json
        .as_ref()
        .and_then(|value| value.pointer("/data/degraded"))
        .and_then(serde_json::Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.get("code").and_then(serde_json::Value::as_str))
                .map(str::to_owned)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    write_json_artifact(
        &dossier_dir.join("degradation-report.json"),
        &serde_json::json!({
            "status": if degraded_codes.is_empty() { "none" } else { "present" },
            "codes": degraded_codes,
            "repair": null
        }),
    )?;
    write_json_artifact(
        &dossier_dir.join("redaction-report.json"),
        &serde_json::json!({
            "status": "checked",
            "redactedClasses": [],
            "secretPatternsObserved": stdout_text.contains("sk-")
        }),
    )?;

    let first_failure = if output.status.success() {
        "No failure observed.\n".to_string()
    } else {
        let stderr_text = String::from_utf8_lossy(&output.stderr);
        format!(
            "Exit code: {:?}\n\nstderr excerpt:\n{}\n",
            output.status.code(),
            stderr_text.lines().take(8).collect::<Vec<_>>().join("\n")
        )
    };
    fs::write(dossier_dir.join("first-failure.md"), first_failure)
        .map_err(|error| error.to_string())?;

    Ok(LoggedEeRun {
        output,
        dossier_dir,
    })
}

#[cfg(unix)]
fn sanitized_env_overrides(envs: &[(&str, OsString)]) -> serde_json::Value {
    let mut overrides = serde_json::Map::new();
    for (key, value) in envs {
        let value = if *key == "PATH" {
            "<path-list>".to_string()
        } else {
            value.to_string_lossy().into_owned()
        };
        overrides.insert((*key).to_string(), serde_json::json!(value));
    }
    serde_json::Value::Object(overrides)
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

#[cfg(unix)]
fn parse_logged_response(run: &LoggedEeRun, context: &str) -> Result<serde_json::Value, String> {
    let stderr = String::from_utf8_lossy(&run.output.stderr);
    ensure(
        run.output.status.success(),
        format!("{context} should succeed; stderr: {stderr}"),
    )?;
    ensure(
        run.output.stderr.is_empty(),
        format!("{context} stderr must be empty"),
    )?;
    let stdout = String::from_utf8_lossy(&run.output.stdout);
    ensure_no_ansi(&stdout, context)?;
    let json: serde_json::Value = serde_json::from_slice(&run.output.stdout)
        .map_err(|error| format!("{context} stdout must be JSON: {error}"))?;
    ensure_equal(
        &json["schema"],
        &serde_json::json!("ee.response.v1"),
        context,
    )?;
    ensure_equal(&json["success"], &serde_json::json!(true), context)?;
    Ok(json)
}

#[cfg(unix)]
fn assert_curate_review_json(
    json: &serde_json::Value,
    command: &str,
    action: &str,
    to_status: &str,
    to_review_state: &str,
) -> TestResult {
    ensure_equal(
        &json["data"]["schema"],
        &serde_json::json!("ee.curate.review.v1"),
        "curate review data schema",
    )?;
    ensure_equal(
        &json["data"]["command"],
        &serde_json::json!(command),
        "curate review command",
    )?;
    ensure_equal(
        &json["data"]["review"]["action"],
        &serde_json::json!(action),
        "curate review action",
    )?;
    ensure_equal(
        &json["data"]["mutation"]["toStatus"],
        &serde_json::json!(to_status),
        "curate review to status",
    )?;
    ensure_equal(
        &json["data"]["mutation"]["toReviewState"],
        &serde_json::json!(to_review_state),
        "curate review to review state",
    )?;
    ensure_equal(
        &json["data"]["mutation"]["persisted"],
        &serde_json::json!(true),
        "curate review persisted",
    )?;
    ensure_equal(
        &json["data"]["durableMutation"],
        &serde_json::json!(true),
        "curate review durable mutation",
    )?;
    ensure(
        json["data"]["mutation"]["auditId"].as_str().is_some(),
        "curate review audit id should be present",
    )
}

#[cfg(unix)]
fn ensure_pack_query_file_machine_error(
    workspace_prefix: &str,
    query_file_arg: &str,
    expected_code: &str,
    context: &str,
) -> TestResult {
    let workspace = unique_artifact_dir(workspace_prefix)?;
    fs::create_dir_all(&workspace).map_err(|error| error.to_string())?;
    let workspace_arg = workspace.to_string_lossy().into_owned();

    let output = run_ee(&[
        "--workspace",
        workspace_arg.as_str(),
        "--json",
        "pack",
        "--query-file",
        query_file_arg,
    ])?;
    let stdout = String::from_utf8_lossy(&output.stdout);

    ensure(!output.status.success(), format!("{context} should fail"))?;
    ensure(output.stderr.is_empty(), format!("{context} stderr clean"))?;
    ensure_no_ansi(&stdout, context)?;
    let json: serde_json::Value = serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("{context} stdout must be JSON: {error}"))?;
    ensure_equal(&json["schema"], &serde_json::json!("ee.error.v1"), context)?;
    ensure_equal(
        &json["error"]["code"],
        &serde_json::json!(expected_code),
        context,
    )?;
    ensure(
        !workspace.join(".ee").exists(),
        format!("{context} should fail before storage mutation"),
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

#[cfg(unix)]
#[test]
fn status_workspace_reports_ambiguity_diagnostics() -> TestResult {
    let root = unique_artifact_dir("workspace-ambiguity")?;
    let outer = root.join("outer");
    let inner = outer.join("inner");
    let leaf = inner.join("src").join("leaf");
    fs::create_dir_all(outer.join(".ee")).map_err(|error| error.to_string())?;
    fs::create_dir_all(inner.join(".ee")).map_err(|error| error.to_string())?;
    fs::create_dir_all(&leaf).map_err(|error| error.to_string())?;

    let outer_arg = outer.to_string_lossy().into_owned();
    let output = run_ee_in_dir(
        &["--workspace", outer_arg.as_str(), "--json", "status"],
        &leaf,
    )?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    ensure(
        output.status.success(),
        format!("status --workspace --json should succeed; stderr: {stderr}"),
    )?;
    ensure(
        stderr.is_empty(),
        "status ambiguity diagnostics must not write stderr for JSON output".to_string(),
    )?;

    let json: serde_json::Value = serde_json::from_str(&stdout)
        .map_err(|error| format!("status ambiguity stdout must be JSON: {error}"))?;
    ensure_equal(
        &json["schema"],
        &serde_json::json!("ee.response.v1"),
        "status schema",
    )?;
    ensure_equal(
        &json["data"]["workspace"]["source"],
        &serde_json::json!("explicit"),
        "status workspace source",
    )?;
    ensure_equal(
        &json["data"]["workspace"]["root"],
        &serde_json::json!(outer_arg),
        "status selected workspace root",
    )?;

    let diagnostics = json["data"]["workspace"]["diagnostics"]
        .as_array()
        .ok_or_else(|| "workspace diagnostics must be an array".to_string())?;
    let codes = diagnostics
        .iter()
        .filter_map(|diagnostic| diagnostic["code"].as_str())
        .collect::<Vec<_>>();
    ensure(
        codes.contains(&"workspace_selected_differs_from_discovered"),
        "status reports explicit/discovered ambiguity".to_string(),
    )?;
    ensure(
        codes.contains(&"workspace_nested_markers"),
        "status reports nested workspace markers".to_string(),
    )?;
    Ok(())
}

#[cfg(unix)]
#[test]
fn workspace_alias_commands_resolve_global_workspace_flag() -> TestResult {
    let root = unique_artifact_dir("workspace-alias")?;
    let workspace = root.join("repo");
    fs::create_dir_all(&workspace).map_err(|error| error.to_string())?;
    let registry = root.join("registry").join("workspaces.db");
    let registry_env = registry.as_os_str().to_os_string();
    let envs = [("EE_WORKSPACE_REGISTRY", registry_env)];
    let workspace_arg = workspace.to_string_lossy().into_owned();

    let init = run_ee_with_env(
        &["--workspace", workspace_arg.as_str(), "--json", "init"],
        &envs,
    )?;
    ensure(
        init.status.success(),
        format!(
            "init for workspace alias test should succeed: {}",
            String::from_utf8_lossy(&init.stderr)
        ),
    )?;

    let alias = run_ee_with_env(
        &[
            "--workspace",
            workspace_arg.as_str(),
            "--json",
            "workspace",
            "alias",
            "--as",
            "client-api",
        ],
        &envs,
    )?;
    let alias_stdout = String::from_utf8_lossy(&alias.stdout);
    let alias_stderr = String::from_utf8_lossy(&alias.stderr);
    ensure(
        alias.status.success(),
        format!("workspace alias should succeed; stderr: {alias_stderr}"),
    )?;
    ensure(
        alias_stderr.is_empty(),
        "workspace alias JSON mode keeps stderr clean".to_string(),
    )?;
    let alias_json: serde_json::Value = serde_json::from_str(&alias_stdout)
        .map_err(|error| format!("workspace alias stdout must be JSON: {error}"))?;
    ensure_equal(
        &alias_json["data"]["alias"],
        &serde_json::json!("client-api"),
        "alias name",
    )?;
    ensure_equal(
        &alias_json["data"]["persisted"],
        &serde_json::json!(true),
        "alias persisted",
    )?;

    let list = run_ee_with_env(&["--json", "workspace", "list"], &envs)?;
    ensure(
        list.status.success(),
        format!(
            "workspace list should succeed: {}",
            String::from_utf8_lossy(&list.stderr)
        ),
    )?;
    let list_json: serde_json::Value = serde_json::from_slice(&list.stdout)
        .map_err(|error| format!("workspace list stdout must be JSON: {error}"))?;
    ensure_equal(
        &list_json["data"]["workspaces"][0]["alias"],
        &serde_json::json!("client-api"),
        "listed alias",
    )?;

    let status = run_ee_with_env(&["--workspace", "client-api", "--json", "status"], &envs)?;
    ensure(
        status.status.success(),
        format!(
            "status should resolve workspace alias: {}",
            String::from_utf8_lossy(&status.stderr)
        ),
    )?;
    let status_json: serde_json::Value = serde_json::from_slice(&status.stdout)
        .map_err(|error| format!("status stdout must be JSON: {error}"))?;
    let canonical_workspace = workspace
        .canonicalize()
        .map_err(|error| format!("workspace canonicalization failed: {error}"))?
        .to_string_lossy()
        .into_owned();
    ensure_equal(
        &status_json["data"]["workspace"]["root"],
        &serde_json::json!(canonical_workspace),
        "status root resolved from alias",
    )
}

#[cfg(unix)]
#[test]
fn workspace_resolve_reports_monorepo_subproject_scope() -> TestResult {
    let root = unique_artifact_dir("workspace-subproject")?;
    let repo = root.join("monorepo");
    let subproject = repo.join("crates").join("api");
    fs::create_dir_all(repo.join(".git")).map_err(|error| error.to_string())?;
    fs::create_dir_all(&subproject).map_err(|error| error.to_string())?;
    let registry = root.join("registry").join("workspaces.db");
    let registry_env = registry.as_os_str().to_os_string();
    let envs = [("EE_WORKSPACE_REGISTRY", registry_env)];
    let subproject_arg = subproject.to_string_lossy().into_owned();

    let init = run_ee_with_env(
        &["--workspace", subproject_arg.as_str(), "--json", "init"],
        &envs,
    )?;
    ensure(
        init.status.success(),
        format!(
            "init for monorepo subproject should succeed: {}",
            String::from_utf8_lossy(&init.stderr)
        ),
    )?;

    let resolve = run_ee_with_env(
        &[
            "--workspace",
            subproject_arg.as_str(),
            "--json",
            "workspace",
            "resolve",
        ],
        &envs,
    )?;
    ensure(
        resolve.status.success(),
        format!(
            "workspace resolve should succeed: {}",
            String::from_utf8_lossy(&resolve.stderr)
        ),
    )?;
    let resolve_json: serde_json::Value = serde_json::from_slice(&resolve.stdout)
        .map_err(|error| format!("workspace resolve stdout must be JSON: {error}"))?;
    let canonical_repo = repo
        .canonicalize()
        .map_err(|error| format!("repo canonicalization failed: {error}"))?
        .to_string_lossy()
        .into_owned();
    ensure_equal(
        &resolve_json["data"]["scopeKind"],
        &serde_json::json!("subproject"),
        "resolve scope kind",
    )?;
    ensure_equal(
        &resolve_json["data"]["repositoryRoot"],
        &serde_json::json!(canonical_repo),
        "resolve repository root",
    )?;
    ensure_equal(
        &resolve_json["data"]["subprojectPath"],
        &serde_json::json!("crates/api"),
        "resolve subproject path",
    )?;
    let repository_fingerprint = resolve_json["data"]["repositoryFingerprint"]
        .as_str()
        .ok_or_else(|| "resolve missing repository fingerprint".to_string())?;
    ensure(
        repository_fingerprint.starts_with("repo:"),
        "repository fingerprint has repo: prefix".to_string(),
    )?;

    let alias = run_ee_with_env(
        &[
            "--workspace",
            subproject_arg.as_str(),
            "--json",
            "workspace",
            "alias",
            "--as",
            "mono-api",
        ],
        &envs,
    )?;
    ensure(
        alias.status.success(),
        format!(
            "workspace alias should persist subproject row: {}",
            String::from_utf8_lossy(&alias.stderr)
        ),
    )?;
    let alias_json: serde_json::Value = serde_json::from_slice(&alias.stdout)
        .map_err(|error| format!("workspace alias stdout must be JSON: {error}"))?;
    ensure_equal(
        &alias_json["data"]["scopeKind"],
        &serde_json::json!("subproject"),
        "alias scope kind",
    )?;
    ensure_equal(
        &alias_json["data"]["repositoryFingerprint"],
        &serde_json::json!(repository_fingerprint),
        "alias repository fingerprint",
    )?;

    let list = run_ee_with_env(&["--json", "workspace", "list"], &envs)?;
    ensure(
        list.status.success(),
        format!(
            "workspace list should include subproject scope: {}",
            String::from_utf8_lossy(&list.stderr)
        ),
    )?;
    let list_json: serde_json::Value = serde_json::from_slice(&list.stdout)
        .map_err(|error| format!("workspace list stdout must be JSON: {error}"))?;
    ensure_equal(
        &list_json["data"]["workspaces"][0]["scopeKind"],
        &serde_json::json!("subproject"),
        "listed scope kind",
    )?;
    ensure_equal(
        &list_json["data"]["workspaces"][0]["subprojectPath"],
        &serde_json::json!("crates/api"),
        "listed subproject path",
    )?;

    let status = run_ee_with_env(&["--workspace", "mono-api", "--json", "status"], &envs)?;
    ensure(
        status.status.success(),
        format!(
            "status should resolve alias with subproject scope: {}",
            String::from_utf8_lossy(&status.stderr)
        ),
    )?;
    let status_json: serde_json::Value = serde_json::from_slice(&status.stdout)
        .map_err(|error| format!("status stdout must be JSON: {error}"))?;
    ensure_equal(
        &status_json["data"]["workspace"]["scopeKind"],
        &serde_json::json!("subproject"),
        "status scope kind",
    )?;
    ensure_equal(
        &status_json["data"]["workspace"]["repositoryFingerprint"],
        &serde_json::json!(repository_fingerprint),
        "status repository fingerprint",
    )
}

#[cfg(unix)]
#[test]
fn workspace_continuity_scenario_keeps_context_scoped() -> TestResult {
    let scenario = "usr_workspace_continuity";
    let root = unique_artifact_dir("workspace-continuity")?;
    let summary_dir = unique_e2e_dossier_dir(scenario)?;
    let client_workspace = root.join("client-api");
    let billing_workspace = root.join("billing-worker");
    let fake_bin_dir = root.join("bin");
    fs::create_dir_all(&client_workspace).map_err(|error| error.to_string())?;
    fs::create_dir_all(&billing_workspace).map_err(|error| error.to_string())?;
    fs::create_dir_all(&fake_bin_dir).map_err(|error| error.to_string())?;
    fs::create_dir_all(&summary_dir).map_err(|error| error.to_string())?;

    let cass_binary = fake_bin_dir.join("cass");
    write_fake_cass_binary(&cass_binary)?;

    let registry = root.join("registry").join("workspaces.db");
    let registry_env = registry.as_os_str().to_os_string();
    let client_arg = client_workspace.to_string_lossy().into_owned();
    let billing_arg = billing_workspace.to_string_lossy().into_owned();
    let client_db = client_workspace.join(".ee").join("ee.db");
    let billing_db = billing_workspace.join(".ee").join("ee.db");
    let client_db_arg = client_db.to_string_lossy().into_owned();
    let cass_session_path = client_workspace.join("session-client-api.jsonl");
    let cass_session_arg = cass_session_path.to_string_lossy().into_owned();
    let path = path_with_fake_cass(&fake_bin_dir)?;
    let envs = [
        ("EE_WORKSPACE_REGISTRY", registry_env),
        ("PATH", path),
        (
            "EE_FAKE_CASS_SESSION",
            OsString::from(cass_session_arg.clone()),
        ),
        ("EE_FAKE_CASS_WORKSPACE", OsString::from(client_arg.clone())),
    ];

    let mut command_dossiers = Vec::new();

    let init_client = run_ee_logged_with_env(
        scenario,
        &["--workspace", client_arg.as_str(), "--json", "init"],
        &client_workspace,
        None,
        "fx.fresh_workspace.v1",
        "ee.response.v1",
        None,
        &envs,
    )?;
    command_dossiers.push(init_client.dossier_dir.clone());
    parse_logged_response(&init_client, "client init")?;

    let init_billing = run_ee_logged_with_env(
        scenario,
        &["--workspace", billing_arg.as_str(), "--json", "init"],
        &billing_workspace,
        None,
        "fx.fresh_workspace.v1",
        "ee.response.v1",
        None,
        &envs,
    )?;
    command_dossiers.push(init_billing.dossier_dir.clone());
    parse_logged_response(&init_billing, "billing init")?;

    let alias_client = run_ee_logged_with_env(
        scenario,
        &[
            "--workspace",
            client_arg.as_str(),
            "--json",
            "workspace",
            "alias",
            "--as",
            "client-api",
        ],
        &client_workspace,
        None,
        "fx.multi_workspace.v1",
        "ee.response.v1",
        None,
        &envs,
    )?;
    command_dossiers.push(alias_client.dossier_dir.clone());
    let alias_client_json = parse_logged_response(&alias_client, "client alias")?;
    ensure_equal(
        &alias_client_json["data"]["alias"],
        &serde_json::json!("client-api"),
        "client alias name",
    )?;

    let alias_billing = run_ee_logged_with_env(
        scenario,
        &[
            "--workspace",
            billing_arg.as_str(),
            "--json",
            "workspace",
            "alias",
            "--as",
            "billing-worker",
        ],
        &billing_workspace,
        None,
        "fx.multi_workspace.v1",
        "ee.response.v1",
        None,
        &envs,
    )?;
    command_dossiers.push(alias_billing.dossier_dir.clone());
    let alias_billing_json = parse_logged_response(&alias_billing, "billing alias")?;
    ensure_equal(
        &alias_billing_json["data"]["alias"],
        &serde_json::json!("billing-worker"),
        "billing alias name",
    )?;

    let status_client = run_ee_logged_with_env(
        scenario,
        &["--workspace", "client-api", "--json", "status"],
        &client_workspace,
        None,
        "fx.multi_workspace.v1",
        "ee.response.v1",
        None,
        &envs,
    )?;
    command_dossiers.push(status_client.dossier_dir.clone());
    let status_client_json = parse_logged_response(&status_client, "client status")?;
    let client_fingerprint = status_client_json["data"]["workspace"]["fingerprint"]
        .as_str()
        .ok_or_else(|| "client status missing workspace fingerprint".to_string())?
        .to_string();

    let status_billing = run_ee_logged_with_env(
        scenario,
        &["--workspace", "billing-worker", "--json", "status"],
        &billing_workspace,
        None,
        "fx.multi_workspace.v1",
        "ee.response.v1",
        None,
        &envs,
    )?;
    command_dossiers.push(status_billing.dossier_dir.clone());
    let status_billing_json = parse_logged_response(&status_billing, "billing status")?;
    let billing_fingerprint = status_billing_json["data"]["workspace"]["fingerprint"]
        .as_str()
        .ok_or_else(|| "billing status missing workspace fingerprint".to_string())?
        .to_string();
    ensure(
        client_fingerprint != billing_fingerprint,
        "workspace fingerprints must differ".to_string(),
    )?;

    let remember_client = run_ee_logged_with_env(
        scenario,
        &[
            "--workspace",
            "client-api",
            "--json",
            "remember",
            "--level",
            "procedural",
            "--kind",
            "rule",
            "--tags",
            "shared,client-api",
            "--confidence",
            "0.93",
            "--source",
            "file://client-api/AGENTS.md#L1",
            "Shared cache migration policy: client-api must run cargo fmt --check before adapter changes.",
        ],
        &client_workspace,
        None,
        "fx.manual_memory.v1",
        "ee.response.v1",
        None,
        &envs,
    )?;
    command_dossiers.push(remember_client.dossier_dir.clone());
    let remember_client_json = parse_logged_response(&remember_client, "client remember")?;
    let client_memory_id = remember_client_json["data"]["memory_id"]
        .as_str()
        .ok_or_else(|| "client remember missing memory_id".to_string())?
        .to_string();
    let client_workspace_id = remember_client_json["data"]["workspace_id"]
        .as_str()
        .ok_or_else(|| "client remember missing workspace_id".to_string())?
        .to_string();

    let remember_billing = run_ee_logged_with_env(
        scenario,
        &[
            "--workspace",
            "billing-worker",
            "--json",
            "remember",
            "--level",
            "procedural",
            "--kind",
            "rule",
            "--tags",
            "shared,billing-worker",
            "--confidence",
            "0.91",
            "--source",
            "file://billing-worker/AGENTS.md#L1",
            "Shared cache migration policy: billing-worker must run cargo test before job queue changes.",
        ],
        &billing_workspace,
        None,
        "fx.manual_memory.v1",
        "ee.response.v1",
        None,
        &envs,
    )?;
    command_dossiers.push(remember_billing.dossier_dir.clone());
    let remember_billing_json = parse_logged_response(&remember_billing, "billing remember")?;
    let billing_memory_id = remember_billing_json["data"]["memory_id"]
        .as_str()
        .ok_or_else(|| "billing remember missing memory_id".to_string())?
        .to_string();
    let billing_workspace_id = remember_billing_json["data"]["workspace_id"]
        .as_str()
        .ok_or_else(|| "billing remember missing workspace_id".to_string())?
        .to_string();
    ensure(
        client_workspace_id != billing_workspace_id,
        "workspace IDs must differ".to_string(),
    )?;

    let import_client = run_ee_logged_with_env(
        scenario,
        &[
            "--workspace",
            "client-api",
            "--json",
            "import",
            "cass",
            "--database",
            client_db_arg.as_str(),
            "--limit",
            "1",
        ],
        &client_workspace,
        None,
        "fx.cass_v1.v1",
        "ee.response.v1",
        None,
        &envs,
    )?;
    command_dossiers.push(import_client.dossier_dir.clone());
    let import_client_json = parse_logged_response(&import_client, "client CASS import")?;
    ensure_equal(
        &import_client_json["data"]["sessionsImported"],
        &serde_json::json!(1),
        "client CASS sessions imported",
    )?;
    ensure_equal(
        &import_client_json["data"]["spansImported"],
        &serde_json::json!(1),
        "client CASS spans imported",
    )?;

    let rebuild_client = run_ee_logged_with_env(
        scenario,
        &["--workspace", "client-api", "--json", "index", "rebuild"],
        &client_workspace,
        None,
        "fx.manual_memory.v1",
        "ee.response.v1",
        None,
        &envs,
    )?;
    command_dossiers.push(rebuild_client.dossier_dir.clone());
    let rebuild_client_json = parse_logged_response(&rebuild_client, "client index rebuild")?;
    ensure_equal(
        &rebuild_client_json["data"]["memories_indexed"],
        &serde_json::json!(1),
        "client indexed memory count",
    )?;

    let rebuild_billing = run_ee_logged_with_env(
        scenario,
        &[
            "--workspace",
            "billing-worker",
            "--json",
            "index",
            "rebuild",
        ],
        &billing_workspace,
        None,
        "fx.manual_memory.v1",
        "ee.response.v1",
        None,
        &envs,
    )?;
    command_dossiers.push(rebuild_billing.dossier_dir.clone());
    let rebuild_billing_json = parse_logged_response(&rebuild_billing, "billing index rebuild")?;
    ensure_equal(
        &rebuild_billing_json["data"]["memories_indexed"],
        &serde_json::json!(1),
        "billing indexed memory count",
    )?;

    let context_client = run_ee_logged_with_env(
        scenario,
        &[
            "--workspace",
            "client-api",
            "--json",
            "context",
            "shared cache migration policy",
            "--profile",
            "compact",
            "--max-tokens",
            "1200",
        ],
        &client_workspace,
        None,
        "fx.multi_workspace.v1",
        "ee.response.v1",
        None,
        &envs,
    )?;
    command_dossiers.push(context_client.dossier_dir.clone());
    let context_client_json = parse_logged_response(&context_client, "client context")?;
    ensure_equal(
        &context_client_json["data"]["request"]["profile"],
        &serde_json::json!("compact"),
        "client context profile",
    )?;
    let client_items = context_client_json["data"]["pack"]["items"]
        .as_array()
        .ok_or_else(|| "client context missing items array".to_string())?;
    ensure(
        client_items
            .iter()
            .any(|item| item["memoryId"] == client_memory_id),
        "client context must select client memory".to_string(),
    )?;
    ensure(
        !client_items
            .iter()
            .any(|item| item["memoryId"] == billing_memory_id),
        "client context must not select billing memory".to_string(),
    )?;
    ensure(
        client_items
            .iter()
            .all(|item| item["provenance"].as_array().is_some_and(|p| !p.is_empty())),
        "client context items must expose provenance".to_string(),
    )?;
    ensure(
        context_client_json["data"]["pack"]["provenanceFooter"]["schemes"]
            .as_array()
            .is_some_and(|schemes| schemes.iter().any(|scheme| scheme == "file")),
        "client context provenance footer includes file scope".to_string(),
    )?;

    let context_billing = run_ee_logged_with_env(
        scenario,
        &[
            "--workspace",
            "billing-worker",
            "--json",
            "context",
            "shared cache migration policy",
            "--profile",
            "thorough",
            "--max-tokens",
            "1200",
        ],
        &billing_workspace,
        None,
        "fx.multi_workspace.v1",
        "ee.response.v1",
        None,
        &envs,
    )?;
    command_dossiers.push(context_billing.dossier_dir.clone());
    let context_billing_json = parse_logged_response(&context_billing, "billing context")?;
    ensure_equal(
        &context_billing_json["data"]["request"]["profile"],
        &serde_json::json!("thorough"),
        "billing context profile",
    )?;
    let billing_items = context_billing_json["data"]["pack"]["items"]
        .as_array()
        .ok_or_else(|| "billing context missing items array".to_string())?;
    ensure(
        billing_items
            .iter()
            .any(|item| item["memoryId"] == billing_memory_id),
        "billing context must select billing memory".to_string(),
    )?;
    ensure(
        !billing_items
            .iter()
            .any(|item| item["memoryId"] == client_memory_id),
        "billing context must not select client memory".to_string(),
    )?;
    ensure(
        billing_items
            .iter()
            .all(|item| item["provenance"].as_array().is_some_and(|p| !p.is_empty())),
        "billing context items must expose provenance".to_string(),
    )?;

    let list = run_ee_logged_with_env(
        scenario,
        &["--json", "workspace", "list"],
        &root,
        None,
        "fx.multi_workspace.v1",
        "ee.response.v1",
        None,
        &envs,
    )?;
    command_dossiers.push(list.dossier_dir.clone());
    let list_json = parse_logged_response(&list, "workspace list")?;
    let aliases = list_json["data"]["workspaces"]
        .as_array()
        .ok_or_else(|| "workspace list missing workspaces array".to_string())?
        .iter()
        .filter_map(|workspace| workspace["alias"].as_str())
        .collect::<Vec<_>>();
    ensure(
        aliases.contains(&"client-api") && aliases.contains(&"billing-worker"),
        "workspace list must include both aliases".to_string(),
    )?;

    let ambiguous_outer = root.join("ambiguous").join("outer");
    let ambiguous_inner = ambiguous_outer.join("inner");
    let ambiguous_leaf = ambiguous_inner.join("src").join("leaf");
    fs::create_dir_all(ambiguous_outer.join(".ee")).map_err(|error| error.to_string())?;
    fs::create_dir_all(ambiguous_inner.join(".ee")).map_err(|error| error.to_string())?;
    fs::create_dir_all(&ambiguous_leaf).map_err(|error| error.to_string())?;
    let ambiguous_outer_arg = ambiguous_outer.to_string_lossy().into_owned();
    let ambiguity = run_ee_logged_with_env_in_dir(
        scenario,
        &[
            "--workspace",
            ambiguous_outer_arg.as_str(),
            "--json",
            "status",
        ],
        &ambiguous_outer,
        None,
        "fx.multi_workspace.v1",
        "ee.response.v1",
        None,
        &envs,
        Some(&ambiguous_leaf),
    )?;
    command_dossiers.push(ambiguity.dossier_dir.clone());
    let ambiguity_json = parse_logged_response(&ambiguity, "workspace ambiguity status")?;
    let ambiguity_codes = ambiguity_json["data"]["workspace"]["diagnostics"]
        .as_array()
        .ok_or_else(|| "ambiguity status missing diagnostics".to_string())?
        .iter()
        .filter_map(|diagnostic| diagnostic["code"].as_str().map(str::to_owned))
        .collect::<Vec<_>>();
    ensure(
        ambiguity_codes
            .iter()
            .any(|code| code == "workspace_selected_differs_from_discovered"),
        "ambiguity status must report explicit/discovered conflict".to_string(),
    )?;
    ensure(
        ambiguity_codes
            .iter()
            .any(|code| code == "workspace_nested_markers"),
        "ambiguity status must report nested markers".to_string(),
    )?;

    let client_connection =
        DbConnection::open(DatabaseConfig::file(client_db.clone())).map_err(|e| e.to_string())?;
    let client_sessions = client_connection
        .list_sessions(&client_workspace_id)
        .map_err(|e| e.to_string())?;
    ensure_equal(&client_sessions.len(), &1, "client CASS session count")?;
    let client_session_id = client_sessions[0].cass_session_id.clone();
    client_connection.close().map_err(|e| e.to_string())?;

    let billing_connection =
        DbConnection::open(DatabaseConfig::file(billing_db.clone())).map_err(|e| e.to_string())?;
    let billing_sessions = billing_connection
        .list_sessions(&billing_workspace_id)
        .map_err(|e| e.to_string())?;
    ensure(
        billing_sessions.is_empty(),
        "billing workspace must not inherit client CASS sessions".to_string(),
    )?;
    billing_connection.close().map_err(|e| e.to_string())?;

    let dossier_paths = command_dossiers
        .iter()
        .map(|path| path.display().to_string())
        .collect::<Vec<_>>();
    write_json_artifact(
        &summary_dir.join("scenario-summary.json"),
        &serde_json::json!({
            "schema": "ee.e2e.scenario_summary.v1",
            "scenarioId": scenario,
            "beadId": "eidetic_engine_cli-jqhn",
            "fixtureIds": [
                "fx.multi_workspace.v1",
                "fx.fresh_workspace.v1",
                "fx.manual_memory.v1",
                "fx.cass_v1.v1"
            ],
            "commandDossiers": dossier_paths,
            "workspaces": [
                {
                    "alias": "client-api",
                    "workspaceId": client_workspace_id,
                    "path": client_workspace.display().to_string(),
                    "repositoryFingerprint": client_fingerprint
                },
                {
                    "alias": "billing-worker",
                    "workspaceId": billing_workspace_id,
                    "path": billing_workspace.display().to_string(),
                    "repositoryFingerprint": billing_fingerprint
                }
            ],
            "cass": {
                "importedWorkspaceAlias": "client-api",
                "sessionPath": cass_session_arg,
                "sessionId": client_session_id,
                "sessionsImported": 1
            },
            "selectedMemoryIds": {
                "client-api": [client_memory_id],
                "billing-worker": [billing_memory_id]
            },
            "rejectedMemoryIds": {
                "client-api": [billing_memory_id],
                "billing-worker": [client_memory_id]
            },
            "scopeReasons": [
                "client-api context used the client-api alias and selected only client-api memory.",
                "billing-worker context used the billing-worker alias and selected only billing-worker memory.",
                "CASS session import was recorded only in the client-api workspace.",
                "Ambiguous nested workspaces emitted diagnostics instead of silently picking an unsafe scope."
            ],
            "ambiguityDiagnosticCodes": ambiguity_codes,
            "redactionStatus": "checked"
        }),
    )?;

    Ok(())
}

#[cfg(unix)]
#[test]
fn curate_candidates_json_lists_empty_pending_queue() -> TestResult {
    let workspace = unique_artifact_dir("curate-candidates-empty")?;
    fs::create_dir_all(&workspace).map_err(|error| error.to_string())?;
    let workspace_arg = workspace.to_string_lossy().into_owned();
    let init = run_ee(&["--workspace", workspace_arg.as_str(), "--json", "init"])?;
    ensure(
        init.status.success(),
        format!(
            "init should succeed before curate candidates; stderr: {}",
            String::from_utf8_lossy(&init.stderr)
        ),
    )?;
    let remember = run_ee(&[
        "--workspace",
        workspace_arg.as_str(),
        "--json",
        "remember",
        "--level",
        "procedural",
        "--kind",
        "rule",
        "Run cargo fmt --check before release.",
    ])?;
    ensure(
        remember.status.success(),
        format!(
            "remember should create the test database; stderr: {}",
            String::from_utf8_lossy(&remember.stderr)
        ),
    )?;

    let run = run_ee_logged(
        "curate-candidates-empty",
        &[
            "--workspace",
            workspace_arg.as_str(),
            "--json",
            "curate",
            "candidates",
        ],
        &workspace,
        None,
        "fx.curate_candidates.empty.v1",
        "ee.response.v1",
        None,
    )?;
    let stdout = String::from_utf8_lossy(&run.output.stdout);
    let stderr = String::from_utf8_lossy(&run.output.stderr);
    ensure(
        run.output.status.success(),
        format!("curate candidates should succeed; stderr: {stderr}"),
    )?;
    ensure(stderr.is_empty(), "curate candidates JSON stderr clean")?;
    ensure_no_ansi(&stdout, "curate candidates JSON stdout")?;
    let json: serde_json::Value = serde_json::from_slice(&run.output.stdout)
        .map_err(|error| format!("curate candidates stdout must be JSON: {error}"))?;
    ensure_equal(
        &json["schema"],
        &serde_json::json!("ee.response.v1"),
        "outer schema",
    )?;
    ensure_equal(&json["success"], &serde_json::json!(true), "success")?;
    ensure_equal(
        &json["data"]["schema"],
        &serde_json::json!("ee.curate.candidates.v1"),
        "data schema",
    )?;
    ensure_equal(
        &json["data"]["command"],
        &serde_json::json!("curate candidates"),
        "command",
    )?;
    ensure_equal(
        &json["data"]["filter"]["status"],
        &serde_json::json!("pending"),
        "default status filter",
    )?;
    ensure_equal(
        &json["data"]["totalCount"],
        &serde_json::json!(0),
        "total count",
    )?;
    ensure_equal(
        &json["data"]["returnedCount"],
        &serde_json::json!(0),
        "returned count",
    )?;
    ensure_equal(
        &json["data"]["durableMutation"],
        &serde_json::json!(false),
        "read-only command",
    )?;
    ensure(
        json["data"]["candidates"]
            .as_array()
            .is_some_and(Vec::is_empty),
        "candidates array should be empty",
    )?;
    ensure(
        run.dossier_dir.join("stdout.schema.json").is_file(),
        "curate candidates dossier should log schema status",
    )
}

#[cfg(unix)]
#[test]
fn curate_validate_json_approves_pending_candidate() -> TestResult {
    let workspace = unique_artifact_dir("curate-validate-approve")?;
    fs::create_dir_all(&workspace).map_err(|error| error.to_string())?;
    let workspace_arg = workspace.to_string_lossy().into_owned();
    let init = run_ee(&["--workspace", workspace_arg.as_str(), "--json", "init"])?;
    ensure(
        init.status.success(),
        format!(
            "init should succeed before curate validate; stderr: {}",
            String::from_utf8_lossy(&init.stderr)
        ),
    )?;
    let remember = run_ee(&[
        "--workspace",
        workspace_arg.as_str(),
        "--json",
        "remember",
        "--level",
        "procedural",
        "--kind",
        "rule",
        "Run cargo fmt --check before release.",
    ])?;
    ensure(
        remember.status.success(),
        format!(
            "remember should create the test database; stderr: {}",
            String::from_utf8_lossy(&remember.stderr)
        ),
    )?;
    let remember_json: serde_json::Value = serde_json::from_slice(&remember.stdout)
        .map_err(|error| format!("remember stdout must be JSON: {error}"))?;
    let workspace_id = remember_json["data"]["workspace_id"]
        .as_str()
        .ok_or_else(|| "remember workspace_id must be a string".to_string())?
        .to_owned();
    let memory_id = remember_json["data"]["memory_id"]
        .as_str()
        .ok_or_else(|| "remember memory_id must be a string".to_string())?
        .to_owned();
    let candidate_id = "curate_00000000000000000000000001";
    let database_path = workspace.join(".ee").join("ee.db");
    let connection = DbConnection::open_file(&database_path).map_err(|error| error.to_string())?;
    connection
        .insert_curation_candidate(
            candidate_id,
            &CreateCurationCandidateInput {
                workspace_id,
                candidate_type: "promote".to_string(),
                target_memory_id: memory_id,
                proposed_content: None,
                proposed_confidence: Some(0.82),
                proposed_trust_class: Some("agent_validated".to_string()),
                source_type: "feedback_event".to_string(),
                source_id: Some("smoke-outcome".to_string()),
                reason: "Validated through smoke coverage.".to_string(),
                confidence: 0.76,
                status: Some("pending".to_string()),
                created_at: Some("2026-05-01T00:00:02Z".to_string()),
                ttl_expires_at: None,
            },
        )
        .map_err(|error| error.to_string())?;

    let run = run_ee_logged(
        "curate-validate-approve",
        &[
            "--workspace",
            workspace_arg.as_str(),
            "--json",
            "curate",
            "validate",
            candidate_id,
            "--actor",
            "smoke-test",
        ],
        &workspace,
        None,
        "fx.curate_validate.approve.v1",
        "ee.response.v1",
        None,
    )?;
    let stdout = String::from_utf8_lossy(&run.output.stdout);
    let stderr = String::from_utf8_lossy(&run.output.stderr);
    ensure(
        run.output.status.success(),
        format!("curate validate should succeed; stderr: {stderr}"),
    )?;
    ensure(stderr.is_empty(), "curate validate JSON stderr clean")?;
    ensure_no_ansi(&stdout, "curate validate JSON stdout")?;
    let json: serde_json::Value = serde_json::from_slice(&run.output.stdout)
        .map_err(|error| format!("curate validate stdout must be JSON: {error}"))?;
    ensure_equal(
        &json["schema"],
        &serde_json::json!("ee.response.v1"),
        "outer schema",
    )?;
    ensure_equal(
        &json["data"]["schema"],
        &serde_json::json!("ee.curate.validate.v1"),
        "data schema",
    )?;
    ensure_equal(
        &json["data"]["command"],
        &serde_json::json!("curate validate"),
        "command",
    )?;
    ensure_equal(
        &json["data"]["validation"]["status"],
        &serde_json::json!("passed"),
        "validation status",
    )?;
    ensure_equal(
        &json["data"]["validation"]["decision"],
        &serde_json::json!("approved"),
        "validation decision",
    )?;
    ensure_equal(
        &json["data"]["mutation"]["fromStatus"],
        &serde_json::json!("pending"),
        "from status",
    )?;
    ensure_equal(
        &json["data"]["mutation"]["toStatus"],
        &serde_json::json!("approved"),
        "to status",
    )?;
    ensure_equal(
        &json["data"]["mutation"]["persisted"],
        &serde_json::json!(true),
        "persisted",
    )?;
    ensure_equal(
        &json["data"]["dryRun"],
        &serde_json::json!(false),
        "dry run flag",
    )?;
    ensure_equal(
        &json["data"]["durableMutation"],
        &serde_json::json!(true),
        "durable mutation flag",
    )?;
    ensure(
        json["data"]["mutation"]["auditId"].as_str().is_some(),
        "audit id should be present",
    )?;
    ensure(
        run.dossier_dir.join("stdout.schema.json").is_file(),
        "curate validate dossier should log schema status",
    )
}

#[cfg(unix)]
#[test]
fn curate_apply_json_updates_approved_candidate_target() -> TestResult {
    let workspace = unique_artifact_dir("curate-apply-approved")?;
    fs::create_dir_all(&workspace).map_err(|error| error.to_string())?;
    let workspace_arg = workspace.to_string_lossy().into_owned();
    let init = run_ee(&["--workspace", workspace_arg.as_str(), "--json", "init"])?;
    ensure(
        init.status.success(),
        format!(
            "init should succeed before curate apply; stderr: {}",
            String::from_utf8_lossy(&init.stderr)
        ),
    )?;
    let remember = run_ee(&[
        "--workspace",
        workspace_arg.as_str(),
        "--json",
        "remember",
        "--level",
        "procedural",
        "--kind",
        "rule",
        "Run cargo fmt --check before release.",
    ])?;
    ensure(
        remember.status.success(),
        format!(
            "remember should create the test database; stderr: {}",
            String::from_utf8_lossy(&remember.stderr)
        ),
    )?;
    let remember_json: serde_json::Value = serde_json::from_slice(&remember.stdout)
        .map_err(|error| format!("remember stdout must be JSON: {error}"))?;
    let workspace_id = remember_json["data"]["workspace_id"]
        .as_str()
        .ok_or_else(|| "remember workspace_id must be a string".to_string())?
        .to_owned();
    let memory_id = remember_json["data"]["memory_id"]
        .as_str()
        .ok_or_else(|| "remember memory_id must be a string".to_string())?
        .to_owned();
    let candidate_id = "curate_00000000000000000000000002";
    let database_path = workspace.join(".ee").join("ee.db");
    let connection = DbConnection::open_file(&database_path).map_err(|error| error.to_string())?;
    connection
        .insert_curation_candidate(
            candidate_id,
            &CreateCurationCandidateInput {
                workspace_id: workspace_id.clone(),
                candidate_type: "promote".to_string(),
                target_memory_id: memory_id.clone(),
                proposed_content: None,
                proposed_confidence: Some(0.91),
                proposed_trust_class: Some("agent_validated".to_string()),
                source_type: "feedback_event".to_string(),
                source_id: Some("smoke-outcome".to_string()),
                reason: "Apply through smoke coverage.".to_string(),
                confidence: 0.84,
                status: Some("approved".to_string()),
                created_at: Some("2026-05-01T00:00:02Z".to_string()),
                ttl_expires_at: None,
            },
        )
        .map_err(|error| error.to_string())?;

    let run = run_ee_logged(
        "curate-apply-approved",
        &[
            "--workspace",
            workspace_arg.as_str(),
            "--json",
            "curate",
            "apply",
            candidate_id,
            "--actor",
            "smoke-test",
        ],
        &workspace,
        None,
        "fx.curate_apply.approved.v1",
        "ee.response.v1",
        None,
    )?;
    let stdout = String::from_utf8_lossy(&run.output.stdout);
    let stderr = String::from_utf8_lossy(&run.output.stderr);
    ensure(
        run.output.status.success(),
        format!("curate apply should succeed; stderr: {stderr}"),
    )?;
    ensure(stderr.is_empty(), "curate apply JSON stderr clean")?;
    ensure_no_ansi(&stdout, "curate apply JSON stdout")?;
    let json: serde_json::Value = serde_json::from_slice(&run.output.stdout)
        .map_err(|error| format!("curate apply stdout must be JSON: {error}"))?;
    ensure_equal(
        &json["schema"],
        &serde_json::json!("ee.response.v1"),
        "outer schema",
    )?;
    ensure_equal(
        &json["data"]["schema"],
        &serde_json::json!("ee.curate.apply.v1"),
        "data schema",
    )?;
    ensure_equal(
        &json["data"]["command"],
        &serde_json::json!("curate apply"),
        "command",
    )?;
    ensure_equal(
        &json["data"]["application"]["status"],
        &serde_json::json!("applied"),
        "application status",
    )?;
    ensure_equal(
        &json["data"]["application"]["decision"],
        &serde_json::json!("update_memory"),
        "application decision",
    )?;
    ensure_equal(
        &json["data"]["mutation"]["fromStatus"],
        &serde_json::json!("approved"),
        "from status",
    )?;
    ensure_equal(
        &json["data"]["mutation"]["toStatus"],
        &serde_json::json!("applied"),
        "to status",
    )?;
    ensure_equal(
        &json["data"]["durableMutation"],
        &serde_json::json!(true),
        "durable mutation flag",
    )?;
    ensure(
        json["data"]["mutation"]["auditId"].as_str().is_some(),
        "audit id should be present",
    )?;
    let memory = connection
        .get_memory(&memory_id)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "memory must exist after apply".to_string())?;
    ensure(
        (memory.confidence - 0.91).abs() < 0.001,
        "memory confidence should be updated",
    )?;
    ensure_equal(
        &memory.trust_class,
        &"agent_validated".to_string(),
        "memory trust class",
    )?;
    let candidate = connection
        .get_curation_candidate(&workspace_id, candidate_id)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "candidate must exist after apply".to_string())?;
    ensure_equal(
        &candidate.status,
        &"applied".to_string(),
        "candidate status",
    )?;
    ensure(
        candidate.applied_at.is_some(),
        "candidate applied_at should be persisted",
    )?;
    ensure(
        run.dossier_dir.join("stdout.schema.json").is_file(),
        "curate apply dossier should log schema status",
    )
}

#[cfg(unix)]
#[test]
fn curate_review_lifecycle_commands_json_update_review_state() -> TestResult {
    let workspace = unique_artifact_dir("curate-review-lifecycle")?;
    fs::create_dir_all(&workspace).map_err(|error| error.to_string())?;
    let workspace_arg = workspace.to_string_lossy().into_owned();
    let init = run_ee(&["--workspace", workspace_arg.as_str(), "--json", "init"])?;
    ensure(
        init.status.success(),
        format!(
            "init should succeed before curate lifecycle; stderr: {}",
            String::from_utf8_lossy(&init.stderr)
        ),
    )?;
    let remember = run_ee(&[
        "--workspace",
        workspace_arg.as_str(),
        "--json",
        "remember",
        "--level",
        "procedural",
        "--kind",
        "rule",
        "Run cargo fmt --check before release.",
    ])?;
    ensure(
        remember.status.success(),
        format!(
            "remember should create the test database; stderr: {}",
            String::from_utf8_lossy(&remember.stderr)
        ),
    )?;
    let remember_json: serde_json::Value = serde_json::from_slice(&remember.stdout)
        .map_err(|error| format!("remember stdout must be JSON: {error}"))?;
    let workspace_id = remember_json["data"]["workspace_id"]
        .as_str()
        .ok_or_else(|| "remember workspace_id must be a string".to_string())?
        .to_owned();
    let memory_id = remember_json["data"]["memory_id"]
        .as_str()
        .ok_or_else(|| "remember memory_id must be a string".to_string())?
        .to_owned();
    let database_path = workspace.join(".ee").join("ee.db");
    let connection = DbConnection::open_file(&database_path).map_err(|error| error.to_string())?;
    let accept_id = "curate_00000000000000000000000003";
    let reject_id = "curate_00000000000000000000000004";
    let snooze_id = "curate_00000000000000000000000005";
    let merge_source_id = "curate_00000000000000000000000006";
    let merge_target_id = "curate_00000000000000000000000007";
    for (candidate_id, reason) in [
        (accept_id, "Accept through smoke coverage."),
        (reject_id, "Reject through smoke coverage."),
        (snooze_id, "Snooze through smoke coverage."),
        (merge_source_id, "Merge source through smoke coverage."),
        (merge_target_id, "Merge target through smoke coverage."),
    ] {
        connection
            .insert_curation_candidate(
                candidate_id,
                &CreateCurationCandidateInput {
                    workspace_id: workspace_id.clone(),
                    candidate_type: "promote".to_string(),
                    target_memory_id: memory_id.clone(),
                    proposed_content: None,
                    proposed_confidence: Some(0.82),
                    proposed_trust_class: Some("agent_validated".to_string()),
                    source_type: "feedback_event".to_string(),
                    source_id: Some(format!("smoke-{candidate_id}")),
                    reason: reason.to_string(),
                    confidence: 0.76,
                    status: Some("pending".to_string()),
                    created_at: Some("2026-05-01T00:00:02Z".to_string()),
                    ttl_expires_at: None,
                },
            )
            .map_err(|error| error.to_string())?;
    }

    let accept = run_ee_logged(
        "curate-review-accept",
        &[
            "--workspace",
            workspace_arg.as_str(),
            "--json",
            "curate",
            "accept",
            accept_id,
            "--actor",
            "smoke-test",
        ],
        &workspace,
        None,
        "fx.curate_review.accept.v1",
        "ee.response.v1",
        None,
    )?;
    let accept_json = parse_logged_response(&accept, "curate accept")?;
    assert_curate_review_json(
        &accept_json,
        "curate accept",
        "accept",
        "approved",
        "accepted",
    )?;

    let reject = run_ee_logged(
        "curate-review-reject",
        &[
            "--workspace",
            workspace_arg.as_str(),
            "--json",
            "curate",
            "reject",
            reject_id,
            "--actor",
            "smoke-test",
        ],
        &workspace,
        None,
        "fx.curate_review.reject.v1",
        "ee.response.v1",
        None,
    )?;
    let reject_json = parse_logged_response(&reject, "curate reject")?;
    assert_curate_review_json(
        &reject_json,
        "curate reject",
        "reject",
        "rejected",
        "rejected",
    )?;

    let snooze = run_ee_logged(
        "curate-review-snooze",
        &[
            "--workspace",
            workspace_arg.as_str(),
            "--json",
            "curate",
            "snooze",
            snooze_id,
            "--until",
            "2030-01-01T00:00:00Z",
            "--actor",
            "smoke-test",
        ],
        &workspace,
        None,
        "fx.curate_review.snooze.v1",
        "ee.response.v1",
        None,
    )?;
    let snooze_json = parse_logged_response(&snooze, "curate snooze")?;
    assert_curate_review_json(
        &snooze_json,
        "curate snooze",
        "snooze",
        "pending",
        "snoozed",
    )?;
    ensure_equal(
        &snooze_json["data"]["mutation"]["snoozedUntil"],
        &serde_json::json!("2030-01-01T00:00:00Z"),
        "snooze until",
    )?;

    let merge = run_ee_logged(
        "curate-review-merge",
        &[
            "--workspace",
            workspace_arg.as_str(),
            "--json",
            "curate",
            "merge",
            merge_source_id,
            merge_target_id,
            "--actor",
            "smoke-test",
        ],
        &workspace,
        None,
        "fx.curate_review.merge.v1",
        "ee.response.v1",
        None,
    )?;
    let merge_json = parse_logged_response(&merge, "curate merge")?;
    assert_curate_review_json(&merge_json, "curate merge", "merge", "rejected", "merged")?;
    ensure_equal(
        &merge_json["data"]["mutation"]["mergedIntoCandidateId"],
        &serde_json::json!(merge_target_id),
        "merge target id",
    )?;

    for (candidate_id, status, review_state) in [
        (accept_id, "approved", "accepted"),
        (reject_id, "rejected", "rejected"),
        (snooze_id, "pending", "snoozed"),
        (merge_source_id, "rejected", "merged"),
    ] {
        let candidate = connection
            .get_curation_candidate(&workspace_id, candidate_id)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| format!("candidate {candidate_id} should exist"))?;
        ensure_equal(&candidate.status, &status.to_string(), "candidate status")?;
        ensure_equal(
            &candidate.review_state,
            &review_state.to_string(),
            "candidate review state",
        )?;
        ensure(
            candidate.reviewed_at.is_some(),
            "reviewed_at should be persisted",
        )?;
        ensure_equal(
            &candidate.reviewed_by,
            &Some("smoke-test".to_string()),
            "reviewed_by",
        )?;
    }
    let snoozed = connection
        .get_curation_candidate(&workspace_id, snooze_id)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "snoozed candidate should exist".to_string())?;
    ensure_equal(
        &snoozed.snoozed_until,
        &Some("2030-01-01T00:00:00Z".to_string()),
        "persisted snooze timestamp",
    )?;
    let merged = connection
        .get_curation_candidate(&workspace_id, merge_source_id)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "merged candidate should exist".to_string())?;
    ensure_equal(
        &merged.merged_into_candidate_id,
        &Some(merge_target_id.to_string()),
        "persisted merge target",
    )
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
fn procedure_export_skill_capsule_json_exposes_render_only_artifact() -> TestResult {
    let output = run_ee(&[
        "--json",
        "procedure",
        "export",
        "proc_smoke",
        "--export-format",
        "skill-capsule",
    ])?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    ensure(
        output.status.success(),
        format!("procedure export should succeed; stderr: {stderr}"),
    )?;
    ensure(
        stderr.is_empty(),
        "procedure export JSON stderr must stay clean",
    )?;
    ensure_starts_with(
        &stdout,
        "{\"schema\":\"ee.response.v1\"",
        "procedure export response schema",
    )?;
    let value: serde_json::Value = serde_json::from_str(&stdout)
        .map_err(|error| format!("procedure export stdout must be JSON: {error}"))?;
    ensure_equal(
        &value["data"]["format"],
        &serde_json::json!("skill_capsule"),
        "skill capsule format",
    )?;
    ensure_equal(
        &value["data"]["installMode"],
        &serde_json::json!("render_only"),
        "render-only install mode",
    )?;
    ensure_contains(
        value["data"]["content"].as_str().unwrap_or_default(),
        "This capsule is render-only",
        "skill capsule safety text",
    )?;
    ensure_contains(
        value["data"]["contentHash"].as_str().unwrap_or_default(),
        "blake3:",
        "content hash",
    )
}

#[test]
fn procedure_promote_dry_run_json_reports_planned_curation_and_audit() -> TestResult {
    let output = run_ee(&[
        "--json",
        "procedure",
        "promote",
        "proc_smoke",
        "--dry-run",
        "--actor",
        "MistySalmon",
    ])?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    ensure(
        output.status.success(),
        format!("procedure promote should succeed; stderr: {stderr}"),
    )?;
    ensure(
        stderr.is_empty(),
        "procedure promote JSON stderr must stay clean",
    )?;
    ensure_starts_with(
        &stdout,
        "{\"schema\":\"ee.response.v1\"",
        "procedure promote response schema",
    )?;
    let value: serde_json::Value = serde_json::from_str(&stdout)
        .map_err(|error| format!("procedure promote stdout must be JSON: {error}"))?;
    ensure_equal(
        &value["data"]["schema"],
        &serde_json::json!("ee.procedure.promote_report.v1"),
        "procedure promote data schema",
    )?;
    ensure_equal(
        &value["data"]["dryRun"],
        &serde_json::json!(true),
        "dry-run flag",
    )?;
    ensure_equal(
        &value["data"]["curation"]["candidateType"],
        &serde_json::json!("promote"),
        "curation candidate type",
    )?;
    ensure_equal(
        &value["data"]["audit"]["recorded"],
        &serde_json::json!(false),
        "audit remains unrecorded",
    )?;
    ensure_equal(
        &value["data"]["plannedEffects"][0]["applied"],
        &serde_json::json!(false),
        "planned effects are not applied",
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
        ("EE_FAKE_CASS_SESSION", OsString::from(session_arg.clone())),
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
    let ledger_id = first_json["data"]["ledgerId"]
        .as_str()
        .ok_or("first import missing ledgerId")?;
    let connection =
        DbConnection::open(DatabaseConfig::file(database.clone())).map_err(|e| e.to_string())?;
    let first_ledger = connection
        .get_import_ledger(ledger_id)
        .map_err(|e| e.to_string())?
        .ok_or("first import ledger not found")?;
    ensure_equal(
        &first_ledger.status.as_str(),
        &"completed",
        "first ledger status",
    )?;
    ensure_equal(&first_ledger.attempt_count, &1, "first ledger attempt")?;
    ensure_equal(
        &first_ledger.imported_session_count,
        &1,
        "first ledger imported sessions",
    )?;
    ensure_equal(
        &first_ledger.imported_span_count,
        &1,
        "first ledger imported spans",
    )?;
    let first_cursor: serde_json::Value = serde_json::from_str(
        first_ledger
            .cursor_json
            .as_deref()
            .ok_or("first ledger missing cursor")?,
    )
    .map_err(|error| format!("first ledger cursor must be JSON: {error}"))?;
    ensure_equal(
        &first_cursor["lastSourcePath"],
        &serde_json::json!(session_arg),
        "first cursor source",
    )?;
    ensure_equal(
        &first_cursor["lastLine"],
        &serde_json::json!(1),
        "first cursor line",
    )?;
    ensure_equal(
        &first_cursor["complete"],
        &serde_json::json!(true),
        "first cursor complete",
    )?;
    connection.close().map_err(|e| e.to_string())?;

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
    )?;
    ensure_equal(
        &second_json["data"]["ledgerId"],
        &serde_json::json!(ledger_id),
        "second import resumes the same ledger",
    )?;
    let second_connection =
        DbConnection::open(DatabaseConfig::file(database.clone())).map_err(|e| e.to_string())?;
    let second_ledger = second_connection
        .get_import_ledger(ledger_id)
        .map_err(|e| e.to_string())?
        .ok_or("second import ledger not found")?;
    ensure_equal(
        &second_ledger.status.as_str(),
        &"completed",
        "second ledger status",
    )?;
    ensure_equal(&second_ledger.attempt_count, &2, "second ledger attempt")?;
    ensure_equal(
        &second_ledger.imported_session_count,
        &1,
        "second ledger preserves imported session total",
    )?;
    ensure_equal(
        &second_ledger.imported_span_count,
        &1,
        "second ledger preserves imported span total",
    )?;
    let second_cursor: serde_json::Value = serde_json::from_str(
        second_ledger
            .cursor_json
            .as_deref()
            .ok_or("second ledger missing cursor")?,
    )
    .map_err(|error| format!("second ledger cursor must be JSON: {error}"))?;
    ensure_equal(
        &second_cursor["sessionsDiscovered"],
        &serde_json::json!(1),
        "second cursor discovered",
    )?;
    ensure_equal(
        &second_cursor["sessionsImported"],
        &serde_json::json!(0),
        "second cursor imported this attempt",
    )?;
    ensure_equal(
        &second_cursor["sessionsSkipped"],
        &serde_json::json!(1),
        "second cursor skipped this attempt",
    )?;
    ensure_equal(
        &second_cursor["complete"],
        &serde_json::json!(true),
        "second cursor complete",
    )?;
    second_connection.close().map_err(|e| e.to_string())?;
    Ok(())
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
fn backup_create_json_writes_redacted_artifacts_and_manifest() -> TestResult {
    let root = unique_artifact_dir("backup-create")?;
    let workspace = root.join("workspace");
    fs::create_dir_all(&workspace).map_err(|error| error.to_string())?;

    let database = workspace.join(".ee").join("ee.db");
    let source = root.join("snapshot.jsonl");
    fs::write(
        &source,
        [
            r#"{"schema":"ee.export.header.v1","format_version":1,"created_at":"2026-04-30T00:00:00Z","workspace_id":"wsp_01234567890123456789012345","workspace_path":"/source","export_scope":"memories","redaction_level":"none","record_count":3,"ee_version":"0.1.0","hostname":null,"export_id":"exp-backup","import_source":"native","trust_level":"validated","checksum":null,"signature":null,"source_schema_version":null}"#,
            r#"{"schema":"ee.export.memory.v1","memory_id":"mem_01234567890123456789012346","workspace_id":"wsp_01234567890123456789012345","level":"procedural","kind":"rule","content":"Authorization header should be redacted from backups.","importance":0.8,"confidence":0.9,"utility":0.7,"created_at":"2026-04-30T00:00:00Z","updated_at":null,"expires_at":null,"source_agent":"MistySalmon","provenance_uri":"ee-export://fixture","superseded_by":null,"supersedes":null,"redacted":false,"redaction_reason":null}"#,
            r#"{"schema":"ee.export.tag.v1","memory_id":"mem_01234567890123456789012346","tag":"backup","created_at":"2026-04-30T00:00:00Z"}"#,
            r#"{"schema":"ee.export.footer.v1","export_id":"exp-backup","completed_at":"2026-04-30T00:01:00Z","total_records":3,"memory_count":1,"link_count":0,"tag_count":1,"audit_count":0,"checksum":null,"success":true,"error_message":null}"#,
        ]
        .join("\n"),
    )
    .map_err(|error| error.to_string())?;

    let workspace_arg = workspace.to_string_lossy().into_owned();
    let database_arg = database.to_string_lossy().into_owned();
    let source_arg = source.to_string_lossy().into_owned();
    let backup_root = root.join("backups");
    let backup_root_arg = backup_root.to_string_lossy().into_owned();

    let import = run_ee(&[
        "--workspace",
        workspace_arg.as_str(),
        "--json",
        "import",
        "jsonl",
        "--source",
        source_arg.as_str(),
        "--database",
        database_arg.as_str(),
    ])?;
    let import_stderr = String::from_utf8_lossy(&import.stderr);
    ensure(
        import.status.success(),
        format!("JSONL import should succeed before backup; stderr: {import_stderr}"),
    )?;
    ensure(import.stderr.is_empty(), "import stderr clean")?;

    let backup = run_ee(&[
        "--workspace",
        workspace_arg.as_str(),
        "--json",
        "backup",
        "create",
        "--database",
        database_arg.as_str(),
        "--output-dir",
        backup_root_arg.as_str(),
        "--redaction",
        "minimal",
        "--label",
        "e2e",
    ])?;
    let backup_stderr = String::from_utf8_lossy(&backup.stderr);
    ensure(
        backup.status.success(),
        format!("backup create should succeed; stderr: {backup_stderr}"),
    )?;
    ensure(backup.stderr.is_empty(), "backup create stderr clean")?;
    let backup_json: serde_json::Value = serde_json::from_slice(&backup.stdout)
        .map_err(|error| format!("backup stdout must be JSON: {error}"))?;
    ensure_equal(
        &backup_json["schema"],
        &serde_json::json!("ee.response.v1"),
        "response schema",
    )?;
    ensure_equal(
        &backup_json["data"]["schema"],
        &serde_json::json!("ee.backup.create.v1"),
        "backup data schema",
    )?;
    ensure_equal(
        &backup_json["data"]["status"],
        &serde_json::json!("completed"),
        "backup status",
    )?;
    ensure_equal(
        &backup_json["data"]["verificationStatus"],
        &serde_json::json!("verified"),
        "backup verification",
    )?;
    ensure_equal(
        &backup_json["data"]["redactionLevel"],
        &serde_json::json!("minimal"),
        "backup redaction level",
    )?;

    let records_path = PathBuf::from(
        backup_json["data"]["recordsPath"]
            .as_str()
            .ok_or_else(|| "records path missing".to_string())?,
    );
    let manifest_path = PathBuf::from(
        backup_json["data"]["manifestPath"]
            .as_str()
            .ok_or_else(|| "manifest path missing".to_string())?,
    );
    ensure(records_path.is_file(), "backup records file exists")?;
    ensure(manifest_path.is_file(), "backup manifest file exists")?;

    let records = fs::read_to_string(&records_path).map_err(|error| error.to_string())?;
    ensure(
        records.contains("[REDACTED]"),
        "backup records contain redaction marker",
    )?;
    ensure(
        !records.contains("Authorization header"),
        "backup records must not contain raw sensitive content",
    )?;
    let manifest = fs::read_to_string(&manifest_path).map_err(|error| error.to_string())?;
    ensure(
        manifest.contains("ee.backup.manifest.v1"),
        "backup manifest schema is present",
    )
}

#[cfg(unix)]
#[test]
fn artifact_registry_registers_indexes_exports_and_supports_context() -> TestResult {
    let workspace = unique_artifact_dir("artifact-registry")?;
    let logs_dir = workspace.join("logs");
    fs::create_dir_all(&logs_dir).map_err(|error| error.to_string())?;
    let artifact_path = logs_dir.join("build.log");
    fs::write(
        &artifact_path,
        "artifact registry sentinel alpha\ncargo fmt passed\n",
    )
    .map_err(|error| error.to_string())?;
    let secret_path = logs_dir.join("secret.log");
    let secret_key_name = ["api", "_", "key"].concat();
    fs::write(
        &secret_path,
        format!("{secret_key_name}=redaction-fixture\nartifact secret sentinel beta\n"),
    )
    .map_err(|error| error.to_string())?;
    let binary_path = logs_dir.join("binary.bin");
    fs::write(&binary_path, [0_u8, 159, 146, 150]).map_err(|error| error.to_string())?;
    let large_path = logs_dir.join("large.log");
    fs::write(&large_path, "artifact registry large fixture payload")
        .map_err(|error| error.to_string())?;
    std::os::unix::fs::symlink(&artifact_path, logs_dir.join("inside-link.log"))
        .map_err(|error| error.to_string())?;
    let outside = unique_artifact_dir("artifact-registry-outside")?;
    fs::create_dir_all(&outside).map_err(|error| error.to_string())?;
    let outside_file = outside.join("outside.log");
    fs::write(&outside_file, "outside artifact\n").map_err(|error| error.to_string())?;
    std::os::unix::fs::symlink(&outside_file, logs_dir.join("outside-link.log"))
        .map_err(|error| error.to_string())?;
    let workspace_arg = workspace.to_string_lossy().into_owned();

    let init = run_ee(&["--workspace", workspace_arg.as_str(), "--json", "init"])?;
    let init_stderr = String::from_utf8_lossy(&init.stderr);
    ensure(
        init.status.success(),
        format!("init should succeed; stderr: {init_stderr}"),
    )?;
    ensure(init.stderr.is_empty(), "init stderr clean")?;

    let dry_run = run_ee(&[
        "--workspace",
        workspace_arg.as_str(),
        "--json",
        "artifact",
        "register",
        "logs/build.log",
        "--kind",
        "log",
        "--title",
        "build log",
        "--dry-run",
    ])?;
    let dry_run_stderr = String::from_utf8_lossy(&dry_run.stderr);
    ensure(
        dry_run.status.success(),
        format!("artifact dry-run should succeed; stderr: {dry_run_stderr}"),
    )?;
    ensure(dry_run.stderr.is_empty(), "artifact dry-run stderr clean")?;
    let dry_run_json: serde_json::Value = serde_json::from_slice(&dry_run.stdout)
        .map_err(|error| format!("artifact dry-run stdout must be JSON: {error}"))?;
    ensure_equal(
        &dry_run_json["data"]["dryRun"],
        &serde_json::json!(true),
        "artifact dry-run flag",
    )?;
    ensure_equal(
        &dry_run_json["data"]["persisted"],
        &serde_json::json!(false),
        "artifact dry-run persisted flag",
    )?;

    let assert_policy_error = |output: &Output, context: &str, message: &str| -> TestResult {
        ensure(!output.status.success(), format!("{context} should fail"))?;
        ensure(output.stderr.is_empty(), format!("{context} stderr clean"))?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        ensure_no_ansi(&stdout, context)?;
        let error_json: serde_json::Value = serde_json::from_slice(&output.stdout)
            .map_err(|error| format!("{context} stdout must be JSON: {error}"))?;
        ensure_equal(
            &error_json["schema"],
            &serde_json::json!("ee.error.v1"),
            context,
        )?;
        ensure_equal(
            &error_json["error"]["code"],
            &serde_json::json!("policy_denied"),
            context,
        )?;
        ensure_contains(
            error_json["error"]["message"].as_str().unwrap_or_default(),
            message,
            context,
        )
    };

    let parent_escape = run_ee(&[
        "--workspace",
        workspace_arg.as_str(),
        "--json",
        "artifact",
        "register",
        "../outside.log",
        "--dry-run",
    ])?;
    assert_policy_error(
        &parent_escape,
        "artifact parent traversal rejection",
        "parent-directory traversal",
    )?;

    let symlink_escape = run_ee(&[
        "--workspace",
        workspace_arg.as_str(),
        "--json",
        "artifact",
        "register",
        "logs/outside-link.log",
        "--dry-run",
    ])?;
    assert_policy_error(
        &symlink_escape,
        "artifact symlink escape rejection",
        "outside the workspace",
    )?;

    let oversized = run_ee(&[
        "--workspace",
        workspace_arg.as_str(),
        "--json",
        "artifact",
        "register",
        "logs/large.log",
        "--max-bytes",
        "8",
        "--dry-run",
    ])?;
    assert_policy_error(&oversized, "artifact oversized rejection", "too large")?;

    let binary_dry_run = run_ee(&[
        "--workspace",
        workspace_arg.as_str(),
        "--json",
        "artifact",
        "register",
        "logs/binary.bin",
        "--kind",
        "fixture",
        "--dry-run",
    ])?;
    let binary_stderr = String::from_utf8_lossy(&binary_dry_run.stderr);
    ensure(
        binary_dry_run.status.success(),
        format!("binary artifact dry-run should succeed; stderr: {binary_stderr}"),
    )?;
    ensure(
        binary_dry_run.stderr.is_empty(),
        "binary artifact dry-run stderr clean",
    )?;
    let binary_json: serde_json::Value = serde_json::from_slice(&binary_dry_run.stdout)
        .map_err(|error| format!("binary artifact dry-run stdout must be JSON: {error}"))?;
    ensure_equal(
        &binary_json["data"]["artifact"]["redactionStatus"],
        &serde_json::json!("not_text"),
        "binary artifact redaction status",
    )?;
    ensure_equal(
        &binary_json["data"]["artifact"]["snippet"],
        &serde_json::Value::Null,
        "binary artifact snippet omitted",
    )?;
    ensure(
        binary_json["data"]["degraded"]
            .as_array()
            .is_some_and(|items| {
                items
                    .iter()
                    .any(|item| item["code"] == "artifact_binary_not_indexed")
            }),
        "binary artifact reports metadata-only degradation",
    )?;

    let inside_symlink = run_ee(&[
        "--workspace",
        workspace_arg.as_str(),
        "--json",
        "artifact",
        "register",
        "logs/inside-link.log",
        "--kind",
        "log",
        "--dry-run",
    ])?;
    let inside_symlink_stderr = String::from_utf8_lossy(&inside_symlink.stderr);
    ensure(
        inside_symlink.status.success(),
        format!("inside symlink dry-run should succeed; stderr: {inside_symlink_stderr}"),
    )?;
    ensure(
        inside_symlink.stderr.is_empty(),
        "inside symlink dry-run stderr clean",
    )?;
    let inside_symlink_json: serde_json::Value = serde_json::from_slice(&inside_symlink.stdout)
        .map_err(|error| format!("inside symlink stdout must be JSON: {error}"))?;
    ensure(
        inside_symlink_json["data"]["degraded"]
            .as_array()
            .is_some_and(|items| {
                items
                    .iter()
                    .any(|item| item["code"] == "artifact_symlink_resolved")
            }),
        "inside symlink reports canonicalization degradation",
    )?;

    let remember = run_ee(&[
        "--workspace",
        workspace_arg.as_str(),
        "--json",
        "remember",
        "--level",
        "procedural",
        "--kind",
        "rule",
        "--source",
        "file://logs/build.log",
        "Treat build log evidence as a release-check memory.",
    ])?;
    let remember_stderr = String::from_utf8_lossy(&remember.stderr);
    ensure(
        remember.status.success(),
        format!("remember should succeed; stderr: {remember_stderr}"),
    )?;
    ensure(remember.stderr.is_empty(), "remember stderr clean")?;
    let remember_json: serde_json::Value = serde_json::from_slice(&remember.stdout)
        .map_err(|error| format!("remember stdout must be JSON: {error}"))?;
    let memory_id = remember_json["data"]["memory_id"]
        .as_str()
        .ok_or_else(|| "remember JSON missing memory_id".to_string())?;

    let register = run_ee(&[
        "--workspace",
        workspace_arg.as_str(),
        "--json",
        "artifact",
        "register",
        "logs/build.log",
        "--kind",
        "log",
        "--link-memory",
        memory_id,
    ])?;
    let register_stderr = String::from_utf8_lossy(&register.stderr);
    ensure(
        register.status.success(),
        format!("artifact register should succeed; stderr: {register_stderr}"),
    )?;
    ensure(register.stderr.is_empty(), "artifact register stderr clean")?;
    let register_json: serde_json::Value = serde_json::from_slice(&register.stdout)
        .map_err(|error| format!("artifact register stdout must be JSON: {error}"))?;
    ensure_equal(
        &register_json["data"]["persisted"],
        &serde_json::json!(true),
        "artifact persisted flag",
    )?;
    ensure_equal(
        &register_json["data"]["artifact"]["redactionStatus"],
        &serde_json::json!("checked"),
        "artifact redaction status",
    )?;
    let artifact_id = register_json["data"]["artifact"]["id"]
        .as_str()
        .ok_or_else(|| "artifact JSON missing id".to_string())?;

    let secret_register = run_ee(&[
        "--workspace",
        workspace_arg.as_str(),
        "--json",
        "artifact",
        "register",
        "logs/secret.log",
        "--kind",
        "log",
    ])?;
    let secret_register_stderr = String::from_utf8_lossy(&secret_register.stderr);
    ensure(
        secret_register.status.success(),
        format!("secret artifact register should succeed; stderr: {secret_register_stderr}"),
    )?;
    ensure(
        secret_register.stderr.is_empty(),
        "secret artifact register stderr clean",
    )?;
    let secret_stdout = String::from_utf8_lossy(&secret_register.stdout);
    ensure(
        !secret_stdout.contains("redaction-fixture"),
        "secret artifact register output must omit raw secret value",
    )?;
    let secret_register_json: serde_json::Value =
        serde_json::from_slice(&secret_register.stdout)
            .map_err(|error| format!("secret artifact register stdout must be JSON: {error}"))?;
    ensure_equal(
        &secret_register_json["data"]["artifact"]["redactionStatus"],
        &serde_json::json!("redacted"),
        "secret artifact redaction status",
    )?;
    ensure_equal(
        &secret_register_json["data"]["artifact"]["snippet"],
        &serde_json::json!("[REDACTED]"),
        "secret artifact redacted snippet",
    )?;
    ensure(
        secret_register_json["data"]["degraded"]
            .as_array()
            .is_some_and(|items| {
                items
                    .iter()
                    .any(|item| item["code"] == "artifact_secret_redacted")
            }),
        "secret artifact reports redaction degradation",
    )?;

    let list = run_ee(&[
        "--workspace",
        workspace_arg.as_str(),
        "--json",
        "artifact",
        "list",
    ])?;
    let list_stderr = String::from_utf8_lossy(&list.stderr);
    ensure(
        list.status.success(),
        format!("artifact list should succeed; stderr: {list_stderr}"),
    )?;
    ensure(list.stderr.is_empty(), "artifact list stderr clean")?;
    let list_json: serde_json::Value = serde_json::from_slice(&list.stdout)
        .map_err(|error| format!("artifact list stdout must be JSON: {error}"))?;
    ensure_equal(
        &list_json["data"]["totalCount"],
        &serde_json::json!(2),
        "artifact registry durable count",
    )?;

    let inspect = run_ee(&[
        "--workspace",
        workspace_arg.as_str(),
        "--json",
        "artifact",
        "inspect",
        artifact_id,
    ])?;
    let inspect_stderr = String::from_utf8_lossy(&inspect.stderr);
    ensure(
        inspect.status.success(),
        format!("artifact inspect should succeed; stderr: {inspect_stderr}"),
    )?;
    ensure(inspect.stderr.is_empty(), "artifact inspect stderr clean")?;
    let inspect_json: serde_json::Value = serde_json::from_slice(&inspect.stdout)
        .map_err(|error| format!("artifact inspect stdout must be JSON: {error}"))?;
    ensure_equal(
        &inspect_json["data"]["found"],
        &serde_json::json!(true),
        "artifact inspect found flag",
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
        &rebuild_json["data"]["artifacts_indexed"],
        &serde_json::json!(2),
        "artifact indexed count",
    )?;

    let search = run_ee(&[
        "--workspace",
        workspace_arg.as_str(),
        "--json",
        "search",
        "sentinel alpha",
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
            .is_some_and(|results| results.iter().any(|hit| hit["doc_id"] == artifact_id)),
        format!(
            "search results include artifact document; stdout: {}",
            String::from_utf8_lossy(&search.stdout)
        ),
    )?;

    let context = run_ee(&[
        "--workspace",
        workspace_arg.as_str(),
        "--json",
        "context",
        "sentinel alpha",
        "--max-tokens",
        "1200",
    ])?;
    let context_stderr = String::from_utf8_lossy(&context.stderr);
    ensure(
        context.status.success(),
        format!("context should succeed; stderr: {context_stderr}"),
    )?;
    ensure(context.stderr.is_empty(), "context stderr clean")?;
    let context_json: serde_json::Value = serde_json::from_slice(&context.stdout)
        .map_err(|error| format!("context stdout must be JSON: {error}"))?;
    ensure(
        context_json["data"]["pack"]["items"]
            .as_array()
            .is_some_and(|items| {
                items.iter().any(|item| {
                    item["memoryId"] == memory_id
                        && item["why"]
                            .as_str()
                            .is_some_and(|why| why.contains("registered artifact"))
                })
            }),
        "context selects linked memory through artifact hit",
    )?;

    let support = run_ee(&[
        "--workspace",
        workspace_arg.as_str(),
        "--json",
        "support",
        "bundle",
        "--dry-run",
    ])?;
    let support_stderr = String::from_utf8_lossy(&support.stderr);
    ensure(
        support.status.success(),
        format!("support bundle should succeed; stderr: {support_stderr}"),
    )?;
    ensure(support.stderr.is_empty(), "support bundle stderr clean")?;
    let support_json: serde_json::Value = serde_json::from_slice(&support.stdout)
        .map_err(|error| format!("support stdout must be JSON: {error}"))?;
    ensure_equal(
        &support_json["data"]["artifact_registry_count"],
        &serde_json::json!(2),
        "support artifact registry count",
    )?;
    ensure_equal(
        &support_json["data"]["artifact_registry_included"],
        &serde_json::json!(true),
        "support artifact registry included",
    )?;
    ensure(
        support_json["data"]["files_collected"]
            .as_array()
            .is_some_and(|files| {
                files
                    .iter()
                    .any(|file| file == ".ee/artifacts.redacted.jsonl")
                    && files.iter().all(|file| file != ".ee/db.sqlite")
            }),
        "support bundle references redacted artifact export and omits raw DB",
    )?;
    ensure(
        !String::from_utf8_lossy(&support.stdout).contains("redaction-fixture"),
        "support bundle dry-run output must omit raw secret value",
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
        &remember_json["data"]["suggested_link_status"],
        &serde_json::json!("no_candidates"),
        "remember suggested link status",
    )?;
    ensure_equal(
        &remember_json["data"]["suggested_link_degradations"],
        &serde_json::json!([]),
        "remember suggested link degradations",
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
    let pack_args = [
        "--workspace",
        workspace_arg.as_str(),
        "pack",
        "--query-file",
        query_file_arg.as_str(),
    ];
    let pack_run = run_ee_logged(
        "query-file-contract",
        &pack_args,
        &workspace,
        Some(&query_file),
        "d19r-valid-query-file-json-pack",
        "ee.response.v1",
        Some("tests/fixtures/golden/agent/query_file_context_pack.json.golden"),
    )?;
    let pack_dossier_dir = pack_run.dossier_dir.clone();
    let pack = pack_run.output;
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
    ensure(
        pack_dossier_dir.join("command.txt").is_file(),
        "query-file dossier should log command argv",
    )?;
    ensure(
        pack_dossier_dir.join("query-file.blake3.txt").is_file(),
        "query-file dossier should log query file hash",
    )?;
    ensure(
        pack_dossier_dir.join("stdout.schema.json").is_file(),
        "query-file dossier should log schema status",
    )?;

    let why_after_pack = run_ee(&[
        "--workspace",
        workspace_arg.as_str(),
        "--json",
        "why",
        memory_id,
    ])?;
    let why_after_pack_stderr = String::from_utf8_lossy(&why_after_pack.stderr);
    ensure(
        why_after_pack.status.success(),
        format!("why after query-file pack should succeed; stderr: {why_after_pack_stderr}"),
    )?;
    ensure(
        why_after_pack.stderr.is_empty(),
        "why after query-file pack stderr clean",
    )?;
    let why_after_pack_json: serde_json::Value = serde_json::from_slice(&why_after_pack.stdout)
        .map_err(|error| format!("why after query-file pack stdout must be JSON: {error}"))?;
    let latest_pack = &why_after_pack_json["data"]["selection"]["latestPackSelection"];
    ensure_equal(
        &latest_pack["query"],
        &serde_json::json!("prepare release"),
        "why latest pack query",
    )?;
    ensure(
        latest_pack["packId"]
            .as_str()
            .is_some_and(|pack_id| pack_id.starts_with("pack_")),
        "why latest pack id should be persisted",
    )?;
    ensure(
        latest_pack["packHash"]
            .as_str()
            .is_some_and(|pack_hash| pack_hash.starts_with("blake3:")),
        "why latest pack hash should be persisted",
    )?;
    write_json_artifact(
        &pack_dossier_dir.join("pack-record.json"),
        &serde_json::json!({
            "packId": latest_pack["packId"].clone(),
            "packHash": latest_pack["packHash"].clone(),
            "query": latest_pack["query"].clone(),
            "fixtureId": "d19r-valid-query-file-json-pack",
            "effectExpected": "query-file pack command persists a pack record",
            "effectObserved": "ee why returned latestPackSelection for the selected memory"
        }),
    )?;

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
    )?;

    let markdown_query_file = workspace.join("task-markdown.eeq.json");
    fs::write(
        &markdown_query_file,
        r#"{
          "version": "ee.query.v1",
          "query": {"text": "prepare release"},
          "output": {"format": "markdown", "profile": "compact"}
        }"#,
    )
    .map_err(|error| error.to_string())?;
    let markdown_query_file_arg = markdown_query_file.to_string_lossy().into_owned();
    let markdown_pack = run_ee(&[
        "--workspace",
        workspace_arg.as_str(),
        "pack",
        "--query-file",
        markdown_query_file_arg.as_str(),
    ])?;
    let markdown_stdout = String::from_utf8_lossy(&markdown_pack.stdout);
    let markdown_stderr = String::from_utf8_lossy(&markdown_pack.stderr);
    ensure(
        markdown_pack.status.success(),
        format!("pack query-file markdown should succeed; stderr: {markdown_stderr}"),
    )?;
    ensure(
        markdown_pack.stderr.is_empty(),
        "pack query-file markdown stderr clean",
    )?;
    ensure_no_ansi(&markdown_stdout, "pack query-file markdown stdout")?;
    ensure_contains(
        &markdown_stdout,
        "# Context Pack: prepare release",
        "query-file markdown title",
    )?;
    ensure_contains(
        &markdown_stdout,
        "**Profile:** compact",
        "query-file markdown profile",
    )?;

    let toon_query_file = workspace.join("task-toon.eeq.json");
    fs::write(
        &toon_query_file,
        r#"{
          "version": "ee.query.v1",
          "query": {"text": "prepare release"},
          "output": {"format": "toon", "profile": "balanced"}
        }"#,
    )
    .map_err(|error| error.to_string())?;
    let toon_query_file_arg = toon_query_file.to_string_lossy().into_owned();
    let toon_pack = run_ee(&[
        "--workspace",
        workspace_arg.as_str(),
        "pack",
        "--query-file",
        toon_query_file_arg.as_str(),
    ])?;
    let toon_stdout = String::from_utf8_lossy(&toon_pack.stdout);
    let toon_stderr = String::from_utf8_lossy(&toon_pack.stderr);
    ensure(
        toon_pack.status.success(),
        format!("pack query-file toon should succeed; stderr: {toon_stderr}"),
    )?;
    ensure(
        toon_pack.stderr.is_empty(),
        "pack query-file toon stderr clean",
    )?;
    ensure_no_ansi(&toon_stdout, "pack query-file toon stdout")?;
    ensure_contains(
        &toon_stdout,
        "schema: ee.response.v1",
        "query-file toon schema",
    )?;
    ensure_contains(
        &toon_stdout,
        "query: prepare release",
        "query-file toon query",
    )?;

    let fields_query_file = workspace.join("task-fields.eeq.json");
    fs::write(
        &fields_query_file,
        r#"{
          "version": "ee.query.v1",
          "query": {"text": "prepare release"},
          "output": {"format": "json", "fields": "summary"}
        }"#,
    )
    .map_err(|error| error.to_string())?;
    let fields_query_file_arg = fields_query_file.to_string_lossy().into_owned();
    let fields_pack = run_ee(&[
        "--workspace",
        workspace_arg.as_str(),
        "--fields",
        "summary",
        "--json",
        "pack",
        "--query-file",
        fields_query_file_arg.as_str(),
    ])?;
    let fields_stdout = String::from_utf8_lossy(&fields_pack.stdout);
    let fields_stderr = String::from_utf8_lossy(&fields_pack.stderr);
    ensure(
        fields_pack.status.success(),
        format!("pack query-file fields should succeed; stderr: {fields_stderr}"),
    )?;
    ensure(
        fields_pack.stderr.is_empty(),
        "pack query-file fields stderr clean",
    )?;
    ensure_no_ansi(&fields_stdout, "pack query-file fields stdout")?;
    let fields_json: serde_json::Value = serde_json::from_slice(&fields_pack.stdout)
        .map_err(|error| format!("fields pack stdout must be JSON: {error}"))?;
    ensure(
        fields_json["data"]["degraded"]
            .as_array()
            .is_some_and(|items| {
                items
                    .iter()
                    .any(|item| item["code"] == "query_output_fields_cli_controlled")
            }),
        "query-file output.fields should be reported as CLI-controlled degradation",
    )
}

#[cfg(unix)]
#[test]
fn index_reembed_json_rebuilds_index_and_records_job() -> TestResult {
    let workspace = unique_artifact_dir("index-reembed-job")?;
    fs::create_dir_all(&workspace).map_err(|error| error.to_string())?;
    let workspace_arg = workspace.to_string_lossy().into_owned();

    let init = run_ee(&["--workspace", workspace_arg.as_str(), "--json", "init"])?;
    let init_stderr = String::from_utf8_lossy(&init.stderr);
    ensure(
        init.status.success(),
        format!("init should succeed; stderr: {init_stderr}"),
    )?;
    ensure(init.stderr.is_empty(), "init stderr clean")?;

    let remember = run_ee(&[
        "--workspace",
        workspace_arg.as_str(),
        "--json",
        "remember",
        "--level",
        "procedural",
        "--kind",
        "rule",
        "Re-embed indexes after changing embedding model metadata.",
    ])?;
    let remember_stderr = String::from_utf8_lossy(&remember.stderr);
    ensure(
        remember.status.success(),
        format!("remember should succeed; stderr: {remember_stderr}"),
    )?;
    ensure(remember.stderr.is_empty(), "remember stderr clean")?;

    let dry_run = run_ee(&[
        "--workspace",
        workspace_arg.as_str(),
        "--json",
        "index",
        "reembed",
        "--dry-run",
    ])?;
    let dry_run_stderr = String::from_utf8_lossy(&dry_run.stderr);
    ensure(
        dry_run.status.success(),
        format!("index reembed dry-run should succeed; stderr: {dry_run_stderr}"),
    )?;
    ensure(
        dry_run.stderr.is_empty(),
        "index reembed dry-run stderr clean",
    )?;
    let dry_run_json: serde_json::Value = serde_json::from_slice(&dry_run.stdout)
        .map_err(|error| format!("dry-run stdout must be JSON: {error}"))?;
    ensure_equal(
        &dry_run_json["data"]["job_status"],
        &serde_json::json!("dry_run_not_queued"),
        "dry-run job status",
    )?;

    let reembed = run_ee(&[
        "--workspace",
        workspace_arg.as_str(),
        "--json",
        "index",
        "reembed",
    ])?;
    let reembed_stdout = String::from_utf8_lossy(&reembed.stdout);
    let reembed_stderr = String::from_utf8_lossy(&reembed.stderr);
    ensure(
        reembed.status.success(),
        format!("index reembed should succeed; stdout: {reembed_stdout}; stderr: {reembed_stderr}"),
    )?;
    ensure(reembed.stderr.is_empty(), "index reembed stderr clean")?;
    ensure_no_ansi(&reembed_stdout, "index reembed JSON stdout")?;
    let reembed_json: serde_json::Value = serde_json::from_slice(&reembed.stdout)
        .map_err(|error| format!("reembed stdout must be JSON: {error}"))?;
    ensure_equal(
        &reembed_json["schema"],
        &serde_json::json!("ee.response.v1"),
        "reembed schema",
    )?;
    ensure_equal(
        &reembed_json["data"]["command"],
        &serde_json::json!("index_reembed"),
        "reembed command",
    )?;
    ensure_equal(
        &reembed_json["data"]["status"],
        &serde_json::json!("success"),
        "reembed status",
    )?;
    ensure_equal(
        &reembed_json["data"]["job_status"],
        &serde_json::json!("completed"),
        "reembed job status",
    )?;
    ensure_equal(
        &reembed_json["data"]["document_source"],
        &serde_json::Value::Null,
        "reembed document source",
    )?;
    ensure_equal(
        &reembed_json["data"]["embedding_scope"],
        &serde_json::json!("all_documents"),
        "reembed embedding scope",
    )?;
    ensure_equal(
        &reembed_json["data"]["embedding"]["fast_model_id"],
        &serde_json::json!("fnv1a-256"),
        "reembed fast embedder",
    )?;
    ensure_equal(
        &reembed_json["data"]["dry_run"],
        &serde_json::json!(false),
        "reembed dry_run flag",
    )?;
    let job_id = reembed_json["data"]["job_id"]
        .as_str()
        .ok_or_else(|| "reembed job_id must be a string".to_string())?;

    let database = workspace.join(".ee").join("ee.db");
    let connection = DbConnection::open_file(database).map_err(|error| error.to_string())?;
    let job = connection
        .get_search_index_job(job_id)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "reembed job should be persisted".to_string())?;
    ensure_equal(&job.status.as_str(), &"completed", "stored job status")?;
    ensure_equal(&job.document_source.as_deref(), &None, "stored job source")?;
    ensure_equal(&job.documents_total, &1, "stored documents_total")?;
    ensure_equal(&job.documents_indexed, &1, "stored documents_indexed")?;
    connection.close().map_err(|error| error.to_string())
}

#[cfg(unix)]
#[test]
fn remember_returns_staged_tag_cooccurrence_suggestions_without_mutating_links() -> TestResult {
    let workspace = unique_artifact_dir("remember-link-suggestions")?;
    fs::create_dir_all(&workspace).map_err(|error| error.to_string())?;
    let workspace_arg = workspace.to_string_lossy().into_owned();

    let init = run_ee(&["--workspace", workspace_arg.as_str(), "--json", "init"])?;
    let init_stderr = String::from_utf8_lossy(&init.stderr);
    ensure(
        init.status.success(),
        format!("init should succeed; stderr: {init_stderr}"),
    )?;
    ensure(init.stderr.is_empty(), "init stderr clean")?;

    let first = run_ee(&[
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
        "Run cargo fmt and clippy before release.",
    ])?;
    let first_stderr = String::from_utf8_lossy(&first.stderr);
    ensure(
        first.status.success(),
        format!("first remember should succeed; stderr: {first_stderr}"),
    )?;
    ensure(first.stderr.is_empty(), "first remember stderr clean")?;
    let first_json: serde_json::Value = serde_json::from_slice(&first.stdout)
        .map_err(|error| format!("first remember stdout must be JSON: {error}"))?;
    let first_memory_id = first_json["data"]["memory_id"]
        .as_str()
        .ok_or_else(|| "first remember memory_id must be a string".to_string())?
        .to_string();
    ensure_equal(
        &first_json["data"]["suggested_links"],
        &serde_json::json!([]),
        "first remember has no suggestions",
    )?;

    let second = run_ee(&[
        "--workspace",
        workspace_arg.as_str(),
        "--json",
        "remember",
        "--level",
        "procedural",
        "--kind",
        "rule",
        "--tags",
        "checks,release",
        "Before release, collect command evidence for checks.",
    ])?;
    let second_stdout = String::from_utf8_lossy(&second.stdout);
    let second_stderr = String::from_utf8_lossy(&second.stderr);
    ensure(
        second.status.success(),
        format!("second remember should succeed; stdout: {second_stdout}; stderr: {second_stderr}"),
    )?;
    ensure(second.stderr.is_empty(), "second remember stderr clean")?;
    ensure_no_ansi(&second_stdout, "second remember JSON stdout")?;
    let second_json: serde_json::Value = serde_json::from_slice(&second.stdout)
        .map_err(|error| format!("second remember stdout must be JSON: {error}"))?;
    ensure_equal(
        &second_json["data"]["schema"],
        &serde_json::Value::Null,
        "remember data should not nest a second schema",
    )?;
    ensure_equal(
        &second_json["data"]["suggested_link_status"],
        &serde_json::json!("ready"),
        "second remember suggestion status",
    )?;
    let suggestions = second_json["data"]["suggested_links"]
        .as_array()
        .ok_or_else(|| "suggested_links must be an array".to_string())?;
    ensure(suggestions.len() == 1, "one staged suggestion")?;
    let suggestion = &suggestions[0];
    ensure_equal(
        &suggestion["schema"],
        &serde_json::json!("ee.remember.suggested_link.v1"),
        "suggestion schema",
    )?;
    ensure_equal(
        &suggestion["relation"],
        &serde_json::json!("co_tag"),
        "suggestion relation",
    )?;
    ensure_equal(
        &suggestion["target_memory_id"],
        &serde_json::json!(first_memory_id),
        "suggestion target",
    )?;
    ensure_equal(
        &suggestion["source"],
        &serde_json::json!("tag_cooccurrence"),
        "suggestion source",
    )?;
    ensure_equal(
        &suggestion["matched_tags"],
        &serde_json::json!(["checks", "release"]),
        "suggestion matched tags",
    )?;
    ensure_equal(
        &suggestion["evidence_count"],
        &serde_json::json!(2),
        "suggestion evidence count",
    )?;
    ensure(
        suggestion["next_action"]
            .as_str()
            .is_some_and(|next_action| next_action.contains("explicit curation/apply command")),
        "suggestion next action should make review/apply explicit",
    )?;

    let why = run_ee(&[
        "--workspace",
        workspace_arg.as_str(),
        "--json",
        "why",
        first_memory_id.as_str(),
    ])?;
    let why_stderr = String::from_utf8_lossy(&why.stderr);
    ensure(
        why.status.success(),
        format!("why should succeed; stderr: {why_stderr}"),
    )?;
    ensure(why.stderr.is_empty(), "why stderr clean")?;
    let why_json: serde_json::Value = serde_json::from_slice(&why.stdout)
        .map_err(|error| format!("why stdout must be JSON: {error}"))?;
    ensure_equal(
        &why_json["data"]["links"],
        &serde_json::json!([]),
        "staged suggestions must not create durable memory_links rows",
    )
}

#[cfg(unix)]
#[test]
fn memory_temporal_links_and_graph_outputs_compose() -> TestResult {
    let workspace = unique_artifact_dir("memory-temporal-links-graph")?;
    fs::create_dir_all(&workspace).map_err(|error| error.to_string())?;
    let workspace_arg = workspace.to_string_lossy().into_owned();

    let init = run_ee(&["--workspace", workspace_arg.as_str(), "--json", "init"])?;
    let init_stderr = String::from_utf8_lossy(&init.stderr);
    ensure(
        init.status.success(),
        format!("init should succeed; stderr: {init_stderr}"),
    )?;
    ensure(init.stderr.is_empty(), "init stderr clean")?;

    let remember = |content: &str,
                    tags: &str,
                    valid_from: Option<&str>,
                    valid_to: Option<&str>|
     -> Result<(serde_json::Value, String), String> {
        let mut args = vec![
            "--workspace",
            workspace_arg.as_str(),
            "--json",
            "remember",
            "--level",
            "semantic",
            "--kind",
            "fact",
            "--tags",
            tags,
        ];
        if let Some(valid_from) = valid_from {
            args.push("--valid-from");
            args.push(valid_from);
        }
        if let Some(valid_to) = valid_to {
            args.push("--valid-to");
            args.push(valid_to);
        }
        args.push(content);

        let output = run_ee(&args)?;
        let stderr = String::from_utf8_lossy(&output.stderr);
        ensure(
            output.status.success(),
            format!("remember should succeed; stderr: {stderr}"),
        )?;
        ensure(output.stderr.is_empty(), "remember stderr clean")?;
        let json: serde_json::Value = serde_json::from_slice(&output.stdout)
            .map_err(|error| format!("remember stdout must be JSON: {error}"))?;
        let memory_id = json["data"]["memory_id"]
            .as_str()
            .ok_or_else(|| "remember memory_id must be a string".to_string())?
            .to_string();
        Ok((json, memory_id))
    };

    let (current_json, current_id) = remember(
        "Current validity memory for graph composition.",
        "temporal,validity,composition,link-proof",
        Some("2020-01-01T00:00:00Z"),
        Some("2099-01-01T00:00:00Z"),
    )?;
    let (expired_json, expired_id) = remember(
        "Expired validity memory that remains explainable.",
        "temporal,validity,composition,link-proof",
        Some("2020-01-01T00:00:00Z"),
        Some("2021-01-01T00:00:00Z"),
    )?;
    let (future_json, future_id) = remember(
        "Future validity memory that remains visible before applicability.",
        "temporal,validity,composition,future-path",
        Some("2099-01-01T00:00:00Z"),
        None,
    )?;
    let (boundary_json, boundary_id) = remember(
        "Boundary equal validity memory for instant applicability.",
        "temporal,validity,composition,link-proof,instant-window",
        Some("2050-01-01T00:00:00Z"),
        Some("2050-01-01T00:00:00Z"),
    )?;
    let (unknown_json, unknown_id) = remember(
        "Unknown validity memory stays visible without temporal bounds.",
        "temporal,validity,composition,unknown-window",
        None,
        None,
    )?;

    ensure_equal(
        &current_json["data"]["validity_status"],
        &serde_json::json!("current"),
        "current validity status",
    )?;
    ensure_equal(
        &expired_json["data"]["validity_status"],
        &serde_json::json!("expired"),
        "expired validity status",
    )?;
    ensure_equal(
        &future_json["data"]["validity_status"],
        &serde_json::json!("future"),
        "future validity status",
    )?;
    ensure_equal(
        &boundary_json["data"]["validity_window_kind"],
        &serde_json::json!("instant"),
        "boundary-equal validity kind",
    )?;
    ensure_equal(
        &unknown_json["data"]["validity_status"],
        &serde_json::json!("unknown"),
        "unknown validity status",
    )?;

    let staged_suggestions = expired_json["data"]["suggested_links"]
        .as_array()
        .ok_or_else(|| "expired remember suggested_links must be an array".to_string())?;
    ensure(
        staged_suggestions.iter().any(|suggestion| {
            suggestion["relation"].as_str() == Some("co_tag")
                && suggestion["target_memory_id"].as_str() == Some(current_id.as_str())
                && suggestion["evidence_count"].as_u64() == Some(4)
        }),
        "remember should stage a deterministic co_tag suggestion without applying it",
    )?;

    let broad_only_candidates = generate_autolink_candidates(
        &[
            AutolinkMemoryInput {
                memory_id: "mem_broad_a".to_string(),
                tags: vec!["temporal".to_string(), "validity".to_string()],
                evidence_count: 1,
            },
            AutolinkMemoryInput {
                memory_id: "mem_broad_b".to_string(),
                tags: vec!["temporal".to_string(), "validity".to_string()],
                evidence_count: 1,
            },
            AutolinkMemoryInput {
                memory_id: "mem_broad_c".to_string(),
                tags: vec!["temporal".to_string(), "validity".to_string()],
                evidence_count: 1,
            },
        ],
        &[],
        &AutolinkCandidateOptions {
            common_tag_max_count: 2,
            ..Default::default()
        },
    );
    ensure(
        broad_only_candidates.is_empty(),
        "autolink should suppress pairs supported only by broad tags",
    )?;

    let autolink_candidates = generate_autolink_candidates(
        &[
            AutolinkMemoryInput {
                memory_id: current_id.clone(),
                tags: vec![
                    "temporal".to_string(),
                    "validity".to_string(),
                    "link-proof".to_string(),
                    "composition".to_string(),
                ],
                evidence_count: 3,
            },
            AutolinkMemoryInput {
                memory_id: expired_id.clone(),
                tags: vec![
                    "temporal".to_string(),
                    "validity".to_string(),
                    "link-proof".to_string(),
                    "composition".to_string(),
                ],
                evidence_count: 2,
            },
            AutolinkMemoryInput {
                memory_id: boundary_id.clone(),
                tags: vec![
                    "temporal".to_string(),
                    "validity".to_string(),
                    "link-proof".to_string(),
                    "composition".to_string(),
                    "instant-window".to_string(),
                ],
                evidence_count: 4,
            },
            AutolinkMemoryInput {
                memory_id: current_id.clone(),
                tags: vec!["link-proof".to_string(), "composition".to_string()],
                evidence_count: 1,
            },
        ],
        &[AutolinkExistingEdge {
            src_memory_id: expired_id.clone(),
            dst_memory_id: current_id.clone(),
            relation: "co_tag".to_string(),
        }],
        &AutolinkCandidateOptions {
            max_candidates: Some(5),
            ..Default::default()
        },
    );
    ensure(
        autolink_candidates
            .iter()
            .all(|candidate| candidate.src_memory_id != candidate.dst_memory_id),
        "autolink candidates must suppress self links",
    )?;
    ensure(
        autolink_candidates.iter().all(|candidate| {
            !(candidate.src_memory_id == current_id && candidate.dst_memory_id == expired_id)
        }),
        "autolink candidates must suppress duplicate existing co_tag edges",
    )?;
    let top_autolink = autolink_candidates
        .first()
        .ok_or_else(|| "expected at least one deterministic autolink candidate".to_string())?;
    ensure_equal(
        &top_autolink.relation,
        &"co_tag".to_string(),
        "top autolink relation",
    )?;

    let database_path = workspace.join(".ee").join("ee.db");
    let connection = DbConnection::open_file(&database_path).map_err(|error| error.to_string())?;
    let links_before = connection
        .list_all_memory_links(None)
        .map_err(|error| error.to_string())?;
    ensure(
        links_before.is_empty(),
        "staged suggestions and dry-run autolink candidates must not mutate memory_links",
    )?;

    let insert_link = |id: &str,
                       src: &str,
                       dst: &str,
                       relation: MemoryLinkRelation,
                       weight: f32,
                       confidence: f32,
                       directed: bool,
                       evidence_count: u32,
                       source: MemoryLinkSource|
     -> TestResult {
        connection
            .insert_memory_link(
                id,
                &CreateMemoryLinkInput {
                    src_memory_id: src.to_string(),
                    dst_memory_id: dst.to_string(),
                    relation,
                    weight,
                    confidence,
                    directed,
                    evidence_count,
                    last_reinforced_at: Some("2026-05-01T00:00:00Z".to_string()),
                    source,
                    created_by: Some("memory-temporal-links-and-graph-smoke".to_string()),
                    metadata_json: Some(
                        serde_json::json!({
                            "redaction": "checked",
                            "scenario": "temporal_links_graph"
                        })
                        .to_string(),
                    ),
                },
            )
            .map_err(|error| error.to_string())
    };
    insert_link(
        "link_00000000000000000000000001",
        &current_id,
        &expired_id,
        MemoryLinkRelation::Supports,
        0.9,
        0.88,
        true,
        3,
        MemoryLinkSource::Human,
    )?;
    insert_link(
        "link_00000000000000000000000002",
        &current_id,
        &future_id,
        MemoryLinkRelation::Contradicts,
        0.7,
        0.72,
        true,
        2,
        MemoryLinkSource::Agent,
    )?;
    insert_link(
        "link_00000000000000000000000003",
        &current_id,
        &boundary_id,
        MemoryLinkRelation::Related,
        0.6,
        0.66,
        false,
        2,
        MemoryLinkSource::Import,
    )?;
    insert_link(
        "link_00000000000000000000000004",
        &current_id,
        &unknown_id,
        MemoryLinkRelation::CoTag,
        0.8,
        0.81,
        false,
        4,
        MemoryLinkSource::Auto,
    )?;

    let links_after = connection
        .list_all_memory_links(None)
        .map_err(|error| error.to_string())?;
    ensure_equal(&links_after.len(), &4, "seeded memory link count")?;

    let show = run_ee(&[
        "--workspace",
        workspace_arg.as_str(),
        "--json",
        "memory",
        "show",
        current_id.as_str(),
    ])?;
    let show_stderr = String::from_utf8_lossy(&show.stderr);
    ensure(
        show.status.success(),
        format!("memory show should succeed; stderr: {show_stderr}"),
    )?;
    ensure(show.stderr.is_empty(), "memory show stderr clean")?;
    let show_json: serde_json::Value = serde_json::from_slice(&show.stdout)
        .map_err(|error| format!("memory show stdout must be JSON: {error}"))?;
    ensure_equal(
        &show_json["data"]["memory"]["validity_status"],
        &serde_json::json!("current"),
        "memory show validity status",
    )?;
    ensure_equal(
        &show_json["data"]["memory"]["valid_from"],
        &serde_json::json!("2020-01-01T00:00:00Z"),
        "memory show valid_from",
    )?;

    let list = run_ee(&[
        "--workspace",
        workspace_arg.as_str(),
        "--json",
        "memory",
        "list",
        "--limit",
        "10",
    ])?;
    let list_stderr = String::from_utf8_lossy(&list.stderr);
    ensure(
        list.status.success(),
        format!("memory list should succeed; stderr: {list_stderr}"),
    )?;
    ensure(list.stderr.is_empty(), "memory list stderr clean")?;
    let list_json: serde_json::Value = serde_json::from_slice(&list.stdout)
        .map_err(|error| format!("memory list stdout must be JSON: {error}"))?;
    let listed = list_json["data"]["memories"]
        .as_array()
        .ok_or_else(|| "memory list must include memories".to_string())?;
    ensure_equal(
        &listed.len(),
        &5,
        "memory list includes every temporal window",
    )?;
    ensure(
        listed.iter().any(|memory| {
            memory["id"].as_str() == Some(expired_id.as_str())
                && memory["validity_status"].as_str() == Some("expired")
        }),
        "expired memories remain visible and explainable by default",
    )?;

    let why_current = run_ee(&[
        "--workspace",
        workspace_arg.as_str(),
        "--json",
        "why",
        current_id.as_str(),
    ])?;
    let why_current_stderr = String::from_utf8_lossy(&why_current.stderr);
    ensure(
        why_current.status.success(),
        format!("why current should succeed; stderr: {why_current_stderr}"),
    )?;
    ensure(why_current.stderr.is_empty(), "why current stderr clean")?;
    let why_current_json: serde_json::Value = serde_json::from_slice(&why_current.stdout)
        .map_err(|error| format!("why current stdout must be JSON: {error}"))?;
    ensure_equal(
        &why_current_json["data"]["storage"]["validityStatus"],
        &serde_json::json!("current"),
        "why current validity status",
    )?;
    let why_links = why_current_json["data"]["links"]
        .as_array()
        .ok_or_else(|| "why links must be an array".to_string())?;
    ensure_equal(&why_links.len(), &4, "why current link count")?;
    let supports = why_links
        .iter()
        .find(|link| link["relation"] == "supports")
        .ok_or_else(|| "supports link missing".to_string())?;
    ensure_equal(
        &supports["linkId"],
        &serde_json::json!("link_00000000000000000000000001"),
        "supports link id",
    )?;
    ensure_equal(
        &supports["direction"],
        &serde_json::json!("outgoing"),
        "supports direction",
    )?;
    ensure_equal(
        &supports["confidence"],
        &serde_json::json!(0.88),
        "supports confidence",
    )?;
    ensure_equal(
        &supports["evidenceCount"],
        &serde_json::json!(3),
        "supports evidence count",
    )?;
    ensure_equal(
        &supports["source"],
        &serde_json::json!("human"),
        "supports source",
    )?;
    let contradicts = why_links
        .iter()
        .find(|link| link["relation"] == "contradicts")
        .ok_or_else(|| "contradicts link missing".to_string())?;
    ensure_equal(
        &contradicts["linkedMemoryId"],
        &serde_json::json!(future_id.as_str()),
        "contradicts target",
    )?;
    let cotag = why_links
        .iter()
        .find(|link| link["relation"] == "co_tag")
        .ok_or_else(|| "co_tag link missing".to_string())?;
    ensure_equal(
        &cotag["direction"],
        &serde_json::json!("undirected"),
        "co_tag direction",
    )?;

    let why_expired = run_ee(&[
        "--workspace",
        workspace_arg.as_str(),
        "--json",
        "why",
        expired_id.as_str(),
    ])?;
    let why_expired_stderr = String::from_utf8_lossy(&why_expired.stderr);
    ensure(
        why_expired.status.success(),
        format!("why expired should succeed; stderr: {why_expired_stderr}"),
    )?;
    ensure(why_expired.stderr.is_empty(), "why expired stderr clean")?;
    let why_expired_json: serde_json::Value = serde_json::from_slice(&why_expired.stdout)
        .map_err(|error| format!("why expired stdout must be JSON: {error}"))?;
    ensure_equal(
        &why_expired_json["data"]["storage"]["validityStatus"],
        &serde_json::json!("expired"),
        "why expired validity status",
    )?;
    ensure(
        why_expired_json["data"]["links"]
            .as_array()
            .is_some_and(|links| {
                links
                    .iter()
                    .any(|link| link["relation"] == "supports" && link["direction"] == "incoming")
            }),
        "why expired should render the incoming supports edge",
    )?;

    let database_arg = database_path.to_string_lossy().into_owned();
    let graph_args = [
        "--workspace",
        workspace_arg.as_str(),
        "--json",
        "graph",
        "centrality-refresh",
        "--dry-run",
        "--database",
        database_arg.as_str(),
    ];
    let graph_run = run_ee_logged(
        "memory-temporal-links-graph",
        &graph_args,
        &workspace,
        None,
        "4ypp-temporal-links-graph-dry-run",
        "ee.graph.centrality_refresh.v1",
        None,
    )?;
    let graph_dossier_dir = graph_run.dossier_dir.clone();
    let graph = graph_run.output;
    let graph_stdout = String::from_utf8_lossy(&graph.stdout);
    let graph_stderr = String::from_utf8_lossy(&graph.stderr);
    ensure(
        graph.status.success(),
        format!("graph centrality dry-run should succeed; stderr: {graph_stderr}"),
    )?;
    ensure(graph.stderr.is_empty(), "graph dry-run stderr clean")?;
    ensure_no_ansi(&graph_stdout, "graph dry-run stdout")?;
    let graph_json: serde_json::Value = serde_json::from_slice(&graph.stdout)
        .map_err(|error| format!("graph dry-run stdout must be JSON: {error}"))?;
    ensure_equal(
        &graph_json["schema"],
        &serde_json::json!("ee.graph.centrality_refresh.v1"),
        "graph dry-run schema",
    )?;
    let graph_status = graph_json["data"]["status"]
        .as_str()
        .ok_or_else(|| "graph status must be a string".to_string())?;
    if graph_status == "graph_feature_disabled" {
        ensure_equal(
            &graph_json["data"]["graph"]["edgeCount"],
            &serde_json::json!(0),
            "disabled graph edge count",
        )?;
    } else {
        ensure_equal(
            &graph_json["data"]["status"],
            &serde_json::json!("dry_run"),
            "graph feature dry-run status",
        )?;
        ensure_equal(
            &graph_json["data"]["graph"]["edgeCount"],
            &serde_json::json!(4),
            "graph dry-run edge count",
        )?;
    }

    write_json_artifact(
        &graph_dossier_dir.join("temporal-link-composition-summary.json"),
        &serde_json::json!({
            "bead": "eidetic_engine_cli-4ypp",
            "workspace": workspace.display().to_string(),
            "fixtureId": "4ypp-temporal-links-graph-dry-run",
            "memoryIds": {
                "current": current_id,
                "expired": expired_id,
                "future": future_id,
                "boundaryEqual": boundary_id,
                "unknown": unknown_id
            },
            "validity": {
                "current": current_json["data"]["validity_status"].clone(),
                "expired": expired_json["data"]["validity_status"].clone(),
                "future": future_json["data"]["validity_status"].clone(),
                "boundaryEqual": {
                    "status": boundary_json["data"]["validity_status"].clone(),
                    "windowKind": boundary_json["data"]["validity_window_kind"].clone(),
                    "validFrom": boundary_json["data"]["valid_from"].clone(),
                    "validTo": boundary_json["data"]["valid_to"].clone()
                },
                "unknown": unknown_json["data"]["validity_status"].clone()
            },
            "stagedSuggestion": staged_suggestions.first().cloned(),
            "autolink": {
                "broadOnlySuppressed": broad_only_candidates.is_empty(),
                "candidateCount": autolink_candidates.len(),
                "topScore": top_autolink.weight,
                "topSharedTags": top_autolink.shared_tags.clone(),
                "duplicateExistingSuppressed": true,
                "selfLinksSuppressed": true
            },
            "links": links_after.iter().map(|link| {
                serde_json::json!({
                    "id": link.id.clone(),
                    "relation": link.relation.clone(),
                    "directed": link.directed,
                    "confidence": link.confidence,
                    "weight": link.weight,
                    "evidenceCount": link.evidence_count,
                    "source": link.source.clone(),
                    "metadata": link.metadata_json.clone()
                })
            }).collect::<Vec<_>>(),
            "graph": {
                "status": graph_json["data"]["status"].clone(),
                "outputHash": format!("blake3:{}", blake3::hash(&graph.stdout).to_hex()),
                "stdoutPath": graph_dossier_dir.join("stdout").display().to_string(),
                "stderrPath": graph_dossier_dir.join("stderr").display().to_string()
            },
            "schemaGolden": {
                "schema": "ee.graph.centrality_refresh.v1",
                "status": "asserted_by_scenario"
            },
            "effects": {
                "linksBefore": links_before.len(),
                "linksAfter": links_after.len(),
                "rememberSuggestionsMutatedLinks": false
            },
            "redaction": {
                "posture": "checked",
                "secretsObservedInMachineOutput": graph_stdout.contains("sk-")
            },
            "firstFailure": graph_dossier_dir.join("first-failure.md").display().to_string()
        }),
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

#[cfg(unix)]
#[test]
fn pack_query_file_invalid_filter_operator_uses_stable_machine_error() -> TestResult {
    let workspace = unique_artifact_dir("pack-invalid-filter")?;
    fs::create_dir_all(&workspace).map_err(|error| error.to_string())?;
    let query_file = workspace.join("invalid-filter.eeq.json");
    fs::write(
        &query_file,
        r#"{
          "version": "ee.query.v1",
          "query": {"text": "prepare release"},
          "filters": {"level": {"approximately": "procedural"}}
        }"#,
    )
    .map_err(|error| error.to_string())?;
    let query_file_arg = query_file.to_string_lossy().into_owned();

    ensure_pack_query_file_machine_error(
        "pack-invalid-filter-run",
        query_file_arg.as_str(),
        "ERR_INVALID_OPERATOR",
        "invalid filter operator",
    )
}

#[cfg(unix)]
#[test]
fn pack_query_file_invalid_timestamp_uses_stable_machine_error() -> TestResult {
    let workspace = unique_artifact_dir("pack-invalid-timestamp")?;
    fs::create_dir_all(&workspace).map_err(|error| error.to_string())?;
    let query_file = workspace.join("invalid-timestamp.eeq.json");
    fs::write(
        &query_file,
        r#"{
          "version": "ee.query.v1",
          "query": {"text": "prepare release"},
          "asOf": "not-a-timestamp"
        }"#,
    )
    .map_err(|error| error.to_string())?;
    let query_file_arg = query_file.to_string_lossy().into_owned();

    ensure_pack_query_file_machine_error(
        "pack-invalid-timestamp-run",
        query_file_arg.as_str(),
        "ERR_INVALID_TIMESTAMP",
        "invalid timestamp",
    )
}

#[cfg(unix)]
#[test]
fn pack_query_file_rejects_unsafe_path_before_io() -> TestResult {
    ensure_pack_query_file_machine_error(
        "pack-unsafe-query-path",
        "../unsafe.eeq.json",
        "ERR_UNSAFE_PATH",
        "unsafe query-file path",
    )
}

#[cfg(unix)]
#[test]
fn pack_query_file_missing_path_uses_stable_machine_error() -> TestResult {
    ensure_pack_query_file_machine_error(
        "pack-missing-query-path",
        "missing.eeq.json",
        "ERR_QUERY_FILE_NOT_FOUND",
        "missing query-file path",
    )
}

#[cfg(unix)]
#[test]
fn pack_query_file_rejects_oversized_document_before_parse() -> TestResult {
    let workspace = unique_artifact_dir("pack-oversized-query")?;
    fs::create_dir_all(&workspace).map_err(|error| error.to_string())?;
    let query_file = workspace.join("oversized.eeq.json");
    fs::write(&query_file, vec![b' '; 256 * 1024 + 1]).map_err(|error| error.to_string())?;
    let query_file_arg = query_file.to_string_lossy().into_owned();

    ensure_pack_query_file_machine_error(
        "pack-oversized-query-run",
        query_file_arg.as_str(),
        "ERR_INVALID_QUERY_FILE",
        "oversized query-file",
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
    let output = run_ee(&["--fields", "full", "status", "--json"])?;
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
        "file://AGENTS.md#L164-173",
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

#[test]
fn mcp_manifest_json_real_binary_smoke() -> TestResult {
    let output = run_ee(&["--json", "mcp", "manifest"])?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    ensure(
        output.status.success(),
        format!("mcp manifest should succeed; stderr: {stderr}"),
    )?;
    ensure(
        output.stderr.is_empty(),
        "mcp manifest stderr must be empty",
    )?;
    ensure_no_ansi(&stdout, "mcp manifest stdout")?;

    let parsed: serde_json::Value = serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("mcp manifest stdout must be valid JSON: {error}"))?;
    ensure_equal(
        &parsed["schema"],
        &serde_json::json!("ee.response.v1"),
        "response schema",
    )?;
    ensure_equal(&parsed["success"], &serde_json::json!(true), "success")?;
    ensure_equal(
        &parsed["data"]["command"],
        &serde_json::json!("mcp manifest"),
        "manifest command",
    )?;
    ensure_equal(
        &parsed["data"]["schema"],
        &serde_json::json!("ee.mcp.manifest.v1"),
        "manifest schema",
    )?;
    ensure_equal(
        &parsed["data"]["protocolVersion"],
        &serde_json::json!("2024-11-05"),
        "protocol version",
    )?;
    ensure_equal(
        &parsed["data"]["registry"]["commandSource"],
        &serde_json::json!("COMMAND_MANIFEST"),
        "command registry source",
    )?;
    ensure_equal(
        &parsed["data"]["registry"]["schemaSource"],
        &serde_json::json!("public_schemas"),
        "schema registry source",
    )?;

    let tools = parsed["data"]["tools"]
        .as_array()
        .ok_or("manifest tools must be an array")?;
    ensure(
        tools
            .iter()
            .any(|tool| tool["name"] == "ee_status" && tool["command"] == "status"),
        "manifest should derive an ee_status tool from the command registry",
    )?;
    ensure(
        tools
            .iter()
            .any(|tool| tool["name"] == "ee_mcp" && tool["command"] == "mcp"),
        "manifest should include its own mcp command entry",
    )?;

    let schemas = parsed["data"]["schemas"]
        .as_array()
        .ok_or("manifest schemas must be an array")?;
    ensure(
        schemas
            .iter()
            .any(|schema| schema["id"] == "ee.response.v1"),
        "manifest should include the response envelope schema",
    )?;
    ensure(
        schemas
            .iter()
            .any(|schema| schema["id"] == "ee.mcp.manifest.v1"),
        "manifest should include its own schema registry entry",
    )?;

    let degraded = parsed["data"]["degraded"]
        .as_array()
        .ok_or("manifest degraded must be an array")?;
    if parsed["data"]["adapter"]["featureEnabled"] == serde_json::json!(false) {
        ensure(
            degraded
                .iter()
                .any(|entry| entry["code"] == "mcp_feature_disabled"),
            "default manifest should report mcp_feature_disabled degradation",
        )?;
    }

    Ok(())
}

#[test]
fn daemon_foreground_once_json_real_binary_smoke() -> TestResult {
    let output = run_ee(&[
        "--json",
        "daemon",
        "--foreground",
        "--once",
        "--interval-ms",
        "0",
        "--job",
        "health_check",
    ])?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    ensure(
        output.status.success(),
        format!("daemon foreground should succeed; stderr: {stderr}"),
    )?;
    ensure(output.stderr.is_empty(), "daemon stderr must be empty")?;
    ensure_no_ansi(&stdout, "daemon stdout")?;

    let parsed: serde_json::Value = serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("daemon stdout must be valid JSON: {error}"))?;
    ensure_equal(
        &parsed["schema"],
        &serde_json::json!("ee.response.v1"),
        "response schema",
    )?;
    ensure_equal(&parsed["success"], &serde_json::json!(true), "success")?;
    ensure_equal(
        &parsed["data"]["schema"],
        &serde_json::json!("ee.steward.daemon_foreground.v1"),
        "daemon schema",
    )?;
    ensure_equal(
        &parsed["data"]["mode"],
        &serde_json::json!("foreground"),
        "daemon mode",
    )?;
    ensure_equal(
        &parsed["data"]["daemonized"],
        &serde_json::json!(false),
        "daemonized flag",
    )?;
    ensure_equal(
        &parsed["data"]["summary"]["tickCount"],
        &serde_json::json!(1),
        "tick count",
    )?;
    ensure_equal(
        &parsed["data"]["jobTypes"][0],
        &serde_json::json!("health_check"),
        "job type",
    )
}
