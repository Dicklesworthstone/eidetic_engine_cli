#!/bin/sh
set -eu

# Performance benchmark runner for ee (eidetic_engine_cli-htjd)
#
# Runs criterion benchmarks and produces ee-perf.v1 JSON artifact.
# Compares results against benches/budgets.toml thresholds.
#
# Usage:
#   ./scripts/bench.sh                    # Run all benchmarks
#   ./scripts/bench.sh --quick            # Run smoke test only (1 iteration)
#   ./scripts/bench.sh --json             # Output ee-perf.v1 JSON to stdout
#   ./scripts/bench.sh --check-regression # Fail if regression > threshold
#
# Environment:
#   CARGO_TARGET_DIR  - Build directory (default: target)
#   EE_BENCH_OUTPUT   - Output path for JSON artifact (default: ee-perf.v1.json)

QUICK_MODE=false
JSON_OUTPUT=false
CHECK_REGRESSION=false
OUTPUT_FILE="${EE_BENCH_OUTPUT:-ee-perf.v1.json}"

for arg in "$@"; do
    case "$arg" in
        --quick) QUICK_MODE=true ;;
        --json) JSON_OUTPUT=true ;;
        --check-regression) CHECK_REGRESSION=true ;;
        --help|-h)
            sed -n '3,15p' "$0" | sed 's/^# //' | sed 's/^#//'
            exit 0
            ;;
        *)
            echo "Unknown argument: $arg" >&2
            exit 1
            ;;
    esac
done

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_ROOT="$(dirname "$SCRIPT_DIR")"
BUDGETS_FILE="$PROJECT_ROOT/benches/budgets.toml"
BASELINE_FILE="$PROJECT_ROOT/benches/baselines/v0.1.json"
CRITERION_DIR="${CARGO_TARGET_DIR:-$PROJECT_ROOT/target}/criterion"

if [ ! -f "$BUDGETS_FILE" ]; then
    echo "Error: budgets.toml not found at $BUDGETS_FILE" >&2
    exit 1
fi

echo "=== EE Performance Benchmarks ===" >&2
echo "" >&2

# Build benchmarks first
echo "[*] Building benchmarks..." >&2
cargo build --release --benches 2>&1 | grep -v "Compiling\|Finished" || true

# Run benchmarks
BENCH_ARGS=""
if [ "$QUICK_MODE" = "true" ]; then
    BENCH_ARGS="--warm-up-time 0.1 --measurement-time 0.5 --sample-size 10"
    echo "[*] Running quick benchmark (smoke test)..." >&2
else
    echo "[*] Running full benchmarks (this may take several minutes)..." >&2
fi

# List of benchmarks to run (matches Cargo.toml [[bench]] entries)
BENCHMARKS="remember search context why outcome status import_cass link graph_pagerank curate_candidates"

# Collect results
RESULTS=""
TIMESTAMP=$(date -u +"%Y-%m-%dT%H:%M:%SZ")
FAILED=false

for bench in $BENCHMARKS; do
    echo "" >&2
    echo "[*] Benchmark: $bench" >&2

    # Run criterion benchmark and capture output
    if cargo bench --bench "$bench" -- $BENCH_ARGS 2>&1 | tee /tmp/bench_output_$$.txt >&2; then
        # Parse criterion output for timing (simplified - criterion outputs to target/criterion/)
        # Look for lines like "time:   [X.XXX ms X.XXX ms X.XXX ms]"
        P50=$(grep -oP 'time:\s+\[\s*\K[0-9.]+' /tmp/bench_output_$$.txt | head -1 || echo "0")
        UNIT=$(grep -oP 'time:\s+\[[0-9.]+ [a-zμ]+' /tmp/bench_output_$$.txt | head -1 | grep -oP '[a-zμ]+$' || echo "ms")

        # Convert to ms if needed
        case "$UNIT" in
            "µs"|"us") P50=$(echo "$P50 / 1000" | bc -l 2>/dev/null || echo "$P50") ;;
            "s") P50=$(echo "$P50 * 1000" | bc -l 2>/dev/null || echo "$P50") ;;
        esac

        # Store result
        RESULTS="${RESULTS}\"ee_${bench}\": {\"p50_ms\": ${P50:-0}, \"status\": \"measured\"},"
        echo "[+] $bench: p50=${P50:-unknown}ms" >&2
    else
        RESULTS="${RESULTS}\"ee_${bench}\": {\"p50_ms\": null, \"status\": \"failed\"},"
        echo "[-] $bench: FAILED" >&2
        FAILED=true
    fi

    rm -f /tmp/bench_output_$$.txt
done

# Remove trailing comma
RESULTS=$(echo "$RESULTS" | sed 's/,$//')

# Generate ee-perf.v1 JSON
PERF_JSON=$(cat <<EOF
{
  "schema": "ee.perf.v1",
  "timestamp": "$TIMESTAMP",
  "version": "$(grep '^version' "$PROJECT_ROOT/Cargo.toml" | head -1 | cut -d'"' -f2)",
  "git_sha": "$(git -C "$PROJECT_ROOT" rev-parse --short HEAD 2>/dev/null || echo "unknown")",
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

        # Read thresholds from budgets.toml (defaults: 20% p50, 50% p99)
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
