//! bd-2vu8m (SRR6.23 final hardening) — structural contract for the
//! `ee.closeout_audit.v1` envelope emitted by
//! `scripts/closeout_audit.sh` (J11.4).
//!
//! tests/closeout_audit_runner_unit.rs::assert_envelope_shape already
//! asserts the LIVE jq emission's shape against this exact field set
//! (line 121-178). This contract pins the schema FILE side so the two
//! halves cannot drift apart: a future bash refactor that emits a new
//! field has to extend the schema, and a future schema refactor that
//! removes a field has to update the live emit.
//!
//! Asserts:
//!
//! 1. Schema file exists at canonical path and parses.
//! 2. `$id`, `title`, `properties.schema.const` agree on
//!    `ee.closeout_audit.v1`.
//! 3. Required top-level fields match the assert_envelope_shape
//!    expectation in the runner unit test (schema, bead_id, readiness,
//!    evidence, blockers, caveats, next_actions).
//! 4. `additionalProperties: false` at top level — closed envelope.
//! 5. `readiness` enum is the closed three-value set
//!    {ready, ready_with_caveats, blocked} the script documents.
//! 6. `evidence` required fields cover every key the runner unit
//!    test names (18 fields).
//! 7. `evidence.dependency_cycle_status` enum matches the bash
//!    script's DEPENDENCY_CYCLE_STATUS assignments
//!    (ok / timeout / br_unavailable / unavailable).
//! 8. `evidence.rch_status` enum matches RCH_STATUS assignments
//!    (unknown / ready / timeout / local_fallback_likely).
//! 9. `evidence.rch_queue_status` enum matches RCH_QUEUE_STATUS
//!    assignments (unknown / idle / active / queued /
//!    stale_active_records / timeout / unavailable).
//! 10. `evidence.agent_mail_status` enum matches AGENT_MAIL_STATUS
//!     assignments (unknown / reachable / unreachable).
//! 11. `srr6_closeout.status` enum is the closed three-value set
//!     {ready, blocked, not_applicable}.
//! 12. `srr6_closeout` required fields cover every key the script
//!     emits — including `deferred_dependencies_missing_rationale`,
//!     the deferred-with-rationale gate bd-2vu8m acceptance demands.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;

use serde_json::Value;

type TestResult = Result<(), String>;

const SCHEMA_PATH: &str = "docs/schemas/ee.closeout_audit.v1.json";
const SCHEMA_ID: &str = "https://eidetic-engine/schemas/ee.closeout_audit.v1.json";
const SCHEMA_NAME: &str = "ee.closeout_audit.v1";

const REQUIRED_TOP_LEVEL: &[&str] = &[
    "schema",
    "bead_id",
    "readiness",
    "evidence",
    "blockers",
    "caveats",
    "next_actions",
];

const READINESS_VALUES: &[&str] = &["ready", "ready_with_caveats", "blocked"];

const REQUIRED_EVIDENCE: &[&str] = &[
    "bead_status",
    "bead_assignee",
    "bead_title",
    "open_dependencies",
    "dependency_cycles",
    "dependency_cycle_count",
    "dependency_cycle_status",
    "dependency_cycle_source",
    "uncommitted_files_referencing_bead",
    "rch_status",
    "rch_queue_status",
    "rch_active_builds",
    "rch_stale_active_builds",
    "rch_queued_builds",
    "agent_mail_status",
    "j1_log_present",
    "j1_log_path",
    "srr6_closeout",
];

const DEPENDENCY_CYCLE_STATUSES: &[&str] = &["ok", "timeout", "br_unavailable", "unavailable"];

const RCH_STATUSES: &[&str] = &["unknown", "ready", "timeout", "local_fallback_likely"];

const RCH_QUEUE_STATUSES: &[&str] = &[
    "unknown",
    "idle",
    "active",
    "queued",
    "stale_active_records",
    "timeout",
    "unavailable",
];

const AGENT_MAIL_STATUSES: &[&str] = &["unknown", "reachable", "unreachable"];

const SRR6_CLOSEOUT_STATUSES: &[&str] = &["ready", "blocked", "not_applicable"];

const REQUIRED_SRR6_CLOSEOUT: &[&str] = &[
    "enabled",
    "status",
    "matrix_path",
    "matrix_present",
    "matrix_row_present",
    "required_proofs",
    "missing_proofs",
    "missing_proof_markers",
    "unresolved_dependencies",
    "deferred_dependencies_missing_rationale",
];

