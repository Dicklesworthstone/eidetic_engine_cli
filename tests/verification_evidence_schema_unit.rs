#![forbid(unsafe_code)]

use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;

use ee::models::{
    VERIFICATION_BROKER_VIEW_SCHEMA_V1, VERIFICATION_CLOSEOUT_CAPSULE_SCHEMA_V1,
    VERIFICATION_EVIDENCE_SCHEMA_V1, VERIFICATION_REUSE_ADVISORY_SCHEMA_V1,
    VERIFICATION_RUN_SCHEMA_V1, VerificationBrokerStatus, VerificationBrokerView,
    VerificationCloseoutCapsule, VerificationCloseoutCapsuleRequest, VerificationEvidenceRecord,
    VerificationReuseRequest, VerificationReuseStatus, VerificationRunImportError,
    VerificationStatus, sample_verification_broker_views, sample_verification_closeout_capsules,
    sample_verification_evidence_records, sample_verification_reuse_advisories,
    sample_verification_run_records, verification_closeout_capsule, verification_reuse_advisory,
    verification_run_records_from_j1_jsonl,
};

type TestResult = Result<(), String>;

fn golden_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("golden")
        .join("verification")
}

fn broker_view_schema_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("docs")
        .join("schemas")
        .join("swarm")
        .join("ee.verification.broker_view.v1.json")
}

fn sample_for(status: VerificationStatus) -> Result<VerificationEvidenceRecord, String> {
    sample_verification_evidence_records()
        .into_iter()
        .find(|record| record.status == status)
        .ok_or_else(|| format!("sample record missing status={}", status.as_str()))
}

#[test]
fn sample_records_cover_named_statuses_and_round_trip() -> TestResult {
    for status in [
        VerificationStatus::Passed,
        VerificationStatus::Failed,
        VerificationStatus::Blocked,
        VerificationStatus::Interrupted,
        VerificationStatus::FallbackDetected,
    ] {
        let record = sample_for(status)?;
        if record.schema != VERIFICATION_EVIDENCE_SCHEMA_V1 {
            return Err(format!(
                "status {} uses schema {}",
                status.as_str(),
                record.schema
            ));
        }
        if record.command_hash.strip_prefix("blake3:").is_none() {
            return Err(format!(
                "status {} missing blake3 command hash: {}",
                status.as_str(),
                record.command_hash
            ));
        }
        let encoded = serde_json::to_string(&record)
            .map_err(|error| format!("serialize {}: {error}", status.as_str()))?;
        let decoded: VerificationEvidenceRecord = serde_json::from_str(&encoded)
            .map_err(|error| format!("deserialize {}: {error}", status.as_str()))?;
        if decoded != record {
            return Err(format!(
                "round trip mismatch for status {}",
                status.as_str()
            ));
        }
    }
    Ok(())
}

#[test]
fn per_status_golden_fixtures_match_samples() -> TestResult {
    for (status, file_name) in [
        (VerificationStatus::Passed, "passed.json.golden"),
        (VerificationStatus::Failed, "failed.json.golden"),
        (VerificationStatus::Blocked, "blocked.json.golden"),
        (VerificationStatus::Interrupted, "interrupted.json.golden"),
        (
            VerificationStatus::FallbackDetected,
            "fallback_detected.json.golden",
        ),
    ] {
        let path = golden_dir().join(file_name);
        let fixture = fs::read_to_string(&path)
            .map_err(|error| format!("read {}: {error}", path.display()))?;
        let expected: VerificationEvidenceRecord = serde_json::from_str(&fixture)
            .map_err(|error| format!("parse {}: {error}", path.display()))?;
        let actual = sample_for(status)?;
        if expected != actual {
            return Err(format!(
                "{} does not match sample status {}",
                path.display(),
                status.as_str()
            ));
        }
    }
    Ok(())
}

