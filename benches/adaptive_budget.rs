//! Adaptive context-pack budget classifier benchmark (bd-1prrl.4).
//!
//! Exercises `classify_adaptive_budget` on representative retrieval
//! distributions and query shapes to keep the per-call cost well under the
//! 2 ms p50 ceiling the swarmx.7 acceptance asks for. The classifier is a
//! pure function over `&[f32]` retrieval scores plus the query text, so
//! the bench stays deterministic and host-portable.

use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Instant;

use criterion::{BenchmarkId, Criterion, black_box};
use ee::pack::budget_classifier::{
    AdaptiveBudgetInput, RETRIEVAL_ENTROPY_SAMPLE_LIMIT, classify_adaptive_budget,
};
use serde::Serialize;

const BENCH_GROUP_NAME: &str = "adaptive_budget";
const PERCENTILE_SUMMARY_RELATIVE_PATH: &str = "criterion/adaptive_budget/percentiles.json";
const PERCENTILE_WARMUP_ITERS: usize = 32;
const PERCENTILE_MEASURE_ITERS: usize = 401;
const PERCENTILE_BATCH_ITERS: usize = 128;
const P50_BUDGET_MS: f64 = 2.0;
const P99_TO_P50_MAX_RATIO: f64 = 2.0;

struct Scenario {
    label: &'static str,
    query: &'static str,
    scores: Vec<f32>,
    fanout: f64,
}

