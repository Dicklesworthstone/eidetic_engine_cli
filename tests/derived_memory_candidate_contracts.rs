// bd-17pa6: ADR 0043 verification-hook gap closure.
//
// Pins three contract surfaces that the ADR documents but did not have
// a discoverable test file for before this bead:
//
// 1. `CandidateType` contract — `all()`, `as_str`, `FromStr`,
//    `requires_content`, `requires_target_memory`, and the parse-error
//    expected-list message all include `create_derived_memory` exactly
//    once and keep `paraphrase_dedup_proposal`.
// 2. Audit-row schema reference — the ADR-named audit schema
//    `ee.audit.derived_memory_created.v1` is referenced from the
//    production source so a future rename of the schema string trips
//    a focused test instead of silently drifting away from the ADR.
// 3. Provenance URI scheme registry — `ProvenanceUri::from_str`
//    accepts only the 5 v1 schemes documented in
//    `src/models/provenance.rs` and rejects anything else with the
//    `UnknownScheme` error, so derived-memory creation cannot smuggle
//    an unregistered scheme through `PackProvenance::new`.
//
// bd-8k69m closed the remaining DB-backed ADR 0043 obligations in
// `src/core/curate.rs`; this file stays focused on the static contract
// hooks that can fail without standing up a database.

#![forbid(unsafe_code)]

use std::str::FromStr;

use ee::curate::{CandidateType, ParseCandidateTypeError};
use ee::models::{ProvenanceUri, ProvenanceUriError};

type TestResult = Result<(), String>;

fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

// --- 1. CandidateType contract ----------------------------------------

#[test]
fn candidate_type_all_includes_create_derived_memory_exactly_once() -> TestResult {
    let all = CandidateType::all();
    let derived_count = all
        .iter()
        .filter(|ct| **ct == CandidateType::CreateDerivedMemory)
        .count();
    ensure(
        derived_count == 1,
        format!(
            "CandidateType::all() must include CreateDerivedMemory exactly \
             once; found {derived_count} occurrences in {all:?}"
        ),
    )?;
    let paraphrase_count = all
        .iter()
        .filter(|ct| **ct == CandidateType::ParaphraseDedupProposal)
        .count();
    ensure(
        paraphrase_count == 1,
        format!(
            "CandidateType::all() must keep ParaphraseDedupProposal exactly \
             once; found {paraphrase_count} occurrences in {all:?}"
        ),
    )
}

#[test]
fn candidate_type_as_str_uses_canonical_create_derived_memory_token() -> TestResult {
    ensure(
        CandidateType::CreateDerivedMemory.as_str() == "create_derived_memory",
        format!(
            "CandidateType::CreateDerivedMemory.as_str() must remain \
             \"create_derived_memory\" so DB CHECK constraints, JSON \
             envelopes, and CLI validators agree on the wire token; \
             got {:?}",
            CandidateType::CreateDerivedMemory.as_str()
        ),
    )?;
    ensure(
        CandidateType::ParaphraseDedupProposal.as_str() == "paraphrase_dedup_proposal",
        format!(
            "CandidateType::ParaphraseDedupProposal.as_str() must remain \
             \"paraphrase_dedup_proposal\"; got {:?}",
            CandidateType::ParaphraseDedupProposal.as_str()
        ),
    )
}

#[test]
fn candidate_type_from_str_roundtrips_create_derived_memory() -> TestResult {
    let parsed = CandidateType::from_str("create_derived_memory")
        .map_err(|error| format!("FromStr rejected canonical token: {error}"))?;
    ensure(
        parsed == CandidateType::CreateDerivedMemory,
        format!(
            "FromStr(\"create_derived_memory\") must map to \
             CreateDerivedMemory; got {parsed:?}"
        ),
    )?;
    // Roundtrip every variant — keeps the contract tight against a
    // future variant that ships with as_str() and FromStr in disagreement.
    for ct in CandidateType::all() {
        let rendered = ct.as_str();
        let reparsed = CandidateType::from_str(rendered).map_err(|error| {
            format!("FromStr rejected canonical as_str() output {rendered:?}: {error}")
        })?;
        ensure(
            reparsed == ct,
            format!(
                "as_str/FromStr roundtrip failed for {ct:?}: {rendered:?} \
                 parsed as {reparsed:?}"
            ),
        )?;
    }
    Ok(())
}

#[test]
fn candidate_type_from_str_error_message_lists_create_derived_memory() -> TestResult {
    let err: ParseCandidateTypeError = CandidateType::from_str("garbage_type")
        .err()
        .ok_or_else(|| "FromStr accepted garbage input".to_string())?;
    let rendered = err.to_string();
    ensure(
        rendered.contains("create_derived_memory"),
        format!(
            "ParseCandidateTypeError message must list \
             create_derived_memory so CLI users see it as a valid option; \
             got {rendered:?}"
        ),
    )?;
    ensure(
        rendered.contains("paraphrase_dedup_proposal"),
        format!(
            "ParseCandidateTypeError message must keep \
             paraphrase_dedup_proposal in the expected-list; got {rendered:?}"
        ),
    )?;
    ensure(
        rendered.contains("garbage_type"),
        format!(
            "ParseCandidateTypeError message must echo the offending \
             input; got {rendered:?}"
        ),
    )
}

