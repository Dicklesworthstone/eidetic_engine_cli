//! Contract coverage for `CassHealth::is_ready` and `CassHealth::is_stale`
//! truth tables (bd-2re3k).
//!
//! `CassHealth::is_ready` is the 3-way AND of `healthy && db.opened &&
//! index.fresh`. `CassHealth::is_stale` is the 2-way AND of
//! `index.exists && index.stale`. Today the inline tests in
//! `src/cass/health.rs` (`is_ready_requires_healthy_and_fresh`,
//! `is_stale_detects_stale_index`) cover the positive case and one or
//! two failure shapes but do not pin the full 2^3 = 8 truth table for
//! `is_ready` or the 2^2 = 4 truth table for `is_stale`. A silent edit
//! that drops the `index.exists` clause from `is_stale` (turning a
//! never-indexed CASS into a "stale" signal) would slip past existing
//! coverage.

use ee::cass::{CassDbHealth, CassHealth, CassIndexHealth};

type TestResult = Result<(), String>;

fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

fn build(healthy: bool, opened: bool, exists: bool, fresh: bool, stale: bool) -> CassHealth {
    CassHealth {
        status: if healthy { "healthy" } else { "unhealthy" }.to_string(),
        healthy,
        errors: vec![],
        latency_ms: 1,
        db: CassDbHealth {
            exists: true,
            opened,
            conversations: None,
            messages: None,
            counts_skipped: false,
            open_skipped: false,
        },
        index: CassIndexHealth {
            exists,
            status: if fresh {
                "fresh".to_string()
            } else {
                "stale".to_string()
            },
            fresh,
            stale,
            documents: None,
        },
    }
}

// ---------------------------------------------------------------------------
// is_ready truth table: (healthy, db.opened, index.fresh) -> 8 combinations
// ---------------------------------------------------------------------------

#[test]
fn is_ready_true_when_all_three_clauses_true() -> TestResult {
    ensure(
        build(true, true, true, true, false).is_ready(),
        "is_ready must be true when healthy && db.opened && index.fresh",
    )
}

#[test]
fn is_ready_false_when_unhealthy() -> TestResult {
    ensure(
        !build(false, true, true, true, false).is_ready(),
        "is_ready must be false when !healthy (regardless of db/index)",
    )
}

#[test]
fn is_ready_false_when_db_not_opened() -> TestResult {
    ensure(
        !build(true, false, true, true, false).is_ready(),
        "is_ready must be false when !db.opened",
    )
}

#[test]
fn is_ready_false_when_index_not_fresh() -> TestResult {
    ensure(
        !build(true, true, true, false, true).is_ready(),
        "is_ready must be false when !index.fresh",
    )
}

#[test]
fn is_ready_false_when_unhealthy_and_db_closed() -> TestResult {
    ensure(
        !build(false, false, true, true, false).is_ready(),
        "is_ready must be false when both !healthy and !db.opened",
    )
}

#[test]
fn is_ready_false_when_unhealthy_and_index_stale() -> TestResult {
    ensure(
        !build(false, true, true, false, true).is_ready(),
        "is_ready must be false when !healthy and !index.fresh",
    )
}

#[test]
fn is_ready_false_when_db_closed_and_index_stale() -> TestResult {
    ensure(
        !build(true, false, true, false, true).is_ready(),
        "is_ready must be false when !db.opened and !index.fresh",
    )
}

#[test]
fn is_ready_false_when_all_three_clauses_false() -> TestResult {
    ensure(
        !build(false, false, true, false, true).is_ready(),
        "is_ready must be false when none of the three clauses hold",
    )
}

// ---------------------------------------------------------------------------
// is_stale truth table: (index.exists, index.stale) -> 4 combinations
// ---------------------------------------------------------------------------

#[test]
fn is_stale_true_when_index_exists_and_stale() -> TestResult {
    ensure(
        build(false, false, true, false, true).is_stale(),
        "is_stale must be true when index.exists && index.stale",
    )
}

#[test]
fn is_stale_false_when_index_does_not_exist() -> TestResult {
    // A CASS that was never indexed must NOT show as "stale" — the
    // signal would mislead the operator into running ee maintenance
    // run --jobs index-refresh when the real action is index_build.
    ensure(
        !build(false, false, false, false, true).is_stale(),
        "is_stale must be false when !index.exists, even when index.stale is true",
    )
}

#[test]
fn is_stale_false_when_index_fresh() -> TestResult {
    ensure(
        !build(true, true, true, true, false).is_stale(),
        "is_stale must be false when index.exists but index.stale is false",
    )
}

#[test]
fn is_stale_false_when_neither_clause_true() -> TestResult {
    ensure(
        !build(true, true, false, true, false).is_stale(),
        "is_stale must be false when both clauses are false",
    )
}
