//! K2 docs/schema gate for public response envelopes (bd-17c65.11.2).
//!
//! The repo intentionally keeps these JSON Schemas hand-curated in
//! docs/schemas instead of generating noisy Rust-derived schemas. This test
//! protects the useful part of that contract: schema files parse, `ee schema
//! export` serves the same documents, and representative emitted responses
//! validate against their per-envelope schemas.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::path::{Path, PathBuf};

use ee::cass::import::{CassImportReport, ImportSessionStatus, ImportedCassSession};
use ee::core::completion_audit::{
    COMPLETION_AUDIT_CHECKLIST_SCHEMA_V1, COMPLETION_AUDIT_REPORT_SCHEMA_V1,
    build_completion_audit_report_for_workspace, extract_completion_checklist,
};
use ee::core::curate::{
    CURATE_CANDIDATES_SCHEMA_V1, CurateCandidatesFilter, CurateCandidatesReport,
};
use ee::core::memory::{
    MemoryDetails, MemoryListFilter, MemoryListReport, MemoryShowReport, MemorySummary,
};
use ee::core::swarm_next_action::SWARM_NEXT_ACTION_SCHEMA_V1;
use ee::db::{GraphSnapshotType, StoredMemory};
use ee::graph::{GRAPH_EXPORT_SCHEMA_V1, GraphExportFormat, GraphExportReport, GraphExportStatus};
use ee::models::{DomainError, IMPORT_CASS_SCHEMA_V1, RESPONSE_SCHEMA_V1};
use ee::output::{
    error_response_json, render_curate_candidates_json, render_mcp_manifest_json,
    render_memory_list_json, render_memory_show_json, render_schema_export_json,
};
use serde_json::{Value, json};

type TestResult = Result<(), String>;

const SCHEMA_DOCS: &[(&str, &str)] = &[
    ("ee.response.v1", "ee.response.v1.json"),
    ("ee.error.v2", "ee.error.v2.json"),
    ("ee.pack.v2", "ee.pack.v2.json"),
    ("ee.search.v1", "ee.search.v1.json"),
    ("ee.memory.show.v1", "ee.memory.show.v1.json"),
    ("ee.memory.list.v1", "ee.memory.list.v1.json"),
    ("ee.status.v1", "ee.status.v1.json"),
    ("ee.doctor.v1", "ee.doctor.v1.json"),
    ("ee.capabilities.v1", "ee.capabilities.v1.json"),
    (SWARM_NEXT_ACTION_SCHEMA_V1, "ee.swarm_next_action.v1.json"),
    ("ee.import.cass.v1", "ee.import.cass.v1.json"),
    ("ee.export.v1", "ee.export.v1.json"),
    ("ee.curate.candidates.v1", "ee.curate.candidates.v1.json"),
    ("ee.graph.export.v1", "ee.graph.export.v1.json"),
    (
        "ee.graph.snapshot_prune.v1",
        "ee.graph.snapshot_prune.v1.json",
    ),
    (
        ee::core::witness_retention::WITNESS_PRUNE_REPORT_SCHEMA_V1,
        "ee.graph.witness_prune_report.v1.json",
    ),
    ("ee.db.inspect.v1", "ee.db.inspect.v1.json"),
    (
        COMPLETION_AUDIT_CHECKLIST_SCHEMA_V1,
        "ee.completion_audit.checklist.v1.json",
    ),
    (
        COMPLETION_AUDIT_REPORT_SCHEMA_V1,
        "ee.completion_audit.report.v1.json",
    ),
    ("ee.mcp.manifest.v1", "ee.mcp.manifest.v1.json"),
];

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn schema_path(file_name: &str) -> PathBuf {
    repo_root().join("docs").join("schemas").join(file_name)
}

fn fixture_path(relative: &str) -> PathBuf {
    repo_root().join("tests").join("fixtures").join(relative)
}

fn read_json(path: &Path) -> Result<Value, String> {
    let text =
        fs::read_to_string(path).map_err(|error| format!("read {}: {error}", path.display()))?;
    serde_json::from_str(&text).map_err(|error| format!("parse {}: {error}", path.display()))
}

