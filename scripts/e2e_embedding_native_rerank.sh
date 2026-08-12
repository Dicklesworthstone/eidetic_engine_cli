#!/usr/bin/env bash
# shellcheck disable=SC2154
# bd-2vq2z.6 - real-binary E2E coverage for embedding-native rerank fallback.
#
# This script proves the no-silent-rerank path: a fresh workspace with no
# registered local reranker must still run real search, emit an explicit
# ee.rerank_posture.v1 permanent capability advisory, and avoid fake rerankScore
# fields. A separate central lane can exercise real model-backed reranking once
# a cached reranker artifact is available.

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

export EE_E2E_KEEP="${EE_E2E_KEEP:-1}"
export EE_E2E_KEEP_ARTIFACTS="${EE_E2E_KEEP_ARTIFACTS:-1}"

# shellcheck source=scripts/lib/e2e_harness.sh
# shellcheck disable=SC1091
source "$REPO_ROOT/scripts/lib/e2e_harness.sh"

harness_init "embedding_native_rerank"

run_ee_capture() {
    local __out_var="${1:?output variable required}"
    local label="${2:?label required}"
    shift 2
    local __captured
    local __exit_code
    __captured="$(e2e_log_command "$EE_BIN" "$@")"
    __exit_code=$?
    printf -v "$__out_var" '%s' "$__captured"
    if [ "$__exit_code" -eq 0 ]; then
        _harness_pass "$label command exit 0"
    else
        _harness_fail "$label command exit $__exit_code"
    fi
    return 0
}

json_scalar() {
    local json="${1:?json required}"
    local filter="${2:?jq filter required}"
    local label="${3:-json_scalar}"
    local value
    if value="$(printf '%s' "$json" | jq -er "$filter" 2>/dev/null)"; then
        printf '%s' "$value"
        return 0
    fi
    _harness_fail "$label: jq filter failed [$filter]"
    printf ''
    return 1
}

require_jq() {
    if command -v jq >/dev/null 2>&1; then
        _harness_pass "required tool available: jq"
    else
        _harness_fail "required tool missing: jq"
    fi
}

remember_fixture() {
    local __id_var="${1:?memory id variable required}"
    local workspace="${2:?workspace required}"
    local content="${3:?content required}"
    local output
    local memory_id

    run_ee_capture output \
        "remember fixture" \
        remember \
        "$content" \
        --workspace "$workspace" \
        --level semantic \
        --kind fact \
        --tags rerank,e2e \
        --no-auto-link \
        --no-propose-candidates \
        --json
    assert_jq "$output" '.schema == "ee.response.v2"' "remember emits response envelope"
    assert_jq "$output" '.success == true' "remember succeeds"
    memory_id="$(json_scalar "$output" '.data.memory_id // .data.memoryId // empty' "remember memory id")"
    assert_jq "$output" '(.data.memory_id // .data.memoryId // "") | length > 0' "remember returns memory id"
    printf -v "$__id_var" '%s' "$memory_id"
}

require_jq

with_temp_workspace WS

step "init isolated workspace"
run_ee_capture init_out init init --workspace "$WS" --json
assert_jq "$init_out" '.schema == "ee.response.v2"' "init emits response envelope"
assert_jq "$init_out" '.success == true' "init succeeds"

step "remember rerank-sensitive search fixtures"
remember_fixture primary_id "$WS" \
    "Cross-encoder rerank should preserve release verification memories above generic release notes."
remember_fixture secondary_id "$WS" \
    "Fusion-only search remains acceptable when the local reranker model is unavailable."
remember_fixture distractor_id "$WS" \
    "A garden calendar note mentions release dates but not semantic retrieval or reranking."
e2e_log_note "fixture_ids primary=$primary_id secondary=$secondary_id distractor=$distractor_id"

step "rebuild derived search index"
run_ee_capture index_out index_rebuild index rebuild --workspace "$WS" --json
assert_jq "$index_out" '.schema == "ee.response.v2"' "index rebuild emits response envelope"
assert_jq "$index_out" '.success == true' "index rebuild succeeds"

QUERY="semantic rerank release verification"

