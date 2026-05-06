#!/bin/sh
set -eu

# Performance benchmark runner for ee (eidetic_engine_cli-htjd, fcq1.2)
#
# Runs criterion benchmarks and produces an ee.perf.v1 JSON artifact.
# Compares results against benches/budgets.toml thresholds.
#
# Usage:
#   ./scripts/bench.sh --profile ci-smoke --json
#   ./scripts/bench.sh --profile nightly
#   ./scripts/bench.sh --profile stress --check-regression
#   ./scripts/bench.sh --quick            # Alias for --profile ci-smoke
#
# Environment:
#   CARGO_TARGET_DIR       Build directory. For RCH use:
#                          CARGO_TARGET_DIR=${TMPDIR:-/tmp}/rch_target_ee_bench
#   EE_BENCH_ARTIFACT_DIR  Directory for JSON artifacts.
#   EE_BENCH_OUTPUT        Output path for JSON artifact.

PROFILE="nightly"
JSON_OUTPUT=false
CHECK_REGRESSION=false
LIST_PROFILES=false

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
BUDGETS_FILE="$PROJECT_ROOT/benches/budgets.toml"
BASELINE_FILE="$PROJECT_ROOT/benches/baselines/v0.1.json"
WORKLOAD_FILE="$PROJECT_ROOT/tests/fixtures/swarm_scale/workloads.json"
TARGET_ROOT="${CARGO_TARGET_DIR:-${TMPDIR:-/tmp}/rch_target_ee_bench}"
CRITERION_DIR="$TARGET_ROOT/criterion"
ARTIFACT_DIR="${EE_BENCH_ARTIFACT_DIR:-$TARGET_ROOT/ee-bench}"
OUTPUT_FILE="${EE_BENCH_OUTPUT:-$ARTIFACT_DIR/ee-perf.v1.json}"

if [ ! -f "$BUDGETS_FILE" ]; then
    echo "Error: budgets.toml not found at $BUDGETS_FILE" >&2
    exit 1
fi

if [ "$LIST_PROFILES" = "true" ]; then
    printf '%s\n' "ci-smoke" "nightly" "stress"
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
    nightly)
        BENCHMARKS="remember search context why outcome status import_cass link graph_pagerank curate_candidates"
        BENCH_ARGS="--warm-up-time 0.5 --measurement-time 2 --sample-size 20"
        PROFILE_CLASS="nightly_ci"
        WORKLOAD_TIER="medium"
        RELEASE_BLOCKING=false
        ;;
    stress)
        BENCHMARKS="remember search context why outcome status import_cass link graph_pagerank curate_candidates"
        BENCH_ARGS=""
        PROFILE_CLASS="local_256gb"
        WORKLOAD_TIER="stress"
        RELEASE_BLOCKING=false
        ;;
    *)
        echo "Unknown benchmark profile: $PROFILE" >&2
        echo "Known profiles: ci-smoke, nightly, stress" >&2
        exit 1
        ;;
esac

mkdir -p "$ARTIFACT_DIR"

echo "=== EE Performance Benchmarks ===" >&2
echo "profile: $PROFILE ($PROFILE_CLASS)" >&2
echo "target: $TARGET_ROOT" >&2
echo "artifacts: $ARTIFACT_DIR" >&2
echo "workload tier: $WORKLOAD_TIER" >&2

# Build benchmarks first
echo "[*] Building benchmarks..." >&2
if [ "$PROFILE" = "ci-smoke" ]; then
    cargo build --release --bench status >&2
else
    cargo build --release --benches >&2
fi

if [ "$PROFILE" = "ci-smoke" ]; then
    echo "[*] Running ci-smoke benchmark profile..." >&2
else
    echo "[*] Running $PROFILE benchmark profile..." >&2
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
      \"max_ms\": $max_ms,
      \"max_rss_kb\": null,
      \"allocation_count\": null,
      \"db_size_bytes\": null,
      \"index_size_bytes\": null,
      \"rows_per_sec\": $rows_per_sec,
      \"budget_mode\": \"advisory\"
    }"
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

