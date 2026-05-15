//! S6 swarm-schema lifecycle gates.
//!
//! These tests keep agent-facing swarm contracts honest: schema filenames are
//! canonical, examples are fixture-backed, docs exist, and availability markers
//! match Beads state.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;

type TestResult = Result<(), String>;

#[derive(Clone, Copy)]
struct SchemaCase {
    id: &'static str,
    file_name: &'static str,
    doc_path: &'static str,
    tracking_bead: &'static str,
    shipped: bool,
}

#[derive(Clone, Copy)]
struct DriftCase {
    schema_id: &'static str,
    command: &'static str,
    json_path: &'static str,
    fixture_manifest_key: &'static str,
}

const SCHEMA_CASES: &[SchemaCase] = &[
    SchemaCase {
        id: "ee.producer.metadata.v1",
        file_name: "ee.producer.metadata.v1.json",
        doc_path: "docs/swarm/producer_metadata.md",
        tracking_bead: "bd-1zb7k.1",
        shipped: true,
    },
    SchemaCase {
        id: "ee.trust_lane.v1",
        file_name: "ee.trust_lane.v1.json",
        doc_path: "docs/swarm/trust_lane.md",
        tracking_bead: "bd-1zb7k.2",
        shipped: true,
    },
    SchemaCase {
        id: "ee.verification.evidence.v1",
        file_name: "ee.verification.evidence.v1.json",
        doc_path: "docs/swarm/verification_evidence.md",
        tracking_bead: "bd-1zb7k.3",
        shipped: true,
    },
    SchemaCase {
        id: "ee.verification.broker_view.v1",
        file_name: "ee.verification.broker_view.v1.json",
        doc_path: "docs/swarm/verification_broker_view.md",
        tracking_bead: "bd-6boyo.1",
        shipped: false,
    },
    SchemaCase {
        id: "ee.coordination_snapshot.v1",
        file_name: "ee.coordination_snapshot.v1.json",
        doc_path: "docs/swarm/coordination_snapshot.md",
        tracking_bead: "bd-1zb7k.4",
        shipped: true,
    },
    SchemaCase {
        id: "ee.coordination_fallback_evidence.v1",
        file_name: "ee.coordination_fallback_evidence.v1.json",
        doc_path: "docs/swarm/coordination_fallback_evidence.md",
        tracking_bead: "bd-1zb7k.13.2",
        shipped: false,
    },
    SchemaCase {
        id: "ee.resource.profile.v1",
        file_name: "ee.resource.profile.v1.json",
        doc_path: "docs/swarm/resource_profile.md",
        tracking_bead: "bd-1zb7k.5",
        shipped: true,
    },
    SchemaCase {
        id: "ee.pack.slo.v1",
        file_name: "ee.pack.slo.v1.json",
        doc_path: "docs/swarm/pack_slo.md",
        tracking_bead: "bd-1zb7k.5",
        shipped: true,
    },
    SchemaCase {
        id: "ee.consensus.v1",
        file_name: "ee.consensus.v1.json",
        doc_path: "docs/swarm/consensus.md",
        tracking_bead: "bd-1zb7k.9",
        shipped: true,
    },
    SchemaCase {
        id: "ee.conflict.v1",
        file_name: "ee.conflict.v1.json",
        doc_path: "docs/swarm/conflict.md",
        tracking_bead: "bd-1zb7k.9",
        shipped: true,
    },
    SchemaCase {
        id: "ee.swarm_fixture_corpus.v1",
        file_name: "ee.swarm_fixture_corpus.v1.json",
        doc_path: "docs/swarm/swarm_fixture_corpus.md",
        tracking_bead: "bd-1zb7k.6",
        shipped: false,
    },
    SchemaCase {
        id: "ee.handoff.memory_set_fingerprint.v1",
        file_name: "ee.handoff.memory_set_fingerprint.v1.json",
        doc_path: "docs/swarm/handoff_memory_set_fingerprint.md",
        tracking_bead: "bd-17c65.13.5",
        shipped: false,
    },
    SchemaCase {
        id: "ee.swarm.brief.v1",
        file_name: "ee.swarm.brief.v1.json",
        doc_path: "docs/swarm/swarm_brief.md",
        tracking_bead: "bd-1zb7k.16.4",
        shipped: false,
    },
    SchemaCase {
        id: "ee.support_bundle.swarm_brief_summary.v1",
        file_name: "ee.support_bundle.swarm_brief_summary.v1.json",
        doc_path: "docs/swarm/support_bundle_swarm_brief_summary.md",
        tracking_bead: "bd-1zb7k.16.4",
        shipped: false,
    },
    SchemaCase {
        id: "ee.swarm.recommendation.v1",
        file_name: "ee.swarm.recommendation.v1.json",
        doc_path: "docs/swarm/swarm_recommendation.md",
        tracking_bead: "bd-2nkbn",
        shipped: true,
    },
    SchemaCase {
        id: "ee.swarm_incident.v1",
        file_name: "ee.swarm_incident.v1.json",
        doc_path: "docs/swarm/swarm_incident_drills.md",
        tracking_bead: "bd-1zb7k.14.1",
        shipped: false,
    },
];