step "search real JSON output with no registered reranker"
run_ee_capture search_one search_fusion_only search "$QUERY" --workspace "$WS" --limit 5 --json
assert_jq "$search_one" '.schema == "ee.response.v2"' "search emits response envelope"
assert_jq "$search_one" '.success == true' "search succeeds"
assert_jq "$search_one" '.data.command == "search"' "search data command is stable"
assert_jq "$search_one" '(.data.results // []) | length >= 1' "search returns at least one fixture result"
assert_jq "$search_one" '.data.rerank.schema == "ee.rerank_posture.v1"' "rerank posture schema is emitted"
assert_jq "$search_one" '.data.rerank.configured == "auto"' "rerank posture records auto mode"
assert_jq "$search_one" '.data.rerank.mode == "fusion_only_degraded"' "missing reranker degrades to fusion-only"
assert_jq "$search_one" '.data.rerank.available == false' "missing reranker is not reported available"
assert_jq "$search_one" '.data.rerank.scoreKind == "rrf_fused"' "fusion-only scoreKind is explicit"
assert_jq "$search_one" '.data.rerank.degradedCode == "rerank_model_unavailable"' "rerank degraded code is explicit"
assert_jq "$search_one" '.data.rerank | has("permanent") | not' "rerank permanence is scoped to the advisory"
assert_jq "$search_one" '.data.rerank.advisory.code == "rerank_model_unavailable"' \
    "rerank posture carries one structured advisory"
assert_jq "$search_one" '.data.rerank.advisory.permanent == true' \
    "structured rerank advisory is permanent"
assert_jq "$search_one" '.data.rerank.advisory.message == "No usable local reranker is registered. Search is using fusion-only ranking. Network download is unavailable, but a verified offline reranker artifact can be imported explicitly."' \
    "rerank advisory carries the canonical permanent message"
assert_jq "$search_one" '.data.rerank.advisory.repair == null' \
    "rerank advisory does not present a path template as automatic repair"
assert_jq "$search_one" '.data.rerank.advisory.resolution == "automatic_repair_unavailable"' \
    "rerank advisory names the unavailable automatic-repair posture"
assert_jq "$search_one" '.data.rerank.rerankScoreCount == 0' "no rerank scores are counted"
assert_jq "$search_one" '.data.metrics.sourceCounts.reranked == 0' "metrics do not fake reranked source hits"
assert_jq "$search_one" '.data.metrics.fieldCoverage.rerankScoreCount == 0' "field coverage does not fake rerankScore"
assert_jq "$search_one" '[.data.results[]? | has("rerankScore")] | any | not' "fusion-only results omit rerankScore"
assert_jq "$search_one" '[.data.degraded[]? | select(.code == "rerank_model_unavailable")] | length == 0' \
    "permanent reranker posture does not repeat in degraded list"

first_order="$(json_scalar "$search_one" '[.data.results[]?.docId] | join(",")' "first search order")"
rerank_mode="$(json_scalar "$search_one" '.data.rerank.mode' "rerank mode")"
degraded_code="$(json_scalar "$search_one" '.data.rerank.degradedCode' "rerank degraded code")"
degraded_severity="$(json_scalar "$search_one" '.data.rerank.advisory.severity' "rerank advisory severity")"
degraded_resolution="$(json_scalar "$search_one" '.data.rerank.advisory.resolution' "rerank advisory resolution")"
e2e_log_note "pre_post_rerank_order mode=$rerank_mode degraded=$degraded_code order=$first_order"
e2e_log_note "rerank_advisory_detail code=$degraded_code severity=$degraded_severity resolution=$degraded_resolution"

step "repeat same query to prove deterministic fusion-only order"
run_ee_capture search_two search_fusion_only_repeat search "$QUERY" --workspace "$WS" --limit 5 --json
assert_jq "$search_two" '.success == true' "repeat search succeeds"
second_order="$(json_scalar "$search_two" '[.data.results[]?.docId] | join(",")' "second search order")"
assert_eq "$second_order" "$first_order" "same query keeps byte-stable result order list"
assert_jq "$search_two" '.data.rerank == {
    "schema": "ee.rerank_posture.v1",
    "mode": "fusion_only_degraded",
    "configured": "auto",
    "topK": 50,
    "rerankScoreCount": 0,
    "scoreKind": "rrf_fused",
    "available": false,
    "degradedCode": "rerank_model_unavailable",
    "advisory": {
        "code": "rerank_model_unavailable",
        "severity": "low",
        "permanent": true,
        "message": "No usable local reranker is registered. Search is using fusion-only ranking. Network download is unavailable, but a verified offline reranker artifact can be imported explicitly.",
        "repair": null,
        "resolution": "automatic_repair_unavailable"
    },
    "advisorySummary": {
        "scope": "process",
        "permanent": true,
        "distinctCount": 1,
        "emittedCount": 1,
        "suppressedCount": 0,
        "sessionOccurrenceCount": 1,
        "sessionSuppressedCount": 0
    }
}' "repeat search keeps identical rerank posture JSON"

end_temp_workspace
harness_summary