#[test]
fn verification_run_golden_covers_rch_and_shell_static_shapes() -> TestResult {
    let path = golden_dir().join("run_records.json.golden");
    let fixture = fs::read_to_string(&path).map_err(|error| format!("read {path:?}: {error}"))?;
    let expected: Vec<ee::models::VerificationRunRecord> =
        serde_json::from_str(&fixture).map_err(|error| format!("parse {path:?}: {error}"))?;
    let actual = sample_verification_run_records();
    assert_eq!(expected, actual);
    assert!(actual.iter().any(|record| {
        record.schema == VERIFICATION_RUN_SCHEMA_V1 && record.execution_substrate == "rch"
    }));
    assert!(
        actual
            .iter()
            .any(|record| record.execution_substrate == "local_shell_static")
    );
    Ok(())
}

#[test]
fn verification_reuse_advisory_golden_covers_reusable_pass_and_stale_source() -> TestResult {
    let path = golden_dir().join("reuse_advisories.json.golden");
    let fixture = fs::read_to_string(&path).map_err(|error| format!("read {path:?}: {error}"))?;
    let expected: Vec<ee::models::VerificationReuseAdvisory> =
        serde_json::from_str(&fixture).map_err(|error| format!("parse {path:?}: {error}"))?;
    let actual = sample_verification_reuse_advisories();
    assert_eq!(expected, actual);
    assert!(actual.iter().any(|advisory| {
        advisory.schema == VERIFICATION_REUSE_ADVISORY_SCHEMA_V1
            && advisory.status == VerificationReuseStatus::ReusablePass
    }));
    assert!(
        actual
            .iter()
            .any(|advisory| advisory.status == VerificationReuseStatus::StaleSource)
    );
    Ok(())
}

#[test]
fn verification_broker_view_golden_covers_all_operator_states() -> TestResult {
    let path = golden_dir().join("broker_views.json.golden");
    let fixture = fs::read_to_string(&path).map_err(|error| format!("read {path:?}: {error}"))?;
    let expected: Vec<VerificationBrokerView> =
        serde_json::from_str(&fixture).map_err(|error| format!("parse {path:?}: {error}"))?;
    let actual = sample_verification_broker_views();
    assert_eq!(expected, actual);

    for status in [
        VerificationBrokerStatus::Reusable,
        VerificationBrokerStatus::KnownBlocker,
        VerificationBrokerStatus::InProgress,
        VerificationBrokerStatus::Stale,
        VerificationBrokerStatus::Incompatible,
        VerificationBrokerStatus::Unavailable,
    ] {
        assert!(
            actual
                .iter()
                .any(|view| view.schema == VERIFICATION_BROKER_VIEW_SCHEMA_V1
                    && view.status == status),
            "missing broker status {}",
            status.as_str()
        );
    }

    let encoded = serde_json::to_string(&actual).map_err(|error| error.to_string())?;
    assert!(!encoded.contains("/Volumes/USBNVME16TB"));
    assert!(!encoded.contains("/tmp/"));
    assert!(!encoded.contains("compile failed"));
    assert!(!encoded.contains("stderr bytes"));
    assert!(actual.iter().all(|view| {
        view.first_failure_summary_ref
            .as_ref()
            .is_none_or(|summary| !summary.raw_output_included)
    }));
    Ok(())
}

#[test]
fn verification_broker_views_validate_against_declared_schema() -> TestResult {
    let schema_path = broker_view_schema_path();
    let schema_text = fs::read_to_string(&schema_path)
        .map_err(|error| format!("read {}: {error}", schema_path.display()))?;
    let schema: serde_json::Value = serde_json::from_str(&schema_text)
        .map_err(|error| format!("parse {}: {error}", schema_path.display()))?;

    for (index, view) in sample_verification_broker_views().iter().enumerate() {
        let value = serde_json::to_value(view)
            .map_err(|error| format!("serialize broker view {index}: {error}"))?;
        validate_json_schema_subset(
            &schema,
            &value,
            &format!("sample_verification_broker_views[{index}]"),
        )?;
    }
    Ok(())
}