const DRIFT_CASES: &[DriftCase] = &[
    DriftCase {
        schema_id: "ee.producer.metadata.v1",
        command: "ee remember --json",
        json_path: ".data.memory.producer",
        fixture_manifest_key: "ee.producer.metadata.v1",
    },
    DriftCase {
        schema_id: "ee.trust_lane.v1",
        command: "ee context --memory-scope swarm --json",
        json_path: ".data.scopeStats",
        fixture_manifest_key: "ee.trust_lane.v1",
    },
    DriftCase {
        schema_id: "ee.verification.evidence.v1",
        command: "ee verification ingest --stdin --json",
        json_path: ".data.evidence",
        fixture_manifest_key: "ee.verification.evidence.v1",
    },
    DriftCase {
        schema_id: "ee.verification.broker_view.v1",
        command: "planned ee verify broker lookup --json",
        json_path: ".data.broker",
        fixture_manifest_key: "ee.verification.broker_view.v1",
    },
    DriftCase {
        schema_id: "ee.coordination_snapshot.v1",
        command: "ee context --coordination-snapshot snapshot.json --json",
        json_path: ".data.pack.coordination",
        fixture_manifest_key: "ee.coordination_snapshot.v1",
    },
    DriftCase {
        schema_id: "ee.coordination_fallback_evidence.v1",
        command: "ee coordination evidence ingest --stdin --json",
        json_path: ".data.evidence",
        fixture_manifest_key: "ee.coordination_fallback_evidence.v1",
    },
    DriftCase {
        schema_id: "ee.resource.profile.v1",
        command: "ee context --resource-profile swarm_heavy --json",
        json_path: ".data.pack.slo.{profile,budgetClass}",
        fixture_manifest_key: "ee.resource.profile.v1",
    },
    DriftCase {
        schema_id: "ee.pack.slo.v1",
        command: "ee context --json",
        json_path: ".data.pack.slo",
        fixture_manifest_key: "ee.pack.slo.v1",
    },
    DriftCase {
        schema_id: "ee.consensus.v1",
        command: "ee context --include-consensus --json",
        json_path: ".data.consensus[]",
        fixture_manifest_key: "ee.consensus.v1",
    },
    DriftCase {
        schema_id: "ee.conflict.v1",
        command: "ee context --include-conflicts --json",
        json_path: ".data.conflicts[]",
        fixture_manifest_key: "ee.conflict.v1",
    },
    DriftCase {
        schema_id: "ee.swarm_fixture_corpus.v1",
        command: "fixture manifest",
        json_path: ".examples[\"ee.swarm_fixture_corpus.v1\"]",
        fixture_manifest_key: "ee.swarm_fixture_corpus.v1",
    },
    DriftCase {
        schema_id: "ee.handoff.memory_set_fingerprint.v1",
        command: "planned handoff capsule output",
        json_path: ".examples[\"ee.handoff.memory_set_fingerprint.v1\"]",
        fixture_manifest_key: "ee.handoff.memory_set_fingerprint.v1",
    },
    DriftCase {
        schema_id: "ee.swarm.brief.v1",
        command: "ee swarm brief --json",
        json_path: ".data",
        fixture_manifest_key: "ee.swarm.brief.v1",
    },
    DriftCase {
        schema_id: "ee.support_bundle.swarm_brief_summary.v1",
        command: "ee support-bundle create --redacted --json",
        json_path: "swarm_brief_summary.json",
        fixture_manifest_key: "ee.support_bundle.swarm_brief_summary.v1",
    },
    DriftCase {
        schema_id: "ee.swarm.recommendation.v1",
        command: "ee swarm brief --json",
        json_path: ".data.recommendations[]",
        fixture_manifest_key: "ee.swarm.recommendation.v1",
    },
    DriftCase {
        schema_id: "ee.swarm_incident.v1",
        command: "ee diag incident --fixture tests/fixtures/swarm_incidents/rch_topology_blocked.json --json",
        json_path: ".examples[\"ee.swarm_incident.v1\"]",
        fixture_manifest_key: "ee.swarm_incident.v1",
    },
];

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn swarm_schema_dir() -> PathBuf {
    repo_root().join("docs").join("schemas").join("swarm")
}

