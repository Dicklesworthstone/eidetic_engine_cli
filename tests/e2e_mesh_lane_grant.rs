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

use serde_json::Value;

type TestResult = Result<(), String>;

const TEST_LANE_ARG: &str = "graph-link";
const TEST_LANE_WIRE: &str = "graph_link";
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

fn write_mesh_policy_config(workspace: &Path, workspace_id: &str, peer_id: &str) -> TestResult {
    let config = format!(
        r#"[mesh]
enabled = true
command_mode = "cache"

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

    write_mesh_policy_config(tempdir.path(), &workspace_id, &peer_id)?;

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
fn bounded_stdin_grant_replay_and_revoke_are_generation_atomic() -> TestResult {
    let fixture = set_up_fixture("mutation")?;
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
    assert_no_bearer(
        serde_json::to_string(&grant_audits[0])
            .map_err(|error| format!("serialize grant audit: {error}"))?
            .as_bytes(),
        "grant audit row",
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
