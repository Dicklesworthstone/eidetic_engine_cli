//! EE-lp4p.8: ee why memory and native entity explanation conformance tests
//!
//! Validates that `ee why <memory-id> --json` output conforms to the expected
//! schema and includes complete explanation fields for storage, retrieval,
//! and selection decisions.

use ee::db::{CreateProceduralRuleInput, DbConnection, UpdateProceduralRuleLifecycleInput};
use ee::search::RuleIndexProjection;
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
    let serialized = match serde_json::to_string_pretty(value) {
        Ok(serialized) => serialized,
        Err(error) => panic!("artifact JSON serialization should not fail: {error}"),
    };
    let _ = fs::write(&path, serialized);
}

fn native_rule_command(
    workspace: &str,
    args: &[&str],
    artifact: &str,
) -> Result<serde_json::Value, String> {
    let mut command_args = args.to_vec();
    command_args.push("--json");
    let output = native_rule_output(workspace, &command_args)?;
    persist_artifact(artifact, &output);
    ensure_equal(
        &output.status.code(),
        &Some(0),
        &format!(
            "{artifact} exit (stderr: {}) (stdout: {})",
            String::from_utf8_lossy(&output.stderr),
            String::from_utf8_lossy(&output.stdout)
        ),
    )?;
    let json = stdout_json(&output)?;
    ensure_equal(&json["success"], &serde_json::json!(true), artifact)?;
    Ok(json)
}

fn native_rule_output(workspace: &str, args: &[&str]) -> Result<Output, String> {
    Command::new(env!("CARGO_BIN_EXE_ee"))
        .args(["--workspace", workspace])
        .args(args)
        .current_dir(workspace)
        .env("EE_EMBED_DOWNLOAD", "off")
        .output()
        .map_err(|error| format!("failed native rule command: {error}"))
}

