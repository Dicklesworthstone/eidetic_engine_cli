use serde_json::Value;
use toml_edit::{DocumentMut, Item};

type TestResult<T = ()> = Result<T, String>;

const CONTEXT_BENCH: &str = include_str!("../benches/context.rs");
const BENCH_SCRIPT: &str = include_str!("../scripts/bench.sh");
const E2E_SCRIPT: &str = include_str!("../scripts/e2e_overhaul/pack_cache_l2.sh");
const ZSTD_DICTIONARY_E2E_SCRIPT: &str =
    include_str!("../scripts/e2e_overhaul/zstd_pack_dictionary.sh");
const BUDGETS_TOML: &str = include_str!("../benches/budgets.toml");
const PERF_BASELINE: &str = include_str!("../benches/baselines/perf_v0_2.json");

fn budgets_manifest() -> TestResult<DocumentMut> {
    BUDGETS_TOML
        .parse::<DocumentMut>()
        .map_err(|error| format!("invalid benches/budgets.toml: {error}"))
}

fn toml_f64(value: &Item, key: &str) -> TestResult<f64> {
    let field = value
        .get(key)
        .and_then(Item::as_value)
        .ok_or_else(|| format!("missing TOML scalar `{key}`"))?;
    field
        .as_float()
        .or_else(|| field.as_integer().map(|integer| integer as f64))
        .ok_or_else(|| format!("TOML scalar `{key}` must be numeric"))
}

#[test]
fn l2_warm_context_cache_benchmark_is_registered() -> TestResult {
    for expected in [
        "L2_WARM_BENCH_GROUP: &str = \"ee_context_pack_l2_warm\"",
        "L2_WARM_BENCH_OPERATION: &str = \"ee_context_pack_l2_warm\"",
        "L2_CONCURRENT_IDENTICAL_REQUESTS: usize = 4",
        "L2_EXPECTED_FRESH_ASSEMBLIES: usize = 1",
        "L2_EXPECTED_WARM_HITS: usize =",
        "PackL2Cache",
        "seed_l2_warm_cache",
        "bench_context_l2_warm_cache",
        "criterion_group!",
    ] {
        if !CONTEXT_BENCH.contains(expected) {
            return Err(format!("benches/context.rs missing `{expected}`"));
        }
    }

    for expected in [
        "run_context_l2_warm_bench",
        "cargo bench --bench \"$bench\" -- ee_context_pack_l2_warm",
        "append_measured_ms \"$key\"",
        "if [ \"$PROFILE\" != \"ci-smoke\" ] &&",
        "run_context_l2_warm_bench",
    ] {
        if !BENCH_SCRIPT.contains(expected) {
            return Err(format!("scripts/bench.sh missing `{expected}`"));
        }
    }

    Ok(())
}

#[test]
fn zstd_pack_dictionary_benchmark_is_registered() -> TestResult {
    for expected in [
        "ZSTD_PACK_DICTIONARY_BENCH_GROUP: &str = \"ee_context_zstd_pack_dictionary\"",
        "ZSTD_PACK_DICTIONARY_OPERATION: &str = \"ee_context_zstd_pack_dictionary_l2\"",
        "ZSTD_PACK_DICTIONARY_SAMPLE_COUNT: usize = 96",
        "train_pack_compression_dictionary",
        "PackL2CompressionDictionary",
        "seed_zstd_pack_dictionary_cache",
        "bench_context_zstd_pack_dictionary",
        "zstd_pack_dictionary_benchmark_contract_matches_e2e_gate",
        "criterion_group!",
    ] {
        if !CONTEXT_BENCH.contains(expected) {
            return Err(format!("benches/context.rs missing `{expected}`"));
        }
    }

    Ok(())
}

