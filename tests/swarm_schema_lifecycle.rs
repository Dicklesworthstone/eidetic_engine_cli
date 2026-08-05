//! S6 swarm-schema lifecycle gates.
//!
//! These tests keep agent-facing swarm contracts honest: schema filenames are
//! canonical, examples are fixture-backed, docs exist, and availability markers
//! match Beads state.

#![allow(clippy::unwrap_used, clippy::expect_used)] // test code may unwrap/expect
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
        id: "ee.proof_broker.v1",
        file_name: "ee.proof_broker.v1.json",
        doc_path: "docs/swarm/proof_broker.md",
        tracking_bead: "bd-1n3x1.1",
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
        id: "ee.agent_mail.snapshot.v1",
        file_name: "ee.agent_mail.snapshot.v1.json",
        doc_path: "docs/swarm/coordination_snapshot.md",
        tracking_bead: "bd-1ur7d.1",
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
        id: "ee.source_run_evidence.v1",
        file_name: "ee.source_run_evidence.v1.json",
        doc_path: "docs/swarm/source_run_evidence.md",
        // bd-12v87.1 was renamed/incremented to .2 in the schema's
        // x-ee-status header when the repairSafety contract landed
        // (commit 3cf623f0). The schema is the source of truth for the
        // tracking bead id.
        tracking_bead: "bd-12v87.2",
        shipped: true,
    },
    SchemaCase {
        // Contract-first schema from bd-3w4pv.1: per-decision source-authority
        // aggregate consumed by claim gates and unsafe-claim planners. The
        // read-only collectors that emit it landed under bd-3w4pv.2 (closed):
        // the claim gate now carries sourceAuthoritySnapshot, and the schema
        // header already flipped shipped/available_in_build to true. This
        // catalog row lagged behind that flip (heal-in-passing alongside
        // bd-1n3x1.16).
        id: "ee.source_authority.snapshot.v1",
        file_name: "ee.source_authority.snapshot.v1.json",
        doc_path: "docs/swarm/source_authority_snapshot.md",
        tracking_bead: "bd-3w4pv.2",
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
        id: "ee.swarm.work_packet.claim_gate.v1",
        file_name: "ee.swarm.work_packet.claim_gate.v1.json",
        doc_path: "docs/swarm/work_packet.md",
        // bd-1tlcd.1 is closed and the CLI emits the read-only --claim-gate
        // surface, so the schema header carries shipped=true. The catalog row
        // previously lagged behind the schema flip (red-main drift fixed
        // alongside bd-3w4pv.1).
        tracking_bead: "bd-1tlcd.1",
        shipped: true,
    },
    SchemaCase {
        id: "ee.swarm.unsafe_claim_plan.v1",
        file_name: "ee.swarm.unsafe_claim_plan.v1.json",
        doc_path: "docs/swarm/unsafe_claim_plan.md",
        tracking_bead: "bd-1n3x1.16.1",
        shipped: true,
    },
    SchemaCase {
        id: "ee.swarm.repair_plan.v1",
        file_name: "ee.swarm.repair_plan.v1.json",
        doc_path: "docs/swarm/repair_plan.md",
        tracking_bead: "bd-22po3.1",
        shipped: true,
    },
    SchemaCase {
        // bd-1zb7k.14.1 is closed in the beads tracker (the synthetic
        // incident scenario schema and fixture catalog landed), so the
        // schema is now shipped and available in build.
        id: "ee.swarm_incident.v1",
        file_name: "ee.swarm_incident.v1.json",
        doc_path: "docs/swarm/swarm_incident_drills.md",
        tracking_bead: "bd-1zb7k.14.1",
        shipped: true,
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
        schema_id: "ee.proof_broker.v1",
        command: "planned ee proof admit --json",
        json_path: ".data.proofBroker",
        fixture_manifest_key: "ee.proof_broker.v1",
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
        schema_id: "ee.agent_mail.snapshot.v1",
        command: "scripts/agent_mail_snapshot.sh --json",
        json_path: ".",
        fixture_manifest_key: "ee.agent_mail.snapshot.v1",
    },
    DriftCase {
        schema_id: "ee.coordination_fallback_evidence.v1",
        command: "ee coordination evidence ingest --stdin --json",
        json_path: ".data.evidence",
        fixture_manifest_key: "ee.coordination_fallback_evidence.v1",
    },
    DriftCase {
        schema_id: "ee.source_run_evidence.v1",
        command: "planned source-run watchdog evidence",
        json_path: ".examples[\"ee.source_run_evidence.v1\"]",
        fixture_manifest_key: "ee.source_run_evidence.v1",
    },
    DriftCase {
        schema_id: "ee.source_authority.snapshot.v1",
        command: "planned source-authority snapshot collectors (bd-3w4pv.2)",
        json_path: ".examples[\"ee.source_authority.snapshot.v1\"]",
        fixture_manifest_key: "ee.source_authority.snapshot.v1",
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
        command: "ee support bundle --workspace . --redacted --out <dir> --json",
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
        schema_id: "ee.swarm.work_packet.claim_gate.v1",
        command: "ee swarm work-packet --claim-gate --json",
        json_path: ".data",
        fixture_manifest_key: "ee.swarm.work_packet.claim_gate.v1",
    },
    DriftCase {
        schema_id: "ee.swarm.unsafe_claim_plan.v1",
        command: "planned unsafe-claim planner over ee.swarm.work_packet.claim_gate.v1",
        json_path: ".examples[\"ee.swarm.unsafe_claim_plan.v1\"]",
        fixture_manifest_key: "ee.swarm.unsafe_claim_plan.v1",
    },
    DriftCase {
        schema_id: "ee.swarm.repair_plan.v1",
        command: "ee swarm repair-plan --workspace . --include-rch --candidate <bead> --json",
        json_path: ".data",
        fixture_manifest_key: "ee.swarm.repair_plan.v1",
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

fn string_array_at(value: &Value, pointer: &str, context: &str) -> Result<Vec<String>, String> {
    value
        .pointer(pointer)
        .and_then(Value::as_array)
        .ok_or_else(|| format!("{context} missing array {pointer}"))?
        .iter()
        .map(|item| {
            item.as_str()
                .map(ToOwned::to_owned)
                .ok_or_else(|| format!("{context} has non-string item in {pointer}"))
        })
        .collect()
}

fn source_state_for_case(case: &Value, source_kind: &str, context: &str) -> Result<String, String> {
    let sources = case
        .pointer("/sourceAuthoritySnapshot/sources")
        .and_then(Value::as_array)
        .ok_or_else(|| format!("{context} missing sourceAuthoritySnapshot.sources"))?;
    for source in sources {
        if string_field(source, "/sourceKind", context)? == source_kind {
            return Ok(string_field(source, "/state", context)?.to_owned());
        }
    }
    Err(format!(
        "{context} missing sourceAuthoritySnapshot sourceKind {source_kind}"
    ))
}

fn replay_case<'a>(cases: &'a BTreeMap<String, &'a Value>, id: &str) -> Result<&'a Value, String> {
    cases
        .get(id)
        .copied()
        .ok_or_else(|| format!("replay fixture missing case {id}"))
}

fn mutating_replay_command_shape(command: &str) -> Option<&'static str> {
    let normalized = command.split_whitespace().collect::<Vec<_>>().join(" ");
    [
        "br comments add",
        "br update",
        "br close",
        "br create",
        "br reopen",
        "br dep add",
        "br sync",
        "git add",
        "git commit",
        "git push",
        "git reset",
        "git restore",
        "git checkout",
        "git clean",
        "git rebase",
        "git stash",
    ]
    .into_iter()
    .find(|marker| {
        normalized == *marker
            || normalized
                .strip_prefix(marker)
                .is_some_and(|rest| rest.starts_with(' '))
    })
}

fn mutating_replay_action_id(action_id: &str) -> Option<&'static str> {
    [
        "bead_comment",
        "bead_claim",
        "bead_close",
        "bead_update",
        "bead_reopen",
        "git_",
        "agent_mail_send",
        "file_reservation",
    ]
    .into_iter()
    .find(|marker| action_id.starts_with(marker))
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
fn source_run_evidence_contract_covers_watchdog_policy() -> TestResult {
    let schema_case = schema_case_by_id("ee.source_run_evidence.v1")?;
    let schema = schema_doc(schema_case)?;
    let required = string_array_at(&schema, "/required", schema_case.id)?;
    let expected_required = [
        "schema",
        "runId",
        "capturedAt",
        "source",
        "command",
        "policy",
        "timing",
        "status",
        "exit",
        "output",
        "degraded",
        "recovery",
        "artifacts",
        "redaction",
        "provenanceHash",
        "producer",
    ];
    if required != expected_required {
        return Err(format!(
            "{} required field order drifted\nactual: {required:?}\nexpected: {expected_required:?}",
            schema_case.id
        ));
    }

    let status_values = string_array_at(&schema, "/properties/status/enum", schema_case.id)?;
    let expected_statuses = [
        "passed",
        "failed",
        "timed_out",
        "spawn_failed",
        "parse_failed",
        "stale_source",
        "malformed_store",
        "blocked",
    ];
    for expected in expected_statuses {
        if !status_values.iter().any(|value| value == expected) {
            return Err(format!("{} missing status {expected}", schema_case.id));
        }
    }

    let severity_values = string_array_at(
        &schema,
        "/properties/degraded/items/properties/severity/enum",
        schema_case.id,
    )?;
    let expected_severities = ["info", "low", "warning", "medium", "high", "critical"];
    if severity_values != expected_severities {
        return Err(format!(
            "{} degraded severity vocabulary drifted\nactual: {severity_values:?}\nexpected: {expected_severities:?}",
            schema_case.id
        ));
    }

    let recovery_required = string_array_at(
        &schema,
        "/properties/recovery/items/required",
        schema_case.id,
    )?;
    // Updated by 3cf623f0 (feat(swarm): require repairSafety metadata on every
    // incident recovery action): recoveryAction[] now mandates a structured
    // repairSafety block so agents can branch on machine-readable risk class
    // instead of parsing display command text.
    let expected_recovery_required = ["priority", "kind", "command", "message", "repairSafety"];
    if recovery_required != expected_recovery_required {
        return Err(format!(
            "{} recovery[] shape drifted\nactual: {recovery_required:?}\nexpected: {expected_recovery_required:?}",
            schema_case.id
        ));
    }

    if bool_field(
        &schema,
        "/properties/redaction/properties/rawBodiesIncluded/const",
        schema_case.id,
    )? {
        return Err(format!(
            "{} must not allow raw mail/body content",
            schema_case.id
        ));
    }
    if bool_field(
        &schema,
        "/properties/redaction/properties/rawEnvIncluded/const",
        schema_case.id,
    )? {
        return Err(format!(
            "{} must not allow raw environment dumps",
            schema_case.id
        ));
    }
    if bool_field(
        &schema,
        "/properties/exit/properties/killedPeerProcesses/const",
        schema_case.id,
    )? {
        return Err(format!(
            "{} must not allow killing peer processes",
            schema_case.id
        ));
    }

    let examples = fixture_examples()?;
    let example = examples
        .get(schema_case.id)
        .ok_or_else(|| format!("fixture manifest missing {}", schema_case.id))?;
    let argv_redaction = string_field(
        example,
        "/command/argvRedaction",
        "source run fixture example",
    )?;
    if argv_redaction != "literal_safe" {
        return Err(format!(
            "source run fixture argvRedaction must be literal_safe, got {argv_redaction}"
        ));
    }
    let on_failure = string_field(example, "/policy/onFailure", "source run fixture example")?;
    if on_failure != "continue_degraded" {
        return Err(format!(
            "source run fixture onFailure must be continue_degraded, got {on_failure}"
        ));
    }
    let example_status = string_field(example, "/status", "source run fixture example")?;
    if example_status != "malformed_store" {
        return Err(format!(
            "source run fixture status must be malformed_store, got {example_status}"
        ));
    }
    let provenance_hash = string_field(example, "/provenanceHash", "source run fixture example")?;
    if !provenance_hash.starts_with("blake3:") {
        return Err(format!(
            "source run fixture provenanceHash must be deterministic blake3, got {provenance_hash}"
        ));
    }
    let command_hash = string_field(
        example,
        "/command/commandHash",
        "source run fixture example",
    )?;
    let argv_hash = string_field(
        example,
        "/command/normalizedArgvHash",
        "source run fixture example",
    )?;
    if !command_hash.starts_with("blake3:") || !argv_hash.starts_with("blake3:") {
        return Err("source run fixture command hashes must be deterministic blake3".into());
    }
    if bool_field(
        example,
        "/redaction/rawBodiesIncluded",
        "source run fixture example",
    )? || bool_field(
        example,
        "/redaction/rawEnvIncluded",
        "source run fixture example",
    )? {
        return Err("source run fixture must not include raw bodies or env dumps".into());
    }

    Ok(())
}

