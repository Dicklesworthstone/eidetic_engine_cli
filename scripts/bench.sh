#!/bin/sh
set -eu

# Performance benchmark runner for ee (eidetic_engine_cli-htjd, fcq1.2)
#
# Runs criterion benchmarks and produces an ee.perf.v1 JSON artifact.
# Compares results against benches/budgets.toml thresholds.
#
# Usage:
#   ./scripts/bench.sh --profile ci-smoke --json
#   ./scripts/bench.sh --profile daemon-search-slo --json
#   ./scripts/bench.sh --profile nightly
#   ./scripts/bench.sh --profile stress --check-regression
#   ./scripts/bench.sh --profile auto_enroll --json
#   ./scripts/bench.sh --profile auto_enroll_idle_24h --json
#   ./scripts/bench_pack_regression.sh
#   ./scripts/bench.sh --quick            # Alias for --profile ci-smoke
#
# Environment:
#   CARGO_TARGET_DIR       Build directory. For RCH use:
#                          CARGO_TARGET_DIR=/Volumes/USBNVME16TB/temp_agent_space/cargo-target
#   EE_BENCH_ARTIFACT_DIR  Directory for JSON artifacts.
#   EE_BENCH_OUTPUT        Output path for JSON artifact.

PROFILE="nightly"
JSON_OUTPUT=false
CHECK_REGRESSION=false
LIST_PROFILES=false
AUTO_ENROLL_BASELINE_ONLY=false
BUDGET_MODE="advisory"

usage() {
    sed -n '3,18p' "$0" | sed 's/^# //' | sed 's/^#//'
}

while [ "$#" -gt 0 ]; do
    case "$1" in
        --quick) PROFILE="ci-smoke" ;;
        --profile)
            shift
            if [ "$#" -eq 0 ]; then
                echo "Missing value for --profile" >&2
                exit 1
            fi
            PROFILE="$1"
            ;;
        --profile=*) PROFILE="${1#--profile=}" ;;
        --list-profiles) LIST_PROFILES=true ;;
        --json) JSON_OUTPUT=true ;;
        --check-regression) CHECK_REGRESSION=true ;;
        --help|-h)
            usage
            exit 0
            ;;
        *)
            echo "Unknown argument: $1" >&2
            exit 1
            ;;
    esac
    shift
done

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_ROOT="$(dirname "$SCRIPT_DIR")"
DEFAULT_AGENT_BUILD_ROOT="/Volumes/USBNVME16TB/temp_agent_space"
BUDGETS_FILE="$PROJECT_ROOT/benches/budgets.toml"
BASELINE_FILE="${EE_BENCH_BASELINE_FILE:-$PROJECT_ROOT/benches/baselines/v0.1.json}"
WORKLOAD_FILE="$PROJECT_ROOT/tests/fixtures/swarm_scale/workloads.json"
if [ -d "$DEFAULT_AGENT_BUILD_ROOT" ]; then
    mkdir -p "$DEFAULT_AGENT_BUILD_ROOT/cargo-target" "$DEFAULT_AGENT_BUILD_ROOT/tmp" 2>/dev/null || true
    export TMPDIR="${EE_AGENT_TMPDIR:-$DEFAULT_AGENT_BUILD_ROOT/tmp}"
fi
if [ -n "${CARGO_TARGET_DIR:-}" ]; then
    TARGET_ROOT="$CARGO_TARGET_DIR"
elif [ -d "$DEFAULT_AGENT_BUILD_ROOT" ]; then
    TARGET_ROOT="$DEFAULT_AGENT_BUILD_ROOT/cargo-target"
else
    TARGET_ROOT="${TMPDIR:-/tmp}/rch_target_ee_bench"
fi
CRITERION_DIR="$TARGET_ROOT/criterion"
ARTIFACT_DIR="${EE_BENCH_ARTIFACT_DIR:-$TARGET_ROOT/ee-bench}"
OUTPUT_FILE="${EE_BENCH_OUTPUT:-$ARTIFACT_DIR/ee-perf.v1.json}"
EE_BIN="$TARGET_ROOT/release/ee"
export CARGO_TARGET_DIR="$TARGET_ROOT"
export EE_BENCH_PROFILE="$PROFILE"
export EE_BENCH_EE_BIN="$EE_BIN"

if [ ! -f "$BUDGETS_FILE" ]; then
    echo "Error: budgets.toml not found at $BUDGETS_FILE" >&2
    exit 1
fi

if [ "$LIST_PROFILES" = "true" ]; then
    printf '%s\n' "ci-smoke" "daemon-search-slo" "nightly" "stress" "auto_enroll" "auto_enroll_idle_24h"
    exit 0
fi

case "$PROFILE" in
    ci-smoke)
        BENCHMARKS="status"
        BENCH_ARGS=""
        PROFILE_CLASS="normal_ci"
        WORKLOAD_TIER="small"
        RELEASE_BLOCKING=false
        ;;
    daemon-search-slo)
        BENCHMARKS=""
        BENCH_ARGS=""
        PROFILE_CLASS="stable_self_hosted_linux_x64"
        WORKLOAD_TIER="10k_search"
        RELEASE_BLOCKING=true
        BUDGET_MODE="hard"
        ;;
    nightly)
        BENCHMARKS="remember search context tiered_recall pack_size why outcome status workspace_init audit_query index_rebuild concurrent_writes import_cass link graph_pagerank graph_ppr graph_louvain graph_ktruss graph_gomory_hu graph_hits graph_full_stack curate_candidates graph_refresh_cooperative"
        BENCH_ARGS="--warm-up-time 0.5 --measurement-time 2 --sample-size 20"
        PROFILE_CLASS="nightly_ci"
        WORKLOAD_TIER="medium"
        RELEASE_BLOCKING=false
        ;;
    stress)
        BENCHMARKS="remember search context tiered_recall pack_size why outcome status workspace_init audit_query index_rebuild concurrent_writes import_cass link graph_pagerank graph_ppr graph_louvain graph_ktruss graph_gomory_hu graph_hits graph_full_stack curate_candidates graph_refresh_cooperative"
        BENCH_ARGS=""
        PROFILE_CLASS="local_256gb"
        WORKLOAD_TIER="stress"
        RELEASE_BLOCKING=false
        ;;
    auto_enroll)
        BENCHMARKS=""
        BENCH_ARGS=""
        PROFILE_CLASS="mac-m3-pro"
        WORKLOAD_TIER="auto_enroll"
        RELEASE_BLOCKING=false
        AUTO_ENROLL_BASELINE_ONLY=true
        BASELINE_FILE="${EE_BENCH_BASELINE_FILE:-$PROJECT_ROOT/benches/baselines/auto_enroll_perf_v0.json}"
        ;;
    auto_enroll_idle_24h)
        BENCHMARKS=""
        BENCH_ARGS=""
        PROFILE_CLASS="mac-m3-pro"
        WORKLOAD_TIER="auto_enroll_idle_24h"
        RELEASE_BLOCKING=false
        AUTO_ENROLL_BASELINE_ONLY=true
        BASELINE_FILE="${EE_BENCH_BASELINE_FILE:-$PROJECT_ROOT/benches/baselines/auto_enroll_perf_v0.json}"
        ;;
    *)
        echo "Unknown benchmark profile: $PROFILE" >&2
        echo "Known profiles: ci-smoke, daemon-search-slo, nightly, stress, auto_enroll, auto_enroll_idle_24h" >&2
        exit 1
        ;;
esac

mkdir -p "$ARTIFACT_DIR"

echo "=== EE Performance Benchmarks ===" >&2
echo "profile: $PROFILE ($PROFILE_CLASS)" >&2
echo "target: $TARGET_ROOT" >&2
echo "artifacts: $ARTIFACT_DIR" >&2
echo "workload tier: $WORKLOAD_TIER" >&2

