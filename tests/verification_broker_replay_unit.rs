#![forbid(unsafe_code)]

use std::fs;
use std::path::PathBuf;

use ee::models::{
    VerificationBrokerStatus, VerificationBrokerViewRequest, VerificationRunRecord,
    verification_broker_view,
};
use serde::Deserialize;
use serde_json::{Value, json};

type TestResult = Result<(), String>;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct BrokerReplayFixture {
    schema: String,
    fixture_id: String,
    fixture_hash: String,
    trace_id: String,
    fixed_clock: String,
    records: Vec<VerificationRunRecord>,
    steps: Vec<BrokerReplayStep>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct BrokerReplayStep {
    step_id: String,
    agent_name: String,
    request: BrokerReplayRequest,
    expected_status: VerificationBrokerStatus,
    expected_suggested_action: String,
    expected_matched_run_id: Option<String>,
    expected_first_failure_diagnosis: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct BrokerReplayRequest {
    bead_id: Option<String>,
    source_hash: Option<String>,
    command_hash: String,
    command_class: String,
    normalized_argv_hash: String,
    execution_substrate: String,
    env_fingerprint_class: Option<String>,
    target_profile: Option<String>,
}

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn fixture_path() -> PathBuf {
    repo_root()
        .join("tests")
        .join("fixtures")
        .join("verification")
        .join("broker_replay_trace.json")
}

fn golden_path() -> PathBuf {
    repo_root()
        .join("tests")
        .join("fixtures")
        .join("golden")
        .join("verification")
        .join("broker_replay_steps.json.golden")
}

#[test]
fn broker_replay_fixture_prevents_duplicate_build_decisions_deterministically() -> TestResult {
    let fixture_text =
        fs::read_to_string(fixture_path()).map_err(|error| format!("read fixture: {error}"))?;
    let fixture: BrokerReplayFixture =
        serde_json::from_str(&fixture_text).map_err(|error| format!("parse fixture: {error}"))?;
    assert_eq!(fixture.schema, "ee.verification.broker_replay_fixture.v1");

    let first = replay_fixture(&fixture)?;
    let second = replay_fixture(&fixture)?;
    let third = replay_fixture(&fixture)?;
    assert_eq!(first, second, "second replay must be byte-identical");
    assert_eq!(first, third, "third replay must be byte-identical");

    let expected =
        fs::read_to_string(golden_path()).map_err(|error| format!("read golden: {error}"))?;
    let expected_json: Value =
        serde_json::from_str(&expected).map_err(|error| format!("parse golden: {error}"))?;
    let actual_json: Value =
        serde_json::from_str(&first).map_err(|error| format!("parse replay output: {error}"))?;
    assert_eq!(actual_json, expected_json);

    assert!(!first.contains("/Volumes/USBNVME16TB"));
    assert!(!first.contains("/tmp/"));
    assert!(!first.contains("error:"));
    assert!(!first.contains("stderr bytes"));
    Ok(())
}

fn replay_fixture(fixture: &BrokerReplayFixture) -> Result<String, String> {
    let mut output = Vec::new();

    for step in &fixture.steps {
        let request = broker_request(&step.request);
        let view = verification_broker_view(request, &fixture.records);
        if view.status != step.expected_status {
            return Err(format!(
                "{} status {:?} != {:?}",
                step.step_id, view.status, step.expected_status
            ));
        }
        if view.suggested_action != step.expected_suggested_action {
            return Err(format!(
                "{} action {} != {}",
                step.step_id, view.suggested_action, step.expected_suggested_action
            ));
        }
        if view.matched_run_id != step.expected_matched_run_id {
            return Err(format!(
                "{} matched {:?} != {:?}",
                step.step_id, view.matched_run_id, step.expected_matched_run_id
            ));
        }
        if view
            .first_failure_summary_ref
            .as_ref()
            .is_some_and(|summary| summary.raw_output_included)
        {
            return Err(format!("{} included raw failure output", step.step_id));
        }

        output.push(json!({
            "schema": "ee.test_event.v1",
            "kind": "verification_broker_replay_step",
            "traceId": fixture.trace_id.as_str(),
            "fixtureId": fixture.fixture_id.as_str(),
            "fixtureHash": fixture.fixture_hash.as_str(),
            "fixedClock": fixture.fixed_clock.as_str(),
            "stepId": step.step_id.as_str(),
            "agentName": step.agent_name.as_str(),
            "commandFingerprint": {
                "commandClass": step.request.command_class.as_str(),
                "commandHash": step.request.command_hash.as_str(),
                "normalizedArgvHash": step.request.normalized_argv_hash.as_str()
            },
            "evidenceArtifactHash": view
                .first_failure_summary_ref
                .as_ref()
                .and_then(|summary| summary.artifact_manifest_hash.clone()),
            "reuseDecision": view.status.as_str(),
            "suggestedAction": view.suggested_action,
            "matchedRunId": view.matched_run_id,
            "degradedSources": [],
            "firstFailureDiagnosis": step.expected_first_failure_diagnosis.as_str(),
            "firstFailureSummaryRef": view.first_failure_summary_ref
        }));
    }

    serde_json::to_string_pretty(&output).map_err(|error| error.to_string())
}

fn broker_request(request: &BrokerReplayRequest) -> VerificationBrokerViewRequest<'_> {
    VerificationBrokerViewRequest {
        bead_id: request.bead_id.as_deref(),
        source_hash: request.source_hash.as_deref(),
        command_hash: request.command_hash.as_str(),
        command_class: request.command_class.as_str(),
        normalized_argv_hash: request.normalized_argv_hash.as_str(),
        execution_substrate: request.execution_substrate.as_str(),
        env_fingerprint_class: request.env_fingerprint_class.as_deref(),
        target_profile: request.target_profile.as_deref(),
    }
}
