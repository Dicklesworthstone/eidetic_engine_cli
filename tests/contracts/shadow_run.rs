#![allow(clippy::expect_used)]

//! Gate 14: shadow-run comparison and promotion-guard contracts.

use std::env;
use std::fs;
use std::path::PathBuf;

use ee::models::{DecisionPlane, DecisionRecord};
use ee::output::{ShadowRunReport, render_shadow_run_json};
use ee::shadow::pack::{PackShadowOutput, compare_outputs};
use ee::shadow::{
    PolicyDomain, PolicyInventoryStatus, PolicyMaturity, ShadowGateConfig, ShadowPromotionGuards,
    ShadowVerdict, candidate_promotion_allowed, find_shadow_policy_inventory_entry,
    shadow_policy_inventory,
};
use serde_json::Value;

type TestResult = Result<(), String>;

fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

fn ensure_contains(haystack: &str, needle: &str, context: &str) -> TestResult {
    ensure(
        haystack.contains(needle),
        format!("{context}: expected to contain '{needle}' but got:\n{haystack}"),
    )
}

fn golden_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("golden")
        .join("shadow")
        .join(format!("{name}.json.golden"))
}

fn schema_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("docs")
        .join("schemas")
        .join(name)
}

fn read_schema(name: &str) -> Result<Value, String> {
    let path = schema_path(name);
    let raw = fs::read_to_string(&path)
        .map_err(|error| format!("failed to read {}: {error}", path.display()))?;
    serde_json::from_str(&raw)
        .map_err(|error| format!("failed to parse {}: {error}", path.display()))
}

fn object_field<'a>(value: &'a Value, field: &str, context: &str) -> Result<&'a Value, String> {
    value
        .get(field)
        .ok_or_else(|| format!("{context}: missing object field {field}"))
}

fn nested_object_field<'a>(
    mut value: &'a Value,
    path: &[&str],
    context: &str,
) -> Result<&'a Value, String> {
    for field in path {
        value = object_field(value, field, context)?;
    }
    Ok(value)
}

fn required_contains(schema: &Value, field: &str, context: &str) -> TestResult {
    let required = object_field(schema, "required", context)?
        .as_array()
        .ok_or_else(|| format!("{context}: required is not an array"))?;
    ensure(
        required.iter().any(|value| value.as_str() == Some(field)),
        format!("{context}: required fields do not include {field}"),
    )
}

fn enum_contains(value: &Value, expected: &[&str], context: &str) -> TestResult {
    let values = object_field(value, "enum", context)?
        .as_array()
        .ok_or_else(|| format!("{context}: enum is not an array"))?;
    for expected_value in expected {
        ensure(
            values
                .iter()
                .any(|value| value.as_str() == Some(*expected_value)),
            format!("{context}: enum does not include {expected_value}"),
        )?;
    }
    Ok(())
}

fn redaction_fields_are_default_deny(schema: &Value, context: &str) -> TestResult {
    let required_fields = [
        "rawMemoryBodyPresent",
        "rawMailBodyPresent",
        "rawPolicyPayloadPresent",
        "absoluteHostPathPresent",
        "secretsPresent",
    ];
    let redaction = nested_object_field(schema, &["$defs", "redactionPosture"], context)?;
    for field in required_fields {
        required_contains(redaction, field, context)?;
        let const_value = nested_object_field(redaction, &["properties", field, "const"], context)?;
        ensure(
            const_value.as_bool() == Some(false),
            format!("{context}: {field} must be const false"),
        )?;
    }
    Ok(())
}

fn assert_golden(name: &str, actual: &str) -> TestResult {
    let path = golden_path(name);
    if env::var("UPDATE_GOLDEN").is_ok() {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        }
        fs::write(&path, actual).map_err(|error| error.to_string())?;
        return Ok(());
    }

    let expected = fs::read_to_string(&path)
        .map_err(|error| format!("failed to read {}: {error}", path.display()))?;
    ensure(actual == expected, format!("golden mismatch for {name}"))
}

#[test]
fn gate14_policy_domain_includes_verification_admission() -> TestResult {
    ensure(
        PolicyDomain::all()
            .iter()
            .any(|domain| *domain == PolicyDomain::VerificationAdmission),
        "PolicyDomain::all must include verification_admission",
    )?;
    ensure(
        PolicyDomain::VerificationAdmission.as_str() == "verification_admission",
        "verification admission domain string is stable",
    )
}

