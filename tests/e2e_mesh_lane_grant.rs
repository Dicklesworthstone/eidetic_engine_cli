//! Public-CLI E2E coverage for authenticated mesh lane grants.
//!
//! The tests use the real `ee` binary and a real temporary FrankenSQLite
//! workspace. Approval bearers cross the process boundary only through the
//! explicit preview JSON projection and bounded stdin; assertions never echo a
//! bearer into failure messages.

use std::fmt::Debug;
use std::fs;
use std::io::Write as _;
use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Barrier};

use ee::config::MeshLane;
use ee::db::{
    CreateAuditInput, DbConnection, MeshLaneGrantAtomicError, MeshLaneGrantMutationError,
    MeshLaneGrantMutationInput, MeshLaneGrantTargetAdapter,
};
use ee::mesh::lane_grant::{APPROVAL_TOKEN_TTL_SECONDS, compare_snapshot, issue, verify_authentic};
use ee::policy::store_auth::{StoreAuthRoot, workspace_keys_dir};
use serde_json::Value;

type TestResult = Result<(), String>;

const TEST_LANE_ARG: &str = "graph-link";
const TEST_LANE_WIRE: &str = "graph_link";
const GRANT_SCHEMA: &str = "ee.mesh.grant.v1";
const GRANT_AUDIT_ACTION: &str = "mesh.audit.lane_grant";
const REVOKE_AUDIT_ACTION: &str = "mesh.audit.lane_revoke";

struct LaneGrantFixture {
    _tempdir: tempfile::TempDir,
    workspace: String,
    workspace_id: String,
    peer_id: String,
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

fn ee_command(workspace: &str, args: &[&str]) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_ee"));
    command
        .arg("--workspace")
        .arg(workspace)
        .args(args)
        .env("EE_EMBED_DOWNLOAD", "off")
        .env("EE_MESH_ENABLED", "1")
        .env("EE_MESH_MODE", "cache")
        .env("TMPDIR", "/tmp")
        .env_remove("EE_DATABASE_PATH")
        .env_remove("EE_WORKSPACE")
        .env_remove("EE_WORKSPACE_REGISTRY");
    command
}

fn run_ee(workspace: &str, args: &[&str]) -> Result<Output, String> {
    ee_command(workspace, args)
        .output()
        .map_err(|error| format!("failed to run ee {}: {error}", args.join(" ")))
}

fn run_ee_with_stdin(workspace: &str, args: &[&str], stdin: &[u8]) -> Result<Output, String> {
    let mut command = ee_command(workspace, args);
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command
        .spawn()
        .map_err(|error| format!("failed to spawn ee {}: {error}", args.join(" ")))?;
    let mut child_stdin = child
        .stdin
        .take()
        .ok_or_else(|| format!("ee {} did not expose piped stdin", args.join(" ")))?;
    child_stdin
        .write_all(stdin)
        .map_err(|error| format!("failed to write bounded ee stdin: {error}"))?;
    drop(child_stdin);
    child
        .wait_with_output()
        .map_err(|error| format!("failed to wait for ee {}: {error}", args.join(" ")))
}

fn redacted_output(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    let mut output = String::with_capacity(text.len());
    let mut cursor = 0;
    while let Some(relative_start) = text[cursor..].find("eeap1_") {
        let start = cursor + relative_start;
        output.push_str(&text[cursor..start]);
        output.push_str("<redacted-approval-bearer>");
        let mut end = start;
        while text
            .as_bytes()
            .get(end)
            .is_some_and(|byte| byte.is_ascii_alphanumeric() || matches!(*byte, b'_' | b'-'))
        {
            end += 1;
        }
        cursor = end;
    }
    output.push_str(&text[cursor..]);
    output
}

fn stdout_json(output: &Output, label: &str) -> Result<Value, String> {
    let stdout = String::from_utf8(output.stdout.clone())
        .map_err(|error| format!("{label}: stdout was not UTF-8: {error}"))?;
    serde_json::from_str(&stdout).map_err(|error| {
        format!(
            "{label}: stdout was not JSON: {error}\nstdout:\n{}",
            redacted_output(&output.stdout)
        )
    })
}

fn ensure_json_stderr_empty(output: &Output, label: &str) -> TestResult {
    let stderr = String::from_utf8_lossy(&output.stderr);
    ensure(
        stderr.trim().is_empty(),
        format!(
            "{label}: JSON mode stderr was not empty:\n{}",
            redacted_output(&output.stderr)
        ),
    )
}

fn success_json(output: &Output, label: &str) -> Result<Value, String> {
    if !output.status.success() {
        return Err(format!(
            "{label} failed with exit {:?}\nstdout:\n{}\nstderr:\n{}",
            output.status.code(),
            redacted_output(&output.stdout),
            redacted_output(&output.stderr),
        ));
    }
    ensure_json_stderr_empty(output, label)?;
    let value = stdout_json(output, label)?;
    ensure_equal(
        &value.pointer("/schema").and_then(Value::as_str),
        &Some("ee.response.v2"),
        &format!("{label} response schema"),
    )?;
    ensure_equal(
        &value.pointer("/success").and_then(Value::as_bool),
        &Some(true),
        &format!("{label} success flag"),
    )?;
    Ok(value)
}

fn json_string(value: &Value, pointer: &str, label: &str) -> Result<String, String> {
    value
        .pointer(pointer)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| format!("{label}: {pointer} must be a string: {value}"))
}

fn sensitive_json_string(value: &Value, pointer: &str, label: &str) -> Result<String, String> {
    value
        .pointer(pointer)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| format!("{label}: {pointer} must contain the sensitive bearer projection"))
}

fn json_u64(value: &Value, pointer: &str, label: &str) -> Result<u64, String> {
    value
        .pointer(pointer)
        .and_then(Value::as_u64)
        .ok_or_else(|| format!("{label}: {pointer} must be an unsigned integer: {value}"))
}

fn assert_no_bearer(text: &[u8], label: &str) -> TestResult {
    let text = String::from_utf8_lossy(text);
    ensure(
        !text.contains("eeap1_") && !text.contains("approvalToken"),
        format!("{label} exposed an approval bearer or bearer field"),
    )
}

fn write_mesh_policy_config(
    workspace: &Path,
    workspace_id: &str,
    peer_id: &str,
    include_peer_group_binding: bool,
) -> TestResult {
    let group_binding = if include_peer_group_binding {
        format!(
            r#"
[[mesh.peer_group_bindings]]
workspace_id = "{workspace_id}"
workspace_alias = "lane-grant-e2e"
peer_group_id = "pg_lane_grant_e2e"
peer_group_label = "lane-grant-e2e"
peer_ids = ["{peer_id}"]
origin_workspace_ids = ["{workspace_id}"]
default_action = "deny"

[mesh.peer_group_bindings.lanes]
metadata = "allow"
body = "deny"
embedding = "deny"
graph_link = "deny"
revision_notice = "allow"
curation_signal = "deny"
"#,
        )
    } else {
        String::new()
    };
    let config = format!(
        r#"[mesh]
enabled = true
command_mode = "cache"
{group_binding}

[[mesh.peer_policies]]
policy_id = "pol_lane_grant_e2e"
workspace_id = "{workspace_id}"
workspace_alias = "lane-grant-e2e"
peer_id = "{peer_id}"
peer_alias = "lane-grant-peer"
origin_workspace_ids = ["{workspace_id}"]
trust_lane = "peerAgent"
import_trust_class = "agent_assertion"
default_action = "deny"

[mesh.peer_policies.allowed_lanes]
metadata = "allow"
body = "deny"
embedding = "deny"
graph_link = "deny"
revision_notice = "allow"
curation_signal = "deny"

[mesh.peer_policies.redaction]
metadata = "share"
preview = "redact"
body = "share"
embedding = "deny"

[mesh.peer_policies.body_fetch]
allowed = false
requires_consent = true
max_bytes = 0
"#,
    );
    let path = workspace.join(".ee").join("config.toml");
    fs::write(&path, config).map_err(|error| format!("failed to write {}: {error}", path.display()))
}