# Build benchmarks first. The auto-enroll profiles are contract profiles:
# they emit the checked baseline rows and leave real measurement to the
# RCH-run e2e harness that produces a candidate report.
if [ "$AUTO_ENROLL_BASELINE_ONLY" = "true" ]; then
    echo "[*] Loading auto-enroll performance baseline contract..." >&2
else
    echo "[*] Building benchmarks..." >&2
    case "$PROFILE" in
        ci-smoke)
            cargo build --release --bench status >&2
            ;;
        daemon-search-slo)
            cargo build --release --bench daemon_round_trip >&2
            cargo build --release --bin ee >&2
            ;;
        *)
            cargo build --release --benches >&2
            cargo build --release --bin ee >&2
            ;;
    esac

    if [ "$PROFILE" = "ci-smoke" ]; then
        echo "[*] Running ci-smoke benchmark profile..." >&2
    else
        echo "[*] Running $PROFILE benchmark profile..." >&2
    fi
fi

# Collect results
RESULTS=""
TIMESTAMP=$(date -u +"%Y-%m-%dT%H:%M:%SZ")
FAILED=false

append_result() {
    key="$1"
    status="$2"
    p50_ms="$3"
    p95_ms="$4"
    p99_ms="$5"
    max_ms="$6"
    rows_per_sec="$7"
    regression_status="${8:-not_checked}"
    allocation_count="${9:-null}"

    if [ "$PROFILE" = "daemon-search-slo" ]; then
        baseline_ref=null
    else
        baseline_ref="{\"file\":\"$BASELINE_FILE\",\"operation\":\"$key\"}"
    fi

    if [ -n "$RESULTS" ]; then
        RESULTS="$RESULTS,"
    fi

    RESULTS="$RESULTS
    \"$key\": {
      \"status\": \"$status\",
      \"profile\": \"$PROFILE\",
      \"workload_tier\": \"$WORKLOAD_TIER\",
      \"p50_ms\": $p50_ms,
      \"p95_ms\": $p95_ms,
      \"p99_ms\": $p99_ms,
      \"samples_count\": null,
      \"max_ms\": $max_ms,
      \"max_rss_kb\": null,
      \"allocation_count\": $allocation_count,
      \"db_size_bytes\": null,
      \"index_size_bytes\": null,
      \"rows_per_sec\": $rows_per_sec,
      \"regression_status\": \"$regression_status\",
      \"baseline_ref\": $baseline_ref,
      \"budget_mode\": \"$BUDGET_MODE\"
    }"

    append_bench_iteration_event "$key" "$status" "$p50_ms" "$p95_ms" "$p99_ms" "$max_ms" "$rows_per_sec" "$regression_status"
}

append_bench_iteration_event() {
    key="$1"
    status="$2"
    p50_ms="$3"
    p95_ms="$4"
    p99_ms="$5"
    max_ms="$6"
    rows_per_sec="$7"
    regression_status="$8"

    if [ -z "${EE_TEST_LOG_PATH:-}" ] || ! command -v jq >/dev/null 2>&1; then
        return 0
    fi

    log_dir=$(dirname "$EE_TEST_LOG_PATH")
    mkdir -p "$log_dir"
    ts=$(date -u +"%Y-%m-%dT%H:%M:%SZ")
    test_id="${EE_TEST_LOG_TEST_ID:-bench_$PROFILE}"

    jq -cn \
        --arg ts "$ts" \
        --arg test_id "$test_id" \
        --arg operation "$key" \
        --arg status "$status" \
        --arg profile "$PROFILE" \
        --arg workload_tier "$WORKLOAD_TIER" \
        --arg p50_ms "$p50_ms" \
        --arg p95_ms "$p95_ms" \
        --arg p99_ms "$p99_ms" \
        --arg max_ms "$max_ms" \
        --arg rows_per_sec "$rows_per_sec" \
        --arg regression_status "$regression_status" \
        'def maybe_number($value):
            if $value == "" or $value == "null" then null else ($value | tonumber) end;
          {
            schema: "ee.test_event.v1",
            ts: $ts,
            test_id: $test_id,
            kind: "bench_iteration",
            fields: {
              operation: $operation,
              status: $status,
              profile: $profile,
              workload_tier: $workload_tier,
              p50_ms: maybe_number($p50_ms),
              p95_ms: maybe_number($p95_ms),
              p99_ms: maybe_number($p99_ms),
              max_ms: maybe_number($max_ms),
              rows_per_sec: maybe_number($rows_per_sec),
              regression_status: $regression_status
            }
          }' >>"$EE_TEST_LOG_PATH"
}

append_measured_ms() {
    key="$1"
    elapsed_ms="$2"
    regression_status=$(budget_status "$key" "$elapsed_ms")
    append_result "$key" "measured" "$elapsed_ms" "$elapsed_ms" "$elapsed_ms" "$elapsed_ms" null "$regression_status"
}

append_smoke_failure() {
    key="$1"
    append_result "$key" "failed" null null null null null
}

to_ms() {
    value="$1"
    unit="$2"
    awk -v value="$value" -v unit="$unit" 'BEGIN {
        if (value == "" || value == "null") {
            print "null";
        } else if (unit == "ns") {
            printf "%.6f", value / 1000000;
        } else if (unit == "us" || unit == "µs") {
            printf "%.6f", value / 1000;
        } else if (unit == "s") {
            printf "%.6f", value * 1000;
        } else {
            printf "%.6f", value;
        }
    }'
}

parse_time_value() {
    output="$1"
    printf '%s\n' "$output" \
        | sed -n 's/.*time:[[:space:]]*\[[[:space:]]*\([0-9.][0-9.]*\)[[:space:]]*\([[:alpha:]µ]*\).*/\1 \2/p' \
        | sed -n '1p'
}

budget_status() {
    key="$1"
    elapsed_ms="$2"

    case "$elapsed_ms" in
        ""|null) printf '%s\n' "not_available"; return ;;
    esac

    if ! command -v jq >/dev/null 2>&1 || [ ! -f "$BASELINE_FILE" ]; then
        printf '%s\n' "not_checked"
        return
    fi

    ceiling_ms=$(jq -r --arg key "$key" '
        .operations[$key].p99_ms
        // .operations[$key].hard_ceiling_ms
        // .operations[$key].p50_ms
        // empty
    ' "$BASELINE_FILE")
    if [ -z "$ceiling_ms" ] || [ "$ceiling_ms" = "null" ]; then
        printf '%s\n' "not_checked"
        return
    fi

    awk -v elapsed="$elapsed_ms" -v ceiling="$ceiling_ms" 'BEGIN {
        if (elapsed <= ceiling) {
            print "within_budget";
        } else {
            print "exceeded_budget";
        }
    }'
}

now_ns() {
    date +%s%N
}

elapsed_ms() {
    start_ns="$1"
    end_ns="$2"
    awk -v start="$start_ns" -v end="$end_ns" 'BEGIN {
        printf "%.6f", (end - start) / 1000000;
    }'
}

json_get() {
    file="$1"
    filter="$2"
    if command -v jq >/dev/null 2>&1; then
        jq -r "$filter // empty" "$file"
    else
        printf ''
    fi
}

json_timing_ms() {
    file="$1"
    timing="$2"
    if command -v jq >/dev/null 2>&1; then
        jq -r --arg timing "$timing" '
            .data.timings[]
            | select(.name == $timing)
            | .elapsedMs
        ' "$file" | sed -n '1p'
    else
        printf ''
    fi
}

