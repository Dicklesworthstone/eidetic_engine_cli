//! Criterion benchmark for ADR 0064 code-anchored recall (bd-u875s.2).
//!
//! Group name: `ee_recall`
//!
//! Pins the recall core's warm-path latency contract: the engine sits on the
//! pre-edit hook path, so the ADR budget is p50 < 30 ms on the mac-m3-pro
//! hardware class. Budgets are advisory until run-to-run variance is known
//! (`ee.perf.v1` consumers read the constants; the bench itself only
//! measures). The fixture exercises the deterministic engine over a corpus
//! larger than the bounded candidate scan cap so the measured path includes
//! glob matching, dedup, ranking, and budget truncation.

#![allow(clippy::expect_used)]

use criterion::{BenchmarkId, Criterion, black_box, criterion_group, criterion_main};
use ee::core::recall::{
    RECALL_CANDIDATE_SCAN_CAP, RecallCandidateRow, RecallProvenanceRef, RecallQuery,
    evaluate_recall,
};
use ee::models::{MemoryAnchorFreshnessState, MemoryAnchorKind};

const RECALL_BENCH_GROUP: &str = "ee_recall";
const RECALL_OPERATION: &str = "ee_recall_evaluate";
const RECALL_BUDGET_P50_MS: f64 = 30.0;
const RECALL_BUDGET_P99_MS: f64 = 90.0;
const RECALL_FIXTURE_ROW_COUNT: usize = RECALL_CANDIDATE_SCAN_CAP + 256;

const _: () = assert!(
    RECALL_BUDGET_P50_MS > 0.0 && RECALL_BUDGET_P99_MS >= RECALL_BUDGET_P50_MS,
    "recall benchmark budgets must be positive and monotonic"
);
const _: () = assert!(
    RECALL_FIXTURE_ROW_COUNT > RECALL_CANDIDATE_SCAN_CAP,
    "fixture must exceed the bounded scan cap so the cap is exercised"
);

fn fixture_rows() -> Vec<RecallCandidateRow> {
    (0..RECALL_FIXTURE_ROW_COUNT)
        .map(|index| {
            let is_symbol = index % 7 == 6;
            let module = ["core", "db", "cli", "search", "output"][index % 5];
            let level = ["procedural", "semantic", "episodic", "working"][index % 4];
            let kind = ["rule", "fact", "failure", "decision", "risk"][index % 5];
            let freshness = match index % 7 {
                0 => MemoryAnchorFreshnessState::Stale,
                1 | 2 => MemoryAnchorFreshnessState::Suspect,
                _ => MemoryAnchorFreshnessState::Current,
            };
            RecallCandidateRow {
                memory_id: format!("mem_{index:026}"),
                anchor_kind: if is_symbol {
                    MemoryAnchorKind::Symbol
                } else {
                    MemoryAnchorKind::Path
                },
                normalized_path: (!is_symbol)
                    .then(|| format!("src/{module}/file_{:04}.rs", index % 800)),
                symbol: is_symbol
                    .then(|| format!("Module{}::function_{}", index % 40, index % 200)),
                freshness_state: freshness,
                row_generation: 11,
                level: level.to_owned(),
                kind: kind.to_owned(),
                confidence: 0.35 + ((index % 60) as f32) / 100.0,
                content: format!(
                    "Recall benchmark memory {index:04}: durable note about src/{module} \
                     conventions, verification commands, and prior failures for this surface."
                ),
                tombstoned: index % 97 == 0,
                tags: vec!["bench".to_owned(), module.to_owned()],
                provenance: vec![RecallProvenanceRef {
                    uri: format!("bench://recall/{index}"),
                    source_type: "bench".to_owned(),
                }],
            }
        })
        .collect()
}

fn glob_query() -> RecallQuery {
    RecallQuery {
        paths: vec!["src/core/*.rs".to_owned(), "src/db/file_00*.rs".to_owned()],
        max_tokens: Some(400),
        ..RecallQuery::default()
    }
}

fn symbol_query() -> RecallQuery {
    RecallQuery {
        symbols: (0..16)
            .map(|index| format!("Module{index}::function_{index}"))
            .collect(),
        max_tokens: Some(400),
        ..RecallQuery::default()
    }
}

fn diff_query() -> RecallQuery {
    RecallQuery {
        diff_paths: (0..32)
            .map(|index| format!("src/search/file_{:04}.rs", (index * 5 + 3) % 800))
            .collect(),
        max_tokens: Some(400),
        ..RecallQuery::default()
    }
}

fn assert_recall_benchmark_contract(rows: &[RecallCandidateRow]) {
    assert_eq!(RECALL_BENCH_GROUP, "ee_recall");
    assert_eq!(RECALL_OPERATION, "ee_recall_evaluate");
    black_box((RECALL_BUDGET_P50_MS, RECALL_BUDGET_P99_MS));

    // The fixture must actually exercise matching, ranking, truncation, and
    // determinism — an accidentally-empty result would benchmark a no-op.
    for query in [glob_query(), symbol_query(), diff_query()] {
        let report = evaluate_recall(&query, rows, Some(11), 11);
        assert!(
            !report.items.is_empty(),
            "recall benchmark query must match fixture rows"
        );
        assert_eq!(
            report,
            evaluate_recall(&query, rows, Some(11), 11),
            "recall benchmark must be deterministic"
        );
    }
    let truncated = evaluate_recall(&glob_query(), rows, Some(11), 11);
    assert!(
        truncated.truncated && truncated.continuation_cursor.is_some(),
        "the 400-token budget must force truncation so the cursor path is measured"
    );
}

fn bench_recall(c: &mut Criterion) {
    let rows = fixture_rows();
    assert_recall_benchmark_contract(&rows);
    let mut group = c.benchmark_group(RECALL_BENCH_GROUP);
    for (label, query) in [
        ("path_glob", glob_query()),
        ("symbol_exact", symbol_query()),
        ("diff_path_set", diff_query()),
    ] {
        group.bench_function(BenchmarkId::new(RECALL_OPERATION, label), |b| {
            b.iter(|| black_box(evaluate_recall(&query, &rows, Some(11), 11)));
        });
    }
    group.finish();
}

criterion_group!(benches, bench_recall);
criterion_main!(benches);