fn set_up_fixture(label: &str) -> Result<LaneGrantFixture, String> {
    let tempdir = tempfile::Builder::new()
        .prefix(&format!("ee-mesh-lane-grant-{label}-"))
        .tempdir_in("/tmp")
        .map_err(|error| format!("failed to create lane-grant temp workspace: {error}"))?;
    let workspace = tempdir.path().to_string_lossy().into_owned();

    let init = run_ee(&workspace, &["init", "--skip-boilerplate", "--json"])?;
    success_json(&init, "ee init")?;

    let mesh_init = run_ee(&workspace, &["mesh", "init", "--json"])?;
    let mesh_init_json = success_json(&mesh_init, "ee mesh init")?;
    let workspace_id = json_string(
        &mesh_init_json,
        "/data/workspaceId",
        "ee mesh init workspace",
    )?;

    let peer_add = run_ee(
        &workspace,
        &[
            "mesh",
            "peer",
            "add",
            "--alias",
            "lane-grant-peer",
            "--tailscale-node-key",
            "nodekey:lane-grant-e2e",
            "--endpoint",
            "100.64.20.2:4747",
            "--tailnet-id",
            "tn_lane_grant_e2e",
            "--profile",
            "metadata-only",
            "--public-key-fingerprint",
            "blake3:lane-grant-e2e",
            "--responder-capability",
            "mesh:metadata",
            "--yes",
            "--json",
        ],
    )?;
    let peer_json = success_json(&peer_add, "ee mesh peer add")?;
    let peer_id = json_string(&peer_json, "/data/peerId", "ee mesh peer add")?;

    write_mesh_policy_config(tempdir.path(), &workspace_id, &peer_id, true)?;

    let remember = run_ee(
        &workspace,
        &[
            "remember",
            "Lane grant E2E rule fixture.",
            "--level",
            "procedural",
            "--kind",
            "rule",
            "--tags",
            "release",
            "--json",
        ],
    )?;
    success_json(&remember, "ee remember")?;

    Ok(LaneGrantFixture {
        _tempdir: tempdir,
        workspace,
        workspace_id,
        peer_id,
    })
}

fn preview(fixture: &LaneGrantFixture, issue_token: bool) -> Result<(Output, Value), String> {
    let mut args = vec![
        "mesh",
        "preview-grant",
        fixture.peer_id.as_str(),
        "--lane",
        TEST_LANE_ARG,
    ];
    if issue_token {
        args.push("--issue-approval-token");
    }
    args.push("--json");
    let output = run_ee(&fixture.workspace, &args)?;
    let json = success_json(&output, "ee mesh preview-grant")?;
    Ok((output, json))
}

fn lane_audit_entries(fixture: &LaneGrantFixture, action: &str) -> Result<Vec<Value>, String> {
    let output = run_ee(
        &fixture.workspace,
        &[
            "audit", "timeline", "--action", action, "--limit", "100", "--json",
        ],
    )?;
    if !output.status.success() {
        return Err(format!(
            "audit timeline {action} failed with exit {:?}\nstdout:\n{}\nstderr:\n{}",
            output.status.code(),
            redacted_output(&output.stdout),
            redacted_output(&output.stderr),
        ));
    }
    ensure_json_stderr_empty(&output, "ee audit timeline")?;
    let json = stdout_json(&output, "ee audit timeline")?;
    ensure_equal(
        &json.pointer("/schema").and_then(Value::as_str),
        &Some("ee.audit.timeline.v1"),
        "audit timeline schema",
    )?;
    json.pointer("/entries")
        .and_then(Value::as_array)
        .cloned()
        .ok_or_else(|| format!("audit timeline {action} omitted entries: {json}"))
}

fn preview_payload(json: &Value, label: &str) -> Result<Value, String> {
    json.pointer("/data/preview")
        .cloned()
        .ok_or_else(|| format!("{label}: missing /data/preview: {json}"))
}

fn canonical_preview_bytes(json: &Value, label: &str) -> Result<Vec<u8>, String> {
    let mut preview = preview_payload(json, label)?;
    preview
        .as_object_mut()
        .ok_or_else(|| format!("{label}: preview payload was not an object"))?
        .remove("approvalToken");
    serde_json::to_vec(&preview)
        .map_err(|error| format!("{label}: failed to serialize canonical preview: {error}"))
}

fn approval_error_json(
    output: &Output,
    label: &str,
    expected_code: &str,
    expected_severity: &str,
    expected_message: &str,
) -> Result<Value, String> {
    ensure(
        !output.status.success(),
        format!("{label}: approval failure unexpectedly succeeded"),
    )?;
    ensure_json_stderr_empty(output, label)?;
    assert_no_bearer(&output.stdout, &format!("{label} stdout"))?;
    assert_no_bearer(&output.stderr, &format!("{label} stderr"))?;
    let json = stdout_json(output, label)?;
    ensure_equal(
        &json.pointer("/schema").and_then(Value::as_str),
        &Some("ee.error.v2"),
        &format!("{label} error schema"),
    )?;
    ensure_equal(
        &json.pointer("/error/code").and_then(Value::as_str),
        &Some(expected_code),
        &format!("{label} error code"),
    )?;
    ensure_equal(
        &json.pointer("/error/severity").and_then(Value::as_str),
        &Some(expected_severity),
        &format!("{label} error severity"),
    )?;
    ensure(
        json.pointer("/error/message")
            .and_then(Value::as_str)
            .is_some_and(|message| message.contains(expected_message)),
        format!("{label}: public message omitted {expected_message:?}: {json}"),
    )?;
    ensure(
        json.pointer("/error/repair")
            .and_then(Value::as_str)
            .is_some_and(|repair| repair.contains("read-only lane preview again")),
        format!("{label}: public repair did not require a fresh read-only preview: {json}"),
    )?;
    ensure(
        json.pointer("/error/details/recovery/0/command")
            .and_then(Value::as_str)
            .is_some_and(|command| {
                command.contains("mesh preview-grant")
                    && command.contains("--issue-approval-token")
                    && command.contains("--json")
            }),
        format!("{label}: structured recovery command was missing or incomplete: {json}"),
    )?;
    Ok(json)
}

