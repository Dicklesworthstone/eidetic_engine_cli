#!/usr/bin/env bash
# shellcheck disable=SC2016
# bd-2vq2z.19 - embedding-native retrieval E2E.
#
# Real-binary, no-Cargo E2E for the embedding-native track:
#   - similar: semantic kNN when a pre-provisioned model cache is supplied,
#     explicit lexical/hash degradation otherwise;
#   - dedup: remember-time near_duplicates[] warning, duplicate storage by
#     default, and persisted/audited curation proposal;
#   - rerank: pre/post order logging, reranked assertions when a local reranker
#     is available, and deterministic fusion-only degradation when absent;
#   - eval: real eval-harness metrics plus a measured top-1 precision delta from
#     the pre/post rerank observations.
#
# The script never downloads models. Set EE_EMBED_MODEL_FIXTURE_DIR to a
# pre-populated model cache root when exercising the semantic model path.

set -uo pipefail

TEST_ID="embedding_native_e2e"
BEAD_ID="bd-2vq2z.19"
SURFACE="embedding_native_e2e"
FAILURES=0
PASSES=0
STEP=0
LAST_STDOUT_FILE=""
LAST_STDERR_FILE=""

if ! command -v jq >/dev/null 2>&1; then
    printf '%s\n' '{"schema":"ee.test_event.v1","test_id":"embedding_native_e2e","kind":"assert_result","fields":{"label":"jq_available","status":"fail","first_failure_diagnosis":"jq executable missing before harness init","stdout_artifact_path":"","stderr_artifact_path":"stderr","schema_validation_status":"not_run","redaction_status":"not_run"}}' >&2
    exit 3
fi

