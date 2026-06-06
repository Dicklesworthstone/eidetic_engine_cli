//! Fixture-driven integration tests for the verification-evidence
//! normalizer (`bd-1nxz4.5`).
//!
//! The fixture corpus lives in `tests/fixtures/verification_evidence/`
//! and mirrors the real-world shapes the swarm has been producing:
//!
//! - `rch_remote_pass.json` — successful RCH proof (style matches
//!   `tests/fixtures/rch_verify_control_plane/remote_cargo_check_pass.json`).
//! - `rch_path_dep_version_skew.json` — the active cross-swarm blocker
//!   the swarm has been chasing on 2026-05-19 (command_hash
//!   `eae0cb5e0af81aca…`).
//! - `rch_topology_refusal.json` — the historical topology-refusal
//!   shape from
//!   `tests/fixtures/rch_verify_control_plane/topology_refusal.json`.
//! - `rch_remote_compile_error.json` — a genuine code failure, used to
//!   prove environment blockers and code failures are separated.
//! - `verify_script_rch_e327_event.json` — `ee.test_event.v1`
//!   verify-script tail with the active RCH-E327 environment blocker.
//! - `github_actions_check_failure.json` — canonical check-run summary
//!   that failed in code.
//! - `static_check_pass.json` / `static_check_failed_shell_text.json` —
//!   local static-only proof records.
//! - `malformed_wrong_schema.json` — wrong-schema input that the parser
//!   must reject loudly rather than silently classify.

use std::path::PathBuf;

use ee::obs::verification_evidence::{
    EvidenceSource, EvidenceStatus, ParseError, VERIFICATION_EVIDENCE_SCHEMA_V1,
    VerificationEvidence, compact_summary, parse_github_actions_job, parse_rch_verify,
    parse_static_check, parse_verify_script_event,
};
use serde_json::{Map, Value};

type TestResult = Result<(), String>;

fn fixture(name: &str) -> Value {
    let mut path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    path.push("tests");
    path.push("fixtures");
    path.push("verification_evidence");
    path.push(name);
    let bytes =
        std::fs::read(&path).unwrap_or_else(|err| panic!("read fixture {}: {err}", path.display()));
    serde_json::from_slice(&bytes)
        .unwrap_or_else(|err| panic!("parse fixture {}: {err}", path.display()))
}

fn parse_rch_fixture(name: &str) -> Result<VerificationEvidence, String> {
    parse_rch_verify(&fixture(name)).map_err(|error| format!("parse {name}: {error}"))
}

fn parse_verify_script_fixture(name: &str) -> Result<VerificationEvidence, String> {
    parse_verify_script_event(&fixture(name)).map_err(|error| format!("parse {name}: {error}"))
}

fn parse_github_actions_fixture(name: &str) -> Result<VerificationEvidence, String> {
    parse_github_actions_job(&fixture(name)).map_err(|error| format!("parse {name}: {error}"))
}

fn parse_static_fixture(name: &str) -> Result<VerificationEvidence, String> {
    parse_static_check(&fixture(name)).map_err(|error| format!("parse {name}: {error}"))
}

fn expect_rch_fixture_parse_error(name: &str) -> Result<ParseError, String> {
    match parse_rch_verify(&fixture(name)) {
        Ok(evidence) => Err(format!("expected {name} to fail, got {evidence:?}")),
        Err(error) => Ok(error),
    }
}

fn object_value<'a>(value: &'a Value, context: &str) -> Result<&'a Map<String, Value>, String> {
    value
        .as_object()
        .ok_or_else(|| format!("{context} must serialize to a JSON object: {value}"))
}

#[test]
fn rch_remote_pass_fixture_normalizes_to_passed_status() -> TestResult {
    let evidence = parse_rch_fixture("rch_remote_pass.json")?;
    assert_eq!(evidence.schema, VERIFICATION_EVIDENCE_SCHEMA_V1);
    assert_eq!(evidence.source, EvidenceSource::RchVerify);
    assert_eq!(evidence.status, EvidenceStatus::Passed);
    assert_eq!(evidence.command_kind.as_deref(), Some("cargo_check"));
    assert!(evidence.environment_blocker_codes.is_empty());
    assert!(evidence.error_codes.is_empty());
    Ok(())
}