fn read_text(path: &Path) -> Result<String, String> {
    fs::read_to_string(path).map_err(|error| format!("read {}: {error}", path.display()))
}

fn read_json(path: &Path) -> Result<Value, String> {
    let text = read_text(path)?;
    serde_json::from_str(&text).map_err(|error| format!("parse {}: {error}", path.display()))
}

fn schema_path(case: SchemaCase) -> PathBuf {
    swarm_schema_dir().join(case.file_name)
}

fn schema_doc(case: SchemaCase) -> Result<Value, String> {
    read_json(&schema_path(case))
}

fn string_field<'a>(value: &'a Value, pointer: &str, context: &str) -> Result<&'a str, String> {
    value
        .pointer(pointer)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("{context} missing string {pointer}"))
}

fn bool_field(value: &Value, pointer: &str, context: &str) -> Result<bool, String> {
    value
        .pointer(pointer)
        .and_then(Value::as_bool)
        .ok_or_else(|| format!("{context} missing boolean {pointer}"))
}

fn fixture_examples() -> Result<BTreeMap<String, Value>, String> {
    let fixture = read_json(
        &repo_root()
            .join("tests")
            .join("fixtures")
            .join("swarm_schemas")
            .join("all_examples.json"),
    )?;
    fixture
        .get("examples")
        .and_then(Value::as_object)
        .ok_or_else(|| "swarm schema fixture manifest missing examples object".to_string())
        .map(|examples| {
            examples
                .iter()
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect()
        })
}

#[test]
fn swarm_schema_catalog_is_complete_and_canonical() -> TestResult {
    let actual_files = fs::read_dir(swarm_schema_dir())
        .map_err(|error| format!("read swarm schema dir: {error}"))?
        .filter_map(|entry| entry.ok())
        .filter_map(|entry| {
            let path = entry.path();
            if path.extension().and_then(|extension| extension.to_str()) == Some("json") {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .map(ToOwned::to_owned)
            } else {
                None
            }
        })
        .collect::<BTreeSet<_>>();
    let expected_files = SCHEMA_CASES
        .iter()
        .map(|case| case.file_name.to_owned())
        .collect::<BTreeSet<_>>();
    if actual_files != expected_files {
        return Err(format!(
            "swarm schema files drifted\nactual: {actual_files:?}\nexpected: {expected_files:?}"
        ));
    }

    let readme = read_text(&swarm_schema_dir().join("README.md"))?;
    if !readme.contains("x-ee-status") || !readme.contains("Non-goals") {
        return Err(
            "docs/schemas/swarm/README.md must describe status markers and non-goals".into(),
        );
    }

    for case in SCHEMA_CASES {
        let schema = schema_doc(*case)?;
        let context = case.file_name;
        if string_field(&schema, "/$schema", context)? != "http://json-schema.org/draft-07/schema#"
        {
            return Err(format!("{} must use JSON Schema draft-07", case.file_name));
        }
        let expected_id = format!("https://eidetic-engine/schemas/swarm/{}", case.file_name);
        if string_field(&schema, "/$id", context)? != expected_id {
            return Err(format!("{} has non-canonical $id", case.file_name));
        }
        if string_field(&schema, "/title", context)? != case.id {
            return Err(format!("{} title must equal {}", case.file_name, case.id));
        }
        if string_field(&schema, "/type", context)? != "object" {
            return Err(format!("{} root type must be object", case.file_name));
        }
        if !matches!(schema.get("additionalProperties"), Some(Value::Bool(false))) {
            return Err(format!(
                "{} root additionalProperties must be false",
                case.file_name
            ));
        }
        if schema
            .get("required")
            .and_then(Value::as_array)
            .is_none_or(Vec::is_empty)
        {
            return Err(format!("{} must declare required fields", case.file_name));
        }
        if schema
            .get("examples")
            .and_then(Value::as_array)
            .is_none_or(Vec::is_empty)
        {
            return Err(format!("{} must include examples", case.file_name));
        }
        if string_field(&schema, "/x-ee-doc", context)? != case.doc_path {
            return Err(format!(
                "{} x-ee-doc must match test catalog",
                case.file_name
            ));
        }
    }

    Ok(())
}

