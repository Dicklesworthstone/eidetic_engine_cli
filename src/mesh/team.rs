//! Local team genesis over the signed origin stream.
//!
//! `ee team create` appends one origin-owned `teamCreated` manifest event.
//! That is the durable team record. Pairing, invites, and the active-member
//! authorizer remain later T3/T4 slices; this module does not advertise
//! `mesh.team.memory.v1`.

use serde::{Deserialize, Serialize};

use crate::db::{DbConnection, StoredMeshOriginEvent};
use crate::mesh::hello_responder::configured_hello_port;
use crate::mesh::origin_stream::{
    ManifestEventPayload, OriginAppendRequest, OriginEventPayload, OriginSigner, OriginStreamError,
    append_origin_event, parse_stored_payload,
};

pub const TEAM_CREATE_SCHEMA_V1: &str = "ee.team.create.v1";
pub const TEAM_STATUS_SCHEMA_V1: &str = "ee.team.status.v1";
pub const TEAM_CREATED_OPERATION: &str = "teamCreated";

/// Workspace-local origin MAC. This is not Ed25519; T3.6 replaces the seam.
pub struct LocalOriginSigner {
    generation: u64,
    key: [u8; 32],
}

impl LocalOriginSigner {
    #[must_use]
    pub fn for_workspace(workspace_id: &str) -> Self {
        let digest = blake3::hash(format!("ee.team.origin_signer.v1:{workspace_id}").as_bytes());
        Self {
            generation: 1,
            key: *digest.as_bytes(),
        }
    }
}

impl OriginSigner for LocalOriginSigner {
    fn signing_key_generation(&self) -> u64 {
        self.generation
    }

