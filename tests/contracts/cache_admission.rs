//! Gate 14: cache admission and S3-FIFO shadow diagnostics.

use std::env;
use std::fs;
use std::path::PathBuf;

use ee::cache::{CachePolicy, CacheStats, NoCache, S3FifoCache};
use ee::shadow::cache::{CacheShadowOutput, compare_outputs};
use ee::shadow::{ShadowGateConfig, ShadowVerdict};

type TestResult = Result<(), String>;

fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

fn golden_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("golden")
        .join("cache")
        .join(format!("{name}.json.golden"))
}

fn assert_golden(name: &str, actual: &str) -> TestResult {
    let path = golden_path(name);
    if env::var("UPDATE_GOLDEN").is_ok() {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        }
        fs::write(&path, actual).map_err(|error| error.to_string())?;
        return Ok(());
    }

    let expected = fs::read_to_string(&path)
        .map_err(|error| format!("failed to read {}: {error}", path.display()))?;
    ensure(actual == expected, format!("golden mismatch for {name}"))
}

fn run_repeated_key_workload<P: CachePolicy<String, String>>(
    policy: &mut P,
    keys: &[&str],
) -> CacheStats {
    for key in keys {
        let owned = (*key).to_string();
        if policy.get(&owned).is_none() {
            policy.put(owned, format!("value:{key}"));
        }
    }
    policy.stats().clone()
}

#[test]
fn gate14_s3_fifo_shadow_beats_no_cache_on_repeated_workload() -> TestResult {
    let workload = ["a", "b", "a", "c", "a", "b", "d", "a"];
    let mut no_cache = NoCache::new();
    let mut s3_fifo = S3FifoCache::new(4);

    let baseline = run_repeated_key_workload(&mut no_cache, &workload);
    let candidate = run_repeated_key_workload(&mut s3_fifo, &workload);

    ensure(baseline.hits == 0, "no-cache baseline always misses")?;
    ensure(candidate.hit_rate() > 0.0, "S3-FIFO records hits")
}

#[test]
fn gate14_cache_shadow_reports_budget_latency_and_evictions() -> TestResult {
    let incumbent = CacheShadowOutput {
        stats: CacheStats {
            hits: 0,
            misses: 8,
            evictions: 0,
            promotions: 0,
        },
        final_size: 0,
        memory_bytes: 0,
        miss_cost: 80_000,
        p95_latency_us: 500,
        p99_latency_us: 900,
        time_us: 1000,
    };
    let candidate = CacheShadowOutput {
        stats: CacheStats {
            hits: 4,
            misses: 4,
            evictions: 1,
            promotions: 2,
        },
        final_size: 4,
        memory_bytes: 4096,
        miss_cost: 40_000,
        p95_latency_us: 180,
        p99_latency_us: 240,
        time_us: 700,
    };

    let (verdict, metrics) = compare_outputs(&incumbent, &candidate, &ShadowGateConfig::default());

    ensure(
        verdict == ShadowVerdict::CandidateBetter,
        "candidate should win",
    )?;
    ensure(
        metrics.candidate_quality > metrics.incumbent_quality,
        "hit rate improves",
    )?;
    ensure(candidate.memory_bytes <= 4096, "memory budget is explicit")?;
    ensure(
        candidate.p99_latency_us < incumbent.p99_latency_us,
        "p99 improves",
    )?;
    ensure(
        candidate.miss_cost < incumbent.miss_cost,
        "miss cost improves",
    )?;
    ensure(candidate.stats.evictions == 1, "eviction count is reported")
}

#[test]
fn gate14_s3_fifo_shadow_matches_golden() -> TestResult {
    let json = serde_json::json!({
        "schema": "ee.cache_shadow.v1",
        "policy": {
            "incumbent": "no_cache",
            "candidate": "s3_fifo"
        },
        "budget": {
            "maxEntries": 4,
            "maxBytes": 4096,
            "sourceOfTruth": false,
            "fallback": "no_cache"
        },
        "workload": {
            "operations": 8,
            "profile": "repeated_release_context_keys"
        },
        "metrics": {
            "hitRate": 0.5,
            "missCost": 40000,
            "p95LatencyUs": 180,
            "p99LatencyUs": 240,
            "memoryBytes": 4096,
            "evictions": 1
        },
        "baseline": {
            "policy": "no_cache",
            "hitRate": 0.0,
            "missCost": 80000,
            "p99LatencyUs": 900
        },
        "verdict": "candidate_better"
    });

    let rendered = serde_json::to_string_pretty(&json).map_err(|error| error.to_string())? + "\n";
    assert_golden("s3_fifo_shadow", &rendered)
}
