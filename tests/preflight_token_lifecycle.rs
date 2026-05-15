#![allow(clippy::expect_used, clippy::unwrap_used)]
// These contract-style tests fail fast on setup drift so assertion failures keep the token lifecycle signal readable.

use std::collections::HashSet;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use chrono::{Duration, Utc};
use ee::core::preflight_guard::{
    GuardAction, GuardMatch, MatchResolution, PreflightMemoryMatch, RuleSource,
};
use ee::core::preflight_token::{
    BYPASS_RATE_LIMIT_EXCEEDED, BYPASS_TOKEN_EXPIRED, BYPASS_TOKEN_REVOKED, DEFAULT_TTL_MINUTES,
    IssueBypassTokenOptions, RecordPreflightBypassAuditOptions, RevokeBypassTokenOptions,
    VerifyBypassTokenOptions, generate_bypass_token, issue_bypass_token, list_bypass_tokens,
    record_preflight_bypass_audit, revoke_bypass_token, token_hash, verify_bypass_token,
};
use ee::db::{CreateWorkspaceInput, DbConnection, audit_actions};

const WORKSPACE_ID: &str = "wsp_01234567890123456789012345";

fn test_connection() -> DbConnection {
    let connection = DbConnection::open_memory().expect("memory database opens");
    connection.migrate().expect("schema migration succeeds");
    connection
        .insert_workspace(
            WORKSPACE_ID,
            &CreateWorkspaceInput {
                path: "/tmp/preflight-token-test".to_owned(),
                name: Some("preflight-token-test".to_owned()),
            },
        )
        .expect("workspace inserts");
    connection
}

fn issue_options(reason: &str, max_uses: u32) -> IssueBypassTokenOptions {
    IssueBypassTokenOptions {
        workspace_id: WORKSPACE_ID.to_owned(),
        issuer_workspace: "/tmp/preflight-token-test".to_owned(),
        reason: reason.to_owned(),
        ttl_minutes: Some(DEFAULT_TTL_MINUTES),
        max_uses: Some(max_uses),
        actor: Some("test-agent".to_owned()),
        now: Some(Utc::now()),
    }
}

#[test]
fn generated_tokens_are_256_bit_base64url_values() {
    let mut seen = HashSet::new();
    for _ in 0..10_000 {
        let token = generate_bypass_token().expect("token generation succeeds");
        assert!(!token.contains('='));
        assert!(
            token
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_')
        );
        let bytes = URL_SAFE_NO_PAD
            .decode(token.as_bytes())
            .expect("token decodes as base64url-no-pad");
        assert_eq!(bytes.len(), 32);
        assert!(
            seen.insert(token),
            "token collision in 10k generated samples"
        );
    }
}

#[test]
fn issued_token_is_stored_as_hash_metadata_and_audited() {
    let connection = test_connection();
    let report = issue_bypass_token(&connection, &issue_options("approve one command", 1))
        .expect("token issues");
    let hash = token_hash(&report.token);
    let stored = connection
        .get_preflight_bypass_token(&hash)
        .expect("token lookup succeeds")
        .expect("token row exists");

    assert_eq!(stored.token_hash, hash);
    assert_ne!(stored.token_hash, report.token);
    assert_eq!(stored.used_count, 0);
    assert_eq!(stored.max_uses, 1);
    assert_eq!(stored.reason, "approve one command");
    assert!(stored.revoked_at.is_none());

    let listed = list_bypass_tokens(&connection, WORKSPACE_ID).expect("tokens list");
    assert_eq!(listed.tokens.len(), 1);
    assert_eq!(listed.tokens[0].token_hash_prefix, report.token_hash_prefix);
    assert!(
        !serde_json::to_string(&listed)
            .unwrap()
            .contains(&report.token)
    );

    let audits = connection
        .list_audit_by_action(audit_actions::PREFLIGHT_BYPASS_TOKEN_ISSUE, Some(10))
        .expect("issue audit is listable");
    assert_eq!(audits.len(), 1);
    assert_eq!(
        audits[0].target_id.as_deref(),
        Some(report.token_hash_prefix.as_str())
    );
}

#[test]
fn one_shot_token_allows_first_use_and_rejects_second_use() {
    let connection = test_connection();
    let issued =
        issue_bypass_token(&connection, &issue_options("one shot", 1)).expect("token issues");
    let use_report = verify_bypass_token(
        &connection,
        &VerifyBypassTokenOptions {
            workspace_id: WORKSPACE_ID.to_owned(),
            token: issued.token.clone(),
            actor: Some("test-agent".to_owned()),
            now: Some(Utc::now()),
        },
    )
    .expect("first use succeeds");

    assert_eq!(use_report.used_count, 1);
    assert_eq!(use_report.remaining_uses, 0);

    let second_use = verify_bypass_token(
        &connection,
        &VerifyBypassTokenOptions {
            workspace_id: WORKSPACE_ID.to_owned(),
            token: issued.token,
            actor: Some("test-agent".to_owned()),
            now: Some(Utc::now()),
        },
    )
    .expect_err("second use is rejected");
    assert_eq!(second_use.code, "bypass_token_exhausted");
}

