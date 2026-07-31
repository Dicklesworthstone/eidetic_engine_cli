//! SRR6.46.8 identity-change-guard audit contract (bd-tc-epic-qzk7o.2.5,
//! part b of the T1.7 honesty backfill — this file was a newline-only stub).
//!
//! Asserts the guard's audit-facing surface: refusal verdicts carry the
//! canonical degraded codes, stable `kind` tags for audit emission, literal
//! copy-paste repair commands with shell-safe quoting, and serde payloads
//! whose evidence fields survive a round-trip (what the audit row records).

use ee::mesh::identity_change_guard::{
    AUTO_ENROLLMENT_NODE_KEY_CHANGED_CODE, BoundIdentity, CurrentIdentity,
    HELLO_RESPONDER_NODE_KEY_MISMATCH_CODE, IdentityGuardVerdict, ResponderBindVerdict,
    evaluate_identity_guard, evaluate_responder_bind,
};

fn bound(tailnet: &str, display: Option<&str>, node_key: &str) -> BoundIdentity {
    BoundIdentity {
        tailnet_id: tailnet.to_owned(),
        tailnet_display_name: display.map(str::to_owned),
        materialized_on_node_key: node_key.to_owned(),
    }
}

fn current(tailnet: &str, display: Option<&str>, node_key: &str) -> CurrentIdentity {
    CurrentIdentity {
        tailnet_id: tailnet.to_owned(),
        tailnet_display_name: display.map(str::to_owned),
        self_node_key: node_key.to_owned(),
    }
}

#[test]
fn node_key_change_refuses_with_canonical_code_kind_and_repair() {
    let verdict = evaluate_identity_guard(
        Some(&bound("tn-1", Some("Acme"), "nodekey:aa")),
        &current("tn-1", Some("Acme"), "nodekey:bb"),
    );

    assert!(verdict.refuses_auto_enrollment());
    assert_eq!(
        verdict.refusal_code(),
        Some(AUTO_ENROLLMENT_NODE_KEY_CHANGED_CODE)
    );
    assert_eq!(verdict.kind_str(), "node_key_changed");

    let repair = verdict
        .repair_command("/work/space")
        .expect("node-key refusal must carry a repair command");
    assert!(repair.contains("ee mesh disable --workspace \"/work/space\""));
    assert!(repair.contains("--reason"));
    assert!(repair.contains("ee mesh auto-enroll --workspace \"/work/space\""));

    match &verdict {
        IdentityGuardVerdict::NodeKeyChanged {
            bound_node_key,
            current_node_key,
        } => {
            assert_eq!(bound_node_key, "nodekey:aa");
            assert_eq!(current_node_key, "nodekey:bb");
        }
        other => panic!("expected NodeKeyChanged, got {other:?}"),
    }

    let payload = serde_json::to_value(&verdict).expect("verdict serializes for audit emission");
    assert_eq!(payload["kind"], "node_key_changed");
    assert_eq!(payload["bound_node_key"], "nodekey:aa");
    assert_eq!(payload["current_node_key"], "nodekey:bb");
}

#[test]
fn repair_command_shell_quotes_hostile_workspace_paths() {
    let verdict = evaluate_identity_guard(
        Some(&bound("tn-1", None, "nodekey:aa")),
        &current("tn-1", None, "nodekey:bb"),
    );
    let repair = verdict
        .repair_command("/tmp/ws\"$(touch pwned)`x`\\")
        .expect("refusal carries repair");
    assert!(repair.contains("\\\"") && repair.contains("\\$") && repair.contains("\\`"));
    assert!(
        !repair.contains("\"$("),
        "unescaped substitution in audit repair: {repair}"
    );
}

#[test]
fn tailnet_change_outranks_node_key_change_in_evaluation_order() {
    let verdict = evaluate_identity_guard(
        Some(&bound("tn-1", None, "nodekey:aa")),
        &current("tn-2", None, "nodekey:bb"),
    );
    assert_eq!(verdict.kind_str(), "tailnet_changed");
}

#[test]
fn clean_and_matching_states_neither_refuse_nor_carry_codes() {
    let none = evaluate_identity_guard(None, &current("tn-1", None, "nodekey:aa"));
    assert_eq!(none.kind_str(), "no_bound_identity");
    assert!(!none.refuses_auto_enrollment());
    assert_eq!(none.refusal_code(), None);
    assert_eq!(none.repair_command("/w"), None);

    let same = evaluate_identity_guard(
        Some(&bound("tn-1", Some("Acme"), "nodekey:aa")),
        &current("tn-1", Some("Acme"), "nodekey:aa"),
    );
    assert_eq!(same.kind_str(), "no_change");
    assert!(!same.refuses_auto_enrollment());
}

#[test]
fn responder_bind_refuses_only_on_node_key_mismatch_with_canonical_code() {
    let mismatch = evaluate_responder_bind(Some(&bound("tn-1", None, "nodekey:aa")), "nodekey:bb");
    assert!(mismatch.refuses_bind());
    assert_eq!(
        mismatch.refusal_code(),
        Some(HELLO_RESPONDER_NODE_KEY_MISMATCH_CODE)
    );

    let matching = evaluate_responder_bind(Some(&bound("tn-1", None, "nodekey:aa")), "nodekey:aa");
    assert!(matches!(matching, ResponderBindVerdict::Bind));
    assert!(!matching.refuses_bind());
    assert_eq!(matching.refusal_code(), None);

    let clean = evaluate_responder_bind(None, "nodekey:aa");
    assert!(matches!(clean, ResponderBindVerdict::BindNoBoundIdentity));
    assert!(!clean.refuses_bind());
}
