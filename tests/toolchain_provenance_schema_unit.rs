//! bd-aunn3.2 — shipped contract tests for `ee.toolchain_provenance.v1` (ADR 0072).
//!
//! Pins the structural contract for the shipped toolchain-provenance capsule
//! (bd-aunn3.2; `x-ee-status.shipped = true`) and the bd-aunn3.4
//! conformance closeout. Asserts:
//!
//! 1. The schema file exists, parses, and `$id`/`title`/`const` agree.
//! 2. The capsule and tool-row required-field sets match ADR 0072 §§1–2.
//! 3. The §3 freshness-state vocabulary is pinned exactly.
//! 4. The redaction posture is pinned: `redactionStatus` is the const
//!    `paths_workspace_relative_or_hashed_no_content` and `redactedPath`
//!    forbids absolute paths.
//! 5. `x-ee-status` marks the surface shipped and tracks bd-aunn3.2.
//! 6. Tool rows record bounded probe evidence: command id, exit class,
//!    and duration.
//! 7. The four round-trip fixtures (`fresh`, `stale_binary`,
//!    `agent_mail_corrupt`, `bv_rch_timeout`) validate structurally against
//!    the schema's required sets, enums, and redaction rules, and
//!    deterministically re-serialize (parse → serialize → parse is
//!    identity).
//! 8. The toolchain degraded codes have matching failure-mode catalog
//!    fixtures and README rows.
//! 9. A no-mock smoke pass runs the live collector on the current machine
//!    and proves the emitted capsule is still schema-valid and redacted.
//!
//! Like `bead_affinity_schema_unit.rs`, this reasons over the schema JSON
//! directly; the live-response drift lane can now exercise the shipped
//! emitter.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeSet;
use std::path::PathBuf;

use ee::core::support_bundle::{
    TOOLCHAIN_PROVENANCE_REDACTION_STATUS, TOOLCHAIN_PROVENANCE_SCHEMA_V1,
    ToolchainProvenanceOptions, collect_toolchain_provenance,
};
use serde_json::Value;

type TestResult = Result<(), String>;

const SCHEMA_REL: &str = "docs/schemas/ee.toolchain_provenance.v1.json";
const SCHEMA_NAME: &str = "ee.toolchain_provenance.v1";
const FIXTURES_REL: &str = "tests/fixtures/toolchain_provenance";
const FIXTURE_NAMES: [&str; 4] = [
    "fresh",
    "stale_binary",
    "agent_mail_corrupt",
    "bv_rch_timeout",
];
const FAILURE_MODES_REL: &str = "tests/fixtures/failure_modes";
const REDACTION_CONST: &str = "paths_workspace_relative_or_hashed_no_content";
const FRESHNESS_STATES: [&str; 8] = [
    "current",
    "stale_binary",
    "source_mismatch",
    "wrapper_missing",
    "health_corrupt",
    "command_timeout",
    "version_unknown",
    "unsupported_platform",
];
const TOOLCHAIN_FAILURE_CODES: [(&str, &str); 3] = [
    ("toolchain_hash_unavailable", "info"),
    ("toolchain_probe_timeout", "low"),
    ("toolchain_tool_unresolved", "low"),
];
const TOOLCHAIN_TOOL_NAMES: [&str; 8] = [
    "agent_mail",
    "br",
    "bv",
    "cargo",
    "cass",
    "ee",
    "git",
    "rch",
];
const TOOLCHAIN_SCRIPT_NAMES: [&str; 5] = [
    "scripts/agent_mail_snapshot.sh",
    "scripts/br_retry.sh",
    "scripts/commit-hygiene-classifier.sh",
    "scripts/rch_lane_doctor.sh",
    "scripts/rch_verify.sh",
];

fn manifest_path(relative: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(relative)
}

fn load_json(relative: &str) -> Result<Value, String> {
    let path = manifest_path(relative);
    let bytes =
        std::fs::read(&path).map_err(|error| format!("read {}: {error}", path.display()))?;
    serde_json::from_slice::<Value>(&bytes)
        .map_err(|error| format!("parse {}: {error}", path.display()))
}

fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

fn string_set(value: &Value, pointer: &str) -> Result<BTreeSet<String>, String> {
    let node = value
        .pointer(pointer)
        .ok_or_else(|| format!("schema is missing pointer {pointer}"))?;
    let array = node
        .as_array()
        .ok_or_else(|| format!("{pointer} must be a JSON array"))?;
    let mut out = BTreeSet::new();
    for entry in array {
        let name = entry
            .as_str()
            .ok_or_else(|| format!("{pointer} contains a non-string entry: {entry}"))?;
        out.insert(name.to_owned());
    }
    Ok(out)
}