EE_BIN="${EE_BIN:-${EE_BINARY:-ee}}"
if [[ "${EE_BIN}" == */* ]]; then
    REAL_EE="${EE_BIN}"
else
    REAL_EE="$(command -v "${EE_BIN}" 2>/dev/null || true)"
fi
if [[ -z "${REAL_EE}" || ! -x "${REAL_EE}" ]]; then
    printf '%s\n' '{"schema":"ee.test_event.v1","test_id":"embedding_native_e2e","kind":"assert_result","fields":{"label":"ee_binary_available","status":"fail","first_failure_diagnosis":"prebuilt ee binary missing before harness init","stdout_artifact_path":"","stderr_artifact_path":"stderr","schema_validation_status":"not_run","redaction_status":"not_run"}}' >&2
    exit 3
fi

ROOT_BASE="${EE_E2E_TMPDIR:-/private/tmp}"
ROOT="$(mktemp -d "${ROOT_BASE%/}/ee-embedding-native-e2e.XXXXXX")"
HOME_DIR="${ROOT}/home"
LOG_DIR="${ROOT}/logs"
EVENT_LOG="${LOG_DIR}/events.jsonl"
mkdir -p "${HOME_DIR}" "${LOG_DIR}"
: >"${EVENT_LOG}"

now_iso() {
    date -u +"%Y-%m-%dT%H:%M:%SZ"
}

args_json() {
    if [[ "$#" -eq 0 ]]; then
        printf '[]'
        return
    fi
    printf '%s\0' "$@" | jq -Rs 'split("\u0000")[:-1]'
}

hash_file() {
    local file="${1:?file required}"
    if command -v b3sum >/dev/null 2>&1; then
        printf 'blake3:%s' "$(b3sum "$file" | awk '{print $1}')"
    else
        printf 'sha256:%s' "$(shasum -a 256 "$file" | awk '{print $1}')"
    fi
}

emit_event() {
    local kind="${1:?kind required}"
    local fields_json="${2:-{}}"
    jq -cn \
        --arg schema "ee.test_event.v1" \
        --arg ts "$(now_iso)" \
        --arg testId "${TEST_ID}" \
        --arg kind "${kind}" \
        --argjson fields "${fields_json}" \
        '{schema:$schema,ts:$ts,test_id:$testId,kind:$kind,fields:$fields}' >>"${EVENT_LOG}"
}

emit_assert_fail() {
    local label="${1:?label required}"
    local detail="${2:-failed}"
    FAILURES=$((FAILURES + 1))
    emit_event "assert_fail" "$(jq -cn \
        --arg bead "${BEAD_ID}" \
        --arg surface "${SURFACE}" \
        --arg label "${label}" \
        --arg detail "${detail}" \
        --arg stdoutArtifact "${LAST_STDOUT_FILE:-}" \
        --arg stderrArtifact "${LAST_STDERR_FILE:-}" \
        '{bead_id:$bead,surface:$surface,label:$label,status:"fail",detail:$detail,first_failure_diagnosis:$detail,stdout_artifact_path:$stdoutArtifact,stderr_artifact_path:$stderrArtifact,schema_validation_status:"checked_by_jq",redaction_status:"local_workspace_artifacts_retained"}')"
    printf '[FAIL] %s: %s\n' "${label}" "${detail}" >&2
}

record_failure() {
    emit_assert_fail "$@"
}

record_pass() {
    local label="${1:?label required}"
    PASSES=$((PASSES + 1))
    emit_event "assert_ok" "$(jq -cn \
        --arg bead "${BEAD_ID}" \
        --arg surface "${SURFACE}" \
        --arg label "${label}" \
        --arg stdoutArtifact "${LAST_STDOUT_FILE:-}" \
        --arg stderrArtifact "${LAST_STDERR_FILE:-}" \
        '{bead_id:$bead,surface:$surface,label:$label,status:"pass",stdout_artifact_path:$stdoutArtifact,stderr_artifact_path:$stderrArtifact,schema_validation_status:"checked_by_jq",redaction_status:"local_workspace_artifacts_retained"}')"
}

emit_assert_result() {
    local label="${1:?label required}"
    local status="${2:?status required}"
    local diagnosis="${3:-}"
    emit_event "assert_result" "$(jq -cn \
        --arg bead "${BEAD_ID}" \
        --arg surface "${SURFACE}" \
        --arg label "${label}" \
        --arg status "${status}" \
        --arg diagnosis "${diagnosis}" \
        --arg stdoutArtifact "${LAST_STDOUT_FILE:-}" \
        --arg stderrArtifact "${LAST_STDERR_FILE:-}" \
        '{bead_id:$bead,surface:$surface,label:$label,status:$status,first_failure_diagnosis:$diagnosis,stdout_artifact_path:$stdoutArtifact,stderr_artifact_path:$stderrArtifact,schema_validation_status:"checked_by_jq",redaction_status:"local_workspace_artifacts_retained"}')"
}

log_note() {
    local label="${1:?label required}"
    local fields_json="${2:-{}}"
    emit_event "note" "$(jq -cn \
        --arg bead "${BEAD_ID}" \
        --arg surface "${SURFACE}" \
        --arg label "${label}" \
        --argjson fields "${fields_json}" \
        '{bead_id:$bead,surface:$surface,label:$label,fields:$fields,redaction_status:"local_workspace_artifacts_retained"}')"
    printf '[note] %s %s\n' "${label}" "${fields_json}" >&2
}

log_step() {
    local label="${1:?label required}"
    STEP=$((STEP + 1))
    printf '[%02d] %s\n' "${STEP}" "${label}" >&2
    log_note "step" "$(jq -cn \
        --arg label "${label}" \
        --arg root "${ROOT}" \
        --arg logDir "${LOG_DIR}" \
        --arg step "${STEP}" \
        '{step:($step|tonumber),label:$label,root:$root,log_dir:$logDir}')"
}

run_ee_json_env() {
    local label="${1:?label required}"
    shift
    local env_overrides=()
    while [[ "${1:-}" != "--" ]]; do
        env_overrides+=("${1:?missing -- before ee args}")
        shift
    done
    shift

    local stdout_file
    stdout_file="${LOG_DIR}/step_$(printf '%02d' "$((STEP + 1))")_${label//[^A-Za-z0-9_]/_}.stdout.json"
    local stderr_file
    stderr_file="${LOG_DIR}/step_$(printf '%02d' "$((STEP + 1))")_${label//[^A-Za-z0-9_]/_}.stderr.txt"
    local args env_json
    args="$(args_json "$@")"
    env_json="$(args_json "${env_overrides[@]}")"

    log_step "${label}"
    jq -cn \
        --arg schema "ee.test_event.v1" \
        --arg ts "$(now_iso)" \
        --arg testId "${TEST_ID}" \
        --argjson args "${args}" \
        --argjson envOverrides "${env_json}" \
        --arg label "${label}" \
        '{schema:$schema,ts:$ts,test_id:$testId,kind:"command_start",command:"ee",args:$args,fields:{bead_id:"bd-2vq2z.19",surface:"embedding_native_e2e",label:$label,env_overrides:$envOverrides,sanitized_env:"explicit_test_overrides_only",cwd:"[REPO_ROOT]",redaction_status:"local_workspace_artifacts_retained"}}' >>"${EVENT_LOG}"

    local start_ms end_ms elapsed_ms exit_code
    start_ms="$(python3 -c 'import time; print(int(time.time() * 1000))')"
    env HOME="${HOME_DIR}" NO_COLOR=1 "${env_overrides[@]}" "${REAL_EE}" "$@" >"${stdout_file}" 2>"${stderr_file}"
    exit_code=$?
    end_ms="$(python3 -c 'import time; print(int(time.time() * 1000))')"
    elapsed_ms=$((end_ms - start_ms))

    LAST_STDOUT_FILE="${stdout_file}"
    LAST_STDERR_FILE="${stderr_file}"
    jq -cn \
        --arg schema "ee.test_event.v1" \
        --arg ts "$(now_iso)" \
        --arg testId "${TEST_ID}" \
        --argjson args "${args}" \
        --argjson envOverrides "${env_json}" \
        --arg label "${label}" \
        --argjson exitCode "${exit_code}" \
        --argjson elapsedMs "${elapsed_ms}" \
        --arg stdoutArtifact "${stdout_file}" \
        --arg stderrArtifact "${stderr_file}" \
        --arg stdoutHash "$(hash_file "${stdout_file}")" \
        --arg stderrHash "$(hash_file "${stderr_file}")" \
        '{schema:$schema,ts:$ts,test_id:$testId,kind:"command_end",command:"ee",args:$args,exit_code:$exitCode,elapsed_ms:$elapsedMs,stdout_hash:$stdoutHash,stderr_hash:$stderrHash,fields:{bead_id:"bd-2vq2z.19",surface:"embedding_native_e2e",label:$label,env_overrides:$envOverrides,sanitized_env:"explicit_test_overrides_only",stdout_artifact_path:$stdoutArtifact,stderr_artifact_path:$stderrArtifact,schema_validation_status:"json_checked_below",redaction_status:"local_workspace_artifacts_retained",rch_status:"not_run_by_harness"}}' >>"${EVENT_LOG}"

    if [[ "${exit_code}" -ne 0 ]]; then
        emit_assert_fail "${label}" "ee exited ${exit_code}; stdout=${stdout_file} stderr=${stderr_file}"
    elif ! jq -e 'type == "object"' "${stdout_file}" >/dev/null 2>&1; then
        emit_assert_fail "${label}" "stdout was not a JSON object; stdout=${stdout_file}"
    else
        record_pass "${label}"
    fi
}

run_ee_json() {
    local label="${1:?label required}"
    shift
    run_ee_json_env "${label}" -- "$@"
}

run_ee_text_env() {
    local label="${1:?label required}"
    shift
    local env_overrides=()
    while [[ "${1:-}" != "--" ]]; do
        env_overrides+=("${1:?missing -- before ee args}")
        shift
    done
    shift

    local stdout_file
    stdout_file="${LOG_DIR}/step_$(printf '%02d' "$((STEP + 1))")_${label//[^A-Za-z0-9_]/_}.stdout.txt"
    local stderr_file
    stderr_file="${LOG_DIR}/step_$(printf '%02d' "$((STEP + 1))")_${label//[^A-Za-z0-9_]/_}.stderr.txt"
    local args env_json
    args="$(args_json "$@")"
    env_json="$(args_json "${env_overrides[@]}")"

    log_step "${label}"
    emit_event "command_start" "$(jq -cn \
        --arg bead "${BEAD_ID}" \
        --arg surface "${SURFACE}" \
        --arg label "${label}" \
        --argjson args "${args}" \
        --argjson envOverrides "${env_json}" \
        '{bead_id:$bead,surface:$surface,label:$label,command:"ee",args:$args,env_overrides:$envOverrides,sanitized_env:"explicit_test_overrides_only",redaction_status:"local_workspace_artifacts_retained"}')"
    local start_ms end_ms elapsed_ms exit_code
    start_ms="$(python3 -c 'import time; print(int(time.time() * 1000))')"
    env HOME="${HOME_DIR}" NO_COLOR=1 "${env_overrides[@]}" "${REAL_EE}" "$@" >"${stdout_file}" 2>"${stderr_file}"
    exit_code=$?
    end_ms="$(python3 -c 'import time; print(int(time.time() * 1000))')"
    elapsed_ms=$((end_ms - start_ms))
    LAST_STDOUT_FILE="${stdout_file}"
    LAST_STDERR_FILE="${stderr_file}"
    emit_event "command_end" "$(jq -cn \
        --arg bead "${BEAD_ID}" \
        --arg surface "${SURFACE}" \
        --arg label "${label}" \
        --argjson exitCode "${exit_code}" \
        --argjson elapsedMs "${elapsed_ms}" \
        --arg stdoutArtifact "${stdout_file}" \
        --arg stderrArtifact "${stderr_file}" \
        --arg stdoutHash "$(hash_file "${stdout_file}")" \
        --arg stderrHash "$(hash_file "${stderr_file}")" \
        '{bead_id:$bead,surface:$surface,label:$label,exit_code:$exitCode,elapsed_ms:$elapsedMs,stdout_hash:$stdoutHash,stderr_hash:$stderrHash,sanitized_env:"explicit_test_overrides_only",stdout_artifact_path:$stdoutArtifact,stderr_artifact_path:$stderrArtifact,schema_validation_status:"text_output",redaction_status:"local_workspace_artifacts_retained"}')"
    if [[ "${exit_code}" -eq 0 ]]; then
        record_pass "${label}"
    else
        record_failure "${label}" "ee exited ${exit_code}; stdout=${stdout_file} stderr=${stderr_file}"
    fi
}

assert_jq_file() {
    local file="${1:?file required}"
    local filter="${2:?jq filter required}"
    local label="${3:?label required}"
    if jq -e "${filter}" "${file}" >/dev/null 2>&1; then
        record_pass "${label}"
    else
        record_failure "${label}" "jq filter false: ${filter}; file=${file}"
    fi
}

assert_jq_file_arg() {
    local file="${1:?file required}"
    local arg_name="${2:?arg name required}"
    local arg_value="${3:-}"
    local filter="${4:?jq filter required}"
    local label="${5:?label required}"
    if jq -e --arg "${arg_name}" "${arg_value}" "${filter}" "${file}" >/dev/null 2>&1; then
        record_pass "${label}"
    else
        record_failure "${label}" "jq filter false with ${arg_name}=${arg_value}: ${filter}; file=${file}"
    fi
}

json_scalar_file() {
    local file="${1:?file required}"
    local filter="${2:?jq filter required}"
    local label="${3:?label required}"
    local value
    if value="$(jq -er "${filter}" "${file}" 2>/dev/null)"; then
        printf '%s' "${value}"
    else
        record_failure "${label}" "jq scalar failed: ${filter}; file=${file}"
        printf ''
    fi
}

assert_text_contains() {
    local file="${1:?file required}"
    local needle="${2:?needle required}"
    local label="${3:?label required}"
    if grep -Fq -- "${needle}" "${file}"; then
        record_pass "${label}"
    else
        record_failure "${label}" "missing text ${needle}; file=${file}"
    fi
}

assert_equal() {
    local actual="${1:-}"
    local expected="${2:-}"
    local label="${3:?label required}"
    if [[ "${actual}" == "${expected}" ]]; then
        record_pass "${label}"
    else
        record_failure "${label}" "expected=${expected} actual=${actual}"
    fi
}

assert_not_equal() {
    local actual="${1:-}"
    local unexpected="${2:-}"
    local label="${3:?label required}"
    if [[ "${actual}" != "${unexpected}" ]]; then
        record_pass "${label}"
    else
        record_failure "${label}" "both values were ${actual}"
    fi
}

workspace_dir() {
    local name="${1:?workspace name required}"
    local ws="${ROOT}/${name}_workspace"
    mkdir -p "${ws}"
    printf '%s' "${ws}"
}

remember_json() {
    local __id_var="${1:?memory id var required}"
    local label="${2:?label required}"
    local workspace="${3:?workspace required}"
    local content="${4:?content required}"
    shift 4
    run_ee_json_env "${label}" "$@" -- \
        remember "${content}" \
        --workspace "${workspace}" \
        --level procedural \
        --kind rule \
        --tags "embedding-native,e2e,${BEAD_ID}" \
        --no-auto-link \
        --no-propose-candidates \
        --json
    assert_jq_file "${LAST_STDOUT_FILE}" '.schema == "ee.response.v2"' "${label} response schema"
    assert_jq_file "${LAST_STDOUT_FILE}" '.success == true' "${label} succeeds"
    assert_jq_file "${LAST_STDOUT_FILE}" '(.data.memory_id // .data.memoryId // "") | length > 0' "${label} returns memory id"
    local memory_id
    memory_id="$(json_scalar_file "${LAST_STDOUT_FILE}" '.data.memory_id // .data.memoryId' "${label} memory id")"
    printf -v "${__id_var}" '%s' "${memory_id}"
}

seed_similarity_workspace() {
    local workspace="${1:?workspace required}"
    shift
    run_ee_json_env "similar_init_${workspace##*/}" "$@" -- init --workspace "${workspace}" --json
    assert_jq_file "${LAST_STDOUT_FILE}" '.success == true' "similar workspace init succeeds"

    remember_json SIM_SEED_ID "similar_remember_seed_${workspace##*/}" "${workspace}" \
        "Asupersync supervisors must propagate cancellation budgets through child scopes before retrying indexing work." "$@"
    remember_json SIM_CLUSTER_ID "similar_remember_cluster_${workspace##*/}" "${workspace}" \
        "When an indexing task retries, carry the same cancellation budget through the asupersync child scope." "$@"
    remember_json SIM_NOISE_ID "similar_remember_noise_${workspace##*/}" "${workspace}" \
        "Garden calendar planning mentions spring bulbs, patio soil, and weekend weather." "$@"

    run_ee_json_env "similar_index_rebuild_${workspace##*/}" "$@" -- index rebuild --workspace "${workspace}" --json
    assert_jq_file "${LAST_STDOUT_FILE}" '.success == true' "similar index rebuild succeeds"
}

