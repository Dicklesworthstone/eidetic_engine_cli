#!/usr/bin/env bash
# bd-1et0v.20 - retrieval-truth cross-surface posture E2E.
#
# Real-binary, no-Cargo E2E:
#   - one workspace is initialized, seeded, and indexed through public ee JSON
#   - index status, capabilities, model status, and reembed dry-run agree on
#     the shared EmbeddingPosture serializer
#   - doctor discloses foreign embedding env traps without changing top-line
#     posture or implying OpenAI/network usage
#   - search emits an interpretable relevance signal, not only a raw score

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

export EE_E2E_KEEP="${EE_E2E_KEEP:-1}"
export EE_E2E_KEEP_ARTIFACTS="${EE_E2E_KEEP_ARTIFACTS:-1}"

# shellcheck source=scripts/lib/e2e_harness.sh
# shellcheck disable=SC1091
source "$REPO_ROOT/scripts/lib/e2e_harness.sh"

harness_init "retrieval_truth"

require_jq() {
    if command -v jq >/dev/null 2>&1; then
        _harness_pass "required tool available: jq"
    else
        _harness_fail "required tool missing: jq"
    fi
}

run_command_capture() {
    local __out_var="${1:?output variable required}"
    local label="${2:?label required}"
    shift 2
    local __captured
    local __exit_code
    __captured="$(e2e_log_command "$@")"
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

json_compact() {
    local json="${1:?json required}"
    local filter="${2:?jq filter required}"
    local label="${3:-json_compact}"
    local value
    if value="$(printf '%s' "$json" | jq -cS "$filter" 2>/dev/null)" \
        && [ -n "$value" ] \
        && [ "$value" != "null" ]; then
        printf '%s' "$value"
        return 0
    fi
    _harness_fail "$label: jq filter failed or returned null [$filter]"
    printf ''
    return 1
}

remember_fixture() {
    local __id_var="${1:?memory id variable required}"
    local workspace="${2:?workspace required}"
    local content="${3:?content required}"
    local output
    local memory_id

    run_command_capture output \
        "remember fixture" \
        "$EE_BIN" remember \
        "$content" \
        --workspace "$workspace" \
        --level semantic \
        --kind fact \
        --tags retrieval-truth,e2e \
        --no-auto-link \
        --no-propose-candidates \
        --json
    assert_jq "$output" '.schema == "ee.response.v2"' "remember emits response envelope"
    assert_jq "$output" '.success == true' "remember succeeds"
    memory_id="$(json_scalar "$output" '.data.memory_id // .data.memoryId // empty' "remember memory id")"
    assert_jq "$output" '(.data.memory_id // .data.memoryId // "") | length > 0' "remember returns memory id"
    printf -v "$__id_var" '%s' "$memory_id"
}

posture_signature_filter='[
    (.semantic | tostring),
    (.source // ""),
    (.fast_model_id // ""),
    ((.fast_dimension // "") | tostring),
    (.mode // "")
] | @tsv'

require_jq

with_temp_workspace WS

step "init isolated workspace"
run_command_capture init_out init "$EE_BIN" init --workspace "$WS" --json
assert_jq "$init_out" '.schema == "ee.response.v2"' "init emits response envelope"
assert_jq "$init_out" '.success == true' "init succeeds"

step "seed retrieval-truth fixtures"
remember_fixture primary_id "$WS" \
    "Retrieval truth sentinel: semantic posture consistency should explain model mode and score kind."
remember_fixture secondary_id "$WS" \
    "Embedding posture diagnostics must agree across doctor, capabilities, model status, and index status."
remember_fixture distractor_id "$WS" \
    "A garden calendar note mentions release dates but not embedding posture or retrieval truth."
e2e_log_note "fixture_ids primary=$primary_id secondary=$secondary_id distractor=$distractor_id"

step "rebuild derived search index"
run_command_capture rebuild_out index_rebuild "$EE_BIN" index rebuild --workspace "$WS" --json
assert_jq "$rebuild_out" '.schema == "ee.response.v2"' "index rebuild emits response envelope"
assert_jq "$rebuild_out" '.success == true' "index rebuild succeeds"

step "capture posture from index status"
run_command_capture index_status index_status "$EE_BIN" index status --workspace "$WS" --json
assert_jq "$index_status" '.schema == "ee.response.v2"' "index status emits response envelope"
assert_jq "$index_status" '.success == true' "index status succeeds"
assert_jq "$index_status" '.data.embedding.schema == "ee.embedding_posture.v1"' \
    "index status alone exposes embedding posture schema"
assert_jq "$index_status" '.data.embedding.mode | type == "string" and length > 0' \
    "index status alone exposes retrieval mode"
index_posture="$(json_compact "$index_status" '.data.embedding' "index posture json")"
index_signature="$(json_scalar "$index_status" ".data.embedding | $posture_signature_filter" "index posture signature")"
mode="$(json_scalar "$index_status" '.data.embedding.mode' "index posture mode")"
semantic="$(json_scalar "$index_status" '.data.embedding.semantic | tostring' "index posture semantic")"
source="$(json_scalar "$index_status" '.data.embedding.source' "index posture source")"
fast_model_id="$(json_scalar "$index_status" '.data.embedding.fast_model_id' "index posture fast model")"
fast_dimension="$(json_scalar "$index_status" '.data.embedding.fast_dimension | tostring' "index posture dimension")"
e2e_log_note "posture_index mode=$mode semantic=$semantic source=$source fast_model_id=$fast_model_id fast_dimension=$fast_dimension"

step "capture posture from capabilities"
run_command_capture capabilities capabilities "$EE_BIN" capabilities --workspace "$WS" --json --fields full
assert_jq "$capabilities" '.schema == "ee.response.v2"' "capabilities emits response envelope"
assert_jq "$capabilities" '.success == true' "capabilities succeeds"
assert_jq "$capabilities" '.data.index.embedding.schema == "ee.embedding_posture.v1"' \
    "capabilities exposes runtime index embedding posture"
capabilities_posture="$(json_compact "$capabilities" '.data.index.embedding' "capabilities posture json")"
capabilities_signature="$(json_scalar "$capabilities" ".data.index.embedding | $posture_signature_filter" "capabilities posture signature")"
assert_eq "$capabilities_posture" "$index_posture" "capabilities posture JSON matches index status"
assert_eq "$capabilities_signature" "$index_signature" "capabilities semantic/source/model signature matches index status"
assert_jq "$capabilities" '
    [.data.envOverrides[]? | select(.name == "EE_EMBED_MODEL_PATH") | .controls]
    | length == 1 and all(.[]; test("Fault-injection"))
' "EE_EMBED_MODEL_PATH is documented only as fault injection"

step "capture posture from model status"
run_command_capture model_status model_status "$EE_BIN" model status --workspace "$WS" --json
assert_jq "$model_status" '.schema == "ee.response.v2"' "model status emits response envelope"
assert_jq "$model_status" '.success == true' "model status succeeds"
assert_jq "$model_status" '.data.active.posture.schema == "ee.embedding_posture.v1"' \
    "model status exposes active embedding posture"
model_posture="$(json_compact "$model_status" '.data.active.posture' "model posture json")"
model_signature="$(json_scalar "$model_status" ".data.active.posture | $posture_signature_filter" "model posture signature")"
assert_eq "$model_posture" "$index_posture" "model status posture JSON matches index status"
assert_eq "$model_signature" "$index_signature" "model status semantic/source/model signature matches index status"

step "capture posture from reembed dry-run"
run_command_capture reembed reembed_dry_run "$EE_BIN" index reembed --workspace "$WS" --dry-run --json
assert_jq "$reembed" '.schema == "ee.response.v2"' "reembed emits response envelope"
assert_jq "$reembed" '.success == true' "reembed dry-run succeeds"
assert_jq "$reembed" '.data.dry_run == true' "reembed records dry-run"
assert_jq "$reembed" '.data.documents_embedded <= .data.documents_total' \
    "reembed reports embedded documents within total"
assert_jq "$reembed" '.data.embedding.posture.schema == "ee.embedding_posture.v1"' \
    "reembed exposes embedding posture"
reembed_posture="$(json_compact "$reembed" '.data.embedding.posture' "reembed posture json")"
reembed_signature="$(json_scalar "$reembed" ".data.embedding.posture | $posture_signature_filter" "reembed posture signature")"
assert_eq "$reembed_posture" "$index_posture" "reembed posture JSON matches index status"
assert_eq "$reembed_signature" "$index_signature" "reembed semantic/source/model signature matches index status"

step "doctor discloses ignored foreign embedding env without network hints"
run_command_capture doctor_trap doctor_env_trap \
    env EMBEDDING_MODEL=text-embedding-3-small OPENAI_API_KEY=sk-e2e-not-used \
    "$EE_BIN" doctor --workspace "$WS" --json --fields full
assert_jq "$doctor_trap" '.schema == "ee.response.v2"' "doctor emits response envelope"
assert_jq "$doctor_trap" '.success == true' "doctor command succeeds with foreign env traps"
assert_jq "$doctor_trap" '
    (.data.checks // [])
    | any(.name == "embedding_posture" and .tier == "advisory")
' "doctor exposes embedding posture as advisory"
doctor_message="$(json_scalar "$doctor_trap" 'first(.data.checks[]? | select(.name == "embedding_posture") | .message) // empty' "doctor embedding message")"
assert_contains "$doctor_message" "EMBEDDING_MODEL" "doctor names EMBEDDING_MODEL trap"
assert_contains "$doctor_message" "OPENAI_API_KEY" "doctor names OPENAI_API_KEY trap"
assert_contains "$doctor_message" "does not consume" "doctor says foreign embedding env is ignored"
assert_contains "$doctor_message" "current retrieval mode: $mode" "doctor note reports current mode"
assert_contains "$doctor_message" "$fast_model_id" "doctor message reports fast model id"
assert_contains "$doctor_message" "${fast_dimension}d" "doctor message reports embedding dimension"
assert_jq "$doctor_trap" '
    [.data.checks[]? | select(.name == "embedding_posture") | (.repair // "")]
    | all(.[]; contains("EE_EMBED_MODEL_PATH") | not)
' "doctor repair does not present EE_EMBED_MODEL_PATH as an enable switch"
assert_jq "$doctor_trap" 'tostring | contains("api.openai.com") | not' \
    "doctor output has no OpenAI network endpoint hint"
e2e_log_note "doctor_embedding_message=$doctor_message"

step "search explains score meaning"
QUERY="retrieval truth semantic posture score kind"
run_command_capture search_out search_explain \
    "$EE_BIN" search "$QUERY" --workspace "$WS" --limit 5 --relevance-floor 0 --explain --json
assert_jq "$search_out" '.schema == "ee.response.v2"' "search emits response envelope"
assert_jq "$search_out" '.success == true' "search succeeds"
assert_jq "$search_out" '(.data.results // []) | length >= 1' "search returns at least one result"
assert_jq "$search_out" '.data.results[0].scoreKind | type == "string" and length > 0' \
    "top search hit exposes scoreKind"
assert_jq "$search_out" '.data.results[0].relevanceScore | type == "number" and . >= 0 and . <= 1' \
    "top search hit exposes bounded relevanceScore"
assert_jq "$search_out" '.data.results[0].scoreInterval | type == "array" and length == 2' \
    "top search hit exposes scoreInterval"
assert_jq "$search_out" '.data.results[0].calibrated | type == "boolean"' \
    "top search hit exposes calibration flag"
assert_jq "$search_out" '.data.results[0] | has("score") and has("relevanceScore") and has("scoreKind")' \
    "top search hit is not a bare raw score"
score_kind="$(json_scalar "$search_out" '.data.results[0].scoreKind' "top score kind")"
relevance_score="$(json_scalar "$search_out" '.data.results[0].relevanceScore | tostring' "top relevance score")"
e2e_log_note "top_search_score query=$QUERY score_kind=$score_kind relevance_score=$relevance_score"

end_temp_workspace
harness_summary
