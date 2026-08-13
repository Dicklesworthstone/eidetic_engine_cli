//! Local team genesis over the signed origin stream.
//!
//! `ee team create` appends one origin-owned `teamCreated` manifest event.
//! That is the durable team record. Pairing, invites, and the active-member
//! authorizer remain later T3/T4 slices; this module does not advertise
//! `mesh.team.memory.v1`.

use serde::{Deserialize, Serialize};

use crate::db::{
    DbConnection, InsertTeamMemberInput, InsertTeamPendingInviteInput, StoredMeshOriginEvent,
    StoredTeamMember,
};
use crate::mesh::bootstrap_envelope::{
    BOOTSTRAP_DECLINE_SCHEMA_V1, BootstrapCapability, BootstrapDeclineV1, decode_envelope,
    encode_envelope, exchange_bootstrap_join, read_std_framed, write_std_framed,
};
use crate::mesh::hello_responder::configured_hello_port;
use crate::mesh::origin_stream::{
    ManifestEventPayload, OriginAppendRequest, OriginEventPayload, OriginSigner, OriginStreamError,
    append_origin_event, parse_stored_payload,
};

pub const TEAM_CREATE_SCHEMA_V1: &str = "ee.team.create.v1";
pub const TEAM_STATUS_SCHEMA_V1: &str = "ee.team.status.v1";
pub const TEAM_INVITE_SCHEMA_V1: &str = "ee.team.invite.v1";
pub const TEAM_JOIN_SCHEMA_V1: &str = "ee.team.join.v1";
pub const TEAM_JOIN_GRANTED_SCHEMA_V1: &str = "ee.team.join_granted.v1";
pub const TEAM_CREATED_OPERATION: &str = "teamCreated";
pub const TEAM_JOINED_OPERATION: &str = "teamJoined";
pub const TEAM_INVITE_CODE_PREFIX: &str = "eeteam1-";

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
    pub members: Vec<TeamMemberRecord>,
    pub mesh_primitives: Vec<&'static str>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TeamMemberRecord {
    pub member_id: String,
    pub team_id: String,
    pub workspace_id: String,
    pub display_name: String,
    pub state: String,
    pub is_self: bool,
    pub origin_node_id: String,
    pub bound_via: String,
    pub joined_at: String,
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
    persist_team_member(
        connection,
        workspace_id,
        &team_id,
        &origin_node_id,
        name,
        true,
        "team_genesis",
        produced_at,
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
    let members = connection
        .list_all_team_members()
        .map_err(|error| OriginStreamError::Db(error.to_string()))?
        .into_iter()
        .map(team_member_record)
        .collect();
    Ok(TeamStatusReport {
        schema: TEAM_STATUS_SCHEMA_V1,
        command: "team status",
        team_count: teams.len(),
        teams,
        members,
        mesh_primitives: vec![
            "mesh_origin_events",
            "ee.team.manifest_event.v1",
            "team_members",
        ],
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
            format!("ee team invite --endpoint <tailscale-ip> {workspace_flag} --json"),
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
    if payload.operation != TEAM_CREATED_OPERATION && payload.operation != TEAM_JOINED_OPERATION {
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

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TeamInviteCodeV1 {
    pub schema: String,
    pub invite_id: String,
    pub team_id: String,
    pub origin_node_id: String,
    pub hello_port: u16,
    pub endpoint: String,
    pub genesis_event_hash: String,
    pub secret: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TeamInviteReport {
    pub schema: &'static str,
    pub command: &'static str,
    pub invite_id: String,
    pub team_id: String,
    pub hello_port: u16,
    pub endpoint: String,
    pub expires_at: String,
    pub invite_code: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub granted: Option<TeamJoinGrantedV1>,
    pub mesh_primitives: Vec<&'static str>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TeamJoinRequestV1 {
    pub schema: String,
    pub invite_id: String,
    pub secret: String,
    pub joiner_node_id: String,
    pub joiner_display_name: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TeamJoinGrantedV1 {
    pub schema: String,
    pub team_id: String,
    pub origin_node_id: String,
    pub display_name: String,
    pub hello_port: u16,
    pub genesis_event_hash: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TeamJoinReport {
    pub schema: &'static str,
    pub command: &'static str,
    pub joined: bool,
    pub team: TeamRecord,
    pub mesh_primitives: Vec<&'static str>,
}

/// Mint a single-use invite for the local team genesis.
pub fn mint_team_invite(
    connection: &DbConnection,
    endpoint: &str,
    produced_at: &str,
    expires_at: &str,
) -> Result<TeamInviteReport, OriginStreamError> {
    let endpoint = endpoint.trim();
    if endpoint.is_empty() {
        return Err(OriginStreamError::Encode(
            "invite endpoint must not be empty".to_owned(),
        ));
    }
    let team = load_local_teams(connection)?
        .into_iter()
        .next()
        .ok_or_else(|| {
            OriginStreamError::Encode("no local team genesis to invite into".to_owned())
        })?;
    let invite_id = random_hex_32()?;
    let secret = random_hex_32()?;
    let secret_hash = format!("blake3:{}", blake3::hash(secret.as_bytes()).to_hex());
    connection
        .insert_team_pending_invite(&InsertTeamPendingInviteInput {
            invite_id: invite_id.clone(),
            team_id: team.team_id.clone(),
            origin_node_id: team.origin_node_id.clone(),
            hello_port: team.hello_port,
            endpoint: endpoint.to_owned(),
            genesis_event_hash: team.genesis_event_hash.clone(),
            secret_hash,
            status: "pending".to_owned(),
            created_at: produced_at.to_owned(),
            expires_at: expires_at.to_owned(),
        })
        .map_err(|error| OriginStreamError::Db(error.to_string()))?;
    let code = encode_invite_code(&TeamInviteCodeV1 {
        schema: TEAM_INVITE_SCHEMA_V1.to_owned(),
        invite_id: invite_id.clone(),
        team_id: team.team_id.clone(),
        origin_node_id: team.origin_node_id,
        hello_port: team.hello_port,
        endpoint: endpoint.to_owned(),
        genesis_event_hash: team.genesis_event_hash,
        secret,
    })?;
    Ok(TeamInviteReport {
        schema: TEAM_INVITE_SCHEMA_V1,
        command: "team invite",
        invite_id,
        team_id: team.team_id,
        hello_port: team.hello_port,
        endpoint: endpoint.to_owned(),
        expires_at: expires_at.to_owned(),
        invite_code: code,
        granted: None,
        mesh_primitives: vec!["team_pending_invites.insert", "eeteam1"],
    })
}

/// Accept one unsigned bootstrap join on an already-bound listener.
pub fn serve_one_bootstrap_join(
    connection: &DbConnection,
    workspace_id: &str,
    listener: &std::net::TcpListener,
    timeout: std::time::Duration,
) -> Result<TeamJoinGrantedV1, OriginStreamError> {
    listener
        .set_nonblocking(false)
        .map_err(|error| OriginStreamError::Encode(format!("join listen: {error}")))?;
    let (mut stream, _) = listener
        .accept()
        .map_err(|error| OriginStreamError::Encode(format!("join accept: {error}")))?;
    stream
        .set_read_timeout(Some(timeout))
        .and_then(|()| stream.set_write_timeout(Some(timeout)))
        .map_err(|error| OriginStreamError::Encode(format!("join timeout: {error}")))?;
    let bytes = read_std_framed(&mut stream)
        .map_err(|error| OriginStreamError::Encode(error.to_string()))?;
    let envelope = match decode_envelope(&bytes) {
        Ok(envelope) => envelope,
        Err(error) => {
            write_join_decline(&mut stream, "bootstrap_malformed")?;
            return Err(OriginStreamError::Encode(error.to_string()));
        }
    };
    if envelope.capability != BootstrapCapability::Join {
        write_join_decline(&mut stream, "bootstrap_unsupported_capability")?;
        return Err(OriginStreamError::Encode(
            "bootstrap join expected join capability".to_owned(),
        ));
    }
    let request = match serde_json::from_value::<TeamJoinRequestV1>(envelope.payload) {
        Ok(request) if request.schema == TEAM_JOIN_SCHEMA_V1 => request,
        _ => {
            write_join_decline(&mut stream, "bootstrap_malformed")?;
            return Err(OriginStreamError::Encode(
                "bootstrap join payload is malformed".to_owned(),
            ));
        }
    };
    let redeemed_at = chrono::Utc::now().to_rfc3339();
    let granted = match redeem_team_invite(
        connection,
        &request.invite_id,
        &request.secret,
        &redeemed_at,
    ) {
        Ok(granted) => granted,
        Err(error) => {
            write_join_decline(&mut stream, "bootstrap_malformed")?;
            return Err(error);
        }
    };
    record_inviter_side_join_member(
        connection,
        workspace_id,
        &granted,
        &request.joiner_node_id,
        &request.joiner_display_name,
        &redeemed_at,
    )?;
    let payload = serde_json::to_value(&granted)
        .map_err(|error| OriginStreamError::Encode(error.to_string()))?;
    let reply = encode_envelope(BootstrapCapability::Join, payload)
        .map_err(|error| OriginStreamError::Encode(error.to_string()))?;
    write_std_framed(&mut stream, &reply)
        .map_err(|error| OriginStreamError::Encode(error.to_string()))?;
    Ok(granted)
}

fn write_join_decline(
    stream: &mut std::net::TcpStream,
    code: &str,
) -> Result<(), OriginStreamError> {
    let decline = BootstrapDeclineV1 {
        schema: BOOTSTRAP_DECLINE_SCHEMA_V1.to_owned(),
        code: code.to_owned(),
    };
    let bytes = serde_json::to_vec(&decline)
        .map_err(|error| OriginStreamError::Encode(error.to_string()))?;
    write_std_framed(stream, &bytes).map_err(|error| OriginStreamError::Encode(error.to_string()))
}

/// Parse an `eeteam1-` invite code.
pub fn parse_team_invite_code(code: &str) -> Result<TeamInviteCodeV1, OriginStreamError> {
    let rest = code
        .strip_prefix(TEAM_INVITE_CODE_PREFIX)
        .ok_or_else(|| OriginStreamError::Encode("invite must start with eeteam1-".to_owned()))?;
    let bytes = hex_decode(rest)
        .ok_or_else(|| OriginStreamError::Encode("invite payload is not hex".to_owned()))?;
    let parsed = serde_json::from_slice::<TeamInviteCodeV1>(&bytes)
        .map_err(|error| OriginStreamError::Encode(format!("invite decode: {error}")))?;
    if parsed.schema != TEAM_INVITE_SCHEMA_V1 {
        return Err(OriginStreamError::Encode(
            "invite schema is not ee.team.invite.v1".to_owned(),
        ));
    }
    Ok(parsed)
}

/// Redeem a pending invite after the secret is proved.
pub fn redeem_team_invite(
    connection: &DbConnection,
    invite_id: &str,
    secret: &str,
    redeemed_at: &str,
) -> Result<TeamJoinGrantedV1, OriginStreamError> {
    let invite = connection
        .get_team_pending_invite(invite_id)
        .map_err(|error| OriginStreamError::Db(error.to_string()))?
        .ok_or_else(|| OriginStreamError::Encode("unknown invite".to_owned()))?;
    if invite.status != "pending" {
        return Err(OriginStreamError::Encode(
            "invite is not pending".to_owned(),
        ));
    }
    if redeemed_at >= invite.expires_at.as_str() {
        return Err(OriginStreamError::Encode("invite expired".to_owned()));
    }
    let expected = format!("blake3:{}", blake3::hash(secret.as_bytes()).to_hex());
    if expected != invite.secret_hash {
        return Err(OriginStreamError::Encode(
            "invite secret mismatch".to_owned(),
        ));
    }
    let changed = connection
        .redeem_team_pending_invite(invite_id, redeemed_at)
        .map_err(|error| OriginStreamError::Db(error.to_string()))?;
    if !changed {
        return Err(OriginStreamError::Encode(
            "invite already redeemed".to_owned(),
        ));
    }
    let display_name = load_local_teams(connection)?
        .into_iter()
        .find(|team| team.team_id == invite.team_id)
        .map(|team| team.display_name)
        .unwrap_or_else(|| "unnamed".to_owned());
    Ok(TeamJoinGrantedV1 {
        schema: TEAM_JOIN_GRANTED_SCHEMA_V1.to_owned(),
        team_id: invite.team_id,
        origin_node_id: invite.origin_node_id,
        display_name,
        hello_port: invite.hello_port,
        genesis_event_hash: invite.genesis_event_hash,
    })
}

/// Record the joining node on the inviter after a successful redeem.
pub fn record_inviter_side_join_member(
    connection: &DbConnection,
    workspace_id: &str,
    granted: &TeamJoinGrantedV1,
    joiner_node_id: &str,
    display_name: &str,
    joined_at: &str,
) -> Result<(), OriginStreamError> {
    persist_team_member(
        connection,
        workspace_id,
        &granted.team_id,
        joiner_node_id,
        display_name,
        false,
        "invite_ceremony",
        joined_at,
    )
}

/// Parse an invite, prove it over TCP join, and persist the granted team.
pub fn join_team_with_code(
    connection: &DbConnection,
    workspace_id: &str,
    invite_code: &str,
    display_name: &str,
    produced_at: &str,
    timeout: std::time::Duration,
) -> Result<TeamJoinReport, OriginStreamError> {
    let parsed = parse_team_invite_code(invite_code)?;
    let address = crate::mesh::bootstrap_envelope::parse_live_peer_endpoint(
        &parsed.endpoint,
        parsed.hello_port,
    )
    .ok_or_else(|| {
        OriginStreamError::Encode("invite endpoint is not a live TCP locator".to_owned())
    })?;
    let name = display_name.trim();
    if name.is_empty() {
        return Err(OriginStreamError::Encode(
            "join display name must not be empty".to_owned(),
        ));
    }
    let joiner_node_id = format!("node_{}", random_hex_32()?);
    let payload = serde_json::to_value(TeamJoinRequestV1 {
        schema: TEAM_JOIN_SCHEMA_V1.to_owned(),
        invite_id: parsed.invite_id,
        secret: parsed.secret,
        joiner_node_id: joiner_node_id.clone(),
        joiner_display_name: name.to_owned(),
    })
    .map_err(|error| OriginStreamError::Encode(error.to_string()))?;
    let granted_value = exchange_bootstrap_join(address, timeout, payload)
        .map_err(|error| OriginStreamError::Encode(error.to_string()))?;
    let granted = serde_json::from_value::<TeamJoinGrantedV1>(granted_value)
        .map_err(|error| OriginStreamError::Encode(format!("join grant decode: {error}")))?;
    persist_granted_join(
        connection,
        workspace_id,
        &granted,
        &joiner_node_id,
        name,
        produced_at,
    )
}

/// Persist a granted join as a local `teamJoined` origin event.
pub fn persist_granted_join(
    connection: &DbConnection,
    workspace_id: &str,
    granted: &TeamJoinGrantedV1,
    joiner_node_id: &str,
    display_name: &str,
    produced_at: &str,
) -> Result<TeamJoinReport, OriginStreamError> {
    if load_local_teams(connection)?
        .iter()
        .any(|team| team.team_id == granted.team_id)
    {
        let team = load_local_teams(connection)?
            .into_iter()
            .find(|team| team.team_id == granted.team_id)
            .expect("just checked");
        return Ok(TeamJoinReport {
            schema: TEAM_JOIN_SCHEMA_V1,
            command: "team join",
            joined: false,
            team,
            mesh_primitives: vec!["ee.team.manifest_event.v1"],
        });
    }
    let seed = blake3::hash(format!("{workspace_id}:join:{}", granted.team_id).as_bytes());
    let hex = seed.to_hex();
    let payload = OriginEventPayload::Manifest(ManifestEventPayload {
        operation: TEAM_JOINED_OPERATION.to_owned(),
        document_id: format!("tdoc_{}", &hex.as_str()[..24]),
        predecessor_revision_id: None,
        document_payload: serde_json::json!({
            "displayName": display_name,
            "helloPort": granted.hello_port,
        }),
    });
    let signer = LocalOriginSigner::for_workspace(workspace_id);
    let appended = append_origin_event(
        connection,
        &signer,
        &OriginAppendRequest {
            team_id: &granted.team_id,
            origin_node_id: joiner_node_id,
            payload,
            required_features: Vec::new(),
            produced_at,
            body_nonce: None,
        },
    )?;
    persist_team_member(
        connection,
        workspace_id,
        &granted.team_id,
        joiner_node_id,
        display_name,
        true,
        "invite_ceremony",
        produced_at,
    )?;
    Ok(TeamJoinReport {
        schema: TEAM_JOIN_SCHEMA_V1,
        command: "team join",
        joined: true,
        team: TeamRecord {
            team_id: granted.team_id.clone(),
            origin_node_id: joiner_node_id.to_owned(),
            display_name: display_name.to_owned(),
            hello_port: granted.hello_port,
            genesis_event_id: appended.event_id,
            genesis_event_hash: appended.event_hash,
            seq: appended.seq,
            produced_at: produced_at.to_owned(),
        },
        mesh_primitives: vec![
            "mesh_origin_events.append",
            "teamJoined",
            "eeteam1",
            "team_members",
        ],
    })
}

fn persist_team_member(
    connection: &DbConnection,
    workspace_id: &str,
    team_id: &str,
    origin_node_id: &str,
    display_name: &str,
    is_self: bool,
    bound_via: &str,
    joined_at: &str,
) -> Result<(), OriginStreamError> {
    let name = display_name.trim();
    if name.is_empty() {
        return Err(OriginStreamError::Encode(
            "member display name must not be empty".to_owned(),
        ));
    }
    connection
        .insert_team_member(&InsertTeamMemberInput {
            member_id: format!("mbr_{}", random_hex_32()?),
            team_id: team_id.to_owned(),
            workspace_id: workspace_id.to_owned(),
            display_name: name.to_owned(),
            state: "active".to_owned(),
            is_self,
            origin_node_id: origin_node_id.to_owned(),
            bound_via: bound_via.to_owned(),
            joined_at: joined_at.to_owned(),
        })
        .map_err(|error| OriginStreamError::Db(error.to_string()))?;
    Ok(())
}

fn team_member_record(row: StoredTeamMember) -> TeamMemberRecord {
    TeamMemberRecord {
        member_id: row.member_id,
        team_id: row.team_id,
        workspace_id: row.workspace_id,
        display_name: row.display_name,
        state: row.state,
        is_self: row.is_self,
        origin_node_id: row.origin_node_id,
        bound_via: row.bound_via,
        joined_at: row.joined_at,
    }
}

fn random_hex_32() -> Result<String, OriginStreamError> {
    let mut bytes = [0_u8; 16];
    getrandom::fill(&mut bytes)
        .map_err(|error| OriginStreamError::Encode(format!("csprng unavailable: {error}")))?;
    Ok(hex_encode(&bytes))
}

fn encode_invite_code(invite: &TeamInviteCodeV1) -> Result<String, OriginStreamError> {
    let bytes =
        serde_json::to_vec(invite).map_err(|error| OriginStreamError::Encode(error.to_string()))?;
    Ok(format!("{TEAM_INVITE_CODE_PREFIX}{}", hex_encode(&bytes)))
}

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len().saturating_mul(2));
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

fn hex_decode(input: &str) -> Option<Vec<u8>> {
    if input.len() % 2 != 0 {
        return None;
    }
    let mut out = Vec::with_capacity(input.len() / 2);
    let bytes = input.as_bytes();
    for chunk in bytes.chunks_exact(2) {
        let high = hex_nibble(chunk[0])?;
        let low = hex_nibble(chunk[1])?;
        out.push((high << 4) | low);
    }
    Some(out)
}

fn hex_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
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
        assert_eq!(status.members.len(), 1);
        assert!(status.members[0].is_self);
        assert_eq!(status.members[0].bound_via, "team_genesis");
        assert_eq!(status.members[0].display_name, "Analysts");
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

    #[test]
    fn mint_parse_and_redeem_invite_is_single_use() {
        let connection = open_db();
        create_local_team(
            &connection,
            "wsp_persistfixture000000000001",
            "Analysts",
            "2026-08-13T00:00:00Z",
        )
        .expect("create");
        let minted = mint_team_invite(
            &connection,
            "127.0.0.1",
            "2026-08-13T00:00:00Z",
            "2026-08-20T00:00:00Z",
        )
        .expect("mint");
        assert!(minted.invite_code.starts_with("eeteam1-"));
        let parsed = parse_team_invite_code(&minted.invite_code).expect("parse");
        assert_eq!(parsed.invite_id, minted.invite_id);
        let granted = redeem_team_invite(
            &connection,
            &parsed.invite_id,
            &parsed.secret,
            "2026-08-13T01:00:00Z",
        )
        .expect("redeem");
        assert_eq!(granted.team_id, minted.team_id);
        let second = mint_team_invite(
            &connection,
            "127.0.0.1",
            "2026-08-13T00:00:00Z",
            "2026-08-20T00:00:00Z",
        )
        .expect("second mint");
        assert_ne!(second.invite_code, minted.invite_code);
        assert!(
            redeem_team_invite(
                &connection,
                &parsed.invite_id,
                &parsed.secret,
                "2026-08-13T02:00:00Z",
            )
            .is_err()
        );
    }

    #[test]
    fn persist_granted_join_records_a_team_joined_origin() {
        let inviter = open_db();
        let created = create_local_team(
            &inviter,
            "wsp_persistfixture000000000001",
            "Analysts",
            "2026-08-13T00:00:00Z",
        )
        .expect("create");
        let joiner = open_db();
        let report = persist_granted_join(
            &joiner,
            "wsp_joinworkspace000000000001",
            &TeamJoinGrantedV1 {
                schema: TEAM_JOIN_GRANTED_SCHEMA_V1.to_owned(),
                team_id: created.team.team_id.clone(),
                origin_node_id: created.team.origin_node_id.clone(),
                display_name: created.team.display_name.clone(),
                hello_port: created.team.hello_port,
                genesis_event_hash: created.team.genesis_event_hash.clone(),
            },
            "node_joinpersist000000000000000001",
            "Priya",
            "2026-08-13T03:00:00Z",
        )
        .expect("join");
        assert!(report.joined);
        assert_eq!(report.team.team_id, created.team.team_id);
        let status = local_team_status(&joiner).expect("status");
        assert_eq!(status.team_count, 1);
        assert_eq!(status.members.len(), 1);
        assert!(status.members[0].is_self);
        assert_eq!(status.members[0].bound_via, "invite_ceremony");
        assert_eq!(status.members[0].display_name, "Priya");
    }

    #[test]
    fn serve_one_bootstrap_join_redeems_and_records_the_joiner() {
        let inviter = open_db();
        create_local_team(
            &inviter,
            "wsp_persistfixture000000000001",
            "Analysts",
            "2026-08-13T00:00:00Z",
        )
        .expect("create");
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let address = listener.local_addr().expect("addr");
        let minted = mint_team_invite(
            &inviter,
            &address.to_string(),
            "2026-08-13T00:00:00Z",
            "2026-08-20T00:00:00Z",
        )
        .expect("mint");
        let invite_code = minted.invite_code.clone();
        let client = std::thread::spawn(move || {
            let joiner = open_db();
            join_team_with_code(
                &joiner,
                "wsp_joinworkspace000000000001",
                &invite_code,
                "Priya",
                "2026-08-13T04:00:00Z",
                std::time::Duration::from_secs(5),
            )
        });
        let granted = serve_one_bootstrap_join(
            &inviter,
            "wsp_persistfixture000000000001",
            &listener,
            std::time::Duration::from_secs(5),
        )
        .expect("serve");
        let joined = client.join().expect("client thread").expect("join");
        assert!(joined.joined);
        assert_eq!(joined.team.team_id, granted.team_id);
        let members = local_team_status(&inviter).expect("status").members;
        assert_eq!(members.len(), 2);
        assert!(
            members
                .iter()
                .any(|member| !member.is_self && member.display_name == "Priya")
        );
    }
}