const REQUIRED_DEPENDENCY_STATUS: &[&str] = &["id", "status"];
const REQUIRED_DEPENDENCY_CYCLE: &[&str] = &["path", "cycle"];
const REQUIRED_MISSING_PROOF_MARKER: &[&str] = &["path", "marker"];

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

fn load_schema() -> Result<Value, String> {
    let path = repo_root().join(SCHEMA_PATH);
    let text =
        fs::read_to_string(&path).map_err(|error| format!("read {}: {error}", path.display()))?;
    serde_json::from_str(&text).map_err(|error| format!("parse {}: {error}", path.display()))
}

fn collect_strings(node: &Value, ctx: &str) -> Result<BTreeSet<String>, String> {
    let array = node
        .as_array()
        .ok_or_else(|| format!("{ctx}: expected array, got: {node}"))?;
    array
        .iter()
        .map(|value| {
            value
                .as_str()
                .map(str::to_owned)
                .ok_or_else(|| format!("{ctx}: non-string entry: {value}"))
        })
        .collect()
}

fn require_closed_set(schema: &Value, pointer: &str, expected: &[&str], label: &str) -> TestResult {
    let actual = collect_strings(schema.pointer(pointer).unwrap_or(&Value::Null), label)?;
    let want: BTreeSet<String> = expected.iter().map(|s| (*s).to_owned()).collect();
    ensure(
        actual == want,
        format!("{label} drifted from closed set; expected {want:?}, got {actual:?}"),
    )
}

fn require_schema_ref(
    schema: &Value,
    pointer: &str,
    expected_ref: &str,
    label: &str,
) -> TestResult {
    let actual = schema
        .pointer(pointer)
        .and_then(Value::as_str)
        .unwrap_or("<missing>");
    ensure(
        actual == expected_ref,
        format!("{label} must use {expected_ref}; got {actual}"),
    )
}

fn require_required_fields(
    schema: &Value,
    pointer: &str,
    expected: &[&str],
    label: &str,
) -> TestResult {
    let required = collect_strings(schema.pointer(pointer).unwrap_or(&Value::Null), label)?;
    let want: BTreeSet<String> = expected.iter().map(|s| (*s).to_owned()).collect();
    ensure(
        required == want,
        format!("{label} required fields drifted; expected {want:?}, got {required:?}"),
    )
}

#[test]
fn closeout_audit_schema_file_exists_and_parses() -> TestResult {
    let _ = load_schema()?;
    Ok(())
}

#[test]
fn closeout_audit_schema_identity_is_consistent() -> TestResult {
    let schema = load_schema()?;
    ensure(
        schema["$id"] == SCHEMA_ID,
        format!("expected $id={SCHEMA_ID}; got: {}", schema["$id"]),
    )?;
    ensure(
        schema["title"] == SCHEMA_NAME,
        format!("expected title={SCHEMA_NAME}; got: {}", schema["title"]),
    )?;
    ensure(
        schema["properties"]["schema"]["const"] == SCHEMA_NAME,
        "properties.schema.const must equal ee.closeout_audit.v1",
    )
}

#[test]
fn closeout_audit_required_top_level_matches_runner_assertion() -> TestResult {
    // Mirrors assert_envelope_shape at tests/closeout_audit_runner_unit.rs:121.
    let schema = load_schema()?;
    let required = collect_strings(&schema["required"], "top-level required")?;
    for field in REQUIRED_TOP_LEVEL {
        ensure(
            required.contains(*field),
            format!("required missing {field}; got {required:?}"),
        )?;
    }
    Ok(())
}

#[test]
fn closeout_audit_top_level_is_closed() -> TestResult {
    let schema = load_schema()?;
    ensure(
        schema["additionalProperties"] == Value::Bool(false),
        "top-level additionalProperties must be false (closed envelope)",
    )
}

#[test]
fn closeout_audit_readiness_enum_is_three_states() -> TestResult {
    let schema = load_schema()?;
    require_closed_set(
        &schema,
        "/properties/readiness/enum",
        READINESS_VALUES,
        "readiness enum",
    )
}

#[test]
fn closeout_audit_evidence_required_matches_runner_assertion() -> TestResult {
    // Mirrors assert_envelope_shape's required_evidence list at
    // tests/closeout_audit_runner_unit.rs:144.
    let schema = load_schema()?;
    let required = collect_strings(
        schema
            .pointer("/$defs/evidence/required")
            .unwrap_or(&Value::Null),
        "evidence.required",
    )?;
    for field in REQUIRED_EVIDENCE {
        ensure(
            required.contains(*field),
            format!("evidence.required missing {field}; got {required:?}"),
        )?;
    }
    Ok(())
}

