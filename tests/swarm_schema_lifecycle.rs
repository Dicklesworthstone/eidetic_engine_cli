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

struct MultiExampleCoverage {
    schema_id: &'static str,
    discriminator_pointer: &'static str,
    expected_values: &'static [&'static str],
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
        tracking_bead: "bd-6boyo.2",
        shipped: true,
    },
    SchemaCase {
        id: "ee.verification.run.v1",
        file_name: "ee.verification.run.v1.json",
        doc_path: "docs/swarm/verification_run.md",
        tracking_bead: "bd-1zb7k.15.1",
        shipped: true,
    },
    SchemaCase {
        id: "ee.verification.reuse_advisory.v1",
        file_name: "ee.verification.reuse_advisory.v1.json",
        doc_path: "docs/swarm/verification_reuse_advisory.md",
        tracking_bead: "bd-1zb7k.15.2",
        shipped: true,
    },
    SchemaCase {
        id: "ee.verification.closeout_capsule.v1",
        file_name: "ee.verification.closeout_capsule.v1.json",
        doc_path: "docs/swarm/verification_closeout_capsule.md",
        tracking_bead: "bd-1zb7k.15.3",
        shipped: true,
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
        shipped: true,
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
        shipped: true,
    },
    SchemaCase {
        id: "ee.handoff.memory_set_fingerprint.v1",
        file_name: "ee.handoff.memory_set_fingerprint.v1.json",
        doc_path: "docs/swarm/handoff_memory_set_fingerprint.md",
        tracking_bead: "bd-17c65.13.5",
        shipped: true,
    },
    SchemaCase {
        id: "ee.swarm.brief.v1",
        file_name: "ee.swarm.brief.v1.json",
        doc_path: "docs/swarm/swarm_brief.md",
        tracking_bead: "bd-1zb7k.16.4",
        shipped: true,
    },
    SchemaCase {
        id: "ee.support_bundle.swarm_brief_summary.v1",
        file_name: "ee.support_bundle.swarm_brief_summary.v1.json",
        doc_path: "docs/swarm/support_bundle_swarm_brief_summary.md",
        tracking_bead: "bd-1zb7k.16.4",
        shipped: true,
    },
    SchemaCase {
        id: "ee.swarm.recommendation.v1",
        file_name: "ee.swarm.recommendation.v1.json",
        doc_path: "docs/swarm/swarm_recommendation.md",
        tracking_bead: "bd-2nkbn",
        shipped: true,
    },
    SchemaCase {
        id: "ee.swarm.work_packet.v1",
        file_name: "ee.swarm.work_packet.v1.json",
        doc_path: "docs/swarm/work_packet.md",
        tracking_bead: "bd-2z5ly.2",
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
        command: "ee verify broker lookup --json",
        json_path: ".data.broker",
        fixture_manifest_key: "ee.verification.broker_view.v1",
    },
    DriftCase {
        schema_id: "ee.verification.run.v1",
        command: "ee verification evidence import --json",
        json_path: ".data.run",
        fixture_manifest_key: "ee.verification.run.v1",
    },
    DriftCase {
        schema_id: "ee.verification.reuse_advisory.v1",
        command: "ee verify broker lookup --json",
        json_path: ".data.advisory",
        fixture_manifest_key: "ee.verification.reuse_advisory.v1",
    },
    DriftCase {
        schema_id: "ee.verification.closeout_capsule.v1",
        command: "ee verify closeout capsule --json",
        json_path: ".data.closeoutCapsule",
        fixture_manifest_key: "ee.verification.closeout_capsule.v1",
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
        schema_id: "ee.swarm.work_packet.v1",
        command: "ee swarm work-packet --json",
        json_path: ".examples[\"ee.swarm.work_packet.v1\"]",
        fixture_manifest_key: "ee.swarm.work_packet.v1",
    },
    DriftCase {
        schema_id: "ee.swarm_incident.v1",
        command: "ee diag incident --fixture tests/fixtures/swarm_incidents/rch_topology_blocked.json --json",
        json_path: ".examples[\"ee.swarm_incident.v1\"]",
        fixture_manifest_key: "ee.swarm_incident.v1",
    },
];

