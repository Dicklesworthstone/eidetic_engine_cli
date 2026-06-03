//! bd-36bbk.1.9 — structural contract for the SRR6.46.9 rollback
//! surfaces: `ee.mesh.disable_result.v1` (full rollback) and
//! `ee.mesh.revoke_result.v1` (per-peer revocation).
//!
//! Implements-surface bead first slice: the bead promises an
//! `ee mesh disable` reversal path the SRR6.46.5 safety snapshot's
//! `reversalCommand` field already names, plus an `ee mesh revoke`
//! command that drops one untrusted peer without nuking the group.
//! Neither command, schema, nor pure-decision module exists on
//! origin/main yet; this contract lands the wire shape so the CLI
//! wiring, DB-transaction logic, audit-row plumbing, and dry-run
//! sequencer in follow-up child slices all compose against a stable
//! envelope.
//!
//! Asserts (both schemas):
//!
//! 1. Schema file exists at the canonical path and parses.
//! 2. `$id`, `title`, and `properties.schema.const` agree on the
//!    canonical schema name.
//! 3. Required top-level fields match what the bead's step contracts
//!    promise the operator (`outcome`, `auditRowId`, `dryRun`, +
//!    surface-specific fields).
//! 4. `additionalProperties: false` at top level — no drift fields.
//! 5. `outcome` is a closed-set enum covering the success / dry-run /
//!    noop or unknown-peer cases the bead names.
//! 6. `caution.kind` is a closed-set enum covering the per-surface
//!    UX warnings the bead documents.
//! 7. `caution.severity` is restricted to `info | warning` only —
//!    rollback surfaces never emit `error` via cautions (errors
//!    propagate through the top-level response envelope error block).
//!
//! Surface-specific assertions:
//!
//! - `disable_result` pins the `bindingsRetainedForOtherWorkspaces`
//!   field the bead's step 5 contract names (peer-group row only
//!   deletes when 0 other workspace bindings remain).
//! - `revoke_result` pins the `newPeerSetHash` BLAKE3 pattern the
//!   bead names so support bundles can compare peer-set identity
//!   without leaking node-keys.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;

use serde_json::Value;

type TestResult = Result<(), String>;

const DISABLE_PATH: &str = "docs/schemas/ee.mesh.disable_result.v1.json";
const DISABLE_ID: &str = "https://eidetic-engine/schemas/ee.mesh.disable_result.v1.json";
const DISABLE_NAME: &str = "ee.mesh.disable_result.v1";

const REVOKE_PATH: &str = "docs/schemas/ee.mesh.revoke_result.v1.json";
const REVOKE_ID: &str = "https://eidetic-engine/schemas/ee.mesh.revoke_result.v1.json";
const REVOKE_NAME: &str = "ee.mesh.revoke_result.v1";

const DISABLE_REQUIRED_TOP_LEVEL: &[&str] = &[
    "schema",
    "outcome",
    "workspaceId",
    "auditRowId",
    "dryRun",
    "removedPeerCount",
    "removedBindings",
    "bindingsRetainedForOtherWorkspaces",
];

const DISABLE_OUTCOMES: &[&str] = &["disabled", "dry_run", "noop_no_peer_group"];

const DISABLE_CAUTION_KINDS: &[&str] = &[
    "mesh_disable_noop",
    "mesh_disable_bindings_retained",
    "mesh_disable_dry_run",
];

const REVOKE_REQUIRED_TOP_LEVEL: &[&str] = &[
    "schema",
    "outcome",
    "workspaceId",
    "revokedNodeKey",
    "auditRowId",
    "dryRun",
    "remainingPeerCount",
    "denylistUpdated",
];

const REVOKE_OUTCOMES: &[&str] = &["revoked", "dry_run", "unknown_peer"];

const REVOKE_CAUTION_KINDS: &[&str] = &[
    "mesh_revoke_unknown_peer",
    "mesh_revoke_last_peer_in_group",
    "mesh_revoke_dry_run",
];

const CAUTION_SEVERITIES: &[&str] = &["info", "warning"];

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