fn schema_doc(schema_id: &str) -> Result<Value, String> {
    let (_, file_name) = SCHEMA_DOCS
        .iter()
        .find(|(id, _)| *id == schema_id)
        .ok_or_else(|| format!("test missing schema doc mapping for {schema_id}"))?;
    read_json(&schema_path(file_name))
}

#[test]
fn docs_schema_files_are_strict_draft_2020_12_documents() -> TestResult {
    for (schema_id, file_name) in SCHEMA_DOCS {
        let schema = read_json(&schema_path(file_name))?;
        ensure_json_str(
            &schema,
            "/$schema",
            "https://json-schema.org/draft/2020-12/schema",
        )?;
        let id = schema
            .get("$id")
            .and_then(Value::as_str)
            .ok_or_else(|| format!("{file_name} missing string $id"))?;
        if !id.starts_with("https://eidetic-engine/schemas/") || !id.ends_with(file_name) {
            return Err(format!("{file_name} has non-canonical $id {id:?}"));
        }
        ensure_json_str(&schema, "/title", schema_id)?;
        ensure_json_bool(&schema, "/additionalProperties", false)?;
        ensure_field_presets(schema_id, &schema)?;
    }
    Ok(())
}

#[test]
fn curate_disposition_schema_documents_structural_adjustments() -> TestResult {
    let schema_id = "ee.curate.disposition.v1";
    let schema = read_json(&schema_path("ee.curate.disposition.v1.json"))?;

    ensure_json_str(
        &schema,
        "/$schema",
        "https://json-schema.org/draft/2020-12/schema",
    )?;
    ensure_json_str(&schema, "/title", schema_id)?;
    ensure_json_bool(&schema, "/additionalProperties", false)?;
    ensure_field_presets(schema_id, &schema)?;
    ensure_json_str(
        &schema,
        "/properties/data/properties/schema/const",
        schema_id,
    )?;
    ensure_json_str(
        &schema,
        "/properties/data/properties/structuralAdjustments/items/$ref",
        "#/$defs/structuralAdjustment",
    )?;

    let required = schema
        .pointer("/$defs/structuralAdjustment/required")
        .and_then(Value::as_array)
        .ok_or_else(|| "structuralAdjustment.required must be an array".to_owned())?
        .iter()
        .filter_map(Value::as_str)
        .collect::<Vec<_>>();
    for field in [
        "candidateId",
        "memoryId",
        "onionLayer",
        "maxLayer",
        "isArticulationPoint",
        "baseDecay",
        "structuralMultiplier",
        "adjustedDecay",
        "adjustedTtlThresholdSeconds",
        "rationale",
    ] {
        if !required.contains(&field) {
            return Err(format!("structuralAdjustment.required missing {field}"));
        }
    }

    Ok(())
}

#[test]
fn memory_impact_analysis_schema_documents_revision_frontiers() -> TestResult {
    let schema_id = "ee.memory.impact_analysis.v1";
    let schema = read_json(&schema_path("ee.memory.impact_analysis.v1.json"))?;

    ensure_json_str(
        &schema,
        "/$schema",
        "https://json-schema.org/draft/2020-12/schema",
    )?;
    ensure_json_str(&schema, "/title", schema_id)?;
    ensure_json_bool(&schema, "/additionalProperties", false)?;
    ensure_field_presets(schema_id, &schema)?;
    ensure_json_str(&schema, "/properties/schema/const", schema_id)?;
    ensure_json_str(
        &schema,
        "/properties/impactAnalysis/$ref",
        "#/$defs/impactAnalysis",
    )?;
    ensure_json_str(
        &schema,
        "/properties/frontiers/items/$ref",
        "#/$defs/frontierItem",
    )?;
    ensure_json_str(
        &schema,
        "/$defs/frontierItem/properties/evidence/properties/algorithm/const",
        "dominance_frontiers",
    )?;

    let impact_required = schema
        .pointer("/$defs/impactAnalysis/required")
        .and_then(Value::as_array)
        .ok_or_else(|| "impactAnalysis.required must be an array".to_owned())?
        .iter()
        .filter_map(Value::as_str)
        .collect::<Vec<_>>();
    for field in [
        "immediateDominator",
        "dominanceFrontier",
        "affectedMemoryCount",
        "validationStatus",
    ] {
        if !impact_required.contains(&field) {
            return Err(format!("impactAnalysis.required missing {field}"));
        }
    }

    Ok(())
}