#[test]
fn arena_workspace_reuse_context_benchmark_is_registered() -> TestResult {
    for expected in [
        "ARENA_MODE_BENCH_GROUP: &str = \"ee_context_arena_mode\"",
        "ARENA_MODE_BENCH_OPERATION: &str = \"ee_context_arena_workspace_reuse\"",
        "ARENA_MODE_EXPECTED_WORKSPACE_FRESH_ALLOCATIONS: u64 = 1",
        "PackArenaWorkspace",
        "PackArenaWorkspaceKey",
        "bench_context_arena_mode",
        "ArenaMode::WorkspaceReuse",
        "criterion_group!",
    ] {
        if !CONTEXT_BENCH.contains(expected) {
            return Err(format!("benches/context.rs missing `{expected}`"));
        }
    }

    for expected in [
        "run_context_arena_mode_bench",
        "cargo bench --bench \"$bench\" -- ee_context_arena_mode",
        "append_result \"$key\" \"measured\"",
        "run_context_arena_mode_bench",
    ] {
        if !BENCH_SCRIPT.contains(expected) {
            return Err(format!("scripts/bench.sh missing `{expected}`"));
        }
    }

    Ok(())
}

#[test]
fn l2_pack_cache_e2e_harness_covers_runtime_contract() -> TestResult {
    for expected in [
        "pack_cache_l2",
        "EE_L2_PACK_CACHE_DIR",
        "EE_L2_PACK_CACHE_BYTES",
        "SEED_JSON_PATH",
        "for index in 1 2 3",
        "cmp -s \"$SEED_JSON_PATH\" \"$HIT_PATH\"",
        "l2_pack_cache_corruption",
        "l2_pack_cache_unavailable",
        "pack_cache_l2_summary",
    ] {
        if !E2E_SCRIPT.contains(expected) {
            return Err(format!(
                "scripts/e2e_overhaul/pack_cache_l2.sh missing `{expected}`"
            ));
        }
    }

    Ok(())
}

#[test]
fn zstd_pack_dictionary_e2e_harness_covers_dictionary_contract() -> TestResult {
    for expected in [
        "zstd_pack_dictionary",
        "EE_L2_PACK_CACHE_DIR",
        "zstd --train",
        "ee.pack.l2_cache.entry.v2",
        "dictionaryId",
        "compressedByteLen",
        "uncompressedByteLen",
        "compression_dictionary_missing",
        "compression_dictionary_corrupt",
        "l2_pack_cache_corruption",
        "zstd_pack_dictionary_perf_proof",
        "pack_hash_unchanged",
        "ledger_hash_unchanged",
        "rendered_json_unchanged",
        "markdown_output_unchanged",
        "missing_dictionary_fallback_specific",
        "corrupt_dictionary_fallback_specific",
    ] {
        if !ZSTD_DICTIONARY_E2E_SCRIPT.contains(expected) {
            return Err(format!(
                "scripts/e2e_overhaul/zstd_pack_dictionary.sh missing `{expected}`"
            ));
        }
    }

    Ok(())
}