fn validate_json_schema_subset(
    schema: &serde_json::Value,
    value: &serde_json::Value,
    context: &str,
) -> TestResult {
    if let Some(expected) = schema.get("const") {
        if value != expected {
            return Err(format!("{context} expected const {expected}, got {value}"));
        }
    }

    if let Some(allowed) = schema.get("enum").and_then(serde_json::Value::as_array) {
        if !allowed.iter().any(|candidate| candidate == value) {
            return Err(format!("{context} value {value} not in enum {allowed:?}"));
        }
    }

    if let Some(type_spec) = schema.get("type") {
        ensure_json_type(type_spec, value, context)?;
    }

    if schema.get("type").and_then(serde_json::Value::as_str) == Some("object")
        || schema.get("properties").is_some()
    {
        validate_json_object_schema(schema, value, context)?;
    }

    if let Some(items_schema) = schema.get("items") {
        let items = value
            .as_array()
            .ok_or_else(|| format!("{context} expected array for items validation"))?;
        for (index, item) in items.iter().enumerate() {
            validate_json_schema_subset(items_schema, item, &format!("{context}[{index}]"))?;
        }
    }

    Ok(())
}

fn validate_json_object_schema(
    schema: &serde_json::Value,
    value: &serde_json::Value,
    context: &str,
) -> TestResult {
    if value.is_null() {
        return Ok(());
    }
    let object = value
        .as_object()
        .ok_or_else(|| format!("{context} expected object"))?;
    let properties = schema
        .get("properties")
        .and_then(serde_json::Value::as_object)
        .ok_or_else(|| format!("{context} schema missing properties"))?;

    let required = schema
        .get("required")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(serde_json::Value::as_str)
        .collect::<BTreeSet<_>>();
    for field in required {
        if !object.contains_key(field) {
            return Err(format!("{context} missing required field {field}"));
        }
    }

    if schema
        .get("additionalProperties")
        .and_then(serde_json::Value::as_bool)
        == Some(false)
    {
        for field in object.keys() {
            if !properties.contains_key(field) {
                return Err(format!("{context} has undeclared field {field}"));
            }
        }
    }

    for (field, field_value) in object {
        let field_schema = properties
            .get(field)
            .ok_or_else(|| format!("{context} has no schema for field {field}"))?;
        validate_json_schema_subset(field_schema, field_value, &format!("{context}.{field}"))?;
    }

    Ok(())
}

fn ensure_json_type(
    type_spec: &serde_json::Value,
    value: &serde_json::Value,
    context: &str,
) -> TestResult {
    let allowed = match type_spec {
        serde_json::Value::String(single) => vec![single.as_str()],
        serde_json::Value::Array(many) => many
            .iter()
            .filter_map(serde_json::Value::as_str)
            .collect::<Vec<_>>(),
        _ => {
            return Err(format!(
                "{context} schema has invalid type spec {type_spec}"
            ));
        }
    };
    if allowed
        .iter()
        .any(|kind| json_value_matches_type(value, kind))
    {
        Ok(())
    } else {
        Err(format!(
            "{context} value {value} does not match {allowed:?}"
        ))
    }
}

fn json_value_matches_type(value: &serde_json::Value, kind: &str) -> bool {
    match kind {
        "array" => value.is_array(),
        "boolean" => value.is_boolean(),
        "integer" => value.as_i64().is_some(),
        "null" => value.is_null(),
        "object" => value.is_object(),
        "string" => value.is_string(),
        _ => false,
    }
}

#[test]
fn verification_closeout_capsule_golden_covers_rch_and_support_bundle_shapes() -> TestResult {
    let path = golden_dir().join("closeout_capsules.json.golden");
    let fixture = fs::read_to_string(&path).map_err(|error| format!("read {path:?}: {error}"))?;
    let expected: Vec<VerificationCloseoutCapsule> =
        serde_json::from_str(&fixture).map_err(|error| format!("parse {path:?}: {error}"))?;
    let actual = sample_verification_closeout_capsules();
    assert_eq!(expected, actual);
    assert!(actual.iter().any(|capsule| {
        capsule.schema == VERIFICATION_CLOSEOUT_CAPSULE_SCHEMA_V1
            && capsule.requested_surface == "beads_comment"
            && capsule.execution_substrate == "rch"
            && capsule.failure_mode_codes.is_empty()
    }));
    assert!(actual.iter().any(|capsule| {
        capsule.requested_surface == "support_bundle"
            && capsule.failure_mode_codes == ["no_artifact_manifest"]
    }));
    Ok(())
}

