//! Local team genesis over the signed origin stream.
//!
//! `ee team create` appends one origin-owned `teamCreated` manifest event.
//! Invites use a signed hello/challenge/prove ceremony, pair keys bind the
//! live transcript, and inbound origin events verify against `team_member_nodes`.
//! This module still does not advertise `mesh.team.memory.v1`.

use std::collections::BTreeSet;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::config::{EnvVar, read_env_var};
use crate::core::tailscale_probe::{
    TailnetOwnerDisposition, TailscaleLocalReport, TailscaleUserProfile, evaluate_tailnet_owner,
};
use crate::db::{
    CreateMemoryInput, CreateSearchIndexJobInput, DbConnection, InsertMeshImportLedgerEventInput,
    InsertTeamHistoryProjectionInput, InsertTeamMemberInput, InsertTeamMemberNodeInput,
    InsertTeamPendingInviteInput, InsertTeamProjectInput, InsertTeamRemovalAckInput,
    SearchIndexJobType, StoredMeshOriginEvent, StoredTeamMember, StoredTeamMemberIdentity,
    StoredTeamProject, UpsertMeshBodyCacheMetadataInput, UpsertMeshPeerInput,
    UpsertTeamAdmissionPeerInput, UpsertTeamJoinAttemptInput,
};
use crate::mesh::bootstrap_envelope::{
    BOOTSTRAP_DECLINE_SCHEMA_V1, BootstrapCapability, BootstrapDeclineV1, SYNC_ROUND_SCHEMA_V1,
    SyncRoundEvent, SyncRoundRequest, SyncRoundResponse, SyncRoundTip, decode_envelope,
    encode_envelope, exchange_live_mesh_round, parse_sync_round_request, read_std_framed,
    write_std_framed,
};
use crate::mesh::hello_responder::configured_hello_port;
use crate::mesh::idp::{
    IDENTITY_ATTEST_FRAME_SCHEMA_V1, IdentityAttestFrameV1, IdentityRevalidationPosture,
    IdpProviderCapability, classify_id_token_claims, classify_identity_revalidation,
    classify_oidc_provider, decide_device_poll, device_poll_deadline_secs,
    discovery_https_endpoint, execute_constrained_https, form_urlencoded,
    identity_attest_frame_leaks_bearer, json_carries_bearer_fields, parse_device_authorization,
    pin_constrained_https_ca, plan_constrained_https_post, reduce_id_token_claims,
    verify_compact_jwt_with_jwks,
};
use crate::mesh::origin_stream::{
    Ed25519OriginSigner, InboundOriginEvent, IngestDisposition, ManifestEventPayload,
    MemoryEventOperation, MemoryEventPayload, OriginAppendRequest, OriginEventPayload,
    OriginSignatureVerifier, OriginSigner, OriginStreamError, append_origin_event, body_commitment,
    inbound_from_stored, ingest_origin_event, parse_stored_payload,
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
/// Placeholder tailnet on team-join enroll until LocalAPI observes the real one.
pub const TEAM_JOIN_TAILNET_ID: &str = "tailnet-team-join";
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
pub const TEAM_PORT_SCHEMA_V1: &str = "ee.team.port.v1";
pub const TEAM_PORT_MIGRATED_OPERATION: &str = "teamPortMigrated";
/// Manifest scan for genesis + `teamPortMigrated`. Membership/project/IdP
/// rows share this table; 256 is too small once a team has been busy.
const TEAM_PORT_MANIFEST_SCAN_LIMIT: u32 = 4096;
const TEAM_PROJECT_ID_PREFIX: &str = "prj_tm_";
const TEAM_PROJECT_ID_LEN: usize = 33;

fn is_team_project_id(project_id: &str) -> bool {
    project_id.starts_with(TEAM_PROJECT_ID_PREFIX) && project_id.len() == TEAM_PROJECT_ID_LEN
}
pub const TEAM_IDP_SCHEMA_V1: &str = "ee.team.idp.v1";
pub const TEAM_IDP_REVALIDATE_SCHEMA_V1: &str = "ee.team.idp.revalidate.v1";
pub const TEAM_IDP_SET_SCHEMA_V1: &str = "ee.team.idp.set.v1";
pub const TEAM_IDP_DEVICE_SCHEMA_V1: &str = "ee.team.idp.device.v1";
pub const TEAM_IDP_POLL_SCHEMA_V1: &str = "ee.team.idp.poll.v1";
pub const TEAM_IDP_ATTEST_SCHEMA_V1: &str = "ee.team.idp.attest.v1";
pub const TEAM_IDP_POLICY_SET_OPERATION: &str = "teamIdpPolicySet";
pub const TEAM_IDP_ATTESTED_OPERATION: &str = "identityAttested";
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
pub struct TeamPortReport {
    pub schema: &'static str,
    pub command: &'static str,
    pub team_id: String,
    pub genesis_event_hash: String,
    pub genesis_hello_port: u16,
    pub current_hello_port: u16,
    pub previous_hello_port: Option<u16>,
    pub port_generation: u64,
    pub configured_hello_port: u16,
    pub migrated: bool,
    pub pair_keys_unchanged: bool,
    pub grants_unchanged: bool,
    pub peer_endpoints_rewritten: usize,
    pub mesh_primitives: Vec<&'static str>,
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
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pending_invites: Vec<TeamPendingInviteRecord>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pending_removal_acks: Vec<TeamRemovalAckRecord>,
    pub admission: TeamAdmissionStatus,
    pub budgets: TeamConfedBudgetProfile,
    pub paused: bool,
    pub steward_would_sync: bool,
    pub mesh_primitives: Vec<&'static str>,
}

pub const TEAM_BUDGETS_SCHEMA_V1: &str = "ee.team.budgets.v1";

/// Published T6.5 join / signed-relay / body / index amplification profile.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TeamConfedBudgetProfile {
    pub schema: &'static str,
    pub join_event_batch_count: u32,
    pub signed_relay_event_batch_bytes: u64,
    pub body_fetch_bytes: u64,
    pub index_jobs_per_round: u32,
    pub concurrent_requests_per_peer: u32,
    pub free_space_floor_bytes: u64,
    pub local_tier1_unaffected: bool,
}