#[test]
fn ordinary_preview_is_token_free_deterministic_and_read_only() -> TestResult {
    let fixture = set_up_fixture("preview")?;
    ensure_equal(
        &lane_audit_entries(&fixture, GRANT_AUDIT_ACTION)?.len(),
        &0,
        "grant audits before preview",
    )?;
    ensure_equal(
        &lane_audit_entries(&fixture, REVOKE_AUDIT_ACTION)?.len(),
        &0,
        "revoke audits before preview",
    )?;

    let (ordinary_output, ordinary_json) = preview(&fixture, false)?;
    assert_no_bearer(&ordinary_output.stdout, "ordinary preview stdout")?;
    let ordinary = preview_payload(&ordinary_json, "ordinary preview")?;
    ensure_equal(
        &ordinary.pointer("/schema").and_then(Value::as_str),
        &Some("ee.mesh.lane_grant_preview.v2"),
        "preview schema",
    )?;
    ensure_equal(
        &ordinary.pointer("/workspaceId").and_then(Value::as_str),
        &Some(fixture.workspace_id.as_str()),
        "preview workspace binding",
    )?;
    ensure_equal(
        &json_u64(&ordinary, "/grantGeneration", "ordinary preview")?,
        &0,
        "initial grant generation",
    )?;
    ensure_equal(
        &ordinary
            .pointer("/currentPolicy/decision")
            .and_then(Value::as_str),
        &Some("deny"),
        "ordinary preview current decision",
    )?;
    ensure_equal(
        &ordinary
            .pointer("/proposedPolicy/decision")
            .and_then(Value::as_str),
        &Some("allow"),
        "ordinary preview proposed decision",
    )?;
    ensure_equal(
        &ordinary.pointer("/lane").and_then(Value::as_str),
        &Some(TEST_LANE_WIRE),
        "ordinary preview lane",
    )?;
    ensure(
        ordinary
            .pointer("/candidateSet")
            .and_then(Value::as_array)
            .is_some_and(|candidates| candidates.len() == 1),
        format!("ordinary preview must bind the complete one-memory candidate set: {ordinary}"),
    )?;

    let (_, repeated_json) = preview(&fixture, false)?;
    let repeated = preview_payload(&repeated_json, "repeated ordinary preview")?;
    ensure_equal(
        &repeated,
        &ordinary,
        "ordinary preview must be deterministic",
    )?;

    let (issued_output, issued_json) = preview(&fixture, true)?;
    let mut issued = preview_payload(&issued_json, "explicit token preview")?;
    let bearer = sensitive_json_string(&issued, "/approvalToken/bearer", "explicit token preview")?;
    ensure(
        bearer.starts_with("eeap1_") && bearer.len() < 512,
        "explicit preview bearer must use the bounded eeap1_ envelope",
    )?;
    ensure_equal(
        &issued
            .pointer("/approvalToken/sensitive")
            .and_then(Value::as_bool),
        &Some(true),
        "explicit preview token sensitivity marker",
    )?;
    ensure(
        issued
            .pointer("/approvalToken/externalRecorderResidual")
            .and_then(Value::as_str)
            .is_some_and(|copy| copy.contains("third-party") && copy.contains("expires")),
        "explicit preview must state the external-recorder residual",
    )?;
    ensure_equal(
        &String::from_utf8_lossy(&issued_output.stdout)
            .matches(bearer.as_str())
            .count(),
        &1,
        "explicit projection must emit the bearer exactly once",
    )?;
    issued
        .as_object_mut()
        .ok_or_else(|| "explicit preview payload was not an object".to_owned())?
        .remove("approvalToken");
    ensure_equal(
        &issued,
        &ordinary,
        "token issuance must authenticate, not alter, the canonical preview",
    )?;

    ensure_equal(
        &lane_audit_entries(&fixture, GRANT_AUDIT_ACTION)?.len(),
        &0,
        "token-free and explicit previews must not append grant audits",
    )?;
    ensure_equal(
        &lane_audit_entries(&fixture, REVOKE_AUDIT_ACTION)?.len(),
        &0,
        "token-free and explicit previews must not append revoke audits",
    )
}

#[test]
fn preview_cautions_are_projected_as_actionable_top_level_degradations() -> TestResult {
    let fixture = set_up_fixture("preview-degraded")?;
    let already_granted_output = run_ee(
        &fixture.workspace,
        &[
            "mesh",
            "preview-grant",
            fixture.peer_id.as_str(),
            "--lane",
            "metadata",
            "--json",
        ],
    )?;
    let already_granted = success_json(&already_granted_output, "already-granted preview")?;
    let already_granted_degraded = already_granted
        .pointer("/degraded")
        .and_then(Value::as_array)
        .and_then(|entries| {
            entries.iter().find(|entry| {
                entry.pointer("/code").and_then(Value::as_str)
                    == Some("lane_grant_preview_lane_already_granted")
            })
        })
        .ok_or_else(|| format!("already-granted preview omitted degradation: {already_granted}"))?;
    ensure_equal(
        &already_granted_degraded
            .pointer("/severity")
            .and_then(Value::as_str),
        &Some("info"),
        "already-granted degradation severity",
    )?;
    ensure(
        already_granted_degraded
            .pointer("/message")
            .and_then(Value::as_str)
            .is_some_and(|message| message.contains("currently exposed")),
        format!("already-granted degradation message drifted: {already_granted_degraded}"),
    )?;

    write_mesh_policy_config(
        Path::new(&fixture.workspace),
        &fixture.workspace_id,
        &fixture.peer_id,
        false,
    )?;
    let omitted_group_output = run_ee(
        &fixture.workspace,
        &[
            "mesh",
            "preview-grant",
            fixture.peer_id.as_str(),
            "--lane",
            TEST_LANE_ARG,
            "--json",
        ],
    )?;
    let omitted_group = success_json(&omitted_group_output, "peer-group-omitted preview")?;
    let omitted_group_degraded = omitted_group
        .pointer("/degraded")
        .and_then(Value::as_array)
        .and_then(|entries| {
            entries.iter().find(|entry| {
                entry.pointer("/code").and_then(Value::as_str)
                    == Some("lane_grant_preview_peer_not_in_group")
            })
        })
        .ok_or_else(|| {
            format!("peer-group-omitted preview omitted degradation: {omitted_group}")
        })?;
    ensure_equal(
        &omitted_group_degraded
            .pointer("/severity")
            .and_then(Value::as_str),
        &Some("info"),
        "peer-group degradation severity",
    )?;
    ensure(
        omitted_group_degraded
            .pointer("/message")
            .and_then(Value::as_str)
            .is_some_and(|message| message.contains("peer-group bindings")),
        format!("peer-group degradation message drifted: {omitted_group_degraded}"),
    )?;
    ensure(
        omitted_group_degraded
            .pointer("/repair")
            .and_then(Value::as_str)
            .is_some_and(|repair| repair.contains("[[mesh.peer_group_bindings]]")),
        format!("peer-group degradation repair drifted: {omitted_group_degraded}"),
    )
}

