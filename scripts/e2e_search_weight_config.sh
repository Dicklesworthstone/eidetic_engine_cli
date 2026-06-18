#!/usr/bin/env bash
# bd-1et0v.5 - real-binary E2E for search lexical/semantic fusion weights.
#
# This script proves search weights are not dead config: a temp workspace is
# seeded through public ee commands, indexed, searched twice with different
# `.ee/config.toml` [search] weights, and the real JSON search output must show
# a deterministic per-doc score change or rank-order change.

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

export EE_E2E_KEEP="${EE_E2E_KEEP:-1}"
export EE_E2E_KEEP_ARTIFACTS="${EE_E2E_KEEP_ARTIFACTS:-1}"

# shellcheck source=scripts/lib/e2e_harness.sh
# shellcheck disable=SC1091
source "$REPO_ROOT/scripts/lib/e2e_harness.sh"

harness_init "search_weight_config"

require_jq() {
    if command -v jq >/dev/null 2>&1; then
        _harness_pass "required tool available: jq"
    else
        _harness_fail "required tool missing: jq"
    fi
}

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

assert_ne() {
    local left="${1:?left value required}"
    local right="${2:?right value required}"
    local label="${3:-assert_ne}"
    if [ "$left" != "$right" ]; then
        e2e_log_assert_eq "different" "different" "$label"
        _harness_pass "$label (different)"
    else
        e2e_log_assert_eq "same" "different" "$label"
        _harness_fail "$label: both values were [$left]"
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
        --tags search-weight,e2e \
        --no-auto-link \
        --no-propose-candidates \
        --json
    assert_jq "$output" '.schema == "ee.response.v2"' "remember emits response envelope"
    assert_jq "$output" '.success == true' "remember succeeds"
    memory_id="$(json_scalar "$output" '.data.memory_id // .data.memoryId // empty' "remember memory id")"
    assert_jq "$output" '(.data.memory_id // .data.memoryId // "") | length > 0' "remember returns memory id"
    printf -v "$__id_var" '%s' "$memory_id"
}

write_search_weights() {
    local workspace="${1:?workspace required}"
    local lexical="${2:?lexical weight required}"
    local semantic="${3:?semantic weight required}"
    local graph="${4:?graph weight required}"
    mkdir -p "$workspace/.ee"
    {
        printf '[search]\n'
        printf 'lexical_weight = %s\n' "$lexical"
        printf 'semantic_weight = %s\n' "$semantic"
        printf 'graph_weight = %s\n' "$graph"
    } >"$workspace/.ee/config.toml"
    e2e_log_note "search_weights_written workspace=$workspace lexical=$lexical semantic=$semantic graph=$graph"
}

score_for_doc() {
    local json="${1:?json required}"
    local doc_id="${2:?doc id required}"
    local label="${3:-score_for_doc}"
    json_scalar "$json" 'first(.data.results[]? | select(.docId == "'"$doc_id"'") | .score)' "$label"
}

first_common_doc() {
    local left_json="${1:?left json required}"
    local right_json="${2:?right json required}"
    jq -nre \
        --argjson left "$left_json" \
        --argjson right "$right_json" \
        '$left.data.results[].docId as $id | select(any($right.data.results[]?; .docId == $id)) | $id' \
        2>/dev/null | head -n 1
}

require_jq

with_temp_workspace WS

step "init isolated workspace"
run_ee_capture init_out init init --workspace "$WS" --json
assert_jq "$init_out" '.schema == "ee.response.v2"' "init emits response envelope"
assert_jq "$init_out" '.success == true' "init succeeds"

step "seed weighted-fusion fixtures"
remember_fixture lexical_id "$WS" \
    "Lexical weight sentinel exact tokens: basalt-caliper amber-keyword release-checklist exact-match exact-match exact-match."
remember_fixture semantic_id "$WS" \
    "Semantic embeddings should retrieve conceptually similar memories about vector meaning, neural recall, and model2vec retrieval even when exact token overlap is sparse."
remember_fixture mixed_id "$WS" \
    "Hybrid retrieval balances exact-match words with vector recall when release-checklist work depends on semantic embeddings."
remember_fixture distractor_id "$WS" \
    "A kitchen inventory note about spoons and cereal should not dominate a search-weight fusion query."
e2e_log_note "fixture_ids lexical=$lexical_id semantic=$semantic_id mixed=$mixed_id distractor=$distractor_id"

step "rebuild derived search index"
run_ee_capture index_out index_rebuild index rebuild --workspace "$WS" --json
assert_jq "$index_out" '.schema == "ee.response.v2"' "index rebuild emits response envelope"
assert_jq "$index_out" '.success == true' "index rebuild succeeds"