/// Conservative join/relay/body/index caps. Same numbers `decide_admission` enforces.
#[must_use]
pub fn team_confed_budget_profile() -> TeamConfedBudgetProfile {
    let limits = crate::mesh::admission::MeshAdmissionLimits::conservative_default();
    TeamConfedBudgetProfile {
        schema: TEAM_BUDGETS_SCHEMA_V1,
        join_event_batch_count: limits.max_event_batch_count,
        signed_relay_event_batch_bytes: limits.max_event_batch_bytes,
        body_fetch_bytes: limits.max_body_fetch_bytes,
        index_jobs_per_round: limits.max_index_jobs_per_round,
        concurrent_requests_per_peer: limits.max_concurrent_requests_per_peer,
        free_space_floor_bytes: TEAM_FREE_SPACE_FLOOR_BYTES,
        local_tier1_unaffected: true,
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TeamRemovalAckRecord {
    pub removal_event_hash: String,
    pub audience_origin_node_id: String,
    pub removal_seq: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub acknowledged_at: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TeamAdmissionStatus {
    pub max_event_batch_count: u32,
    pub max_event_batch_bytes: u64,
    pub max_body_fetch_bytes: u64,
    pub max_index_jobs_per_round: u32,
    pub local_tier1_unaffected: bool,
    pub peer_snapshot_count: usize,
    pub throttled_peer_count: usize,
    pub budget_exhausted_peer_count: usize,
    pub coalesced_exhaustion: bool,
}

/// Team doctor warns when the workspace volume has less than this free.
pub const TEAM_FREE_SPACE_FLOOR_BYTES: u64 = 64 * 1024 * 1024;

#[cfg(unix)]
fn workspace_available_bytes(path: &std::path::Path) -> Option<u64> {
    let candidate = if path.exists() { path } else { path.parent()? };
    let stat = rustix::fs::statvfs(candidate).ok()?;
    let block = if stat.f_frsize == 0 {
        stat.f_bsize
    } else {
        stat.f_frsize
    };
    Some(stat.f_bavail.saturating_mul(block))
}

#[cfg(not(unix))]
fn workspace_available_bytes(_path: &std::path::Path) -> Option<u64> {
    None
}

/// Persist the authenticated admission map so doctor/status can report usage
/// after the broker process exits.
pub fn persist_team_admission_states(
    connection: &DbConnection,
    workspace_id: &str,
    peers: &[crate::mesh::admission::MeshPeerAdmissionState],
    updated_at: &str,
) -> Result<usize, OriginStreamError> {
    let mut written = 0_usize;
    for peer in peers {
        connection
            .upsert_team_admission_peer(&UpsertTeamAdmissionPeerInput {
                workspace_id: workspace_id.to_owned(),
                peer_id: peer.peer_id.clone(),
                in_flight_requests: peer.in_flight_requests,
                malformed_frame_count: peer.malformed_frame_count,
                policy_denial_count: peer.policy_denial_count,
                backoff_until_epoch_ms: peer.backoff_until_epoch_ms,
                local_tier1_reserved: peer.local_tier1_reserved,
                updated_at: updated_at.to_owned(),
            })
            .map_err(|error| OriginStreamError::Db(error.to_string()))?;
        written = written.saturating_add(1);
    }
    Ok(written)
}

fn load_team_admission_status(
    connection: &DbConnection,
    limits: crate::mesh::admission::MeshAdmissionLimits,
) -> TeamAdmissionStatus {
    let workspace_id = self_workspace_id(connection).ok().flatten();
    let peers = workspace_id
        .as_deref()
        .and_then(|workspace_id| connection.list_team_admission_peers(workspace_id).ok())
        .unwrap_or_default();
    let throttled_peer_count = peers
        .iter()
        .filter(|peer| peer.backoff_until_epoch_ms.is_some() || peer.malformed_frame_count > 0)
        .count();
    let budget_exhausted_peer_count = peers
        .iter()
        .filter(|peer| peer.policy_denial_count > 0)
        .count();
    let coalesced_exhaustion = throttled_peer_count > 0 || budget_exhausted_peer_count > 0;
    let local_tier1_unaffected =
        peers.is_empty() || peers.iter().all(|peer| peer.local_tier1_reserved);
    TeamAdmissionStatus {
        max_event_batch_count: limits.max_event_batch_count,
        max_event_batch_bytes: limits.max_event_batch_bytes,
        max_body_fetch_bytes: limits.max_body_fetch_bytes,
        max_index_jobs_per_round: limits.max_index_jobs_per_round,
        local_tier1_unaffected,
        peer_snapshot_count: peers.len(),
        throttled_peer_count,
        budget_exhausted_peer_count,
        coalesced_exhaustion,
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TeamPendingInviteRecord {
    pub invite_id: String,
    pub team_id: String,
    pub status: String,
    pub created_at: String,
    pub expires_at: String,
    pub endpoint: String,
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
    pub representation: String,
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
    pub applied_additions: usize,
    pub applied_removals: usize,
    pub stalled_cursors: usize,
    pub deferred_pairings: usize,
    pub applied_pair_promotions: usize,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub oidc_issuer: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub oidc_capability: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub leases: Vec<TeamIdentityLease>,
    pub mesh_primitives: Vec<&'static str>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TeamIdentityLease {
    pub member_id: String,
    pub login: String,
    pub state: String,
    pub checked_at: String,
    pub posture: String,
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
pub struct TeamIdpDeviceReport {
    pub schema: &'static str,
    pub command: &'static str,
    pub team_id: String,
    pub capability: String,
    pub user_code: String,
    pub verification_uri: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verification_uri_complete: Option<String>,
    pub expires_in: u64,
    pub interval: u64,
    pub deadline_secs: u64,
    pub first_wait_secs: u64,
    pub curl_argv: Vec<String>,
    pub mesh_primitives: Vec<&'static str>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TeamIdpPollReport {
    pub schema: &'static str,
    pub command: &'static str,
    pub team_id: String,
    pub user_code: String,
    pub verification_uri: String,
    pub curl_exit_code: i32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token_error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub jwt: Option<String>,
    pub has_access_token: bool,
    pub has_refresh_token: bool,
    pub mesh_primitives: Vec<&'static str>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TeamIdpAttestReport {
    pub schema: &'static str,
    pub command: &'static str,
    pub team_id: String,
    pub member_id: String,
    pub subject: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    pub matched_groups: Vec<String>,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub since: Option<String>,
    pub time_filter_basis: &'static str,
    pub sequence_complete: bool,
    pub event_count: usize,
    pub events: Vec<TeamActivityItem>,
    pub clock_anomalies: Vec<TeamActivityItem>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cursor_error: Option<&'static str>,
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
    connection
        .raise_team_invite_auth_floor(&team_id, produced_at, produced_at)
        .map_err(|error| OriginStreamError::Db(error.to_string()))?;
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
        .list_mesh_manifest_origin_events(TEAM_PORT_MANIFEST_SCAN_LIMIT)
        .map_err(|error| OriginStreamError::Db(error.to_string()))?;
    let mut teams = Vec::new();
    let mut migrated_ports = std::collections::BTreeMap::<String, u16>::new();
    for row in rows {
        if let Ok(OriginEventPayload::Manifest(payload)) = parse_stored_payload(&row)
            && payload.operation == TEAM_PORT_MIGRATED_OPERATION
        {
            if let Some(port) = payload
                .document_payload
                .get("helloPort")
                .and_then(serde_json::Value::as_u64)
                .and_then(|port| u16::try_from(port).ok())
                .filter(|port| *port >= 1024)
            {
                migrated_ports.insert(row.team_id.clone(), port);
            }
            continue;
        }
        if let Some(team) = team_record_from_origin(&row)? {
            teams.push(team);
        }
    }
    for team in &mut teams {
        if let Some(port) = migrated_ports.get(&team.team_id) {
            team.hello_port = *port;
        }
    }
    Ok(teams)
}

/// Report the folded hello port and generation without mutating state.
pub fn inspect_team_port(connection: &DbConnection) -> Result<TeamPortReport, OriginStreamError> {
    let team = load_local_teams(connection)?
        .into_iter()
        .next()
        .ok_or_else(|| OriginStreamError::Encode("no local team genesis".to_owned()))?;
    let (genesis_hello_port, port_generation, previous_hello_port) =
        port_migration_state(connection, &team.team_id)?;
    Ok(TeamPortReport {
        schema: TEAM_PORT_SCHEMA_V1,
        command: "team port show",
        team_id: team.team_id,
        genesis_event_hash: team.genesis_event_hash,
        genesis_hello_port,
        current_hello_port: team.hello_port,
        previous_hello_port,
        port_generation,
        configured_hello_port: configured_hello_port(),
        migrated: false,
        pair_keys_unchanged: true,
        grants_unchanged: true,
        peer_endpoints_rewritten: 0,
        mesh_primitives: if port_generation > 1 {
            vec![
                "ee.team.manifest_event.v1",
                TEAM_CREATED_OPERATION,
                TEAM_PORT_MIGRATED_OPERATION,
            ]
        } else {
            vec!["ee.team.manifest_event.v1", TEAM_CREATED_OPERATION]
        },
    })
}

/// Append a versioned `teamPortMigrated` event and rewrite enrolled peer
/// endpoints that still advertise the previous port. Pair keys and grants
/// are not opened or rewritten.
pub fn migrate_local_team_port(
    connection: &DbConnection,
    workspace_id: &str,
    next_port: u16,
    produced_at: &str,
    workspace_path: Option<&std::path::Path>,
) -> Result<TeamPortReport, OriginStreamError> {
    if next_port < 1024 {
        return Err(OriginStreamError::Encode(
            "team hello port must be nonprivileged".to_owned(),
        ));
    }
    let team = load_local_teams(connection)?
        .into_iter()
        .next()
        .ok_or_else(|| OriginStreamError::Encode("no local team genesis".to_owned()))?;
    if team_is_paused(connection, &team.team_id)? {
        return Err(OriginStreamError::Encode(
            "team is paused; resume before migrating the hello port".to_owned(),
        ));
    }
    let (genesis_hello_port, current_generation, _) =
        port_migration_state(connection, &team.team_id)?;
    if next_port == team.hello_port {
        return Err(OriginStreamError::Encode(
            "team hello port is already the requested port".to_owned(),
        ));
    }
    let next_generation = current_generation.saturating_add(1);
    let previous_port = team.hello_port;
    let hex = blake3::hash(
        format!(
            "{}:{TEAM_PORT_MIGRATED_OPERATION}:{next_port}:{next_generation}",
            team.team_id
        )
        .as_bytes(),
    )
    .to_hex();
    let payload = OriginEventPayload::Manifest(ManifestEventPayload {
        operation: TEAM_PORT_MIGRATED_OPERATION.to_owned(),
        document_id: format!("tdoc_{}", &hex.as_str()[..24]),
        predecessor_revision_id: None,
        document_payload: serde_json::json!({
            "helloPort": next_port,
            "previousHelloPort": previous_port,
            "portGeneration": next_generation,
            "genesisEventHash": team.genesis_event_hash,
        }),
    });
    let ed25519 = workspace_path
        .map(|path| Ed25519OriginSigner::load_or_create(path, &team.origin_node_id, produced_at))
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
    let rewritten =
        rewrite_peer_endpoints_for_port(connection, workspace_id, previous_port, next_port)?;
    Ok(TeamPortReport {
        schema: TEAM_PORT_SCHEMA_V1,
        command: "team port migrate",
        team_id: team.team_id,
        genesis_event_hash: team.genesis_event_hash,
        genesis_hello_port,
        current_hello_port: next_port,
        previous_hello_port: Some(previous_port),
        port_generation: next_generation,
        configured_hello_port: configured_hello_port(),
        migrated: true,
        pair_keys_unchanged: true,
        grants_unchanged: true,
        peer_endpoints_rewritten: rewritten,
        mesh_primitives: vec![
            "mesh_origin_events.append",
            "ee.team.manifest_event.v1",
            TEAM_PORT_MIGRATED_OPERATION,
        ],
    })
}

/// Apply imported `teamPortMigrated` events onto enrolled peer locators.
pub fn apply_imported_team_port_migrations(
    connection: &DbConnection,
    workspace_id: &str,
) -> Result<usize, OriginStreamError> {
    let local_team_ids = load_local_teams(connection)?
        .into_iter()
        .map(|team| team.team_id)
        .collect::<BTreeSet<_>>();
    let rows = connection
        .list_mesh_manifest_origin_events(TEAM_PORT_MANIFEST_SCAN_LIMIT)
        .map_err(|error| OriginStreamError::Db(error.to_string()))?;
    let mut rewritten = 0_usize;
    for row in rows {
        if !local_team_ids.is_empty() && !local_team_ids.contains(&row.team_id) {
            continue;
        }
        let Ok(OriginEventPayload::Manifest(payload)) = parse_stored_payload(&row) else {
            continue;
        };
        if payload.operation != TEAM_PORT_MIGRATED_OPERATION {
            continue;
        }
        let Some(next_port) = payload
            .document_payload
            .get("helloPort")
            .and_then(serde_json::Value::as_u64)
            .and_then(|port| u16::try_from(port).ok())
            .filter(|port| *port >= 1024)
        else {
            continue;
        };
        let Some(previous_port) = payload
            .document_payload
            .get("previousHelloPort")
            .and_then(serde_json::Value::as_u64)
            .and_then(|port| u16::try_from(port).ok())
        else {
            continue;
        };
        rewritten = rewritten.saturating_add(rewrite_peer_endpoints_for_port(
            connection,
            workspace_id,
            previous_port,
            next_port,
        )?);
    }
    Ok(rewritten)
}

fn port_migration_state(
    connection: &DbConnection,
    team_id: &str,
) -> Result<(u16, u64, Option<u16>), OriginStreamError> {
    let rows = connection
        .list_mesh_manifest_origin_events(TEAM_PORT_MANIFEST_SCAN_LIMIT)
        .map_err(|error| OriginStreamError::Db(error.to_string()))?;
    let mut genesis_port = configured_hello_port();
    let mut generation = 1_u64;
    let mut previous = None;
    for row in rows {
        if row.team_id != team_id {
            continue;
        }
        let Ok(OriginEventPayload::Manifest(payload)) = parse_stored_payload(&row) else {
            continue;
        };
        if payload.operation == TEAM_CREATED_OPERATION || payload.operation == TEAM_JOINED_OPERATION
        {
            if let Some(port) = payload
                .document_payload
                .get("helloPort")
                .and_then(serde_json::Value::as_u64)
                .and_then(|port| u16::try_from(port).ok())
                .filter(|port| *port >= 1024)
            {
                genesis_port = port;
            }
        }
        if payload.operation == TEAM_PORT_MIGRATED_OPERATION {
            generation = payload
                .document_payload
                .get("portGeneration")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(generation.saturating_add(1));
            previous = payload
                .document_payload
                .get("previousHelloPort")
                .and_then(serde_json::Value::as_u64)
                .and_then(|port| u16::try_from(port).ok());
        }
    }
    Ok((genesis_port, generation, previous))
}

fn rewrite_peer_endpoints_for_port(
    connection: &DbConnection,
    workspace_id: &str,
    previous_port: u16,
    next_port: u16,
) -> Result<usize, OriginStreamError> {
    if previous_port == next_port {
        return Ok(0);
    }
    let peers = connection
        .list_mesh_peers(workspace_id)
        .map_err(|error| OriginStreamError::Db(error.to_string()))?;
    let mut rewritten = 0_usize;
    for stored in peers {
        let Some(json) = stored.policy_summary_json.as_deref() else {
            continue;
        };
        let Ok(mut record) = serde_json::from_str::<crate::mesh::peer::MeshPeerRecord>(json) else {
            continue;
        };
        let Some(updated) =
            replace_endpoint_port(&record.endpoint.endpoint, previous_port, next_port)
        else {
            continue;
        };
        record.endpoint.endpoint = updated;
        let policy = serde_json::to_string(&record)
            .map_err(|error| OriginStreamError::Encode(error.to_string()))?;
        connection
            .upsert_mesh_peer(&UpsertMeshPeerInput {
                workspace_id: stored.workspace_id,
                peer_id: stored.peer_id,
                origin_node_id: stored.origin_node_id,
                display_name: stored.display_name,
                policy_summary_json: Some(policy),
                enabled: stored.enabled,
                last_seen_at: Some(stored.last_seen_at),
            })
            .map_err(|error| OriginStreamError::Db(error.to_string()))?;
        rewritten = rewritten.saturating_add(1);
    }
    Ok(rewritten)
}

fn replace_endpoint_port(endpoint: &str, previous_port: u16, next_port: u16) -> Option<String> {
    let (host, port) = endpoint.rsplit_once(':')?;
    if port.parse::<u16>().ok()? != previous_port || host.is_empty() {
        return None;
    }
    // Bare IPv6 (`fd7a:115c::1`) contains colons but is not host:port.
    // Bracket form (`[fd7a:115c::1]:41888`) is the only IPv6 locator we rewrite.
    if host.contains(':') && !host.starts_with('[') {
        return None;
    }
    Some(format!("{host}:{next_port}"))
}

/// Port a local hello responder should bind.
///
/// `EE_MESH_HELLO_PORT` wins when set to a non-privileged integer. Otherwise
/// the folded team hello port (including `teamPortMigrated`) is used.
/// No team genesis falls back to [`configured_hello_port`].
#[must_use]
pub fn local_hello_bind_port(connection: &DbConnection) -> u16 {
    if read_env_var(EnvVar::MeshHelloPort)
        .as_deref()
        .is_some_and(|value| value.trim().parse::<u16>().is_ok_and(|port| port >= 1024))
    {
        return configured_hello_port();
    }
    load_local_teams(connection)
        .ok()
        .and_then(|teams| teams.into_iter().next())
        .map(|team| team.hello_port)
        .filter(|port| *port >= 1024)
        .unwrap_or_else(configured_hello_port)
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
    let mut pending_invites = Vec::new();
    let mut pending_removal_acks = Vec::new();
    for team in &teams {
        pending_invites.extend(
            connection
                .list_team_pending_invites(&team.team_id)
                .map_err(|error| OriginStreamError::Db(error.to_string()))?
                .into_iter()
                .map(|invite| TeamPendingInviteRecord {
                    invite_id: invite.invite_id,
                    team_id: invite.team_id,
                    status: invite.status,
                    created_at: invite.created_at,
                    expires_at: invite.expires_at,
                    endpoint: invite.endpoint,
                }),
        );
        pending_removal_acks.extend(
            connection
                .list_team_removal_acks(&team.team_id)
                .map_err(|error| OriginStreamError::Db(error.to_string()))?
                .into_iter()
                .filter(|ack| ack.acknowledged_at.is_none())
                .map(|ack| TeamRemovalAckRecord {
                    removal_event_hash: ack.removal_event_hash,
                    audience_origin_node_id: ack.audience_origin_node_id,
                    removal_seq: ack.removal_seq,
                    acknowledged_at: ack.acknowledged_at,
                }),
        );
    }
    let limits = crate::mesh::admission::MeshAdmissionLimits::conservative_default();
    let admission = load_team_admission_status(connection, limits);
    Ok(TeamStatusReport {
        schema: TEAM_STATUS_SCHEMA_V1,
        command: "team status",
        team_count: teams.len(),
        teams,
        members,
        nodes,
        pending_invites,
        pending_removal_acks,
        admission,
        budgets: team_confed_budget_profile(),
        paused,
        steward_would_sync,
        mesh_primitives: vec![
            "mesh_origin_events",
            "ee.team.manifest_event.v1",
            "team_members",
            "team_member_nodes",
            "team_pending_invites",
            "team_removal_acknowledgements",
            "mesh_admission_control",
            "ee.team.budgets.v1",
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
            format!("ee mesh hello-responder run {workspace_flag} --json"),
            format!("ee team invite {workspace_flag} --json"),
            format!("ee mesh sync --once {workspace_flag} --json"),
            format!("ee daemon install {workspace_flag} --json"),
        ],
        team,
        mesh_primitives: vec![
            "mesh_origin_events.append",
            "ee.team.manifest_event.v1",
            "teamCreated",
            "team_invite_auth_floor",
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
        .filter(|port| *port >= 1024)
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
    /// Inviter workspace. Empty on pre-campaign invites.
    #[serde(default)]
    pub origin_workspace_id: String,
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
    pub first_sync_served: bool,
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
    /// Joiner workspace. Empty on pre-campaign hellos.
    #[serde(default)]
    pub joiner_workspace_id: String,
    /// Joiner hello listen port. Zero on pre-campaign hellos.
    #[serde(default)]
    pub joiner_hello_port: u16,
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
    /// Inviter workspace. Empty on pre-campaign grants.
    #[serde(default)]
    pub origin_workspace_id: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TeamJoinFirstSync {
    pub complete: bool,
    pub imported_events: u32,
}

impl TeamJoinFirstSync {
    #[must_use]
    pub const fn incomplete() -> Self {
        Self {
            complete: false,
            imported_events: 0,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TeamJoinReport {
    pub schema: &'static str,
    pub command: &'static str,
    pub joined: bool,
    pub team: TeamRecord,
    pub first_sync: TeamJoinFirstSync,
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
    let origin_workspace_id = self_workspace_id(connection)?.unwrap_or_default();
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
        origin_workspace_id,
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
        first_sync_served: false,
        mesh_primitives: vec!["team_pending_invites.insert", "eeteam1"],
    })
}

/// Reload a pending invite locator without re-emitting the secret.
pub fn resume_pending_invite(
    connection: &DbConnection,
    invite_id: &str,
) -> Result<TeamInviteReport, OriginStreamError> {
    let invite = connection
        .get_team_pending_invite(invite_id)
        .map_err(|error| OriginStreamError::Db(error.to_string()))?
        .ok_or_else(|| OriginStreamError::Encode("unknown invite".to_owned()))?;
    if invite.status != "pending" {
        return Err(OriginStreamError::Encode(
            "invite is not pending".to_owned(),
        ));
    }
    Ok(TeamInviteReport {
        schema: TEAM_INVITE_SCHEMA_V1,
        command: "team invite",
        invite_id: invite.invite_id,
        team_id: invite.team_id,
        hello_port: invite.hello_port,
        endpoint: invite.endpoint,
        expires_at: invite.expires_at,
        invite_code: String::new(),
        granted: None,
        first_sync_served: false,
        mesh_primitives: vec!["team_pending_invites", "team_join_attempts"],
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
    let (mut stream, joiner_addr) = listener
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
    // Grant is already on the wire. Enroll must still succeed so the
    // inviter can hello-responder / EventFetch back; do not swallow FK
    // or path-id failures.
    enroll_joiner_from_accept(
        connection,
        workspace_id,
        &granted.team_id,
        &prove.joiner_node_id,
        &prove.joiner_display_name,
        joiner_addr,
        &hello.joiner_workspace_id,
        hello.joiner_hello_port,
        &redeemed_at,
    )?;
    Ok(granted)
}

/// Accept one unsigned hello+sync so the joiner's first metadata round
/// can complete after `ee team invite --wait` redeems.
pub fn serve_one_invite_first_sync(
    connection: &DbConnection,
    workspace_id: &str,
    listener: &std::net::TcpListener,
    timeout: std::time::Duration,
) -> Result<u32, OriginStreamError> {
    let (mut stream, _) = accept_one_with_timeout(listener, timeout)?;
    stream
        .set_read_timeout(Some(timeout))
        .and_then(|()| stream.set_write_timeout(Some(timeout)))
        .map_err(|error| OriginStreamError::Encode(format!("first-sync timeout: {error}")))?;
    let hello_bytes = read_std_framed(&mut stream)
        .map_err(|error| OriginStreamError::Encode(error.to_string()))?;
    let envelope = decode_envelope(&hello_bytes)
        .map_err(|error| OriginStreamError::Encode(error.to_string()))?;
    if envelope.capability != BootstrapCapability::Hello {
        return Err(OriginStreamError::Encode(
            "invite first-sync expected hello capability".to_owned(),
        ));
    }
    let request_id = envelope
        .payload
        .get("requestId")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("team-join-first-sync")
        .to_owned();
    let team = load_local_teams(connection)?
        .into_iter()
        .next()
        .ok_or_else(|| OriginStreamError::Encode("no local team genesis".to_owned()))?;
    let hello = crate::mesh::hello::HelloResponse {
        schema: crate::mesh::hello::HELLO_RESPONSE_SCHEMA_V1,
        request_id,
        responder_node_key: format!("nodekey:{}", team.origin_node_id),
        responder_ee_version: env!("CARGO_PKG_VERSION").to_owned(),
        responder_ee_protocol_version: crate::mesh::hello::local_protocol_version_string(),
        responder_workspace_ids: vec![workspace_id.to_owned()],
        responder_capabilities: vec!["hello".to_owned(), "sync".to_owned()],
        responder_advertised_tags: Vec::new(),
        discovery_consent: true,
        response_elapsed_micros: 0,
    };
    let hello_payload = serde_json::to_value(&hello)
        .map_err(|error| OriginStreamError::Encode(error.to_string()))?;
    let hello_reply = encode_envelope(BootstrapCapability::Hello, hello_payload)
        .map_err(|error| OriginStreamError::Encode(error.to_string()))?;
    write_std_framed(&mut stream, &hello_reply)
        .map_err(|error| OriginStreamError::Encode(error.to_string()))?;
    let sync_bytes = read_std_framed(&mut stream)
        .map_err(|error| OriginStreamError::Encode(error.to_string()))?;
    let request = parse_sync_round_request(&sync_bytes).ok_or_else(|| {
        OriginStreamError::Encode("invite first-sync expected ee.mesh.sync_round.v1".to_owned())
    })?;
    let events = load_invite_first_sync_events(
        connection,
        workspace_id,
        &team.team_id,
        &team.origin_node_id,
        request.range_start_seq,
        request.max_events,
    )?;
    let served = u32::try_from(events.len()).unwrap_or(u32::MAX);
    let last_seq = events.last().map_or(0, |event| event.seq);
    let tip_hash = events.last().map(|event| event.event_hash.clone());
    let response = SyncRoundResponse {
        schema: SYNC_ROUND_SCHEMA_V1.to_owned(),
        tips: vec![SyncRoundTip {
            origin_node_id: team.origin_node_id,
            origin_workspace_id: workspace_id.to_owned(),
            last_seq,
            tip_event_hash: tip_hash,
        }],
        events,
    };
    let reply = serde_json::to_vec(&response)
        .map_err(|error| OriginStreamError::Encode(error.to_string()))?;
    write_std_framed(&mut stream, &reply)
        .map_err(|error| OriginStreamError::Encode(error.to_string()))?;
    Ok(served)
}

fn accept_one_with_timeout(
    listener: &std::net::TcpListener,
    timeout: std::time::Duration,
) -> Result<(std::net::TcpStream, std::net::SocketAddr), OriginStreamError> {
    let started = std::time::Instant::now();
    listener
        .set_nonblocking(true)
        .map_err(|error| OriginStreamError::Encode(format!("first-sync listen: {error}")))?;
    loop {
        match listener.accept() {
            Ok((stream, addr)) => {
                let _ = listener.set_nonblocking(false);
                let _ = stream.set_nonblocking(false);
                return Ok((stream, addr));
            }
            Err(error)
                if error.kind() == std::io::ErrorKind::WouldBlock
                    || error.kind() == std::io::ErrorKind::Interrupted =>
            {
                if started.elapsed() >= timeout {
                    let _ = listener.set_nonblocking(false);
                    return Err(OriginStreamError::Encode(
                        "first-sync accept timeout".to_owned(),
                    ));
                }
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            Err(error) => {
                let _ = listener.set_nonblocking(false);
                return Err(OriginStreamError::Encode(format!(
                    "first-sync accept: {error}"
                )));
            }
        }
    }
}

fn load_invite_first_sync_events(
    connection: &DbConnection,
    workspace_id: &str,
    team_id: &str,
    origin_node_id: &str,
    range_start_seq: u64,
    max_events: u32,
) -> Result<Vec<SyncRoundEvent>, OriginStreamError> {
    let limit = max_events.max(1).min(512);
    let rows = connection
        .list_mesh_origin_events(team_id, origin_node_id, range_start_seq, limit)
        .map_err(|error| OriginStreamError::Db(error.to_string()))?;
    Ok(rows
        .into_iter()
        .map(|row| {
            let payload_json = inbound_from_stored(&row)
                .ok()
                .and_then(|event| serde_json::to_string(&event).ok())
                .unwrap_or(row.payload_json);
            SyncRoundEvent {
                origin_node_id: row.origin_node_id,
                origin_workspace_id: workspace_id.to_owned(),
                seq: row.seq,
                event_hash: row.event_hash,
                payload_json,
            }
        })
        .collect())
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

/// Pair-key handle shared by join, grants, and BodyFetch.
#[must_use]
pub fn team_pair_peer_handle(team_id: &str, remote_node_id: &str) -> String {
    let digest = blake3::hash(format!("{team_id}:{remote_node_id}").as_bytes());
    format!("peer_{}", &digest.to_hex().as_str()[..32])
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
    let handle = team_pair_peer_handle(team_id, remote_node_id);
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

/// Revoke every pending invite created before the authorization floor.
pub fn revoke_team_invites_before_floor(
    connection: &DbConnection,
    produced_at: &str,
) -> Result<usize, OriginStreamError> {
    let team = load_local_teams(connection)?
        .into_iter()
        .next()
        .ok_or_else(|| OriginStreamError::Encode("no local team genesis".to_owned()))?;
    let Some(floor_at) = connection
        .team_invite_auth_floor(&team.team_id)
        .map_err(|error| OriginStreamError::Db(error.to_string()))?
    else {
        return Ok(0);
    };
    let invites = connection
        .list_team_pending_invites(&team.team_id)
        .map_err(|error| OriginStreamError::Db(error.to_string()))?;
    let mut revoked = 0_usize;
    for invite in invites {
        if invite.status != "pending" || invite.created_at.as_str() >= floor_at.as_str() {
            continue;
        }
        if revoke_team_invite(connection, &invite.invite_id, produced_at)? {
            revoked = revoked.saturating_add(1);
        }
    }
    Ok(revoked)
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
        origin_workspace_id: self_workspace_id(connection)?.unwrap_or_default(),
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
            project_binding: project_binding_for_workspace(connection, &team.team_id, workspace_id),
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
        nonce_hex: None,
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
        Ok(Some(bytes)) => {
            let actual_hash = format!("blake3:{}", blake3::hash(&bytes).to_hex());
            let hash_ok = row.local_body_hash.as_deref() == Some(actual_hash.as_str());
            let size_ok = row
                .size_bytes
                .is_none_or(|expected| u64::try_from(bytes.len()).ok() == Some(expected));
            if !hash_ok || !size_ok {
                return Ok(empty("metadata_only", row.size_bytes.unwrap_or(0)));
            }
            Ok(BodyFetchResponse {
                schema: BODY_FETCH_RESPONSE_SCHEMA_V1.to_owned(),
                body_cache_key: body_cache_key.to_owned(),
                cache_status: "available".to_owned(),
                size_bytes: u64::try_from(bytes.len()).unwrap_or(0),
                body_hex: Some(hex_encode(&bytes)),
                nonce_hex: nonce_hex_for_body_row(connection, &row),
            })
        }
        Ok(None) | Err(_) => Ok(empty("metadata_only", row.size_bytes.unwrap_or(0))),
    }
}

/// Remote BodyFetch is allowed only when the requester's durable grant
/// explicitly allows the body lane. Missing or deny/quarantine stays
/// metadata-only.
#[must_use]
pub fn body_lane_allows_fetch(
    connection: &DbConnection,
    workspace_id: &str,
    peer_id: &str,
) -> bool {
    connection
        .get_mesh_lane_grant_state(workspace_id, peer_id)
        .ok()
        .flatten()
        .is_some_and(|grant| {
            grant.target_matches_current_peer
                && matches!(
                    grant.body_override,
                    Some(crate::config::MeshLaneDecision::Allow)
                )
        })
}

fn nonce_hex_for_body_row(
    connection: &DbConnection,
    row: &crate::db::StoredMeshBodyCacheMetadata,
) -> Option<String> {
    let event_id = row
        .body_ref_json
        .as_deref()
        .and_then(|json| serde_json::from_str::<serde_json::Value>(json).ok())
        .and_then(|value| {
            value
                .get("originEventId")
                .and_then(serde_json::Value::as_str)
                .map(ToOwned::to_owned)
        })?;
    connection.mesh_origin_event_nonce(&event_id).ok().flatten()
}

fn inbound_body_is_fetchable(representation: Option<&str>) -> bool {
    matches!(representation, Some("exact" | "already_redacted"))
}

fn record_inbound_body_placeholder(
    connection: &DbConnection,
    workspace_id: &str,
    inbound: &InboundOriginEvent,
    payload: &MemoryEventPayload,
    local_memory_id: &str,
) -> Result<(), OriginStreamError> {
    if !inbound_body_is_fetchable(payload.body_representation.as_deref()) {
        return Ok(());
    }
    if !payload.logical_memory_id.starts_with("mem_") || payload.logical_memory_id.len() <= 6 {
        return Ok(());
    }
    if !payload.body_commitment.starts_with("blake3:") {
        return Ok(());
    }
    let key = team_body_cache_key(&payload.logical_memory_id);
    if connection
        .get_mesh_body_cache_metadata(workspace_id, &key)
        .map_err(|error| OriginStreamError::Db(error.to_string()))?
        .is_some()
    {
        return Ok(());
    }
    connection
        .upsert_mesh_body_cache_metadata(&UpsertMeshBodyCacheMetadataInput {
            workspace_id: workspace_id.to_owned(),
            body_cache_key: key,
            origin_node_id: inbound.origin_node_id.clone(),
            origin_workspace_id: workspace_id.to_owned(),
            logical_memory_id: payload.logical_memory_id.clone(),
            content_hash: payload.body_commitment.clone(),
            body_ref_json: Some(
                serde_json::json!({
                    "originEventId": inbound.event_id,
                    "localMemoryId": local_memory_id,
                })
                .to_string(),
            ),
            preview_hash: None,
            size_bytes: None,
            cache_status: "metadata_only".to_owned(),
            local_body_hash: None,
            cached_at: Some(inbound.produced_at.clone()),
            expires_at: None,
        })
        .map_err(|error| OriginStreamError::Db(error.to_string()))?;
    Ok(())
}

fn body_ref_field(row: &crate::db::StoredMeshBodyCacheMetadata, field: &str) -> Option<String> {
    row.body_ref_json
        .as_deref()
        .and_then(|json| serde_json::from_str::<serde_json::Value>(json).ok())
        .and_then(|value| {
            value
                .get(field)
                .and_then(serde_json::Value::as_str)
                .map(ToOwned::to_owned)
        })
}

fn local_inbound_memory_id_for_body_row(
    connection: &DbConnection,
    workspace_id: &str,
    row: &crate::db::StoredMeshBodyCacheMetadata,
) -> Option<String> {
    if let Some(local_id) =
        body_ref_field(row, "localMemoryId").filter(|id| id.starts_with("mem_") && id.len() == 30)
    {
        return Some(local_id);
    }
    let event_id = body_ref_field(row, "originEventId")?;
    connection
        .list_memories(workspace_id, None, false)
        .ok()?
        .into_iter()
        .find(|memory| {
            memory.trust_class == "peer_human_attested"
                && memory.provenance_uri.as_deref() == Some(event_id.as_str())
        })
        .map(|memory| memory.id)
}

/// After an authorized body lands in cache, replace the metadata stub so
/// `ee search --memory-scope team` / `ee pack --memory-scope team` can
/// recall the teammate's text. Non-UTF-8 or non-stub rows stay metadata-only.
fn hydrate_inbound_team_memory_body(
    connection: &DbConnection,
    workspace_id: &str,
    row: &crate::db::StoredMeshBodyCacheMetadata,
    bytes: &[u8],
) -> Result<(), OriginStreamError> {
    let Ok(text) = std::str::from_utf8(bytes) else {
        return Ok(());
    };
    let Some(local_id) = local_inbound_memory_id_for_body_row(connection, workspace_id, row) else {
        return Ok(());
    };
    let Some(existing) = connection
        .get_memory(&local_id)
        .map_err(|error| OriginStreamError::Db(error.to_string()))?
    else {
        return Ok(());
    };
    if existing.workspace_id != workspace_id
        || existing.trust_class != "peer_human_attested"
        || !existing.content.starts_with("[ee.team.history]")
    {
        return Ok(());
    }
    connection
        .apply_memory_curation_update(
            &local_id,
            &crate::db::ApplyMemoryCurationInput {
                workspace_id: workspace_id.to_owned(),
                content: text.to_owned(),
                confidence: existing.confidence,
                trust_class: existing.trust_class,
            },
        )
        .map_err(|error| OriginStreamError::Db(error.to_string()))?;
    enqueue_inbound_memory_index_job(connection, workspace_id, &local_id, "team-inbound-body")
}

/// Drain coalesced inbound Incremental jobs so `--memory-scope team`
/// search/pack can see rematerialized or hydrated teammate text.
fn drain_team_inbound_search_index(
    connection: &DbConnection,
    workspace_id: &str,
    workspace_path: &std::path::Path,
) -> Result<usize, OriginStreamError> {
    let index_dir = workspace_path.join(".ee").join("index");
    crate::core::index::process_pending_index_jobs_coalesced(
        connection,
        workspace_id,
        &index_dir,
        None,
    )
    .map(|reports| reports.len())
    .map_err(|error| OriginStreamError::Encode(error.to_string()))
}

/// Verify an authorized BodyFetch against the inbound placeholder commitment
/// and publish bytes through staging → available. Wrong nonce or body stays
/// metadata-only or quarantined; the nonce is not persisted.
pub fn apply_fetched_team_body(
    connection: &DbConnection,
    workspace_id: &str,
    workspace_path: &std::path::Path,
    fetched: &crate::mesh::bootstrap_envelope::BodyFetchResponse,
) -> Result<crate::mesh::bootstrap_envelope::BodyFetchResponse, OriginStreamError> {
    use crate::mesh::bootstrap_envelope::{BODY_FETCH_RESPONSE_SCHEMA_V1, BodyFetchResponse};
    let unchanged = fetched.clone();
    if fetched.cache_status != "available" {
        return Ok(unchanged);
    }
    let Some(body_hex) = fetched.body_hex.as_deref() else {
        return Ok(unchanged);
    };
    let Some(nonce_hex) = fetched.nonce_hex.as_deref() else {
        return Ok(unchanged);
    };
    let Some(bytes) = hex_decode(body_hex) else {
        return Ok(unchanged);
    };
    let Some(nonce) = hex_decode(nonce_hex).and_then(|bytes| <[u8; 32]>::try_from(bytes).ok())
    else {
        return Ok(unchanged);
    };
    let Some(row) = connection
        .get_mesh_body_cache_metadata(workspace_id, &fetched.body_cache_key)
        .map_err(|error| OriginStreamError::Db(error.to_string()))?
    else {
        return Ok(unchanged);
    };
    if row.cache_status == "available" {
        // Retry/upgrade: the cache may already be published while the
        // inbound stub is still "[ee.team.history] …". Hydrate from the
        // local bytes so search/pack can recall teammate text without a
        // second BodyFetch.
        hydrate_available_inbound_team_body(connection, workspace_id, workspace_path, &row)?;
        return fetch_local_team_body(
            connection,
            workspace_id,
            workspace_path,
            &fetched.body_cache_key,
        );
    }
    let expected = body_commitment(&nonce, &bytes);
    if expected != row.content_hash {
        connection
            .upsert_mesh_body_cache_metadata(&UpsertMeshBodyCacheMetadataInput {
                workspace_id: row.workspace_id,
                body_cache_key: row.body_cache_key,
                origin_node_id: row.origin_node_id,
                origin_workspace_id: row.origin_workspace_id,
                logical_memory_id: row.logical_memory_id,
                content_hash: row.content_hash,
                body_ref_json: row.body_ref_json,
                preview_hash: row.preview_hash,
                size_bytes: row.size_bytes,
                cache_status: "quarantined".to_owned(),
                local_body_hash: None,
                cached_at: Some(chrono::Utc::now().to_rfc3339()),
                expires_at: row.expires_at,
            })
            .map_err(|error| OriginStreamError::Db(error.to_string()))?;
        return Ok(BodyFetchResponse {
            schema: BODY_FETCH_RESPONSE_SCHEMA_V1.to_owned(),
            body_cache_key: fetched.body_cache_key.clone(),
            cache_status: "quarantined".to_owned(),
            size_bytes: row.size_bytes.unwrap_or(0),
            body_hex: None,
            nonce_hex: None,
        });
    }
    crate::mesh::key_store::require_mesh_credential_store_platform("publish fetched team body")
        .map_err(|error| {
            OriginStreamError::Encode(format!(
                "{}: {error}",
                crate::mesh::cache::MESH_BODY_CACHE_LIFECYCLE_FAILED_CODE
            ))
        })?;
    let cache_dir = workspace_path.join(".ee").join("mesh-body-cache");
    let cache = crate::mesh::key_store::SecureLocalDir::open_or_create(workspace_path, &cache_dir)
        .map_err(|error| OriginStreamError::Encode(error.to_string()))?;
    let local_body_hash = format!("blake3:{}", blake3::hash(&bytes).to_hex());
    let mut meta = UpsertMeshBodyCacheMetadataInput {
        workspace_id: row.workspace_id.clone(),
        body_cache_key: row.body_cache_key.clone(),
        origin_node_id: row.origin_node_id.clone(),
        origin_workspace_id: row.origin_workspace_id.clone(),
        logical_memory_id: row.logical_memory_id.clone(),
        content_hash: row.content_hash.clone(),
        body_ref_json: row.body_ref_json.clone(),
        preview_hash: row.preview_hash.clone(),
        size_bytes: Some(u64::try_from(bytes.len()).unwrap_or(0)),
        cache_status: "staging".to_owned(),
        local_body_hash: Some(local_body_hash),
        cached_at: Some(chrono::Utc::now().to_rfc3339()),
        expires_at: row.expires_at.clone(),
    };
    connection
        .upsert_mesh_body_cache_metadata(&meta)
        .map_err(|error| OriginStreamError::Db(error.to_string()))?;
    cache
        .write_replace(&row.body_cache_key, &bytes)
        .map_err(|error| OriginStreamError::Encode(error.to_string()))?;
    meta.cache_status = "available".to_owned();
    connection
        .upsert_mesh_body_cache_metadata(&meta)
        .map_err(|error| OriginStreamError::Db(error.to_string()))?;
    hydrate_inbound_team_memory_body(connection, workspace_id, &row, &bytes)?;
    let _ = drain_team_inbound_search_index(connection, workspace_id, workspace_path);
    fetch_local_team_body(
        connection,
        workspace_id,
        workspace_path,
        &fetched.body_cache_key,
    )
}

/// Hydrate a still-stub inbound memory from an already-published cache row.
fn hydrate_available_inbound_team_body(
    connection: &DbConnection,
    workspace_id: &str,
    workspace_path: &std::path::Path,
    row: &crate::db::StoredMeshBodyCacheMetadata,
) -> Result<(), OriginStreamError> {
    let local = fetch_local_team_body(
        connection,
        workspace_id,
        workspace_path,
        &row.body_cache_key,
    )?;
    let Some(bytes) = local.body_hex.as_deref().and_then(hex_decode) else {
        return Ok(());
    };
    hydrate_inbound_team_memory_body(connection, workspace_id, row, &bytes)?;
    let _ = drain_team_inbound_search_index(connection, workspace_id, workspace_path);
    Ok(())
}

/// Walk available inbound cache rows and hydrate leftover history stubs.
/// Covers the upgrade path after a node already published bytes under the
/// pre-hydrate apply path.
fn rematerialize_available_inbound_team_bodies(
    connection: &DbConnection,
    workspace_id: &str,
    workspace_path: &std::path::Path,
) -> Result<usize, OriginStreamError> {
    let rows = connection
        .list_mesh_body_cache_metadata(workspace_id)
        .map_err(|error| OriginStreamError::Db(error.to_string()))?;
    let mut attempted = 0_usize;
    for row in rows {
        if row.cache_status != "available" {
            continue;
        }
        hydrate_available_inbound_team_body(connection, workspace_id, workspace_path, &row)?;
        attempted = attempted.saturating_add(1);
    }
    Ok(attempted)
}

/// Body-cache keys waiting on an authorized fetch. Filesystem presence is
/// not authority; only `metadata_only` rows are returned.
pub fn pending_team_body_fetch_keys(
    connection: &DbConnection,
    workspace_id: &str,
) -> Result<Vec<String>, OriginStreamError> {
    Ok(connection
        .list_mesh_body_cache_metadata(workspace_id)
        .map_err(|error| OriginStreamError::Db(error.to_string()))?
        .into_iter()
        .filter(|row| row.cache_status == "metadata_only")
        .map(|row| row.body_cache_key)
        .collect())
}

/// Enroll the remote join peer under the same handle as the pair key so
/// sync/BodyFetch can find an endpoint after the ceremony.
///
/// Also persists the remote human as an active `team_members` row. Team
/// scope reads that table; a mesh peer alone cannot admit teammate text.
pub fn enroll_team_pair_peer(
    connection: &DbConnection,
    workspace_id: &str,
    team_id: &str,
    remote_node_id: &str,
    display_name: &str,
    endpoint: &str,
    hello_port: u16,
    produced_at: &str,
    origin_workspace_id: &str,
) -> Result<String, OriginStreamError> {
    let peer_id = team_pair_peer_handle(team_id, remote_node_id);
    persist_team_member(
        connection,
        workspace_id,
        team_id,
        remote_node_id,
        display_name,
        false,
        "invite_ceremony",
        produced_at,
        None,
    )?;
    if connection
        .get_mesh_peer(workspace_id, &peer_id)
        .map_err(|error| OriginStreamError::Db(error.to_string()))?
        .is_some()
    {
        return Ok(peer_id);
    }
    let locator = if endpoint.contains(':') {
        endpoint.to_owned()
    } else {
        format!("{endpoint}:{hello_port}")
    };
    let record = crate::mesh::peer::MeshPeerRecord {
        schema: crate::mesh::peer::MESH_PEER_RECORD_SCHEMA_V1.to_owned(),
        peer_id: peer_id.clone(),
        alias: display_name.to_owned(),
        workspace_id: workspace_id.to_owned(),
        origin_workspace_id: origin_workspace_id.to_owned(),
        endpoint: crate::mesh::peer::MeshPeerEndpoint {
            tailscale_node_key: remote_node_id.to_owned(),
            tailnet_id: TEAM_JOIN_TAILNET_ID.to_owned(),
            tailnet_display_name: None,
            endpoint: locator,
            magic_dns_name: None,
        },
        materialized_on_node_key: None,
        capabilities: crate::mesh::peer::MeshPeerCapabilities::from_profile(
            crate::mesh::peer::MeshPeerCapabilityProfile::BodyAllowed,
        ),
        handshake: crate::mesh::peer::MeshPeerHandshake::granted(
            "team-join",
            "1.0",
            remote_node_id,
            vec!["metadata".to_owned(), "body".to_owned()],
        ),
        key: crate::mesh::peer::MeshPeerKey {
            generation: 1,
            public_key_fingerprint: format!("blake3:{}", blake3::hash(peer_id.as_bytes()).to_hex()),
            created_at: produced_at.to_owned(),
            rotated_at: None,
            revoked_at: None,
        },
        state: crate::mesh::peer::MeshPeerState::Active,
        enrolled_at: produced_at.to_owned(),
        revoked_at: None,
        trust_established_by: "explicit_human_consent".to_owned(),
    };
    let policy_summary_json = serde_json::to_string(&record)
        .map_err(|error| OriginStreamError::Encode(error.to_string()))?;
    connection
        .upsert_mesh_peer(&UpsertMeshPeerInput {
            workspace_id: workspace_id.to_owned(),
            peer_id: peer_id.clone(),
            origin_node_id: remote_node_id.to_owned(),
            display_name: Some(display_name.to_owned()),
            policy_summary_json: Some(policy_summary_json),
            enabled: true,
            last_seen_at: Some(produced_at.to_owned()),
        })
        .map_err(|error| OriginStreamError::Db(error.to_string()))?;
    Ok(peer_id)
}

/// Enroll the accepted joiner using the join TCP source IP and the joiner's
/// advertised hello port so the inviter can EventFetch/BodyFetch back.
pub fn enroll_joiner_from_accept(
    connection: &DbConnection,
    workspace_id: &str,
    team_id: &str,
    joiner_node_id: &str,
    joiner_display_name: &str,
    joiner_addr: std::net::SocketAddr,
    joiner_workspace_id: &str,
    joiner_hello_port: u16,
    produced_at: &str,
) -> Result<String, OriginStreamError> {
    let hello_port = if joiner_hello_port >= 1024 {
        joiner_hello_port
    } else {
        configured_hello_port()
    };
    enroll_team_pair_peer(
        connection,
        workspace_id,
        team_id,
        joiner_node_id,
        joiner_display_name,
        &joiner_addr.ip().to_string(),
        hello_port,
        produced_at,
        joiner_workspace_id,
    )
}

/// Team-join enroll stores [`TEAM_JOIN_TAILNET_ID`] until LocalAPI observes
/// the real tailnet. Resolve treats that placeholder as the current tailnet.
#[must_use]
pub fn team_join_tailnet_matches(peer_tailnet: &str, local_tailnet: &str) -> bool {
    peer_tailnet == local_tailnet || peer_tailnet == TEAM_JOIN_TAILNET_ID
}

/// Team-join stores `team-join-<ee-node>` until LocalAPI observes the real
/// Tailscale stable ID. Treat that placeholder as compatible with any
/// observed ID so `hello-responder run` can bind on a live tailnet.
#[must_use]
pub fn team_join_stable_id_matches(stored: &str, observed: &str) -> bool {
    stored == observed || stored.starts_with("team-join-")
}

/// Team-join stores `nodekey:<ee-node-id>` until LocalAPI observes the
/// Tailscale node key. Same placeholder rule as [`team_join_stable_id_matches`].
#[must_use]
pub fn team_join_node_pubkey_matches(stored: &str, observed: &str) -> bool {
    stored == observed || stored.starts_with("nodekey:node_")
}

/// Team-join handshake is enough for inbound EventFetch. BodyFetch still
/// requires a durable Body-lane Allow at serve time.
#[must_use]
pub fn team_join_allows_ungranted_route(policy: &crate::mesh::peer::MeshPeerRecord) -> bool {
    policy.handshake.granted
        && policy.handshake.discovery_consent
        && policy.handshake.request_id == "team-join"
}

/// Grant-gated BodyFetch retry. `fetch` is called only for peers whose
/// durable body lane is Allow. Applied counts available publications.
pub fn retry_pending_team_body_fetches(
    connection: &DbConnection,
    workspace_id: &str,
    workspace_path: &std::path::Path,
    mut fetch: impl FnMut(&str, &str) -> Option<crate::mesh::bootstrap_envelope::BodyFetchResponse>,
) -> Result<usize, OriginStreamError> {
    let keys = pending_team_body_fetch_keys(connection, workspace_id)?;
    if keys.is_empty() {
        return Ok(0);
    }
    let granted = connection
        .list_mesh_peers(workspace_id)
        .map_err(|error| OriginStreamError::Db(error.to_string()))?
        .into_iter()
        .filter(|peer| {
            peer.enabled && body_lane_allows_fetch(connection, workspace_id, &peer.peer_id)
        })
        .map(|peer| peer.peer_id)
        .collect::<Vec<_>>();
    let mut applied = 0_usize;
    for key in keys {
        for peer_id in &granted {
            let Some(fetched) = fetch(peer_id, &key) else {
                continue;
            };
            let published =
                apply_fetched_team_body(connection, workspace_id, workspace_path, &fetched)?;
            if published.cache_status == "available" {
                applied = applied.saturating_add(1);
                break;
            }
        }
    }
    Ok(applied)
}

/// Session binding for BodyFetch. Distinct workspaces and nodes are required
/// so initiator and responder cannot collide on the handshake.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TeamBodyFetchBinding {
    pub team_id: String,
    pub tailnet_id: String,
    pub initiator_node_id: String,
    pub responder_node_id: String,
    pub initiator_workspace_id: String,
    pub responder_workspace_id: String,
}

#[must_use]
pub fn plan_team_body_fetch_binding(
    local_workspace_id: &str,
    local_origin_node_id: &str,
    team_id: &str,
    remote_node_id: &str,
    remote_workspace_id: &str,
    tailnet_id: &str,
) -> Option<TeamBodyFetchBinding> {
    if local_workspace_id.is_empty()
        || remote_workspace_id.is_empty()
        || local_workspace_id == remote_workspace_id
        || local_origin_node_id.is_empty()
        || remote_node_id.is_empty()
        || local_origin_node_id == remote_node_id
        || team_id.is_empty()
        || tailnet_id.is_empty()
    {
        return None;
    }
    Some(TeamBodyFetchBinding {
        team_id: team_id.to_owned(),
        tailnet_id: tailnet_id.to_owned(),
        initiator_node_id: local_origin_node_id.to_owned(),
        responder_node_id: remote_node_id.to_owned(),
        initiator_workspace_id: local_workspace_id.to_owned(),
        responder_workspace_id: remote_workspace_id.to_owned(),
    })
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
    share_team_bodies_represented(
        connection,
        workspace_id,
        produced_at,
        confirm,
        limit,
        workspace_path,
        issue_token,
        approval_token,
        "exact",
    )
}

/// Publish origin-owned bodies with an explicit signed representation.
/// `already_redacted` is allowed; switching an `exact` publication to
/// `already_redacted` is refused so redact-over-exact cannot masquerade.
pub fn share_team_bodies_represented(
    connection: &DbConnection,
    workspace_id: &str,
    produced_at: &str,
    confirm: bool,
    limit: usize,
    workspace_path: Option<&std::path::Path>,
    issue_token: bool,
    approval_token: Option<&str>,
    representation: &str,
) -> Result<TeamBodyShareReport, OriginStreamError> {
    if representation != "exact" && representation != "already_redacted" {
        return Err(OriginStreamError::Encode(
            "body representation must be exact or already_redacted".to_owned(),
        ));
    }
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
    if confirm {
        crate::mesh::key_store::require_mesh_credential_store_platform("publish team body cache")
            .map_err(|error| {
            OriginStreamError::Encode(format!(
                "{}: {error}",
                crate::mesh::cache::MESH_BODY_CACHE_LIFECYCLE_FAILED_CODE
            ))
        })?;
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
            representation: representation.to_owned(),
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
            if representation == "already_redacted" {
                return Err(OriginStreamError::Encode(
                    "refuse redact-over-exact; unshare and preview a fresh already_redacted set"
                        .to_owned(),
                ));
            }
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
            project_binding: project_binding_for_workspace(connection, &team.team_id, workspace_id),
            origin_trust_claim: Some(memory.trust_class.clone()),
            provenance_refs: Vec::new(),
            body_representation: Some(representation.to_owned()),
            redaction_provenance: (representation == "already_redacted")
                .then(|| "origin_already_redacted".to_owned()),
            body_commitment: commitment.clone(),
        });
        let appended = append_origin_event(
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
        let local_body_hash = format!(
            "blake3:{}",
            blake3::hash(memory.content.as_bytes()).to_hex()
        );
        let mut meta = UpsertMeshBodyCacheMetadataInput {
            workspace_id: workspace_id.to_owned(),
            body_cache_key: key.clone(),
            origin_node_id: team.origin_node_id.clone(),
            origin_workspace_id: workspace_id.to_owned(),
            logical_memory_id: memory.id.clone(),
            content_hash: commitment,
            body_ref_json: Some(
                serde_json::json!({ "originEventId": appended.event_id }).to_string(),
            ),
            preview_hash: None,
            size_bytes: Some(u64::try_from(memory.content.len()).unwrap_or(0)),
            cache_status: "staging".to_owned(),
            local_body_hash: Some(local_body_hash),
            cached_at: Some(produced_at.to_owned()),
            expires_at: None,
        };
        connection
            .upsert_mesh_body_cache_metadata(&meta)
            .map_err(|error| OriginStreamError::Db(error.to_string()))?;
        cache
            .write_replace(&key, memory.content.as_bytes())
            .map_err(|error| OriginStreamError::Encode(error.to_string()))?;
        meta.cache_status = "available".to_owned();
        connection
            .upsert_mesh_body_cache_metadata(&meta)
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
        representation: representation.to_owned(),
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
    issue_body_share_token_at(
        workspace_path,
        workspace_id,
        consent_hash,
        chrono::Utc::now().timestamp(),
    )
}

fn issue_body_share_token_at(
    workspace_path: &std::path::Path,
    workspace_id: &str,
    consent_hash: &str,
    now_unix_seconds: i64,
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
        now_unix_seconds,
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
        if existing.cache_status != "available"
            && existing.cache_status != "staging"
            && existing.cache_status != "invalidated_pending_purge"
        {
            items.push(TeamBodyShareItem {
                memory_id: memory.id,
                revision_id,
                size_bytes: existing.size_bytes.unwrap_or(0),
                cache_status: existing.cache_status,
            });
            continue;
        }
        let mut meta = UpsertMeshBodyCacheMetadataInput {
            workspace_id: workspace_id.to_owned(),
            body_cache_key: key,
            origin_node_id: existing.origin_node_id,
            origin_workspace_id: existing.origin_workspace_id,
            logical_memory_id: existing.logical_memory_id,
            content_hash: existing.content_hash,
            body_ref_json: existing.body_ref_json,
            preview_hash: existing.preview_hash,
            size_bytes: existing.size_bytes,
            cache_status: "invalidated_pending_purge".to_owned(),
            local_body_hash: existing.local_body_hash,
            cached_at: Some(produced_at.to_owned()),
            expires_at: existing.expires_at,
        };
        connection
            .upsert_mesh_body_cache_metadata(&meta)
            .map_err(|error| OriginStreamError::Db(error.to_string()))?;
        meta.cache_status = "evicted".to_owned();
        connection
            .upsert_mesh_body_cache_metadata(&meta)
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
        representation: "withdrawn".to_owned(),
        approval_token: None,
        mesh_primitives: vec!["mesh_body_cache_metadata"],
    })
}

/// Fold crash-retained body-cache rows. Filesystem presence is never authority.
pub fn reconcile_team_body_cache(
    connection: &DbConnection,
    workspace_id: &str,
    workspace_path: Option<&std::path::Path>,
    produced_at: &str,
) -> Result<usize, OriginStreamError> {
    let rows = connection
        .list_mesh_body_cache_metadata(workspace_id)
        .map_err(|error| OriginStreamError::Db(error.to_string()))?;
    let cache = workspace_path.and_then(|path| {
        let cache_dir = path.join(".ee").join("mesh-body-cache");
        crate::mesh::key_store::SecureLocalDir::open_existing(path, &cache_dir)
            .ok()
            .flatten()
    });
    let mut changed = 0_usize;
    for row in rows {
        let next = match row.cache_status.as_str() {
            "invalidated_pending_purge" => Some("evicted"),
            "staging" => {
                let present = cache
                    .as_ref()
                    .and_then(|dir| dir.read(&row.body_cache_key).ok().flatten())
                    .is_some();
                Some(if present {
                    "available"
                } else {
                    "metadata_only"
                })
            }
            "available" => {
                let present = cache
                    .as_ref()
                    .and_then(|dir| dir.read(&row.body_cache_key).ok().flatten())
                    .is_some();
                if workspace_path.is_some() && !present {
                    Some("metadata_only")
                } else {
                    None
                }
            }
            _ => None,
        };
        let Some(next) = next else {
            continue;
        };
        if next == row.cache_status {
            continue;
        }
        connection
            .upsert_mesh_body_cache_metadata(&UpsertMeshBodyCacheMetadataInput {
                workspace_id: row.workspace_id,
                body_cache_key: row.body_cache_key,
                origin_node_id: row.origin_node_id,
                origin_workspace_id: row.origin_workspace_id,
                logical_memory_id: row.logical_memory_id,
                content_hash: row.content_hash,
                body_ref_json: row.body_ref_json,
                preview_hash: row.preview_hash,
                size_bytes: row.size_bytes,
                cache_status: next.to_owned(),
                local_body_hash: row.local_body_hash,
                cached_at: Some(produced_at.to_owned()),
                expires_at: row.expires_at,
            })
            .map_err(|error| OriginStreamError::Db(error.to_string()))?;
        changed = changed.saturating_add(1);
    }
    Ok(changed)
}

fn self_workspace_id(connection: &DbConnection) -> Result<Option<String>, OriginStreamError> {
    Ok(connection
        .list_all_team_members()
        .map_err(|error| OriginStreamError::Db(error.to_string()))?
        .into_iter()
        .find(|member| member.is_self)
        .map(|member| member.workspace_id))
}

fn stalled_peer_cursor_count(
    connection: &DbConnection,
    workspace_id: &str,
) -> Result<usize, OriginStreamError> {
    Ok(connection
        .list_mesh_peer_cursors(workspace_id)
        .map_err(|error| OriginStreamError::Db(error.to_string()))?
        .into_iter()
        .filter(|cursor| matches!(cursor.status.as_str(), "behind" | "blocked" | "quarantined"))
        .count())
}

/// Decide whether one foreground steward pass should run mesh sync.
/// Read-only: doctor and status must not mutate membership or the body cache.
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
    let stalled_cursors = self_workspace_id(connection)?
        .map(|workspace_id| stalled_peer_cursor_count(connection, &workspace_id))
        .transpose()?
        .unwrap_or(0);
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
            applied_additions: 0,
            applied_removals: 0,
            stalled_cursors,
            deferred_pairings: 0,
            applied_pair_promotions: 0,
            mesh_primitives: vec!["team_posture", "steward_decision"],
        });
    }
    let identities = connection
        .list_team_member_identities(&team.team_id)
        .map_err(|error| OriginStreamError::Db(error.to_string()))?;
    let suspended_identities = identities
        .iter()
        .filter(|identity| identity.state == "suspended")
        .count();
    if suspended_identities > 0
        || worst_identity_timer(&identities) == Some(IdentityRevalidationPosture::Suspended)
    {
        return Ok(TeamStewardReport {
            schema: TEAM_STEWARD_SCHEMA_V1,
            command: "team steward run-once",
            team_id: team.team_id,
            paused: false,
            active_member_count,
            outcome: "no_op".to_owned(),
            reason: "identity_revalidation_failed".to_owned(),
            ran_sync: false,
            applied_additions: 0,
            applied_removals: 0,
            stalled_cursors,
            deferred_pairings: 0,
            applied_pair_promotions: 0,
            mesh_primitives: vec![
                "team_idp_policy",
                "team_member_identity",
                "steward_decision",
            ],
        });
    }
    let (drift_severity, drift_kind) = if active_member_count > 1 {
        (
            crate::mesh::steward_decision::DriftSeverity::Warning,
            crate::mesh::steward_decision::DriftKind::NewPeersAvailable,
        )
    } else if stalled_cursors > 0 {
        (
            crate::mesh::steward_decision::DriftSeverity::Warning,
            crate::mesh::steward_decision::DriftKind::StalePeersInConfig,
        )
    } else {
        (
            crate::mesh::steward_decision::DriftSeverity::None,
            crate::mesh::steward_decision::DriftKind::None,
        )
    };
    let decision = crate::mesh::steward_decision::decide_steward_outcome(
        &crate::mesh::steward_decision::StewardDecisionInput {
            enabled: true,
            drift_severity,
            drift_kind,
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
        applied_additions: 0,
        applied_removals: 0,
        stalled_cursors,
        deferred_pairings: 0,
        applied_pair_promotions: 0,
        mesh_primitives: vec!["team_members", "steward_decision", "mesh_sync"],
    })
}

/// Apply local steward repairs: membership fanout, body-cache lifecycle,
/// and crash-orphaned Next pair keys. Network sync stays with
/// `ee mesh sync --once` after this returns `ran_sync`.
pub fn execute_team_steward_once(
    connection: &DbConnection,
    workspace_path: Option<&std::path::Path>,
) -> Result<TeamStewardReport, OriginStreamError> {
    let mut report = plan_team_steward_once(connection)?;
    if report.paused || report.reason == "identity_revalidation_failed" {
        return Ok(report);
    }
    let Some(workspace_id) = self_workspace_id(connection)? else {
        return Ok(report);
    };
    let reconciled = reconcile_local_team_membership(connection, &workspace_id)?;
    let projects = reconcile_local_team_projects(connection)?;
    let memories = reconcile_inbound_team_memories(connection, &workspace_id)?;
    report.applied_additions = reconciled
        .applied_additions
        .saturating_add(projects.applied_additions)
        .saturating_add(memories.applied_additions);
    report.applied_removals = reconciled.applied_removals;
    reconcile_team_body_cache(
        connection,
        &workspace_id,
        None,
        &chrono::Utc::now().to_rfc3339(),
    )?;
    advance_team_removal_acknowledgements(
        connection,
        &workspace_id,
        &chrono::Utc::now().to_rfc3339(),
    )?;
    if let Some(path) = workspace_path {
        let (deferred, promoted) = retry_deferred_pairings(connection, &workspace_id, path)?;
        report.deferred_pairings = deferred;
        report.applied_pair_promotions = promoted;
        let _ = rematerialize_available_inbound_team_bodies(connection, &workspace_id, path);
        let _ = drain_team_inbound_search_index(connection, &workspace_id, path);
    }
    report.stalled_cursors = stalled_peer_cursor_count(connection, &workspace_id)?;
    report.mesh_primitives = vec![
        "team_members",
        "team_projects",
        "mesh_import_ledger",
        "mesh_body_cache_metadata",
        "team_removal_acknowledgements",
        "mesh_key_store",
        "steward_decision",
        "mesh_sync",
    ];
    Ok(report)
}

/// Promote a Next pair key when Current is missing (crash during rotation).
/// A staged Next beside a live Current stays deferred for the peer ceremony.
fn retry_deferred_pairings(
    _connection: &DbConnection,
    _workspace_id: &str,
    workspace_path: &std::path::Path,
) -> Result<(usize, usize), OriginStreamError> {
    let Ok(Some(store)) = crate::mesh::key_store::MeshKeyStore::open_existing(workspace_path)
    else {
        return Ok((0, 0));
    };
    let handles = store
        .list_next_pair_peer_handles()
        .map_err(|error| OriginStreamError::Encode(error.to_string()))?;
    let mut deferred = 0_usize;
    let mut promoted = 0_usize;
    for handle in handles {
        let next = store
            .load_pair_key(&handle, crate::mesh::key_store::PairKeyClass::Next)
            .map_err(|error| OriginStreamError::Encode(error.to_string()))?;
        let Some(next) = next else {
            continue;
        };
        deferred = deferred.saturating_add(1);
        let current = store
            .load_pair_key(&handle, crate::mesh::key_store::PairKeyClass::Current)
            .map_err(|error| OriginStreamError::Encode(error.to_string()))?;
        if current.is_some() {
            continue;
        }
        store
            .store_pair_key(
                &handle,
                crate::mesh::key_store::PairKeyClass::Current,
                next.generation,
                &crate::mesh::key_store::SecretBytes::new(*next.key.as_bytes()),
                &next.created_at,
                true,
            )
            .map_err(|error| OriginStreamError::Encode(error.to_string()))?;
        store
            .retire_pair_key(
                &handle,
                crate::mesh::key_store::PairKeyClass::Next,
                "steward-promote",
            )
            .map_err(|error| OriginStreamError::Encode(error.to_string()))?;
        promoted = promoted.saturating_add(1);
    }
    Ok((deferred, promoted))
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
    let limits = crate::mesh::admission::MeshAdmissionLimits::conservative_default();
    let admission = load_team_admission_status(connection, limits);
    checks.push(TeamDoctorCheck {
        name: "admission".to_owned(),
        status: if admission.coalesced_exhaustion {
            "warning".to_owned()
        } else {
            "ok".to_owned()
        },
        message: if admission.peer_snapshot_count == 0 {
            format!(
                "authenticated caps: event_batch<={} events/{} bytes; body_fetch<={} bytes; index_jobs<={}; local_tier1_unaffected={}; no peer snapshot",
                admission.max_event_batch_count,
                admission.max_event_batch_bytes,
                admission.max_body_fetch_bytes,
                admission.max_index_jobs_per_round,
                admission.local_tier1_unaffected
            )
        } else {
            format!(
                "authenticated caps: event_batch<={} events/{} bytes; body_fetch<={} bytes; index_jobs<={}; local_tier1_unaffected={}; {} peer snapshot(s); {} throttled; {} exhausted; coalesced_exhaustion={}",
                admission.max_event_batch_count,
                admission.max_event_batch_bytes,
                admission.max_body_fetch_bytes,
                admission.max_index_jobs_per_round,
                admission.local_tier1_unaffected,
                admission.peer_snapshot_count,
                admission.throttled_peer_count,
                admission.budget_exhausted_peer_count,
                admission.coalesced_exhaustion
            )
        },
        repair: admission.coalesced_exhaustion.then(|| {
            "wait for peer backoff, then retry a smaller EventFetch or BodyFetch".to_owned()
        }),
    });
    if let Some(path) = workspace_path {
        match workspace_available_bytes(path) {
            Some(available) if available < TEAM_FREE_SPACE_FLOOR_BYTES => {
                checks.push(TeamDoctorCheck {
                    name: "free_space".to_owned(),
                    status: "warning".to_owned(),
                    message: format!(
                        "{available} bytes free; floor is {TEAM_FREE_SPACE_FLOOR_BYTES}"
                    ),
                    repair: Some(
                        "free local disk before accepting peer event or body transfers".to_owned(),
                    ),
                });
            }
            Some(available) => {
                checks.push(TeamDoctorCheck {
                    name: "free_space".to_owned(),
                    status: "ok".to_owned(),
                    message: format!(
                        "{available} bytes free; floor is {TEAM_FREE_SPACE_FLOOR_BYTES}"
                    ),
                    repair: None,
                });
            }
            None => {}
        }
    }
    let cached_bodies = connection
        .mesh_storage_status(workspace_id)
        .map(|status| status.cached_body_count)
        .unwrap_or(0);
    let cache_dir_present = workspace_path
        .map(|path| path.join(".ee").join("mesh-body-cache").is_dir())
        .unwrap_or(false);
    let cache_platform = crate::mesh::key_store::mesh_credential_store_platform();
    checks.push(TeamDoctorCheck {
        name: "key_store".to_owned(),
        status: if cache_platform.is_hardened() {
            "ok".to_owned()
        } else {
            "error".to_owned()
        },
        message: match cache_platform {
            crate::mesh::key_store::MeshCredentialStorePlatform::HardenedUnix => {
                "hardened Unix secure-file adapter".to_owned()
            }
            crate::mesh::key_store::MeshCredentialStorePlatform::HardenedWindows => {
                "hardened Windows SID/DACL/reparse adapter".to_owned()
            }
            crate::mesh::key_store::MeshCredentialStorePlatform::Unsupported => {
                "mesh_key_store_unavailable; no reviewed secure-file adapter".to_owned()
            }
        },
        repair: cache_platform
            .is_hardened()
            .then_some(())
            .is_none()
            .then(|| "use a Unix or Windows host with the reviewed secure-file adapter".to_owned()),
    });
    checks.push(TeamDoctorCheck {
        name: "body_cache".to_owned(),
        status: if cache_platform.is_hardened() {
            "ok".to_owned()
        } else {
            "error".to_owned()
        },
        message: format!(
            "{cached_bodies} body cache row(s); dir_present={cache_dir_present}; platform={}",
            match cache_platform {
                crate::mesh::key_store::MeshCredentialStorePlatform::HardenedUnix =>
                    "hardened_unix",
                crate::mesh::key_store::MeshCredentialStorePlatform::HardenedWindows => {
                    "hardened_windows"
                }
                crate::mesh::key_store::MeshCredentialStorePlatform::Unsupported => {
                    crate::mesh::cache::MESH_BODY_CACHE_LIFECYCLE_FAILED_CODE
                }
            }
        ),
        repair: cache_platform
            .is_hardened()
            .then_some(())
            .is_none()
            .then(|| "use a Unix or Windows host with the reviewed secure-file adapter".to_owned()),
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
        if let Some(worst) = worst_identity_timer(&identities) {
            checks.push(TeamDoctorCheck {
                name: "identity_timer".to_owned(),
                status: match worst {
                    IdentityRevalidationPosture::Current => "ok".to_owned(),
                    IdentityRevalidationPosture::Due => "ok".to_owned(),
                    IdentityRevalidationPosture::Overdue => "warning".to_owned(),
                    IdentityRevalidationPosture::Suspended => "warning".to_owned(),
                },
                message: format!("revalidation posture {}", worst.as_str()),
                repair: matches!(
                    worst,
                    IdentityRevalidationPosture::Overdue | IdentityRevalidationPosture::Suspended
                )
                .then(|| "ee team members revalidate --workspace .".to_owned()),
            });
        }
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
    let configured_port = configured_hello_port();
    checks.push(if team.hello_port == configured_port {
        TeamDoctorCheck {
            name: "broker_port".to_owned(),
            status: "ok".to_owned(),
            message: format!(
                "current and configured responder share port {configured_port}; additional workspaces register over the control channel"
            ),
            repair: None,
        }
    } else {
        TeamDoctorCheck {
            name: "broker_port".to_owned(),
            status: "warning".to_owned(),
            message: format!(
                "team hello port {} does not match configured responder port {configured_port}",
                team.hello_port
            ),
            repair: Some(format!(
                "ee team port migrate --to {configured_port} --confirm --workspace . or set EE_MESH_HELLO_PORT={}",
                team.hello_port
            )),
        }
    });
    checks.push(TeamDoctorCheck {
        name: "client_only".to_owned(),
        status: match cache_platform {
            crate::mesh::key_store::MeshCredentialStorePlatform::HardenedUnix => "ok".to_owned(),
            crate::mesh::key_store::MeshCredentialStorePlatform::HardenedWindows => {
                "warning".to_owned()
            }
            crate::mesh::key_store::MeshCredentialStorePlatform::Unsupported => "error".to_owned(),
        },
        message: match cache_platform {
            crate::mesh::key_store::MeshCredentialStorePlatform::HardenedUnix => {
                "this host can run the inbound responder".to_owned()
            }
            crate::mesh::key_store::MeshCredentialStorePlatform::HardenedWindows => {
                "Windows inbound uses TeamJoin; credentials use the hardened DACL adapter"
                    .to_owned()
            }
            crate::mesh::key_store::MeshCredentialStorePlatform::Unsupported => {
                "no reviewed secure-file adapter; team credentials stay blocked".to_owned()
            }
        },
        repair: match cache_platform {
            crate::mesh::key_store::MeshCredentialStorePlatform::HardenedUnix => None,
            crate::mesh::key_store::MeshCredentialStorePlatform::HardenedWindows => {
                Some("ee mesh hello-responder run --workspace . --json".to_owned())
            }
            crate::mesh::key_store::MeshCredentialStorePlatform::Unsupported => {
                Some("use a Unix or Windows host with the reviewed secure-file adapter".to_owned())
            }
        },
    });
    checks.push(TeamDoctorCheck {
        name: "whois".to_owned(),
        status: "ok".to_owned(),
        message: "accept requires WhoIs-verified peer identity before pair-key use; doctor does not probe Tailscale"
            .to_owned(),
        repair: None,
    });
    if let Ok(jobs) = connection
        .list_search_index_jobs(workspace_id, Some(crate::db::SearchIndexJobStatus::Pending))
    {
        checks.push(TeamDoctorCheck {
            name: "index_rematerialization".to_owned(),
            status: if jobs.is_empty() {
                "ok".to_owned()
            } else {
                "warning".to_owned()
            },
            message: format!("{} pending index rematerialization job(s)", jobs.len()),
            repair: (!jobs.is_empty()).then(|| "ee index rebuild --workspace .".to_owned()),
        });
    }
    let stalled_cursors = connection
        .list_mesh_peer_cursors(workspace_id)
        .ok()
        .map(|cursors| {
            let stalled = cursors
                .iter()
                .filter(|cursor| {
                    matches!(cursor.status.as_str(), "behind" | "blocked" | "quarantined")
                })
                .count();
            checks.push(TeamDoctorCheck {
                name: "origin_outbox".to_owned(),
                status: if stalled == 0 {
                    "ok".to_owned()
                } else {
                    "warning".to_owned()
                },
                message: format!(
                    "{} peer cursor(s); {stalled} behind/blocked/quarantined",
                    cursors.len()
                ),
                repair: (stalled > 0).then(|| "ee team steward once --workspace .".to_owned()),
            });
            stalled
        })
        .unwrap_or(0);
    let floor = connection
        .team_invite_auth_floor(&team.team_id)
        .ok()
        .flatten();
    let invites = connection
        .list_team_pending_invites(&team.team_id)
        .unwrap_or_default();
    let pending = invites
        .iter()
        .filter(|invite| invite.status == "pending")
        .count();
    let now = chrono::Utc::now().to_rfc3339();
    let expired_pending = invites
        .iter()
        .filter(|invite| invite.status == "pending" && invite.expires_at.as_str() < now.as_str())
        .count();
    let below_floor = floor
        .as_ref()
        .map(|floor_at| {
            invites
                .iter()
                .filter(|invite| {
                    invite.status == "pending" && invite.created_at.as_str() < floor_at.as_str()
                })
                .count()
        })
        .unwrap_or(0);
    checks.push(TeamDoctorCheck {
        name: "invite_auth_floor".to_owned(),
        status: if below_floor == 0 {
            "ok".to_owned()
        } else {
            "error".to_owned()
        },
        message: match floor.as_deref() {
            Some(floor_at) => format!(
                "floor {floor_at}; {below_floor} pending invite(s) created before the floor"
            ),
            None => "no invite-authorization floor recorded".to_owned(),
        },
        repair: (below_floor > 0)
            .then(|| "ee team revoke --all-before-floor --workspace .".to_owned()),
    });
    let pending_ids: Vec<&str> = invites
        .iter()
        .filter(|invite| invite.status == "pending")
        .map(|invite| invite.invite_id.as_str())
        .collect();
    let shown_ids = pending_ids
        .iter()
        .take(8)
        .copied()
        .collect::<Vec<_>>()
        .join(",");
    checks.push(TeamDoctorCheck {
        name: "pending_invites".to_owned(),
        status: if expired_pending == 0 {
            "ok".to_owned()
        } else {
            "warning".to_owned()
        },
        message: if pending_ids.is_empty() {
            format!("{pending} pending invite(s); {expired_pending} expired")
        } else {
            format!("{pending} pending invite(s); {expired_pending} expired; ids={shown_ids}")
        },
        repair: (expired_pending > 0).then(|| {
            format!(
                "ee team revoke --invite-id {} --workspace .",
                pending_ids[0]
            )
        }),
    });
    let delegated = connection
        .list_all_team_members()
        .unwrap_or_default()
        .into_iter()
        .filter(|member| member.state == "active" && !member.is_self)
        .count();
    checks.push(TeamDoctorCheck {
        name: "delegated_members".to_owned(),
        status: "ok".to_owned(),
        message: format!("{delegated} active non-self member(s) to review"),
        repair: None,
    });
    let missing_projects = connection
        .list_mesh_manifest_origin_events(256)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|row| {
            let Ok(OriginEventPayload::Manifest(payload)) = parse_stored_payload(&row) else {
                return None;
            };
            if payload.operation != TEAM_PROJECT_SHARED_OPERATION {
                return None;
            }
            payload
                .document_payload
                .get("projectId")
                .and_then(serde_json::Value::as_str)
                .map(str::trim)
                .filter(|project_id| is_team_project_id(project_id))
                .map(str::to_owned)
        })
        .filter(|project_id| {
            connection
                .get_team_project(project_id)
                .ok()
                .flatten()
                .is_none()
        })
        .count();
    checks.push(TeamDoctorCheck {
        name: "projects".to_owned(),
        status: if missing_projects == 0 {
            "ok".to_owned()
        } else {
            "warning".to_owned()
        },
        message: format!("{missing_projects} origin project share(s) missing a local row"),
        repair: (missing_projects > 0)
            .then(|| "ee team projects reconcile --workspace .".to_owned()),
    });
    let missing_inbound_memories = connection
        .list_mesh_import_ledger_events_for_workspace(workspace_id)
        .unwrap_or_default()
        .into_iter()
        .filter(|row| row.import_decision == "allow")
        .filter_map(|row| inbound_team_memory_id(&row.event_hash))
        .filter(|memory_id| connection.get_memory(memory_id).ok().flatten().is_none())
        .count();
    checks.push(TeamDoctorCheck {
        name: "inbound_memories".to_owned(),
        status: if missing_inbound_memories == 0 {
            "ok".to_owned()
        } else {
            "warning".to_owned()
        },
        message: format!(
            "{missing_inbound_memories} allowed import-ledger event(s) missing a local memory stub"
        ),
        repair: (missing_inbound_memories > 0)
            .then(|| "ee team steward once --workspace .".to_owned()),
    });
    let stuck_signing = connection
        .list_all_team_member_nodes()
        .unwrap_or_default()
        .into_iter()
        .filter(|node| node.state != "active")
        .count();
    checks.push(TeamDoctorCheck {
        name: "signing_rotation".to_owned(),
        status: if stuck_signing == 0 {
            "ok".to_owned()
        } else {
            "warning".to_owned()
        },
        message: format!("{stuck_signing} non-active member-node signing binding(s)"),
        repair: (stuck_signing > 0).then(|| "ee team members rotate-key --workspace .".to_owned()),
    });
    let signed_removals = connection
        .list_mesh_manifest_origin_events(256)
        .unwrap_or_default()
        .into_iter()
        .filter(|row| {
            matches!(
                parse_stored_payload(row),
                Ok(OriginEventPayload::Manifest(payload))
                    if matches!(
                        payload.operation.as_str(),
                        TEAM_MEMBER_REMOVED_OPERATION | TEAM_LEFT_OPERATION
                    )
            )
        })
        .count();
    let acks = connection
        .list_team_removal_acks(&team.team_id)
        .unwrap_or_default();
    let audience = acks.len();
    let pending_acks = acks
        .iter()
        .filter(|ack| ack.acknowledged_at.is_none())
        .count();
    checks.push(if signed_removals == 0 && audience == 0 {
        TeamDoctorCheck {
            name: "removal_acknowledgements".to_owned(),
            status: "ok".to_owned(),
            message: "no signed removals awaiting acknowledgement".to_owned(),
            repair: None,
        }
    } else if pending_acks == 0 {
        TeamDoctorCheck {
            name: "removal_acknowledgements".to_owned(),
            status: "ok".to_owned(),
            message: if audience == 0 {
                format!(
                    "{signed_removals} signed removal(s); no remaining members to acknowledge"
                )
            } else {
                format!("{signed_removals} signed removal(s); 0/{audience} acknowledgements pending")
            },
            repair: None,
        }
    } else if pending_acks == audience {
        TeamDoctorCheck {
            name: "removal_acknowledgements".to_owned(),
            status: "warning".to_owned(),
            message: format!(
                "{pending_acks}/{audience} acknowledgement(s) pending; nobody has applied the removal; fanout is not bounded"
            ),
            repair: Some("ee team steward once --workspace .".to_owned()),
        }
    } else {
        TeamDoctorCheck {
            name: "removal_acknowledgements".to_owned(),
            status: "warning".to_owned(),
            message: format!(
                "{pending_acks}/{audience} acknowledgement(s) pending; {stalled_cursors} stalled cursor(s)"
            ),
            repair: Some("ee team steward once --workspace .".to_owned()),
        }
    });
    if let Some(path) = workspace_path
        && let Ok(Some(store)) = crate::mesh::key_store::MeshKeyStore::open_existing(path)
        && let Ok(peers) = connection.list_mesh_peers(workspace_id)
    {
        let stuck_next = peers
            .iter()
            .filter(|peer| {
                store
                    .load_pair_key(&peer.peer_id, crate::mesh::key_store::PairKeyClass::Next)
                    .ok()
                    .flatten()
                    .is_some()
            })
            .count();
        checks.push(TeamDoctorCheck {
            name: "pair_rotation".to_owned(),
            status: if stuck_next == 0 {
                "ok".to_owned()
            } else {
                "warning".to_owned()
            },
            message: format!("{stuck_next} peer(s) have a staged Next pair key"),
            repair: (stuck_next > 0).then(|| "ee mesh peer rotate --workspace .".to_owned()),
        });
    }
    if let Ok(rows) = connection.list_mesh_body_cache_metadata(workspace_id) {
        let staging = rows
            .iter()
            .filter(|row| row.cache_status == "staging")
            .count();
        let pending_purge = rows
            .iter()
            .filter(|row| row.cache_status == "invalidated_pending_purge")
            .count();
        checks.push(TeamDoctorCheck {
            name: "body_cache_lifecycle".to_owned(),
            status: if staging == 0 && pending_purge == 0 {
                "ok".to_owned()
            } else {
                "warning".to_owned()
            },
            message: format!(
                "{staging} staging row(s), {pending_purge} invalidated_pending_purge row(s)"
            ),
            repair: (staging > 0 || pending_purge > 0)
                .then(|| "ee team steward once --workspace .".to_owned()),
        });
        let pending_fetches = rows
            .iter()
            .filter(|row| row.cache_status == "metadata_only")
            .count();
        checks.push(TeamDoctorCheck {
            name: "inbound_body_fetches".to_owned(),
            status: if pending_fetches == 0 {
                "ok".to_owned()
            } else {
                "warning".to_owned()
            },
            message: format!("{pending_fetches} inbound body placeholder(s) still metadata_only"),
            repair: (pending_fetches > 0).then(|| "ee team steward once --workspace .".to_owned()),
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
            "mesh_admission_control",
            "team_invite_auth_floor",
            "team_projects",
            "team_removal_acknowledgements",
            "mesh_import_ledger",
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
        oidc_issuer: None,
        oidc_capability: None,
        leases: Vec::new(),
        mesh_primitives: vec!["team_idp_policy", "teamIdpPolicySet"],
    })
}

/// Apply a token-free identity-attest frame from an authenticated peer.
pub fn apply_identity_attest_frame(
    connection: &DbConnection,
    payload: &serde_json::Value,
) -> Result<IdentityAttestFrameV1, OriginStreamError> {
    if json_carries_bearer_fields(payload) {
        return Err(OriginStreamError::Encode(
            "identity_attest frame must not carry bearer material".to_owned(),
        ));
    }
    let frame: IdentityAttestFrameV1 =
        serde_json::from_value(payload.clone()).map_err(|error| {
            OriginStreamError::Encode(format!("identity_attest malformed: {error}"))
        })?;
    if frame.schema != IDENTITY_ATTEST_FRAME_SCHEMA_V1 || identity_attest_frame_leaks_bearer(&frame)
    {
        return Err(OriginStreamError::Encode(
            "identity_attest frame is not token-free".to_owned(),
        ));
    }
    if !frame.token_hash.starts_with("blake3:") || frame.token_hash.len() != 71 {
        return Err(OriginStreamError::Encode(
            "identity_attest token hash is not a blake3 digest".to_owned(),
        ));
    }
    let member = connection
        .get_team_member(&frame.member_id)
        .map_err(|error| OriginStreamError::Db(error.to_string()))?
        .ok_or_else(|| OriginStreamError::Encode("identity_attest member is unknown".to_owned()))?;
    if member.team_id != frame.team_id {
        return Err(OriginStreamError::Encode(
            "identity_attest member is not on the named team".to_owned(),
        ));
    }
    let consumed = connection
        .insert_team_idp_token_replay(
            &frame.token_hash,
            &frame.team_id,
            &frame.member_id,
            &frame.checked_at,
        )
        .map_err(|error| OriginStreamError::Db(error.to_string()))?;
    if !consumed {
        return Err(OriginStreamError::Encode(
            "id token hash was already consumed".to_owned(),
        ));
    }
    let login = frame.email.clone().unwrap_or_else(|| frame.subject.clone());
    record_member_tailnet_identity(
        connection,
        &frame.member_id,
        &login,
        Some(&frame.subject),
        &frame.checked_at,
    )?;
    Ok(frame)
}

/// Reduce a compact ID token and bind the allowlisted claims to the local self member.
pub fn attest_local_id_token(
    connection: &DbConnection,
    token: &str,
    configured_groups: &[&str],
    checked_at: &str,
    jwks: Option<&serde_json::Value>,
) -> Result<TeamIdpAttestReport, OriginStreamError> {
    if any_local_team_paused(connection)? {
        return Err(OriginStreamError::Encode(
            "team is paused; resume before attesting identity".to_owned(),
        ));
    }
    if let Some(jwks) = jwks {
        let verified = verify_compact_jwt_with_jwks(token, jwks);
        if !verified.accepted() {
            return Err(OriginStreamError::Encode(format!(
                "id token signature {}",
                verified.as_str()
            )));
        }
    }
    let claims = reduce_id_token_claims(token, configured_groups).map_err(|disposition| {
        OriginStreamError::Encode(format!("id token is {}", disposition.as_str()))
    })?;
    let self_member = connection
        .list_all_team_members()
        .map_err(|error| OriginStreamError::Db(error.to_string()))?
        .into_iter()
        .find(|member| member.is_self && member.state == "active")
        .ok_or_else(|| OriginStreamError::Encode("no active self member".to_owned()))?;
    if let Some(oidc) = connection
        .get_team_idp_oidc(&self_member.team_id)
        .map_err(|error| OriginStreamError::Db(error.to_string()))?
    {
        let now = chrono::DateTime::parse_from_rfc3339(checked_at)
            .map(|value| value.timestamp())
            .unwrap_or_else(|_| chrono::Utc::now().timestamp());
        let disposition = classify_id_token_claims(token, &oidc.issuer, &oidc.client_id, now);
        if !disposition.accepted() {
            return Err(OriginStreamError::Encode(format!(
                "id token claims {}",
                disposition.as_str()
            )));
        }
    }
    let login = claims
        .email
        .clone()
        .unwrap_or_else(|| claims.subject.clone());
    let token_hash = format!("blake3:{}", blake3::hash(token.as_bytes()).to_hex());
    let consumed = connection
        .insert_team_idp_token_replay(
            &token_hash,
            &self_member.team_id,
            &self_member.member_id,
            checked_at,
        )
        .map_err(|error| OriginStreamError::Db(error.to_string()))?;
    if !consumed {
        return Err(OriginStreamError::Encode(
            "id token hash was already consumed".to_owned(),
        ));
    }
    record_member_tailnet_identity(
        connection,
        &self_member.member_id,
        &login,
        Some(&claims.subject),
        checked_at,
    )?;
    let seed = blake3::hash(format!("{}:attest:{token_hash}", self_member.team_id).as_bytes());
    let hex = seed.to_hex();
    let payload = OriginEventPayload::Manifest(ManifestEventPayload {
        operation: TEAM_IDP_ATTESTED_OPERATION.to_owned(),
        document_id: format!("tdoc_{}", &hex.as_str()[..24]),
        predecessor_revision_id: None,
        document_payload: serde_json::json!({
            "subject": claims.subject,
            "email": claims.email,
            "matchedGroups": claims.matched_groups,
            "tokenHash": token_hash,
        }),
    });
    let mac = LocalOriginSigner::for_workspace(&self_member.workspace_id);
    append_origin_event(
        connection,
        &mac,
        &OriginAppendRequest {
            team_id: &self_member.team_id,
            origin_node_id: &self_member.origin_node_id,
            payload,
            required_features: Vec::new(),
            produced_at: checked_at,
            body_nonce: None,
        },
    )?;
    Ok(TeamIdpAttestReport {
        schema: TEAM_IDP_ATTEST_SCHEMA_V1,
        command: "team idp attest",
        team_id: self_member.team_id,
        member_id: self_member.member_id,
        subject: claims.subject,
        email: claims.email,
        matched_groups: claims.matched_groups,
        mesh_primitives: vec![
            "team_member_identity",
            "id_token_claim_reduction",
            "team_idp_token_replay",
            "identityAttested",
        ],
    })
}

/// Plan a local RFC 8628 device ceremony from offline discovery + authorization JSON.
pub fn plan_team_idp_device(
    connection: &DbConnection,
    discovery: &serde_json::Value,
    authorization: &serde_json::Value,
    curl_binary: &str,
) -> Result<TeamIdpDeviceReport, OriginStreamError> {
    let team = load_local_teams(connection)?
        .into_iter()
        .next()
        .ok_or_else(|| OriginStreamError::Encode("no local team genesis".to_owned()))?;
    let capability = classify_oidc_provider(discovery);
    if !capability.accepted() {
        return Err(OriginStreamError::Encode(format!(
            "OIDC provider is unsupported ({})",
            capability.as_str()
        )));
    }
    let token_url = discovery_https_endpoint(discovery, "token_endpoint").ok_or_else(|| {
        OriginStreamError::Encode("discovery is missing an https token_endpoint".to_owned())
    })?;
    let grant = parse_device_authorization(authorization)
        .map_err(|reason| OriginStreamError::Encode(format!("device authorization {reason}")))?;
    let deadline_secs = device_poll_deadline_secs(grant.expires_in);
    let first = decide_device_poll(
        0,
        deadline_secs,
        grant.interval,
        0,
        Some("authorization_pending"),
    );
    let first_wait_secs = match first {
        crate::mesh::idp::DevicePollDisposition::Wait { delay_secs } => delay_secs,
        _ => {
            return Err(OriginStreamError::Encode(
                "device authorization expired before the first poll".to_owned(),
            ));
        }
    };
    let curl = plan_constrained_https_post(curl_binary, &token_url, first_wait_secs.max(15))
        .ok_or_else(|| {
            OriginStreamError::Encode(
                "constrained curl plan requires an absolute curl binary and https token URL"
                    .to_owned(),
            )
        })?;
    Ok(TeamIdpDeviceReport {
        schema: TEAM_IDP_DEVICE_SCHEMA_V1,
        command: "team idp device",
        team_id: team.team_id,
        capability: capability.as_str().to_owned(),
        user_code: grant.user_code,
        verification_uri: grant.verification_uri,
        verification_uri_complete: grant.verification_uri_complete,
        expires_in: grant.expires_in,
        interval: grant.interval,
        deadline_secs,
        first_wait_secs,
        curl_argv: curl.argv,
        mesh_primitives: vec!["team_idp_oidc", "rfc8628"],
    })
}

/// Execute one constrained HTTPS token poll. Raw tokens stay off the report.
pub fn execute_team_idp_token_poll(
    connection: &DbConnection,
    discovery: &serde_json::Value,
    authorization: &serde_json::Value,
    curl_binary: &str,
    ca_bundle: Option<&str>,
) -> Result<TeamIdpPollReport, OriginStreamError> {
    let planned = plan_team_idp_device(connection, discovery, authorization, curl_binary)?;
    let oidc = connection
        .get_team_idp_oidc(&planned.team_id)
        .map_err(|error| OriginStreamError::Db(error.to_string()))?
        .ok_or_else(|| {
            OriginStreamError::Encode(
                "OIDC provider must be set before a live token poll".to_owned(),
            )
        })?;
    let grant = parse_device_authorization(authorization)
        .map_err(|reason| OriginStreamError::Encode(format!("device authorization {reason}")))?;
    let token_url = discovery_https_endpoint(discovery, "token_endpoint").ok_or_else(|| {
        OriginStreamError::Encode("discovery is missing an https token_endpoint".to_owned())
    })?;
    let mut plan = plan_constrained_https_post(curl_binary, &token_url, 15).ok_or_else(|| {
        OriginStreamError::Encode(
            "constrained curl plan requires an absolute curl binary and https token URL".to_owned(),
        )
    })?;
    if let Some(ca_bundle) = ca_bundle {
        plan = pin_constrained_https_ca(plan, ca_bundle).ok_or_else(|| {
            OriginStreamError::Encode(
                "constrained curl CA pin requires an absolute existing CA bundle".to_owned(),
            )
        })?;
    }
    let body = form_urlencoded(&[
        ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
        ("device_code", &grant.device_code),
        ("client_id", &oidc.client_id),
    ]);
    let executed = execute_constrained_https(&plan, Some(body.as_bytes()))
        .map_err(OriginStreamError::Encode)?;
    let parsed = serde_json::from_slice::<serde_json::Value>(&executed.stdout).ok();
    let token_error = parsed
        .as_ref()
        .and_then(|value| value.get("error"))
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned);
    let gate = parsed
        .as_ref()
        .and_then(|value| crate::mesh::idp::classify_token_response(value).ok());
    Ok(TeamIdpPollReport {
        schema: TEAM_IDP_POLL_SCHEMA_V1,
        command: "team idp device --execute",
        team_id: planned.team_id,
        user_code: planned.user_code,
        verification_uri: planned.verification_uri,
        curl_exit_code: executed.exit_code,
        token_error,
        jwt: gate.as_ref().map(|value| value.jwt.as_str().to_owned()),
        has_access_token: gate.as_ref().is_some_and(|value| value.has_access_token),
        has_refresh_token: gate.as_ref().is_some_and(|value| value.has_refresh_token),
        mesh_primitives: vec!["team_idp_oidc", "rfc8628", "constrained_https"],
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
    let oidc = connection
        .get_team_idp_oidc(&team.team_id)
        .map_err(|error| OriginStreamError::Db(error.to_string()))?;
    let leases = identity_leases(connection, &team.team_id)?;
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
        oidc_issuer: oidc.as_ref().map(|row| row.issuer.clone()),
        oidc_capability: oidc.as_ref().map(|row| row.capability.clone()),
        leases,
        mesh_primitives: vec!["team_idp_policy", "team_idp_oidc", "team_member_identity"],
    })
}

fn identity_leases(
    connection: &DbConnection,
    team_id: &str,
) -> Result<Vec<TeamIdentityLease>, OriginStreamError> {
    let now = chrono::Utc::now().timestamp();
    Ok(connection
        .list_team_member_identities(team_id)
        .map_err(|error| OriginStreamError::Db(error.to_string()))?
        .into_iter()
        .map(|identity| {
            let posture = chrono::DateTime::parse_from_rfc3339(&identity.checked_at)
                .ok()
                .map(|checked| {
                    classify_identity_revalidation(checked.timestamp(), now, 86_400, 86_400)
                        .as_str()
                        .to_owned()
                })
                .unwrap_or_else(|| identity.state.clone());
            TeamIdentityLease {
                member_id: identity.member_id,
                login: identity.login,
                state: identity.state,
                checked_at: identity.checked_at,
                posture,
            }
        })
        .collect())
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

fn worst_identity_timer(
    identities: &[crate::db::StoredTeamMemberIdentity],
) -> Option<IdentityRevalidationPosture> {
    let now = chrono::Utc::now().timestamp();
    identities
        .iter()
        .filter_map(|identity| {
            chrono::DateTime::parse_from_rfc3339(&identity.checked_at)
                .ok()
                .map(|checked| {
                    classify_identity_revalidation(checked.timestamp(), now, 86_400, 86_400)
                })
        })
        .max_by_key(|posture| match posture {
            IdentityRevalidationPosture::Current => 0,
            IdentityRevalidationPosture::Due => 1,
            IdentityRevalidationPosture::Overdue => 2,
            IdentityRevalidationPosture::Suspended => 3,
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
    let members = connection
        .list_all_team_members()
        .map_err(|error| OriginStreamError::Db(error.to_string()))?;
    let local_origin = members
        .iter()
        .find(|row| row.is_self)
        .map(|row| row.origin_node_id.clone())
        .unwrap_or_else(|| member.origin_node_id.clone());
    let audience = members
        .into_iter()
        .filter(|row| row.state == "active" && row.member_id != member.member_id)
        .collect::<Vec<_>>();
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
    let appended = append_origin_event(
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
    if state == "removed" {
        seed_team_removal_acknowledgements(
            connection,
            &member.team_id,
            &local_origin,
            &appended.event_hash,
            appended.seq,
            &audience,
            produced_at,
        )?;
    }
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
            "team_removal_acknowledgements",
        ],
    })
}

fn seed_team_removal_acknowledgements(
    connection: &DbConnection,
    team_id: &str,
    removal_origin_node_id: &str,
    removal_event_hash: &str,
    removal_seq: u64,
    audience: &[StoredTeamMember],
    produced_at: &str,
) -> Result<(), OriginStreamError> {
    for member in audience {
        connection
            .insert_team_removal_ack(&InsertTeamRemovalAckInput {
                removal_event_hash: removal_event_hash.to_owned(),
                team_id: team_id.to_owned(),
                removal_origin_node_id: removal_origin_node_id.to_owned(),
                removal_seq,
                audience_origin_node_id: member.origin_node_id.clone(),
                audience_member_id: member.member_id.clone(),
                acknowledged_at: member.is_self.then(|| produced_at.to_owned()),
                created_at: produced_at.to_owned(),
            })
            .map_err(|error| OriginStreamError::Db(error.to_string()))?;
    }
    Ok(())
}

/// Mark audience rows applied once a peer cursor covers the removal seq.
pub fn advance_team_removal_acknowledgements(
    connection: &DbConnection,
    workspace_id: &str,
    acknowledged_at: &str,
) -> Result<usize, OriginStreamError> {
    let cursors = connection
        .list_mesh_peer_cursors(workspace_id)
        .map_err(|error| OriginStreamError::Db(error.to_string()))?;
    let mut marked = 0_usize;
    for cursor in cursors {
        if matches!(cursor.status.as_str(), "blocked" | "quarantined") {
            continue;
        }
        marked = marked.saturating_add(
            connection
                .acknowledge_team_removal_acks_for_origin(
                    &cursor.origin_node_id,
                    cursor.last_seq,
                    acknowledged_at,
                )
                .map_err(|error| OriginStreamError::Db(error.to_string()))?,
        );
    }
    Ok(marked)
}

/// Mark audience rows applied for one origin without a peer-cursor row.
pub fn acknowledge_team_removal_audience(
    connection: &DbConnection,
    audience_origin_node_id: &str,
    applied_seq: u64,
    acknowledged_at: &str,
) -> Result<usize, OriginStreamError> {
    connection
        .acknowledge_team_removal_acks_for_origin(
            audience_origin_node_id,
            applied_seq,
            acknowledged_at,
        )
        .map_err(|error| OriginStreamError::Db(error.to_string()))
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
    let memory_id = inbound_team_memory_id(&inbound.event_hash).ok_or_else(|| {
        OriginStreamError::Encode("inbound event hash is too short for a memory id".to_owned())
    })?;
    if connection
        .get_memory(&memory_id)
        .map_err(|error| OriginStreamError::Db(error.to_string()))?
        .is_some()
    {
        record_inbound_body_placeholder(connection, workspace_id, inbound, &payload, &memory_id)?;
        enqueue_inbound_memory_index_job(connection, workspace_id, &memory_id, "team-inbound")?;
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
                trust_subclass: Some(inbound_team_trust_subclass(
                    &producer,
                    &inbound.produced_at,
                    payload.project_binding.as_deref(),
                    payload.origin_trust_claim.as_deref(),
                )),
                tags: Vec::new(),
                valid_from: payload.valid_from.clone(),
                valid_to: payload.valid_until.clone(),
            },
        )
        .map_err(|error| OriginStreamError::Db(error.to_string()))?;
    record_inbound_body_placeholder(connection, workspace_id, inbound, &payload, &memory_id)?;
    enqueue_inbound_memory_index_job(connection, workspace_id, &memory_id, "team-inbound")?;
    Ok(Some(memory_id))
}

fn inbound_team_trust_subclass(
    producer: &str,
    produced_at: &str,
    project_binding: Option<&str>,
    origin_trust_claim: Option<&str>,
) -> String {
    let mut parts = vec![
        format!("agent:{producer}"),
        format!("produced_at={produced_at}"),
    ];
    if let Some(project) = project_binding
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        parts.push(format!("project={project}"));
    }
    if let Some(origin) = origin_trust_claim
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .and_then(|raw| crate::models::TrustClass::from_str(raw).ok())
        .map(crate::models::TrustClass::as_str)
    {
        parts.push(format!("origin_trust={origin}"));
    }
    parts.join("; ")
}

/// One coalesced Incremental job per inbound source stays inside the
/// 16-job amplification budget even when a round admits hundreds of rows.
const TEAM_INBOUND_INDEX_JOB_CAP: usize = 16;

/// Inbound projections are memory documents. The jobs table CHECK only
/// allows `memory|session|rule|import`; `team-inbound*` is not a source.
const TEAM_INBOUND_INDEX_SOURCE: &str = "memory";

fn enqueue_inbound_memory_index_job(
    connection: &DbConnection,
    workspace_id: &str,
    _memory_id: &str,
    reason: &str,
) -> Result<(), OriginStreamError> {
    let jobs = connection
        .list_search_index_jobs(workspace_id, None)
        .map_err(|error| OriginStreamError::Db(error.to_string()))?;
    if jobs.iter().any(|job| {
        job.document_source.as_deref() == Some(TEAM_INBOUND_INDEX_SOURCE)
            && job.document_id.is_none()
            && matches!(
                job.status_enum(),
                Some(crate::db::SearchIndexJobStatus::Pending)
                    | Some(crate::db::SearchIndexJobStatus::Running)
            )
    }) {
        return Ok(());
    }
    let pending = jobs
        .iter()
        .filter(|job| job.status_enum() == Some(crate::db::SearchIndexJobStatus::Pending))
        .count();
    if pending >= TEAM_INBOUND_INDEX_JOB_CAP {
        return Ok(());
    }
    let generation = jobs
        .iter()
        .filter(|job| job.document_source.as_deref() == Some(TEAM_INBOUND_INDEX_SOURCE))
        .count();
    let hex = blake3::hash(format!("{reason}:{workspace_id}:{generation}").as_bytes()).to_hex();
    let job_id = format!("sidx_{}", &hex.as_str()[..26]);
    if connection
        .get_search_index_job(&job_id)
        .map_err(|error| OriginStreamError::Db(error.to_string()))?
        .is_some()
    {
        return Ok(());
    }
    connection
        .insert_search_index_job(
            &job_id,
            &CreateSearchIndexJobInput {
                workspace_id: workspace_id.to_owned(),
                job_type: SearchIndexJobType::Incremental,
                document_source: Some(TEAM_INBOUND_INDEX_SOURCE.to_owned()),
                document_id: None,
                documents_total: 1,
            },
        )
        .map_err(|error| OriginStreamError::Db(error.to_string()))?;
    Ok(())
}

/// Mint a typed `mem_*` Crockford id from the origin event hash so pack
/// candidate resolution can parse it. Raw blake3 hex overflows the
/// 26-character payload (first digit must be 0-7).
fn inbound_team_memory_id(event_hash: &str) -> Option<String> {
    let hex = event_hash.trim_start_matches("blake3:");
    let mut bytes = [0_u8; 16];
    for (index, slot) in bytes.iter_mut().enumerate() {
        let start = index.saturating_mul(2);
        let end = start.saturating_add(2);
        let pair = hex.get(start..end)?;
        *slot = u8::from_str_radix(pair, 16).ok()?;
    }
    Some(crate::models::MemoryId::from_uuid(uuid::Uuid::from_bytes(bytes)).to_string())
}

/// Replay allowed import-ledger memory events onto local stub rows.
///
/// Sync may persist the ledger and crash before `project_inbound_team_memory`.
/// Steward rematerializes those stubs; body bytes stay off the wire.
pub fn reconcile_inbound_team_memories(
    connection: &DbConnection,
    workspace_id: &str,
) -> Result<TeamReconcileReport, OriginStreamError> {
    let team = load_local_teams(connection)?
        .into_iter()
        .next()
        .ok_or_else(|| OriginStreamError::Encode("no local team genesis".to_owned()))?;
    let rows = connection
        .list_mesh_import_ledger_events_for_workspace(workspace_id)
        .map_err(|error| OriginStreamError::Db(error.to_string()))?;
    let mut applied_additions = 0_usize;
    let mut inspected = 0_usize;
    for row in rows {
        if row.import_decision != "allow" {
            continue;
        }
        inspected = inspected.saturating_add(1);
        let Some(memory_id) = inbound_team_memory_id(&row.event_hash) else {
            continue;
        };
        if connection
            .get_memory(&memory_id)
            .map_err(|error| OriginStreamError::Db(error.to_string()))?
            .is_some()
        {
            enqueue_inbound_memory_index_job(connection, workspace_id, &memory_id, "team-inbound")?;
            continue;
        }
        let inbound = serde_json::from_str::<InboundOriginEvent>(&row.event_json)
            .map_err(|error| OriginStreamError::PayloadInvalid(error.to_string()));
        let Ok(inbound) = inbound else {
            continue;
        };
        if project_inbound_team_memory(connection, workspace_id, &inbound)?.is_some() {
            applied_additions = applied_additions.saturating_add(1);
        }
    }
    Ok(TeamReconcileReport {
        schema: TEAM_RECONCILE_SCHEMA_V1,
        command: "team memories reconcile",
        team_id: team.team_id,
        applied_additions,
        applied_removals: 0,
        inspected_events: inspected,
        mesh_primitives: vec!["mesh_import_ledger", "memories"],
    })
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
    apply_imported_team_port_migrations(connection, workspace_id)?;
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

fn project_binding_for_workspace(
    connection: &DbConnection,
    team_id: &str,
    workspace_id: &str,
) -> Option<String> {
    let workspace = connection.get_workspace(workspace_id).ok().flatten()?;
    let projects = connection.list_team_projects(team_id).ok()?;
    projects.into_iter().find_map(|project| {
        if project.local_path.is_empty() {
            return None;
        }
        let bound = workspace.path == project.local_path
            || workspace
                .path
                .starts_with(&format!("{}/", project.local_path.trim_end_matches('/')));
        bound.then_some(project.display_name)
    })
}

fn activity_item_matches(
    item: &TeamActivityItem,
    member: Option<&str>,
    project: Option<&str>,
) -> bool {
    if let Some(member) = member.filter(|value| !value.is_empty())
        && item.member_display_name != member
    {
        return false;
    }
    if let Some(project) = project.filter(|value| !value.is_empty())
        && item.project.as_deref() != Some(project)
    {
        return false;
    }
    true
}

/// Closed-metadata team activity over origin events and inbound stubs.
pub fn list_team_activity(
    connection: &DbConnection,
    workspace_id: &str,
    as_of: &str,
    limit: usize,
    member: Option<&str>,
    project: Option<&str>,
    since: Option<&str>,
    cursor: Option<&str>,
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
    let since_cutoff = since
        .map(|raw| {
            chrono::DateTime::parse_from_rfc3339(raw)
                .map(|stamp| stamp.with_timezone(&chrono::Utc))
                .map_err(|error| {
                    OriginStreamError::Encode(format!(
                        "since must be an RFC 3339 timestamp: {error}"
                    ))
                })
        })
        .transpose()?;
    let mut events = Vec::new();
    let mut clock_anomalies = Vec::new();
    let origin_rows = connection
        .list_all_mesh_origin_events(&team.team_id, 1000)
        .map_err(|error| OriginStreamError::Db(error.to_string()))?;
    let origin_generation = u64::try_from(origin_rows.len()).unwrap_or(0);
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
        if let (Some(produced), Some(since_cutoff)) = (produced, since_cutoff)
            && produced < since_cutoff
        {
            continue;
        }
        let item = match parse_stored_payload(&row) {
            Ok(OriginEventPayload::Memory(payload)) => TeamActivityItem {
                event_id: row.event_id.clone(),
                origin_node_id: row.origin_node_id.clone(),
                member_display_name: display_for(&row.origin_node_id),
                project: payload.project_binding,
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
                project: payload
                    .document_payload
                    .get("displayName")
                    .and_then(serde_json::Value::as_str)
                    .map(ToOwned::to_owned),
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
        let provenance = crate::core::memory_scope::team_provenance_from_memory(&memory);
        let member_display_name = provenance
            .as_ref()
            .map(|item| item.member_display_name.clone())
            .or_else(|| crate::core::memory_scope::memory_producer_agent(&memory))
            .unwrap_or_default();
        let origin_node_id = members
            .iter()
            .find(|member| member.display_name == member_display_name)
            .map(|member| member.origin_node_id.clone())
            .unwrap_or_default();
        let produced_at = provenance
            .as_ref()
            .map(|item| item.produced_at.clone())
            .unwrap_or_else(|| memory.created_at.clone());
        let produced = chrono::DateTime::parse_from_rfc3339(&produced_at)
            .ok()
            .map(|stamp| stamp.with_timezone(&chrono::Utc));
        if let (Some(produced), Some(as_of_cutoff)) = (produced, as_of_cutoff)
            && produced > as_of_cutoff
            && anomaly_cutoff.is_none_or(|limit| produced <= limit)
        {
            continue;
        }
        if let (Some(produced), Some(since_cutoff)) = (produced, since_cutoff)
            && produced < since_cutoff
        {
            continue;
        }
        let item = TeamActivityItem {
            event_id: memory
                .provenance_uri
                .clone()
                .unwrap_or_else(|| memory.id.clone()),
            origin_node_id,
            member_display_name,
            project: provenance
                .as_ref()
                .and_then(|item| item.project_name.clone()),
            kind: memory.kind,
            level: memory.level,
            produced_at,
            body_available: !memory.content.starts_with("[ee.team.history]"),
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
    events.retain(|item| activity_item_matches(item, member, project));
    clock_anomalies.retain(|item| activity_item_matches(item, member, project));
    events.sort_by(|left, right| {
        right
            .produced_at
            .cmp(&left.produced_at)
            .then_with(|| left.event_id.cmp(&right.event_id))
    });
    let params_hash = activity_cursor_params_hash(as_of, since, member, project, cap);
    let mac_key = activity_cursor_mac_key(&team.team_id);
    let mut offset = 0_usize;
    let mut cursor_error = None;
    if let Some(token) = cursor.filter(|value| !value.is_empty()) {
        match crate::output::governor::decode_cursor(
            token,
            &mac_key,
            &params_hash,
            origin_generation,
        ) {
            Ok(payload) => {
                offset = payload.position_key.parse().unwrap_or(0);
            }
            Err(crate::output::governor::CursorRejection::Invalid) => {
                cursor_error = Some("cursor_invalid");
            }
            Err(crate::output::governor::CursorRejection::Stale { .. }) => {
                cursor_error = Some("cursor_stale");
            }
        }
    }
    if cursor_error.is_some() {
        events.clear();
        clock_anomalies.clear();
    } else if offset > 0 {
        clock_anomalies.clear();
        let skip = offset.min(events.len());
        events.drain(..skip);
    }
    let remaining_after_page = events.len().saturating_sub(cap);
    events.truncate(cap);
    let next_cursor = if remaining_after_page > 0 && cursor_error.is_none() {
        let next_offset = offset.saturating_add(events.len());
        activity_encode_cursor(
            &mac_key,
            origin_generation,
            &params_hash,
            next_offset,
            remaining_after_page,
        )
    } else {
        None
    };
    let since_used = since_cutoff.is_some();
    Ok(TeamActivityReport {
        schema: TEAM_ACTIVITY_SCHEMA_V1,
        command: "team activity",
        team_id: team.team_id,
        as_of: as_of.to_owned(),
        since: since.map(ToOwned::to_owned),
        time_filter_basis: if since_used {
            "member_attested"
        } else {
            "as_of"
        },
        sequence_complete: !since_used,
        event_count: events.len(),
        events,
        clock_anomalies,
        next_cursor,
        cursor_error,
        mesh_primitives: vec!["mesh_origin_events", "memories", "team_members"],
    })
}

fn activity_cursor_params_hash(
    as_of: &str,
    since: Option<&str>,
    member: Option<&str>,
    project: Option<&str>,
    limit: usize,
) -> String {
    crate::output::governor::hash_invocation_params([
        as_of,
        since.unwrap_or(""),
        member.unwrap_or(""),
        project.unwrap_or(""),
        &limit.to_string(),
    ])
}

fn activity_cursor_mac_key(team_id: &str) -> [u8; 32] {
    crate::output::governor::derive_workspace_mac_key(&format!("ee.team.activity:{team_id}"))
}

fn activity_encode_cursor(
    mac_key: &[u8; 32],
    db_generation: u64,
    params_hash: &str,
    next_offset: usize,
    remaining: usize,
) -> Option<String> {
    let payload = crate::output::governor::CursorPayload {
        schema: crate::output::governor::CURSOR_SCHEMA_V1.to_owned(),
        target_schema: TEAM_ACTIVITY_SCHEMA_V1.to_owned(),
        db_generation,
        position_key: next_offset.to_string(),
        dropped_count: u64::try_from(remaining).unwrap_or(0),
        params_hash: params_hash.to_owned(),
    };
    crate::output::governor::encode_cursor(&payload, mac_key).ok()
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

/// Replay origin `teamProjectShared` events onto local `team_projects` rows.
///
/// Local path stays empty until `ee team projects adopt`. Existing minted or
/// adopted rows are left alone.
pub fn reconcile_local_team_projects(
    connection: &DbConnection,
) -> Result<TeamReconcileReport, OriginStreamError> {
    let team = load_local_teams(connection)?
        .into_iter()
        .next()
        .ok_or_else(|| OriginStreamError::Encode("no local team genesis".to_owned()))?;
    let rows = connection
        .list_mesh_manifest_origin_events(256)
        .map_err(|error| OriginStreamError::Db(error.to_string()))?;
    let mut applied_additions = 0_usize;
    let mut inspected = 0_usize;
    for row in rows {
        let Ok(OriginEventPayload::Manifest(payload)) = parse_stored_payload(&row) else {
            continue;
        };
        if payload.operation != TEAM_PROJECT_SHARED_OPERATION {
            continue;
        }
        inspected = inspected.saturating_add(1);
        let Some(project_id) = payload
            .document_payload
            .get("projectId")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
        else {
            continue;
        };
        if !is_team_project_id(project_id) {
            continue;
        }
        if connection
            .get_team_project(project_id)
            .map_err(|error| OriginStreamError::Db(error.to_string()))?
            .is_some()
        {
            continue;
        }
        let display_name = payload
            .document_payload
            .get("displayName")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .unwrap_or(project_id);
        let inserted = connection
            .insert_team_project(&InsertTeamProjectInput {
                project_id: project_id.to_owned(),
                team_id: team.team_id.clone(),
                display_name: display_name.to_owned(),
                local_path: String::new(),
                source: "reconciled".to_owned(),
                created_at: row.produced_at.clone(),
            })
            .map_err(|error| OriginStreamError::Db(error.to_string()))?;
        if inserted {
            applied_additions = applied_additions.saturating_add(1);
        }
    }
    Ok(TeamReconcileReport {
        schema: TEAM_RECONCILE_SCHEMA_V1,
        command: "team projects reconcile",
        team_id: team.team_id,
        applied_additions,
        applied_removals: 0,
        inspected_events: inspected,
        mesh_primitives: vec!["mesh_origin_events", "team_projects"],
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
    if !is_team_project_id(project_id) {
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
        && (attempt.phase == "granted" || attempt.phase == "first_sync_complete")
        && let Some(granted_json) = attempt.granted_json.as_deref()
        && let Ok(granted) = serde_json::from_str::<TeamJoinGrantedV1>(granted_json)
    {
        let report = persist_granted_join_with_store(
            connection,
            workspace_id,
            &granted,
            &attempt.joiner_node_id,
            name,
            produced_at,
            workspace_path,
            Some(parsed.inviter_verifying_key.as_str()).filter(|key| key.len() == 64),
        )?;
        if attempt.phase == "first_sync_complete" {
            return Ok(TeamJoinReport {
                first_sync: TeamJoinFirstSync {
                    complete: true,
                    imported_events: 0,
                },
                ..report
            });
        }
        return complete_join_first_sync(
            connection,
            workspace_id,
            address,
            &parsed.invite_id,
            &granted,
            &attempt.joiner_node_id,
            produced_at,
            timeout,
            report,
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
        joiner_workspace_id: workspace_id.to_owned(),
        joiner_hello_port: configured_hello_port(),
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
        // Grant is already on the wire. Enroll is how this node EventFetch/BodyFetch
        // the inviter; a missing workspace row must not drop the membership persist.
        let _ = enroll_team_pair_peer(
            connection,
            workspace_id,
            &granted.team_id,
            &granted.origin_node_id,
            &granted.display_name,
            &parsed.endpoint,
            granted.hello_port,
            produced_at,
            if granted.origin_workspace_id.is_empty() {
                parsed.origin_workspace_id.as_str()
            } else {
                granted.origin_workspace_id.as_str()
            },
        );
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
    let report = persist_granted_join_with_store(
        connection,
        workspace_id,
        &granted,
        &joiner_node_id,
        name,
        produced_at,
        workspace_path,
        Some(parsed.inviter_verifying_key.as_str()).filter(|key| key.len() == 64),
    )?;
    complete_join_first_sync(
        connection,
        workspace_id,
        address,
        &parsed.invite_id,
        &granted,
        &joiner_node_id,
        produced_at,
        timeout,
        report,
    )
}

fn complete_join_first_sync(
    connection: &DbConnection,
    workspace_id: &str,
    address: std::net::SocketAddr,
    invite_id: &str,
    granted: &TeamJoinGrantedV1,
    joiner_node_id: &str,
    produced_at: &str,
    timeout: std::time::Duration,
    mut report: TeamJoinReport,
) -> Result<TeamJoinReport, OriginStreamError> {
    match run_join_first_sync(
        connection,
        workspace_id,
        address,
        &granted.team_id,
        &granted.origin_node_id,
        joiner_node_id,
        timeout,
    ) {
        Ok(imported) => {
            report.first_sync = TeamJoinFirstSync {
                complete: true,
                imported_events: imported,
            };
            if !report
                .mesh_primitives
                .iter()
                .any(|item| *item == "mesh_sync")
            {
                report.mesh_primitives.push("mesh_sync");
            }
            let granted_json = serde_json::to_string(granted)
                .map_err(|error| OriginStreamError::Encode(error.to_string()))?;
            let existing = connection
                .get_team_join_attempt(invite_id)
                .map_err(|error| OriginStreamError::Db(error.to_string()))?;
            connection
                .upsert_team_join_attempt(&UpsertTeamJoinAttemptInput {
                    invite_id: invite_id.to_owned(),
                    team_id: granted.team_id.clone(),
                    joiner_node_id: joiner_node_id.to_owned(),
                    joiner_nonce: existing
                        .as_ref()
                        .map(|attempt| attempt.joiner_nonce.clone())
                        .unwrap_or_default(),
                    inviter_nonce: existing.and_then(|attempt| attempt.inviter_nonce),
                    phase: "first_sync_complete".to_owned(),
                    granted_json: Some(granted_json),
                    updated_at: produced_at.to_owned(),
                })
                .map_err(|error| OriginStreamError::Db(error.to_string()))?;
        }
        Err(_) => {}
    }
    Ok(report)
}

fn run_join_first_sync(
    connection: &DbConnection,
    workspace_id: &str,
    address: std::net::SocketAddr,
    team_id: &str,
    origin_node_id: &str,
    joiner_node_id: &str,
    timeout: std::time::Duration,
) -> Result<u32, OriginStreamError> {
    let request = crate::mesh::hello::build_request(
        "team-join-first-sync",
        format!("nodekey:{joiner_node_id}"),
        env!("CARGO_PKG_VERSION"),
        vec![workspace_id.to_owned()],
        vec!["hello".to_owned(), "sync".to_owned()],
        Vec::new(),
    );
    let payload_bytes = crate::mesh::hello::serialize_within_budget(&request)
        .map_err(|error| OriginStreamError::Encode(error.to_string()))?;
    let payload = serde_json::from_slice(&payload_bytes)
        .map_err(|error| OriginStreamError::Encode(error.to_string()))?;
    let (_, sync) = exchange_live_mesh_round(
        address,
        timeout,
        payload,
        &SyncRoundRequest::new(Vec::new(), 0, 32),
    )
    .map_err(|error| OriginStreamError::Encode(error.to_string()))?;
    Ok(apply_join_first_sync_events(
        connection,
        workspace_id,
        team_id,
        origin_node_id,
        &sync.events,
    ))
}

fn apply_join_first_sync_events(
    connection: &DbConnection,
    workspace_id: &str,
    team_id: &str,
    origin_node_id: &str,
    events: &[crate::mesh::bootstrap_envelope::SyncRoundEvent],
) -> u32 {
    let producer_peer_id = team_pair_peer_handle(team_id, origin_node_id);
    let own_origin = connection
        .list_all_team_members()
        .ok()
        .and_then(|members| {
            members
                .into_iter()
                .find(|member| member.is_self)
                .map(|member| member.origin_node_id)
        })
        .unwrap_or_default();
    let now = chrono::Utc::now().to_rfc3339();
    let mut imported = 0_u32;
    for event in events {
        if matches!(
            origin_node_is_active_member(connection, &event.origin_node_id),
            Ok(Some(false))
        ) {
            continue;
        }
        let inbound = serde_json::from_str::<InboundOriginEvent>(&event.payload_json).ok();
        if let Some(inbound) = inbound.as_ref() {
            let verifier = TeamMemberKeyVerifier { connection };
            match ingest_origin_event(
                connection,
                &verifier,
                &own_origin,
                &std::collections::BTreeSet::new(),
                inbound,
                now.as_str(),
            ) {
                Ok(IngestDisposition::Applied) => {}
                Ok(_) | Err(_) => continue,
            }
            let _ = project_inbound_team_memory(connection, workspace_id, inbound);
        }
        let parsed = serde_json::from_str::<serde_json::Value>(&event.payload_json).ok();
        let raw_event_id = parsed
            .as_ref()
            .and_then(|value| value.get("eventId").and_then(serde_json::Value::as_str))
            .unwrap_or(event.event_hash.as_str());
        let suffix = event.event_hash.trim_start_matches("blake3:");
        let compact = suffix.chars().take(24).collect::<String>();
        let event_id = if raw_event_id.starts_with("mesh_evt_") {
            raw_event_id.to_owned()
        } else {
            format!("mesh_evt_{compact}")
        };
        if connection
            .insert_mesh_import_ledger_event(&InsertMeshImportLedgerEventInput {
                workspace_id: workspace_id.to_owned(),
                event_id,
                origin_node_id: event.origin_node_id.clone(),
                origin_workspace_id: event.origin_workspace_id.clone(),
                producer_peer_id: Some(producer_peer_id.to_owned()),
                seq: event.seq.max(1),
                prev_event_hash: parsed.as_ref().and_then(|value| {
                    value
                        .get("prevEventHash")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_owned)
                }),
                event_hash: event.event_hash.clone(),
                event_kind: "create".to_owned(),
                logical_memory_id: format!("mem_{compact}"),
                content_hash: event.event_hash.clone(),
                material_lane: "metadata".to_owned(),
                redaction_class: "metadataOnly".to_owned(),
                trust_lane: "peerAgent".to_owned(),
                import_decision: "allow".to_owned(),
                local_memory_id: None,
                body_cache_key: None,
                policy_failure_surface_json: None,
                policy_decision_json: None,
                event_json: event.payload_json.clone(),
                imported_at: None,
            })
            .is_ok()
        {
            imported = imported.saturating_add(1);
        }
    }
    if let Err(error) = apply_imported_team_port_migrations(connection, workspace_id) {
        tracing::warn!(
            workspace_id,
            error = %error,
            "failed to apply imported teamPortMigrated locators after join first-sync"
        );
    }
    imported
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
            first_sync: TeamJoinFirstSync::incomplete(),
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
    connection
        .raise_team_invite_auth_floor(&granted.team_id, produced_at, produced_at)
        .map_err(|error| OriginStreamError::Db(error.to_string()))?;
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
        first_sync: TeamJoinFirstSync::incomplete(),
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
    if let Some(existing) = find_member_by_origin_node(connection, origin_node_id)? {
        if existing.state != "active" {
            connection
                .set_team_member_state(&existing.member_id, "active")
                .map_err(|error| OriginStreamError::Db(error.to_string()))?;
        }
        if let Some(key) = verifying_key_hex.filter(|key| key.len() == 64) {
            connection
                .insert_team_member_node(&InsertTeamMemberNodeInput {
                    node_id: origin_node_id.to_owned(),
                    member_id: existing.member_id.clone(),
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
        return Ok(existing.member_id);
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

    use std::str::FromStr;

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
        assert_eq!(
            connection
                .team_invite_auth_floor(&report.team.team_id)
                .expect("floor"),
            Some("2026-08-13T00:00:00Z".to_owned())
        );
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
        let listed = local_team_status(&connection).expect("status");
        assert!(
            listed
                .pending_invites
                .iter()
                .any(|invite| invite.invite_id == minted.invite_id && invite.status == "pending")
        );
        let parsed = parse_team_invite_code(&minted.invite_code).expect("parse");
        assert_eq!(parsed.invite_id, minted.invite_id);
        assert_eq!(parsed.origin_workspace_id, "wsp_persistfixture000000000001");
        let granted = redeem_team_invite(
            &connection,
            &parsed.invite_id,
            &parsed.secret,
            "2026-08-13T01:00:00Z",
        )
        .expect("redeem");
        assert_eq!(granted.team_id, minted.team_id);
        assert_eq!(
            granted.origin_workspace_id,
            "wsp_persistfixture000000000001"
        );
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
    fn resume_pending_invite_omits_the_secret() {
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
        let resumed = resume_pending_invite(&connection, &minted.invite_id).expect("resume");
        assert_eq!(resumed.invite_id, minted.invite_id);
        assert_eq!(resumed.endpoint, minted.endpoint);
        assert_eq!(resumed.hello_port, minted.hello_port);
        assert!(resumed.invite_code.is_empty());
        revoke_team_invite(&connection, &minted.invite_id, "2026-08-13T00:30:00Z").expect("revoke");
        assert!(resume_pending_invite(&connection, &minted.invite_id).is_err());
    }

    #[test]
    fn migrate_team_port_folds_without_rewriting_genesis() {
        let connection = open_db();
        let created = create_local_team(
            &connection,
            "wsp_persistfixture000000000001",
            "Analysts",
            "2026-08-13T00:00:00Z",
        )
        .expect("create");
        let genesis = created.team.genesis_event_hash.clone();
        let genesis_port = created.team.hello_port;
        assert!(
            migrate_local_team_port(
                &connection,
                "wsp_persistfixture000000000001",
                genesis_port,
                "2026-08-13T01:00:00Z",
                None,
            )
            .is_err()
        );
        assert!(
            migrate_local_team_port(
                &connection,
                "wsp_persistfixture000000000001",
                80,
                "2026-08-13T01:00:00Z",
                None,
            )
            .is_err()
        );
        let migrated = migrate_local_team_port(
            &connection,
            "wsp_persistfixture000000000001",
            41999,
            "2026-08-13T01:00:00Z",
            None,
        )
        .expect("migrate");
        assert!(migrated.migrated);
        assert_eq!(migrated.current_hello_port, 41999);
        assert_eq!(migrated.previous_hello_port, Some(genesis_port));
        assert_eq!(migrated.genesis_event_hash, genesis);
        assert_eq!(migrated.port_generation, 2);
        assert!(migrated.pair_keys_unchanged);
        assert!(migrated.grants_unchanged);
        let loaded = load_local_teams(&connection).expect("reload");
        assert_eq!(loaded[0].hello_port, 41999);
        assert_eq!(loaded[0].genesis_event_hash, genesis);
        let shown = inspect_team_port(&connection).expect("show");
        assert_eq!(shown.current_hello_port, 41999);
        assert_eq!(shown.genesis_hello_port, genesis_port);
        assert_eq!(shown.port_generation, 2);
        let invited = mint_team_invite(
            &connection,
            "127.0.0.1",
            "2026-08-13T02:00:00Z",
            "2026-08-20T00:00:00Z",
        )
        .expect("invite after migrate");
        assert_eq!(invited.hello_port, 41999);
        assert_eq!(local_hello_bind_port(&connection), 41999);
        let doctor = inspect_team_health(&connection, "wsp_persistfixture000000000001", None)
            .expect("doctor after migrate");
        assert!(doctor.checks.iter().any(|check| {
            check.name == "broker_port"
                && check.status == "warning"
                && check.message.contains("41999")
                && check
                    .repair
                    .as_deref()
                    .is_some_and(|repair| repair.contains("ee team port migrate --to"))
        }));
        let second = migrate_local_team_port(
            &connection,
            "wsp_persistfixture000000000001",
            42000,
            "2026-08-13T03:00:00Z",
            None,
        )
        .expect("second migrate");
        assert_eq!(second.current_hello_port, 42000);
        assert_eq!(second.previous_hello_port, Some(41999));
        assert_eq!(second.port_generation, 3);
        assert_eq!(second.genesis_event_hash, genesis);
        assert_eq!(
            load_local_teams(&connection).expect("reload after second")[0].hello_port,
            42000
        );
        assert_eq!(
            mint_team_invite(
                &connection,
                "127.0.0.1",
                "2026-08-13T04:00:00Z",
                "2026-08-20T00:00:00Z",
            )
            .expect("invite after second")
            .hello_port,
            42000
        );
    }

    #[test]
    fn replace_endpoint_port_rewrites_ipv4_and_bracket_ipv6_only() {
        assert_eq!(
            replace_endpoint_port("127.0.0.1:41888", 41888, 41999).as_deref(),
            Some("127.0.0.1:41999")
        );
        assert_eq!(
            replace_endpoint_port("[fd7a:115c:a1e0::1]:41888", 41888, 41999).as_deref(),
            Some("[fd7a:115c:a1e0::1]:41999")
        );
        assert_eq!(
            replace_endpoint_port("fd7a:115c:a1e0::41888", 41888, 41999),
            None
        );
        assert_eq!(replace_endpoint_port("127.0.0.1:41888", 41999, 42000), None);
    }

    #[test]
    fn migrate_team_port_rewrites_enrolled_peer_locator_and_leaves_pair_key() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = dir.path().canonicalize().expect("canon workspace");
        let database = workspace.join("ee.db");
        let connection = crate::db::DbConnection::open_file(&database).expect("open");
        connection.migrate().expect("migrate");
        connection
            .insert_workspace(
                "wsp_persistfixture000000000001",
                &crate::db::CreateWorkspaceInput {
                    path: workspace.display().to_string(),
                    name: Some("port-migrate".to_owned()),
                },
            )
            .expect("workspace");
        let created = create_local_team_with_store(
            &connection,
            "wsp_persistfixture000000000001",
            "Analysts",
            "2026-08-13T00:00:00Z",
            Some(workspace.as_path()),
        )
        .expect("create");
        let genesis_port = created.team.hello_port;
        let joiner_node = "node_joiner0000000000000000000001";
        enroll_team_pair_peer(
            &connection,
            "wsp_persistfixture000000000001",
            &created.team.team_id,
            joiner_node,
            "Priya",
            "127.0.0.1",
            genesis_port,
            "2026-08-13T04:00:00Z",
            "wsp_joinworkspace0000000000001",
        )
        .expect("enroll");
        persist_pair_key(
            &workspace,
            &created.team.team_id,
            joiner_node,
            &[7_u8; 32],
            "2026-08-13T04:00:00Z",
        )
        .expect("pair");
        let before = crate::mesh::key_store::MeshKeyStore::open_existing(&workspace)
            .expect("open keys")
            .expect("keys present")
            .list_pair_slots()
            .expect("list pairs");
        let migrated = migrate_local_team_port(
            &connection,
            "wsp_persistfixture000000000001",
            41999,
            "2026-08-13T05:00:00Z",
            Some(workspace.as_path()),
        )
        .expect("migrate");
        assert_eq!(migrated.peer_endpoints_rewritten, 1);
        let after = crate::mesh::key_store::MeshKeyStore::open_existing(&workspace)
            .expect("reopen keys")
            .expect("keys present")
            .list_pair_slots()
            .expect("list pairs after");
        assert_eq!(before, after);
        let peer_id = team_pair_peer_handle(&created.team.team_id, joiner_node);
        let stored = connection
            .get_mesh_peer("wsp_persistfixture000000000001", &peer_id)
            .expect("peer")
            .expect("present");
        let mut record: crate::mesh::peer::MeshPeerRecord =
            serde_json::from_str(stored.policy_summary_json.as_deref().expect("json"))
                .expect("parse peer");
        assert_eq!(record.endpoint.endpoint, "127.0.0.1:41999");
        record.endpoint.endpoint = format!("127.0.0.1:{genesis_port}");
        connection
            .upsert_mesh_peer(&UpsertMeshPeerInput {
                workspace_id: stored.workspace_id,
                peer_id: stored.peer_id,
                origin_node_id: stored.origin_node_id,
                display_name: stored.display_name,
                policy_summary_json: Some(serde_json::to_string(&record).expect("replant")),
                enabled: stored.enabled,
                last_seen_at: Some(stored.last_seen_at),
            })
            .expect("replant old locator");
        let rewritten =
            apply_imported_team_port_migrations(&connection, "wsp_persistfixture000000000001")
                .expect("apply imported");
        assert_eq!(rewritten, 1);
        let stored = connection
            .get_mesh_peer("wsp_persistfixture000000000001", &peer_id)
            .expect("peer after import")
            .expect("present after import");
        let record: crate::mesh::peer::MeshPeerRecord =
            serde_json::from_str(stored.policy_summary_json.as_deref().expect("json"))
                .expect("parse peer after import");
        assert_eq!(record.endpoint.endpoint, "127.0.0.1:41999");
        assert_eq!(
            crate::mesh::key_store::MeshKeyStore::open_existing(&workspace)
                .expect("reopen keys after import")
                .expect("keys present after import")
                .list_pair_slots()
                .expect("list pairs after import"),
            after
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
                origin_workspace_id: "wsp_persistfixture000000000001".to_owned(),
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

    #[test]
    fn enroll_team_pair_peer_uses_the_pair_key_handle() {
        let connection = open_db();
        connection
            .insert_workspace(
                "wsp_persistfixture000000000001",
                &crate::db::CreateWorkspaceInput {
                    path: "/tmp/ee-team-enroll".to_owned(),
                    name: Some("enroll".to_owned()),
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
        let handle = enroll_team_pair_peer(
            &connection,
            "wsp_persistfixture000000000001",
            &created.team.team_id,
            &created.team.origin_node_id,
            "Analysts",
            "127.0.0.1",
            created.team.hello_port,
            "2026-08-13T04:00:00Z",
            "wsp_joinworkspace0000000000001",
        )
        .expect("enroll");
        assert_eq!(
            handle,
            team_pair_peer_handle(&created.team.team_id, &created.team.origin_node_id)
        );
        assert_eq!(handle.len(), 37);
        let peer = connection
            .get_mesh_peer("wsp_persistfixture000000000001", &handle)
            .expect("get")
            .expect("row");
        assert!(peer.enabled);
        let record = serde_json::from_str::<crate::mesh::peer::MeshPeerRecord>(
            peer.policy_summary_json.as_deref().expect("policy"),
        )
        .expect("record");
        assert_eq!(record.trust_established_by, "explicit_human_consent");
        assert!(record.capabilities.may_receive.body);
        assert_eq!(
            record.endpoint.endpoint,
            "127.0.0.1:".to_owned() + &created.team.hello_port.to_string()
        );
        assert_eq!(record.origin_workspace_id, "wsp_joinworkspace0000000000001");
        assert!(
            plan_team_body_fetch_binding(
                "wsp_persistfixture000000000001",
                "node_local0000000000000000000001",
                &created.team.team_id,
                &created.team.origin_node_id,
                &record.origin_workspace_id,
                "tailnet-team-join",
            )
            .is_some()
        );
        assert!(
            plan_team_body_fetch_binding(
                "wsp_persistfixture000000000001",
                &created.team.origin_node_id,
                &created.team.team_id,
                &created.team.origin_node_id,
                &record.origin_workspace_id,
                "tailnet-team-join",
            )
            .is_none()
        );
        let again = enroll_team_pair_peer(
            &connection,
            "wsp_persistfixture000000000001",
            &created.team.team_id,
            &created.team.origin_node_id,
            "Analysts",
            "127.0.0.1",
            created.team.hello_port,
            "2026-08-13T04:01:00Z",
            "wsp_joinworkspace0000000000001",
        )
        .expect("idempotent");
        assert_eq!(again, handle);
    }

    #[test]
    fn enroll_team_pair_peer_persists_remote_member_without_config() {
        let root = tempfile::tempdir().unwrap();
        let ee = root.path().join(".ee");
        std::fs::create_dir_all(&ee).expect("ee");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&ee, std::fs::Permissions::from_mode(0o700)).expect("mode");
        }
        let db = ee.join("ee.db");
        let connection = crate::db::DbConnection::open_file(&db).expect("open");
        connection.migrate().expect("migrate");
        connection
            .insert_workspace(
                "wsp_persistfixture000000000001",
                &crate::db::CreateWorkspaceInput {
                    path: root.path().display().to_string(),
                    name: Some("enroll-member".to_owned()),
                },
            )
            .expect("workspace");
        let created = create_local_team(
            &connection,
            "wsp_persistfixture000000000001",
            "Priya",
            "2026-08-13T00:00:00Z",
        )
        .expect("create");
        enroll_team_pair_peer(
            &connection,
            "wsp_persistfixture000000000001",
            &created.team.team_id,
            "node_remoteanalyst00000000000001",
            "Analysts",
            "127.0.0.1",
            41888,
            "2026-08-13T04:00:00Z",
            "wsp_joinworkspace0000000000001",
        )
        .expect("enroll");
        enroll_team_pair_peer(
            &connection,
            "wsp_persistfixture000000000001",
            &created.team.team_id,
            "node_remoteanalyst00000000000001",
            "Analysts",
            "127.0.0.1",
            41888,
            "2026-08-13T04:01:00Z",
            "wsp_joinworkspace0000000000001",
        )
        .expect("idempotent enroll");
        let members = connection.list_all_team_members().expect("members");
        let remotes = members
            .iter()
            .filter(|member| member.origin_node_id == "node_remoteanalyst00000000000001")
            .collect::<Vec<_>>();
        assert_eq!(
            remotes.len(),
            1,
            "enroll must persist the remote human once: {members:?}"
        );
        assert!(
            remotes[0].display_name == "Analysts"
                && remotes[0].state == "active"
                && !remotes[0].is_self,
            "enrolled remote must be an active teammate: {remotes:?}"
        );
        let scope = crate::core::memory_scope::MemoryScopeContext::for_workspace(
            root.path(),
            crate::models::MemoryScope::Team,
            false,
        );
        assert!(
            scope.team_members.contains("Analysts"),
            "Team scope must load the enrolled member from the store without trust.team_members: {:?}",
            scope.team_members
        );
        let inbound = crate::db::StoredMemory {
            id: crate::models::MemoryId::from_uuid(uuid::Uuid::from_u128(0x21)).to_string(),
            workspace_id: "wsp_persistfixture000000000001".to_owned(),
            level: "semantic".to_owned(),
            kind: "note".to_owned(),
            content: "team-join body".to_owned(),
            workflow_id: None,
            confidence: 0.5,
            utility: 0.5,
            importance: 0.5,
            provenance_uri: Some("evt_teamjoinbody".to_owned()),
            trust_class: "peer_human_attested".to_owned(),
            trust_subclass: Some("agent:Analysts; produced_at=2026-08-13T22:00:00Z".to_owned()),
            provenance_chain_hash: None,
            provenance_chain_hash_version: "none".to_owned(),
            provenance_verification_status: "unverified".to_owned(),
            provenance_verified_at: None,
            provenance_verification_note: None,
            created_at: "2026-08-13T22:00:00Z".to_owned(),
            updated_at: "2026-08-13T22:00:00Z".to_owned(),
            tombstoned_at: None,
            valid_from: None,
            valid_to: None,
        };
        assert!(
            scope.memory_in_scope(&inbound),
            "enrolled teammate inbound memory must be in Team scope without config.toml"
        );
    }

    #[test]
    fn enroll_joiner_from_accept_uses_source_ip_and_advertised_port() {
        let connection = open_db();
        connection
            .insert_workspace(
                "wsp_persistfixture000000000001",
                &crate::db::CreateWorkspaceInput {
                    path: "/tmp/ee-team-joiner-enroll".to_owned(),
                    name: Some("enroll".to_owned()),
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
        let joiner_addr: std::net::SocketAddr = "198.51.100.17:54321".parse().expect("addr");
        let handle = enroll_joiner_from_accept(
            &connection,
            "wsp_persistfixture000000000001",
            &created.team.team_id,
            "node_joiner0000000000000000000001",
            "Priya",
            joiner_addr,
            "wsp_joinworkspace0000000000001",
            41999,
            "2026-08-13T04:00:00Z",
        )
        .expect("enroll");
        assert_eq!(
            handle,
            team_pair_peer_handle(&created.team.team_id, "node_joiner0000000000000000000001")
        );
        let peer = connection
            .get_mesh_peer("wsp_persistfixture000000000001", &handle)
            .expect("get")
            .expect("row");
        let record = serde_json::from_str::<crate::mesh::peer::MeshPeerRecord>(
            peer.policy_summary_json.as_deref().expect("policy"),
        )
        .expect("record");
        assert_eq!(record.endpoint.endpoint, "198.51.100.17:41999");
        assert_eq!(record.origin_workspace_id, "wsp_joinworkspace0000000000001");
        assert_eq!(record.alias, "Priya");
        let fallback = enroll_joiner_from_accept(
            &connection,
            "wsp_persistfixture000000000001",
            &created.team.team_id,
            "node_joiner0000000000000000000002",
            "Priya",
            joiner_addr,
            "wsp_joinworkspace0000000000002",
            0,
            "2026-08-13T04:01:00Z",
        )
        .expect("fallback");
        let fallback_peer = connection
            .get_mesh_peer("wsp_persistfixture000000000001", &fallback)
            .expect("get fallback")
            .expect("row");
        let fallback_record = serde_json::from_str::<crate::mesh::peer::MeshPeerRecord>(
            fallback_peer
                .policy_summary_json
                .as_deref()
                .expect("policy"),
        )
        .expect("record");
        assert_eq!(
            fallback_record.endpoint.endpoint,
            format!("198.51.100.17:{}", configured_hello_port())
        );
        assert!(team_join_tailnet_matches(
            TEAM_JOIN_TAILNET_ID,
            "tailnet-real"
        ));
        assert!(team_join_tailnet_matches("tailnet-real", "tailnet-real"));
        assert!(!team_join_tailnet_matches("tailnet-other", "tailnet-real"));
        assert!(team_join_stable_id_matches(
            "team-join-node_abc",
            "nREALSTABLE"
        ));
        assert!(team_join_stable_id_matches("nREALSTABLE", "nREALSTABLE"));
        assert!(!team_join_stable_id_matches("nOTHER", "nREALSTABLE"));
        assert!(team_join_node_pubkey_matches(
            "nodekey:node_abc",
            "nodekey:deadbeef"
        ));
        assert!(team_join_node_pubkey_matches(
            "nodekey:deadbeef",
            "nodekey:deadbeef"
        ));
        assert!(!team_join_node_pubkey_matches(
            "nodekey:other",
            "nodekey:deadbeef"
        ));
        assert!(team_join_allows_ungranted_route(&record));
        let planned = crate::mesh::responder_broker::plan_team_responder_registrations(
            &connection,
            "wsp_persistfixture000000000001",
            std::path::Path::new("/tmp/ee-team-enroll"),
            std::path::Path::new("/tmp/ee-team-enroll/ee.db"),
            created.team.hello_port,
        );
        assert_eq!(planned.len(), 1);
        assert_eq!(planned[0].peer_handle, handle);
        assert_eq!(planned[0].team_id, created.team.team_id);
        assert_eq!(planned[0].responder_node_id, created.team.origin_node_id);
        let api = crate::mesh::responder_broker::TeamJoinLocalApi::from_registrations(
            &connection,
            &planned,
        )
        .expect("team-join local api");
        let who = api
            .identity_for_source(
                format!("127.0.0.1:{}", created.team.hello_port)
                    .parse()
                    .expect("addr"),
            )
            .expect("whois");
        assert_eq!(
            who.current_node_pubkey,
            format!("nodekey:{}", created.team.origin_node_id)
        );
        assert!(
            api.identity_for_source("8.8.8.8:41888".parse().expect("other"))
                .is_none()
        );
        assert!(api.all_loopback());
        let selected =
            crate::mesh::responder_broker::InboundLocalApi::prefer(&connection, &planned, None)
                .expect("prefer");
        assert!(
            selected.is_team_join(),
            "loopback team-join enroll must not prefer tailscaled"
        );
    }

    #[cfg(unix)]
    #[test]
    fn team_join_local_api_start_durable_binds_loopback() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = dir.path().canonicalize().expect("canon workspace");
        let database = workspace.join("ee.db");
        let connection = crate::db::DbConnection::open_file(&database).expect("open");
        connection.migrate().expect("migrate");
        let database = database.canonicalize().expect("canon db");
        connection
            .insert_workspace(
                "wsp_persistfixture000000000001",
                &crate::db::CreateWorkspaceInput {
                    path: workspace.display().to_string(),
                    name: Some("bind".to_owned()),
                },
            )
            .expect("workspace");
        let created = create_local_team_with_store(
            &connection,
            "wsp_persistfixture000000000001",
            "Analysts",
            "2026-08-13T00:00:00Z",
            Some(workspace.as_path()),
        )
        .expect("create");
        let port = {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("free port");
            listener.local_addr().expect("addr").port()
        };
        assert!(port >= 1024);
        let joiner_node = "node_joiner0000000000000000000001";
        enroll_team_pair_peer(
            &connection,
            "wsp_persistfixture000000000001",
            &created.team.team_id,
            joiner_node,
            "Priya",
            "127.0.0.1",
            port,
            "2026-08-13T04:00:00Z",
            "wsp_joinworkspace0000000000001",
        )
        .expect("enroll");
        persist_pair_key(
            &workspace,
            &created.team.team_id,
            joiner_node,
            &[7_u8; 32],
            "2026-08-13T04:00:00Z",
        )
        .expect("pair");
        let registrations = crate::mesh::responder_broker::plan_team_responder_registrations(
            &connection,
            "wsp_persistfixture000000000001",
            &workspace,
            &database,
            port,
        );
        assert_eq!(registrations.len(), 1);
        let api = crate::mesh::responder_broker::TeamJoinLocalApi::from_registrations(
            &connection,
            &registrations,
        )
        .expect("api");
        let mut owner = crate::core::run_cli_with_cx(std::time::Duration::from_secs(30), |cx| {
            let api = api.clone();
            let registrations = registrations.clone();
            async move {
                crate::mesh::responder_broker::ResponderBrokerOwner::start_durable(
                    &cx,
                    api,
                    registrations,
                    crate::mesh::responder_broker::PreAuthAdmissionLimits::default(),
                    std::time::Duration::from_millis(250),
                )
                .await
            }
        })
        .expect("runtime")
        .expect("start");
        assert!(
            owner
                .bound_addresses()
                .iter()
                .any(|address| address.ip().is_loopback() && address.port() == port),
            "bound {:?}",
            owner.bound_addresses()
        );
        let bound = *owner
            .bound_addresses()
            .iter()
            .find(|address| address.ip().is_loopback() && address.port() == port)
            .expect("loopback bound");
        std::net::TcpStream::connect_timeout(&bound, std::time::Duration::from_secs(1))
            .expect("connect to team-join inbound");
        owner.shutdown();
    }

    #[test]
    fn team_join_start_durable_serves_unsigned_hello_sync() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = dir.path().canonicalize().expect("canon workspace");
        let database = workspace.join("ee.db");
        let connection = crate::db::DbConnection::open_file(&database).expect("open");
        connection.migrate().expect("migrate");
        let database = database.canonicalize().expect("canon db");
        connection
            .insert_workspace(
                "wsp_persistfixture000000000001",
                &crate::db::CreateWorkspaceInput {
                    path: workspace.display().to_string(),
                    name: Some("bind".to_owned()),
                },
            )
            .expect("workspace");
        let created = create_local_team_with_store(
            &connection,
            "wsp_persistfixture000000000001",
            "Analysts",
            "2026-08-13T00:00:00Z",
            Some(workspace.as_path()),
        )
        .expect("create");
        let expected_hash = created.team.genesis_event_hash.clone();
        let port = {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("free port");
            listener.local_addr().expect("addr").port()
        };
        assert!(port >= 1024);
        let joiner_node = "node_joiner0000000000000000000001";
        enroll_team_pair_peer(
            &connection,
            "wsp_persistfixture000000000001",
            &created.team.team_id,
            joiner_node,
            "Priya",
            "127.0.0.1",
            port,
            "2026-08-13T04:00:00Z",
            "wsp_joinworkspace0000000000001",
        )
        .expect("enroll");
        persist_pair_key(
            &workspace,
            &created.team.team_id,
            joiner_node,
            &[7_u8; 32],
            "2026-08-13T04:00:00Z",
        )
        .expect("pair");
        let registrations = crate::mesh::responder_broker::plan_team_responder_registrations(
            &connection,
            "wsp_persistfixture000000000001",
            &workspace,
            &database,
            port,
        );
        assert_eq!(registrations.len(), 1);
        let api = crate::mesh::responder_broker::TeamJoinLocalApi::from_registrations(
            &connection,
            &registrations,
        )
        .expect("api");
        let mut owner = crate::core::run_cli_with_cx(std::time::Duration::from_secs(30), |cx| {
            let api = api.clone();
            let registrations = registrations.clone();
            async move {
                crate::mesh::responder_broker::ResponderBrokerOwner::start_durable(
                    &cx,
                    api,
                    registrations,
                    crate::mesh::responder_broker::PreAuthAdmissionLimits::default(),
                    std::time::Duration::from_millis(250),
                )
                .await
            }
        })
        .expect("runtime")
        .expect("start");
        let bound = *owner
            .bound_addresses()
            .iter()
            .find(|address| address.ip().is_loopback() && address.port() == port)
            .expect("loopback bound");
        let client = std::thread::spawn(move || {
            let request = crate::mesh::hello::build_request(
                "team-join-hello-sync",
                format!("nodekey:{joiner_node}"),
                env!("CARGO_PKG_VERSION"),
                vec!["wsp_joinworkspace0000000000001".to_owned()],
                vec!["hello".to_owned(), "sync".to_owned()],
                Vec::new(),
            );
            let payload_bytes =
                crate::mesh::hello::serialize_within_budget(&request).expect("serialize hello");
            let payload = serde_json::from_slice(&payload_bytes).expect("hello json");
            crate::mesh::bootstrap_envelope::exchange_live_mesh_round(
                bound,
                std::time::Duration::from_secs(8),
                payload,
                &crate::mesh::bootstrap_envelope::SyncRoundRequest::new(Vec::new(), 0, 8),
            )
        });
        crate::core::run_cli_with_cx(std::time::Duration::from_secs(15), |cx| {
            let owner = &owner;
            async move { owner.serve_one(&cx).await }
        })
        .expect("serve runtime")
        .expect("serve one");
        let (_hello, sync) = client.join().expect("client thread").expect("live round");
        assert!(
            sync.events
                .iter()
                .any(|event| event.event_hash == expected_hash),
            "unsigned hello sync did not return genesis {expected_hash}: {sync:?}"
        );
        owner.shutdown();
    }

    #[test]
    fn team_join_start_durable_serves_authenticated_event_fetch() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = dir.path().canonicalize().expect("canon workspace");
        let database = workspace.join("ee.db");
        let connection = crate::db::DbConnection::open_file(&database).expect("open");
        connection.migrate().expect("migrate");
        let database = database.canonicalize().expect("canon db");
        connection
            .insert_workspace(
                "wsp_persistfixture000000000001",
                &crate::db::CreateWorkspaceInput {
                    path: workspace.display().to_string(),
                    name: Some("bind".to_owned()),
                },
            )
            .expect("workspace");
        let created = create_local_team_with_store(
            &connection,
            "wsp_persistfixture000000000001",
            "Analysts",
            "2026-08-13T00:00:00Z",
            Some(workspace.as_path()),
        )
        .expect("create");
        let expected_hash = created.team.genesis_event_hash.clone();
        let team_id = created.team.team_id.clone();
        let origin_node = created.team.origin_node_id.clone();
        let port = {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("free port");
            listener.local_addr().expect("addr").port()
        };
        assert!(port >= 1024);
        let joiner_node = "node_0123456789abcdef0123456789abcdef";
        enroll_team_pair_peer(
            &connection,
            "wsp_persistfixture000000000001",
            &team_id,
            joiner_node,
            "Priya",
            "127.0.0.1",
            port,
            "2026-08-13T04:00:00Z",
            "wsp_joinworkspace0000000000001",
        )
        .expect("enroll");
        persist_pair_key(
            &workspace,
            &team_id,
            joiner_node,
            &[7_u8; 32],
            "2026-08-13T04:00:00Z",
        )
        .expect("pair");
        let registrations = crate::mesh::responder_broker::plan_team_responder_registrations(
            &connection,
            "wsp_persistfixture000000000001",
            &workspace,
            &database,
            port,
        );
        assert_eq!(registrations.len(), 1);
        let api = crate::mesh::responder_broker::TeamJoinLocalApi::from_registrations(
            &connection,
            &registrations,
        )
        .expect("api");
        let mut owner = crate::core::run_cli_with_cx(std::time::Duration::from_secs(30), |cx| {
            let api = api.clone();
            let registrations = registrations.clone();
            async move {
                crate::mesh::responder_broker::ResponderBrokerOwner::start_durable(
                    &cx,
                    api,
                    registrations,
                    crate::mesh::responder_broker::PreAuthAdmissionLimits::default(),
                    std::time::Duration::from_millis(250),
                )
                .await
            }
        })
        .expect("runtime")
        .expect("start");
        let bound = *owner
            .bound_addresses()
            .iter()
            .find(|address| address.ip().is_loopback() && address.port() == port)
            .expect("loopback bound");
        let client = std::thread::spawn(move || {
            let config = crate::mesh::transport_session::InitiatorSessionConfig {
                local_address: "127.0.0.1:0".parse().expect("loopback source"),
                binding: crate::mesh::transport_session::SessionBinding {
                    team_id,
                    tailnet_id: TEAM_JOIN_TAILNET_ID.to_owned(),
                    initiator_node_id: joiner_node.to_owned(),
                    responder_node_id: origin_node.clone(),
                    initiator_workspace_id: "wsp_joinworkspace0000000000001".to_owned(),
                    responder_workspace_id: "wsp_persistfixture000000000001".to_owned(),
                    initiator_stable_id: format!("team-join-{joiner_node}"),
                    responder_stable_id: format!("team-join-{origin_node}"),
                    session_id: "replaced-by-connect".to_owned(),
                },
                pair_key: crate::mesh::key_store::SecretBytes::new([7_u8; 32]),
                pair_key_generation: 1,
                observations: crate::mesh::transport_session::HandshakeObservations {
                    initiator_node_pubkey: format!("nodekey:{joiner_node}"),
                    responder_node_pubkey: format!("nodekey:{origin_node}"),
                },
                capabilities: crate::mesh::transport_session::SessionCapabilities::base(),
                limits: crate::mesh::transport_session::SessionChannelLimits {
                    connect_timeout: std::time::Duration::from_secs(8),
                    io_timeout: std::time::Duration::from_secs(8),
                    max_requested_budget_ms: 10_000,
                    max_authenticated_frames: 128,
                    max_authenticated_bytes: 1024 * 1024,
                },
            };
            crate::core::run_cli_with_cx(std::time::Duration::from_secs(15), |cx| async move {
                crate::mesh::foreground_cli::contact_authenticated_mesh_peer(
                    &cx,
                    bound,
                    config,
                    &crate::mesh::bootstrap_envelope::SyncRoundRequest::new(Vec::new(), 0, 8),
                )
                .await
            })
        });
        crate::core::run_cli_with_cx(std::time::Duration::from_secs(15), |cx| {
            let owner = &owner;
            async move { owner.serve_one(&cx).await }
        })
        .expect("serve runtime")
        .expect("serve one");
        let sync = client
            .join()
            .expect("client thread")
            .expect("client runtime")
            .expect("authenticated EventFetch");
        assert!(
            sync.events
                .iter()
                .any(|event| event.event_hash == expected_hash),
            "authenticated EventFetch did not return genesis {expected_hash}: {sync:?}"
        );
        owner.shutdown();
    }

    #[cfg(unix)]
    #[test]
    fn team_join_start_durable_serves_authenticated_body_fetch() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = dir.path().canonicalize().expect("canon workspace");
        let database = workspace.join("ee.db");
        let connection = crate::db::DbConnection::open_file(&database).expect("open");
        connection.migrate().expect("migrate");
        let database = database.canonicalize().expect("canon db");
        connection
            .insert_workspace(
                "wsp_persistfixture000000000001",
                &crate::db::CreateWorkspaceInput {
                    path: workspace.display().to_string(),
                    name: Some("bind".to_owned()),
                },
            )
            .expect("workspace");
        let created = create_local_team_with_store(
            &connection,
            "wsp_persistfixture000000000001",
            "Analysts",
            "2026-08-13T00:00:00Z",
            Some(workspace.as_path()),
        )
        .expect("create");
        let team_id = created.team.team_id.clone();
        let origin_node = created.team.origin_node_id.clone();
        connection
            .insert_memory(
                "mem_teamjoinbody00000000000001",
                &crate::db::CreateMemoryInput {
                    workspace_id: "wsp_persistfixture000000000001".to_owned(),
                    level: "semantic".to_owned(),
                    kind: "note".to_owned(),
                    content: "team-join body".to_owned(),
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
        share_team_bodies(
            &connection,
            "wsp_persistfixture000000000001",
            "2026-08-13T22:00:00Z",
            true,
            16,
            Some(workspace.as_path()),
            false,
            None,
        )
        .expect("share");
        let cache_key = team_body_cache_key("mem_teamjoinbody00000000000001");
        let origin_node_for_client = origin_node.clone();
        let cache_key_for_client = cache_key.clone();
        let port = {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("free port");
            listener.local_addr().expect("addr").port()
        };
        assert!(port >= 1024);
        let joiner_node = "node_0123456789abcdef0123456789abcdef";
        let peer_id = enroll_team_pair_peer(
            &connection,
            "wsp_persistfixture000000000001",
            &team_id,
            joiner_node,
            "Priya",
            "127.0.0.1",
            port,
            "2026-08-13T04:00:00Z",
            "wsp_joinworkspace0000000000001",
        )
        .expect("enroll");
        persist_pair_key(
            &workspace,
            &team_id,
            joiner_node,
            &[7_u8; 32],
            "2026-08-13T04:00:00Z",
        )
        .expect("pair");
        let mutation = crate::db::MeshLaneGrantMutationInput {
            workspace_id: "wsp_persistfixture000000000001".to_owned(),
            peer_id: peer_id.clone(),
            target_adapter: crate::db::MeshLaneGrantTargetAdapter::new(&peer_id, joiner_node),
            material_lane: crate::config::MeshLane::Body,
            expected_generation: 0,
            approval_config_digest: Some(format!("blake3:{}", "a".repeat(64))),
            updated_at: Some("2026-08-13T22:01:00Z".to_owned()),
        };
        connection
            .apply_mesh_lane_grant_with_effect(&mutation, |_| Ok::<(), String>(()))
            .expect("grant");
        assert!(body_lane_allows_fetch(
            &connection,
            "wsp_persistfixture000000000001",
            &peer_id
        ));
        let registrations = crate::mesh::responder_broker::plan_team_responder_registrations(
            &connection,
            "wsp_persistfixture000000000001",
            &workspace,
            &database,
            port,
        );
        assert_eq!(registrations.len(), 1);
        let api = crate::mesh::responder_broker::TeamJoinLocalApi::from_registrations(
            &connection,
            &registrations,
        )
        .expect("api");
        let mut owner = crate::core::run_cli_with_cx(std::time::Duration::from_secs(30), |cx| {
            let api = api.clone();
            let registrations = registrations.clone();
            async move {
                crate::mesh::responder_broker::ResponderBrokerOwner::start_durable(
                    &cx,
                    api,
                    registrations,
                    crate::mesh::responder_broker::PreAuthAdmissionLimits::default(),
                    std::time::Duration::from_millis(250),
                )
                .await
            }
        })
        .expect("runtime")
        .expect("start");
        let bound = *owner
            .bound_addresses()
            .iter()
            .find(|address| address.ip().is_loopback() && address.port() == port)
            .expect("loopback bound");
        let client = std::thread::spawn(move || {
            let config = crate::mesh::transport_session::InitiatorSessionConfig {
                local_address: "127.0.0.1:0".parse().expect("loopback source"),
                binding: crate::mesh::transport_session::SessionBinding {
                    team_id,
                    tailnet_id: TEAM_JOIN_TAILNET_ID.to_owned(),
                    initiator_node_id: joiner_node.to_owned(),
                    responder_node_id: origin_node_for_client.clone(),
                    initiator_workspace_id: "wsp_joinworkspace0000000000001".to_owned(),
                    responder_workspace_id: "wsp_persistfixture000000000001".to_owned(),
                    initiator_stable_id: format!("team-join-{joiner_node}"),
                    responder_stable_id: format!("team-join-{origin_node_for_client}"),
                    session_id: "replaced-by-connect".to_owned(),
                },
                pair_key: crate::mesh::key_store::SecretBytes::new([7_u8; 32]),
                pair_key_generation: 1,
                observations: crate::mesh::transport_session::HandshakeObservations {
                    initiator_node_pubkey: format!("nodekey:{joiner_node}"),
                    responder_node_pubkey: format!("nodekey:{origin_node_for_client}"),
                },
                capabilities: crate::mesh::transport_session::SessionCapabilities::base(),
                limits: crate::mesh::transport_session::SessionChannelLimits {
                    connect_timeout: std::time::Duration::from_secs(20),
                    io_timeout: std::time::Duration::from_secs(20),
                    max_requested_budget_ms: 10_000,
                    max_authenticated_frames: 128,
                    max_authenticated_bytes: 1024 * 1024,
                },
            };
            crate::core::run_cli_with_cx(std::time::Duration::from_secs(30), |cx| async move {
                crate::mesh::foreground_cli::contact_authenticated_body_fetch(
                    &cx,
                    bound,
                    config,
                    &cache_key_for_client,
                )
                .await
            })
        });
        let served = crate::core::run_cli_with_cx(std::time::Duration::from_secs(30), |cx| {
            let owner = &owner;
            async move { owner.serve_one(&cx).await }
        })
        .expect("serve runtime");
        let fetched = client
            .join()
            .expect("client thread")
            .expect("client runtime");
        let fetched = match (served, fetched) {
            (_, Ok(fetched)) => fetched,
            (served, Err(error)) => {
                panic!("authenticated BodyFetch failed client={error:?} served={served:?}")
            }
        };
        assert_eq!(fetched.cache_status, "available");
        assert_eq!(
            fetched.body_hex.as_deref(),
            Some(hex_encode(b"team-join body").as_str()),
            "TeamJoin inbound BodyFetch did not return published bytes: {fetched:?}"
        );
        owner.shutdown();

        let joiner_dir = tempfile::tempdir().unwrap();
        let joiner_ee = joiner_dir.path().join(".ee");
        std::fs::create_dir_all(&joiner_ee).expect("joiner ee");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&joiner_ee, std::fs::Permissions::from_mode(0o700))
                .expect("joiner mode");
        }
        let joiner_db = joiner_ee.join("ee.db");
        let joiner = crate::db::DbConnection::open_file(&joiner_db).expect("joiner open");
        joiner.migrate().expect("joiner migrate");
        joiner
            .insert_workspace(
                "wsp_joinworkspace0000000000001",
                &crate::db::CreateWorkspaceInput {
                    path: joiner_dir.path().display().to_string(),
                    name: Some("joiner".to_owned()),
                },
            )
            .expect("joiner workspace");
        let joiner_team = create_local_team(
            &joiner,
            "wsp_joinworkspace0000000000001",
            "Priya",
            "2026-08-13T00:00:00Z",
        )
        .expect("joiner team");
        enroll_team_pair_peer(
            &joiner,
            "wsp_joinworkspace0000000000001",
            &joiner_team.team.team_id,
            &origin_node,
            "Analysts",
            "127.0.0.1",
            41888,
            "2026-08-13T04:00:00Z",
            "wsp_persistfixture000000000001",
        )
        .expect("admit Analysts");
        let stub_id = crate::models::MemoryId::from_uuid(uuid::Uuid::from_u128(0x21)).to_string();
        joiner
            .insert_memory(
                &stub_id,
                &crate::db::CreateMemoryInput {
                    workspace_id: "wsp_joinworkspace0000000000001".to_owned(),
                    level: "semantic".to_owned(),
                    kind: "note".to_owned(),
                    content: "[ee.team.history] note blake3:deadbeef".to_owned(),
                    workflow_id: None,
                    confidence: 0.5,
                    utility: 0.5,
                    importance: 0.5,
                    provenance_uri: Some("evt_teamjoinbody".to_owned()),
                    trust_class: "peer_human_attested".to_owned(),
                    trust_subclass: Some(
                        "agent:Analysts; produced_at=2026-08-13T22:00:00Z".to_owned(),
                    ),
                    tags: Vec::new(),
                    valid_from: None,
                    valid_to: None,
                },
            )
            .expect("stub");
        let nonce = hex_decode(fetched.nonce_hex.as_deref().expect("nonce"))
            .and_then(|bytes| <[u8; 32]>::try_from(bytes).ok())
            .expect("nonce32");
        let body = hex_decode(fetched.body_hex.as_deref().expect("body")).expect("body bytes");
        joiner
            .upsert_mesh_body_cache_metadata(&UpsertMeshBodyCacheMetadataInput {
                workspace_id: "wsp_joinworkspace0000000000001".to_owned(),
                body_cache_key: cache_key.clone(),
                origin_node_id: origin_node.clone(),
                origin_workspace_id: "wsp_persistfixture000000000001".to_owned(),
                logical_memory_id: "mem_teamjoinbody00000000000001".to_owned(),
                content_hash: body_commitment(&nonce, &body),
                body_ref_json: Some(
                    serde_json::json!({
                        "originEventId": "evt_teamjoinbody",
                        "localMemoryId": stub_id,
                    })
                    .to_string(),
                ),
                preview_hash: None,
                size_bytes: None,
                cache_status: "metadata_only".to_owned(),
                local_body_hash: None,
                cached_at: Some("2026-08-13T22:02:00Z".to_owned()),
                expires_at: None,
            })
            .expect("placeholder");
        let applied = apply_fetched_team_body(
            &joiner,
            "wsp_joinworkspace0000000000001",
            joiner_dir.path(),
            &fetched,
        )
        .expect("apply live fetch");
        assert_eq!(applied.cache_status, "available");
        let stored = joiner.get_memory(&stub_id).expect("get").expect("row");
        assert_eq!(stored.content, "team-join body");
        let _ = drain_team_inbound_search_index(
            &joiner,
            "wsp_joinworkspace0000000000001",
            joiner_dir.path(),
        );
        let packed =
            crate::core::context::run_context_pack(&crate::core::context::ContextPackOptions {
                workspace_path: joiner_dir.path().to_path_buf(),
                database_path: Some(joiner_db),
                index_dir: Some(joiner_ee.join("index")),
                query: "team-join body".to_owned(),
                speed: crate::search::SpeedMode::Instant,
                source_mode: crate::core::search::SearchSourceMode::LexicalOnly,
                strict_source_mode: true,
                filters: crate::models::QueryFilters::default(),
                profile: Some(crate::pack::ContextPackProfile::Balanced),
                max_tokens: Some(800),
                candidate_pool: Some(8),
                max_results: Some(4),
                include_tombstoned: false,
                as_of: None,
                include_expired: false,
                include_future: false,
                include_stale: false,
                relevance_floor: Some(0.0),
                redaction_level: crate::models::RedactionLevel::Minimal,
                memory_scope: crate::models::MemoryScope::Team,
                strict_scope: false,
                ppr_weight: None,
                changed_symbols: Vec::new(),
                changed_symbols_from_git: false,
                pagination: None,
                coordination_snapshot_path: None,
                coordination_stale_after_ms: crate::pack::DEFAULT_COORDINATION_STALE_AFTER_MS,
                task_lens: None,
                require_fresh_sentinels: false,
                output_options: crate::core::context::ContextPackOutputOptions::default(),
                persist_pack: false,
                baseline_write: None,
                no_lod: true,
            })
            .expect("pack");
        assert!(
            packed
                .data
                .pack
                .items
                .iter()
                .any(|item| item.memory_id.to_string() == stub_id),
            "live BodyFetch then pack --memory-scope team must select the teammate memory: items={:?} degraded={:?}",
            packed
                .data
                .pack
                .items
                .iter()
                .map(|item| item.memory_id.to_string())
                .collect::<Vec<_>>(),
            packed.data.degraded
        );
        let pack_json = crate::output::render_context_response_json(&packed);
        assert!(
            pack_json.contains("\"teamProvenance\"") && pack_json.contains("Analysts"),
            "live joiner pack JSON must attribute the teammate: {pack_json}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn team_join_start_durable_denies_ungranted_body_fetch() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = dir.path().canonicalize().expect("canon workspace");
        let database = workspace.join("ee.db");
        let connection = crate::db::DbConnection::open_file(&database).expect("open");
        connection.migrate().expect("migrate");
        let database = database.canonicalize().expect("canon db");
        connection
            .insert_workspace(
                "wsp_persistfixture000000000001",
                &crate::db::CreateWorkspaceInput {
                    path: workspace.display().to_string(),
                    name: Some("bind".to_owned()),
                },
            )
            .expect("workspace");
        let created = create_local_team_with_store(
            &connection,
            "wsp_persistfixture000000000001",
            "Analysts",
            "2026-08-13T00:00:00Z",
            Some(workspace.as_path()),
        )
        .expect("create");
        let team_id = created.team.team_id.clone();
        let origin_node = created.team.origin_node_id.clone();
        connection
            .insert_memory(
                "mem_teamjoinungr00000000000001",
                &crate::db::CreateMemoryInput {
                    workspace_id: "wsp_persistfixture000000000001".to_owned(),
                    level: "semantic".to_owned(),
                    kind: "note".to_owned(),
                    content: "secret team-join body".to_owned(),
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
        share_team_bodies(
            &connection,
            "wsp_persistfixture000000000001",
            "2026-08-13T22:00:00Z",
            true,
            16,
            Some(workspace.as_path()),
            false,
            None,
        )
        .expect("share");
        let cache_key = team_body_cache_key("mem_teamjoinungr00000000000001");
        let port = {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("free port");
            listener.local_addr().expect("addr").port()
        };
        assert!(port >= 1024);
        let joiner_node = "node_0123456789abcdef0123456789abcdef";
        let peer_id = enroll_team_pair_peer(
            &connection,
            "wsp_persistfixture000000000001",
            &team_id,
            joiner_node,
            "Priya",
            "127.0.0.1",
            port,
            "2026-08-13T04:00:00Z",
            "wsp_joinworkspace0000000000001",
        )
        .expect("enroll");
        persist_pair_key(
            &workspace,
            &team_id,
            joiner_node,
            &[7_u8; 32],
            "2026-08-13T04:00:00Z",
        )
        .expect("pair");
        assert!(!body_lane_allows_fetch(
            &connection,
            "wsp_persistfixture000000000001",
            &peer_id
        ));
        let registrations = crate::mesh::responder_broker::plan_team_responder_registrations(
            &connection,
            "wsp_persistfixture000000000001",
            &workspace,
            &database,
            port,
        );
        assert_eq!(registrations.len(), 1);
        let api = crate::mesh::responder_broker::TeamJoinLocalApi::from_registrations(
            &connection,
            &registrations,
        )
        .expect("api");
        let mut owner = crate::core::run_cli_with_cx(std::time::Duration::from_secs(30), |cx| {
            let api = api.clone();
            let registrations = registrations.clone();
            async move {
                crate::mesh::responder_broker::ResponderBrokerOwner::start_durable(
                    &cx,
                    api,
                    registrations,
                    crate::mesh::responder_broker::PreAuthAdmissionLimits::default(),
                    std::time::Duration::from_millis(250),
                )
                .await
            }
        })
        .expect("runtime")
        .expect("start");
        let bound = *owner
            .bound_addresses()
            .iter()
            .find(|address| address.ip().is_loopback() && address.port() == port)
            .expect("loopback bound");
        let client = std::thread::spawn(move || {
            let config = crate::mesh::transport_session::InitiatorSessionConfig {
                local_address: "127.0.0.1:0".parse().expect("loopback source"),
                binding: crate::mesh::transport_session::SessionBinding {
                    team_id,
                    tailnet_id: TEAM_JOIN_TAILNET_ID.to_owned(),
                    initiator_node_id: joiner_node.to_owned(),
                    responder_node_id: origin_node.clone(),
                    initiator_workspace_id: "wsp_joinworkspace0000000000001".to_owned(),
                    responder_workspace_id: "wsp_persistfixture000000000001".to_owned(),
                    initiator_stable_id: format!("team-join-{joiner_node}"),
                    responder_stable_id: format!("team-join-{origin_node}"),
                    session_id: "replaced-by-connect".to_owned(),
                },
                pair_key: crate::mesh::key_store::SecretBytes::new([7_u8; 32]),
                pair_key_generation: 1,
                observations: crate::mesh::transport_session::HandshakeObservations {
                    initiator_node_pubkey: format!("nodekey:{joiner_node}"),
                    responder_node_pubkey: format!("nodekey:{origin_node}"),
                },
                capabilities: crate::mesh::transport_session::SessionCapabilities::base(),
                limits: crate::mesh::transport_session::SessionChannelLimits {
                    connect_timeout: std::time::Duration::from_secs(20),
                    io_timeout: std::time::Duration::from_secs(20),
                    max_requested_budget_ms: 10_000,
                    max_authenticated_frames: 128,
                    max_authenticated_bytes: 1024 * 1024,
                },
            };
            crate::core::run_cli_with_cx(std::time::Duration::from_secs(30), |cx| async move {
                crate::mesh::foreground_cli::contact_authenticated_body_fetch(
                    &cx, bound, config, &cache_key,
                )
                .await
            })
        });
        let served = crate::core::run_cli_with_cx(std::time::Duration::from_secs(30), |cx| {
            let owner = &owner;
            async move { owner.serve_one(&cx).await }
        })
        .expect("serve runtime");
        let fetched = client
            .join()
            .expect("client thread")
            .expect("client runtime");
        let fetched = match (served, fetched) {
            (_, Ok(fetched)) => fetched,
            (served, Err(error)) => {
                panic!("ungranted BodyFetch failed client={error:?} served={served:?}")
            }
        };
        assert_eq!(fetched.cache_status, "metadata_only");
        assert!(
            fetched.body_hex.is_none(),
            "ungranted TeamJoin BodyFetch leaked bytes: {fetched:?}"
        );
        owner.shutdown();
    }

    #[cfg(unix)]
    #[test]
    fn team_join_start_durable_applies_authenticated_identity_attest() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = dir.path().canonicalize().expect("canon workspace");
        let database = workspace.join("ee.db");
        let connection = crate::db::DbConnection::open_file(&database).expect("open");
        connection.migrate().expect("migrate");
        let database = database.canonicalize().expect("canon db");
        connection
            .insert_workspace(
                "wsp_persistfixture000000000001",
                &crate::db::CreateWorkspaceInput {
                    path: workspace.display().to_string(),
                    name: Some("bind".to_owned()),
                },
            )
            .expect("workspace");
        let created = create_local_team_with_store(
            &connection,
            "wsp_persistfixture000000000001",
            "Analysts",
            "2026-08-13T00:00:00Z",
            Some(workspace.as_path()),
        )
        .expect("create");
        let team_id = created.team.team_id.clone();
        let origin_node = created.team.origin_node_id.clone();
        let member_id = connection
            .list_all_team_members()
            .expect("members")
            .into_iter()
            .next()
            .expect("self")
            .member_id;
        let port = {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("free port");
            listener.local_addr().expect("addr").port()
        };
        assert!(port >= 1024);
        let joiner_node = "node_0123456789abcdef0123456789abcdef";
        enroll_team_pair_peer(
            &connection,
            "wsp_persistfixture000000000001",
            &team_id,
            joiner_node,
            "Priya",
            "127.0.0.1",
            port,
            "2026-08-13T04:00:00Z",
            "wsp_joinworkspace0000000000001",
        )
        .expect("enroll");
        persist_pair_key(
            &workspace,
            &team_id,
            joiner_node,
            &[7_u8; 32],
            "2026-08-13T04:00:00Z",
        )
        .expect("pair");
        let registrations = crate::mesh::responder_broker::plan_team_responder_registrations(
            &connection,
            "wsp_persistfixture000000000001",
            &workspace,
            &database,
            port,
        );
        assert_eq!(registrations.len(), 1);
        let api = crate::mesh::responder_broker::TeamJoinLocalApi::from_registrations(
            &connection,
            &registrations,
        )
        .expect("api");
        let mut owner = crate::core::run_cli_with_cx(std::time::Duration::from_secs(30), |cx| {
            let api = api.clone();
            let registrations = registrations.clone();
            async move {
                crate::mesh::responder_broker::ResponderBrokerOwner::start_durable(
                    &cx,
                    api,
                    registrations,
                    crate::mesh::responder_broker::PreAuthAdmissionLimits::default(),
                    std::time::Duration::from_millis(250),
                )
                .await
            }
        })
        .expect("runtime")
        .expect("start");
        let bound = *owner
            .bound_addresses()
            .iter()
            .find(|address| address.ip().is_loopback() && address.port() == port)
            .expect("loopback bound");
        let frame_member_id = member_id.clone();
        let frame_team_id = team_id.clone();
        let client = std::thread::spawn(move || {
            let config = crate::mesh::transport_session::InitiatorSessionConfig {
                local_address: "127.0.0.1:0".parse().expect("loopback source"),
                binding: crate::mesh::transport_session::SessionBinding {
                    team_id,
                    tailnet_id: TEAM_JOIN_TAILNET_ID.to_owned(),
                    initiator_node_id: joiner_node.to_owned(),
                    responder_node_id: origin_node.clone(),
                    initiator_workspace_id: "wsp_joinworkspace0000000000001".to_owned(),
                    responder_workspace_id: "wsp_persistfixture000000000001".to_owned(),
                    initiator_stable_id: format!("team-join-{joiner_node}"),
                    responder_stable_id: format!("team-join-{origin_node}"),
                    session_id: "replaced-by-connect".to_owned(),
                },
                pair_key: crate::mesh::key_store::SecretBytes::new([7_u8; 32]),
                pair_key_generation: 1,
                observations: crate::mesh::transport_session::HandshakeObservations {
                    initiator_node_pubkey: format!("nodekey:{joiner_node}"),
                    responder_node_pubkey: format!("nodekey:{origin_node}"),
                },
                capabilities: crate::mesh::transport_session::SessionCapabilities::base(),
                limits: crate::mesh::transport_session::SessionChannelLimits {
                    connect_timeout: std::time::Duration::from_secs(20),
                    io_timeout: std::time::Duration::from_secs(20),
                    max_requested_budget_ms: 10_000,
                    max_authenticated_frames: 128,
                    max_authenticated_bytes: 1024 * 1024,
                },
            };
            let frame = IdentityAttestFrameV1 {
                schema: IDENTITY_ATTEST_FRAME_SCHEMA_V1.to_owned(),
                team_id: frame_team_id,
                member_id: frame_member_id,
                subject: "user-1".to_owned(),
                email: Some("alice@acme.com".to_owned()),
                matched_groups: vec!["eng".to_owned()],
                token_hash:
                    "blake3:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc"
                        .to_owned(),
                checked_at: "2026-08-13T22:00:00Z".to_owned(),
            };
            crate::core::run_cli_with_cx(std::time::Duration::from_secs(30), |cx| async move {
                crate::mesh::foreground_cli::contact_authenticated_identity_attest(
                    &cx, bound, config, &frame,
                )
                .await
            })
        });
        let served = crate::core::run_cli_with_cx(std::time::Duration::from_secs(30), |cx| {
            let owner = &owner;
            async move { owner.serve_one(&cx).await }
        })
        .expect("serve runtime");
        let applied = client
            .join()
            .expect("client thread")
            .expect("client runtime");
        let applied = match (served, applied) {
            (_, Ok(applied)) => applied,
            (served, Err(error)) => {
                panic!("authenticated identity_attest failed client={error:?} served={served:?}")
            }
        };
        assert_eq!(applied.subject, "user-1");
        assert_eq!(applied.member_id, member_id);
        let identity = connection
            .get_team_member_identity(&member_id)
            .expect("load")
            .expect("identity row");
        assert_eq!(identity.login, "alice@acme.com");
        owner.shutdown();
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
        inviter
            .insert_workspace(
                "wsp_persistfixture000000000001",
                &crate::db::CreateWorkspaceInput {
                    path: "/tmp/ee-team-join-enroll".to_owned(),
                    name: Some("inviter".to_owned()),
                },
            )
            .expect("workspace");
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
            joiner
                .insert_workspace(
                    "wsp_joinworkspace0000000000001",
                    &crate::db::CreateWorkspaceInput {
                        path: "/tmp/ee-team-join-joiner".to_owned(),
                        name: Some("Priya".to_owned()),
                    },
                )
                .expect("joiner workspace");
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
        let served = serve_one_invite_first_sync(
            &inviter,
            "wsp_persistfixture000000000001",
            &listener,
            std::time::Duration::from_secs(8),
        )
        .expect("first sync serve");
        assert!(
            served >= 1,
            "invite waiter must serve origin genesis on first sync: served={served}"
        );
        let joined = client.join().expect("client thread").expect("join");
        assert!(joined.joined);
        assert_eq!(joined.team.team_id, granted.team_id);
        assert!(
            joined.first_sync.complete,
            "invite waiter must stay up for join first sync: {joined:?}"
        );
        assert!(
            joined.first_sync.imported_events >= 1,
            "join first sync must import origin events: {joined:?}"
        );
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
        let joiner_node = members
            .iter()
            .find(|member| !member.is_self && member.display_name == "Priya")
            .expect("joiner member");
        let joiner_handle = team_pair_peer_handle(&granted.team_id, &joiner_node.origin_node_id);
        let enrolled = inviter
            .get_mesh_peer("wsp_persistfixture000000000001", &joiner_handle)
            .expect("peer")
            .expect("enrolled");
        let record = serde_json::from_str::<crate::mesh::peer::MeshPeerRecord>(
            enrolled.policy_summary_json.as_deref().expect("policy"),
        )
        .expect("record");
        assert!(record.endpoint.endpoint.starts_with("127.0.0.1:"));
        assert_eq!(record.origin_workspace_id, "wsp_joinworkspace0000000000001");
    }

    #[test]
    fn join_team_first_sync_imports_origin_genesis() {
        let origin = open_db();
        origin
            .insert_workspace(
                "wsp_persistfixture000000000001",
                &crate::db::CreateWorkspaceInput {
                    path: "/tmp/ee-team-first-sync-origin".to_owned(),
                    name: Some("origin".to_owned()),
                },
            )
            .expect("origin workspace");
        let created = create_local_team(
            &origin,
            "wsp_persistfixture000000000001",
            "Analysts",
            "2026-08-13T00:00:00Z",
        )
        .expect("create");
        let genesis = origin
            .list_mesh_manifest_origin_events(8)
            .expect("origin events")
            .into_iter()
            .find(|row| row.event_hash == created.team.genesis_event_hash)
            .expect("genesis row");
        let joiner = open_db();
        joiner
            .insert_workspace(
                "wsp_joinworkspace0000000000001",
                &crate::db::CreateWorkspaceInput {
                    path: "/tmp/ee-team-first-sync-joiner".to_owned(),
                    name: Some("Priya".to_owned()),
                },
            )
            .expect("joiner workspace");
        persist_granted_join(
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
                origin_workspace_id: "wsp_persistfixture000000000001".to_owned(),
            },
            "node_joinerself00000000000000001",
            "Priya",
            "2026-08-13T04:00:00Z",
        )
        .expect("persist join");
        let imported = apply_join_first_sync_events(
            &joiner,
            "wsp_joinworkspace0000000000001",
            &created.team.team_id,
            &created.team.origin_node_id,
            &[crate::mesh::bootstrap_envelope::SyncRoundEvent {
                origin_node_id: genesis.origin_node_id.clone(),
                origin_workspace_id: "wsp_persistfixture000000000001".to_owned(),
                seq: genesis.seq,
                event_hash: genesis.event_hash.clone(),
                payload_json: genesis.payload_json.clone(),
            }],
        );
        assert!(
            imported >= 1,
            "first sync must import the origin genesis {}: imported={imported}",
            created.team.genesis_event_hash
        );
        let ledger = joiner
            .list_mesh_import_ledger_events_for_workspace("wsp_joinworkspace0000000000001")
            .expect("ledger");
        assert!(
            ledger
                .iter()
                .any(|row| row.event_hash == created.team.genesis_event_hash),
            "import ledger must record the genesis hash: {ledger:?}"
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
        assert_eq!(
            crate::core::memory_scope::memory_producer_agent(&stored).as_deref(),
            Some("Analysts")
        );
        let provenance = crate::core::memory_scope::team_provenance_from_memory(&stored)
            .expect("inbound stub is attributable");
        assert_eq!(provenance.member_display_name, "Analysts");
        assert_eq!(provenance.origin_time_assurance, "member_attested");
        assert!(!provenance.produced_at.is_empty());
        assert!(
            pending_team_body_fetch_keys(&connection, "wsp_persistfixture000000000001")
                .expect("pending")
                .is_empty(),
            "omitted history share must not create a body-fetch placeholder"
        );
        assert_eq!(
            provenance.project_name, None,
            "unbound workspace must not invent a project name"
        );
    }

    #[test]
    fn project_inbound_team_memory_persists_project_binding() {
        let connection = open_db();
        connection
            .insert_workspace(
                "wsp_persistfixture000000000001",
                &crate::db::CreateWorkspaceInput {
                    path: "/tmp/acme-analysis".to_owned(),
                    name: Some("acme".to_owned()),
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
        share_team_project(
            &connection,
            "wsp_persistfixture000000000001",
            "acme-analysis",
            "/tmp/acme-analysis",
            "2026-08-13T13:00:00Z",
            None,
        )
        .expect("project");
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
            "2026-08-13T14:00:00Z",
            true,
            16,
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
        let provenance = crate::core::memory_scope::team_provenance_from_memory(&stored)
            .expect("inbound stub is attributable");
        assert_eq!(provenance.member_display_name, "Analysts");
        assert_eq!(provenance.project_name.as_deref(), Some("acme-analysis"));
        assert_eq!(provenance.origin_trust_class, "human_explicit");
        assert!(
            stored
                .trust_subclass
                .as_deref()
                .is_some_and(|value| value.contains("origin_trust=human_explicit")),
            "inbound trust_subclass must persist origin_trust=: {:?}",
            stored.trust_subclass
        );
        assert_eq!(
            provenance.compact_suffix(),
            format!(
                "· from Analysts / acme-analysis · {}",
                provenance.produced_at
            )
        );
        assert!(
            stored
                .trust_subclass
                .as_deref()
                .is_some_and(|value| value.contains("project=acme-analysis")),
            "inbound trust_subclass must persist project=: {:?}",
            stored.trust_subclass
        );
    }

    #[test]
    fn inbound_team_memory_id_is_a_parseable_memory_id() {
        let high = inbound_team_memory_id(
            "blake3:ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff",
        )
        .expect("id");
        crate::models::MemoryId::from_str(&high).expect(&high);
        let mixed = inbound_team_memory_id(
            "blake3:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
        )
        .expect("id");
        crate::models::MemoryId::from_str(&mixed).expect(&mixed);
        assert_ne!(high, mixed);
    }

    #[test]
    fn hydrate_inbound_team_memory_body_replaces_history_stub() {
        let connection = open_db();
        connection
            .insert_workspace(
                "wsp_persistfixture000000000001",
                &crate::db::CreateWorkspaceInput {
                    path: "/tmp/ee-team-hydrate".to_owned(),
                    name: Some("hydrate".to_owned()),
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
        let stub_id = crate::models::MemoryId::from_uuid(uuid::Uuid::from_u128(0x11)).to_string();
        connection
            .insert_memory(
                &stub_id,
                &crate::db::CreateMemoryInput {
                    workspace_id: "wsp_persistfixture000000000001".to_owned(),
                    level: "semantic".to_owned(),
                    kind: "note".to_owned(),
                    content: "[ee.team.history] note blake3:deadbeef".to_owned(),
                    workflow_id: None,
                    confidence: 0.5,
                    utility: 0.5,
                    importance: 0.5,
                    provenance_uri: Some("evt_teamhydrate".to_owned()),
                    trust_class: "peer_human_attested".to_owned(),
                    trust_subclass: Some(
                        "agent:Analysts; produced_at=2026-08-13T11:00:00Z".to_owned(),
                    ),
                    tags: Vec::new(),
                    valid_from: None,
                    valid_to: None,
                },
            )
            .expect("stub");
        let row = crate::db::StoredMeshBodyCacheMetadata {
            workspace_id: "wsp_persistfixture000000000001".to_owned(),
            body_cache_key: "body_teamhydrate00000000000001".to_owned(),
            origin_node_id: "node_hydrate".to_owned(),
            origin_workspace_id: "wsp_persistfixture000000000001".to_owned(),
            logical_memory_id: "mem_originbody0000000000000001".to_owned(),
            content_hash: "blake3:deadbeef".to_owned(),
            body_ref_json: Some(
                serde_json::json!({
                    "originEventId": "evt_teamhydrate",
                    "localMemoryId": stub_id,
                })
                .to_string(),
            ),
            preview_hash: None,
            size_bytes: None,
            cache_status: "available".to_owned(),
            local_body_hash: None,
            cached_at: "2026-08-15T00:00:00Z".to_owned(),
            expires_at: None,
        };
        hydrate_inbound_team_memory_body(
            &connection,
            "wsp_persistfixture000000000001",
            &row,
            b"Acme Corp analysis from Priya",
        )
        .expect("hydrate");
        let stored = connection.get_memory(&stub_id).expect("get").expect("row");
        assert_eq!(stored.content, "Acme Corp analysis from Priya");
        assert_eq!(stored.trust_class, "peer_human_attested");
        assert_eq!(
            crate::core::memory_scope::memory_producer_agent(&stored).as_deref(),
            Some("Analysts")
        );
        let context = crate::core::memory_scope::MemoryScopeContext {
            scope: crate::models::MemoryScope::Team,
            strict_scope: false,
            current_agent: Some("local".to_owned()),
            team_members: std::collections::BTreeSet::from(["Analysts".to_owned()]),
        };
        assert!(
            context.memory_in_scope(&stored),
            "hydrated teammate memory must pass --memory-scope team"
        );
        let jobs = connection
            .list_search_index_jobs(
                "wsp_persistfixture000000000001",
                Some(crate::db::SearchIndexJobStatus::Pending),
            )
            .expect("jobs");
        assert!(
            jobs.iter().any(|job| {
                job.document_source.as_deref() == Some("memory")
                    && job.document_id.is_none()
                    && job.job_type == "incremental"
            }),
            "hydrate must enqueue a coalesced Incremental job so search can see the body: {jobs:?}"
        );
    }

    #[test]
    fn inbound_index_jobs_coalesce_under_amplification_cap() {
        let connection = open_db();
        let workspace_id = "wsp_indexcoalesce0000000000001";
        connection
            .insert_workspace(
                workspace_id,
                &crate::db::CreateWorkspaceInput {
                    path: "/tmp/ee-team-index-coalesce".to_owned(),
                    name: Some("coalesce".to_owned()),
                },
            )
            .expect("workspace");
        for index in 0..500_u32 {
            enqueue_inbound_memory_index_job(
                &connection,
                workspace_id,
                &format!("mem_coalesce{index:018}"),
                "team-inbound",
            )
            .expect("enqueue");
        }
        let jobs = connection
            .list_search_index_jobs(workspace_id, None)
            .expect("jobs");
        assert_eq!(
            jobs.len(),
            1,
            "a 500-row inbound burst must stay one Incremental job: {jobs:?}"
        );
        assert_eq!(jobs[0].job_type, "incremental");
        assert_eq!(jobs[0].document_source.as_deref(), Some("memory"));
        assert!(jobs[0].document_id.is_none());
        assert!(jobs[0].status_enum() == Some(crate::db::SearchIndexJobStatus::Pending));
    }

    #[test]
    fn apply_fetched_team_body_hydrates_already_available_cache() {
        use crate::mesh::bootstrap_envelope::{BODY_FETCH_RESPONSE_SCHEMA_V1, BodyFetchResponse};

        let workspace = tempfile::tempdir().expect("workspace");
        let ee_dir = workspace.path().join(".ee");
        std::fs::create_dir_all(&ee_dir).expect("ee dir");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&ee_dir, std::fs::Permissions::from_mode(0o700))
                .expect("ee mode");
        }
        let database = ee_dir.join("ee.db");
        let connection = crate::db::DbConnection::open_file(&database).expect("open");
        connection.migrate().expect("migrate");
        connection
            .insert_workspace(
                "wsp_persistfixture000000000001",
                &crate::db::CreateWorkspaceInput {
                    path: workspace.path().display().to_string(),
                    name: Some("hydrate-retry".to_owned()),
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
        let stub_id = crate::models::MemoryId::from_uuid(uuid::Uuid::from_u128(0x12)).to_string();
        connection
            .insert_memory(
                &stub_id,
                &crate::db::CreateMemoryInput {
                    workspace_id: "wsp_persistfixture000000000001".to_owned(),
                    level: "semantic".to_owned(),
                    kind: "note".to_owned(),
                    content: "[ee.team.history] note blake3:deadbeef".to_owned(),
                    workflow_id: None,
                    confidence: 0.5,
                    utility: 0.5,
                    importance: 0.5,
                    provenance_uri: Some("evt_teamhydrateretry".to_owned()),
                    trust_class: "peer_human_attested".to_owned(),
                    trust_subclass: Some(
                        "agent:Analysts; produced_at=2026-08-13T11:00:00Z".to_owned(),
                    ),
                    tags: Vec::new(),
                    valid_from: None,
                    valid_to: None,
                },
            )
            .expect("stub");
        let key = "body_teamhydrateretry0000000001";
        let body = b"Acme Corp analysis from Priya after retry";
        let local_body_hash = format!("blake3:{}", blake3::hash(body).to_hex());
        let cache_dir = workspace.path().join(".ee").join("mesh-body-cache");
        let cache =
            crate::mesh::key_store::SecureLocalDir::open_or_create(workspace.path(), &cache_dir)
                .expect("cache");
        cache.write_replace(key, body).expect("write cache");
        connection
            .upsert_mesh_body_cache_metadata(&UpsertMeshBodyCacheMetadataInput {
                workspace_id: "wsp_persistfixture000000000001".to_owned(),
                body_cache_key: key.to_owned(),
                origin_node_id: "node_hydrate".to_owned(),
                origin_workspace_id: "wsp_persistfixture000000000001".to_owned(),
                logical_memory_id: "mem_originbody0000000000000001".to_owned(),
                content_hash: "blake3:deadbeef".to_owned(),
                body_ref_json: Some(
                    serde_json::json!({
                        "originEventId": "evt_teamhydrateretry",
                        "localMemoryId": stub_id,
                    })
                    .to_string(),
                ),
                preview_hash: None,
                size_bytes: Some(u64::try_from(body.len()).unwrap_or(0)),
                cache_status: "available".to_owned(),
                local_body_hash: Some(local_body_hash),
                cached_at: Some("2026-08-15T00:00:00Z".to_owned()),
                expires_at: None,
            })
            .expect("available row");
        let applied = apply_fetched_team_body(
            &connection,
            "wsp_persistfixture000000000001",
            workspace.path(),
            &BodyFetchResponse {
                schema: BODY_FETCH_RESPONSE_SCHEMA_V1.to_owned(),
                body_cache_key: key.to_owned(),
                cache_status: "available".to_owned(),
                size_bytes: u64::try_from(body.len()).unwrap_or(0),
                body_hex: Some(hex_encode(body)),
                nonce_hex: Some(hex_encode(&[7_u8; 32])),
            },
        )
        .expect("apply retry");
        assert_eq!(applied.cache_status, "available");
        let stored = connection.get_memory(&stub_id).expect("get").expect("row");
        assert_eq!(
            stored.content, "Acme Corp analysis from Priya after retry",
            "already-available BodyFetch must still hydrate leftover history stubs"
        );
    }

    #[test]
    fn steward_hydrates_leftover_history_stubs_from_available_cache() {
        let workspace = tempfile::tempdir().expect("workspace");
        let ee_dir = workspace.path().join(".ee");
        std::fs::create_dir_all(&ee_dir).expect("ee dir");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&ee_dir, std::fs::Permissions::from_mode(0o700))
                .expect("ee mode");
        }
        let database = ee_dir.join("ee.db");
        let connection = crate::db::DbConnection::open_file(&database).expect("open");
        connection.migrate().expect("migrate");
        connection
            .insert_workspace(
                "wsp_persistfixture000000000001",
                &crate::db::CreateWorkspaceInput {
                    path: workspace.path().display().to_string(),
                    name: Some("hydrate-steward".to_owned()),
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
        let stub_id = crate::models::MemoryId::from_uuid(uuid::Uuid::from_u128(0x13)).to_string();
        connection
            .insert_memory(
                &stub_id,
                &crate::db::CreateMemoryInput {
                    workspace_id: "wsp_persistfixture000000000001".to_owned(),
                    level: "semantic".to_owned(),
                    kind: "note".to_owned(),
                    content: "[ee.team.history] note blake3:deadbeef".to_owned(),
                    workflow_id: None,
                    confidence: 0.5,
                    utility: 0.5,
                    importance: 0.5,
                    provenance_uri: Some("evt_teamhydratesteward".to_owned()),
                    trust_class: "peer_human_attested".to_owned(),
                    trust_subclass: Some(
                        "agent:Analysts; produced_at=2026-08-13T11:00:00Z".to_owned(),
                    ),
                    tags: Vec::new(),
                    valid_from: None,
                    valid_to: None,
                },
            )
            .expect("stub");
        let key = "body_teamhydratesteward00000001";
        let body = b"Acme Corp analysis from Priya via steward";
        let local_body_hash = format!("blake3:{}", blake3::hash(body).to_hex());
        let cache_dir = workspace.path().join(".ee").join("mesh-body-cache");
        let cache =
            crate::mesh::key_store::SecureLocalDir::open_or_create(workspace.path(), &cache_dir)
                .expect("cache");
        cache.write_replace(key, body).expect("write cache");
        connection
            .upsert_mesh_body_cache_metadata(&UpsertMeshBodyCacheMetadataInput {
                workspace_id: "wsp_persistfixture000000000001".to_owned(),
                body_cache_key: key.to_owned(),
                origin_node_id: "node_hydrate".to_owned(),
                origin_workspace_id: "wsp_persistfixture000000000001".to_owned(),
                logical_memory_id: "mem_originbody0000000000000001".to_owned(),
                content_hash: "blake3:deadbeef".to_owned(),
                body_ref_json: Some(
                    serde_json::json!({
                        "originEventId": "evt_teamhydratesteward",
                        "localMemoryId": stub_id,
                    })
                    .to_string(),
                ),
                preview_hash: None,
                size_bytes: Some(u64::try_from(body.len()).unwrap_or(0)),
                cache_status: "available".to_owned(),
                local_body_hash: Some(local_body_hash),
                cached_at: Some("2026-08-15T00:00:00Z".to_owned()),
                expires_at: None,
            })
            .expect("available row");
        execute_team_steward_once(&connection, Some(workspace.path())).expect("steward");
        let stored = connection.get_memory(&stub_id).expect("get").expect("row");
        assert_eq!(
            stored.content, "Acme Corp analysis from Priya via steward",
            "ee team steward once must hydrate leftover history stubs from available cache"
        );
    }

    #[test]
    fn hydrated_team_memory_is_searchable_under_team_scope() {
        let workspace = tempfile::tempdir().expect("workspace");
        let ee_dir = workspace.path().join(".ee");
        std::fs::create_dir_all(&ee_dir).expect("ee dir");
        let database = ee_dir.join("ee.db");
        let connection = crate::db::DbConnection::open_file(&database).expect("open");
        connection.migrate().expect("migrate");
        connection
            .insert_workspace(
                "wsp_persistfixture000000000001",
                &crate::db::CreateWorkspaceInput {
                    path: workspace.path().display().to_string(),
                    name: Some("hydrate-search".to_owned()),
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
        let stub_id = crate::models::MemoryId::from_uuid(uuid::Uuid::from_u128(0x11)).to_string();
        connection
            .insert_memory(
                &stub_id,
                &crate::db::CreateMemoryInput {
                    workspace_id: "wsp_persistfixture000000000001".to_owned(),
                    level: "semantic".to_owned(),
                    kind: "note".to_owned(),
                    content: "[ee.team.history] note blake3:deadbeef".to_owned(),
                    workflow_id: None,
                    confidence: 0.5,
                    utility: 0.5,
                    importance: 0.5,
                    provenance_uri: Some("evt_teamhydratesearch".to_owned()),
                    trust_class: "peer_human_attested".to_owned(),
                    trust_subclass: Some(
                        "agent:Analysts; produced_at=2026-08-13T11:00:00Z".to_owned(),
                    ),
                    tags: Vec::new(),
                    valid_from: None,
                    valid_to: None,
                },
            )
            .expect("stub");
        let row = crate::db::StoredMeshBodyCacheMetadata {
            workspace_id: "wsp_persistfixture000000000001".to_owned(),
            body_cache_key: "body_teamhydrate000000000000001".to_owned(),
            origin_node_id: "node_hydrate".to_owned(),
            origin_workspace_id: "wsp_persistfixture000000000001".to_owned(),
            logical_memory_id: "mem_originbody0000000000000001".to_owned(),
            content_hash: "blake3:deadbeef".to_owned(),
            body_ref_json: Some(
                serde_json::json!({
                    "originEventId": "evt_teamhydratesearch",
                    "localMemoryId": stub_id,
                })
                .to_string(),
            ),
            preview_hash: None,
            size_bytes: None,
            cache_status: "available".to_owned(),
            local_body_hash: None,
            cached_at: "2026-08-15T00:00:00Z".to_owned(),
            expires_at: None,
        };
        hydrate_inbound_team_memory_body(
            &connection,
            "wsp_persistfixture000000000001",
            &row,
            b"Acme Corp analysis from Priya",
        )
        .expect("hydrate");
        let pending_before = connection
            .list_search_index_jobs(
                "wsp_persistfixture000000000001",
                Some(crate::db::SearchIndexJobStatus::Pending),
            )
            .expect("pending jobs");
        assert!(
            pending_before.iter().any(|job| {
                job.document_source.as_deref() == Some("memory")
                    && job.document_id.is_none()
                    && job.job_type == "incremental"
            }),
            "hydrate must enqueue a coalesced Incremental job: {pending_before:?}"
        );
        let drained = drain_team_inbound_search_index(
            &connection,
            "wsp_persistfixture000000000001",
            workspace.path(),
        )
        .unwrap_or_else(|error| panic!("drain failed: {error}"));
        assert!(
            drained > 0,
            "hydrate must leave a drainable Incremental job ({pending_before:?})"
        );
        let search = crate::core::search::run_search(&crate::core::search::SearchOptions {
            workspace_path: workspace.path().to_path_buf(),
            database_path: Some(database),
            index_dir: Some(ee_dir.join("index")),
            query: "Acme Corp".to_owned(),
            limit: 10,
            speed: crate::search::SpeedMode::Instant,
            explain: false,
            as_of: None,
            include_tombstoned: false,
            include_expired: false,
            include_future: false,
            include_stale: false,
            relevance_floor: Some(0.0),
            dedup_mode: crate::core::search::SearchDedupMode::DocId,
            source_mode: crate::core::search::SearchSourceMode::LexicalOnly,
            strict_source_mode: true,
            memory_scope: crate::models::MemoryScope::Team,
            strict_scope: false,
        })
        .expect("search");
        let hit = search
            .results
            .iter()
            .find(|hit| hit.doc_id == stub_id)
            .unwrap_or_else(|| {
                panic!(
                    "ee search --memory-scope team must return the hydrated teammate memory: {search:?}"
                )
            });
        let provenance = hit
            .metadata
            .as_ref()
            .and_then(|metadata| metadata.get("teamProvenance"))
            .expect("search hit must carry teamProvenance");
        assert_eq!(provenance["schema"], "ee.team.provenance.v1");
        assert_eq!(provenance["memberDisplayName"], "Analysts");
        assert_eq!(provenance["originTimeAssurance"], "member_attested");
        assert_eq!(provenance["producedAt"], "2026-08-13T11:00:00Z");

        let packed =
            crate::core::context::run_context_pack(&crate::core::context::ContextPackOptions {
                workspace_path: workspace.path().to_path_buf(),
                database_path: Some(ee_dir.join("ee.db")),
                index_dir: Some(ee_dir.join("index")),
                query: "Acme Corp".to_owned(),
                speed: crate::search::SpeedMode::Instant,
                source_mode: crate::core::search::SearchSourceMode::LexicalOnly,
                strict_source_mode: true,
                filters: crate::models::QueryFilters::default(),
                profile: Some(crate::pack::ContextPackProfile::Balanced),
                max_tokens: Some(800),
                candidate_pool: Some(8),
                max_results: Some(4),
                include_tombstoned: false,
                as_of: None,
                include_expired: false,
                include_future: false,
                include_stale: false,
                relevance_floor: Some(0.0),
                redaction_level: crate::models::RedactionLevel::Minimal,
                memory_scope: crate::models::MemoryScope::Team,
                strict_scope: false,
                ppr_weight: None,
                changed_symbols: Vec::new(),
                changed_symbols_from_git: false,
                pagination: None,
                coordination_snapshot_path: None,
                coordination_stale_after_ms: crate::pack::DEFAULT_COORDINATION_STALE_AFTER_MS,
                task_lens: None,
                require_fresh_sentinels: false,
                output_options: crate::core::context::ContextPackOutputOptions::default(),
                persist_pack: false,
                baseline_write: None,
                no_lod: true,
            })
            .expect("pack");
        assert!(
            packed
                .data
                .pack
                .items
                .iter()
                .any(|item| item.memory_id.to_string() == stub_id),
            "ee pack --memory-scope team must select the hydrated teammate memory: items={:?} degraded={:?}",
            packed
                .data
                .pack
                .items
                .iter()
                .map(|item| item.memory_id.to_string())
                .collect::<Vec<_>>(),
            packed.data.degraded
        );
        let pack_json = crate::output::render_context_response_json(&packed);
        assert!(
            pack_json.contains("\"teamProvenance\"") && pack_json.contains("Analysts"),
            "ee pack --json must emit teamProvenance for the teammate item: {pack_json}"
        );
    }

    #[test]
    fn reconcile_inbound_team_memories_replays_allowed_import_ledger_events() {
        let connection = open_db();
        connection
            .insert_workspace(
                "wsp_persistfixture000000000001",
                &crate::db::CreateWorkspaceInput {
                    path: "/tmp/ee-team-inbound".to_owned(),
                    name: Some("inbound".to_owned()),
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
        let stub_id = inbound_team_memory_id(&inbound.event_hash).expect("id");
        connection
            .insert_mesh_import_ledger_event(&crate::db::InsertMeshImportLedgerEventInput {
                workspace_id: "wsp_persistfixture000000000001".to_owned(),
                event_id: format!(
                    "mesh_evt_{}",
                    inbound
                        .event_hash
                        .trim_start_matches("blake3:")
                        .chars()
                        .take(24)
                        .collect::<String>()
                ),
                origin_node_id: inbound.origin_node_id.clone(),
                origin_workspace_id: "wsp_persistfixture000000000001".to_owned(),
                producer_peer_id: None,
                seq: inbound.seq.max(1),
                prev_event_hash: inbound.prev_event_hash.clone(),
                event_hash: inbound.event_hash.clone(),
                event_kind: "create".to_owned(),
                logical_memory_id: format!(
                    "mem_{}",
                    inbound
                        .event_hash
                        .trim_start_matches("blake3:")
                        .chars()
                        .take(24)
                        .collect::<String>()
                ),
                content_hash: inbound.event_hash.clone(),
                material_lane: "metadata".to_owned(),
                redaction_class: "metadataOnly".to_owned(),
                trust_lane: "peerAgent".to_owned(),
                import_decision: "allow".to_owned(),
                local_memory_id: None,
                body_cache_key: None,
                policy_failure_surface_json: None,
                policy_decision_json: None,
                event_json: serde_json::to_string(&inbound).expect("json"),
                imported_at: None,
            })
            .expect("ledger");
        let sick =
            inspect_team_health(&connection, "wsp_persistfixture000000000001", None).expect("sick");
        assert!(sick.checks.iter().any(|check| {
            check.name == "inbound_memories"
                && check.status == "warning"
                && check.repair.as_deref() == Some("ee team steward once --workspace .")
        }));
        let first = reconcile_inbound_team_memories(&connection, "wsp_persistfixture000000000001")
            .expect("reconcile");
        assert_eq!(first.applied_additions, 1);
        let stored = connection.get_memory(&stub_id).expect("get").expect("row");
        assert!(stored.content.starts_with("[ee.team.history]"));
        assert!(!stored.content.contains("secret body"));
        let jobs = connection
            .list_search_index_jobs(
                "wsp_persistfixture000000000001",
                Some(crate::db::SearchIndexJobStatus::Pending),
            )
            .expect("jobs");
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].document_id.as_deref(), None);
        assert_eq!(jobs[0].document_source.as_deref(), Some("memory"));
        assert_eq!(jobs[0].job_type, "incremental");
        let again = reconcile_inbound_team_memories(&connection, "wsp_persistfixture000000000001")
            .expect("idempotent");
        assert_eq!(again.applied_additions, 0);
        let healthy =
            inspect_team_health(&connection, "wsp_persistfixture000000000001", None).expect("ok");
        assert!(
            healthy
                .checks
                .iter()
                .any(|check| check.name == "inbound_memories" && check.status == "ok")
        );
    }

    #[test]
    fn reconcile_existing_inbound_stub_retries_index_enqueue() {
        let connection = open_db();
        connection
            .insert_workspace(
                "wsp_persistfixture000000000001",
                &crate::db::CreateWorkspaceInput {
                    path: "/tmp/ee-team-inbound-retry".to_owned(),
                    name: Some("inbound-retry".to_owned()),
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
        let stub_id = inbound_team_memory_id(&inbound.event_hash).expect("id");
        connection
            .insert_memory(
                &stub_id,
                &crate::db::CreateMemoryInput {
                    workspace_id: "wsp_persistfixture000000000001".to_owned(),
                    level: "semantic".to_owned(),
                    kind: "rule".to_owned(),
                    content: format!("[ee.team.history] rule {}", inbound.event_hash),
                    workflow_id: None,
                    confidence: 0.5,
                    utility: 0.5,
                    importance: 0.5,
                    provenance_uri: Some(inbound.event_id.clone()),
                    trust_class: "peer_human_attested".to_owned(),
                    trust_subclass: Some(
                        "agent:Analysts; produced_at=2026-08-13T07:01:00Z".to_owned(),
                    ),
                    tags: Vec::new(),
                    valid_from: None,
                    valid_to: None,
                },
            )
            .expect("preexisting stub");
        connection
            .insert_mesh_import_ledger_event(&crate::db::InsertMeshImportLedgerEventInput {
                workspace_id: "wsp_persistfixture000000000001".to_owned(),
                event_id: format!(
                    "mesh_evt_{}",
                    inbound
                        .event_hash
                        .trim_start_matches("blake3:")
                        .chars()
                        .take(24)
                        .collect::<String>()
                ),
                origin_node_id: inbound.origin_node_id.clone(),
                origin_workspace_id: "wsp_persistfixture000000000001".to_owned(),
                producer_peer_id: None,
                seq: inbound.seq.max(1),
                prev_event_hash: inbound.prev_event_hash.clone(),
                event_hash: inbound.event_hash.clone(),
                event_kind: "create".to_owned(),
                logical_memory_id: stub_id.clone(),
                content_hash: inbound.event_hash.clone(),
                material_lane: "metadata".to_owned(),
                redaction_class: "metadataOnly".to_owned(),
                trust_lane: "peerAgent".to_owned(),
                import_decision: "allow".to_owned(),
                local_memory_id: Some(stub_id.clone()),
                body_cache_key: None,
                policy_failure_surface_json: None,
                policy_decision_json: None,
                event_json: serde_json::to_string(&inbound).expect("json"),
                imported_at: None,
            })
            .expect("ledger");
        assert!(
            connection
                .list_search_index_jobs("wsp_persistfixture000000000001", None)
                .expect("jobs")
                .is_empty(),
            "fixture must start with no index job"
        );
        let report = reconcile_inbound_team_memories(&connection, "wsp_persistfixture000000000001")
            .expect("reconcile");
        assert_eq!(report.applied_additions, 0);
        let jobs = connection
            .list_search_index_jobs(
                "wsp_persistfixture000000000001",
                Some(crate::db::SearchIndexJobStatus::Pending),
            )
            .expect("jobs");
        assert_eq!(
            jobs.len(),
            1,
            "steward rematerialize must retry Incremental enqueue for an already-projected stub: {jobs:?}"
        );
        assert_eq!(jobs[0].document_source.as_deref(), Some("memory"));
        assert_eq!(jobs[0].job_type, "incremental");
        assert!(jobs[0].document_id.is_none());
    }

    #[cfg(unix)]
    #[test]
    fn inbound_body_placeholder_verifies_nonce_before_publication() {
        use crate::mesh::bootstrap_envelope::{BODY_FETCH_RESPONSE_SCHEMA_V1, BodyFetchResponse};

        let producer_dir = tempfile::tempdir().unwrap();
        let producer = open_db();
        producer
            .insert_workspace(
                "wsp_persistfixture000000000001",
                &crate::db::CreateWorkspaceInput {
                    path: producer_dir.path().display().to_string(),
                    name: Some("producer".to_owned()),
                },
            )
            .expect("workspace");
        let created = create_local_team_with_store(
            &producer,
            "wsp_persistfixture000000000001",
            "Analysts",
            "2026-08-13T00:00:00Z",
            Some(producer_dir.path()),
        )
        .expect("create");
        producer
            .insert_memory(
                "mem_teambodies0000000000000001",
                &crate::db::CreateMemoryInput {
                    workspace_id: "wsp_persistfixture000000000001".to_owned(),
                    level: "semantic".to_owned(),
                    kind: "note".to_owned(),
                    content: "secret body stays off the metadata event".to_owned(),
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
        share_team_bodies(
            &producer,
            "wsp_persistfixture000000000001",
            "2026-08-13T15:00:00Z",
            true,
            8,
            Some(producer_dir.path()),
            false,
            None,
        )
        .expect("publish");
        let rows = producer
            .list_mesh_origin_events(&created.team.team_id, &created.team.origin_node_id, 0, 16)
            .expect("chain");
        let memory_row = rows
            .iter()
            .find(|row| row.payload_schema == "ee.mesh.memory_event.v1")
            .expect("memory event");
        let inbound = crate::mesh::origin_stream::inbound_from_stored(memory_row).expect("inbound");

        let receiver_dir = tempfile::tempdir().unwrap();
        let receiver = open_db();
        receiver
            .insert_workspace(
                "wsp_persistfixture000000000001",
                &crate::db::CreateWorkspaceInput {
                    path: receiver_dir.path().display().to_string(),
                    name: Some("receiver".to_owned()),
                },
            )
            .expect("receiver workspace");
        create_local_team(
            &receiver,
            "wsp_persistfixture000000000001",
            "Analysts",
            "2026-08-13T00:00:00Z",
        )
        .expect("receiver team");
        let projected =
            project_inbound_team_memory(&receiver, "wsp_persistfixture000000000001", &inbound)
                .expect("project")
                .expect("id");
        assert_ne!(projected, "mem_teambodies0000000000000001");
        let key = team_body_cache_key("mem_teambodies0000000000000001");
        assert_eq!(
            pending_team_body_fetch_keys(&receiver, "wsp_persistfixture000000000001")
                .expect("pending"),
            vec![key.clone()]
        );
        let sick =
            inspect_team_health(&receiver, "wsp_persistfixture000000000001", None).expect("sick");
        assert!(sick.checks.iter().any(|check| {
            check.name == "inbound_body_fetches"
                && check.status == "warning"
                && check.repair.as_deref() == Some("ee team steward once --workspace .")
        }));

        let fetched = fetch_local_team_body(
            &producer,
            "wsp_persistfixture000000000001",
            producer_dir.path(),
            &key,
        )
        .expect("producer fetch");
        assert_eq!(fetched.cache_status, "available");
        assert!(fetched.nonce_hex.is_some());

        let wrong = apply_fetched_team_body(
            &receiver,
            "wsp_persistfixture000000000001",
            receiver_dir.path(),
            &BodyFetchResponse {
                schema: BODY_FETCH_RESPONSE_SCHEMA_V1.to_owned(),
                body_cache_key: key.clone(),
                cache_status: "available".to_owned(),
                size_bytes: 4,
                body_hex: Some(hex_encode(b"nope")),
                nonce_hex: fetched.nonce_hex.clone(),
            },
        )
        .expect("mismatch");
        assert_eq!(wrong.cache_status, "quarantined");
        assert!(wrong.body_hex.is_none());
        assert!(wrong.nonce_hex.is_none());

        receiver
            .upsert_mesh_body_cache_metadata(&UpsertMeshBodyCacheMetadataInput {
                workspace_id: "wsp_persistfixture000000000001".to_owned(),
                body_cache_key: key.clone(),
                origin_node_id: inbound.origin_node_id.clone(),
                origin_workspace_id: "wsp_persistfixture000000000001".to_owned(),
                logical_memory_id: "mem_teambodies0000000000000001".to_owned(),
                content_hash: serde_json::from_value::<MemoryEventPayload>(inbound.payload.clone())
                    .expect("payload")
                    .body_commitment,
                body_ref_json: Some(
                    serde_json::json!({ "originEventId": inbound.event_id }).to_string(),
                ),
                preview_hash: None,
                size_bytes: None,
                cache_status: "metadata_only".to_owned(),
                local_body_hash: None,
                cached_at: Some("2026-08-13T15:01:00Z".to_owned()),
                expires_at: None,
            })
            .expect("reset placeholder");

        let applied = apply_fetched_team_body(
            &receiver,
            "wsp_persistfixture000000000001",
            receiver_dir.path(),
            &fetched,
        )
        .expect("apply");
        assert_eq!(applied.cache_status, "available");
        assert_eq!(
            applied.body_hex.as_deref(),
            Some(hex_encode(b"secret body stays off the metadata event").as_str())
        );
        assert!(
            pending_team_body_fetch_keys(&receiver, "wsp_persistfixture000000000001")
                .expect("drained")
                .is_empty()
        );
        let local = fetch_local_team_body(
            &receiver,
            "wsp_persistfixture000000000001",
            receiver_dir.path(),
            &key,
        )
        .expect("receiver fetch");
        assert_eq!(local.cache_status, "available");
        assert_eq!(local.body_hex, applied.body_hex);
        let hydrated = receiver
            .get_memory(&projected)
            .expect("hydrated get")
            .expect("hydrated row");
        assert_eq!(
            hydrated.content, "secret body stays off the metadata event",
            "authorized BodyFetch must hydrate the team stub so search/pack can recall it"
        );
        assert_eq!(hydrated.trust_class, "peer_human_attested");
    }

    #[cfg(unix)]
    #[test]
    fn retry_pending_team_body_fetches_requires_a_body_lane_grant() {
        use crate::mesh::bootstrap_envelope::{BODY_FETCH_RESPONSE_SCHEMA_V1, BodyFetchResponse};

        let receiver_dir = tempfile::tempdir().unwrap();
        let receiver = open_db();
        receiver
            .insert_workspace(
                "wsp_persistfixture000000000001",
                &crate::db::CreateWorkspaceInput {
                    path: receiver_dir.path().display().to_string(),
                    name: Some("retry".to_owned()),
                },
            )
            .expect("workspace");
        let created = create_local_team(
            &receiver,
            "wsp_persistfixture000000000001",
            "Analysts",
            "2026-08-13T00:00:00Z",
        )
        .expect("create");
        let nonce = [7_u8; 32];
        let body = b"granted fetch lands after retry";
        let commitment = body_commitment(&nonce, body);
        let key = team_body_cache_key("mem_teambodies0000000000000001");
        receiver
            .upsert_mesh_body_cache_metadata(&UpsertMeshBodyCacheMetadataInput {
                workspace_id: "wsp_persistfixture000000000001".to_owned(),
                body_cache_key: key.clone(),
                origin_node_id: created.team.origin_node_id.clone(),
                origin_workspace_id: "wsp_persistfixture000000000001".to_owned(),
                logical_memory_id: "mem_teambodies0000000000000001".to_owned(),
                content_hash: commitment,
                body_ref_json: None,
                preview_hash: None,
                size_bytes: None,
                cache_status: "metadata_only".to_owned(),
                local_body_hash: None,
                cached_at: Some("2026-08-13T16:00:00Z".to_owned()),
                expires_at: None,
            })
            .expect("placeholder");
        let peer_id = enroll_team_pair_peer(
            &receiver,
            "wsp_persistfixture000000000001",
            &created.team.team_id,
            &created.team.origin_node_id,
            "Analysts",
            "127.0.0.1",
            created.team.hello_port,
            "2026-08-13T16:00:00Z",
            "wsp_joinworkspace0000000000001",
        )
        .expect("enroll");
        let mut fetch_calls = 0_usize;
        let denied = retry_pending_team_body_fetches(
            &receiver,
            "wsp_persistfixture000000000001",
            receiver_dir.path(),
            |_, _| {
                fetch_calls = fetch_calls.saturating_add(1);
                None
            },
        )
        .expect("denied");
        assert_eq!(denied, 0);
        assert_eq!(fetch_calls, 0);
        let mutation = crate::db::MeshLaneGrantMutationInput {
            workspace_id: "wsp_persistfixture000000000001".to_owned(),
            peer_id: peer_id.clone(),
            target_adapter: crate::db::MeshLaneGrantTargetAdapter::new(
                &peer_id,
                &created.team.origin_node_id,
            ),
            material_lane: crate::config::MeshLane::Body,
            expected_generation: 0,
            approval_config_digest: Some(format!("blake3:{}", "b".repeat(64))),
            updated_at: Some("2026-08-13T16:01:00Z".to_owned()),
        };
        receiver
            .apply_mesh_lane_grant_with_effect(&mutation, |_| Ok::<(), String>(()))
            .expect("grant");
        let fetched = BodyFetchResponse {
            schema: BODY_FETCH_RESPONSE_SCHEMA_V1.to_owned(),
            body_cache_key: key,
            cache_status: "available".to_owned(),
            size_bytes: u64::try_from(body.len()).unwrap(),
            body_hex: Some(hex_encode(body)),
            nonce_hex: Some(hex_encode(&nonce)),
        };
        let applied = retry_pending_team_body_fetches(
            &receiver,
            "wsp_persistfixture000000000001",
            receiver_dir.path(),
            |granted_peer, _| {
                fetch_calls = fetch_calls.saturating_add(1);
                (granted_peer == peer_id).then(|| fetched.clone())
            },
        )
        .expect("retry");
        assert_eq!(applied, 1);
        assert_eq!(fetch_calls, 1);
        assert!(
            pending_team_body_fetch_keys(&receiver, "wsp_persistfixture000000000001")
                .expect("drained")
                .is_empty()
        );
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
            origin_workspace_id: "wsp_persistfixture000000000001".to_owned(),
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
        let doctor = inspect_team_health(&connection, "wsp_persistfixture000000000001", None)
            .expect("doctor");
        assert!(doctor.checks.iter().any(|check| {
            check.name == "removal_acknowledgements"
                && check.status == "ok"
                && check.message.contains("signed removal")
        }));
    }

    #[test]
    fn removal_acknowledgement_matrix_stays_pending_until_audience_applies() {
        let connection = open_db();
        let created = create_local_team(
            &connection,
            "wsp_persistfixture000000000001",
            "Analysts",
            "2026-08-13T00:00:00Z",
        )
        .expect("create");
        persist_team_member(
            &connection,
            "wsp_persistfixture000000000002",
            &created.team.team_id,
            "node_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "Priya",
            false,
            "invite_ceremony",
            "2026-08-13T01:00:00Z",
            None,
        )
        .expect("peer a");
        let target = persist_team_member(
            &connection,
            "wsp_persistfixture000000000003",
            &created.team.team_id,
            "node_bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            "Omar",
            false,
            "invite_ceremony",
            "2026-08-13T01:05:00Z",
            None,
        )
        .expect("peer b");
        remove_team_member(
            &connection,
            "wsp_persistfixture000000000001",
            &target,
            "2026-08-13T02:00:00Z",
            None,
        )
        .expect("remove");
        let acks = connection
            .list_team_removal_acks(&created.team.team_id)
            .expect("acks");
        assert_eq!(acks.len(), 2);
        assert_eq!(
            acks.iter()
                .filter(|ack| ack.acknowledged_at.is_none())
                .count(),
            1
        );
        let sick =
            inspect_team_health(&connection, "wsp_persistfixture000000000001", None).expect("sick");
        assert!(sick.checks.iter().any(|check| {
            check.name == "removal_acknowledgements"
                && check.status == "warning"
                && check.repair.as_deref() == Some("ee team steward once --workspace .")
        }));
        let listed = local_team_status(&connection).expect("status");
        assert_eq!(listed.pending_removal_acks.len(), 1);
        assert_eq!(
            listed.pending_removal_acks[0].audience_origin_node_id,
            "node_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
        );
        assert!(listed.admission.local_tier1_unaffected);
        assert_eq!(listed.admission.max_event_batch_count, 512);
        assert_eq!(listed.admission.max_body_fetch_bytes, 512 * 1024);
        let marked = acknowledge_team_removal_audience(
            &connection,
            "node_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            32,
            "2026-08-13T02:30:00Z",
        )
        .expect("ack");
        assert_eq!(marked, 1);
        let healthy = inspect_team_health(&connection, "wsp_persistfixture000000000001", None)
            .expect("healthy");
        assert!(
            healthy
                .checks
                .iter()
                .any(|check| check.name == "removal_acknowledgements" && check.status == "ok")
        );
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
            None,
            None,
            None,
            None,
        )
        .expect("activity");
        assert!(report.event_count >= 2);
        assert!(report.sequence_complete);
        assert_eq!(report.time_filter_basis, "as_of");
        assert!(report.events.iter().any(|item| item.kind == "rule"));
        assert!(report.events.iter().any(|item| item.kind == "teamCreated"));
        assert!(report.events.iter().all(|item| !item.body_available));
        let json = serde_json::to_string(&report).expect("json");
        assert!(!json.contains("secret body must stay off activity"));
    }

    #[test]
    fn list_team_activity_attributes_hydrated_inbound_memory() {
        let connection = open_db();
        connection
            .insert_workspace(
                "wsp_persistfixture000000000001",
                &crate::db::CreateWorkspaceInput {
                    path: "/tmp/ee-team-activity-inbound".to_owned(),
                    name: Some("activity".to_owned()),
                },
            )
            .expect("workspace");
        create_local_team(
            &connection,
            "wsp_persistfixture000000000001",
            "Priya",
            "2026-08-13T00:00:00Z",
        )
        .expect("create");
        connection
            .insert_memory(
                "mem_teaminboundact000000000001",
                &crate::db::CreateMemoryInput {
                    workspace_id: "wsp_persistfixture000000000001".to_owned(),
                    level: "semantic".to_owned(),
                    kind: "note".to_owned(),
                    content: "Acme Corp analysis from Hana".to_owned(),
                    workflow_id: None,
                    confidence: 0.5,
                    utility: 0.5,
                    importance: 0.5,
                    provenance_uri: Some("evt_teaminboundact".to_owned()),
                    trust_class: "peer_human_attested".to_owned(),
                    trust_subclass: Some("agent:Hana; produced_at=2026-08-13T11:00:00Z".to_owned()),
                    tags: Vec::new(),
                    valid_from: None,
                    valid_to: None,
                },
            )
            .expect("inbound");
        connection
            .insert_memory(
                "mem_teaminboundstub00000000001",
                &crate::db::CreateMemoryInput {
                    workspace_id: "wsp_persistfixture000000000001".to_owned(),
                    level: "semantic".to_owned(),
                    kind: "note".to_owned(),
                    content: "[ee.team.history] note blake3:deadbeef".to_owned(),
                    workflow_id: None,
                    confidence: 0.5,
                    utility: 0.5,
                    importance: 0.5,
                    provenance_uri: Some("evt_teaminboundstub".to_owned()),
                    trust_class: "peer_human_attested".to_owned(),
                    trust_subclass: Some("agent:Hana; produced_at=2026-08-13T10:00:00Z".to_owned()),
                    tags: Vec::new(),
                    valid_from: None,
                    valid_to: None,
                },
            )
            .expect("stub");
        let report = list_team_activity(
            &connection,
            "wsp_persistfixture000000000001",
            "2026-08-13T12:00:00Z",
            100,
            None,
            None,
            None,
            None,
        )
        .expect("activity");
        let inbound = report
            .events
            .iter()
            .find(|item| item.source == "inbound_projection")
            .expect("inbound activity");
        assert_eq!(inbound.member_display_name, "Hana");
        assert_eq!(inbound.produced_at, "2026-08-13T11:00:00Z");
        assert!(inbound.body_available);
        assert_eq!(inbound.kind, "note");
        let stub = report
            .events
            .iter()
            .find(|item| item.event_id == "evt_teaminboundstub")
            .expect("stub activity");
        assert_eq!(stub.member_display_name, "Hana");
        assert!(!stub.body_available);
        let json = serde_json::to_string(&report).expect("json");
        assert!(
            !json.contains("Acme Corp analysis from Hana"),
            "activity must not leak teammate body text: {json}"
        );
        let hana = list_team_activity(
            &connection,
            "wsp_persistfixture000000000001",
            "2026-08-13T12:00:00Z",
            100,
            Some("Hana"),
            None,
            None,
            None,
        )
        .expect("member filter");
        assert!(
            hana.events
                .iter()
                .all(|item| item.member_display_name == "Hana")
        );
        assert!(hana.events.iter().any(|item| item.body_available));
    }

    #[test]
    fn list_team_activity_filters_by_project_binding() {
        let connection = open_db();
        connection
            .insert_workspace(
                "wsp_persistfixture000000000001",
                &crate::db::CreateWorkspaceInput {
                    path: "/tmp/acme-analysis".to_owned(),
                    name: Some("acme".to_owned()),
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
        share_team_project(
            &connection,
            "wsp_persistfixture000000000001",
            "acme-analysis",
            "/tmp/acme-analysis",
            "2026-08-13T13:00:00Z",
            None,
        )
        .expect("project");
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
            "2026-08-13T14:00:00Z",
            true,
            16,
            None,
        )
        .expect("share");
        let by_project = list_team_activity(
            &connection,
            "wsp_persistfixture000000000001",
            "2026-08-13T15:00:00Z",
            100,
            None,
            Some("acme-analysis"),
            None,
            None,
        )
        .expect("project filter");
        assert!(
            by_project.events.iter().any(|item| {
                item.kind == "rule" && item.project.as_deref() == Some("acme-analysis")
            }),
            "shared history must carry the workspace project: {:?}",
            by_project.events
        );
        assert!(
            by_project
                .events
                .iter()
                .any(|item| item.kind == "teamProjectShared")
        );
        let other = list_team_activity(
            &connection,
            "wsp_persistfixture000000000001",
            "2026-08-13T15:00:00Z",
            100,
            None,
            Some("other-project"),
            None,
            None,
        )
        .expect("other");
        assert!(
            !other.events.iter().any(|item| item.kind == "rule"),
            "unrelated project filter must not return the shared rule"
        );
    }

    #[test]
    fn list_team_activity_since_excludes_earlier_events_and_labels_incompleteness() {
        let connection = open_db();
        connection
            .insert_workspace(
                "wsp_persistfixture000000000001",
                &crate::db::CreateWorkspaceInput {
                    path: "/tmp/ee-team-activity-since".to_owned(),
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
        let since = list_team_activity(
            &connection,
            "wsp_persistfixture000000000001",
            "2026-08-13T12:00:00Z",
            100,
            None,
            None,
            Some("2026-08-13T10:00:00Z"),
            None,
        )
        .expect("since");
        assert_eq!(since.time_filter_basis, "member_attested");
        assert!(!since.sequence_complete);
        assert_eq!(since.since.as_deref(), Some("2026-08-13T10:00:00Z"));
        assert!(since.events.iter().any(|item| item.kind == "rule"));
        assert!(
            !since.events.iter().any(|item| item.kind == "teamCreated"),
            "teamCreated at 00:00 must fall before --since 10:00: {since:?}"
        );
        let rejected = list_team_activity(
            &connection,
            "wsp_persistfixture000000000001",
            "2026-08-13T12:00:00Z",
            100,
            None,
            None,
            Some("2h"),
            None,
        )
        .expect_err("relative");
        assert!(rejected.to_string().contains("RFC 3339"));
    }

    #[test]
    fn list_team_activity_cursor_pages_without_overlap() {
        let connection = open_db();
        connection
            .insert_workspace(
                "wsp_persistfixture000000000001",
                &crate::db::CreateWorkspaceInput {
                    path: "/tmp/ee-team-activity-cursor".to_owned(),
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
        let first = list_team_activity(
            &connection,
            "wsp_persistfixture000000000001",
            "2026-08-13T12:00:00Z",
            1,
            None,
            None,
            None,
            None,
        )
        .expect("page 1");
        assert_eq!(first.event_count, 1);
        let cursor = first.next_cursor.clone().expect("nextCursor");
        let second = list_team_activity(
            &connection,
            "wsp_persistfixture000000000001",
            "2026-08-13T12:00:00Z",
            1,
            None,
            None,
            None,
            Some(cursor.as_str()),
        )
        .expect("page 2");
        assert_eq!(second.event_count, 1);
        assert_ne!(first.events[0].event_id, second.events[0].event_id);
        assert!(second.cursor_error.is_none());
        let invalid = list_team_activity(
            &connection,
            "wsp_persistfixture000000000001",
            "2026-08-13T12:00:00Z",
            1,
            None,
            None,
            None,
            Some("not-a-cursor"),
        )
        .expect("invalid");
        assert!(invalid.events.is_empty());
        assert_eq!(invalid.cursor_error, Some("cursor_invalid"));
        let mismatched = list_team_activity(
            &connection,
            "wsp_persistfixture000000000001",
            "2026-08-13T12:00:00Z",
            2,
            None,
            None,
            None,
            Some(cursor.as_str()),
        )
        .expect("params mismatch");
        assert!(mismatched.events.is_empty());
        assert_eq!(mismatched.cursor_error, Some("cursor_invalid"));
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

    #[test]
    fn reconcile_rematerializes_origin_project_shares() {
        let connection = open_db();
        let created = create_local_team(
            &connection,
            "wsp_persistfixture000000000001",
            "Analysts",
            "2026-08-13T00:00:00Z",
        )
        .expect("create");
        let project_id = "prj_tm_reconcile00000000000000001";
        let payload = OriginEventPayload::Manifest(ManifestEventPayload {
            operation: TEAM_PROJECT_SHARED_OPERATION.to_owned(),
            document_id: "tdoc_projectreconcile000001".to_owned(),
            predecessor_revision_id: None,
            document_payload: serde_json::json!({
                "projectId": project_id,
                "displayName": "acme-analysis",
            }),
        });
        let signer = LocalOriginSigner::for_workspace("wsp_persistfixture000000000001");
        append_origin_event(
            &connection,
            &signer,
            &OriginAppendRequest {
                team_id: &created.team.team_id,
                origin_node_id: &created.team.origin_node_id,
                payload,
                required_features: Vec::new(),
                produced_at: "2026-08-13T13:30:00Z",
                body_nonce: None,
            },
        )
        .expect("append");
        assert_eq!(
            list_team_projects(&connection)
                .expect("empty")
                .project_count,
            0
        );
        let sick =
            inspect_team_health(&connection, "wsp_persistfixture000000000001", None).expect("sick");
        assert!(sick.checks.iter().any(|check| {
            check.name == "projects"
                && check.status == "warning"
                && check.repair.as_deref() == Some("ee team projects reconcile --workspace .")
        }));
        let first = reconcile_local_team_projects(&connection).expect("reconcile");
        assert_eq!(first.applied_additions, 1);
        let listed = list_team_projects(&connection).expect("listed");
        assert_eq!(listed.project_count, 1);
        assert_eq!(listed.projects[0].project_id, project_id);
        assert_eq!(listed.projects[0].source, "reconciled");
        assert!(listed.projects[0].local_path.is_empty());
        let again = reconcile_local_team_projects(&connection).expect("idempotent");
        assert_eq!(again.applied_additions, 0);
        let healthy = inspect_team_health(&connection, "wsp_persistfixture000000000001", None)
            .expect("healthy");
        assert!(
            healthy
                .checks
                .iter()
                .any(|check| check.name == "projects" && check.status == "ok")
        );
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
        assert!(
            fetched
                .nonce_hex
                .as_deref()
                .is_some_and(|nonce| nonce.len() == 64),
            "authorized serve must release the commitment nonce"
        );
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
        let key = team_body_cache_key("mem_teambodies0000000000000001");
        let existing = connection
            .get_mesh_body_cache_metadata("wsp_persistfixture000000000001", &key)
            .expect("row")
            .expect("present");
        connection
            .upsert_mesh_body_cache_metadata(&UpsertMeshBodyCacheMetadataInput {
                workspace_id: existing.workspace_id,
                body_cache_key: existing.body_cache_key.clone(),
                origin_node_id: existing.origin_node_id,
                origin_workspace_id: existing.origin_workspace_id,
                logical_memory_id: existing.logical_memory_id,
                content_hash: existing.content_hash,
                body_ref_json: existing.body_ref_json,
                preview_hash: existing.preview_hash,
                size_bytes: existing.size_bytes,
                cache_status: "staging".to_owned(),
                local_body_hash: existing.local_body_hash,
                cached_at: Some("2026-08-13T14:04:00Z".to_owned()),
                expires_at: existing.expires_at,
            })
            .expect("stage");
        let folded = reconcile_team_body_cache(
            &connection,
            "wsp_persistfixture000000000001",
            Some(workspace.path()),
            "2026-08-13T14:05:00Z",
        )
        .expect("reconcile");
        assert_eq!(folded, 1);
        let after = connection
            .get_mesh_body_cache_metadata("wsp_persistfixture000000000001", &key)
            .expect("reload")
            .expect("present");
        assert_eq!(after.cache_status, "available");
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

    #[cfg(unix)]
    #[test]
    fn body_cache_crash_boundaries_and_tokens_stay_unlinkable() {
        let workspace = tempfile::tempdir().unwrap();
        let connection = open_db();
        connection
            .insert_workspace(
                "wsp_persistfixture000000000001",
                &crate::db::CreateWorkspaceInput {
                    path: workspace.path().display().to_string(),
                    name: Some("bodies-crash".to_owned()),
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
                "mem_teamcrash00000000000000001",
                &crate::db::CreateMemoryInput {
                    workspace_id: "wsp_persistfixture000000000001".to_owned(),
                    level: "semantic".to_owned(),
                    kind: "note".to_owned(),
                    content: "crash-boundary secret body".to_owned(),
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

        let first = share_team_bodies(
            &connection,
            "wsp_persistfixture000000000001",
            "2026-08-13T18:00:00Z",
            false,
            16,
            Some(workspace.path()),
            true,
            None,
        )
        .expect("preview-one");
        let second = share_team_bodies(
            &connection,
            "wsp_persistfixture000000000001",
            "2026-08-13T18:00:01Z",
            false,
            16,
            Some(workspace.path()),
            true,
            None,
        )
        .expect("preview-two");
        let token_one = first.approval_token.clone().expect("token-one");
        let token_two = second.approval_token.clone().expect("token-two");
        assert_ne!(token_one, token_two);
        assert_eq!(first.consent_hash, second.consent_hash);
        for report in [&first, &second] {
            let json = serde_json::to_string(report).expect("json");
            assert!(!json.contains("crash-boundary secret body"));
            assert!(!json.contains("tokenId"));
            assert!(!json.contains("token_id"));
        }

        let malformed = share_team_bodies(
            &connection,
            "wsp_persistfixture000000000001",
            "2026-08-13T18:00:02Z",
            true,
            16,
            Some(workspace.path()),
            false,
            Some("eeap1_not-a-real-mac"),
        )
        .expect_err("malformed");
        let malformed_text = malformed.to_string();
        assert!(
            malformed_text.contains("invalid")
                || malformed_text.contains("authentic")
                || malformed_text.contains("MAC")
                || malformed_text.contains("token")
        );
        assert!(!malformed_text.contains("crash-boundary secret body"));

        let published = share_team_bodies(
            &connection,
            "wsp_persistfixture000000000001",
            "2026-08-13T18:01:00Z",
            true,
            16,
            Some(workspace.path()),
            false,
            Some(token_two.as_str()),
        )
        .expect("publish");
        assert_eq!(published.published_count, 1);

        let key = team_body_cache_key("mem_teamcrash00000000000000001");
        let existing = connection
            .get_mesh_body_cache_metadata("wsp_persistfixture000000000001", &key)
            .expect("row")
            .expect("present");
        connection
            .upsert_mesh_body_cache_metadata(&UpsertMeshBodyCacheMetadataInput {
                workspace_id: existing.workspace_id.clone(),
                body_cache_key: existing.body_cache_key.clone(),
                origin_node_id: existing.origin_node_id.clone(),
                origin_workspace_id: existing.origin_workspace_id.clone(),
                logical_memory_id: existing.logical_memory_id.clone(),
                content_hash: existing.content_hash.clone(),
                body_ref_json: existing.body_ref_json.clone(),
                preview_hash: existing.preview_hash.clone(),
                size_bytes: existing.size_bytes,
                cache_status: "invalidated_pending_purge".to_owned(),
                local_body_hash: existing.local_body_hash.clone(),
                cached_at: Some("2026-08-13T18:02:00Z".to_owned()),
                expires_at: existing.expires_at.clone(),
            })
            .expect("invalidate");
        let mid = fetch_local_team_body(
            &connection,
            "wsp_persistfixture000000000001",
            workspace.path(),
            &key,
        )
        .expect("mid-fetch");
        assert_eq!(mid.cache_status, "invalidated_pending_purge");
        assert!(mid.body_hex.is_none());
        let folded = reconcile_team_body_cache(
            &connection,
            "wsp_persistfixture000000000001",
            Some(workspace.path()),
            "2026-08-13T18:03:00Z",
        )
        .expect("reconcile-invalidate");
        assert_eq!(folded, 1);
        let after_purge = fetch_local_team_body(
            &connection,
            "wsp_persistfixture000000000001",
            workspace.path(),
            &key,
        )
        .expect("post-purge");
        assert_eq!(after_purge.cache_status, "evicted");
        assert!(after_purge.body_hex.is_none());

        let leftover = reconcile_team_body_cache(
            &connection,
            "wsp_persistfixture000000000001",
            Some(workspace.path()),
            "2026-08-13T18:04:00Z",
        )
        .expect("reconcile-leftover-file");
        assert_eq!(leftover, 0);
        let still_evicted = connection
            .get_mesh_body_cache_metadata("wsp_persistfixture000000000001", &key)
            .expect("reload")
            .expect("present");
        assert_eq!(still_evicted.cache_status, "evicted");

        connection
            .upsert_mesh_body_cache_metadata(&UpsertMeshBodyCacheMetadataInput {
                workspace_id: "wsp_persistfixture000000000001".to_owned(),
                body_cache_key: "body_orphan000000000000000001".to_owned(),
                origin_node_id: existing.origin_node_id,
                origin_workspace_id: existing.origin_workspace_id,
                logical_memory_id: "mem_orphan00000000000000000001".to_owned(),
                content_hash: existing.content_hash,
                body_ref_json: None,
                preview_hash: None,
                size_bytes: Some(12),
                cache_status: "staging".to_owned(),
                local_body_hash: None,
                cached_at: Some("2026-08-13T18:05:00Z".to_owned()),
                expires_at: None,
            })
            .expect("orphan-stage");
        let staged = fetch_local_team_body(
            &connection,
            "wsp_persistfixture000000000001",
            workspace.path(),
            "body_orphan000000000000000001",
        )
        .expect("staging-fetch");
        assert_eq!(staged.cache_status, "staging");
        assert!(staged.body_hex.is_none());
        let folded_orphan = reconcile_team_body_cache(
            &connection,
            "wsp_persistfixture000000000001",
            Some(workspace.path()),
            "2026-08-13T18:06:00Z",
        )
        .expect("reconcile-orphan");
        assert_eq!(folded_orphan, 1);
        let orphan = fetch_local_team_body(
            &connection,
            "wsp_persistfixture000000000001",
            workspace.path(),
            "body_orphan000000000000000001",
        )
        .expect("orphan-fetch");
        assert_eq!(orphan.cache_status, "metadata_only");
        assert!(orphan.body_hex.is_none());
    }

    #[cfg(unix)]
    #[test]
    fn body_share_token_rejects_expired_and_wrong_store() {
        let workspace = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        let connection = open_db();
        connection
            .insert_workspace(
                "wsp_persistfixture000000000001",
                &crate::db::CreateWorkspaceInput {
                    path: workspace.path().display().to_string(),
                    name: Some("bodies-expiry".to_owned()),
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
                "mem_teamexpire0000000000000001",
                &crate::db::CreateMemoryInput {
                    workspace_id: "wsp_persistfixture000000000001".to_owned(),
                    level: "semantic".to_owned(),
                    kind: "note".to_owned(),
                    content: "expiry-bound body".to_owned(),
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
            "2026-08-13T19:00:00Z",
            false,
            16,
            Some(workspace.path()),
            false,
            None,
        )
        .expect("preview");
        let expired = issue_body_share_token_at(
            workspace.path(),
            "wsp_persistfixture000000000001",
            &preview.consent_hash,
            1_700_000_000,
        )
        .expect("old token");
        let stale = share_team_bodies(
            &connection,
            "wsp_persistfixture000000000001",
            "2026-08-13T19:01:00Z",
            true,
            16,
            Some(workspace.path()),
            false,
            Some(expired.as_str()),
        )
        .expect_err("expired");
        let stale_text = stale.to_string();
        assert!(stale_text.contains("stale") || stale_text.contains("expired"));
        assert!(!stale_text.contains("expiry-bound body"));

        let fresh = issue_body_share_token(
            workspace.path(),
            "wsp_persistfixture000000000001",
            &preview.consent_hash,
        )
        .expect("fresh");
        let wrong_store = share_team_bodies(
            &connection,
            "wsp_persistfixture000000000001",
            "2026-08-13T19:02:00Z",
            true,
            16,
            Some(other.path()),
            false,
            Some(fresh.as_str()),
        )
        .expect_err("wrong store");
        let wrong_text = wrong_store.to_string();
        assert!(
            wrong_text.contains("invalid")
                || wrong_text.contains("authentic")
                || wrong_text.contains("token")
                || wrong_text.contains("store")
        );
        let still = fetch_local_team_body(
            &connection,
            "wsp_persistfixture000000000001",
            workspace.path(),
            &team_body_cache_key("mem_teamexpire0000000000000001"),
        )
        .expect("unshared");
        assert_ne!(still.cache_status, "available");
        assert!(still.body_hex.is_none());
    }

    #[cfg(unix)]
    #[test]
    fn already_redacted_share_passes_and_redact_over_exact_is_refused() {
        let workspace = tempfile::tempdir().unwrap();
        let connection = open_db();
        connection
            .insert_workspace(
                "wsp_persistfixture000000000001",
                &crate::db::CreateWorkspaceInput {
                    path: workspace.path().display().to_string(),
                    name: Some("bodies-redacted".to_owned()),
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
                "mem_teamredact0000000000000001",
                &crate::db::CreateMemoryInput {
                    workspace_id: "wsp_persistfixture000000000001".to_owned(),
                    level: "semantic".to_owned(),
                    kind: "note".to_owned(),
                    content: "already-redacted body".to_owned(),
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
        let published = share_team_bodies(
            &connection,
            "wsp_persistfixture000000000001",
            "2026-08-13T20:00:00Z",
            true,
            16,
            Some(workspace.path()),
            false,
            None,
        )
        .expect("exact");
        assert_eq!(published.published_count, 1);
        let fetched = fetch_local_team_body(
            &connection,
            "wsp_persistfixture000000000001",
            workspace.path(),
            &team_body_cache_key("mem_teamredact0000000000000001"),
        )
        .expect("fetch");
        assert_eq!(fetched.cache_status, "available");
        let refused = share_team_bodies_represented(
            &connection,
            "wsp_persistfixture000000000001",
            "2026-08-13T20:01:00Z",
            true,
            16,
            Some(workspace.path()),
            false,
            None,
            "already_redacted",
        )
        .expect_err("redact-over-exact");
        assert!(refused.to_string().contains("redact-over-exact"));
        unshare_team_bodies(
            &connection,
            "wsp_persistfixture000000000001",
            "2026-08-13T20:02:00Z",
        )
        .expect("unshare");
        let redacted = share_team_bodies_represented(
            &connection,
            "wsp_persistfixture000000000001",
            "2026-08-13T20:03:00Z",
            true,
            16,
            Some(workspace.path()),
            false,
            None,
            "already_redacted",
        )
        .expect("already_redacted");
        assert_eq!(redacted.published_count, 1);
        assert_eq!(redacted.representation, "already_redacted");
        assert!(!body_lane_allows_fetch(
            &connection,
            "wsp_persistfixture000000000001",
            "peer_missing_body_grant",
        ));
    }

    #[test]
    fn body_lane_grant_then_revoke_gates_fetch() {
        let connection = open_db();
        connection
            .insert_workspace(
                "wsp_persistfixture000000000001",
                &crate::db::CreateWorkspaceInput {
                    path: "/tmp/ee-team-body-grant".to_owned(),
                    name: Some("bodies-grant".to_owned()),
                },
            )
            .expect("workspace");
        connection
            .upsert_mesh_peer(&crate::db::UpsertMeshPeerInput {
                workspace_id: "wsp_persistfixture000000000001".to_owned(),
                peer_id: "peer_bodygrant000000000001".to_owned(),
                origin_node_id: "node_bodygrant000000000001".to_owned(),
                display_name: Some("contractor".to_owned()),
                policy_summary_json: None,
                enabled: true,
                last_seen_at: Some("2026-08-13T21:00:00Z".to_owned()),
            })
            .expect("peer");
        assert!(!body_lane_allows_fetch(
            &connection,
            "wsp_persistfixture000000000001",
            "peer_bodygrant000000000001",
        ));
        let mutation = crate::db::MeshLaneGrantMutationInput {
            workspace_id: "wsp_persistfixture000000000001".to_owned(),
            peer_id: "peer_bodygrant000000000001".to_owned(),
            target_adapter: crate::db::MeshLaneGrantTargetAdapter::new(
                "peer_bodygrant000000000001",
                "node_bodygrant000000000001",
            ),
            material_lane: crate::config::MeshLane::Body,
            expected_generation: 0,
            approval_config_digest: Some(format!("blake3:{}", "a".repeat(64))),
            updated_at: Some("2026-08-13T21:00:00Z".to_owned()),
        };
        connection
            .apply_mesh_lane_grant_with_effect(&mutation, |_| Ok::<(), String>(()))
            .expect("grant");
        assert!(body_lane_allows_fetch(
            &connection,
            "wsp_persistfixture000000000001",
            "peer_bodygrant000000000001",
        ));
        connection
            .revoke_mesh_lane_with_effect(
                &crate::db::MeshLaneGrantMutationInput {
                    expected_generation: 1,
                    updated_at: Some("2026-08-13T21:01:00Z".to_owned()),
                    ..mutation
                },
                |_| Ok::<(), String>(()),
            )
            .expect("revoke");
        assert!(!body_lane_allows_fetch(
            &connection,
            "wsp_persistfixture000000000001",
            "peer_bodygrant000000000001",
        ));
    }

    #[cfg(unix)]
    #[test]
    fn substituted_body_cache_bytes_stay_metadata_only() {
        let workspace = tempfile::tempdir().unwrap();
        let connection = open_db();
        connection
            .insert_workspace(
                "wsp_persistfixture000000000001",
                &crate::db::CreateWorkspaceInput {
                    path: workspace.path().display().to_string(),
                    name: Some("bodies-sub".to_owned()),
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
                "mem_teamsubst00000000000000001",
                &crate::db::CreateMemoryInput {
                    workspace_id: "wsp_persistfixture000000000001".to_owned(),
                    level: "semantic".to_owned(),
                    kind: "note".to_owned(),
                    content: "canonical body".to_owned(),
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
        share_team_bodies(
            &connection,
            "wsp_persistfixture000000000001",
            "2026-08-13T22:00:00Z",
            true,
            16,
            Some(workspace.path()),
            false,
            None,
        )
        .expect("share");
        let key = team_body_cache_key("mem_teamsubst00000000000000001");
        let cache_dir = workspace.path().join(".ee").join("mesh-body-cache");
        let cache =
            crate::mesh::key_store::SecureLocalDir::open_existing(workspace.path(), &cache_dir)
                .expect("open cache")
                .expect("cache dir");
        cache
            .write_replace(&key, b"substituted revision")
            .expect("swap");
        let fetched = fetch_local_team_body(
            &connection,
            "wsp_persistfixture000000000001",
            workspace.path(),
            &key,
        )
        .expect("fetch");
        assert_eq!(fetched.cache_status, "metadata_only");
        assert!(fetched.body_hex.is_none());
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
    fn execute_team_steward_once_reapplies_a_missed_member_removal() {
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
        let planned = plan_team_steward_once(&connection).expect("plan");
        assert_eq!(planned.applied_removals, 0);
        assert!(
            origin_node_is_active_member(&connection, &left.origin_node_id)
                .expect("authz")
                .expect("members")
        );
        let executed = execute_team_steward_once(&connection, None).expect("execute");
        assert_eq!(executed.applied_removals, 1);
        assert!(
            !origin_node_is_active_member(&connection, &left.origin_node_id)
                .expect("authz after")
                .expect("members")
        );
        let again = execute_team_steward_once(&connection, None).expect("idempotent");
        assert_eq!(again.applied_removals, 0);
    }

    #[cfg(unix)]
    #[test]
    fn execute_team_steward_once_promotes_orphaned_next_pair_key() {
        let workspace = tempfile::tempdir().unwrap();
        let store =
            crate::mesh::key_store::MeshKeyStore::open_or_create(workspace.path()).expect("store");
        let key = crate::mesh::key_store::SecretBytes::new([9_u8; 32]);
        store
            .store_pair_key(
                "peer-a1",
                crate::mesh::key_store::PairKeyClass::Next,
                std::num::NonZeroU64::MIN,
                &key,
                "2026-08-13T00:00:00Z",
                false,
            )
            .expect("stage next");
        let connection = open_db();
        create_local_team(
            &connection,
            "wsp_persistfixture000000000001",
            "Analysts",
            "2026-08-13T00:00:00Z",
        )
        .expect("create");
        let executed =
            execute_team_steward_once(&connection, Some(workspace.path())).expect("execute");
        assert_eq!(executed.deferred_pairings, 1);
        assert_eq!(executed.applied_pair_promotions, 1);
        assert!(
            store
                .load_pair_key("peer-a1", crate::mesh::key_store::PairKeyClass::Current)
                .expect("current")
                .is_some()
        );
        assert!(
            store
                .load_pair_key("peer-a1", crate::mesh::key_store::PairKeyClass::Next)
                .expect("next")
                .is_none()
        );
        let again =
            execute_team_steward_once(&connection, Some(workspace.path())).expect("idempotent");
        assert_eq!(again.deferred_pairings, 0);
        assert_eq!(again.applied_pair_promotions, 0);
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
        assert!(healthy.checks.iter().any(|check| check.name == "admission"
            && check.status == "ok"
            && check.message.contains("local_tier1_unaffected=true")));
        assert!(
            healthy
                .checks
                .iter()
                .any(|check| check.name == "key_store" && check.status == "ok")
        );
        assert!(healthy.checks.iter().any(|check| {
            check.name == "broker_port"
                && check.status == "ok"
                && check.message.contains(&configured_hello_port().to_string())
        }));
        assert!(
            healthy
                .checks
                .iter()
                .any(|check| check.name == "whois" && check.status == "ok")
        );
        assert!(
            healthy
                .checks
                .iter()
                .any(|check| check.name == "client_only" && check.status == "ok")
        );
        assert!(
            healthy
                .checks
                .iter()
                .any(|check| { check.name == "index_rematerialization" && check.status == "ok" })
        );
        assert!(
            healthy
                .checks
                .iter()
                .any(|check| check.name == "origin_outbox" && check.status == "ok")
        );
        assert!(
            healthy
                .checks
                .iter()
                .any(|check| check.name == "invite_auth_floor" && check.status == "ok")
        );
        assert!(
            healthy
                .checks
                .iter()
                .any(|check| check.name == "pending_invites" && check.status == "ok")
        );
        assert!(
            healthy
                .checks
                .iter()
                .any(|check| check.name == "delegated_members" && check.status == "ok")
        );
        assert!(
            healthy
                .checks
                .iter()
                .any(|check| check.name == "signing_rotation" && check.status == "ok")
        );
        assert!(
            healthy
                .checks
                .iter()
                .any(|check| check.name == "projects" && check.status == "ok")
        );
        assert!(
            healthy
                .checks
                .iter()
                .any(|check| check.name == "inbound_memories" && check.status == "ok")
        );
        assert!(
            healthy
                .checks
                .iter()
                .any(|check| check.name == "removal_acknowledgements" && check.status == "ok")
        );
        set_local_team_paused(&connection, true, "2026-08-13T17:00:00Z").expect("pause");
        let paused = inspect_team_health(&connection, "wsp_persistfixture000000000001", None)
            .expect("paused");
        assert_eq!(paused.posture, "paused");
    }

    #[cfg(unix)]
    #[test]
    fn inspect_team_health_reports_free_space_above_the_floor() {
        let workspace = tempfile::tempdir().unwrap();
        let connection = open_db();
        create_local_team(
            &connection,
            "wsp_persistfixture000000000001",
            "Analysts",
            "2026-08-13T00:00:00Z",
        )
        .expect("create");
        let report = inspect_team_health(
            &connection,
            "wsp_persistfixture000000000001",
            Some(workspace.path()),
        )
        .expect("doctor");
        assert!(report.checks.iter().any(|check| {
            check.name == "free_space" && check.status == "ok" && check.message.contains("floor is")
        }));
        assert_eq!(TEAM_FREE_SPACE_FLOOR_BYTES, 64 * 1024 * 1024);
    }

    #[test]
    fn persisted_admission_snapshot_warns_doctor_and_status() {
        let connection = open_db();
        create_local_team(
            &connection,
            "wsp_persistfixture000000000001",
            "Analysts",
            "2026-08-13T00:00:00Z",
        )
        .expect("create");
        let mut peer = crate::mesh::admission::MeshPeerAdmissionState::new("peer_noisy_000001");
        peer = peer
            .with_policy_denial_count(2)
            .with_malformed_frame_count(1);
        persist_team_admission_states(
            &connection,
            "wsp_persistfixture000000000001",
            &[peer],
            "2026-08-14T15:00:00Z",
        )
        .expect("persist");
        let doctor = inspect_team_health(&connection, "wsp_persistfixture000000000001", None)
            .expect("doctor");
        assert!(doctor.checks.iter().any(|check| {
            check.name == "admission"
                && check.status == "warning"
                && check.message.contains("coalesced_exhaustion=true")
        }));
        let status = local_team_status(&connection).expect("status");
        assert_eq!(status.admission.peer_snapshot_count, 1);
        assert_eq!(status.admission.budget_exhausted_peer_count, 1);
        assert_eq!(status.admission.throttled_peer_count, 1);
        assert!(status.admission.coalesced_exhaustion);
        assert!(status.admission.local_tier1_unaffected);
    }

    #[test]
    fn team_confed_budget_profile_names_join_relay_body_and_index_caps() {
        let limits = crate::mesh::admission::MeshAdmissionLimits::conservative_default();
        let profile = team_confed_budget_profile();
        assert_eq!(profile.schema, TEAM_BUDGETS_SCHEMA_V1);
        assert_eq!(profile.join_event_batch_count, limits.max_event_batch_count);
        assert_eq!(
            profile.signed_relay_event_batch_bytes,
            limits.max_event_batch_bytes
        );
        assert_eq!(profile.body_fetch_bytes, limits.max_body_fetch_bytes);
        assert_eq!(
            profile.index_jobs_per_round,
            limits.max_index_jobs_per_round
        );
        assert_eq!(
            profile.concurrent_requests_per_peer,
            limits.max_concurrent_requests_per_peer
        );
        assert_eq!(profile.free_space_floor_bytes, TEAM_FREE_SPACE_FLOOR_BYTES);
        assert!(profile.local_tier1_unaffected);
        let connection = open_db();
        create_local_team(
            &connection,
            "wsp_persistfixture000000000001",
            "Analysts",
            "2026-08-13T00:00:00Z",
        )
        .expect("create");
        let status = local_team_status(&connection).expect("status");
        assert_eq!(status.budgets, profile);
        let at_cap = crate::mesh::admission::decide_admission(
            limits,
            &crate::mesh::admission::MeshPeerAdmissionState::new("peer-join"),
            &crate::mesh::admission::MeshAdmissionRequest::new(
                "peer-join",
                crate::mesh::admission::MeshAdmissionRequestKind::EventBatch,
                0,
            )
            .with_event_count(profile.join_event_batch_count)
            .with_payload(profile.signed_relay_event_batch_bytes),
        );
        assert!(at_cap.allowed());
        assert!(at_cap.local_tier1_unaffected);
        let over = crate::mesh::admission::decide_admission(
            limits,
            &crate::mesh::admission::MeshPeerAdmissionState::new("peer-join"),
            &crate::mesh::admission::MeshAdmissionRequest::new(
                "peer-join",
                crate::mesh::admission::MeshAdmissionRequestKind::EventBatch,
                0,
            )
            .with_event_count(profile.join_event_batch_count.saturating_add(1)),
        );
        assert!(!over.allowed());
        assert!(over.local_tier1_unaffected);
    }

    #[test]
    fn revoke_before_floor_clears_stale_pending_invites() {
        let connection = open_db();
        let created = create_local_team(
            &connection,
            "wsp_persistfixture000000000001",
            "Analysts",
            "2026-08-13T00:00:00Z",
        )
        .expect("create");
        connection
            .insert_team_pending_invite(&crate::db::InsertTeamPendingInviteInput {
                invite_id: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_owned(),
                team_id: created.team.team_id.clone(),
                origin_node_id: created.team.origin_node_id.clone(),
                hello_port: 7421,
                endpoint: "127.0.0.1".to_owned(),
                genesis_event_hash: created.team.genesis_event_hash.clone(),
                secret_hash: format!("blake3:{}", "c".repeat(64)),
                status: "pending".to_owned(),
                created_at: "2026-08-12T00:00:00Z".to_owned(),
                expires_at: "2026-08-20T00:00:00Z".to_owned(),
            })
            .expect("invite");
        let sick =
            inspect_team_health(&connection, "wsp_persistfixture000000000001", None).expect("sick");
        assert!(sick.checks.iter().any(|check| {
            check.name == "invite_auth_floor"
                && check.status == "error"
                && check.repair.as_deref()
                    == Some("ee team revoke --all-before-floor --workspace .")
        }));
        let revoked =
            revoke_team_invites_before_floor(&connection, "2026-08-13T12:00:00Z").expect("revoke");
        assert_eq!(revoked, 1);
        let healthy = inspect_team_health(&connection, "wsp_persistfixture000000000001", None)
            .expect("healthy");
        assert!(
            healthy
                .checks
                .iter()
                .any(|check| check.name == "invite_auth_floor" && check.status == "ok")
        );
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
        let leased = team_idp_status(&connection).expect("leases");
        assert_eq!(leased.leases.len(), 1);
        assert_eq!(leased.leases[0].state, "attested");
        assert_eq!(leased.leases[0].posture, "current");
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
        let status = team_idp_status(&connection).expect("status");
        assert_eq!(status.oidc_issuer.as_deref(), Some("https://idp.example"));
        assert_eq!(status.oidc_capability.as_deref(), Some("secretless_public"));
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

    #[test]
    fn plan_team_idp_device_omits_device_code_and_builds_https_curl() {
        let connection = open_db();
        create_local_team(
            &connection,
            "wsp_persistfixture000000000001",
            "Analysts",
            "2026-08-13T00:00:00Z",
        )
        .expect("create");
        let report = plan_team_idp_device(
            &connection,
            &serde_json::json!({
                "token_endpoint": "https://idp.example/token",
                "device_authorization_endpoint": "https://idp.example/device",
                "token_endpoint_auth_methods_supported": ["none"]
            }),
            &serde_json::json!({
                "device_code": "secret-device-code",
                "user_code": "WDJB-MJHT",
                "verification_uri": "https://idp.example/device",
                "verification_uri_complete": "https://idp.example/device?user_code=WDJB-MJHT",
                "expires_in": 600
            }),
            "/usr/bin/curl",
        )
        .expect("plan");
        assert_eq!(report.user_code, "WDJB-MJHT");
        assert_eq!(report.interval, 5);
        assert_eq!(report.first_wait_secs, 5);
        assert!(
            report
                .curl_argv
                .iter()
                .any(|arg| arg == "https://idp.example/token")
        );
        let json = serde_json::to_string(&report).expect("json");
        assert!(!json.contains("secret-device-code"));
        assert!(json.contains("WDJB-MJHT"));
    }

    #[test]
    fn attest_local_id_token_binds_reduced_claims_to_self() {
        let connection = open_db();
        create_local_team(
            &connection,
            "wsp_persistfixture000000000001",
            "Analysts",
            "2026-08-13T00:00:00Z",
        )
        .expect("create");
        let token = format!(
            "{}.{}.{}",
            crate::mesh::idp::encode_unpadded_base64url(br#"{"alg":"RS256","kid":"k1"}"#),
            crate::mesh::idp::encode_unpadded_base64url(
                br#"{"sub":"user-1","email":"alice@acme.com","groups":["eng","secret-admin"]}"#,
            ),
            crate::mesh::idp::encode_unpadded_base64url(b"sig"),
        );
        let report =
            attest_local_id_token(&connection, &token, &["eng"], "2026-08-13T20:00:00Z", None)
                .expect("attest");
        assert_eq!(report.email.as_deref(), Some("alice@acme.com"));
        assert_eq!(report.matched_groups, vec!["eng".to_owned()]);
        let json = serde_json::to_string(&report).expect("json");
        assert!(!json.contains("secret-admin"));
        let identity = connection
            .get_team_member_identity(&report.member_id)
            .expect("load")
            .expect("row");
        assert_eq!(identity.login, "alice@acme.com");
        assert_eq!(identity.user_id.as_deref(), Some("user-1"));
        let events = connection
            .list_all_mesh_origin_events(&report.team_id, 16)
            .expect("events");
        assert!(events.iter().any(|event| {
            event.payload_json.contains("identityAttested")
                && event.payload_json.contains("alice@acme.com")
                && !event.payload_json.contains(&token)
        }));
        let replayed =
            attest_local_id_token(&connection, &token, &["eng"], "2026-08-13T20:01:00Z", None)
                .expect_err("replay");
        assert!(replayed.to_string().contains("already consumed"));
    }

    #[test]
    fn apply_identity_attest_frame_rejects_bearer_and_accepts_hash_only() {
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
        let forbidden = serde_json::json!({
            "schema": IDENTITY_ATTEST_FRAME_SCHEMA_V1,
            "teamId": member.team_id,
            "memberId": member.member_id,
            "subject": "user-1",
            "matchedGroups": ["eng"],
            "tokenHash": "blake3:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "checkedAt": "2026-08-13T21:00:00Z",
            "idToken": "eyJhbGciOiJSUzI1NiJ9.e30.c2ln"
        });
        let err = apply_identity_attest_frame(&connection, &forbidden).expect_err("bearer");
        assert!(err.to_string().contains("bearer"));
        let ok = apply_identity_attest_frame(
            &connection,
            &serde_json::json!({
                "schema": IDENTITY_ATTEST_FRAME_SCHEMA_V1,
                "teamId": member.team_id,
                "memberId": member.member_id,
                "subject": "user-1",
                "email": "alice@acme.com",
                "matchedGroups": ["eng"],
                "tokenHash": "blake3:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
                "checkedAt": "2026-08-13T21:00:00Z"
            }),
        )
        .expect("apply");
        assert_eq!(ok.subject, "user-1");
        let _ = created;
    }
}