#[test]
fn human_confirmation_reuses_json_preview_and_commits_the_reviewed_effect() -> TestResult {
    let fixture = set_up_fixture("human-confirm")?;
    let (_, preview_json) = preview(&fixture, false)?;
    let snapshot = preview_payload(&preview_json, "pre-confirmation JSON preview")?;
    let generation = json_u64(&snapshot, "/grantGeneration", "JSON preview")?;
    let proposed_decision = json_string(&snapshot, "/proposedPolicy/decision", "JSON preview")?;

    let human_preview = run_ee(
        &fixture.workspace,
        &[
            "mesh",
            "preview-grant",
            fixture.peer_id.as_str(),
            "--lane",
            TEST_LANE_ARG,
        ],
    )?;
    ensure(
        human_preview.status.success(),
        format!(
            "human preview failed with exit {:?}\nstdout:\n{}\nstderr:\n{}",
            human_preview.status.code(),
            redacted_output(&human_preview.stdout),
            redacted_output(&human_preview.stderr),
        ),
    )?;
    ensure(
        String::from_utf8_lossy(&human_preview.stderr)
            .trim()
            .is_empty(),
        format!(
            "human preview unexpectedly wrote stderr: {}",
            redacted_output(&human_preview.stderr)
        ),
    )?;
    assert_no_bearer(&human_preview.stdout, "human preview stdout")?;
    let human_preview_text = String::from_utf8(human_preview.stdout)
        .map_err(|error| format!("human preview stdout was not UTF-8: {error}"))?;
    let expected_lines = [
        format!(
            "  peer: {}",
            json_string(&snapshot, "/target/peerId", "JSON preview")?
        ),
        format!(
            "  lane: {}",
            json_string(&snapshot, "/lane", "JSON preview")?
        ),
        format!("  generation: {generation}"),
        format!(
            "  current: {}",
            json_string(&snapshot, "/currentPolicy/decision", "JSON preview")?
        ),
        format!("  proposed: {proposed_decision}"),
        format!(
            "  affected memories: {}",
            json_u64(&snapshot, "/affectedMemoryCount", "JSON preview")?
        ),
        format!(
            "  redacted from exposure: {}",
            json_u64(&snapshot, "/redactedFromExposureCount", "JSON preview")?
        ),
    ];
    for line in expected_lines {
        ensure(
            human_preview_text.lines().any(|actual| actual == line),
            format!("human preview omitted JSON-derived line {line:?}"),
        )?;
    }

    let grant = run_ee_with_stdin(
        &fixture.workspace,
        &[
            "mesh",
            "grant",
            fixture.peer_id.as_str(),
            "--lane",
            TEST_LANE_ARG,
        ],
        b"yes\n",
    )?;
    ensure(
        grant.status.success(),
        format!(
            "confirmed human grant failed with exit {:?}\nstdout:\n{}\nstderr:\n{}",
            grant.status.code(),
            redacted_output(&grant.stdout),
            redacted_output(&grant.stderr),
        ),
    )?;
    assert_no_bearer(&grant.stdout, "confirmed human grant stdout")?;
    assert_no_bearer(&grant.stderr, "confirmed human grant stderr")?;
    let grant_stdout = String::from_utf8(grant.stdout)
        .map_err(|error| format!("confirmed human grant stdout was not UTF-8: {error}"))?;
    ensure(
        grant_stdout.starts_with(&human_preview_text),
        "human grant must render the same canonical preview that the standalone preview rendered",
    )?;
    let next_generation = generation
        .checked_add(1)
        .ok_or_else(|| "test preview generation overflowed".to_owned())?;
    ensure(
        grant_stdout.contains(&format!(
            "ee mesh grant: peer={} lane={TEST_LANE_WIRE} decision={proposed_decision} generation {generation} -> {next_generation}",
            fixture.peer_id
        )),
        format!("human mutation report did not match the reviewed JSON snapshot: {grant_stdout}"),
    )?;
    let grant_stderr = String::from_utf8(grant.stderr)
        .map_err(|error| format!("confirmed human grant stderr was not UTF-8: {error}"))?;
    ensure_equal(
        &grant_stderr,
        &format!(
            "Grant lane '{TEST_LANE_WIRE}' to peer {}? [y/N] ",
            fixture.peer_id
        ),
        "human confirmation prompt",
    )?;

    let (_, after_json) = preview(&fixture, false)?;
    let after = preview_payload(&after_json, "post-human-grant preview")?;
    ensure_equal(
        &json_u64(&after, "/grantGeneration", "post-human-grant preview")?,
        &next_generation,
        "human grant generation effect",
    )?;
    ensure_equal(
        &after
            .pointer("/currentPolicy/decision")
            .and_then(Value::as_str),
        &Some(proposed_decision.as_str()),
        "human grant policy effect",
    )?;
    ensure_equal(
        &lane_audit_entries(&fixture, GRANT_AUDIT_ACTION)?.len(),
        &1,
        "confirmed human grant audit count",
    )
}

#[test]
fn invalid_and_expired_bearers_have_distinct_public_errors_and_zero_effect() -> TestResult {
    let fixture = set_up_fixture("invalid-expired")?;
    let (_, baseline_json) = preview(&fixture, false)?;
    let baseline = preview_payload(&baseline_json, "invalid/expired baseline")?;

    let invalid = run_ee_with_stdin(
        &fixture.workspace,
        &[
            "mesh",
            "grant",
            fixture.peer_id.as_str(),
            "--lane",
            TEST_LANE_ARG,
            "--preview-token-stdin",
            "--json",
        ],
        b"eeap1_invalid-bearer\n",
    )?;
    approval_error_json(
        &invalid,
        "malformed approval bearer",
        "mesh_approval_token_invalid",
        "high",
        "invalid for this store, workspace, and command",
    )?;

    let canonical_snapshot = canonical_preview_bytes(&baseline_json, "expired-token preview")?;
    let keys_dir = workspace_keys_dir(Path::new(&fixture.workspace));
    let root = StoreAuthRoot::open(&keys_dir)
        .map_err(|error| format!("failed to open store-auth root for expired bearer: {error}"))?;
    let now = chrono::Utc::now().timestamp();
    let issued_at = now
        .checked_sub(APPROVAL_TOKEN_TTL_SECONDS)
        .ok_or_else(|| "approval token expiry fixture timestamp underflowed".to_owned())?;
    let expired = issue(
        &root,
        &fixture.workspace_id,
        GRANT_SCHEMA,
        &canonical_snapshot,
        issued_at,
    )
    .map_err(|error| format!("failed to issue authentic expired bearer: {error}"))?;
    let expired_bearer = expired.token().expose_bearer();
    drop(root);

    let expired_result = run_ee_with_stdin(
        &fixture.workspace,
        &[
            "mesh",
            "grant",
            fixture.peer_id.as_str(),
            "--lane",
            TEST_LANE_ARG,
            "--preview-token-stdin",
            "--json",
        ],
        format!("{expired_bearer}\n").as_bytes(),
    )?;
    approval_error_json(
        &expired_result,
        "authentic expired approval bearer",
        "mesh_approval_token_stale",
        "warning",
        "authentic but its approved preview is stale",
    )?;

    let (_, after_json) = preview(&fixture, false)?;
    let after = preview_payload(&after_json, "post-invalid/expired preview")?;
    ensure_equal(
        &after,
        &baseline,
        "invalid and expired bearers must leave the lane snapshot unchanged",
    )?;
    ensure_equal(
        &lane_audit_entries(&fixture, GRANT_AUDIT_ACTION)?.len(),
        &0,
        "invalid and expired bearers must append no grant audit",
    )
}

#[test]
fn config_byte_drift_after_preview_stales_bearer_with_zero_effect() -> TestResult {
    let fixture = set_up_fixture("config-drift-before-grant")?;
    let (_, issued_json) = preview(&fixture, true)?;
    let bearer = sensitive_json_string(
        &issued_json,
        "/data/preview/approvalToken/bearer",
        "pre-config-drift approval preview",
    )?;
    let config_path = Path::new(&fixture.workspace)
        .join(".ee")
        .join("config.toml");
    let mut drifted_config_bytes = fs::read(&config_path)
        .map_err(|error| format!("read pre-drift lane-grant config: {error}"))?;
    drifted_config_bytes.extend_from_slice(b"\n# comment-only drift after approval preview\n");
    fs::write(&config_path, drifted_config_bytes)
        .map_err(|error| format!("write comment-drifted lane-grant config: {error}"))?;

    let rejected = run_ee_with_stdin(
        &fixture.workspace,
        &[
            "mesh",
            "grant",
            fixture.peer_id.as_str(),
            "--lane",
            TEST_LANE_ARG,
            "--preview-token-stdin",
            "--json",
        ],
        format!("{bearer}\n").as_bytes(),
    )?;
    approval_error_json(
        &rejected,
        "comment-drifted approval bearer",
        "mesh_approval_token_stale",
        "warning",
        "authentic but its approved preview is stale",
    )?;

    let (_, after_json) = preview(&fixture, false)?;
    let after = preview_payload(&after_json, "post-config-drift rejection preview")?;
    ensure_equal(
        &json_u64(
            &after,
            "/grantGeneration",
            "post-config-drift rejection preview",
        )?,
        &0,
        "config-drift rejection must not advance consent generation",
    )?;
    ensure_equal(
        &after
            .pointer("/currentPolicy/decision")
            .and_then(Value::as_str),
        &Some("deny"),
        "config-drift rejection must leave the lane denied",
    )?;
    ensure_equal(
        &lane_audit_entries(&fixture, GRANT_AUDIT_ACTION)?.len(),
        &0,
        "config-drift rejection must append no grant audit",
    )
}

