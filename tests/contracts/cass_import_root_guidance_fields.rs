//! Contract coverage for `CassImportRootGuidance` struct field
//! round-trip (bd-2okvr).
//!
//! `CassImportRootGuidance` (defined at `src/core/doctor.rs:1475`)
//! carries the per-agent-root guidance assembled by
//! `CassImportGuidance::from_agent_inventory` and rendered into the
//! `cassImportGuidance.roots[]` array of `ee doctor --fix-plan`.
//! Today peer bd-2cmcu pins the `from_agent_inventory` routing
//! behavior, but no test pins the bare struct's field round-trip — a
//! future agent who renamed `connector` -> `agent_slug` or reordered
//! fields could break consumer parsing without surfacing in
//! coverage above the routing layer.
//!
//! Sister to bd-rja7x (CassSessionInfo defaults), bd-2bwqd
//! (CassImportOptions defaults), bd-20dng (CassViewSpan defaults).

use ee::core::doctor::CassImportRootGuidance;

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

fn fixture() -> CassImportRootGuidance {
    CassImportRootGuidance {
        connector: "claude_code".to_string(),
        root_path: "/Users/test-fixture/.claude".to_string(),
        guidance: "Review CASS dry-run coverage for claude_code history rooted at /Users/test-fixture/.claude."
            .to_string(),
    }
}

#[test]
fn struct_literal_preserves_connector() -> TestResult {
    let guidance = fixture();
    ensure_equal(
        &guidance.connector,
        &"claude_code".to_string(),
        "connector field round-trip",
    )
}

#[test]
fn struct_literal_preserves_root_path() -> TestResult {
    let guidance = fixture();
    ensure_equal(
        &guidance.root_path,
        &"/Users/test-fixture/.claude".to_string(),
        "root_path field round-trip",
    )
}

#[test]
fn struct_literal_preserves_guidance_message() -> TestResult {
    let guidance = fixture();
    ensure_equal(
        &guidance.guidance,
        &"Review CASS dry-run coverage for claude_code history rooted at /Users/test-fixture/.claude.".to_string(),
        "guidance field round-trip",
    )
}

#[test]
fn struct_literal_preserves_all_three_fields_simultaneously() -> TestResult {
    // Catches a refactor that accidentally swaps fields (e.g.
    // connector getter accidentally returns root_path's storage).
    let guidance = CassImportRootGuidance {
        connector: "codex".to_string(),
        root_path: "/data/codex/sessions".to_string(),
        guidance: "scan codex sessions".to_string(),
    };
    ensure_equal(
        &guidance.connector,
        &"codex".to_string(),
        "connector simultaneously",
    )?;
    ensure_equal(
        &guidance.root_path,
        &"/data/codex/sessions".to_string(),
        "root_path simultaneously",
    )?;
    ensure_equal(
        &guidance.guidance,
        &"scan codex sessions".to_string(),
        "guidance simultaneously",
    )
}

#[test]
fn struct_derives_clone_and_partial_eq() -> TestResult {
    // src/core/doctor.rs:1475 derives Clone, Debug, Eq, PartialEq.
    // Pin Clone-equality round-trip so a future agent who removes
    // Clone or PartialEq from the derive list is caught.
    let original = fixture();
    let cloned = original.clone();
    ensure_equal(
        &cloned,
        &original,
        "Clone must produce a PartialEq-equal value",
    )
}
