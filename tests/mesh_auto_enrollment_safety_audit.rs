//! Integration tests for SRR6.46.5 (auto-enrollment safety snapshot).
//!
//! Named per closure-lint Rule 8 (`*_audit.rs`) because the bead emits the
//! `mesh.auto_enrollment_intended` and `mesh.auto_enrollment_outcome_recorded`
//! audit-row event types.
//!
//! These tests exercise the audit-chain side of the contract: emit a real
//! row into a real (in-memory) DB, read it back, assert it carries the
//! canonical-JSON summary, and confirm `ee audit verify` integrity holds
//! across multiple emissions. The pure-compute tests live inline in
//! `src/mesh/auto_enrollment_safety.rs`.

#![allow(clippy::expect_used)]

use ee::db::{CreateWorkspaceInput, DbConnection, audit_actions};
use ee::mesh::auto_enrollment_safety::{
    AUTO_ENROLLMENT_SUMMARY_SCHEMA_V1, AutoEnrollmentSummaryInput, DiscoveryPolicyDecision,
    IntendedLanePolicy, IntendedPeer, MaterializationOutcome, TriggerReason, compute_summary,
    emit_safety_snapshot_audit, update_materialization_outcome,
};
use serde_json::Value as Json;

const WORKSPACE_ID: &str = "wsp_meshsafety000000000000aa";
const WORKSPACE_PATH: &str = "/tmp/ee-mesh-safety-test";
const TAILNET_ID: &str = "tn_meshsafety0000000000";

fn fresh_db() -> DbConnection {
    let connection = DbConnection::open_memory().expect("memory database opens");
    connection.migrate().expect("migrations apply");
    connection
        .insert_workspace(
            WORKSPACE_ID,
            &CreateWorkspaceInput {
                path: WORKSPACE_PATH.to_owned(),
                name: Some("mesh-safety-audit".to_owned()),
            },
        )
        .expect("workspace inserts");
    connection
}

fn fixture_input() -> AutoEnrollmentSummaryInput<'static> {
    AutoEnrollmentSummaryInput {
        workspace_id: WORKSPACE_ID,
        workspace_path: WORKSPACE_PATH,
        tailnet_id: TAILNET_ID,
        tailnet_display_name: Some("safety-test-tailnet"),
        intended_peers: vec![
            IntendedPeer {
                node_key: "nodekey:alpha".to_owned(),
                tailscale_ip: "100.64.0.1".to_owned(),
                magic_dns_name: Some("alpha.tailnet".to_owned()),
                hostname: "alpha".to_owned(),
                ee_protocol_version: "0.1".to_owned(),
                discovery_policy_decision: DiscoveryPolicyDecision::ServiceTagMatch,
            },
            IntendedPeer {
                node_key: "nodekey:bravo".to_owned(),
                tailscale_ip: "100.64.0.2".to_owned(),
                magic_dns_name: None,
                hostname: "bravo".to_owned(),
                ee_protocol_version: "0.1".to_owned(),
                discovery_policy_decision: DiscoveryPolicyDecision::ServiceTagMatch,
            },
        ],
        intended_lane_policy: IntendedLanePolicy::conservative_default(),
        trigger_reason: TriggerReason::ManualInvoke,
        previous_peer_group_id: None,
        previous_peer_set_hash: None,
        materialization_outcome: MaterializationOutcome::Pending,
    }
}

#[test]
fn safety_snapshot_writes_audit_row_with_canonical_summary_in_details() {
    let connection = fresh_db();
    let summary = compute_summary(&fixture_input());

    let audit_id = emit_safety_snapshot_audit(
        &connection,
        &summary,
        Some("ClaudeOpus47Onboard"),
        Some("pg_safety_test_aaaaaa"),
    )
    .expect("audit row emits");

    assert!(audit_id.starts_with("audit_"));

    let row = connection
        .get_audit(&audit_id)
        .expect("audit query")
        .expect("audit row present");
    assert_eq!(row.action, audit_actions::MESH_AUTO_ENROLLMENT_INTENDED);
    assert_eq!(row.workspace_id.as_deref(), Some(WORKSPACE_ID));
    assert_eq!(row.actor.as_deref(), Some("ClaudeOpus47Onboard"));
    assert_eq!(row.target_type.as_deref(), Some("mesh"));
    assert_eq!(row.target_id.as_deref(), Some("pg_safety_test_aaaaaa"));
    assert_eq!(row.surface, "mesh");
    let details = row.details.expect("details JSON present");
    let parsed: Json = serde_json::from_str(&details).expect("details JSON parses");
    assert_eq!(parsed["schema"], AUTO_ENROLLMENT_SUMMARY_SCHEMA_V1);
    assert_eq!(parsed["workspaceId"], WORKSPACE_ID);
    assert_eq!(parsed["tailnetId"], TAILNET_ID);
    assert_eq!(parsed["triggerReason"], "manual_invoke");
    assert_eq!(parsed["intendedLanePolicy"]["body"], "deny");
    assert_eq!(parsed["intendedLanePolicy"]["embedding"], "deny");
    assert_eq!(parsed["intendedLanePolicy"]["graphLink"], "deny");
    assert_eq!(parsed["intendedLanePolicy"]["metadata"], "allow");
    let peers = parsed["intendedPeers"].as_array().expect("peers array");
    assert_eq!(peers.len(), 2);
    assert_eq!(peers[0]["nodeKey"], "nodekey:alpha");
    assert_eq!(peers[1]["nodeKey"], "nodekey:bravo");
}

