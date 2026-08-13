//! Local team genesis over the signed origin stream.
//!
//! `ee team create` appends one origin-owned `teamCreated` manifest event.
//! Invites use a signed hello/challenge/prove ceremony, pair keys bind the
//! live transcript, and inbound origin events verify against `team_member_nodes`.
//! This module still does not advertise `mesh.team.memory.v1`.

use serde::{Deserialize, Serialize};

use crate::core::tailscale_probe::{
    TailnetOwnerDisposition, TailscaleLocalReport, TailscaleUserProfile, evaluate_tailnet_owner,
};
use crate::db::{
    CreateMemoryInput, DbConnection, InsertTeamHistoryProjectionInput, InsertTeamMemberInput,
    InsertTeamMemberNodeInput, InsertTeamPendingInviteInput, InsertTeamProjectInput,
    StoredMeshOriginEvent, StoredTeamMember, StoredTeamMemberIdentity, StoredTeamProject,
    UpsertMeshBodyCacheMetadataInput, UpsertTeamJoinAttemptInput,
};
use crate::mesh::bootstrap_envelope::{
    BOOTSTRAP_DECLINE_SCHEMA_V1, BootstrapCapability, BootstrapDeclineV1, decode_envelope,
    encode_envelope, read_std_framed, write_std_framed,
};
use crate::mesh::hello_responder::configured_hello_port;
use crate::mesh::idp::{IdpProviderCapability, classify_oidc_provider};
use crate::mesh::origin_stream::{
    Ed25519OriginSigner, InboundOriginEvent, ManifestEventPayload, MemoryEventOperation,
    MemoryEventPayload, OriginAppendRequest, OriginEventPayload, OriginSignatureVerifier,
    OriginSigner, OriginStreamError, append_origin_event, body_commitment, parse_stored_payload,
    verify_ed25519_origin_signature,
};

pub const TEAM_CREATE_SCHEMA_V1: &str = "ee.team.create.v1";
pub const TEAM_STATUS_SCHEMA_V1: &str = "ee.team.status.v1";
pub const TEAM_INVITE_SCHEMA_V1: &str = "ee.team.invite.v1";
pub const TEAM_JOIN_SCHEMA_V1: &str = "ee.team.join.v1";
pub const TEAM_JOIN_HELLO_SCHEMA_V1: &str = "ee.team.join_hello.v1";
pub const TEAM_JOIN_CHALLENGE_SCHEMA_V1: &str = "ee.team.join_challenge.v1";
pub const TEAM_JOIN_PROVE_SCHEMA_V1: &str = "ee.team.join_prove.v1";
pub const TEAM_JOIN_GRANTED_SCHEMA_V1: &str = "ee.team.join_granted.v1";
pub const TEAM_PAIR_TRANSCRIPT_DOMAIN: &str = "ee.team.pair.transcript.v1";
pub const TEAM_PAIR_KDF_DOMAIN: &str = "ee.team.pair.v1";
pub const TEAM_JOIN_CHALLENGE_DOMAIN: &str = "ee.team.join_challenge.signature.v1";
pub const TEAM_SHARE_HISTORY_SCHEMA_V1: &str = "ee.team.share_history.v1";
pub const TEAM_SHARE_BODIES_SCHEMA_V1: &str = "ee.team.share.bodies.v1";
pub const TEAM_UNSHARE_BODIES_SCHEMA_V1: &str = "ee.team.unshare.bodies.v1";
pub const TEAM_HISTORY_CONSENT_DOMAIN: &str = "ee.team.share_history.consent.v1";
pub const TEAM_BODIES_CONSENT_DOMAIN: &str = "ee.team.share_bodies.consent.v1";
pub const TEAM_CREATED_OPERATION: &str = "teamCreated";
pub const TEAM_JOINED_OPERATION: &str = "teamJoined";
pub const TEAM_MEMBER_REMOVED_OPERATION: &str = "teamMemberRemoved";
pub const TEAM_LEFT_OPERATION: &str = "teamLeft";
pub const TEAM_MEMBER_REMOVE_SCHEMA_V1: &str = "ee.team.member_remove.v1";
pub const TEAM_LEAVE_SCHEMA_V1: &str = "ee.team.leave.v1";
pub const TEAM_POSTURE_SCHEMA_V1: &str = "ee.team.posture.v1";
pub const TEAM_ADD_NODE_SCHEMA_V1: &str = "ee.team.add_node.v1";
pub const TEAM_ADD_NODE_OPERATION: &str = "teamNodeAdded";
pub const TEAM_RECONCILE_SCHEMA_V1: &str = "ee.team.reconcile.v1";
pub const TEAM_ACTIVITY_SCHEMA_V1: &str = "ee.team.activity.v1";
pub const TEAM_STEWARD_SCHEMA_V1: &str = "ee.team.steward.v1";
pub const TEAM_DOCTOR_SCHEMA_V1: &str = "ee.team.doctor.v1";
pub const TEAM_PROJECTS_SCHEMA_V1: &str = "ee.team.projects.v1";
pub const TEAM_PROJECT_SHARED_OPERATION: &str = "teamProjectShared";
pub const TEAM_IDP_SCHEMA_V1: &str = "ee.team.idp.v1";
pub const TEAM_IDP_REVALIDATE_SCHEMA_V1: &str = "ee.team.idp.revalidate.v1";
pub const TEAM_IDP_SET_SCHEMA_V1: &str = "ee.team.idp.set.v1";
pub const TEAM_IDP_POLICY_SET_OPERATION: &str = "teamIdpPolicySet";
pub const TEAM_INVITE_CODE_PREFIX: &str = "eeteam1-";
pub const TEAM_ACTIVITY_CLOCK_SKEW_SECS: i64 = 600;

