#!/usr/bin/env bash
# F4.a - Graph determinism harness.
#
# Exercises graph-facing JSON surfaces three times each, strips known volatile
# fields, canonicalizes with jq -S, and hash-compares the canonical payloads.
# Missing future surfaces are recorded as structured TODO notes so the harness
# can land before the full GraphAccretion command surface does.
#
# Bead: bd-8jvg.1

set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/e2e_overhaul/lib/shared.sh
source "$SCRIPT_DIR/lib/shared.sh"

GRAPH_DETERMINISM_WATCHDOG_PID=""

start_graph_determinism_watchdog() {
    local max_seconds="${EE_GRAPH_DETERMINISM_MAX_SECONDS:-180}"
    (
        sleep "$max_seconds"
        echo "graph_determinism: timed out after ${max_seconds}s" >&2
        kill -TERM "$$" 2>/dev/null || true
    ) &
    GRAPH_DETERMINISM_WATCHDOG_PID="$!"
}

stop_graph_determinism_watchdog() {
    if [ -n "$GRAPH_DETERMINISM_WATCHDOG_PID" ]; then
        kill "$GRAPH_DETERMINISM_WATCHDOG_PID" 2>/dev/null || true
    fi
}

hash_stdin() {
    if command -v blake3sum >/dev/null 2>&1; then
        blake3sum | awk '{print $1}'
    else
        shasum -a 256 | awk '{print $1}'
    fi
}

VOLATILE_FIELD_NAMES=(
    generatedAt
    generated_at
    computed_at
    last_accessed
    last_accessed_at
    last_seen_at
    last_used_at
    audit_ts
    elapsedMs
    elapsed_ms
    startedAt
    started_at
    endedAt
    ended_at
    ts
    timestamp
    runIndex
    run_index
    ee_binary_hash
    databasePath
    workspacePath
    indexDir
    snapshotRefreshedAt
    runDurationMs
    witnessElapsedMs
    witnessRecordedAt
    algorithmStartedAt
)

volatile_field_delete_filter() {
    local filter='walk(if type == "object" then del('
    local separator=""
    local field

    for field in "${VOLATILE_FIELD_NAMES[@]}"; do
        filter="${filter}${separator}.${field}"
        separator=","
    done

    printf '%s) else . end)\n' "$filter"
}

strip_variable_fields() {
    jq "$(volatile_field_delete_filter)"
}

ee_with_timeout() {
    local timeout_seconds="${EE_GRAPH_DETERMINISM_TIMEOUT_SECONDS:-20}"
    perl -e 'my $timeout = shift @ARGV; alarm $timeout; exec @ARGV' \
        "$timeout_seconds" "$EE_BINARY" "$@"
}

hash_sample_for_run() {
    local run_index="$1"
    printf '{"schema":"ee.graph_determinism.self_test.v1","data":{"items":["a","b","c"]}}\n' \
        | strip_variable_fields \
        | if [ "${GRAPH_DETERMINISM_INJECT_NONDETERMINISM:-0}" = "1" ]; then
            jq --arg run_index "$run_index" '. + {injectedHashMapIterationOrder: $run_index}'
        else
            cat
        fi \
        | jq -S '.' \
        | hash_stdin
}

run_injection_self_test() {
    require_jq
    GRAPH_DETERMINISM_INJECT_NONDETERMINISM=1
    export GRAPH_DETERMINISM_INJECT_NONDETERMINISM

    local h1 h2 h3
    h1=$(hash_sample_for_run 1)
    h2=$(hash_sample_for_run 2)
    h3=$(hash_sample_for_run 3)
    if [ "$h1" != "$h2" ] && [ "$h2" != "$h3" ] && [ "$h1" != "$h3" ]; then
        printf 'graph_determinism injection self-test caught divergence\n'
        return 0
    fi
    printf 'graph_determinism injection self-test failed: %s %s %s\n' "$h1" "$h2" "$h3" >&2
    return 3
}

canonical_hash_for_run() {
    local output
    local status
    local run_index="$1"
    shift
    output=$(ee_with_timeout "$@" --workspace "$EPIC_WORKSPACE" 2>/dev/null)
    status=$?
    if [ "$status" -ne 0 ] || ! printf '%s' "$output" | jq . >/dev/null 2>&1; then
        return 1
    fi
    printf '%s' "$output" \
        | strip_variable_fields \
        | if [ "${GRAPH_DETERMINISM_INJECT_NONDETERMINISM:-0}" = "1" ]; then
            jq --arg run_index "$run_index" '. + {injectedHashMapIterationOrder: $run_index}'
        else
            cat
        fi \
        | jq -S '.' \
        | hash_stdin
}