#[test]
fn graph_snapshot_prune_schema_documents_archived_row_safety() -> TestResult {
    let schema_id = "ee.graph.snapshot_prune.v1";
    let schema = read_json(&schema_path("ee.graph.snapshot_prune.v1.json"))?;

    ensure_json_str(
        &schema,
        "/$schema",
        "https://json-schema.org/draft/2020-12/schema",
    )?;
    ensure_json_str(&schema, "/title", schema_id)?;
    ensure_json_bool(&schema, "/additionalProperties", false)?;
    ensure_field_presets(schema_id, &schema)?;
    ensure_json_str(&schema, "/properties/schema/const", schema_id)?;
    ensure_json_str(
        &schema,
        "/properties/command/const",
        "maintenance graph-snapshot-prune",
    )?;
    ensure_json_str(
        &schema,
        "/properties/candidates/items/$ref",
        "#/$defs/pruneCandidate",
    )?;
    ensure_json_str(&schema, "/properties/lock/$ref", "#/$defs/pruneLock")?;
    ensure_json_str(
        &schema,
        "/$defs/pruneLock/properties/resourceType/const",
        "graph_snapshot_prune",
    )?;

    let required = schema
        .pointer("/required")
        .and_then(Value::as_array)
        .ok_or_else(|| "graph snapshot prune required must be an array".to_owned())?
        .iter()
        .filter_map(Value::as_str)
        .collect::<Vec<_>>();
    for field in [
        "workspaceId",
        "graphType",
        "dryRun",
        "retentionDays",
        "cutoffTimestamp",
        "candidateCount",
        "prunedCount",
        "candidateBytes",
        "prunedBytes",
        "lock",
        "degraded",
    ] {
        if !required.contains(&field) {
            return Err(format!("graph snapshot prune required missing {field}"));
        }
    }

    let example = schema
        .pointer("/examples/0")
        .ok_or_else(|| "graph snapshot prune schema must include an example".to_owned())?;
    validate_json_schema(example, &schema, &schema, "$.examples[0]")
        .map_err(|error| format!("graph snapshot prune example invalid: {error}"))?;

    Ok(())
}

#[test]
fn graph_witness_prune_schema_documents_active_snapshot_safety() -> TestResult {
    let schema_id = ee::core::witness_retention::WITNESS_PRUNE_REPORT_SCHEMA_V1;
    let schema = read_json(&schema_path("ee.graph.witness_prune_report.v1.json"))?;

    ensure_json_str(
        &schema,
        "/$schema",
        "https://json-schema.org/draft/2020-12/schema",
    )?;
    ensure_json_str(&schema, "/title", schema_id)?;
    ensure_json_bool(&schema, "/additionalProperties", false)?;
    ensure_field_presets(schema_id, &schema)?;
    ensure_json_str(&schema, "/properties/schema/const", schema_id)?;
    ensure_json_str(
        &schema,
        "/properties/command/const",
        "maintenance graph-witnesses-prune",
    )?;
    ensure_json_str(
        &schema,
        "/properties/report/$ref",
        "#/$defs/witnessPruneReport",
    )?;
    ensure_json_str(
        &schema,
        "/properties/summary/$ref",
        "#/$defs/witnessPruneSummaryWithDelete",
    )?;
    ensure_json_str(
        &schema,
        "/$defs/activeSnapshotReason/properties/code/const",
        "active_snapshot",
    )?;
    ensure_json_str(&schema, "/$defs/pruneAction/properties/kind/const", "prune")?;

    let required = schema
        .pointer("/required")
        .and_then(Value::as_array)
        .ok_or_else(|| "graph witness prune required must be an array".to_owned())?
        .iter()
        .filter_map(Value::as_str)
        .collect::<Vec<_>>();
    for field in [
        "workspaceId",
        "dryRun",
        "durableMutation",
        "deletedCount",
        "report",
        "summary",
    ] {
        if !required.contains(&field) {
            return Err(format!("graph witness prune required missing {field}"));
        }
    }

    let example = schema
        .pointer("/examples/0")
        .ok_or_else(|| "graph witness prune schema must include an example".to_owned())?;
    validate_json_schema(example, &schema, &schema, "$.examples[0]")
        .map_err(|error| format!("graph witness prune example invalid: {error}"))?;

    Ok(())
}