#[test]
fn why_native_rule_and_result_target_use_sourceless_identity_and_feedback() -> TestResult {
    let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
    let workspace = tempdir.path().to_string_lossy().to_string();
    native_rule_command(&workspace, &["init"], "native_rule_init")?;
    let content = "Verify the release checklist before publishing a release tag.";
    let added = native_rule_command(
        &workspace,
        &[
            "rule",
            "add",
            content,
            "--trust-class",
            "agent_assertion",
            "--confidence",
            "0.55",
        ],
        "native_rule_add",
    )?;
    let rule_id = added["data"]["ruleId"]
        .as_str()
        .ok_or_else(|| "rule add must return a native ruleId".to_owned())?
        .to_owned();
    ensure(rule_id.starts_with("rule_"), "native rule ID prefix")?;

    let database = tempdir.path().join(".ee").join("ee.db");
    let connection = DbConnection::open_file(&database).map_err(|error| error.to_string())?;
    let rule = connection
        .get_procedural_rule(&rule_id)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "native rule must exist".to_owned())?;
    let workspace_id = rule.workspace_id.clone();
    // Seed distinct native counters without creating or consulting a memory
    // posterior. Why must project this row's state, including its revision.
    ensure(
        connection
            .update_procedural_rule_lifecycle(
                &rule_id,
                &UpdateProceduralRuleLifecycleInput {
                    workspace_id: workspace_id.clone(),
                    maturity: "candidate".to_owned(),
                    confidence: rule.confidence,
                    utility: rule.utility,
                    positive_feedback_delta: 3,
                    negative_feedback_delta: 2,
                    validation_passes_delta: 1,
                    validation_contradictions_delta: 1,
                    last_validated_at: Some("2026-09-25T12:00:00Z".to_owned()),
                    superseded_by: None,
                    updated_at: "2026-09-25T12:00:00Z".to_owned(),
                },
            )
            .map_err(|error| error.to_string())?,
        "native lifecycle fixture updated",
    )?;
    let rule = connection
        .get_procedural_rule(&rule_id)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "updated native rule must exist".to_owned())?;
    let tags = connection
        .get_rule_tags(&rule_id)
        .map_err(|error| error.to_string())?;
    let sources = connection
        .get_rule_source_memory_ids(&rule_id)
        .map_err(|error| error.to_string())?;
    ensure(sources.is_empty(), "candidate rule is sourceless")?;
    let projection = RuleIndexProjection::new(rule, tempdir.path(), tags, sources);
    let revision = projection.entity_revision().to_owned();
    connection.close().map_err(|error| error.to_string())?;

    for (target, artifact) in [
        (rule_id.clone(), "native_rule_direct_why"),
        (format!("result:{rule_id}"), "native_rule_result_why"),
    ] {
        let why = native_rule_command(&workspace, &["why", &target], artifact)?;
        ensure_equal(&why["data"]["found"], &serde_json::json!(true), artifact)?;
        ensure_equal(
            &why["data"]["entity"]["kind"],
            &serde_json::json!("rule"),
            "native why entity kind",
        )?;
        ensure_equal(
            &why["data"]["entity"]["id"],
            &serde_json::json!(&rule_id),
            "native why entity ID",
        )?;
        ensure_equal(
            &why["data"]["entity"]["revision"],
            &serde_json::json!(&revision),
            "native why canonical revision",
        )?;
        ensure(
            why["data"]["memoryId"].is_null(),
            "no memory identity alias",
        )?;
        ensure_equal(
            &why["data"]["content"],
            &serde_json::json!(content),
            "why uses the rule body",
        )?;
        let details = &why["data"]["entity"]["details"];
        ensure_equal(
            &details["feedback"],
            &serde_json::json!({
                "target": {"kind": "rule", "id": &rule_id},
                "positiveCount": 3,
                "negativeCount": 2,
                "validationPasses": 1,
                "validationContradictions": 1,
                "lastAppliedAt": null,
                "lastValidatedAt": "2026-09-25T12:00:00Z",
            }),
            "native rule feedback counters",
        )?;
        ensure_equal(
            &details["provenance"]["status"],
            &serde_json::json!("unlinked"),
            "sourceless provenance posture",
        )?;
        ensure_equal(
            &details["provenance"]["sourceMemories"],
            &serde_json::json!([]),
            "no fabricated source memory",
        )?;
        ensure(
            why["data"]["selection"]["latestPackSelection"].is_null(),
            "unpacked native rule has no pack selection",
        )?;
    }

    for format in ["human", "markdown"] {
        let output = native_rule_output(&workspace, &["--format", format, "why", &rule_id])?;
        ensure_equal(
            &output.status.code(),
            &Some(0),
            &format!(
                "typed text why exit; stdout: {}; stderr: {}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            ),
        )?;
        let text = String::from_utf8_lossy(&output.stdout);
        ensure(
            text.starts_with(&format!("Rule: {rule_id}\n")),
            "text why names a rule",
        )?;
        ensure(text.contains(&revision), "text why carries native revision")?;
        ensure(
            text.contains("\"positiveCount\": 3") && text.contains("\"scope\":"),
            "text why preserves rule feedback and scope",
        )?;
    }
    let mermaid = native_rule_output(&workspace, &["--format", "mermaid", "why", &rule_id])?;
    ensure_equal(
        &mermaid.status.code(),
        &Some(0),
        &format!(
            "typed Mermaid why exit; stdout: {}; stderr: {}",
            String::from_utf8_lossy(&mermaid.stdout),
            String::from_utf8_lossy(&mermaid.stderr)
        ),
    )?;
    ensure(
        String::from_utf8_lossy(&mermaid.stdout).contains(&format!("rule: {rule_id}")),
        "Mermaid labels native rule identity",
    )?;
    let unsupported = native_rule_output(
        &workspace,
        &["why", &rule_id, "--include-sentinel", "--json"],
    )?;
    // A deliberate usage refusal (handle_why): exit 1 is the project's usage
    // code (README exit-code table; ProcessExitCode::Usage). Exit 2 is a
    // configuration error. Pin the envelope code too, so another exit-1
    // failure cannot pass as this refusal.
    ensure_equal(
        &unsupported.status.code(),
        &Some(1),
        &format!(
            "rule rejects memory sentinel options; stdout: {}; stderr: {}",
            String::from_utf8_lossy(&unsupported.stdout),
            String::from_utf8_lossy(&unsupported.stderr)
        ),
    )?;
    ensure_equal(
        &stdout_json(&unsupported)?["error"]["code"].as_str(),
        &Some("usage"),
        "sentinel refusal is a usage error",
    )?;

    let foreign = tempfile::tempdir().map_err(|error| error.to_string())?;
    let foreign_workspace = foreign.path().to_string_lossy().to_string();
    native_rule_command(&foreign_workspace, &["init"], "native_rule_foreign_init")?;
    let database_text = database.to_string_lossy().to_string();
    let wrong_workspace = native_rule_output(
        &foreign_workspace,
        &["why", &rule_id, "--database", &database_text, "--json"],
    )?;
    // A rule outside the requesting workspace is withheld as not found
    // (DomainError::NotFound => exit 1, code "not_found"), exactly like any
    // other `why` miss, so a foreign workspace cannot probe rule existence.
    // Exit 3 would claim a storage failure, and there is none.
    ensure_equal(
        &wrong_workspace.status.code(),
        &Some(1),
        &format!(
            "explicit database does not override native workspace admission; stdout: {}; stderr: {}",
            String::from_utf8_lossy(&wrong_workspace.stdout),
            String::from_utf8_lossy(&wrong_workspace.stderr)
        ),
    )?;
    ensure_equal(
        &stdout_json(&wrong_workspace)?["error"]["code"].as_str(),
        &Some("not_found"),
        "foreign workspace sees not_found",
    )?;
    ensure(
        !String::from_utf8_lossy(&wrong_workspace.stdout).contains(content),
        "wrong workspace cannot expose rule body",
    )?;

    let connection =
        DbConnection::open_file_read_only(&database).map_err(|error| error.to_string())?;
    ensure(
        connection
            .list_memories(&workspace_id, None, true)
            .map_err(|error| error.to_string())?
            .is_empty(),
        "rule add and why never synthesize memory rows",
    )?;
    connection.close().map_err(|error| error.to_string())
}

