//! SRR6.45 executable mesh search-surrogate compatibility proof.
//!
//! The scenarios are intentionally pure and deterministic: they model two mesh
//! nodes by comparing a remote surrogate descriptor against a local model and
//! body-availability posture. The e2e shell wrapper runs this test through RCH
//! and preserves the structured `ee.test_event.v1` evidence lines.

use ee::search::{
    SearchSurrogateAuditDecision, SearchSurrogateAuditInput, SearchSurrogateDegradedCode,
    SearchSurrogateDescriptor, SearchSurrogateModelFingerprint, SearchSurrogatePolicy,
    SearchSurrogateType, audit_search_surrogate,
};
use serde_json::{Value, json};

type TestResult = Result<(), String>;

const CONTENT_HASH: &str = "blake3:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const OBSERVED_AT: &str = "2026-05-19T23:30:00Z";

fn local_model() -> SearchSurrogateModelFingerprint {
    SearchSurrogateModelFingerprint::new("hash-256", "2026-05-19", ["normalize_l2", "utf8"])
}

fn remote_surrogate(
    surrogate_type: SearchSurrogateType,
    model_id: &str,
    model_version: &str,
) -> SearchSurrogateDescriptor {
    SearchSurrogateDescriptor {
        surrogate_type,
        model_fingerprint: SearchSurrogateModelFingerprint::new(
            model_id,
            model_version,
            ["utf8", "normalize_l2", "utf8"],
        ),
        content_hash: CONTENT_HASH.to_owned(),
        valid_until: Some("2026-05-20T00:00:00Z".to_owned()),
    }
}

fn audit_event(
    scenario: &str,
    surrogate: &SearchSurrogateDescriptor,
    decision: SearchSurrogateAuditDecision,
    codes: &[SearchSurrogateDegradedCode],
) -> Value {
    json!({
        "schema": "ee.test_event.v1",
        "surface": "mesh_search_surrogate_audit",
        "scenario": scenario,
        "surrogateType": surrogate.surrogate_type.as_str(),
        "decision": decision.as_str(),
        "degradedCodes": codes.iter().map(|code| code.as_str()).collect::<Vec<_>>(),
    })
}

fn assert_outcome(
    scenario: &str,
    outcome: &ee::search::SearchSurrogateAuditOutcome,
    expected_decision: SearchSurrogateAuditDecision,
    expected_codes: &[SearchSurrogateDegradedCode],
) -> TestResult {
    if outcome.decision != expected_decision {
        return Err(format!(
            "{scenario}: expected decision {}, got {}",
            expected_decision.as_str(),
            outcome.decision.as_str()
        ));
    }
    if outcome.degraded_codes != expected_codes {
        return Err(format!(
            "{scenario}: expected degraded codes {:?}, got {:?}",
            expected_codes
                .iter()
                .map(|code| code.as_str())
                .collect::<Vec<_>>(),
            outcome
                .degraded_codes
                .iter()
                .map(|code| code.as_str())
                .collect::<Vec<_>>()
        ));
    }
    Ok(())
}

#[test]
fn metadata_only_policy_denies_embedding_export_and_keeps_lexical_fallback() -> TestResult {
    let surrogate = remote_surrogate(SearchSurrogateType::Embedding, "hash-256", "2026-05-19");
    let policy = SearchSurrogatePolicy::metadata_only_for(SearchSurrogateType::Embedding);
    let local_model = local_model();
    let input = SearchSurrogateAuditInput {
        surrogate: &surrogate,
        policy: &policy,
        local_model: &local_model,
        local_content_hash: Some(CONTENT_HASH),
        observed_at: OBSERVED_AT,
        local_body_available: true,
    };

    let outcome = audit_search_surrogate(&input);

    assert_outcome(
        "metadata_only_embedding_denied",
        &outcome,
        SearchSurrogateAuditDecision::LexicalFallback,
        &[
            SearchSurrogateDegradedCode::Denied,
            SearchSurrogateDegradedCode::LexicalFallbackUsed,
        ],
    )?;
    let audit_json = outcome.data_json(&input);
    if audit_json.to_string().contains("raw private memory body") {
        return Err("audit JSON leaked raw body text".to_owned());
    }
    println!(
        "{}",
        audit_event(
            "metadata_only_embedding_denied",
            &surrogate,
            outcome.decision,
            &outcome.degraded_codes
        )
    );

    Ok(())
}