#[test]
fn closeout_capsule_redacts_local_paths_and_raw_output_bytes() -> TestResult {
    let encoded = serde_json::to_string(&sample_verification_closeout_capsules())
        .map_err(|error| error.to_string())?;
    assert!(!encoded.contains("/Volumes/USBNVME16TB"));
    assert!(!encoded.contains("/tmp/"));
    assert!(!encoded.contains("remote worker css passed"));
    assert!(!encoded.contains("stderr bytes"));
    assert!(encoded.contains("retained_log_path_hash:blake3:retained-log-path"));
    for capsule in sample_verification_closeout_capsules() {
        assert!(!capsule.support_bundle_metadata.raw_output_included);
        assert!(capsule.support_bundle_metadata.local_paths_redacted);
    }
    Ok(())
}

#[test]
fn closeout_capsule_emits_caveats_for_local_cargo_and_source_mismatch() -> TestResult {
    let record = ee::models::VerificationRunRecord::from_input(ee::models::VerificationRunInput {
        run_id: Some("vrun_local_cargo"),
        bead_id: Some("bd-example"),
        agent_name: Some("NobleStork"),
        source_hash: Some("blake3:old-source"),
        command_hash: Some("blake3:local-cargo-command"),
        command_argv: &["cargo", "test", "--lib"],
        cargo_target_dir: Some("target"),
        execution_substrate: "local_cargo",
        worker_host: None,
        started_at: Some("2026-05-15T05:04:00Z"),
        finished_at: Some("2026-05-15T05:04:30Z"),
        exit_code: Some(0),
        stdout_hash: Some("blake3:stdout"),
        stderr_excerpt: Some("local cargo finished"),
        artifact_manifest_hash: None,
        retained_log_path: Some("/tmp/local-cargo.log"),
        provenance: Vec::new(),
    });
    let capsule = verification_closeout_capsule(
        VerificationCloseoutCapsuleRequest {
            requested_surface: "agent_mail",
            bead_id: Some("bd-example"),
            source_hash: Some("blake3:new-source"),
            reusable_until: None,
            source_must_match: true,
        },
        &record,
    );
    assert_eq!(capsule.result, "passed");
    assert_eq!(capsule.passed_count, Some(1));
    assert!(
        capsule
            .failure_mode_codes
            .contains(&"evidence_incomplete".to_owned())
    );
    assert!(
        capsule
            .failure_mode_codes
            .contains(&"local_cargo_disallowed".to_owned())
    );
    assert!(
        capsule
            .failure_mode_codes
            .contains(&"no_artifact_manifest".to_owned())
    );
    assert!(
        capsule
            .failure_mode_codes
            .contains(&"source_hash_mismatch".to_owned())
    );
    assert!(
        capsule
            .caveats
            .iter()
            .any(|caveat| caveat.contains("RCH rerun"))
    );
    let encoded = serde_json::to_string(&capsule).map_err(|error| error.to_string())?;
    assert!(!encoded.contains("/tmp/local-cargo.log"));
    assert!(!encoded.contains("local cargo finished"));
    Ok(())
}