#[test]
fn gate14_shadow_policy_inventory_lists_pack_and_cache_incumbents_and_candidates() -> TestResult {
    ensure(
        shadow_policy_inventory().len() >= 8,
        "inventory includes all currently documented shadow surfaces",
    )?;

    let pack_incumbent = find_shadow_policy_inventory_entry("incumbent.pack.mmr_redundancy")
        .ok_or_else(|| "missing pack incumbent".to_string())?;
    ensure(
        pack_incumbent.policy_domain == PolicyDomain::PackSelection.as_str(),
        "pack incumbent domain is stable",
    )?;
    ensure(
        pack_incumbent.status == PolicyInventoryStatus::Incumbent,
        "pack incumbent status is stable",
    )?;
    ensure(
        pack_incumbent.maturity == PolicyMaturity::Stable,
        "pack incumbent maturity is stable",
    )?;
    ensure(
        pack_incumbent.required_inputs.contains(&"token_budget"),
        "pack incumbent records token budget input",
    )?;
    ensure(
        pack_incumbent.shadowable_without_mutation,
        "pack incumbent is shadowable without mutation",
    )?;

    let pack_candidate = find_shadow_policy_inventory_entry("candidate.pack.facility_location")
        .ok_or_else(|| "missing pack candidate".to_string())?;
    ensure(
        pack_candidate.status == PolicyInventoryStatus::Candidate,
        "pack candidate status is stable",
    )?;
    ensure(
        pack_candidate.maturity == PolicyMaturity::Experimental,
        "pack candidate maturity is stable",
    )?;
    ensure(
        pack_candidate.supported_cohorts.contains(&"swarm_heavy"),
        "pack candidate is available for swarm-heavy replay",
    )?;

    let cache_incumbent = find_shadow_policy_inventory_entry("incumbent.cache.no_cache")
        .ok_or_else(|| "missing cache incumbent".to_string())?;
    ensure(
        cache_incumbent.policy_domain == PolicyDomain::CacheAdmission.as_str(),
        "cache incumbent domain is stable",
    )?;
    ensure(
        cache_incumbent.status == PolicyInventoryStatus::Incumbent,
        "cache incumbent status is stable",
    )?;

    let cache_candidate = find_shadow_policy_inventory_entry("candidate.cache.s3_fifo")
        .ok_or_else(|| "missing cache candidate".to_string())?;
    ensure(
        cache_candidate.status == PolicyInventoryStatus::Candidate,
        "cache candidate status is stable",
    )?;
    ensure(
        cache_candidate
            .known_degraded_modes
            .contains(&"cache_admission_unavailable"),
        "cache candidate records known degraded mode",
    )
}

#[test]
fn gate14_shadow_policy_inventory_abstains_for_unsupported_resource_budget_domain() -> TestResult {
    let unsupported =
        find_shadow_policy_inventory_entry("unsupported.resource_profile_budget_admission")
            .ok_or_else(|| "missing unsupported resource-budget policy".to_string())?;

    ensure(
        unsupported.policy_domain == "resource_profile_budget_admission",
        "unsupported domain is still inventoried",
    )?;
    ensure(
        unsupported.status == PolicyInventoryStatus::Unsupported,
        "unsupported status is stable",
    )?;
    ensure(
        unsupported.maturity == PolicyMaturity::Unsupported,
        "unsupported maturity is stable",
    )?;
    ensure(
        !unsupported.shadowable_without_mutation,
        "unsupported domain cannot be shadowed",
    )?;
    ensure(
        unsupported.abstention_reason == Some("unsupported_policy_domain"),
        "unsupported domain abstains instead of promoting or rejecting",
    )
}

#[test]
fn gate14_pack_policy_compare_records_incumbent_and_candidate_without_mutation() -> TestResult {
    let incumbent = PackShadowOutput {
        selected_ids: vec!["mem.release_rule".to_string(), "mem.old_note".to_string()],
        tokens_used: 900,
        quality_score: 0.72,
        time_us: 100,
    };
    let candidate = PackShadowOutput {
        selected_ids: vec![
            "mem.release_rule".to_string(),
            "mem.failure_evidence".to_string(),
            "mem.preflight_warning".to_string(),
        ],
        tokens_used: 980,
        quality_score: 0.86,
        time_us: 120,
    };

    let (verdict, metrics) = compare_outputs(&incumbent, &candidate, &ShadowGateConfig::default());

    ensure(
        verdict == ShadowVerdict::CandidateBetter || verdict == ShadowVerdict::Divergent,
        "candidate should be materially different and higher quality",
    )?;
    ensure(
        metrics.candidate_quality > metrics.incumbent_quality,
        "quality improves",
    )?;
    ensure(
        incumbent.selected_ids.len() == 2,
        "incumbent output is unchanged",
    )?;
    ensure(
        candidate.selected_ids.len() == 3,
        "candidate output is unchanged",
    )
}

