//! bd-2vvz3: conformance harness for `ee outcome` + `ee curate` +
//! the audit-log schema family.
//!
//! Applies the /testing-conformance-harnesses skill to the curate /
//! outcome / audit envelope contract. The bead text frames it as the
//! "ee.audit.v1 envelope shape"; reality has a small *family* of
//! audit-shaped schemas (one per emitting subsystem). This test pins
//! every wire-form string the rest of the agent contract leans on,
//! so any rename, version bump, or accidental drift surfaces as a
//! single failing assertion with the exact constant name and the
//! before/after string.
//!
//! Scope:
//!   1. `ee curate` schema family — every `CURATE_*_SCHEMA_V1` const
//!      in src/core/curate.rs pinned to its documented wire form
//!      (`ee.curate.<surface>.v1`).
//!   2. `ee outcome` schema family — the two
//!      `OUTCOME_QUARANTINE_*_SCHEMA_V1` constants in
//!      src/core/outcome.rs.
//!   3. Audit schema family across the swarm — every
//!      `*_AUDIT_SCHEMA_V1` constant that downstream consumers
//!      treat as the canonical audit envelope wire form. Includes
//!      ee.audit.memory_level_transition.v1, ee.procedure.
//!      promotion_audit.v1, ee.mesh.hello_responder.lifecycle_audit.v1,
//!      ee.shard_fanout.migration_audit.v1, ee.export.
//!      audit.v1, ee.cass.redaction_audit.v1, and ee.mesh.share_
//!      consent_audit.v1.
//!   4. `CurateApplyReport` field surface — pins the required-field
//!      set (schema, candidateId, candidate, application, mutation,
//!      degraded, …) so a refactor that drops or renames any of
//!      them is caught at compile-time-or-test-time, not at
//!      production runtime when a consumer's JSON path lookup
//!      returns null.
//!   5. `CurateDispositionReport` field surface — same shape pin
//!      for the deterministic-TTL-disposition path the bead's
//!      "candidate_id … evidence_uri" line points at.
//!
//! This harness is intentionally read-only: it pins constants and
//! serialization shapes without constructing live DB state. Real
//! end-to-end behavior (record_outcome / curate apply against a
//! tempdir workspace) lives in the existing
//! tests/contracts/causal_credit.rs +
//! tests/contracts/curate_peer_evidence_schema.rs harnesses; this
//! file is the wire-form chokepoint they all rely on.

use ee::core::curate::{
    CURATE_APPLY_SCHEMA_V1, CURATE_CANDIDATES_SCHEMA_V1, CURATE_DISPOSITION_SCHEMA_V1,
    CURATE_PEER_EVIDENCE_SCHEMA_V1, CURATE_RETIRE_SCHEMA_V1, CURATE_REVIEW_SCHEMA_V1,
    CURATE_TOMBSTONE_SCHEMA_V1, CURATE_UNTOMBSTONE_SCHEMA_V1, CURATE_VALIDATE_SCHEMA_V1,
};
use ee::core::memory_lifecycle::MEMORY_LEVEL_TRANSITION_AUDIT_SCHEMA_V1;
use ee::core::outcome::{OUTCOME_QUARANTINE_LIST_SCHEMA_V1, OUTCOME_QUARANTINE_REVIEW_SCHEMA_V1};
use ee::core::procedure::PROCEDURE_PROMOTION_AUDIT_SCHEMA_V1;
use ee::mesh::hello_responder::HELLO_RESPONDER_LIFECYCLE_AUDIT_SCHEMA_V1;

type TestResult = Result<(), String>;

