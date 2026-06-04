//! Contract coverage for `ee.regression_causality.v1`.
//!
//! The regression-causality capsule is a redaction-safe diagnostic contract:
//! it separates direct evidence from derived hypotheses and prevents future
//! producers from turning raw logs or private paths into support-bundle data.

use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;

use serde_json::Value;

type TestResult = Result<(), String>;

const SCHEMA_PATH: &str = "docs/schemas/ee.regression_causality.v1.json";
const DOC_PATH: &str = "docs/agent-ux/regression-causality.md";
const SCHEMA_ID: &str = "https://eidetic-engine/schemas/ee.regression_causality.v1.json";
const SCHEMA_NAME: &str = "ee.regression_causality.v1";

const REQUIRED_TOP_LEVEL: &[&str] = &[
    "schema",
    "subject",
    "sourceState",
    "evidenceSources",
    "hypotheses",
    "redaction",
    "degraded",
    "nextCommands",
];

const REQUIRED_SUBJECT: &[&str] = &[
    "surface",
    "artifactKind",
    "artifactId",
    "commandHash",
    "observedAt",
    "workspaceHash",
];
const REQUIRED_SOURCE_STATE: &[&str] = &[
    "materialization",
    "verificationAttribution",
    "localDirty",
    "remoteSourceMaterialized",
    "sourceHash",
    "degradedCodes",
];
const REQUIRED_EVIDENCE_SOURCE: &[&str] = &[
    "id",
    "kind",
    "schema",
    "status",
    "artifactHash",
    "summary",
    "redactionStatus",
    "authoritative",
];
const REQUIRED_HYPOTHESIS: &[&str] = &[
    "rank",
    "code",
    "confidence",
    "severity",
    "summary",
    "evidenceRefs",
    "counterEvidence",
    "ownerHints",
    "nextCommands",
    "authoritative",
];
const REQUIRED_COUNTER_EVIDENCE: &[&str] = &["sourceId", "summary", "effect"];
const REQUIRED_OWNER_HINT: &[&str] = &["kind", "value", "confidence"];
const REQUIRED_COMMAND: &[&str] = &["command", "rationale", "mutatesWorkspace", "requiresRch"];
const REQUIRED_REDACTION: &[&str] = &[
    "rawLogsPresent",
    "rawMailBodiesPresent",
    "rawMemoryBodiesPresent",
    "privatePathsPresent",
    "secretScanApplied",
    "truncationApplied",
    "hashesOnly",
];
const REQUIRED_DEGRADED_ENTRY: &[&str] = &["code", "severity", "message", "evidenceSourceId"];

const PRESET_MINIMAL: &[&str] = &["schema", "subject", "hypotheses"];
const PRESET_SUMMARY: &[&str] = &[
    "schema",
    "subject",
    "sourceState",
    "evidenceSources",
    "hypotheses",
    "degraded",
    "nextCommands",
];
const PRESET_STANDARD: &[&str] = &[
    "schema",
    "subject",
    "sourceState",
    "evidenceSources",
    "hypotheses",
    "redaction",
    "degraded",
    "nextCommands",
];
const PRESET_FULL: &[&str] = &["*"];

