//! bd-1fjhu: structural contract for `docs/schemas/ee.curate.peer_evidence.v1.json`.
//!
//! Peer-aware learn and curate (SRR6.28) folds cached peer-origin
//! evidence into curation candidates without auto-promoting remote
//! claims into local procedural truth. The actual sync pipeline is
//! sequenced behind `bd-1k0ql` / `bd-1lgq6` / `bd-wl4ja` / `bd-273tl`;
//! this contract locks the wire shape now so the emission code that
//! lands later can compile against a stable schema instead of
//! redesigning mid-stream.
//!
//! What this contract asserts:
//!
//! 1. Envelope shape: `$id`, `title`, `properties.schema.const`, and
//!    the top-level `required` set match the SRR6.28 contract.
//!
//! 2. The trust-cap invariant is encoded structurally: `trustCap` only
//!    accepts `agent_assertion` or `agent_validated`. Peer-only
//!    evidence cannot escalate a candidate to `human_explicit` or
//!    `cass_evidence` because those values are reserved for local
//!    human or local-replay evidence.
//!
//! 3. The four documented `promotionBlockReason` values are present:
//!    `peer_evidence_only_below_trust_cap`,
//!    `contradicting_peer_evidence`,
//!    `peer_outcome_feedback_pending`,
//!    `human_review_required_for_rule_kind`. Additions are allowed,
//!    deletions break the bd-1fjhu acceptance contract because
//!    `ee learn` and `ee curate apply` block on these literals.
//!
//! 4. `peerEvidenceEntry` requires `peerId`, `memoryRef`, `scoreDelta`,
//!    and `recordedAt` so consumers can audit and replay the candidate's
//!    score deterministically. Peer memory bodies are NOT included
//!    in the entry — only redaction-safe identifiers.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;

use serde_json::Value;

type TestResult = Result<(), String>;

const SCHEMA_PATH: &str = "docs/schemas/ee.curate.peer_evidence.v1.json";
const SCHEMA_ID: &str = "https://eidetic-engine/schemas/ee.curate.peer_evidence.v1.json";
const SCHEMA_NAME: &str = "ee.curate.peer_evidence.v1";
const REQUIRED_TOP_LEVEL: &[&str] = &[
    "schema",
    "candidateId",
    "candidateKind",
    "score",
    "trustClass",
    "peerEvidence",
    "contributingPeerCount",
    "trustCap",
    "promotable",
];
const TRUST_CAP_ALLOWED: &[&str] = &["agent_assertion", "agent_validated"];
const REQUIRED_PROMOTION_BLOCK_REASONS: &[&str] = &[
    "peer_evidence_only_below_trust_cap",
    "contradicting_peer_evidence",
    "peer_outcome_feedback_pending",
    "human_review_required_for_rule_kind",
];
const PEER_ENTRY_REQUIRED: &[&str] = &["peerId", "memoryRef", "scoreDelta", "recordedAt"];

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

fn collect_strings(node: &Value, ctx: &str) -> Result<Vec<String>, String> {
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

fn collect_string_set(node: &Value, ctx: &str) -> Result<BTreeSet<String>, String> {
    Ok(collect_strings(node, ctx)?.into_iter().collect())
}

#[test]
fn ee_curate_peer_evidence_v1_schema_has_expected_envelope() -> TestResult {
    let schema = load_schema()?;
    ensure(
        schema["$id"] == SCHEMA_ID,
        format!("expected `$id` = {SCHEMA_ID}; got: {}", schema["$id"]),
    )?;
    ensure(
        schema["title"] == SCHEMA_NAME,
        format!("expected `title` = {SCHEMA_NAME}; got: {}", schema["title"]),
    )?;
    let schema_const = &schema["properties"]["schema"]["const"];
    ensure(
        schema_const == SCHEMA_NAME,
        format!("expected properties.schema.const = {SCHEMA_NAME}; got: {schema_const}"),
    )?;
    let actual = collect_string_set(&schema["required"], "top-level required")?;
    let expected = REQUIRED_TOP_LEVEL
        .iter()
        .map(|field| (*field).to_owned())
        .collect::<BTreeSet<_>>();
    ensure(
        actual == expected,
        format!(
            "REQUIRED_TOP_LEVEL drifted from schema required array\nexpected={expected:?}\nactual={actual:?}"
        ),
    )?;
    Ok(())
}

#[test]
fn ee_curate_peer_evidence_v1_trust_cap_excludes_local_human_lanes() -> TestResult {
    let schema = load_schema()?;
    let cap_enum = &schema["$defs"]["trustCap"];
    // trustCap is defined inline on the property; check both shapes.
    let inline = &schema["properties"]["trustCap"]["enum"];
    let values = if !inline.is_null() {
        collect_strings(inline, "properties.trustCap.enum")?
    } else {
        collect_strings(&cap_enum["enum"], "$defs.trustCap.enum")?
    };
    ensure(
        values.len() == TRUST_CAP_ALLOWED.len()
            && TRUST_CAP_ALLOWED
                .iter()
                .all(|a| values.iter().any(|v| v == a)),
        format!(
            "trustCap enum must be exactly {TRUST_CAP_ALLOWED:?}; got: {values:?}. \
             bd-1fjhu peer-only evidence must not escalate candidates to \
             human_explicit or cass_evidence; those values are reserved for \
             local human or local replay evidence respectively."
        ),
    )?;
    Ok(())
}

#[test]
fn ee_curate_peer_evidence_v1_promotion_block_reason_taxonomy_is_present() -> TestResult {
    let schema = load_schema()?;
    let enum_node = &schema["$defs"]["promotionBlockReason"]["enum"];
    let values = collect_strings(enum_node, "$defs.promotionBlockReason.enum")?;
    for required in REQUIRED_PROMOTION_BLOCK_REASONS {
        ensure(
            values.iter().any(|v| v == required),
            format!(
                "promotionBlockReason enum is missing `{required}`; got: {values:?}. \
                 The four base reasons pinned here are the bd-1fjhu acceptance \
                 contract for `ee curate apply` promotion gating."
            ),
        )?;
    }
    Ok(())
}

#[test]
fn ee_curate_peer_evidence_v1_peer_entry_audits_provenance_without_body() -> TestResult {
    let schema = load_schema()?;
    let entry = &schema["$defs"]["peerEvidenceEntry"];
    let required = collect_strings(&entry["required"], "peerEvidenceEntry.required")?;
    for field in PEER_ENTRY_REQUIRED {
        ensure(
            required.iter().any(|r| r == field),
            format!(
                "peerEvidenceEntry.required is missing `{field}`; got: {required:?}. \
                 bd-1fjhu requires deterministic score replay from peerId + \
                 memoryRef + scoreDelta + recordedAt."
            ),
        )?;
    }
    let properties = entry["properties"]
        .as_object()
        .ok_or_else(|| format!("peerEvidenceEntry.properties is not an object: {entry}"))?;
    ensure(
        !properties.contains_key("body") && !properties.contains_key("memoryBody"),
        format!(
            "peerEvidenceEntry must not carry peer memory bodies inline; got \
             properties: {:?}. Body fetch is gated by mesh peer policy, not \
             by curation candidates.",
            properties.keys().collect::<Vec<_>>()
        ),
    )?;
    Ok(())
}