#[test]
fn l2_warm_context_cache_budget_and_baseline_are_present() -> TestResult {
    let budgets = budgets_manifest()?;
    let profiles = budgets
        .get("profiles")
        .and_then(Item::as_table)
        .ok_or_else(|| "budgets manifest missing [profiles] table".to_owned())?;
    for profile in ["nightly", "stress"] {
        let benches = profiles
            .get(profile)
            .and_then(|item| item.get("benches"))
            .and_then(Item::as_value)
            .and_then(|value| value.as_array())
            .ok_or_else(|| format!("profile `{profile}` missing benches array"))?;
        if !benches
            .iter()
            .any(|entry| entry.as_str() == Some("context_l2_warm"))
        {
            return Err(format!(
                "profile `{profile}` must include context_l2_warm in the perf gate"
            ));
        }
    }

    let operations = budgets
        .get("operations")
        .and_then(Item::as_table)
        .ok_or_else(|| "budgets manifest missing [operations] table".to_owned())?;
    let operation = operations
        .get("ee_context_pack_l2_warm")
        .ok_or_else(|| "budgets manifest missing ee_context_pack_l2_warm".to_owned())?;
    let p50 = toml_f64(operation, "p50_ms_max")?;
    let p99 = toml_f64(operation, "p99_ms_max")?;
    if !(p50 > 0.0 && p99 >= p50) {
        return Err(format!(
            "L2 warm cache budget must be positive and monotonic, got p50={p50}, p99={p99}"
        ));
    }

    let baseline: Value = serde_json::from_str(PERF_BASELINE)
        .map_err(|error| format!("invalid perf_v0_2 baseline JSON: {error}"))?;
    let operation = baseline
        .pointer("/operations/ee_context_pack_l2_warm")
        .ok_or_else(|| "perf baseline missing operations.ee_context_pack_l2_warm".to_owned())?;
    for field in [
        "p50_ms",
        "p99_ms",
        "tolerance_pct_p50",
        "tolerance_pct_p99",
        "unstable",
    ] {
        if operation.get(field).is_none() {
            return Err(format!("L2 warm cache baseline missing `{field}`"));
        }
    }
    for scale in ["warm_hit_json", "4_concurrent_identical_requests"] {
        if operation.pointer(&format!("/scales/{scale}")).is_none() {
            return Err(format!("L2 warm cache baseline missing scale `{scale}`"));
        }
    }

    Ok(())
}

#[test]
fn arena_workspace_reuse_budget_and_baseline_are_present() -> TestResult {
    let budgets = budgets_manifest()?;
    let profiles = budgets
        .get("profiles")
        .and_then(Item::as_table)
        .ok_or_else(|| "budgets manifest missing [profiles] table".to_owned())?;
    for profile in ["nightly", "stress"] {
        let benches = profiles
            .get(profile)
            .and_then(|item| item.get("benches"))
            .and_then(Item::as_value)
            .and_then(|value| value.as_array())
            .ok_or_else(|| format!("profile `{profile}` missing benches array"))?;
        if !benches
            .iter()
            .any(|entry| entry.as_str() == Some("context_arena_mode"))
        {
            return Err(format!(
                "profile `{profile}` must include context_arena_mode in the perf gate"
            ));
        }
    }

    let operations = budgets
        .get("operations")
        .and_then(Item::as_table)
        .ok_or_else(|| "budgets manifest missing [operations] table".to_owned())?;
    let operation = operations
        .get("ee_context_arena_workspace_reuse")
        .ok_or_else(|| "budgets manifest missing ee_context_arena_workspace_reuse".to_owned())?;
    let p50 = toml_f64(operation, "p50_ms_max")?;
    let p99 = toml_f64(operation, "p99_ms_max")?;
    if !(p50 > 0.0 && p99 >= p50) {
        return Err(format!(
            "arena workspace reuse budget must be positive and monotonic, got p50={p50}, p99={p99}"
        ));
    }

    let baseline: Value = serde_json::from_str(PERF_BASELINE)
        .map_err(|error| format!("invalid perf_v0_2 baseline JSON: {error}"))?;
    let operation = baseline
        .pointer("/operations/ee_context_arena_workspace_reuse")
        .ok_or_else(|| {
            "perf baseline missing operations.ee_context_arena_workspace_reuse".to_owned()
        })?;
    for field in [
        "p50_ms",
        "p99_ms",
        "tolerance_pct_p50",
        "tolerance_pct_p99",
        "unstable",
    ] {
        if operation.get(field).is_none() {
            return Err(format!("arena workspace reuse baseline missing `{field}`"));
        }
    }
    for scale in ["fixture_coverage_fill", "fixture_provenance_heavy"] {
        if operation.pointer(&format!("/scales/{scale}")).is_none() {
            return Err(format!(
                "arena workspace reuse baseline missing scale `{scale}`"
            ));
        }
    }

    Ok(())
}