workload_json() {
    if [ "$PROFILE" = "daemon-search-slo" ]; then
        printf '%s' '{"schema":"ee.perf.workload_ref.v1","manifest":"benches/daemon_round_trip.rs","tier":"10k_search","ci_suitability":"stable_self_hosted_linux_x64","memory_count":10000,"agent_count":1}'
        return
    fi
    if [ "$AUTO_ENROLL_BASELINE_ONLY" = "true" ]; then
        printf '{"schema":"ee.perf.workload_ref.v1","manifest":"benches/baselines/auto_enroll_perf_v0.json","tier":"%s","ci_suitability":"contract","memory_count":null,"agent_count":null}' "$WORKLOAD_TIER"
        return
    fi
    if command -v jq >/dev/null 2>&1 && [ -f "$WORKLOAD_FILE" ]; then
        jq -c --arg tier "$WORKLOAD_TIER" '
            .tiers[]
            | select(.name == $tier)
            | {
                schema: "ee.perf.workload_ref.v1",
                manifest: "tests/fixtures/swarm_scale/workloads.json",
                tier: .name,
                ci_suitability: .ci_suitability,
                memory_count: .memory_count,
                agent_count: .agent_count,
                expected_db_rows: .resource_profile.expected_db_rows,
                expected_index_bytes: .resource_profile.expected_index_bytes,
                expected_graph_nodes: .resource_profile.expected_graph_nodes
              }
        ' "$WORKLOAD_FILE"
    else
        printf '{"schema":"ee.perf.workload_ref.v1","manifest":"tests/fixtures/swarm_scale/workloads.json","tier":"%s"}' "$WORKLOAD_TIER"
    fi
}

run_status_smoke() {
    if output=$(cargo bench --bench status -- --quick --advisory); then
        printf '%s\n' "$output" >&2
        if command -v jq >/dev/null 2>&1; then
            p50_ms=$(printf '%s\n' "$output" | jq -r '.aggregate_p50_ms // null')
            max_ms=$(printf '%s\n' "$output" | jq -r '[.scales[].max_ms] | max // null')
            regression_status=$(printf '%s\n' "$output" | jq -r '.regression.status // "not_checked"')
        else
            p50_ms=null
            max_ms=null
            regression_status=not_checked
        fi
        append_result "ee_status" "measured" "$p50_ms" null null "$max_ms" null "$regression_status"
        echo "[+] status: p50=${p50_ms}ms max=${max_ms}ms regression=${regression_status}" >&2
    else
        append_result "ee_status" "failed" null null null null null
        echo "[-] status: FAILED" >&2
        FAILED=true
    fi
}

run_primer_smoke() {
    echo "" >&2
    echo "[*] Primer latency smoke (bd-39tzu.5: cold assemble + warm cache hit)..." >&2
    for op in ee_primer_cold_assemble ee_primer_warm_cache_hit; do
        if output=$(cargo bench --bench primer -- "$op" --warm-up-time 0.5 --measurement-time 1 --sample-size 10 2>&1); then
            time_line=$(parse_time_value "$output")
            if [ -n "$time_line" ]; then
                value=${time_line% *}
                unit=${time_line#* }
                elapsed=$(to_ms "$value" "$unit")
                append_measured_ms "$op" "$elapsed"
                echo "[+] $op: ${elapsed}ms" >&2
            else
                append_smoke_failure "$op"
                echo "[-] $op: no time line in criterion output" >&2
                FAILED=true
            fi
        else
            printf '%s\n' "$output" >&2
            append_smoke_failure "$op"
            echo "[-] $op: FAILED" >&2
            FAILED=true
        fi
    done
}

run_criterion_bench() {
    bench="$1"
    compare_only=false
    if [ "$CHECK_REGRESSION" = "true" ]; then
        case "$bench" in
            graph_*) compare_only=true ;;
        esac
    fi
    # BENCH_ARGS intentionally expands into separate Criterion CLI arguments.
    # shellcheck disable=SC2086
    if [ "$compare_only" = "true" ]; then
        if output=$(EE_BENCH_COMPARE_ONLY=1 cargo bench --bench "$bench" -- $BENCH_ARGS 2>&1); then
            status=0
        else
            status=$?
        fi
    else
        if output=$(cargo bench --bench "$bench" -- $BENCH_ARGS 2>&1); then
            status=0
        else
            status=$?
        fi
    fi
    if [ "$status" -eq 0 ]; then
        printf '%s\n' "$output" >&2
        parsed=$(parse_time_value "$output" || true)
        if [ -n "$parsed" ]; then
            raw_value=$(printf '%s\n' "$parsed" | awk '{print $1}')
            raw_unit=$(printf '%s\n' "$parsed" | awk '{print $2}')
            p50_ms=$(to_ms "$raw_value" "$raw_unit")
        else
            p50_ms=null
        fi
        if [ "$compare_only" = "true" ]; then
            append_result "ee_$bench" "budget_checked" "$p50_ms" null null null null
        else
            append_result "ee_$bench" "measured" "$p50_ms" null null null null
        fi
        echo "[+] $bench: p50=${p50_ms}ms" >&2
        if [ "$bench" = "context" ]; then
            orient_fast_p99_ms=$(printf '%s\n' "$output" \
                | sed -n 's/^ee_orient_fast_10k_sampled_p99_ms=\([0-9][0-9.]*\)$/\1/p' \
                | tail -n 1)
            if [ -n "$orient_fast_p99_ms" ]; then
                append_result "ee_orient_fast_content" "measured" null null \
                    "$orient_fast_p99_ms" "$orient_fast_p99_ms" null "within_budget"
                echo "[+] orient_fast_content: p99=${orient_fast_p99_ms}ms (<1000ms hard gate)" >&2
            else
                append_result "ee_orient_fast_content" "failed" null null null null null
                echo "[-] context benchmark did not emit the orient fast 10k p99 gate" >&2
                FAILED=true
            fi
        fi
    else
        printf '%s\n' "$output" >&2
        append_result "ee_$bench" "failed" null null null null null
        if [ "$bench" = "context" ]; then
            append_result "ee_orient_fast_content" "failed" null null null null null
        fi
        echo "[-] $bench: FAILED" >&2
        FAILED=true
    fi
}

run_daemon_search_slo() {
    markers_file="$ARTIFACT_DIR/daemon-search-slo.markers"
    echo "" >&2
    echo "[*] Benchmark: daemon-search-slo" >&2
    echo "    fixture: 10000 documents, prebuilt before measurement" >&2
    echo "    backend: neural_local required; downloads disabled" >&2

    if output=$(EE_EMBED_DOWNLOAD=off cargo bench --bench daemon_round_trip -- --warm-search-gate 2>&1); then
        status=0
    else
        status=$?
    fi
    printf '%s\n' "$output" >"$markers_file"
    printf '%s\n' "$output" >&2

    marker_number() {
        marker_key="$1"
        printf '%s\n' "$output" \
            | sed -n "s/^${marker_key}=\([0-9][0-9.]*\)$/\1/p" \
            | tail -n 1
    }
    marker_text() {
        marker_key="$1"
        printf '%s\n' "$output" \
            | sed -n "s/^${marker_key}=\([^[:space:]][^[:space:]]*\)$/\1/p" \
            | tail -n 1
    }

    cold_p50_ms=$(marker_number "ee_search_cli_cold_10k_p50_ms")
    warm_p50_ms=$(marker_number "ee_search_daemon_warm_10k_p50_ms")
    sample_count=$(marker_number "ee_search_daemon_10k_samples")
    backend=$(marker_text "ee_search_daemon_10k_backend")
    parity=$(marker_text "ee_search_daemon_10k_result_parity")
    cold_within_budget=$(awk -v value="${cold_p50_ms:-999999}" 'BEGIN { print (value < 1500.0) ? "yes" : "no" }')
    warm_within_budget=$(awk -v value="${warm_p50_ms:-999999}" 'BEGIN { print (value < 500.0) ? "yes" : "no" }')

    if [ "$status" -eq 0 ] \
        && [ -n "$cold_p50_ms" ] \
        && [ -n "$warm_p50_ms" ] \
        && [ "$cold_within_budget" = "yes" ] \
        && [ "$warm_within_budget" = "yes" ] \
        && [ "$sample_count" = "21" ] \
        && [ "$backend" = "neural_local" ] \
        && [ "$parity" = "exact_results_array" ]; then
        append_result "ee_search_cli_cold_10k" "measured" "$cold_p50_ms" null null null null "within_budget"
        append_result "ee_search_daemon_warm_10k" "measured" "$warm_p50_ms" null null null null "within_budget"
        echo "[+] cold fresh-process search: p50=${cold_p50_ms}ms (<1500ms)" >&2
        echo "[+] warm daemon search: p50=${warm_p50_ms}ms (<500ms)" >&2
        echo "[+] backend=${backend} samples=${sample_count} parity=${parity}" >&2
        echo "[+] stage markers: $markers_file" >&2
        return
    fi

    [ -n "$cold_p50_ms" ] || cold_p50_ms=null
    [ -n "$warm_p50_ms" ] || warm_p50_ms=null
    append_result "ee_search_cli_cold_10k" "failed" "$cold_p50_ms" null null null null "hard_gate_failed"
    append_result "ee_search_daemon_warm_10k" "failed" "$warm_p50_ms" null null null null "hard_gate_failed"
    echo "[-] daemon-search-slo failed: exit=${status} backend=${backend:-missing} samples=${sample_count:-missing} parity=${parity:-missing}" >&2
    echo "[-] retained markers: $markers_file" >&2
    FAILED=true
}