#[test]
fn candidate_type_requires_content_includes_create_derived_memory() -> TestResult {
    ensure(
        CandidateType::CreateDerivedMemory.requires_content(),
        "create_derived_memory must require content per ADR 0043 \
         (the new memory's body comes from the candidate itself)",
    )?;
    ensure(
        !CandidateType::Tombstone.requires_content(),
        "Tombstone must remain content-less so the requires_content \
         contract does not collapse to always-true",
    )
}

#[test]
fn candidate_type_requires_target_memory_excludes_only_create_derived_memory() -> TestResult {
    // ADR 0043: create_derived_memory is the ONLY candidate type that
    // does not point at an existing target memory; every other variant
    // does. If a future variant joins the no-target club it should also
    // be reflected in ADR 0043's Apply path section.
    for ct in CandidateType::all() {
        let expected = ct != CandidateType::CreateDerivedMemory;
        ensure(
            ct.requires_target_memory() == expected,
            format!(
                "{ct:?}.requires_target_memory() == {actual} but ADR 0043 \
                 expects {expected}: only create_derived_memory may omit \
                 a target_memory_id",
                actual = ct.requires_target_memory()
            ),
        )?;
    }
    Ok(())
}

// --- 2. Audit-row schema reference ------------------------------------

#[test]
fn derived_memory_created_audit_schema_is_referenced_from_curate_source() -> TestResult {
    // The ADR documents that `ee curate apply` on a create_derived_memory
    // candidate writes a `memory.create` audit row whose
    // `details.schema = "ee.audit.derived_memory_created.v1"`. A
    // dedicated runtime assertion is tracked under bd-17pa6's
    // Verification gaps as an apply-path DB test. This static test
    // pins the upstream invariant: the schema string must be present
    // in the curate source so a rename trips a focused failure rather
    // than silently drifting away from the ADR.
    const SCHEMA: &str = "ee.audit.derived_memory_created.v1";
    const SOURCE: &str = include_str!("../src/core/curate.rs");
    let occurrences = SOURCE.matches(SCHEMA).count();
    ensure(
        occurrences >= 2,
        format!(
            "src/core/curate.rs must reference the ADR-named audit \
             schema {SCHEMA:?} at least twice (apply-path emission + \
             at least one consistency guard); found {occurrences} \
             occurrences"
        ),
    )
}

// --- 3. Provenance URI scheme registry --------------------------------

#[test]
fn provenance_uri_accepts_registered_schemes() -> TestResult {
    // The v1 registry in src/models/provenance.rs accepts exactly the
    // source schemes represented in this fixture list.
    // Derived-memory creation must never write an unregistered scheme;
    // the type system enforces this at the PackProvenance boundary
    // because PackProvenance::new takes a `ProvenanceUri`, not a raw
    // string. This test pins the scheme registry so a rename or
    // removal trips a focused failure before deeper tests notice.
    let accepted = [
        "cass-session://session-id-1234#L10",
        "file:///tmp/example.rs#L1-12",
        "ee-mem://mem_01jw0a2b3c4d5e6f7g8h9j0k1m",
        "https://example.com/path",
        "http://example.com/path",
        "agent-mail://thread-id/message-id",
        "manual://agent-note/2026-06-02",
        "bench-run://2026-09-12T14:23/oltp-mixed-small-n",
        "git-sha://9af3c21-pre-revert",
        "flamegraph://artifacts/9af3c21/cpu-prof.svg",
    ];
    for input in accepted {
        ProvenanceUri::from_str(input)
            .map_err(|error| format!("v1 scheme rejected {input:?}: {error}"))?;
    }
    Ok(())
}

#[test]
fn provenance_uri_rejects_unregistered_scheme_with_named_error() -> TestResult {
    // The bd-17pa6 contract: derived-memory creation can only carry
    // schemes the registry accepts. Anything else surfaces
    // `ProvenanceUriError::UnknownScheme` so the error is identifiable
    // by downstream callers rather than swallowed into a generic parse
    // failure.
    let err = ProvenanceUri::from_str("gopher://example.com/menu")
        .err()
        .ok_or_else(|| "unregistered scheme `gopher://` was accepted".to_string())?;
    match err {
        ProvenanceUriError::UnknownScheme { scheme, .. } => ensure(
            scheme == "gopher",
            format!(
                "UnknownScheme error must echo the offending scheme; \
                 got scheme={scheme:?}"
            ),
        ),
        other => Err(format!(
            "unregistered scheme must surface UnknownScheme; got {other:?}"
        )),
    }
}