#[test]
fn rch_path_dep_version_skew_fixture_classifies_as_environment_blocked() -> TestResult {
    let evidence = parse_rch_fixture("rch_path_dep_version_skew.json")?;
    assert_eq!(evidence.status, EvidenceStatus::EnvironmentBlocked);
    assert!(
        evidence
            .environment_blocker_codes
            .iter()
            .any(|code| code == "rch_verify_cargo_path_dependency_version_blocked"),
        "expected the path-dependency version-skew code among env blockers"
    );
    assert_eq!(
        evidence.command_hash.as_deref(),
        Some("eae0cb5e0af81aca484ac22464070a7f17dc1021c11099dbbfa45d7f0939d261")
    );
    assert_eq!(evidence.worker_id.as_deref(), Some("vmi1149989"));
    // The same code should not appear twice after worker-state union:
    let occurrences = evidence
        .degraded_codes
        .iter()
        .filter(|c| c.as_str() == "rch_verify_cargo_path_dependency_version_blocked")
        .count();
    assert_eq!(occurrences, 1);
    Ok(())
}

#[test]
fn rch_topology_refusal_fixture_preserves_error_codes_and_classifies_env_blocked() -> TestResult {
    let evidence = parse_rch_fixture("rch_topology_refusal.json")?;
    assert_eq!(evidence.status, EvidenceStatus::EnvironmentBlocked);
    assert_eq!(evidence.error_codes, vec!["RCH-E327".to_owned()]);
    assert!(
        evidence
            .environment_blocker_codes
            .iter()
            .any(|code| code == "rch_verify_topology_blocked")
    );
    assert!(
        evidence
            .environment_blocker_codes
            .iter()
            .any(|code| code == "rch_verify_local_fallback_refused")
    );
    assert!(
        evidence
            .environment_blocker_codes
            .iter()
            .any(|code| code == "rch_verify_remote_marker_missing")
    );
    Ok(())
}

#[test]
fn rch_remote_compile_error_fixture_classifies_as_code_failure() -> TestResult {
    let evidence = parse_rch_fixture("rch_remote_compile_error.json")?;
    assert_eq!(evidence.status, EvidenceStatus::FailedInCode);
    assert!(evidence.environment_blocker_codes.is_empty());
    assert_eq!(
        evidence.first_error.file.as_deref(),
        Some("/data/projects/eidetic_engine_cli/src/db/mod.rs")
    );
    assert_eq!(evidence.first_error.line, Some(17092));
    assert_eq!(
        evidence.first_error.message.as_deref(),
        Some("expected struct, got enum")
    );
    Ok(())
}

#[test]
fn verify_script_event_fixture_classifies_rch_e327_as_environment_blocked() -> TestResult {
    let evidence = parse_verify_script_fixture("verify_script_rch_e327_event.json")?;
    assert_eq!(evidence.source, EvidenceSource::VerifyScript);
    assert_eq!(evidence.status, EvidenceStatus::EnvironmentBlocked);
    assert_eq!(evidence.bead_id.as_deref(), Some("bd-1nxz4.5"));
    assert_eq!(evidence.command_kind.as_deref(), Some("command_end"));
    assert_eq!(
        evidence.command_hash.as_deref(),
        Some("blake3:rch-e327-focused-test")
    );
    assert_eq!(evidence.error_codes, vec!["RCH-E327".to_owned()]);
    assert!(
        evidence
            .environment_blocker_codes
            .iter()
            .any(|code| code == "rch_verify_topology_blocked")
    );
    assert!(
        evidence
            .environment_blocker_codes
            .iter()
            .any(|code| code == "rch_verify_local_fallback_refused")
    );
    Ok(())
}

#[test]
fn github_actions_check_failure_fixture_classifies_as_code_failure() -> TestResult {
    let evidence = parse_github_actions_fixture("github_actions_check_failure.json")?;
    assert_eq!(evidence.source, EvidenceSource::GitHubActionsJob);
    assert_eq!(evidence.status, EvidenceStatus::FailedInCode);
    assert_eq!(
        evidence.command.as_deref(),
        Some("ci / verification_evidence_parsers")
    );
    assert_eq!(
        evidence.command_kind.as_deref(),
        Some("github_actions_check_run")
    );
    assert_eq!(
        evidence.git_head.as_deref(),
        Some("308e122e5e44b7b1f8c9d7101b5f5edb5ad1e000")
    );
    assert_eq!(
        evidence.first_error.message.as_deref(),
        Some("assertion failed: environment blockers were empty")
    );
    Ok(())
}