run_context_l2_warm_bench() {
    bench="context"
    key="ee_context_pack_l2_warm"

    echo "" >&2
    echo "[*] Benchmark: context_l2_warm" >&2
    # BENCH_ARGS intentionally expands into separate Criterion CLI arguments.
    # shellcheck disable=SC2086
    if output=$(cargo bench --bench "$bench" -- ee_context_pack_l2_warm $BENCH_ARGS 2>&1); then
        status=0
    else
        status=$?
    fi
    if [ "$status" -eq 0 ]; then
        printf '%s\n' "$output" >&2
        parsed=$(parse_time_value "$output" || true)
        if [ -n "$parsed" ]; then
            raw_value=$(printf '%s\n' "$parsed" | awk '{print $1}')
            raw_unit=$(printf '%s\n' "$parsed" | awk '{print $2}')
            p50_ms=$(to_ms "$raw_value" "$raw_unit")
        else
            p50_ms=null
        fi
        append_measured_ms "$key" "$p50_ms"
        echo "[+] context_l2_warm: p50=${p50_ms}ms" >&2
    else
        printf '%s\n' "$output" >&2
        append_result "$key" "failed" null null null null null
        echo "[-] context_l2_warm: FAILED" >&2
        FAILED=true
    fi
}

run_context_arena_mode_bench() {
    bench="context"
    key="ee_context_arena_workspace_reuse"
    filter="/workspace_reuse"

    echo "" >&2
    echo "[*] Benchmark: context_arena_mode" >&2
    echo "    filter: $filter" >&2
    # BENCH_ARGS intentionally expands into separate Criterion CLI arguments.
    # shellcheck disable=SC2086
    if output=$(cargo bench --bench "$bench" -- "$filter" $BENCH_ARGS 2>&1); then
        status=0
    else
        status=$?
    fi
    if [ "$status" -eq 0 ]; then
        printf '%s\n' "$output" >&2
        parsed=$(parse_time_value "$output" || true)
        if [ -n "$parsed" ]; then
            raw_value=$(printf '%s\n' "$parsed" | awk '{print $1}')
            raw_unit=$(printf '%s\n' "$parsed" | awk '{print $2}')
            p50_ms=$(to_ms "$raw_value" "$raw_unit")
        else
            p50_ms=null
        fi
        # WorkspaceReuse should allocate one reusable scratch workspace per
        # benchmark fixture, then reset it for later iterations. This is a
        # scratch-allocation count, not a process-global allocator count.
        append_result "$key" "measured" "$p50_ms" null null null null "not_checked" 1
        echo "[+] context_arena_mode: p50=${p50_ms}ms scratch_allocations=1" >&2
    else
        printf '%s\n' "$output" >&2
        append_result "$key" "failed" null null null null null
        echo "[-] context_arena_mode: FAILED" >&2
        FAILED=true
    fi
}