#[test]
fn why_native_rule_does_not_inherit_source_memory_pack_selection() -> TestResult {
    let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
    let workspace = tempdir.path().to_string_lossy().to_string();
    native_rule_command(&workspace, &["init"], "native_linked_rule_init")?;
    native_rule_command(
        &workspace,
        &["config", "set", "memory.include_global", "false"],
        "native_linked_rule_disable_global_recall",
    )?;
    native_rule_command(
        &workspace,
        &["config", "set", "memory.participate", "false"],
        "native_linked_rule_disable_global_writes",
    )?;
    let memory = native_rule_command(
        &workspace,
        &[
            "remember",
            "Release checklist observation: formatting prevented a broken release tag.",
            "--level",
            "episodic",
            "--kind",
            "fact",
        ],
        "native_linked_rule_remember",
    )?;
    let memory_id = memory["data"]["memory_id"]
        .as_str()
        .ok_or_else(|| "remember must return memory_id".to_owned())?;
    native_rule_command(
        &workspace,
        &["index", "rebuild"],
        "native_linked_rule_index",
    )?;
    native_rule_command(
        &workspace,
        &[
            "pack",
            "release checklist formatting",
            "--source-mode",
            "lexical_only",
            "--max-tokens",
            "4000",
        ],
        "native_linked_rule_pack",
    )?;
    let memory_why = native_rule_command(
        &workspace,
        &["why", memory_id],
        "native_linked_rule_memory_why",
    )?;
    ensure(
        memory_why["data"]["selection"]["latestPackSelection"].is_object(),
        "source memory must really have a recorded pack selection",
    )?;

    let rule_content = "Always run the release formatter before tagging the release.";
    let added = native_rule_command(
        &workspace,
        &["rule", "add", rule_content, "--source-memory", memory_id],
        "native_linked_rule_add",
    )?;
    let rule_id = added["data"]["ruleId"]
        .as_str()
        .ok_or_else(|| "rule add must return ruleId".to_owned())?;
    let why = native_rule_command(
        &workspace,
        &["why", &format!("result:{rule_id}")],
        "native_linked_rule_why",
    )?;
    ensure_equal(
        &why["data"]["entity"]["id"],
        &serde_json::json!(rule_id),
        "linked rule preserves native identity",
    )?;
    ensure_equal(
        &why["data"]["content"],
        &serde_json::json!(rule_content),
        "linked rule uses its own body",
    )?;
    ensure(
        why["data"]["memoryId"].is_null(),
        "source is not an identity alias",
    )?;
    ensure(
        why["data"]["selection"]["latestPackSelection"].is_null(),
        "source memory pack selection is not rule pack selection",
    )?;
    ensure_equal(
        &why["data"]["entity"]["details"]["provenance"]["sourceMemories"],
        &serde_json::json!([{"id": memory_id, "uri": format!("ee://memory/{memory_id}")}]),
        "source memory is represented only as provenance",
    )
}

