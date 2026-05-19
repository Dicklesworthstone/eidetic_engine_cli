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
//! - `malformed_wrong_schema.json` — wrong-schema input that the parser
//!   must reject loudly rather than silently classify.

use std::path::PathBuf;

use ee::obs::verification_evidence::{
    EvidenceSource, EvidenceStatus, ParseError, VERIFICATION_EVIDENCE_SCHEMA_V1, compact_summary,
    parse_rch_verify,
};

fn fixture(name: &str) -> serde_json::Value {
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

#[test]
fn rch_remote_pass_fixture_normalizes_to_passed_status() {
    let evidence = parse_rch_verify(&fixture("rch_remote_pass.json")).expect("parses");
    assert_eq!(evidence.schema, VERIFICATION_EVIDENCE_SCHEMA_V1);
    assert_eq!(evidence.source, EvidenceSource::RchVerify);
    assert_eq!(evidence.status, EvidenceStatus::Passed);
    assert_eq!(evidence.command_kind.as_deref(), Some("cargo_check"));
    assert!(evidence.environment_blocker_codes.is_empty());
    assert!(evidence.error_codes.is_empty());
}

#[test]
fn rch_path_dep_version_skew_fixture_classifies_as_environment_blocked() {
    let evidence = parse_rch_verify(&fixture("rch_path_dep_version_skew.json")).expect("parses");
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
}

#[test]
fn rch_topology_refusal_fixture_preserves_error_codes_and_classifies_env_blocked() {
    let evidence = parse_rch_verify(&fixture("rch_topology_refusal.json")).expect("parses");
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
}

#[test]
fn rch_remote_compile_error_fixture_classifies_as_code_failure() {
    let evidence = parse_rch_verify(&fixture("rch_remote_compile_error.json")).expect("parses");
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
}

#[test]
fn malformed_wrong_schema_fixture_returns_unexpected_schema_error() {
    let error = parse_rch_verify(&fixture("malformed_wrong_schema.json")).unwrap_err();
    match error {
        ParseError::UnexpectedSchema { found, expected } => {
            assert_eq!(found, "ee.test_event.v1");
            assert_eq!(expected, "ee.rch.verify.v1");
        }
        other => panic!("unexpected error variant: {other:?}"),
    }
}

#[test]
fn compact_summary_for_env_blocked_evidence_is_beads_ready_markdown() {
    let evidence = parse_rch_verify(&fixture("rch_path_dep_version_skew.json")).expect("parses");
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
fn evidence_envelopes_serialize_to_json_objects_with_required_fields() {
    let evidence = parse_rch_verify(&fixture("rch_path_dep_version_skew.json")).expect("parses");
    let json = serde_json::to_value(&evidence).expect("serializes");
    let object = json.as_object().expect("object");
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
}