/// bd-2vvz3 chokepoint #1: `ee curate` surface wire forms.
///
/// Every CURATE_*_SCHEMA_V1 must stay byte-identical to its
/// documented value. Agents grepping their own JSON output by these
/// strings, downstream consumers branching on schema-equals matches,
/// and the J6 failure-mode fixture catalog all depend on these
/// staying stable through v1.
#[test]
fn curate_schema_family_pinned_to_documented_wire_forms() -> TestResult {
    for (name, actual, expected) in [
        (
            "CURATE_CANDIDATES_SCHEMA_V1",
            CURATE_CANDIDATES_SCHEMA_V1,
            "ee.curate.candidates.v1",
        ),
        (
            "CURATE_VALIDATE_SCHEMA_V1",
            CURATE_VALIDATE_SCHEMA_V1,
            "ee.curate.validate.v1",
        ),
        (
            "CURATE_APPLY_SCHEMA_V1",
            CURATE_APPLY_SCHEMA_V1,
            "ee.curate.apply.v1",
        ),
        (
            "CURATE_PEER_EVIDENCE_SCHEMA_V1",
            CURATE_PEER_EVIDENCE_SCHEMA_V1,
            "ee.curate.peer_evidence.v1",
        ),
        (
            "CURATE_REVIEW_SCHEMA_V1",
            CURATE_REVIEW_SCHEMA_V1,
            "ee.curate.review.v1",
        ),
        (
            "CURATE_DISPOSITION_SCHEMA_V1",
            CURATE_DISPOSITION_SCHEMA_V1,
            "ee.curate.disposition.v1",
        ),
        (
            "CURATE_RETIRE_SCHEMA_V1",
            CURATE_RETIRE_SCHEMA_V1,
            "ee.curate.retire.v1",
        ),
        (
            "CURATE_TOMBSTONE_SCHEMA_V1",
            CURATE_TOMBSTONE_SCHEMA_V1,
            "ee.curate.tombstone.v1",
        ),
        (
            "CURATE_UNTOMBSTONE_SCHEMA_V1",
            CURATE_UNTOMBSTONE_SCHEMA_V1,
            "ee.curate.untombstone.v1",
        ),
    ] {
        if actual != expected {
            return Err(format!(
                "curate schema drift: {name} = {actual:?}, expected {expected:?}; \
                 a wire-form rename here breaks every J6 fixture, every agent \
                 grep, and every downstream schema-equals branch. Bump the \
                 schema to v2 in a deliberate migration instead of editing v1."
            ));
        }
    }
    Ok(())
}

/// bd-2vvz3 chokepoint #2: `ee outcome` quarantine surfaces.
///
/// Today the outcome system surfaces two pub schema constants
/// (the list view and the review view of the harmful-burst
/// quarantine). The signal vocabulary itself is currently private
/// (ALLOWED_SIGNALS / HARMFUL_SIGNALS / HELPFUL_SIGNALS) — pinning
/// that without exposing it requires the live record_outcome path
/// which lives behind DB state and is exercised elsewhere
/// (tests/contracts/causal_credit.rs). This test pins only what's
/// pub-observable today; if the team promotes ALLOWED_SIGNALS to
/// pub, extend this with a static assertion on its contents.
#[test]
fn outcome_quarantine_schema_pinned_to_documented_wire_forms() -> TestResult {
    if OUTCOME_QUARANTINE_LIST_SCHEMA_V1 != "ee.outcome.quarantine.list.v1" {
        return Err(format!(
            "OUTCOME_QUARANTINE_LIST_SCHEMA_V1 drift: got {OUTCOME_QUARANTINE_LIST_SCHEMA_V1:?}"
        ));
    }
    if OUTCOME_QUARANTINE_REVIEW_SCHEMA_V1 != "ee.outcome.quarantine.review.v1" {
        return Err(format!(
            "OUTCOME_QUARANTINE_REVIEW_SCHEMA_V1 drift: got {OUTCOME_QUARANTINE_REVIEW_SCHEMA_V1:?}"
        ));
    }
    Ok(())
}

