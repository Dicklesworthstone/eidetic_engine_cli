//! bd-36bbk.1.17 — structural contract for the SRR6.46.17 pre-grant
//! lane visibility audit schema (`ee.mesh.lane_grant_preview.v1`).
//!
//! The pure-decision module at `src/mesh/lane_grant_preview.rs`
//! computes the preview rows + cautions; this contract pins the
//! wire shape so a future renderer or trust-policy refactor can't
//! drift the schema without an explicit update here.
//!
//! Asserts:
//!
//! 1. The schema file exists at the canonical path and parses.
//! 2. `$id`, `title`, and `properties.schema.const` agree on
//!    `ee.mesh.lane_grant_preview.v1`.
//! 3. The required top-level fields match what the pure module emits.
//! 4. `additionalProperties: false` at the top level — read-only
//!    surface, no drift fields.
//! 5. `lane` enum is the closed set of six SRR6 trust lanes
//!    (`metadata`, `body`, `embedding`, `graph_link`,
//!    `curation_signal`, `revision_notice`) matching
//!    `IntendedLanePolicy` field names.
//! 6. `laneDecision` enum is `allow | quarantine | deny`.
//! 7. `trustClass` enum is the closed five-class set.
//! 8. `caution.kind` enum is the closed seven-kind set the bead
//!    acceptance names.
//! 9. `caution.severity` is restricted to `info | warning` only —
//!    the read-only computation cannot take an `error` path.
//! 10. `previewSampleStrategy` enum is the closed three-strategy set.
//! 11. `previewRow` required fields match the bead's documented row
//!    shape.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;

use serde_json::Value;

type TestResult = Result<(), String>;

const SCHEMA_PATH: &str = "docs/schemas/ee.mesh.lane_grant_preview.v1.json";
const SCHEMA_ID: &str = "https://eidetic-engine/schemas/ee.mesh.lane_grant_preview.v1.json";
const SCHEMA_NAME: &str = "ee.mesh.lane_grant_preview.v1";

const REQUIRED_TOP_LEVEL: &[&str] = &[
    "schema",
    "peerNodeKey",
    "lane",
    "workspaceId",
    "currentPolicy",
    "proposedPolicy",
    "affectedMemoryCount",
    "redactedFromExposureCount",
    "previewSampleStrategy",
    "previewSample",
    "redactionRulesApplied",
    "cautions",
];

const REQUIRED_LANES: &[&str] = &[
    "metadata",
    "body",
    "embedding",
    "graph_link",
    "curation_signal",
    "revision_notice",
];

const REQUIRED_LANE_DECISIONS: &[&str] = &["allow", "quarantine", "deny"];

const REQUIRED_TRUST_CLASSES: &[&str] = &[
    "human_explicit",
    "human_revised",
    "agent_validated",
    "agent_proposed",
    "external",
];

const REQUIRED_CAUTION_KINDS: &[&str] = &[
    "high_trust_class_exposure",
    "large_volume_exposure",
    "sensitive_tags_in_exposure",
    "tombstoned_in_exposure",
    "redaction_active",
    "peer_not_in_group",
    "lane_already_granted",
];

const REQUIRED_CAUTION_SEVERITIES: &[&str] = &["info", "warning"];

const REQUIRED_SAMPLE_STRATEGIES: &[&str] = &["random", "highest-trust", "most-recent"];

const REQUIRED_PREVIEW_ROW_FIELDS: &[&str] = &[
    "memoryId",
    "level",
    "kind",
    "contentPreview",
    "tags",
    "trustClass",
    "hasSensitiveTags",
    "redactedFields",
    "wouldExposeUnderProposedPolicy",
];

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

#[test]
fn lane_grant_preview_schema_file_exists_and_parses() -> TestResult {
    let _ = load_schema()?;
    Ok(())
}

#[test]
fn lane_grant_preview_schema_identity_is_consistent() -> TestResult {
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
        "properties.schema.const must equal ee.mesh.lane_grant_preview.v1",
    )?;
    Ok(())
}

#[test]
fn lane_grant_preview_required_top_level_fields_match_spec() -> TestResult {
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
fn lane_grant_preview_top_level_is_closed() -> TestResult {
    let schema = load_schema()?;
    ensure(
        schema["additionalProperties"] == Value::Bool(false),
        "top-level additionalProperties must be false (closed schema; read-only surface)",
    )
}

#[test]
fn lane_grant_preview_lane_enum_is_six_trust_lanes() -> TestResult {
    let schema = load_schema()?;
    require_closed_set(&schema, "/$defs/lane/enum", REQUIRED_LANES, "lane enum")
}

#[test]
fn lane_grant_preview_lane_decision_enum_is_three_states() -> TestResult {
    let schema = load_schema()?;
    require_closed_set(
        &schema,
        "/$defs/laneDecision/enum",
        REQUIRED_LANE_DECISIONS,
        "laneDecision enum",
    )
}

#[test]
fn lane_grant_preview_trust_class_enum_is_five_authority_classes() -> TestResult {
    let schema = load_schema()?;
    require_closed_set(
        &schema,
        "/$defs/trustClass/enum",
        REQUIRED_TRUST_CLASSES,
        "trustClass enum",
    )
}

#[test]
fn lane_grant_preview_caution_kind_enum_matches_bead_acceptance() -> TestResult {
    let schema = load_schema()?;
    require_closed_set(
        &schema,
        "/$defs/caution/properties/kind/enum",
        REQUIRED_CAUTION_KINDS,
        "caution.kind enum",
    )
}

#[test]
fn lane_grant_preview_caution_severity_excludes_error_per_read_only_invariant() -> TestResult {
    let schema = load_schema()?;
    require_closed_set(
        &schema,
        "/$defs/caution/properties/severity/enum",
        REQUIRED_CAUTION_SEVERITIES,
        "caution.severity enum",
    )?;
    let severities = collect_strings(
        schema
            .pointer("/$defs/caution/properties/severity/enum")
            .unwrap_or(&Value::Null),
        "caution.severity enum",
    )?;
    ensure(
        !severities.contains("error"),
        "caution.severity must not allow `error`; the read-only computation cannot take an error path",
    )
}

#[test]
fn lane_grant_preview_sample_strategy_enum_is_three_strategies() -> TestResult {
    let schema = load_schema()?;
    require_closed_set(
        &schema,
        "/properties/previewSampleStrategy/enum",
        REQUIRED_SAMPLE_STRATEGIES,
        "previewSampleStrategy enum",
    )
}

#[test]
fn lane_grant_preview_row_required_fields_match_bead_spec() -> TestResult {
    let schema = load_schema()?;
    let required = collect_strings(
        schema
            .pointer("/$defs/previewRow/required")
            .unwrap_or(&Value::Null),
        "previewRow.required",
    )?;
    for field in REQUIRED_PREVIEW_ROW_FIELDS {
        ensure(
            required.contains(*field),
            format!("previewRow.required missing {field}; got {required:?}"),
        )?;
    }
    Ok(())
}
