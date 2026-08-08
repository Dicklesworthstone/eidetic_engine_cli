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

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use ee::config::MeshLane;
use ee::db::{
    CreateAuditInput, DbConnection, MeshLaneGrantAtomicError, MeshLaneGrantMutationError,
    MeshLaneGrantMutationInput, MeshLaneGrantTargetAdapter,
};
use ee::mesh::foreground_cli::{
    MESH_EXPORT_ARTIFACT_SCHEMA_V1, MeshCursorRow, MeshEventRow, MeshExportArtifact, MeshPeerRow,
    MeshStorageCounts,
};
use ee::mesh::lane_grant::{
    APPROVAL_TOKEN_ENVELOPE_LEN, APPROVAL_TOKEN_PREFIX, APPROVAL_TOKEN_TTL_SECONDS,
    ApprovalPurpose, compare_snapshot, issue, verify_authentic,
};
use ee::mesh::peer::build_peer_origin_node_id;
use ee::policy::store_auth::{StoreAuthRoot, workspace_keys_dir};
use serde_json::Value;

type TestResult = Result<(), String>;

const TEST_LANE_ARG: &str = "graph-link";
const TEST_LANE_WIRE: &str = "graph_link";
const TEST_MATERIAL_LANE_WIRE: &str = "graphLink";
const TEST_TAILSCALE_NODE_KEY: &str = "nodekey:lane-grant-e2e";
const GRANT_SCHEMA: &str = "ee.mesh.grant.v1";
const EXPORT_AUDIT_ACTION: &str = "mesh.audit.export";
const GRANT_AUDIT_ACTION: &str = "mesh.audit.lane_grant";
const REVOKE_AUDIT_ACTION: &str = "mesh.audit.lane_revoke";

struct LaneGrantFixture {
    _tempdir: tempfile::TempDir,
    workspace: String,
    workspace_id: String,
    peer_id: String,
    memory_id: String,
}

#[derive(Debug, Eq, PartialEq)]
struct MeshImportDurableSnapshot {
    peers: Vec<ee::db::StoredMeshPeer>,
    cursors: Vec<ee::db::StoredMeshPeerCursor>,
    ledger_events: Vec<ee::db::StoredMeshImportLedgerEvent>,
    index_jobs: Vec<ee::db::StoredSearchIndexJob>,
}