fn load_schema(rel: &str) -> Result<Value, String> {
    let path = repo_root().join(rel);
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

fn require_envelope_identity(schema: &Value, id: &str, name: &str) -> TestResult {
    ensure(
        schema["$id"] == id,
        format!("expected $id={id}; got: {}", schema["$id"]),
    )?;
    ensure(
        schema["title"] == name,
        format!("expected title={name}; got: {}", schema["title"]),
    )?;
    ensure(
        schema["properties"]["schema"]["const"] == name,
        format!("properties.schema.const must equal {name}"),
    )
}

fn require_top_level_required(schema: &Value, expected: &[&str], label: &str) -> TestResult {
    let required = collect_strings(&schema["required"], label)?;
    let want: BTreeSet<String> = expected.iter().map(|field| (*field).to_owned()).collect();
    ensure(
        required == want,
        format!("{label} drifted from exact required set; expected {want:?}, got {required:?}"),
    )
}

fn require_closed_top_level(schema: &Value, label: &str) -> TestResult {
    ensure(
        schema["additionalProperties"] == Value::Bool(false),
        format!("{label} top-level additionalProperties must be false (closed schema)"),
    )
}

#[test]
fn disable_result_schema_parses_and_envelope_is_consistent() -> TestResult {
    let schema = load_schema(DISABLE_PATH)?;
    require_envelope_identity(&schema, DISABLE_ID, DISABLE_NAME)?;
    require_closed_top_level(&schema, "disable_result")?;
    require_top_level_required(
        &schema,
        DISABLE_REQUIRED_TOP_LEVEL,
        "disable_result.required",
    )
}

#[test]
fn disable_result_outcome_enum_covers_disabled_dry_run_and_noop() -> TestResult {
    let schema = load_schema(DISABLE_PATH)?;
    require_closed_set(
        &schema,
        "/properties/outcome/enum",
        DISABLE_OUTCOMES,
        "disable_result.outcome enum",
    )
}

#[test]
fn disable_result_caution_kind_enum_matches_bead_acceptance() -> TestResult {
    let schema = load_schema(DISABLE_PATH)?;
    require_closed_set(
        &schema,
        "/$defs/caution/properties/kind/enum",
        DISABLE_CAUTION_KINDS,
        "disable_result.caution.kind enum",
    )
}

#[test]
fn disable_result_caution_severity_excludes_error() -> TestResult {
    let schema = load_schema(DISABLE_PATH)?;
    require_closed_set(
        &schema,
        "/$defs/caution/properties/severity/enum",
        CAUTION_SEVERITIES,
        "disable_result.caution.severity enum",
    )
}

#[test]
fn disable_result_pins_bindings_retained_field() -> TestResult {
    // Step 5 of the bead's contract: peer-group row only deletes when
    // 0 other workspace bindings remain. The field is named here so a
    // future refactor can't quietly drop the cross-workspace check.
    let schema = load_schema(DISABLE_PATH)?;
    let kind = schema
        .pointer("/properties/bindingsRetainedForOtherWorkspaces/type")
        .and_then(Value::as_str);
    ensure(
        kind == Some("integer"),
        "bindingsRetainedForOtherWorkspaces must be an integer field; the cross-workspace retention check is the bead's load-bearing safety invariant",
    )
}

#[test]
fn revoke_result_schema_parses_and_envelope_is_consistent() -> TestResult {
    let schema = load_schema(REVOKE_PATH)?;
    require_envelope_identity(&schema, REVOKE_ID, REVOKE_NAME)?;
    require_closed_top_level(&schema, "revoke_result")?;
    require_top_level_required(&schema, REVOKE_REQUIRED_TOP_LEVEL, "revoke_result.required")
}

#[test]
fn revoke_result_outcome_enum_covers_revoked_dry_run_and_unknown_peer() -> TestResult {
    let schema = load_schema(REVOKE_PATH)?;
    require_closed_set(
        &schema,
        "/properties/outcome/enum",
        REVOKE_OUTCOMES,
        "revoke_result.outcome enum",
    )
}

#[test]
fn revoke_result_caution_kind_enum_matches_bead_acceptance() -> TestResult {
    let schema = load_schema(REVOKE_PATH)?;
    require_closed_set(
        &schema,
        "/$defs/caution/properties/kind/enum",
        REVOKE_CAUTION_KINDS,
        "revoke_result.caution.kind enum",
    )
}

#[test]
fn revoke_result_caution_severity_excludes_error() -> TestResult {
    let schema = load_schema(REVOKE_PATH)?;
    require_closed_set(
        &schema,
        "/$defs/caution/properties/severity/enum",
        CAUTION_SEVERITIES,
        "revoke_result.caution.severity enum",
    )
}

#[test]
fn revoke_result_new_peer_set_hash_requires_blake3_prefix() -> TestResult {
    // The bead names BLAKE3 hashing so support bundles can compare
    // peer-set identity without leaking node-keys. Pin the pattern so
    // a future refactor can't quietly drop the prefix or widen the hex
    // length envelope.
    let schema = load_schema(REVOKE_PATH)?;
    let pattern = schema
        .pointer("/properties/newPeerSetHash/pattern")
        .and_then(Value::as_str)
        .ok_or_else(|| "newPeerSetHash.pattern missing".to_string())?;
    ensure(
        pattern.contains("blake3:"),
        format!("newPeerSetHash pattern must require blake3 prefix; got: {pattern}"),
    )?;
    ensure(
        pattern.contains("32,128"),
        format!("newPeerSetHash pattern must constrain hex length to [32, 128]; got: {pattern}"),
    )
}

#[test]
fn revoke_result_node_key_pattern_is_redactable() -> TestResult {
    // nodeKey must be defined as a $ref under $defs so the renderer
    // can apply the tailscale_metadata redaction class uniformly with
    // other mesh schemas.
    let schema = load_schema(REVOKE_PATH)?;
    let revoked_ref = schema
        .pointer("/properties/revokedNodeKey/$ref")
        .and_then(Value::as_str);
    ensure(
        revoked_ref == Some("#/$defs/nodeKey"),
        "revokedNodeKey must $ref the shared $defs/nodeKey so the tailscale_metadata redaction class applies uniformly",
    )
}