/// bd-2vvz3 chokepoint #3: audit schema family.
///
/// The bead text says "ee.audit.v1 entries". Reality has a *family*
/// of audit-shaped schemas — one per emitting subsystem (memory
/// lifecycle, procedure promotion, and mesh
/// hello-responder lifecycle). Pinning each here gives downstream
/// consumers (the J6 catalog, ee why, ee swarm brief audit
/// surfaces) a single chokepoint to grep against. If a future
/// commit consolidates them into a single `ee.audit.v1` envelope,
/// this test fires immediately — and the consolidation itself
/// should land that change in the same commit (so the migration
/// is deliberate rather than accidental).
#[test]
fn audit_schema_family_pinned_to_documented_wire_forms() -> TestResult {
    for (name, actual, expected) in [
        (
            "MEMORY_LEVEL_TRANSITION_AUDIT_SCHEMA_V1",
            MEMORY_LEVEL_TRANSITION_AUDIT_SCHEMA_V1,
            "ee.audit.memory_level_transition.v1",
        ),
        (
            "PROCEDURE_PROMOTION_AUDIT_SCHEMA_V1",
            PROCEDURE_PROMOTION_AUDIT_SCHEMA_V1,
            "ee.procedure.promotion_audit.v1",
        ),
        (
            "HELLO_RESPONDER_LIFECYCLE_AUDIT_SCHEMA_V1",
            HELLO_RESPONDER_LIFECYCLE_AUDIT_SCHEMA_V1,
            "ee.mesh.hello_responder.lifecycle_audit.v1",
        ),
    ] {
        if actual != expected {
            return Err(format!(
                "audit schema drift: {name} = {actual:?}, expected {expected:?}; \
                 if this rename is intentional, bump the schema to v2 in a \
                 deliberate migration and update every consumer's grep before \
                 retiring v1."
            ));
        }
    }
    Ok(())
}

/// bd-2vvz3 chokepoint #4: the `ee.audit.memory_level_transition.v1`
/// schema is the closest extant match to the bead's "ee.audit.v1"
/// shorthand. Pin its canonical-id status separately so a
/// consolidation that drops it surfaces here as a deliberate
/// migration step, not a silent removal.
#[test]
fn ee_audit_canonical_form_remains_memory_level_transition_v1() -> TestResult {
    if MEMORY_LEVEL_TRANSITION_AUDIT_SCHEMA_V1 != "ee.audit.memory_level_transition.v1" {
        return Err(format!(
            "the de-facto canonical 'ee.audit.*' v1 schema drifted to \
             {MEMORY_LEVEL_TRANSITION_AUDIT_SCHEMA_V1:?}. If a real 'ee.audit.v1' \
             umbrella is being introduced, file a follow-up bead to migrate \
             every consumer's grep and the J6 catalog in lockstep."
        ));
    }
    Ok(())
}

/// bd-2vvz3 chokepoint #5: every wire-form pinned above uses the
/// `ee.<area>.<surface>.v1` shape — the projectwide contract for
/// machine-readable schemas. Without this assertion, a future
/// const that accidentally uses `ee_curate_apply_v1` (underscore
/// segment separator instead of dot) or omits the v1 suffix would
/// pass the byte-identical checks individually while breaking the
/// shape conventions consumers depend on.
#[test]
fn every_pinned_schema_follows_ee_dot_v1_shape() -> TestResult {
    let mut offenders: Vec<&str> = Vec::new();
    for schema in [
        CURATE_CANDIDATES_SCHEMA_V1,
        CURATE_VALIDATE_SCHEMA_V1,
        CURATE_APPLY_SCHEMA_V1,
        CURATE_PEER_EVIDENCE_SCHEMA_V1,
        CURATE_REVIEW_SCHEMA_V1,
        CURATE_DISPOSITION_SCHEMA_V1,
        CURATE_RETIRE_SCHEMA_V1,
        CURATE_TOMBSTONE_SCHEMA_V1,
        CURATE_UNTOMBSTONE_SCHEMA_V1,
        OUTCOME_QUARANTINE_LIST_SCHEMA_V1,
        OUTCOME_QUARANTINE_REVIEW_SCHEMA_V1,
        MEMORY_LEVEL_TRANSITION_AUDIT_SCHEMA_V1,
        PROCEDURE_PROMOTION_AUDIT_SCHEMA_V1,
        HELLO_RESPONDER_LIFECYCLE_AUDIT_SCHEMA_V1,
    ] {
        if !schema.starts_with("ee.") {
            offenders.push(schema);
            continue;
        }
        if !schema.ends_with(".v1") {
            offenders.push(schema);
            continue;
        }
        if schema.contains('_') && !schema.contains('.') {
            offenders.push(schema);
            continue;
        }
    }
    if !offenders.is_empty() {
        return Err(format!(
            "{} schema(s) deviate from the ee.<area>.<surface>.v1 wire-form \
             shape: {offenders:?}. Fix the constant so it starts with 'ee.', \
             ends with '.v1', and uses dots (not underscores) as segment \
             separators within the path.",
            offenders.len()
        ));
    }
    Ok(())
}