observe_search_order() {
    local label="${1:?label required}"
    local file="${2:?file required}"
    local target_id="${3:-}"
    local order top_id top_is_target
    order="$(jq -c '(.data.results // []) | map(.memoryId // .memory_id // .docId // "")' "${file}" 2>/dev/null || printf '[]')"
    top_id="$(jq -r '(.data.results[0].memoryId // .data.results[0].memory_id // .data.results[0].docId // "")' "${file}" 2>/dev/null || true)"
    if [[ -n "${target_id}" && "${top_id}" == "${target_id}" ]]; then
        top_is_target="true"
    else
        top_is_target="false"
    fi
    emit_event "retrieval_order_observed" "$(jq -cn \
        --arg bead "${BEAD_ID}" \
        --arg surface "${SURFACE}" \
        --arg label "${label}" \
        --arg file "${file}" \
        --arg targetId "${target_id}" \
        --arg topId "${top_id}" \
        --argjson order "${order}" \
        --argjson topIsTarget "${top_is_target}" \
        '{
          bead_id:$bead,
          surface:$surface,
          label:$label,
          response_artifact_path:$file,
          target_memory_id:$targetId,
          top_memory_id:$topId,
          top_is_target:$topIsTarget,
          order:$order,
          redaction_status:"local_workspace_artifacts_retained"
        }')"
}