#[test]
fn metadata_only_policy_allows_lexical_metadata_surrogate_reuse() -> TestResult {
    let surrogate = remote_surrogate(
        SearchSurrogateType::LexicalMetadata,
        "hash-256",
        "2026-05-19",
    );
    let policy = SearchSurrogatePolicy::metadata_only_for(SearchSurrogateType::LexicalMetadata);
    let local_model = local_model();
    let input = SearchSurrogateAuditInput {
        surrogate: &surrogate,
        policy: &policy,
        local_model: &local_model,
        local_content_hash: Some(CONTENT_HASH),
        observed_at: OBSERVED_AT,
        local_body_available: false,
    };

    let outcome = audit_search_surrogate(&input);

    assert_outcome(
        "metadata_only_lexical_metadata_reused",
        &outcome,
        SearchSurrogateAuditDecision::ReuseRemote,
        &[],
    )?;
    println!(
        "{}",
        audit_event(
            "metadata_only_lexical_metadata_reused",
            &surrogate,
            outcome.decision,
            &outcome.degraded_codes
        )
    );

    Ok(())
}

#[test]
fn incompatible_remote_version_falls_back_when_body_is_unavailable() -> TestResult {
    let surrogate = remote_surrogate(SearchSurrogateType::Embedding, "hash-256", "2026-04-01");
    let policy = SearchSurrogatePolicy::allow_reuse_after_compatibility_check();
    let local_model = local_model();
    let input = SearchSurrogateAuditInput {
        surrogate: &surrogate,
        policy: &policy,
        local_model: &local_model,
        local_content_hash: Some(CONTENT_HASH),
        observed_at: OBSERVED_AT,
        local_body_available: false,
    };

    let outcome = audit_search_surrogate(&input);

    assert_outcome(
        "two_node_incompatible_version_lexical_fallback",
        &outcome,
        SearchSurrogateAuditDecision::LexicalFallback,
        &[
            SearchSurrogateDegradedCode::Incompatible,
            SearchSurrogateDegradedCode::LexicalFallbackUsed,
        ],
    )?;
    println!(
        "{}",
        audit_event(
            "two_node_incompatible_version_lexical_fallback",
            &surrogate,
            outcome.decision,
            &outcome.degraded_codes
        )
    );

    Ok(())
}

#[test]
fn incompatible_remote_version_recomputes_when_body_is_available() -> TestResult {
    let surrogate = remote_surrogate(
        SearchSurrogateType::Embedding,
        "model2vec-base",
        "2026-05-19",
    );
    let policy = SearchSurrogatePolicy::allow_reuse_after_compatibility_check();
    let local_model = local_model();
    let input = SearchSurrogateAuditInput {
        surrogate: &surrogate,
        policy: &policy,
        local_model: &local_model,
        local_content_hash: Some(CONTENT_HASH),
        observed_at: OBSERVED_AT,
        local_body_available: true,
    };

    let outcome = audit_search_surrogate(&input);

    assert_outcome(
        "two_node_incompatible_model_recomputed",
        &outcome,
        SearchSurrogateAuditDecision::RecomputeLocal,
        &[
            SearchSurrogateDegradedCode::Incompatible,
            SearchSurrogateDegradedCode::Recomputed,
        ],
    )?;
    println!(
        "{}",
        audit_event(
            "two_node_incompatible_model_recomputed",
            &surrogate,
            outcome.decision,
            &outcome.degraded_codes
        )
    );

    Ok(())
}

#[test]
fn stale_content_hash_invalidates_surrogate_and_recomputes_locally() -> TestResult {
    let surrogate = remote_surrogate(SearchSurrogateType::Embedding, "hash-256", "2026-05-19");
    let policy = SearchSurrogatePolicy::allow_reuse_after_compatibility_check();
    let local_model = local_model();
    let input = SearchSurrogateAuditInput {
        surrogate: &surrogate,
        policy: &policy,
        local_model: &local_model,
        local_content_hash: Some("blake3:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"),
        observed_at: OBSERVED_AT,
        local_body_available: true,
    };

    let outcome = audit_search_surrogate(&input);

    assert_outcome(
        "content_hash_mismatch_recomputed",
        &outcome,
        SearchSurrogateAuditDecision::RecomputeLocal,
        &[SearchSurrogateDegradedCode::Recomputed],
    )?;
    println!(
        "{}",
        audit_event(
            "content_hash_mismatch_recomputed",
            &surrogate,
            outcome.decision,
            &outcome.degraded_codes
        )
    );

    Ok(())
}
