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
BASELINE_FILE="$PROJECT_ROOT/benches/baselines/v0.1.json"
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
        BENCHMARKS="remember search context pack_size why outcome status import_cass link graph_pagerank curate_candidates"
        BENCH_ARGS="--warm-up-time 0.5 --measurement-time 2 --sample-size 20"
        PROFILE_CLASS="nightly_ci"
        WORKLOAD_TIER="medium"
        RELEASE_BLOCKING=false
        ;;
    stress)
        BENCHMARKS="remember search context pack_size why outcome status import_cass link graph_pagerank curate_candidates"
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
    regression_status="${8:-not_checked}"

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
      \"regression_status\": \"$regression_status\",
      \"budget_mode\": \"advisory\"
    }"
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

for bench in $BENCHMARKS; do
    echo "" >&2
    echo "[*] Benchmark: $bench" >&2
    if [ "$PROFILE" = "ci-smoke" ] && [ "$bench" = "status" ]; then
        run_status_smoke
    else
        run_criterion_bench "$bench"
    fi
done

if [ "$PROFILE" = "ci-smoke" ]; then
    run_pack_replay_freshness_smoke
fi

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

if [ "$FAILED" = "true" ]; then
    echo "" >&2
    echo "[-] Some benchmarks failed" >&2
    exit 1
fi

echo "" >&2
echo "[+] All benchmarks completed" >&2
exit 0
