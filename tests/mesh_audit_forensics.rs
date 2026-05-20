//! SRR6.39 mesh audit and forensics ledger e2e companion.
//!
//! The shell e2e wrapper emits the operator-visible scenario matrix, then runs
//! this integration test through RCH. The test stays in-process so it can pin
//! the redaction-safe ledger contract without needing a live mesh peer.

use std::collections::BTreeSet;

use ee::mesh::audit::{
    MeshAuditDetails, MeshAuditEvent, MeshAuditEventInput, MeshAuditEventKind,
    compute_mesh_audit_event, support_bundle_entry,
};
use serde_json::Value;

type TestResult = Result<(), String>;

const WORKSPACE_ID: &str = "wsp_mesh_audit_node01";
const ORIGIN_WORKSPACE_ID: &str = "wsp_mesh_audit_node02";
const TARGET_WORKSPACE_ID: &str = "wsp_mesh_audit_target";
const WORKSPACE_SCOPE: &str = "workspace:repo-only";
const REDACTED_SENTINEL: &str =
    "MESH_AUDIT_FORENSICS_SENTINEL_sk_live_51N0TREAL000000000000000000000000";
const PAYLOAD_DIGEST: &str =
    "blake3:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

#[derive(Clone, Copy)]
struct ForensicScenario {
    scenario: &'static str,
    kind: MeshAuditEventKind,
    peer_id: Option<&'static str>,
    local_row_refs: &'static [&'static str],
    cached_body_refs: &'static [&'static str],
    policy_outcome: &'static str,
    body_fetch_allowed: Option<bool>,
}

const FORENSIC_SCENARIOS: &[ForensicScenario] = &[
    ForensicScenario {
        scenario: "peer_enrollment",
        kind: MeshAuditEventKind::PeerEnrollment,
        peer_id: Some("peer_forensics_alpha"),
        local_row_refs: &[],
        cached_body_refs: &[],
        policy_outcome: "allow",
        body_fetch_allowed: None,
    },
    ForensicScenario {
        scenario: "preview_consent",
        kind: MeshAuditEventKind::SharePreviewConsent,
        peer_id: Some("peer_forensics_alpha"),
        local_row_refs: &["mem_preview_public", "mem_preview_private"],
        cached_body_refs: &[],
        policy_outcome: "dry_run_only",
        body_fetch_allowed: None,
    },
    ForensicScenario {
        scenario: "policy_decision",
        kind: MeshAuditEventKind::PolicyDecision,
        peer_id: Some("peer_forensics_alpha"),
        local_row_refs: &["mem_preview_private"],
        cached_body_refs: &[],
        policy_outcome: "deny_body",
        body_fetch_allowed: Some(false),
    },
    ForensicScenario {
        scenario: "export",
        kind: MeshAuditEventKind::Export,
        peer_id: Some("peer_forensics_alpha"),
        local_row_refs: &["mem_export_a", "mem_export_b", "mem_export_a"],
        cached_body_refs: &[],
        policy_outcome: "metadata_exported",
        body_fetch_allowed: Some(false),
    },
    ForensicScenario {
        scenario: "import",
        kind: MeshAuditEventKind::Import,
        peer_id: Some("peer_forensics_beta"),
        local_row_refs: &["remote_mem_imported"],
        cached_body_refs: &["cache_body_remote_001"],
        policy_outcome: "metadata_imported",
        body_fetch_allowed: Some(false),
    },
    ForensicScenario {
        scenario: "denied_body_fetch",
        kind: MeshAuditEventKind::BodyFetch,
        peer_id: Some("peer_forensics_beta"),
        local_row_refs: &["remote_mem_imported"],
        cached_body_refs: &["cache_body_remote_001"],
        policy_outcome: "deny_body_fetch",
        body_fetch_allowed: Some(false),
    },
    ForensicScenario {
        scenario: "withdrawal",
        kind: MeshAuditEventKind::Withdrawal,
        peer_id: Some("peer_forensics_alpha"),
        local_row_refs: &["remote_mem_withdrawn"],
        cached_body_refs: &["cache_body_withdrawn_001"],
        policy_outcome: "purge_peer_cache",
        body_fetch_allowed: None,
    },
    ForensicScenario {
        scenario: "quarantine",
        kind: MeshAuditEventKind::Quarantine,
        peer_id: Some("peer_forensics_beta"),
        local_row_refs: &["remote_mem_quarantined"],
        cached_body_refs: &["cache_body_quarantine_001"],
        policy_outcome: "content_hash_mismatch",
        body_fetch_allowed: Some(false),
    },
    ForensicScenario {
        scenario: "revision",
        kind: MeshAuditEventKind::Revision,
        peer_id: Some("peer_forensics_alpha"),
        local_row_refs: &["mem_revision_parent", "mem_revision_child"],
        cached_body_refs: &[],
        policy_outcome: "revision_notice_linked",
        body_fetch_allowed: None,
    },
];