finish() {
    local verdict="PASS"
    if [[ "${FAILURES}" -gt 0 ]]; then
        verdict="FAIL"
    fi
    emit_event "summary" "$(jq -cn \
        --arg bead "${BEAD_ID}" \
        --arg surface "${SURFACE}" \
        --arg root "${ROOT}" \
        --arg logDir "${LOG_DIR}" \
        --arg eventLog "${EVENT_LOG}" \
        --arg verdict "${verdict}" \
        --argjson passes "${PASSES}" \
        --argjson failures "${FAILURES}" \
        --argjson steps "${STEP}" \
        '{bead_id:$bead,surface:$surface,root:$root,log_dir:$logDir,event_log:$eventLog,verdict:$verdict,passes:$passes,failures:$failures,steps:$steps,redaction_status:"local_workspace_artifacts_retained"}')"
    jq -cn \
        --arg schema "ee.test_event.v1.summary" \
        --arg test "${TEST_ID}" \
        --arg verdict "${verdict}" \
        --arg root "${ROOT}" \
        --arg events "${EVENT_LOG}" \
        --argjson pass "${PASSES}" \
        --argjson fail "${FAILURES}" \
        --argjson steps "${STEP}" \
        '{schema:$schema,test:$test,verdict:$verdict,pass:$pass,fail:$fail,steps:$steps,root:$root,events:$events}' >"${LOG_DIR}/summary.json"
    printf 'embedding_native_e2e: verdict=%s pass=%d fail=%d root=%s events=%s\n' \
        "${verdict}" "${PASSES}" "${FAILURES}" "${ROOT}" "${EVENT_LOG}" >&2
    if [[ "${FAILURES}" -gt 0 ]]; then
        local summary_artifacts="stdout_artifact_path=${LAST_STDOUT_FILE:-} stderr_artifact_path=${LAST_STDERR_FILE:-}"
        emit_assert_result "summary" "fail" "embedding_native_e2e recorded ${FAILURES} failing assertion(s); see ${EVENT_LOG}; ${summary_artifacts}"
        exit 1
    fi
}
trap finish EXIT