/// Workspace-local origin MAC used only when no hardened key store is present.
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
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub nodes: Vec<TeamMemberNodeRecord>,
    pub paused: bool,
    pub steward_would_sync: bool,
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

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TeamMemberNodeRecord {
    pub node_id: String,
    pub member_id: String,
    pub team_id: String,
    pub verifying_key_hex: String,
    pub signing_key_generation: u64,
    pub state: String,
    pub bound_at: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TeamHistoryShareItem {
    pub memory_id: String,
    pub revision_id: String,
    pub level: String,
    pub kind: String,
    pub created_at: String,
    pub already_projected: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TeamHistoryShareReport {
    pub schema: &'static str,
    pub command: &'static str,
    pub team_id: String,
    pub confirmed: bool,
    pub consent_hash: String,
    pub candidate_count: usize,
    pub projected_count: usize,
    pub skipped_count: usize,
    pub items: Vec<TeamHistoryShareItem>,
    pub mesh_primitives: Vec<&'static str>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TeamBodyShareItem {
    pub memory_id: String,
    pub revision_id: String,
    pub size_bytes: u64,
    pub cache_status: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TeamBodyShareReport {
    pub schema: &'static str,
    pub command: &'static str,
    pub team_id: String,
    pub confirmed: bool,
    pub consent_hash: String,
    pub candidate_count: usize,
    pub published_count: usize,
    pub skipped_count: usize,
    pub items: Vec<TeamBodyShareItem>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval_token: Option<String>,
    pub mesh_primitives: Vec<&'static str>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TeamMemberMutationReport {
    pub schema: &'static str,
    pub command: &'static str,
    pub team_id: String,
    pub member_id: String,
    pub origin_node_id: String,
    pub state: String,
    pub mesh_primitives: Vec<&'static str>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TeamPostureReport {
    pub schema: &'static str,
    pub command: &'static str,
    pub team_id: String,
    pub paused: bool,
    pub pause_generation: u64,
    pub mesh_primitives: Vec<&'static str>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TeamDoctorCheck {
    pub name: String,
    pub status: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repair: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TeamDoctorReport {
    pub schema: &'static str,
    pub command: &'static str,
    pub team_id: Option<String>,
    pub posture: String,
    pub checks: Vec<TeamDoctorCheck>,
    pub mesh_primitives: Vec<&'static str>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TeamStewardReport {
    pub schema: &'static str,
    pub command: &'static str,
    pub team_id: String,
    pub paused: bool,
    pub active_member_count: usize,
    pub outcome: String,
    pub reason: String,
    pub ran_sync: bool,
    pub mesh_primitives: Vec<&'static str>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TeamReconcileReport {
    pub schema: &'static str,
    pub command: &'static str,
    pub team_id: String,
    pub applied_additions: usize,
    pub applied_removals: usize,
    pub inspected_events: usize,
    pub mesh_primitives: Vec<&'static str>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TeamActivityItem {
    pub event_id: String,
    pub origin_node_id: String,
    pub member_display_name: String,
    pub kind: String,
    pub level: String,
    pub produced_at: String,
    pub body_available: bool,
    pub source: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TeamProjectRecord {
    pub project_id: String,
    pub team_id: String,
    pub display_name: String,
    pub local_path: String,
    pub source: String,
    pub created_at: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TeamProjectsReport {
    pub schema: &'static str,
    pub command: &'static str,
    pub team_id: String,
    pub minted: bool,
    pub project_count: usize,
    pub projects: Vec<TeamProjectRecord>,
    pub mesh_primitives: Vec<&'static str>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TeamIdpPolicyReport {
    pub schema: &'static str,
    pub command: &'static str,
    pub team_id: String,
    pub kind: String,
    pub allowed_domain: Option<String>,
    pub policy_generation: u64,
    pub mesh_primitives: Vec<&'static str>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TeamIdpMemberCheck {
    pub member_id: String,
    pub origin_node_id: String,
    pub is_self: bool,
    pub login: Option<String>,
    pub disposition: String,
    pub state: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TeamIdpSetReport {
    pub schema: &'static str,
    pub command: &'static str,
    pub team_id: String,
    pub issuer: String,
    pub client_id: String,
    pub capability: String,
    pub discovery_hash: String,
    pub mesh_primitives: Vec<&'static str>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TeamIdpRevalidateReport {
    pub schema: &'static str,
    pub command: &'static str,
    pub team_id: String,
    pub kind: String,
    pub allowed_domain: Option<String>,
    pub checked: usize,
    pub attested: usize,
    pub suspended: usize,
    pub missing: usize,
    pub members: Vec<TeamIdpMemberCheck>,
    pub mesh_primitives: Vec<&'static str>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TeamActivityReport {
    pub schema: &'static str,
    pub command: &'static str,
    pub team_id: String,
    pub as_of: String,
    pub event_count: usize,
    pub events: Vec<TeamActivityItem>,
    pub clock_anomalies: Vec<TeamActivityItem>,
    pub mesh_primitives: Vec<&'static str>,
}

/// Create a local team genesis, or return the existing one.
pub fn create_local_team(
    connection: &DbConnection,
    workspace_id: &str,
    display_name: &str,
    produced_at: &str,
) -> Result<TeamCreateReport, OriginStreamError> {
    create_local_team_with_store(connection, workspace_id, display_name, produced_at, None)
}

/// Create a local team genesis, signing with the workspace key store when given.
pub fn create_local_team_with_store(
    connection: &DbConnection,
    workspace_id: &str,
    display_name: &str,
    produced_at: &str,
    workspace_path: Option<&std::path::Path>,
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
    let ed25519 = workspace_path
        .map(|path| Ed25519OriginSigner::load_or_create(path, &origin_node_id, produced_at))
        .transpose()?;
    let mac = LocalOriginSigner::for_workspace(workspace_id);
    let signer: &dyn OriginSigner = ed25519
        .as_ref()
        .map(|signer| signer as &dyn OriginSigner)
        .unwrap_or(&mac);
    let appended = append_origin_event(
        connection,
        signer,
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
        ed25519
            .as_ref()
            .map(|signer| hex_encode(&signer.verifying_key_bytes()))
            .as_deref(),
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

/// Whether an origin node may contribute events under recorded membership.
///
/// `None` means this store has no `team_members` rows yet, so the older
/// policy/bindings path remains in force. `Some(false)` is a hard deny.
pub fn origin_node_is_active_member(
    connection: &DbConnection,
    origin_node_id: &str,
) -> Result<Option<bool>, OriginStreamError> {
    let members = connection
        .list_all_team_members()
        .map_err(|error| OriginStreamError::Db(error.to_string()))?;
    if members.is_empty() {
        return Ok(None);
    }
    Ok(Some(members.iter().any(|member| {
        member.origin_node_id == origin_node_id && member.state == "active"
    })))
}

/// Resolve `(origin_node_id, generation)` to the stored Ed25519 verifying key.
pub struct TeamMemberKeyVerifier<'a> {
    pub connection: &'a DbConnection,
}

impl OriginSignatureVerifier for TeamMemberKeyVerifier<'_> {
    fn verify(
        &self,
        origin_node_id: &str,
        signing_key_generation: u64,
        domain: &str,
        canonical_bytes: &[u8],
        signature: &str,
    ) -> bool {
        let Ok(Some(node)) = self
            .connection
            .get_team_member_node(origin_node_id, signing_key_generation)
        else {
            return false;
        };
        if node.state != "active" {
            return false;
        }
        let Some(key_bytes) = hex_decode(&node.verifying_key_hex) else {
            return false;
        };
        let Ok(key_array) = <[u8; 32]>::try_from(key_bytes.as_slice()) else {
            return false;
        };
        let Ok(verifying_key) = ed25519_dalek::VerifyingKey::from_bytes(&key_array) else {
            return false;
        };
        verify_ed25519_origin_signature(&verifying_key, domain, canonical_bytes, signature)
    }
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
    let nodes = connection
        .list_all_team_member_nodes()
        .map_err(|error| OriginStreamError::Db(error.to_string()))?
        .into_iter()
        .map(|row| TeamMemberNodeRecord {
            node_id: row.node_id,
            member_id: row.member_id,
            team_id: row.team_id,
            verifying_key_hex: row.verifying_key_hex,
            signing_key_generation: row.signing_key_generation,
            state: row.state,
            bound_at: row.bound_at,
        })
        .collect();
    let paused = any_local_team_paused(connection)?;
    let steward_would_sync = plan_team_steward_once(connection)
        .map(|plan| plan.ran_sync)
        .unwrap_or(false);
    Ok(TeamStatusReport {
        schema: TEAM_STATUS_SCHEMA_V1,
        command: "team status",
        team_count: teams.len(),
        teams,
        members,
        nodes,
        paused,
        steward_would_sync,
        mesh_primitives: vec![
            "mesh_origin_events",
            "ee.team.manifest_event.v1",
            "team_members",
            "team_member_nodes",
            "team_posture",
            "steward_decision",
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
            format!("ee daemon install {workspace_flag} --json"),
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
    pub inviter_verifying_key: String,
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
pub struct TeamJoinHelloV1 {
    pub schema: String,
    pub invite_id: String,
    pub joiner_node_id: String,
    pub joiner_display_name: String,
    pub joiner_nonce: String,
    pub joiner_verifying_key: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TeamJoinChallengeV1 {
    pub schema: String,
    pub invite_id: String,
    pub team_id: String,
    pub origin_node_id: String,
    pub hello_port: u16,
    pub genesis_event_hash: String,
    pub joiner_nonce: String,
    pub inviter_nonce: String,
    pub inviter_verifying_key: String,
    pub signature: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TeamJoinProveV1 {
    pub schema: String,
    pub invite_id: String,
    pub secret: String,
    pub joiner_node_id: String,
    pub joiner_display_name: String,
    pub joiner_nonce: String,
    pub inviter_nonce: String,
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
    pub pair_confirmation: String,
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
    mint_team_invite_with_store(connection, endpoint, produced_at, expires_at, None)
}

/// Mint an invite, pinning the inviter verifying key when a key store is given.
pub fn mint_team_invite_with_store(
    connection: &DbConnection,
    endpoint: &str,
    produced_at: &str,
    expires_at: &str,
    workspace_path: Option<&std::path::Path>,
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
    if produced_at < invite_auth_floor(connection, &team.team_id)?.as_str() {
        return Err(OriginStreamError::Encode(
            "invite mint is below the authorization clock floor".to_owned(),
        ));
    }
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
    connection
        .raise_team_invite_auth_floor(&team.team_id, produced_at, produced_at)
        .map_err(|error| OriginStreamError::Db(error.to_string()))?;
    let inviter_verifying_key = match workspace_path {
        Some(path) => {
            let signer =
                Ed25519OriginSigner::load_or_create(path, &team.origin_node_id, produced_at)?;
            hex_encode(&signer.verifying_key_bytes())
        }
        None => String::new(),
    };
    let code = encode_invite_code(&TeamInviteCodeV1 {
        schema: TEAM_INVITE_SCHEMA_V1.to_owned(),
        invite_id: invite_id.clone(),
        team_id: team.team_id.clone(),
        origin_node_id: team.origin_node_id,
        hello_port: team.hello_port,
        endpoint: endpoint.to_owned(),
        genesis_event_hash: team.genesis_event_hash,
        secret,
        inviter_verifying_key,
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
    serve_one_bootstrap_join_with_store(connection, workspace_id, listener, timeout, None)
}

/// Accept one join, signing the challenge from the workspace key store.
pub fn serve_one_bootstrap_join_with_store(
    connection: &DbConnection,
    workspace_id: &str,
    listener: &std::net::TcpListener,
    timeout: std::time::Duration,
    workspace_path: Option<&std::path::Path>,
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
    let hello = match serde_json::from_value::<TeamJoinHelloV1>(read_join_payload(&mut stream)?) {
        Ok(hello) if hello.schema == TEAM_JOIN_HELLO_SCHEMA_V1 => hello,
        _ => {
            write_join_decline(&mut stream, "bootstrap_malformed")?;
            return Err(OriginStreamError::Encode(
                "bootstrap join hello is malformed".to_owned(),
            ));
        }
    };
    let Some(path) = workspace_path else {
        write_join_decline(&mut stream, "bootstrap_malformed")?;
        return Err(OriginStreamError::Encode(
            "join challenge requires a key store".to_owned(),
        ));
    };
    let invite = connection
        .get_team_pending_invite(&hello.invite_id)
        .map_err(|error| OriginStreamError::Db(error.to_string()))?
        .ok_or_else(|| OriginStreamError::Encode("unknown invite".to_owned()))?;
    let signer =
        Ed25519OriginSigner::load_or_create(path, &invite.origin_node_id, &invite.created_at)?;
    let challenge = match sign_join_challenge(&signer, &invite, &hello) {
        Ok(challenge) => challenge,
        Err(error) => {
            write_join_decline(&mut stream, "bootstrap_malformed")?;
            return Err(error);
        }
    };
    write_join_payload(
        &mut stream,
        serde_json::to_value(&challenge)
            .map_err(|error| OriginStreamError::Encode(error.to_string()))?,
    )?;
    let prove = match serde_json::from_value::<TeamJoinProveV1>(read_join_payload(&mut stream)?) {
        Ok(prove) if prove.schema == TEAM_JOIN_PROVE_SCHEMA_V1 => prove,
        _ => {
            write_join_decline(&mut stream, "bootstrap_malformed")?;
            return Err(OriginStreamError::Encode(
                "bootstrap join prove is malformed".to_owned(),
            ));
        }
    };
    if prove.invite_id != hello.invite_id
        || prove.joiner_nonce != hello.joiner_nonce
        || prove.inviter_nonce != challenge.inviter_nonce
    {
        write_join_decline(&mut stream, "bootstrap_malformed")?;
        return Err(OriginStreamError::Encode(
            "join prove does not match the signed challenge".to_owned(),
        ));
    }
    let redeemed_at = chrono::Utc::now().to_rfc3339();
    let mut granted =
        match redeem_team_invite(connection, &prove.invite_id, &prove.secret, &redeemed_at) {
            Ok(granted) => granted,
            Err(error) => {
                write_join_decline(&mut stream, "bootstrap_malformed")?;
                return Err(error);
            }
        };
    let pair = derive_team_pair_key(
        &prove.secret,
        &granted.team_id,
        &prove.invite_id,
        &prove.joiner_node_id,
        &granted.origin_node_id,
        &prove.joiner_nonce,
        &prove.inviter_nonce,
    );
    granted.pair_confirmation = pair_confirmation(&pair);
    record_inviter_side_join_member(
        connection,
        workspace_id,
        &granted,
        &prove.joiner_node_id,
        &prove.joiner_display_name,
        &redeemed_at,
        Some(hello.joiner_verifying_key.as_str()).filter(|key| key.len() == 64),
    )?;
    persist_pair_key(
        path,
        &granted.team_id,
        &prove.joiner_node_id,
        &pair,
        &redeemed_at,
    )?;
    write_join_payload(
        &mut stream,
        serde_json::to_value(&granted)
            .map_err(|error| OriginStreamError::Encode(error.to_string()))?,
    )?;
    Ok(granted)
}

fn read_join_payload(
    stream: &mut std::net::TcpStream,
) -> Result<serde_json::Value, OriginStreamError> {
    let bytes =
        read_std_framed(stream).map_err(|error| OriginStreamError::Encode(error.to_string()))?;
    if let Ok(decline) = serde_json::from_slice::<BootstrapDeclineV1>(&bytes)
        && decline.schema == BOOTSTRAP_DECLINE_SCHEMA_V1
    {
        return Err(OriginStreamError::Encode(format!(
            "bootstrap join declined: {}",
            decline.code
        )));
    }
    let envelope =
        decode_envelope(&bytes).map_err(|error| OriginStreamError::Encode(error.to_string()))?;
    if envelope.capability != BootstrapCapability::Join {
        return Err(OriginStreamError::Encode(
            "bootstrap join expected join capability".to_owned(),
        ));
    }
    Ok(envelope.payload)
}

fn write_join_payload(
    stream: &mut std::net::TcpStream,
    payload: serde_json::Value,
) -> Result<(), OriginStreamError> {
    let reply = encode_envelope(BootstrapCapability::Join, payload)
        .map_err(|error| OriginStreamError::Encode(error.to_string()))?;
    write_std_framed(stream, &reply).map_err(|error| OriginStreamError::Encode(error.to_string()))
}

fn length_prefixed(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(4 + bytes.len());
    out.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
    out.extend_from_slice(bytes);
    out
}

/// Derive the long-term pair key. A copied invite without the live nonces
/// cannot reproduce this key.
pub fn derive_team_pair_key(
    secret: &str,
    team_id: &str,
    invite_id: &str,
    joiner_node_id: &str,
    origin_node_id: &str,
    joiner_nonce: &str,
    inviter_nonce: &str,
) -> [u8; 32] {
    let mut transcript = Vec::new();
    for part in [
        "ee.team.pair.v1",
        team_id,
        invite_id,
        joiner_node_id,
        origin_node_id,
        joiner_nonce,
        inviter_nonce,
    ] {
        transcript.extend_from_slice(&length_prefixed(part.as_bytes()));
    }
    let transcript_hash = blake3::derive_key(TEAM_PAIR_TRANSCRIPT_DOMAIN, &transcript);
    let mut material = length_prefixed(secret.as_bytes());
    material.extend_from_slice(&length_prefixed(&transcript_hash));
    blake3::derive_key(TEAM_PAIR_KDF_DOMAIN, &material)
}

pub fn pair_confirmation(pair_key: &[u8; 32]) -> String {
    format!(
        "blake3:{}",
        blake3::derive_key("ee.team.pair.confirm.v1", pair_key)
            .iter()
            .fold(String::new(), |mut out, byte| {
                const HEX: &[u8; 16] = b"0123456789abcdef";
                out.push(HEX[(byte >> 4) as usize] as char);
                out.push(HEX[(byte & 0x0f) as usize] as char);
                out
            })
    )
}

pub(crate) fn persist_pair_key(
    workspace_path: &std::path::Path,
    team_id: &str,
    remote_node_id: &str,
    pair: &[u8; 32],
    produced_at: &str,
) -> Result<(), OriginStreamError> {
    let digest = blake3::hash(format!("{team_id}:{remote_node_id}").as_bytes());
    let handle = format!("peer_{}", &digest.to_hex().as_str()[..32]);
    let store = crate::mesh::key_store::MeshKeyStore::open_or_create(workspace_path)
        .map_err(|error| OriginStreamError::Encode(error.to_string()))?;
    store
        .store_pair_key(
            &handle,
            crate::mesh::key_store::PairKeyClass::Current,
            std::num::NonZeroU64::MIN,
            &crate::mesh::key_store::SecretBytes::new(*pair),
            produced_at,
            true,
        )
        .map_err(|error| OriginStreamError::Encode(error.to_string()))
}

fn challenge_preimage(challenge: &TeamJoinChallengeV1) -> Vec<u8> {
    let mut bytes = Vec::new();
    for part in [
        challenge.invite_id.as_str(),
        challenge.team_id.as_str(),
        challenge.origin_node_id.as_str(),
        challenge.genesis_event_hash.as_str(),
        challenge.joiner_nonce.as_str(),
        challenge.inviter_nonce.as_str(),
        challenge.inviter_verifying_key.as_str(),
    ] {
        bytes.extend_from_slice(&length_prefixed(part.as_bytes()));
    }
    bytes.extend_from_slice(&challenge.hello_port.to_le_bytes());
    bytes
}

pub(crate) fn sign_join_challenge(
    signer: &Ed25519OriginSigner,
    invite: &crate::db::StoredTeamPendingInvite,
    hello: &TeamJoinHelloV1,
) -> Result<TeamJoinChallengeV1, OriginStreamError> {
    let mut challenge = TeamJoinChallengeV1 {
        schema: TEAM_JOIN_CHALLENGE_SCHEMA_V1.to_owned(),
        invite_id: hello.invite_id.clone(),
        team_id: invite.team_id.clone(),
        origin_node_id: invite.origin_node_id.clone(),
        hello_port: invite.hello_port,
        genesis_event_hash: invite.genesis_event_hash.clone(),
        joiner_nonce: hello.joiner_nonce.clone(),
        inviter_nonce: random_hex_32()?,
        inviter_verifying_key: hex_encode(&signer.verifying_key_bytes()),
        signature: String::new(),
    };
    challenge.signature = signer.sign(TEAM_JOIN_CHALLENGE_DOMAIN, &challenge_preimage(&challenge));
    Ok(challenge)
}

pub fn verify_join_challenge(
    expected_verifying_key: &str,
    invite: &TeamInviteCodeV1,
    hello: &TeamJoinHelloV1,
    challenge: &TeamJoinChallengeV1,
) -> Result<(), OriginStreamError> {
    if challenge.schema != TEAM_JOIN_CHALLENGE_SCHEMA_V1
        || challenge.invite_id != invite.invite_id
        || challenge.team_id != invite.team_id
        || challenge.origin_node_id != invite.origin_node_id
        || challenge.genesis_event_hash != invite.genesis_event_hash
        || challenge.hello_port != invite.hello_port
        || challenge.joiner_nonce != hello.joiner_nonce
        || challenge.inviter_verifying_key != expected_verifying_key
    {
        return Err(OriginStreamError::Encode(
            "join challenge does not bind the invite".to_owned(),
        ));
    }
    let key_bytes = hex_decode(expected_verifying_key)
        .ok_or_else(|| OriginStreamError::Encode("invite verifying key is not hex".to_owned()))?;
    let key_array: [u8; 32] = key_bytes.as_slice().try_into().map_err(|_| {
        OriginStreamError::Encode("invite verifying key must be 32 bytes".to_owned())
    })?;
    let verifying_key = ed25519_dalek::VerifyingKey::from_bytes(&key_array)
        .map_err(|error| OriginStreamError::Encode(format!("verifying key: {error}")))?;
    if !crate::mesh::origin_stream::verify_ed25519_origin_signature(
        &verifying_key,
        TEAM_JOIN_CHALLENGE_DOMAIN,
        &challenge_preimage(challenge),
        &challenge.signature,
    ) {
        return Err(OriginStreamError::Encode(
            "join challenge signature is invalid".to_owned(),
        ));
    }
    Ok(())
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

/// Revoke a pending invite and raise the authorization floor.
pub fn revoke_team_invite(
    connection: &DbConnection,
    invite_id: &str,
    revoked_at: &str,
) -> Result<bool, OriginStreamError> {
    let invite = connection
        .get_team_pending_invite(invite_id)
        .map_err(|error| OriginStreamError::Db(error.to_string()))?
        .ok_or_else(|| OriginStreamError::Encode("unknown invite".to_owned()))?;
    if revoked_at < invite_auth_floor(connection, &invite.team_id)?.as_str() {
        return Err(OriginStreamError::Encode(
            "invite revoke is below the authorization clock floor".to_owned(),
        ));
    }
    let changed = connection
        .revoke_team_pending_invite(invite_id, revoked_at)
        .map_err(|error| OriginStreamError::Db(error.to_string()))?;
    if changed {
        connection
            .raise_team_invite_auth_floor(&invite.team_id, revoked_at, revoked_at)
            .map_err(|error| OriginStreamError::Db(error.to_string()))?;
    }
    Ok(changed)
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
    if redeemed_at < invite_auth_floor(connection, &invite.team_id)?.as_str() {
        return Err(OriginStreamError::Encode(
            "invite redeem is below the authorization clock floor".to_owned(),
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
    connection
        .raise_team_invite_auth_floor(&invite.team_id, redeemed_at, redeemed_at)
        .map_err(|error| OriginStreamError::Db(error.to_string()))?;
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
        pair_confirmation: String::new(),
    })
}

fn history_revision_id(memory_id: &str, updated_at: &str) -> String {
    let digest = blake3::hash(format!("{memory_id}:{updated_at}").as_bytes());
    format!("rev_{}", &digest.to_hex().as_str()[..24])
}

fn history_consent_hash(team_id: &str, items: &[TeamHistoryShareItem]) -> String {
    let mut material = length_prefixed(TEAM_HISTORY_CONSENT_DOMAIN.as_bytes());
    material.extend_from_slice(&length_prefixed(team_id.as_bytes()));
    for item in items {
        material.extend_from_slice(&length_prefixed(item.memory_id.as_bytes()));
        material.extend_from_slice(&length_prefixed(item.revision_id.as_bytes()));
    }
    format!("blake3:{}", blake3::hash(&material).to_hex())
}

/// Preview or project origin-owned local memories as metadata-only history.
pub fn share_team_history(
    connection: &DbConnection,
    workspace_id: &str,
    produced_at: &str,
    confirm: bool,
    limit: usize,
    workspace_path: Option<&std::path::Path>,
) -> Result<TeamHistoryShareReport, OriginStreamError> {
    let team = load_local_teams(connection)?
        .into_iter()
        .next()
        .ok_or_else(|| OriginStreamError::Encode("no local team genesis".to_owned()))?;
    let cap = limit.max(1).min(256);
    let memories = connection
        .list_memories(workspace_id, None, false)
        .map_err(|error| OriginStreamError::Db(error.to_string()))?;
    let mut items = Vec::new();
    for memory in memories.into_iter().take(cap) {
        let revision_id = history_revision_id(&memory.id, &memory.updated_at);
        let already = connection
            .team_history_projection_exists(&team.team_id, &memory.id, &revision_id)
            .map_err(|error| OriginStreamError::Db(error.to_string()))?;
        items.push(TeamHistoryShareItem {
            memory_id: memory.id,
            revision_id,
            level: memory.level,
            kind: memory.kind,
            created_at: memory.created_at,
            already_projected: already,
        });
    }
    let consent_hash = history_consent_hash(&team.team_id, &items);
    if team_is_paused(connection, &team.team_id)? && confirm {
        return Err(OriginStreamError::Encode(
            "team is paused; resume before sharing history".to_owned(),
        ));
    }
    if !confirm {
        let skipped = items.iter().filter(|item| item.already_projected).count();
        return Ok(TeamHistoryShareReport {
            schema: TEAM_SHARE_HISTORY_SCHEMA_V1,
            command: "team share history",
            team_id: team.team_id,
            confirmed: false,
            consent_hash,
            candidate_count: items.len(),
            projected_count: 0,
            skipped_count: skipped,
            items,
            mesh_primitives: vec!["team_history_projections", "ee.mesh.memory_event.v1"],
        });
    }

    let signer_store = workspace_path
        .map(|path| Ed25519OriginSigner::load_or_create(path, &team.origin_node_id, produced_at))
        .transpose()?;
    let mac = LocalOriginSigner::for_workspace(workspace_id);
    let signer: &dyn OriginSigner = signer_store
        .as_ref()
        .map(|signer| signer as &dyn OriginSigner)
        .unwrap_or(&mac);

    let mut projected = 0_usize;
    let mut skipped = 0_usize;
    let mut projected_items = Vec::new();
    for item in items {
        if item.already_projected {
            skipped = skipped.saturating_add(1);
            projected_items.push(item);
            continue;
        }
        let Some(memory) = connection
            .list_memories(workspace_id, None, false)
            .map_err(|error| OriginStreamError::Db(error.to_string()))?
            .into_iter()
            .find(|row| row.id == item.memory_id)
        else {
            skipped = skipped.saturating_add(1);
            projected_items.push(item);
            continue;
        };
        if history_revision_id(&memory.id, &memory.updated_at) != item.revision_id {
            return Err(OriginStreamError::Encode(
                "history preview is stale; rerun without --confirm".to_owned(),
            ));
        }
        let mut nonce = [0_u8; 32];
        getrandom::fill(&mut nonce)
            .map_err(|error| OriginStreamError::Encode(format!("csprng unavailable: {error}")))?;
        let payload = OriginEventPayload::Memory(MemoryEventPayload {
            operation: MemoryEventOperation::Create,
            logical_memory_id: memory.id.clone(),
            revision_id: item.revision_id.clone(),
            predecessor_revision_id: None,
            level: Some(memory.level.clone()),
            memory_kind: Some(memory.kind.clone()),
            valid_from: memory.valid_from.clone(),
            valid_until: memory.valid_to.clone(),
            project_binding: None,
            origin_trust_claim: Some(memory.trust_class.clone()),
            provenance_refs: Vec::new(),
            body_representation: Some("omitted".to_owned()),
            redaction_provenance: None,
            body_commitment: body_commitment(&nonce, memory.content.as_bytes()),
        });
        let appended = append_origin_event(
            connection,
            signer,
            &OriginAppendRequest {
                team_id: &team.team_id,
                origin_node_id: &team.origin_node_id,
                payload,
                required_features: Vec::new(),
                produced_at,
                body_nonce: Some(nonce),
            },
        )?;
        let wrote = connection
            .insert_team_history_projection(&InsertTeamHistoryProjectionInput {
                team_id: team.team_id.clone(),
                memory_id: item.memory_id.clone(),
                revision_id: item.revision_id.clone(),
                origin_event_id: appended.event_id,
                projected_at: produced_at.to_owned(),
            })
            .map_err(|error| OriginStreamError::Db(error.to_string()))?;
        if wrote {
            projected = projected.saturating_add(1);
        } else {
            skipped = skipped.saturating_add(1);
        }
        projected_items.push(TeamHistoryShareItem {
            already_projected: true,
            ..item
        });
    }
    Ok(TeamHistoryShareReport {
        schema: TEAM_SHARE_HISTORY_SCHEMA_V1,
        command: "team share history",
        team_id: team.team_id,
        confirmed: true,
        consent_hash,
        candidate_count: projected_items.len(),
        projected_count: projected,
        skipped_count: skipped,
        items: projected_items,
        mesh_primitives: vec![
            "team_history_projections",
            "mesh_origin_events.append",
            "ee.mesh.memory_event.v1",
        ],
    })
}

/// Stable cache key for one origin-owned memory body.
#[must_use]
pub fn team_body_cache_key(memory_id: &str) -> String {
    format!("body_{}", memory_id.trim_start_matches("mem_"))
}

fn body_cache_key(memory_id: &str) -> String {
    team_body_cache_key(memory_id)
}

/// Read one published body from the hardened local cache. Unshared or
/// missing rows stay metadata-only and never return bytes.
pub fn fetch_local_team_body(
    connection: &DbConnection,
    workspace_id: &str,
    workspace_path: &std::path::Path,
    body_cache_key: &str,
) -> Result<crate::mesh::bootstrap_envelope::BodyFetchResponse, OriginStreamError> {
    use crate::mesh::bootstrap_envelope::{BODY_FETCH_RESPONSE_SCHEMA_V1, BodyFetchResponse};
    let empty = |status: &str, size: u64| BodyFetchResponse {
        schema: BODY_FETCH_RESPONSE_SCHEMA_V1.to_owned(),
        body_cache_key: body_cache_key.to_owned(),
        cache_status: status.to_owned(),
        size_bytes: size,
        body_hex: None,
    };
    if body_cache_key.is_empty() {
        return Ok(empty("metadata_only", 0));
    }
    let Some(row) = connection
        .get_mesh_body_cache_metadata(workspace_id, body_cache_key)
        .map_err(|error| OriginStreamError::Db(error.to_string()))?
    else {
        return Ok(empty("metadata_only", 0));
    };
    if row.cache_status != "available" {
        return Ok(empty(&row.cache_status, row.size_bytes.unwrap_or(0)));
    }
    let cache_dir = workspace_path.join(".ee").join("mesh-body-cache");
    let cache = crate::mesh::key_store::SecureLocalDir::open_existing(workspace_path, &cache_dir)
        .map_err(|error| OriginStreamError::Encode(error.to_string()))?;
    let Some(cache) = cache else {
        return Ok(empty("metadata_only", row.size_bytes.unwrap_or(0)));
    };
    match cache.read(body_cache_key) {
        Ok(Some(bytes)) => Ok(BodyFetchResponse {
            schema: BODY_FETCH_RESPONSE_SCHEMA_V1.to_owned(),
            body_cache_key: body_cache_key.to_owned(),
            cache_status: "available".to_owned(),
            size_bytes: u64::try_from(bytes.len()).unwrap_or(0),
            body_hex: Some(hex_encode(&bytes)),
        }),
        Ok(None) | Err(_) => Ok(empty("metadata_only", row.size_bytes.unwrap_or(0))),
    }
}

fn bodies_consent_hash(team_id: &str, items: &[TeamBodyShareItem]) -> String {
    let mut material = length_prefixed(TEAM_BODIES_CONSENT_DOMAIN.as_bytes());
    material.extend_from_slice(&length_prefixed(team_id.as_bytes()));
    for item in items {
        material.extend_from_slice(&length_prefixed(item.memory_id.as_bytes()));
        material.extend_from_slice(&length_prefixed(item.revision_id.as_bytes()));
        material.extend_from_slice(&length_prefixed(item.cache_status.as_bytes()));
        material.extend_from_slice(&item.size_bytes.to_le_bytes());
    }
    format!("blake3:{}", blake3::hash(&material).to_hex())
}

/// Preview or publish origin-owned bodies into the hardened local cache.
pub fn share_team_bodies(
    connection: &DbConnection,
    workspace_id: &str,
    produced_at: &str,
    confirm: bool,
    limit: usize,
    workspace_path: Option<&std::path::Path>,
    issue_token: bool,
    approval_token: Option<&str>,
) -> Result<TeamBodyShareReport, OriginStreamError> {
    let team = load_local_teams(connection)?
        .into_iter()
        .next()
        .ok_or_else(|| OriginStreamError::Encode("no local team genesis".to_owned()))?;
    let cap = limit.max(1).min(256);
    let memories = connection
        .list_memories(workspace_id, None, false)
        .map_err(|error| OriginStreamError::Db(error.to_string()))?;
    let mut items = Vec::new();
    for memory in memories.into_iter().take(cap) {
        let revision_id = history_revision_id(&memory.id, &memory.updated_at);
        let key = body_cache_key(&memory.id);
        let cache_status = connection
            .get_mesh_body_cache_metadata(workspace_id, &key)
            .map_err(|error| OriginStreamError::Db(error.to_string()))?
            .map(|row| row.cache_status)
            .unwrap_or_else(|| "metadata_only".to_owned());
        items.push(TeamBodyShareItem {
            memory_id: memory.id,
            revision_id,
            size_bytes: u64::try_from(memory.content.len()).unwrap_or(u64::MAX),
            cache_status,
        });
    }
    let consent_hash = bodies_consent_hash(&team.team_id, &items);
    if issue_token && confirm {
        return Err(OriginStreamError::Encode(
            "issue a body-share token on preview, then confirm with --token".to_owned(),
        ));
    }
    if let Some(token) = approval_token {
        if !confirm {
            return Err(OriginStreamError::Encode(
                "approval tokens are only consumed by --confirm".to_owned(),
            ));
        }
        let store_path = workspace_path.ok_or_else(|| {
            OriginStreamError::Encode(
                "body-share token verify requires a workspace path".to_owned(),
            )
        })?;
        verify_body_share_token(store_path, workspace_id, &consent_hash, token)?;
    }
    if !confirm {
        let token = if issue_token {
            let store_path = workspace_path.ok_or_else(|| {
                OriginStreamError::Encode(
                    "issuing a body-share token requires a workspace path".to_owned(),
                )
            })?;
            Some(issue_body_share_token(
                store_path,
                workspace_id,
                &consent_hash,
            )?)
        } else {
            None
        };
        return Ok(TeamBodyShareReport {
            schema: TEAM_SHARE_BODIES_SCHEMA_V1,
            command: "team share bodies",
            team_id: team.team_id,
            confirmed: false,
            consent_hash,
            candidate_count: items.len(),
            published_count: 0,
            skipped_count: items
                .iter()
                .filter(|item| item.cache_status == "available")
                .count(),
            items,
            approval_token: token,
            mesh_primitives: vec!["mesh_body_cache_metadata", "ee.mesh.memory_event.v1"],
        });
    }
    if team_is_paused(connection, &team.team_id)? {
        return Err(OriginStreamError::Encode(
            "team is paused; resume before sharing bodies".to_owned(),
        ));
    }
    let Some(store_path) = workspace_path else {
        return Err(OriginStreamError::Encode(
            "sharing bodies requires a workspace key-store path".to_owned(),
        ));
    };
    let cache_dir = store_path.join(".ee").join("mesh-body-cache");
    let cache = crate::mesh::key_store::SecureLocalDir::open_or_create(store_path, &cache_dir)
        .map_err(|error| OriginStreamError::Encode(error.to_string()))?;
    let signer_store =
        Ed25519OriginSigner::load_or_create(store_path, &team.origin_node_id, produced_at)?;
    let mut published = 0_usize;
    let mut skipped = 0_usize;
    let mut out = Vec::new();
    for item in items {
        if item.cache_status == "available" {
            skipped = skipped.saturating_add(1);
            out.push(item);
            continue;
        }
        let Some(memory) = connection
            .list_memories(workspace_id, None, false)
            .map_err(|error| OriginStreamError::Db(error.to_string()))?
            .into_iter()
            .find(|row| row.id == item.memory_id)
        else {
            skipped = skipped.saturating_add(1);
            out.push(item);
            continue;
        };
        if u64::try_from(memory.content.len()).unwrap_or(u64::MAX)
            > crate::mesh::key_store::MAX_RECORD_BYTES
        {
            return Err(OriginStreamError::Encode(
                "body exceeds the hardened cache record cap".to_owned(),
            ));
        }
        let mut nonce = [0_u8; 32];
        getrandom::fill(&mut nonce)
            .map_err(|error| OriginStreamError::Encode(format!("csprng unavailable: {error}")))?;
        let commitment = body_commitment(&nonce, memory.content.as_bytes());
        let payload = OriginEventPayload::Memory(MemoryEventPayload {
            operation: MemoryEventOperation::Create,
            logical_memory_id: memory.id.clone(),
            revision_id: item.revision_id.clone(),
            predecessor_revision_id: None,
            level: Some(memory.level.clone()),
            memory_kind: Some(memory.kind.clone()),
            valid_from: memory.valid_from.clone(),
            valid_until: memory.valid_to.clone(),
            project_binding: None,
            origin_trust_claim: Some(memory.trust_class.clone()),
            provenance_refs: Vec::new(),
            body_representation: Some("exact".to_owned()),
            redaction_provenance: None,
            body_commitment: commitment.clone(),
        });
        append_origin_event(
            connection,
            &signer_store,
            &OriginAppendRequest {
                team_id: &team.team_id,
                origin_node_id: &team.origin_node_id,
                payload,
                required_features: Vec::new(),
                produced_at,
                body_nonce: Some(nonce),
            },
        )?;
        let key = body_cache_key(&memory.id);
        cache
            .write_replace(&key, memory.content.as_bytes())
            .map_err(|error| OriginStreamError::Encode(error.to_string()))?;
        connection
            .upsert_mesh_body_cache_metadata(&UpsertMeshBodyCacheMetadataInput {
                workspace_id: workspace_id.to_owned(),
                body_cache_key: key,
                origin_node_id: team.origin_node_id.clone(),
                origin_workspace_id: workspace_id.to_owned(),
                logical_memory_id: memory.id.clone(),
                content_hash: commitment,
                body_ref_json: None,
                preview_hash: None,
                size_bytes: Some(u64::try_from(memory.content.len()).unwrap_or(0)),
                cache_status: "available".to_owned(),
                local_body_hash: Some(format!(
                    "blake3:{}",
                    blake3::hash(memory.content.as_bytes()).to_hex()
                )),
                cached_at: Some(produced_at.to_owned()),
                expires_at: None,
            })
            .map_err(|error| OriginStreamError::Db(error.to_string()))?;
        published = published.saturating_add(1);
        out.push(TeamBodyShareItem {
            cache_status: "available".to_owned(),
            ..item
        });
    }
    Ok(TeamBodyShareReport {
        schema: TEAM_SHARE_BODIES_SCHEMA_V1,
        command: "team share bodies",
        team_id: team.team_id,
        confirmed: true,
        consent_hash,
        candidate_count: out.len(),
        published_count: published,
        skipped_count: skipped,
        items: out,
        approval_token: None,
        mesh_primitives: vec![
            "mesh_body_cache_metadata",
            "secure_local_dir",
            "mesh_origin_events.append",
            "ee.mesh.memory_event.v1",
        ],
    })
}

const BODY_SHARE_TOKEN_SURFACE: &str = "ee.team.share.bodies.v1";

fn issue_body_share_token(
    workspace_path: &std::path::Path,
    workspace_id: &str,
    consent_hash: &str,
) -> Result<String, OriginStreamError> {
    let keys_dir = crate::policy::store_auth::workspace_keys_dir(workspace_path);
    let root = crate::policy::store_auth::StoreAuthRoot::open_or_create(keys_dir)
        .map_err(|error| OriginStreamError::Encode(error.to_string()))?;
    let issued = crate::mesh::lane_grant::issue(
        &root,
        crate::mesh::lane_grant::ApprovalPurpose::Body,
        workspace_id,
        BODY_SHARE_TOKEN_SURFACE,
        consent_hash.as_bytes(),
        chrono::Utc::now().timestamp(),
    )
    .map_err(|error| OriginStreamError::Encode(error.to_string()))?;
    Ok(issued.token().expose_bearer())
}

fn verify_body_share_token(
    workspace_path: &std::path::Path,
    workspace_id: &str,
    consent_hash: &str,
    token: &str,
) -> Result<(), OriginStreamError> {
    let keys_dir = crate::policy::store_auth::workspace_keys_dir(workspace_path);
    let root = crate::policy::store_auth::StoreAuthRoot::open_or_create(keys_dir)
        .map_err(|error| OriginStreamError::Encode(error.to_string()))?;
    let now = chrono::Utc::now().timestamp();
    let authentic = crate::mesh::lane_grant::verify_authentic(
        &root,
        crate::mesh::lane_grant::ApprovalPurpose::Body,
        workspace_id,
        BODY_SHARE_TOKEN_SURFACE,
        token,
        now,
    )
    .map_err(|error| OriginStreamError::Encode(error.to_string()))?;
    crate::mesh::lane_grant::compare_snapshot(&root, &authentic, consent_hash.as_bytes(), now)
        .map_err(|error| OriginStreamError::Encode(error.to_string()))?;
    Ok(())
}

/// Stop future body serving from this node. Cached bytes are not erased.
pub fn unshare_team_bodies(
    connection: &DbConnection,
    workspace_id: &str,
    produced_at: &str,
) -> Result<TeamBodyShareReport, OriginStreamError> {
    let team = load_local_teams(connection)?
        .into_iter()
        .next()
        .ok_or_else(|| OriginStreamError::Encode("no local team genesis".to_owned()))?;
    if team_is_paused(connection, &team.team_id)? {
        return Err(OriginStreamError::Encode(
            "team is paused; resume before unsharing bodies".to_owned(),
        ));
    }
    let memories = connection
        .list_memories(workspace_id, None, false)
        .map_err(|error| OriginStreamError::Db(error.to_string()))?;
    let mut items = Vec::new();
    let mut published = 0_usize;
    for memory in memories {
        let key = body_cache_key(&memory.id);
        let Some(existing) = connection
            .get_mesh_body_cache_metadata(workspace_id, &key)
            .map_err(|error| OriginStreamError::Db(error.to_string()))?
        else {
            continue;
        };
        let revision_id = history_revision_id(&memory.id, &memory.updated_at);
        if existing.cache_status != "available" {
            items.push(TeamBodyShareItem {
                memory_id: memory.id,
                revision_id,
                size_bytes: existing.size_bytes.unwrap_or(0),
                cache_status: existing.cache_status,
            });
            continue;
        }
        connection
            .upsert_mesh_body_cache_metadata(&UpsertMeshBodyCacheMetadataInput {
                workspace_id: workspace_id.to_owned(),
                body_cache_key: key,
                origin_node_id: existing.origin_node_id,
                origin_workspace_id: existing.origin_workspace_id,
                logical_memory_id: existing.logical_memory_id,
                content_hash: existing.content_hash,
                body_ref_json: existing.body_ref_json,
                preview_hash: existing.preview_hash,
                size_bytes: existing.size_bytes,
                cache_status: "evicted".to_owned(),
                local_body_hash: existing.local_body_hash,
                cached_at: Some(produced_at.to_owned()),
                expires_at: existing.expires_at,
            })
            .map_err(|error| OriginStreamError::Db(error.to_string()))?;
        published = published.saturating_add(1);
        items.push(TeamBodyShareItem {
            memory_id: memory.id,
            revision_id,
            size_bytes: existing.size_bytes.unwrap_or(0),
            cache_status: "evicted".to_owned(),
        });
    }
    Ok(TeamBodyShareReport {
        schema: TEAM_UNSHARE_BODIES_SCHEMA_V1,
        command: "team unshare bodies",
        team_id: team.team_id.clone(),
        confirmed: true,
        consent_hash: bodies_consent_hash(&team.team_id, &items),
        candidate_count: items.len(),
        published_count: published,
        skipped_count: items
            .iter()
            .filter(|item| item.cache_status != "evicted")
            .count(),
        items,
        approval_token: None,
        mesh_primitives: vec!["mesh_body_cache_metadata"],
    })
}

/// Decide whether one foreground steward pass should run mesh sync.
pub fn plan_team_steward_once(
    connection: &DbConnection,
) -> Result<TeamStewardReport, OriginStreamError> {
    let team = load_local_teams(connection)?
        .into_iter()
        .next()
        .ok_or_else(|| OriginStreamError::Encode("no local team genesis".to_owned()))?;
    let paused = team_is_paused(connection, &team.team_id)?;
    let active_member_count = connection
        .list_all_team_members()
        .map_err(|error| OriginStreamError::Db(error.to_string()))?
        .into_iter()
        .filter(|member| member.state == "active")
        .count();
    if paused {
        return Ok(TeamStewardReport {
            schema: TEAM_STEWARD_SCHEMA_V1,
            command: "team steward run-once",
            team_id: team.team_id,
            paused: true,
            active_member_count,
            outcome: "no_op".to_owned(),
            reason: "team_paused".to_owned(),
            ran_sync: false,
            mesh_primitives: vec!["team_posture", "steward_decision"],
        });
    }
    let suspended_identities = connection
        .list_team_member_identities(&team.team_id)
        .map_err(|error| OriginStreamError::Db(error.to_string()))?
        .into_iter()
        .filter(|identity| identity.state == "suspended")
        .count();
    if suspended_identities > 0 {
        return Ok(TeamStewardReport {
            schema: TEAM_STEWARD_SCHEMA_V1,
            command: "team steward run-once",
            team_id: team.team_id,
            paused: false,
            active_member_count,
            outcome: "no_op".to_owned(),
            reason: "identity_revalidation_failed".to_owned(),
            ran_sync: false,
            mesh_primitives: vec![
                "team_idp_policy",
                "team_member_identity",
                "steward_decision",
            ],
        });
    }
    let decision = crate::mesh::steward_decision::decide_steward_outcome(
        &crate::mesh::steward_decision::StewardDecisionInput {
            enabled: true,
            drift_severity: if active_member_count > 1 {
                crate::mesh::steward_decision::DriftSeverity::Warning
            } else {
                crate::mesh::steward_decision::DriftSeverity::None
            },
            drift_kind: if active_member_count > 1 {
                crate::mesh::steward_decision::DriftKind::NewPeersAvailable
            } else {
                crate::mesh::steward_decision::DriftKind::None
            },
            reconciliations_today: 0,
            max_daily: crate::mesh::steward_decision::STEWARD_DEFAULT_MAX_DAILY,
        },
    );
    Ok(TeamStewardReport {
        schema: TEAM_STEWARD_SCHEMA_V1,
        command: "team steward run-once",
        team_id: team.team_id,
        paused: false,
        active_member_count,
        outcome: decision.outcome.as_str().to_owned(),
        reason: decision.reason.to_owned(),
        ran_sync: decision.outcome == crate::mesh::steward_decision::StewardOutcome::Triggered,
        mesh_primitives: vec!["team_members", "steward_decision", "mesh_sync"],
    })
}

/// Read-only team health for `ee team doctor`.
pub fn inspect_team_health(
    connection: &DbConnection,
    workspace_id: &str,
    workspace_path: Option<&std::path::Path>,
) -> Result<TeamDoctorReport, OriginStreamError> {
    let mut checks = Vec::new();
    let teams = load_local_teams(connection)?;
    let Some(team) = teams.into_iter().next() else {
        checks.push(TeamDoctorCheck {
            name: "genesis".to_owned(),
            status: "error".to_owned(),
            message: "no local team genesis".to_owned(),
            repair: Some("ee team create --name \"<team>\" --workspace .".to_owned()),
        });
        return Ok(TeamDoctorReport {
            schema: TEAM_DOCTOR_SCHEMA_V1,
            command: "team doctor",
            team_id: None,
            posture: "no_team".to_owned(),
            checks,
            mesh_primitives: vec!["mesh_origin_events"],
        });
    };
    checks.push(TeamDoctorCheck {
        name: "genesis".to_owned(),
        status: "ok".to_owned(),
        message: format!("team {} exists", team.team_id),
        repair: None,
    });
    let paused = team_is_paused(connection, &team.team_id)?;
    checks.push(if paused {
        TeamDoctorCheck {
            name: "posture".to_owned(),
            status: "warning".to_owned(),
            message: "team is paused; network exchange is fenced".to_owned(),
            repair: Some("ee team resume --confirm --workspace .".to_owned()),
        }
    } else {
        TeamDoctorCheck {
            name: "posture".to_owned(),
            status: "ok".to_owned(),
            message: "team is not paused".to_owned(),
            repair: None,
        }
    });
    let active_members = connection
        .list_all_team_members()
        .map_err(|error| OriginStreamError::Db(error.to_string()))?
        .into_iter()
        .filter(|member| member.state == "active")
        .count();
    checks.push(TeamDoctorCheck {
        name: "members".to_owned(),
        status: if active_members == 0 {
            "error".to_owned()
        } else {
            "ok".to_owned()
        },
        message: format!("{active_members} active member(s)"),
        repair: (active_members == 0).then(|| "ee team members reconcile --workspace .".to_owned()),
    });
    let steward = plan_team_steward_once(connection)?;
    checks.push(TeamDoctorCheck {
        name: "steward".to_owned(),
        status: "ok".to_owned(),
        message: format!(
            "run-once would {} ({})",
            if steward.ran_sync { "sync" } else { "skip" },
            steward.reason
        ),
        repair: None,
    });
    let cached_bodies = connection
        .mesh_storage_status(workspace_id)
        .map(|status| status.cached_body_count)
        .unwrap_or(0);
    let cache_dir_present = workspace_path
        .map(|path| path.join(".ee").join("mesh-body-cache").is_dir())
        .unwrap_or(false);
    checks.push(TeamDoctorCheck {
        name: "body_cache".to_owned(),
        status: "ok".to_owned(),
        message: format!("{cached_bodies} body cache row(s); dir_present={cache_dir_present}"),
        repair: None,
    });
    if let Some(path) = workspace_path {
        let keys = crate::policy::store_auth::workspace_keys_dir(path);
        checks.push(if keys.is_dir() {
            TeamDoctorCheck {
                name: "store_auth".to_owned(),
                status: "ok".to_owned(),
                message: "store-auth key directory is present".to_owned(),
                repair: None,
            }
        } else {
            TeamDoctorCheck {
                name: "store_auth".to_owned(),
                status: "warning".to_owned(),
                message: "store-auth keys are not initialized; body tokens cannot be issued"
                    .to_owned(),
                repair: Some("ee team share bodies --issue-token --workspace .".to_owned()),
            }
        });
    }
    if let Ok(Some(policy)) = connection.get_team_idp_policy(&team.team_id) {
        let identities = connection
            .list_team_member_identities(&team.team_id)
            .unwrap_or_default();
        let suspended = identities
            .iter()
            .filter(|identity| identity.state == "suspended")
            .count();
        checks.push(TeamDoctorCheck {
            name: "idp".to_owned(),
            status: if suspended > 0 {
                "warning".to_owned()
            } else {
                "ok".to_owned()
            },
            message: format!(
                "policy {} gen {} ({} identity row(s), {suspended} suspended)",
                policy.kind,
                policy.policy_generation,
                identities.len()
            ),
            repair: (suspended > 0).then(|| "ee team members revalidate --workspace .".to_owned()),
        });
    } else {
        checks.push(TeamDoctorCheck {
            name: "idp".to_owned(),
            status: "ok".to_owned(),
            message: "no tailnet-attested identity policy".to_owned(),
            repair: None,
        });
    }
    if workspace_path.is_some()
        && let Some(home) = std::env::var_os("HOME").map(std::path::PathBuf::from)
    {
        let kind = crate::daemon::service_install::current_service_kind();
        let installed = crate::daemon::service_install::default_unit_path(kind, &home)
            .is_some_and(|path| path.is_file());
        checks.push(TeamDoctorCheck {
            name: "daemon_service".to_owned(),
            status: if installed {
                "ok".to_owned()
            } else {
                "warning".to_owned()
            },
            message: if installed {
                "user-scoped steward service unit is present".to_owned()
            } else {
                "user-scoped steward service is not installed".to_owned()
            },
            repair: (!installed).then(|| "ee daemon install --confirm".to_owned()),
        });
    }
    let posture = if paused {
        "paused"
    } else if checks.iter().any(|check| check.status == "error") {
        "error"
    } else if checks.iter().any(|check| check.status == "warning") {
        "warning"
    } else {
        "ok"
    };
    Ok(TeamDoctorReport {
        schema: TEAM_DOCTOR_SCHEMA_V1,
        command: "team doctor",
        team_id: Some(team.team_id),
        posture: posture.to_owned(),
        checks,
        mesh_primitives: vec![
            "team_posture",
            "team_members",
            "mesh_body_cache_metadata",
            "steward_decision",
            "team_idp_policy",
        ],
    })
}

/// Persist `ee team idp require --tailnet-attested`.
pub fn require_tailnet_attested(
    connection: &DbConnection,
    workspace_id: &str,
    allowed_domain: Option<&str>,
    produced_at: &str,
    workspace_path: Option<&std::path::Path>,
) -> Result<TeamIdpPolicyReport, OriginStreamError> {
    if any_local_team_paused(connection)? {
        return Err(OriginStreamError::Encode(
            "team is paused; resume before changing identity policy".to_owned(),
        ));
    }
    let domain = allowed_domain
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| value.trim_start_matches('@').to_ascii_lowercase());
    if let Some(domain) = domain.as_deref()
        && (domain.contains('@')
            || domain.starts_with('.')
            || domain.ends_with('.')
            || domain.contains(".."))
    {
        return Err(OriginStreamError::Encode(
            "allowed domain must be a hostname without @".to_owned(),
        ));
    }
    let team = load_local_teams(connection)?
        .into_iter()
        .next()
        .ok_or_else(|| OriginStreamError::Encode("no local team genesis".to_owned()))?;
    let generation = connection
        .upsert_team_idp_policy(
            &team.team_id,
            "tailnet_attested",
            domain.as_deref(),
            produced_at,
        )
        .map_err(|error| OriginStreamError::Db(error.to_string()))?;
    let seed = blake3::hash(format!("{}:idp:{generation}:{produced_at}", team.team_id).as_bytes());
    let hex = seed.to_hex();
    let payload = OriginEventPayload::Manifest(ManifestEventPayload {
        operation: TEAM_IDP_POLICY_SET_OPERATION.to_owned(),
        document_id: format!("tdoc_{}", &hex.as_str()[..24]),
        predecessor_revision_id: None,
        document_payload: serde_json::json!({
            "kind": "tailnet_attested",
            "allowedDomain": domain,
            "policyGeneration": generation,
        }),
    });
    let origin_node_id = team.origin_node_id.clone();
    let ed25519 = workspace_path
        .map(|path| Ed25519OriginSigner::load_or_create(path, &origin_node_id, produced_at))
        .transpose()?;
    let mac = LocalOriginSigner::for_workspace(workspace_id);
    let signer: &dyn OriginSigner = ed25519
        .as_ref()
        .map(|signer| signer as &dyn OriginSigner)
        .unwrap_or(&mac);
    append_origin_event(
        connection,
        signer,
        &OriginAppendRequest {
            team_id: &team.team_id,
            origin_node_id: &origin_node_id,
            payload,
            required_features: Vec::new(),
            produced_at,
            body_nonce: None,
        },
    )?;
    Ok(TeamIdpPolicyReport {
        schema: TEAM_IDP_SCHEMA_V1,
        command: "team idp require",
        team_id: team.team_id,
        kind: "tailnet_attested".to_owned(),
        allowed_domain: domain,
        policy_generation: generation,
        mesh_primitives: vec!["team_idp_policy", "teamIdpPolicySet"],
    })
}

/// Pin a secretless-public OIDC issuer from a local discovery document.
pub fn set_team_oidc_provider(
    connection: &DbConnection,
    issuer: &str,
    client_id: &str,
    discovery: &serde_json::Value,
    set_at: &str,
) -> Result<TeamIdpSetReport, OriginStreamError> {
    if any_local_team_paused(connection)? {
        return Err(OriginStreamError::Encode(
            "team is paused; resume before changing identity policy".to_owned(),
        ));
    }
    let issuer = issuer.trim();
    let client_id = client_id.trim();
    if !issuer.starts_with("https://") || client_id.is_empty() {
        return Err(OriginStreamError::Encode(
            "OIDC issuer must be https and client_id must be non-empty".to_owned(),
        ));
    }
    match classify_oidc_provider(discovery) {
        IdpProviderCapability::SecretlessPublic => {}
        other => {
            return Err(OriginStreamError::Encode(format!(
                "OIDC provider is unsupported ({})",
                other.as_str()
            )));
        }
    }
    let team = load_local_teams(connection)?
        .into_iter()
        .next()
        .ok_or_else(|| OriginStreamError::Encode("no local team genesis".to_owned()))?;
    let canonical = serde_json::to_vec(discovery)
        .map_err(|error| OriginStreamError::Encode(error.to_string()))?;
    let discovery_hash = format!("blake3:{}", blake3::hash(&canonical).to_hex());
    connection
        .upsert_team_idp_oidc(
            &team.team_id,
            issuer,
            client_id,
            IdpProviderCapability::SecretlessPublic.as_str(),
            &discovery_hash,
            set_at,
        )
        .map_err(|error| OriginStreamError::Db(error.to_string()))?;
    Ok(TeamIdpSetReport {
        schema: TEAM_IDP_SET_SCHEMA_V1,
        command: "team idp set",
        team_id: team.team_id,
        issuer: issuer.to_owned(),
        client_id: client_id.to_owned(),
        capability: IdpProviderCapability::SecretlessPublic.as_str().to_owned(),
        discovery_hash,
        mesh_primitives: vec!["team_idp_oidc"],
    })
}

/// Read the recorded IdP policy, or a none-policy when unset.
pub fn team_idp_status(
    connection: &DbConnection,
) -> Result<TeamIdpPolicyReport, OriginStreamError> {
    let team = load_local_teams(connection)?
        .into_iter()
        .next()
        .ok_or_else(|| OriginStreamError::Encode("no local team genesis".to_owned()))?;
    let policy = connection
        .get_team_idp_policy(&team.team_id)
        .map_err(|error| OriginStreamError::Db(error.to_string()))?;
    Ok(TeamIdpPolicyReport {
        schema: TEAM_IDP_SCHEMA_V1,
        command: "team idp status",
        team_id: team.team_id,
        kind: policy
            .as_ref()
            .map(|row| row.kind.clone())
            .unwrap_or_else(|| "none".to_owned()),
        allowed_domain: policy.as_ref().and_then(|row| row.allowed_domain.clone()),
        policy_generation: policy
            .as_ref()
            .map(|row| row.policy_generation)
            .unwrap_or(0),
        mesh_primitives: vec!["team_idp_policy"],
    })
}

/// Record the first or refreshed tailnet login for one member.
pub fn record_member_tailnet_identity(
    connection: &DbConnection,
    member_id: &str,
    login: &str,
    user_id: Option<&str>,
    checked_at: &str,
) -> Result<StoredTeamMemberIdentity, OriginStreamError> {
    let member = connection
        .get_team_member(member_id)
        .map_err(|error| OriginStreamError::Db(error.to_string()))?
        .ok_or_else(|| OriginStreamError::Encode("member is not recorded locally".to_owned()))?;
    let login = login.trim();
    if login.is_empty() {
        return Err(OriginStreamError::Encode(
            "tailnet login must not be empty".to_owned(),
        ));
    }
    connection
        .upsert_team_member_identity(
            member_id,
            &member.team_id,
            login,
            user_id.map(str::trim).filter(|value| !value.is_empty()),
            "attested",
            checked_at,
        )
        .map_err(|error| OriginStreamError::Db(error.to_string()))?;
    connection
        .get_team_member_identity(member_id)
        .map_err(|error| OriginStreamError::Db(error.to_string()))?
        .ok_or_else(|| OriginStreamError::Encode("identity row missing after upsert".to_owned()))
}

/// Revalidate every local member against a tailscale status report.
pub fn revalidate_team_identities(
    connection: &DbConnection,
    report: &TailscaleLocalReport,
    checked_at: &str,
) -> Result<TeamIdpRevalidateReport, OriginStreamError> {
    let team = load_local_teams(connection)?
        .into_iter()
        .next()
        .ok_or_else(|| OriginStreamError::Encode("no local team genesis".to_owned()))?;
    let policy = connection
        .get_team_idp_policy(&team.team_id)
        .map_err(|error| OriginStreamError::Db(error.to_string()))?;
    let kind = policy
        .as_ref()
        .map(|row| row.kind.as_str())
        .unwrap_or("none");
    let allowed_domain = policy
        .as_ref()
        .and_then(|row| row.allowed_domain.as_deref());
    let members = connection
        .list_all_team_members()
        .map_err(|error| OriginStreamError::Db(error.to_string()))?
        .into_iter()
        .filter(|member| member.team_id == team.team_id && member.state == "active")
        .collect::<Vec<_>>();
    if kind != "tailnet_attested" {
        return Ok(TeamIdpRevalidateReport {
            schema: TEAM_IDP_REVALIDATE_SCHEMA_V1,
            command: "team members revalidate",
            team_id: team.team_id,
            kind: kind.to_owned(),
            allowed_domain: allowed_domain.map(str::to_owned),
            checked: 0,
            attested: 0,
            suspended: 0,
            missing: 0,
            members: Vec::new(),
            mesh_primitives: vec!["team_idp_policy"],
        });
    }
    let mut checks = Vec::new();
    let mut attested = 0;
    let mut suspended = 0;
    let mut missing = 0;
    for member in members {
        let recorded = connection
            .get_team_member_identity(&member.member_id)
            .map_err(|error| OriginStreamError::Db(error.to_string()))?;
        let observed = observed_owner_for_member(&member, report, recorded.as_ref());
        let disposition = evaluate_tailnet_owner(
            observed.as_ref(),
            recorded.as_ref().map(|row| row.login.as_str()),
            allowed_domain,
        );
        let (state, login, user_id) = match (disposition, observed) {
            (TailnetOwnerDisposition::Attested, Some(owner)) => {
                attested += 1;
                ("attested", owner.login_name, owner.user_id)
            }
            (TailnetOwnerDisposition::Attested, None) => {
                missing += 1;
                let login = recorded
                    .as_ref()
                    .map(|row| row.login.clone())
                    .unwrap_or_else(|| "unknown".to_owned());
                let user_id = recorded.as_ref().and_then(|row| row.user_id.clone());
                ("missing", login, user_id.unwrap_or_default())
            }
            (TailnetOwnerDisposition::Missing, owner) => {
                missing += 1;
                let login = owner
                    .as_ref()
                    .map(|row| row.login_name.clone())
                    .or_else(|| recorded.as_ref().map(|row| row.login.clone()))
                    .unwrap_or_else(|| "unknown".to_owned());
                let user_id = owner
                    .as_ref()
                    .map(|row| row.user_id.clone())
                    .or_else(|| recorded.and_then(|row| row.user_id));
                ("missing", login, user_id.unwrap_or_default())
            }
            (
                TailnetOwnerDisposition::DomainMismatch | TailnetOwnerDisposition::Reassigned,
                owner,
            ) => {
                suspended += 1;
                let login = owner
                    .as_ref()
                    .map(|row| row.login_name.clone())
                    .or_else(|| recorded.as_ref().map(|row| row.login.clone()))
                    .unwrap_or_else(|| "unknown".to_owned());
                let user_id = owner
                    .as_ref()
                    .map(|row| row.user_id.clone())
                    .or_else(|| recorded.and_then(|row| row.user_id));
                ("suspended", login, user_id.unwrap_or_default())
            }
        };
        connection
            .upsert_team_member_identity(
                &member.member_id,
                &team.team_id,
                &login,
                (!user_id.is_empty()).then_some(user_id.as_str()),
                state,
                checked_at,
            )
            .map_err(|error| OriginStreamError::Db(error.to_string()))?;
        checks.push(TeamIdpMemberCheck {
            member_id: member.member_id,
            origin_node_id: member.origin_node_id,
            is_self: member.is_self,
            login: Some(login),
            disposition: disposition.as_str().to_owned(),
            state: state.to_owned(),
        });
    }
    Ok(TeamIdpRevalidateReport {
        schema: TEAM_IDP_REVALIDATE_SCHEMA_V1,
        command: "team members revalidate",
        team_id: team.team_id,
        kind: kind.to_owned(),
        allowed_domain: allowed_domain.map(str::to_owned),
        checked: checks.len(),
        attested,
        suspended,
        missing,
        members: checks,
        mesh_primitives: vec!["team_idp_policy", "team_member_identity"],
    })
}

fn observed_owner_for_member(
    member: &StoredTeamMember,
    report: &TailscaleLocalReport,
    recorded: Option<&StoredTeamMemberIdentity>,
) -> Option<TailscaleUserProfile> {
    if member.is_self {
        return report.self_owner.clone();
    }
    if let Some(recorded) = recorded {
        if let Some(peer) = report.peers.iter().find(|peer| {
            peer.owner
                .as_ref()
                .is_some_and(|owner| owner.login_name.eq_ignore_ascii_case(&recorded.login))
        }) {
            return peer.owner.clone();
        }
        if let Some(user_id) = recorded.user_id.as_deref()
            && let Some(peer) = report.peers.iter().find(|peer| {
                peer.owner
                    .as_ref()
                    .is_some_and(|owner| owner.user_id == user_id)
            })
        {
            return peer.owner.clone();
        }
    }
    None
}

/// Whether any local team is paused.
pub fn any_local_team_paused(connection: &DbConnection) -> Result<bool, OriginStreamError> {
    for team in load_local_teams(connection)? {
        if team_is_paused(connection, &team.team_id)? {
            return Ok(true);
        }
    }
    Ok(false)
}

fn team_is_paused(connection: &DbConnection, team_id: &str) -> Result<bool, OriginStreamError> {
    connection
        .team_is_paused(team_id)
        .map_err(|error| OriginStreamError::Db(error.to_string()))
}

/// Pause or resume the local team. Every change advances pause_generation.
pub fn set_local_team_paused(
    connection: &DbConnection,
    paused: bool,
    updated_at: &str,
) -> Result<TeamPostureReport, OriginStreamError> {
    let team = load_local_teams(connection)?
        .into_iter()
        .next()
        .ok_or_else(|| OriginStreamError::Encode("no local team genesis".to_owned()))?;
    let generation = connection
        .upsert_team_posture(&team.team_id, paused, updated_at)
        .map_err(|error| OriginStreamError::Db(error.to_string()))?;
    Ok(TeamPostureReport {
        schema: TEAM_POSTURE_SCHEMA_V1,
        command: if paused { "team pause" } else { "team resume" },
        team_id: team.team_id,
        paused,
        pause_generation: generation,
        mesh_primitives: vec!["team_posture"],
    })
}

/// Remove a non-self member and revoke their signing bindings.
pub fn remove_team_member(
    connection: &DbConnection,
    workspace_id: &str,
    member_id: &str,
    produced_at: &str,
    workspace_path: Option<&std::path::Path>,
) -> Result<TeamMemberMutationReport, OriginStreamError> {
    let member = connection
        .get_team_member(member_id)
        .map_err(|error| OriginStreamError::Db(error.to_string()))?
        .ok_or_else(|| OriginStreamError::Encode("unknown member".to_owned()))?;
    if member.is_self {
        return leave_local_team(connection, workspace_id, produced_at, workspace_path);
    }
    mutate_member_state(
        connection,
        workspace_id,
        &member,
        "removed",
        TEAM_MEMBER_REMOVED_OPERATION,
        TEAM_MEMBER_REMOVE_SCHEMA_V1,
        "team members remove",
        produced_at,
        workspace_path,
    )
}

/// Mark the local self member removed.
pub fn leave_local_team(
    connection: &DbConnection,
    workspace_id: &str,
    produced_at: &str,
    workspace_path: Option<&std::path::Path>,
) -> Result<TeamMemberMutationReport, OriginStreamError> {
    let member = connection
        .list_all_team_members()
        .map_err(|error| OriginStreamError::Db(error.to_string()))?
        .into_iter()
        .find(|row| row.is_self)
        .ok_or_else(|| OriginStreamError::Encode("no local self member".to_owned()))?;
    mutate_member_state(
        connection,
        workspace_id,
        &member,
        "removed",
        TEAM_LEFT_OPERATION,
        TEAM_LEAVE_SCHEMA_V1,
        "team leave",
        produced_at,
        workspace_path,
    )
}

fn mutate_member_state(
    connection: &DbConnection,
    workspace_id: &str,
    member: &crate::db::StoredTeamMember,
    state: &str,
    operation: &str,
    schema: &'static str,
    command: &'static str,
    produced_at: &str,
    workspace_path: Option<&std::path::Path>,
) -> Result<TeamMemberMutationReport, OriginStreamError> {
    if member.state == state {
        return Ok(TeamMemberMutationReport {
            schema,
            command,
            team_id: member.team_id.clone(),
            member_id: member.member_id.clone(),
            origin_node_id: member.origin_node_id.clone(),
            state: member.state.clone(),
            mesh_primitives: vec!["team_members"],
        });
    }
    if team_is_paused(connection, &member.team_id)? {
        return Err(OriginStreamError::Encode(
            "team is paused; resume before changing membership".to_owned(),
        ));
    }
    let local_origin = connection
        .list_all_team_members()
        .map_err(|error| OriginStreamError::Db(error.to_string()))?
        .into_iter()
        .find(|row| row.is_self)
        .map(|row| row.origin_node_id)
        .unwrap_or_else(|| member.origin_node_id.clone());
    let hex = blake3::hash(format!("{}:{operation}:{}", member.member_id, produced_at).as_bytes())
        .to_hex();
    let payload = OriginEventPayload::Manifest(ManifestEventPayload {
        operation: operation.to_owned(),
        document_id: format!("tdoc_{}", &hex.as_str()[..24]),
        predecessor_revision_id: None,
        document_payload: serde_json::json!({
            "memberId": member.member_id,
            "originNodeId": member.origin_node_id,
            "displayName": member.display_name,
        }),
    });
    let ed25519 = workspace_path
        .map(|path| Ed25519OriginSigner::load_or_create(path, &local_origin, produced_at))
        .transpose()?;
    let mac = LocalOriginSigner::for_workspace(workspace_id);
    let signer: &dyn OriginSigner = ed25519
        .as_ref()
        .map(|signer| signer as &dyn OriginSigner)
        .unwrap_or(&mac);
    append_origin_event(
        connection,
        signer,
        &OriginAppendRequest {
            team_id: &member.team_id,
            origin_node_id: &local_origin,
            payload,
            required_features: Vec::new(),
            produced_at,
            body_nonce: None,
        },
    )?;
    connection
        .set_team_member_state(&member.member_id, state)
        .map_err(|error| OriginStreamError::Db(error.to_string()))?;
    connection
        .revoke_team_member_nodes(&member.member_id)
        .map_err(|error| OriginStreamError::Db(error.to_string()))?;
    connection
        .raise_team_invite_auth_floor(&member.team_id, produced_at, produced_at)
        .map_err(|error| OriginStreamError::Db(error.to_string()))?;
    Ok(TeamMemberMutationReport {
        schema,
        command,
        team_id: member.team_id.clone(),
        member_id: member.member_id.clone(),
        origin_node_id: member.origin_node_id.clone(),
        state: state.to_owned(),
        mesh_primitives: vec![
            "mesh_origin_events.append",
            "team_members",
            "team_member_nodes",
            "team_invite_auth_floor",
        ],
    })
}

/// Project an applied inbound memory event into the local memory table.
///
/// Body bytes stay off the wire: the local row is a metadata stub whose
/// `trust_subclass` names the producing member so `--memory-scope team` hits.
pub fn project_inbound_team_memory(
    connection: &DbConnection,
    workspace_id: &str,
    inbound: &InboundOriginEvent,
) -> Result<Option<String>, OriginStreamError> {
    if inbound.payload_schema != crate::mesh::origin_stream::MEMORY_EVENT_PAYLOAD_SCHEMA_V1 {
        return Ok(None);
    }
    let payload = serde_json::from_value::<MemoryEventPayload>(inbound.payload.clone())
        .map_err(|error| OriginStreamError::PayloadInvalid(error.to_string()))?;
    if payload.operation != MemoryEventOperation::Create {
        return Ok(None);
    }
    let hex = inbound
        .event_hash
        .trim_start_matches("blake3:")
        .chars()
        .take(26)
        .collect::<String>();
    if hex.len() != 26 {
        return Err(OriginStreamError::Encode(
            "inbound event hash is too short for a memory id".to_owned(),
        ));
    }
    let memory_id = format!("mem_{hex}");
    if connection
        .get_memory(&memory_id)
        .map_err(|error| OriginStreamError::Db(error.to_string()))?
        .is_some()
    {
        return Ok(Some(memory_id));
    }
    let producer = connection
        .list_all_team_members()
        .map_err(|error| OriginStreamError::Db(error.to_string()))?
        .into_iter()
        .find(|member| member.origin_node_id == inbound.origin_node_id && member.state == "active")
        .map(|member| member.display_name)
        .unwrap_or_else(|| inbound.origin_node_id.clone());
    let level = payload
        .level
        .as_deref()
        .filter(|level| matches!(*level, "working" | "episodic" | "semantic" | "procedural"))
        .unwrap_or("semantic");
    let kind = payload
        .memory_kind
        .as_deref()
        .filter(|kind| !kind.trim().is_empty())
        .unwrap_or("note");
    connection
        .insert_memory(
            &memory_id,
            &CreateMemoryInput {
                workspace_id: workspace_id.to_owned(),
                level: level.to_owned(),
                kind: kind.to_owned(),
                content: format!("[ee.team.history] {} {}", kind, payload.body_commitment),
                workflow_id: None,
                confidence: 0.5,
                utility: 0.5,
                importance: 0.5,
                provenance_uri: Some(inbound.event_id.clone()),
                trust_class: "peer_human_attested".to_owned(),
                trust_subclass: Some(format!("agent:{producer}")),
                tags: Vec::new(),
                valid_from: payload.valid_from.clone(),
                valid_to: payload.valid_until.clone(),
            },
        )
        .map_err(|error| OriginStreamError::Db(error.to_string()))?;
    Ok(Some(memory_id))
}

/// Bind a new local node to the self member.
pub fn add_local_team_node(
    connection: &DbConnection,
    workspace_id: &str,
    produced_at: &str,
    workspace_path: Option<&std::path::Path>,
) -> Result<TeamMemberMutationReport, OriginStreamError> {
    let self_member = connection
        .list_all_team_members()
        .map_err(|error| OriginStreamError::Db(error.to_string()))?
        .into_iter()
        .find(|row| row.is_self && row.state == "active")
        .ok_or_else(|| OriginStreamError::Encode("no active self member".to_owned()))?;
    if team_is_paused(connection, &self_member.team_id)? {
        return Err(OriginStreamError::Encode(
            "team is paused; resume before adding a node".to_owned(),
        ));
    }
    let node_id = format!("node_{}", random_hex_32()?);
    let verifying_key = match workspace_path {
        Some(path) => {
            let signer = Ed25519OriginSigner::load_or_create(path, &node_id, produced_at)?;
            Some(hex_encode(&signer.verifying_key_bytes()))
        }
        None => None,
    };
    persist_team_member(
        connection,
        workspace_id,
        &self_member.team_id,
        &node_id,
        &self_member.display_name,
        true,
        "member_added_node",
        produced_at,
        verifying_key.as_deref(),
    )?;
    let hex = blake3::hash(
        format!(
            "{}:{TEAM_ADD_NODE_OPERATION}:{node_id}",
            self_member.member_id
        )
        .as_bytes(),
    )
    .to_hex();
    let payload = OriginEventPayload::Manifest(ManifestEventPayload {
        operation: TEAM_ADD_NODE_OPERATION.to_owned(),
        document_id: format!("tdoc_{}", &hex.as_str()[..24]),
        predecessor_revision_id: None,
        document_payload: serde_json::json!({
            "memberId": self_member.member_id,
            "originNodeId": node_id,
            "displayName": self_member.display_name,
        }),
    });
    let ed25519 = workspace_path
        .map(|path| {
            Ed25519OriginSigner::load_or_create(path, &self_member.origin_node_id, produced_at)
        })
        .transpose()?;
    let mac = LocalOriginSigner::for_workspace(workspace_id);
    let signer: &dyn OriginSigner = ed25519
        .as_ref()
        .map(|signer| signer as &dyn OriginSigner)
        .unwrap_or(&mac);
    append_origin_event(
        connection,
        signer,
        &OriginAppendRequest {
            team_id: &self_member.team_id,
            origin_node_id: &self_member.origin_node_id,
            payload,
            required_features: Vec::new(),
            produced_at,
            body_nonce: None,
        },
    )?;
    Ok(TeamMemberMutationReport {
        schema: TEAM_ADD_NODE_SCHEMA_V1,
        command: "team members add-node",
        team_id: self_member.team_id,
        member_id: self_member.member_id,
        origin_node_id: node_id,
        state: "active".to_owned(),
        mesh_primitives: vec![
            "team_members",
            "team_member_nodes",
            "mesh_origin_events.append",
        ],
    })
}

/// Rotate the local self signing key. Previous generations stay verifiable.
pub fn rotate_local_signing_key(
    connection: &DbConnection,
    _workspace_id: &str,
    produced_at: &str,
    workspace_path: &std::path::Path,
) -> Result<TeamMemberMutationReport, OriginStreamError> {
    let self_member = connection
        .list_all_team_members()
        .map_err(|error| OriginStreamError::Db(error.to_string()))?
        .into_iter()
        .find(|row| row.is_self && row.state == "active")
        .ok_or_else(|| OriginStreamError::Encode("no active self member".to_owned()))?;
    if team_is_paused(connection, &self_member.team_id)? {
        return Err(OriginStreamError::Encode(
            "team is paused; resume before rotating a key".to_owned(),
        ));
    }
    let current = Ed25519OriginSigner::load_or_create(
        workspace_path,
        &self_member.origin_node_id,
        produced_at,
    )?;
    let next_generation = current.signing_key_generation().saturating_add(1);
    let mut seed = zeroize::Zeroizing::new([0_u8; 32]);
    getrandom::fill(seed.as_mut())
        .map_err(|error| OriginStreamError::Encode(format!("csprng unavailable: {error}")))?;
    let store = crate::mesh::key_store::MeshKeyStore::open_or_create(workspace_path)
        .map_err(|error| OriginStreamError::Encode(error.to_string()))?;
    let secret = crate::mesh::key_store::SecretBytes::new(*seed);
    let generation = std::num::NonZeroU64::new(next_generation)
        .ok_or_else(|| OriginStreamError::Encode("signing key generation overflow".to_owned()))?;
    store
        .store_signing_key(
            &self_member.origin_node_id,
            crate::mesh::key_store::SigningKeyClass::Next,
            generation,
            &secret,
            produced_at,
            true,
        )
        .map_err(|error| OriginStreamError::Encode(error.to_string()))?;
    store
        .store_signing_key(
            &self_member.origin_node_id,
            crate::mesh::key_store::SigningKeyClass::Current,
            generation,
            &secret,
            produced_at,
            true,
        )
        .map_err(|error| OriginStreamError::Encode(error.to_string()))?;
    let rotated = Ed25519OriginSigner::from_seed(next_generation, &*seed);
    connection
        .insert_team_member_signing_key(
            &self_member.origin_node_id,
            next_generation,
            &hex_encode(&rotated.verifying_key_bytes()),
            produced_at,
        )
        .map_err(|error| OriginStreamError::Db(error.to_string()))?;
    Ok(TeamMemberMutationReport {
        schema: TEAM_ADD_NODE_SCHEMA_V1,
        command: "team members rotate-key",
        team_id: self_member.team_id,
        member_id: self_member.member_id,
        origin_node_id: self_member.origin_node_id,
        state: format!("generation:{next_generation}"),
        mesh_primitives: vec!["team_member_signing_keys", "mesh_key_store"],
    })
}

/// Replay local origin membership events onto `team_members` in seq order.
pub fn reconcile_local_team_membership(
    connection: &DbConnection,
    workspace_id: &str,
) -> Result<TeamReconcileReport, OriginStreamError> {
    let team = load_local_teams(connection)?
        .into_iter()
        .next()
        .ok_or_else(|| OriginStreamError::Encode("no local team genesis".to_owned()))?;
    let rows = connection
        .list_mesh_manifest_origin_events(256)
        .map_err(|error| OriginStreamError::Db(error.to_string()))?;
    let mut desired: std::collections::BTreeMap<String, (String, String, String)> =
        std::collections::BTreeMap::new();
    let mut inspected = 0_usize;
    for row in rows {
        let Ok(OriginEventPayload::Manifest(payload)) = parse_stored_payload(&row) else {
            continue;
        };
        inspected = inspected.saturating_add(1);
        match payload.operation.as_str() {
            TEAM_CREATED_OPERATION | TEAM_JOINED_OPERATION | TEAM_ADD_NODE_OPERATION => {
                let origin_node_id = payload
                    .document_payload
                    .get("originNodeId")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or(row.origin_node_id.as_str())
                    .to_owned();
                let display_name = payload
                    .document_payload
                    .get("displayName")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or(team.display_name.as_str())
                    .to_owned();
                desired.insert(
                    origin_node_id,
                    ("active".to_owned(), display_name, row.produced_at.clone()),
                );
            }
            TEAM_MEMBER_REMOVED_OPERATION | TEAM_LEFT_OPERATION => {
                let origin_node_id = payload
                    .document_payload
                    .get("originNodeId")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_owned)
                    .or_else(|| {
                        payload
                            .document_payload
                            .get("memberId")
                            .and_then(serde_json::Value::as_str)
                            .and_then(|member_id| {
                                connection
                                    .get_team_member(member_id)
                                    .ok()
                                    .flatten()
                                    .map(|member| member.origin_node_id)
                            })
                    });
                let Some(origin_node_id) = origin_node_id else {
                    continue;
                };
                let display_name = payload
                    .document_payload
                    .get("displayName")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or(team.display_name.as_str())
                    .to_owned();
                desired.insert(
                    origin_node_id,
                    ("removed".to_owned(), display_name, row.produced_at.clone()),
                );
            }
            _ => {}
        }
    }
    let mut applied_additions = 0_usize;
    let mut applied_removals = 0_usize;
    for (origin_node_id, (state, display_name, produced_at)) in desired {
        if let Some(member) = find_member_by_origin_node(connection, &origin_node_id)? {
            if member.state == state {
                continue;
            }
            connection
                .set_team_member_state(&member.member_id, &state)
                .map_err(|error| OriginStreamError::Db(error.to_string()))?;
            if state == "removed" {
                connection
                    .revoke_team_member_nodes(&member.member_id)
                    .map_err(|error| OriginStreamError::Db(error.to_string()))?;
                applied_removals = applied_removals.saturating_add(1);
            } else {
                applied_additions = applied_additions.saturating_add(1);
            }
        } else if state == "active" {
            persist_team_member(
                connection,
                workspace_id,
                &team.team_id,
                &origin_node_id,
                &display_name,
                origin_node_id == team.origin_node_id,
                "reconcile",
                &produced_at,
                None,
            )?;
            applied_additions = applied_additions.saturating_add(1);
        }
    }
    Ok(TeamReconcileReport {
        schema: TEAM_RECONCILE_SCHEMA_V1,
        command: "team members reconcile",
        team_id: team.team_id,
        applied_additions,
        applied_removals,
        inspected_events: inspected,
        mesh_primitives: vec!["mesh_origin_events", "team_members", "team_member_nodes"],
    })
}

/// Closed-metadata team activity over origin events and inbound stubs.
pub fn list_team_activity(
    connection: &DbConnection,
    workspace_id: &str,
    as_of: &str,
    limit: usize,
) -> Result<TeamActivityReport, OriginStreamError> {
    let team = load_local_teams(connection)?
        .into_iter()
        .next()
        .ok_or_else(|| OriginStreamError::Encode("no local team genesis".to_owned()))?;
    let cap = limit.max(1).min(1000);
    let members = connection
        .list_all_team_members()
        .map_err(|error| OriginStreamError::Db(error.to_string()))?;
    let display_for = |origin_node_id: &str| -> String {
        members
            .iter()
            .find(|member| member.origin_node_id == origin_node_id)
            .map(|member| member.display_name.clone())
            .unwrap_or_else(|| origin_node_id.to_owned())
    };
    let as_of_cutoff = chrono::DateTime::parse_from_rfc3339(as_of)
        .map(|stamp| stamp.with_timezone(&chrono::Utc))
        .ok();
    let anomaly_cutoff =
        as_of_cutoff.map(|stamp| stamp + chrono::Duration::seconds(TEAM_ACTIVITY_CLOCK_SKEW_SECS));
    let mut events = Vec::new();
    let mut clock_anomalies = Vec::new();
    let origin_rows = connection
        .list_all_mesh_origin_events(&team.team_id, 1000)
        .map_err(|error| OriginStreamError::Db(error.to_string()))?;
    let mut seen_event_ids = std::collections::BTreeSet::new();
    for row in origin_rows {
        seen_event_ids.insert(row.event_id.clone());
        let produced = chrono::DateTime::parse_from_rfc3339(&row.produced_at)
            .ok()
            .map(|stamp| stamp.with_timezone(&chrono::Utc));
        if let (Some(produced), Some(as_of_cutoff)) = (produced, as_of_cutoff)
            && produced > as_of_cutoff
            && anomaly_cutoff.is_none_or(|limit| produced <= limit)
        {
            continue;
        }
        let item = match parse_stored_payload(&row) {
            Ok(OriginEventPayload::Memory(payload)) => TeamActivityItem {
                event_id: row.event_id.clone(),
                origin_node_id: row.origin_node_id.clone(),
                member_display_name: display_for(&row.origin_node_id),
                kind: payload.memory_kind.unwrap_or_else(|| "note".to_owned()),
                level: payload.level.unwrap_or_else(|| "semantic".to_owned()),
                produced_at: row.produced_at.clone(),
                body_available: payload.body_representation.as_deref() == Some("exact")
                    || payload.body_representation.as_deref() == Some("already_redacted"),
                source: "origin_event".to_owned(),
            },
            Ok(OriginEventPayload::Manifest(payload)) => TeamActivityItem {
                event_id: row.event_id.clone(),
                origin_node_id: row.origin_node_id.clone(),
                member_display_name: display_for(&row.origin_node_id),
                kind: payload.operation,
                level: "manifest".to_owned(),
                produced_at: row.produced_at.clone(),
                body_available: false,
                source: "origin_event".to_owned(),
            },
            Err(_) => continue,
        };
        let is_anomaly = produced
            .zip(anomaly_cutoff)
            .is_some_and(|(produced, limit)| produced > limit);
        if is_anomaly {
            clock_anomalies.push(item);
        } else {
            events.push(item);
        }
    }
    let memories = connection
        .list_memories(workspace_id, None, false)
        .map_err(|error| OriginStreamError::Db(error.to_string()))?;
    for memory in memories {
        if memory.trust_class != "peer_human_attested" {
            continue;
        }
        if memory
            .provenance_uri
            .as_deref()
            .is_some_and(|event_id| seen_event_ids.contains(event_id))
        {
            continue;
        }
        let produced_at = memory.created_at.clone();
        let produced = chrono::DateTime::parse_from_rfc3339(&produced_at)
            .ok()
            .map(|stamp| stamp.with_timezone(&chrono::Utc));
        if let (Some(produced), Some(as_of_cutoff)) = (produced, as_of_cutoff)
            && produced > as_of_cutoff
            && anomaly_cutoff.is_none_or(|limit| produced <= limit)
        {
            continue;
        }
        let origin_node_id = members
            .iter()
            .find(|member| {
                memory
                    .trust_subclass
                    .as_deref()
                    .is_some_and(|subclass| subclass == format!("agent:{}", member.display_name))
            })
            .map(|member| member.origin_node_id.clone())
            .unwrap_or_default();
        let item = TeamActivityItem {
            event_id: memory
                .provenance_uri
                .clone()
                .unwrap_or_else(|| memory.id.clone()),
            origin_node_id: origin_node_id.clone(),
            member_display_name: display_for(&origin_node_id),
            kind: memory.kind,
            level: memory.level,
            produced_at,
            body_available: false,
            source: "inbound_projection".to_owned(),
        };
        let is_anomaly = produced
            .zip(anomaly_cutoff)
            .is_some_and(|(produced, limit)| produced > limit);
        if is_anomaly {
            clock_anomalies.push(item);
        } else {
            events.push(item);
        }
    }
    events.sort_by(|left, right| {
        right
            .produced_at
            .cmp(&left.produced_at)
            .then_with(|| left.event_id.cmp(&right.event_id))
    });
    events.truncate(cap);
    Ok(TeamActivityReport {
        schema: TEAM_ACTIVITY_SCHEMA_V1,
        command: "team activity",
        team_id: team.team_id,
        as_of: as_of.to_owned(),
        event_count: events.len(),
        events,
        clock_anomalies,
        mesh_primitives: vec!["mesh_origin_events", "memories", "team_members"],
    })
}

fn find_member_by_origin_node(
    connection: &DbConnection,
    origin_node_id: &str,
) -> Result<Option<StoredTeamMember>, OriginStreamError> {
    Ok(connection
        .list_all_team_members()
        .map_err(|error| OriginStreamError::Db(error.to_string()))?
        .into_iter()
        .find(|member| member.origin_node_id == origin_node_id))
}

fn team_project_record(row: StoredTeamProject) -> TeamProjectRecord {
    TeamProjectRecord {
        project_id: row.project_id,
        team_id: row.team_id,
        display_name: row.display_name,
        local_path: row.local_path,
        source: row.source,
        created_at: row.created_at,
    }
}

/// List minted/adopted team projects.
pub fn list_team_projects(
    connection: &DbConnection,
) -> Result<TeamProjectsReport, OriginStreamError> {
    let team = load_local_teams(connection)?
        .into_iter()
        .next()
        .ok_or_else(|| OriginStreamError::Encode("no local team genesis".to_owned()))?;
    let projects = connection
        .list_team_projects(&team.team_id)
        .map_err(|error| OriginStreamError::Db(error.to_string()))?
        .into_iter()
        .map(team_project_record)
        .collect::<Vec<_>>();
    Ok(TeamProjectsReport {
        schema: TEAM_PROJECTS_SCHEMA_V1,
        command: "team projects list",
        team_id: team.team_id,
        minted: false,
        project_count: projects.len(),
        projects,
        mesh_primitives: vec!["team_projects"],
    })
}

/// Mint a team-scoped project id for a non-git workspace.
pub fn share_team_project(
    connection: &DbConnection,
    workspace_id: &str,
    display_name: &str,
    local_path: &str,
    produced_at: &str,
    workspace_path: Option<&std::path::Path>,
) -> Result<TeamProjectsReport, OriginStreamError> {
    let name = display_name.trim();
    let path = local_path.trim();
    if name.is_empty() || path.is_empty() {
        return Err(OriginStreamError::Encode(
            "project name and path must not be empty".to_owned(),
        ));
    }
    let team = load_local_teams(connection)?
        .into_iter()
        .next()
        .ok_or_else(|| OriginStreamError::Encode("no local team genesis".to_owned()))?;
    if team_is_paused(connection, &team.team_id)? {
        return Err(OriginStreamError::Encode(
            "team is paused; resume before sharing a project".to_owned(),
        ));
    }
    if let Some(existing) = connection
        .get_team_project_by_name(&team.team_id, name)
        .map_err(|error| OriginStreamError::Db(error.to_string()))?
    {
        return Ok(TeamProjectsReport {
            schema: TEAM_PROJECTS_SCHEMA_V1,
            command: "team projects share",
            team_id: team.team_id,
            minted: false,
            project_count: 1,
            projects: vec![team_project_record(existing)],
            mesh_primitives: vec!["team_projects"],
        });
    }
    let project_id = format!("prj_tm_{}", &random_hex_32()?[..26]);
    connection
        .insert_team_project(&InsertTeamProjectInput {
            project_id: project_id.clone(),
            team_id: team.team_id.clone(),
            display_name: name.to_owned(),
            local_path: path.to_owned(),
            source: "minted".to_owned(),
            created_at: produced_at.to_owned(),
        })
        .map_err(|error| OriginStreamError::Db(error.to_string()))?;
    let hex = blake3::hash(format!("{project_id}:{name}").as_bytes()).to_hex();
    let payload = OriginEventPayload::Manifest(ManifestEventPayload {
        operation: TEAM_PROJECT_SHARED_OPERATION.to_owned(),
        document_id: format!("tdoc_{}", &hex.as_str()[..24]),
        predecessor_revision_id: None,
        document_payload: serde_json::json!({
            "projectId": project_id,
            "displayName": name,
        }),
    });
    let ed25519 = workspace_path
        .map(|store_path| {
            Ed25519OriginSigner::load_or_create(store_path, &team.origin_node_id, produced_at)
        })
        .transpose()?;
    let mac = LocalOriginSigner::for_workspace(workspace_id);
    let signer: &dyn OriginSigner = ed25519
        .as_ref()
        .map(|signer| signer as &dyn OriginSigner)
        .unwrap_or(&mac);
    append_origin_event(
        connection,
        signer,
        &OriginAppendRequest {
            team_id: &team.team_id,
            origin_node_id: &team.origin_node_id,
            payload,
            required_features: Vec::new(),
            produced_at,
            body_nonce: None,
        },
    )?;
    let stored = connection
        .get_team_project(&project_id)
        .map_err(|error| OriginStreamError::Db(error.to_string()))?
        .ok_or_else(|| OriginStreamError::Encode("project row missing after mint".to_owned()))?;
    Ok(TeamProjectsReport {
        schema: TEAM_PROJECTS_SCHEMA_V1,
        command: "team projects share",
        team_id: team.team_id,
        minted: true,
        project_count: 1,
        projects: vec![team_project_record(stored)],
        mesh_primitives: vec!["team_projects", "mesh_origin_events.append"],
    })
}

/// Map an existing team project onto a local path.
pub fn adopt_team_project(
    connection: &DbConnection,
    project_id: &str,
    display_name: &str,
    local_path: &str,
    produced_at: &str,
) -> Result<TeamProjectsReport, OriginStreamError> {
    let project_id = project_id.trim();
    let name = display_name.trim();
    let path = local_path.trim();
    if !project_id.starts_with("prj_tm_") || project_id.len() != 33 {
        return Err(OriginStreamError::Encode(
            "project id must be prj_tm_ plus 26 chars".to_owned(),
        ));
    }
    if name.is_empty() || path.is_empty() {
        return Err(OriginStreamError::Encode(
            "project name and path must not be empty".to_owned(),
        ));
    }
    let team = load_local_teams(connection)?
        .into_iter()
        .next()
        .ok_or_else(|| OriginStreamError::Encode("no local team genesis".to_owned()))?;
    if team_is_paused(connection, &team.team_id)? {
        return Err(OriginStreamError::Encode(
            "team is paused; resume before adopting a project".to_owned(),
        ));
    }
    connection
        .upsert_team_project_path(project_id, &team.team_id, name, path, produced_at)
        .map_err(|error| OriginStreamError::Db(error.to_string()))?;
    let stored = connection
        .get_team_project(project_id)
        .map_err(|error| OriginStreamError::Db(error.to_string()))?
        .ok_or_else(|| OriginStreamError::Encode("project row missing after adopt".to_owned()))?;
    Ok(TeamProjectsReport {
        schema: TEAM_PROJECTS_SCHEMA_V1,
        command: "team projects adopt",
        team_id: team.team_id,
        minted: false,
        project_count: 1,
        projects: vec![team_project_record(stored)],
        mesh_primitives: vec!["team_projects"],
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
    joiner_verifying_key: Option<&str>,
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
        joiner_verifying_key,
    )?;
    Ok(())
}

fn invite_auth_floor(
    connection: &DbConnection,
    team_id: &str,
) -> Result<String, OriginStreamError> {
    Ok(connection
        .team_invite_auth_floor(team_id)
        .map_err(|error| OriginStreamError::Db(error.to_string()))?
        .unwrap_or_default())
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
    join_team_with_code_on_store(
        connection,
        workspace_id,
        invite_code,
        display_name,
        produced_at,
        timeout,
        None,
    )
}

/// Parse an invite, prove it over TCP join, and persist using the key store.
pub fn join_team_with_code_on_store(
    connection: &DbConnection,
    workspace_id: &str,
    invite_code: &str,
    display_name: &str,
    produced_at: &str,
    timeout: std::time::Duration,
    workspace_path: Option<&std::path::Path>,
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
    if let Some(attempt) = connection
        .get_team_join_attempt(&parsed.invite_id)
        .map_err(|error| OriginStreamError::Db(error.to_string()))?
        && attempt.phase == "granted"
        && let Some(granted_json) = attempt.granted_json.as_deref()
        && let Ok(granted) = serde_json::from_str::<TeamJoinGrantedV1>(granted_json)
    {
        return persist_granted_join_with_store(
            connection,
            workspace_id,
            &granted,
            &attempt.joiner_node_id,
            name,
            produced_at,
            workspace_path,
            Some(parsed.inviter_verifying_key.as_str()).filter(|key| key.len() == 64),
        );
    }
    if parsed.inviter_verifying_key.len() != 64 {
        return Err(OriginStreamError::Encode(
            "invite is missing the inviter verifying key; remint with a key store".to_owned(),
        ));
    }
    let existing = connection
        .get_team_join_attempt(&parsed.invite_id)
        .map_err(|error| OriginStreamError::Db(error.to_string()))?;
    let joiner_node_id = match existing.as_ref() {
        Some(attempt) => attempt.joiner_node_id.clone(),
        None => format!("node_{}", random_hex_32()?),
    };
    let joiner_verifying_key = match workspace_path {
        Some(path) => {
            let signer = Ed25519OriginSigner::load_or_create(path, &joiner_node_id, produced_at)?;
            hex_encode(&signer.verifying_key_bytes())
        }
        None => String::new(),
    };
    let joiner_nonce = existing
        .as_ref()
        .map(|attempt| attempt.joiner_nonce.clone())
        .unwrap_or(random_hex_32()?);
    let hello = TeamJoinHelloV1 {
        schema: TEAM_JOIN_HELLO_SCHEMA_V1.to_owned(),
        invite_id: parsed.invite_id.clone(),
        joiner_node_id: joiner_node_id.clone(),
        joiner_display_name: name.to_owned(),
        joiner_nonce,
        joiner_verifying_key,
    };
    if existing.is_none() {
        connection
            .upsert_team_join_attempt(&UpsertTeamJoinAttemptInput {
                invite_id: parsed.invite_id.clone(),
                team_id: parsed.team_id.clone(),
                joiner_node_id: joiner_node_id.clone(),
                joiner_nonce: hello.joiner_nonce.clone(),
                inviter_nonce: None,
                phase: "hello".to_owned(),
                granted_json: None,
                updated_at: produced_at.to_owned(),
            })
            .map_err(|error| OriginStreamError::Db(error.to_string()))?;
    }
    let mut stream = std::net::TcpStream::connect_timeout(&address, timeout)
        .map_err(|error| OriginStreamError::Encode(format!("bootstrap join connect: {error}")))?;
    stream
        .set_read_timeout(Some(timeout))
        .and_then(|()| stream.set_write_timeout(Some(timeout)))
        .map_err(|error| OriginStreamError::Encode(format!("join timeout: {error}")))?;
    write_join_payload(
        &mut stream,
        serde_json::to_value(&hello)
            .map_err(|error| OriginStreamError::Encode(error.to_string()))?,
    )?;
    let challenge = serde_json::from_value::<TeamJoinChallengeV1>(read_join_payload(&mut stream)?)
        .map_err(|error| OriginStreamError::Encode(format!("join challenge decode: {error}")))?;
    verify_join_challenge(&parsed.inviter_verifying_key, &parsed, &hello, &challenge)?;
    connection
        .upsert_team_join_attempt(&UpsertTeamJoinAttemptInput {
            invite_id: parsed.invite_id.clone(),
            team_id: parsed.team_id.clone(),
            joiner_node_id: joiner_node_id.clone(),
            joiner_nonce: hello.joiner_nonce.clone(),
            inviter_nonce: Some(challenge.inviter_nonce.clone()),
            phase: "challenged".to_owned(),
            granted_json: None,
            updated_at: produced_at.to_owned(),
        })
        .map_err(|error| OriginStreamError::Db(error.to_string()))?;
    let prove = TeamJoinProveV1 {
        schema: TEAM_JOIN_PROVE_SCHEMA_V1.to_owned(),
        invite_id: parsed.invite_id.clone(),
        secret: parsed.secret.clone(),
        joiner_node_id: joiner_node_id.clone(),
        joiner_display_name: name.to_owned(),
        joiner_nonce: hello.joiner_nonce.clone(),
        inviter_nonce: challenge.inviter_nonce.clone(),
    };
    write_join_payload(
        &mut stream,
        serde_json::to_value(&prove)
            .map_err(|error| OriginStreamError::Encode(error.to_string()))?,
    )?;
    let granted = serde_json::from_value::<TeamJoinGrantedV1>(read_join_payload(&mut stream)?)
        .map_err(|error| OriginStreamError::Encode(format!("join grant decode: {error}")))?;
    let pair = derive_team_pair_key(
        &parsed.secret,
        &granted.team_id,
        &parsed.invite_id,
        &joiner_node_id,
        &granted.origin_node_id,
        &hello.joiner_nonce,
        &challenge.inviter_nonce,
    );
    if granted.pair_confirmation != pair_confirmation(&pair) {
        return Err(OriginStreamError::Encode(
            "pair key confirmation mismatch".to_owned(),
        ));
    }
    if let Some(path) = workspace_path {
        persist_pair_key(
            path,
            &granted.team_id,
            &granted.origin_node_id,
            &pair,
            produced_at,
        )?;
    }
    let granted_json = serde_json::to_string(&granted)
        .map_err(|error| OriginStreamError::Encode(error.to_string()))?;
    connection
        .upsert_team_join_attempt(&UpsertTeamJoinAttemptInput {
            invite_id: parsed.invite_id.clone(),
            team_id: granted.team_id.clone(),
            joiner_node_id: joiner_node_id.clone(),
            joiner_nonce: hello.joiner_nonce.clone(),
            inviter_nonce: Some(challenge.inviter_nonce.clone()),
            phase: "granted".to_owned(),
            granted_json: Some(granted_json),
            updated_at: produced_at.to_owned(),
        })
        .map_err(|error| OriginStreamError::Db(error.to_string()))?;
    persist_granted_join_with_store(
        connection,
        workspace_id,
        &granted,
        &joiner_node_id,
        name,
        produced_at,
        workspace_path,
        Some(parsed.inviter_verifying_key.as_str()).filter(|key| key.len() == 64),
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
    persist_granted_join_with_store(
        connection,
        workspace_id,
        granted,
        joiner_node_id,
        display_name,
        produced_at,
        None,
        None,
    )
}

/// Persist a granted join, signing with the workspace key store when given.
pub fn persist_granted_join_with_store(
    connection: &DbConnection,
    workspace_id: &str,
    granted: &TeamJoinGrantedV1,
    joiner_node_id: &str,
    display_name: &str,
    produced_at: &str,
    workspace_path: Option<&std::path::Path>,
    inviter_verifying_key: Option<&str>,
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
    let ed25519 = workspace_path
        .map(|path| Ed25519OriginSigner::load_or_create(path, joiner_node_id, produced_at))
        .transpose()?;
    let mac = LocalOriginSigner::for_workspace(workspace_id);
    let signer: &dyn OriginSigner = ed25519
        .as_ref()
        .map(|signer| signer as &dyn OriginSigner)
        .unwrap_or(&mac);
    let appended = append_origin_event(
        connection,
        signer,
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
        granted.origin_node_id.as_str(),
        &granted.display_name,
        false,
        "invite_ceremony",
        produced_at,
        inviter_verifying_key.filter(|key| key.len() == 64),
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
        ed25519
            .as_ref()
            .map(|signer| hex_encode(&signer.verifying_key_bytes()))
            .as_deref(),
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
    verifying_key_hex: Option<&str>,
) -> Result<String, OriginStreamError> {
    let name = display_name.trim();
    if name.is_empty() {
        return Err(OriginStreamError::Encode(
            "member display name must not be empty".to_owned(),
        ));
    }
    if !(workspace_id.starts_with("wsp_") && workspace_id.len() == 30) {
        return Err(OriginStreamError::Encode(
            "team member workspace_id must be wsp_ + 26 chars".to_owned(),
        ));
    }
    let member_id = format!("mbr_{}", random_hex_32()?);
    connection
        .insert_team_member(&InsertTeamMemberInput {
            member_id: member_id.clone(),
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
    if let Some(key) = verifying_key_hex.filter(|key| key.len() == 64) {
        connection
            .insert_team_member_node(&InsertTeamMemberNodeInput {
                node_id: origin_node_id.to_owned(),
                member_id: member_id.clone(),
                team_id: team_id.to_owned(),
                verifying_key_hex: key.to_owned(),
                signing_key_generation: 1,
                state: "active".to_owned(),
                bound_at: joined_at.to_owned(),
            })
            .map_err(|error| OriginStreamError::Db(error.to_string()))?;
        connection
            .insert_team_member_signing_key(origin_node_id, 1, key, joined_at)
            .map_err(|error| OriginStreamError::Db(error.to_string()))?;
    }
    Ok(member_id)
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
    fn origin_node_authorizer_requires_an_active_member_when_any_exist() {
        let connection = open_db();
        assert_eq!(
            origin_node_is_active_member(&connection, "node_unknown00000000000000000001")
                .expect("empty"),
            None
        );
        let created = create_local_team(
            &connection,
            "wsp_persistfixture000000000001",
            "Analysts",
            "2026-08-13T00:00:00Z",
        )
        .expect("create");
        assert_eq!(
            origin_node_is_active_member(&connection, &created.team.origin_node_id).expect("self"),
            Some(true)
        );
        assert_eq!(
            origin_node_is_active_member(&connection, "node_unknown00000000000000000001")
                .expect("stranger"),
            Some(false)
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
            "2026-08-13T01:30:00Z",
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
    fn revoke_invite_blocks_later_redeem() {
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
        assert!(
            revoke_team_invite(&connection, &minted.invite_id, "2026-08-13T00:30:00Z")
                .expect("revoke")
        );
        let parsed = parse_team_invite_code(&minted.invite_code).expect("parse");
        assert!(
            redeem_team_invite(
                &connection,
                &parsed.invite_id,
                &parsed.secret,
                "2026-08-13T01:00:00Z",
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
            "wsp_joinworkspace0000000000001",
            &TeamJoinGrantedV1 {
                schema: TEAM_JOIN_GRANTED_SCHEMA_V1.to_owned(),
                team_id: created.team.team_id.clone(),
                origin_node_id: created.team.origin_node_id.clone(),
                display_name: created.team.display_name.clone(),
                hello_port: created.team.hello_port,
                genesis_event_hash: created.team.genesis_event_hash.clone(),
                pair_confirmation: String::new(),
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
        assert_eq!(status.members.len(), 2);
        assert!(status.members.iter().any(|member| member.is_self));
        assert!(
            status
                .members
                .iter()
                .any(|member| member.is_self && member.bound_via == "invite_ceremony")
        );
        assert!(
            status
                .members
                .iter()
                .any(|member| member.is_self && member.display_name == "Priya")
        );
    }

    #[cfg(unix)]
    #[test]
    fn create_local_team_with_store_signs_genesis_with_ed25519() {
        let workspace = tempfile::tempdir().unwrap();
        let connection = open_db();
        let report = create_local_team_with_store(
            &connection,
            "wsp_persistfixture000000000001",
            "Analysts",
            "2026-08-13T00:00:00Z",
            Some(workspace.path()),
        )
        .expect("create");
        let rows = connection
            .list_mesh_manifest_origin_events(8)
            .expect("list");
        assert_eq!(rows.len(), 1);
        assert!(rows[0].signature.starts_with("ed25519:"));
        assert_eq!(rows[0].event_id, report.team.genesis_event_id);
        let signer = Ed25519OriginSigner::load_or_create(
            workspace.path(),
            &report.team.origin_node_id,
            "2026-08-13T00:00:00Z",
        )
        .expect("reload");
        assert_eq!(signer.signing_key_generation(), 1);
    }

    #[test]
    fn serve_one_bootstrap_join_redeems_and_records_the_joiner() {
        let keys = tempfile::tempdir().unwrap();
        let inviter = open_db();
        create_local_team_with_store(
            &inviter,
            "wsp_persistfixture000000000001",
            "Analysts",
            "2026-08-13T00:00:00Z",
            Some(keys.path()),
        )
        .expect("create");
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let address = listener.local_addr().expect("addr");
        let minted = mint_team_invite_with_store(
            &inviter,
            &address.to_string(),
            "2026-08-13T00:00:00Z",
            "2026-08-20T00:00:00Z",
            Some(keys.path()),
        )
        .expect("mint");
        assert_eq!(
            parse_team_invite_code(&minted.invite_code)
                .expect("parse")
                .inviter_verifying_key
                .len(),
            64
        );
        let invite_code = minted.invite_code.clone();
        let joiner_keys = tempfile::tempdir().unwrap();
        let joiner_path = joiner_keys.path().to_path_buf();
        let client = std::thread::spawn(move || {
            let joiner = open_db();
            join_team_with_code_on_store(
                &joiner,
                "wsp_joinworkspace0000000000001",
                &invite_code,
                "Priya",
                "2026-08-13T04:00:00Z",
                std::time::Duration::from_secs(5),
                Some(joiner_path.as_path()),
            )
        });
        let granted = serve_one_bootstrap_join_with_store(
            &inviter,
            "wsp_persistfixture000000000001",
            &listener,
            std::time::Duration::from_secs(5),
            Some(keys.path()),
        )
        .expect("serve");
        let joined = client.join().expect("client thread").expect("join");
        assert!(joined.joined);
        assert_eq!(joined.team.team_id, granted.team_id);
        assert!(!granted.pair_confirmation.is_empty());
        let inviter_status = local_team_status(&inviter).expect("status");
        let members = inviter_status.members;
        assert_eq!(members.len(), 2);
        assert_eq!(inviter_status.nodes.len(), 2);
        assert!(
            members
                .iter()
                .any(|member| !member.is_self && member.display_name == "Priya")
        );
    }

    #[test]
    fn pair_key_requires_the_live_transcript_not_just_the_invite_secret() {
        let from_ceremony = derive_team_pair_key(
            "secret",
            "team_a",
            "invite",
            "node_join",
            "node_origin",
            "nonce_j",
            "nonce_i",
        );
        let copied_invite_only = derive_team_pair_key(
            "secret",
            "team_a",
            "invite",
            "node_join",
            "node_origin",
            "other_j",
            "other_i",
        );
        assert_ne!(from_ceremony, copied_invite_only);
        assert_ne!(
            pair_confirmation(&from_ceremony),
            pair_confirmation(&copied_invite_only)
        );
    }

    #[cfg(unix)]
    #[test]
    fn create_local_team_binds_member_node_and_verifies_genesis() {
        let workspace = tempfile::tempdir().unwrap();
        let connection = open_db();
        let report = create_local_team_with_store(
            &connection,
            "wsp_persistfixture000000000001",
            "Analysts",
            "2026-08-13T00:00:00Z",
            Some(workspace.path()),
        )
        .expect("create");
        let node = connection
            .get_team_member_node(&report.team.origin_node_id, 1)
            .expect("load node")
            .expect("bound");
        assert_eq!(node.verifying_key_hex.len(), 64);
        let status = local_team_status(&connection).expect("status");
        assert_eq!(status.nodes.len(), 1);
        assert_eq!(status.nodes[0].node_id, report.team.origin_node_id);
        let rows = connection
            .list_mesh_manifest_origin_events(8)
            .expect("list");
        let inbound = crate::mesh::origin_stream::inbound_from_stored(&rows[0]).expect("inbound");
        let verifier = TeamMemberKeyVerifier {
            connection: &connection,
        };
        let disposition = crate::mesh::origin_stream::ingest_origin_event(
            &connection,
            &verifier,
            "node_notself00000000000000000001",
            &std::collections::BTreeSet::new(),
            &inbound,
            "2026-08-13T00:00:01Z",
        )
        .expect("ingest");
        assert_eq!(
            disposition,
            crate::mesh::origin_stream::IngestDisposition::Applied
        );
    }

    #[test]
    fn invite_mint_refuses_a_clock_below_the_authorization_floor() {
        let connection = open_db();
        create_local_team(
            &connection,
            "wsp_persistfixture000000000001",
            "Analysts",
            "2026-08-13T00:00:00Z",
        )
        .expect("create");
        mint_team_invite(
            &connection,
            "127.0.0.1",
            "2026-08-13T02:00:00Z",
            "2026-08-20T00:00:00Z",
        )
        .expect("mint");
        let err = mint_team_invite(
            &connection,
            "127.0.0.1",
            "2026-08-13T01:00:00Z",
            "2026-08-20T00:00:00Z",
        )
        .expect_err("rollback");
        assert!(err.to_string().contains("clock floor"));
    }

    #[test]
    fn share_team_history_previews_then_projects_metadata_once() {
        let connection = open_db();
        connection
            .insert_workspace(
                "wsp_persistfixture000000000001",
                &crate::db::CreateWorkspaceInput {
                    path: "/tmp/ee-team-share-history".to_owned(),
                    name: Some("share".to_owned()),
                },
            )
            .expect("workspace");
        create_local_team(
            &connection,
            "wsp_persistfixture000000000001",
            "Analysts",
            "2026-08-13T00:00:00Z",
        )
        .expect("create");
        connection
            .insert_memory(
                "mem_sharehistory00000000000001",
                &crate::db::CreateMemoryInput {
                    workspace_id: "wsp_persistfixture000000000001".to_owned(),
                    level: "procedural".to_owned(),
                    kind: "rule".to_owned(),
                    content: "Do not put body text on the history wire.".to_owned(),
                    workflow_id: None,
                    confidence: 0.9,
                    utility: 0.5,
                    importance: 0.5,
                    provenance_uri: None,
                    trust_class: "human_explicit".to_owned(),
                    trust_subclass: None,
                    tags: Vec::new(),
                    valid_from: None,
                    valid_to: None,
                },
            )
            .expect("remember");
        let preview = share_team_history(
            &connection,
            "wsp_persistfixture000000000001",
            "2026-08-13T05:00:00Z",
            false,
            16,
            None,
        )
        .expect("preview");
        assert!(!preview.confirmed);
        assert_eq!(preview.candidate_count, 1);
        assert_eq!(preview.projected_count, 0);
        assert_eq!(preview.items[0].memory_id, "mem_sharehistory00000000000001");
        let preview_json = serde_json::to_string(&preview).expect("json");
        assert!(!preview_json.contains("Do not put body text"));
        let first = share_team_history(
            &connection,
            "wsp_persistfixture000000000001",
            "2026-08-13T05:01:00Z",
            true,
            16,
            None,
        )
        .expect("confirm");
        assert!(first.confirmed);
        assert_eq!(first.projected_count, 1);
        let second = share_team_history(
            &connection,
            "wsp_persistfixture000000000001",
            "2026-08-13T05:02:00Z",
            true,
            16,
            None,
        )
        .expect("again");
        assert_eq!(second.projected_count, 0);
        assert_eq!(second.skipped_count, 1);
        let rows = connection
            .list_mesh_manifest_origin_events(16)
            .expect("origin");
        assert!(!rows.is_empty());
        let memory_rows = connection
            .list_mesh_origin_events(&first.team_id, rows[0].origin_node_id.as_str(), 0, 16)
            .expect("chain");
        assert!(
            memory_rows
                .iter()
                .any(|row| row.payload_schema == "ee.mesh.memory_event.v1"
                    && !row.payload_json.contains("Do not put body text"))
        );
    }

    #[test]
    fn leave_marks_self_removed_and_authorizer_denies() {
        let connection = open_db();
        let created = create_local_team(
            &connection,
            "wsp_persistfixture000000000001",
            "Analysts",
            "2026-08-13T00:00:00Z",
        )
        .expect("create");
        assert!(
            origin_node_is_active_member(&connection, &created.team.origin_node_id)
                .expect("authz")
                .expect("members exist")
        );
        let left = leave_local_team(
            &connection,
            "wsp_persistfixture000000000001",
            "2026-08-13T06:00:00Z",
            None,
        )
        .expect("leave");
        assert_eq!(left.state, "removed");
        assert!(
            !origin_node_is_active_member(&connection, &created.team.origin_node_id)
                .expect("authz after")
                .expect("members exist")
        );
    }

    #[test]
    fn pause_blocks_history_confirm_until_resume() {
        let connection = open_db();
        create_local_team(
            &connection,
            "wsp_persistfixture000000000001",
            "Analysts",
            "2026-08-13T00:00:00Z",
        )
        .expect("create");
        let paused =
            set_local_team_paused(&connection, true, "2026-08-13T06:00:00Z").expect("pause");
        assert!(paused.paused);
        assert_eq!(paused.pause_generation, 1);
        let err = share_team_history(
            &connection,
            "wsp_persistfixture000000000001",
            "2026-08-13T06:01:00Z",
            true,
            8,
            None,
        )
        .expect_err("paused share");
        assert!(err.to_string().contains("paused"));
        let resumed =
            set_local_team_paused(&connection, false, "2026-08-13T06:02:00Z").expect("resume");
        assert!(!resumed.paused);
        assert_eq!(resumed.pause_generation, 2);
        assert!(!any_local_team_paused(&connection).expect("any"));
    }

    #[test]
    fn add_local_team_node_binds_a_second_self_node() {
        let connection = open_db();
        create_local_team(
            &connection,
            "wsp_persistfixture000000000001",
            "Analysts",
            "2026-08-13T00:00:00Z",
        )
        .expect("create");
        let added = add_local_team_node(
            &connection,
            "wsp_persistfixture000000000001",
            "2026-08-13T07:00:00Z",
            None,
        )
        .expect("add");
        assert!(added.origin_node_id.starts_with("node_"));
        let status = local_team_status(&connection).expect("status");
        assert_eq!(
            status
                .members
                .iter()
                .filter(|member| member.is_self && member.state == "active")
                .count(),
            2
        );
    }

    #[test]
    fn project_inbound_team_memory_writes_a_metadata_stub() {
        let connection = open_db();
        connection
            .insert_workspace(
                "wsp_persistfixture000000000001",
                &crate::db::CreateWorkspaceInput {
                    path: "/tmp/ee-team-project".to_owned(),
                    name: Some("project".to_owned()),
                },
            )
            .expect("workspace");
        let created = create_local_team(
            &connection,
            "wsp_persistfixture000000000001",
            "Analysts",
            "2026-08-13T00:00:00Z",
        )
        .expect("create");
        connection
            .insert_memory(
                "mem_sharehistory00000000000001",
                &crate::db::CreateMemoryInput {
                    workspace_id: "wsp_persistfixture000000000001".to_owned(),
                    level: "procedural".to_owned(),
                    kind: "rule".to_owned(),
                    content: "secret body must not land on the receiver".to_owned(),
                    workflow_id: None,
                    confidence: 0.9,
                    utility: 0.5,
                    importance: 0.5,
                    provenance_uri: None,
                    trust_class: "human_explicit".to_owned(),
                    trust_subclass: None,
                    tags: Vec::new(),
                    valid_from: None,
                    valid_to: None,
                },
            )
            .expect("remember");
        share_team_history(
            &connection,
            "wsp_persistfixture000000000001",
            "2026-08-13T07:01:00Z",
            true,
            8,
            None,
        )
        .expect("share");
        let rows = connection
            .list_mesh_origin_events(&created.team.team_id, &created.team.origin_node_id, 0, 16)
            .expect("chain");
        let memory_row = rows
            .iter()
            .find(|row| row.payload_schema == "ee.mesh.memory_event.v1")
            .expect("memory event");
        let inbound = crate::mesh::origin_stream::inbound_from_stored(memory_row).expect("inbound");
        let projected =
            project_inbound_team_memory(&connection, "wsp_persistfixture000000000001", &inbound)
                .expect("project")
                .expect("id");
        let stored = connection
            .get_memory(&projected)
            .expect("get")
            .expect("row");
        assert!(stored.content.starts_with("[ee.team.history]"));
        assert!(!stored.content.contains("secret body"));
        assert_eq!(stored.trust_class, "peer_human_attested");
        assert_eq!(stored.trust_subclass.as_deref(), Some("agent:Analysts"));
    }

    #[test]
    fn granted_join_attempt_resumes_without_a_live_socket() {
        let connection = open_db();
        let created = create_local_team(
            &connection,
            "wsp_persistfixture000000000001",
            "Analysts",
            "2026-08-13T00:00:00Z",
        )
        .expect("create");
        let minted = mint_team_invite(
            &connection,
            "127.0.0.1:9",
            "2026-08-13T00:00:00Z",
            "2026-08-20T00:00:00Z",
        )
        .expect("mint");
        let granted = TeamJoinGrantedV1 {
            schema: TEAM_JOIN_GRANTED_SCHEMA_V1.to_owned(),
            team_id: created.team.team_id.clone(),
            origin_node_id: created.team.origin_node_id.clone(),
            display_name: created.team.display_name.clone(),
            hello_port: created.team.hello_port,
            genesis_event_hash: created.team.genesis_event_hash.clone(),
            pair_confirmation: "blake3:dead".to_owned(),
        };
        let joiner = open_db();
        joiner
            .upsert_team_join_attempt(&crate::db::UpsertTeamJoinAttemptInput {
                invite_id: minted.invite_id,
                team_id: created.team.team_id.clone(),
                joiner_node_id: "node_joinresume000000000000000001".to_owned(),
                joiner_nonce: "aa".repeat(16),
                inviter_nonce: Some("bb".repeat(16)),
                phase: "granted".to_owned(),
                granted_json: Some(serde_json::to_string(&granted).expect("json")),
                updated_at: "2026-08-13T08:00:00Z".to_owned(),
            })
            .expect("attempt");
        let report = join_team_with_code_on_store(
            &joiner,
            "wsp_joinworkspace0000000000001",
            &minted.invite_code,
            "Priya",
            "2026-08-13T08:01:00Z",
            std::time::Duration::from_millis(50),
            None,
        )
        .expect("resume");
        assert!(report.joined);
        assert_eq!(report.team.team_id, created.team.team_id);
    }

    #[cfg(unix)]
    #[test]
    fn rotate_local_signing_key_keeps_the_previous_generation() {
        let workspace = tempfile::tempdir().unwrap();
        let connection = open_db();
        let created = create_local_team_with_store(
            &connection,
            "wsp_persistfixture000000000001",
            "Analysts",
            "2026-08-13T00:00:00Z",
            Some(workspace.path()),
        )
        .expect("create");
        let first = connection
            .get_team_member_node(&created.team.origin_node_id, 1)
            .expect("gen1")
            .expect("bound");
        let rotated = rotate_local_signing_key(
            &connection,
            "wsp_persistfixture000000000001",
            "2026-08-13T08:00:00Z",
            workspace.path(),
        )
        .expect("rotate");
        assert!(rotated.state.contains("generation:2"));
        let second = connection
            .get_team_member_node(&created.team.origin_node_id, 2)
            .expect("gen2")
            .expect("bound");
        assert_ne!(first.verifying_key_hex, second.verifying_key_hex);
        assert_eq!(
            connection
                .get_team_member_node(&created.team.origin_node_id, 1)
                .expect("still gen1")
                .expect("kept")
                .verifying_key_hex,
            first.verifying_key_hex
        );
    }

    #[test]
    fn reconcile_reapplies_a_missed_member_removal() {
        let connection = open_db();
        create_local_team(
            &connection,
            "wsp_persistfixture000000000001",
            "Analysts",
            "2026-08-13T00:00:00Z",
        )
        .expect("create");
        let left = leave_local_team(
            &connection,
            "wsp_persistfixture000000000001",
            "2026-08-13T09:00:00Z",
            None,
        )
        .expect("leave");
        connection
            .set_team_member_state(&left.member_id, "active")
            .expect("rewind");
        assert!(
            origin_node_is_active_member(&connection, &left.origin_node_id)
                .expect("authz")
                .expect("members")
        );
        let report = reconcile_local_team_membership(&connection, "wsp_persistfixture000000000001")
            .expect("reconcile");
        assert_eq!(report.applied_removals, 1);
        assert!(
            !origin_node_is_active_member(&connection, &left.origin_node_id)
                .expect("authz after")
                .expect("members")
        );
        let again = reconcile_local_team_membership(&connection, "wsp_persistfixture000000000001")
            .expect("idempotent");
        assert_eq!(again.applied_removals, 0);
        assert_eq!(again.applied_additions, 0);
    }

    #[test]
    fn reconcile_reactivates_a_missed_member_addition() {
        let connection = open_db();
        let created = create_local_team(
            &connection,
            "wsp_persistfixture000000000001",
            "Analysts",
            "2026-08-13T00:00:00Z",
        )
        .expect("create");
        let member = connection
            .list_all_team_members()
            .expect("members")
            .into_iter()
            .next()
            .expect("self");
        connection
            .set_team_member_state(&member.member_id, "removed")
            .expect("rewind");
        assert!(
            !origin_node_is_active_member(&connection, &created.team.origin_node_id)
                .expect("authz")
                .expect("members")
        );
        let report = reconcile_local_team_membership(&connection, "wsp_persistfixture000000000001")
            .expect("reconcile");
        assert_eq!(report.applied_additions, 1);
        assert_eq!(report.applied_removals, 0);
        assert!(
            origin_node_is_active_member(&connection, &created.team.origin_node_id)
                .expect("authz after")
                .expect("members")
        );
    }

    #[cfg(unix)]
    #[test]
    fn challenged_join_attempt_keeps_joiner_identity_on_connect_failure() {
        let workspace = tempfile::tempdir().unwrap();
        let connection = open_db();
        create_local_team_with_store(
            &connection,
            "wsp_persistfixture000000000001",
            "Analysts",
            "2026-08-13T00:00:00Z",
            Some(workspace.path()),
        )
        .expect("create");
        let minted = mint_team_invite_with_store(
            &connection,
            "127.0.0.1:1",
            "2026-08-13T10:00:00Z",
            "2026-08-20T00:00:00Z",
            Some(workspace.path()),
        )
        .expect("mint");
        let parsed = parse_team_invite_code(&minted.invite_code).expect("parse");
        connection
            .upsert_team_join_attempt(&UpsertTeamJoinAttemptInput {
                invite_id: parsed.invite_id.clone(),
                team_id: parsed.team_id.clone(),
                joiner_node_id: "node_challengedresume000000000001".to_owned(),
                joiner_nonce: "aa".repeat(16),
                inviter_nonce: Some("bb".repeat(16)),
                phase: "challenged".to_owned(),
                granted_json: None,
                updated_at: "2026-08-13T10:01:00Z".to_owned(),
            })
            .expect("seed attempt");
        let error = join_team_with_code_on_store(
            &connection,
            "wsp_persistfixture000000000001",
            &minted.invite_code,
            "joiner",
            "2026-08-13T10:02:00Z",
            std::time::Duration::from_millis(50),
            Some(workspace.path()),
        )
        .expect_err("unreachable");
        assert!(error.to_string().contains("bootstrap join connect"));
        let attempt = connection
            .get_team_join_attempt(&parsed.invite_id)
            .expect("load")
            .expect("kept");
        assert_eq!(attempt.joiner_node_id, "node_challengedresume000000000001");
        assert_eq!(attempt.joiner_nonce, "aa".repeat(16));
        assert_eq!(attempt.phase, "challenged");
    }

    #[test]
    fn list_team_activity_lists_origin_events_without_bodies() {
        let connection = open_db();
        connection
            .insert_workspace(
                "wsp_persistfixture000000000001",
                &crate::db::CreateWorkspaceInput {
                    path: "/tmp/ee-team-activity".to_owned(),
                    name: Some("activity".to_owned()),
                },
            )
            .expect("workspace");
        create_local_team(
            &connection,
            "wsp_persistfixture000000000001",
            "Analysts",
            "2026-08-13T00:00:00Z",
        )
        .expect("create");
        connection
            .insert_memory(
                "mem_teamactivity00000000000001",
                &crate::db::CreateMemoryInput {
                    workspace_id: "wsp_persistfixture000000000001".to_owned(),
                    level: "procedural".to_owned(),
                    kind: "rule".to_owned(),
                    content: "secret body must stay off activity".to_owned(),
                    workflow_id: None,
                    confidence: 0.9,
                    utility: 0.5,
                    importance: 0.5,
                    provenance_uri: None,
                    trust_class: "human_explicit".to_owned(),
                    trust_subclass: None,
                    tags: Vec::new(),
                    valid_from: None,
                    valid_to: None,
                },
            )
            .expect("remember");
        share_team_history(
            &connection,
            "wsp_persistfixture000000000001",
            "2026-08-13T11:00:00Z",
            true,
            16,
            None,
        )
        .expect("share");
        let report = list_team_activity(
            &connection,
            "wsp_persistfixture000000000001",
            "2026-08-13T12:00:00Z",
            100,
        )
        .expect("activity");
        assert!(report.event_count >= 2);
        assert!(report.events.iter().any(|item| item.kind == "rule"));
        assert!(report.events.iter().any(|item| item.kind == "teamCreated"));
        assert!(report.events.iter().all(|item| !item.body_available));
        let json = serde_json::to_string(&report).expect("json");
        assert!(!json.contains("secret body must stay off activity"));
    }

    #[test]
    fn share_then_adopt_team_project_is_idempotent_by_name() {
        let connection = open_db();
        create_local_team(
            &connection,
            "wsp_persistfixture000000000001",
            "Analysts",
            "2026-08-13T00:00:00Z",
        )
        .expect("create");
        let first = share_team_project(
            &connection,
            "wsp_persistfixture000000000001",
            "acme-analysis",
            "/tmp/acme-analysis",
            "2026-08-13T13:00:00Z",
            None,
        )
        .expect("share");
        assert!(first.minted);
        assert!(first.projects[0].project_id.starts_with("prj_tm_"));
        let again = share_team_project(
            &connection,
            "wsp_persistfixture000000000001",
            "acme-analysis",
            "/tmp/other",
            "2026-08-13T13:01:00Z",
            None,
        )
        .expect("idempotent");
        assert!(!again.minted);
        assert_eq!(again.projects[0].project_id, first.projects[0].project_id);
        let adopted = adopt_team_project(
            &connection,
            &first.projects[0].project_id,
            "acme-analysis",
            "/tmp/clients/acme",
            "2026-08-13T13:02:00Z",
        )
        .expect("adopt");
        assert_eq!(adopted.projects[0].local_path, "/tmp/clients/acme");
        let listed = list_team_projects(&connection).expect("list");
        assert_eq!(listed.project_count, 1);
        assert_eq!(listed.projects[0].local_path, "/tmp/clients/acme");
    }

    #[cfg(unix)]
    #[test]
    fn share_team_bodies_publishes_then_unshare_stops_serving() {
        let workspace = tempfile::tempdir().unwrap();
        let connection = open_db();
        connection
            .insert_workspace(
                "wsp_persistfixture000000000001",
                &crate::db::CreateWorkspaceInput {
                    path: workspace.path().display().to_string(),
                    name: Some("bodies".to_owned()),
                },
            )
            .expect("workspace");
        create_local_team_with_store(
            &connection,
            "wsp_persistfixture000000000001",
            "Analysts",
            "2026-08-13T00:00:00Z",
            Some(workspace.path()),
        )
        .expect("create");
        connection
            .insert_memory(
                "mem_teambodies0000000000000001",
                &crate::db::CreateMemoryInput {
                    workspace_id: "wsp_persistfixture000000000001".to_owned(),
                    level: "semantic".to_owned(),
                    kind: "note".to_owned(),
                    content: "body bytes stay in the hardened cache".to_owned(),
                    workflow_id: None,
                    confidence: 0.8,
                    utility: 0.5,
                    importance: 0.5,
                    provenance_uri: None,
                    trust_class: "human_explicit".to_owned(),
                    trust_subclass: None,
                    tags: Vec::new(),
                    valid_from: None,
                    valid_to: None,
                },
            )
            .expect("remember");
        let preview = share_team_bodies(
            &connection,
            "wsp_persistfixture000000000001",
            "2026-08-13T14:00:00Z",
            false,
            16,
            Some(workspace.path()),
            false,
            None,
        )
        .expect("preview");
        assert!(!preview.confirmed);
        let preview_json = serde_json::to_string(&preview).expect("json");
        assert!(!preview_json.contains("body bytes stay"));
        let published = share_team_bodies(
            &connection,
            "wsp_persistfixture000000000001",
            "2026-08-13T14:01:00Z",
            true,
            16,
            Some(workspace.path()),
            false,
            None,
        )
        .expect("publish");
        assert_eq!(published.published_count, 1);
        assert_eq!(published.items[0].cache_status, "available");
        let again = share_team_bodies(
            &connection,
            "wsp_persistfixture000000000001",
            "2026-08-13T14:02:00Z",
            true,
            16,
            Some(workspace.path()),
            false,
            None,
        )
        .expect("idempotent");
        assert_eq!(again.published_count, 0);
        let fetched = fetch_local_team_body(
            &connection,
            "wsp_persistfixture000000000001",
            workspace.path(),
            &team_body_cache_key("mem_teambodies0000000000000001"),
        )
        .expect("fetch");
        assert_eq!(fetched.cache_status, "available");
        let body = fetched.body_hex.expect("bytes");
        assert_eq!(body, hex_encode(b"body bytes stay in the hardened cache"));
        let unshared = unshare_team_bodies(
            &connection,
            "wsp_persistfixture000000000001",
            "2026-08-13T14:03:00Z",
        )
        .expect("unshare");
        assert_eq!(unshared.published_count, 1);
        assert_eq!(unshared.items[0].cache_status, "evicted");
        let cache_json = serde_json::to_string(&unshared).expect("json");
        assert!(!cache_json.contains("body bytes stay"));
        let denied = fetch_local_team_body(
            &connection,
            "wsp_persistfixture000000000001",
            workspace.path(),
            &team_body_cache_key("mem_teambodies0000000000000001"),
        )
        .expect("denied");
        assert_eq!(denied.cache_status, "evicted");
        assert!(denied.body_hex.is_none());
    }

    #[cfg(unix)]
    #[test]
    fn share_team_bodies_token_binds_the_preview_and_rejects_drift() {
        let workspace = tempfile::tempdir().unwrap();
        let connection = open_db();
        connection
            .insert_workspace(
                "wsp_persistfixture000000000001",
                &crate::db::CreateWorkspaceInput {
                    path: workspace.path().display().to_string(),
                    name: Some("bodies-token".to_owned()),
                },
            )
            .expect("workspace");
        create_local_team_with_store(
            &connection,
            "wsp_persistfixture000000000001",
            "Analysts",
            "2026-08-13T00:00:00Z",
            Some(workspace.path()),
        )
        .expect("create");
        connection
            .insert_memory(
                "mem_teambodytoken0000000000001",
                &crate::db::CreateMemoryInput {
                    workspace_id: "wsp_persistfixture000000000001".to_owned(),
                    level: "semantic".to_owned(),
                    kind: "note".to_owned(),
                    content: "token-bound body".to_owned(),
                    workflow_id: None,
                    confidence: 0.8,
                    utility: 0.5,
                    importance: 0.5,
                    provenance_uri: None,
                    trust_class: "human_explicit".to_owned(),
                    trust_subclass: None,
                    tags: Vec::new(),
                    valid_from: None,
                    valid_to: None,
                },
            )
            .expect("remember");
        let preview = share_team_bodies(
            &connection,
            "wsp_persistfixture000000000001",
            "2026-08-13T16:00:00Z",
            false,
            16,
            Some(workspace.path()),
            true,
            None,
        )
        .expect("issue");
        let token = preview.approval_token.clone().expect("token");
        assert!(token.starts_with("eeap1_"));
        assert!(
            !serde_json::to_string(&preview)
                .expect("json")
                .contains("token-bound body")
        );
        let published = share_team_bodies(
            &connection,
            "wsp_persistfixture000000000001",
            "2026-08-13T16:01:00Z",
            true,
            16,
            Some(workspace.path()),
            false,
            Some(token.as_str()),
        )
        .expect("confirm");
        assert_eq!(published.published_count, 1);
        let drifted = share_team_bodies(
            &connection,
            "wsp_persistfixture000000000001",
            "2026-08-13T16:02:00Z",
            true,
            16,
            Some(workspace.path()),
            false,
            Some(token.as_str()),
        )
        .expect_err("stale after publish");
        assert!(drifted.to_string().contains("stale") || drifted.to_string().contains("authentic"));
    }

    #[test]
    fn team_steward_once_skips_when_paused_or_solo() {
        let connection = open_db();
        create_local_team(
            &connection,
            "wsp_persistfixture000000000001",
            "Analysts",
            "2026-08-13T00:00:00Z",
        )
        .expect("create");
        let solo = plan_team_steward_once(&connection).expect("solo");
        assert!(!solo.ran_sync);
        assert_eq!(solo.reason, "no_actionable_drift");
        set_local_team_paused(&connection, true, "2026-08-13T15:00:00Z").expect("pause");
        let paused = plan_team_steward_once(&connection).expect("paused");
        assert!(!paused.ran_sync);
        assert_eq!(paused.reason, "team_paused");
        set_local_team_paused(&connection, false, "2026-08-13T15:01:00Z").expect("resume");
        add_local_team_node(
            &connection,
            "wsp_persistfixture000000000001",
            "2026-08-13T15:02:00Z",
            None,
        )
        .expect("second node");
        let ready = plan_team_steward_once(&connection).expect("ready");
        assert!(ready.ran_sync);
        assert_eq!(ready.reason, "new_peers");
    }

    #[test]
    fn inspect_team_health_reports_no_team_then_ok_then_paused() {
        let connection = open_db();
        let missing = inspect_team_health(&connection, "wsp_persistfixture000000000001", None)
            .expect("missing");
        assert_eq!(missing.posture, "no_team");
        create_local_team(
            &connection,
            "wsp_persistfixture000000000001",
            "Analysts",
            "2026-08-13T00:00:00Z",
        )
        .expect("create");
        let healthy = inspect_team_health(&connection, "wsp_persistfixture000000000001", None)
            .expect("healthy");
        assert_eq!(healthy.posture, "ok");
        assert!(healthy.checks.iter().any(|check| check.name == "genesis"));
        set_local_team_paused(&connection, true, "2026-08-13T17:00:00Z").expect("pause");
        let paused = inspect_team_health(&connection, "wsp_persistfixture000000000001", None)
            .expect("paused");
        assert_eq!(paused.posture, "paused");
    }

    fn tailnet_report(self_login: &str, peer_login: Option<(&str, &str)>) -> TailscaleLocalReport {
        TailscaleLocalReport {
            schema: crate::core::tailscale_probe::TAILSCALE_LOCAL_SCHEMA_V1,
            installed: true,
            daemon_reachable: true,
            authenticated: true,
            binary_authentic: true,
            binary_version_raw: None,
            binary_absolute_path: None,
            shields_up: Some(false),
            tailnet_id: Some("tailnet-alpha".to_owned()),
            tailnet_display_name: Some("alpha.example".to_owned()),
            self_node_key: Some("nodekey:self".to_owned()),
            self_tailscale_ip: Some("100.64.0.1".to_owned()),
            self_magic_dns_name: Some("self.tailnet.test.".to_owned()),
            self_advertised_tags: Vec::new(),
            self_owner: Some(TailscaleUserProfile {
                user_id: "11".to_owned(),
                login_name: self_login.to_owned(),
                display_name: Some("Self".to_owned()),
            }),
            peers: peer_login
                .map(|(login, user_id)| {
                    vec![crate::core::tailscale_probe::TailscalePeerReport {
                        node_key: "nodekey:peer".to_owned(),
                        tailscale_ips: vec!["100.64.0.2".to_owned()],
                        magic_dns_name: Some("peer.tailnet.test.".to_owned()),
                        hostname: Some("peer".to_owned()),
                        advertised_tags: Vec::new(),
                        online: Some(true),
                        ee_capability: None,
                        owner: Some(TailscaleUserProfile {
                            user_id: user_id.to_owned(),
                            login_name: login.to_owned(),
                            display_name: Some("Peer".to_owned()),
                        }),
                    }]
                })
                .unwrap_or_default(),
            version: Some("1.66.0".to_owned()),
            probe_method: crate::core::tailscale_probe::TailscaleProbeMethod::Cli,
            probe_elapsed_ms: 4,
            platform: crate::core::tailscale_probe::TailscalePlatform::Linux,
            degradations: Vec::new(),
        }
    }

    #[test]
    fn require_tailnet_attested_persists_policy_and_revalidate_suspends_reassignment() {
        let connection = open_db();
        create_local_team(
            &connection,
            "wsp_persistfixture000000000001",
            "Analysts",
            "2026-08-13T00:00:00Z",
        )
        .expect("create");
        let required = require_tailnet_attested(
            &connection,
            "wsp_persistfixture000000000001",
            Some("acme.com"),
            "2026-08-13T18:00:00Z",
            None,
        )
        .expect("require");
        assert_eq!(required.kind, "tailnet_attested");
        assert_eq!(required.allowed_domain.as_deref(), Some("acme.com"));
        assert_eq!(required.policy_generation, 1);
        let status = team_idp_status(&connection).expect("status");
        assert_eq!(status.kind, "tailnet_attested");
        let first = revalidate_team_identities(
            &connection,
            &tailnet_report("alice@acme.com", None),
            "2026-08-13T18:01:00Z",
        )
        .expect("first");
        assert_eq!(first.attested, 1);
        assert_eq!(first.suspended, 0);
        let reassigned = revalidate_team_identities(
            &connection,
            &tailnet_report("mallory@acme.com", None),
            "2026-08-13T18:02:00Z",
        )
        .expect("reassigned");
        assert_eq!(reassigned.attested, 0);
        assert_eq!(reassigned.suspended, 1);
        assert_eq!(reassigned.members[0].disposition, "reassigned");
        assert_eq!(reassigned.members[0].state, "suspended");
        let identity = connection
            .get_team_member_identity(&reassigned.members[0].member_id)
            .expect("load")
            .expect("row");
        assert_eq!(identity.state, "suspended");
        let blocked = plan_team_steward_once(&connection).expect("blocked");
        assert!(!blocked.ran_sync);
        assert_eq!(blocked.reason, "identity_revalidation_failed");
        let outside = revalidate_team_identities(
            &connection,
            &tailnet_report("alice@other.com", None),
            "2026-08-13T18:03:00Z",
        )
        .expect("domain");
        assert_eq!(outside.members[0].disposition, "domain_mismatch");
        assert_eq!(outside.suspended, 1);
    }

    #[test]
    fn set_team_oidc_provider_accepts_secretless_and_rejects_client_secret() {
        let connection = open_db();
        create_local_team(
            &connection,
            "wsp_persistfixture000000000001",
            "Analysts",
            "2026-08-13T00:00:00Z",
        )
        .expect("create");
        let accepted = set_team_oidc_provider(
            &connection,
            "https://idp.example",
            "ee-public",
            &serde_json::json!({
                "token_endpoint": "https://idp.example/token",
                "device_authorization_endpoint": "https://idp.example/device",
                "token_endpoint_auth_methods_supported": ["none"]
            }),
            "2026-08-13T19:00:00Z",
        )
        .expect("set");
        assert_eq!(accepted.capability, "secretless_public");
        assert!(accepted.discovery_hash.starts_with("blake3:"));
        let rejected = set_team_oidc_provider(
            &connection,
            "https://idp.example",
            "ee-public",
            &serde_json::json!({
                "token_endpoint": "https://idp.example/token",
                "device_authorization_endpoint": "https://idp.example/device",
                "token_endpoint_auth_methods_supported": ["client_secret_basic"]
            }),
            "2026-08-13T19:01:00Z",
        )
        .expect_err("secret");
        assert!(rejected.to_string().contains("client_secret_required"));
    }
}