fn details_for(scenario: &ForensicScenario) -> Result<MeshAuditDetails, String> {
    let mut details = MeshAuditDetails::default();
    details
        .insert_reference("scenario", scenario.scenario)
        .map_err(|error| error.to_string())?;
    details
        .insert_reference("policy_outcome", scenario.policy_outcome)
        .map_err(|error| error.to_string())?;
    details
        .insert_count("local_row_count", scenario.local_row_refs.len() as u64)
        .map_err(|error| error.to_string())?;
    details
        .insert_count("cached_body_count", scenario.cached_body_refs.len() as u64)
        .map_err(|error| error.to_string())?;
    details
        .insert_digest("payload_digest", PAYLOAD_DIGEST)
        .map_err(|error| error.to_string())?;
    details
        .insert_redacted_text(
            "operator_note",
            "body_preview",
            &format!("support bundle must not contain {REDACTED_SENTINEL}"),
        )
        .map_err(|error| error.to_string())?;
    if let Some(allowed) = scenario.body_fetch_allowed {
        details
            .insert_bool("body_fetch_allowed", allowed)
            .map_err(|error| error.to_string())?;
    }
    Ok(details)
}

fn input_for(
    scenario: &ForensicScenario,
    previous_event_hash: Option<String>,
) -> Result<MeshAuditEventInput, String> {
    Ok(MeshAuditEventInput {
        workspace_id: WORKSPACE_ID.to_owned(),
        event_kind: scenario.kind,
        peer_id: scenario.peer_id.map(str::to_owned),
        origin_workspace_id: Some(ORIGIN_WORKSPACE_ID.to_owned()),
        target_workspace_id: Some(TARGET_WORKSPACE_ID.to_owned()),
        workspace_scope: Some(WORKSPACE_SCOPE.to_owned()),
        policy_decision_id: Some(format!("policy_decision_{}", scenario.scenario)),
        local_row_refs: scenario
            .local_row_refs
            .iter()
            .map(|value| (*value).to_owned())
            .collect(),
        cached_body_refs: scenario
            .cached_body_refs
            .iter()
            .map(|value| (*value).to_owned())
            .collect(),
        details: details_for(scenario)?,
        previous_event_hash,
    })
}

fn assert_no_secret(surface: &str, rendered: &str) -> TestResult {
    if rendered.contains(REDACTED_SENTINEL) {
        return Err(format!("{surface} leaked sentinel secret: {rendered}"));
    }
    Ok(())
}

fn build_chain() -> Result<Vec<MeshAuditEvent>, String> {
    let mut previous_hash = None;
    let mut events = Vec::new();
    for scenario in FORENSIC_SCENARIOS {
        let input = input_for(scenario, previous_hash.clone())?;
        let event = compute_mesh_audit_event(&input).map_err(|error| error.to_string())?;
        let repeated = compute_mesh_audit_event(&input).map_err(|error| error.to_string())?;
        if event.event_id != repeated.event_id || event.event_hash != repeated.event_hash {
            return Err(format!(
                "event identity/hash drifted for {}",
                scenario.scenario
            ));
        }
        assert_no_secret(
            scenario.scenario,
            &serde_json::to_string(&event).map_err(|error| error.to_string())?,
        )?;
        previous_hash = Some(event.event_hash.clone());
        events.push(event);
    }
    Ok(events)
}

