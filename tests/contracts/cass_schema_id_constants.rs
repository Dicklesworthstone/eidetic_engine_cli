//! Contract coverage for cass-side schema-id string constants exported
//! from `ee::models` (bd-1ewgp).
//!
//! These wire-format identifiers are written into import-ledger rows,
//! session-metadata JSON, and evidence-span audit details. Agent
//! harnesses key on the exact strings. Today only `IMPORT_CASS_SCHEMA_V1`
//! is indirectly pinned through `docs_schemas_match_responses`
//! (registered in SCHEMA_DOCS). The other three constants in
//! `src/models/mod.rs` have no direct test pin — silently renaming
//! `ee.import_ledger.cass.v1` -> `ee.cass.import_ledger.v1` would shift
//! the wire format without any test failing.

use ee::models::{
    CASS_EVIDENCE_SPAN_SCHEMA_V1, CASS_SESSION_SCHEMA_V1, IMPORT_CASS_SCHEMA_V1,
    IMPORT_LEDGER_CASS_SCHEMA_V1,
};

type TestResult = Result<(), String>;

fn ensure_equal<T: std::fmt::Debug + PartialEq>(
    actual: &T,
    expected: &T,
    context: &str,
) -> TestResult {
    if actual == expected {
        Ok(())
    } else {
        Err(format!("{context}: expected {expected:?}, got {actual:?}"))
    }
}

#[test]
fn import_cass_schema_v1_is_pinned() -> TestResult {
    ensure_equal(
        &IMPORT_CASS_SCHEMA_V1,
        &"ee.import.cass.v1",
        "ee.import.cass.v1 is the top-level command response envelope for ee import cass; \
         agent harnesses match on this string",
    )
}

#[test]
fn import_ledger_cass_schema_v1_is_pinned() -> TestResult {
    ensure_equal(
        &IMPORT_LEDGER_CASS_SCHEMA_V1,
        &"ee.import_ledger.cass.v1",
        "ee.import_ledger.cass.v1 is stored as metadata_json on every CASS import-ledger row; \
         changing it breaks downstream replay and audit tooling",
    )
}

#[test]
fn cass_session_schema_v1_is_pinned() -> TestResult {
    ensure_equal(
        &CASS_SESSION_SCHEMA_V1,
        &"ee.cass_session.v1",
        "ee.cass_session.v1 tags per-session details written into redaction-audit envelopes",
    )
}

#[test]
fn cass_evidence_span_schema_v1_is_pinned() -> TestResult {
    ensure_equal(
        &CASS_EVIDENCE_SPAN_SCHEMA_V1,
        &"ee.cass_evidence_span.v1",
        "ee.cass_evidence_span.v1 tags per-span details written into evidence-span audit details",
    )
}

#[test]
fn cass_schema_id_constants_are_distinct() -> TestResult {
    // Sanity check that no two constants collapse to the same wire
    // string. Collapsing would erase the discriminator that downstream
    // schema-aware consumers use to dispatch.
    let all = [
        ("IMPORT_CASS_SCHEMA_V1", IMPORT_CASS_SCHEMA_V1),
        ("IMPORT_LEDGER_CASS_SCHEMA_V1", IMPORT_LEDGER_CASS_SCHEMA_V1),
        ("CASS_SESSION_SCHEMA_V1", CASS_SESSION_SCHEMA_V1),
        ("CASS_EVIDENCE_SPAN_SCHEMA_V1", CASS_EVIDENCE_SPAN_SCHEMA_V1),
    ];
    for (i, (name_a, value_a)) in all.iter().enumerate() {
        for (name_b, value_b) in all.iter().skip(i + 1) {
            if value_a == value_b {
                return Err(format!(
                    "{name_a} and {name_b} both equal {value_a:?}; cass schema ids must be distinct"
                ));
            }
        }
    }
    Ok(())
}
