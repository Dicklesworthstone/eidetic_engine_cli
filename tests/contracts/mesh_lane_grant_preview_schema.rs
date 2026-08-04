//! bd-tc-epic-qzk7o.2.2 — authenticated mesh lane approval contracts.
//!
//! ADR 0086 supersedes the historical raw-node-key v1 preview at runtime.
//! This contract pins the canonical v2 snapshot, its optional sensitive
//! approval-token projection, and the generation-advancing grant/revoke
//! mutation results. The historical v1 JSON file remains documentation, but
//! it must no longer be published through `ee schema list/export`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;

use ee::mesh::lane_grant::{APPROVAL_TOKEN_BEARER_LEN, APPROVAL_TOKEN_SCHEMA_V1};
use ee::mesh::lane_grant_preview::{
    LANE_GRANT_MEMORY_CANDIDATE_KIND, LANE_GRANT_MESH_LEDGER_EVENT_CANDIDATE_KIND,
    LANE_GRANT_PREVIEW_COPY_VERSION, LANE_GRANT_PREVIEW_SCHEMA_V2,
    LANE_GRANT_TARGET_ADAPTER_VERSION, lane_grant_redaction_scanner_generation,
};
use ee::output::{public_schemas, render_schema_export_json};
use serde_json::Value;

type TestResult = Result<(), String>;

const PREVIEW_PATH: &str = "docs/schemas/ee.mesh.lane_grant_preview.v2.json";
const PREVIEW_ID: &str = "https://eidetic-engine/schemas/ee.mesh.lane_grant_preview.v2.json";
const PREVIEW_SCHEMA: &str = LANE_GRANT_PREVIEW_SCHEMA_V2;
const TOKEN_PATH: &str = "docs/schemas/ee.mesh.approval_token.v1.json";
const TOKEN_ID: &str = "https://eidetic-engine/schemas/ee.mesh.approval_token.v1.json";
const TOKEN_SCHEMA: &str = APPROVAL_TOKEN_SCHEMA_V1;
const GRANT_PATH: &str = "docs/schemas/ee.mesh.grant.v1.json";
const GRANT_ID: &str = "https://eidetic-engine/schemas/ee.mesh.grant.v1.json";
const GRANT_SCHEMA: &str = "ee.mesh.grant.v1";
const REVOKE_PATH: &str = "docs/schemas/ee.mesh.revoke_lane.v1.json";
const REVOKE_ID: &str = "https://eidetic-engine/schemas/ee.mesh.revoke_lane.v1.json";
const REVOKE_SCHEMA: &str = "ee.mesh.revoke_lane.v1";
const HISTORICAL_PREVIEW_PATH: &str = "docs/schemas/ee.mesh.lane_grant_preview.v1.json";
const HISTORICAL_PREVIEW_SCHEMA: &str = "ee.mesh.lane_grant_preview.v1";