const SURFACES: &[&str] = &[
    "verification_gate",
    "swarm_replay",
    "e2e_event_radar",
    "pack_quality",
    "perf_budget",
    "tracker_state",
    "support_bundle",
    "unknown",
];
const MATERIALIZATIONS: &[&str] = &[
    "committed_tree",
    "dirty_source_materialized",
    "remote_checkout_unverified",
    "source_state_refused",
    "not_applicable",
    "unknown",
];
const EVIDENCE_KINDS: &[&str] = &[
    "verification_evidence",
    "rch_selector_admission",
    "swarm_replay",
    "e2e_event_log",
    "pack_replay",
    "pack_diff",
    "perf_report",
    "beads_history",
    "bv_history",
    "degraded_fixture",
    "git_metadata",
    "support_bundle",
];
const EVIDENCE_STATUSES: &[&str] = &[
    "available",
    "missing",
    "malformed",
    "stale",
    "blocked",
    "unsupported",
    "redacted_only",
];
const REDACTION_STATUSES: &[&str] = &["safe", "redacted", "hash_only", "refused", "unknown"];
const HYPOTHESIS_CODES: &[&str] = &[
    "source_not_materialized",
    "schema_contract_drift",
    "stale_derived_asset",
    "known_environment_blocker",
    "output_budget_regression",
    "fixture_gap",
    "pack_selection_change",
    "perf_budget_regression",
    "tracker_state_mismatch",
    "unknown_insufficient_evidence",
];
const SEVERITIES: &[&str] = &["info", "low", "warning", "medium", "high", "critical"];
const OWNER_HINT_KINDS: &[&str] = &["bead", "agent", "module", "command", "unknown"];
const COUNTER_EVIDENCE_EFFECTS: &[&str] =
    &["supports", "weakens", "neutral", "missing_required_source"];

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

fn load_doc() -> Result<String, String> {
    let path = repo_root().join(DOC_PATH);
    fs::read_to_string(&path).map_err(|error| format!("read {}: {error}", path.display()))
}

fn collect_string_vec(node: &Value, ctx: &str) -> Result<Vec<String>, String> {
    let array = node
        .as_array()
        .ok_or_else(|| format!("{ctx}: expected array, got {node}"))?;
    array
        .iter()
        .map(|value| {
            value
                .as_str()
                .map(str::to_owned)
                .ok_or_else(|| format!("{ctx}: non-string entry {value}"))
        })
        .collect()
}

fn collect_string_set(node: &Value, ctx: &str) -> Result<BTreeSet<String>, String> {
    Ok(collect_string_vec(node, ctx)?.into_iter().collect())
}

fn expected_string_set(values: &[&str]) -> BTreeSet<String> {
    values.iter().map(|value| (*value).to_owned()).collect()
}

fn require_exact_strings(
    schema: &Value,
    pointer: &str,
    expected: &[&str],
    label: &str,
) -> TestResult {
    let actual = collect_string_set(schema.pointer(pointer).unwrap_or(&Value::Null), label)?;
    let expected = expected_string_set(expected);
    ensure(
        actual == expected,
        format!("{label} drifted from exact set; expected {expected:?}, got {actual:?}"),
    )
}

fn require_exact_ordered_strings(
    schema: &Value,
    pointer: &str,
    expected: &[&str],
    label: &str,
) -> TestResult {
    let actual = collect_string_vec(schema.pointer(pointer).unwrap_or(&Value::Null), label)?;
    let expected: Vec<String> = expected.iter().map(|value| (*value).to_owned()).collect();
    ensure(
        actual == expected,
        format!("{label} drifted from exact ordered list; expected {expected:?}, got {actual:?}"),
    )
}

fn require_schema_identity(schema: &Value) -> TestResult {
    ensure(
        schema.pointer("/$id").and_then(Value::as_str) == Some(SCHEMA_ID),
        format!("regression causality schema $id must stay {SCHEMA_ID}"),
    )?;
    ensure(
        schema.pointer("/title").and_then(Value::as_str) == Some(SCHEMA_NAME),
        format!("regression causality schema title must stay {SCHEMA_NAME}"),
    )?;
    ensure(
        schema
            .pointer("/properties/schema/const")
            .and_then(Value::as_str)
            == Some(SCHEMA_NAME),
        format!("regression causality schema const must stay {SCHEMA_NAME}"),
    )?;
    ensure(
        schema
            .pointer("/additionalProperties")
            .and_then(Value::as_bool)
            == Some(false),
        "regression causality top-level schema must remain closed",
    )
}

fn require_example_fields(schema: &Value, required: &[&str]) -> TestResult {
    let example = schema
        .pointer("/examples/0")
        .and_then(Value::as_object)
        .ok_or_else(|| "regression causality schema must include an object example".to_string())?;
    for field in required {
        ensure(
            example.contains_key(*field),
            format!("regression causality example missing required field `{field}`"),
        )?;
    }
    Ok(())
}

