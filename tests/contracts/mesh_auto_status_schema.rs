//! bd-36bbk.1.4 — structural contract for the SRR6.46.4
//! auto-enrollment status block (`ee.mesh.auto_status.v1`).
//!
//! `ee mesh status --json` exposes the zero-touch posture under
//! `data.autoEnrollment`. The implements-surface bead names the v4
//! enriched view: tailscale + hello responder + discovery cache +
//! materialized peer-set + drift + steward posture + degraded array,
//! all composed under a single read-only block.
//!
//! This contract pins the wire shape so a future renderer or
//! trust-policy refactor can't drift fields the v4 enrichment
//! introduces (nodeKeyChanged, materializedOnNodeKey,
//! enrollmentSource=auto_replaced_manual, manualConflictPresent).
//!
//! Asserts:
//!
//! 1. Schema file exists at canonical path and parses.
//! 2. `$id`, `title`, and `properties.schema.const` agree on
//!    `ee.mesh.auto_status.v1`.
//! 3. Required top-level fields cover the 12-block v4 envelope.
//! 4. `additionalProperties: false` at top level — closed envelope.
//! 5. `readOnly` is `const true` — pins the bead's load-bearing
//!    read-only invariant ("never materializes config").
//! 6. `tailscale.status` enum is `{not_probed, authenticated,
//!    not_authenticated}` matching SRR6.46.1's local probe.
//! 7. `helloResponder.status` enum is `{not_probed, running,
//!    not_running}` matching SRR6.46.12.
//! 8. `materialized.enrollmentSource` enum covers the
//!    `auto_replaced_manual` case the v4 enrichment introduced (so a
//!    future refactor cannot drop the manual-replacement audit
//!    trail).
//! 9. `materialized.peerSetHash` pattern requires the
//!    `blake3:[0-9a-f]{64}` shape so support bundles can compare
//!    peer-set identity without leaking node-keys.
//! 10. `lanePolicy.laneDecision` enum is `{allow, quarantine,
//!     deny}` matching the canonical SRR6.5 lane vocabulary.
//! 11. `peerStateBreakdown` requires the four-field breakdown
//!     (`active`, `softStale`, `hardStale`, `denylisted`) the bead
//!     spec names.
//! 12. `drift.driftSeverity` enum is `{none, info, warning,
//!     medium}` — no `error` path; the read-only computation never
//!     escalates to error severity.
//! 13. `drift` required fields cover the v4 enrichment
//!     (`nodeKeyChanged`, `manualConflictPresent`,
//!     `transientUnreachable`).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;

use serde_json::Value;

type TestResult = Result<(), String>;

const SCHEMA_PATH: &str = "docs/schemas/ee.mesh.auto_status.v1.json";
const SCHEMA_ID: &str = "https://eidetic-engine/schemas/ee.mesh.auto_status.v1.json";
const SCHEMA_NAME: &str = "ee.mesh.auto_status.v1";

const REQUIRED_TOP_LEVEL: &[&str] = &[
    "schema",
    "enabled",
    "readOnly",
    "tailscale",
    "helloResponder",
    "discovery",
    "discoveryCache",
    "materialized",
    "peerStateBreakdown",
    "drift",
    "stewardPosture",
    "degraded",
];

const TAILSCALE_STATUSES: &[&str] = &["not_probed", "authenticated", "not_authenticated"];

const HELLO_RESPONDER_STATUSES: &[&str] = &["not_probed", "running", "not_running"];

const ENROLLMENT_SOURCES: &[&str] = &["auto", "manual", "auto_replaced_manual"];

const LANE_DECISIONS: &[&str] = &["allow", "quarantine", "deny"];

const DRIFT_SEVERITIES: &[&str] = &["none", "info", "warning", "medium"];

const PEER_STATE_BREAKDOWN_FIELDS: &[&str] = &["active", "softStale", "hardStale", "denylisted"];

const TAILSCALE_AUTODISCOVERY_FIELDS: &[&str] = &[
    "schema",
    "tailnetId",
    "tailnetDisplayName",
    "selfNodeKey",
    "probedPeerCount",
    "eligiblePeerCount",
    "eeCapablePeers",
    "skippedPeers",
    "degraded",
];

const DRIFT_REQUIRED_FIELDS: &[&str] = &[
    "newPeersAvailable",
    "newPeerCount",
    "stalePeersInConfig",
    "transientUnreachable",
    "tailnetChanged",
    "nodeKeyChanged",
    "manualConflictPresent",
    "driftSeverity",
    "actionGraph",
    "nextActionHint",
];