run_json_surface_3x() {
    local mode="$1"
    local name="$2"
    local tracking_bead="$3"
    local unavailable_description="$4"
    shift 4

    local h1 h2 h3
    h1=$(canonical_hash_for_run 1 "$@") || h1=""
    if [ -z "$h1" ]; then
        if [ "$mode" = "future" ]; then
            todo_assert "graph_determinism_${name}_available" "$tracking_bead" "$unavailable_description"
        else
            e2e_log_assert_eq "json_unavailable" "json_available" "graph_determinism_${name}_available"
        fi
        return 0
    fi
    h2=$(canonical_hash_for_run 2 "$@") || h2=""
    h3=$(canonical_hash_for_run 3 "$@") || h3=""
    if [ -z "$h2" ] || [ -z "$h3" ]; then
        e2e_log_assert_eq "json_unavailable_after_first_run" "json_available" "graph_determinism_${name}_stable_availability"
        return 0
    fi
    if [ -n "$h1" ] && [ "$h1" = "$h2" ] && [ "$h2" = "$h3" ]; then
        e2e_log_assert_eq "true" "true" "graph_determinism_${name}"
        e2e_log_note "graph_determinism_${name}_hash=$h1"
    else
        e2e_log_assert_eq "h1=$h1 h2=$h2 h3=$h3" "all_equal" "graph_determinism_${name}"
    fi
}

seed_graph_determinism_memory() {
    local content="$1"
    ee_with_timeout remember "$content" \
        --workspace "$EPIC_WORKSPACE" --level semantic --kind note --json 2>/dev/null \
        | jq -r '.data.memory.id // .data.public_id // .data.memory_id // .data.id // empty' 2>/dev/null
}

seed_graph_determinism_causal_edge() {
    local edge_id="$1"
    local failure_id="$2"
    local cause_id="$3"
    local contribution_score="$4"
    local computed_at="$5"
    ee_with_timeout diag causal-edge \
        --edge-id "$edge_id" \
        --failure-id "$failure_id" \
        --candidate-cause-id "$cause_id" \
        --contribution-score "$contribution_score" \
        --computed-at "$computed_at" \
        --evidence-uri "agent-mail://bd-qnfw.4/$edge_id" \
        --workspace "$EPIC_WORKSPACE" \
        --json >/dev/null 2>/dev/null
}

if [ "${BASH_SOURCE[0]}" != "$0" ]; then
    return 0
fi

if [ "${1:-}" = "--self-test-injection" ]; then
    run_injection_self_test
    exit $?
fi

require_jq
start_graph_determinism_watchdog
trap stop_graph_determinism_watchdog EXIT
epic_setup "epic_F4_graph_determinism"
trap 'stop_graph_determinism_watchdog; _epic_teardown' EXIT

GRAPH_CPU_ARCH="$(uname -m 2>/dev/null || printf 'unknown')"
GRAPH_RUSTC_VERSION="$(rustc --version 2>/dev/null || printf 'rustc unavailable')"
e2e_log_note "graph_determinism_environment cpu_arch=${GRAPH_CPU_ARCH} rustc=${GRAPH_RUSTC_VERSION} ee_binary=${EE_BINARY}"

MEM_A=$(ee_with_timeout remember "Graph determinism source memory." \
    --workspace "$EPIC_WORKSPACE" --level semantic --kind note --json 2>/dev/null \
    | jq -r '.data.memory.id // .data.memory_id // .data.id // empty' 2>/dev/null || true)
MEM_B=$(ee_with_timeout remember "Graph determinism destination memory." \
    --workspace "$EPIC_WORKSPACE" --level semantic --kind note --json 2>/dev/null \
    | jq -r '.data.memory.id // .data.memory_id // .data.id // empty' 2>/dev/null || true)
ANY_MEM="${MEM_A:-$MEM_B}"
e2e_log_note "graph_determinism_seed mem_a=${MEM_A:-?} mem_b=${MEM_B:-?}"