#[test]
fn store_key_rotation_invalidates_outstanding_bearer_with_zero_effect() -> TestResult {
    let fixture = set_up_fixture("key-rotation")?;
    let (_, baseline_json) = preview(&fixture, false)?;
    let baseline = preview_payload(&baseline_json, "key-rotation baseline")?;
    let (_, issued_json) = preview(&fixture, true)?;
    let bearer = sensitive_json_string(
        &issued_json,
        "/data/preview/approvalToken/bearer",
        "pre-rotation approval preview",
    )?;

    let keys_dir = workspace_keys_dir(Path::new(&fixture.workspace));
    let mut root = StoreAuthRoot::open(&keys_dir)
        .map_err(|error| format!("failed to open store-auth root for rotation: {error}"))?;
    let original_key_id = root.current_key_id();
    let rotated_key_id = root
        .rotate()
        .map_err(|error| format!("failed to rotate store-auth root: {error}"))?;
    ensure(
        rotated_key_id != original_key_id,
        "store-auth rotation must install a distinct current key",
    )?;
    drop(root);

    let rejected = run_ee_with_stdin(
        &fixture.workspace,
        &[
            "mesh",
            "grant",
            fixture.peer_id.as_str(),
            "--lane",
            TEST_LANE_ARG,
            "--preview-token-stdin",
            "--json",
        ],
        format!("{bearer}\n").as_bytes(),
    )?;
    approval_error_json(
        &rejected,
        "rotated-key approval bearer",
        "mesh_approval_token_invalid",
        "high",
        "invalid for this store, workspace, and command",
    )?;

    let (_, after_json) = preview(&fixture, false)?;
    let after = preview_payload(&after_json, "post-key-rotation preview")?;
    ensure_equal(
        &after,
        &baseline,
        "key rotation and rejected bearer must leave lane state unchanged",
    )?;
    ensure_equal(
        &lane_audit_entries(&fixture, GRANT_AUDIT_ACTION)?.len(),
        &0,
        "rotated-key rejection must append no grant audit",
    )
}

#[test]
fn concurrent_double_apply_commits_once_and_stales_the_loser() -> TestResult {
    let fixture = set_up_fixture("concurrent-apply")?;
    let database_path = Path::new(&fixture.workspace).join(".ee").join("ee.db");
    let config_bytes = fs::read(
        Path::new(&fixture.workspace)
            .join(".ee")
            .join("config.toml"),
    )
    .map_err(|error| format!("read concurrent approval config: {error}"))?;
    let approval_config_digest = ee::mesh::lane_grant::approval_config_digest(&config_bytes);
    let keys_dir = workspace_keys_dir(Path::new(&fixture.workspace));
    let root = StoreAuthRoot::open(&keys_dir)
        .map_err(|error| format!("open concurrent store-auth root: {error}"))?;
    let canonical_snapshot = b"authenticated concurrent lane-grant snapshot".to_vec();
    let approval_now = chrono::Utc::now().timestamp();
    let issued = issue(
        &root,
        &fixture.workspace_id,
        GRANT_SCHEMA,
        &canonical_snapshot,
        approval_now,
    )
    .map_err(|error| format!("issue concurrent approval bearer: {error}"))?;
    let bearer = issued.token().expose_bearer();
    drop(root);
    let barrier = Arc::new(Barrier::new(3));
    let effect_count = Arc::new(AtomicUsize::new(0));
    let spawn_contender = |label: &'static str| {
        let database_path = database_path.clone();
        let workspace_id = fixture.workspace_id.clone();
        let peer_id = fixture.peer_id.clone();
        let approval_config_digest = approval_config_digest.clone();
        let keys_dir = keys_dir.clone();
        let bearer = bearer.clone();
        let canonical_snapshot = canonical_snapshot.clone();
        let barrier = Arc::clone(&barrier);
        let effect_count = Arc::clone(&effect_count);
        std::thread::spawn(move || -> Result<&'static str, String> {
            // Each contender independently reads generation zero and prepares
            // an identical CAS input before either may enter the writer-fenced
            // transaction. Even setup errors rendezvous at the barrier so the
            // test cannot deadlock while reporting the real preparation fault.
            let prepared = (|| {
                let root = StoreAuthRoot::open(&keys_dir)
                    .map_err(|error| format!("{label}: open store-auth root: {error}"))?;
                let authenticated =
                    verify_authentic(&root, &workspace_id, GRANT_SCHEMA, &bearer, approval_now)
                        .map_err(|error| format!("{label}: authenticate shared bearer: {error}"))?;
                let connection = DbConnection::open_file(&database_path)
                    .map_err(|error| format!("{label}: open database: {error}"))?;
                let peer = connection
                    .get_mesh_peer(&workspace_id, &peer_id)
                    .map_err(|error| format!("{label}: load peer: {error}"))?
                    .ok_or_else(|| format!("{label}: enrolled peer disappeared"))?;
                let generation = connection
                    .mesh_lane_grant_generation(&workspace_id, &peer_id)
                    .map_err(|error| format!("{label}: load generation: {error}"))?;
                ensure_equal(&generation, &0, &format!("{label} prepared generation"))?;
                let target_adapter =
                    MeshLaneGrantTargetAdapter::new(peer.peer_id.clone(), peer.origin_node_id);
                Ok((
                    connection,
                    root,
                    authenticated,
                    MeshLaneGrantMutationInput {
                        workspace_id,
                        peer_id: peer.peer_id,
                        target_adapter,
                        material_lane: MeshLane::GraphLink,
                        expected_generation: generation,
                        approval_config_digest: Some(approval_config_digest),
                        updated_at: Some("2026-08-04T00:00:00Z".to_owned()),
                    },
                ))
            })();
            barrier.wait();
            let (connection, root, authenticated, input) = prepared?;

            let transaction = connection.apply_mesh_lane_grant_transaction(
                &input,
                || {
                    compare_snapshot(&root, &authenticated, &canonical_snapshot, approval_now)
                        .map_err(|error| error.code().to_owned())
                },
                |state, verified| {
                    let approval_audit_id = verified.audit_id().to_opaque_string();
                    connection
                        .insert_audit_with_mutation_kind(
                            "audit_concurrent_lane_grant",
                            &CreateAuditInput {
                                workspace_id: Some(input.workspace_id.clone()),
                                actor: Some("e2e concurrent lane grant".to_owned()),
                                action: GRANT_AUDIT_ACTION.to_owned(),
                                target_type: Some("mesh_peer".to_owned()),
                                target_id: Some(input.peer_id.clone()),
                                details: Some(
                                    serde_json::json!({
                                        "policyDecisionId": approval_audit_id,
                                        "grantGeneration": state.grant_generation,
                                    })
                                    .to_string(),
                                ),
                            },
                            GRANT_AUDIT_ACTION,
                        )
                        .map_err(|error| format!("audit insert failed: {error}"))?;
                    effect_count.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                },
            );
            match transaction {
                Ok((state, _, ())) => {
                    ensure_equal(
                        &state.grant_generation,
                        &1,
                        &format!("{label} committed generation"),
                    )?;
                    Ok("committed")
                }
                Err(MeshLaneGrantAtomicError::Verification(code))
                    if code == "mesh_approval_token_stale" =>
                {
                    Ok("stale")
                }
                Err(MeshLaneGrantAtomicError::Mutation(
                    MeshLaneGrantMutationError::GenerationConflict { .. },
                )) => Ok("stale"),
                Err(error) => Err(format!("{label}: unexpected atomic outcome: {error}")),
            }
        })
    };
    let first = spawn_contender("first contender");
    let second = spawn_contender("second contender");
    barrier.wait();
    let first_outcome = first
        .join()
        .map_err(|_| "first concurrent contender panicked".to_owned())??;
    let second_outcome = second
        .join()
        .map_err(|_| "second concurrent contender panicked".to_owned())??;
    let mut outcomes = [first_outcome, second_outcome];
    outcomes.sort_unstable();
    ensure_equal(
        &outcomes,
        &["committed", "stale"],
        "barrier-coordinated concurrent outcomes",
    )?;
    ensure_equal(
        &effect_count.load(Ordering::SeqCst),
        &1,
        "stale contender must execute no transactional effect",
    )?;

    let (_, after_json) = preview(&fixture, false)?;
    let after = preview_payload(&after_json, "post-concurrent-grant preview")?;
    ensure_equal(
        &json_u64(&after, "/grantGeneration", "post-concurrent-grant preview")?,
        &1,
        "concurrent double apply generation",
    )?;
    ensure_equal(
        &after
            .pointer("/currentPolicy/decision")
            .and_then(Value::as_str),
        &Some("allow"),
        "concurrent double apply decision",
    )?;
    let audits = lane_audit_entries(&fixture, GRANT_AUDIT_ACTION)?;
    ensure_equal(&audits.len(), &1, "concurrent double apply audit count")?;
    ensure_equal(
        &audits[0]
            .pointer("/details/policyDecisionId")
            .and_then(Value::as_str)
            .is_some_and(|value| value.starts_with("eela1_")),
        &true,
        "concurrent double apply committed authenticated audit binding",
    )?;
    assert_no_bearer(
        serde_json::to_string(&audits)
            .map_err(|error| format!("serialize concurrent audits: {error}"))?
            .as_bytes(),
        "concurrent audit rows",
    )
}