run_pack_replay_freshness_smoke() {
    echo "" >&2
    echo "[*] Pack replay/freshness overhead smoke..." >&2

    if ! command -v jq >/dev/null 2>&1; then
        echo "[-] jq is required for pack replay/freshness smoke measurement" >&2
        for op in \
            ee_context_pack_assembly_no_ledger \
            ee_context_pack_persistence_ledger \
            ee_context_pack_with_ledger \
            ee_pack_query_file_assembly_no_ledger \
            ee_pack_query_file_persistence_ledger \
            ee_pack_query_file_with_ledger \
            ee_context_freshness_scan \
            ee_pack_replay_ledger \
            ee_pack_diff_ledger
        do
            append_smoke_failure "$op"
        done
        FAILED=true
        return
    fi

    echo "[*] Building ee binary for smoke workload..." >&2
    if ! cargo build --release --bin ee >&2; then
        echo "[-] ee binary build failed" >&2
        for op in \
            ee_context_pack_assembly_no_ledger \
            ee_context_pack_persistence_ledger \
            ee_context_pack_with_ledger \
            ee_pack_query_file_assembly_no_ledger \
            ee_pack_query_file_persistence_ledger \
            ee_pack_query_file_with_ledger \
            ee_context_freshness_scan \
            ee_pack_replay_ledger \
            ee_pack_diff_ledger
        do
            append_smoke_failure "$op"
        done
        FAILED=true
        return
    fi

    smoke_root="$ARTIFACT_DIR/pack-replay-freshness-smoke-$$-$(date -u +%Y%m%dT%H%M%SZ)"
    smoke_workspace="$smoke_root/workspace"
    smoke_artifacts="$smoke_root/artifacts"
    smoke_source="$smoke_workspace/freshness-source.md"
    smoke_query_file="$smoke_root/query.eeq.json"
    smoke_marker="dcub pack replay freshness smoke"
    mkdir -p "$smoke_workspace" "$smoke_artifacts"
    printf '%s\n' "$smoke_marker source evidence line" > "$smoke_source"
    cat > "$smoke_query_file" <<EOF
{
  "version": "ee.query.v1",
  "query": { "text": "$smoke_marker" },
  "budget": { "maxTokens": 2000, "candidatePool": 20 },
  "output": { "profile": "compact" }
}
EOF

    run_smoke_command() {
        step="$1"
        shift
        step_slug=$(printf '%s' "$step" | sed 's/[^A-Za-z0-9_]/_/g')
        LAST_STDOUT_FILE="$smoke_artifacts/$step_slug.stdout.json"
        LAST_STDERR_FILE="$smoke_artifacts/$step_slug.stderr.log"
        start_ns=$(now_ns)
        if "$EE_BIN" "$@" >"$LAST_STDOUT_FILE" 2>"$LAST_STDERR_FILE"; then
            LAST_EXIT_CODE=0
        else
            LAST_EXIT_CODE=$?
        fi
        end_ns=$(now_ns)
        LAST_ELAPSED_MS=$(elapsed_ms "$start_ns" "$end_ns")
        if [ "$LAST_EXIT_CODE" -ne 0 ]; then
            echo "[-] $step failed with exit $LAST_EXIT_CODE; stdout=$LAST_STDOUT_FILE stderr=$LAST_STDERR_FILE" >&2
            return 1
        fi
        if [ -s "$LAST_STDERR_FILE" ]; then
            echo "[-] $step wrote stderr; stdout=$LAST_STDOUT_FILE stderr=$LAST_STDERR_FILE" >&2
            return 1
        fi
        if ! jq -e . "$LAST_STDOUT_FILE" >/dev/null 2>&1; then
            echo "[-] $step stdout is not JSON; stdout=$LAST_STDOUT_FILE" >&2
            return 1
        fi
        return 0
    }

    if ! run_smoke_command init --workspace "$smoke_workspace" --json init; then
        append_smoke_failure "ee_context_pack_with_ledger"
        FAILED=true
        return
    fi

    source_uri="file://$smoke_source#L1"
    source_content="$smoke_marker source evidence line"
    if ! run_smoke_command remember-source \
        --workspace "$smoke_workspace" --json remember \
        --level procedural --kind rule --tags dcub,replay,freshness \
        --source "$source_uri" "$source_content"; then
        append_smoke_failure "ee_context_pack_with_ledger"
        FAILED=true
        return
    fi
    source_memory_id=$(json_get "$LAST_STDOUT_FILE" '.data.memory_id')

    if ! run_smoke_command remember-redaction-safe \
        --workspace "$smoke_workspace" --json remember \
        --level procedural --kind rule --tags dcub,replay,egress \
        --source "agent-mail://eidetic_engine_cli-dcub#benchmark" \
        "$smoke_marker redaction-safe placeholder [REDACTED:alpha] [REDACTED:beta]"; then
        append_smoke_failure "ee_context_pack_with_ledger"
        FAILED=true
        return
    fi

    if ! run_smoke_command index-rebuild --workspace "$smoke_workspace" --json index rebuild; then
        append_smoke_failure "ee_context_pack_with_ledger"
        FAILED=true
        return
    fi

    if ! run_smoke_command context-performance-before \
        --workspace "$smoke_workspace" --json context "$smoke_marker" \
        --max-tokens 2000 --explain-performance; then
        append_smoke_failure "ee_context_pack_with_ledger"
        FAILED=true
        return
    fi
    context_assembly_ms=$(json_timing_ms "$LAST_STDOUT_FILE" "packAssembly")
    context_persistence_ms=$(json_timing_ms "$LAST_STDOUT_FILE" "packPersistence")
    context_total_ms=$(json_timing_ms "$LAST_STDOUT_FILE" "total")
    append_measured_ms "ee_context_pack_assembly_no_ledger" "${context_assembly_ms:-null}"
    append_measured_ms "ee_context_pack_persistence_ledger" "${context_persistence_ms:-null}"
    append_measured_ms "ee_context_pack_with_ledger" "${context_total_ms:-$LAST_ELAPSED_MS}"

    if ! run_smoke_command why-before --workspace "$smoke_workspace" --json why "$source_memory_id"; then
        append_smoke_failure "ee_pack_replay_ledger"
        FAILED=true
        return
    fi
    before_pack_id=$(json_get "$LAST_STDOUT_FILE" '.data.selection.latestPackSelection.packId')

    if ! run_smoke_command pack-query-performance \
        --workspace "$smoke_workspace" --json pack --query-file "$smoke_query_file" \
        --explain-performance; then
        append_smoke_failure "ee_pack_query_file_with_ledger"
        FAILED=true
        return
    fi
    pack_assembly_ms=$(json_timing_ms "$LAST_STDOUT_FILE" "packAssembly")
    pack_persistence_ms=$(json_timing_ms "$LAST_STDOUT_FILE" "packPersistence")
    pack_total_ms=$(json_timing_ms "$LAST_STDOUT_FILE" "total")
    append_measured_ms "ee_pack_query_file_assembly_no_ledger" "${pack_assembly_ms:-null}"
    append_measured_ms "ee_pack_query_file_persistence_ledger" "${pack_persistence_ms:-null}"
    append_measured_ms "ee_pack_query_file_with_ledger" "${pack_total_ms:-$LAST_ELAPSED_MS}"

    printf '%s\n' "$smoke_marker source evidence changed after first pack" > "$smoke_source"
    if ! run_smoke_command context-performance-after \
        --workspace "$smoke_workspace" --json context "$smoke_marker" \
        --max-tokens 2000 --explain-performance; then
        append_smoke_failure "ee_context_freshness_scan"
        FAILED=true
        return
    fi
    freshness_total_ms=$(json_timing_ms "$LAST_STDOUT_FILE" "total")
    freshness_code_count=$(jq '[.data.fallbacks[].code | select(. == "context_evidence_freshness_changed_source")] | length' "$LAST_STDOUT_FILE")
    if [ "$freshness_code_count" -eq 0 ]; then
        echo "[-] freshness smoke did not report context_evidence_freshness_changed_source; stdout=$LAST_STDOUT_FILE" >&2
        append_smoke_failure "ee_context_freshness_scan"
        FAILED=true
        return
    fi
    append_measured_ms "ee_context_freshness_scan" "${freshness_total_ms:-$LAST_ELAPSED_MS}"

    if ! run_smoke_command why-after --workspace "$smoke_workspace" --json why "$source_memory_id"; then
        append_smoke_failure "ee_pack_replay_ledger"
        FAILED=true
        return
    fi
    after_pack_id=$(json_get "$LAST_STDOUT_FILE" '.data.selection.latestPackSelection.packId')

    if [ -z "$before_pack_id" ] || [ -z "$after_pack_id" ] || [ "$before_pack_id" = "$after_pack_id" ]; then
        echo "[-] smoke pack ids unavailable or identical: before=$before_pack_id after=$after_pack_id" >&2
        append_smoke_failure "ee_pack_replay_ledger"
        append_smoke_failure "ee_pack_diff_ledger"
        FAILED=true
        return
    fi

    if ! run_smoke_command pack-replay-after \
        --workspace "$smoke_workspace" --json pack replay "$after_pack_id"; then
        append_smoke_failure "ee_pack_replay_ledger"
        FAILED=true
        return
    fi
    replay_status=$(json_get "$LAST_STDOUT_FILE" '.data.replay.status')
    if [ "$replay_status" != "available" ]; then
        echo "[-] pack replay ledger status was $replay_status; stdout=$LAST_STDOUT_FILE" >&2
        append_smoke_failure "ee_pack_replay_ledger"
        FAILED=true
        return
    fi
    append_measured_ms "ee_pack_replay_ledger" "$LAST_ELAPSED_MS"

    if ! run_smoke_command pack-diff \
        --workspace "$smoke_workspace" --json pack diff "$before_pack_id" "$after_pack_id"; then
        append_smoke_failure "ee_pack_diff_ledger"
        FAILED=true
        return
    fi
    replayable=$(json_get "$LAST_STDOUT_FILE" '.data.diff.summary.replayable')
    if [ "$replayable" != "true" ]; then
        echo "[-] pack diff was not replayable; stdout=$LAST_STDOUT_FILE" >&2
        append_smoke_failure "ee_pack_diff_ledger"
        FAILED=true
        return
    fi
    append_measured_ms "ee_pack_diff_ledger" "$LAST_ELAPSED_MS"
    echo "[+] pack replay/freshness smoke artifacts: $smoke_root" >&2
}