#[test]
fn swarm_schema_docs_cover_every_schema() -> TestResult {
    for case in SCHEMA_CASES {
        let path = repo_root().join(case.doc_path);
        let text = read_text(&path)?;
        for required in [case.id, case.tracking_bead, "Non-goals"] {
            if !text.contains(required) {
                return Err(format!(
                    "{} must mention {required}",
                    path.strip_prefix(repo_root())
                        .unwrap_or(path.as_path())
                        .display()
                ));
            }
        }
    }
    Ok(())
}

#[test]
fn swarm_schema_examples_have_fixture_rows() -> TestResult {
    let fixtures = fixture_examples()?;
    let fixture_keys = fixtures.keys().cloned().collect::<BTreeSet<_>>();
    let schema_ids = SCHEMA_CASES
        .iter()
        .map(|case| case.id.to_owned())
        .collect::<BTreeSet<_>>();
    if fixture_keys != schema_ids {
        return Err(format!(
            "swarm fixture manifest keys drifted\nactual: {fixture_keys:?}\nexpected: {schema_ids:?}"
        ));
    }

    for case in SCHEMA_CASES {
        let schema = schema_doc(*case)?;
        let first_example = schema
            .get("examples")
            .and_then(Value::as_array)
            .and_then(|examples| examples.first())
            .ok_or_else(|| format!("{} missing first example", case.file_name))?;
        let fixture = fixtures
            .get(case.id)
            .ok_or_else(|| format!("fixture manifest missing {}", case.id))?;
        if fixture != first_example {
            return Err(format!(
                "{} first schema example drifted from tests/fixtures/swarm_schemas/all_examples.json",
                case.id
            ));
        }
    }

    Ok(())
}

#[test]
fn coordination_fallback_examples_cover_statuses_and_redaction_contract() -> TestResult {
    let case = SCHEMA_CASES
        .iter()
        .copied()
        .find(|case| case.id == "ee.coordination_fallback_evidence.v1")
        .ok_or_else(|| "coordination fallback schema case missing".to_owned())?;
    let schema = schema_doc(case)?;
    let examples = schema
        .get("examples")
        .and_then(Value::as_array)
        .ok_or_else(|| "coordination fallback schema missing examples".to_owned())?;

    let mut statuses = BTreeSet::new();
    let mut content_hashes = BTreeSet::new();
    for (index, example) in examples.iter().enumerate() {
        let context = format!("coordination fallback example {index}");
        statuses.insert(string_field(example, "/status", &context)?.to_owned());
        let content_hash = string_field(example, "/summary/contentHash", &context)?;
        if !content_hashes.insert(content_hash.to_owned()) {
            return Err(format!("{context} reuses content hash {content_hash}"));
        }
        if !bool_field(example, "/summary/redacted", &context)? {
            return Err(format!("{context} summary must be redacted"));
        }
        if bool_field(example, "/redaction/rawInboxIncluded", &context)?
            || bool_field(example, "/redaction/rawLogIncluded", &context)?
        {
            return Err(format!("{context} must not include raw inboxes or logs"));
        }
        if !bool_field(example, "/redaction/secretScanApplied", &context)? {
            return Err(format!("{context} must apply secret scanning"));
        }
        if string_field(example, "/redaction/pathPolicy", &context)? != "labels_only" {
            return Err(format!("{context} must keep path policy labels_only"));
        }
    }

    let expected = ["blocked", "stale", "unavailable", "unknown"]
        .into_iter()
        .map(str::to_owned)
        .collect::<BTreeSet<_>>();
    if statuses != expected {
        return Err(format!(
            "coordination fallback examples must cover required non-available statuses\nactual: {statuses:?}\nexpected: {expected:?}"
        ));
    }

    Ok(())
}