#[test]
fn why_native_rule_redacts_historical_secret_and_path_without_mutation() -> TestResult {
    let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
    let workspace = tempdir.path().to_string_lossy().to_string();
    native_rule_command(&workspace, &["init"], "native_rule_redaction_init")?;
    let database = tempdir.path().join(".ee").join("ee.db");
    let connection = DbConnection::open_file(&database).map_err(|error| error.to_string())?;
    let workspace_id = connection
        .get_workspace_by_path(&workspace)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "workspace must exist".to_owned())?
        .id;
    let rule_id = "rule_00000000000000000000000044";
    let content = concat!(
        "Review release guidance in /Users/alice/private/release.txt with ",
        "api",
        "_key=why-rule-secret-fixture before publishing."
    );
    // Model historical source data directly: public creation policy may reject
    // secrets, but explanation egress must still protect existing durable rows.
    connection
        .insert_procedural_rule(
            rule_id,
            &CreateProceduralRuleInput {
                workspace_id: workspace_id.clone(),
                content: content.to_owned(),
                confidence: 0.5,
                utility: 0.5,
                importance: 0.5,
                trust_class: "agent_assertion".to_owned(),
                scope: "workspace".to_owned(),
                scope_pattern: None,
                maturity: "candidate".to_owned(),
                protected: false,
                source_memory_ids: vec![],
                tags: vec![],
            },
        )
        .map_err(|error| error.to_string())?;
    let before = connection
        .get_procedural_rule(rule_id)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "historical native rule must exist".to_owned())?;
    let projection = RuleIndexProjection::new(before.clone(), tempdir.path(), vec![], vec![]);
    connection.close().map_err(|error| error.to_string())?;

    let why = native_rule_command(&workspace, &["why", rule_id], "native_rule_redaction_why")?;
    ensure_equal(
        &why["data"]["found"],
        &serde_json::json!(true),
        "rule found",
    )?;
    ensure_equal(
        &why["data"]["entity"]["revision"],
        &serde_json::json!(projection.entity_revision()),
        "revision binds original rule state, not its redacted rendering",
    )?;
    let public_json = why.to_string();
    ensure(
        !public_json.contains("/Users/alice") && !public_json.contains("why-rule-secret-fixture"),
        "native why must not emit stored secret or private path",
    )?;
    let public_content = why["data"]["content"]
        .as_str()
        .ok_or_else(|| "native rule content must be a string".to_owned())?;
    ensure(
        public_content.contains("[REDACTED:"),
        "native why replaces sensitive content with a redaction placeholder",
    )?;
    ensure_equal(
        &why["data"]["entity"]["details"]["redaction"]["egressRedacted"],
        &serde_json::json!(true),
        "native why reports egress redaction",
    )?;

    let connection =
        DbConnection::open_file_read_only(&database).map_err(|error| error.to_string())?;
    let after = connection
        .get_procedural_rule(rule_id)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "why must preserve historical rule".to_owned())?;
    ensure_equal(&after, &before, "why does not rewrite native rule source")?;
    ensure(
        connection
            .list_memories(&workspace_id, None, true)
            .map_err(|error| error.to_string())?
            .is_empty(),
        "why redaction does not synthesize memory rows",
    )?;
    connection.close().map_err(|error| error.to_string())
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
        "pack",
        "schema test",
        "--max-tokens",
        "4000",
        "--json",
    ])?;
    // CAPTURE THE DIAGNOSIS. THE PREDICATE IS UNCHANGED.
    //
    // `ee` maps Outcome::Cancelled to exit 130 (src/core/outcome.rs:158) and the
    // reason IS available on the error surface -- cancel_message has production
    // references including core/context.rs:561 and :1406, on this very path.
    // This assertion reported only the code, so seven rows failed as a bare
    // "context exit: expected Some(0), got Some(130)" with the reason discarded
    // at the assertion boundary, and six of the seven never persisted the
    // context output either (bd-2bdos).
    //
    // Still compared against &Some(0). Only the failure LABEL changed, so these
    // rows must keep failing after this edit -- a green here would mean the
    // expectation moved, which is the opposite of the intent.
    ensure_equal(
        &context.status.code(),
        &Some(0),
        &format!(
            "context exit (stderr: {}) (stdout head: {})",
            String::from_utf8_lossy(&context.stderr).trim(),
            String::from_utf8_lossy(&context.stdout)
                .chars()
                .take(300)
                .collect::<String>()
        ),
    )?;

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
        &serde_json::json!("ee.response.v2"),
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
fn why_result_target_non_memory_doc_id_explains_unsupported_source() -> TestResult {
    let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
    let workspace = tempdir.path().to_string_lossy().to_string();
    let doc_id = "sess_00000000000000000000000001";
    let target = format!("result:{doc_id}");

    let init = run_ee(&["--workspace", &workspace, "init", "--json"])?;
    ensure_equal(&init.status.code(), &Some(0), "init exit")?;

    let why = run_ee(&["--workspace", &workspace, "why", &target, "--json"])?;
    persist_artifact("result_target_session_why", &why);
    let why_stderr = String::from_utf8_lossy(&why.stderr);
    ensure_equal(
        &why.status.code(),
        &Some(0),
        &format!("why exit (stderr: {why_stderr})"),
    )?;

    let why_json = stdout_json(&why)?;
    persist_json_artifact("result_target_session_why", &why_json);

    ensure_equal(
        &why_json["schema"],
        &serde_json::json!("ee.response.v2"),
        "response schema",
    )?;
    ensure_equal(
        &why_json["data"]["memoryId"],
        &serde_json::json!(doc_id),
        "document id preserved",
    )?;
    ensure_equal(
        &why_json["data"]["found"],
        &serde_json::json!(true),
        "unsupported source is renderable instead of not_found",
    )?;
    ensure_equal(
        &why_json["data"]["retrieval"]["kind"],
        &serde_json::json!("session"),
        "session source kind",
    )?;
    ensure(
        why_json["data"]["degraded"]
            .as_array()
            .is_some_and(|items| {
                items
                    .iter()
                    .any(|item| item["code"] == "why_result_target_unsupported_source")
            }),
        "unsupported source degradation must be present",
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
        "file:///tests/fixtures/storage_test.json#L42",
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
        "pack",
        "storage test",
        "--max-tokens",
        "4000",
        "--json",
    ])?;
    // CAPTURE THE DIAGNOSIS. THE PREDICATE IS UNCHANGED.
    //
    // `ee` maps Outcome::Cancelled to exit 130 (src/core/outcome.rs:158) and the
    // reason IS available on the error surface -- cancel_message has production
    // references including core/context.rs:561 and :1406, on this very path.
    // This assertion reported only the code, so seven rows failed as a bare
    // "context exit: expected Some(0), got Some(130)" with the reason discarded
    // at the assertion boundary, and six of the seven never persisted the
    // context output either (bd-2bdos).
    //
    // Still compared against &Some(0). Only the failure LABEL changed, so these
    // rows must keep failing after this edit -- a green here would mean the
    // expectation moved, which is the opposite of the intent.
    ensure_equal(
        &context.status.code(),
        &Some(0),
        &format!(
            "context exit (stderr: {}) (stdout head: {})",
            String::from_utf8_lossy(&context.stderr).trim(),
            String::from_utf8_lossy(&context.stdout)
                .chars()
                .take(300)
                .collect::<String>()
        ),
    )?;

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
        &serde_json::json!("file:///tests/fixtures/storage_test.json#L42"),
        "storage provenanceUri",
    )?;
    // Storage section should contain memory metadata; exact fields may vary
    ensure(
        storage.as_object().is_some_and(|obj| !obj.is_empty()),
        "storage section must contain fields",
    )
}

