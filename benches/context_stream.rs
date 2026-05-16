//! Criterion benchmark for context stream adapter latency (bd-1prrl.1.5).
//!
//! Group name: `context_stream`

#![allow(clippy::expect_used)]

use std::str::FromStr;
use std::time::Instant;

use criterion::{BenchmarkId, Criterion, black_box, criterion_group, criterion_main};
use ee::models::{MemoryId, ProvenanceUri, TrustClass, UnitScore};
use ee::output::streaming::{ContextStreamFrameOptions, context_response_stream_frames};
use ee::pack::{
    ContextRequest, ContextResponse, PackCandidate, PackCandidateInput, PackProvenance,
    PackSection, PackTrustSignal, TokenBudget, assemble_draft,
};
use uuid::Uuid;

const BENCH_GROUP_NAME: &str = "context_stream";
const TIME_TO_FIRST_FRAME_P50_MS_BUDGET: f64 = 10.0;
const TIME_TO_FIRST_ITEM_P50_MS_BUDGET: f64 = 10.0;
const ITEM_COUNTS: &[usize] = &[1, 16, 64];
const QUICK_MEASURE_ITERS: usize = 21;

#[derive(Clone, Copy, Debug)]
struct QuickStats {
    first_frame_p50_ms: f64,
    first_item_p50_ms: f64,
    full_stream_p50_ms: f64,
}

fn memory_id(seed: u128) -> MemoryId {
    MemoryId::from_uuid(Uuid::from_u128(seed))
}

fn unit(value: f32) -> UnitScore {
    UnitScore::parse(value).expect("unit score in range")
}

fn provenance() -> PackProvenance {
    PackProvenance::new(
        ProvenanceUri::from_str("file://benches/context_stream.rs").expect("provenance URI parses"),
        "context stream bench",
    )
    .expect("pack provenance constructs")
}

fn fixture_response(item_count: usize) -> ContextResponse {
    let request =
        ContextRequest::from_query("stream release guardrail").expect("request query accepts");
    let budget = TokenBudget::new(8_000).expect("budget accepts 8000");
    let candidates: Vec<PackCandidate> = (0..item_count)
        .map(|index| {
            PackCandidate::new(PackCandidateInput {
                memory_id: memory_id(0x1000 + index as u128),
                section: PackSection::ProceduralRules,
                content: format!(
                    "Context stream benchmark memory {index}: incremental frame emission should preserve batch order."
                ),
                estimated_tokens: 16,
                relevance: unit((0.95_f32 - (index as f32 * 0.001)).max(0.1)),
                utility: unit(0.7),
                provenance: vec![provenance()],
                why: "benchmark fixture item".to_owned(),
            })
            .expect("candidate constructs")
            .with_trust_signal(PackTrustSignal::new(
                TrustClass::HumanExplicit,
                Some("context-stream-bench".to_owned()),
            ))
        })
        .collect();
    let mut draft =
        assemble_draft(&request.query, budget, candidates).expect("draft assembles for bench");
    draft.hash = Some(format!("blake3:context-stream-bench-{item_count}"));
    ContextResponse::new(request, draft, Vec::new()).expect("context response constructs")
}

fn stream_options(item_count: usize) -> ContextStreamFrameOptions {
    ContextStreamFrameOptions::new(
        format!("pack_stream_bench_{item_count}"),
        "workspace_bench",
        format!("request_bench_{item_count}"),
        "2026-05-16T00:00:00Z",
        "2026-05-16T00:00:01Z",
    )
}

fn run_stream_once(response: &ContextResponse, item_count: usize) -> f64 {
    let start = Instant::now();
    let frames = context_response_stream_frames(response, stream_options(item_count))
        .expect("stream frames generated");
    black_box(frames);
    start.elapsed().as_secs_f64() * 1000.0
}