#[test]
fn schema_identity_and_status_marker() -> TestResult {
    let schema = load_json(SCHEMA_REL)?;
    ensure(
        schema.pointer("/title").and_then(Value::as_str) == Some(SCHEMA_NAME),
        "schema title must be the schema name",
    )?;
    ensure(
        schema
            .pointer("/$id")
            .and_then(Value::as_str)
            .is_some_and(|id| id.ends_with(&format!("{SCHEMA_NAME}.json"))),
        "$id must end with the schema file name",
    )?;
    ensure(
        schema
            .pointer("/properties/schema/const")
            .and_then(Value::as_str)
            == Some(SCHEMA_NAME),
        "schema.const must pin the schema name",
    )?;
    ensure(
        schema
            .pointer("/x-ee-status/shipped")
            .and_then(Value::as_bool)
            == Some(true),
        "x-ee-status.shipped must be true once bd-aunn3.2 lands collectors",
    )?;
    ensure(
        schema
            .pointer("/x-ee-status/available_in_build")
            .and_then(Value::as_bool)
            == Some(true),
        "x-ee-status.available_in_build must be true once the collector is wired",
    )?;
    ensure(
        schema
            .pointer("/x-ee-status/tracking_bead")
            .and_then(Value::as_str)
            == Some("bd-aunn3.2"),
        "x-ee-status.tracking_bead must point at the collector bead",
    )
}