fn require_preset_fields_are_top_level_or_wildcard(schema: &Value) -> TestResult {
    let top_level_fields: BTreeSet<String> = schema
        .pointer("/properties")
        .and_then(Value::as_object)
        .ok_or_else(|| "regression causality schema properties must be an object".to_string())?
        .keys()
        .cloned()
        .collect();
    let presets = schema
        .pointer("/field_presets")
        .and_then(Value::as_object)
        .ok_or_else(|| "regression causality schema field_presets must be an object".to_string())?;
    for (preset_name, preset) in presets {
        for field in collect_string_vec(
            preset,
            &format!("regression causality {preset_name} preset"),
        )? {
            ensure(
                field == "*" || top_level_fields.contains(&field),
                format!(
                    "regression causality {preset_name} preset references non-top-level field `{field}`"
                ),
            )?;
        }
    }
    Ok(())
}

fn require_no_raw_sensitive_example_values(schema: &Value) -> TestResult {
    let example = schema
        .pointer("/examples/0")
        .ok_or_else(|| "regression causality schema must include an example".to_string())?
        .to_string();
    for forbidden in [
        "/Users/",
        "/data/projects/",
        "/tmp/",
        "api_key",
        "id_ed25519",
        "raw stdout",
        "raw stderr",
        "mail body",
        "memory body",
    ] {
        ensure(
            !example.contains(forbidden),
            format!("regression causality example leaks raw sensitive fixture value `{forbidden}`"),
        )?;
    }
    Ok(())
}

fn require_bool_const(schema: &Value, pointer: &str, expected: bool, label: &str) -> TestResult {
    ensure(
        schema.pointer(pointer).and_then(Value::as_bool) == Some(expected),
        format!("{label} must stay const {expected}"),
    )
}

#[test]
fn regression_causality_schema_identity_and_top_level_required_fields_are_pinned() -> TestResult {
    let schema = load_schema()?;
    require_schema_identity(&schema)?;
    require_exact_strings(
        &schema,
        "/required",
        REQUIRED_TOP_LEVEL,
        "regression causality top-level required fields",
    )?;
    require_example_fields(&schema, REQUIRED_TOP_LEVEL)?;
    require_no_raw_sensitive_example_values(&schema)
}

#[test]
fn regression_causality_schema_nested_required_fields_are_pinned() -> TestResult {
    let schema = load_schema()?;
    for (pointer, expected, label) in [
        (
            "/$defs/subject/required",
            REQUIRED_SUBJECT,
            "regression causality subject required fields",
        ),
        (
            "/$defs/sourceState/required",
            REQUIRED_SOURCE_STATE,
            "regression causality source state required fields",
        ),
        (
            "/$defs/evidenceSource/required",
            REQUIRED_EVIDENCE_SOURCE,
            "regression causality evidence source required fields",
        ),
        (
            "/$defs/hypothesis/required",
            REQUIRED_HYPOTHESIS,
            "regression causality hypothesis required fields",
        ),
        (
            "/$defs/counterEvidence/required",
            REQUIRED_COUNTER_EVIDENCE,
            "regression causality counter-evidence required fields",
        ),
        (
            "/$defs/ownerHint/required",
            REQUIRED_OWNER_HINT,
            "regression causality owner hint required fields",
        ),
        (
            "/$defs/command/required",
            REQUIRED_COMMAND,
            "regression causality command required fields",
        ),
        (
            "/$defs/redaction/required",
            REQUIRED_REDACTION,
            "regression causality redaction required fields",
        ),
        (
            "/$defs/degradedEntry/required",
            REQUIRED_DEGRADED_ENTRY,
            "regression causality degraded entry required fields",
        ),
    ] {
        require_exact_strings(&schema, pointer, expected, label)?;
    }
    Ok(())
}