const REQUIRED_TOP_LEVEL: &[&str] = &[
    "schema",
    "copyVersion",
    "workspaceId",
    "target",
    "lane",
    "grantGeneration",
    "currentPolicy",
    "proposedPolicy",
    "candidateSet",
    "affectedMemoryCount",
    "affectedLedgerEventCount",
    "redactedFromExposureCount",
    "previewSampleStrategy",
    "previewSampleLimit",
    "previewSample",
    "redactionRulesApplied",
    "redactionScannerGeneration",
    "cautionCodes",
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
    "agent_validated",
    "agent_assertion",
    "cass_evidence",
    "legacy_import",
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

const REQUIRED_CAUTION_FIELDS: &[&str] = &["kind", "message", "severity"];

const REQUIRED_POLICY_SNAPSHOT_FIELDS: &[&str] = &["generation", "lane", "decision"];

const REQUIRED_GRANT_TARGET_FIELDS: &[&str] = &["adapterVersion", "peerId"];

const REQUIRED_CANDIDATE_PIN_FIELDS: &[&str] = &["candidateKind", "candidateId", "revisionId"];

const REQUIRED_CANDIDATE_KINDS: &[&str] = &[
    LANE_GRANT_MEMORY_CANDIDATE_KIND,
    LANE_GRANT_MESH_LEDGER_EVENT_CANDIDATE_KIND,
];

const REQUIRED_PREVIEW_ROW_FIELDS: &[&str] = &[
    "memoryId",
    "revisionId",
    "level",
    "kind",
    "contentPreview",
    "tags",
    "trustClass",
    "hasSensitiveTags",
    "redactedFields",
    "wouldExposeUnderProposedPolicy",
];

const REQUIRED_TOKEN_FIELDS: &[&str] = &["schema", "value", "expiresAt", "handling"];

const REQUIRED_MUTATION_FIELDS: &[&str] = &[
    "schema",
    "command",
    "workspaceId",
    "target",
    "lane",
    "previousGrantGeneration",
    "newGrantGeneration",
    "decision",
    "auditId",
    "remoteErasureGuaranteed",
    "residual",
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

fn load_schema(relative_path: &str) -> Result<Value, String> {
    let path = repo_root().join(relative_path);
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

fn expected_string_set(expected: &[&str]) -> BTreeSet<String> {
    expected.iter().map(|value| (*value).to_owned()).collect()
}

fn collect_object_keys(node: &Value, ctx: &str) -> Result<BTreeSet<String>, String> {
    let object = node
        .as_object()
        .ok_or_else(|| format!("{ctx}: expected object, got: {node}"))?;
    Ok(object.keys().cloned().collect())
}

fn require_closed_set(schema: &Value, pointer: &str, expected: &[&str], label: &str) -> TestResult {
    let actual = collect_strings(schema.pointer(pointer).unwrap_or(&Value::Null), label)?;
    let want = expected_string_set(expected);
    ensure(
        actual == want,
        format!("{label} drifted from closed set; expected {want:?}, got {actual:?}"),
    )
}

fn require_required_fields(
    schema: &Value,
    pointer: &str,
    expected: &[&str],
    label: &str,
) -> TestResult {
    let actual = collect_strings(schema.pointer(pointer).unwrap_or(&Value::Null), label)?;
    let expected = expected_string_set(expected);
    ensure(
        actual == expected,
        format!("{label} drifted from required field set; expected {expected:?}, got {actual:?}"),
    )
}

fn require_property_fields(
    schema: &Value,
    pointer: &str,
    expected: &[&str],
    label: &str,
) -> TestResult {
    let actual = collect_object_keys(schema.pointer(pointer).unwrap_or(&Value::Null), label)?;
    let expected = expected_string_set(expected);
    ensure(
        actual == expected,
        format!("{label} drifted from property field set; expected {expected:?}, got {actual:?}"),
    )
}

fn require_identity(schema: &Value, schema_id: &str, schema_name: &str) -> TestResult {
    ensure(
        schema["$id"] == schema_id,
        format!("expected $id={schema_id}; got: {}", schema["$id"]),
    )?;
    ensure(
        schema["title"] == schema_name,
        format!("expected title={schema_name}; got: {}", schema["title"]),
    )?;
    ensure(
        schema["properties"]["schema"]["const"] == schema_name,
        format!("properties.schema.const must equal {schema_name}"),
    )
}

#[test]
fn authenticated_lane_contract_schema_files_exist_and_parse() -> TestResult {
    for path in [PREVIEW_PATH, TOKEN_PATH, GRANT_PATH, REVOKE_PATH] {
        let _ = load_schema(path)?;
    }
    Ok(())
}

#[test]
fn authenticated_lane_contract_schema_identities_are_consistent() -> TestResult {
    for (path, schema_id, schema_name) in [
        (PREVIEW_PATH, PREVIEW_ID, PREVIEW_SCHEMA),
        (TOKEN_PATH, TOKEN_ID, TOKEN_SCHEMA),
        (GRANT_PATH, GRANT_ID, GRANT_SCHEMA),
        (REVOKE_PATH, REVOKE_ID, REVOKE_SCHEMA),
    ] {
        require_identity(&load_schema(path)?, schema_id, schema_name)?;
    }
    Ok(())
}

#[test]
fn schema_registry_publishes_v2_token_and_mutations_but_not_historical_v1() -> TestResult {
    let historical = load_schema(HISTORICAL_PREVIEW_PATH)?;
    require_identity(
        &historical,
        "https://eidetic-engine/schemas/ee.mesh.lane_grant_preview.v1.json",
        HISTORICAL_PREVIEW_SCHEMA,
    )?;
    let published = public_schemas()
        .iter()
        .map(|entry| entry.id)
        .collect::<BTreeSet<_>>();
    for required in [PREVIEW_SCHEMA, TOKEN_SCHEMA, GRANT_SCHEMA, REVOKE_SCHEMA] {
        ensure(
            published.contains(required),
            format!("public schema registry is missing {required}"),
        )?;
    }
    ensure(
        !published.contains(HISTORICAL_PREVIEW_SCHEMA),
        "historical ee.mesh.lane_grant_preview.v1 must not remain runtime-published",
    )
}

#[test]
fn public_schema_exports_are_byte_semantically_equal_to_docs() -> TestResult {
    for (path, schema_name) in [
        (PREVIEW_PATH, PREVIEW_SCHEMA),
        (TOKEN_PATH, TOKEN_SCHEMA),
        (GRANT_PATH, GRANT_SCHEMA),
        (REVOKE_PATH, REVOKE_SCHEMA),
    ] {
        let exported: Value =
            serde_json::from_str(&render_schema_export_json(Some(schema_name)))
                .map_err(|error| format!("parse schema export {schema_name}: {error}"))?;
        let documented = load_schema(path)?;
        ensure(
            exported == documented,
            format!("schema export {schema_name} drifted from {path}"),
        )?;
    }
    Ok(())
}

#[test]
fn lane_grant_preview_required_top_level_fields_are_exact_and_closed() -> TestResult {
    let schema = load_schema(PREVIEW_PATH)?;
    require_required_fields(&schema, "/required", REQUIRED_TOP_LEVEL, "preview.required")?;
    ensure(
        schema["additionalProperties"] == Value::Bool(false),
        "preview top level must reject unversioned drift fields",
    )?;
    ensure(
        schema["properties"]["copyVersion"]["const"] == LANE_GRANT_PREVIEW_COPY_VERSION,
        "preview copyVersion must pin the human/JSON renderer copy",
    )
}

#[test]
fn lane_grant_preview_lane_enum_is_six_trust_lanes() -> TestResult {
    let schema = load_schema(PREVIEW_PATH)?;
    require_closed_set(&schema, "/$defs/lane/enum", REQUIRED_LANES, "lane enum")
}

#[test]
fn lane_grant_preview_lane_decision_enum_is_three_states() -> TestResult {
    let schema = load_schema(PREVIEW_PATH)?;
    require_closed_set(
        &schema,
        "/$defs/laneDecision/enum",
        REQUIRED_LANE_DECISIONS,
        "laneDecision enum",
    )
}

#[test]
fn lane_grant_preview_trust_class_enum_is_five_authority_classes() -> TestResult {
    let schema = load_schema(PREVIEW_PATH)?;
    require_closed_set(
        &schema,
        "/$defs/trustClass/enum",
        REQUIRED_TRUST_CLASSES,
        "trustClass enum",
    )
}

#[test]
fn lane_grant_preview_caution_kind_enum_matches_bead_acceptance() -> TestResult {
    let schema = load_schema(PREVIEW_PATH)?;
    require_closed_set(
        &schema,
        "/$defs/cautionCode/enum",
        REQUIRED_CAUTION_KINDS,
        "caution code enum",
    )
}

#[test]
fn lane_grant_preview_caution_severity_excludes_error_per_read_only_invariant() -> TestResult {
    let schema = load_schema(PREVIEW_PATH)?;
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
fn lane_grant_preview_caution_required_fields_match_schema_contract() -> TestResult {
    let schema = load_schema(PREVIEW_PATH)?;
    require_required_fields(
        &schema,
        "/$defs/caution/required",
        REQUIRED_CAUTION_FIELDS,
        "caution.required",
    )
}

#[test]
fn lane_grant_preview_policy_snapshot_required_fields_match_schema_contract() -> TestResult {
    let schema = load_schema(PREVIEW_PATH)?;
    require_required_fields(
        &schema,
        "/$defs/policySnapshot/required",
        REQUIRED_POLICY_SNAPSHOT_FIELDS,
        "policySnapshot.required",
    )
}

#[test]
fn lane_grant_preview_target_and_candidate_pins_are_exact() -> TestResult {
    let schema = load_schema(PREVIEW_PATH)?;
    require_required_fields(
        &schema,
        "/$defs/grantTarget/required",
        REQUIRED_GRANT_TARGET_FIELDS,
        "grantTarget.required",
    )?;
    require_required_fields(
        &schema,
        "/$defs/candidatePin/required",
        REQUIRED_CANDIDATE_PIN_FIELDS,
        "candidatePin.required",
    )?;
    ensure(
        schema["properties"]["target"]["$ref"] == "#/$defs/grantTarget",
        "preview target must use the versioned grant-target adapter",
    )?;
    ensure(
        schema["properties"]["candidateSet"]["items"]["$ref"] == "#/$defs/candidatePin",
        "the complete candidate set must consist of generic candidate pins",
    )?;
    ensure(
        schema["$defs"]["grantTarget"]["properties"]["adapterVersion"]["const"]
            == LANE_GRANT_TARGET_ADAPTER_VERSION,
        "grant target adapter version must remain explicit",
    )?;
    ensure(
        schema["$defs"]["grantTarget"]["properties"]
            .get("originNodeId")
            .is_none(),
        "public grant target must not expose its internal origin-node adapter binding",
    )?;
    require_closed_set(
        &schema,
        "/$defs/candidateKind/enum",
        REQUIRED_CANDIDATE_KINDS,
        "candidateKind enum",
    )?;
    require_property_fields(
        &schema,
        "/$defs/candidatePin/properties",
        REQUIRED_CANDIDATE_PIN_FIELDS,
        "candidatePin.properties",
    )?;
    for forbidden in [
        "memoryId",
        "eventJson",
        "contentHash",
        "eventHash",
        "bodyCacheKey",
        "uri",
        "policyDecisionJson",
        "policyFailureSurfaceJson",
    ] {
        ensure(
            schema["$defs"]["candidatePin"]["properties"]
                .get(forbidden)
                .is_none(),
            format!("candidate pins must not expose raw ledger field {forbidden}"),
        )?;
    }
    Ok(())
}

#[test]
fn lane_grant_preview_scanner_generation_is_required_opaque_and_source_derived() -> TestResult {
    let schema = load_schema(PREVIEW_PATH)?;
    ensure(
        schema["properties"]["redactionScannerGeneration"]["pattern"] == "^redscan1_[0-9a-f]{64}$",
        "redaction scanner generation must use the closed opaque v1 shape",
    )?;
    let generation = lane_grant_redaction_scanner_generation();
    ensure(
        generation.starts_with("redscan1_") && generation.len() == "redscan1_".len() + 64,
        format!("runtime scanner generation has invalid shape: {generation}"),
    )?;
    ensure(
        schema["properties"]["redactionScannerGeneration"]["description"]
            .as_str()
            .is_some_and(|description| {
                description.contains("source-derived")
                    && description.contains("without exposing any memory-content hash")
            }),
        "scanner generation description must state its source binding and non-content contract",
    )
}

#[test]
fn lane_grant_preview_sample_contract_is_revision_pinned_and_bounded() -> TestResult {
    let schema = load_schema(PREVIEW_PATH)?;
    require_closed_set(
        &schema,
        "/$defs/sampleStrategy/enum",
        REQUIRED_SAMPLE_STRATEGIES,
        "previewSampleStrategy enum",
    )?;
    require_required_fields(
        &schema,
        "/$defs/previewRow/required",
        REQUIRED_PREVIEW_ROW_FIELDS,
        "previewRow.required",
    )?;
    ensure(
        schema["properties"]["previewSampleLimit"]["maximum"] == 500,
        "previewSampleLimit must remain capped at 500",
    )?;
    ensure(
        schema["properties"]["previewSample"]["maxItems"] == 500,
        "previewSample must remain capped at 500 rows",
    )?;
    ensure(
        schema["properties"]["previewSample"]["items"]["$ref"] == "#/$defs/previewRow",
        "preview samples must use the revision-pinned row contract",
    )?;
    ensure(
        schema["properties"]["cautionCodes"]["items"]["$ref"] == "#/$defs/cautionCode",
        "approval-bound caution codes must use the canonical caution vocabulary",
    )
}

#[test]
fn approval_token_is_optional_sensitive_opaque_and_identifier_free() -> TestResult {
    let preview = load_schema(PREVIEW_PATH)?;
    let preview_required = collect_strings(&preview["required"], "preview.required")?;
    ensure(
        !preview_required.contains("approvalToken"),
        "ordinary deterministic previews must not require or emit approvalToken",
    )?;
    ensure(
        preview["properties"]["approvalToken"]["$ref"] == TOKEN_ID,
        "explicit issuance must use the registered approval-token schema",
    )?;

    let token = load_schema(TOKEN_PATH)?;
    require_required_fields(&token, "/required", REQUIRED_TOKEN_FIELDS, "token.required")?;
    require_property_fields(
        &token,
        "/properties",
        REQUIRED_TOKEN_FIELDS,
        "token.properties",
    )?;
    ensure(
        token["additionalProperties"] == Value::Bool(false),
        "approval token projection must reject identifier/debug field drift",
    )?;
    ensure(
        token["properties"]["handling"]["const"] == "secret",
        "approval token handling must be explicitly marked secret",
    )?;
    ensure(
        token["properties"]["value"]["pattern"] == "^eeap1_[A-Za-z0-9_-]+$",
        "approval bearer must retain its redaction-recognizable eeap1_ prefix",
    )?;
    ensure(
        token["properties"]["value"]["minLength"] == serde_json::json!(APPROVAL_TOKEN_BEARER_LEN)
            && token["properties"]["value"]["maxLength"]
                == serde_json::json!(APPROVAL_TOKEN_BEARER_LEN),
        "v1 approval bearer must remain the fixed 157-character envelope projection",
    )?;

    let properties = token["properties"]
        .as_object()
        .ok_or_else(|| "token.properties must be an object".to_string())?;
    for forbidden in [
        "storeId",
        "storeKeyNamespace",
        "workspaceId",
        "keyId",
        "nonce",
        "snapshotTag",
        "envelopeMac",
        "issuedAt",
    ] {
        ensure(
            !properties.contains_key(forbidden),
            format!("approval token must not expose internal/context field {forbidden}"),
        )?;
    }
    ensure(
        token["description"]
            .as_str()
            .is_some_and(|copy| copy.contains("third-party") && copy.contains("expiry")),
        "approval token schema must name the third-party recorder residual until expiry",
    )
}

#[test]
fn grant_and_revoke_results_pin_generation_audit_and_non_erasure() -> TestResult {
    for (path, command, decision) in [
        (GRANT_PATH, "ee mesh grant", "allow"),
        (REVOKE_PATH, "ee mesh revoke-lane", "deny"),
    ] {
        let schema = load_schema(path)?;
        require_required_fields(
            &schema,
            "/required",
            REQUIRED_MUTATION_FIELDS,
            "mutation.required",
        )?;
        ensure(
            schema["additionalProperties"] == Value::Bool(false),
            format!("{path} must be closed"),
        )?;
        ensure(
            schema["properties"]["command"]["const"] == command,
            format!("{path} command drifted"),
        )?;
        ensure(
            schema["properties"]["decision"]["const"] == decision,
            format!("{path} decision drifted"),
        )?;
        ensure(
            schema["properties"]["remoteErasureGuaranteed"]["const"] == Value::Bool(false),
            format!("{path} must never claim remote erasure"),
        )?;
        ensure(
            schema["properties"]["residual"]["const"]
                .as_str()
                .is_some_and(|copy| copy.contains("cannot erase bytes")),
            format!("{path} must state the cached/copied-byte residual"),
        )?;
        require_required_fields(
            &schema,
            "/$defs/grantTarget/required",
            REQUIRED_GRANT_TARGET_FIELDS,
            "mutation grantTarget.required",
        )?;
        ensure(
            schema["properties"]["target"]["$ref"] == "#/$defs/grantTarget",
            format!("{path} target must use the versioned adapter"),
        )?;
    }
    Ok(())
}
