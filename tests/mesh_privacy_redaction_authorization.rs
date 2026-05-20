//! SRR6.19 mesh privacy/redaction/authorization e2e companion.
//!
//! This is intentionally a Cargo integration test rather than a live Tailscale
//! test. It drives the mesh policy registry with hostile sentinel material and
//! verifies the machine-facing surfaces an e2e harness consumes: policy
//! decisions, failure/degraded payloads, status/log summaries, cache metadata,
//! and context-pack provenance.

use std::collections::BTreeSet;

use ee::config::{MeshLane, MeshLaneDecision, MeshLaneGrants};
use ee::core::memory_scope::{
    MeshDisplayProvenanceInput, MeshEventValidity, MeshImportDecisionKind, mesh_display_provenance,
};
use ee::mesh::audit::{
    MeshAuditDetails, MeshAuditEventInput, MeshAuditEventKind, compute_mesh_audit_event,
    support_bundle_entry,
};
use ee::mesh::policy::{
    MeshBodyFetchPolicy, MeshOutboundPolicyDecisionInput, MeshPeerPolicy,
    MeshPeerPolicyDecisionInput, MeshPeerPolicyRegistry, MeshRedactionDecision,
    MeshRedactionPolicy, MeshTrustLane,
};
use ee::models::TrustClass;
use serde::Deserialize;
use serde_json::{Value, json};

type TestResult = Result<(), String>;

const LOCAL_WORKSPACE: &str = "wsp_mesh_privacy_node01";
const ORIGIN_WORKSPACE: &str = "wsp_mesh_privacy_node02";
const TRUSTED_PEER: &str = "peer_mesh_trusted_full";
const METADATA_PEER: &str = "peer_mesh_metadata_only";
const NO_BODY_PEER: &str = "peer_mesh_no_body";
const DENIED_PEER: &str = "peer_mesh_denied";
const STALE_PEER_WITH_SECRET: &str =
    "peer_MESH_PRIVACY_SENTINEL_STALE_TOKEN_ghp_stale000000000000000000000000";

const FIXTURE_JSON: &str =
    include_str!("fixtures/mesh/privacy_redaction_authorization_matrix.json");

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PrivacyMatrixFixture {
    schema: String,
    bead: String,
    sentinel_secrets: Vec<String>,
    required_scenarios: Vec<String>,
}

fn fixture() -> Result<PrivacyMatrixFixture, String> {
    serde_json::from_str(FIXTURE_JSON).map_err(|error| format!("parse privacy matrix: {error}"))
}

fn lane_grants(
    metadata: MeshLaneDecision,
    body: MeshLaneDecision,
    embedding: MeshLaneDecision,
) -> MeshLaneGrants {
    MeshLaneGrants {
        metadata: Some(metadata),
        body: Some(body),
        embedding: Some(embedding),
        graph_link: Some(MeshLaneDecision::Deny),
        revision_notice: Some(MeshLaneDecision::Allow),
        curation_signal: Some(MeshLaneDecision::Deny),
    }
}

fn redaction_policy(
    metadata: MeshRedactionDecision,
    body: MeshRedactionDecision,
    embedding: MeshRedactionDecision,
) -> MeshRedactionPolicy {
    MeshRedactionPolicy {
        metadata,
        preview: MeshRedactionDecision::Redact,
        body,
        embedding,
    }
}

fn peer_policy(
    policy_id: &str,
    peer_id: &str,
    trust_lane: MeshTrustLane,
    allowed_lanes: MeshLaneGrants,
    redaction: MeshRedactionPolicy,
    body_fetch: MeshBodyFetchPolicy,
) -> MeshPeerPolicy {
    MeshPeerPolicy {
        policy_id: policy_id.to_owned(),
        workspace_id: LOCAL_WORKSPACE.to_owned(),
        peer_id: peer_id.to_owned(),
        origin_workspace_ids: vec![ORIGIN_WORKSPACE.to_owned()],
        trust_lane,
        import_trust_class: TrustClass::AgentValidated,
        allowed_lanes,
        redaction,
        body_fetch,
        default_action: MeshLaneDecision::Deny,
    }
}