QUERY="exact-match release-checklist semantic embeddings vector recall"

step "search with lexical-heavy weights"
write_search_weights "$WS" "0.95" "0.05" "0.0"
run_ee_capture lexical_config config_get_lexical config get search.lexical_weight --workspace "$WS" --json
assert_jq "$lexical_config" '.schema == "ee.response.v2" and .success == true' "lexical weight config get succeeds"
assert_jq "$lexical_config" '.data.key == "search.lexical_weight" and .data.value == "0.95"' "lexical-heavy config is resolved"
run_ee_capture lexical_search search_lexical_heavy search "$QUERY" \
    --workspace "$WS" \
    --limit 5 \
    --relevance-floor 0.0 \
    --source-mode hybrid \
    --explain \
    --json
assert_jq "$lexical_search" '.schema == "ee.response.v2" and .success == true' "lexical-heavy search succeeds"
assert_jq "$lexical_search" '(.data.results // []) | length >= 2' "lexical-heavy search returns multiple results"
assert_jq "$lexical_search" 'any(.data.results[]?; has("lexicalScore"))' "lexical-heavy search exposes lexical scores"
assert_jq "$lexical_search" 'any(.data.results[]?; has("fastScore") or has("qualityScore"))' "lexical-heavy search exposes semantic scores"
assert_jq "$lexical_search" 'any(.data.results[]?.explanation.factors[]?; .name == "configured_fusion_weight")' \
    "lexical-heavy explanations include configured fusion weight factor"

step "search with semantic-heavy weights"
write_search_weights "$WS" "0.05" "0.95" "0.0"
run_ee_capture semantic_config config_get_semantic config get search.semantic_weight --workspace "$WS" --json
assert_jq "$semantic_config" '.schema == "ee.response.v2" and .success == true' "semantic weight config get succeeds"
assert_jq "$semantic_config" '.data.key == "search.semantic_weight" and .data.value == "0.95"' "semantic-heavy config is resolved"
run_ee_capture semantic_search search_semantic_heavy search "$QUERY" \
    --workspace "$WS" \
    --limit 5 \
    --relevance-floor 0.0 \
    --source-mode hybrid \
    --explain \
    --json
assert_jq "$semantic_search" '.schema == "ee.response.v2" and .success == true' "semantic-heavy search succeeds"
assert_jq "$semantic_search" '(.data.results // []) | length >= 2' "semantic-heavy search returns multiple results"
assert_jq "$semantic_search" 'any(.data.results[]?; has("lexicalScore"))' "semantic-heavy search exposes lexical scores"
assert_jq "$semantic_search" 'any(.data.results[]?; has("fastScore") or has("qualityScore"))' "semantic-heavy search exposes semantic scores"
assert_jq "$semantic_search" 'any(.data.results[]?.explanation.factors[]?; .name == "configured_fusion_weight")' \
    "semantic-heavy explanations include configured fusion weight factor"

step "compare ranking and score effects"
lexical_order="$(json_scalar "$lexical_search" '[.data.results[]?.docId] | join(",")' "lexical-heavy order")"
semantic_order="$(json_scalar "$semantic_search" '[.data.results[]?.docId] | join(",")' "semantic-heavy order")"
lexical_top="$(json_scalar "$lexical_search" '.data.results[0].docId' "lexical-heavy top doc")"
semantic_top="$(json_scalar "$semantic_search" '.data.results[0].docId' "semantic-heavy top doc")"
e2e_log_note "search_weight_orders lexical_top=$lexical_top semantic_top=$semantic_top lexical_order=$lexical_order semantic_order=$semantic_order"

if [ "$lexical_order" != "$semantic_order" ]; then
    _harness_pass "configured weights changed result order"
    e2e_log_assert_eq "different" "different" "configured weights changed result order"
else
    log_drop 1 "configured weights preserved order for this corpus; asserting per-doc score changed instead"
fi

common_doc="$(first_common_doc "$lexical_search" "$semantic_search")"
if [ -z "$common_doc" ]; then
    _harness_fail "weighted searches should share at least one result docId"
else
    lexical_score="$(score_for_doc "$lexical_search" "$common_doc" "lexical-heavy common score")"
    semantic_score="$(score_for_doc "$semantic_search" "$common_doc" "semantic-heavy common score")"
    e2e_log_note "search_weight_score_delta doc=$common_doc lexical_score=$lexical_score semantic_score=$semantic_score"
    assert_ne "$lexical_score" "$semantic_score" "configured weights changed common-doc score"
fi

end_temp_workspace
harness_summary