    fn sign(&self, domain: &str, canonical_bytes: &[u8]) -> String {
        let mut hasher = blake3::Hasher::new_keyed(&self.key);
        hasher.update(domain.as_bytes());
        hasher.update(&(canonical_bytes.len() as u64).to_le_bytes());
        hasher.update(canonical_bytes);
        format!("blake3:{}", hasher.finalize().to_hex())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TeamRecord {
    pub team_id: String,
    pub origin_node_id: String,
    pub display_name: String,
    pub hello_port: u16,
    pub genesis_event_id: String,
    pub genesis_event_hash: String,
    pub seq: u64,
    pub produced_at: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TeamCreateReport {
    pub schema: &'static str,
    pub command: &'static str,
    pub created: bool,
    pub team: TeamRecord,
    pub mesh_primitives: Vec<&'static str>,
    pub next_commands: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TeamStatusReport {
    pub schema: &'static str,
    pub command: &'static str,
    pub team_count: usize,
    pub teams: Vec<TeamRecord>,
    pub mesh_primitives: Vec<&'static str>,
}

/// Create a local team genesis, or return the existing one.
pub fn create_local_team(
    connection: &DbConnection,
    workspace_id: &str,
    display_name: &str,
    produced_at: &str,
) -> Result<TeamCreateReport, OriginStreamError> {
    let name = display_name.trim();
    if name.is_empty() {
        return Err(OriginStreamError::Encode(
            "team display name must not be empty".to_owned(),
        ));
    }
    if let Some(existing) = load_local_teams(connection)?.into_iter().next() {
        return Ok(team_create_report(existing, false));
    }

    let seed = blake3::hash(format!("{workspace_id}:{name}:{produced_at}").as_bytes());
    let hex = seed.to_hex();
    let team_id = format!("team_{}", &hex.as_str()[..26]);
    let origin_node_id = format!("node_{}", &hex.as_str()[6..38]);
    let hello_port = configured_hello_port();
    let document_id = format!("tdoc_{}", &hex.as_str()[..24]);
    let payload = OriginEventPayload::Manifest(ManifestEventPayload {
        operation: TEAM_CREATED_OPERATION.to_owned(),
        document_id,
        predecessor_revision_id: None,
        document_payload: serde_json::json!({
            "displayName": name,
            "helloPort": hello_port,
        }),
    });
    let signer = LocalOriginSigner::for_workspace(workspace_id);
    let appended = append_origin_event(
        connection,
        &signer,
        &OriginAppendRequest {
            team_id: &team_id,
            origin_node_id: &origin_node_id,
            payload,
            required_features: Vec::new(),
            produced_at,
            body_nonce: None,
        },
    )?;
    Ok(team_create_report(
        TeamRecord {
            team_id,
            origin_node_id,
            display_name: name.to_owned(),
            hello_port,
            genesis_event_id: appended.event_id,
            genesis_event_hash: appended.event_hash,
            seq: appended.seq,
            produced_at: produced_at.to_owned(),
        },
        true,
    ))
}

/// Load local `teamCreated` genesis events.
pub fn load_local_teams(connection: &DbConnection) -> Result<Vec<TeamRecord>, OriginStreamError> {
    let rows = connection
        .list_mesh_manifest_origin_events(64)
        .map_err(|error| OriginStreamError::Db(error.to_string()))?;
    let mut teams = Vec::new();
    for row in rows {
        if let Some(team) = team_record_from_origin(&row)? {
            teams.push(team);
        }
    }
    Ok(teams)
}

/// Status projection over local genesis events.
pub fn local_team_status(connection: &DbConnection) -> Result<TeamStatusReport, OriginStreamError> {
    let teams = load_local_teams(connection)?;
    Ok(TeamStatusReport {
        schema: TEAM_STATUS_SCHEMA_V1,
        command: "team status",
        team_count: teams.len(),
        teams,
        mesh_primitives: vec!["mesh_origin_events", "ee.team.manifest_event.v1"],
    })
}

fn team_create_report(team: TeamRecord, created: bool) -> TeamCreateReport {
    let workspace_flag = "--workspace .";
    TeamCreateReport {
        schema: TEAM_CREATE_SCHEMA_V1,
        command: "team create",
        created,
        next_commands: vec![
            format!("ee team status {workspace_flag} --json"),
            format!(
                "ee mesh hello-responder run {workspace_flag} --team-id {} --responder-node-id {} --peer <peer-id> --json",
                team.team_id, team.origin_node_id
            ),
            format!("ee mesh sync --once {workspace_flag} --json"),
        ],
        team,
        mesh_primitives: vec![
            "mesh_origin_events.append",
            "ee.team.manifest_event.v1",
            "teamCreated",
        ],
    }
}

fn team_record_from_origin(
    row: &StoredMeshOriginEvent,
) -> Result<Option<TeamRecord>, OriginStreamError> {
    let OriginEventPayload::Manifest(payload) = parse_stored_payload(row)? else {
        return Ok(None);
    };
    if payload.operation != TEAM_CREATED_OPERATION {
        return Ok(None);
    }
    let display_name = payload
        .document_payload
        .get("displayName")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("unnamed")
        .to_owned();
    let hello_port = payload
        .document_payload
        .get("helloPort")
        .and_then(serde_json::Value::as_u64)
        .and_then(|port| u16::try_from(port).ok())
        .unwrap_or_else(configured_hello_port);
    Ok(Some(TeamRecord {
        team_id: row.team_id.clone(),
        origin_node_id: row.origin_node_id.clone(),
        display_name,
        hello_port,
        genesis_event_id: row.event_id.clone(),
        genesis_event_hash: row.event_hash.clone(),
        seq: row.seq,
        produced_at: row.produced_at.clone(),
    }))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    fn open_db() -> DbConnection {
        let connection = DbConnection::open_memory().expect("open");
        connection.migrate().expect("migrate");
        connection
    }

    #[test]
    fn create_local_team_appends_one_team_created_origin_event() {
        let connection = open_db();
        let report = create_local_team(
            &connection,
            "wsp_persistfixture000000000001",
            "Analysts",
            "2026-08-13T00:00:00Z",
        )
        .expect("create");
        assert!(report.created);
        assert!(report.team.team_id.starts_with("team_"));
        assert!(report.team.origin_node_id.starts_with("node_"));
        assert_eq!(report.team.display_name, "Analysts");
        assert_eq!(report.team.seq, 0);
        let status = local_team_status(&connection).expect("status");
        assert_eq!(status.team_count, 1);
        assert_eq!(status.teams[0].team_id, report.team.team_id);
    }

    #[test]
    fn create_local_team_is_idempotent_for_an_existing_genesis() {
        let connection = open_db();
        let first = create_local_team(
            &connection,
            "wsp_persistfixture000000000001",
            "Analysts",
            "2026-08-13T00:00:00Z",
        )
        .expect("first");
        let second = create_local_team(
            &connection,
            "wsp_persistfixture000000000001",
            "Other",
            "2026-08-13T01:00:00Z",
        )
        .expect("second");
        assert!(!second.created);
        assert_eq!(second.team.team_id, first.team.team_id);
        assert_eq!(second.team.display_name, "Analysts");
        assert_eq!(
            local_team_status(&connection).expect("status").team_count,
            1
        );
    }

    #[test]
    fn create_local_team_rejects_an_empty_name() {
        let connection = open_db();
        let error = create_local_team(
            &connection,
            "wsp_persistfixture000000000001",
            "   ",
            "2026-08-13T00:00:00Z",
        )
        .expect_err("empty");
        assert!(error.to_string().contains("display name"));
    }
}
