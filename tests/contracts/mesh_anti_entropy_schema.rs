//! bd-3lx0p (SRR6.7) contract checks for the mesh anti-entropy sync summary.
//!
//! Pins the `ee.mesh.anti_entropy.v1` schema invariants so future runtime work
//! (range supervisor, status renderer, doctor surface) cannot quietly drift
//! away from the public contract described in ADR 0041 and
//! `docs/mesh/anti_entropy.md`.

use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;

use serde_json::Value;

type TestResult = Result<(), String>;

const SCHEMA_PATH: &str = "docs/schemas/ee.mesh.anti_entropy.v1.json";
const ADR_PATH: &str = "docs/adr/0041-mesh-anti-entropy-model.md";
const PROTOCOL_DOC_PATH: &str = "docs/mesh/anti_entropy.md";
const ANTI_ENTROPY_SCHEMA_ID: &str = "ee.mesh.anti_entropy.v1";
const REQUIRED_TOP_LEVEL: &[&str] = &[
    "schema",
    "lastRoundCompletedAt",
    "originsTracked",
    "peerCount",
    "perPeerCounts",
    "backoffPosture",
    "degraded",
];
const FIXTURES: &[&str] = &[
    "tests/fixtures/mesh/anti_entropy_idle.json",
    "tests/fixtures/mesh/anti_entropy_blocked_range.json",
];

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read_json(relative: &str) -> Result<Value, String> {
    let path = repo_root().join(relative);
    let text =
        fs::read_to_string(&path).map_err(|error| format!("read {}: {error}", path.display()))?;
    serde_json::from_str(&text).map_err(|error| format!("parse {}: {error}", path.display()))
}

fn read_text(relative: &str) -> Result<String, String> {
    let path = repo_root().join(relative);
    fs::read_to_string(&path).map_err(|error| format!("read {}: {error}", path.display()))
}

fn ensure<S: Into<String>>(condition: bool, context: S) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(context.into())
    }
}

