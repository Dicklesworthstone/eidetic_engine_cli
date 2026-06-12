use std::{collections::BTreeSet, fs, path::PathBuf};

use ee::core::unsafe_claim_planner::{
    UnsafeClaimAlternateCandidateFacts, UnsafeClaimAlternatePlannerInput,
    UnsafeClaimCandidateFacts, classify_unsafe_claim_evidence, recommend_unsafe_claim_alternates,
    suggest_unsafe_claim_decomposition,
};
use serde::Deserialize;
use serde_json::{Value, json};

type TestResult = Result<(), String>;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct FixtureCorpus {
    schema: String,
    required_coverage: Vec<String>,
    cases: Vec<FixtureCase>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct FixtureCase {
    id: String,
    coverage: Vec<String>,
    unsafe_reasons: Vec<String>,
    degraded_codes: Vec<String>,
    decomposition: Option<DecompositionFixture>,
    alternate_planner: AlternatePlannerFixture,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DecompositionFixture {
    candidate_id: String,
    issue_type: String,
    title: String,
    priority: i64,
    path_families: Vec<String>,
    raw_path_that_must_not_appear: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AlternatePlannerFixture {
    candidate_id: String,
    unsafe_path_families: Vec<String>,
    source_authority_degraded: bool,
    candidates: Vec<AlternateCandidateFixture>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AlternateCandidateFixture {
    id: String,
    title: String,
    issue_type: String,
    status: String,
    priority: i64,
    assignee: Option<String>,
    labels: Vec<String>,
    paths: Vec<String>,
    gate_safe_to_claim: Option<bool>,
    gate_claim_command_action_present: bool,
    reason_group_refs: Vec<String>,
    candidate_specific_deltas: Vec<String>,
}

#[test]
fn unsafe_claim_planner_conformance_matches_fixture_golden() -> TestResult {
    let corpus: FixtureCorpus = read_json(&[
        "tests",
        "fixtures",
        "unsafe_claim_planner",
        "conformance_cases.json",
    ])?;
    let actual = build_conformance_report(&corpus)?;
    assert_report_invariants(&actual)?;

    let expected: Value = read_json(&[
        "tests",
        "fixtures",
        "golden",
        "swarm",
        "unsafe_claim_planner_conformance.json.golden",
    ])?;

    if actual != expected {
        return Err(format!(
            "unsafe claim planner conformance golden drifted\nexpected:\n{}\nactual:\n{}",
            pretty_json(&expected)?,
            pretty_json(&actual)?
        ));
    }

    Ok(())
}

#[test]
fn unsafe_claim_planner_source_stays_read_only() -> TestResult {
    let source = fs::read_to_string(repo_root().join("src/core/unsafe_claim_planner.rs"))
        .map_err(|err| format!("read planner source: {err}"))?;
    let forbidden_snippets = [
        "std::process::Command",
        "Command::new",
        "file_reservation_paths",
        "send_message(",
        "acknowledge_message",
        "br update",
        "br close",
        "git add",
        "git commit",
        "cargo test",
        "cargo check",
        "rch_verify.sh",
        "std::fs::write",
        "remove_file",
        "remove_dir",
        "create_dir",
    ];

    for snippet in forbidden_snippets {
        if source.contains(snippet) {
            return Err(format!(
                "unsafe claim planner must stay pure/read-only; found mutating snippet {snippet:?}"
            ));
        }
    }

    Ok(())
}

fn build_conformance_report(corpus: &FixtureCorpus) -> TestResult {
    let mut covered = BTreeSet::new();
    let mut case_reports = Vec::new();

    for case in &corpus.cases {
        covered.extend(case.coverage.iter().cloned());
        let classification =
            classify_unsafe_claim_evidence(&case.unsafe_reasons, &case.degraded_codes);

        let reason_categories = classification
            .reason_groups
            .iter()
            .map(|group| group.category.as_str())
            .collect::<Vec<_>>();
        let raw_reason_index_count = classification
            .reason_groups
            .iter()
            .map(|group| group.raw_reason_indexes.len())
            .sum::<usize>();
        let unknown_reasons = classification
            .reason_groups
            .iter()
            .filter(|group| group.category.as_str() == "unknown")
            .flat_map(|group| group.reason_codes.clone())
            .collect::<Vec<_>>();
        let planner_action_kinds = classification
            .planner_actions
            .iter()
            .map(|action| action.kind.as_str())
            .collect::<Vec<_>>();
        let planner_actions_mutate_state = classification
            .planner_actions
            .iter()
            .any(|action| action.mutates_state || !action.advisory_only);

        let decomposition = case
            .decomposition
            .as_ref()
            .map(|fixture| {
                let facts = UnsafeClaimCandidateFacts {
                    candidate_id: fixture.candidate_id.clone(),
                    title: fixture.title.clone(),
                    description: String::new(),
                    issue_type: fixture.issue_type.clone(),
                    priority: Some(fixture.priority),
                    labels: Vec::new(),
                    path_families: fixture.path_families.clone(),
                };
                let plan = suggest_unsafe_claim_decomposition(&facts, &classification);
                let labels = plan
                    .suggested_beads
                    .iter()
                    .map(|bead| bead.labels.clone())
                    .collect::<Vec<_>>();

                json!({
                    "commentTemplateContainsRawPath": plan
                        .comment_template
                        .contains(&fixture.raw_path_that_must_not_appear),
                    "decompose": plan.decompose,
                    "labels": labels,
                    "suggestedCount": plan.suggested_beads.len(),
                })
            })
            .unwrap_or(Value::Null);

        let alternate = {
            let input = UnsafeClaimAlternatePlannerInput {
                requested_candidate_id: Some(case.alternate_planner.candidate_id.clone()),
                source_authority_degraded: case.alternate_planner.source_authority_degraded,
                tracker_health: "degraded".to_owned(),
                agent_mail_status: "skipped".to_owned(),
                source_freshness: "fixture".to_owned(),
                unsafe_path_families: case.alternate_planner.unsafe_path_families.clone(),
                candidates: case
                    .alternate_planner
                    .candidates
                    .iter()
                    .map(|candidate| UnsafeClaimAlternateCandidateFacts {
                        candidate_id: candidate.id.clone(),
                        title: candidate.title.clone(),
                        issue_type: candidate.issue_type.clone(),
                        status: candidate.status.clone(),
                        assignee: candidate.assignee.clone(),
                        priority: Some(candidate.priority),
                        score: 0,
                        labels: candidate.labels.clone(),
                        path_families: candidate.paths.clone(),
                        gate_verdict: candidate.gate_safe_to_claim.map(|safe_to_claim| {
                            if safe_to_claim {
                                "safe_to_claim".to_owned()
                            } else {
                                "unsafe_due_to_conflict".to_owned()
                            }
                        }),
                        gate_safe_to_claim: candidate.gate_safe_to_claim,
                        gate_claim_command_action_present: candidate
                            .gate_claim_command_action_present,
                        evidence_freshness: if candidate.gate_safe_to_claim.is_some() {
                            "fresh".to_owned()
                        } else {
                            "unknown".to_owned()
                        },
                        reason_group_refs: candidate.reason_group_refs.clone(),
                        candidate_specific_deltas: candidate.candidate_specific_deltas.clone(),
                    })
                    .collect(),
            };
            let plan = recommend_unsafe_claim_alternates(&input);
            let candidate_reports = plan
                .candidates
                .iter()
                .map(|candidate| {
                    json!({
                        "actionsMutateState": candidate
                            .next_command_actions
                            .iter()
                            .any(|action| action.mutates_state),
                        "deltaCount": candidate.candidate_specific_deltas.len(),
                        "id": candidate.candidate_id.clone(),
                        "mayEmitClaimCommand": candidate.may_emit_claim_command,
                        "needsFreshClaimGate": candidate.needs_fresh_claim_gate,
                        "state": candidate.candidate_state.as_str(),
                        "workClass": candidate.work_class.as_str(),
                    })
                })
                .collect::<Vec<_>>();
            let top_command_ids = plan
                .next_command_actions
                .iter()
                .map(|action| action.command_id.clone())
                .collect::<Vec<_>>();
            let top_copy_safety = plan
                .next_command_actions
                .iter()
                .map(|action| action.copy_safety)
                .collect::<Vec<_>>();

            json!({
                "candidates": candidate_reports,
                "recommendedAction": plan.recommended_action.as_str(),
                "topCommandIds": top_command_ids,
                "topCopySafety": top_copy_safety,
            })
        };

        let case_report = json!({
            "alternate": alternate.clone(),
            "coverage": case.coverage.clone(),
            "decomposition": decomposition.clone(),
            "forbiddenMarkersPresent": contains_forbidden_marker(&json!({
                "alternate": alternate.clone(),
                "decomposition": decomposition.clone(),
                "id": case.id.clone(),
                "unknownReasons": unknown_reasons.clone(),
            })),
            "id": case.id.clone(),
            "plannerActionKinds": planner_action_kinds,
            "plannerActionsMutateState": planner_actions_mutate_state,
            "rawReasonIndexCount": raw_reason_index_count,
            "reasonCategories": reason_categories,
            "unknownReasons": unknown_reasons,
        });
        case_reports.push(case_report);
    }

    let coverage_satisfied = corpus
        .required_coverage
        .iter()
        .all(|required| covered.contains(required));

    Ok(json!({
        "cases": case_reports,
        "coverageSatisfied": coverage_satisfied,
        "fixtureSchema": corpus.schema,
        "nonMutationPolicy": {
            "claimsBeads": false,
            "deletesFiles": false,
            "reservesFiles": false,
            "runsCargo": false,
            "runsRch": false,
            "sendsAgentMail": false,
            "stagesGit": false,
        },
        "requiredCoverage": corpus.required_coverage,
        "schema": "ee.unsafe_claim_planner.conformance.v1",
    }))
}

fn assert_report_invariants(report: &Value) -> TestResult {
    if report["schema"] != "ee.unsafe_claim_planner.conformance.v1" {
        return Err("unexpected conformance report schema".to_owned());
    }
    if report["coverageSatisfied"] != true {
        return Err("fixture coverage matrix is incomplete".to_owned());
    }
    if contains_forbidden_marker(report) {
        return Err("conformance report leaked a forbidden raw/private marker".to_owned());
    }
    let policy = &report["nonMutationPolicy"];
    for field in [
        "claimsBeads",
        "deletesFiles",
        "reservesFiles",
        "runsCargo",
        "runsRch",
        "sendsAgentMail",
        "stagesGit",
    ] {
        if policy[field] != false {
            return Err(format!("non-mutation policy field {field} must be false"));
        }
    }
    for case in report["cases"]
        .as_array()
        .ok_or_else(|| "cases must be an array".to_owned())?
    {
        if case["plannerActionsMutateState"] != false {
            return Err(format!(
                "planner action mutated state in case {}",
                case["id"]
            ));
        }
        if case["forbiddenMarkersPresent"] != false {
            return Err(format!("forbidden marker present in case {}", case["id"]));
        }
        let candidates = case["alternate"]["candidates"]
            .as_array()
            .ok_or_else(|| format!("alternate candidates missing for case {}", case["id"]))?;
        for candidate in candidates {
            if candidate["mayEmitClaimCommand"] != false {
                return Err(format!(
                    "candidate {} unexpectedly emitted a claim command",
                    candidate["id"]
                ));
            }
            if candidate["actionsMutateState"] != false {
                return Err(format!(
                    "candidate {} unexpectedly emitted a mutating action",
                    candidate["id"]
                ));
            }
        }
    }

    Ok(())
}

fn contains_forbidden_marker(value: &Value) -> bool {
    let rendered = serde_json::to_string(value).unwrap_or_default();
    [
        "/Users/",
        "/home/",
        "From:",
        "Subject:",
        "Message-ID:",
        "stdout:",
        "stderr:",
        "BEGIN PRIVATE KEY",
        "BEGIN OPENSSH PRIVATE KEY",
        "ghp_",
        "Bearer ",
    ]
    .iter()
    .any(|marker| rendered.contains(marker))
}

fn read_json<T>(path_parts: &[&str]) -> Result<T, String>
where
    T: serde::de::DeserializeOwned,
{
    let path = path_parts.iter().fold(repo_root(), |mut path, part| {
        path.push(part);
        path
    });
    let raw = fs::read_to_string(&path).map_err(|err| format!("read {}: {err}", path.display()))?;
    serde_json::from_str(&raw).map_err(|err| format!("parse {}: {err}", path.display()))
}

fn pretty_json(value: &Value) -> Result<String, String> {
    serde_json::to_string_pretty(value).map_err(|err| format!("render json: {err}"))
}

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}