MODEL_FIXTURE_DIR="${EE_EMBED_MODEL_FIXTURE_DIR:-}"
if [[ -z "${MODEL_FIXTURE_DIR}" && -n "${EE_EMBED_MODEL_DIR:-}" ]]; then
    MODEL_FIXTURE_DIR="${EE_EMBED_MODEL_DIR}"
fi

MODEL_FIXTURE_PRESENT=false
if [[ -n "${MODEL_FIXTURE_DIR}" && -d "${MODEL_FIXTURE_DIR}" ]]; then
    MODEL_FIXTURE_PRESENT=true
fi

log_note "start" "$(jq -cn \
    --arg ee "${REAL_EE}" \
    --arg root "${ROOT}" \
    --arg modelFixture "${MODEL_FIXTURE_DIR:-}" \
    --argjson modelFixturePresent "${MODEL_FIXTURE_PRESENT}" \
    '{ee_bin:$ee,root:$root,model_fixture_dir:$modelFixture,model_fixture_present:$modelFixturePresent,network_policy:"no_downloads"}')"

SEMANTIC_WS="$(workspace_dir semantic)"
HASH_WS="$(workspace_dir hash_fallback)"
DEDUP_WS="$(workspace_dir dedup)"
RERANK_WS="$(workspace_dir rerank)"

HASH_MODEL_DIR="${ROOT}/hash_model_cache"
mkdir -p "${HASH_MODEL_DIR}"
HASH_ENV=(
    "EE_EMBED_DOWNLOAD=off"
    "EE_EMBED_MODEL_DIR=${HASH_MODEL_DIR}"
    "FRANKENSEARCH_OFFLINE=1"
    "FRANKENSEARCH_ALLOW_DOWNLOAD=0"
)

if [[ "${MODEL_FIXTURE_PRESENT}" == "true" ]]; then
    SEMANTIC_ENV=(
        "EE_EMBED_DOWNLOAD=off"
        "EE_EMBED_MODEL_DIR=${MODEL_FIXTURE_DIR}"
        "FRANKENSEARCH_OFFLINE=1"
        "FRANKENSEARCH_ALLOW_DOWNLOAD=0"
    )
else
    SEMANTIC_ENV=("${HASH_ENV[@]}")
    log_note "semantic_fixture_absent" "$(jq -cn \
        --arg expected "Set EE_EMBED_MODEL_FIXTURE_DIR to a pre-provisioned potion-multilingual-128M cache root." \
        '{status:"fallback_branch_only",expected_fixture:$expected,download_attempted:false}')"
fi

seed_similarity_workspace "${SEMANTIC_WS}" "${SEMANTIC_ENV[@]}"

run_ee_json_env "similar_index_status" "${SEMANTIC_ENV[@]}" -- \
    index status --workspace "${SEMANTIC_WS}" --json
assert_jq_file "${LAST_STDOUT_FILE}" '.schema == "ee.response.v2"' "similar index status schema"
assert_jq_file "${LAST_STDOUT_FILE}" '.success == true' "similar index status succeeds"
assert_jq_file "${LAST_STDOUT_FILE}" '.data.embedding.schema == "ee.embedding_posture.v1"' "similar index status embeds posture"
semantic_posture_file="${LAST_STDOUT_FILE}"
semantic_available="$(json_scalar_file "${semantic_posture_file}" '.data.embedding.semantic | tostring' "semantic posture availability")"
log_note "semantic_posture" "$(jq -c '.data.embedding' "${semantic_posture_file}")"

run_ee_json_env "similar_query_semantic_or_fallback" "${SEMANTIC_ENV[@]}" -- \
    similar "${SIM_SEED_ID}" --workspace "${SEMANTIC_WS}" --limit 3 --min-score 0 --json
similar_file="${LAST_STDOUT_FILE}"
assert_jq_file "${similar_file}" '.schema == "ee.response.v2"' "similar response schema"
assert_jq_file "${similar_file}" '.success == true' "similar command succeeds"
assert_jq_file "${similar_file}" '.data.command == "similar"' "similar command field"
assert_jq_file "${similar_file}" '.data.targetMemoryId | type == "string" and length > 0' "similar targetMemoryId emitted"
assert_jq_file "${similar_file}" '.data.embeddingPosture.schema == "ee.embedding_posture.v1"' "similar embeds posture"
assert_jq_file_arg "${similar_file}" seed "${SIM_SEED_ID}" '.data.targetMemoryId == $seed' "similar target is the seed"
assert_jq_file_arg "${similar_file}" cluster "${SIM_CLUSTER_ID}" '[.data.results[]? | (.memoryId // .memory_id // .docId)] | index($cluster) != null' "similar returns related cluster"

if [[ "${semantic_available}" == "true" ]]; then
    assert_jq_file "${similar_file}" '.data.semanticAvailable == true' "similar semantic path reports available"
    assert_jq_file "${similar_file}" '.data.lexicalFallback == false' "similar semantic path avoids lexical fallback"
    assert_jq_file "${similar_file}" '.data.request.similarityMode == "semantic_knn"' "similar request names semantic kNN"
    assert_jq_file_arg "${similar_file}" noise "${SIM_NOISE_ID}" '(.data.results[0].memoryId // .data.results[0].memory_id // .data.results[0].docId) != $noise' "similar top result is not unrelated noise"