#[test]
fn db_inspect_schema_documents_read_only_database_surfaces() -> TestResult {
    let schema_id = "ee.db.inspect.v1";
    let schema = read_json(&schema_path("ee.db.inspect.v1.json"))?;

    ensure_json_str(
        &schema,
        "/$schema",
        "https://json-schema.org/draft/2020-12/schema",
    )?;
    ensure_json_str(&schema, "/title", schema_id)?;
    ensure_json_bool(&schema, "/additionalProperties", false)?;
    ensure_field_presets(schema_id, &schema)?;
    ensure_json_str(&schema, "/properties/schema/const", RESPONSE_SCHEMA_V1)?;
    ensure_json_str(
        &schema,
        "/properties/data/properties/command/type",
        "string",
    )?;
    ensure_json_str(&schema, "/properties/data/properties/report/type", "object")?;

    let examples = schema
        .pointer("/examples")
        .and_then(Value::as_array)
        .ok_or_else(|| "db inspect schema examples must be an array".to_owned())?;
    let commands = examples
        .iter()
        .filter_map(|example| {
            example
                .pointer("/data/command")
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .collect::<Vec<_>>();
    for command in ["db status", "db inspect", "db check-integrity"] {
        if !commands.iter().any(|value| value == command) {
            return Err(format!("db inspect schema examples missing {command}"));
        }
    }

    for (index, example) in examples.iter().enumerate() {
        validate_json_schema(example, &schema, &schema, &format!("$.examples[{index}]"))
            .map_err(|error| format!("db inspect example {index} invalid: {error}"))?;
    }

    Ok(())
}

fn ensure_field_presets(schema_id: &str, schema: &Value) -> TestResult {
    let presets = schema
        .get("field_presets")
        .and_then(Value::as_object)
        .ok_or_else(|| format!("{schema_id} missing field_presets object"))?;
    for preset in ["minimal", "summary", "standard", "full"] {
        let fields = presets
            .get(preset)
            .and_then(Value::as_array)
            .ok_or_else(|| format!("{schema_id} field_presets.{preset} must be an array"))?;
        if fields.is_empty() {
            return Err(format!(
                "{schema_id} field_presets.{preset} must not be empty"
            ));
        }
    }
    let full = presets
        .get("full")
        .and_then(Value::as_array)
        .ok_or_else(|| format!("{schema_id} field_presets.full missing"))?;
    if !full.iter().any(|value| value.as_str() == Some("*")) {
        return Err(format!("{schema_id} field_presets.full must include `*`"));
    }
    Ok(())
}

#[test]
fn public_schema_exports_match_docs_schema_files() -> TestResult {
    for (schema_id, file_name) in SCHEMA_DOCS {
        let exported: Value = serde_json::from_str(&render_schema_export_json(Some(schema_id)))
            .map_err(|error| format!("schema export {schema_id} did not parse: {error}"))?;
        let documented = read_json(&schema_path(file_name))?;
        if exported != documented {
            return Err(format!(
                "ee schema export {schema_id} drifted from docs/schemas/{file_name}"
            ));
        }
    }
    Ok(())
}

#[test]
fn canonical_response_fixtures_match_docs_schemas() -> TestResult {
    let fixture_cases = [
        (
            "ee.response.v1",
            read_json(&fixture_path("golden/agent/status.json.golden"))?,
        ),
        (
            "ee.pack.v2",
            read_json(&fixture_path("golden/agent/context_pack.json.golden"))?,
        ),
        (
            "ee.search.v1",
            read_json(&fixture_path(
                "golden/agent/search_deterministic_ranking.json.golden",
            ))?,
        ),
        (
            "ee.status.v1",
            read_json(&fixture_path("golden/status/status_json.golden"))?,
        ),
        (
            "ee.doctor.v1",
            read_json(&fixture_path("golden/doctor/doctor_json.golden"))?,
        ),
        (
            "ee.capabilities.v1",
            read_json(&fixture_path(
                "golden/capabilities/capabilities_json.golden",
            ))?,
        ),
        ("ee.error.v2", domain_error_sample()?),
        ("ee.memory.show.v1", memory_show_sample()?),
        ("ee.memory.list.v1", memory_list_sample()?),
        ("ee.import.cass.v1", import_cass_sample()),
        ("ee.export.v1", export_sample()),
        ("ee.curate.candidates.v1", curate_candidates_sample()?),
        (SWARM_NEXT_ACTION_SCHEMA_V1, swarm_next_action_sample()),
        ("ee.graph.export.v1", graph_export_sample()),
        ("ee.db.inspect.v1", db_inspect_sample()?),
        (
            "ee.mcp.manifest.v1",
            serde_json::from_str(&render_mcp_manifest_json())
                .map_err(|error| format!("mcp manifest sample invalid JSON: {error}"))?,
        ),
        (
            COMPLETION_AUDIT_CHECKLIST_SCHEMA_V1,
            serde_json::to_value(extract_completion_checklist(
                "schema-test-objective",
                "Read AGENTS.md, coordinate with Agent Mail, and verify with `cargo fmt --check` through RCH.",
            ))
            .map_err(|error| format!("completion audit sample invalid JSON: {error}"))?,
        ),
        (
            COMPLETION_AUDIT_REPORT_SCHEMA_V1,
            serde_json::from_str(&ee::output::render_completion_audit_json(
                &build_completion_audit_report_for_workspace(
                    "schema-test-objective",
                    "Read AGENTS.md, coordinate with Agent Mail, and verify with `cargo fmt --check` through RCH.",
                    Path::new("."),
                    None,
                ),
            ))
            .map_err(|error| format!("completion audit report sample invalid JSON: {error}"))?,
        ),
    ];

    for (schema_id, response) in fixture_cases {
        let schema = schema_doc(schema_id)?;
        validate_json_schema(&response, &schema, &schema, "$")
            .map_err(|error| format!("{schema_id}: {error}"))?;
    }

    Ok(())
}

#[test]
fn swarm_next_action_golden_snapshots_match_schema() -> TestResult {
    let schema = schema_doc(SWARM_NEXT_ACTION_SCHEMA_V1)?;
    for fixture_name in [
        "clean",
        "dirty",
        "degraded_beads",
        "degraded_mail",
        "saturated_rch",
    ] {
        let response = read_json(&fixture_path(&format!(
            "golden/swarm_next_action/{fixture_name}.json.golden"
        )))?;
        ensure_json_str(&response, "/data/schema", SWARM_NEXT_ACTION_SCHEMA_V1)?;
        validate_json_schema(&response, &schema, &schema, "$")
            .map_err(|error| format!("swarm next-action {fixture_name}: {error}"))?;
    }
    Ok(())
}

fn domain_error_sample() -> Result<Value, String> {
    serde_json::from_str(&error_response_json(&DomainError::Storage {
        message: "Database file corrupted.".to_string(),
        repair: Some("ee doctor --fix-plan --json".to_string()),
    }))
    .map_err(|error| error.to_string())
}

fn swarm_next_action_sample() -> Value {
    json!({
        "schema": RESPONSE_SCHEMA_V1,
        "success": true,
        "data": {
            "schema": SWARM_NEXT_ACTION_SCHEMA_V1,
            "workspace": "/repo",
            "redactionStatus": "counts_ids_statuses_paths_redacted_no_mail_body_no_file_content",
            "inputs": {
                "sourceCount": 5,
                "readyBeadCount": 2,
                "inProgressBeadCount": 1,
                "blockedBeadCount": 3,
                "bvTopPickCount": 1
            },
            "candidates": [{
                "id": "bd-123",
                "title": "Add next-action schema",
                "source": "bv_top_pick",
                "scoreMilli": 900,
                "status": "open",
                "priority": 2,
                "assignee": null,
                "blockedBy": [],
                "actionHint": "inspect_and_reserve_before_editing"
            }],
            "coordination": {
                "activeReservationCount": 1,
                "reservationHolders": ["GoldenCompass"],
                "unreadInboxCount": 0,
                "ackRequiredCount": 0
            },
            "checkout": {
                "dirtyPathCount": 1,
                "dirtyPaths": ["docs/schemas/ee.swarm_next_action.v1.json"]
            },
            "verification": {
                "rchSourceEnabled": true,
                "remoteOnlyRequired": true,
                "remoteOnlySafe": false,
                "healthyWorkerCount": 1,
                "activeRemoteBuildCount": 4,
                "queuedRemoteBuildCount": 0,
                "slotsAvailable": 0
            },
            "environment": {
                "cargoTargetExternalized": true,
                "tmpdirExternalized": true,
                "externalAgentSpacePresent": true,
                "diskPressureHintCount": 0
            },
            "degraded": []
        },
        "degraded": []
    })
}

fn memory_show_sample() -> Result<Value, String> {
    let report = MemoryShowReport::found(MemoryDetails {
        memory: stored_memory_sample(),
        tags: vec!["release".to_string(), "formatting".to_string()],
    });
    serde_json::from_str(&render_memory_show_json(&report)).map_err(|error| error.to_string())
}

fn memory_list_sample() -> Result<Value, String> {
    let report = MemoryListReport::success(
        vec![MemorySummary {
            id: "mem_00000000000000000000010001".to_string(),
            level: "procedural".to_string(),
            kind: "rule".to_string(),
            content: "Run cargo fmt --check before release.".to_string(),
            content_truncated: false,
            confidence: 0.92,
            provenance_uri: Some("file://AGENTS.md#L1".to_string()),
            is_tombstoned: false,
            valid_from: None,
            valid_to: None,
            validity_status: "active".to_string(),
            validity_window_kind: "always".to_string(),
            created_at: "2026-05-13T00:00:00Z".to_string(),
        }],
        1,
        false,
        MemoryListFilter {
            level: None,
            tag: Some("release".to_string()),
            include_tombstoned: true,
        },
    );
    serde_json::from_str(&render_memory_list_json(&report)).map_err(|error| error.to_string())
}

fn stored_memory_sample() -> StoredMemory {
    StoredMemory {
        id: "mem_00000000000000000000010001".to_string(),
        workspace_id: "ws_00000000000000000000010001".to_string(),
        level: "procedural".to_string(),
        kind: "rule".to_string(),
        content: "Run cargo fmt --check before release.".to_string(),
        workflow_id: None,
        confidence: 0.92,
        utility: 0.8,
        importance: 0.7,
        provenance_uri: Some("file://AGENTS.md#L1".to_string()),
        trust_class: "human_explicit".to_string(),
        trust_subclass: Some("project-rule".to_string()),
        provenance_chain_hash: Some("blake3:fixture".to_string()),
        provenance_chain_hash_version: "v1".to_string(),
        provenance_verification_status: "verified".to_string(),
        provenance_verified_at: None,
        provenance_verification_note: None,
        created_at: "2026-05-13T00:00:00Z".to_string(),
        updated_at: "2026-05-13T00:00:00Z".to_string(),
        tombstoned_at: None,
        valid_from: None,
        valid_to: None,
    }
}

fn import_cass_sample() -> Value {
    let report = CassImportReport {
        schema: IMPORT_CASS_SCHEMA_V1,
        workspace_path: "/tmp/workspace".to_string(),
        database_path: Some("/tmp/workspace/.ee/ee.db".to_string()),
        source_id: "cass://fixture".to_string(),
        ledger_id: Some("ledger_fixture".to_string()),
        dry_run: true,
        since: None,
        sessions_discovered: 1,
        sessions_imported: 0,
        sessions_skipped: 0,
        spans_imported: 0,
        index_jobs_queued: 0,
        index_required_action: None,
        status: "dry_run".to_string(),
        sessions: vec![ImportedCassSession {
            source_path: "/tmp/session.json".to_string(),
            session_id: Some("session_fixture".to_string()),
            index_job_id: None,
            status: ImportSessionStatus::WouldImport,
            spans_imported: 0,
            message_count: Some(3),
            missing_metadata: Vec::new(),
        }],
    };
    json!({
        "schema": RESPONSE_SCHEMA_V1,
        "success": true,
        "data": report.data_json(),
    })
}

fn export_sample() -> Value {
    json!({
        "schema": RESPONSE_SCHEMA_V1,
        "success": true,
        "data": {
            "schema": "ee.export.report.v1",
            "command": "export",
            "version": "0.1.0",
            "status": "dry_run",
            "dryRun": true,
            "workspacePath": "/tmp/workspace",
            "workspaceId": "ws_fixture",
            "databasePath": "/tmp/workspace/.ee/ee.db",
            "outputPath": "/tmp/workspace/.ee/backups/backup_fixture",
            "manifestPath": "/tmp/workspace/.ee/backups/backup_fixture/manifest.json",
            "recordsPath": "/tmp/workspace/.ee/backups/backup_fixture/records.jsonl",
            "manifestHash": null,
            "recordsHash": null,
            "redactionLevel": "standard",
            "exportScope": "workspace",
            "counts": {
                "totalRecords": 0,
                "memoryRecords": 0,
                "linkRecords": 0,
                "tagRecords": 0,
                "auditRecords": 0
            },
            "provenance": {
                "source": "backup_jsonl_export",
                "backupSchema": "ee.backup.create.v1",
                "backupId": "backup_fixture"
            },
            "verificationStatus": "not_run",
            "artifacts": [],
            "degraded": []
        }
    })
}

fn curate_candidates_sample() -> Result<Value, String> {
    let report = CurateCandidatesReport {
        schema: CURATE_CANDIDATES_SCHEMA_V1,
        command: "curate candidates",
        version: "0.1.0",
        workspace_id: "ws_fixture".to_string(),
        workspace_path: "/tmp/workspace".to_string(),
        database_path: "/tmp/workspace/.ee/ee.db".to_string(),
        total_count: 0,
        returned_count: 0,
        limit: 25,
        offset: 0,
        truncated: false,
        durable_mutation: false,
        filter: CurateCandidatesFilter {
            candidate_type: None,
            status: None,
            target_memory_id: None,
            sort: "priority".to_string(),
            group_duplicates: true,
        },
        candidates: Vec::new(),
        degraded: Vec::new(),
        next_action: "ee curate candidates --json".to_string(),
    };
    serde_json::from_str(&render_curate_candidates_json(&report)).map_err(|error| error.to_string())
}

fn graph_export_sample() -> Value {
    let report = GraphExportReport {
        schema: GRAPH_EXPORT_SCHEMA_V1,
        version: "0.1.0",
        status: GraphExportStatus::NoSnapshot,
        format: GraphExportFormat::Mermaid,
        workspace_id: "ws_fixture".to_string(),
        graph_type: GraphSnapshotType::MemoryLinks.as_str().to_string(),
        snapshot: None,
        node_count: 0,
        edge_count: 0,
        diagram: String::new(),
        degraded: Vec::new(),
    };
    json!({
        "schema": RESPONSE_SCHEMA_V1,
        "success": true,
        "data": report.data_json(),
    })
}

fn db_inspect_sample() -> Result<Value, String> {
    let schema = read_json(&schema_path("ee.db.inspect.v1.json"))?;
    schema
        .pointer("/examples/0")
        .cloned()
        .ok_or_else(|| "ee.db.inspect.v1 schema missing examples[0]".to_owned())
}

fn validate_json_schema(
    value: &Value,
    schema: &Value,
    root_schema: &Value,
    path: &str,
) -> TestResult {
    if let Some(reference) = schema.get("$ref").and_then(Value::as_str) {
        let target = resolve_local_ref(root_schema, reference)?;
        return validate_json_schema(value, target, root_schema, path);
    }

    if let Some(options) = schema.get("oneOf").and_then(Value::as_array) {
        if options
            .iter()
            .any(|candidate| validate_json_schema(value, candidate, root_schema, path).is_ok())
        {
            return Ok(());
        }
        return Err(format!("{path} did not match any oneOf branch"));
    }

    if let Some(expected) = schema.get("const") {
        if value != expected {
            return Err(format!("{path} expected const {expected}, got {value}"));
        }
    }

    if let Some(enum_values) = schema.get("enum").and_then(Value::as_array) {
        if !enum_values.iter().any(|candidate| candidate == value) {
            return Err(format!(
                "{path} value {value} is not in enum {enum_values:?}"
            ));
        }
    }

    let expected_types = schema_types(schema);
    if !expected_types.is_empty() {
        if !expected_types
            .iter()
            .any(|expected_type| json_type_matches(value, expected_type))
        {
            return Err(format!(
                "{path} expected type {:?}, got {}",
                expected_types,
                json_type_name(value)
            ));
        }
        if value.is_null() {
            return Ok(());
        }
    }

    if let Some(object) = value.as_object() {
        if let Some(required) = schema.get("required").and_then(Value::as_array) {
            for field in required {
                let field = field
                    .as_str()
                    .ok_or_else(|| format!("{path} schema required entry is not a string"))?;
                if !object.contains_key(field) {
                    return Err(format!("{path} missing required field {field}"));
                }
            }
        }

        let properties = schema.get("properties").and_then(Value::as_object);
        for (key, child) in object {
            let child_path = format!("{path}.{key}");
            if let Some(property_schema) = properties.and_then(|props| props.get(key)) {
                validate_json_schema(child, property_schema, root_schema, &child_path)?;
                continue;
            }
            match schema.get("additionalProperties") {
                Some(Value::Bool(false)) => {
                    return Err(format!("{path} contains unexpected field {key}"));
                }
                Some(Value::Object(_)) => {
                    validate_json_schema(
                        child,
                        &schema["additionalProperties"],
                        root_schema,
                        &child_path,
                    )?;
                }
                Some(Value::Bool(true)) | None => {}
                Some(other) => {
                    return Err(format!("{path} unsupported additionalProperties: {other}"));
                }
            }
        }
    }

    if let Some(items) = value.as_array() {
        if let Some(item_schema) = schema.get("items") {
            for (index, item) in items.iter().enumerate() {
                validate_json_schema(item, item_schema, root_schema, &format!("{path}[{index}]"))?;
            }
        }
    }

    Ok(())
}

fn resolve_local_ref<'a>(root: &'a Value, reference: &str) -> Result<&'a Value, String> {
    let pointer = reference
        .strip_prefix('#')
        .ok_or_else(|| format!("only local JSON Schema refs are supported, got {reference}"))?;
    root.pointer(pointer)
        .ok_or_else(|| format!("schema reference {reference} did not resolve"))
}