CAUSAL_FAILURE=$(seed_graph_determinism_memory "Graph determinism causal failure memory." || true)
CAUSAL_BRIDGE=$(seed_graph_determinism_memory "Graph determinism causal bridge memory." || true)
CAUSAL_ROOT=$(seed_graph_determinism_memory "Graph determinism causal root memory." || true)
if [ -n "${CAUSAL_FAILURE:-}" ] && [ -n "${CAUSAL_BRIDGE:-}" ] && [ -n "${CAUSAL_ROOT:-}" ] \
    && seed_graph_determinism_causal_edge "cev_graph_determinism_bridge" \
        "$CAUSAL_FAILURE" "$CAUSAL_BRIDGE" "0.82" "2026-05-15T12:30:00Z" \
    && seed_graph_determinism_causal_edge "cev_graph_determinism_root" \
        "$CAUSAL_BRIDGE" "$CAUSAL_ROOT" "0.91" "2026-05-15T12:31:00Z"; then
    e2e_log_note "graph_determinism_causal_seed failure=${CAUSAL_FAILURE} bridge=${CAUSAL_BRIDGE} root=${CAUSAL_ROOT}"
else
    e2e_log_note "graph_determinism_causal_seed_unavailable"
    CAUSAL_FAILURE=""
fi

# Required future GraphAccretion surfaces named by bd-8jvg.1.
run_json_surface_3x future "insights_full" "bd-t6wd.1" \
    "ee insights full JSON surface is not implemented yet." \
    insights --json
run_json_surface_3x future "insights_section_centrality" "bd-t6wd.1" \
    "ee insights --section centrality JSON surface is not implemented yet." \
    insights --section centrality --json
run_json_surface_3x future "insights_section_contradictions" "bd-t6wd.1" \
    "ee insights --section contradictions JSON surface is not implemented yet." \
    insights --section contradictions --json
run_json_surface_3x future "context_explain" "bd-t6wd.2" \
    "ee context --explain JSON surface is not implemented yet." \
    context "graph determinism harness" --max-tokens 1000 --json --explain
run_json_surface_3x future "status_skyline" "bd-t6wd.2" \
    "ee status --skyline JSON surface is not implemented yet." \
    status --skyline --json
run_json_surface_3x required "health_robot_insights" "bd-zx2v.4" \
    "ee health --robot-insights JSON surface should be deterministic." \
    health --robot-insights --json

# Shipped graph-adjacent surfaces that give the harness real coverage today.
run_json_surface_3x required "context_explain_performance" "bd-17c65" \
    "ee context --explain-performance JSON surface should be present." \
    context "graph determinism harness" --max-tokens 1000 --json --explain-performance
if [ -n "${ANY_MEM:-}" ]; then
    run_json_surface_3x required "why_graph_badges" "bd-t6wd.2" \
        "ee why JSON output should be deterministic and ready for graph badges." \
        why "$ANY_MEM" --json
else
    todo_assert "graph_determinism_why_seed_available" "bd-8jvg.1" \
        "Unable to seed a memory for ee why graph determinism coverage."
fi
if [ -n "${CAUSAL_FAILURE:-}" ]; then
    run_json_surface_3x required "why_causal_explain" "bd-qnfw.4" \
        "ee why --causal-explain JSON output should be deterministic for a seeded three-memory causal chain." \
        why "$CAUSAL_FAILURE" --causal-explain --json
else
    todo_assert "graph_determinism_why_causal_seed_available" "bd-qnfw.4" \
        "Unable to seed causal evidence for ee why --causal-explain determinism coverage."
fi
run_json_surface_3x required "graph_centrality_refresh_dry_run" "bd-rnfh.1" \
    "ee graph centrality-refresh --dry-run JSON surface should be present." \
    graph centrality-refresh --dry-run --json
run_json_surface_3x required "graph_feature_enrichment_dry_run" "bd-rnfh.1" \
    "ee graph feature-enrichment --dry-run JSON surface should be present." \
    graph feature-enrichment --dry-run --json
run_json_surface_3x required "curate_candidates_read_only" "bd-17c65.7" \
    "ee curate candidates read-only surface should be deterministic." \
    curate candidates --json

if [ "$EE_TEST_LOG_ASSERTS_FAIL" -gt 0 ]; then
    exit 3
fi