#[test]
fn forensic_event_chain_covers_mesh_boundary_scenarios() -> TestResult {
    let events = build_chain()?;
    let scenarios = FORENSIC_SCENARIOS
        .iter()
        .map(|scenario| scenario.scenario)
        .collect::<BTreeSet<_>>();
    for required in [
        "peer_enrollment",
        "preview_consent",
        "policy_decision",
        "export",
        "import",
        "denied_body_fetch",
        "withdrawal",
        "quarantine",
        "revision",
    ] {
        if !scenarios.contains(required) {
            return Err(format!("forensic chain missing scenario {required}"));
        }
    }

    for (index, event) in events.iter().enumerate() {
        if event.schema != "ee.mesh.audit_event.v1" {
            return Err(format!("unexpected event schema: {}", event.schema));
        }
        if !event.event_id.starts_with("mesh_audit_") {
            return Err(format!("unstable event id prefix: {}", event.event_id));
        }
        if !event.event_hash.starts_with("blake3:") {
            return Err(format!("event hash must be digest-qualified: {event:?}"));
        }
        if index == 0 {
            if event.previous_event_hash.is_some() {
                return Err(format!(
                    "first event should not have previous hash: {event:?}"
                ));
            }
        } else if event.previous_event_hash.as_deref()
            != Some(events[index - 1].event_hash.as_str())
        {
            return Err(format!(
                "event {} did not link to previous hash",
                event.event_kind
            ));
        }
    }

    let export_event = events
        .iter()
        .find(|event| event.event_kind == "export")
        .ok_or_else(|| "export event missing".to_owned())?;
    let expected_export_refs = vec!["mem_export_a".to_owned(), "mem_export_b".to_owned()];
    if export_event.local_row_refs != expected_export_refs {
        return Err(format!(
            "export refs should be sorted and deduplicated: {:?}",
            export_event.local_row_refs
        ));
    }

    Ok(())
}

#[test]
fn support_bundle_projection_keeps_forensics_redaction_safe() -> TestResult {
    let events = build_chain()?;

    for event in &events {
        let bundle_entry = support_bundle_entry(event);
        let rendered = serde_json::to_string(&bundle_entry).map_err(|error| error.to_string())?;
        assert_no_secret("support bundle projection", &rendered)?;
        if rendered.contains("operator_note")
            || rendered.contains("payload_digest")
            || rendered.contains("mem_")
            || rendered.contains("cache_body_")
        {
            return Err(format!(
                "support bundle projection leaked event details or refs: {rendered}"
            ));
        }
        if bundle_entry.event_hash != event.event_hash
            || bundle_entry.previous_event_hash != event.previous_event_hash
        {
            return Err(format!(
                "support bundle hash continuity drifted for {}",
                event.event_kind
            ));
        }
        if bundle_entry.local_row_count != event.local_row_refs.len()
            || bundle_entry.cached_body_ref_count != event.cached_body_refs.len()
        {
            return Err(format!(
                "support bundle counts drifted for {}",
                event.event_kind
            ));
        }
    }

    Ok(())
}

#[test]
fn failure_mode_fixtures_cover_missing_and_corrupt_ledgers() -> TestResult {
    for (fixture_json, expected_code) in [
        (
            include_str!("fixtures/failure_modes/mesh_audit_ledger_missing.json"),
            "mesh_audit_ledger_missing",
        ),
        (
            include_str!("fixtures/failure_modes/mesh_audit_ledger_corrupt.json"),
            "mesh_audit_ledger_corrupt",
        ),
    ] {
        let fixture: Value =
            serde_json::from_str(fixture_json).map_err(|error| error.to_string())?;
        if fixture["schema"] != "ee.failure_mode_fixture.v1"
            || fixture["code"] != expected_code
            || fixture["expected_emission"]["code"] != expected_code
        {
            return Err(format!(
                "mesh audit failure fixture drifted for {expected_code}: {fixture}"
            ));
        }
        let fixture_text = fixture.to_string();
        if !fixture_text.contains("mesh audit ledger") || !fixture_text.contains("ee audit verify")
        {
            return Err(format!(
                "mesh audit fixture should explain ledger repair: {fixture}"
            ));
        }
        assert_no_secret(expected_code, &fixture_text)?;
    }

    Ok(())
}
