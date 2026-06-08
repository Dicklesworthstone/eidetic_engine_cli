//! bd-1n0np.15.5 - structured tracing and no-silent-cap contract.
//!
//! This is a static contract for the dueling-wizards initiative. It pins the
//! subsystem vocabulary, common tracing fields, and cap-event fields that future
//! runtime implementation slices must satisfy. Planned subsystems do not fail
//! for missing source yet, but the implemented harness anchors must exist.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::PathBuf;

use serde_json::Value;

type TestResult = Result<(), String>;

const MANIFEST_REL: &str =
    "tests/fixtures/contracts/dueling_wizards_observability_no_silent_cap.json";
const DOC_REL: &str = "docs/agent-ux/dueling-wizards/observability-no-silent-cap.md";

const REQUIRED_SUBSYSTEMS: &[&str] = &[
    "evidence_harvester",
    "anchors_freshness",
    "error_recall",
    "read_fence",
    "write_immune",
    "gap_honesty",
    "contradiction_resolution",
    "harness_contract",
];

const REQUIRED_TRACE_FIELDS: &[&str] = &[
    "workspace_id",
    "request_id",
    "bead_id",
    "surface",
    "phase",
    "elapsed_ms",
    "degraded_codes",
];

const REQUIRED_PHASES: &[&str] = &[
    "input",
    "dispatch",
    "dependency_check",
    "persistence",
    "response",
];

const REQUIRED_CAP_OPERATIONS: &[&str] = &["truncation", "sampling", "top_n", "abstention"];

const REQUIRED_CAP_FIELDS: &[&str] = &[
    "cap_kind",
    "dropped_count",
    "drop_reason",
    "cap_limit",
    "retained_count",
];

const REQUIRED_SUBSYSTEM_MUST_CLAUSES: u64 = 10;
const MIN_MUST_COVERAGE_MILLI: u64 = 950;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read_text(rel: &str) -> Result<String, String> {
    let path = repo_root().join(rel);
    fs::read_to_string(&path).map_err(|error| format!("read {rel}: {error}"))
}

fn read_json(rel: &str) -> Result<Value, String> {
    let text = read_text(rel)?;
    serde_json::from_str(&text).map_err(|error| format!("parse {rel}: {error}"))
}

fn string_field<'a>(value: &'a Value, pointer: &str, context: &str) -> Result<&'a str, String> {
    value
        .pointer(pointer)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("{context}: missing string field {pointer}"))
}

fn bool_field(value: &Value, pointer: &str, context: &str) -> Result<bool, String> {
    value
        .pointer(pointer)
        .and_then(Value::as_bool)
        .ok_or_else(|| format!("{context}: missing bool field {pointer}"))
}

fn u64_field(value: &Value, pointer: &str, context: &str) -> Result<u64, String> {
    value
        .pointer(pointer)
        .and_then(Value::as_u64)
        .ok_or_else(|| format!("{context}: missing u64 field {pointer}"))
}

fn array_field<'a>(
    value: &'a Value,
    pointer: &str,
    context: &str,
) -> Result<&'a Vec<Value>, String> {
    value
        .pointer(pointer)
        .and_then(Value::as_array)
        .ok_or_else(|| format!("{context}: missing array field {pointer}"))
}

fn string_set(values: &[Value], context: &str) -> Result<BTreeSet<String>, String> {
    let mut out = BTreeSet::new();
    for (index, value) in values.iter().enumerate() {
        let text = value
            .as_str()
            .ok_or_else(|| format!("{context}[{index}] must be a string"))?;
        if text.trim().is_empty() {
            return Err(format!("{context}[{index}] must not be empty"));
        }
        out.insert(text.to_owned());
    }
    Ok(out)
}

fn required_set(values: &[&str]) -> BTreeSet<String> {
    values.iter().map(|value| (*value).to_owned()).collect()
}