#[test]
fn why_history_section_includes_audit_timeline() -> TestResult {
    let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
    let workspace = tempdir.path().to_string_lossy().to_string();

    let init = run_ee(&["--workspace", &workspace, "init", "--json"])?;
    ensure_equal(&init.status.code(), &Some(0), "init exit")?;

    let remember = run_ee(&[
        "--workspace",
        &workspace,
        "remember",
        "History section test memory.",
        "--level",
        "procedural",
        "--kind",
        "rule",
        "--json",
    ])?;
    persist_artifact("history_remember", &remember);
    ensure_equal(&remember.status.code(), &Some(0), "remember exit")?;
    let remember_json = stdout_json(&remember)?;
    let memory_id = remember_json["data"]["memory_id"]
        .as_str()
        .ok_or_else(|| "memory_id must be a string".to_string())?;

    let why = run_ee(&["--workspace", &workspace, "why", memory_id, "--json"])?;
    persist_artifact("history_why", &why);
    ensure_equal(&why.status.code(), &Some(0), "why exit")?;

    let why_json = stdout_json(&why)?;
    persist_json_artifact("history_why", &why_json);
    let history = &why_json["data"]["history"];
    ensure(history.is_object(), "history section must exist")?;
    ensure(
        history["totalCount"]
            .as_u64()
            .is_some_and(|count| count >= 1),
        "history.totalCount must include remember audit entry",
    )?;
    let entries = history["entries"]
        .as_array()
        .ok_or_else(|| "history.entries must be an array".to_string())?;
    ensure(!entries.is_empty(), "history.entries must not be empty")?;
    ensure(
        entries[0]["auditId"]
            .as_str()
            .is_some_and(|id| id.starts_with("audit_")),
        "history entry auditId must have audit_ prefix",
    )?;
    ensure(
        entries[0]["action"].as_str().is_some(),
        "history entry action must be a string",
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
        "pack",
        "retrieval test",
        "--max-tokens",
        "4000",
        "--json",
    ])?;
    // CAPTURE THE DIAGNOSIS. THE PREDICATE IS UNCHANGED.
    //
    // `ee` maps Outcome::Cancelled to exit 130 (src/core/outcome.rs:158) and the
    // reason IS available on the error surface -- cancel_message has production
    // references including core/context.rs:561 and :1406, on this very path.
    // This assertion reported only the code, so seven rows failed as a bare
    // "context exit: expected Some(0), got Some(130)" with the reason discarded
    // at the assertion boundary, and six of the seven never persisted the
    // context output either (bd-2bdos).
    //
    // Still compared against &Some(0). Only the failure LABEL changed, so these
    // rows must keep failing after this edit -- a green here would mean the
    // expectation moved, which is the opposite of the intent.
    ensure_equal(
        &context.status.code(),
        &Some(0),
        &format!(
            "context exit (stderr: {}) (stdout head: {})",
            String::from_utf8_lossy(&context.stderr).trim(),
            String::from_utf8_lossy(&context.stdout)
                .chars()
                .take(300)
                .collect::<String>()
        ),
    )?;

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
        "pack",
        "selection test",
        "--max-tokens",
        "4000",
        "--json",
    ])?;
    // CAPTURE THE DIAGNOSIS. THE PREDICATE IS UNCHANGED.
    //
    // `ee` maps Outcome::Cancelled to exit 130 (src/core/outcome.rs:158) and the
    // reason IS available on the error surface -- cancel_message has production
    // references including core/context.rs:561 and :1406, on this very path.
    // This assertion reported only the code, so seven rows failed as a bare
    // "context exit: expected Some(0), got Some(130)" with the reason discarded
    // at the assertion boundary, and six of the seven never persisted the
    // context output either (bd-2bdos).
    //
    // Still compared against &Some(0). Only the failure LABEL changed, so these
    // rows must keep failing after this edit -- a green here would mean the
    // expectation moved, which is the opposite of the intent.
    ensure_equal(
        &context.status.code(),
        &Some(0),
        &format!(
            "context exit (stderr: {}) (stdout head: {})",
            String::from_utf8_lossy(&context.stderr).trim(),
            String::from_utf8_lossy(&context.stdout)
                .chars()
                .take(300)
                .collect::<String>()
        ),
    )?;

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
        "pack",
        "pack reference test",
        "--max-tokens",
        "4000",
        "--json",
    ])?;
    persist_artifact("pack_ref_context", &context);
    // CAPTURE THE DIAGNOSIS. THE PREDICATE IS UNCHANGED.
    //
    // `ee` maps Outcome::Cancelled to exit 130 (src/core/outcome.rs:158) and the
    // reason IS available on the error surface -- cancel_message has production
    // references including core/context.rs:561 and :1406, on this very path.
    // This assertion reported only the code, so seven rows failed as a bare
    // "context exit: expected Some(0), got Some(130)" with the reason discarded
    // at the assertion boundary, and six of the seven never persisted the
    // context output either (bd-2bdos).
    //
    // Still compared against &Some(0). Only the failure LABEL changed, so these
    // rows must keep failing after this edit -- a green here would mean the
    // expectation moved, which is the opposite of the intent.
    ensure_equal(
        &context.status.code(),
        &Some(0),
        &format!(
            "context exit (stderr: {}) (stdout head: {})",
            String::from_utf8_lossy(&context.stderr).trim(),
            String::from_utf8_lossy(&context.stdout)
                .chars()
                .take(300)
                .collect::<String>()
        ),
    )?;
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
        &serde_json::json!("ee.error.v2"),
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
        why_json["schema"].as_str() == Some("ee.error.v2")
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
        "pack",
        "completeness test",
        "--max-tokens",
        "8000",
        "--json",
    ])?;
    // CAPTURE THE DIAGNOSIS. THE PREDICATE IS UNCHANGED.
    //
    // `ee` maps Outcome::Cancelled to exit 130 (src/core/outcome.rs:158) and the
    // reason IS available on the error surface -- cancel_message has production
    // references including core/context.rs:561 and :1406, on this very path.
    // This assertion reported only the code, so seven rows failed as a bare
    // "context exit: expected Some(0), got Some(130)" with the reason discarded
    // at the assertion boundary, and six of the seven never persisted the
    // context output either (bd-2bdos).
    //
    // Still compared against &Some(0). Only the failure LABEL changed, so these
    // rows must keep failing after this edit -- a green here would mean the
    // expectation moved, which is the opposite of the intent.
    ensure_equal(
        &context.status.code(),
        &Some(0),
        &format!(
            "context exit (stderr: {}) (stdout head: {})",
            String::from_utf8_lossy(&context.stderr).trim(),
            String::from_utf8_lossy(&context.stdout)
                .chars()
                .take(300)
                .collect::<String>()
        ),
    )?;

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
        "pack",
        "kinds test",
        "--max-tokens",
        "8000",
        "--json",
    ])?;
    // CAPTURE THE DIAGNOSIS. THE PREDICATE IS UNCHANGED.
    //
    // `ee` maps Outcome::Cancelled to exit 130 (src/core/outcome.rs:158) and the
    // reason IS available on the error surface -- cancel_message has production
    // references including core/context.rs:561 and :1406, on this very path.
    // This assertion reported only the code, so seven rows failed as a bare
    // "context exit: expected Some(0), got Some(130)" with the reason discarded
    // at the assertion boundary, and six of the seven never persisted the
    // context output either (bd-2bdos).
    //
    // Still compared against &Some(0). Only the failure LABEL changed, so these
    // rows must keep failing after this edit -- a green here would mean the
    // expectation moved, which is the opposite of the intent.
    ensure_equal(
        &context.status.code(),
        &Some(0),
        &format!(
            "context exit (stderr: {}) (stdout head: {})",
            String::from_utf8_lossy(&context.stderr).trim(),
            String::from_utf8_lossy(&context.stdout)
                .chars()
                .take(300)
                .collect::<String>()
        ),
    )?;

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
        "pack",
        "output test",
        "--max-tokens",
        "4000",
        "--json",
    ])?;
    // CAPTURE THE DIAGNOSIS. THE PREDICATE IS UNCHANGED.
    //
    // `ee` maps Outcome::Cancelled to exit 130 (src/core/outcome.rs:158) and the
    // reason IS available on the error surface -- cancel_message has production
    // references including core/context.rs:561 and :1406, on this very path.
    // This assertion reported only the code, so seven rows failed as a bare
    // "context exit: expected Some(0), got Some(130)" with the reason discarded
    // at the assertion boundary, and six of the seven never persisted the
    // context output either (bd-2bdos).
    //
    // Still compared against &Some(0). Only the failure LABEL changed, so these
    // rows must keep failing after this edit -- a green here would mean the
    // expectation moved, which is the opposite of the intent.
    ensure_equal(
        &context.status.code(),
        &Some(0),
        &format!(
            "context exit (stderr: {}) (stdout head: {})",
            String::from_utf8_lossy(&context.stderr).trim(),
            String::from_utf8_lossy(&context.stdout)
                .chars()
                .take(300)
                .collect::<String>()
        ),
    )?;

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
        "pack",
        "human mode test",
        "--max-tokens",
        "4000",
        "--json",
    ])?;
    // CAPTURE THE DIAGNOSIS. THE PREDICATE IS UNCHANGED.
    //
    // `ee` maps Outcome::Cancelled to exit 130 (src/core/outcome.rs:158) and the
    // reason IS available on the error surface -- cancel_message has production
    // references including core/context.rs:561 and :1406, on this very path.
    // This assertion reported only the code, so seven rows failed as a bare
    // "context exit: expected Some(0), got Some(130)" with the reason discarded
    // at the assertion boundary, and six of the seven never persisted the
    // context output either (bd-2bdos).
    //
    // Still compared against &Some(0). Only the failure LABEL changed, so these
    // rows must keep failing after this edit -- a green here would mean the
    // expectation moved, which is the opposite of the intent.
    ensure_equal(
        &context.status.code(),
        &Some(0),
        &format!(
            "context exit (stderr: {}) (stdout head: {})",
            String::from_utf8_lossy(&context.stderr).trim(),
            String::from_utf8_lossy(&context.stdout)
                .chars()
                .take(300)
                .collect::<String>()
        ),
    )?;

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