else
    assert_jq_file "${similar_file}" '.data.semanticAvailable == false' "similar fallback reports semantic unavailable"
    assert_jq_file "${similar_file}" '.data.lexicalFallback == true' "similar fallback reports lexical fallback"
    assert_jq_file "${similar_file}" '.data.request.similarityMode == "lexical_fallback"' "similar request names lexical fallback"
    assert_jq_file "${similar_file}" '[.data.degraded[]? | select(.code == "embed_model_unavailable")] | length >= 1' "similar fallback has embed_model_unavailable degradation"
fi

seed_similarity_workspace "${HASH_WS}" "${HASH_ENV[@]}"
run_ee_json_env "similar_hash_fallback" "${HASH_ENV[@]}" -- \
    similar "${SIM_SEED_ID}" --workspace "${HASH_WS}" --limit 3 --min-score 0 --json
hash_similar_file="${LAST_STDOUT_FILE}"
assert_jq_file "${hash_similar_file}" '.success == true' "hash similar succeeds"
assert_jq_file "${hash_similar_file}" '.data.semanticAvailable == false' "hash similar reports semantic unavailable"
assert_jq_file "${hash_similar_file}" '.data.lexicalFallback == true' "hash similar reports lexical fallback"
assert_jq_file "${hash_similar_file}" '.data.request.similarityMode == "lexical_fallback"' "hash similar request names lexical fallback"
assert_jq_file "${hash_similar_file}" '[.data.degraded[]? | select(.code == "embed_model_unavailable")] | length >= 1' "hash similar emits explicit degradation"

DEDUP_ENV=(
    "${HASH_ENV[@]}"
    "EE_EMBED_DEDUP_ENABLED=1"
    "EE_EMBED_DEDUP_HAMMING_K=0"
    "EE_EMBED_DEDUP_COSINE_FLOOR=0.97"
)

run_ee_json_env "dedup_init" "${DEDUP_ENV[@]}" -- init --workspace "${DEDUP_WS}" --json
assert_jq_file "${LAST_STDOUT_FILE}" '.success == true' "dedup workspace init succeeds"

DEDUP_BASE_CONTENT="Agents must route Rust verification through RCH before claiming a bead is green."
DEDUP_PARAPHRASE_CONTENT="  Agents must route Rust verification through RCH before claiming a bead is green.  "

remember_json DEDUP_BASE_ID "dedup_remember_base" "${DEDUP_WS}" "${DEDUP_BASE_CONTENT}" "${DEDUP_ENV[@]}"
base_remember_file="${LAST_STDOUT_FILE}"
assert_jq_file "${base_remember_file}" '.data.persisted == true' "base dedup memory persisted"
assert_jq_file "${base_remember_file}" '(.data.near_duplicates // []) | length == 0' "base memory has no prior near duplicate"

remember_json DEDUP_DUP_ID "dedup_remember_duplicate_json" "${DEDUP_WS}" "${DEDUP_PARAPHRASE_CONTENT}" "${DEDUP_ENV[@]}"
dup_remember_file="${LAST_STDOUT_FILE}"
assert_jq_file "${dup_remember_file}" '.data.persisted == true' "duplicate memory is not silently blocked"
assert_not_equal "${DEDUP_DUP_ID}" "${DEDUP_BASE_ID}" "duplicate remember stores a second memory id by default"
assert_jq_file_arg "${dup_remember_file}" base "${DEDUP_BASE_ID}" '[.data.near_duplicates[]? | (.memory_id // .memoryId)] | index($base) != null' "near_duplicates surfaces existing id"
assert_jq_file "${dup_remember_file}" '(.data.near_duplicates // []) | length >= 1' "near_duplicates array is populated"
assert_jq_file "${dup_remember_file}" '[.data.near_duplicates[]? | ((.next_actions // .nextActions // [])[]?)] | length >= 1' "near duplicate carries next actions"
log_note "dedup_implicit_allow_duplicate" "$(jq -cn \
    --arg base "${DEDUP_BASE_ID}" \
    --arg duplicate "${DEDUP_DUP_ID}" \
    '{mode:"default_store_with_warning",base_memory_id:$base,duplicate_memory_id:$duplicate,explicit_allow_duplicate_flag_present:false}')"

run_ee_text_env "dedup_remember_duplicate_human_warning" "${DEDUP_ENV[@]}" -- \
    remember "${DEDUP_PARAPHRASE_CONTENT}" \
    --workspace "${DEDUP_WS}" \
    --level procedural \
    --kind rule \
    --tags "embedding-native,e2e,${BEAD_ID}" \
    --no-auto-link \
    --no-propose-candidates
human_duplicate_file="${LAST_STDOUT_FILE}"
assert_text_contains "${human_duplicate_file}" "Near duplicates:" "human remember warns about near duplicates"
assert_text_contains "${human_duplicate_file}" "Next: review near_duplicates" "human remember gives near duplicate next step"

run_ee_json_env "dedup_curate_candidates" "${DEDUP_ENV[@]}" -- \
    curate candidates --workspace "${DEDUP_WS}" --type dedup --target-memory "${DEDUP_BASE_ID}" --json
