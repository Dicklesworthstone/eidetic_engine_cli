#![allow(clippy::expect_used, clippy::unwrap_used)]

use ee::mesh::foreground_cli::{
    MESH_EXPORT_ARTIFACT_SCHEMA_V1, MeshCliDegradation, MeshEventRow, MeshExportArtifact,
    MeshForegroundSnapshot, MeshStorageCounts,
};
use ee::policy::MESH_SECRET_EXPORT_DENIED_CODE;

fn pretty_json<T: serde::Serialize>(value: &T) -> String {
    let mut rendered = serde_json::to_string_pretty(value).expect("render foreground mesh JSON");
    rendered.push('\n');
    rendered
}

fn fixture(name: &str) -> &'static str {
    match name {
        "status_disabled" => {
            include_str!("fixtures/golden/mesh/foreground_status_disabled.json")
        }
        "export_empty" => include_str!("fixtures/golden/mesh/foreground_export_empty.json"),
        other => panic!("unknown fixture {other}"),
    }
}

fn disabled_snapshot() -> MeshForegroundSnapshot {
    MeshForegroundSnapshot {
        workspace_id: "wsp_meshforeground0000000001".to_owned(),
        workspace_path: "/tmp/ee-mesh".to_owned(),
        database_path: "/tmp/ee-mesh/.ee/ee.db".to_owned(),
        initialized: true,
        mesh_enabled: false,
        mode: "off".to_owned(),
        storage: MeshStorageCounts::default(),
        peers: Vec::new(),
        cursors: Vec::new(),
        events: Vec::new(),
        degraded: vec![MeshCliDegradation::mesh_disabled()],
    }
}

fn snapshot_with_event(event: MeshEventRow) -> MeshForegroundSnapshot {
    MeshForegroundSnapshot {
        workspace_id: "wsp_meshforeground0000000001".to_owned(),
        workspace_path: "/tmp/ee-mesh".to_owned(),
        database_path: "/tmp/ee-mesh/.ee/ee.db".to_owned(),
        initialized: true,
        mesh_enabled: true,
        mode: "cache".to_owned(),
        storage: MeshStorageCounts {
            peer_count: 0,
            cursor_count: 0,
            imported_event_count: 1,
            policy_decision_event_count: 1,
            policy_failure_event_count: 0,
            mapped_memory_count: 1,
            cached_body_count: 1,
        },
        peers: Vec::new(),
        cursors: Vec::new(),
        events: vec![event],
        degraded: Vec::new(),
    }
}

fn mesh_event(event_json: String) -> MeshEventRow {
    MeshEventRow {
        event_id: "mesh_event_preexport_001".to_owned(),
        origin_node_id: "node_alpha".to_owned(),
        origin_workspace_id: "wsp_origin".to_owned(),
        producer_peer_id: Some("peer_alpha".to_owned()),
        seq: 1,
        prev_event_hash: None,
        event_hash: "blake3:meshpreexporteventhash".to_owned(),
        event_kind: "memory_upsert".to_owned(),
        logical_memory_id: "mem_logical_001".to_owned(),
        content_hash: "blake3:contenthash".to_owned(),
        material_lane: "body".to_owned(),
        redaction_class: "shared_body".to_owned(),
        trust_lane: "human_explicit".to_owned(),
        import_decision: "allow".to_owned(),
        local_memory_id: Some("mem_local_001".to_owned()),
        body_cache_key: Some("body_cache_safe_001".to_owned()),
        policy_failure_surface_json: None,
        policy_decision_json: Some(r#"{"decision":"allow","bodyFetchAllowed":true}"#.to_owned()),
        event_json,
        policy_attestation: None,
        imported_at: "2026-05-19T20:00:00Z".to_owned(),
    }
}

#[test]
fn foreground_status_json_matches_golden_fixture() {
    let report = disabled_snapshot().status_report();
    assert_eq!(pretty_json(&report), fixture("status_disabled"));
}

#[test]
fn foreground_export_json_matches_golden_fixture_and_round_trips() {
    let artifact = disabled_snapshot().export_artifact();
    let rendered = pretty_json(&artifact);
    assert_eq!(rendered, fixture("export_empty"));

    let parsed: MeshExportArtifact =
        serde_json::from_str(&rendered).expect("foreground export artifact should parse");
    assert_eq!(parsed.schema, MESH_EXPORT_ARTIFACT_SCHEMA_V1);
    assert_eq!(parsed.workspace_id, "wsp_meshforeground0000000001");
    assert_eq!(parsed.storage.peer_count, 0);
}

#[test]
fn pre_export_secret_scan_denies_hostile_event_without_leaking_secret() {
    let secret = ["sk_live_", "bd38dqkpreexport123456789012345"].concat();
    let event_json = serde_json::json!({
        "body": format!("release note API_KEY={secret}"),
        "tags": [format!("deploy-token={secret}")],
        "evidenceRefs": [{"artifactPath": "keys/id_ed25519"}],
        "embeddingSurrogate": secret,
    })
    .to_string();
    let snapshot = snapshot_with_event(mesh_event(event_json));

    let scan = snapshot
        .checked_export_artifact()
        .expect_err("secret-bearing mesh event must fail closed before export");
    assert_eq!(scan.code, MESH_SECRET_EXPORT_DENIED_CODE);
    assert_eq!(scan.status, "denied");
    assert_eq!(scan.policy_action, "deny");
    assert!(scan.finding_count >= 2);
    assert!(
        scan.denied_secret_classes
            .iter()
            .any(|class| class == "api_key" || class == "stripe_secret_key")
    );
    assert!(
        scan.denied_secret_classes
            .iter()
            .any(|class| class == "private_key_path")
    );

    let rendered = serde_json::to_string(&scan).expect("render secret scan report");
    assert!(!rendered.contains(&secret));
    assert!(!rendered.contains("id_ed25519"));
    assert!(rendered.contains("[REDACTED:"));
}

#[test]
fn clean_pre_export_scan_attaches_policy_attestations() {
    let event_json = serde_json::json!({
        "body": "safe redaction-reviewed summary",
        "tags": ["release", "mesh"],
        "evidenceRefs": [{"artifactPath": "docs/release-notes.md"}],
        "embeddingSurrogate": "hash:7d3f",
    })
    .to_string();
    let snapshot = snapshot_with_event(mesh_event(event_json));

    let checked = snapshot
        .checked_export_artifact()
        .expect("clean mesh event should export");
    assert_eq!(checked.secret_scan.status, "passed");
    assert_eq!(checked.secret_scan.finding_count, 0);
    let artifact_attestation = checked
        .artifact
        .policy_attestation
        .as_ref()
        .expect("artifact attestation missing");
    assert_eq!(artifact_attestation.decision, "allow");
    assert!(artifact_attestation.scanned_field_count > 0);

    let event_attestation = checked.artifact.events[0]
        .policy_attestation
        .as_ref()
        .expect("event attestation missing");
    assert_eq!(event_attestation.decision, "allow");
    assert!(event_attestation.scanned_field_count > 0);
}