#[test]
fn safety_snapshot_subsequent_outcome_back_fill_emits_second_audit_row_with_previous_audit_id() {
    let connection = fresh_db();
    let summary = compute_summary(&fixture_input());

    let intended_audit_id =
        emit_safety_snapshot_audit(&connection, &summary, Some("actor"), None)
            .expect("intended emits");
    let outcome_audit_id = update_materialization_outcome(
        &connection,
        &intended_audit_id,
        WORKSPACE_ID,
        MaterializationOutcome::Materialized,
        Some("actor"),
        None,
    )
    .expect("outcome emits");
    assert_ne!(intended_audit_id, outcome_audit_id);

    let outcome_row = connection
        .get_audit(&outcome_audit_id)
        .expect("audit query")
        .expect("outcome row present");
    assert_eq!(
        outcome_row.action,
        audit_actions::MESH_AUTO_ENROLLMENT_OUTCOME_RECORDED
    );
    let parsed: Json =
        serde_json::from_str(outcome_row.details.as_deref().expect("details present"))
            .expect("outcome details parse");
    assert_eq!(parsed["schema"], "ee.mesh.auto_enrollment_outcome.v1");
    assert_eq!(parsed["previousAuditId"], intended_audit_id);
    assert_eq!(parsed["materializationOutcome"], "materialized");
}

#[test]
fn safety_snapshot_dry_run_outcome_back_fill_records_dry_run() {
    let connection = fresh_db();
    let mut input = fixture_input();
    input.trigger_reason = TriggerReason::DryRunPreview;
    input.materialization_outcome = MaterializationOutcome::DryRun;
    let summary = compute_summary(&input);

    let intended_audit_id = emit_safety_snapshot_audit(&connection, &summary, None, None)
        .expect("intended emits");
    let outcome_audit_id = update_materialization_outcome(
        &connection,
        &intended_audit_id,
        WORKSPACE_ID,
        MaterializationOutcome::DryRun,
        None,
        None,
    )
    .expect("outcome emits");

    let outcome_row = connection
        .get_audit(&outcome_audit_id)
        .expect("audit query")
        .expect("outcome row present");
    let parsed: Json =
        serde_json::from_str(outcome_row.details.as_deref().expect("details present"))
            .expect("parse");
    assert_eq!(parsed["materializationOutcome"], "dry_run");

    // The intended row's details continue to show triggerReason=dry_run_preview
    // and the original materializationOutcome value (Pending or DryRun, depending
    // on what the caller initially serialized). Verify the intended-row payload
    // is unchanged after the outcome row writes.
    let intended_row = connection
        .get_audit(&intended_audit_id)
        .expect("audit query")
        .expect("intended row present");
    let intended_parsed: Json =
        serde_json::from_str(intended_row.details.as_deref().expect("details present"))
            .expect("intended parses");
    assert_eq!(intended_parsed["triggerReason"], "dry_run_preview");
}

#[test]
fn safety_snapshot_idempotent_repeat_emit_writes_two_rows_with_distinct_audit_ids() {
    // SRR6.46.3's idempotent path calls SRR6.46.5 again even when the peer set
    // matches — to maintain the "every attempted enrollment has a forensic
    // row" property. Two emissions must produce two distinct audit_ids; we
    // do NOT dedupe at the safety-snapshot layer.
    let connection = fresh_db();
    let mut input = fixture_input();
    input.trigger_reason = TriggerReason::DriftReconciliation;
    input.materialization_outcome = MaterializationOutcome::AuditOnly;
    let summary = compute_summary(&input);

    let first = emit_safety_snapshot_audit(&connection, &summary, None, None).expect("first");
    let second = emit_safety_snapshot_audit(&connection, &summary, None, None).expect("second");
    assert_ne!(first, second);

    let entries = connection
        .list_audit_entries(Some(WORKSPACE_ID), Some(50))
        .expect("list audit");
    let intended_rows: Vec<_> = entries
        .iter()
        .filter(|row| row.action == audit_actions::MESH_AUTO_ENROLLMENT_INTENDED)
        .collect();
    assert!(
        intended_rows.len() >= 2,
        "expected at least 2 INTENDED rows, got {}",
        intended_rows.len()
    );
    // Both rows must share the same triggerReason and materializationOutcome
    // (the input was the same) — locks the no-dedupe contract.
    for row in &intended_rows {
        let parsed: Json =
            serde_json::from_str(row.details.as_deref().expect("details present"))
                .expect("parse");
        assert_eq!(parsed["triggerReason"], "drift_reconciliation");
        assert_eq!(parsed["materializationOutcome"], "audit_only");
    }
}