#[test]
fn j1_artifact_manifest_import_builds_redacted_run_record() -> TestResult {
    let jsonl = r#"{"schema":"ee.test_event.v1","ts":"2026-05-15T05:00:00Z","test_id":"focused_rch","kind":"command_end","command":"/data/ee","args":["/data/ee","--json"],"stdout_hash":"blake3:stdout","stderr_excerpt":"remote worker css passed","exit_code":0,"elapsed_ms":42.0}
{"schema":"ee.test_event.v1","ts":"2026-05-15T05:00:01Z","test_id":"focused_rch","kind":"artifact_manifest","fields":{"manifest_schema":"ee.test_artifact_manifest.v1","phase":"command_end","binary_path":"/data/ee","binary_hash":"blake3:binary","source_hash":"blake3:source","command_hash":"blake3:manifest-command","command_arg_count":"2","execution_substrate":"rch","worker_host":"css","target_directory":"/Volumes/USBNVME16TB/temp_agent_space/rch-target-focused","log_path":"/tmp/ee-test-log.jsonl","artifact_manifest_hash":"blake3:manifest"}}"#;

    let records = verification_run_records_from_j1_jsonl(jsonl)
        .map_err(|error| format!("import J1 records: {error}"))?;

    assert_eq!(records.len(), 1);
    let record = &records[0];
    assert_eq!(record.schema, VERIFICATION_RUN_SCHEMA_V1);
    assert!(record.run_id.starts_with("vrun_"));
    assert_eq!(record.source_hash.as_deref(), Some("blake3:source"));
    assert_eq!(record.command_hash, "blake3:manifest-command");
    assert!(record.command_argv_hash.starts_with("blake3:"));
    assert_eq!(
        record.cargo_target_dir_hash_or_class.as_deref(),
        Some("class:external_cargo_target")
    );
    assert_eq!(record.execution_substrate, "rch");
    assert_eq!(record.worker_host.as_deref(), Some("css"));
    assert_eq!(record.finished_at.as_deref(), Some("2026-05-15T05:00:00Z"));
    assert_eq!(record.exit_code, Some(0));
    assert_eq!(record.stdout_hash.as_deref(), Some("blake3:stdout"));
    assert!(record.stderr_excerpt_hash.as_deref().is_some_and(|hash| {
        hash.starts_with("blake3:") && !hash.contains("remote worker css passed")
    }));
    assert_eq!(
        record.artifact_manifest_hash.as_deref(),
        Some("blake3:manifest")
    );
    assert!(
        record
            .retained_log_path_hash
            .as_deref()
            .is_some_and(|hash| {
                hash.starts_with("blake3:") && !hash.contains("/tmp/ee-test-log.jsonl")
            })
    );
    assert_eq!(record.provenance.len(), 2);
    let encoded = serde_json::to_string(record).map_err(|error| error.to_string())?;
    assert!(!encoded.contains("remote worker css passed"));
    assert!(!encoded.contains("/Volumes/USBNVME16TB"));
    assert!(!encoded.contains("/tmp/ee-test-log.jsonl"));
    Ok(())
}

#[test]
fn j1_import_rejects_raw_output_and_missing_manifest() -> TestResult {
    let raw_output = r#"{"schema":"ee.test_event.v1","ts":"2026-05-15T05:00:00Z","test_id":"focused_rch","kind":"command_end","command":"ee","stdout":"raw bytes","exit_code":0}"#;
    let error = match verification_run_records_from_j1_jsonl(raw_output) {
        Ok(records) => panic!("raw stdout field must be rejected, got {records:?}"),
        Err(error) => error,
    };
    assert_eq!(
        error,
        VerificationRunImportError::RawOutputRejected {
            line: 1,
            field: "stdout".to_owned()
        }
    );

    let missing_manifest = r#"{"schema":"ee.test_event.v1","ts":"2026-05-15T05:00:00Z","test_id":"focused_rch","kind":"command_end","command":"ee","exit_code":0}"#;
    let error = match verification_run_records_from_j1_jsonl(missing_manifest) {
        Ok(records) => {
            panic!("command_end without artifact_manifest must be rejected, got {records:?}")
        }
        Err(error) => error,
    };
    assert_eq!(
        error,
        VerificationRunImportError::MissingArtifactManifest { line: 1 }
    );
    Ok(())
}