impl Scenario {
    fn input(&self) -> AdaptiveBudgetInput<'_> {
        AdaptiveBudgetInput::new(self.query, self.scores.as_slice(), self.fanout)
            .with_max_tokens(8_000)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
struct AdaptiveBudgetPercentileSummary {
    schema: &'static str,
    group: &'static str,
    warmup_iterations: usize,
    measure_iterations: usize,
    batch_iterations_per_sample: usize,
    p50_budget_ms: f64,
    p99_to_p50_max_ratio: f64,
    samples: Vec<AdaptiveBudgetPercentileSample>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
struct AdaptiveBudgetPercentileSample {
    label: &'static str,
    retrieval_score_count: usize,
    graph_fanout: f64,
    p50_ms: f64,
    p95_ms: f64,
    p99_ms: f64,
    p99_to_p50_ratio: f64,
    budget_status: &'static str,
}

fn skewed_scores(len: usize) -> Vec<f32> {
    (0..len).map(|i| 1.0_f32 / ((i as f32) + 1.0)).collect()
}

fn uniform_scores(len: usize) -> Vec<f32> {
    vec![0.75_f32; len]
}

fn scenarios() -> Vec<Scenario> {
    vec![
        Scenario {
            label: "trivial_lookup_empty_retrieval",
            query: "show memory mem_release_policy",
            scores: Vec::new(),
            fanout: 0.0,
        },
        Scenario {
            label: "balanced_small_retrieval",
            query: "context for release workflow",
            scores: skewed_scores(8),
            fanout: 1.0,
        },
        Scenario {
            label: "complex_keyword_uniform_topk",
            query: "audit refactor migrate security performance",
            scores: uniform_scores(RETRIEVAL_ENTROPY_SAMPLE_LIMIT),
            fanout: 2.5,
        },
        Scenario {
            label: "fanout_just_below_cap",
            query: "diagnose hardening regression in retrieval ranking",
            scores: skewed_scores(RETRIEVAL_ENTROPY_SAMPLE_LIMIT),
            fanout: 2.9,
        },
        Scenario {
            label: "fanout_at_cap",
            query: "diagnose hardening regression in retrieval ranking",
            scores: skewed_scores(RETRIEVAL_ENTROPY_SAMPLE_LIMIT),
            fanout: 3.0,
        },
        Scenario {
            label: "fanout_just_above_cap",
            query: "diagnose hardening regression in retrieval ranking",
            scores: skewed_scores(RETRIEVAL_ENTROPY_SAMPLE_LIMIT),
            fanout: 3.1,
        },
        Scenario {
            label: "retrieval_scores_at_1000_elements",
            query: "audit refactor migrate security performance",
            scores: skewed_scores(1_000),
            fanout: 2.5,
        },
    ]
}

fn bench_adaptive_budget(c: &mut Criterion) {
    let mut group = c.benchmark_group(BENCH_GROUP_NAME);
    let scenarios = scenarios();
    let inputs = scenarios
        .iter()
        .map(Scenario::input)
        .collect::<Vec<AdaptiveBudgetInput<'_>>>();
    for (scenario, input) in scenarios.iter().zip(inputs.iter()) {
        group.bench_with_input(
            BenchmarkId::new("classify", scenario.label),
            input,
            |b, input| {
                b.iter(|| black_box(classify_prebuilt_input(black_box(input))));
            },
        );
    }
    group.finish();
}

fn classify_prebuilt_input(
    input: &AdaptiveBudgetInput<'_>,
) -> ee::pack::budget_classifier::AdaptiveBudgetDecision {
    classify_adaptive_budget(
        AdaptiveBudgetInput::new(input.query, input.retrieval_scores, input.graph_fanout)
            .with_max_tokens(input.max_tokens),
    )
}

fn measure_percentiles() -> AdaptiveBudgetPercentileSummary {
    let mut samples = Vec::new();
    for scenario in scenarios() {
        let input = scenario.input();
        for _ in 0..PERCENTILE_WARMUP_ITERS {
            black_box(classify_prebuilt_input(black_box(&input)));
        }

        let mut elapsed_ms = Vec::with_capacity(PERCENTILE_MEASURE_ITERS);
        for _ in 0..PERCENTILE_MEASURE_ITERS {
            let start = Instant::now();
            for _ in 0..PERCENTILE_BATCH_ITERS {
                black_box(classify_prebuilt_input(black_box(&input)));
            }
            let elapsed = start.elapsed();
            elapsed_ms.push(elapsed.as_secs_f64() * 1_000.0 / PERCENTILE_BATCH_ITERS as f64);
        }
        elapsed_ms.sort_by(f64::total_cmp);

        let p50_ms = percentile(&elapsed_ms, 0.50);
        let p95_ms = percentile(&elapsed_ms, 0.95);
        let p99_ms = percentile(&elapsed_ms, 0.99);
        let p99_to_p50_ratio = if p50_ms > 0.0 {
            p99_ms / p50_ms
        } else {
            f64::INFINITY
        };
        let budget_status = if p50_ms <= P50_BUDGET_MS && p99_to_p50_ratio <= P99_TO_P50_MAX_RATIO {
            "pass"
        } else {
            "fail"
        };

        samples.push(AdaptiveBudgetPercentileSample {
            label: scenario.label,
            retrieval_score_count: scenario.scores.len(),
            graph_fanout: scenario.fanout,
            p50_ms,
            p95_ms,
            p99_ms,
            p99_to_p50_ratio,
            budget_status,
        });
    }

    AdaptiveBudgetPercentileSummary {
        schema: "ee.bench.adaptive_budget.percentiles.v1",
        group: BENCH_GROUP_NAME,
        warmup_iterations: PERCENTILE_WARMUP_ITERS,
        measure_iterations: PERCENTILE_MEASURE_ITERS,
        batch_iterations_per_sample: PERCENTILE_BATCH_ITERS,
        p50_budget_ms: P50_BUDGET_MS,
        p99_to_p50_max_ratio: P99_TO_P50_MAX_RATIO,
        samples,
    }
}

fn percentile(sorted_values: &[f64], percentile: f64) -> f64 {
    if sorted_values.is_empty() {
        return 0.0;
    }
    let upper = sorted_values.len() - 1;
    let rank = ((upper as f64) * percentile).round() as usize;
    sorted_values[rank.min(upper)]
}

fn target_dir() -> PathBuf {
    env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("target"))
}

fn percentile_summary_path() -> PathBuf {
    target_dir().join(PERCENTILE_SUMMARY_RELATIVE_PATH)
}

fn measure_and_write_percentiles(print_json: bool) -> Result<(), String> {
    let summary = measure_percentiles();
    let path = percentile_summary_path();
    let parent = path
        .parent()
        .ok_or_else(|| format!("summary path has no parent: {}", path.display()))?;
    fs::create_dir_all(parent).map_err(|error| format!("create {}: {error}", parent.display()))?;
    let json = serde_json::to_string_pretty(&summary)
        .map_err(|error| format!("serialize percentile summary: {error}"))?;
    fs::write(&path, format!("{json}\n"))
        .map_err(|error| format!("write {}: {error}", path.display()))?;
    if print_json {
        println!("{json}");
    }
    if summary
        .samples
        .iter()
        .any(|sample| sample.budget_status != "pass")
    {
        return Err(format!(
            "adaptive budget percentile gate failed; report: {}",
            path.display()
        ));
    }
    Ok(())
}

fn run_criterion_mode() {
    let mut criterion = Criterion::default().configure_from_args();
    bench_adaptive_budget(&mut criterion);
    criterion.final_summary();
}

fn percentile_mode_enabled() -> bool {
    matches!(
        env::var("EE_ADAPTIVE_BUDGET_PERCENTILES").as_deref(),
        Ok("1" | "true" | "TRUE" | "yes" | "YES")
    )
}

fn main() -> ExitCode {
    let args = env::args().skip(1).collect::<Vec<_>>();
    let percentiles_only = percentile_mode_enabled()
        || args
            .iter()
            .any(|arg| arg == "--percentiles" || arg == "--quick");
    let print_json = args.iter().any(|arg| arg == "--summary-json");

    if percentiles_only {
        return match measure_and_write_percentiles(print_json) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("error: {error}");
                ExitCode::from(1)
            }
        };
    }

    if let Err(error) = measure_and_write_percentiles(false) {
        eprintln!(
            "warning: adaptive-budget percentile summary was not written before Criterion run: {error}"
        );
    }

    run_criterion_mode();
    ExitCode::SUCCESS
}
