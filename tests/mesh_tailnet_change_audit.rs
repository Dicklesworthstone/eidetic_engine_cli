//! SRR6.46.8 tailnet-change audit contract (bd-tc-epic-qzk7o.2.5, part b of
//! the T1.7 honesty backfill — this file was a newline-only stub).
//!
//! Asserts the tailnet-change half of the identity guard: a changed tailnet
//! id refuses auto-enrollment with the canonical degraded code and an
//! audit-ready payload carrying both identities; an admin rename (same id,
//! different display name) is informational and proceeds; display-name
//! presence transitions are the documented probe artifact and are treated
//! as no-change.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use ee::mesh::identity_change_guard::{
    AUTO_ENROLLMENT_TAILNET_CHANGED_CODE, BoundIdentity, CurrentIdentity, IdentityGuardVerdict,
    evaluate_identity_guard,
};

fn bound(tailnet: &str, display: Option<&str>) -> BoundIdentity {
    BoundIdentity {
        tailnet_id: tailnet.to_owned(),
        tailnet_display_name: display.map(str::to_owned),
        materialized_on_node_key: "nodekey:aa".to_owned(),
    }
}

fn current(tailnet: &str, display: Option<&str>) -> CurrentIdentity {
    CurrentIdentity {
        tailnet_id: tailnet.to_owned(),
        tailnet_display_name: display.map(str::to_owned),
        self_node_key: "nodekey:aa".to_owned(),
    }
}

#[test]
fn tailnet_change_refuses_with_canonical_code_repair_and_audit_payload() {
    let verdict = evaluate_identity_guard(
        Some(&bound("tn-old", Some("Old Corp"))),
        &current("tn-new", Some("New Corp")),
    );

    assert!(verdict.refuses_auto_enrollment());
    assert_eq!(
        verdict.refusal_code(),
        Some(AUTO_ENROLLMENT_TAILNET_CHANGED_CODE)
    );
    assert_eq!(verdict.kind_str(), "tailnet_changed");

    let repair = verdict
        .repair_command("/work/space")
        .expect("tailnet refusal must carry a repair command");
    assert!(repair.contains("ee mesh disable --workspace \"/work/space\""));
    assert!(repair.contains("ee mesh auto-enroll --workspace \"/work/space\""));

    let payload = serde_json::to_value(&verdict).expect("verdict serializes for audit emission");
    assert_eq!(payload["kind"], "tailnet_changed");
    assert_eq!(payload["bound_tailnet_id"], "tn-old");
    assert_eq!(payload["current_tailnet_id"], "tn-new");
    assert_eq!(payload["bound_tailnet_display_name"], "Old Corp");
    assert_eq!(payload["current_tailnet_display_name"], "New Corp");
}

#[test]
fn tailnet_rename_is_informational_and_proceeds() {
    let verdict = evaluate_identity_guard(
        Some(&bound("tn-1", Some("Old Name"))),
        &current("tn-1", Some("New Name")),
    );

    assert_eq!(verdict.kind_str(), "tailnet_renamed");
    assert!(!verdict.refuses_auto_enrollment());
    assert_eq!(verdict.refusal_code(), None);
    assert_eq!(verdict.repair_command("/w"), None);

    match &verdict {
        IdentityGuardVerdict::TailnetRenamed {
            tailnet_id,
            bound_display_name,
            current_display_name,
        } => {
            assert_eq!(tailnet_id, "tn-1");
            assert_eq!(bound_display_name.as_deref(), Some("Old Name"));
            assert_eq!(current_display_name.as_deref(), Some("New Name"));
        }
        other => panic!("expected TailnetRenamed, got {other:?}"),
    }
}

#[test]
fn display_name_presence_transitions_are_no_change_probe_artifacts() {
    let some_to_none =
        evaluate_identity_guard(Some(&bound("tn-1", Some("Acme"))), &current("tn-1", None));
    assert_eq!(some_to_none.kind_str(), "no_change");

    let none_to_some =
        evaluate_identity_guard(Some(&bound("tn-1", None)), &current("tn-1", Some("Acme")));
    assert_eq!(none_to_some.kind_str(), "no_change");
}