#[test]
fn reuse_advisory_reports_all_statuses() -> TestResult {
    let records = sample_verification_run_records();
    let failed_record =
        ee::models::VerificationRunRecord::from_input(ee::models::VerificationRunInput {
            run_id: Some("vrun_failed"),
            bead_id: Some("bd-example"),
            agent_name: Some("RubyWolf"),
            source_hash: Some("blake3:source"),
            command_hash: Some("blake3:failed-command"),
            command_argv: &["cargo", "test", "failed"],
            cargo_target_dir: Some("/Volumes/USBNVME16TB/temp_agent_space/rch-target-failed"),
            execution_substrate: "rch",
            worker_host: Some("css"),
            started_at: Some("2026-05-15T05:02:00Z"),
            finished_at: Some("2026-05-15T05:02:42Z"),
            exit_code: Some(101),
            stdout_hash: Some("blake3:stdout"),
            stderr_excerpt: Some("compile failed"),
            artifact_manifest_hash: Some("blake3:manifest"),
            retained_log_path: Some("/tmp/log.jsonl"),
            provenance: Vec::new(),
        });
    let in_flight_record =
        ee::models::VerificationRunRecord::from_input(ee::models::VerificationRunInput {
            run_id: Some("vrun_in_flight"),
            bead_id: Some("bd-example"),
            agent_name: Some("RubyWolf"),
            source_hash: Some("blake3:source"),
            command_hash: Some("blake3:in-flight-command"),
            command_argv: &["cargo", "test", "in-flight"],
            cargo_target_dir: Some("/Volumes/USBNVME16TB/temp_agent_space/rch-target-in-flight"),
            execution_substrate: "rch",
            worker_host: Some("css"),
            started_at: Some("2026-05-15T05:03:00Z"),
            finished_at: None,
            exit_code: None,
            stdout_hash: None,
            stderr_excerpt: None,
            artifact_manifest_hash: Some("blake3:manifest"),
            retained_log_path: Some("/tmp/log.jsonl"),
            provenance: Vec::new(),
        });
    let mut extended = records.clone();
    extended.push(failed_record);
    extended.push(in_flight_record);

    for (request, expected_status) in [
        (
            reuse_request(Some("blake3:source"), "blake3:rch-command", "rch"),
            VerificationReuseStatus::ReusablePass,
        ),
        (
            reuse_request(Some("blake3:source"), "blake3:failed-command", "rch"),
            VerificationReuseStatus::ReusableFail,
        ),
        (
            reuse_request(Some("blake3:source"), "blake3:in-flight-command", "rch"),
            VerificationReuseStatus::InFlight,
        ),
        (
            reuse_request(Some("blake3:new-source"), "blake3:rch-command", "rch"),
            VerificationReuseStatus::StaleSource,
        ),
        (
            reuse_request(Some("blake3:source"), "blake3:new-command", "rch"),
            VerificationReuseStatus::MismatchedCommand,
        ),
        (
            reuse_request(Some("blake3:other-source"), "blake3:other-command", "rch"),
            VerificationReuseStatus::RerunRequired,
        ),
    ] {
        let advisory = verification_reuse_advisory(request, &extended);
        assert_eq!(advisory.status, expected_status);
        assert!(!advisory.repair_actions.is_empty());
    }

    let missing = verification_reuse_advisory(
        reuse_request(Some("blake3:source"), "blake3:rch-command", "rch"),
        &[],
    );
    assert_eq!(missing.status, VerificationReuseStatus::MissingEvidence);
    assert_eq!(missing.repair_actions[0].kind, "import_retained_j1_log");
    Ok(())
}

fn reuse_request<'a>(
    source_hash: Option<&'a str>,
    command_hash: &'a str,
    execution_substrate: &'a str,
) -> VerificationReuseRequest<'a> {
    VerificationReuseRequest {
        bead_id: Some("bd-example"),
        source_hash,
        command_hash,
        execution_substrate,
        feature_profile_hash: Some("blake3:profile"),
        workspace_generation: Some(42),
        strictness_flags: vec!["RCH_REQUIRE_REMOTE=1"],
    }
}