dedup_candidates_file="${LAST_STDOUT_FILE}"
assert_jq_file "${dedup_candidates_file}" '.schema == "ee.response.v2"' "curate candidates response schema"
assert_jq_file "${dedup_candidates_file}" '.success == true' "curate candidates succeeds"
assert_jq_file "${dedup_candidates_file}" '.data.command == "curate candidates"' "curate candidates command field"
assert_jq_file_arg "${dedup_candidates_file}" base "${DEDUP_BASE_ID}" '[.data.candidates[]? | select(.proposalSource == "embedding_dedup" and .targetMemoryId == $base)] | length >= 1' "curate dedupe clusters duplicate memories"
assert_jq_file "${dedup_candidates_file}" '[.data.candidates[]? | select(.proposalSource == "embedding_dedup") | .audit.proposedBy] | any(. == "embedding_dedup:v1")' "curate dedupe proposal is audited"
assert_jq_file "${dedup_candidates_file}" '.data.durableMutation | type == "boolean"' "curate candidates reports durable mutation posture"
DEDUP_CANDIDATE_ID="$(json_scalar_file "${dedup_candidates_file}" 'first(.data.candidates[]? | select(.proposalSource == "embedding_dedup") | .id)' "dedupe candidate id")"
if [[ -n "${DEDUP_CANDIDATE_ID}" ]]; then
    run_ee_json_env "dedup_curate_show" "${DEDUP_ENV[@]}" -- \
        curate show "${DEDUP_CANDIDATE_ID}" --workspace "${DEDUP_WS}" --json
    assert_jq_file "${LAST_STDOUT_FILE}" '.success == true' "curate show dedupe candidate succeeds"
    assert_jq_file_arg "${LAST_STDOUT_FILE}" candidate "${DEDUP_CANDIDATE_ID}" '.data.candidateId == $candidate' "curate show returns requested candidate"
    assert_jq_file "${LAST_STDOUT_FILE}" '.data.durableMutation == false' "curate show remains read-only"
fi

run_ee_json_env "rerank_init" "${HASH_ENV[@]}" -- init --workspace "${RERANK_WS}" --json
assert_jq_file "${LAST_STDOUT_FILE}" '.success == true' "rerank workspace init succeeds"

RERANK_QUERY="release format checklist cargo clippy"
RERANK_TARGET_SENTINEL="BD2VQ2Z19_PRECISE_RERANK_TARGET"
remember_json RERANK_TRAP_ID "rerank_remember_lexical_trap" "${RERANK_WS}" \
    "BD2VQ2Z19_LEXICAL_TRAP release release release format format cargo cargo clippy clippy repeat words, but this note is a noisy lexical trap and not the release policy target." "${HASH_ENV[@]}"
remember_json RERANK_TARGET_ID "rerank_remember_precise_target" "${RERANK_WS}" \
    "${RERANK_TARGET_SENTINEL} The correct release policy target says run cargo fmt --check and cargo clippy before publishing a Rust release." "${HASH_ENV[@]}"
remember_json RERANK_NOISE_ID "rerank_remember_unrelated_noise" "${RERANK_WS}" \
    "Design review notes discuss onboarding screenshots and do not mention release formatting gates." "${HASH_ENV[@]}"

run_ee_json_env "rerank_index_rebuild" "${HASH_ENV[@]}" -- index rebuild --workspace "${RERANK_WS}" --json
assert_jq_file "${LAST_STDOUT_FILE}" '.success == true' "rerank index rebuild succeeds"

run_ee_json_env "rerank_lexical_baseline" "${HASH_ENV[@]}" -- \
    search "${RERANK_QUERY}" --workspace "${RERANK_WS}" --source-mode lexical-only --strict-source-mode --limit 3 --relevance-floor 0 --json
lexical_file="${LAST_STDOUT_FILE}"
assert_jq_file "${lexical_file}" '.success == true' "rerank lexical baseline succeeds"
assert_jq_file "${lexical_file}" '.data.rerank.schema == "ee.rerank_posture.v1"' "lexical baseline carries rerank posture"
assert_jq_file "${lexical_file}" '.data.rerank.mode == "fusion_only"' "lexical baseline disables rerank"
observe_search_order "rerank_pre_baseline" "${lexical_file}" "${RERANK_TARGET_ID}"

run_ee_json_env "rerank_hybrid_or_degraded" "${HASH_ENV[@]}" -- \
    search "${RERANK_QUERY}" --workspace "${RERANK_WS}" --limit 3 --relevance-floor 0 --json
hybrid_file="${LAST_STDOUT_FILE}"
assert_jq_file "${hybrid_file}" '.success == true' "rerank hybrid search succeeds"
assert_jq_file "${hybrid_file}" '.data.rerank.schema == "ee.rerank_posture.v1"' "hybrid carries rerank posture"
observe_search_order "rerank_post_hybrid" "${hybrid_file}" "${RERANK_TARGET_ID}"

lexical_top="$(json_scalar_file "${lexical_file}" '(.data.results[0].memoryId // .data.results[0].memory_id // .data.results[0].docId // "")' "lexical top id")"
hybrid_top="$(json_scalar_file "${hybrid_file}" '(.data.results[0].memoryId // .data.results[0].memory_id // .data.results[0].docId // "")' "hybrid top id")"
lexical_precision=0
hybrid_precision=0
if [[ "${lexical_top}" == "${RERANK_TARGET_ID}" ]]; then
    lexical_precision=1
fi
if [[ "${hybrid_top}" == "${RERANK_TARGET_ID}" ]]; then
    hybrid_precision=1
fi
precision_gain=$((hybrid_precision - lexical_precision))