run_ask_fixture_smoke() {
    echo "" >&2
    echo "[*] Ask v1 fixture latency smoke (bd-169v0.5: answerable + abstention)..." >&2

    ask_fixture="$PROJECT_ROOT/tests/fixtures/eval/ask_v1/source_memory.json"
    if ! command -v jq >/dev/null 2>&1; then
        echo "[-] jq is required for ask fixture smoke measurement" >&2
        append_smoke_failure "ee_ask_v1_answerable"
        append_smoke_failure "ee_ask_v1_abstention"
        FAILED=true
        return
    fi
    if [ ! -f "$ask_fixture" ]; then
        echo "[-] ask fixture source memory missing: $ask_fixture" >&2
        append_smoke_failure "ee_ask_v1_answerable"
        append_smoke_failure "ee_ask_v1_abstention"
        FAILED=true
        return
    fi

    if [ ! -x "$EE_BIN" ]; then
        echo "[*] Building ee binary for ask smoke workload..." >&2
        if ! cargo build --release --bin ee >&2; then
            echo "[-] ee binary build failed" >&2
            append_smoke_failure "ee_ask_v1_answerable"
            append_smoke_failure "ee_ask_v1_abstention"
            FAILED=true
            return
        fi
    fi

    ask_root="$ARTIFACT_DIR/ask-v1-smoke-$$-$(date -u +%Y%m%dT%H%M%SZ)"
    ask_workspace="$ask_root/workspace"
    ask_artifacts="$ask_root/artifacts"
    ask_rows="$ask_artifacts/source_memories.jsonl"
    mkdir -p "$ask_workspace" "$ask_artifacts"

    run_ask_smoke_command() {
        step="$1"
        shift
        step_slug=$(printf '%s' "$step" | sed 's/[^A-Za-z0-9_]/_/g')
        LAST_STDOUT_FILE="$ask_artifacts/$step_slug.stdout.json"
        LAST_STDERR_FILE="$ask_artifacts/$step_slug.stderr.log"
        start_ns=$(now_ns)
        if "$EE_BIN" "$@" >"$LAST_STDOUT_FILE" 2>"$LAST_STDERR_FILE"; then
            LAST_EXIT_CODE=0
        else
            LAST_EXIT_CODE=$?
        fi
        end_ns=$(now_ns)
        LAST_ELAPSED_MS=$(elapsed_ms "$start_ns" "$end_ns")
        if [ "$LAST_EXIT_CODE" -ne 0 ]; then
            echo "[-] $step failed with exit $LAST_EXIT_CODE; stdout=$LAST_STDOUT_FILE stderr=$LAST_STDERR_FILE" >&2
            return 1
        fi
        if ! jq -e . "$LAST_STDOUT_FILE" >/dev/null 2>&1; then
            echo "[-] $step stdout is not JSON; stdout=$LAST_STDOUT_FILE" >&2
            return 1
        fi
        return 0
    }

    if ! run_ask_smoke_command init --workspace "$ask_workspace" --json init; then
        append_smoke_failure "ee_ask_v1_answerable"
        append_smoke_failure "ee_ask_v1_abstention"
        FAILED=true
        return
    fi

    jq -c '.memories[]' "$ask_fixture" >"$ask_rows"
    while IFS= read -r memory_row; do
        content=$(printf '%s' "$memory_row" | jq -r '.content')
        level=$(printf '%s' "$memory_row" | jq -r '.level // "episodic"')
        kind=$(printf '%s' "$memory_row" | jq -r '.kind // "fact"')
        confidence=$(printf '%s' "$memory_row" | jq -r '.confidence // 0.8')
        source=$(printf '%s' "$memory_row" | jq -r '.provenance_uri // "fixture://ask_v1/unknown"')
        if ! run_ask_smoke_command "remember-$(printf '%s' "$source" | sed 's/[^A-Za-z0-9_]/_/g')" \
            --workspace "$ask_workspace" --json remember \
            --level "$level" --kind "$kind" --confidence "$confidence" \
            --source "$source" --no-auto-link --no-propose-candidates \
            "$content"; then
            append_smoke_failure "ee_ask_v1_answerable"
            append_smoke_failure "ee_ask_v1_abstention"
            FAILED=true
            return
        fi
    done <"$ask_rows"

    answerable_query="Project Zephyr Rust nightly 1.96.0 active toolchain release verification"
    if ! run_ask_smoke_command ask-answerable \
        --workspace "$ask_workspace" --json ask "$answerable_query"; then
        append_smoke_failure "ee_ask_v1_answerable"
        append_smoke_failure "ee_ask_v1_abstention"
        FAILED=true
        return
    fi
    if ! jq -e '.success == true and .data.schema == "ee.ask.v1" and .data.abstained == false' \
        "$LAST_STDOUT_FILE" >/dev/null 2>&1; then
        echo "[-] ask answerable smoke did not return a confident ee.ask.v1 answer; stdout=$LAST_STDOUT_FILE" >&2
        append_smoke_failure "ee_ask_v1_answerable"
        append_smoke_failure "ee_ask_v1_abstention"
        FAILED=true
        return
    fi
    answerable_ms="$LAST_ELAPSED_MS"
    append_result "ee_ask_v1_answerable" "measured" "$answerable_ms" "$answerable_ms" "$answerable_ms" "$answerable_ms" null "advisory_fixture_smoke"

    abstention_query="Who approved the lunar invoice for Project Zephyr?"
    if ! run_ask_smoke_command ask-abstention \
        --workspace "$ask_workspace" --json ask "$abstention_query" --min-confidence 0.95; then
        append_smoke_failure "ee_ask_v1_abstention"
        FAILED=true
        return
    fi
    if ! jq -e '.success == true and .data.schema == "ee.ask.v1" and .data.abstained == true' \
        "$LAST_STDOUT_FILE" >/dev/null 2>&1; then
        echo "[-] ask abstention smoke did not return an abstention payload; stdout=$LAST_STDOUT_FILE" >&2
        append_smoke_failure "ee_ask_v1_abstention"
        FAILED=true
        return
    fi
    abstention_ms="$LAST_ELAPSED_MS"
    if awk -v answerable="$answerable_ms" -v abstention="$abstention_ms" 'BEGIN {
        limit = (answerable * 1.5) + 5.0;
        exit !(abstention <= limit);
    }'; then
        abstention_status="advisory_abstention_not_slow_path"
    else
        abstention_status="advisory_abstention_slower"
        echo "[!] ask abstention smoke was slower than answerable path: answerable=${answerable_ms}ms abstention=${abstention_ms}ms" >&2
    fi
    append_result "ee_ask_v1_abstention" "measured" "$abstention_ms" "$abstention_ms" "$abstention_ms" "$abstention_ms" null "$abstention_status"
    echo "[+] ask v1 smoke: answerable=${answerable_ms}ms abstention=${abstention_ms}ms status=$abstention_status artifacts=$ask_root" >&2
}

