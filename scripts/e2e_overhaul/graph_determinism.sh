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
    output=$("$EE_BINARY" "$@" --workspace "$EPIC_WORKSPACE" 2>/dev/null)
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
    h2=$(canonical_hash_for_run 2 "$@") || h2=""
    h3=$(canonical_hash_for_run 3 "$@") || h3=""
    if [ -z "$h1" ] || [ -z "$h2" ] || [ -z "$h3" ]; then
        if [ "$mode" = "future" ]; then
            todo_assert "graph_determinism_${name}_available" "$tracking_bead" "$unavailable_description"
        else
            e2e_log_assert_eq "json_unavailable" "json_available" "graph_determinism_${name}_available"
        fi
        return 0
    fi
    if [ -n "$h1" ] && [ "$h1" = "$h2" ] && [ "$h2" = "$h3" ]; then
        e2e_log_assert_eq "true" "true" "graph_determinism_${name}"
        e2e_log_note "graph_determinism_${name}_hash=$h1"
    else
        e2e_log_assert_eq "h1=$h1 h2=$h2 h3=$h3" "all_equal" "graph_determinism_${name}"
    fi
}

if [ "${BASH_SOURCE[0]}" != "$0" ]; then
    return 0
fi

if [ "${1:-}" = "--self-test-injection" ]; then
    run_injection_self_test
    exit $?
fi

require_jq
epic_setup "epic_F4_graph_determinism"

seed_corpus
MEM_A=$("$EE_BINARY" remember "Graph determinism source memory." \
    --workspace "$EPIC_WORKSPACE" --level semantic --kind note --json 2>/dev/null \
    | jq -r '.data.memory.id // .data.memory_id // .data.id // empty' 2>/dev/null || true)
MEM_B=$("$EE_BINARY" remember "Graph determinism destination memory." \
    --workspace "$EPIC_WORKSPACE" --level semantic --kind note --json 2>/dev/null \
    | jq -r '.data.memory.id // .data.memory_id // .data.id // empty' 2>/dev/null || true)
ANY_MEM="${MEM_A:-$MEM_B}"
e2e_log_note "graph_determinism_seed mem_a=${MEM_A:-?} mem_b=${MEM_B:-?}"

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
run_json_surface_3x future "health_contradiction_clusters" "bd-t6wd.2" \
    "ee health --contradiction-clusters JSON surface is not implemented yet." \
    health --contradiction-clusters --json

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