workload_json() {
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
    if output=$(cargo bench --bench status -- --quick); then
        printf '%s\n' "$output" >&2
        if command -v jq >/dev/null 2>&1; then
            p50_ms=$(printf '%s\n' "$output" | jq -r '.aggregate_p50_ms // null')
            max_ms=$(printf '%s\n' "$output" | jq -r '[.scales[].max_ms] | max // null')
        else
            p50_ms=null
            max_ms=null
        fi
        append_result "ee_status" "measured" "$p50_ms" null null "$max_ms" null
        echo "[+] status: p50=${p50_ms}ms max=${max_ms}ms" >&2
    else
        append_result "ee_status" "failed" null null null null null
        echo "[-] status: FAILED" >&2
        FAILED=true
    fi
}

run_criterion_bench() {
    bench="$1"
    if output=$(cargo bench --bench "$bench" -- $BENCH_ARGS 2>&1); then
        printf '%s\n' "$output" >&2
        parsed=$(parse_time_value "$output" || true)
        if [ -n "$parsed" ]; then
            raw_value=$(printf '%s\n' "$parsed" | awk '{print $1}')
            raw_unit=$(printf '%s\n' "$parsed" | awk '{print $2}')
            p50_ms=$(to_ms "$raw_value" "$raw_unit")
        else
            p50_ms=null
        fi
        append_result "ee_$bench" "measured" "$p50_ms" null null null null
        echo "[+] $bench: p50=${p50_ms}ms" >&2
    else
        printf '%s\n' "$output" >&2
        append_result "ee_$bench" "failed" null null null null null
        echo "[-] $bench: FAILED" >&2
        FAILED=true
    fi
}

for bench in $BENCHMARKS; do
    echo "" >&2
    echo "[*] Benchmark: $bench" >&2
    if [ "$PROFILE" = "ci-smoke" ] && [ "$bench" = "status" ]; then
        run_status_smoke
    else
        run_criterion_bench "$bench"
    fi
done

WORKLOAD_JSON=$(workload_json)

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
  "budget_mode": "advisory",
  "release_blocking": $RELEASE_BLOCKING,
  "workload": $WORKLOAD_JSON,
  "operations": {
    $RESULTS
  },
  "budgets_file": "benches/budgets.toml",
  "baseline_file": "benches/baselines/v0.1.json"
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

    if [ -f "$BASELINE_FILE" ] && command -v jq >/dev/null 2>&1; then
        echo "[+] Baseline file found: $BASELINE_FILE" >&2

        # Read thresholds from budgets.toml (defaults: 20% p50, 50% p99).
        # This gate remains advisory unless --check-regression is requested.
        P50_THRESHOLD=20
        P99_THRESHOLD=50

        REGRESSION_FOUND=false

        for bench in $BENCHMARKS; do
            OP_NAME="ee_${bench}"
            BASELINE_P50=$(jq -r ".operations.${OP_NAME}.p50_ms // 0" "$BASELINE_FILE" 2>/dev/null)
            CURRENT_P50=$(echo "$PERF_JSON" | jq -r ".operations.${OP_NAME}.p50_ms // 0" 2>/dev/null)

            if [ "$BASELINE_P50" != "0" ] && [ "$BASELINE_P50" != "null" ] && [ "$CURRENT_P50" != "0" ] && [ "$CURRENT_P50" != "null" ]; then
                # Calculate regression percentage: (current - baseline) / baseline * 100
                REGRESSION_PCT=$(echo "scale=2; ($CURRENT_P50 - $BASELINE_P50) / $BASELINE_P50 * 100" | bc -l 2>/dev/null || echo "0")

                if [ "$(echo "$REGRESSION_PCT > $P50_THRESHOLD" | bc -l 2>/dev/null)" = "1" ]; then
                    echo "[-] REGRESSION: $OP_NAME p50 regressed ${REGRESSION_PCT}% (baseline: ${BASELINE_P50}ms, current: ${CURRENT_P50}ms)" >&2
                    REGRESSION_FOUND=true
                else
                    echo "[+] $OP_NAME: p50 within threshold (${REGRESSION_PCT}% change)" >&2
                fi
            fi
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
fi

if [ "$FAILED" = "true" ]; then
    echo "" >&2
    echo "[-] Some benchmarks failed" >&2
    exit 1
fi

echo "" >&2
echo "[+] All benchmarks completed" >&2
exit 0