run_journal_capture_smoke() {
    echo "" >&2
    echo "[*] Journal capture smoke (bd-1pi9m.6: append, batch, distill)..." >&2

    if ! command -v jq >/dev/null 2>&1; then
        echo "[-] jq is required for journal capture smoke measurement" >&2
        append_smoke_failure "ee_journal_append_single"
        append_smoke_failure "ee_journal_append_batch_50"
        append_smoke_failure "ee_journal_distill_200"
        FAILED=true
        return
    fi

    if [ ! -x "$EE_BIN" ]; then
        echo "[*] Building ee binary for journal capture smoke workload..." >&2
        if ! cargo build --release --bin ee >&2; then
            echo "[-] ee binary build failed" >&2
            append_smoke_failure "ee_journal_append_single"
            append_smoke_failure "ee_journal_append_batch_50"
            append_smoke_failure "ee_journal_distill_200"
            FAILED=true
            return
        fi
    fi

    journal_root="$ARTIFACT_DIR/journal-capture-smoke-$$-$(date -u +%Y%m%dT%H%M%SZ)"
    journal_workspace="$journal_root/workspace"
    journal_artifacts="$journal_root/artifacts"
    journal_cmd="cargo test --lib journal_capture"
    mkdir -p "$journal_workspace" "$journal_artifacts"

    run_journal_smoke_command() {
        step="$1"
        shift
        step_slug=$(printf '%s' "$step" | sed 's/[^A-Za-z0-9_]/_/g')
        LAST_STDOUT_FILE="$journal_artifacts/$step_slug.stdout.json"
        LAST_STDERR_FILE="$journal_artifacts/$step_slug.stderr.log"
        start_ns=$(now_ns)
        if "$EE_BIN" "$@" >"$LAST_STDOUT_FILE" 2>"$LAST_STDERR_FILE"; then
            LAST_EXIT_CODE=0
        else
            LAST_EXIT_CODE=$?
        fi
        end_ns=$(now_ns)
        LAST_ELAPSED_MS=$(elapsed_ms "$start_ns" "$end_ns")
        if [ "$LAST_EXIT_CODE" -ne 0 ]; then
            echo "[-] $step failed with exit $LAST_EXIT_CODE; stdout=$LAST_STDOUT_FILE stderr=$LAST_STDERR_FILE" >&2
            return 1
        fi
        if ! jq -e . "$LAST_STDOUT_FILE" >/dev/null 2>&1; then
            echo "[-] $step stdout is not JSON; stdout=$LAST_STDOUT_FILE" >&2
            return 1
        fi
        return 0
    }

    if ! run_journal_smoke_command init --workspace "$journal_workspace" --json init; then
        append_smoke_failure "ee_journal_append_single"
        append_smoke_failure "ee_journal_append_batch_50"
        append_smoke_failure "ee_journal_distill_200"
        FAILED=true
        return
    fi

    single_session="journal-bench-single-$$"
    if ! run_journal_smoke_command append-single \
        --workspace "$journal_workspace" journal append \
        "journal benchmark single command failure: linker cache missing object" \
        --kind command_failure \
        --source hook \
        --cmd "$journal_cmd" \
        --exit-code 101 \
        --cwd "$journal_workspace" \
        --stderr-tail "error: linker cache missing object" \
        --session "$single_session" \
        --json; then
        append_smoke_failure "ee_journal_append_single"
        FAILED=true
        return
    fi
    if ! jq -e '.success == true and .data.status == "stored" and (.data.entry.entryId // "" | startswith("jrn_"))' \
        "$LAST_STDOUT_FILE" >/dev/null 2>&1; then
        echo "[-] journal append smoke did not store one entry; stdout=$LAST_STDOUT_FILE" >&2
        append_smoke_failure "ee_journal_append_single"
        FAILED=true
        return
    fi
    append_result "ee_journal_append_single" "measured" "$LAST_ELAPSED_MS" "$LAST_ELAPSED_MS" "$LAST_ELAPSED_MS" "$LAST_ELAPSED_MS" null "advisory_fixture_smoke"

    batch_rows="$journal_artifacts/batch_50.jsonl"
    batch_session="journal-bench-batch-$$"
    i=1
    while [ "$i" -le 50 ]; do
        jq -nc \
            --arg session "$batch_session" \
            --arg cwd "$journal_workspace" \
            --arg cmd "$journal_cmd" \
            --arg body "journal benchmark batch command failure $i: linker cache missing object" \
            '{body:$body,kind:"command_failure",sessionKey:$session,cmd:$cmd,exitCode:101,cwd:$cwd,paths:["src/core/journal.rs"],stderrTail:"error: linker cache missing object"}'
        i=$((i + 1))
    done >"$batch_rows"

    LAST_STDOUT_FILE="$journal_artifacts/append_batch_50.stdout.json"
    LAST_STDERR_FILE="$journal_artifacts/append_batch_50.stderr.log"
    start_ns=$(now_ns)
    if "$EE_BIN" --workspace "$journal_workspace" journal append --stdin --source stdin --json \
        <"$batch_rows" >"$LAST_STDOUT_FILE" 2>"$LAST_STDERR_FILE"; then
        LAST_EXIT_CODE=0
    else
        LAST_EXIT_CODE=$?
    fi
    end_ns=$(now_ns)
    LAST_ELAPSED_MS=$(elapsed_ms "$start_ns" "$end_ns")
    if [ "$LAST_EXIT_CODE" -ne 0 ] \
        || ! jq -e '.success == true and .data.lineCount == 50 and .data.storedCount == 50 and .data.failedCount == 0' \
            "$LAST_STDOUT_FILE" >/dev/null 2>&1; then
        echo "[-] journal batch smoke failed; stdout=$LAST_STDOUT_FILE stderr=$LAST_STDERR_FILE" >&2
        append_smoke_failure "ee_journal_append_batch_50"
        FAILED=true
        return
    fi
    append_result "ee_journal_append_batch_50" "measured" "$LAST_ELAPSED_MS" "$LAST_ELAPSED_MS" "$LAST_ELAPSED_MS" "$LAST_ELAPSED_MS" null "advisory_fixture_smoke"

    distill_rows="$journal_artifacts/distill_200.jsonl"
    distill_session="journal-bench-distill-$$"
    i=1
    while [ "$i" -le 200 ]; do
        jq -nc \
            --arg session "$distill_session" \
            --arg cwd "$journal_workspace" \
            --arg cmd "$journal_cmd" \
            --arg body "journal benchmark distill repeated cargo failure $i: linker cache missing object after retry" \
            '{body:$body,kind:"command_failure",sessionKey:$session,cmd:$cmd,exitCode:101,cwd:$cwd,paths:["src/core/journal.rs"],stderrTail:"error: linker cache missing object"}'
        i=$((i + 1))
    done >"$distill_rows"

    if ! "$EE_BIN" --workspace "$journal_workspace" journal append --stdin --source stdin --json \
        <"$distill_rows" >"$journal_artifacts/distill_seed.stdout.json" 2>"$journal_artifacts/distill_seed.stderr.log"; then
        echo "[-] journal distill seed failed; stdout=$journal_artifacts/distill_seed.stdout.json stderr=$journal_artifacts/distill_seed.stderr.log" >&2
        append_smoke_failure "ee_journal_distill_200"
        FAILED=true
        return
    fi

    if ! run_journal_smoke_command distill-200 \
        --workspace "$journal_workspace" journal distill \
        --session "$distill_session" \
        --dry-run \
        --json; then
        append_smoke_failure "ee_journal_distill_200"
        FAILED=true
        return
    fi
    if ! jq -e '.success == true and .data.schema == "ee.journal.distill.v1" and .data.scannedCount == 200 and ((.data.proposals // []) | length) >= 1' \
        "$LAST_STDOUT_FILE" >/dev/null 2>&1; then
        echo "[-] journal distill smoke did not produce a 200-entry proposal report; stdout=$LAST_STDOUT_FILE" >&2
        append_smoke_failure "ee_journal_distill_200"
        FAILED=true
        return
    fi
    append_result "ee_journal_distill_200" "measured" "$LAST_ELAPSED_MS" "$LAST_ELAPSED_MS" "$LAST_ELAPSED_MS" "$LAST_ELAPSED_MS" null "advisory_fixture_smoke"
    echo "[+] journal capture smoke: single append, 50-line batch, distill-200 artifacts=$journal_root" >&2
}

append_auto_enroll_baseline_rows() {
    if ! command -v jq >/dev/null 2>&1; then
        echo "[-] jq is required for auto-enroll baseline profile" >&2
        FAILED=true
        return
    fi
    if [ ! -f "$BASELINE_FILE" ]; then
        echo "[-] auto-enroll baseline missing at $BASELINE_FILE" >&2
        FAILED=true
        return
    fi

    rows_file="$ARTIFACT_DIR/auto_enroll_baseline_rows.tsv"
    case "$PROFILE" in
        auto_enroll_idle_24h)
            jq -r '
                .operations
                | to_entries[]
                | select(.value.class == "idle")
                | [.key, .value.p50_ms, .value.p99_ms] | @tsv
            ' "$BASELINE_FILE" >"$rows_file"
            ;;
        *)
            jq -r '
                .operations
                | to_entries[]
                | [.key, .value.p50_ms, .value.p99_ms] | @tsv
            ' "$BASELINE_FILE" >"$rows_file"
            ;;
    esac

    while IFS="$(printf '\t')" read -r key p50_ms p99_ms; do
        [ -n "$key" ] || continue
        append_result "$key" "baseline_contract" "$p50_ms" null "$p99_ms" "$p99_ms" null "not_checked"
        echo "[+] $key: baseline p50=${p50_ms} p99=${p99_ms}" >&2
    done <"$rows_file"
}

if [ "$AUTO_ENROLL_BASELINE_ONLY" = "true" ]; then
    append_auto_enroll_baseline_rows