#[test]
fn observability_manifest_identity_and_policy_are_stable() -> TestResult {
    let manifest = read_json(MANIFEST_REL)?;
    if string_field(&manifest, "/schema", MANIFEST_REL)?
        != "ee.dueling_wizards.observability_no_silent_cap.v1"
    {
        return Err(format!(
            "{MANIFEST_REL}: schema must be ee.dueling_wizards.observability_no_silent_cap.v1"
        ));
    }
    if string_field(&manifest, "/initiativeBead", MANIFEST_REL)? != "bd-1n0np" {
        return Err("manifest must identify initiativeBead bd-1n0np".to_owned());
    }
    if string_field(&manifest, "/gateBead", MANIFEST_REL)? != "bd-1n0np.15.5" {
        return Err("manifest must identify gateBead bd-1n0np.15.5".to_owned());
    }
    if string_field(&manifest, "/manifestOwner", MANIFEST_REL)?
        != "tests/contracts/dueling_wizards_observability_no_silent_cap.rs"
    {
        return Err("manifestOwner must point at this contract test".to_owned());
    }
    if string_field(&manifest, "/doc", MANIFEST_REL)? != DOC_REL {
        return Err(format!("/doc must point at {DOC_REL}"));
    }
    if string_field(&manifest, "/implementationState", MANIFEST_REL)? != "planned_contract" {
        return Err("implementationState must stay planned_contract".to_owned());
    }

    for pointer in [
        "/policy/structuredTracingRequired",
        "/policy/noSilentCapRequired",
        "/policy/rchProofRequiredForRuntimeTests",
    ] {
        if !bool_field(&manifest, pointer, MANIFEST_REL)? {
            return Err(format!("{pointer} must stay true"));
        }
    }
    for (pointer, expected) in [
        ("/policy/capEventCompatibility", "stable_additive"),
        ("/policy/missingCapEventBehavior", "degraded_not_silent"),
        ("/policy/localCargoProof", "invalid"),
    ] {
        if string_field(&manifest, pointer, MANIFEST_REL)? != expected {
            return Err(format!("{pointer} must stay {expected}"));
        }
    }
    Ok(())
}

#[test]
fn top_level_field_vocabularies_are_complete() -> TestResult {
    let manifest = read_json(MANIFEST_REL)?;
    let checks = [
        (
            "/requiredTraceFields",
            REQUIRED_TRACE_FIELDS,
            "required trace fields",
        ),
        ("/standardPhases", REQUIRED_PHASES, "standard phases"),
        ("/capOperations", REQUIRED_CAP_OPERATIONS, "cap operations"),
        ("/capEventFields", REQUIRED_CAP_FIELDS, "cap event fields"),
    ];

    for (pointer, expected, label) in checks {
        let actual = string_set(array_field(&manifest, pointer, MANIFEST_REL)?, pointer)?;
        let expected = required_set(expected);
        if actual != expected {
            return Err(format!(
                "{label} drifted: missing={:?}, extra={:?}",
                expected.difference(&actual).collect::<Vec<_>>(),
                actual.difference(&expected).collect::<Vec<_>>()
            ));
        }
    }
    Ok(())
}

#[test]
fn cap_event_examples_cover_all_operations_and_are_not_silent() -> TestResult {
    let manifest = read_json(MANIFEST_REL)?;
    let expected_operations = required_set(REQUIRED_CAP_OPERATIONS);
    let expected_fields = required_set(REQUIRED_CAP_FIELDS);
    let expected_phases = required_set(REQUIRED_PHASES);
    let expected_subsystems = required_set(REQUIRED_SUBSYSTEMS);
    let mut actual_operations = BTreeSet::new();

    for (index, example) in array_field(&manifest, "/capEventExamples", MANIFEST_REL)?
        .iter()
        .enumerate()
    {
        let context = format!("capEventExamples[{index}]");
        let surface = string_field(example, "/surface", &context)?;
        if !expected_subsystems.contains(surface) {
            return Err(format!("{context}: unknown surface {surface}"));
        }
        let phase = string_field(example, "/phase", &context)?;
        if !expected_phases.contains(phase) {
            return Err(format!("{context}: unsupported phase {phase}"));
        }

        let operation = string_field(example, "/cap_kind", &context)?;
        if !expected_operations.contains(operation) {
            return Err(format!("{context}: unsupported cap_kind {operation}"));
        }
        actual_operations.insert(operation.to_owned());

        for field in &expected_fields {
            if example.get(field).is_none() {
                return Err(format!("{context}: missing cap event field {field}"));
            }
        }

        let dropped_count = u64_field(example, "/dropped_count", &context)?;
        let cap_limit = u64_field(example, "/cap_limit", &context)?;
        let retained_count = u64_field(example, "/retained_count", &context)?;
        if retained_count > cap_limit {
            return Err(format!(
                "{context}: retained_count {retained_count} must not exceed cap_limit {cap_limit}"
            ));
        }
        if dropped_count == 0 {
            return Err(format!(
                "{context}: example must prove a non-zero drop or abstention"
            ));
        }
        if string_field(example, "/drop_reason", &context)?
            .trim()
            .is_empty()
        {
            return Err(format!("{context}: drop_reason must not be empty"));
        }
    }

    if actual_operations != expected_operations {
        return Err(format!(
            "capEventExamples must cover every cap operation: missing={:?}, extra={:?}",
            expected_operations
                .difference(&actual_operations)
                .collect::<Vec<_>>(),
            actual_operations
                .difference(&expected_operations)
                .collect::<Vec<_>>()
        ));
    }
    Ok(())
}

