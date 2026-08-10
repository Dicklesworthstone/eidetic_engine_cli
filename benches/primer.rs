//! Criterion benchmark for the ADR 0065 workspace primer (bd-39tzu.5).
//!
//! Group name: `ee_primer`
//!
//! Two operations pin the primer latency story:
//!
//! - `ee_primer_cold_assemble`: the pure deterministic assembly
//!   (`assemble_primer`) over a fixture larger than the per-section
//!   candidate cap, so the measured path includes redaction gating,
//!   ranking, cross-section dedup, quota fill, and the rules floor.
//!   Cold assembly is unbounded by contract but measured.
//! - `ee_primer_warm_cache_hit`: `run_primer_with_persistence` against a
//!   warmed `primer_cache` row. This is the load-bearing number for the
//!   SessionStart hook recipe: the epic budget is p50 < 100 ms
//!   warm-from-cache on the mac-m3-pro hardware class. Budgets are
//!   advisory until run-to-run variance is known (`ee.perf.v1` consumers
//!   read the constants; the bench itself only measures).

#![allow(clippy::expect_used, clippy::unwrap_used)]

use criterion::{Criterion, criterion_group, criterion_main};
use ee::core::primer::{
    PrimerCandidate, PrimerCentralityRow, PrimerFormat, PrimerSettings, assemble_primer,
    primer_config_hash, run_primer_with_persistence,
};
use std::hint::black_box;

const PRIMER_BENCH_GROUP: &str = "ee_primer";
const PRIMER_COLD_OPERATION: &str = "ee_primer_cold_assemble";
const PRIMER_WARM_OPERATION: &str = "ee_primer_warm_cache_hit";
/// Epic latency target: warm-from-cache p50 (advisory).
const PRIMER_WARM_BUDGET_P50_MS: f64 = 100.0;
/// Fixture larger than one section's candidate cap (512) so ranking and
/// truncation are exercised, not skipped.
const PRIMER_FIXTURE_MEMORY_COUNT: usize = 768;

const _: () = assert!(
    PRIMER_WARM_BUDGET_P50_MS > 0.0,
    "warm budget must be positive"
);

fn fixture_candidates() -> Vec<PrimerCandidate> {
    (0..PRIMER_FIXTURE_MEMORY_COUNT)
        .map(|index| {
            let (level, kind) = match index % 4 {
                0 => ("procedural", "rule"),
                1 => ("episodic", "failure"),
                2 => ("semantic", "decision"),
                _ => ("semantic", "fact"),
            };
            PrimerCandidate {
                memory_id: format!("mem_{index:026}"),
                level: level.to_owned(),
                kind: kind.to_owned(),
                content: format!(
                    "Fixture memory {index}: deterministic primer assembly content long \
                     enough to cost real tokens in the budget fill loop."
                ),
                confidence: 0.5 + ((index % 50) as f32) / 100.0,
                utility: 0.4 + ((index % 60) as f32) / 100.0,
                importance: 0.3 + ((index % 70) as f32) / 100.0,
                updated_at: format!("2026-01-{:02}T00:00:00Z", (index % 28) + 1),
                provenance_uri: Some("bench://primer".to_owned()),
                superseded: index % 97 == 96,
                global_lane: false,
            }
        })
        .collect()
}

fn fixture_centrality() -> Vec<PrimerCentralityRow> {
    (0..64)
        .map(|index| PrimerCentralityRow {
            memory_id: format!("mem_{:026}", index * 4 + 3),
            authority: 1.0 - (index as f64) / 64.0,
            betweenness: 0.5,
        })
        .collect()
}

fn bench_settings() -> PrimerSettings {
    PrimerSettings {
        budget_tokens: 600,
        format: PrimerFormat::Markdown,
        config_hash: primer_config_hash(600, true),
        redact_secrets: true,
        global_lane_enabled: false,
    }
}

fn bench_cold_assemble(c: &mut Criterion) {
    let candidates = fixture_candidates();
    let centrality = fixture_centrality();
    let settings = bench_settings();
    let mut group = c.benchmark_group(PRIMER_BENCH_GROUP);
    group.bench_function(PRIMER_COLD_OPERATION, |bencher| {
        bencher.iter(|| {
            let report = assemble_primer(
                black_box(&candidates),
                Some(black_box(&centrality)),
                black_box(&settings),
                7,
            );
            black_box(report.meta.tokens_used)
        });
    });
    group.finish();
}

fn bench_warm_cache_hit(c: &mut Criterion) {
    let connection = ee::db::DbConnection::open_memory().expect("open in-memory db");
    connection.migrate().expect("migrate");
    let workspace_id = "wsp_00000000000000000000000074";
    connection
        .insert_workspace(
            workspace_id,
            &ee::db::CreateWorkspaceInput {
                path: "/bench/primer".to_owned(),
                name: Some("primer-bench".to_owned()),
            },
        )
        .expect("insert workspace");
    for index in 0..32 {
        connection
            .insert_memory(
                &format!("mem_{index:026}"),
                &ee::db::CreateMemoryInput {
                    workspace_id: workspace_id.to_owned(),
                    level: "procedural".to_owned(),
                    kind: "rule".to_owned(),
                    content: format!("Bench rule {index}: warm cache hit fixture content."),
                    workflow_id: None,
                    confidence: 0.8,
                    utility: 0.8,
                    importance: 0.7,
                    provenance_uri: Some("bench://primer".to_owned()),
                    trust_class: "human_explicit".to_owned(),
                    trust_subclass: None,
                    tags: vec!["primer-bench".to_owned()],
                    valid_from: None,
                    valid_to: None,
                },
            )
            .expect("insert memory");
    }
    let settings = bench_settings();
    // Warm the cache once; the bench then measures pure hit latency.
    let warmed = run_primer_with_persistence(&connection, workspace_id, &settings, false, true)
        .expect("warm primer cache");
    assert!(!warmed.cache_hit, "first run must be the cold warm-up");

    let mut group = c.benchmark_group(PRIMER_BENCH_GROUP);
    group.bench_function(PRIMER_WARM_OPERATION, |bencher| {
        bencher.iter(|| {
            let report =
                run_primer_with_persistence(&connection, workspace_id, &settings, false, true)
                    .expect("warm primer run");
            assert!(
                report.cache_hit,
                "bench loop must stay on the cache-hit path"
            );
            black_box(report.meta.tokens_used)
        });
    });
    group.finish();
}

criterion_group!(benches, bench_cold_assemble, bench_warm_cache_hit);
criterion_main!(benches);