elif [ "$PROFILE" = "daemon-search-slo" ]; then
    run_daemon_search_slo
else
    for bench in $BENCHMARKS; do
        echo "" >&2
        echo "[*] Benchmark: $bench" >&2
        if [ "$PROFILE" = "ci-smoke" ] && [ "$bench" = "status" ]; then
            run_status_smoke
        else
            run_criterion_bench "$bench"
        fi
    done
fi

case "$PROFILE" in
    nightly|stress)
        run_context_l2_warm_bench
        run_context_arena_mode_bench
        ;;
esac

if [ "$PROFILE" = "ci-smoke" ]; then
    run_pack_replay_freshness_smoke
    run_ask_fixture_smoke
    run_journal_capture_smoke
    run_primer_smoke
fi

WORKLOAD_JSON=$(workload_json)
if [ "$PROFILE" = "daemon-search-slo" ]; then
    PERF_BASELINE_FILE=null
else
    PERF_BASELINE_FILE="\"$BASELINE_FILE\""
fi

# Generate ee.perf.v1 JSON
PERF_JSON=$(cat <<EOF
{
  "schema": "ee.perf.v1",
  "profile": "$PROFILE",
  "profile_class": "$PROFILE_CLASS",
  "timestamp": "$TIMESTAMP",
  "version": "$(grep '^version' "$PROJECT_ROOT/Cargo.toml" | head -1 | cut -d'"' -f2)",
  "git_sha": "$(git -C "$PROJECT_ROOT" rev-parse --short HEAD 2>/dev/null || echo "unknown")",
  "target_dir": "$TARGET_ROOT",
  "criterion_dir": "$CRITERION_DIR",
  "artifact_dir": "$ARTIFACT_DIR",
  "budget_mode": "$BUDGET_MODE",
  "release_blocking": $RELEASE_BLOCKING,
  "artifact_redaction": {
    "status": "redaction_safe",
    "raw_secret_material": "not_used",
    "policy": "synthetic placeholders only; command artifacts are JSON/stderr files under artifact_dir"
  },
  "workload": $WORKLOAD_JSON,
  "operations": {
    $RESULTS
  },
  "budgets_file": "benches/budgets.toml",
  "baseline_file": $PERF_BASELINE_FILE
}
EOF
)

if [ "$JSON_OUTPUT" = "true" ]; then
    echo "$PERF_JSON"
else
    echo "$PERF_JSON" > "$OUTPUT_FILE"
    echo "" >&2
    echo "[+] Results written to: $OUTPUT_FILE" >&2
fi

# Check regressions if requested
if [ "$CHECK_REGRESSION" = "true" ]; then
    echo "" >&2
    echo "[*] Checking for regressions against baseline..." >&2

    if [ "$AUTO_ENROLL_BASELINE_ONLY" = "true" ]; then
        echo "[+] Auto-enroll baseline contract loaded from: $BASELINE_FILE" >&2
        echo "[+] Candidate comparison belongs to scripts/e2e_overhaul/auto_enroll_perf_gate.sh" >&2
    elif [ -f "$BASELINE_FILE" ] && command -v jq >/dev/null 2>&1; then
        echo "[+] Baseline file found: $BASELINE_FILE" >&2

        # Read thresholds from budgets.toml (defaults: 20% p50, 50% p99).
        # This gate remains advisory unless --check-regression is requested.
        P50_THRESHOLD=$(jq -r '.meta.regression_pct_p50 // 20' "$BUDGETS_FILE" 2>/dev/null || printf '%s\n' 20)
        # Reserved for the p99 gate once Criterion summaries are normalized.
        # shellcheck disable=SC2034
        P99_THRESHOLD=50

        REGRESSION_FOUND=false

        # check_regression_for_op OP_NAME
        # Emitted keys from run_context_l2_warm_bench / run_context_arena_mode_bench
        # do not follow the `ee_${bench}` form and must be passed in explicitly.
        # Honors per-op tolerance_pct_p50 from the baseline if set; otherwise uses
        # the global P50_THRESHOLD from budgets.toml [meta]. unstable=true ops emit
        # a warning instead of failing the build (bd-3925e).
        check_regression_for_op() {
            op_name="$1"
            baseline_p50=$(jq -r ".operations.${op_name}.p50_ms // 0" "$BASELINE_FILE" 2>/dev/null)
            current_p50=$(echo "$PERF_JSON" | jq -r ".operations.${op_name}.p50_ms // 0" 2>/dev/null)
            op_tolerance=$(jq -r ".operations.${op_name}.tolerance_pct_p50 // ${P50_THRESHOLD}" "$BASELINE_FILE" 2>/dev/null)
            op_unstable=$(jq -r ".operations.${op_name}.unstable // false" "$BASELINE_FILE" 2>/dev/null)

            if [ "$baseline_p50" = "0" ] || [ "$baseline_p50" = "null" ] || [ "$current_p50" = "0" ] || [ "$current_p50" = "null" ]; then
                return 0
            fi

            regression_pct=$(echo "scale=2; ($current_p50 - $baseline_p50) / $baseline_p50 * 100" | bc -l 2>/dev/null || echo "0")
            if [ "$(echo "$regression_pct > $op_tolerance" | bc -l 2>/dev/null)" = "1" ]; then
                if [ "$op_unstable" = "true" ]; then
                    echo "[!] UNSTABLE REGRESSION (warning): $op_name p50 regressed ${regression_pct}% (baseline: ${baseline_p50}ms, current: ${current_p50}ms, tolerance: ${op_tolerance}%)" >&2
                else
                    echo "[-] REGRESSION: $op_name p50 regressed ${regression_pct}% (baseline: ${baseline_p50}ms, current: ${current_p50}ms, tolerance: ${op_tolerance}%)" >&2
                    REGRESSION_FOUND=true
                fi
            else
                echo "[+] $op_name: p50 within tolerance (${regression_pct}% change, tolerance: ${op_tolerance}%)" >&2
            fi
        }

        for bench in $BENCHMARKS; do
            check_regression_for_op "ee_${bench}"
        done

        # Extra operations emitted under custom keys. Without this list, their
        # results land in the perf JSON with regression_status="not_checked"
        # and never participate in baseline comparison.
        EXTRA_REGRESSION_OPERATIONS="ee_context_pack_l2_warm ee_context_arena_workspace_reuse ee_orient_fast_content"
        for op in $EXTRA_REGRESSION_OPERATIONS; do
            check_regression_for_op "$op"
        done

        if [ "$REGRESSION_FOUND" = "true" ]; then
            echo "" >&2
            echo "[-] Performance regression detected - failing build" >&2
            FAILED=true
        else
            echo "" >&2
            echo "[+] No significant regressions detected" >&2
        fi
    elif [ ! -f "$BASELINE_FILE" ]; then
        echo "[!] No baseline file found - skipping regression check" >&2
    else
        echo "[!] jq not available - skipping regression check" >&2
    fi

    if [ "$AUTO_ENROLL_BASELINE_ONLY" != "true" ]; then
        PACK_SIZE_GATE="$PROJECT_ROOT/scripts/bench_pack_regression.sh"
        if [ -x "$PACK_SIZE_GATE" ]; then
            echo "" >&2
            echo "[*] Checking pack-size regression gate..." >&2
            if "$PACK_SIZE_GATE" --skip-run --summary "$CRITERION_DIR/pack_size/summary.json"; then
                echo "[+] pack-size regression gate passed" >&2
            else
                echo "[-] pack-size regression gate failed" >&2
                FAILED=true
            fi
        else
            echo "[!] Pack-size regression gate missing or not executable: $PACK_SIZE_GATE" >&2
            FAILED=true
        fi
    fi
fi

if [ "$FAILED" = "true" ]; then
    echo "" >&2
    echo "[-] Some benchmarks failed" >&2
    exit 1
fi

echo "" >&2
echo "[+] All benchmarks completed" >&2
exit 0