fn quick_stats(item_count: usize) -> QuickStats {
    let response = fixture_response(item_count);
    let mut first_frame_samples = Vec::with_capacity(QUICK_MEASURE_ITERS);
    let mut first_item_samples = Vec::with_capacity(QUICK_MEASURE_ITERS);
    let mut full_stream_samples = Vec::with_capacity(QUICK_MEASURE_ITERS);

    for _ in 0..QUICK_MEASURE_ITERS {
        let start = Instant::now();
        let frames = context_response_stream_frames(&response, stream_options(item_count))
            .expect("stream frames generated");
        let first_frame_ms = start.elapsed().as_secs_f64() * 1000.0;
        let first_item_ms = if frames.iter().any(|frame| frame.kind() == "item") {
            first_frame_ms
        } else {
            start.elapsed().as_secs_f64() * 1000.0
        };
        let full_stream_ms = start.elapsed().as_secs_f64() * 1000.0;
        black_box(frames);
        first_frame_samples.push(first_frame_ms);
        first_item_samples.push(first_item_ms);
        full_stream_samples.push(full_stream_ms);
    }

    first_frame_samples.sort_by(|left, right| left.total_cmp(right));
    first_item_samples.sort_by(|left, right| left.total_cmp(right));
    full_stream_samples.sort_by(|left, right| left.total_cmp(right));
    QuickStats {
        first_frame_p50_ms: percentile(&first_frame_samples, 0.50),
        first_item_p50_ms: percentile(&first_item_samples, 0.50),
        full_stream_p50_ms: percentile(&full_stream_samples, 0.50),
    }
}

fn percentile(sorted_samples: &[f64], percentile: f64) -> f64 {
    let last_index = sorted_samples.len() - 1;
    sorted_samples[(percentile * last_index as f64).round() as usize]
}

fn compare_only_mode_enabled() -> bool {
    std::env::var("EE_BENCH_COMPARE_ONLY")
        .map(|value| value == "1" || value.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

fn assert_latency_budget(stats: QuickStats) {
    assert!(
        stats.first_frame_p50_ms <= TIME_TO_FIRST_FRAME_P50_MS_BUDGET,
        "context stream first-frame p50 above budget: {:.3}ms > {:.3}ms",
        stats.first_frame_p50_ms,
        TIME_TO_FIRST_FRAME_P50_MS_BUDGET
    );
    assert!(
        stats.first_item_p50_ms <= TIME_TO_FIRST_ITEM_P50_MS_BUDGET,
        "context stream first-item p50 above budget: {:.3}ms > {:.3}ms",
        stats.first_item_p50_ms,
        TIME_TO_FIRST_ITEM_P50_MS_BUDGET
    );
    assert!(
        stats.full_stream_p50_ms.is_finite(),
        "context stream full-stream p50 must be finite"
    );
}

fn bench_context_stream(c: &mut Criterion) {
    if compare_only_mode_enabled() {
        assert_latency_budget(quick_stats(16));
        return;
    }

    let mut group = c.benchmark_group(BENCH_GROUP_NAME);
    group.sample_size(10);
    for &item_count in ITEM_COUNTS {
        let response = fixture_response(item_count);
        group.bench_with_input(
            BenchmarkId::new("batch_adapter_full_stream", format!("{item_count}_items")),
            &response,
            |b, response| b.iter(|| black_box(run_stream_once(response, item_count))),
        );
    }
    group.finish();
}

criterion_group!(benches, bench_context_stream);
criterion_main!(benches);

#[cfg(test)]
mod tests {
    #[test]
    fn benchmark_group_name_is_canonical() {
        assert_eq!(super::BENCH_GROUP_NAME, "context_stream");
    }

    #[test]
    fn first_frame_budget_matches_stream_acceptance() {
        assert!((super::TIME_TO_FIRST_FRAME_P50_MS_BUDGET - 10.0).abs() < f64::EPSILON);
    }

    #[test]
    fn first_item_budget_matches_stream_acceptance() {
        assert!((super::TIME_TO_FIRST_ITEM_P50_MS_BUDGET - 10.0).abs() < f64::EPSILON);
    }
}