const MULTI_EXAMPLE_COVERAGE: &[MultiExampleCoverage] = &[
    MultiExampleCoverage {
        schema_id: "ee.coordination_fallback_evidence.v1",
        discriminator_pointer: "/status",
        expected_values: &["blocked", "stale", "unavailable", "unknown"],
    },
    MultiExampleCoverage {
        schema_id: "ee.producer.metadata.v1",
        discriminator_pointer: "/sourceSystem",
        expected_values: &["cli", "verification"],
    },
    MultiExampleCoverage {
        schema_id: "ee.swarm.work_packet.v1",
        discriminator_pointer: "/observedStateClass",
        expected_values: &[
            "crowded_checkout",
            "degraded_mail_rch_topology",
            "healthy_small_repo",
        ],
    },
    MultiExampleCoverage {
        schema_id: "ee.trust_lane.v1",
        discriminator_pointer: "/scopeApplied",
        expected_values: &["self", "swarm"],
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

fn schema_case_by_id(schema_id: &str) -> Result<SchemaCase, String> {
    SCHEMA_CASES
        .iter()
        .copied()
        .find(|case| case.id == schema_id)
        .ok_or_else(|| format!("schema case missing for {schema_id}"))
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
        let dialect = string_field(&schema, "/$schema", context)?;
        if !matches!(
            dialect,
            "http://json-schema.org/draft-07/schema#"
                | "https://json-schema.org/draft/2020-12/schema"
        ) {
            return Err(format!(
                "{} must use draft-07 or draft/2020-12, got {dialect}",
                case.file_name
            ));
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
fn multi_example_schema_examples_have_explicit_golden_coverage() -> TestResult {
    let covered_schema_ids = MULTI_EXAMPLE_COVERAGE
        .iter()
        .map(|coverage| coverage.schema_id)
        .collect::<BTreeSet<_>>();

    for case in SCHEMA_CASES {
        let schema = schema_doc(*case)?;
        let example_count = schema
            .get("examples")
            .and_then(Value::as_array)
            .map_or(0, Vec::len);
        if example_count > 1 && !covered_schema_ids.contains(case.id) {
            return Err(format!(
                "{} has {example_count} embedded examples but no MULTI_EXAMPLE_COVERAGE entry",
                case.id
            ));
        }
    }

    for coverage in MULTI_EXAMPLE_COVERAGE {
        let case = schema_case_by_id(coverage.schema_id)?;
        let schema = schema_doc(case)?;
        let examples = schema
            .get("examples")
            .and_then(Value::as_array)
            .ok_or_else(|| format!("{} missing examples array", coverage.schema_id))?;
        if examples.len() <= 1 {
            return Err(format!(
                "{} is listed in MULTI_EXAMPLE_COVERAGE but has only {} embedded example(s)",
                coverage.schema_id,
                examples.len()
            ));
        }

        let mut actual_values = BTreeSet::new();
        let mut serialized_examples = BTreeSet::new();
        for (index, example) in examples.iter().enumerate() {
            let context = format!("{} examples[{index}]", coverage.schema_id);
            actual_values.insert(
                string_field(example, coverage.discriminator_pointer, &context)?.to_owned(),
            );
            let serialized = serde_json::to_string(example)
                .map_err(|error| format!("serialize {context}: {error}"))?;
            if !serialized_examples.insert(serialized) {
                return Err(format!("{context} duplicates another embedded example"));
            }
        }

        let expected_values = coverage
            .expected_values
            .iter()
            .copied()
            .map(str::to_owned)
            .collect::<BTreeSet<_>>();
        if actual_values != expected_values {
            return Err(format!(
                "{} example coverage drifted for {}\nactual: {actual_values:?}\nexpected: {expected_values:?}",
                coverage.schema_id, coverage.discriminator_pointer
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
fn work_packet_agent_mail_fallback_semantics_are_contractual() -> TestResult {
    // bd-2z5ly.8: the agentMail block of ee.swarm.work_packet.v1 must
    // expose the richer status/recovery/parity/authority surface and
    // structured fallback actions, with redaction-safe semantics and
    // deterministic ordering. The degraded example must exercise it.
    let case = SCHEMA_CASES
        .iter()
        .copied()
        .find(|case| case.id == "ee.swarm.work_packet.v1")
        .ok_or_else(|| "ee.swarm.work_packet.v1 schema case missing".to_owned())?;
    let schema = schema_doc(case)?;

    let agent_mail = schema
        .pointer("/definitions/agentMail")
        .ok_or_else(|| "agentMail definition missing".to_owned())?;
    let property_names = |pointer: &str| -> Result<BTreeSet<String>, String> {
        agent_mail
            .pointer(pointer)
            .and_then(Value::as_object)
            .ok_or_else(|| format!("agentMail {pointer} object missing"))
            .map(|map| map.keys().cloned().collect())
    };
    let properties = property_names("/properties")?;
    for required_property in [
        "recoveryMode",
        "archiveIndexParity",
        "reservationAuthoritative",
        "inboxAuthoritative",
        "fallbackActions",
    ] {
        if !properties.contains(required_property) {
            return Err(format!(
                "ee.swarm.work_packet.v1 agentMail must expose {required_property}"
            ));
        }
    }

    let status_enum = agent_mail
        .pointer("/properties/status/enum")
        .and_then(Value::as_array)
        .ok_or_else(|| "agentMail.status enum missing".to_owned())?
        .iter()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect::<BTreeSet<_>>();
    for required_status in [
        "healthy",
        "degraded_read_only",
        "archive_ahead_of_sqlite",
        "inbox_unavailable",
        "reservation_unavailable",
        "outbox_only",
        "unreachable",
    ] {
        if !status_enum.contains(required_status) {
            return Err(format!(
                "ee.swarm.work_packet.v1 agentMail.status must include {required_status}"
            ));
        }
    }

    let action_enum = schema
        .pointer("/definitions/agentMailFallbackAction/properties/kind/enum")
        .and_then(Value::as_array)
        .ok_or_else(|| "agentMailFallbackAction.kind enum missing".to_owned())?
        .iter()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect::<BTreeSet<_>>();
    for required_kind in ["beads_comment", "retry_later", "switch_to_static_work"] {
        if !action_enum.contains(required_kind) {
            return Err(format!(
                "agentMailFallbackAction.kind must include {required_kind}"
            ));
        }
    }

    let examples = schema
        .get("examples")
        .and_then(Value::as_array)
        .ok_or_else(|| "ee.swarm.work_packet.v1 missing examples".to_owned())?;
    let degraded_example = examples
        .iter()
        .find(|example| {
            example
                .pointer("/observedStateClass")
                .and_then(Value::as_str)
                == Some("degraded_mail_rch_topology")
        })
        .ok_or_else(|| "ee.swarm.work_packet.v1 missing degraded example".to_owned())?;
    let example_agent_mail = degraded_example
        .pointer("/coordination/agentMail")
        .ok_or_else(|| "degraded example missing coordination.agentMail".to_owned())?;
    if example_agent_mail
        .pointer("/reservationAuthoritative")
        .and_then(Value::as_bool)
        != Some(false)
    {
        return Err(
            "degraded example must mark reservationAuthoritative=false so candidate-safety downgrades confidence".into(),
        );
    }
    if example_agent_mail
        .pointer("/inboxAuthoritative")
        .and_then(Value::as_bool)
        != Some(false)
    {
        return Err("degraded example must mark inboxAuthoritative=false".into());
    }
    let fallback_actions = example_agent_mail
        .pointer("/fallbackActions")
        .and_then(Value::as_array)
        .ok_or_else(|| "degraded example missing fallbackActions array".to_owned())?;
    if fallback_actions.is_empty() {
        return Err("degraded example must enumerate at least one fallback action".into());
    }
    let mut last_kind: Option<&str> = None;
    for (index, action) in fallback_actions.iter().enumerate() {
        let context = format!("fallbackActions[{index}]");
        let kind = string_field(action, "/kind", &context)?;
        if let Some(previous) = last_kind {
            if kind < previous {
                return Err(format!(
                    "fallbackActions must be sorted by kind; saw {previous} before {kind}"
                ));
            }
        }
        last_kind = Some(kind);
        let summary = string_field(action, "/summary", &context)?;
        for forbidden in ["body:", "raw_inbox", "From: ", "Subject: ", "Message-ID:"] {
            if summary.contains(forbidden) {
                return Err(format!(
                    "{context}.summary leaks mail-body marker {forbidden}"
                ));
            }
        }
        if !action
            .get("command")
            .is_some_and(|value| value.is_null() || value.is_string())
        {
            return Err(format!("{context}.command must be string or null"));
        }
        if !action
            .get("manualStep")
            .is_some_and(|value| value.is_null() || value.is_string())
        {
            return Err(format!("{context}.manualStep must be string or null"));
        }
    }

    let rendered = serde_json::to_string(example_agent_mail)
        .map_err(|error| format!("serialize degraded agentMail: {error}"))?;
    for forbidden in ["rawInbox", "From:", "Subject:", "Message-ID", "BEGIN PGP"] {
        if rendered.contains(forbidden) {
            return Err(format!(
                "degraded agentMail example must not leak mail-body marker {forbidden}"
            ));
        }
    }

    let fixture_path = repo_root()
        .join("tests")
        .join("fixtures")
        .join("swarm_work_packet")
        .join("agent_mail_degraded_read_only.json");
    let fixture = read_json(&fixture_path)?;
    if string_field(&fixture, "/schema", "agent_mail_degraded_read_only fixture")?
        != "ee.swarm.work_packet.v1"
    {
        return Err("agent_mail_degraded_read_only fixture schema drifted".into());
    }
    let fixture_agent_mail = fixture
        .pointer("/coordination/agentMail")
        .ok_or_else(|| "agent_mail_degraded_read_only fixture missing agentMail".to_owned())?;
    if fixture_agent_mail
        .pointer("/reservationAuthoritative")
        .and_then(Value::as_bool)
        != Some(false)
        || fixture_agent_mail
            .pointer("/inboxAuthoritative")
            .and_then(Value::as_bool)
            != Some(false)
    {
        return Err(
            "agent_mail_degraded_read_only fixture must mark reservation/inbox as non-authoritative"
                .into(),
        );
    }
    if string_field(
        fixture_agent_mail,
        "/archiveIndexParity",
        "agent_mail_degraded_read_only fixture agentMail",
    )? != "archive_ahead"
    {
        return Err(
            "agent_mail_degraded_read_only fixture must surface archive_ahead parity drift".into(),
        );
    }

    Ok(())
}

#[test]
fn work_packet_agent_mail_semantic_readiness_gate_is_contractual() -> TestResult {
    // bd-2z5ly.8.1: when Agent Mail responds with healthLevel=green but
    // semantic_readiness.status=fail (for example malformed SQLite storage),
    // the work-packet must surface the contradiction as an independent
    // signal and downgrade reservation/inbox authority even though the
    // transport itself was reachable. The schema must expose the new
    // semanticReadiness shape and status enum value; the fixture must
    // prove the green-transport contradiction cannot be misread as a
    // healthy coordination authority.
    let case = SCHEMA_CASES
        .iter()
        .copied()
        .find(|case| case.id == "ee.swarm.work_packet.v1")
        .ok_or_else(|| "ee.swarm.work_packet.v1 schema case missing".to_owned())?;
    let schema = schema_doc(case)?;

    let agent_mail = schema
        .pointer("/definitions/agentMail")
        .ok_or_else(|| "agentMail definition missing".to_owned())?;

    let semantic_readiness = agent_mail
        .pointer("/properties/semanticReadiness")
        .ok_or_else(|| {
            "ee.swarm.work_packet.v1 agentMail must expose semanticReadiness".to_owned()
        })?;

    let semantic_readiness_types = semantic_readiness
        .pointer("/type")
        .and_then(Value::as_array)
        .ok_or_else(|| "agentMail.semanticReadiness must allow null".to_owned())?
        .iter()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect::<BTreeSet<_>>();
    for required in ["object", "null"] {
        if !semantic_readiness_types.contains(required) {
            return Err(format!(
                "agentMail.semanticReadiness type must include {required}"
            ));
        }
    }

    let semantic_readiness_status_enum = semantic_readiness
        .pointer("/properties/status/enum")
        .and_then(Value::as_array)
        .ok_or_else(|| "agentMail.semanticReadiness.status enum missing".to_owned())?
        .iter()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect::<BTreeSet<_>>();
    for required in ["pass", "fail", "unknown"] {
        if !semantic_readiness_status_enum.contains(required) {
            return Err(format!(
                "agentMail.semanticReadiness.status enum must include {required}"
            ));
        }
    }

    let semantic_readiness_reason_enum = semantic_readiness
        .pointer("/properties/reason/enum")
        .and_then(Value::as_array)
        .ok_or_else(|| "agentMail.semanticReadiness.reason enum missing".to_owned())?
        .iter()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect::<BTreeSet<_>>();
    for required in [
        "malformed_sqlite",
        "archive_corruption",
        "index_rebuild_required",
        "permission_denied",
        "unknown",
    ] {
        if !semantic_readiness_reason_enum.contains(required) {
            return Err(format!(
                "agentMail.semanticReadiness.reason enum must include {required}"
            ));
        }
    }

    let status_enum = agent_mail
        .pointer("/properties/status/enum")
        .and_then(Value::as_array)
        .ok_or_else(|| "agentMail.status enum missing".to_owned())?
        .iter()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect::<BTreeSet<_>>();
    if !status_enum.contains("semantic_readiness_failed") {
        return Err(
            "agentMail.status enum must include semantic_readiness_failed for bd-2z5ly.8.1".into(),
        );
    }

    let fixture_path = repo_root()
        .join("tests")
        .join("fixtures")
        .join("swarm_work_packet")
        .join("agent_mail_semantic_readiness_failed.json");
    let fixture = read_json(&fixture_path)?;

    if string_field(
        &fixture,
        "/schema",
        "agent_mail_semantic_readiness_failed fixture",
    )? != "ee.swarm.work_packet.v1"
    {
        return Err("agent_mail_semantic_readiness_failed fixture schema drifted".into());
    }

    let fixture_agent_mail = fixture.pointer("/coordination/agentMail").ok_or_else(|| {
        "agent_mail_semantic_readiness_failed fixture missing coordination.agentMail".to_owned()
    })?;

    if string_field(
        fixture_agent_mail,
        "/status",
        "agent_mail_semantic_readiness_failed fixture agentMail",
    )? != "semantic_readiness_failed"
    {
        return Err(
            "agent_mail_semantic_readiness_failed fixture must set status=semantic_readiness_failed"
                .into(),
        );
    }

    if string_field(
        fixture_agent_mail,
        "/healthLevel",
        "agent_mail_semantic_readiness_failed fixture agentMail",
    )? != "green"
    {
        return Err(
            "agent_mail_semantic_readiness_failed fixture must demonstrate green transport so the gate proves green health does not imply coordination authority"
                .into(),
        );
    }

    if string_field(
        fixture_agent_mail,
        "/semanticReadiness/status",
        "agent_mail_semantic_readiness_failed fixture agentMail",
    )? != "fail"
    {
        return Err(
            "agent_mail_semantic_readiness_failed fixture must set semanticReadiness.status=fail"
                .into(),
        );
    }

    let fixture_reason = string_field(
        fixture_agent_mail,
        "/semanticReadiness/reason",
        "agent_mail_semantic_readiness_failed fixture agentMail",
    )?;
    if !semantic_readiness_reason_enum.contains(fixture_reason) {
        return Err(format!(
            "agent_mail_semantic_readiness_failed fixture reason {fixture_reason} is not in the schema reason enum"
        ));
    }

    if fixture_agent_mail
        .pointer("/reservationAuthoritative")
        .and_then(Value::as_bool)
        != Some(false)
        || fixture_agent_mail
            .pointer("/inboxAuthoritative")
            .and_then(Value::as_bool)
            != Some(false)
    {
        return Err(
            "agent_mail_semantic_readiness_failed fixture must mark reservation/inbox as non-authoritative even when healthLevel is green"
                .into(),
        );
    }

    let degraded_codes = fixture_agent_mail
        .pointer("/degradedCodes")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            "agent_mail_semantic_readiness_failed fixture missing degradedCodes".to_owned()
        })?
        .iter()
        .filter_map(Value::as_str)
        .collect::<BTreeSet<_>>();
    if !degraded_codes.contains("agent_mail_semantic_readiness_failed") {
        return Err(
            "agent_mail_semantic_readiness_failed fixture must include agent_mail_semantic_readiness_failed in degradedCodes"
                .into(),
        );
    }

    let recommended_safe = fixture
        .pointer("/recommendedAction/safeToClaim")
        .and_then(Value::as_bool)
        .ok_or_else(|| {
            "agent_mail_semantic_readiness_failed fixture missing recommendedAction.safeToClaim"
                .to_owned()
        })?;
    if recommended_safe {
        return Err(
            "agent_mail_semantic_readiness_failed fixture must not recommend safeToClaim=true when semantic readiness has failed"
                .into(),
        );
    }

    let fallback_actions = fixture_agent_mail
        .pointer("/fallbackActions")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            "agent_mail_semantic_readiness_failed fixture missing fallbackActions".to_owned()
        })?;
    if fallback_actions.is_empty() {
        return Err(
            "agent_mail_semantic_readiness_failed fixture must enumerate fallback actions".into(),
        );
    }
    let mut last_kind: Option<&str> = None;
    for (index, action) in fallback_actions.iter().enumerate() {
        let context = format!("fallbackActions[{index}]");
        let kind = string_field(action, "/kind", &context)?;
        if let Some(previous) = last_kind
            && kind < previous
        {
            return Err(format!(
                "fallbackActions must be sorted by kind; saw {previous} before {kind}"
            ));
        }
        last_kind = Some(kind);
    }

    let rendered = serde_json::to_string(fixture_agent_mail)
        .map_err(|error| format!("serialize semantic_readiness agentMail: {error}"))?;
    let lowered = rendered.to_ascii_lowercase();
    for forbidden in [
        "/private/",
        "/users/",
        "/var/",
        ".sqlite-shm",
        ".sqlite-wal",
        "page 283",
        "page_283",
        "btree page",
        "stack trace",
        "from: ",
        "subject: ",
        "message-id:",
        "begin pgp",
    ] {
        if lowered.contains(forbidden) {
            return Err(format!(
                "agent_mail_semantic_readiness_failed fixture must not leak raw path or error marker {forbidden}"
            ));
        }
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