#[test]
fn gate14_promotion_is_blocked_by_safety_guards_even_when_candidate_wins() -> TestResult {
    let guards = ShadowPromotionGuards {
        dropped_critical_warnings: true,
        redaction_differences: true,
        p99_regression: false,
        tail_risk_regression: true,
        shadow_mismatch_above_tolerance: false,
    };

    ensure(guards.blocks_promotion(), "guards block promotion")?;
    ensure(
        guards.blocker_codes()
            == vec![
                "dropped_critical_warnings",
                "redaction_differences",
                "tail_risk_regression",
            ],
        "blocker codes are stable",
    )?;
    ensure(
        !candidate_promotion_allowed(ShadowVerdict::CandidateBetter, &guards),
        "candidate better verdict is still blocked by guards",
    )
}

#[test]
fn gate14_shadow_run_report_matches_golden() -> TestResult {
    let mut report = ShadowRunReport::new("candidate.pack.facility_location", "incumbent.pack.mmr")
        .with_command("shadow compare");

    let divergent = DecisionRecord::builder()
        .plane(DecisionPlane::Packing)
        .policy_id("candidate.pack.facility_location")
        .decision_id("decision_pack_001")
        .trace_id("trace_gate14_001")
        .decided_at("2026-05-01T00:00:00Z")
        .outcome("include:mem.failure_evidence")
        .incumbent_outcome("exclude:mem.failure_evidence")
        .reason("candidate surfaced high-severity failure evidence")
        .confidence(0.91)
        .shadow(true)
        .build();

    let matched = DecisionRecord::builder()
        .plane(DecisionPlane::CacheAdmission)
        .policy_id("candidate.cache.s3_fifo")
        .decision_id("decision_cache_001")
        .trace_id("trace_gate14_001")
        .decided_at("2026-05-01T00:00:01Z")
        .outcome("admit")
        .incumbent_outcome("admit")
        .reason("both policies admit repeated key")
        .confidence(0.83)
        .shadow(true)
        .build();

    report.add_from_record(&divergent);
    report.add_from_record(&matched);
    report.compute_avg_confidence();

    let json = render_shadow_run_json(&report);
    ensure_contains(&json, "\"divergenceRate\":0.5000", "divergence rate")?;
    ensure_contains(&json, "\"traceId\":\"trace_gate14_001\"", "trace linkage")?;
    assert_golden("pack_policy_compare", &(json + "\n"))
}

#[test]
fn gate14_shadow_policy_experiment_schema_pins_admission_contract() -> TestResult {
    let schema = read_schema("ee.shadow_policy_experiment.v1.json")?;
    for field in [
        "schema",
        "experimentId",
        "shadowRunId",
        "sideEffectFree",
        "decisionPlane",
        "policyDomain",
        "incumbentPolicyId",
        "candidatePolicyId",
        "traceInput",
        "admission",
        "redactionPosture",
        "evidence",
        "degraded",
    ] {
        required_contains(&schema, field, "shadow policy experiment schema")?;
    }

    let policy_domain = nested_object_field(&schema, &["$defs", "policyDomain"], "policy domain")?;
    enum_contains(
        policy_domain,
        &[
            "pack_selection",
            "cache_admission",
            "verification_admission",
            "curation_filter",
        ],
        "policy domain",
    )?;

    let admission = nested_object_field(&schema, &["$defs", "admission"], "admission")?;
    for field in [
        "status",
        "sourceAuthority",
        "replayEvidencePosture",
        "localCargoPosture",
        "rchPosture",
        "supportedDomain",
        "abstentionReasons",
    ] {
        required_contains(admission, field, "admission")?;
    }
    let abstention_reasons = nested_object_field(
        admission,
        &["properties", "abstentionReasons", "items"],
        "admission",
    )?;
    enum_contains(
        abstention_reasons,
        &[
            "missing_replay_evidence",
            "stale_source_authority",
            "unsafe_local_cargo_posture",
            "unsupported_policy_domain",
            "rch_blocked",
        ],
        "admission abstention reasons",
    )?;
    redaction_fields_are_default_deny(&schema, "shadow policy experiment schema")
}