#[test]
fn bounded_stdin_grant_replay_and_revoke_are_generation_atomic() -> TestResult {
    let fixture = set_up_fixture("mutation")?;
    let config_path = Path::new(&fixture.workspace)
        .join(".ee")
        .join("config.toml");
    let original_config_bytes = fs::read(&config_path)
        .map_err(|error| format!("read original lane-grant config: {error}"))?;
    let expected_config_digest =
        ee::mesh::lane_grant::approval_config_digest(&original_config_bytes);
    let (_, issued_json) = preview(&fixture, true)?;
    let bearer = sensitive_json_string(
        &issued_json,
        "/data/preview/approvalToken/bearer",
        "grant approval preview",
    )?;
    let bounded_stdin = format!("{bearer}\n");

    let grant = run_ee_with_stdin(
        &fixture.workspace,
        &[
            "mesh",
            "grant",
            fixture.peer_id.as_str(),
            "--lane",
            TEST_LANE_ARG,
            "--preview-token-stdin",
            "--json",
        ],
        bounded_stdin.as_bytes(),
    )?;
    let grant_json = success_json(&grant, "ee mesh grant")?;
    assert_no_bearer(&grant.stdout, "grant success stdout")?;
    assert_no_bearer(&grant.stderr, "grant success stderr")?;
    ensure_equal(
        &grant_json.pointer("/data/schema").and_then(Value::as_str),
        &Some("ee.mesh.grant.v1"),
        "grant result schema",
    )?;
    ensure_equal(
        &grant_json.pointer("/data/command").and_then(Value::as_str),
        &Some("ee mesh grant"),
        "grant result command",
    )?;
    ensure_equal(
        &json_u64(&grant_json, "/data/previousGrantGeneration", "grant result")?,
        &0,
        "grant previous generation",
    )?;
    ensure_equal(
        &json_u64(&grant_json, "/data/newGrantGeneration", "grant result")?,
        &1,
        "grant new generation",
    )?;
    ensure_equal(
        &grant_json.pointer("/data/decision").and_then(Value::as_str),
        &Some("allow"),
        "grant decision",
    )?;
    ensure_equal(
        &grant_json
            .pointer("/data/remoteErasureGuaranteed")
            .and_then(Value::as_bool),
        &Some(false),
        "grant remote-erasure claim",
    )?;
    ensure(
        grant_json
            .pointer("/data/residual")
            .and_then(Value::as_str)
            .is_some_and(|copy| copy.contains("cannot erase bytes")),
        "grant must state the cached/copied-byte residual",
    )?;
    let approval_audit_id = json_string(&grant_json, "/data/auditId", "grant result")?;
    ensure(
        approval_audit_id.starts_with("eela1_"),
        "grant auditId must be the opaque approval audit handle",
    )?;

    let grant_audits = lane_audit_entries(&fixture, GRANT_AUDIT_ACTION)?;
    ensure_equal(&grant_audits.len(), &1, "durable grant audit count")?;
    ensure_equal(
        &grant_audits[0]
            .pointer("/mutation_kind")
            .and_then(Value::as_str),
        &Some(GRANT_AUDIT_ACTION),
        "grant audit mutation kind",
    )?;
    ensure_equal(
        &grant_audits[0]
            .pointer("/details/policyDecisionId")
            .and_then(Value::as_str),
        &Some(approval_audit_id.as_str()),
        "grant audit approval binding",
    )?;
    ensure_equal(
        &grant_audits[0]
            .pointer("/details/details/entries/approval_config_digest/kind")
            .and_then(Value::as_str),
        &Some("digest"),
        "grant audit config-binding kind",
    )?;
    ensure_equal(
        &grant_audits[0]
            .pointer("/details/details/entries/approval_config_digest/value")
            .and_then(Value::as_str),
        &Some(expected_config_digest.as_str()),
        "grant audit exact approved config digest",
    )?;
    let serialized_grant_audit = serde_json::to_string(&grant_audits[0])
        .map_err(|error| format!("serialize grant audit: {error}"))?;
    assert_no_bearer(serialized_grant_audit.as_bytes(), "grant audit row")?;
    ensure(
        !serialized_grant_audit.contains("[mesh]")
            && !serialized_grant_audit.contains("command_mode"),
        "grant audit must persist only the config digest, never raw config bytes",
    )?;

    let (_, granted_preview_json) = preview(&fixture, false)?;
    let granted_preview = preview_payload(&granted_preview_json, "post-grant preview")?;
    ensure_equal(
        &json_u64(&granted_preview, "/grantGeneration", "post-grant preview")?,
        &1,
        "post-grant generation",
    )?;
    ensure_equal(
        &granted_preview
            .pointer("/currentPolicy/decision")
            .and_then(Value::as_str),
        &Some("allow"),
        "post-grant decision",
    )?;

    let mut drifted_config_bytes = original_config_bytes.clone();
    drifted_config_bytes.extend_from_slice(b"\n# post-approval byte drift\n");
    fs::write(&config_path, &drifted_config_bytes)
        .map_err(|error| format!("write drifted lane-grant config: {error}"))?;
    let (_, drifted_preview_json) = preview(&fixture, false)?;
    let drifted_preview = preview_payload(&drifted_preview_json, "config-drifted preview")?;
    ensure_equal(
        &json_u64(
            &drifted_preview,
            "/grantGeneration",
            "config-drifted preview",
        )?,
        &1,
        "config drift does not mutate consent generation",
    )?;
    ensure_equal(
        &drifted_preview
            .pointer("/currentPolicy/decision")
            .and_then(Value::as_str),
        &Some("deny"),
        "config drift makes the widened allow dormant",
    )?;

    fs::write(&config_path, &original_config_bytes)
        .map_err(|error| format!("restore exact lane-grant config: {error}"))?;
    let (_, restored_preview_json) = preview(&fixture, false)?;
    let restored_preview = preview_payload(&restored_preview_json, "config-restored preview")?;
    ensure_equal(
        &json_u64(
            &restored_preview,
            "/grantGeneration",
            "config-restored preview",
        )?,
        &1,
        "byte-exact config restore preserves consent generation",
    )?;
    ensure_equal(
        &restored_preview
            .pointer("/currentPolicy/decision")
            .and_then(Value::as_str),
        &Some("allow"),
        "byte-exact config restore reactivates only the matching approval",
    )?;

    let replay = run_ee_with_stdin(
        &fixture.workspace,
        &[
            "mesh",
            "grant",
            fixture.peer_id.as_str(),
            "--lane",
            TEST_LANE_ARG,
            "--preview-token-stdin",
            "--json",
        ],
        bounded_stdin.as_bytes(),
    )?;
    ensure(
        !replay.status.success(),
        "replaying a committed preview token must fail",
    )?;
    ensure_json_stderr_empty(&replay, "stale grant replay")?;
    assert_no_bearer(&replay.stdout, "stale grant replay stdout")?;
    assert_no_bearer(&replay.stderr, "stale grant replay stderr")?;
    let replay_json = stdout_json(&replay, "stale grant replay")?;
    ensure_equal(
        &replay_json.pointer("/schema").and_then(Value::as_str),
        &Some("ee.error.v2"),
        "stale replay error schema",
    )?;
    ensure_equal(
        &replay_json.pointer("/error/code").and_then(Value::as_str),
        &Some("mesh_approval_token_stale"),
        "stale replay code",
    )?;
    ensure_equal(
        &replay_json
            .pointer("/error/severity")
            .and_then(Value::as_str),
        &Some("warning"),
        "stale replay severity",
    )?;

    let (_, after_replay_json) = preview(&fixture, false)?;
    let after_replay = preview_payload(&after_replay_json, "post-replay preview")?;
    ensure_equal(
        &after_replay,
        &granted_preview,
        "stale replay must not mutate lane state",
    )?;
    let grant_audits_after_replay = lane_audit_entries(&fixture, GRANT_AUDIT_ACTION)?;
    ensure_equal(
        &grant_audits_after_replay,
        &grant_audits,
        "stale replay must not append an audit row",
    )?;

    fs::write(&config_path, &drifted_config_bytes)
        .map_err(|error| format!("reapply config drift before revoke: {error}"))?;

    let revoke = run_ee(
        &fixture.workspace,
        &[
            "mesh",
            "revoke-lane",
            fixture.peer_id.as_str(),
            "--lane",
            TEST_LANE_ARG,
            "--json",
        ],
    )?;
    let revoke_json = success_json(&revoke, "ee mesh revoke-lane")?;
    assert_no_bearer(&revoke.stdout, "revoke stdout")?;
    ensure_equal(
        &revoke_json.pointer("/data/schema").and_then(Value::as_str),
        &Some("ee.mesh.revoke_lane.v1"),
        "revoke result schema",
    )?;
    ensure_equal(
        &json_u64(
            &revoke_json,
            "/data/previousGrantGeneration",
            "revoke result",
        )?,
        &1,
        "revoke previous generation",
    )?;
    ensure_equal(
        &json_u64(&revoke_json, "/data/newGrantGeneration", "revoke result")?,
        &2,
        "revoke new generation",
    )?;
    ensure_equal(
        &revoke_json
            .pointer("/data/decision")
            .and_then(Value::as_str),
        &Some("deny"),
        "revoke decision",
    )?;
    ensure_equal(
        &revoke_json
            .pointer("/data/remoteErasureGuaranteed")
            .and_then(Value::as_bool),
        &Some(false),
        "revoke remote-erasure claim",
    )?;
    ensure(
        revoke_json
            .pointer("/data/residual")
            .and_then(Value::as_str)
            .is_some_and(|copy| {
                copy.contains("cannot erase bytes")
                    && (copy.contains("cached") || copy.contains("copied"))
            }),
        "revoke must state the cached/copied-byte residual",
    )?;
    let revoke_audit_id = json_string(&revoke_json, "/data/auditId", "revoke result")?;
    let revoke_audits = lane_audit_entries(&fixture, REVOKE_AUDIT_ACTION)?;
    ensure_equal(&revoke_audits.len(), &1, "durable revoke audit count")?;
    ensure_equal(
        &revoke_audits[0].pointer("/id").and_then(Value::as_str),
        &Some(revoke_audit_id.as_str()),
        "revoke response audit row binding",
    )?;
    ensure_equal(
        &revoke_audits[0]
            .pointer("/mutation_kind")
            .and_then(Value::as_str),
        &Some(REVOKE_AUDIT_ACTION),
        "revoke audit mutation kind",
    )?;

    let (_, revoked_preview_json) = preview(&fixture, false)?;
    let revoked_preview = preview_payload(&revoked_preview_json, "post-revoke preview")?;
    ensure_equal(
        &json_u64(&revoked_preview, "/grantGeneration", "post-revoke preview")?,
        &2,
        "post-revoke generation",
    )?;
    ensure_equal(
        &revoked_preview
            .pointer("/currentPolicy/decision")
            .and_then(Value::as_str),
        &Some("deny"),
        "post-revoke decision",
    )?;
    fs::write(&config_path, &original_config_bytes)
        .map_err(|error| format!("restore config after revoke: {error}"))?;
    let (_, restored_after_revoke_json) = preview(&fixture, false)?;
    let restored_after_revoke =
        preview_payload(&restored_after_revoke_json, "restored post-revoke preview")?;
    ensure_equal(
        &json_u64(
            &restored_after_revoke,
            "/grantGeneration",
            "restored post-revoke preview",
        )?,
        &2,
        "config restore cannot undo revoke generation",
    )?;
    ensure_equal(
        &restored_after_revoke
            .pointer("/currentPolicy/decision")
            .and_then(Value::as_str),
        &Some("deny"),
        "revoke remains deny after config drift and byte-exact restore",
    )
}