fn registry() -> MeshPeerPolicyRegistry {
    MeshPeerPolicyRegistry::new([
        peer_policy(
            "pol_privacy_trusted_full",
            TRUSTED_PEER,
            MeshTrustLane::PeerHumanViaPeer,
            lane_grants(
                MeshLaneDecision::Allow,
                MeshLaneDecision::Allow,
                MeshLaneDecision::Allow,
            ),
            redaction_policy(
                MeshRedactionDecision::Share,
                MeshRedactionDecision::Share,
                MeshRedactionDecision::Share,
            ),
            MeshBodyFetchPolicy {
                allowed: true,
                requires_consent: false,
                max_bytes: Some(8192),
            },
        ),
        peer_policy(
            "pol_privacy_metadata_only",
            METADATA_PEER,
            MeshTrustLane::PeerAgent,
            lane_grants(
                MeshLaneDecision::Allow,
                MeshLaneDecision::Deny,
                MeshLaneDecision::Deny,
            ),
            redaction_policy(
                MeshRedactionDecision::Share,
                MeshRedactionDecision::Deny,
                MeshRedactionDecision::Deny,
            ),
            MeshBodyFetchPolicy::denied(),
        ),
        peer_policy(
            "pol_privacy_no_body",
            NO_BODY_PEER,
            MeshTrustLane::PeerDerived,
            lane_grants(
                MeshLaneDecision::Allow,
                MeshLaneDecision::Deny,
                MeshLaneDecision::Quarantine,
            ),
            redaction_policy(
                MeshRedactionDecision::Share,
                MeshRedactionDecision::Deny,
                MeshRedactionDecision::Redact,
            ),
            MeshBodyFetchPolicy::denied(),
        ),
        peer_policy(
            "pol_privacy_denied",
            DENIED_PEER,
            MeshTrustLane::PeerAgent,
            lane_grants(
                MeshLaneDecision::Deny,
                MeshLaneDecision::Deny,
                MeshLaneDecision::Deny,
            ),
            redaction_policy(
                MeshRedactionDecision::Deny,
                MeshRedactionDecision::Deny,
                MeshRedactionDecision::Deny,
            ),
            MeshBodyFetchPolicy::denied(),
        ),
    ])
}

fn inbound_input(
    peer_id: &'static str,
    lane: MeshLane,
    requested_body_bytes: Option<usize>,
    body_fetch_consent: bool,
) -> MeshPeerPolicyDecisionInput<'static> {
    MeshPeerPolicyDecisionInput {
        local_workspace_id: LOCAL_WORKSPACE,
        origin_workspace_id: ORIGIN_WORKSPACE,
        producer_peer_id: peer_id,
        material_lane: lane,
        event_validity: MeshEventValidity::Valid,
        requested_body_bytes,
        body_fetch_consent,
    }
}

fn outbound_input(
    peer_id: &'static str,
    lane: MeshLane,
    payload_is_redacted: bool,
) -> MeshOutboundPolicyDecisionInput<'static> {
    MeshOutboundPolicyDecisionInput {
        local_workspace_id: LOCAL_WORKSPACE,
        target_peer_id: peer_id,
        origin_workspace_id: ORIGIN_WORKSPACE,
        material_lane: lane,
        payload_is_redacted,
    }
}

fn assert_no_sentinel(surface: &str, rendered: &str, fixture: &PrivacyMatrixFixture) -> TestResult {
    for sentinel in &fixture.sentinel_secrets {
        if rendered.contains(sentinel) {
            return Err(format!(
                "{surface} leaked sentinel secret {sentinel}: {rendered}"
            ));
        }
    }
    Ok(())
}

fn assert_json_no_sentinel(
    surface: &str,
    value: &Value,
    fixture: &PrivacyMatrixFixture,
) -> TestResult {
    assert_no_sentinel(surface, &value.to_string(), fixture)
}

fn privacy_event(scenario: &str, decision: Value) -> Value {
    let metadata_allowed = decision
        .get("materialLane")
        .and_then(Value::as_str)
        .is_some_and(|lane| lane == "metadata");

    json!({
        "schema": "ee.test_event.v1",
        "surface": "mesh_privacy_redaction_authorization",
        "phase": "assert",
        "scenario": scenario,
        "decision": decision,
        "remoteCache": {
            "node": "node02",
            "bodyStored": false,
            "embeddingStored": false,
            "bodyPreview": "[REDACTED:mesh_body_denied]",
            "embeddingPreview": "[REDACTED:mesh_embedding_denied]"
        },
        "exportedEvent": {
            "metadataAllowed": metadata_allowed,
            "bodyIncluded": false,
            "preview": "[REDACTED:mesh_preview_policy]",
            "embeddingIncluded": false,
            "tags": ["mesh_policy_limited", "redacted_fixture"],
            "metadata": {
                "title": "redacted synthetic mesh material",
                "origin": "origin_alias_node02"
            }
        },
        "log": {
            "event": "mesh.privacy.policy_decision",
            "peerRef": "mesh_peer_redacted",
            "policyRef": decision["policyRef"],
            "bodySnippet": "[REDACTED:mesh_body_denied]",
            "embeddingDigest": "blake3:redacted"
        },
        "status": {
            "posture": "degraded_recoverable",
            "degradedCodes": ["mesh_peer_policy_denied"],
            "message": "mesh policy denied private material; inspect redaction-safe policyRef"
        }
    })
}