#[test]
fn expiry_revocation_and_rate_limit_are_enforced() {
    let connection = test_connection();
    let now = Utc::now();

    let expired = issue_bypass_token(
        &connection,
        &IssueBypassTokenOptions {
            now: Some(now - Duration::minutes(20)),
            ..issue_options("expired", 1)
        },
    )
    .expect("expired fixture token issues");
    let expired_error = verify_bypass_token(
        &connection,
        &VerifyBypassTokenOptions {
            workspace_id: WORKSPACE_ID.to_owned(),
            token: expired.token,
            actor: Some("test-agent".to_owned()),
            now: Some(now),
        },
    )
    .expect_err("expired token is rejected");
    assert_eq!(expired_error.code, BYPASS_TOKEN_EXPIRED);

    let revoked = issue_bypass_token(&connection, &issue_options("revoke", 1))
        .expect("revocation fixture token issues");
    revoke_bypass_token(
        &connection,
        &RevokeBypassTokenOptions {
            workspace_id: WORKSPACE_ID.to_owned(),
            token: revoked.token.clone(),
            actor: Some("test-agent".to_owned()),
            now: Some(Utc::now()),
        },
    )
    .expect("token revokes");
    let revoked_error = verify_bypass_token(
        &connection,
        &VerifyBypassTokenOptions {
            workspace_id: WORKSPACE_ID.to_owned(),
            token: revoked.token,
            actor: Some("test-agent".to_owned()),
            now: Some(Utc::now()),
        },
    )
    .expect_err("revoked token is rejected");
    assert_eq!(revoked_error.code, BYPASS_TOKEN_REVOKED);

    for index in 0..5 {
        let issued = issue_bypass_token(&connection, &issue_options(&format!("rate {index}"), 1))
            .expect("rate fixture token issues");
        verify_bypass_token(
            &connection,
            &VerifyBypassTokenOptions {
                workspace_id: WORKSPACE_ID.to_owned(),
                token: issued.token,
                actor: Some("test-agent".to_owned()),
                now: Some(Utc::now()),
            },
        )
        .expect("token use within hourly limit succeeds");
    }

    let over_limit = issue_bypass_token(&connection, &issue_options("rate limit", 1))
        .expect("rate limit fixture token issues");
    let rate_error = verify_bypass_token(
        &connection,
        &VerifyBypassTokenOptions {
            workspace_id: WORKSPACE_ID.to_owned(),
            token: over_limit.token,
            actor: Some("test-agent".to_owned()),
            now: Some(Utc::now()),
        },
    )
    .expect_err("sixth hourly token use is rejected");
    assert_eq!(rate_error.code, BYPASS_RATE_LIMIT_EXCEEDED);
}

#[test]
fn bypass_audit_records_token_hash_matches_and_blocking_memories() {
    let connection = test_connection();
    let issued =
        issue_bypass_token(&connection, &issue_options("audited bypass", 1)).expect("token issues");
    let report = record_preflight_bypass_audit(
        &connection,
        &RecordPreflightBypassAuditOptions {
            workspace_id: WORKSPACE_ID.to_owned(),
            token: issued.token.clone(),
            actor: Some("test-agent".to_owned()),
            command: "rm -rf /tmp/work".to_owned(),
            matches: vec![GuardMatch {
                rule_id: "builtin:rm_rf_tmp".to_owned(),
                pattern: "*rm -rf /tmp/*".to_owned(),
                action: GuardAction::Halt,
                message: "Destructive recursive delete requires confirmation.".to_owned(),
                source: RuleSource::Builtin {
                    name: "rm_rf_tmp".to_owned(),
                },
                resolution: MatchResolution::BypassedWithToken,
            }],
            matched_memories: vec![PreflightMemoryMatch {
                memory_id: "mem_risk000000000000000000001".to_owned(),
                kind: "risk".to_owned(),
                content: "Prior rm -rf /tmp/work incident".to_owned(),
                provenance_uri: Some("cass-session://incident-rm-rf#L1-L3".to_owned()),
                severity: "high",
                severity_source: "inferred_from_memory_kind",
                score: 0.75,
                matched_terms: vec!["rm".to_owned(), "recursive".to_owned()],
            }],
        },
    )
    .expect("bypass audit records");

    assert_eq!(report.token_hash_prefix, issued.token_hash_prefix);

    let audits = connection
        .list_audit_by_action(audit_actions::PREFLIGHT_BYPASS, Some(10))
        .expect("bypass audit is listable");
    assert_eq!(audits.len(), 1);
    assert_eq!(
        audits[0].target_id.as_deref(),
        Some(issued.token_hash_prefix.as_str())
    );
    assert_eq!(
        audits[0].target_type.as_deref(),
        Some("preflight_bypass_token")
    );

    let details: serde_json::Value =
        serde_json::from_str(audits[0].details.as_deref().expect("audit details"))
            .expect("details are JSON");
    assert_eq!(details["schema"], "ee.preflight.bypass.v1");
    assert_eq!(details["token_hash_prefix"], issued.token_hash_prefix);
    assert_eq!(details["command"], "rm -rf /tmp/work");
    assert_eq!(details["rule_ids"][0], "builtin:rm_rf_tmp");
    assert_eq!(
        details["matched_memory_ids"][0],
        "mem_risk000000000000000000001"
    );
    assert_eq!(
        details["matched_memories"][0]["provenance_uri"],
        "cass-session://incident-rm-rf#L1-L3"
    );
    assert!(
        !audits[0]
            .details
            .as_deref()
            .expect("audit details")
            .contains(&issued.token),
        "raw bypass token must not be written to audit details"
    );
}