#[test]
fn peer_revoke_and_same_node_reenrollment_cannot_resurrect_lane_consent() -> TestResult {
    let fixture = set_up_fixture("peer-reenroll")?;
    let (_, first_token_json) = preview(&fixture, true)?;
    let first_bearer = sensitive_json_string(
        &first_token_json,
        "/data/preview/approvalToken/bearer",
        "initial grant approval",
    )?;
    let first_grant = run_ee_with_stdin(
        &fixture.workspace,
        &[
            "mesh",
            "grant",
            fixture.peer_id.as_str(),
            "--lane",
            TEST_LANE_ARG,
            "--preview-token-stdin",
            "--json",
        ],
        format!("{first_bearer}\n").as_bytes(),
    )?;
    success_json(&first_grant, "initial lane grant")?;

    // This fresh bearer is authentic for the granted generation. Revoking the
    // peer and re-enrolling the same deterministic node must stale it even
    // though peer_id and origin_node_id are reproduced exactly.
    let (_, outstanding_token_json) = preview(&fixture, true)?;
    let outstanding_bearer = sensitive_json_string(
        &outstanding_token_json,
        "/data/preview/approvalToken/bearer",
        "outstanding post-grant approval",
    )?;

    let peer_revoke = run_ee(
        &fixture.workspace,
        &["mesh", "peer", "revoke", fixture.peer_id.as_str(), "--json"],
    )?;
    success_json(&peer_revoke, "ee mesh peer revoke")?;

    let disabled_target = run_ee_with_stdin(
        &fixture.workspace,
        &[
            "mesh",
            "grant",
            fixture.peer_id.as_str(),
            "--lane",
            TEST_LANE_ARG,
            "--preview-token-stdin",
            "--json",
        ],
        format!("{outstanding_bearer}\n").as_bytes(),
    )?;
    ensure(
        !disabled_target.status.success(),
        "an authentic bearer for a now-disabled target must fail",
    )?;
    ensure_json_stderr_empty(&disabled_target, "disabled target approval")?;
    let disabled_target_json = stdout_json(&disabled_target, "disabled target approval")?;
    ensure_equal(
        &disabled_target_json
            .pointer("/error/code")
            .and_then(Value::as_str),
        &Some("mesh_approval_token_stale"),
        "authenticated disabled-target drift must be classified stale",
    )?;
    ensure_equal(
        &lane_audit_entries(&fixture, GRANT_AUDIT_ACTION)?.len(),
        &1,
        "disabled-target rejection must append no grant audit",
    )?;

    let peer_add = run_ee(
        &fixture.workspace,
        &[
            "mesh",
            "peer",
            "add",
            "--alias",
            "lane-grant-peer",
            "--tailscale-node-key",
            "nodekey:lane-grant-e2e",
            "--endpoint",
            "100.64.20.2:4747",
            "--tailnet-id",
            "tn_lane_grant_e2e",
            "--profile",
            "metadata-only",
            "--public-key-fingerprint",
            "blake3:lane-grant-e2e",
            "--responder-capability",
            "mesh:metadata",
            "--yes",
            "--json",
        ],
    )?;
    let peer_add_json = success_json(&peer_add, "same-node ee mesh peer add")?;
    ensure_equal(
        &peer_add_json
            .pointer("/data/peerId")
            .and_then(Value::as_str),
        &Some(fixture.peer_id.as_str()),
        "same-node re-enrollment must reproduce the peer id used by the regression",
    )?;

    let (_, reenrolled_preview_json) = preview(&fixture, false)?;
    let reenrolled_preview =
        preview_payload(&reenrolled_preview_json, "same-node re-enrollment preview")?;
    ensure_equal(
        &reenrolled_preview
            .pointer("/currentPolicy/decision")
            .and_then(Value::as_str),
        &Some("deny"),
        "re-enrollment must inherit the deny baseline instead of resurrecting an allow",
    )?;
    ensure_equal(
        &json_u64(
            &reenrolled_preview,
            "/grantGeneration",
            "same-node re-enrollment preview",
        )?,
        &3,
        "grant, revoke, and re-enrollment must each leave a monotonic generation fence",
    )?;

    let stale = run_ee_with_stdin(
        &fixture.workspace,
        &[
            "mesh",
            "grant",
            fixture.peer_id.as_str(),
            "--lane",
            TEST_LANE_ARG,
            "--preview-token-stdin",
            "--json",
        ],
        format!("{outstanding_bearer}\n").as_bytes(),
    )?;
    ensure(!stale.status.success(), "pre-revoke bearer must be stale")?;
    ensure_json_stderr_empty(&stale, "pre-revoke bearer replay")?;
    assert_no_bearer(&stale.stdout, "pre-revoke bearer replay stdout")?;
    assert_no_bearer(&stale.stderr, "pre-revoke bearer replay stderr")?;
    let stale_json = stdout_json(&stale, "pre-revoke bearer replay")?;
    ensure_equal(
        &stale_json.pointer("/error/code").and_then(Value::as_str),
        &Some("mesh_approval_token_stale"),
        "pre-revoke bearer error code",
    )?;
    ensure_equal(
        &lane_audit_entries(&fixture, GRANT_AUDIT_ACTION)?.len(),
        &1,
        "stale replay after re-enrollment must append no grant audit",
    )
}