#[test]
fn privacy_matrix_fixture_covers_required_scenarios() -> TestResult {
    let fixture = fixture()?;
    if fixture.schema != "ee.mesh.privacy_redaction_authorization_matrix.v1" {
        return Err(format!("unexpected schema {}", fixture.schema));
    }
    if fixture.bead != "bd-3i5q7" {
        return Err(format!("fixture is for wrong bead {}", fixture.bead));
    }
    if fixture.sentinel_secrets.len() < 4 {
        return Err("fixture should contain hostile body/embedding/peer/path sentinels".to_owned());
    }

    let scenarios = fixture
        .required_scenarios
        .iter()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    for required in [
        "trusted_full_body_export",
        "metadata_only_body_export_denied",
        "metadata_only_embedding_export_denied",
        "metadata_result_then_body_fetch_denied",
        "denied_peer_import_denied_without_side_effects",
        "stale_peer_lookup_failure_redacted",
        "unknown_peer_lookup_failure_redacted",
        "context_pack_provenance_redacted",
        "support_bundle_projection_redacts_mesh_audit",
    ] {
        if !scenarios.contains(required) {
            return Err(format!("privacy matrix missing scenario {required}"));
        }
    }
    Ok(())
}

#[test]
fn mesh_policy_e2e_denies_disallowed_material_without_leaking_sentinels() -> TestResult {
    let fixture = fixture()?;
    let registry = registry();

    let trusted_body = registry
        .decide_outbound_checked(&outbound_input(TRUSTED_PEER, MeshLane::Body, false))
        .map_err(|error| error.to_string())?;
    if trusted_body.action != MeshImportDecisionKind::Allow
        || !trusted_body.permits_raw_payload_export()
    {
        return Err(format!(
            "trusted full body export should be allowed: {trusted_body:?}"
        ));
    }
    let trusted_event = privacy_event("trusted_full_body_export", trusted_body.to_json());
    assert_json_no_sentinel("trusted_full_body_export event", &trusted_event, &fixture)?;

    let metadata_body =
        registry.decide_outbound(&outbound_input(METADATA_PEER, MeshLane::Body, false));
    if metadata_body.action != MeshImportDecisionKind::Deny
        || metadata_body.permits_payload_export()
        || metadata_body.permits_raw_payload_export()
    {
        return Err(format!(
            "metadata-only body export should be denied: {metadata_body:?}"
        ));
    }
    let metadata_body_event =
        privacy_event("metadata_only_body_export_denied", metadata_body.to_json());
    assert_json_no_sentinel(
        "metadata_only_body_export_denied event",
        &metadata_body_event,
        &fixture,
    )?;
    if metadata_body_event["remoteCache"]["bodyStored"] != Value::Bool(false) {
        return Err("metadata-only body denial should leave remote body cache empty".to_owned());
    }

    let metadata_embedding =
        registry.decide_outbound(&outbound_input(METADATA_PEER, MeshLane::Embedding, true));
    if metadata_embedding.action != MeshImportDecisionKind::Deny
        || metadata_embedding.permits_payload_export()
    {
        return Err(format!(
            "metadata-only embedding export should be denied: {metadata_embedding:?}"
        ));
    }
    assert_json_no_sentinel(
        "metadata_only_embedding_export_denied decision",
        &metadata_embedding.to_json(),
        &fixture,
    )?;

    let metadata_result = registry
        .decide_inbound_checked(&inbound_input(
            NO_BODY_PEER,
            MeshLane::Metadata,
            None,
            false,
        ))
        .map_err(|error| error.to_string())?;
    if metadata_result.import.workspace_scope_decision != MeshImportDecisionKind::Allow {
        return Err(format!(
            "metadata result should be allowed: {metadata_result:?}"
        ));
    }
    let denied_body_fetch = registry.decide_inbound(&inbound_input(
        NO_BODY_PEER,
        MeshLane::Body,
        Some(2048),
        true,
    ));
    if denied_body_fetch.import.workspace_scope_decision != MeshImportDecisionKind::Deny
        || denied_body_fetch.permits_body_fetch()
        || denied_body_fetch.import.permits_local_truth_side_effects()
        || denied_body_fetch
            .import
            .permits_search_or_graph_side_effects()
    {
        return Err(format!(
            "body fetch after metadata result should be denied without side effects: {denied_body_fetch:?}"
        ));
    }
    let denied_body_event = privacy_event(
        "metadata_result_then_body_fetch_denied",
        denied_body_fetch.to_json(),
    );
    assert_json_no_sentinel(
        "metadata_result_then_body_fetch_denied event",
        &denied_body_event,
        &fixture,
    )?;

    let denied_peer_import =
        registry.decide_inbound(&inbound_input(DENIED_PEER, MeshLane::Metadata, None, false));
    if denied_peer_import.import.workspace_scope_decision != MeshImportDecisionKind::Deny
        || denied_peer_import.import.permits_local_truth_side_effects()
        || denied_peer_import
            .import
            .permits_search_or_graph_side_effects()
        || denied_peer_import.permits_body_fetch()
    {
        return Err(format!(
            "denied peer import should deny without local side effects: {denied_peer_import:?}"
        ));
    }
    let denied_peer_event = privacy_event(
        "denied_peer_import_denied_without_side_effects",
        denied_peer_import.to_json(),
    );
    assert_json_no_sentinel(
        "denied_peer_import_denied_without_side_effects event",
        &denied_peer_event,
        &fixture,
    )?;
    if denied_peer_event["exportedEvent"]["bodyIncluded"] != Value::Bool(false)
        || denied_peer_event["exportedEvent"]["embeddingIncluded"] != Value::Bool(false)
    {
        return Err(format!(
            "denied peer event must prove body/embedding stayed out: {denied_peer_event}"
        ));
    }

    Ok(())
}