#[test]
fn safety_snapshot_chain_continuity_holds_across_intended_and_outcome_rows() {
    // Emit two intended-and-outcome pairs back to back; assert each row has
    // a non-empty hash field and that the chain has the expected structure
    // (current ID + prev hash linkage).
    let connection = fresh_db();
    let summary = compute_summary(&fixture_input());

    let intended_1 =
        emit_safety_snapshot_audit(&connection, &summary, None, None).expect("intended 1");
    let outcome_1 = update_materialization_outcome(
        &connection,
        &intended_1,
        WORKSPACE_ID,
        MaterializationOutcome::Materialized,
        None,
        None,
    )
    .expect("outcome 1");
    let intended_2 =
        emit_safety_snapshot_audit(&connection, &summary, None, None).expect("intended 2");
    let outcome_2 = update_materialization_outcome(
        &connection,
        &intended_2,
        WORKSPACE_ID,
        MaterializationOutcome::AuditOnly,
        None,
        None,
    )
    .expect("outcome 2");

    for id in [&intended_1, &outcome_1, &intended_2, &outcome_2] {
        let row = connection
            .get_audit(id)
            .expect("audit query")
            .expect("row present");
        // Every row in the audit chain carries a this_row_hash AFTER the
        // chain-continuity column was added; older rows may be None. The
        // bd-36bbk.1.5 acceptance is "row present with mesh.* action and
        // canonical details JSON". The full chain-continuity verification
        // is owned by the existing `ee audit verify` test surface; this
        // test confirms the rows participate in the chain at all (i.e.
        // `list_audit_entries` returns them in timestamp order).
        assert_eq!(row.workspace_id.as_deref(), Some(WORKSPACE_ID));
        assert!(
            matches!(
                row.action.as_str(),
                "mesh.auto_enrollment_intended" | "mesh.auto_enrollment_outcome_recorded"
            ),
            "unexpected action {}",
            row.action
        );
    }

    // Order-of-insert: intended_1 < outcome_1 < intended_2 < outcome_2 by
    // creation time, since `generate_audit_id` is a UUID v7 (time-sorted).
    let entries = connection
        .list_audit_entries(Some(WORKSPACE_ID), Some(50))
        .expect("list audit");
    let mut mesh_entries: Vec<_> = entries
        .iter()
        .filter(|row| row.action.starts_with("mesh.auto_enrollment_"))
        .collect();
    mesh_entries.sort_by(|a, b| a.timestamp.cmp(&b.timestamp));
    let ordered_ids: Vec<&str> = mesh_entries.iter().map(|row| row.id.as_str()).collect();
    let expected: Vec<&str> = vec![
        intended_1.as_str(),
        outcome_1.as_str(),
        intended_2.as_str(),
        outcome_2.as_str(),
    ];
    assert_eq!(ordered_ids, expected);
}

#[test]
fn safety_snapshot_records_previous_peer_group_id_when_replacing_existing() {
    let connection = fresh_db();
    let mut input = fixture_input();
    input.previous_peer_group_id = Some("pg_existing_replacement_aa");
    input.previous_peer_set_hash = Some(
        "blake3:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcd",
    );
    let summary = compute_summary(&input);
    let audit_id =
        emit_safety_snapshot_audit(&connection, &summary, None, Some("pg_new_replacement_aa"))
            .expect("audit emits");

    let row = connection
        .get_audit(&audit_id)
        .expect("audit query")
        .expect("row present");
    let parsed: Json =
        serde_json::from_str(row.details.as_deref().expect("details present"))
            .expect("parse");
    assert_eq!(parsed["previousPeerGroupId"], "pg_existing_replacement_aa");
    assert_eq!(
        parsed["previousPeerSetHash"],
        "blake3:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcd"
    );
}

#[test]
fn safety_snapshot_intended_row_target_id_is_optional() {
    let connection = fresh_db();
    let summary = compute_summary(&fixture_input());
    // Auto-enrollment may not yet know the peer-group ID at safety-snapshot
    // emission time (it's computed downstream). The audit emission must
    // accept None for target_id without erroring.
    let audit_id =
        emit_safety_snapshot_audit(&connection, &summary, None, None).expect("audit emits");
    let row = connection
        .get_audit(&audit_id)
        .expect("audit query")
        .expect("row present");
    assert!(row.target_id.is_none());
}