const MATERIALIZED_REQUIRED_FIELDS: &[&str] = &[
    "peerGroupId",
    "peerSetHash",
    "peerCount",
    "lanePolicy",
    "boundTailnetId",
    "materializedOnNodeKey",
    "lastMaterializedAt",
    "enrollmentSource",
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

fn load_json(path: &str) -> Result<Value, String> {
    let path = repo_root().join(path);
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
fn auto_status_schema_file_exists_and_parses() -> TestResult {
    let _ = load_schema()?;
    Ok(())
}

#[test]
fn auto_status_schema_identity_is_consistent() -> TestResult {
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
        "properties.schema.const must equal ee.mesh.auto_status.v1",
    )
}

#[test]
fn auto_status_required_top_level_fields_match_spec() -> TestResult {
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
fn auto_status_top_level_is_closed() -> TestResult {
    let schema = load_schema()?;
    ensure(
        schema["additionalProperties"] == Value::Bool(false),
        "top-level additionalProperties must be false (closed envelope)",
    )
}

#[test]
fn auto_status_read_only_is_const_true() -> TestResult {
    // Load-bearing invariant: the bead acceptance says "Read-only:
    // never materializes config." Pin readOnly:const true so a future
    // refactor can't widen the field without an explicit schema update.
    let schema = load_schema()?;
    ensure(
        schema["properties"]["readOnly"]["const"] == Value::Bool(true),
        "properties.readOnly.const must equal true (read-only invariant)",
    )
}

#[test]
fn auto_status_tailscale_status_enum_matches_local_probe() -> TestResult {
    let schema = load_schema()?;
    require_closed_set(
        &schema,
        "/properties/tailscale/properties/status/enum",
        TAILSCALE_STATUSES,
        "tailscale.status enum",
    )
}

#[test]
fn auto_status_hello_responder_status_enum_matches_srr6_46_12() -> TestResult {
    let schema = load_schema()?;
    require_closed_set(
        &schema,
        "/properties/helloResponder/properties/status/enum",
        HELLO_RESPONDER_STATUSES,
        "helloResponder.status enum",
    )
}

#[test]
fn auto_status_discovery_ref_targets_tailscale_autodiscovery_def() -> TestResult {
    let schema = load_schema()?;
    ensure(
        schema["properties"]["discovery"]["$ref"] == "#/$defs/tailscaleAutodiscovery",
        "properties.discovery must point at the tailscaleAutodiscovery definition",
    )?;
    ensure(
        schema
            .pointer("/$defs/tailscaleAutodiscovery")
            .and_then(Value::as_object)
            .is_some(),
        "$defs.tailscaleAutodiscovery must exist",
    )?;
    ensure(
        schema["properties"].get("tailscaleAutodiscovery").is_none(),
        "tailscaleAutodiscovery is a reusable definition, not a top-level property",
    )
}

#[test]
fn auto_status_discovery_required_fields_match_tailscale_report() -> TestResult {
    let schema = load_schema()?;
    let required = collect_strings(
        schema
            .pointer("/$defs/tailscaleAutodiscovery/required")
            .unwrap_or(&Value::Null),
        "tailscaleAutodiscovery.required",
    )?;
    for field in TAILSCALE_AUTODISCOVERY_FIELDS {
        ensure(
            required.contains(*field),
            format!("tailscaleAutodiscovery.required missing {field}; got {required:?}"),
        )?;
    }
    Ok(())
}

#[test]
fn auto_status_constructed_golden_discovery_matches_schema_contract() -> TestResult {
    let golden = load_json("tests/fixtures/golden/mesh/foreground_status_disabled.json")?;
    let discovery = golden
        .pointer("/autoEnrollment/discovery")
        .and_then(Value::as_object)
        .ok_or_else(|| "foreground_status_disabled golden missing discovery object".to_owned())?;
    let actual: BTreeSet<String> = discovery.keys().cloned().collect();
    let expected: BTreeSet<String> = TAILSCALE_AUTODISCOVERY_FIELDS
        .iter()
        .map(|field| (*field).to_owned())
        .collect();
    ensure(
        actual == expected,
        format!(
            "foreground_status_disabled discovery shape drifted; expected {expected:?}, got {actual:?}"
        ),
    )?;
    ensure(
        discovery.get("schema").and_then(Value::as_str) == Some("ee.tailscale.autodiscovery.v1"),
        "foreground_status_disabled discovery must use ee.tailscale.autodiscovery.v1",
    )
}

#[test]
fn auto_status_enrollment_source_covers_auto_replaced_manual() -> TestResult {
    // The v4 enrichment names auto_replaced_manual as the audit-trail
    // case where auto-enrollment overrode a manual binding. Pin so a
    // future refactor can't drop the case and silently lose the audit
    // trail.
    let schema = load_schema()?;
    require_closed_set(
        &schema,
        "/$defs/materialized/properties/enrollmentSource/enum",
        ENROLLMENT_SOURCES,
        "materialized.enrollmentSource enum",
    )
}

#[test]
fn auto_status_peer_set_hash_requires_blake3_pattern() -> TestResult {
    let schema = load_schema()?;
    let pattern = schema
        .pointer("/$defs/materialized/properties/peerSetHash/pattern")
        .and_then(Value::as_str)
        .ok_or_else(|| "peerSetHash.pattern missing".to_string())?;
    ensure(
        pattern.contains("blake3:"),
        format!("peerSetHash pattern must require blake3 prefix; got: {pattern}"),
    )?;
    ensure(
        pattern.contains("[0-9a-f]{64}"),
        format!("peerSetHash pattern must constrain to 64 hex chars; got: {pattern}"),
    )
}

#[test]
fn auto_status_lane_decision_enum_matches_srr6_5_vocabulary() -> TestResult {
    let schema = load_schema()?;
    require_closed_set(
        &schema,
        "/$defs/laneDecision/enum",
        LANE_DECISIONS,
        "laneDecision enum",
    )
}

#[test]
fn auto_status_peer_state_breakdown_has_four_buckets() -> TestResult {
    let schema = load_schema()?;
    let required = collect_strings(
        schema
            .pointer("/$defs/peerStateBreakdown/required")
            .unwrap_or(&Value::Null),
        "peerStateBreakdown.required",
    )?;
    for field in PEER_STATE_BREAKDOWN_FIELDS {
        ensure(
            required.contains(*field),
            format!("peerStateBreakdown.required missing {field}; got {required:?}"),
        )?;
    }
    Ok(())
}

#[test]
fn auto_status_drift_severity_excludes_error() -> TestResult {
    let schema = load_schema()?;
    require_closed_set(
        &schema,
        "/$defs/drift/properties/driftSeverity/enum",
        DRIFT_SEVERITIES,
        "drift.driftSeverity enum",
    )?;
    let severities = collect_strings(
        schema
            .pointer("/$defs/drift/properties/driftSeverity/enum")
            .unwrap_or(&Value::Null),
        "drift.driftSeverity enum",
    )?;
    ensure(
        !severities.contains("error"),
        "drift.driftSeverity must not allow `error`; the read-only computation cannot escalate to error severity",
    )
}

#[test]
fn auto_status_drift_required_fields_cover_v4_enrichment() -> TestResult {
    // v4 enrichment: nodeKeyChanged (SRR6.46.8 backup-restored class),
    // manualConflictPresent (auto_replaced_manual signal),
    // transientUnreachable (drift partition). Pin all three so the v4
    // additions can't quietly disappear.
    let schema = load_schema()?;
    let required = collect_strings(
        schema
            .pointer("/$defs/drift/required")
            .unwrap_or(&Value::Null),
        "drift.required",
    )?;
    for field in DRIFT_REQUIRED_FIELDS {
        ensure(
            required.contains(*field),
            format!("drift.required missing {field}; got {required:?}"),
        )?;
    }
    Ok(())
}

#[test]
fn auto_status_materialized_required_fields_cover_v4_enrichment() -> TestResult {
    // materializedOnNodeKey (v4, SRR6.46.8 node-key change detector)
    // and enrollmentSource (v4, audit trail for auto_replaced_manual)
    // are both v4 additions; pin them so neither can be dropped.
    let schema = load_schema()?;
    let required = collect_strings(
        schema
            .pointer("/$defs/materialized/required")
            .unwrap_or(&Value::Null),
        "materialized.required",
    )?;
    for field in MATERIALIZED_REQUIRED_FIELDS {
        ensure(
            required.contains(*field),
            format!("materialized.required missing {field}; got {required:?}"),
        )?;
    }
    Ok(())
}
