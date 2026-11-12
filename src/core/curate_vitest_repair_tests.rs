//! Native Vitest reports through the actual store and reviewed lesson workflow.

use super::jest::{assert_native_repair_lifecycle, native_episode};
use super::*;
use crate::core::cass_error_recall::record_session_error_recall;

const VITEST: &str = "npx vitest run --reporter=verbose";
const FAILED: &str = include_str!("../../tests/fixtures/vitest_terminal/failed.txt");
const PASSED: &str = include_str!("../../tests/fixtures/vitest_terminal/passed.txt");

#[test]
fn native_vitest_repair_reaches_recall_card_review_and_explicit_durable_apply() -> TestResult {
    assert_native_repair_lifecycle(VITEST, FAILED, PASSED, 92_001)
}

#[test]
fn native_vitest_unproven_retries_cannot_become_cards_or_durable_lessons() -> TestResult {
    let workspace = Workspace::new()?;
    let aggregate = PASSED
        .lines()
        .filter(|line| !line.starts_with(" ✓ "))
        .collect::<Vec<_>>()
        .join("\n")
        .replace(
            "\n Test Files ",
            "\n ✓ src/cache.test.ts (2 tests) 3ms\n\n Test Files ",
        );
    let removed = PASSED
        .replace(
            " ✓ src/cache.test.ts > cache > rejects stale entries 1ms\n",
            "",
        )
        .replace("2 passed (2)", "1 passed (1)");
    let renamed = PASSED.replace("returns cached value", "unrelated replacement");
    let skipped = PASSED
        .replace(
            " ✓ src/cache.test.ts > cache > returns cached value 2ms",
            " ↓ src/cache.test.ts > cache > returns cached value",
        )
        .replace("2 passed (2)", "1 passed | 1 skipped (2)");
    let truncated = format!("{PASSED}\nOutput truncated");
    let console = format!("stdout | src/cache.test.ts > cache > returns cached value\n{PASSED}");
    let watch = PASSED.replace(" RUN  ", " DEV  ");
    let snapshots = PASSED.replace(" Test Files ", "  Snapshots  1 updated\n Test Files ");
    for (offset, retry, proof, status) in [
        (0, VITEST, aggregate.as_str(), Some(false)),
        (1, VITEST, removed.as_str(), Some(false)),
        (2, VITEST, renamed.as_str(), Some(false)),
        (3, VITEST, skipped.as_str(), Some(false)),
        (4, VITEST, truncated.as_str(), Some(false)),
        (5, VITEST, console.as_str(), Some(false)),
        (6, VITEST, watch.as_str(), Some(false)),
        (7, VITEST, snapshots.as_str(), Some(false)),
        (
            8,
            "npx vitest run --reporter=verbose other.test.ts",
            PASSED,
            Some(false),
        ),
        (9, VITEST, PASSED, None),
        (10, VITEST, PASSED, Some(true)),
    ] {
        let episode = native_episode(
            &workspace,
            92_010 + offset,
            VITEST,
            FAILED,
            retry,
            proof,
            status,
        )?;
        let report = record_session_error_recall(&workspace.db, &workspace.id, &episode.session)?;
        assert_eq!(report.fingerprints_recorded, 1, "case {offset}");
        assert_eq!(report.resolved_failures, 0, "case {offset}");
        assert_eq!(report.repair_links_recorded, 0, "case {offset}");
        assert_eq!(report.incident_cards_recorded, 0, "case {offset}");
        assert!(
            native(&workspace.review(&episode, 0.0, false)?).is_empty(),
            "case {offset}"
        );
    }
    assert!(
        workspace
            .db
            .list_memories(&workspace.id, None, true)?
            .is_empty()
    );
    Ok(())
}