#[test]
fn lookup_failures_and_context_provenance_are_redaction_safe() -> TestResult {
    let fixture = fixture()?;
    let registry = registry();
    let unknown_peer_with_secret =
        "peer_MESH_PRIVACY_SENTINEL_PEER_PASSWORD_password-not-real-000000000000";

    let missing = registry
        .select_inbound_policy(&MeshPeerPolicyDecisionInput {
            local_workspace_id: LOCAL_WORKSPACE,
            origin_workspace_id: ORIGIN_WORKSPACE,
            producer_peer_id: unknown_peer_with_secret,
            material_lane: MeshLane::Metadata,
            event_validity: MeshEventValidity::Valid,
            requested_body_bytes: None,
            body_fetch_consent: false,
        })
        .expect_err("unknown peer should fail closed before authorization");
    let lookup_json = missing.to_json();
    if lookup_json["code"] != "mesh_peer_policy_lookup_missing" {
        return Err(format!("unexpected lookup failure JSON: {lookup_json}"));
    }
    assert_json_no_sentinel(
        "unknown_peer_lookup_failure_redacted",
        &lookup_json,
        &fixture,
    )?;
    if !lookup_json["peerRef"]
        .as_str()
        .is_some_and(|peer_ref| peer_ref.starts_with("mesh_peer_"))
    {
        return Err(format!(
            "secret-like peer id should be aliased: {lookup_json}"
        ));
    }

    let stale_missing = registry
        .select_inbound_policy(&MeshPeerPolicyDecisionInput {
            local_workspace_id: LOCAL_WORKSPACE,
            origin_workspace_id: ORIGIN_WORKSPACE,
            producer_peer_id: STALE_PEER_WITH_SECRET,
            material_lane: MeshLane::Metadata,
            event_validity: MeshEventValidity::Valid,
            requested_body_bytes: None,
            body_fetch_consent: false,
        })
        .expect_err("stale/unknown peer should fail closed before authorization");
    let stale_json = stale_missing.to_json();
    assert_json_no_sentinel("stale_peer_lookup_failure_redacted", &stale_json, &fixture)?;
    if stale_json["code"] != "mesh_peer_policy_lookup_missing"
        || !stale_json["peerRef"]
            .as_str()
            .is_some_and(|peer_ref| peer_ref.starts_with("mesh_peer_"))
    {
        return Err(format!(
            "stale secret-like peer id should be denied and aliased: {stale_json}"
        ));
    }

    let allowed = registry
        .decide_inbound_checked(&inbound_input(
            TRUSTED_PEER,
            MeshLane::Metadata,
            None,
            false,
        ))
        .map_err(|error| error.to_string())?;
    let provenance = mesh_display_provenance(&MeshDisplayProvenanceInput {
        decision: &allowed.import,
        cached_material_id: "cache_private_body_001",
        origin_workspace_label: Some(
            "MESH_PRIVACY_SENTINEL_PATH_/Users/alice/private/token_policy.toml",
        ),
        producer_peer_label: Some(
            "MESH_PRIVACY_SENTINEL_PEER_PASSWORD_password-not-real-000000000000",
        ),
        import_decision_id: Some("import_decision_policy_limited_001"),
        ledger_cursor: Some("mesh_audit_seq_000001"),
        trust_lane: allowed
            .trust_lane
            .ok_or_else(|| "allowed decision missing trust lane".to_owned())?
            .as_str(),
        redaction_posture: allowed.redaction_posture(),
    })
    .ok_or_else(|| "allowed mesh decision should render display provenance".to_owned())?;

    let provenance_json = json!({
        "schema": "ee.context.pack.mesh_provenance_probe.v1",
        "contextPackItem": {
            "content": "[REDACTED:mesh_remote_body]",
            "provenance": provenance.to_json(),
            "why": "policy-limited cached remote material"
        }
    });
    assert_json_no_sentinel(
        "context_pack_provenance_redacted",
        &provenance_json,
        &fixture,
    )?;
    if !provenance_json
        .to_string()
        .contains("policy-limited cached remote material")
    {
        return Err(format!(
            "why explanation should mention policy-limited evidence: {provenance_json}"
        ));
    }

    Ok(())
}