#[test]
fn required_field_sets_match_adr() -> TestResult {
    let schema = load_json(SCHEMA_REL)?;
    let capsule_required = string_set(&schema, "/required")?;
    let expected_capsule: BTreeSet<String> = [
        "schema",
        "collectedAt",
        "workspaceFingerprint",
        "redactionStatus",
        "tools",
        "scriptHashes",
        "degraded",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    ensure(
        capsule_required == expected_capsule,
        format!("capsule required set drifted: {capsule_required:?}"),
    )?;

    let tool_required = string_set(&schema, "/$defs/toolRow/required")?;
    let expected_tool: BTreeSet<String> = [
        "tool",
        "kind",
        "resolvedPath",
        "version",
        "binaryHash",
        "sourceHint",
        "freshness",
        "probe",
        "degraded",
        "checkedAt",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    ensure(
        tool_required == expected_tool,
        format!("toolRow required set drifted: {tool_required:?}"),
    )
}

#[test]
fn freshness_vocabulary_and_redaction_are_pinned() -> TestResult {
    let schema = load_json(SCHEMA_REL)?;
    let states = string_set(&schema, "/$defs/freshnessState/enum")?;
    let expected: BTreeSet<String> = FRESHNESS_STATES.iter().map(|s| (*s).to_owned()).collect();
    ensure(
        states == expected,
        format!("freshness vocabulary drifted: {states:?}"),
    )?;
    ensure(
        schema
            .pointer("/properties/redactionStatus/const")
            .and_then(Value::as_str)
            == Some(REDACTION_CONST),
        "redactionStatus const drifted",
    )?;
    ensure(
        schema
            .pointer("/$defs/redactedPath/not/pattern")
            .and_then(Value::as_str)
            == Some("^/"),
        "redactedPath must forbid absolute paths",
    )
}

/// Minimal structural validator for this schema family: required sets,
/// closed property sets, enum membership, and the redaction rules. (The
/// repo intentionally has no general jsonschema engine dependency; this
/// covers exactly the constructs the schema uses.)
fn validate_capsule(schema: &Value, capsule: &Value, label: &str) -> TestResult {
    let object = capsule
        .as_object()
        .ok_or_else(|| format!("{label}: capsule must be an object"))?;
    for field in string_set(schema, "/required")? {
        ensure(
            object.contains_key(&field),
            format!("{label}: capsule missing required field {field}"),
        )?;
    }
    let allowed = schema
        .pointer("/properties")
        .and_then(Value::as_object)
        .ok_or("schema /properties missing")?;
    for key in object.keys() {
        ensure(
            allowed.contains_key(key),
            format!("{label}: capsule has unknown field {key} (additionalProperties=false)"),
        )?;
    }
    ensure(
        capsule.pointer("/schema").and_then(Value::as_str) == Some(SCHEMA_NAME),
        format!("{label}: schema field must be {SCHEMA_NAME}"),
    )?;
    ensure(
        capsule.pointer("/redactionStatus").and_then(Value::as_str) == Some(REDACTION_CONST),
        format!("{label}: redactionStatus must be the pinned const"),
    )?;

    let states: BTreeSet<String> = FRESHNESS_STATES.iter().map(|s| (*s).to_owned()).collect();
    let tool_required = string_set(schema, "/$defs/toolRow/required")?;
    let tool_enum = string_set(schema, "/$defs/toolRow/properties/tool/enum")?;
    let kind_enum = string_set(schema, "/$defs/toolRow/properties/kind/enum")?;
    let hint_enum = string_set(schema, "/$defs/toolRow/properties/sourceHint/enum")?;
    let probe_exit_enum = string_set(schema, "/$defs/probeEvidence/properties/exitClass/enum")?;
    let degraded_code_enum = string_set(schema, "/$defs/degradedEntry/properties/code/enum")?;
    let severity_enum = string_set(schema, "/$defs/degradedEntry/properties/severity/enum")?;

    let validate_degraded = |entries: &Value, where_: &str| -> TestResult {
        for entry in entries.as_array().unwrap_or(&Vec::new()) {
            let code = entry.pointer("/code").and_then(Value::as_str).unwrap_or("");
            let severity = entry
                .pointer("/severity")
                .and_then(Value::as_str)
                .unwrap_or("");
            ensure(
                degraded_code_enum.contains(code),
                format!("{label}: {where_} degraded code {code:?} not in enum"),
            )?;
            ensure(
                severity_enum.contains(severity),
                format!("{label}: {where_} severity {severity:?} not in enum"),
            )?;
            ensure(
                entry.pointer("/message").and_then(Value::as_str).is_some(),
                format!("{label}: {where_} degraded entry missing message"),
            )?;
        }
        Ok(())
    };

    let tools = capsule
        .pointer("/tools")
        .and_then(Value::as_array)
        .ok_or_else(|| format!("{label}: tools must be an array"))?;
    for (index, row) in tools.iter().enumerate() {
        let row_object = row
            .as_object()
            .ok_or_else(|| format!("{label}: tools[{index}] must be an object"))?;
        for field in &tool_required {
            ensure(
                row_object.contains_key(field),
                format!("{label}: tools[{index}] missing required field {field}"),
            )?;
        }
        let tool = row.pointer("/tool").and_then(Value::as_str).unwrap_or("");
        ensure(
            tool_enum.contains(tool),
            format!("{label}: tools[{index}].tool {tool:?} not in enum"),
        )?;
        let kind = row.pointer("/kind").and_then(Value::as_str).unwrap_or("");
        ensure(
            kind_enum.contains(kind),
            format!("{label}: tools[{index}].kind {kind:?} not in enum"),
        )?;
        let hint = row
            .pointer("/sourceHint")
            .and_then(Value::as_str)
            .unwrap_or("");
        ensure(
            hint_enum.contains(hint),
            format!("{label}: tools[{index}].sourceHint {hint:?} not in enum"),
        )?;
        let freshness = row
            .pointer("/freshness")
            .and_then(Value::as_str)
            .unwrap_or("");
        ensure(
            states.contains(freshness),
            format!("{label}: tools[{index}].freshness {freshness:?} not in vocabulary"),
        )?;
        if let Some(path) = row.pointer("/resolvedPath").and_then(Value::as_str) {
            ensure(
                !path.starts_with('/'),
                format!("{label}: tools[{index}].resolvedPath leaks an absolute path"),
            )?;
        }
        if let Some(hash) = row.pointer("/binaryHash").and_then(Value::as_str) {
            ensure(
                hash.starts_with("blake3:") && hash.len() == 71,
                format!("{label}: tools[{index}].binaryHash malformed"),
            )?;
        }
        let probe = row
            .pointer("/probe")
            .ok_or_else(|| format!("{label}: tools[{index}].probe missing"))?;
        let command_id = probe
            .pointer("/commandId")
            .and_then(Value::as_str)
            .unwrap_or("");
        ensure(
            command_id.starts_with("toolchain_"),
            format!("{label}: tools[{index}].probe.commandId must be stable toolchain_* id"),
        )?;
        let exit_class = probe
            .pointer("/exitClass")
            .and_then(Value::as_str)
            .unwrap_or("");
        ensure(
            probe_exit_enum.contains(exit_class),
            format!("{label}: tools[{index}].probe.exitClass {exit_class:?} not in enum"),
        )?;
        ensure(
            probe
                .pointer("/durationMs")
                .and_then(Value::as_u64)
                .is_some(),
            format!("{label}: tools[{index}].probe.durationMs must be an integer"),
        )?;
        validate_degraded(&row["degraded"], &format!("tools[{index}]"))?;
    }

    for (index, row) in capsule
        .pointer("/scriptHashes")
        .and_then(Value::as_array)
        .ok_or_else(|| format!("{label}: scriptHashes must be an array"))?
        .iter()
        .enumerate()
    {
        let script = row.pointer("/script").and_then(Value::as_str).unwrap_or("");
        ensure(
            script.starts_with("scripts/"),
            format!("{label}: scriptHashes[{index}].script must be workspace-relative scripts/"),
        )?;
        let hash = row.pointer("/blake3").and_then(Value::as_str).unwrap_or("");
        ensure(
            hash.starts_with("blake3:") && hash.len() == 71,
            format!("{label}: scriptHashes[{index}].blake3 malformed"),
        )?;
        ensure(
            row.pointer("/tracked").and_then(Value::as_bool).is_some(),
            format!("{label}: scriptHashes[{index}].tracked must be a boolean"),
        )?;
    }

    validate_degraded(&capsule["degraded"], "capsule")
}

#[test]
fn fixtures_round_trip_and_validate() -> TestResult {
    let schema = load_json(SCHEMA_REL)?;
    for name in FIXTURE_NAMES {
        let relative = format!("{FIXTURES_REL}/{name}.json");
        let capsule = load_json(&relative)?;
        validate_capsule(&schema, &capsule, name)?;
        // Round trip: parse → serialize → parse is identity, so the fixture
        // contains nothing serde_json cannot faithfully re-emit.
        let serialized = serde_json::to_string(&capsule)
            .map_err(|error| format!("{name}: serialize: {error}"))?;
        let reparsed: Value = serde_json::from_str(&serialized)
            .map_err(|error| format!("{name}: reparse: {error}"))?;
        ensure(reparsed == capsule, format!("{name}: round trip drifted"))?;
    }
    Ok(())
}

#[test]
fn fixtures_cover_the_required_failure_modes() -> TestResult {
    let stale = load_json(&format!("{FIXTURES_REL}/stale_binary.json"))?;
    ensure(
        stale.pointer("/tools/0/freshness").and_then(Value::as_str) == Some("stale_binary"),
        "stale_binary fixture must carry a stale_binary ee row",
    )?;
    let corrupt = load_json(&format!("{FIXTURES_REL}/agent_mail_corrupt.json"))?;
    ensure(
        corrupt
            .pointer("/tools/0/freshness")
            .and_then(Value::as_str)
            == Some("health_corrupt"),
        "agent_mail_corrupt fixture must carry a health_corrupt agent_mail row",
    )?;
    let timeout = load_json(&format!("{FIXTURES_REL}/bv_rch_timeout.json"))?;
    let timed_out: Vec<&str> = timeout
        .pointer("/tools")
        .and_then(Value::as_array)
        .unwrap()
        .iter()
        .filter(|row| row.pointer("/freshness").and_then(Value::as_str) == Some("command_timeout"))
        .map(|row| row.pointer("/tool").and_then(Value::as_str).unwrap_or(""))
        .collect();
    ensure(
        timed_out.contains(&"bv") && timed_out.contains(&"rch"),
        "bv_rch_timeout fixture must time out both bv and rch",
    )?;
    let fresh = load_json(&format!("{FIXTURES_REL}/fresh.json"))?;
    ensure(
        fresh
            .pointer("/tools")
            .and_then(Value::as_array)
            .is_some_and(|tools| {
                tools
                    .iter()
                    .all(|row| row.pointer("/freshness").and_then(Value::as_str) == Some("current"))
            }),
        "fresh fixture must be fully current",
    )
}

#[test]
fn failure_catalog_entries_match_toolchain_schema_codes() -> TestResult {
    let schema = load_json(SCHEMA_REL)?;
    let degraded_code_enum = string_set(&schema, "/$defs/degradedEntry/properties/code/enum")?;
    let readme_path = manifest_path(&format!("{FAILURE_MODES_REL}/README.md"));
    let readme = std::fs::read_to_string(&readme_path)
        .map_err(|error| format!("read {}: {error}", readme_path.display()))?;

    for (code, severity) in TOOLCHAIN_FAILURE_CODES {
        ensure(
            degraded_code_enum.contains(code),
            format!("{code}: schema degraded-code enum is missing the cataloged code"),
        )?;
        let fixture = load_json(&format!("{FAILURE_MODES_REL}/{code}.json"))?;
        ensure(
            fixture.pointer("/schema").and_then(Value::as_str)
                == Some("ee.failure_mode_fixture.v1"),
            format!("{code}: failure fixture schema drifted"),
        )?;
        ensure(
            fixture.pointer("/code").and_then(Value::as_str) == Some(code),
            format!("{code}: failure fixture code drifted"),
        )?;
        ensure(
            fixture.pointer("/severity").and_then(Value::as_str) == Some(severity),
            format!("{code}: failure fixture severity drifted"),
        )?;
        ensure(
            fixture
                .pointer("/introduced_by/bead")
                .and_then(Value::as_str)
                == Some("bd-aunn3.2"),
            format!("{code}: failure fixture must keep the collector bead as introducer"),
        )?;
        ensure(
            fixture
                .pointer("/surfaces")
                .and_then(Value::as_array)
                .is_some_and(|surfaces| {
                    surfaces
                        .iter()
                        .any(|surface| surface.as_str() == Some("diag toolchain-provenance"))
                }),
            format!("{code}: failure fixture must include diag toolchain-provenance surface"),
        )?;
        ensure(
            fixture
                .pointer("/expected_emission/code")
                .and_then(Value::as_str)
                == Some(code),
            format!("{code}: expected emission code drifted"),
        )?;
        ensure(
            fixture
                .pointer("/expected_emission/severity")
                .and_then(Value::as_str)
                == Some(severity),
            format!("{code}: expected emission severity drifted"),
        )?;
        let readme_row =
            format!("| `{code}` | diag toolchain-provenance | {severity} | bd-aunn3.2 |");
        ensure(
            readme.contains(&readme_row),
            format!("{code}: failure-mode README row missing or drifted"),
        )?;
    }
    Ok(())
}

#[test]
fn live_toolchain_collector_smoke_is_schema_valid_and_redacted() -> TestResult {
    let workspace = std::env::current_dir().map_err(|error| format!("current_dir: {error}"))?;
    let mut options = ToolchainProvenanceOptions::for_workspace(&workspace);
    options.command_timeout_ms = 750;

    let report = collect_toolchain_provenance(&options);
    ensure(
        report.schema == TOOLCHAIN_PROVENANCE_SCHEMA_V1,
        "live smoke emitted the wrong schema",
    )?;
    ensure(
        report.redaction_status == TOOLCHAIN_PROVENANCE_REDACTION_STATUS,
        "live smoke emitted the wrong redaction posture",
    )?;

    let schema = load_json(SCHEMA_REL)?;
    let capsule =
        serde_json::to_value(&report).map_err(|error| format!("serialize live smoke: {error}"))?;
    validate_capsule(&schema, &capsule, "live_smoke")?;

    let tools = capsule
        .pointer("/tools")
        .and_then(Value::as_array)
        .ok_or_else(|| "live_smoke: tools must be an array".to_owned())?
        .iter()
        .map(|row| {
            row.pointer("/tool")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned()
        })
        .collect::<BTreeSet<_>>();
    let expected_tools = TOOLCHAIN_TOOL_NAMES
        .iter()
        .map(|tool| (*tool).to_owned())
        .collect::<BTreeSet<_>>();
    ensure(
        tools == expected_tools,
        format!("live_smoke: tool inventory drifted: {tools:?}"),
    )?;

    let scripts = report
        .script_hashes
        .iter()
        .map(|row| row.script.clone())
        .collect::<BTreeSet<_>>();
    let expected_scripts = TOOLCHAIN_SCRIPT_NAMES
        .iter()
        .map(|script| (*script).to_owned())
        .collect::<BTreeSet<_>>();
    ensure(
        scripts == expected_scripts,
        format!("live_smoke: default script hash inventory drifted: {scripts:?}"),
    )?;
    ensure(
        report
            .script_hashes
            .iter()
            .all(|row| row.tracked && row.blake3.starts_with("blake3:")),
        "live_smoke: script hashes must be tracked blake3 rows",
    )?;

    let rendered = capsule.to_string();
    let workspace_text = workspace.display().to_string();
    ensure(
        !rendered.contains(&workspace_text),
        format!("live_smoke: capsule leaked the raw workspace path {workspace_text}"),
    )?;
    Ok(())
}
