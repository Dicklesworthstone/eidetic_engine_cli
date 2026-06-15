use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
};

use ee::shadow::{
    RESOURCE_QUEUE_PRESSURE_BOUNDED_PREVIEW_MAX_CHARS, RESOURCE_QUEUE_PRESSURE_MAX_SOURCE_REFS,
    RESOURCE_QUEUE_PRESSURE_REDACTION_POSTURE, ResourceCostClass,
    ResourceQueuePressureBackoffAdvice, ResourceQueuePressureBackoffInput,
    ResourceQueuePressureInventory, ResourceQueuePressureReasonCode, ResourceQueuePressureReport,
    ResourceQueuePressureSourceKind, ResourceQueuePressureSourceRef,
    ResourceQueuePressureSourceState, evaluate_resource_queue_pressure_backoff,
};
use serde::{Deserialize, de::DeserializeOwned};
use serde_json::{Value, json};

type TestResult = Result<(), String>;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Corpus {
    schema: String,
    bead_id: String,
    clauses: Vec<Clause>,
    required_coverage: RequiredCoverage,
    cases: Vec<Case>,
    no_mutation: NoMutation,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Clause {
    id: String,
    level: String,
    requirement: String,
    status: String,
    cases: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RequiredCoverage {
    levels: Vec<String>,
    reason_codes: Vec<String>,
    source_kinds: Vec<String>,
    source_states: Vec<String>,
    decisions: Vec<String>,
    redaction_checks: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Case {
    id: String,
    description: String,
    estimated_cost_class: String,
    claim_gate_safe_to_claim: bool,
    source_refs: Vec<SourceRefFixture>,
    expected: ExpectedCase,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SourceRefFixture {
    kind: String,
    state: String,
    reason_code: Option<String>,
    source_schema: Option<String>,
    hash: Option<String>,
    bounded_preview: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ExpectedCase {
    level: String,
    reason_codes: Vec<String>,
    abstained_sources: Vec<String>,
    decision: String,
    primary_reason: String,
    contributing_reasons: Vec<String>,
    blocked_by: Vec<String>,
    next_safe_action: String,
    what_would_change: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct NoMutation {
    forbidden_snippets: Vec<String>,
    redaction_forbidden_substrings: Vec<String>,
}

#[test]
fn resource_admission_queue_pressure_conformance_matches_golden() -> TestResult {
    let corpus: Corpus = read_json(&[
        "tests",
        "fixtures",
        "resource_admission",
        "queue_pressure",
        "conformance_cases.json",
    ])?;
    ensure(
        corpus.schema == "ee.resource_admission.queue_pressure_conformance.v1",
        format!("unexpected corpus schema {}", corpus.schema),
    )?;
    assert_matrix_is_accounted_for(&corpus)?;

    let actual = build_conformance_report(&corpus)?;
    let expected: Value = read_json(&[
        "tests",
        "fixtures",
        "golden",
        "resource_admission",
        "queue_pressure_conformance.json.golden",
    ])?;

    if actual != expected {
        return Err(format!(
            "queue-pressure conformance golden drifted\nexpected:\n{}\nactual:\n{}",
            pretty_json(&expected)?,
            pretty_json(&actual)?
        ));
    }

    assert_no_mutation_contract(&corpus, &actual)?;
    assert_live_style_pressure_log(&corpus.no_mutation)?;
    Ok(())
}

fn build_conformance_report(corpus: &Corpus) -> Result<Value, String> {
    let mut covered_levels = BTreeSet::new();
    let mut covered_reason_codes = BTreeSet::new();
    let mut covered_source_kinds = BTreeSet::new();
    let mut covered_source_states = BTreeSet::new();
    let mut covered_decisions = BTreeSet::new();
    let mut case_reports = Vec::new();
    let mut representative_diag = None;
    let mut representative_claim_gate = None;
    let mut support_bundle_summary = Vec::new();
    let mut can_authorize_claim_ever_true = false;

    for case in &corpus.cases {
        ensure(
            !case.description.trim().is_empty(),
            format!("case {} has empty description", case.id),
        )?;
        let inventory = ResourceQueuePressureInventory::new(
            case.source_refs
                .iter()
                .map(source_ref_from_fixture)
                .collect::<Result<Vec<_>, _>>()?,
        );
        ensure(
            inventory.source_refs().len() <= RESOURCE_QUEUE_PRESSURE_MAX_SOURCE_REFS,
            format!("case {} exceeded max source refs", case.id),
        )?;

        let pressure = inventory.report();
        let advice = evaluate_resource_queue_pressure_backoff(&ResourceQueuePressureBackoffInput {
            queue_pressure: pressure.clone(),
            estimated_cost_class: cost_class_from_str(&case.estimated_cost_class)?,
            claim_gate_safe_to_claim: case.claim_gate_safe_to_claim,
        });

        assert_case_expectations(case, &pressure, &advice)?;
        can_authorize_claim_ever_true |= pressure.can_authorize_claim || advice.can_authorize_claim;

        covered_levels.insert(pressure.level.as_str().to_owned());
        covered_reason_codes.extend(pressure.reason_codes.iter().cloned());
        covered_source_kinds.extend(
            pressure
                .source_refs
                .iter()
                .map(|source| source.kind.as_str().to_owned()),
        );
        covered_source_states.extend(
            pressure
                .source_refs
                .iter()
                .map(|source| source.state.as_str().to_owned()),
        );
        covered_decisions.insert(advice.decision.as_str().to_owned());

        let case_report = render_case_summary(case, &pressure, &advice);
        if case.id == "rch_slots_exhausted_wait" {
            representative_diag = Some(render_diag_output(case, &pressure, &advice));
        }
        if case.id == "healthy_low_pressure_unsafe_claim_gate" {
            representative_claim_gate = Some(render_claim_gate_embedding(case, &advice));
        }
        if matches!(
            case.id.as_str(),
            "rch_slots_exhausted_wait" | "agent_mail_recovery_corrupt_read_failure"
        ) {
            support_bundle_summary.push(json!({
                "caseId": case.id,
                "sourceKinds": pressure
                    .source_refs
                    .iter()
                    .map(|source| source.kind.as_str())
                    .collect::<Vec<_>>(),
                "reasonCodes": pressure.reason_codes,
            }));
        }
        case_reports.push(case_report);
    }

    assert_required_coverage("levels", &corpus.required_coverage.levels, &covered_levels)?;
    assert_required_coverage(
        "reasonCodes",
        &corpus.required_coverage.reason_codes,
        &covered_reason_codes,
    )?;
    assert_required_coverage(
        "sourceKinds",
        &corpus.required_coverage.source_kinds,
        &covered_source_kinds,
    )?;
    assert_required_coverage(
        "sourceStates",
        &corpus.required_coverage.source_states,
        &covered_source_states,
    )?;
    assert_required_coverage(
        "decisions",
        &corpus.required_coverage.decisions,
        &covered_decisions,
    )?;
    assert_redaction_checks(&corpus.required_coverage.redaction_checks)?;

    let live_log = live_style_log_summary(&corpus.no_mutation)?;
    Ok(json!({
        "schema": "ee.resource_admission.queue_pressure_conformance_report.v1",
        "beadId": corpus.bead_id,
        "matrix": {
            "clauseCount": corpus.clauses.len(),
            "testedClauseCount": corpus
                .clauses
                .iter()
                .filter(|clause| clause.status == "tested")
                .count(),
            "coverage": {
                "levels": covered_levels.into_iter().collect::<Vec<_>>(),
                "reasonCodes": covered_reason_codes.into_iter().collect::<Vec<_>>(),
                "sourceKinds": covered_source_kinds.into_iter().collect::<Vec<_>>(),
                "sourceStates": covered_source_states.into_iter().collect::<Vec<_>>(),
                "decisions": covered_decisions.into_iter().collect::<Vec<_>>(),
            }
        },
        "cases": case_reports,
        "representativeOutputs": {
            "diagResourceAdmission": representative_diag
                .ok_or_else(|| "missing diag representative".to_owned())?,
            "workPacketClaimGateEmbedding": representative_claim_gate
                .ok_or_else(|| "missing claim-gate representative".to_owned())?,
        },
        "supportBundleSummary": {
            "redactionPosture": RESOURCE_QUEUE_PRESSURE_REDACTION_POSTURE,
            "redactedQueuePressureEvidence": support_bundle_summary,
        },
        "liveStyleLog": live_log,
        "noMutation": {
            "sideEffectFree": true,
            "advisoryOnly": true,
            "canAuthorizeClaimEverTrue": can_authorize_claim_ever_true,
            "forbiddenSnippetCount": corpus.no_mutation.forbidden_snippets.len(),
        }
    }))
}

fn assert_matrix_is_accounted_for(corpus: &Corpus) -> Result<(), String> {
    let case_ids: BTreeSet<_> = corpus.cases.iter().map(|case| case.id.as_str()).collect();
    for clause in &corpus.clauses {
        ensure(
            clause.level == "MUST" || clause.level == "SHOULD",
            format!(
                "clause {} has unsupported level {}",
                clause.id, clause.level
            ),
        )?;
        ensure(
            matches!(
                clause.status.as_str(),
                "tested" | "divergent" | "intentionally_not_applicable"
            ),
            format!(
                "clause {} has unsupported status {}",
                clause.id, clause.status
            ),
        )?;
        ensure(
            !clause.requirement.trim().is_empty(),
            format!("clause {} has empty requirement", clause.id),
        )?;
        if clause.status == "tested" {
            ensure(
                !clause.cases.is_empty(),
                format!("tested clause {} has no cases", clause.id),
            )?;
        }
        for case_id in &clause.cases {
            ensure(
                case_ids.contains(case_id.as_str()),
                format!("clause {} references unknown case {case_id}", clause.id),
            )?;
        }
    }
    Ok(())
}

fn assert_case_expectations(
    case: &Case,
    pressure: &ResourceQueuePressureReport,
    advice: &ResourceQueuePressureBackoffAdvice,
) -> Result<(), String> {
    let expected = &case.expected;
    ensure(
        pressure.level.as_str() == expected.level,
        format!(
            "case {} level drift: expected {}, got {}",
            case.id,
            expected.level,
            pressure.level.as_str()
        ),
    )?;
    ensure(
        pressure.reason_codes == expected.reason_codes,
        format!(
            "case {} reason drift: expected {:?}, got {:?}",
            case.id, expected.reason_codes, pressure.reason_codes
        ),
    )?;
    ensure(
        pressure.abstained_sources == expected.abstained_sources,
        format!(
            "case {} abstained source drift: expected {:?}, got {:?}",
            case.id, expected.abstained_sources, pressure.abstained_sources
        ),
    )?;
    ensure(
        !pressure.can_authorize_claim,
        format!("case {} queue pressure authorized a claim", case.id),
    )?;
    ensure(
        pressure.redaction_posture == RESOURCE_QUEUE_PRESSURE_REDACTION_POSTURE,
        format!("case {} changed redaction posture", case.id),
    )?;
    for source_ref in &pressure.source_refs {
        ensure(
            source_ref.bounded_preview.as_ref().is_none_or(|preview| {
                preview.len() <= RESOURCE_QUEUE_PRESSURE_BOUNDED_PREVIEW_MAX_CHARS
            }),
            format!("case {} source preview exceeded bounded length", case.id),
        )?;
    }

    ensure(
        advice.decision.as_str() == expected.decision,
        format!(
            "case {} decision drift: expected {}, got {}",
            case.id,
            expected.decision,
            advice.decision.as_str()
        ),
    )?;
    ensure(
        !advice.can_authorize_claim,
        format!("case {} advice authorized a claim", case.id),
    )?;
    ensure(
        advice.primary_reason == expected.primary_reason,
        format!("case {} primary reason drift", case.id),
    )?;
    ensure(
        advice.contributing_reasons == expected.contributing_reasons,
        format!("case {} contributing reason drift", case.id),
    )?;
    ensure(
        advice.blocked_by == expected.blocked_by,
        format!("case {} blocker drift", case.id),
    )?;
    ensure(
        advice.next_safe_action == expected.next_safe_action,
        format!("case {} next-safe-action drift", case.id),
    )?;
    ensure(
        advice.what_would_change == expected.what_would_change,
        format!("case {} what-would-change drift", case.id),
    )?;
    Ok(())
}

fn render_case_summary(
    case: &Case,
    pressure: &ResourceQueuePressureReport,
    advice: &ResourceQueuePressureBackoffAdvice,
) -> Value {
    json!({
        "id": case.id,
        "level": pressure.level.as_str(),
        "reasonCodes": pressure.reason_codes,
        "abstainedSources": pressure.abstained_sources,
        "decision": advice.decision.as_str(),
        "primaryReason": advice.primary_reason,
        "contributingReasons": advice.contributing_reasons,
        "blockedBy": advice.blocked_by,
        "nextSafeAction": advice.next_safe_action,
        "whatWouldChange": advice.what_would_change,
        "canAuthorizeClaim": advice.can_authorize_claim,
    })
}

fn render_diag_output(
    case: &Case,
    pressure: &ResourceQueuePressureReport,
    advice: &ResourceQueuePressureBackoffAdvice,
) -> Value {
    json!({
        "caseId": case.id,
        "queuePressure": {
            "level": pressure.level.as_str(),
            "canAuthorizeClaim": pressure.can_authorize_claim,
            "reasonCodes": pressure.reason_codes,
            "abstainedSources": pressure.abstained_sources,
            "redactionPosture": pressure.redaction_posture,
            "sourceRefCount": pressure.source_refs.len(),
        },
        "advice": {
            "decision": advice.decision.as_str(),
            "primaryReason": advice.primary_reason,
            "nextSafeAction": advice.next_safe_action,
            "whatWouldChange": advice.what_would_change,
            "canAuthorizeClaim": advice.can_authorize_claim,
        }
    })
}

fn render_claim_gate_embedding(case: &Case, advice: &ResourceQueuePressureBackoffAdvice) -> Value {
    json!({
        "caseId": case.id,
        "safeToClaim": case.claim_gate_safe_to_claim,
        "queuePressureAdvice": {
            "decision": advice.decision.as_str(),
            "canAuthorizeClaim": advice.can_authorize_claim,
            "blockedBy": advice.blocked_by,
        }
    })
}

fn source_ref_from_fixture(
    fixture: &SourceRefFixture,
) -> Result<ResourceQueuePressureSourceRef, String> {
    let mut source_ref = ResourceQueuePressureSourceRef::new(
        source_kind_from_str(&fixture.kind)?,
        source_state_from_str(&fixture.state)?,
    );
    if let Some(reason_code) = &fixture.reason_code {
        source_ref = source_ref.with_reason_code(reason_code_from_str(reason_code)?);
    }
    if let Some(source_schema) = &fixture.source_schema {
        source_ref = source_ref.with_source_schema(source_schema_literal(source_schema)?);
    }
    if let Some(hash) = &fixture.hash {
        source_ref = source_ref.with_hash(hash);
    }
    if let Some(preview) = &fixture.bounded_preview {
        source_ref = source_ref.with_bounded_preview(preview);
    }
    Ok(source_ref)
}

fn assert_required_coverage(
    label: &str,
    required: &[String],
    covered: &BTreeSet<String>,
) -> Result<(), String> {
    for item in required {
        ensure(
            covered.contains(item),
            format!("missing queue-pressure {label} coverage for {item}"),
        )?;
    }
    Ok(())
}

fn assert_redaction_checks(required: &[String]) -> Result<(), String> {
    let required: BTreeSet<_> = required.iter().map(String::as_str).collect();
    for check in [
        "no_raw_mail_bodies",
        "no_raw_command_argv",
        "no_host_private_paths",
        "no_full_dirty_path_listing",
    ] {
        ensure(
            required.contains(check),
            format!("missing redaction coverage check {check}"),
        )?;
    }
    Ok(())
}

fn assert_no_mutation_contract(corpus: &Corpus, actual: &Value) -> Result<(), String> {
    ensure(
        actual["noMutation"]["canAuthorizeClaimEverTrue"] == false,
        "queue-pressure conformance observed claim authorization".to_owned(),
    )?;
    let source = fs::read_to_string(repo_root().join("src/shadow.rs"))
        .map_err(|error| format!("read shadow source: {error}"))?;
    let queue_pressure_source = source_slice(
        &source,
        "pub const RESOURCE_QUEUE_PRESSURE_REDACTION_POSTURE",
        "#[derive(Clone, Copy, Debug, Eq, PartialEq)]\npub struct ResourceAdmissionInput",
    )?;
    let artifacts = [
        queue_pressure_source.to_owned(),
        fs::read_to_string(repo_root().join(
            "tests/fixtures/resource_admission/queue_pressure/live_style_pressure_log.jsonl",
        ))
        .map_err(|error| format!("read queue-pressure log: {error}"))?,
        pretty_json(actual)?,
    ];

    for artifact in &artifacts {
        for snippet in &corpus.no_mutation.forbidden_snippets {
            ensure(
                !artifact.contains(snippet),
                format!(
                    "queue-pressure conformance contains forbidden mutation snippet {snippet:?}"
                ),
            )?;
        }
        for snippet in &corpus.no_mutation.redaction_forbidden_substrings {
            ensure(
                !artifact.contains(snippet),
                format!(
                    "queue-pressure conformance contains forbidden redaction snippet {snippet:?}"
                ),
            )?;
        }
    }
    Ok(())
}

fn assert_live_style_pressure_log(no_mutation: &NoMutation) -> Result<(), String> {
    let summary = live_style_log_summary(no_mutation)?;
    ensure(
        summary["lineCount"] == 3,
        format!("unexpected live-style log summary: {summary}"),
    )?;
    ensure(
        summary["selectedDecision"] == "wait_for_rch",
        format!("live-style log selected wrong decision: {summary}"),
    )?;
    ensure(
        summary["reasonCodes"] == json!(["rch_telemetry_gap", "active_build_slot_exhausted"]),
        format!("live-style log selected wrong reasons: {summary}"),
    )?;
    Ok(())
}

fn live_style_log_summary(no_mutation: &NoMutation) -> Result<Value, String> {
    let content = fs::read_to_string(
        repo_root()
            .join("tests/fixtures/resource_admission/queue_pressure/live_style_pressure_log.jsonl"),
    )
    .map_err(|error| format!("read live-style log: {error}"))?;
    let mut line_count = 0usize;
    let mut selected_decision = None;
    let mut reason_codes = None;

    for line in content.lines() {
        line_count += 1;
        let value: Value = serde_json::from_str(line)
            .map_err(|error| format!("parse live-style pressure log line {line_count}: {error}"))?;
        ensure(
            value["schema"] == "ee.resource_admission.queue_pressure_event.v1",
            format!("line {line_count} has wrong schema: {value}"),
        )?;
        for field in [
            "rawMailBodies",
            "rawCommandArgv",
            "hostPrivatePaths",
            "fullDirtyPaths",
        ] {
            ensure(
                value["redaction"][field] == false,
                format!("line {line_count} redaction flag {field} was not false: {value}"),
            )?;
        }
        for snippet in &no_mutation.redaction_forbidden_substrings {
            ensure(
                !line.contains(snippet),
                format!("live-style log contains forbidden redaction snippet {snippet:?}"),
            )?;
        }
        if value["event"] == "advisory_decision" {
            selected_decision = value["selectedDecision"].as_str().map(str::to_owned);
        }
        if value["event"] == "normalized_queue_pressure" {
            reason_codes = Some(value["reasonCodes"].clone());
        }
    }

    Ok(json!({
        "lineCount": line_count,
        "selectedDecision": selected_decision
            .ok_or_else(|| "live-style log missing advisory decision".to_owned())?,
        "reasonCodes": reason_codes
            .ok_or_else(|| "live-style log missing normalized reasons".to_owned())?,
    }))
}

fn source_slice<'a>(source: &'a str, start: &str, end: &str) -> Result<&'a str, String> {
    let start_index = source
        .find(start)
        .ok_or_else(|| format!("missing source slice start {start:?}"))?;
    let end_index = source[start_index..]
        .find(end)
        .map(|index| start_index + index)
        .ok_or_else(|| format!("missing source slice end {end:?}"))?;
    Ok(&source[start_index..end_index])
}

fn source_kind_from_str(value: &str) -> Result<ResourceQueuePressureSourceKind, String> {
    match value {
        "rch_status" => Ok(ResourceQueuePressureSourceKind::RchStatus),
        "rch_selector_admission_probe" => {
            Ok(ResourceQueuePressureSourceKind::RchSelectorAdmissionProbe)
        }
        "build_slot_lease" => Ok(ResourceQueuePressureSourceKind::BuildSlotLease),
        "beads_in_progress_summary" => Ok(ResourceQueuePressureSourceKind::BeadsInProgressSummary),
        "agent_mail_health" => Ok(ResourceQueuePressureSourceKind::AgentMailHealth),
        "agent_mail_recovery_probe" => Ok(ResourceQueuePressureSourceKind::AgentMailRecoveryProbe),
        "git_dirty_summary" => Ok(ResourceQueuePressureSourceKind::GitDirtySummary),
        "local_cargo_tripwire" => Ok(ResourceQueuePressureSourceKind::LocalCargoTripwire),
        "output_budget_governor" => Ok(ResourceQueuePressureSourceKind::OutputBudgetGovernor),
        "host_calibration_posture" => Ok(ResourceQueuePressureSourceKind::HostCalibrationPosture),
        "source_authority_snapshot" => Ok(ResourceQueuePressureSourceKind::SourceAuthoritySnapshot),
        "manual_fixture" => Ok(ResourceQueuePressureSourceKind::ManualFixture),
        _ => Err(format!("unknown source kind {value}")),
    }
}

fn source_state_from_str(value: &str) -> Result<ResourceQueuePressureSourceState, String> {
    match value {
        "fresh" => Ok(ResourceQueuePressureSourceState::Fresh),
        "partial" => Ok(ResourceQueuePressureSourceState::Partial),
        "degraded" => Ok(ResourceQueuePressureSourceState::Degraded),
        "unavailable" => Ok(ResourceQueuePressureSourceState::Unavailable),
        "corrupt" => Ok(ResourceQueuePressureSourceState::Corrupt),
        "stale" => Ok(ResourceQueuePressureSourceState::Stale),
        "contradictory" => Ok(ResourceQueuePressureSourceState::Contradictory),
        _ => Err(format!("unknown source state {value}")),
    }
}

fn reason_code_from_str(value: &str) -> Result<ResourceQueuePressureReasonCode, String> {
    match value {
        "rch_lane_busy" => Ok(ResourceQueuePressureReasonCode::RchLaneBusy),
        "rch_telemetry_gap" => Ok(ResourceQueuePressureReasonCode::RchTelemetryGap),
        "active_build_slot_exhausted" => {
            Ok(ResourceQueuePressureReasonCode::ActiveBuildSlotExhausted)
        }
        "stale_in_progress_bead" => Ok(ResourceQueuePressureReasonCode::StaleInProgressBead),
        "agent_mail_unavailable" => Ok(ResourceQueuePressureReasonCode::AgentMailUnavailable),
        "agent_mail_recovery_corrupt" => {
            Ok(ResourceQueuePressureReasonCode::AgentMailRecoveryCorrupt)
        }
        "dirty_checkout_saturated" => Ok(ResourceQueuePressureReasonCode::DirtyCheckoutSaturated),
        "local_cargo_refused" => Ok(ResourceQueuePressureReasonCode::LocalCargoRefused),
        "output_budget_pressure" => Ok(ResourceQueuePressureReasonCode::OutputBudgetPressure),
        "host_calibration_missing" => Ok(ResourceQueuePressureReasonCode::HostCalibrationMissing),
        "contradictory_source_state" => {
            Ok(ResourceQueuePressureReasonCode::ContradictorySourceState)
        }
        _ => Err(format!("unknown reason code {value}")),
    }
}

fn cost_class_from_str(value: &str) -> Result<ResourceCostClass, String> {
    match value {
        "tiny" => Ok(ResourceCostClass::Tiny),
        "small" => Ok(ResourceCostClass::Small),
        "standard" => Ok(ResourceCostClass::Standard),
        "swarm_heavy" => Ok(ResourceCostClass::SwarmHeavy),
        "unknown" => Ok(ResourceCostClass::Unknown),
        _ => Err(format!("unknown cost class {value}")),
    }
}

fn source_schema_literal(value: &str) -> Result<&'static str, String> {
    match value {
        "ee.rch.status.v1" => Ok("ee.rch.status.v1"),
        "ee.rch.selector_admission_probe.v1" => Ok("ee.rch.selector_admission_probe.v1"),
        "ee.rch.build_slot_lease.v1" => Ok("ee.rch.build_slot_lease.v1"),
        "ee.agent_mail.health.v1" => Ok("ee.agent_mail.health.v1"),
        "ee.agent_mail.recovery.v1" => Ok("ee.agent_mail.recovery.v1"),
        "ee.br.status.v1" => Ok("ee.br.status.v1"),
        "ee.br.doctor.v1" => Ok("ee.br.doctor.v1"),
        "ee.git.status_summary.v1" => Ok("ee.git.status_summary.v1"),
        "ee.local_cargo_tripwire.v1" => Ok("ee.local_cargo_tripwire.v1"),
        "ee.output_budget.v1" => Ok("ee.output_budget.v1"),
        "ee.host_calibration.v1" => Ok("ee.host_calibration.v1"),
        "ee.resource_admission.fixture.v1" => Ok("ee.resource_admission.fixture.v1"),
        _ => Err(format!("unknown source schema {value}")),
    }
}

fn read_json<T: DeserializeOwned>(path: &[&str]) -> Result<T, String> {
    let path = join_repo_path(path);
    let content =
        fs::read_to_string(&path).map_err(|error| format!("read {}: {error}", path.display()))?;
    serde_json::from_str(&content).map_err(|error| format!("parse {}: {error}", path.display()))
}

fn pretty_json(value: &Value) -> Result<String, String> {
    serde_json::to_string_pretty(value).map_err(|error| format!("render JSON: {error}"))
}

fn join_repo_path(parts: &[&str]) -> PathBuf {
    parts.iter().fold(repo_root(), |acc, part| acc.join(part))
}

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn ensure(condition: bool, message: impl Into<String>) -> Result<(), String> {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}