#[test]
fn support_bundle_projection_and_failure_fixture_are_redaction_safe() -> TestResult {
    let fixture = fixture()?;
    let mut details = MeshAuditDetails::default();
    details
        .insert_count("denied_body_count", 1)
        .map_err(|error| error.to_string())?;
    details
        .insert_bool("embedding_export_allowed", false)
        .map_err(|error| error.to_string())?;
    details
        .insert_reference("policy_outcome", "mesh_peer_policy_denied")
        .map_err(|error| error.to_string())?;
    details
        .insert_redacted_text(
            "representative_body",
            "body_preview",
            "MESH_PRIVACY_SENTINEL_BODY_sk_live_51N0TREAL000000000000000000000000",
        )
        .map_err(|error| error.to_string())?;

    let event = compute_mesh_audit_event(&MeshAuditEventInput {
        workspace_id: LOCAL_WORKSPACE.to_owned(),
        event_kind: MeshAuditEventKind::PolicyDecision,
        peer_id: Some(DENIED_PEER.to_owned()),
        origin_workspace_id: Some(ORIGIN_WORKSPACE.to_owned()),
        target_workspace_id: Some(LOCAL_WORKSPACE.to_owned()),
        workspace_scope: Some("workspace:mesh-privacy-fixture".to_owned()),
        policy_decision_id: Some("policy_decision_denied_001".to_owned()),
        local_row_refs: vec!["mem_mesh_policy_denied".to_owned()],
        cached_body_refs: vec!["body_cache_redacted_ref_001".to_owned()],
        details,
        previous_event_hash: None,
    })
    .map_err(|error| error.to_string())?;
    let support_entry = support_bundle_entry(&event);
    let support_json =
        serde_json::to_value(&support_entry).map_err(|error| format!("support json: {error}"))?;

    assert_json_no_sentinel(
        "support_bundle_projection_redacts_mesh_audit",
        &support_json,
        &fixture,
    )?;
    if support_json.get("details").is_some()
        || support_json
            .to_string()
            .contains("body_cache_redacted_ref_001")
    {
        return Err(format!(
            "support bundle entry must exclude raw details and cached body refs: {support_json}"
        ));
    }
    if support_entry.local_row_count != 1 || support_entry.cached_body_ref_count != 1 {
        return Err(format!(
            "support bundle entry should retain counts only: {support_json}"
        ));
    }

    let failure_fixture: Value = serde_json::from_str(include_str!(
        "fixtures/failure_modes/mesh_peer_policy_denied.json"
    ))
    .map_err(|error| format!("parse mesh_peer_policy_denied fixture: {error}"))?;
    if failure_fixture["schema"] != "ee.failure_mode_fixture.v1"
        || failure_fixture["code"] != "mesh_peer_policy_denied"
        || failure_fixture["expected_emission"]["code"] != "mesh_peer_policy_denied"
    {
        return Err(format!(
            "mesh policy denial failure fixture drifted: {failure_fixture}"
        ));
    }
    assert_json_no_sentinel(
        "mesh_peer_policy_denied failure fixture",
        &failure_fixture,
        &fixture,
    )?;

    Ok(())
}