impl MeshImportDurableSnapshot {
    fn counts(&self) -> (usize, usize, usize, usize) {
        (
            self.peers.len(),
            self.cursors.len(),
            self.ledger_events.len(),
            self.index_jobs.len(),
        )
    }
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

fn tamper_approval_bearer(bearer: &str) -> Result<String, String> {
    let mut bytes = bearer.as_bytes().to_vec();
    let index = APPROVAL_TOKEN_PREFIX.len() + 20;
    let byte = bytes
        .get_mut(index)
        .ok_or_else(|| "approval bearer was too short for a one-byte tamper".to_owned())?;
    *byte = if *byte == b'A' { b'B' } else { b'A' };
    String::from_utf8(bytes).map_err(|error| format!("tampered bearer was not UTF-8: {error}"))
}

fn decoded_approval_envelope(bearer: &str) -> Result<Vec<u8>, String> {
    let encoded = bearer
        .strip_prefix(APPROVAL_TOKEN_PREFIX)
        .ok_or_else(|| "approval bearer omitted its expected prefix".to_owned())?;
    let envelope = URL_SAFE_NO_PAD
        .decode(encoded)
        .map_err(|error| format!("approval bearer failed base64url decoding: {error}"))?;
    ensure_equal(
        &envelope.len(),
        &APPROVAL_TOKEN_ENVELOPE_LEN,
        "decoded approval bearer envelope length",
    )?;
    Ok(envelope)
}

fn decoded_approval_bearer_omits_key_id(bearer: &str, key_id: &[u8]) -> TestResult {
    let envelope = decoded_approval_envelope(bearer)?;
    ensure(
        !envelope
            .windows(key_id.len())
            .any(|window| window == key_id),
        "decoded approval bearer envelope serialized a store-auth key ID",
    )
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
            TEST_TAILSCALE_NODE_KEY,
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
    let remember_json = success_json(&remember, "ee remember")?;
    let memory_id = json_string(&remember_json, "/data/memory_id", "ee remember")?;

    Ok(LaneGrantFixture {
        _tempdir: tempdir,
        workspace,
        workspace_id,
        peer_id,
        memory_id,
    })
}

fn fixture_hash(label: &str) -> String {
    format!("blake3:{}", blake3::hash(label.as_bytes()).to_hex())
}

fn canonical_mesh_event_json(value: &Value) -> Value {
    match value {
        Value::Array(values) => {
            Value::Array(values.iter().map(canonical_mesh_event_json).collect())
        }
        Value::Object(values) => {
            let mut keys = values.keys().collect::<Vec<_>>();
            keys.sort_unstable();
            let mut canonical = serde_json::Map::with_capacity(values.len());
            for key in keys {
                canonical.insert(key.clone(), canonical_mesh_event_json(&values[key]));
            }
            Value::Object(canonical)
        }
        _ => value.clone(),
    }
}

fn seal_mesh_event_json(event: &mut Value) -> Result<(String, String), String> {
    let mut hashable = event.clone();
    let object = hashable
        .as_object_mut()
        .ok_or_else(|| "mesh event fixture must be an object".to_owned())?;
    object.remove("eventHash");
    object.remove("eventId");
    let canonical = canonical_mesh_event_json(&hashable);
    let bytes = serde_json::to_vec(&canonical)
        .map_err(|error| format!("serialize canonical mesh event fixture: {error}"))?;
    let digest = blake3::hash(&bytes).to_hex().to_string();
    let event_hash = format!("blake3:{digest}");
    let event_id = format!("mesh_evt_{digest}");
    let object = event
        .as_object_mut()
        .ok_or_else(|| "mesh event fixture must be an object".to_owned())?;
    object.insert("eventHash".to_owned(), Value::String(event_hash.clone()));
    object.insert("eventId".to_owned(), Value::String(event_id.clone()));
    Ok((event_id, event_hash))
}

fn write_graph_link_artifact(
    fixture: &LaneGrantFixture,
    label: &str,
    seq: u64,
    prev_event_hash: Option<&str>,
) -> Result<(String, String, String), String> {
    let logical_memory_id = format!("mem_lane_grant_{label}_{seq:02}");
    let origin_node_id = build_peer_origin_node_id(TEST_TAILSCALE_NODE_KEY);
    let content_hash = fixture_hash(&format!("content:{label}:{seq}"));
    let produced_at = format!("2026-08-04T00:00:{:02}Z", seq.min(59));
    let mut event_json = serde_json::json!({
        "schema": "ee.mesh.event.v1",
        "eventId": format!("mesh_evt_{}", "0".repeat(64)),
        "originNodeId": origin_node_id.clone(),
        "originWorkspaceId": fixture.workspace_id.clone(),
        "producerPeerId": fixture.peer_id.clone(),
        "seq": seq,
        "prevEventHash": prev_event_hash,
        "eventHash": format!("blake3:{}", "0".repeat(64)),
        "eventKind": "create",
        "logicalMemoryId": logical_memory_id.clone(),
        "contentHash": content_hash.clone(),
        "bodyRef": null,
        "materialLane": TEST_MATERIAL_LANE_WIRE,
        "redactionClass": "metadataOnly",
        "trustLane": "peerAgent",
        "requiredFeatures": ["mesh.event.v1"],
        "producedAt": produced_at.clone(),
    });
    let (event_id, event_hash) = seal_mesh_event_json(&mut event_json)?;
    let event = MeshEventRow {
        event_id: event_id.clone(),
        origin_node_id,
        origin_workspace_id: fixture.workspace_id.clone(),
        producer_peer_id: Some(fixture.peer_id.clone()),
        seq,
        prev_event_hash: prev_event_hash.map(str::to_owned),
        event_hash: event_hash.clone(),
        event_kind: "create".to_owned(),
        logical_memory_id: logical_memory_id.clone(),
        content_hash,
        material_lane: TEST_MATERIAL_LANE_WIRE.to_owned(),
        redaction_class: "metadataOnly".to_owned(),
        trust_lane: "peerAgent".to_owned(),
        // The importer must ignore this transported claim and recompute policy
        // from the local enrollment, config snapshot, and durable grant state.
        import_decision: "allow".to_owned(),
        local_memory_id: None,
        body_cache_key: None,
        policy_failure_surface_json: None,
        policy_decision_json: None,
        event_json: serde_json::to_string(&canonical_mesh_event_json(&event_json))
            .map_err(|error| format!("serialize canonical mesh event fixture: {error}"))?,
        policy_attestation: None,
        imported_at: produced_at,
    };
    let artifact = MeshExportArtifact {
        schema: MESH_EXPORT_ARTIFACT_SCHEMA_V1.to_owned(),
        workspace_id: fixture.workspace_id.clone(),
        source: "ee mesh export".to_owned(),
        policy_attestation: None,
        storage: MeshStorageCounts {
            imported_event_count: 1,
            ..MeshStorageCounts::default()
        },
        peers: Vec::new(),
        cursors: Vec::new(),
        events: vec![event],
    };
    let path = Path::new(&fixture.workspace).join(format!("mesh-{label}-{seq}.json"));
    let rendered = serde_json::to_string_pretty(&artifact)
        .map_err(|error| format!("serialize {label} mesh artifact: {error}"))?;
    fs::write(&path, format!("{rendered}\n"))
        .map_err(|error| format!("write {}: {error}", path.display()))?;
    Ok((path.to_string_lossy().into_owned(), event_id, event_hash))
}

fn write_hash_bound_metadata_body_artifact(
    fixture: &LaneGrantFixture,
    label: &str,
    seq: u64,
) -> Result<(String, String, String), String> {
    let (path, _, _) = write_graph_link_artifact(fixture, label, seq, None)?;
    let mut artifact: MeshExportArtifact =
        serde_json::from_slice(&fs::read(&path).map_err(|error| format!("read {path}: {error}"))?)
            .map_err(|error| format!("parse {path}: {error}"))?;
    let event = artifact
        .events
        .first_mut()
        .ok_or_else(|| format!("{path} omitted its metadata event"))?;
    let mut event_json: Value = serde_json::from_str(&event.event_json)
        .map_err(|error| format!("parse canonical eventJson from {path}: {error}"))?;
    let body_uri = format!("ee-body://lane-grant-e2e/{label}/{seq}");
    let object = event_json
        .as_object_mut()
        .ok_or_else(|| format!("canonical eventJson from {path} was not an object"))?;
    object.insert(
        "bodyRef".to_owned(),
        serde_json::json!({
            "kind": "remoteAvailable",
            "uri": body_uri.clone(),
            "sizeBytes": 64,
        }),
    );
    object.insert(
        "trustClaim".to_owned(),
        serde_json::json!({
            "assertedBy": "lane-grant-e2e-peer",
        }),
    );
    object.insert(
        "materialLane".to_owned(),
        Value::String("metadata".to_owned()),
    );
    let (event_id, event_hash) = seal_mesh_event_json(&mut event_json)?;
    event.event_id = event_id.clone();
    event.event_hash = event_hash;
    event.material_lane = "metadata".to_owned();
    event.event_json = serde_json::to_string(&canonical_mesh_event_json(&event_json))
        .map_err(|error| format!("serialize canonical metadata event fixture: {error}"))?;

    let rendered = serde_json::to_string_pretty(&artifact)
        .map_err(|error| format!("serialize {label} metadata mesh artifact: {error}"))?;
    fs::write(&path, format!("{rendered}\n"))
        .map_err(|error| format!("rewrite {path}: {error}"))?;
    Ok((path, event_id, body_uri))
}

fn write_disabled_import_effect_artifact(
    fixture: &LaneGrantFixture,
    label: &str,
    seq: u64,
) -> Result<(String, String, String), String> {
    let (path, _, _) = write_graph_link_artifact(fixture, label, seq, None)?;
    let mut artifact: MeshExportArtifact =
        serde_json::from_slice(&fs::read(&path).map_err(|error| format!("read {path}: {error}"))?)
            .map_err(|error| format!("parse {path}: {error}"))?;
    let event = artifact
        .events
        .first_mut()
        .ok_or_else(|| format!("{path} omitted its import-effect event"))?;
    let mut event_json: Value = serde_json::from_str(&event.event_json)
        .map_err(|error| format!("parse canonical eventJson from {path}: {error}"))?;
    event_json
        .as_object_mut()
        .ok_or_else(|| format!("canonical eventJson from {path} was not an object"))?
        .insert(
            "materialLane".to_owned(),
            Value::String("metadata".to_owned()),
        );
    let (event_id, event_hash) = seal_mesh_event_json(&mut event_json)?;
    event.event_id = event_id.clone();
    event.event_hash = event_hash.clone();
    event.material_lane = "metadata".to_owned();
    event.event_json = serde_json::to_string(&canonical_mesh_event_json(&event_json))
        .map_err(|error| format!("serialize canonical import-effect event fixture: {error}"))?;

    let candidate_peer_id = "peer_disabled_import_candidate".to_owned();
    artifact.peers = vec![MeshPeerRow {
        peer_id: candidate_peer_id.clone(),
        origin_node_id: "node_disabled_import_candidate".to_owned(),
        display_name: Some("disabled import candidate".to_owned()),
        enabled: true,
        last_seen_at: "2026-08-04T00:01:00Z".to_owned(),
        policy_summary_json: None,
    }];
    artifact.cursors = vec![MeshCursorRow {
        peer_id: fixture.peer_id.clone(),
        origin_node_id: build_peer_origin_node_id(TEST_TAILSCALE_NODE_KEY),
        origin_workspace_id: fixture.workspace_id.clone(),
        last_seq: seq,
        tip_event_hash: Some(event_hash),
        tip_audit_hash: None,
        status: "active".to_owned(),
        updated_at: "2026-08-04T00:01:00Z".to_owned(),
    }];
    artifact.storage.peer_count = 1;
    artifact.storage.cursor_count = 1;

    let rendered = serde_json::to_string_pretty(&artifact)
        .map_err(|error| format!("serialize {label} import-effect artifact: {error}"))?;
    fs::write(&path, format!("{rendered}\n"))
        .map_err(|error| format!("rewrite {path}: {error}"))?;
    Ok((path, event_id, candidate_peer_id))
}

fn stored_graph_link_event(
    fixture: &LaneGrantFixture,
    seq: u64,
) -> Result<ee::db::StoredMeshImportLedgerEvent, String> {
    let database_path = Path::new(&fixture.workspace).join(".ee").join("ee.db");
    let connection = DbConnection::open_file(&database_path)
        .map_err(|error| format!("open {}: {error}", database_path.display()))?;
    connection
        .get_mesh_import_ledger_event(
            &fixture.workspace_id,
            &build_peer_origin_node_id(TEST_TAILSCALE_NODE_KEY),
            &fixture.workspace_id,
            seq,
        )
        .map_err(|error| format!("load imported graph-link event {seq}: {error}"))?
        .ok_or_else(|| format!("imported graph-link event {seq} was not persisted"))
}

fn matching_import_job_count(fixture: &LaneGrantFixture, event_id: &str) -> Result<usize, String> {
    let database_path = Path::new(&fixture.workspace).join(".ee").join("ee.db");
    let connection = DbConnection::open_file(&database_path)
        .map_err(|error| format!("open {}: {error}", database_path.display()))?;
    let jobs = connection
        .list_search_index_jobs(&fixture.workspace_id, None)
        .map_err(|error| format!("list search-index jobs for {event_id}: {error}"))?;
    Ok(jobs
        .iter()
        .filter(|job| {
            job.document_source.as_deref() == Some("import")
                && job.document_id.as_deref() == Some(event_id)
        })
        .count())
}

fn mesh_import_durable_snapshot(
    fixture: &LaneGrantFixture,
) -> Result<MeshImportDurableSnapshot, String> {
    let database_path = Path::new(&fixture.workspace).join(".ee").join("ee.db");
    let connection = DbConnection::open_file(&database_path)
        .map_err(|error| format!("open {}: {error}", database_path.display()))?;
    Ok(MeshImportDurableSnapshot {
        peers: connection
            .list_mesh_peers(&fixture.workspace_id)
            .map_err(|error| format!("snapshot mesh peers: {error}"))?,
        cursors: connection
            .list_mesh_peer_cursors(&fixture.workspace_id)
            .map_err(|error| format!("snapshot mesh cursors: {error}"))?,
        ledger_events: connection
            .list_mesh_import_ledger_events_for_workspace(&fixture.workspace_id)
            .map_err(|error| format!("snapshot mesh import ledger: {error}"))?,
        index_jobs: connection
            .list_search_index_jobs(&fixture.workspace_id, None)
            .map_err(|error| format!("snapshot search-index jobs: {error}"))?,
    })
}

fn fixture_workspace_generation(fixture: &LaneGrantFixture) -> Result<u64, String> {
    let database_path = Path::new(&fixture.workspace).join(".ee").join("ee.db");
    let connection = DbConnection::open_file(&database_path)
        .map_err(|error| format!("open {}: {error}", database_path.display()))?;
    connection
        .get_workspace_generation(&fixture.workspace_id)
        .map_err(|error| format!("load workspace generation: {error}"))
        .map(|generation| generation.unwrap_or(0))
}

fn mesh_export_audit_count(fixture: &LaneGrantFixture) -> Result<usize, String> {
    let database_path = Path::new(&fixture.workspace).join(".ee").join("ee.db");
    let connection = DbConnection::open_file(&database_path)
        .map_err(|error| format!("open {}: {error}", database_path.display()))?;
    connection
        .list_audit_by_action(EXPORT_AUDIT_ACTION, None)
        .map(|entries| entries.len())
        .map_err(|error| format!("list mesh export audit rows: {error}"))
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
        &Some("ee.response.v2"),
        "audit timeline envelope schema",
    )?;
    ensure_equal(
        &json.pointer("/data/schema").and_then(Value::as_str),
        &Some("ee.audit.timeline.v1"),
        "audit timeline schema",
    )?;
    json.pointer("/data/entries")
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
    ensure_equal(
        &ordinary
            .pointer("/candidateSet/0/candidateKind")
            .and_then(Value::as_str),
        &Some("memory"),
        "ordinary preview memory candidate kind",
    )?;
    ensure_equal(
        &ordinary
            .pointer("/candidateSet/0/candidateId")
            .and_then(Value::as_str),
        &Some(fixture.memory_id.as_str()),
        "ordinary preview memory candidate id",
    )?;
    ensure_equal(
        &json_u64(&ordinary, "/affectedLedgerEventCount", "ordinary preview")?,
        &0,
        "ordinary preview has no mesh-ledger candidates",
    )?;
    ensure(
        ordinary
            .pointer("/redactionScannerGeneration")
            .and_then(Value::as_str)
            .is_some_and(|generation| {
                generation.starts_with("redscan1_") && generation.len() == "redscan1_".len() + 64
            }),
        format!("ordinary preview omitted the source-derived scanner generation: {ordinary}"),
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
    let bearer = sensitive_json_string(&issued, "/approvalToken/value", "explicit token preview")?;
    ensure(
        bearer.starts_with("eeap1_") && bearer.len() < 512,
        "explicit preview bearer must use the bounded eeap1_ envelope",
    )?;
    ensure_equal(
        &issued
            .pointer("/approvalToken/handling")
            .and_then(Value::as_str),
        &Some("secret"),
        "explicit preview token handling marker",
    )?;
    let mut token_fields = issued
        .pointer("/approvalToken")
        .and_then(Value::as_object)
        .ok_or_else(|| "explicit preview approvalToken must be an object".to_owned())?
        .keys()
        .map(String::as_str)
        .collect::<Vec<_>>();
    token_fields.sort_unstable();
    let expected_token_fields = vec!["expiresAt", "handling", "schema", "value"];
    ensure_equal(
        &token_fields,
        &expected_token_fields,
        "explicit preview token closed field set",
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
fn approval_token_issuance_rejects_non_json_machine_renderers() -> TestResult {
    let fixture = set_up_fixture("preview-token-renderers")?;

    for renderer in ["hook", "jsonl", "compact"] {
        let output = run_ee(
            &fixture.workspace,
            &[
                "mesh",
                "preview-grant",
                fixture.peer_id.as_str(),
                "--lane",
                TEST_LANE_ARG,
                "--issue-approval-token",
                "--format",
                renderer,
            ],
        )?;
        ensure(
            !output.status.success(),
            format!("{renderer} token issuance unexpectedly succeeded"),
        )?;
        assert_no_bearer(&output.stdout, &format!("{renderer} token issuance stdout"))?;
        assert_no_bearer(&output.stderr, &format!("{renderer} token issuance stderr"))?;
        let json = stdout_json(&output, &format!("{renderer} token issuance"))?;
        ensure_equal(
            &json.pointer("/error/code").and_then(Value::as_str),
            &Some("usage"),
            &format!("{renderer} token issuance error code"),
        )?;
        ensure(
            json.pointer("/error/message")
                .and_then(Value::as_str)
                .is_some_and(|message| message.contains("only with --json")),
            format!("{renderer} token issuance error did not explain JSON-only output: {json}"),
        )?;
    }

    ensure_equal(
        &lane_audit_entries(&fixture, GRANT_AUDIT_ACTION)?.len(),
        &0,
        "rejected renderer token previews must not append grant audits",
    )
}

#[test]
fn lane_grant_rejects_non_json_machine_renderers_without_confirmation_or_effects() -> TestResult {
    let fixture = set_up_fixture("grant-machine-renderers")?;

    for renderer in ["hook", "jsonl", "compact"] {
        for token_stdin in [false, true] {
            let mut args = vec![
                "mesh",
                "grant",
                fixture.peer_id.as_str(),
                "--lane",
                TEST_LANE_ARG,
                "--format",
                renderer,
            ];
            if token_stdin {
                args.push("--preview-token-stdin");
            }
            let output = run_ee_with_stdin(&fixture.workspace, &args, b"yes\n")?;
            ensure(
                !output.status.success(),
                format!(
                    "{renderer} grant unexpectedly entered a mutation flow (token stdin: {token_stdin})"
                ),
            )?;
            assert_no_bearer(&output.stdout, &format!("{renderer} grant stdout"))?;
            assert_no_bearer(&output.stderr, &format!("{renderer} grant stderr"))?;
            let json = stdout_json(&output, &format!("{renderer} grant rejection"))?;
            ensure_equal(
                &json.pointer("/error/code").and_then(Value::as_str),
                &Some("usage"),
                &format!("{renderer} grant error code"),
            )?;
            ensure(
                json.pointer("/error/message")
                    .and_then(Value::as_str)
                    .is_some_and(|message| {
                        message.contains("only human confirmation or --json bearer submission")
                    }),
                format!("{renderer} grant error did not explain the closed renderer set: {json}"),
            )?;
        }
    }

    ensure_equal(
        &lane_audit_entries(&fixture, GRANT_AUDIT_ACTION)?.len(),
        &0,
        "rejected renderer grants must not append grant audits",
    )?;
    let (_, preview_json) = preview(&fixture, false)?;
    ensure_equal(
        &json_u64(
            &preview_payload(&preview_json, "post-renderer-rejection preview")?,
            "/grantGeneration",
            "post-renderer-rejection preview",
        )?,
        &0,
        "rejected renderer grants must not advance consent generation",
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
            .is_some_and(|repair| {
                repair.contains("[[mesh.peer_group_bindings]]")
                    && repair.contains("fresh approval")
                    && repair.contains("membership alone does not grant")
            }),
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
            "  affected ledger events: {}",
            json_u64(&snapshot, "/affectedLedgerEventCount", "JSON preview")?
        ),
        format!(
            "  redacted from exposure: {}",
            json_u64(&snapshot, "/redactedFromExposureCount", "JSON preview")?
        ),
        format!(
            "  redaction scanner generation: {}",
            json_string(&snapshot, "/redactionScannerGeneration", "JSON preview")?
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
        ApprovalPurpose::Lane,
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
fn config_byte_or_parse_drift_after_preview_stales_bearer_with_zero_effect() -> TestResult {
    let fixture = set_up_fixture("config-drift-before-grant")?;
    let (_, issued_json) = preview(&fixture, true)?;
    let bearer = sensitive_json_string(
        &issued_json,
        "/data/preview/approvalToken/value",
        "pre-config-drift approval preview",
    )?;
    let config_path = Path::new(&fixture.workspace)
        .join(".ee")
        .join("config.toml");
    let original_config_bytes = fs::read(&config_path)
        .map_err(|error| format!("read pre-drift lane-grant config: {error}"))?;
    let mut drifted_config_bytes = original_config_bytes.clone();
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

    fs::write(&config_path, b"[mesh\n")
        .map_err(|error| format!("write malformed lane-grant config: {error}"))?;
    let malformed_rejected = run_ee_with_stdin(
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
        &malformed_rejected,
        "malformed-config approval bearer",
        "mesh_approval_token_stale",
        "warning",
        "authentic but its approved preview is stale",
    )?;

    fs::write(
        &config_path,
        b"[mesh]\nenabled = true\ncommand_mode = \"cache\"\n",
    )
    .map_err(|error| format!("write valid config without peer policy: {error}"))?;
    let missing_policy_rejected = run_ee_with_stdin(
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
        &missing_policy_rejected,
        "missing-policy approval bearer",
        "mesh_approval_token_stale",
        "warning",
        "authentic but its approved preview is stale",
    )?;

    fs::write(&config_path, original_config_bytes)
        .map_err(|error| format!("restore lane-grant config after drift checks: {error}"))?;

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
        "/data/preview/approvalToken/value",
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
        ApprovalPurpose::Lane,
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
            let prepared: Result<_, String> = (|| {
                let root = StoreAuthRoot::open(&keys_dir)
                    .map_err(|error| format!("{label}: open store-auth root: {error}"))?;
                let authenticated = verify_authentic(
                    &root,
                    ApprovalPurpose::Lane,
                    &workspace_id,
                    GRANT_SCHEMA,
                    &bearer,
                    approval_now,
                )
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
                            &ee::db::generate_audit_id(),
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
                    Ok::<(), String>(())
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
    let (_, issued_json) = preview(&fixture, true)?;
    let bearer = sensitive_json_string(
        &issued_json,
        "/data/preview/approvalToken/value",
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
    let serialized_grant_audit = serde_json::to_string(&grant_audits[0])
        .map_err(|error| format!("serialize grant audit: {error}"))?;
    assert_no_bearer(serialized_grant_audit.as_bytes(), "grant audit row")?;
    ensure(
        !serialized_grant_audit.contains("approval_config_digest"),
        "grant audit must persist only the opaque approval ID, not a config fingerprint",
    )?;
    ensure(
        !serialized_grant_audit.contains("[mesh]")
            && !serialized_grant_audit.contains("command_mode"),
        "grant audit must never persist raw config bytes",
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
        "/data/preview/approvalToken/value",
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
        "/data/preview/approvalToken/value",
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
            TEST_TAILSCALE_NODE_KEY,
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
        "/data/preview/approvalToken/value",
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
            TEST_TAILSCALE_NODE_KEY,
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

#[test]
fn mesh_import_rejects_unconsented_control_rows_and_surfaces_stable_counts() -> TestResult {
    let fixture = set_up_fixture("control-row-consent")?;
    let (artifact_path, event_id, candidate_peer_id) =
        write_disabled_import_effect_artifact(&fixture, "control-row-consent", 1)?;
    let mut artifact: MeshExportArtifact = serde_json::from_slice(
        &fs::read(&artifact_path).map_err(|error| format!("read control-row artifact: {error}"))?,
    )
    .map_err(|error| format!("parse control-row artifact: {error}"))?;
    artifact.cursors[0].peer_id = candidate_peer_id.clone();
    artifact.cursors[0].origin_node_id = "node_disabled_import_candidate".to_owned();
    let rendered = serde_json::to_string_pretty(&artifact)
        .map_err(|error| format!("serialize control-row artifact: {error}"))?;
    fs::write(&artifact_path, format!("{rendered}\n"))
        .map_err(|error| format!("rewrite control-row artifact: {error}"))?;

    let imported = run_ee(
        &fixture.workspace,
        &["mesh", "import", "--file", artifact_path.as_str(), "--json"],
    )?;
    let response = success_json(&imported, "control-row ee mesh import")?;
    ensure_equal(
        &response.pointer("/data/schema").and_then(Value::as_str),
        &Some("ee.mesh.cli.import.v2"),
        "control-row import schema",
    )?;
    for (pointer, expected, label) in [
        ("/data/importedPeerCount", 0, "imported peer count"),
        ("/data/importedCursorCount", 0, "imported cursor count"),
        ("/data/importedEventCount", 1, "imported event count"),
        ("/data/rejectedPeerCount", 1, "rejected peer count"),
        ("/data/rejectedCursorCount", 1, "rejected cursor count"),
    ] {
        ensure_equal(
            &json_u64(&response, pointer, "control-row import report")?,
            &expected,
            label,
        )?;
    }
    for pointer in ["/degraded", "/data/degraded"] {
        let codes = response
            .pointer(pointer)
            .and_then(Value::as_array)
            .ok_or_else(|| format!("control-row response omitted {pointer}: {response}"))?
            .iter()
            .filter_map(|item| item.pointer("/code").and_then(Value::as_str))
            .collect::<std::collections::BTreeSet<_>>();
        ensure(
            codes.contains("mesh_import_peer_not_consented")
                && codes.contains("mesh_import_cursor_unverified"),
            format!("control-row response {pointer} omitted rejection codes: {codes:?}"),
        )?;
    }

    let durable = mesh_import_durable_snapshot(&fixture)?;
    ensure(
        durable
            .peers
            .iter()
            .all(|peer| peer.peer_id != candidate_peer_id),
        "artifact must not create an unconsented peer",
    )?;
    ensure(
        durable.cursors.is_empty(),
        "artifact must not create a cursor for an unconsented peer",
    )?;
    ensure(
        durable
            .ledger_events
            .iter()
            .any(|event| event.event_id == event_id),
        "event replay remains independently ledgered",
    )
}

#[test]
fn disabled_mesh_import_is_policy_denied_with_zero_durable_effects() -> TestResult {
    let fixture = set_up_fixture("disabled-import")?;
    let (artifact_path, event_id, candidate_peer_id) =
        write_disabled_import_effect_artifact(&fixture, "disabled-import", 1)?;
    let config_path = Path::new(&fixture.workspace)
        .join(".ee")
        .join("config.toml");
    let enabled_config = fs::read_to_string(&config_path)
        .map_err(|error| format!("read enabled mesh config: {error}"))?;
    let disabled_config = enabled_config.replacen("enabled = true", "enabled = false", 1);
    ensure(
        disabled_config != enabled_config,
        "disabled-import fixture config did not contain the enabled mesh sentinel",
    )?;
    fs::write(&config_path, disabled_config)
        .map_err(|error| format!("write disabled mesh config: {error}"))?;

    let before = mesh_import_durable_snapshot(&fixture)?;
    ensure(
        before
            .peers
            .iter()
            .all(|peer| peer.peer_id != candidate_peer_id),
        "disabled-import candidate peer unexpectedly existed before replay",
    )?;
    ensure(
        before
            .ledger_events
            .iter()
            .all(|event| event.event_id != event_id),
        "disabled-import event unexpectedly existed before replay",
    )?;
    ensure(
        before
            .index_jobs
            .iter()
            .all(|job| job.document_id.as_deref() != Some(event_id.as_str())),
        "disabled-import event unexpectedly had an index job before replay",
    )?;

    // The common E2E command helper explicitly enables mesh so unrelated lane
    // tests exercise production paths. Remove only that test override here so
    // this process observes the disabled workspace config snapshot.
    let mut command = ee_command(
        &fixture.workspace,
        &["mesh", "import", "--file", artifact_path.as_str(), "--json"],
    );
    command.env_remove("EE_MESH_ENABLED");
    let denied = command
        .output()
        .map_err(|error| format!("run disabled ee mesh import: {error}"))?;
    ensure(
        !denied.status.success(),
        "disabled ee mesh import unexpectedly succeeded",
    )?;
    ensure_json_stderr_empty(&denied, "disabled ee mesh import")?;
    assert_no_bearer(&denied.stdout, "disabled ee mesh import stdout")?;
    assert_no_bearer(&denied.stderr, "disabled ee mesh import stderr")?;
    let denied_json = stdout_json(&denied, "disabled ee mesh import")?;
    ensure_equal(
        &denied_json.pointer("/schema").and_then(Value::as_str),
        &Some("ee.error.v2"),
        "disabled mesh import error schema",
    )?;
    ensure_equal(
        &denied_json.pointer("/error/code").and_then(Value::as_str),
        &Some("policy_denied"),
        "disabled mesh import error code",
    )?;
    ensure(
        denied_json
            .pointer("/error/message")
            .and_then(Value::as_str)
            .is_some_and(|message| {
                message.contains("Mesh is disabled") && message.contains("import is denied")
            }),
        format!("disabled mesh import error omitted the containment reason: {denied_json}"),
    )?;

    let after = mesh_import_durable_snapshot(&fixture)?;
    ensure_equal(
        &after.counts(),
        &before.counts(),
        "disabled import peer/cursor/ledger/index counts",
    )?;
    ensure_equal(
        &after,
        &before,
        "disabled import durable peer/cursor/ledger/index sentinels",
    )
}

#[test]
fn mesh_export_rejects_unknown_and_disabled_targets_without_effects() -> TestResult {
    let fixture = set_up_fixture("export-target-gate")?;
    let output_path = Path::new(&fixture.workspace).join("guarded-mesh-export.json");
    let sentinel = b"existing export target bytes\n";
    fs::write(&output_path, sentinel)
        .map_err(|error| format!("write guarded export sentinel: {error}"))?;
    let output_arg = output_path.to_string_lossy().into_owned();

    let missing_flag = run_ee(
        &fixture.workspace,
        &["mesh", "export", "--out", output_arg.as_str(), "--json"],
    )?;
    ensure(
        !missing_flag.status.success(),
        "mesh export without --peer must fail argument parsing",
    )?;
    ensure_json_stderr_empty(&missing_flag, "missing mesh export peer")?;
    let missing_flag_json = stdout_json(&missing_flag, "missing mesh export peer")?;
    ensure_equal(
        &missing_flag_json.pointer("/schema").and_then(Value::as_str),
        &Some("ee.error.v2"),
        "missing-peer parse error schema",
    )?;
    ensure_equal(
        &missing_flag_json
            .pointer("/error/code")
            .and_then(Value::as_str),
        &Some("usage"),
        "missing-peer parse error code",
    )?;
    ensure(
        missing_flag_json
            .pointer("/error/message")
            .and_then(Value::as_str)
            .is_some_and(|message| {
                message.contains("the following required arguments were not provided:")
                    && message.contains("--peer <PEER_ID>")
            }),
        format!("missing-peer parse failure must identify the required flag: {missing_flag_json}"),
    )?;
    ensure_equal(
        &fs::read(&output_path)
            .map_err(|error| format!("read guarded export sentinel: {error}"))?,
        &sentinel.to_vec(),
        "missing-peer export output bytes",
    )?;
    ensure_equal(
        &mesh_export_audit_count(&fixture)?,
        &0,
        "missing-peer export audit count",
    )?;

    let unknown_peer = format!("{}-unknown", fixture.peer_id);
    let unknown = run_ee(
        &fixture.workspace,
        &[
            "mesh",
            "export",
            "--peer",
            unknown_peer.as_str(),
            "--out",
            output_arg.as_str(),
            "--json",
        ],
    )?;
    ensure(!unknown.status.success(), "unknown export peer must fail")?;
    ensure_json_stderr_empty(&unknown, "unknown mesh export target")?;
    let unknown_json = stdout_json(&unknown, "unknown mesh export target")?;
    ensure(
        unknown_json
            .pointer("/error/message")
            .and_then(Value::as_str)
            .is_some_and(|message| message.contains("No enrolled mesh peer found")),
        format!("unknown export peer error was not canonical: {unknown_json}"),
    )?;
    ensure_equal(
        &fs::read(&output_path)
            .map_err(|error| format!("read guarded export sentinel: {error}"))?,
        &sentinel.to_vec(),
        "unknown-peer export output bytes",
    )?;
    ensure_equal(
        &mesh_export_audit_count(&fixture)?,
        &0,
        "unknown-peer export audit count",
    )?;

    let revoke = run_ee(
        &fixture.workspace,
        &["mesh", "peer", "revoke", fixture.peer_id.as_str(), "--json"],
    )?;
    success_json(&revoke, "disable mesh export target")?;
    let disabled = run_ee(
        &fixture.workspace,
        &[
            "mesh",
            "export",
            "--peer",
            fixture.peer_id.as_str(),
            "--out",
            output_arg.as_str(),
            "--json",
        ],
    )?;
    ensure(!disabled.status.success(), "disabled export peer must fail")?;
    ensure_json_stderr_empty(&disabled, "disabled mesh export target")?;
    let disabled_json = stdout_json(&disabled, "disabled mesh export target")?;
    ensure(
        disabled_json
            .pointer("/error/message")
            .and_then(Value::as_str)
            .is_some_and(|message| message.contains("mesh export is denied")),
        format!("disabled export peer error was not canonical: {disabled_json}"),
    )?;
    ensure_equal(
        &fs::read(&output_path)
            .map_err(|error| format!("read guarded export sentinel: {error}"))?,
        &sentinel.to_vec(),
        "disabled-peer export output bytes",
    )?;
    ensure_equal(
        &mesh_export_audit_count(&fixture)?,
        &0,
        "disabled-peer export audit count",
    )
}

#[test]
fn body_grant_pins_and_releases_hash_bound_metadata_body_fields() -> TestResult {
    let fixture = set_up_fixture("metadata-body-boundary")?;
    let (artifact_path, event_id, body_uri) =
        write_hash_bound_metadata_body_artifact(&fixture, "metadata-body", 1)?;

    // The production importer validates and preserves this canonical event,
    // but local policy denies its arbitrary body-bearing metadata until the
    // body lane is explicitly approved.
    let import = run_ee(
        &fixture.workspace,
        &["mesh", "import", "--file", artifact_path.as_str(), "--json"],
    )?;
    let import_json = success_json(&import, "metadata-body ee mesh import")?;
    ensure_equal(
        &json_u64(
            &import_json,
            "/data/importedEventCount",
            "metadata-body import report",
        )?,
        &1,
        "metadata-body import ledger count",
    )?;
    let stored = stored_graph_link_event(&fixture, 1)?;
    ensure_equal(
        &stored.event_id,
        &event_id,
        "metadata-body durable event identity",
    )?;
    ensure_equal(
        &stored.import_decision.as_str(),
        &"deny",
        "metadata-body event requires explicit body authority",
    )?;
    ensure_equal(
        &matching_import_job_count(&fixture, &event_id)?,
        &0,
        "denied metadata-body event index job count",
    )?;

    let before_grant = run_ee(
        &fixture.workspace,
        &[
            "mesh",
            "export",
            "--peer",
            fixture.peer_id.as_str(),
            "--json",
        ],
    )?;
    let before_grant_json = success_json(&before_grant, "pre-body-grant ee mesh export")?;
    ensure_equal(
        &json_u64(
            &before_grant_json,
            "/data/eventCount",
            "pre-body-grant export report",
        )?,
        &0,
        "metadata-only policy must not export hash-bound body fields",
    )?;

    // The explicit token authenticates the exact body-lane preview. Its
    // complete ledger candidate projection pins the immutable event identity
    // and an opaque revision without disclosing its URI or authority claim.
    let issued = run_ee(
        &fixture.workspace,
        &[
            "mesh",
            "preview-grant",
            fixture.peer_id.as_str(),
            "--lane",
            "body",
            "--issue-approval-token",
            "--json",
        ],
    )?;
    let issued_json = success_json(&issued, "metadata-body approval preview")?;
    let issued_preview = preview_payload(&issued_json, "metadata-body approval preview")?;
    ensure_equal(
        &issued_preview.pointer("/lane").and_then(Value::as_str),
        &Some("body"),
        "metadata-body approval lane",
    )?;
    ensure_equal(
        &json_u64(
            &issued_preview,
            "/affectedLedgerEventCount",
            "metadata-body approval preview",
        )?,
        &1,
        "metadata-body preview ledger-event count",
    )?;
    let candidates = issued_preview
        .pointer("/candidateSet")
        .and_then(Value::as_array)
        .ok_or_else(|| format!("metadata-body preview omitted candidateSet: {issued_preview}"))?;
    let ledger_pins = candidates
        .iter()
        .filter(|candidate| {
            candidate.pointer("/candidateKind").and_then(Value::as_str) == Some("mesh_ledger_event")
        })
        .collect::<Vec<_>>();
    ensure_equal(
        &ledger_pins.len(),
        &1,
        "metadata-body complete ledger candidate pins",
    )?;
    let ledger_pin = ledger_pins[0];
    ensure_equal(
        &ledger_pin.pointer("/candidateId").and_then(Value::as_str),
        &Some(event_id.as_str()),
        "metadata-body ledger candidate identity",
    )?;
    ensure(
        ledger_pin
            .pointer("/revisionId")
            .and_then(Value::as_str)
            .is_some_and(|revision| revision.starts_with("revme1_")),
        format!("metadata-body candidate omitted its opaque revision pin: {ledger_pin}"),
    )?;
    let mut pin_fields = ledger_pin
        .as_object()
        .ok_or_else(|| "metadata-body ledger pin was not an object".to_owned())?
        .keys()
        .map(String::as_str)
        .collect::<Vec<_>>();
    pin_fields.sort_unstable();
    ensure_equal(
        &pin_fields,
        &vec!["candidateId", "candidateKind", "revisionId"],
        "metadata-body ledger pin closed field set",
    )?;
    let serialized_preview = serde_json::to_string(&issued_preview)
        .map_err(|error| format!("serialize metadata-body approval preview: {error}"))?;
    ensure(
        !serialized_preview.contains(&body_uri)
            && !serialized_preview.contains("trustClaim")
            && !serialized_preview.contains("eventJson"),
        "metadata-body preview must expose only opaque ledger pins",
    )?;

    let ordinary = run_ee(
        &fixture.workspace,
        &[
            "mesh",
            "preview-grant",
            fixture.peer_id.as_str(),
            "--lane",
            "body",
            "--json",
        ],
    )?;
    let ordinary_json = success_json(&ordinary, "token-free metadata-body preview")?;
    ensure_equal(
        &canonical_preview_bytes(&issued_json, "issued metadata-body preview")?,
        &canonical_preview_bytes(&ordinary_json, "token-free metadata-body preview")?,
        "approval token exact preview binding",
    )?;
    let prior_key_bearer = sensitive_json_string(
        &issued_json,
        "/data/preview/approvalToken/value",
        "metadata-body approval preview",
    )?;
    let baseline_preview = preview_payload(&ordinary_json, "body approval rejection baseline")?;
    let canonical_body_preview =
        canonical_preview_bytes(&ordinary_json, "body-lane approval domain check")?;

    let keys_dir = workspace_keys_dir(Path::new(&fixture.workspace));
    let root = StoreAuthRoot::open(&keys_dir)
        .map_err(|error| format!("open body-lane approval store root: {error}"))?;
    let approval_now = chrono::Utc::now().timestamp();
    let lane_authenticated = verify_authentic(
        &root,
        ApprovalPurpose::Lane,
        &fixture.workspace_id,
        GRANT_SCHEMA,
        &prior_key_bearer,
        approval_now,
    )
    .map_err(|error| format!("authenticate body-lane bearer in lane domain: {error}"))?;
    compare_snapshot(
        &root,
        &lane_authenticated,
        &canonical_body_preview,
        approval_now,
    )
    .map_err(|error| format!("verify body-lane snapshot in lane domain: {error}"))?;
    decoded_approval_bearer_omits_key_id(&prior_key_bearer, root.current_key_id().as_bytes())?;
    drop(root);

    let tampered_bearer = tamper_approval_bearer(&prior_key_bearer)?;
    let tampered = run_ee_with_stdin(
        &fixture.workspace,
        &[
            "mesh",
            "grant",
            fixture.peer_id.as_str(),
            "--lane",
            "body",
            "--preview-token-stdin",
            "--json",
        ],
        format!("{tampered_bearer}\n").as_bytes(),
    )?;
    approval_error_json(
        &tampered,
        "tampered body approval bearer",
        "mesh_approval_token_invalid",
        "high",
        "invalid for this store, workspace, and command",
    )?;
    let tampered_after = run_ee(
        &fixture.workspace,
        &[
            "mesh",
            "preview-grant",
            fixture.peer_id.as_str(),
            "--lane",
            "body",
            "--json",
        ],
    )?;
    let tampered_after_json = success_json(&tampered_after, "post-tamper body preview")?;
    ensure_equal(
        &preview_payload(&tampered_after_json, "post-tamper body preview")?,
        &baseline_preview,
        "tampered body approval must leave persisted consent state unchanged",
    )?;
    ensure_equal(
        &lane_audit_entries(&fixture, GRANT_AUDIT_ACTION)?.len(),
        &0,
        "tampered body approval must append no grant audit",
    )?;

    let mut root = StoreAuthRoot::open(&keys_dir)
        .map_err(|error| format!("open body-approval store root for rotation: {error}"))?;
    let prior_key_id = root.current_key_id();
    let current_key_id = root
        .rotate()
        .map_err(|error| format!("rotate body-approval store root: {error}"))?;
    ensure(
        prior_key_id != current_key_id,
        "body-approval rotation must install a distinct current key",
    )?;
    decoded_approval_bearer_omits_key_id(&prior_key_bearer, prior_key_id.as_bytes())?;
    decoded_approval_bearer_omits_key_id(&prior_key_bearer, current_key_id.as_bytes())?;
    drop(root);

    let prior_key = run_ee_with_stdin(
        &fixture.workspace,
        &[
            "mesh",
            "grant",
            fixture.peer_id.as_str(),
            "--lane",
            "body",
            "--preview-token-stdin",
            "--json",
        ],
        format!("{prior_key_bearer}\n").as_bytes(),
    )?;
    approval_error_json(
        &prior_key,
        "prior-key body approval bearer",
        "mesh_approval_token_invalid",
        "high",
        "invalid for this store, workspace, and command",
    )?;
    let prior_key_after = run_ee(
        &fixture.workspace,
        &[
            "mesh",
            "preview-grant",
            fixture.peer_id.as_str(),
            "--lane",
            "body",
            "--json",
        ],
    )?;
    let prior_key_after_json = success_json(&prior_key_after, "post-prior-key body preview")?;
    ensure_equal(
        &preview_payload(&prior_key_after_json, "post-prior-key body preview")?,
        &baseline_preview,
        "prior-key body approval must leave persisted consent state unchanged",
    )?;
    ensure_equal(
        &lane_audit_entries(&fixture, GRANT_AUDIT_ACTION)?.len(),
        &0,
        "prior-key body approval must append no grant audit",
    )?;

    let refreshed = run_ee(
        &fixture.workspace,
        &[
            "mesh",
            "preview-grant",
            fixture.peer_id.as_str(),
            "--lane",
            "body",
            "--issue-approval-token",
            "--json",
        ],
    )?;
    let refreshed_json = success_json(&refreshed, "current-key body approval preview")?;
    let bearer = sensitive_json_string(
        &refreshed_json,
        "/data/preview/approvalToken/value",
        "current-key body approval preview",
    )?;
    let grant = run_ee_with_stdin(
        &fixture.workspace,
        &[
            "mesh",
            "grant",
            fixture.peer_id.as_str(),
            "--lane",
            "body",
            "--preview-token-stdin",
            "--json",
        ],
        format!("{bearer}\n").as_bytes(),
    )?;
    let grant_json = success_json(&grant, "metadata-body ee mesh grant")?;
    assert_no_bearer(&grant.stdout, "metadata-body grant stdout")?;
    assert_no_bearer(&grant.stderr, "metadata-body grant stderr")?;
    ensure_equal(
        &grant_json.pointer("/data/decision").and_then(Value::as_str),
        &Some("allow"),
        "metadata-body grant decision",
    )?;

    let after_grant = run_ee(
        &fixture.workspace,
        &[
            "mesh",
            "export",
            "--peer",
            fixture.peer_id.as_str(),
            "--json",
        ],
    )?;
    let after_grant_json = success_json(&after_grant, "post-body-grant ee mesh export")?;
    ensure_equal(
        &json_u64(
            &after_grant_json,
            "/data/eventCount",
            "post-body-grant export report",
        )?,
        &1,
        "body grant must export the exact previously denied metadata event",
    )?;
    let exported = after_grant_json
        .pointer("/data/artifact/events/0")
        .ok_or_else(|| format!("post-body-grant export omitted its event: {after_grant_json}"))?;
    ensure_equal(
        &exported.pointer("/eventId").and_then(Value::as_str),
        &Some(event_id.as_str()),
        "post-body-grant exported event identity",
    )?;
    let exported_event_json = serde_json::from_str::<Value>(&json_string(
        exported,
        "/eventJson",
        "post-body-grant exported event",
    )?)
    .map_err(|error| format!("parse post-body-grant canonical eventJson: {error}"))?;
    ensure_equal(
        &exported_event_json
            .pointer("/bodyRef/uri")
            .and_then(Value::as_str),
        &Some(body_uri.as_str()),
        "post-body-grant hash-bound body URI",
    )?;
    ensure_equal(
        &exported_event_json
            .pointer("/trustClaim/assertedBy")
            .and_then(Value::as_str),
        &Some("lane-grant-e2e-peer"),
        "post-body-grant hash-bound trust claim",
    )
}

#[test]
fn committed_grant_controls_production_inbound_and_outbound_paths() -> TestResult {
    let fixture = set_up_fixture("production-policy")?;
    let config_path = Path::new(&fixture.workspace)
        .join(".ee")
        .join("config.toml");
    let original_config_bytes = fs::read(&config_path)
        .map_err(|error| format!("read production-path mesh config: {error}"))?;

    // The artifact claims that graph-link material is allowed. Before a local
    // grant, the production importer must override that untrusted claim with a
    // deny, retain only the honest ledger record, and enqueue no index work.
    let (pregrant_path, pregrant_event_id, pregrant_event_hash) =
        write_graph_link_artifact(&fixture, "pregrant", 1, None)?;
    let pregrant_import = run_ee(
        &fixture.workspace,
        &["mesh", "import", "--file", pregrant_path.as_str(), "--json"],
    )?;
    let pregrant_import_json = success_json(&pregrant_import, "pre-grant ee mesh import")?;
    ensure_equal(
        &pregrant_import_json
            .pointer("/data/schema")
            .and_then(Value::as_str),
        &Some("ee.mesh.cli.import.v2"),
        "pre-grant import report schema",
    )?;
    ensure_equal(
        &json_u64(
            &pregrant_import_json,
            "/data/importedEventCount",
            "pre-grant import report",
        )?,
        &1,
        "pre-grant import ledger count",
    )?;
    let pregrant_row = stored_graph_link_event(&fixture, 1)?;
    ensure_equal(
        &pregrant_row.event_id,
        &pregrant_event_id,
        "pre-grant durable event id",
    )?;
    ensure_equal(
        &pregrant_row.import_decision.as_str(),
        &"deny",
        "pre-grant locally recomputed import decision",
    )?;
    ensure(
        pregrant_row.policy_failure_surface_json.is_some(),
        "pre-grant denial must persist a structured policy failure surface",
    )?;
    ensure_equal(
        &matching_import_job_count(&fixture, &pregrant_event_id)?,
        &0,
        "pre-grant denial must enqueue no import index job",
    )?;

    let pregrant_export = run_ee(
        &fixture.workspace,
        &[
            "mesh",
            "export",
            "--peer",
            fixture.peer_id.as_str(),
            "--json",
        ],
    )?;
    let pregrant_export_json = success_json(&pregrant_export, "pre-grant ee mesh export")?;
    ensure_equal(
        &pregrant_export_json
            .pointer("/data/schema")
            .and_then(Value::as_str),
        &Some("ee.mesh.cli.export.v1"),
        "pre-grant export report schema",
    )?;
    ensure_equal(
        &json_u64(
            &pregrant_export_json,
            "/data/eventCount",
            "pre-grant export report",
        )?,
        &0,
        "pre-grant outbound graph-link exposure",
    )?;

    // Cross the real process boundary used by operators: issue an authenticated
    // preview bearer. The complete candidate set must bind the denied import
    // row because the proposed outbound policy would expose that exact event.
    let (_, issued_json) = preview(&fixture, true)?;
    let issued_preview = preview_payload(&issued_json, "production-path grant approval")?;
    ensure_equal(
        &json_u64(
            &issued_preview,
            "/affectedLedgerEventCount",
            "production-path grant approval",
        )?,
        &1,
        "pre-grant preview affected ledger-event count",
    )?;
    let issued_candidates = issued_preview
        .pointer("/candidateSet")
        .and_then(Value::as_array)
        .ok_or_else(|| format!("production preview omitted candidateSet: {issued_preview}"))?;
    ensure(
        issued_candidates.iter().any(|candidate| {
            candidate.pointer("/candidateKind").and_then(Value::as_str) == Some("mesh_ledger_event")
                && candidate.pointer("/candidateId").and_then(Value::as_str)
                    == Some(pregrant_event_id.as_str())
                && candidate
                    .pointer("/revisionId")
                    .and_then(Value::as_str)
                    .is_some_and(|revision| revision.starts_with("revme1_"))
        }),
        format!(
            "production preview did not bind the denied ledger event by immutable identity: {issued_preview}"
        ),
    )?;
    let serialized_issued_preview = serde_json::to_string(&issued_preview)
        .map_err(|error| format!("serialize production preview: {error}"))?;
    let pregrant_content_hash = fixture_hash("content:pregrant:1");
    for forbidden in [
        pregrant_event_hash.as_str(),
        pregrant_content_hash.as_str(),
        "mem_lane_grant_pregrant_01",
        "ee.mesh.event.v1",
        "eventJson",
        "contentHash",
        "eventHash",
        "bodyCacheKey",
        "policyDecisionJson",
        "policyFailureSurfaceJson",
        "https://",
    ] {
        ensure(
            !serialized_issued_preview.contains(forbidden),
            format!("production preview leaked raw ledger material {forbidden:?}"),
        )?;
    }
    let bearer = sensitive_json_string(
        &issued_json,
        "/data/preview/approvalToken/value",
        "production-path grant approval",
    )?;

    // Ledger inserts intentionally do not advance the workspace memory
    // generation. Adding another matching event must still change the complete
    // candidate set and stale the already-issued bearer with zero grant/audit
    // effects.
    let generation_before_ledger_insert = fixture_workspace_generation(&fixture)?;
    let (post_issuance_path, post_issuance_event_id, post_issuance_event_hash) =
        write_graph_link_artifact(&fixture, "post-issuance", 2, Some(&pregrant_event_hash))?;
    let post_issuance_import = run_ee(
        &fixture.workspace,
        &[
            "mesh",
            "import",
            "--file",
            post_issuance_path.as_str(),
            "--json",
        ],
    )?;
    success_json(
        &post_issuance_import,
        "post-issuance pre-grant ee mesh import",
    )?;
    ensure_equal(
        &fixture_workspace_generation(&fixture)?,
        &generation_before_ledger_insert,
        "mesh-ledger insert must not rely on workspace memory generation",
    )?;
    let post_issuance_row = stored_graph_link_event(&fixture, 2)?;
    ensure_equal(
        &post_issuance_row.import_decision.as_str(),
        &"deny",
        "post-issuance event remains denied before grant",
    )?;
    ensure_equal(
        &matching_import_job_count(&fixture, &post_issuance_event_id)?,
        &0,
        "post-issuance denied event must enqueue no index job",
    )?;
    let stale_grant = run_ee_with_stdin(
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
        &stale_grant,
        "ledger-drifted production grant",
        "mesh_approval_token_stale",
        "warning",
        "authentic but its approved preview is stale",
    )?;
    ensure_equal(
        &lane_audit_entries(&fixture, GRANT_AUDIT_ACTION)?.len(),
        &0,
        "ledger-drifted bearer must append no grant audit",
    )?;

    // Issue a fresh snapshot that binds both immutable events, pass only its
    // bearer over bounded stdin, and commit generation 1.
    let (_, refreshed_issued_json) = preview(&fixture, true)?;
    let refreshed_preview =
        preview_payload(&refreshed_issued_json, "refreshed production approval")?;
    ensure_equal(
        &json_u64(
            &refreshed_preview,
            "/affectedLedgerEventCount",
            "refreshed production approval",
        )?,
        &2,
        "refreshed preview affected ledger-event count",
    )?;
    let refreshed_bearer = sensitive_json_string(
        &refreshed_issued_json,
        "/data/preview/approvalToken/value",
        "refreshed production-path grant approval",
    )?;
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
        format!("{refreshed_bearer}\n").as_bytes(),
    )?;
    let grant_json = success_json(&grant, "production-path ee mesh grant")?;
    assert_no_bearer(&grant.stdout, "production-path grant stdout")?;
    assert_no_bearer(&grant.stderr, "production-path grant stderr")?;
    ensure_equal(
        &grant_json.pointer("/data/decision").and_then(Value::as_str),
        &Some("allow"),
        "production-path grant decision",
    )?;
    ensure_equal(
        &json_u64(
            &grant_json,
            "/data/newGrantGeneration",
            "production-path grant",
        )?,
        &1,
        "production-path grant generation",
    )?;

    // Both durable events that the named export omitted before the grant must
    // now cross the production outbound policy gate.
    let granted_export = run_ee(
        &fixture.workspace,
        &[
            "mesh",
            "export",
            "--peer",
            fixture.peer_id.as_str(),
            "--json",
        ],
    )?;
    let granted_export_json = success_json(&granted_export, "granted ee mesh export")?;
    ensure_equal(
        &json_u64(
            &granted_export_json,
            "/data/eventCount",
            "granted export report",
        )?,
        &2,
        "granted outbound graph-link exposure",
    )?;
    let granted_events = granted_export_json
        .pointer("/data/artifact/events")
        .and_then(Value::as_array)
        .ok_or_else(|| format!("granted export omitted artifact events: {granted_export_json}"))?;
    ensure(
        granted_events.iter().any(|event| {
            event.pointer("/eventId").and_then(Value::as_str) == Some(pregrant_event_id.as_str())
        }),
        "granted export must contain the exact previously denied graph-link event",
    )?;
    ensure(
        granted_events.iter().any(|event| {
            event.pointer("/eventId").and_then(Value::as_str)
                == Some(post_issuance_event_id.as_str())
        }),
        "granted export must contain the event that staled the first approval",
    )?;

    // A new inbound event on the granted lane now admits to local truth and
    // enqueues exactly one durable import-index job.
    let (postgrant_path, postgrant_event_id, postgrant_event_hash) =
        write_graph_link_artifact(&fixture, "postgrant", 3, Some(&post_issuance_event_hash))?;
    let postgrant_import = run_ee(
        &fixture.workspace,
        &[
            "mesh",
            "import",
            "--file",
            postgrant_path.as_str(),
            "--json",
        ],
    )?;
    let postgrant_import_json = success_json(&postgrant_import, "granted ee mesh import")?;
    ensure_equal(
        &json_u64(
            &postgrant_import_json,
            "/data/importedEventCount",
            "granted import report",
        )?,
        &1,
        "granted import ledger count",
    )?;
    let postgrant_row = stored_graph_link_event(&fixture, 3)?;
    ensure_equal(
        &postgrant_row.event_id,
        &postgrant_event_id,
        "granted durable event id",
    )?;
    ensure_equal(
        &postgrant_row.import_decision.as_str(),
        &"allow",
        "granted locally recomputed import decision",
    )?;
    ensure(
        postgrant_row.policy_failure_surface_json.is_none(),
        "granted import must not persist a policy failure surface",
    )?;
    let postgrant_policy_raw = postgrant_row
        .policy_decision_json
        .as_deref()
        .ok_or_else(|| "granted import omitted its durable policy decision".to_owned())?;
    let postgrant_policy: Value = serde_json::from_str(postgrant_policy_raw)
        .map_err(|error| format!("parse granted import policy decision: {error}"))?;
    ensure_equal(
        &postgrant_policy
            .pointer("/direction")
            .and_then(Value::as_str),
        &Some("inbound"),
        "granted import policy direction",
    )?;
    ensure_equal(
        &postgrant_policy.pointer("/action").and_then(Value::as_str),
        &Some("allow"),
        "granted import policy action",
    )?;
    ensure_equal(
        &postgrant_policy
            .pointer("/materialLane")
            .and_then(Value::as_str),
        &Some("graphLink"),
        "granted import policy lane",
    )?;
    ensure_equal(
        &matching_import_job_count(&fixture, &postgrant_event_id)?,
        &1,
        "granted import index job count",
    )?;

    // Approval is bound to the exact successfully parsed config bytes. A
    // comment-only digest drift must make both production directions fail
    // closed even though the underlying TOML policy still parses as `deny`.
    let mut drifted_config_bytes = original_config_bytes.clone();
    drifted_config_bytes.extend_from_slice(b"\n# production path digest drift\n");
    fs::write(&config_path, drifted_config_bytes)
        .map_err(|error| format!("write production-path config drift: {error}"))?;

    let drifted_export = run_ee(
        &fixture.workspace,
        &[
            "mesh",
            "export",
            "--peer",
            fixture.peer_id.as_str(),
            "--json",
        ],
    )?;
    let drifted_export_json = success_json(&drifted_export, "drifted ee mesh export")?;
    ensure_equal(
        &json_u64(
            &drifted_export_json,
            "/data/eventCount",
            "drifted export report",
        )?,
        &0,
        "config-drifted outbound graph-link exposure",
    )?;

    let (drifted_path, drifted_event_id, drifted_event_hash) =
        write_graph_link_artifact(&fixture, "drifted", 4, Some(&postgrant_event_hash))?;
    let drifted_import = run_ee(
        &fixture.workspace,
        &["mesh", "import", "--file", drifted_path.as_str(), "--json"],
    )?;
    let drifted_import_json = success_json(&drifted_import, "drifted ee mesh import")?;
    ensure_equal(
        &json_u64(
            &drifted_import_json,
            "/data/importedEventCount",
            "drifted import report",
        )?,
        &1,
        "drifted import ledger count",
    )?;
    let drifted_row = stored_graph_link_event(&fixture, 4)?;
    ensure_equal(
        &drifted_row.event_id,
        &drifted_event_id,
        "drifted durable event id",
    )?;
    ensure_equal(
        &drifted_row.import_decision.as_str(),
        &"deny",
        "config-drifted locally recomputed import decision",
    )?;
    ensure(
        drifted_row.policy_failure_surface_json.is_some(),
        "config-drifted denial must persist a structured policy failure surface",
    )?;
    ensure_equal(
        &matching_import_job_count(&fixture, &drifted_event_id)?,
        &0,
        "config-drifted denial must enqueue no import index job",
    )?;

    fs::write(&config_path, original_config_bytes)
        .map_err(|error| format!("restore production-path mesh config: {error}"))?;

    // Revocation is the symmetric production fence: it advances the same
    // generation and immediately closes both future serving and admission.
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
    let revoke_json = success_json(&revoke, "production-path ee mesh revoke-lane")?;
    ensure_equal(
        &json_u64(
            &revoke_json,
            "/data/newGrantGeneration",
            "production-path revoke",
        )?,
        &2,
        "production-path revoke generation",
    )?;

    let revoked_export = run_ee(
        &fixture.workspace,
        &[
            "mesh",
            "export",
            "--peer",
            fixture.peer_id.as_str(),
            "--json",
        ],
    )?;
    let revoked_export_json = success_json(&revoked_export, "revoked ee mesh export")?;
    ensure_equal(
        &json_u64(
            &revoked_export_json,
            "/data/eventCount",
            "revoked export report",
        )?,
        &0,
        "revoked outbound graph-link exposure",
    )?;

    let (revoked_path, revoked_event_id, _) =
        write_graph_link_artifact(&fixture, "revoked", 5, Some(&drifted_event_hash))?;
    let revoked_import = run_ee(
        &fixture.workspace,
        &["mesh", "import", "--file", revoked_path.as_str(), "--json"],
    )?;
    success_json(&revoked_import, "revoked ee mesh import")?;
    let revoked_row = stored_graph_link_event(&fixture, 5)?;
    ensure_equal(
        &revoked_row.import_decision.as_str(),
        &"deny",
        "revoked locally recomputed import decision",
    )?;
    ensure_equal(
        &matching_import_job_count(&fixture, &revoked_event_id)?,
        &0,
        "revoked denial must enqueue no import index job",
    )?;
    ensure_equal(
        &lane_audit_entries(&fixture, REVOKE_AUDIT_ACTION)?.len(),
        &1,
        "production-path revoke audit count",
    )?;
    Ok(())
}