fn schema_types(schema: &Value) -> Vec<&str> {
    match schema.get("type") {
        Some(Value::String(kind)) => vec![kind.as_str()],
        Some(Value::Array(kinds)) => kinds.iter().filter_map(Value::as_str).collect(),
        _ => Vec::new(),
    }
}

fn json_type_matches(value: &Value, expected: &str) -> bool {
    match expected {
        "null" => value.is_null(),
        "boolean" => value.is_boolean(),
        "number" => value.is_number(),
        "integer" => value.as_i64().is_some() || value.as_u64().is_some(),
        "string" => value.is_string(),
        "array" => value.is_array(),
        "object" => value.is_object(),
        _ => false,
    }
}

fn json_type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(number) if number.as_i64().is_some() || number.as_u64().is_some() => {
            "integer"
        }
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

fn ensure_json_str(value: &Value, pointer: &str, expected: &str) -> TestResult {
    let actual = value
        .pointer(pointer)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("missing string field {pointer}"))?;
    if actual == expected {
        Ok(())
    } else {
        Err(format!("{pointer}: expected {expected:?}, got {actual:?}"))
    }
}

fn ensure_json_bool(value: &Value, pointer: &str, expected: bool) -> TestResult {
    let actual = value
        .pointer(pointer)
        .and_then(Value::as_bool)
        .ok_or_else(|| format!("missing boolean field {pointer}"))?;
    if actual == expected {
        Ok(())
    } else {
        Err(format!("{pointer}: expected {expected}, got {actual}"))
    }
}