#[test]
fn every_subsystem_declares_trace_fields_and_no_silent_cap_fields() -> TestResult {
    let manifest = read_json(MANIFEST_REL)?;
    let expected_subsystems = required_set(REQUIRED_SUBSYSTEMS);
    let expected_trace_fields = required_set(REQUIRED_TRACE_FIELDS);
    let expected_cap_operations = required_set(REQUIRED_CAP_OPERATIONS);
    let expected_cap_fields = required_set(REQUIRED_CAP_FIELDS);
    let mut actual_subsystems = BTreeSet::new();

    for (index, subsystem) in array_field(&manifest, "/subsystems", MANIFEST_REL)?
        .iter()
        .enumerate()
    {
        let context = format!("subsystems[{index}]");
        let id = string_field(subsystem, "/id", &context)?;
        if !actual_subsystems.insert(id.to_owned()) {
            return Err(format!("duplicate subsystem id {id}"));
        }
        if string_field(subsystem, "/surface", &context)? != id {
            return Err(format!("{id}: surface must match subsystem id"));
        }
        let status = string_field(subsystem, "/status", &context)?;
        if !matches!(status, "planned" | "in_progress" | "implemented") {
            return Err(format!("{id}: unsupported status {status}"));
        }
        let owner_beads = string_set(
            array_field(subsystem, "/ownerBeads", &context)?,
            &format!("{id}.ownerBeads"),
        )?;
        if !owner_beads.contains("bd-1n0np.15.5") {
            return Err(format!("{id}: ownerBeads must include bd-1n0np.15.5"));
        }
        if string_set(
            array_field(subsystem, "/requiredTraceFields", &context)?,
            &format!("{id}.requiredTraceFields"),
        )? != expected_trace_fields
        {
            return Err(format!(
                "{id}: requiredTraceFields must carry the shared set"
            ));
        }
        if string_set(
            array_field(subsystem, "/capOperations", &context)?,
            &format!("{id}.capOperations"),
        )? != expected_cap_operations
        {
            return Err(format!("{id}: capOperations must carry the shared set"));
        }
        if string_set(
            array_field(subsystem, "/capEventFields", &context)?,
            &format!("{id}.capEventFields"),
        )? != expected_cap_fields
        {
            return Err(format!("{id}: capEventFields must carry the shared set"));
        }

        let anchors = array_field(subsystem, "/sourceAnchors", &context)?;
        if status == "implemented" && anchors.is_empty() {
            return Err(format!(
                "{id}: implemented subsystem must list sourceAnchors"
            ));
        }
        for (anchor_index, anchor) in anchors.iter().enumerate() {
            let rel = anchor
                .as_str()
                .ok_or_else(|| format!("{id}.sourceAnchors[{anchor_index}] must be a string"))?;
            let path = repo_root().join(rel);
            if !path.exists() {
                return Err(format!("{id}: source anchor {rel} does not exist"));
            }
        }
    }

    if actual_subsystems != expected_subsystems {
        return Err(format!(
            "subsystem set drifted: missing={:?}, extra={:?}",
            expected_subsystems
                .difference(&actual_subsystems)
                .collect::<Vec<_>>(),
            actual_subsystems
                .difference(&expected_subsystems)
                .collect::<Vec<_>>()
        ));
    }
    Ok(())
}