#[test]
fn gate14_shadow_policy_inventory_schema_pins_policy_ids_and_abstention_contract() -> TestResult {
    let schema = read_schema("ee.shadow_policy_inventory.v1.json")?;
    for field in [
        "schema",
        "inventoryId",
        "sideEffectFree",
        "policies",
        "redactionPosture",
        "degraded",
    ] {
        required_contains(&schema, field, "shadow policy inventory schema")?;
    }

    let policy_entry = nested_object_field(
        &schema,
        &["$defs", "policyEntry"],
        "shadow policy inventory entry",
    )?;
    for field in [
        "policyId",
        "policyDomain",
        "status",
        "maturity",
        "requiredInputs",
        "supportedCohorts",
        "knownDegradedModes",
        "sideEffectFree",
        "shadowableWithoutMutation",
        "abstentionReason",
    ] {
        required_contains(policy_entry, field, "shadow policy inventory entry")?;
    }

    let policy_id = nested_object_field(&schema, &["$defs", "policyId"], "policy id")?;
    enum_contains(
        policy_id,
        &[
            "incumbent.pack.mmr_redundancy",
            "candidate.pack.facility_location",
            "incumbent.cache.no_cache",
            "candidate.cache.s3_fifo",
            "candidate.verification.environment_attestation",
            "unsupported.resource_profile_budget_admission",
        ],
        "policy id",
    )?;

    let policy_domain = nested_object_field(&schema, &["$defs", "policyDomain"], "policy domain")?;
    enum_contains(
        policy_domain,
        &[
            "pack_selection",
            "cache_admission",
            "verification_admission",
            "curation_filter",
            "resource_profile_budget_admission",
        ],
        "policy domain",
    )?;

    let status = nested_object_field(&schema, &["$defs", "policyStatus"], "policy status")?;
    enum_contains(status, &["incumbent", "candidate", "unsupported"], "status")?;

    let maturity = nested_object_field(&schema, &["$defs", "policyMaturity"], "maturity")?;
    enum_contains(
        maturity,
        &["stable", "experimental", "fixture_only", "unsupported"],
        "maturity",
    )?;

    redaction_fields_are_default_deny(&schema, "shadow policy inventory schema")
}

#[test]
fn gate14_shadow_policy_comparison_schema_pins_promotion_contract() -> TestResult {
    let schema = read_schema("ee.shadow_policy_comparison.v1.json")?;
    for field in [
        "schema",
        "experimentId",
        "shadowRunId",
        "sideEffectFree",
        "decisionPlane",
        "policyDomain",
        "summary",
        "decisions",
        "verdict",
        "abstentionReasons",
        "safetyGuardsTriggered",
        "confidence",
        "redactionPosture",
        "evidence",
        "counterEvidence",
        "nextCommands",
        "degraded",
    ] {
        required_contains(&schema, field, "shadow policy comparison schema")?;
    }

    let verdict = nested_object_field(&schema, &["properties", "verdict"], "verdict")?;
    enum_contains(
        verdict,
        &["promote", "hold", "reject", "abstain"],
        "verdict",
    )?;

    let summary = nested_object_field(&schema, &["$defs", "comparisonSummary"], "summary")?;
    for field in [
        "totalDecisions",
        "divergedDecisions",
        "matchedDecisions",
        "p50LatencyMs",
        "p95LatencyMs",
        "p99LatencyMs",
        "outputBytesDelta",
        "memoryBytesDelta",
        "degradedDelta",
        "utilityDelta",
    ] {
        required_contains(summary, field, "summary")?;
    }

    let decision = nested_object_field(&schema, &["$defs", "shadowDecision"], "shadow decision")?;
    for field in [
        "decisionId",
        "traceId",
        "decisionPlane",
        "incumbentOutcomeHash",
        "candidateOutcomeHash",
        "diverged",
        "confidence",
        "evidenceRefs",
    ] {
        required_contains(decision, field, "shadow decision")?;
    }
    redaction_fields_are_default_deny(&schema, "shadow policy comparison schema")
}
