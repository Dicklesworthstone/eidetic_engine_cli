#![allow(clippy::expect_used, clippy::unwrap_used)]

use ee::mesh::foreground_cli::{
    MESH_EXPORT_ARTIFACT_SCHEMA_V1, MeshCliDegradation, MeshExportArtifact, MeshForegroundSnapshot,
    MeshStorageCounts,
};

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