#[test]
fn subsystem_coverage_matrix_accounts_for_every_observability_contract() -> TestResult {
    let manifest = read_json(MANIFEST_REL)?;
    let expected_subsystems = required_set(REQUIRED_SUBSYSTEMS);
    let proof_policy = match (
        string_field(&manifest, "/policy/localCargoProof", MANIFEST_REL)?,
        bool_field(
            &manifest,
            "/policy/rchProofRequiredForRuntimeTests",
            MANIFEST_REL,
        )?,
    ) {
        ("invalid", true) => "rch_required_local_invalid",
        (local_policy, rch_required) => {
            return Err(format!(
                "unsupported proof posture localCargoProof={local_policy} rchProofRequiredForRuntimeTests={rch_required}"
            ));
        }
    };

    let mut subsystems = BTreeMap::new();
    for (index, subsystem) in array_field(&manifest, "/subsystems", MANIFEST_REL)?
        .iter()
        .enumerate()
    {
        let context = format!("subsystems[{index}]");
        let id = string_field(subsystem, "/id", &context)?;
        if subsystems.insert(id.to_owned(), subsystem).is_some() {
            return Err(format!("duplicate subsystem id {id}"));
        }
    }

    let mut matrix_ids = BTreeSet::new();
    for (index, row) in array_field(&manifest, "/subsystemCoverageMatrix", MANIFEST_REL)?
        .iter()
        .enumerate()
    {
        let context = format!("subsystemCoverageMatrix[{index}]");
        let subsystem_id = string_field(row, "/subsystem", &context)?;
        if !matrix_ids.insert(subsystem_id.to_owned()) {
            return Err(format!(
                "duplicate subsystemCoverageMatrix row {subsystem_id}"
            ));
        }

        let subsystem = subsystems
            .get(subsystem_id)
            .ok_or_else(|| format!("{context}: missing subsystem {subsystem_id}"))?;
        let subsystem_context = format!("subsystems[{subsystem_id}]");
        let surface = string_field(subsystem, "/surface", &subsystem_context)?;
        let status = string_field(subsystem, "/status", &subsystem_context)?;
        let expected_anchor_status = match status {
            "planned" => "planned_contract_only",
            "in_progress" => "anchors_checked_when_present",
            "implemented" => "source_anchors_required",
            other => {
                return Err(format!(
                    "{subsystem_id}: unsupported subsystem status {other}"
                ));
            }
        };

        if string_field(row, "/surface", &context)? != surface {
            return Err(format!(
                "{subsystem_id}: matrix surface must mirror subsystem surface {surface}"
            ));
        }
        if string_field(row, "/status", &context)? != status {
            return Err(format!(
                "{subsystem_id}: matrix status must mirror subsystem status {status}"
            ));
        }
        if string_field(row, "/traceStatus", &context)? != "shared_fields_declared" {
            return Err(format!(
                "{subsystem_id}: traceStatus must be shared_fields_declared"
            ));
        }
        if string_field(row, "/capStatus", &context)? != "no_silent_cap_declared" {
            return Err(format!(
                "{subsystem_id}: capStatus must be no_silent_cap_declared"
            ));
        }
        if string_field(row, "/anchorEvidenceStatus", &context)? != expected_anchor_status {
            return Err(format!(
                "{subsystem_id}: anchorEvidenceStatus must be {expected_anchor_status}"
            ));
        }
        if string_field(row, "/runtimeProofPolicy", &context)? != proof_policy {
            return Err(format!(
                "{subsystem_id}: runtimeProofPolicy must be {proof_policy}"
            ));
        }
        if string_field(row, "/complianceStatus", &context)? != "declared_conformant" {
            return Err(format!(
                "{subsystem_id}: complianceStatus must be declared_conformant"
            ));
        }

        for (pointer, expected) in [
            (
                "/ownerBeadCount",
                array_field(subsystem, "/ownerBeads", &subsystem_context)?.len() as u64,
            ),
            (
                "/traceFieldCount",
                array_field(subsystem, "/requiredTraceFields", &subsystem_context)?.len() as u64,
            ),
            (
                "/capOperationCount",
                array_field(subsystem, "/capOperations", &subsystem_context)?.len() as u64,
            ),
            (
                "/capEventFieldCount",
                array_field(subsystem, "/capEventFields", &subsystem_context)?.len() as u64,
            ),
            (
                "/sourceAnchorCount",
                array_field(subsystem, "/sourceAnchors", &subsystem_context)?.len() as u64,
            ),
            ("/mustClauses", REQUIRED_SUBSYSTEM_MUST_CLAUSES),
            ("/tested", REQUIRED_SUBSYSTEM_MUST_CLAUSES),
            ("/passing", REQUIRED_SUBSYSTEM_MUST_CLAUSES),
            ("/divergent", 0),
        ] {
            let actual = u64_field(row, pointer, &context)?;
            if actual != expected {
                return Err(format!(
                    "{subsystem_id}: {pointer} must be {expected}, got {actual}"
                ));
            }
        }

        let score_milli = u64_field(row, "/scoreMilli", &context)?;
        let computed_score = u64_field(row, "/passing", &context)?.saturating_mul(1000)
            / u64_field(row, "/mustClauses", &context)?;
        if score_milli != computed_score {
            return Err(format!(
                "{subsystem_id}: scoreMilli={score_milli} must match computed MUST score={computed_score}"
            ));
        }
        if score_milli < MIN_MUST_COVERAGE_MILLI {
            return Err(format!(
                "{subsystem_id}: MUST coverage {score_milli} is below {MIN_MUST_COVERAGE_MILLI}"
            ));
        }
    }

    if matrix_ids != expected_subsystems {
        return Err(format!(
            "subsystemCoverageMatrix drifted: missing={:?}, extra={:?}",
            expected_subsystems
                .difference(&matrix_ids)
                .collect::<Vec<_>>(),
            matrix_ids
                .difference(&expected_subsystems)
                .collect::<Vec<_>>()
        ));
    }
    Ok(())
}