#[test]
fn regression_causality_schema_field_presets_are_pinned() -> TestResult {
    let schema = load_schema()?;
    for (pointer, expected, label) in [
        (
            "/field_presets/minimal",
            PRESET_MINIMAL,
            "regression causality minimal field preset",
        ),
        (
            "/field_presets/summary",
            PRESET_SUMMARY,
            "regression causality summary field preset",
        ),
        (
            "/field_presets/standard",
            PRESET_STANDARD,
            "regression causality standard field preset",
        ),
        (
            "/field_presets/full",
            PRESET_FULL,
            "regression causality full field preset",
        ),
    ] {
        require_exact_ordered_strings(&schema, pointer, expected, label)?;
    }
    require_preset_fields_are_top_level_or_wildcard(&schema)
}

#[test]
fn regression_causality_schema_enum_vocabularies_are_pinned() -> TestResult {
    let schema = load_schema()?;
    for (pointer, expected, label) in [
        (
            "/$defs/subject/properties/surface/enum",
            SURFACES,
            "regression causality subject surface enum",
        ),
        (
            "/$defs/sourceState/properties/materialization/enum",
            MATERIALIZATIONS,
            "regression causality source materialization enum",
        ),
        (
            "/$defs/evidenceSource/properties/kind/enum",
            EVIDENCE_KINDS,
            "regression causality evidence kind enum",
        ),
        (
            "/$defs/evidenceSource/properties/status/enum",
            EVIDENCE_STATUSES,
            "regression causality evidence status enum",
        ),
        (
            "/$defs/evidenceSource/properties/redactionStatus/enum",
            REDACTION_STATUSES,
            "regression causality evidence redaction status enum",
        ),
        (
            "/$defs/hypothesis/properties/code/enum",
            HYPOTHESIS_CODES,
            "regression causality hypothesis code enum",
        ),
        (
            "/$defs/severity/enum",
            SEVERITIES,
            "regression causality severity enum",
        ),
        (
            "/$defs/ownerHint/properties/kind/enum",
            OWNER_HINT_KINDS,
            "regression causality owner hint kind enum",
        ),
        (
            "/$defs/counterEvidence/properties/effect/enum",
            COUNTER_EVIDENCE_EFFECTS,
            "regression causality counter-evidence effect enum",
        ),
    ] {
        require_exact_strings(&schema, pointer, expected, label)?;
    }
    Ok(())
}

#[test]
fn regression_causality_schema_redaction_booleans_and_non_authoritative_hypotheses_are_pinned()
-> TestResult {
    let schema = load_schema()?;
    for field in [
        "rawLogsPresent",
        "rawMailBodiesPresent",
        "rawMemoryBodiesPresent",
        "privatePathsPresent",
    ] {
        require_bool_const(
            &schema,
            &format!("/$defs/redaction/properties/{field}/const"),
            false,
            &format!("regression causality redaction {field}"),
        )?;
    }
    require_bool_const(
        &schema,
        "/$defs/redaction/properties/secretScanApplied/const",
        true,
        "regression causality redaction secretScanApplied",
    )?;
    require_bool_const(
        &schema,
        "/$defs/hypothesis/properties/authoritative/const",
        false,
        "regression causality hypothesis authoritative",
    )?;
    require_bool_const(
        &schema,
        "/$defs/command/properties/mutatesWorkspace/const",
        false,
        "regression causality command mutatesWorkspace",
    )
}

#[test]
fn regression_causality_agent_docs_cover_inventory_and_abstention_rules() -> TestResult {
    let doc = load_doc()?;
    for expected in [
        SCHEMA_NAME,
        "Accepted Inputs",
        "verification_evidence",
        "rch_selector_admission",
        "swarm_replay",
        "e2e_event_log",
        "pack_replay",
        "pack_diff",
        "perf_report",
        "beads_history",
        "bv_history",
        "degraded_fixture",
        "git_metadata",
        "support_bundle",
        "source_not_materialized",
        "unknown_insufficient_evidence",
        "authoritative: false",
        "RCH-only",
    ] {
        ensure(
            doc.contains(expected),
            format!("regression causality docs missing `{expected}`"),
        )?;
    }
    Ok(())
}