#[test]
fn closeout_audit_dependency_cycle_status_matches_bash_assignments() -> TestResult {
    let schema = load_schema()?;
    require_closed_set(
        &schema,
        "/$defs/evidence/properties/dependency_cycle_status/enum",
        DEPENDENCY_CYCLE_STATUSES,
        "evidence.dependency_cycle_status enum",
    )
}

#[test]
fn closeout_audit_rch_status_matches_bash_assignments() -> TestResult {
    let schema = load_schema()?;
    require_closed_set(
        &schema,
        "/$defs/evidence/properties/rch_status/enum",
        RCH_STATUSES,
        "evidence.rch_status enum",
    )
}

#[test]
fn closeout_audit_rch_queue_status_matches_bash_assignments() -> TestResult {
    let schema = load_schema()?;
    require_closed_set(
        &schema,
        "/$defs/evidence/properties/rch_queue_status/enum",
        RCH_QUEUE_STATUSES,
        "evidence.rch_queue_status enum",
    )
}

#[test]
fn closeout_audit_agent_mail_status_matches_bash_assignments() -> TestResult {
    let schema = load_schema()?;
    require_closed_set(
        &schema,
        "/$defs/evidence/properties/agent_mail_status/enum",
        AGENT_MAIL_STATUSES,
        "evidence.agent_mail_status enum",
    )
}

#[test]
fn closeout_audit_srr6_closeout_status_enum_is_three_states() -> TestResult {
    let schema = load_schema()?;
    require_closed_set(
        &schema,
        "/$defs/srr6Closeout/properties/status/enum",
        SRR6_CLOSEOUT_STATUSES,
        "srr6_closeout.status enum",
    )
}

#[test]
fn closeout_audit_structured_arrays_match_script_payloads() -> TestResult {
    let schema = load_schema()?;

    for (pointer, expected_ref, label) in [
        (
            "/$defs/evidence/properties/open_dependencies/items/$ref",
            "#/$defs/dependencyStatus",
            "evidence.open_dependencies items",
        ),
        (
            "/$defs/evidence/properties/dependency_cycles/items/$ref",
            "#/$defs/dependencyCycle",
            "evidence.dependency_cycles items",
        ),
        (
            "/$defs/srr6Closeout/properties/missing_proof_markers/items/$ref",
            "#/$defs/missingProofMarker",
            "srr6_closeout.missing_proof_markers items",
        ),
        (
            "/$defs/srr6Closeout/properties/unresolved_dependencies/items/$ref",
            "#/$defs/dependencyStatus",
            "srr6_closeout.unresolved_dependencies items",
        ),
        (
            "/$defs/srr6Closeout/properties/deferred_dependencies_missing_rationale/items/$ref",
            "#/$defs/dependencyStatus",
            "srr6_closeout.deferred_dependencies_missing_rationale items",
        ),
    ] {
        require_schema_ref(&schema, pointer, expected_ref, label)?;
    }

    for (def_name, required_fields) in [
        ("dependencyStatus", REQUIRED_DEPENDENCY_STATUS),
        ("dependencyCycle", REQUIRED_DEPENDENCY_CYCLE),
        ("missingProofMarker", REQUIRED_MISSING_PROOF_MARKER),
    ] {
        ensure(
            schema["$defs"][def_name]["additionalProperties"] == Value::Bool(false),
            format!("{def_name} must be closed over additional properties"),
        )?;
        require_required_fields(
            &schema,
            &format!("/$defs/{def_name}/required"),
            required_fields,
            def_name,
        )?;
    }

    Ok(())
}

#[test]
fn closeout_audit_srr6_closeout_required_pins_deferred_rationale_gate() -> TestResult {
    // bd-2vu8m acceptance: "all SRR6 child beads either closed or
    // explicitly deferred with rationale". Pin
    // deferred_dependencies_missing_rationale so a future refactor
    // can't drop the gate.
    let schema = load_schema()?;
    let required = collect_strings(
        schema
            .pointer("/$defs/srr6Closeout/required")
            .unwrap_or(&Value::Null),
        "srr6_closeout.required",
    )?;
    for field in REQUIRED_SRR6_CLOSEOUT {
        ensure(
            required.contains(*field),
            format!("srr6_closeout.required missing {field}; got {required:?}"),
        )?;
    }
    ensure(
        required.contains("deferred_dependencies_missing_rationale"),
        "srr6_closeout.required must include deferred_dependencies_missing_rationale (bd-2vu8m deferred-with-rationale gate)",
    )
}