#[test]
fn anchor_files_still_contain_required_terms() -> TestResult {
    let manifest = read_json(MANIFEST_REL)?;
    for (index, anchor) in array_field(&manifest, "/anchors", MANIFEST_REL)?
        .iter()
        .enumerate()
    {
        let context = format!("anchors[{index}]");
        let source = string_field(anchor, "/source", &context)?;
        let source_text = read_text(source)?;
        for needle in array_field(anchor, "/needles", &context)? {
            let needle = needle
                .as_str()
                .ok_or_else(|| format!("{context}: needle must be a string"))?;
            if !source_text.contains(needle) {
                return Err(format!("{source}: missing required anchor {needle}"));
            }
        }
    }
    Ok(())
}

#[test]
fn documentation_mentions_manifest_fields_and_no_silent_cap_terms() -> TestResult {
    let doc = read_text(DOC_REL)?;
    for needle in [
        MANIFEST_REL,
        "bd-1n0np.15.5",
        "Local Cargo fallback is not valid proof",
        "scripts/lib/e2e_harness.sh",
        "scripts/check-tracing-fields.sh",
        "docs/observability/tracing_field_convention.md",
        "capEventExamples",
        "Subsystem Coverage Matrix",
        "subsystemCoverageMatrix",
        "rch_required_local_invalid",
        "declared_conformant",
        "token_budget_exceeded",
        "required_dependency_unavailable",
    ] {
        if !doc.contains(needle) {
            return Err(format!("{DOC_REL}: missing required reference {needle}"));
        }
    }
    for subsystem in REQUIRED_SUBSYSTEMS {
        if !doc.contains(subsystem) {
            return Err(format!("{DOC_REL}: missing subsystem {subsystem}"));
        }
    }
    for field in REQUIRED_TRACE_FIELDS
        .iter()
        .chain(REQUIRED_CAP_FIELDS.iter())
    {
        if !doc.contains(field) {
            return Err(format!("{DOC_REL}: missing field {field}"));
        }
    }
    for operation in REQUIRED_CAP_OPERATIONS {
        if !doc.contains(operation) {
            return Err(format!("{DOC_REL}: missing cap operation {operation}"));
        }
    }
    Ok(())
}