#[test]
fn static_check_pass_fixture_classifies_as_passed() -> TestResult {
    let evidence = parse_static_fixture("static_check_pass.json")?;
    assert_eq!(evidence.source, EvidenceSource::StaticCheck);
    assert_eq!(evidence.status, EvidenceStatus::Passed);
    assert_eq!(evidence.command_kind.as_deref(), Some("rustfmt_check"));
    assert_eq!(
        evidence.command_hash.as_deref(),
        Some("blake3:rustfmt-static")
    );
    assert!(evidence.environment_blocker_codes.is_empty());
    Ok(())
}

#[test]
fn static_check_failed_fixture_classifies_as_code_failure() -> TestResult {
    let evidence = parse_static_fixture("static_check_failed_shell_text.json")?;
    assert_eq!(evidence.source, EvidenceSource::StaticCheck);
    assert_eq!(evidence.status, EvidenceStatus::FailedInCode);
    assert_eq!(evidence.exit_code, Some(1));
    assert_eq!(
        evidence.first_error.file.as_deref(),
        Some("src/obs/verification_evidence.rs")
    );
    Ok(())
}

#[test]
fn compact_summary_strips_shell_command_substitution_text() -> TestResult {
    let evidence = parse_static_fixture("static_check_failed_shell_text.json")?;
    let summary = compact_summary(&evidence);
    assert!(
        !summary.contains("$("),
        "summary should strip command substitution openers: {summary}"
    );
    assert!(
        !summary.contains("`touch"),
        "summary should not preserve executable payload inside inline code: {summary}"
    );
    assert!(
        summary.contains("git diff --check __touch /tmp/ee-owned_"),
        "summary should keep a readable sanitized command: {summary}"
    );
    Ok(())
}

#[test]
fn malformed_wrong_schema_fixture_returns_unexpected_schema_error() -> TestResult {
    let error = expect_rch_fixture_parse_error("malformed_wrong_schema.json")?;
    match error {
        ParseError::UnexpectedSchema { found, expected } => {
            assert_eq!(found, "ee.test_event.v1");
            assert_eq!(expected, "ee.rch.verify.v1");
        }
        other => panic!("unexpected error variant: {other:?}"),
    }
    Ok(())
}

#[test]
fn compact_summary_for_env_blocked_evidence_is_beads_ready_markdown() -> TestResult {
    let evidence = parse_rch_fixture("rch_path_dep_version_skew.json")?;
    let summary = compact_summary(&evidence);
    // First line names the command and the normalized status:
    let first = summary.lines().next().unwrap_or("");
    assert!(
        first.starts_with("Verification evidence:"),
        "first line should be the summary header: {first}"
    );
    assert!(
        first.contains("environment_blocked"),
        "first line should announce the normalized status: {first}"
    );
    // Bullet rows we rely on downstream:
    assert!(summary.contains("- source: `rch_verify`"));
    assert!(summary.contains("- raw_status: `rch_environment_failure`"));
    assert!(summary.contains("- command_hash: `eae0cb5e0af81aca"));
    assert!(summary.contains("- environment_blocker_codes:"));
    assert!(summary.contains("rch_verify_cargo_path_dependency_version_blocked"));
    Ok(())
}

#[test]
fn schema_tag_constant_matches_public_schema_file() {
    // Belt-and-suspenders: the const exported by the library must match
    // the `schema` const declared in
    // `docs/schemas/ee.verification_evidence.v1.json`.
    assert_eq!(
        VERIFICATION_EVIDENCE_SCHEMA_V1,
        "ee.verification_evidence.v1"
    );
}

#[test]
fn evidence_envelopes_serialize_to_json_objects_with_required_fields() -> TestResult {
    let evidence = parse_rch_fixture("rch_path_dep_version_skew.json")?;
    let json =
        serde_json::to_value(&evidence).map_err(|error| format!("serialize evidence: {error}"))?;
    let object = object_value(&json, "verification evidence")?;
    for required in [
        "schema",
        "source",
        "status",
        "degradedCodes",
        "errorCodes",
        "environmentBlockerCodes",
    ] {
        assert!(
            object.contains_key(required),
            "envelope should expose required schema field `{required}` in {json}"
        );
    }
    Ok(())
}