rerank_mode="$(json_scalar_file "${hybrid_file}" '.data.rerank.mode' "hybrid rerank mode")"
case "${rerank_mode}" in
    reranked)
        assert_jq_file "${hybrid_file}" '.data.rerank.available == true' "reranked mode reports available"
        assert_jq_file "${hybrid_file}" '(.data.rerank.rerankScoreCount | tonumber) > 0' "reranked mode reports rerank scores"
        assert_jq_file "${hybrid_file}" '((.data.results // []) | map(select(.scoreKind == "reranked" and has("rerankScore"))) | length) > 0' "reranked results expose scoreKind and rerankScore"
        assert_jq_file_arg "${hybrid_file}" target "${RERANK_TARGET_ID}" '(.data.results[0].memoryId // .data.results[0].memory_id // .data.results[0].docId) == $target' "rerank corrects top result to precise target"
        ;;
    fusion_only_degraded)
        assert_jq_file "${hybrid_file}" '.data.rerank.available == false' "degraded rerank reports unavailable"
        assert_jq_file "${hybrid_file}" '.data.rerank.degradedCode == "rerank_model_unavailable"' "degraded rerank names unavailable model"
        assert_jq_file "${hybrid_file}" '.data.rerank.advisory == {
            "code": "rerank_model_unavailable",
            "severity": "low",
            "permanent": true,
            "message": "No usable local reranker is registered. Search is using fusion-only ranking. Network download is unavailable, but a verified offline reranker artifact can be imported explicitly.",
            "repair": null,
            "resolution": "automatic_repair_unavailable"
        }' "missing reranker exposes the canonical permanent advisory without a fake repair"
        assert_jq_file "${hybrid_file}" '[.data.degraded[]? | select(.code == "rerank_model_unavailable")] | length == 0' "permanent reranker gap stays out of query degradations"
        assert_jq_file "${hybrid_file}" '((.data.results // []) | map(select(.scoreKind == "reranked" or has("rerankScore"))) | length) == 0' "fusion-only degraded omits rerankScore"
        run_ee_json_env "rerank_degraded_replay" "${HASH_ENV[@]}" -- \
            search "${RERANK_QUERY}" --workspace "${RERANK_WS}" --limit 3 --relevance-floor 0 --json
        replay_file="${LAST_STDOUT_FILE}"
        assert_jq_file "${replay_file}" '.success == true' "rerank degraded replay succeeds"
        first_order="$(jq -c '(.data.results // []) | map(.memoryId // .memory_id // .docId // "")' "${hybrid_file}")"
        replay_order="$(jq -c '(.data.results // []) | map(.memoryId // .memory_id // .docId // "")' "${replay_file}")"
        assert_equal "${replay_order}" "${first_order}" "fusion-only degraded replay preserves order"
        assert_jq_file "${replay_file}" '.data.rerank.advisory == {
            "code": "rerank_model_unavailable",
            "severity": "low",
            "permanent": true,
            "message": "No usable local reranker is registered. Search is using fusion-only ranking. Network download is unavailable, but a verified offline reranker artifact can be imported explicitly.",
            "repair": null,
            "resolution": "automatic_repair_unavailable"
        }' "permanent reranker advisory and unavailable-repair posture are deterministic"
        observe_search_order "rerank_degraded_replay" "${replay_file}" "${RERANK_TARGET_ID}"
        ;;
    fusion_only)
        record_failure "hybrid rerank posture is decisive" "hybrid search returned fusion_only without rerank_model_unavailable"
        ;;
    *)
        record_failure "hybrid rerank posture mode known" "unexpected mode=${rerank_mode}"
        ;;
esac

emit_event "rerank_precision_gain_measured" "$(jq -cn \
    --arg bead "${BEAD_ID}" \
    --arg surface "${SURFACE}" \
    --arg query "${RERANK_QUERY}" \
    --arg mode "${rerank_mode}" \
    --arg target "${RERANK_TARGET_ID}" \
    --arg lexicalTop "${lexical_top}" \
    --arg hybridTop "${hybrid_top}" \
    --argjson lexicalPrecision "${lexical_precision}" \
    --argjson hybridPrecision "${hybrid_precision}" \
    --argjson precisionGain "${precision_gain}" \
    '{bead_id:$bead,surface:$surface,query:$query,rerank_mode:$mode,target_memory_id:$target,lexical_top_memory_id:$lexicalTop,hybrid_top_memory_id:$hybridTop,lexical_precision_at_1:$lexicalPrecision,hybrid_precision_at_1:$hybridPrecision,precision_gain_at_1:$precisionGain,redaction_status:"local_workspace_artifacts_retained"}')"
record_pass "rerank precision gain metric emitted"

run_ee_json_env "eval_dangerous_cleanup_metrics" "${HASH_ENV[@]}" -- \
    eval run fx.dangerous_cleanup.v1 --json
eval_file="${LAST_STDOUT_FILE}"
assert_jq_file "${eval_file}" '.schema == "ee.response.v2"' "eval response schema"
assert_jq_file "${eval_file}" '.success == true' "eval run succeeds"
assert_jq_file "${eval_file}" '.data.command == "eval run"' "eval command field"
assert_jq_file "${eval_file}" '.data.report.status == "passed"' "eval report passed"
assert_jq_file "${eval_file}" '(.data.report.metrics.queries_evaluated // 0) > 0' "eval metrics query count"
assert_jq_file "${eval_file}" '.data.report.metrics.mean_precision_at_1 | type == "number"' "eval mean precision at 1 measured"
assert_jq_file "${eval_file}" '.data.report.metrics.mean_precision_at_5 | type == "number"' "eval mean precision at 5 measured"
assert_jq_file "${eval_file}" '.data.report.metrics.mean_mrr | type == "number"' "eval mean MRR measured"
log_note "eval_metrics" "$(jq -c '.data.report.metrics' "${eval_file}")"