#[test]
fn swarm_schema_availability_matches_bead_state() -> TestResult {
    let issue_states = latest_issue_states()?;
    for case in SCHEMA_CASES {
        let schema = schema_doc(*case)?;
        let context = case.file_name;
        let shipped = bool_field(&schema, "/x-ee-status/shipped", context)?;
        let available = bool_field(&schema, "/x-ee-status/available_in_build", context)?;
        let tracking_bead = string_field(&schema, "/x-ee-status/tracking_bead", context)?;
        if shipped != case.shipped {
            return Err(format!("{} shipped marker drifted", case.file_name));
        }
        if available != case.shipped {
            return Err(format!(
                "{} available_in_build must match shipped",
                case.file_name
            ));
        }
        if tracking_bead != case.tracking_bead {
            return Err(format!("{} tracking_bead drifted", case.file_name));
        }

        let status = issue_states
            .get(case.tracking_bead)
            .ok_or_else(|| format!("{} tracking bead not found", case.tracking_bead))?;
        match (case.shipped, status.as_str()) {
            (true, "closed") => {}
            (false, "open" | "in_progress") => {}
            _ => {
                return Err(format!(
                    "{} x-ee-status says shipped={}, but {} is {}",
                    case.id, case.shipped, case.tracking_bead, status
                ));
            }
        }
    }
    Ok(())
}

#[test]
fn swarm_schema_drift_rows_cover_catalog() -> TestResult {
    let schema_ids = SCHEMA_CASES
        .iter()
        .map(|case| case.id.to_owned())
        .collect::<BTreeSet<_>>();
    let drift_ids = DRIFT_CASES
        .iter()
        .map(|case| case.schema_id.to_owned())
        .collect::<BTreeSet<_>>();
    if drift_ids != schema_ids {
        return Err(format!(
            "swarm drift cases must cover every schema\nactual: {drift_ids:?}\nexpected: {schema_ids:?}"
        ));
    }

    let fixtures = fixture_examples()?;
    for case in DRIFT_CASES {
        if !fixtures.contains_key(case.fixture_manifest_key) {
            return Err(format!(
                "{} drift case references missing fixture key {}",
                case.schema_id, case.fixture_manifest_key
            ));
        }
        tracing::info!(
            target: "ee::contracts::schema_drift",
            schema_id = case.schema_id,
            cmd_hash = %stable_command_hash(case.command),
            json_path = case.json_path,
            fixture_path = "tests/fixtures/swarm_schemas/all_examples.json",
            validation_errors = 0_u8,
            "swarm schema drift case covered"
        );
    }
    Ok(())
}

#[test]
fn swarm_brief_golden_ownership_risks_match_schema_contract() -> TestResult {
    let golden = read_json(
        &repo_root()
            .join("tests")
            .join("fixtures")
            .join("golden")
            .join("swarm")
            .join("brief_contract_matrix.json.golden"),
    )?;
    if string_field(
        &golden,
        "/payloadSchema",
        "tests/fixtures/golden/swarm/brief_contract_matrix.json.golden",
    )? != "ee.swarm.brief.v1"
    {
        return Err("swarm brief golden payloadSchema must be ee.swarm.brief.v1".into());
    }

    let cases = golden
        .get("cases")
        .and_then(Value::as_array)
        .ok_or_else(|| "swarm brief golden missing cases array".to_string())?;
    let mut ownership_case_count = 0_usize;
    for case in cases {
        let case_name = case
            .get("case")
            .and_then(Value::as_str)
            .unwrap_or("<unknown>");
        if case
            .get("schema")
            .and_then(Value::as_str)
            .is_some_and(|schema| schema != "ee.swarm.brief.v1")
        {
            return Err(format!("{case_name} uses a non-swarm-brief schema"));
        }
        let risks = case
            .get("fileSurfaceRisks")
            .and_then(Value::as_array)
            .ok_or_else(|| format!("{case_name} missing fileSurfaceRisks array"))?;
        for (index, risk) in risks.iter().enumerate() {
            ownership_case_count += 1;
            let context = format!("{case_name}.fileSurfaceRisks[{index}]");
            for field in [
                "pathPattern",
                "gitStatusBuckets",
                "reservationHolders",
                "relatedBeadIds",
                "severity",
                "score",
                "riskFactors",
                "evidence",
                "suggestedCommands",
            ] {
                if risk.get(field).is_none() {
                    return Err(format!("{context} missing {field}"));
                }
            }
        }
    }

    if ownership_case_count == 0 {
        return Err("swarm brief golden must cover at least one file surface risk".into());
    }

    Ok(())
}