#[test]
fn source_authority_snapshot_contract_covers_source_state_taxonomy() -> TestResult {
    let schema_case = schema_case_by_id("ee.source_authority.snapshot.v1")?;
    let schema = schema_doc(schema_case)?;

    let required = string_array_at(&schema, "/required", schema_case.id)?;
    let expected_required = [
        "schema",
        "snapshotId",
        "generatedAt",
        "workspace",
        "redactionStatus",
        "ordering",
        "sources",
        "candidateEvidence",
        "contradictions",
        "overall",
        "degraded",
        "provenanceHash",
    ];
    if required != expected_required {
        return Err(format!(
            "{} required field order drifted\nactual: {required:?}\nexpected: {expected_required:?}",
            schema_case.id
        ));
    }

    let source_states = string_array_at(&schema, "/definitions/sourceState/enum", schema_case.id)?;
    let expected_states = [
        "ready",
        "degraded_read_only",
        "unavailable",
        "timed_out",
        "stale_fallback",
        "corrupt_recovery",
        "contradicted",
    ];
    if source_states != expected_states {
        return Err(format!(
            "{} source-state taxonomy drifted\nactual: {source_states:?}\nexpected: {expected_states:?}",
            schema_case.id
        ));
    }

    let source_kinds = string_array_at(&schema, "/definitions/sourceKind/enum", schema_case.id)?;
    let expected_kinds = [
        "actionable_queue",
        "agent_mail",
        "beads",
        "bv",
        "git",
        "host_profile",
        "installed_binary",
        "memory_drift",
        "rch",
        "support_bundle",
        "toolchain",
        "workspace_hygiene",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect::<Vec<_>>();
    if source_kinds != expected_kinds {
        return Err(format!(
            "{} source-kind catalog drifted\nactual: {source_kinds:?}\nexpected: {expected_kinds:?}",
            schema_case.id
        ));
    }

    let lookup_outcomes = string_array_at(
        &schema,
        "/definitions/candidateEvidence/properties/lookupOutcome/enum",
        schema_case.id,
    )?;
    let expected_outcomes = [
        "candidate_present",
        "candidate_absent_confirmed",
        "candidate_known_non_actionable",
        "candidate_lookup_unavailable",
        "candidate_lookup_timed_out",
        "candidate_stale_fallback_only",
        "candidate_contradicted",
    ];
    if lookup_outcomes != expected_outcomes {
        return Err(format!(
            "{} candidate lookup outcomes drifted\nactual: {lookup_outcomes:?}\nexpected: {expected_outcomes:?}",
            schema_case.id
        ));
    }

    let lookup_description = string_field(
        &schema,
        "/definitions/candidateEvidence/properties/lookupOutcome/description",
        schema_case.id,
    )?;
    let claim_gate_case = schema_case_by_id("ee.swarm.work_packet.claim_gate.v1")?;
    let claim_gate_schema = schema_doc(claim_gate_case)?;
    let compact_lookup_pointer = "/definitions/sourceAuthoritySnapshot/properties/candidateEvidence/oneOf/1/properties/lookupOutcome";
    let compact_lookup_outcomes = string_array_at(
        &claim_gate_schema,
        &format!("{compact_lookup_pointer}/enum"),
        claim_gate_case.id,
    )?;
    if compact_lookup_outcomes != lookup_outcomes {
        return Err(format!(
            "{} compact candidate lookup outcomes drifted from {}\ncompact: {compact_lookup_outcomes:?}\ncanonical: {lookup_outcomes:?}",
            claim_gate_case.id, schema_case.id
        ));
    }
    let compact_lookup_description = string_field(
        &claim_gate_schema,
        &format!("{compact_lookup_pointer}/description"),
        claim_gate_case.id,
    )?;
    if compact_lookup_description != lookup_description {
        return Err(format!(
            "{} compact candidate lookup description drifted from {}",
            claim_gate_case.id, schema_case.id
        ));
    }
    let gate_verdicts = string_array_at(
        &claim_gate_schema,
        "/definitions/gateVerdict/enum",
        claim_gate_case.id,
    )?;
    if gate_verdicts
        .iter()
        .any(|verdict| verdict == "candidate_known_non_actionable")
    {
        return Err("candidate_known_non_actionable is lookup evidence, not a gate verdict".into());
    }

    if string_field(&schema, "/properties/redactionStatus/const", schema_case.id)?
        != "paths_counts_subjects_only_no_content"
    {
        return Err(format!(
            "{} redactionStatus must be pinned to paths_counts_subjects_only_no_content",
            schema_case.id
        ));
    }

    Ok(())
}

#[test]
fn source_authority_fixtures_cover_taxonomy_and_redaction() -> TestResult {
    let fixture_dir = repo_root()
        .join("tests")
        .join("fixtures")
        .join("source_authority");

    let all_states = read_json(&fixture_dir.join("all_source_states.json"))?;
    let candidate_timeout = read_json(&fixture_dir.join("candidate_beads_timeout.json"))?;
    let known_non_actionable = read_json(&fixture_dir.join("known_non_actionable_candidate.json"))?;
    let redaction_proof = read_json(&fixture_dir.join("redaction_proof.json"))?;

    // 1. The state-coverage fixture must exercise every source state.
    let expected_states = [
        "ready",
        "degraded_read_only",
        "unavailable",
        "timed_out",
        "stale_fallback",
        "corrupt_recovery",
        "contradicted",
    ];
    let sources = all_states
        .pointer("/sources")
        .and_then(Value::as_array)
        .ok_or("all_source_states fixture missing sources array")?;
    let mut seen_states = BTreeSet::new();
    let mut kinds_in_order = Vec::new();
    for source in sources {
        seen_states.insert(string_field(source, "/state", "all_source_states source")?.to_owned());
        kinds_in_order
            .push(string_field(source, "/sourceKind", "all_source_states source")?.to_owned());
    }
    for state in expected_states {
        if !seen_states.contains(state) {
            return Err(format!(
                "all_source_states fixture missing source state {state}"
            ));
        }
    }

    // 2. Sources must be sorted by sourceKind ascending byte order.
    let mut sorted_kinds = kinds_in_order.clone();
    sorted_kinds.sort();
    if kinds_in_order != sorted_kinds {
        return Err(format!(
            "all_source_states sources must be sorted by sourceKind\nactual: {kinds_in_order:?}"
        ));
    }
    let expected_kinds = [
        "actionable_queue",
        "agent_mail",
        "beads",
        "bv",
        "git",
        "host_profile",
        "installed_binary",
        "memory_drift",
        "rch",
        "support_bundle",
        "toolchain",
        "workspace_hygiene",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect::<Vec<_>>();
    if kinds_in_order != expected_kinds {
        return Err(format!(
            "all_source_states fixture must cover every source kind\nactual: {kinds_in_order:?}\nexpected: {expected_kinds:?}"
        ));
    }
    let actionable_queue = sources
        .iter()
        .find(|source| {
            string_field(source, "/sourceKind", "all_source_states source").ok()
                == Some("actionable_queue")
        })
        .ok_or("all_source_states fixture missing actionable_queue source")?;
    if actionable_queue.pointer("/actionableQueue").is_none() {
        return Err("actionable_queue source must carry actionableQueue extension".into());
    }

    // 3. The candidate fixture pins timeout-vs-absence: present in stale-safe
    //    Beads, live lookup timed out, gate fails closed.
    if string_field(
        &candidate_timeout,
        "/candidateEvidence/lookupOutcome",
        "candidate_beads_timeout fixture",
    )? != "candidate_lookup_timed_out"
    {
        return Err(
            "candidate_beads_timeout fixture must report candidate_lookup_timed_out, never candidate_absent_confirmed"
                .into(),
        );
    }
    if !bool_field(
        &candidate_timeout,
        "/candidateEvidence/staleFallbackPresence/present",
        "candidate_beads_timeout fixture",
    )? {
        return Err(
            "candidate_beads_timeout fixture must record candidate presence in the stale-safe fallback".into(),
        );
    }
    if !bool_field(
        &candidate_timeout,
        "/overall/failClosed",
        "candidate_beads_timeout fixture",
    )? {
        return Err("candidate_beads_timeout fixture must fail closed".into());
    }

    // 4. The non-actionable fixture pins existence-vs-actionability:
    //    Beads knows the id, the actionable queue excludes it, and the
    //    contract does not collapse that into true absence.
    if string_field(
        &known_non_actionable,
        "/candidateEvidence/lookupOutcome",
        "known_non_actionable_candidate fixture",
    )? != "candidate_known_non_actionable"
    {
        return Err(
            "known_non_actionable_candidate fixture must report candidate_known_non_actionable"
                .into(),
        );
    }
    if string_array_at(
        &known_non_actionable,
        "/candidateEvidence/presentIn",
        "known_non_actionable_candidate fixture",
    )? != ["beads".to_owned()]
    {
        return Err("known_non_actionable_candidate fixture must prove Beads presence".into());
    }
    if string_array_at(
        &known_non_actionable,
        "/candidateEvidence/absentFrom",
        "known_non_actionable_candidate fixture",
    )? != ["actionable_queue".to_owned()]
    {
        return Err(
            "known_non_actionable_candidate fixture must prove actionable queue exclusion".into(),
        );
    }
    if !bool_field(
        &known_non_actionable,
        "/overall/failClosed",
        "known_non_actionable_candidate fixture",
    )? {
        return Err("known_non_actionable_candidate fixture must fail closed".into());
    }

    // 5. Redaction posture: serialized fixtures must not leak host-private
    //    absolute paths, raw mail/memory bodies, or secret-shaped argv.
    for (name, fixture) in [
        ("all_source_states", &all_states),
        ("candidate_beads_timeout", &candidate_timeout),
        ("known_non_actionable_candidate", &known_non_actionable),
        ("redaction_proof", &redaction_proof),
    ] {
        let serialized = fixture.to_string();
        for forbidden in [
            "/Users/",
            "/home/",
            "/private/",
            "body_md",
            "bodyMd",
            "rawBody",
            "sk-ant-",
            "AKIA",
            "-----BEGIN",
        ] {
            if serialized.contains(forbidden) {
                return Err(format!(
                    "source_authority fixture {name} leaks forbidden content {forbidden}"
                ));
            }
        }
        if string_field(fixture, "/redactionStatus", name)?
            != "paths_counts_subjects_only_no_content"
        {
            return Err(format!("{name} fixture redactionStatus drifted"));
        }
    }

    Ok(())
}

#[test]
fn source_authority_support_bundle_handoff_summary_is_redacted() -> TestResult {
    let fixture = read_json(
        &repo_root()
            .join("tests")
            .join("fixtures")
            .join("source_authority")
            .join("support_bundle_handoff_summary.json"),
    )?;
    if string_field(&fixture, "/schema", "source-authority handoff summary")?
        != "ee.source_authority.handoff_summary.v1"
    {
        return Err("source-authority handoff summary schema drifted".into());
    }
    if string_field(
        &fixture,
        "/snapshotRef/schema",
        "source-authority handoff summary",
    )? != "ee.source_authority.snapshot.v1"
    {
        return Err("source-authority handoff summary must reference snapshot schema".into());
    }
    if string_field(
        &fixture,
        "/redactionStatus",
        "source-authority handoff summary",
    )? != "paths_counts_subjects_only_no_content"
    {
        return Err("source-authority handoff summary redactionStatus drifted".into());
    }

    let generated_for = string_array_at(
        &fixture,
        "/generatedFor",
        "source-authority handoff summary",
    )?;
    for expected in ["support_bundle", "handoff_capsule", "agent_mail"] {
        if !generated_for.iter().any(|item| item == expected) {
            return Err(format!(
                "source-authority handoff summary missing generatedFor={expected}"
            ));
        }
    }

    let source_summaries = fixture
        .pointer("/sourceSummaries")
        .and_then(Value::as_array)
        .ok_or_else(|| "source-authority handoff summary missing sourceSummaries".to_owned())?;
    let source_kinds = source_summaries
        .iter()
        .filter_map(|source| source.pointer("/sourceKind").and_then(Value::as_str))
        .collect::<BTreeSet<_>>();
    for expected in ["agent_mail", "beads", "bv", "memory_drift", "rch"] {
        if !source_kinds.contains(expected) {
            return Err(format!(
                "source-authority handoff summary missing sourceKind {expected}"
            ));
        }
    }
    if !source_summaries.iter().any(|source| {
        source.pointer("/sourceKind").and_then(Value::as_str) == Some("agent_mail")
            && source.pointer("/state").and_then(Value::as_str) == Some("corrupt_recovery")
    }) {
        return Err("summary must preserve Agent Mail corrupt-recovery posture".into());
    }
    if !source_summaries.iter().any(|source| {
        source.pointer("/sourceKind").and_then(Value::as_str) == Some("bv")
            && source.pointer("/timeoutClass").and_then(Value::as_str)
                == Some("robot_next_no_output")
    }) {
        return Err("summary must preserve BV timeout/no-output posture".into());
    }

    if string_field(
        &fixture,
        "/candidateEvidence/lookupOutcome",
        "source-authority handoff summary",
    )? != "candidate_lookup_timed_out"
    {
        return Err("summary must not collapse timed-out lookup into absence".into());
    }
    let blocker_codes = fixture
        .pointer("/blockerGroups")
        .and_then(Value::as_array)
        .ok_or_else(|| "summary missing blockerGroups".to_owned())?
        .iter()
        .filter_map(|blocker| blocker.pointer("/code").and_then(Value::as_str))
        .collect::<BTreeSet<_>>();
    if !blocker_codes.contains("claim_gate_degraded_authority") {
        return Err("summary missing claim_gate_degraded_authority blocker".into());
    }

    let commands = fixture
        .pointer("/nextNonMutatingCommands")
        .and_then(Value::as_array)
        .ok_or_else(|| "summary missing nextNonMutatingCommands".to_owned())?;
    let command_ids = commands
        .iter()
        .filter_map(|command| command.pointer("/commandId").and_then(Value::as_str))
        .collect::<BTreeSet<_>>();
    for expected in [
        "refresh_actionable_queue",
        "rerun_claim_gate",
        "inspect_swarm_brief",
    ] {
        if !command_ids.contains(expected) {
            return Err(format!("summary missing command {expected}"));
        }
    }
    for command in commands {
        if string_field(command, "/safety", "source-authority handoff command")?
            != "read_only_probe"
        {
            return Err("summary follow-up commands must be read-only probes".into());
        }
        let template = string_field(
            command,
            "/commandTemplate",
            "source-authority handoff command",
        )?;
        for forbidden in ["br update", "br close", "git commit", "send_message"] {
            if template.contains(forbidden) {
                return Err(format!(
                    "summary follow-up command must not mutate state: {forbidden}"
                ));
            }
        }
    }

    if fixture
        .pointer("/agentMailTemplate/ackRequired")
        .and_then(Value::as_bool)
        != Some(false)
    {
        return Err("Agent Mail handoff template should be FYI by default".into());
    }
    let body = string_field(
        &fixture,
        "/agentMailTemplate/bodyMarkdown",
        "source-authority handoff summary",
    )?;
    for expected in [
        "Agent Mail corrupt-recovery",
        "Beads live lookup timed out",
        "BV robot-next no-output",
        "RCH telemetry gap",
        "No claim or source edit was performed",
    ] {
        if !body.contains(expected) {
            return Err(format!("Agent Mail template missing phrase {expected}"));
        }
    }

    for field in [
        "/privacy/rawMailBodiesIncluded",
        "/privacy/rawMemoryBodiesIncluded",
        "/privacy/rawSourceSnippetsIncluded",
        "/privacy/rawCommandOutputIncluded",
        "/privacy/secretsIncluded",
        "/privacy/fullHostPathsIncluded",
    ] {
        if fixture.pointer(field).and_then(Value::as_bool) != Some(false) {
            return Err(format!("summary privacy field {field} must be false"));
        }
    }

    let serialized =
        serde_json::to_string(&fixture).map_err(|error| format!("serialize fixture: {error}"))?;
    for forbidden in [
        "/Users/",
        "/home/",
        "/private/",
        "body_md",
        "rawBody",
        "From:",
        "Subject:",
        "Message-ID:",
        "sk-ant-",
        "AKIA",
        "-----BEGIN",
    ] {
        if serialized.contains(forbidden) {
            return Err(format!(
                "source-authority handoff summary leaked forbidden content {forbidden}"
            ));
        }
    }

    Ok(())
}

#[test]
fn source_authority_bv_robot_next_no_output_fixture_is_bounded() -> TestResult {
    let fixture = read_json(
        &repo_root()
            .join("tests")
            .join("fixtures")
            .join("source_authority")
            .join("bv_robot_next_no_output_large_tracker.json"),
    )?;
    if string_field(&fixture, "/schema", "bv robot-next no-output fixture")?
        != "ee.source_authority.snapshot.v1"
    {
        return Err("bv robot-next no-output fixture schema drifted".into());
    }
    if string_field(
        &fixture,
        "/overall/verdict",
        "bv robot-next no-output fixture",
    )? != "fail_closed_timeout"
        || !bool_field(
            &fixture,
            "/overall/failClosed",
            "bv robot-next no-output fixture",
        )?
    {
        return Err("bv robot-next no-output fixture must fail closed on timeout".into());
    }

    let sources = fixture
        .pointer("/sources")
        .and_then(Value::as_array)
        .ok_or_else(|| "bv robot-next no-output fixture missing sources".to_owned())?;
    let bv_source = sources
        .iter()
        .find(|source| source.pointer("/sourceKind").and_then(Value::as_str) == Some("bv"))
        .ok_or_else(|| "bv robot-next no-output fixture missing BV source".to_owned())?;
    if string_field(bv_source, "/state", "bv source")? != "timed_out" {
        return Err("bv source must preserve timed_out instead of candidate absence".into());
    }
    if string_field(bv_source, "/exit/exitClass", "bv source")? != "timeout"
        || bv_source.pointer("/exit/exitCode").and_then(Value::as_i64) != Some(124)
        || !bool_field(bv_source, "/budget/timedOut", "bv source")?
    {
        return Err("bv source must pin timeout exit and budget posture".into());
    }
    if !bool_field(bv_source, "/partialData/available", "bv source")? {
        return Err("bv source must retain bounded partial graph metadata".into());
    }

    let dropped = string_array_at(bv_source, "/partialData/droppedSections", "bv source")?;
    for phase in ["cycles", "robot_next", "claim_command"] {
        if !dropped.iter().any(|item| item == phase) {
            return Err(format!("bv source droppedSections missing {phase}"));
        }
    }
    let skipped = string_array_at(bv_source, "/bvRobotNext/skippedPhases", "bv source")?;
    for phase in ["cycles", "robot_next", "claim_command"] {
        if !skipped.iter().any(|item| item == phase) {
            return Err(format!("bvRobotNext skippedPhases missing {phase}"));
        }
    }
    if bv_source
        .pointer("/bvRobotNext/graphNodeCount")
        .and_then(Value::as_i64)
        .is_none_or(|count| count < 4000)
        || bv_source
            .pointer("/bvRobotNext/graphEdgeCount")
            .and_then(Value::as_i64)
            .is_none_or(|count| count < 6000)
    {
        return Err("bvRobotNext must pin sanitized large-graph shape".into());
    }
    if string_field(bv_source, "/bvRobotNext/recommendationState", "bv source")? != "no_output" {
        return Err("bvRobotNext must distinguish no_output from empty_queue".into());
    }
    if !bool_field(
        bv_source,
        "/bvRobotNext/claimCommandSuppressed",
        "bv source",
    )? {
        return Err("degraded BV posture must suppress claim commands".into());
    }
    if string_field(bv_source, "/bvRobotNext/fallbackCommand", "bv source")?
        != "bv --robot-insights --format json"
    {
        return Err("bvRobotNext fallback command drifted".into());
    }

    let serialized = fixture.to_string();
    let lowered = serialized.to_ascii_lowercase();
    for forbidden in [
        "/private/",
        "/users/",
        "/home/",
        "raw stdout",
        "raw stderr",
        "safe_to_claim",
        "\"safetoclaim\"",
        "\"claimcommandaction\"",
        "claimable",
    ] {
        if lowered.contains(forbidden) {
            return Err(format!(
                "bv robot-next no-output fixture leaked forbidden detail {forbidden}"
            ));
        }
    }

    Ok(())
}

#[test]
fn source_authority_replay_fixtures_pin_claim_gate_projections() -> TestResult {
    let fixture_path = repo_root()
        .join("tests")
        .join("fixtures")
        .join("source_authority")
        .join("replay_claim_gate_cases.json");
    let fixture = read_json(&fixture_path)?;
    if string_field(&fixture, "/schema", "replay_claim_gate_cases fixture")?
        != "ee.source_authority.replay_claim_gate_cases.v1"
    {
        return Err("replay_claim_gate_cases fixture schema drifted".into());
    }

    let cases = fixture
        .pointer("/cases")
        .and_then(Value::as_array)
        .ok_or("replay_claim_gate_cases fixture missing cases array")?;
    let mut case_ids = Vec::new();
    let mut cases_by_id = BTreeMap::new();
    for case in cases {
        let id = string_field(case, "/id", "source-authority replay case")?.to_owned();
        case_ids.push(id.clone());
        cases_by_id.insert(id, case);

        if string_field(
            case,
            "/sourceAuthoritySnapshot/schema",
            "source-authority replay snapshot",
        )? != "ee.source_authority.snapshot.v1"
        {
            return Err("replay case must embed source-authority snapshot schema".into());
        }
        if string_field(
            case,
            "/claimGateProjection/schema",
            "source-authority replay claim-gate projection",
        )? != "ee.swarm.work_packet.claim_gate.v1"
        {
            return Err("replay case must embed claim-gate projection schema".into());
        }
        if bool_field(
            case,
            "/claimGateProjection/safeToClaim",
            &case_ids.last().unwrap(),
        )? {
            return Err(format!(
                "{} must never be safe to claim",
                case_ids.last().unwrap()
            ));
        }
        if !case
            .pointer("/claimGateProjection/claimCommandAction")
            .is_some_and(Value::is_null)
        {
            return Err(format!(
                "{} must keep claimCommandAction null",
                case_ids.last().unwrap()
            ));
        }
        if !bool_field(
            case,
            "/sourceAuthoritySnapshot/overall/failClosed",
            &case_ids.last().unwrap(),
        )? {
            return Err(format!(
                "{} must preserve fail-closed source authority",
                case_ids.last().unwrap()
            ));
        }
        for action in case
            .pointer("/claimGateProjection/nextCommandActions")
            .and_then(Value::as_array)
            .ok_or_else(|| format!("{} missing nextCommandActions", case_ids.last().unwrap()))?
        {
            let context = case_ids.last().unwrap();
            if bool_field(action, "/mutatesState", context)? {
                return Err(format!(
                    "{} emitted a mutating next command action",
                    context
                ));
            }
            let action_id = string_field(action, "/commandId", context)?;
            if let Some(marker) = mutating_replay_action_id(action_id) {
                return Err(format!(
                    "{context} labels mutating action id {action_id} as read-only via {marker}"
                ));
            }
        }

        for source in case
            .pointer("/sourceAuthoritySnapshot/sources")
            .and_then(Value::as_array)
            .ok_or_else(|| {
                format!(
                    "{} missing sourceAuthoritySnapshot.sources",
                    case_ids.last().unwrap()
                )
            })?
        {
            let context = case_ids.last().unwrap();
            if source.pointer("/repair/safety").and_then(Value::as_str) == Some("read_only_probe")
                && let Some(command) = source.pointer("/repair/command").and_then(Value::as_str)
                && let Some(marker) = mutating_replay_command_shape(command)
            {
                return Err(format!(
                    "{context} labels mutating repair command {command:?} as read_only_probe via {marker}"
                ));
            }
        }

        for degraded in case
            .pointer("/sourceAuthoritySnapshot/degraded")
            .and_then(Value::as_array)
            .ok_or_else(|| {
                format!(
                    "{} missing sourceAuthoritySnapshot.degraded",
                    case_ids.last().unwrap()
                )
            })?
        {
            let context = case_ids.last().unwrap();
            if let Some(repair) = degraded.pointer("/repair").and_then(Value::as_str)
                && let Some(marker) = mutating_replay_command_shape(repair)
            {
                return Err(format!(
                    "{context} degraded repair {repair:?} smuggles mutating command via {marker}"
                ));
            }
        }

        let serialized = case.to_string();
        for forbidden in [
            "/Users/",
            "/home/",
            "/private/",
            "body_md",
            "rawBody",
            "AKIA",
        ] {
            if serialized.contains(forbidden) {
                return Err(format!(
                    "{} leaks forbidden replay content {forbidden}",
                    case_ids.last().unwrap()
                ));
            }
        }
    }
    let mut sorted_case_ids = case_ids.clone();
    sorted_case_ids.sort();
    if case_ids != sorted_case_ids {
        return Err(format!(
            "replay_claim_gate_cases must be sorted by id\nactual: {case_ids:?}"
        ));
    }

    let timeout = replay_case(&cases_by_id, "candidate_present_but_actionable_timeout")?;
    if string_field(
        timeout,
        "/sourceAuthoritySnapshot/candidateEvidence/lookupOutcome",
        "candidate_present_but_actionable_timeout",
    )? != "candidate_lookup_timed_out"
    {
        return Err("timeout replay must not collapse timed-out lookup into absence".into());
    }
    if !bool_field(
        timeout,
        "/sourceAuthoritySnapshot/candidateEvidence/staleFallbackPresence/present",
        "candidate_present_but_actionable_timeout",
    )? {
        return Err("timeout replay must preserve stale fallback candidate presence".into());
    }
    let timeout_reasons = string_array_at(
        timeout,
        "/claimGateProjection/unsafeReasons",
        "candidate_present_but_actionable_timeout",
    )?;
    if timeout_reasons.contains(&"candidate_not_found:bd-27dae".to_owned()) {
        return Err("timeout replay must not emit candidate_not_found".into());
    }
    if !timeout_reasons.contains(
        &"candidate_unresolved_due_to_tracker_state:external_changes_pending_import:bd-27dae"
            .to_owned(),
    ) {
        return Err("timeout replay must preserve tracker-state unresolved reason".into());
    }

    let corrupt_mail = replay_case(&cases_by_id, "agent_mail_green_but_recovery_corrupt")?;
    if source_state_for_case(
        corrupt_mail,
        "agent_mail",
        "agent_mail_green_but_recovery_corrupt",
    )? != "corrupt_recovery"
    {
        return Err("Agent Mail corrupt replay must use corrupt_recovery source state".into());
    }
    if bool_field(
        corrupt_mail,
        "/claimGateProjection/sourceAuthority/reservationAuthoritative",
        "agent_mail_green_but_recovery_corrupt",
    )? || bool_field(
        corrupt_mail,
        "/claimGateProjection/sourceAuthority/inboxAuthoritative",
        "agent_mail_green_but_recovery_corrupt",
    )? {
        return Err(
            "Agent Mail corrupt replay must make inbox/reservation non-authoritative".into(),
        );
    }
    let corrupt_codes = string_array_at(
        corrupt_mail,
        "/claimGateProjection/degradedCodes",
        "agent_mail_green_but_recovery_corrupt",
    )?;
    if !corrupt_codes.contains(&"agent_mail_unavailable".to_owned()) {
        return Err("Agent Mail corrupt replay must preserve agent_mail_unavailable".into());
    }

    let memory_drift = replay_case(&cases_by_id, "memory_drift_read_snapshot_contention")?;
    if source_state_for_case(
        memory_drift,
        "memory_drift",
        "memory_drift_read_snapshot_contention",
    )? != "degraded_read_only"
    {
        return Err("memory-drift lock replay must stay degraded_read_only".into());
    }
    if string_field(
        memory_drift,
        "/claimGateProjection/verdict",
        "memory_drift_read_snapshot_contention",
    )? != "external_state_required"
    {
        return Err("memory-drift lock replay must fail as external_state_required".into());
    }
    let drift_codes = string_array_at(
        memory_drift,
        "/claimGateProjection/degradedCodes",
        "memory_drift_read_snapshot_contention",
    )?;
    if !drift_codes.contains(&"memory_drift_lock_contention".to_owned()) {
        return Err("memory-drift lock replay must preserve the lock-contention code".into());
    }

    let rch_pressure = replay_case(&cases_by_id, "rch_pressure_telemetry_unavailable")?;
    if source_state_for_case(rch_pressure, "rch", "rch_pressure_telemetry_unavailable")?
        != "degraded_read_only"
    {
        return Err("RCH pressure replay must be degraded_read_only, not source failure".into());
    }
    if string_field(
        rch_pressure,
        "/claimGateProjection/verdict",
        "rch_pressure_telemetry_unavailable",
    )? != "blocked_by_verification"
    {
        return Err("RCH pressure replay must block proof-required claims".into());
    }
    if bool_field(
        rch_pressure,
        "/claimGateProjection/sourceAuthority/rchSafeToLaunchCargoVerification",
        "rch_pressure_telemetry_unavailable",
    )? {
        return Err("RCH pressure replay must not authorize Cargo verification launch".into());
    }
    let rch_reasons = string_array_at(
        rch_pressure,
        "/claimGateProjection/unsafeReasons",
        "rch_pressure_telemetry_unavailable",
    )?;
    if !rch_reasons.contains(&"rch_pressure_telemetry_unavailable".to_owned())
        || !rch_reasons.contains(&"no_rust_source_verdict_reached".to_owned())
    {
        return Err(
            "RCH pressure replay must preserve proof gap and no-source-verdict reasons".into(),
        );
    }

    Ok(())
}

#[test]
fn unsafe_claim_plan_contract_pins_reason_taxonomy_and_non_mutation() -> TestResult {
    let schema_case = schema_case_by_id("ee.swarm.unsafe_claim_plan.v1")?;
    let schema = schema_doc(schema_case)?;

    let required = string_array_at(&schema, "/required", schema_case.id)?;
    let expected_required = [
        "schema",
        "planId",
        "generatedAt",
        "workspace",
        "redactionStatus",
        "ordering",
        "sourceGate",
        "reasonGroups",
        "candidatePlans",
        "plannerActions",
        "nextCommandActions",
        "evidenceSources",
        "nonMutationPolicy",
        "degraded",
        "provenanceHash",
    ];
    if required != expected_required {
        return Err(format!(
            "{} required field order drifted\nactual: {required:?}\nexpected: {expected_required:?}",
            schema_case.id
        ));
    }

    let source_gate_required =
        string_array_at(&schema, "/definitions/sourceGate/required", schema_case.id)?;
    for field in [
        "gateId",
        "packetId",
        "requestedCandidateId",
        "selectedCandidateId",
        "verdict",
        "safeToClaim",
        "recommendedAction",
        "recommendedSafeToClaim",
        "claimCommandAction",
        "unsafeReasons",
        "staleReasons",
        "degradedCodes",
        "sourceRefs",
        "nextCommandActions",
    ] {
        if !source_gate_required.iter().any(|actual| actual == field) {
            return Err(format!(
                "unsafe-claim sourceGate must preserve original claim-gate field {field}"
            ));
        }
    }
    if schema
        .pointer("/definitions/sourceGate/properties/safeToClaim/const")
        .and_then(Value::as_bool)
        != Some(false)
    {
        return Err("unsafe-claim sourceGate.safeToClaim must be pinned false".into());
    }
    if !schema
        .pointer("/definitions/sourceGate/properties/claimCommandAction/const")
        .is_some_and(Value::is_null)
    {
        return Err("unsafe-claim sourceGate.claimCommandAction must be pinned null".into());
    }

    let reason_categories =
        string_array_at(&schema, "/definitions/reasonCategory/enum", schema_case.id)?;
    let expected_categories = [
        "tracker_authority",
        "agent_mail_readiness",
        "source_overlap",
        "dirty_checkout",
        "rch_proof_admission",
        "installed_binary_freshness",
        "reservation_conflict",
        "bv_staleness",
        "recommendation_mismatch",
        "memory_source_drift",
        "resource_admission",
        "action_suppression",
        "unknown",
    ];
    if reason_categories != expected_categories {
        return Err(format!(
            "{} reason-category taxonomy drifted\nactual: {reason_categories:?}\nexpected: {expected_categories:?}",
            schema_case.id
        ));
    }

    let action_kinds = string_array_at(
        &schema,
        "/definitions/plannerActionKind/enum",
        schema_case.id,
    )?;
    let expected_action_kinds = [
        "inspect",
        "comment_template",
        "decompose_candidate",
        "alternate_candidate",
        "retry_with_snapshot",
        "wait_or_coordinate",
        "stop",
    ];
    if action_kinds != expected_action_kinds {
        return Err(format!(
            "{} planner-action taxonomy drifted\nactual: {action_kinds:?}\nexpected: {expected_action_kinds:?}",
            schema_case.id
        ));
    }

    let fixtures = fixture_examples()?;
    let example = fixtures
        .get(schema_case.id)
        .ok_or_else(|| format!("fixture manifest missing {}", schema_case.id))?;

    if string_field(example, "/redactionStatus", schema_case.id)?
        != "counts_ids_statuses_path_patterns_command_templates_no_mail_body_no_file_content"
    {
        return Err("unsafe-claim plan redactionStatus drifted".into());
    }
    if bool_field(example, "/sourceGate/safeToClaim", schema_case.id)? {
        return Err("unsafe-claim plan fixture sourceGate.safeToClaim must be false".into());
    }
    if !example
        .pointer("/sourceGate/claimCommandAction")
        .is_some_and(Value::is_null)
    {
        return Err("unsafe-claim plan fixture must preserve claimCommandAction=null".into());
    }

    let source_gate_unsafe =
        string_array_at(example, "/sourceGate/unsafeReasons", "unsafe plan example")?;
    let unknown_reason = "future_gate_reason:opaque_value".to_owned();
    if !source_gate_unsafe.contains(&unknown_reason) {
        return Err("unsafe-claim plan fixture must preserve unknown raw reason".into());
    }

    let category_order = reason_categories
        .iter()
        .enumerate()
        .map(|(index, category)| (category.as_str(), index))
        .collect::<BTreeMap<_, _>>();
    let reason_groups = example
        .pointer("/reasonGroups")
        .and_then(Value::as_array)
        .ok_or_else(|| "unsafe-claim plan fixture missing reasonGroups".to_owned())?;
    let mut last_category_position = None;
    let mut seen_unknown = false;
    for (index, group) in reason_groups.iter().enumerate() {
        let context = format!("unsafe-claim reasonGroups[{index}]");
        let category = string_field(group, "/category", &context)?;
        let position = category_order
            .get(category)
            .copied()
            .ok_or_else(|| format!("{context} uses unknown category {category}"))?;
        if last_category_position.is_some_and(|last| position < last) {
            return Err("unsafe-claim reasonGroups are not sorted by taxonomy order".into());
        }
        last_category_position = Some(position);

        if category == "unknown" {
            seen_unknown = true;
            if !bool_field(group, "/preservesUnknown", &context)? {
                return Err("unknown reason group must set preservesUnknown=true".into());
            }
            let codes = string_array_at(group, "/reasonCodes", &context)?;
            if !codes.contains(&unknown_reason) {
                return Err("unknown reason group must keep the raw unknown reason".into());
            }
        }
    }
    if !seen_unknown {
        return Err("unsafe-claim plan fixture must include an unknown reason group".into());
    }

    let action_order = action_kinds
        .iter()
        .enumerate()
        .map(|(index, kind)| (kind.as_str(), index))
        .collect::<BTreeMap<_, _>>();
    let planner_actions = example
        .pointer("/plannerActions")
        .and_then(Value::as_array)
        .ok_or_else(|| "unsafe-claim plan fixture missing plannerActions".to_owned())?;
    let mut last_action_position = None;
    for (index, action) in planner_actions.iter().enumerate() {
        let context = format!("unsafe-claim plannerActions[{index}]");
        let kind = string_field(action, "/kind", &context)?;
        let position = action_order
            .get(kind)
            .copied()
            .ok_or_else(|| format!("{context} uses unknown action kind {kind}"))?;
        if last_action_position.is_some_and(|last| position < last) {
            return Err("unsafe-claim plannerActions are not sorted by taxonomy order".into());
        }
        last_action_position = Some(position);

        if bool_field(action, "/mutatesState", &context)?
            || !bool_field(action, "/advisoryOnly", &context)?
        {
            return Err(format!("{context} must be advisory and non-mutating"));
        }
        if let Some(command) = action.pointer("/commandAction") {
            if !command.is_null() && bool_field(command, "/mutatesState", &context)? {
                return Err(format!("{context}.commandAction must be read-only"));
            }
        }
    }

    let next_actions = example
        .pointer("/nextCommandActions")
        .and_then(Value::as_array)
        .ok_or_else(|| "unsafe-claim plan fixture missing nextCommandActions".to_owned())?;
    for (index, action) in next_actions.iter().enumerate() {
        let context = format!("unsafe-claim nextCommandActions[{index}]");
        if bool_field(action, "/mutatesState", &context)? {
            return Err(format!("{context} must be read-only"));
        }
    }

    let candidate_plans = example
        .pointer("/candidatePlans")
        .and_then(Value::as_array)
        .ok_or_else(|| "unsafe-claim plan fixture missing candidatePlans".to_owned())?;
    for (index, plan) in candidate_plans.iter().enumerate() {
        let context = format!("unsafe-claim candidatePlans[{index}]");
        if bool_field(plan, "/mayEmitClaimCommand", &context)? {
            return Err(format!("{context} must suppress claim commands"));
        }
    }

    for (pointer, expected) in [
        ("/nonMutationPolicy/advisoryOnly", true),
        ("/nonMutationPolicy/claimsBeads", false),
        ("/nonMutationPolicy/reservesFiles", false),
        ("/nonMutationPolicy/sendsAgentMail", false),
        ("/nonMutationPolicy/runsCargo", false),
        ("/nonMutationPolicy/stagesGit", false),
        ("/nonMutationPolicy/deletesFiles", false),
    ] {
        if bool_field(example, pointer, schema_case.id)? != expected {
            return Err(format!(
                "unsafe-claim plan nonMutationPolicy drifted at {pointer}"
            ));
        }
    }

    let rendered = serde_json::to_string(example)
        .map_err(|error| format!("serialize unsafe-claim example: {error}"))?;
    for forbidden in [
        "/Users/",
        "/home/",
        "From:",
        "Subject:",
        "Message-ID:",
        "raw_inbox",
        "stdout:",
        "stderr:",
        "BEGIN PRIVATE KEY",
        "BEGIN OPENSSH PRIVATE KEY",
        "ghp_",
        "Bearer ",
        "DATABASE_URL=",
    ] {
        if rendered.contains(forbidden) {
            return Err(format!(
                "unsafe-claim plan fixture leaks forbidden marker {forbidden}"
            ));
        }
    }

    Ok(())
}

#[test]
fn repair_plan_contract_pins_action_vocabulary_and_stop_conditions() -> TestResult {
    let schema_case = schema_case_by_id("ee.swarm.repair_plan.v1")?;
    let schema = schema_doc(schema_case)?;

    let required = string_array_at(&schema, "/required", schema_case.id)?;
    let expected_required = [
        "schema",
        "planId",
        "packetId",
        "gateId",
        "generatedAt",
        "workspace",
        "redactionStatus",
        "ordering",
        "sourceGate",
        "sourceEvidence",
        "actionVocabulary",
        "actions",
        "stopConditions",
        "nonMutationPolicy",
        "degraded",
        "provenanceHash",
    ];
    if required != expected_required {
        return Err(format!(
            "{} required field order drifted\nactual: {required:?}\nexpected: {expected_required:?}",
            schema_case.id
        ));
    }

    let action_kinds = string_array_at(
        &schema,
        "/definitions/repairActionKind/enum",
        schema_case.id,
    )?;
    let expected_action_kinds = [
        "wait_for_rch_build",
        "message_holder",
        "repair_agent_mail_archive",
        "rerun_snapshot",
        "refresh_bv_bounded",
        "inspect_beads_doctor",
        "rerun_claim_gate",
        "ask_human_for_destructive_repair",
    ];
    if action_kinds != expected_action_kinds {
        return Err(format!(
            "{} action vocabulary drifted\nactual: {action_kinds:?}\nexpected: {expected_action_kinds:?}",
            schema_case.id
        ));
    }

    let safety_classes = string_array_at(&schema, "/definitions/safetyClass/enum", schema_case.id)?;
    for required_class in [
        "read_only_probe",
        "coordination_mutation",
        "external_repair",
        "human_approval_required",
        "forbidden_out_of_scope",
    ] {
        if !safety_classes.contains(&required_class.to_owned()) {
            return Err(format!(
                "{} missing repair safety class {required_class}",
                schema_case.id
            ));
        }
    }

    let fixtures = fixture_examples()?;
    let example = fixtures
        .get(schema_case.id)
        .ok_or_else(|| format!("fixture missing {}", schema_case.id))?;
    let fixture_vocabulary = example
        .pointer("/actionVocabulary")
        .and_then(Value::as_array)
        .ok_or_else(|| "repair-plan fixture missing actionVocabulary".to_owned())?;
    let fixture_kinds = fixture_vocabulary
        .iter()
        .map(|entry| string_field(entry, "/kind", schema_case.id).map(ToOwned::to_owned))
        .collect::<Result<Vec<_>, _>>()?;
    if fixture_kinds != expected_action_kinds {
        return Err("repair-plan fixture actionVocabulary does not match schema enum".into());
    }

    for field in [
        "/nonMutationPolicy/sideEffectFree",
        "/nonMutationPolicy/claimsBeads",
        "/nonMutationPolicy/reservesFiles",
        "/nonMutationPolicy/sendsAgentMail",
        "/nonMutationPolicy/mutatesTracker",
        "/nonMutationPolicy/runsCargo",
        "/nonMutationPolicy/stagesGit",
        "/nonMutationPolicy/deletesFiles",
        "/nonMutationPolicy/executesRepairs",
    ] {
        let value = bool_field(example, field, "repair-plan fixture")?;
        if field == "/nonMutationPolicy/sideEffectFree" {
            if !value {
                return Err("repair-plan fixture must be side-effect-free".into());
            }
        } else if value {
            return Err(format!("repair-plan fixture must not set {field}"));
        }
    }

    let stop_ids = example
        .pointer("/stopConditions")
        .and_then(Value::as_array)
        .ok_or_else(|| "repair-plan fixture missing stopConditions".to_owned())?
        .iter()
        .map(|entry| {
            string_field(entry, "/id", "repair-plan stop condition").map(ToOwned::to_owned)
        })
        .collect::<Result<BTreeSet<_>, _>>()?;
    for required_stop in [
        "fresh_claim_gate_safe_to_claim",
        "source_authority_fail_closed",
        "no_source_verdict_without_rch_cargo",
        "human_approval_required_before_destructive_repair",
        "agent_mail_or_tracker_not_authoritative",
    ] {
        if !stop_ids.contains(required_stop) {
            return Err(format!(
                "repair-plan fixture missing stop condition {required_stop}"
            ));
        }
    }

    let external_repair = example
        .pointer("/actions")
        .and_then(Value::as_array)
        .ok_or_else(|| "repair-plan fixture missing actions".to_owned())?
        .iter()
        .find(|entry| {
            string_field(entry, "/kind", "repair-plan action").ok()
                == Some("repair_agent_mail_archive")
        })
        .ok_or_else(|| "repair-plan fixture missing repair_agent_mail_archive action".to_owned())?;
    if !bool_field(
        external_repair,
        "/safety/requiresHumanApproval",
        "repair-plan external repair",
    )? {
        return Err("repair_agent_mail_archive must require human approval".into());
    }
    if external_repair
        .pointer("/commandAction")
        .is_some_and(|value| !value.is_null())
    {
        return Err("repair_agent_mail_archive must not expose an executable command".into());
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
fn work_packet_command_actions_require_shell_safe_argv_contract() -> TestResult {
    // bd-13dmm.3: command strings in work packets are migration-era display
    // text. The executable contract is a structured argv action with explicit
    // shell/copy-safety posture and redaction guards on executable fields.
    let case = SCHEMA_CASES
        .iter()
        .copied()
        .find(|case| case.id == "ee.swarm.work_packet.v1")
        .ok_or_else(|| "ee.swarm.work_packet.v1 schema case missing".to_owned())?;
    let schema = schema_doc(case)?;

    let command_action = schema
        .pointer("/definitions/commandAction")
        .ok_or_else(|| "commandAction definition missing".to_owned())?;
    let required_fields = command_action
        .pointer("/required")
        .and_then(Value::as_array)
        .ok_or_else(|| "commandAction.required missing".to_owned())?
        .iter()
        .filter_map(Value::as_str)
        .collect::<BTreeSet<_>>();
    for field in [
        "commandId",
        "displayCommand",
        "argv",
        "shellRequired",
        "copySafety",
        "mutatesState",
        "requiredSubstrate",
        "when",
        "rationale",
    ] {
        if !required_fields.contains(field) {
            return Err(format!("commandAction must require {field}"));
        }
    }
    if command_action
        .pointer("/additionalProperties")
        .and_then(Value::as_bool)
        != Some(false)
    {
        return Err("commandAction must reject undeclared fields".into());
    }
    for (pointer, expected_ref) in [
        (
            "/definitions/recommendedAction/properties/suggestedCommandActions/items/$ref",
            "#/definitions/commandAction",
        ),
        (
            "/definitions/verificationCommand/properties/commandAction/$ref",
            "#/definitions/commandAction",
        ),
        (
            "/definitions/agentMailFallbackAction/properties/commandAction/anyOf/0/$ref",
            "#/definitions/commandAction",
        ),
        (
            "/definitions/commandAction/properties/argv/$ref",
            "#/definitions/argvArray",
        ),
    ] {
        if string_field(&schema, pointer, "ee.swarm.work_packet.v1")? != expected_ref {
            return Err(format!("{pointer} must reference {expected_ref}"));
        }
    }

    let command_template_description = string_field(
        &schema,
        "/definitions/verificationCommand/properties/commandTemplate/description",
        "verificationCommand.commandTemplate",
    )?;
    for marker in [
        "Legacy display-only",
        "MUST NOT be executed",
        "not shell-safe",
    ] {
        if !command_template_description.contains(marker) {
            return Err(format!(
                "commandTemplate description must mark legacy display-only posture with {marker}"
            ));
        }
    }

    let copy_safety = schema
        .pointer("/definitions/copySafety/enum")
        .and_then(Value::as_array)
        .ok_or_else(|| "copySafety enum missing".to_owned())?
        .iter()
        .filter_map(Value::as_str)
        .collect::<BTreeSet<_>>();
    for variant in [
        "safe_structured_argv",
        "display_only",
        "shell_required_review",
        "forbidden_until_human_approval",
    ] {
        if !copy_safety.contains(variant) {
            return Err(format!("copySafety enum must include {variant}"));
        }
    }

    let substrates = schema
        .pointer("/definitions/commandSubstrate/enum")
        .and_then(Value::as_array)
        .ok_or_else(|| "commandSubstrate enum missing".to_owned())?
        .iter()
        .filter_map(Value::as_str)
        .collect::<BTreeSet<_>>();
    for substrate in ["agent_mail", "beads", "git", "jq", "rch", "static_local"] {
        if !substrates.contains(substrate) {
            return Err(format!("commandSubstrate enum must include {substrate}"));
        }
    }

    if schema
        .pointer("/definitions/argvArray/minItems")
        .and_then(Value::as_u64)
        != Some(1)
    {
        return Err("argvArray must require at least one argv element".into());
    }
    if string_field(
        &schema,
        "/definitions/argvArray/items/$ref",
        "argvArray.items",
    )? != "#/definitions/safeCommandString"
    {
        return Err("argvArray items must use safeCommandString".into());
    }

    let guard_patterns = schema
        .pointer("/definitions/safeCommandString/not/anyOf")
        .and_then(Value::as_array)
        .ok_or_else(|| "safeCommandString redaction guards missing".to_owned())?
        .iter()
        .filter_map(|value| value.get("pattern").and_then(Value::as_str))
        .collect::<BTreeSet<_>>();
    for pattern in [
        "BEGIN PRIVATE KEY",
        "ghp_[A-Za-z0-9_]+",
        "DATABASE_URL=",
        "From: ",
        "Subject: ",
        "Message-ID:",
        "stdout:",
        "stderr:",
        "/Users/[^\\s]+",
        "/home/[^\\s]+",
    ] {
        if !guard_patterns.contains(pattern) {
            return Err(format!(
                "safeCommandString must forbid executable-field marker {pattern}"
            ));
        }
    }

    Ok(())
}

#[test]
fn work_packet_command_surfaces_reject_unsafe_command_drift() -> TestResult {
    // bd-13dmm.2: work-packet command fields are agent-facing copy surfaces.
    // Legacy display strings may remain during migration, but concrete
    // executable actions must carry structured argv/copy-safety posture, and
    // no example or fixture command surface may smuggle shell-eval, local Cargo,
    // raw paths, mail bodies, or secret-looking material.
    let case = SCHEMA_CASES
        .iter()
        .copied()
        .find(|case| case.id == "ee.swarm.work_packet.v1")
        .ok_or_else(|| "ee.swarm.work_packet.v1 schema case missing".to_owned())?;
    let schema = schema_doc(case)?;

    let mut documents = Vec::new();
    let schema_examples = schema
        .pointer("/examples")
        .and_then(Value::as_array)
        .ok_or_else(|| "ee.swarm.work_packet.v1 examples missing".to_owned())?;
    for (index, example) in schema_examples.iter().enumerate() {
        documents.push((format!("schema.examples[{index}]"), example.clone()));
    }

    let fixture_dir = repo_root()
        .join("tests")
        .join("fixtures")
        .join("swarm_work_packet");
    for entry in fs::read_dir(&fixture_dir)
        .map_err(|error| format!("read {}: {error}", fixture_dir.display()))?
    {
        let entry = entry.map_err(|error| format!("read fixture entry: {error}"))?;
        let path = entry.path();
        if path.extension().and_then(|extension| extension.to_str()) == Some("json") {
            let fixture = read_json(&path)?;
            let name = path
                .file_name()
                .and_then(|name| name.to_str())
                .ok_or_else(|| format!("fixture path has non-UTF8 name: {}", path.display()))?
                .to_owned();
            documents.push((format!("tests/fixtures/swarm_work_packet/{name}"), fixture));
        }
    }

    let redaction_markers = [
        "BEGIN PRIVATE KEY",
        "BEGIN OPENSSH PRIVATE KEY",
        "ghp_",
        "Bearer ",
        "DATABASE_URL=",
        "From: ",
        "Subject: ",
        "Message-ID:",
        "raw_inbox",
        "stdout:",
        "stderr:",
        "/Users/",
        "/home/",
        ".sqlite-shm",
        ".sqlite-wal",
    ];
    let shell_markers = ["`", "$(", "|", ">", "<", "&&", "||"];

    fn command_strings<'a>(value: &'a Value, path: &str, out: &mut Vec<(String, &'a str)>) {
        match value {
            Value::Object(map) => {
                for (key, child) in map {
                    let child_path = format!("{path}/{key}");
                    match (key.as_str(), child) {
                        ("commandTemplate" | "displayCommand" | "command", Value::String(text)) => {
                            out.push((child_path, text.as_str()))
                        }
                        ("suggestedCommands", Value::Array(commands)) => {
                            for (index, command) in commands.iter().enumerate() {
                                if let Some(text) = command.as_str() {
                                    out.push((format!("{child_path}[{index}]"), text));
                                }
                            }
                        }
                        _ => command_strings(child, &child_path, out),
                    }
                }
            }
            Value::Array(values) => {
                for (index, child) in values.iter().enumerate() {
                    command_strings(child, &format!("{path}[{index}]"), out);
                }
            }
            _ => {}
        }
    }

    fn command_actions<'a>(value: &'a Value, path: &str, out: &mut Vec<(String, &'a Value)>) {
        match value {
            Value::Object(map) => {
                for (key, child) in map {
                    let child_path = format!("{path}/{key}");
                    if key == "commandAction" {
                        out.push((child_path.clone(), child));
                    }
                    if key == "suggestedCommandActions"
                        && let Value::Array(actions) = child
                    {
                        for (index, action) in actions.iter().enumerate() {
                            out.push((format!("{child_path}[{index}]"), action));
                        }
                    }
                    command_actions(child, &child_path, out);
                }
            }
            Value::Array(values) => {
                for (index, child) in values.iter().enumerate() {
                    command_actions(child, &format!("{path}[{index}]"), out);
                }
            }
            _ => {}
        }
    }

    for (document_name, document) in &documents {
        let mut strings = Vec::new();
        command_strings(document, document_name, &mut strings);
        for (path, text) in strings {
            for marker in redaction_markers {
                if text.contains(marker) {
                    return Err(format!("{path} leaks forbidden command marker {marker}"));
                }
            }
            for marker in shell_markers {
                if text.contains(marker) {
                    return Err(format!("{path} contains shell-eval marker {marker}"));
                }
            }
            // Forbid running `cargo ...` locally; the contract is that all
            // cargo invocations go through an RCH wrapper. Both the legacy
            // `rch_verify.sh` script and the structured `rch exec --` form
            // satisfy the wrapper requirement.
            if text.contains("cargo ")
                && !text.contains("rch_verify.sh")
                && !text.contains("rch exec")
            {
                return Err(format!(
                    "{path} contains local Cargo fallback instead of RCH wrapper"
                ));
            }
        }

        let mut actions = Vec::new();
        command_actions(document, document_name, &mut actions);
        for (path, action) in actions {
            // agentMailFallbackAction.commandAction is `commandAction | null`
            // in the schema (bd-2z5ly.8): emitters set commandAction xor
            // manualStep. A null commandAction simply means the step is
            // manual-only and has no executable argv to validate here.
            if action.is_null() {
                continue;
            }
            if !action.is_object() {
                return Err(format!("{path} must be an object"));
            }
            let argv = action
                .pointer("/argv")
                .and_then(Value::as_array)
                .ok_or_else(|| format!("{path} missing argv array"))?;
            if argv.is_empty() {
                return Err(format!("{path} argv must not be empty"));
            }
            let copy_safety = string_field(action, "/copySafety", &path)?;
            let shell_required = bool_field(action, "/shellRequired", &path)?;
            if copy_safety == "safe_structured_argv" && shell_required {
                return Err(format!(
                    "{path} cannot require a shell when copySafety is safe_structured_argv"
                ));
            }
            for (index, arg) in argv.iter().enumerate() {
                let arg = arg
                    .as_str()
                    .ok_or_else(|| format!("{path}/argv[{index}] must be a string"))?;
                for marker in redaction_markers {
                    if arg.contains(marker) {
                        return Err(format!(
                            "{path}/argv[{index}] leaks forbidden command marker {marker}"
                        ));
                    }
                }
                for marker in shell_markers {
                    if arg.contains(marker) {
                        return Err(format!(
                            "{path}/argv[{index}] contains shell-eval marker {marker}"
                        ));
                    }
                }
            }
            let argv_text = argv
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(" ");
            if argv_text.contains("cargo ") && !argv_text.contains("rch_verify.sh") {
                return Err(format!(
                    "{path}/argv contains local Cargo fallback instead of RCH wrapper"
                ));
            }
        }
    }

    for doc_path in [
        "docs/swarm/work_packet.md",
        "docs/agent-ux/swarm-work-packet.md",
    ] {
        let text = read_text(&repo_root().join(doc_path))?;
        for required in [
            "commandAction",
            "safe_structured_argv",
            "MUST NOT be passed to a shell",
        ] {
            if !text.contains(required) {
                return Err(format!(
                    "{doc_path} must document command-action safety marker {required}"
                ));
            }
        }
        for forbidden in ["rm -rf", "git reset --hard", "BEGIN PRIVATE KEY", "ghp_"] {
            if text.contains(forbidden) {
                return Err(format!(
                    "{doc_path} contains unsafe command marker {forbidden}"
                ));
            }
        }
    }

    Ok(())
}

#[test]
fn work_packet_candidate_decision_vocabulary_is_contractual() -> TestResult {
    // bd-2z5ly.7.5: candidate decisions are an agent-facing safety
    // vocabulary. The enum order is part of the contract because packet IDs
    // depend on deterministic serialized payloads and consumers need stable
    // docs/golden drift signals when a decision is added or reclassified.
    let case = SCHEMA_CASES
        .iter()
        .copied()
        .find(|case| case.id == "ee.swarm.work_packet.v1")
        .ok_or_else(|| "ee.swarm.work_packet.v1 schema case missing".to_owned())?;
    let schema = schema_doc(case)?;
    let decision_enum = schema
        .pointer("/definitions/candidateDecision/enum")
        .and_then(Value::as_array)
        .ok_or_else(|| "ee.swarm.work_packet.v1 candidateDecision enum missing".to_owned())?
        .iter()
        .map(|value| {
            value
                .as_str()
                .ok_or_else(|| "candidateDecision enum entries must be strings".to_owned())
        })
        .collect::<Result<Vec<_>, _>>()?;

    let expected_decisions = vec![
        "safe_to_claim",
        "already_owned",
        "unsafe_due_to_conflict",
        "blocked_by_dependency",
        "blocked_by_verification",
        "stale_but_reclaimable",
        "stale_review",
        "external_state_required",
        "release_operator_required",
        "rollup_only",
        "blocked_rollup",
        "coordinate_first",
        "blocked",
        "stale_or_advisory",
        "skip",
    ];
    if decision_enum != expected_decisions {
        return Err(format!(
            "candidateDecision enum drifted: expected {expected_decisions:?}, got {decision_enum:?}"
        ));
    }

    let candidate_decision_ref = string_field(
        &schema,
        "/definitions/candidate/properties/decision/$ref",
        "ee.swarm.work_packet.v1 candidate decision",
    )?;
    if candidate_decision_ref != "#/definitions/candidateDecision" {
        return Err(
            "candidate.decision must reference the canonical candidateDecision definition".into(),
        );
    }

    for doc_path in [
        "docs/swarm/work_packet.md",
        "docs/agent-ux/swarm-work-packet.md",
    ] {
        let text = read_text(&repo_root().join(doc_path))?;
        for required in [
            "bd-2z5ly.7.5",
            "safe_to_claim",
            "already_owned",
            "unsafe_due_to_conflict",
            "blocked_by_dependency",
            "blocked_by_verification",
            "stale_but_reclaimable",
            "external_state_required",
            "release_operator_required",
            "rollup_only",
            "blocked_rollup",
            "stale_or_advisory",
            "unsafeReasons",
            "staleReasons",
            "sourceRefs",
        ] {
            if !text.contains(required) {
                return Err(format!(
                    "{doc_path} must document candidate-decision marker {required}"
                ));
            }
        }
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
fn work_packet_agent_mail_recovery_corrupt_is_contractual() -> TestResult {
    // bd-18jfx: Agent Mail recovery/durability corruption has the same
    // authority as semantic readiness. A green transport health level and a
    // semanticReadiness pass cannot make reservation or inbox reads
    // authoritative while recovery.mode or durability state says corrupt.
    let case = SCHEMA_CASES
        .iter()
        .copied()
        .find(|case| case.id == "ee.swarm.work_packet.v1")
        .ok_or_else(|| "ee.swarm.work_packet.v1 schema case missing".to_owned())?;
    let schema = schema_doc(case)?;
    let agent_mail = schema
        .pointer("/definitions/agentMail")
        .ok_or_else(|| "agentMail definition missing".to_owned())?;

    let properties = agent_mail
        .pointer("/properties")
        .and_then(Value::as_object)
        .ok_or_else(|| "agentMail properties missing".to_owned())?
        .keys()
        .cloned()
        .collect::<BTreeSet<_>>();
    for required in ["recovery", "durabilityState"] {
        if !properties.contains(required) {
            return Err(format!("agentMail must expose {required} for bd-18jfx"));
        }
    }

    let recovery = agent_mail
        .pointer("/properties/recovery")
        .ok_or_else(|| "agentMail.recovery missing".to_owned())?;
    let recovery_mode_enum = recovery
        .pointer("/properties/mode/enum")
        .and_then(Value::as_array)
        .ok_or_else(|| "agentMail.recovery.mode enum missing".to_owned())?
        .iter()
        .filter_map(Value::as_str)
        .collect::<BTreeSet<_>>();
    if !recovery_mode_enum.contains("corrupt") {
        return Err("agentMail.recovery.mode enum must include corrupt".into());
    }
    let recovery_reason_enum = recovery
        .pointer("/properties/reason/enum")
        .and_then(Value::as_array)
        .ok_or_else(|| "agentMail.recovery.reason enum missing".to_owned())?
        .iter()
        .filter_map(Value::as_str)
        .collect::<BTreeSet<_>>();
    if !recovery_reason_enum.contains("archive_corruption") {
        return Err("agentMail.recovery.reason enum must include archive_corruption".into());
    }
    let durability_state_enum = agent_mail
        .pointer("/properties/durabilityState/enum")
        .and_then(Value::as_array)
        .ok_or_else(|| "agentMail.durabilityState enum missing".to_owned())?
        .iter()
        .filter_map(Value::as_str)
        .collect::<BTreeSet<_>>();
    if !durability_state_enum.contains("corrupt") {
        return Err("agentMail.durabilityState enum must include corrupt".into());
    }

    let fixture_path = repo_root()
        .join("tests")
        .join("fixtures")
        .join("swarm_work_packet")
        .join("agent_mail_recovery_corrupt.json");
    let fixture = read_json(&fixture_path)?;
    if string_field(&fixture, "/schema", "agent_mail_recovery_corrupt fixture")?
        != "ee.swarm.work_packet.v1"
    {
        return Err("agent_mail_recovery_corrupt fixture schema drifted".into());
    }
    if string_field(
        &fixture,
        "/observedStateClass",
        "agent_mail_recovery_corrupt fixture",
    )? != "agent_mail_recovery_corrupt"
    {
        return Err(
            "agent_mail_recovery_corrupt fixture must use a distinct observedStateClass".into(),
        );
    }

    let fixture_agent_mail = fixture.pointer("/coordination/agentMail").ok_or_else(|| {
        "agent_mail_recovery_corrupt fixture missing coordination.agentMail".to_owned()
    })?;
    if string_field(
        fixture_agent_mail,
        "/healthLevel",
        "agent_mail_recovery_corrupt fixture agentMail",
    )? != "green"
        || string_field(
            fixture_agent_mail,
            "/semanticReadiness/status",
            "agent_mail_recovery_corrupt fixture agentMail",
        )? != "pass"
    {
        return Err(
            "agent_mail_recovery_corrupt fixture must prove green transport and semantic pass are insufficient"
                .into(),
        );
    }
    if string_field(
        fixture_agent_mail,
        "/recovery/mode",
        "agent_mail_recovery_corrupt fixture agentMail",
    )? != "corrupt"
        || string_field(
            fixture_agent_mail,
            "/recovery/reason",
            "agent_mail_recovery_corrupt fixture agentMail",
        )? != "archive_corruption"
        || string_field(
            fixture_agent_mail,
            "/durabilityState",
            "agent_mail_recovery_corrupt fixture agentMail",
        )? != "corrupt"
    {
        return Err(
            "agent_mail_recovery_corrupt fixture must carry bounded recovery and durability corruption classes"
                .into(),
        );
    }
    if string_field(
        fixture_agent_mail,
        "/recoveryMode",
        "agent_mail_recovery_corrupt fixture agentMail",
    )? != "wait_for_repair"
    {
        return Err(
            "agent_mail_recovery_corrupt fixture must set recoveryMode=wait_for_repair".into(),
        );
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
            "agent_mail_recovery_corrupt fixture must mark reservation/inbox as non-authoritative"
                .into(),
        );
    }

    let degraded_codes = fixture_agent_mail
        .pointer("/degradedCodes")
        .and_then(Value::as_array)
        .ok_or_else(|| "agent_mail_recovery_corrupt fixture missing degradedCodes".to_owned())?
        .iter()
        .filter_map(Value::as_str)
        .collect::<BTreeSet<_>>();
    if !degraded_codes.contains("agent_mail_unavailable") {
        return Err(
            "agent_mail_recovery_corrupt fixture must include agent_mail_unavailable in degradedCodes"
                .into(),
        );
    }

    if fixture
        .pointer("/recommendedAction/safeToClaim")
        .and_then(Value::as_bool)
        != Some(false)
    {
        return Err(
            "agent_mail_recovery_corrupt fixture must not recommend safeToClaim=true".into(),
        );
    }
    let fallback_actions = fixture_agent_mail
        .pointer("/fallbackActions")
        .and_then(Value::as_array)
        .ok_or_else(|| "agent_mail_recovery_corrupt fixture missing fallbackActions".to_owned())?;
    if fallback_actions.is_empty() {
        return Err("agent_mail_recovery_corrupt fixture must enumerate fallback actions".into());
    }
    let mut last_kind: Option<&str> = None;
    for (index, action) in fallback_actions.iter().enumerate() {
        let context = format!("recoveryCorruptFallbackActions[{index}]");
        let kind = string_field(action, "/kind", &context)?;
        if let Some(previous) = last_kind
            && kind < previous
        {
            return Err(format!(
                "recovery-corrupt fallbackActions must be sorted by kind; saw {previous} before {kind}"
            ));
        }
        last_kind = Some(kind);
    }

    let rendered = serde_json::to_string(fixture_agent_mail)
        .map_err(|error| format!("serialize recovery_corrupt agentMail: {error}"))?;
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
        "support-bundle/",
        "from: ",
        "subject: ",
        "message-id:",
        "begin pgp",
    ] {
        if lowered.contains(forbidden) {
            return Err(format!(
                "agent_mail_recovery_corrupt fixture must not leak raw path or error marker {forbidden}"
            ));
        }
    }

    Ok(())
}

#[test]
fn work_packet_agent_mail_database_contention_timeout_is_contractual() -> TestResult {
    // bd-2z5ly.3.1: Agent Mail timeout/database-contention is distinct from
    // degraded_read_only and semantic_readiness_failed. The fixture pins the
    // non-authoritative coordination posture without preserving raw stderr,
    // inbox contents, mailbox paths, PIDs, or live process details.
    let case = SCHEMA_CASES
        .iter()
        .copied()
        .find(|case| case.id == "ee.swarm.work_packet.v1")
        .ok_or_else(|| "ee.swarm.work_packet.v1 schema case missing".to_owned())?;
    let schema = schema_doc(case)?;
    let agent_mail = schema
        .pointer("/definitions/agentMail")
        .ok_or_else(|| "agentMail definition missing".to_owned())?;
    let status_enum = agent_mail
        .pointer("/properties/status/enum")
        .and_then(Value::as_array)
        .ok_or_else(|| "agentMail.status enum missing".to_owned())?
        .iter()
        .filter_map(Value::as_str)
        .collect::<BTreeSet<_>>();
    if !status_enum.contains("unavailable") {
        return Err("agentMail.status enum must include unavailable".into());
    }

    let fixture_path = repo_root()
        .join("tests")
        .join("fixtures")
        .join("swarm_work_packet")
        .join("agent_mail_database_contention_timeout.json");
    let fixture = read_json(&fixture_path)?;
    if string_field(&fixture, "/schema", "database contention fixture")?
        != "ee.swarm.work_packet.v1"
    {
        return Err("database contention fixture schema drifted".into());
    }
    if string_field(
        &fixture,
        "/observedStateClass",
        "database contention fixture",
    )? != "agent_mail_database_contention_timeout"
    {
        return Err("database contention fixture must use a distinct observedStateClass".into());
    }

    let fixture_agent_mail = fixture
        .pointer("/coordination/agentMail")
        .ok_or_else(|| "database contention fixture missing coordination.agentMail".to_owned())?;
    if string_field(
        fixture_agent_mail,
        "/status",
        "database contention fixture agentMail",
    )? != "unavailable"
    {
        return Err("database contention fixture must set agentMail.status=unavailable".into());
    }
    if fixture_agent_mail.pointer("/healthLevel") != Some(&Value::Null) {
        return Err(
            "database contention fixture must not invent a healthLevel after timeout".into(),
        );
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
            "database contention fixture must mark reservation/inbox as non-authoritative".into(),
        );
    }

    let degraded_codes = fixture_agent_mail
        .pointer("/degradedCodes")
        .and_then(Value::as_array)
        .ok_or_else(|| "database contention fixture missing degradedCodes".to_owned())?
        .iter()
        .filter_map(Value::as_str)
        .collect::<BTreeSet<_>>();
    for expected in [
        "agent_mail_database_contention_timeout",
        "agent_mail_unavailable",
    ] {
        if !degraded_codes.contains(expected) {
            return Err(format!(
                "database contention fixture missing degraded code {expected}"
            ));
        }
    }

    let recommended_safe = fixture
        .pointer("/recommendedAction/safeToClaim")
        .and_then(Value::as_bool)
        .ok_or_else(|| {
            "database contention fixture missing recommendedAction.safeToClaim".to_owned()
        })?;
    if recommended_safe {
        return Err("database contention fixture must not recommend safeToClaim=true".into());
    }
    let candidate_decision = string_field(
        &fixture,
        "/candidates/0/decision",
        "database contention fixture candidate",
    )?;
    if candidate_decision == "safe_to_claim" {
        return Err(
            "database contention fixture must not classify the candidate as safe_to_claim".into(),
        );
    }

    let fallback_actions = fixture_agent_mail
        .pointer("/fallbackActions")
        .and_then(Value::as_array)
        .ok_or_else(|| "database contention fixture missing fallbackActions".to_owned())?;
    let kinds = fallback_actions
        .iter()
        .enumerate()
        .map(|(index, action)| {
            string_field(action, "/kind", &format!("fallbackActions[{index}]")).map(str::to_owned)
        })
        .collect::<Result<Vec<_>, _>>()?;
    let expected_kinds = vec![
        "beads_comment".to_owned(),
        "manual_coordination".to_owned(),
        "retry_later".to_owned(),
        "switch_to_static_work".to_owned(),
    ];
    if kinds != expected_kinds {
        return Err(format!(
            "database contention fallback actions drifted: {kinds:?}"
        ));
    }

    let rendered = serde_json::to_string(&fixture)
        .map_err(|error| format!("serialize database contention fixture: {error}"))?;
    let lowered = rendered.to_ascii_lowercase();
    for forbidden in [
        "/private/",
        "/users/",
        "/var/",
        "\"pid\"",
        "pid ",
        "raw stderr",
        "raw stdout",
        "mail body",
        "message-id:",
        "subject:",
        "from:",
        ".sqlite-shm",
        ".sqlite-wal",
        "page 283",
        "btree page",
        "stack trace",
    ] {
        if lowered.contains(forbidden) {
            return Err(format!(
                "database contention fixture leaked forbidden detail {forbidden}"
            ));
        }
    }

    Ok(())
}

#[test]
fn work_packet_bv_timeout_no_output_is_contractual() -> TestResult {
    // bd-2z5ly.3.2: BV robot-source timeout/no-output must be a
    // bounded degraded source, not an omitted source, a healthy empty
    // recommendation, or a reason to wait indefinitely. The fixture pins
    // stale-safe Beads fallback and prevents interactive `bv` suggestions.
    let fixture_path = repo_root()
        .join("tests")
        .join("fixtures")
        .join("swarm_work_packet")
        .join("bv_timeout_no_output.json");
    let fixture = read_json(&fixture_path)?;
    if string_field(&fixture, "/schema", "bv timeout fixture")? != "ee.swarm.work_packet.v1" {
        return Err("bv timeout fixture schema drifted".into());
    }
    if string_field(&fixture, "/observedStateClass", "bv timeout fixture")?
        != "bv_timeout_no_output"
    {
        return Err("bv timeout fixture must use observedStateClass=bv_timeout_no_output".into());
    }
    if fixture
        .pointer("/recommendedAction/safeToClaim")
        .and_then(Value::as_bool)
        != Some(false)
    {
        return Err("bv timeout fixture must not recommend safeToClaim=true".into());
    }
    if string_field(
        &fixture,
        "/candidates/0/decision",
        "bv timeout fixture candidate",
    )? != "blocked"
    {
        return Err("bv timeout fixture must block stale fallback candidates".into());
    }

    let source_provenance = fixture
        .pointer("/sourceProvenance")
        .and_then(Value::as_array)
        .ok_or_else(|| "bv timeout fixture missing sourceProvenance".to_owned())?;
    let bv_source = source_provenance
        .iter()
        .find(|source| source.pointer("/source").and_then(Value::as_str) == Some("bv"))
        .ok_or_else(|| "bv timeout fixture missing BV source provenance".to_owned())?;
    if string_field(bv_source, "/status", "bv source provenance")? != "degraded"
        || string_field(bv_source, "/freshness", "bv source provenance")? != "timeout_no_output"
    {
        return Err(format!(
            "bv source provenance must be degraded/timeout_no_output, got {bv_source}"
        ));
    }

    let degraded = fixture
        .pointer("/degraded")
        .and_then(Value::as_array)
        .ok_or_else(|| "bv timeout fixture missing degraded list".to_owned())?;
    let degraded_codes = degraded
        .iter()
        .filter_map(|entry| entry.pointer("/code").and_then(Value::as_str))
        .collect::<BTreeSet<_>>();
    for expected in ["bv_command_timeout", "bv_no_output"] {
        if !degraded_codes.contains(expected) {
            return Err(format!(
                "bv timeout fixture missing degraded code {expected}"
            ));
        }
    }

    let recommended_reasons = fixture
        .pointer("/recommendedAction/reasons")
        .and_then(Value::as_array)
        .ok_or_else(|| "bv timeout fixture missing recommendedAction.reasons".to_owned())?
        .iter()
        .filter_map(Value::as_str)
        .collect::<BTreeSet<_>>();
    for expected in [
        "bv_timeout_no_output",
        "fallback_rows_not_authoritative",
        "graph_triage_not_authoritative",
    ] {
        if !recommended_reasons.contains(expected) {
            return Err(format!("bv timeout fixture missing reason {expected}"));
        }
    }

    let rendered =
        serde_json::to_string(&fixture).map_err(|error| format!("serialize fixture: {error}"))?;
    let lowered = rendered.to_ascii_lowercase();
    for forbidden in [
        "/users/",
        "/private/",
        "/var/",
        "\"pid\"",
        "pid ",
        "raw stdout",
        "raw stderr",
        "mail body",
        "message-id:",
        "subject:",
        "from:",
        "file content",
        "stack trace",
    ] {
        if lowered.contains(forbidden) {
            return Err(format!(
                "bv timeout fixture leaked forbidden detail {forbidden}"
            ));
        }
    }

    let command_strings = fixture
        .pointer("/recommendedAction/suggestedCommands")
        .and_then(Value::as_array)
        .ok_or_else(|| "bv timeout fixture missing suggestedCommands".to_owned())?
        .iter()
        .filter_map(Value::as_str)
        .collect::<Vec<_>>();
    if !command_strings.contains(&"br --no-auto-import --allow-stale ready --json") {
        return Err("bv timeout fixture must recommend stale-safe Beads fallback".into());
    }
    for command in command_strings {
        if command == "bv" {
            return Err("bv timeout fixture must not recommend bare interactive bv".into());
        }
    }

    Ok(())
}

#[test]
fn work_packet_bv_robot_insights_projection_is_bounded_and_advisory() -> TestResult {
    // bd-ifoh3.6: robot-insights can be useful on large trackers only when its
    // graph output is bounded and clearly advisory. This pins the compact
    // projection shape expected from BV or from an ee collector projection.
    let fixture_path = repo_root()
        .join("tests")
        .join("fixtures")
        .join("swarm_work_packet")
        .join("bv_robot_insights_bounded_projection.json");
    let fixture = read_json(&fixture_path)?;
    if string_field(&fixture, "/schema", "bv robot-insights fixture")?
        != "ee.bv.robot_insights_projection.v1"
    {
        return Err("bv robot-insights fixture schema drifted".into());
    }
    if string_field(
        &fixture,
        "/command/commandTemplate",
        "bv robot-insights fixture",
    )? != "bv --robot-insights --format json --fields status,top_what_ifs,advanced_insights --limit 8 --max-bytes 32768"
    {
        return Err("bv robot-insights fixture must pin a bounded command template".into());
    }
    if fixture.pointer("/budget/maxBytes").and_then(Value::as_i64) != Some(32768)
        || fixture
            .pointer("/budget/estimatedBytes")
            .and_then(Value::as_i64)
            > Some(32768)
        || fixture
            .pointer("/budget/truncated")
            .and_then(Value::as_bool)
            != Some(true)
    {
        return Err("bv robot-insights fixture must report bounded output bytes".into());
    }
    if fixture
        .pointer("/graph/nodeCount")
        .and_then(Value::as_i64)
        .is_none_or(|count| count <= 2000)
        || fixture
            .pointer("/graph/edgeCount")
            .and_then(Value::as_i64)
            .is_none_or(|count| count <= 4000)
    {
        return Err("bv robot-insights fixture must preserve large-graph shape".into());
    }
    if string_field(&fixture, "/graph/cycles/state", "bv robot-insights fixture")? != "skipped"
        || !string_field(
            &fixture,
            "/graph/cycles/reason",
            "bv robot-insights fixture",
        )?
        .contains(">2000 nodes")
    {
        return Err("bv robot-insights fixture must pin cycle-analysis skip reason".into());
    }
    if fixture
        .pointer("/caps/maxTopWhatIfs")
        .and_then(Value::as_i64)
        != Some(8)
        || fixture
            .pointer("/caps/emittedTopWhatIfs")
            .and_then(Value::as_i64)
            .is_none_or(|count| count > 8)
        || fixture
            .pointer("/caps/omittedTopWhatIfs")
            .and_then(Value::as_i64)
            .is_none_or(|count| count == 0)
        || fixture
            .pointer("/caps/largeMapsTruncated")
            .and_then(Value::as_bool)
            != Some(true)
    {
        return Err("bv robot-insights fixture must report top-what-if and map caps".into());
    }

    let omitted = string_array_at(
        &fixture,
        "/caps/omittedSections",
        "bv robot-insights fixture",
    )?;
    for section in [
        "full_pagerank_map",
        "full_betweenness_map",
        "raw_cycle_candidates",
    ] {
        if !omitted.iter().any(|item| item == section) {
            return Err(format!(
                "bv robot-insights fixture missing omitted section {section}"
            ));
        }
    }
    if fixture
        .pointer("/recommendationPosture/advisoryOnly")
        .and_then(Value::as_bool)
        != Some(true)
        || fixture
            .pointer("/recommendationPosture/claimCommandSuppressed")
            .and_then(Value::as_bool)
            != Some(true)
        || fixture
            .pointer("/recommendationPosture/requiresActionableQueue")
            .and_then(Value::as_bool)
            != Some(true)
        || fixture
            .pointer("/recommendationPosture/requiresClaimGate")
            .and_then(Value::as_bool)
            != Some(true)
        || fixture
            .pointer("/recommendationPosture/recommendationsContainNonActionable")
            .and_then(Value::as_bool)
            != Some(true)
    {
        return Err("bv robot-insights recommendations must remain advisory".into());
    }
    let top_what_ifs = fixture
        .pointer("/topWhatIfs")
        .and_then(Value::as_array)
        .ok_or_else(|| "bv robot-insights fixture missing topWhatIfs".to_owned())?;
    let statuses = top_what_ifs
        .iter()
        .filter_map(|item| item.pointer("/status").and_then(Value::as_str))
        .collect::<BTreeSet<_>>();
    for status in ["blocked", "in_progress", "open"] {
        if !statuses.contains(status) {
            return Err(format!(
                "bv robot-insights fixture missing {status} example"
            ));
        }
    }
    let serialized =
        serde_json::to_string(&fixture).map_err(|error| format!("serialize fixture: {error}"))?;
    let lowered = serialized.to_ascii_lowercase();
    for forbidden in [
        "/users/",
        "/private/",
        "/home/",
        "raw stdout",
        "raw stderr",
        "\"claimcommandaction\"",
        "\"safe_to_claim\"",
        "claimable",
        "stack trace",
    ] {
        if lowered.contains(forbidden) {
            return Err(format!(
                "bv robot-insights fixture leaked forbidden detail {forbidden}"
            ));
        }
    }
    if fixture
        .pointer("/supportBundleSafety/rawTrackerRowsIncluded")
        .and_then(Value::as_bool)
        != Some(false)
        || fixture
            .pointer("/supportBundleSafety/rawStdoutIncluded")
            .and_then(Value::as_bool)
            != Some(false)
        || fixture
            .pointer("/supportBundleSafety/privatePathsIncluded")
            .and_then(Value::as_bool)
            != Some(false)
        || fixture
            .pointer("/supportBundleSafety/unboundedMapsIncluded")
            .and_then(Value::as_bool)
            != Some(false)
    {
        return Err("bv robot-insights fixture must remain support-bundle safe".into());
    }

    Ok(())
}

#[test]
fn work_packet_tracker_mismatch_blocks_claim_mutation() -> TestResult {
    // bd-1tlcd.3: when tracker integrity says Beads DB/JSONL state is not
    // authoritative, a visible ready candidate must stay explainable but cannot
    // become a Beads claim recommendation. This pins the real crowded-checkout
    // contradiction where BV-style ranking can surface a candidate while the
    // tracker itself requires repair before mutation.
    let fixture_path = repo_root()
        .join("tests")
        .join("fixtures")
        .join("swarm_work_packet")
        .join("tracker_mismatch.json");
    let fixture = read_json(&fixture_path)?;
    let packet = fixture
        .pointer("/data")
        .ok_or_else(|| "tracker mismatch fixture missing response data".to_owned())?;

    if string_field(packet, "/schema", "tracker mismatch fixture")? != "ee.swarm.work_packet.v1" {
        return Err("tracker mismatch fixture schema drifted".into());
    }
    if packet
        .pointer("/trackerIntegrity/brReadsAuthoritative")
        .and_then(Value::as_bool)
        != Some(false)
    {
        return Err("tracker mismatch fixture must mark brReadsAuthoritative=false".into());
    }
    if packet
        .pointer("/trackerIntegrity/requiresCandidateDowngrade")
        .and_then(Value::as_bool)
        != Some(true)
    {
        return Err(
            "tracker mismatch fixture must require candidate downgrade before claim".into(),
        );
    }
    if packet
        .pointer("/recommendedAction/safeToClaim")
        .and_then(Value::as_bool)
        != Some(false)
        || packet.pointer("/safeToClaim").and_then(Value::as_bool) != Some(false)
    {
        return Err("tracker mismatch fixture must never recommend safeToClaim=true".into());
    }
    if string_field(
        packet,
        "/recommendedAction/action",
        "tracker mismatch fixture",
    )? != "blocked_no_action"
    {
        return Err("tracker mismatch fixture must use blocked_no_action".into());
    }
    if string_field(
        packet,
        "/candidates/0/decision",
        "tracker mismatch fixture candidate",
    )? == "safe_to_claim"
    {
        return Err("tracker mismatch candidate must not keep a safe_to_claim decision".into());
    }

    let rendered = serde_json::to_string(packet)
        .map_err(|error| format!("serialize tracker mismatch fixture: {error}"))?;
    for forbidden in ["br update", "--status in_progress", "br claim"] {
        if rendered.contains(forbidden) {
            return Err(format!(
                "tracker mismatch fixture contains forbidden claim command marker {forbidden}"
            ));
        }
    }

    let actions = packet
        .pointer("/recommendedAction/suggestedCommandActions")
        .and_then(Value::as_array)
        .ok_or_else(|| "tracker mismatch fixture missing suggestedCommandActions".to_owned())?;
    if actions.is_empty() {
        return Err("tracker mismatch fixture must keep read-only inspection actions".into());
    }
    for (index, action) in actions.iter().enumerate() {
        if action.pointer("/mutatesState").and_then(Value::as_bool) != Some(false) {
            return Err(format!("tracker mismatch action {index} must be read-only"));
        }
    }
    let action_ids = actions
        .iter()
        .filter_map(|action| action.pointer("/commandId").and_then(Value::as_str))
        .collect::<BTreeSet<_>>();
    for expected in [
        "bead_show_candidate_stale_safe",
        "beads_doctor_no_db",
        "swarm_brief_refresh",
    ] {
        if !action_ids.contains(expected) {
            return Err(format!(
                "tracker mismatch fixture missing action {expected}"
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
    let mut ready_pressure_case_count = 0_usize;
    let mut liveness_case_count = 0_usize;
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
        let pressures = case
            .get("readyReservationPressure")
            .and_then(Value::as_array)
            .ok_or_else(|| format!("{case_name} missing readyReservationPressure array"))?;
        for (index, pressure) in pressures.iter().enumerate() {
            ready_pressure_case_count += 1;
            let context = format!("{case_name}.readyReservationPressure[{index}]");
            for field in [
                "beadId",
                "title",
                "priority",
                "action",
                "severity",
                "likelySurfaces",
                "reservationHolders",
                "exclusiveReservationCount",
                "sharedReservationCount",
                "earliestExpiresAt",
                "maxRiskScore",
                "riskFactors",
                "evidence",
                "suggestedCommands",
            ] {
                if pressure.get(field).is_none() {
                    return Err(format!("{context} missing {field}"));
                }
            }
        }
        let liveness_rows = case
            .get("stalledBeadLiveness")
            .and_then(Value::as_array)
            .ok_or_else(|| format!("{case_name} missing stalledBeadLiveness array"))?;
        for (index, liveness) in liveness_rows.iter().enumerate() {
            liveness_case_count += 1;
            let context = format!("{case_name}.stalledBeadLiveness[{index}]");
            for field in [
                "beadId",
                "title",
                "assignee",
                "priority",
                "posture",
                "action",
                "severity",
                "lastActivityAt",
                "ageSecondsPresent",
                "evidenceSources",
                "evidence",
                "suggestedCommands",
                "mustNotDo",
            ] {
                if liveness.get(field).is_none() {
                    return Err(format!("{context} missing {field}"));
                }
            }
        }
    }

    if ownership_case_count == 0 {
        return Err("swarm brief golden must cover at least one file surface risk".into());
    }
    if ready_pressure_case_count == 0 {
        return Err("swarm brief golden must cover at least one ready reservation pressure".into());
    }
    if liveness_case_count == 0 {
        return Err("swarm brief golden must cover at least one stalled bead liveness row".into());
    }

    Ok(())
}

#[test]
fn swarm_brief_fixture_covers_stalled_liveness_posture_matrix() -> TestResult {
    let fixtures = fixture_examples()?;
    let swarm_brief = fixtures
        .get("ee.swarm.brief.v1")
        .ok_or_else(|| "fixture manifest missing ee.swarm.brief.v1".to_owned())?;
    let rows = swarm_brief
        .get("stalledBeadLiveness")
        .and_then(Value::as_array)
        .ok_or_else(|| "ee.swarm.brief.v1 fixture missing stalledBeadLiveness".to_owned())?;
    let postures = rows
        .iter()
        .filter_map(|row| row.get("posture").and_then(Value::as_str))
        .collect::<BTreeSet<_>>();
    for expected in [
        "active",
        "blocked_with_evidence",
        "human_approval_required",
        "quiet_but_recent",
        "reclaim_candidate",
        "stale_needs_message",
    ] {
        if !postures.contains(expected) {
            return Err(format!(
                "ee.swarm.brief.v1 fixture must cover stalled bead liveness posture {expected}"
            ));
        }
    }

    for row in rows {
        let posture = row
            .get("posture")
            .and_then(Value::as_str)
            .ok_or_else(|| "stalledBeadLiveness fixture row missing posture".to_owned())?;
        if matches!(
            posture,
            "blocked_with_evidence"
                | "human_approval_required"
                | "quiet_but_recent"
                | "stale_needs_message"
        ) {
            let suggested_commands = row
                .get("suggestedCommands")
                .and_then(Value::as_array)
                .ok_or_else(|| {
                    format!("stalledBeadLiveness posture {posture} missing suggestedCommands")
                })?;
            if suggested_commands
                .iter()
                .filter_map(Value::as_str)
                .any(|command| command.contains("--status open"))
            {
                return Err(format!(
                    "stalledBeadLiveness posture {posture} must not include reopen guidance"
                ));
            }
        }
    }

    let support_bundle = fixtures
        .get("ee.support_bundle.swarm_brief_summary.v1")
        .ok_or_else(|| {
            "fixture manifest missing ee.support_bundle.swarm_brief_summary.v1".to_owned()
        })?;
    if support_bundle
        .pointer("/counts/stalledBeadLivenessCount")
        .and_then(Value::as_u64)
        != Some(rows.len() as u64)
    {
        return Err("support-bundle liveness count must match swarm brief fixture rows".into());
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
