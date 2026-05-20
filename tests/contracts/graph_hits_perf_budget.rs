//! bd-jy4w.4 — static contract that pins the HITS perf-budget
//! ≤100ms / 1k-node assertion the bead acceptance demands.
//!
//! benches/graph_hits.rs runs only under `cargo bench` (criterion);
//! the existing budget machinery enforces p50 ≤ BUDGET_P50_MS for
//! every scale in SCALES at compare-only time, BUT only when an
//! operator actually executes that bench (and only on hosts where
//! local cargo runs — RCH-blocked in this checkout per
//! bd-17c65.10.17.1.3).
//!
//! This static contract closes the gap by parsing the bench source
//! and asserting the load-bearing constants haven't drifted:
//! BUDGET_P50_MS == 100.0 (the bead's ≤100ms ceiling) and the SCALES
//! array includes 1000 (the bead's 1k-node fixture). A future
//! refactor that raises the budget or drops the 1k scale fails the
//! contract in the default-feature build, BEFORE the bench would
//! have caught the drift.
//!
//! Asserts:
//!
//! 1. benches/graph_hits.rs exists at the canonical path.
//! 2. BUDGET_P50_MS is declared and equals `100.0` (the bead's
//!    ≤100ms ceiling).
//! 3. SCALES array includes `1000` (the 1k-node fixture the bead
//!    names).
//! 4. BENCH_GROUP_NAME equals `"graph_hits"` (the documented group
//!    name renderers anchor against).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::path::PathBuf;

type TestResult = Result<(), String>;

const BENCH_PATH: &str = "benches/graph_hits.rs";

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn bench_body() -> Result<String, String> {
    let path = repo_root().join(BENCH_PATH);
    fs::read_to_string(&path).map_err(|error| format!("read {}: {error}", path.display()))
}

fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

#[test]
fn graph_hits_bench_exists_at_canonical_path() -> TestResult {
    let _ = bench_body()?;
    Ok(())
}

#[test]
fn graph_hits_p50_budget_pinned_at_100ms() -> TestResult {
    // The bead's perf assertion: "HITS compute ≤100ms on 1k-node
    // fixture." benches/graph_hits.rs encodes this as
    // BUDGET_P50_MS = 100.0. Pin the value so a future refactor that
    // raises the ceiling fails the contract instead of silently
    // letting a slower implementation through.
    let body = bench_body()?;
    let needle = "const BUDGET_P50_MS: f64 = 100.0;";
    ensure(
        body.contains(needle),
        format!(
            "benches/graph_hits.rs must declare `{needle}` so the ≤100ms perf assertion the bead acceptance names stays locked in",
        ),
    )
}

#[test]
fn graph_hits_scales_includes_thousand_node_fixture() -> TestResult {
    // The bead names a 1k-node fixture explicitly. Pin SCALES so it
    // can't quietly lose the 1000 scale.
    let body = bench_body()?;
    let needle = "SCALES: &[usize] = &[10, 100, 1000]";
    ensure(
        body.contains(needle),
        format!(
            "benches/graph_hits.rs SCALES must include the 1k-node fixture the bead acceptance names; expected literal substring `{needle}`",
        ),
    )
}

#[test]
fn graph_hits_bench_group_name_pinned() -> TestResult {
    // Renderers and baselines under benches/baselines/ anchor on the
    // bench group name `graph_hits`. Pin so the baseline lookup
    // can't break under rename.
    let body = bench_body()?;
    let needle = "BENCH_GROUP_NAME: &str = \"graph_hits\"";
    ensure(
        body.contains(needle),
        format!(
            "benches/graph_hits.rs must declare `{needle}` so the criterion group name stays stable for baseline lookup",
        ),
    )
}