fn ensure_equal<T>(actual: &T, expected: &T, context: &str) -> TestResult
where
    T: std::fmt::Debug + PartialEq,
{
    if actual == expected {
        Ok(())
    } else {
        Err(format!("{context}: expected {expected:?}, got {actual:?}"))
    }
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

#[test]
fn anti_entropy_schema_pins_redaction_safe_surface() -> TestResult {
    let schema = read_json(SCHEMA_PATH)?;

    ensure_equal(
        &schema.pointer("/$schema").and_then(Value::as_str),
        &Some("https://json-schema.org/draft/2020-12/schema"),
        "json schema draft",
    )?;
    ensure_equal(
        &schema.pointer("/$id").and_then(Value::as_str),
        &Some("https://eidetic-engine/schemas/ee.mesh.anti_entropy.v1.json"),
        "schema id",
    )?;
    ensure_equal(
        &schema.pointer("/title").and_then(Value::as_str),
        &Some(ANTI_ENTROPY_SCHEMA_ID),
        "schema title",
    )?;
    ensure_equal(
        &schema.pointer("/type").and_then(Value::as_str),
        &Some("object"),
        "schema root type",
    )?;
    ensure_equal(
        &schema
            .pointer("/additionalProperties")
            .and_then(Value::as_bool),
        &Some(false),
        "schema root must reject unknown fields",
    )?;
    ensure_equal(
        &schema
            .pointer("/properties/schema/const")
            .and_then(Value::as_str),
        &Some(ANTI_ENTROPY_SCHEMA_ID),
        "schema discriminator",
    )?;

    let actual = collect_strings(
        schema.pointer("/required").unwrap_or(&Value::Null),
        "top-level required",
    )?;
    let expected = expected_string_set(REQUIRED_TOP_LEVEL);
    ensure(
        actual == expected,
        format!(
            "REQUIRED_TOP_LEVEL drifted from schema required array\nexpected={expected:?}\nactual={actual:?}"
        ),
    )?;

    let degraded_codes = schema
        .pointer("/properties/degraded/items/enum")
        .and_then(Value::as_array)
        .ok_or_else(|| "degraded enum missing".to_string())?;
    for code in [
        "mesh_anti_entropy_round_blocked",
        "mesh_anti_entropy_partition_observed",
        "mesh_anti_entropy_fork_observed",
        "mesh_anti_entropy_protocol_error",
        "mesh_anti_entropy_supervisor_budget_exceeded",
        "mesh_anti_entropy_peer_policy_refused",
        "mesh_anti_entropy_transport_unavailable",
    ] {
        ensure(
            degraded_codes
                .iter()
                .any(|value| value.as_str() == Some(code)),
            format!("degraded enum missing {code}"),
        )?;
    }

    let peer_alias_pattern = schema
        .pointer("/$defs/peerAlias/pattern")
        .and_then(Value::as_str)
        .ok_or_else(|| "peerAlias pattern missing".to_string())?;
    ensure(
        peer_alias_pattern.starts_with("^peer_"),
        format!("peerAlias must redact identity behind peer_<hash>, got {peer_alias_pattern}"),
    )?;

    Ok(())
}

#[test]
fn anti_entropy_schema_disallows_raw_identity_fields() -> TestResult {
    let schema = read_json(SCHEMA_PATH)?;
    let schema_text =
        serde_json::to_string(&schema).map_err(|error| format!("serialize schema: {error}"))?;

    // The public sync-summary schema must not invite any caller to render raw
    // peer/origin identity. The enum and field names above are the only safe
    // surface; the schema must not introduce a node-key, IP, or query field.
    for forbidden in [
        "nodeKey",
        "node_key",
        "tailscaleIp",
        "tailnet",
        "queryText",
        "memoryBody",
        "rawOriginNodeId",
        "rawPeerId",
    ] {
        ensure(
            !schema_text.contains(forbidden),
            format!("anti_entropy schema must not expose {forbidden}"),
        )?;
    }
    Ok(())
}

#[test]
fn anti_entropy_fixtures_round_trip_against_schema() -> TestResult {
    for fixture in FIXTURES {
        let value = read_json(fixture)?;
        ensure_equal(
            &value.pointer("/schema").and_then(Value::as_str),
            &Some(ANTI_ENTROPY_SCHEMA_ID),
            fixture,
        )?;
        let serialized = serde_json::to_string(&value)
            .map_err(|error| format!("serialize {fixture}: {error}"))?;
        // Identity-leak guard: fixture authors must use aliases, never raw
        // identifiers, even though the schema also forbids them.
        for forbidden in ["100.64.", "@example.", "node_key=", "ts-node-"] {
            ensure(
                !serialized.contains(forbidden),
                format!("{fixture} embeds raw identity fragment {forbidden}"),
            )?;
        }

        let backoff_posture = value
            .pointer("/backoffPosture")
            .ok_or_else(|| format!("{fixture} missing backoffPosture"))?;
        let max_attempts = backoff_posture
            .pointer("/maxAttempts")
            .and_then(Value::as_u64)
            .ok_or_else(|| format!("{fixture} missing backoffPosture.maxAttempts"))?;
        ensure(
            (1..=10).contains(&max_attempts),
            format!(
                "{fixture} backoffPosture.maxAttempts out of bounded retry range: {max_attempts}"
            ),
        )?;
        let initial_ms = backoff_posture
            .pointer("/initialMs")
            .and_then(Value::as_u64)
            .ok_or_else(|| format!("{fixture} missing backoffPosture.initialMs"))?;
        let max_ms = backoff_posture
            .pointer("/maxMs")
            .and_then(Value::as_u64)
            .ok_or_else(|| format!("{fixture} missing backoffPosture.maxMs"))?;
        ensure(
            initial_ms <= max_ms,
            format!("{fixture} backoff initial {initial_ms} must be <= max {max_ms}"),
        )?;
    }
    Ok(())
}

#[test]
fn anti_entropy_blocked_fixture_carries_consistent_retry_evidence() -> TestResult {
    let value = read_json("tests/fixtures/mesh/anti_entropy_blocked_range.json")?;
    let blocked = value
        .pointer("/blockedRanges")
        .and_then(Value::as_array)
        .ok_or_else(|| "blockedRanges missing".to_string())?;
    ensure(
        !blocked.is_empty(),
        "blocked-range fixture must include at least one blocked range",
    )?;
    let degraded = value
        .pointer("/degraded")
        .and_then(Value::as_array)
        .ok_or_else(|| "degraded missing".to_string())?;
    ensure(
        degraded
            .iter()
            .any(|value| value.as_str() == Some("mesh_anti_entropy_round_blocked")),
        "blocked-range fixture must emit mesh_anti_entropy_round_blocked",
    )?;
    let next_retry = value
        .pointer("/backoffPosture/nextRetryAfter")
        .and_then(Value::as_str)
        .ok_or_else(|| "backoffPosture.nextRetryAfter missing".to_string())?;
    let blocked_retry = blocked[0]
        .pointer("/retryAfter")
        .and_then(Value::as_str)
        .ok_or_else(|| "blockedRanges[0].retryAfter missing".to_string())?;
    ensure_equal(
        &next_retry,
        &blocked_retry,
        "backoffPosture.nextRetryAfter must match the blocked range retryAfter",
    )?;
    Ok(())
}

#[test]
fn anti_entropy_adr_and_protocol_doc_exist_and_reference_the_schema() -> TestResult {
    let adr = read_text(ADR_PATH)?;
    ensure(
        adr.contains("ADR 0041"),
        format!("{ADR_PATH} must self-identify as ADR 0041"),
    )?;
    ensure(
        adr.contains("bd-3lx0p"),
        format!("{ADR_PATH} must reference bd-3lx0p (SRR6.7)"),
    )?;
    ensure(
        adr.contains(ANTI_ENTROPY_SCHEMA_ID),
        format!("{ADR_PATH} must reference the {ANTI_ENTROPY_SCHEMA_ID} sync summary schema"),
    )?;

    let protocol = read_text(PROTOCOL_DOC_PATH)?;
    for kind in [
        "TipAdvertise",
        "RangeRequest",
        "EventBatch",
        "RevisionNotice",
    ] {
        ensure(
            protocol.contains(kind),
            format!("{PROTOCOL_DOC_PATH} must document message kind {kind}"),
        )?;
    }
    ensure(
        protocol.contains("bounded retry") || protocol.contains("Bounded retry"),
        format!("{PROTOCOL_DOC_PATH} must document bounded retry/backoff invariant"),
    )?;
    Ok(())
}