#[test]
fn ownership_posture_fixture_catalog_covers_required_cases() -> TestResult {
    let fixture = read_json(
        &repo_root()
            .join("tests")
            .join("fixtures")
            .join("swarm")
            .join("ownership_posture_cases.json"),
    )?;
    if string_field(&fixture, "/schema", "ownership_posture_cases.json")?
        != "ee.swarm.ownership_posture_cases.v1"
    {
        return Err("ownership posture fixture catalog schema drifted".into());
    }

    let payload_schemas = fixture
        .get("payloadSchemas")
        .and_then(Value::as_array)
        .ok_or_else(|| "ownership posture fixture missing payloadSchemas".to_string())?
        .iter()
        .filter_map(Value::as_str)
        .collect::<BTreeSet<_>>();
    for required in [
        "ee.swarm.brief.v1",
        "ee.support_bundle.swarm_brief_summary.v1",
    ] {
        if !payload_schemas.contains(required) {
            return Err(format!(
                "ownership posture fixture missing payload schema {required}"
            ));
        }
    }

    let cases = fixture
        .get("cases")
        .and_then(Value::as_array)
        .ok_or_else(|| "ownership posture fixture missing cases".to_string())?;
    let categories = cases
        .iter()
        .filter_map(|case| case.get("category").and_then(Value::as_str))
        .collect::<BTreeSet<_>>();
    for required in ["healthy", "degraded_source", "unattributed_blocker"] {
        if !categories.contains(required) {
            return Err(format!(
                "ownership posture fixture missing required category {required}"
            ));
        }
    }

    let rendered = serde_json::to_string(&fixture)
        .map_err(|error| format!("serialize ownership posture fixture: {error}"))?;
    for forbidden in [
        "ghp_",
        "raw secret body",
        "BEGIN PRIVATE KEY",
        "DATABASE_URL=",
    ] {
        if rendered.contains(forbidden) {
            return Err(format!(
                "ownership posture fixture leaked forbidden marker {forbidden}"
            ));
        }
    }

    for case in cases {
        let id = case
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or("<unknown>");
        if string_field(case, "/fullOutput/schema", id)? != "ee.swarm.brief.v1" {
            return Err(format!("{id} fullOutput must use ee.swarm.brief.v1"));
        }
        if string_field(case, "/compactSummary/schema", id)?
            != "ee.support_bundle.swarm_brief_summary.v1"
        {
            return Err(format!(
                "{id} compactSummary must use ee.support_bundle.swarm_brief_summary.v1"
            ));
        }
        for pointer in [
            "/compactSummary/redaction/rawMailBodiesIncluded",
            "/compactSummary/redaction/rawQueryTextIncluded",
            "/compactSummary/redaction/rawProvenanceTextIncluded",
            "/compactSummary/redaction/fullFileListingsIncluded",
        ] {
            if bool_field(case, pointer, id)? {
                return Err(format!("{id} must keep {pointer} false"));
            }
        }
    }

    Ok(())
}

fn latest_issue_states() -> Result<BTreeMap<String, String>, String> {
    let text = read_text(&repo_root().join(".beads").join("issues.jsonl"))?;
    let mut states = BTreeMap::new();
    for (line_index, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let issue: Value = serde_json::from_str(line).map_err(|error| {
            format!("parse .beads/issues.jsonl line {}: {error}", line_index + 1)
        })?;
        let id = issue
            .get("id")
            .and_then(Value::as_str)
            .ok_or_else(|| format!(".beads/issues.jsonl line {} missing id", line_index + 1))?;
        let status = issue
            .get("status")
            .and_then(Value::as_str)
            .ok_or_else(|| format!(".beads/issues.jsonl line {} missing status", line_index + 1))?;
        states.insert(id.to_owned(), status.to_owned());
    }
    Ok(states)
}

fn stable_command_hash(command: &str) -> String {
    format!("blake3:{}", blake3::hash(command.as_bytes()).to_hex())
}
