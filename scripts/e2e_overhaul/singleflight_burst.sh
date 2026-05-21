#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# shellcheck source=scripts/e2e_overhaul/lib/shared.sh
source "$SCRIPT_DIR/lib/shared.sh"

require_jq

TIER="${EE_SINGLEFLIGHT_BURST_TIER:-small}"
case "$TIER" in
    small)
        IDENTICAL="${EE_SINGLEFLIGHT_BURST_IDENTICAL:-6}"
        DISTINCT="${EE_SINGLEFLIGHT_BURST_DISTINCT:-3}"
        NODES="${EE_SINGLEFLIGHT_BURST_NODES:-64}"
        ;;
    large)
        IDENTICAL="${EE_SINGLEFLIGHT_BURST_IDENTICAL:-32}"
        DISTINCT="${EE_SINGLEFLIGHT_BURST_DISTINCT:-8}"
        NODES="${EE_SINGLEFLIGHT_BURST_NODES:-10000}"
        ;;
    *)
        echo "singleflight_burst: unsupported EE_SINGLEFLIGHT_BURST_TIER=$TIER" >&2
        exit 2
        ;;
esac

RUNS="${EE_SINGLEFLIGHT_BURST_RUNS:-3}"
if [ "$RUNS" -lt 3 ]; then
    echo "singleflight_burst: EE_SINGLEFLIGHT_BURST_RUNS must be >= 3" >&2
    exit 2
fi

epic_setup "singleflight_burst"

IDENTICAL_HASH=""
DISTINCT_HASH=""
run_index=1
while [ "$run_index" -le "$RUNS" ]; do
    json="$(ee_workspace graph feature-enrichment \
        --dry-run \
        --singleflight-burst "$IDENTICAL" \
        --singleflight-distinct "$DISTINCT" \
        --singleflight-nodes "$NODES" \
        --max-features "$NODES" \
        --json)"

    artifact="$EPIC_WORKSPACE/singleflight_burst_run_${run_index}.json"
    printf '%s\n' "$json" > "$artifact"
    e2e_log_note "singleflight_burst_artifact run=$run_index path=$artifact"

    assert_jq "$json" '.success' "true" "run_${run_index}_success"
    assert_jq "$json" '.data.schema' "ee.graph.feature_enrichment.singleflight_burst.v1" "run_${run_index}_schema"
    assert_jq "$json" '.data.summary.identicalLeaderCount' "1" "run_${run_index}_one_identical_leader"
    assert_jq "$json" '.data.summary.identicalFollowerCount' "$((IDENTICAL - 1))" "run_${run_index}_identical_followers"
    assert_jq "$json" '.data.summary.distinctLeaderCount' "$DISTINCT" "run_${run_index}_distinct_leaders"
    assert_jq "$json" '.data.summary.distinctFollowerCount' "0" "run_${run_index}_no_distinct_followers"
    assert_jq "$json" '.data.summary.executionCount' "$((DISTINCT + 1))" "run_${run_index}_expensive_execution_count"
    assert_jq "$json" '.data.resultHashes.identicalUniqueCount' "1" "run_${run_index}_identical_hash_stability"
    assert_jq "$json" '.data.resultHashes.distinctUniqueCount' "$DISTINCT" "run_${run_index}_distinct_hashes_do_not_collapse"
    assert_jq "$json" '.data.summary.timeoutCount' "0" "run_${run_index}_no_timeouts"
    assert_jq "$json" '.data.summary.leaderFailureCount' "0" "run_${run_index}_no_leader_failures"

    current_identical_hash="$(printf '%s' "$json" | jq -r '.data.resultHashes.identical[0]')"
    current_distinct_hash="$(printf '%s' "$json" | jq -r '.data.resultHashes.distinct | join(",")')"
    if [ "$run_index" -eq 1 ]; then
        IDENTICAL_HASH="$current_identical_hash"
        DISTINCT_HASH="$current_distinct_hash"
    else
        e2e_log_assert_eq "$current_identical_hash" "$IDENTICAL_HASH" "run_${run_index}_identical_hash_matches_run_1" || true
        e2e_log_assert_eq "$current_distinct_hash" "$DISTINCT_HASH" "run_${run_index}_distinct_hashes_match_run_1" || true
    fi

    _e2e_emit_event "singleflight_burst_summary" \
        "bead_id" "bd-gni47.4" \
        "tier" "$TIER" \
        "run_index" "$run_index" \
        "identical_requests" "$IDENTICAL" \
        "distinct_requests" "$DISTINCT" \
        "node_count" "$NODES" \
        "leader_count" "$(printf '%s' "$json" | jq -r '.data.summary.identicalLeaderCount')" \
        "follower_count" "$(printf '%s' "$json" | jq -r '.data.summary.identicalFollowerCount')" \
        "distinct_leader_count" "$(printf '%s' "$json" | jq -r '.data.summary.distinctLeaderCount')" \
        "timeout_count" "$(printf '%s' "$json" | jq -r '.data.summary.timeoutCount')" \
        "latency_p50_ms" "$(printf '%s' "$json" | jq -r '.data.latencyMs.p50')" \
        "latency_p95_ms" "$(printf '%s' "$json" | jq -r '.data.latencyMs.p95')" \
        "latency_p99_ms" "$(printf '%s' "$json" | jq -r '.data.latencyMs.p99')" \
        "identical_hash" "$current_identical_hash" \
        "distinct_hashes" "$current_distinct_hash"

    run_index=$((run_index + 1))
done

if [ "$EE_TEST_LOG_ASSERTS_FAIL" -ne 0 ]; then
    echo "singleflight_burst: $EE_TEST_LOG_ASSERTS_FAIL assertions failed" >&2
    exit 1
fi

echo "singleflight_burst passed: tier=$TIER runs=$RUNS events=$EE_TEST_LOG_PATH" >&2