#[test]
fn pregrant_bearer_is_stale_after_same_node_reenrollment() -> TestResult {
    let fixture = set_up_fixture("pregrant-reenroll")?;
    let (_, token_json) = preview(&fixture, true)?;
    let bearer = sensitive_json_string(
        &token_json,
        "/data/preview/approvalToken/bearer",
        "generation-zero approval",
    )?;
    ensure_equal(
        &lane_audit_entries(&fixture, GRANT_AUDIT_ACTION)?.len(),
        &0,
        "generation-zero preview is read-only",
    )?;

    let peer_revoke = run_ee(
        &fixture.workspace,
        &["mesh", "peer", "revoke", fixture.peer_id.as_str(), "--json"],
    )?;
    success_json(&peer_revoke, "pregrant ee mesh peer revoke")?;
    let peer_add = run_ee(
        &fixture.workspace,
        &[
            "mesh",
            "peer",
            "add",
            "--alias",
            "lane-grant-peer",
            "--tailscale-node-key",
            "nodekey:lane-grant-e2e",
            "--endpoint",
            "100.64.20.2:4747",
            "--tailnet-id",
            "tn_lane_grant_e2e",
            "--profile",
            "metadata-only",
            "--public-key-fingerprint",
            "blake3:lane-grant-e2e",
            "--responder-capability",
            "mesh:metadata",
            "--yes",
            "--json",
        ],
    )?;
    let peer_add_json = success_json(&peer_add, "pregrant same-node ee mesh peer add")?;
    ensure_equal(
        &peer_add_json
            .pointer("/data/peerId")
            .and_then(Value::as_str),
        &Some(fixture.peer_id.as_str()),
        "pregrant regression must exercise deterministic same-node identity reuse",
    )?;

    let (_, current_json) = preview(&fixture, false)?;
    let current = preview_payload(&current_json, "post-reenrollment pregrant preview")?;
    ensure_equal(
        &json_u64(
            &current,
            "/grantGeneration",
            "post-reenrollment pregrant preview",
        )?,
        &2,
        "revoke and re-enroll must fence a generation-zero bearer twice",
    )?;
    ensure_equal(
        &current
            .pointer("/currentPolicy/decision")
            .and_then(Value::as_str),
        &Some("deny"),
        "same-node re-enrollment retains the configured deny baseline",
    )?;

    let stale = run_ee_with_stdin(
        &fixture.workspace,
        &[
            "mesh",
            "grant",
            fixture.peer_id.as_str(),
            "--lane",
            TEST_LANE_ARG,
            "--preview-token-stdin",
            "--json",
        ],
        format!("{bearer}\n").as_bytes(),
    )?;
    ensure(
        !stale.status.success(),
        "generation-zero bearer must not survive peer lifecycle reuse",
    )?;
    ensure_json_stderr_empty(&stale, "generation-zero stale bearer")?;
    assert_no_bearer(&stale.stdout, "generation-zero stale bearer stdout")?;
    assert_no_bearer(&stale.stderr, "generation-zero stale bearer stderr")?;
    let stale_json = stdout_json(&stale, "generation-zero stale bearer")?;
    ensure_equal(
        &stale_json.pointer("/error/code").and_then(Value::as_str),
        &Some("mesh_approval_token_stale"),
        "generation-zero stale error code",
    )?;
    ensure_equal(
        &lane_audit_entries(&fixture, GRANT_AUDIT_ACTION)?.len(),
        &0,
        "stale generation-zero replay must append no grant audit",
    )
}
