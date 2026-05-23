//! Contract coverage for `CassError`'s `PartialEq+Eq` variant-distinctness
//! semantics (bd-27d6b).
//!
//! `CassError` derives `Eq, PartialEq` (src/cass/error.rs:32). Today the
//! inline tests exercise specific variants for `kind_str`, `repair_hint`,
//! `is_degraded`, and `Display`, but no test pins the cross-variant
//! distinctness invariant: a `CassError::Io { message: "x" }` must
//! never compare equal to `CassError::Runtime { kind: "io", message: "x" }`
//! despite carrying similar field shapes. A future agent who manually
//! overrides `PartialEq` (to e.g. consider all I/O-shaped variants
//! equal for deduplication) could silently collapse variants and break
//! equality-based caching in upstream error consumers.

use std::path::PathBuf;

use ee::cass::CassError;

type TestResult = Result<(), String>;

fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

#[test]
fn same_variant_same_fields_compares_equal() -> TestResult {
    let a = CassError::Io {
        message: "broken pipe".to_string(),
    };
    let b = CassError::Io {
        message: "broken pipe".to_string(),
    };
    ensure(
        a == b,
        "CassError::Io with identical message must compare equal (PartialEq derive contract)",
    )
}

#[test]
fn same_variant_different_message_compares_unequal() -> TestResult {
    let a = CassError::Io {
        message: "broken pipe".to_string(),
    };
    let b = CassError::Io {
        message: "permission denied".to_string(),
    };
    ensure(
        a != b,
        "CassError::Io values with different messages must NOT compare equal — error caches rely on this to keep separate entries",
    )
}

#[test]
fn io_and_runtime_with_overlapping_fields_are_distinct() -> TestResult {
    // The most plausible silent collapse: a manual PartialEq override
    // that hashes only on (kind_str, message) would consider these
    // equal because Runtime carries kind="io". Pin that variants stay
    // discriminated structurally even when their kind_str overlaps
    // and message strings match.
    let io = CassError::Io {
        message: "synthetic message".to_string(),
    };
    let runtime = CassError::Runtime {
        kind: "io".to_string(),
        message: "synthetic message".to_string(),
    };
    ensure(
        io != runtime,
        "CassError::Io and CassError::Runtime{kind:\"io\"} must remain structurally distinct \
         even when stringly-similar — kind_str overlap is not equality",
    )
}

#[test]
fn unknown_and_runtime_with_same_kind_are_distinct() -> TestResult {
    // Same fields { kind, message } in two variants — the discriminant
    // is what differentiates Unknown (forward-compat) from Runtime
    // (known-but-not-degraded).
    let unknown = CassError::Unknown {
        kind: "future_kind".to_string(),
        message: "future message".to_string(),
    };
    let runtime = CassError::Runtime {
        kind: "future_kind".to_string(),
        message: "future message".to_string(),
    };
    ensure(
        unknown != runtime,
        "CassError::Unknown and CassError::Runtime carry identical field shapes; \
         the variant discriminant must keep them distinct (forward-compat is not runtime)",
    )
}

#[test]
fn invalid_binary_and_binary_not_found_are_distinct() -> TestResult {
    let invalid = CassError::InvalidBinary {
        binary: PathBuf::from("/usr/local/bin/cass"),
        reason: "outside allowlist".to_string(),
    };
    let not_found = CassError::BinaryNotFound {
        binary: PathBuf::from("/usr/local/bin/cass"),
    };
    ensure(
        invalid != not_found,
        "InvalidBinary and BinaryNotFound carry the same `binary` field but distinct semantics; \
         must compare unequal",
    )
}

#[test]
fn empty_stdout_equals_itself() -> TestResult {
    // Unit variants always equal themselves under derive. Pin so a
    // future agent who converts EmptyStdout to a struct variant with
    // a payload cannot silently break this identity.
    let a = CassError::EmptyStdout;
    let b = CassError::EmptyStdout;
    ensure(
        a == b,
        "CassError::EmptyStdout is a unit variant and must equal itself",
    )
}

#[test]
fn empty_stdout_unequal_to_io_with_empty_message() -> TestResult {
    let empty = CassError::EmptyStdout;
    let io_empty = CassError::Io {
        message: String::new(),
    };
    ensure(
        empty != io_empty,
        "CassError::EmptyStdout (no stdout payload was emitted) must never compare equal to \
         CassError::Io{message:\"\"} (subprocess IO failed with no message) — the variants \
         carry different semantic meanings",
    )
}

#[test]
fn degraded_with_different_repair_hints_are_unequal() -> TestResult {
    // The repair_hint string is part of Degraded's identity for
    // equality purposes — error caches keyed on equal-Degraded must
    // see distinct entries when the repair surface differs.
    let a = CassError::Degraded {
        kind: "stale_index".to_string(),
        repair_hint: "ee maintenance run --jobs index-refresh".to_string(),
    };
    let b = CassError::Degraded {
        kind: "stale_index".to_string(),
        repair_hint: "rebuild manually".to_string(),
    };
    ensure(
        a != b,
        "Degraded values with same kind but different repair_hint must NOT collapse",
    )
}

#[test]
fn contract_mismatch_compares_both_required_and_observed() -> TestResult {
    let a = CassError::ContractMismatch {
        required: "api_version=1".to_string(),
        observed: "api_version=2".to_string(),
    };
    let b = CassError::ContractMismatch {
        required: "api_version=1".to_string(),
        observed: "api_version=3".to_string(),
    };
    ensure(
        a != b,
        "ContractMismatch carries both required and observed; equality must inspect both",
    )
}
