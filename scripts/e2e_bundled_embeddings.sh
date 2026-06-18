#!/usr/bin/env bash
# shellcheck disable=SC2016
# bd-1et0v.19 - bundled embeddings regression and eval proof E2E.
#
# Real-binary, no-Cargo E2E for the bundled local semantic model:
#   - pre-provisioned model cache path loads semantic retrieval without network;
#   - the analyst RBLX paraphrase regression ranks the RBLX memory first;
#   - fresh workspaces report ready semantic posture without rebuild noise;
#   - forced hash fallback emits embed_model_unavailable and still retrieves;
#   - eval run exposes the deterministic neural-vs-hash semantic recall gain.
#
# Default runs require EE_EMBED_MODEL_FIXTURE_DIR or EE_EMBED_MODEL_DIR to point
# at a pre-populated potion-multilingual-128M cache. The optional real download
# lifecycle only runs when EE_E2E_ALLOW_REAL_DOWNLOAD=1.

set -uo pipefail

TEST_ID="bundled_embeddings_e2e"
BEAD_ID="bd-1et0v.19"
SURFACE="bundled_embeddings_e2e"
FAILURES=0
PASSES=0
STEP=0
LAST_STDOUT_FILE=""
LAST_STDERR_FILE=""

if ! command -v jq >/dev/null 2>&1; then
    printf '%s\n' '{"schema":"ee.test_event.v1","test_id":"bundled_embeddings_e2e","kind":"assert_result","fields":{"label":"jq_available","status":"fail","first_failure_diagnosis":"jq executable missing before harness init","stdout_artifact_path":"","stderr_artifact_path":"stderr","schema_validation_status":"not_run","redaction_status":"not_run"}}' >&2
    exit 3
fi

EE_BIN="${EE_BIN:-${EE_BINARY:-ee}}"
if [[ "${EE_BIN}" == */* ]]; then
    REAL_EE="${EE_BIN}"
else
    REAL_EE="$(command -v "${EE_BIN}" 2>/dev/null || true)"
fi
if [[ -z "${REAL_EE}" || ! -x "${REAL_EE}" ]]; then
    printf '%s\n' '{"schema":"ee.test_event.v1","test_id":"bundled_embeddings_e2e","kind":"assert_result","fields":{"label":"ee_binary_available","status":"fail","first_failure_diagnosis":"prebuilt ee binary missing before harness init","stdout_artifact_path":"","stderr_artifact_path":"stderr","schema_validation_status":"not_run","redaction_status":"not_run"}}' >&2
    exit 3
fi

ROOT_BASE="${EE_E2E_TMPDIR:-/private/tmp}"
ROOT="$(mktemp -d "${ROOT_BASE%/}/ee-bundled-embeddings-e2e.XXXXXX")"
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

record_failure() {
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
        --argjson step "${STEP}" \
        '{step:$step,label:$label,root:$root,log_dir:$logDir}')"
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

    local stdout_file stderr_file args env_json
    stdout_file="${LOG_DIR}/step_$(printf '%02d' "$((STEP + 1))")_${label//[^A-Za-z0-9_]/_}.stdout.json"
    stderr_file="${LOG_DIR}/step_$(printf '%02d' "$((STEP + 1))")_${label//[^A-Za-z0-9_]/_}.stderr.txt"
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
        --argjson args "${args}" \
        --argjson envOverrides "${env_json}" \
        --argjson exitCode "${exit_code}" \
        --argjson elapsedMs "${elapsed_ms}" \
        --arg stdoutArtifact "${stdout_file}" \
        --arg stderrArtifact "${stderr_file}" \
        --arg stdoutHash "$(hash_file "${stdout_file}")" \
        --arg stderrHash "$(hash_file "${stderr_file}")" \
        '{bead_id:$bead,surface:$surface,label:$label,command:"ee",args:$args,env_overrides:$envOverrides,exit_code:$exitCode,elapsed_ms:$elapsedMs,stdout_hash:$stdoutHash,stderr_hash:$stderrHash,stdout_artifact_path:$stdoutArtifact,stderr_artifact_path:$stderrArtifact,sanitized_env:"explicit_test_overrides_only",schema_validation_status:"json_checked_below",redaction_status:"local_workspace_artifacts_retained",rch_status:"not_run_by_harness"}')"

    if [[ "${exit_code}" -ne 0 ]]; then
        record_failure "${label}" "ee exited ${exit_code}; stdout=${stdout_file} stderr=${stderr_file}"
    elif ! jq -e 'type == "object"' "${stdout_file}" >/dev/null 2>&1; then
        record_failure "${label}" "stdout was not a JSON object; stdout=${stdout_file}"
    else
        record_pass "${label}"
    fi
}

assert_jq_file() {
    local file="${1:?file required}"
    local filter="${2:?jq filter required}"
    local label="${3:?label required}"
    if jq -e "${filter}" "${file}" >/dev/null 2>&1; then
        record_pass "${label}"
    else
        record_failure "${label}" "jq assertion failed: ${filter}; file=${file}"
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
        record_failure "${label}" "jq assertion failed: ${filter}; ${arg_name}=${arg_value}; file=${file}"
    fi
}

json_scalar_file() {
    local file="${1:?file required}"
    local filter="${2:?jq filter required}"
    local label="${3:?label required}"
    local value
    if value="$(jq -r "${filter} // empty" "${file}" 2>/dev/null)"; then
        printf '%s' "${value}"
    else
        record_failure "${label}" "failed to read scalar with ${filter} from ${file}"
        printf ''
    fi
}

assert_text_not_contains() {
    local file="${1:?file required}"
    local needle="${2:?needle required}"
    local label="${3:?label required}"
    if grep -Fq "${needle}" "${file}"; then
        record_failure "${label}" "unexpected text '${needle}' in ${file}"
    else
        record_pass "${label}"
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

workspace_dir() {
    local name="${1:?workspace name required}"
    local ws="${ROOT}/${name}_workspace"
    mkdir -p "${ws}"
    printf '%s' "${ws}"
}

remember_fact() {
    local __id_var="${1:?memory id var required}"
    local label="${2:?label required}"
    local workspace="${3:?workspace required}"
    local content="${4:?content required}"
    shift 4
    run_ee_json_env "${label}" "$@" -- \
        remember "${content}" \
        --workspace "${workspace}" \
        --level semantic \
        --kind fact \
        --tags "bundled-embeddings,e2e,${BEAD_ID}" \
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

finish() {
    local verdict="pass"
    if [[ "${FAILURES}" -gt 0 ]]; then
        verdict="fail"
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
    printf 'bundled_embeddings_e2e: verdict=%s pass=%d fail=%d root=%s events=%s\n' \
        "${verdict}" "${PASSES}" "${FAILURES}" "${ROOT}" "${EVENT_LOG}" >&2
    if [[ "${FAILURES}" -gt 0 ]]; then
        emit_assert_result "summary" "fail" "bundled_embeddings_e2e recorded ${FAILURES} failing assertion(s); see ${EVENT_LOG}"
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
    '{ee_bin:$ee,root:$root,model_fixture_dir:$modelFixture,model_fixture_present:$modelFixturePresent,default_network_policy:"no_downloads"}')"

SEMANTIC_WS="$(workspace_dir semantic)"
FRESH_WS="$(workspace_dir fresh)"
HASH_WS="$(workspace_dir hash_fallback)"
HASH_MODEL_DIR="${ROOT}/hash_model_cache"
mkdir -p "${HASH_MODEL_DIR}"

HASH_ENV=(
    "EE_EMBED_DOWNLOAD=off"
    "EE_EMBED_MODEL_DIR=${HASH_MODEL_DIR}"
    "EE_EMBED_MODEL_PATH=${ROOT}/missing-model-fixture"
    "FRANKENSEARCH_OFFLINE=1"
    "FRANKENSEARCH_ALLOW_DOWNLOAD=0"
)

if [[ "${MODEL_FIXTURE_PRESENT}" != "true" ]]; then
    record_failure "preprovisioned bundled model fixture available" "Set EE_EMBED_MODEL_FIXTURE_DIR or EE_EMBED_MODEL_DIR to a pre-populated potion-multilingual-128M cache root; default e2e does not download 531MB"
else
    SEMANTIC_ENV=(
        "EE_EMBED_DOWNLOAD=auto"
        "EE_EMBED_MODEL_DIR=${MODEL_FIXTURE_DIR}"
        "FRANKENSEARCH_MODEL_DIR=${MODEL_FIXTURE_DIR}"
        "FRANKENSEARCH_OFFLINE=1"
        "FRANKENSEARCH_ALLOW_DOWNLOAD=0"
    )
    record_pass "preprovisioned bundled model fixture available"

    run_ee_json_env "semantic_init" "${SEMANTIC_ENV[@]}" -- init --workspace "${SEMANTIC_WS}" --json
    assert_jq_file "${LAST_STDOUT_FILE}" '.success == true' "semantic workspace init succeeds"

    RBLX_CONTENT="RBLX bookings/FCF watchlist: Roblox Robux engagement cohorts and creator marketplace economics signal durable bookings conversion and FCF resilience."
    SNOW_CONTENT="SNOW warehouse optimization watchlist: compute credits, retention cohorts, and data cloud workload consolidation shape net revenue expansion."
    NKE_CONTENT="NKE margin watchlist: channel inventory, wholesale discounting, and direct-to-consumer mix drive gross margin pressure."
    remember_fact RBLX_ID "remember_rblx_watchlist" "${SEMANTIC_WS}" "${RBLX_CONTENT}" "${SEMANTIC_ENV[@]}"
    remember_fact SNOW_ID "remember_snow_noise" "${SEMANTIC_WS}" "${SNOW_CONTENT}" "${SEMANTIC_ENV[@]}"
    remember_fact NKE_ID "remember_nke_noise" "${SEMANTIC_WS}" "${NKE_CONTENT}" "${SEMANTIC_ENV[@]}"

    run_ee_json_env "semantic_index_rebuild" "${SEMANTIC_ENV[@]}" -- index rebuild --workspace "${SEMANTIC_WS}" --json
    assert_jq_file "${LAST_STDOUT_FILE}" '.success == true' "semantic index rebuild succeeds"
    assert_jq_file "${LAST_STDOUT_FILE}" '.data.embedding.semantic == true' "reembed summary reports semantic true"
    assert_jq_file "${LAST_STDOUT_FILE}" '.data.embedding.source == "registry_observed"' "reembed summary source registry_observed"
    assert_jq_file "${LAST_STDOUT_FILE}" '(.data.embedding.registered_model_count | tonumber) >= 1' "reembed registered model count"
    assert_jq_file "${LAST_STDOUT_FILE}" '.data.embedding.fast_model_id | test("potion-multilingual-128M")' "reembed fast model id"

    run_ee_json_env "semantic_model_status" "${SEMANTIC_ENV[@]}" -- model status --workspace "${SEMANTIC_WS}" --json
    model_status_file="${LAST_STDOUT_FILE}"
    assert_jq_file "${model_status_file}" '.success == true' "model status succeeds"
    assert_jq_file "${model_status_file}" '.data.active.semantic == true' "model status active semantic"
    assert_jq_file "${model_status_file}" '(.data.registeredCount | tonumber) >= 1' "model status registered count"
    assert_jq_file "${model_status_file}" '.data.modelLifecycle.semanticReadiness.state == "available"' "model lifecycle semantic ready"

    run_ee_json_env "semantic_index_status" "${SEMANTIC_ENV[@]}" -- index status --workspace "${SEMANTIC_WS}" --json
    index_status_file="${LAST_STDOUT_FILE}"
    assert_jq_file "${index_status_file}" '.success == true' "index status succeeds"
    assert_jq_file "${index_status_file}" '.data.health == "ready"' "index status ready"
    assert_jq_file "${index_status_file}" '.data.embedding.semantic == true' "index status semantic true"
    assert_jq_file "${index_status_file}" '.data.embedding.source == "registry_observed"' "index status registry observed"

    ANALYST_QUERY="video game virtual currency platform owner cash generation"
    run_ee_json_env "analyst_paraphrase_search" "${SEMANTIC_ENV[@]}" -- \
        search "${ANALYST_QUERY}" --workspace "${SEMANTIC_WS}" --limit 5 --relevance-floor 0 --json
    analyst_search_file="${LAST_STDOUT_FILE}"
    assert_jq_file "${analyst_search_file}" '.success == true' "analyst search succeeds"
    assert_jq_file_arg "${analyst_search_file}" rblx "${RBLX_ID}" '(.data.results[0].memoryId // .data.results[0].memory_id // .data.results[0].docId) == $rblx' "analyst paraphrase ranks RBLX first"
    assert_jq_file_arg "${analyst_search_file}" snow "${SNOW_ID}" '(.data.results[0].memoryId // .data.results[0].memory_id // .data.results[0].docId) != $snow' "analyst paraphrase does not rank SNOW first"
    assert_jq_file_arg "${analyst_search_file}" nke "${NKE_ID}" '(.data.results[0].memoryId // .data.results[0].memory_id // .data.results[0].docId) != $nke' "analyst paraphrase does not rank NKE first"
    log_note "analyst_ranked_results" "$(jq -c --arg query "${ANALYST_QUERY}" '{query:$query,results:[.data.results[]? | {memoryId:(.memoryId // .memory_id // .docId),score,scoreKind,source,contentPreview:(.contentPreview // .content_preview // .content // "")}]}' "${analyst_search_file}")"

    run_ee_json_env "pack_hash_first" "${SEMANTIC_ENV[@]}" -- \
        pack "${ANALYST_QUERY}" --workspace "${SEMANTIC_WS}" --max-tokens 2000 --json
    pack_first_file="${LAST_STDOUT_FILE}"
    assert_jq_file "${pack_first_file}" '.success == true' "first pack succeeds"
    PACK_HASH_FIRST="$(json_scalar_file "${pack_first_file}" '.data.pack.hash' "first pack hash")"
    run_ee_json_env "pack_hash_second" "${SEMANTIC_ENV[@]}" -- \
        pack "${ANALYST_QUERY}" --workspace "${SEMANTIC_WS}" --max-tokens 2000 --json
    pack_second_file="${LAST_STDOUT_FILE}"
    assert_jq_file "${pack_second_file}" '.success == true' "second pack succeeds"
    PACK_HASH_SECOND="$(json_scalar_file "${pack_second_file}" '.data.pack.hash' "second pack hash")"
    assert_equal "${PACK_HASH_SECOND}" "${PACK_HASH_FIRST}" "pack hash stable across repeated semantic query"

    run_ee_json_env "fresh_init" "${SEMANTIC_ENV[@]}" -- init --workspace "${FRESH_WS}" --json
    assert_jq_file "${LAST_STDOUT_FILE}" '.success == true' "fresh init succeeds"
    run_ee_json_env "fresh_status" "${SEMANTIC_ENV[@]}" -- status --workspace "${FRESH_WS}" --json
    fresh_status_file="${LAST_STDOUT_FILE}"
    assert_jq_file "${fresh_status_file}" '.success == true' "fresh status succeeds"
    assert_jq_file "${fresh_status_file}" '(.data.posture // .data.status // "ok") | tostring | test("ok|ready|Ready|READY")' "fresh status is ready-like"
    assert_jq_file "${fresh_status_file}" '[(.. | objects | select(.semantic? == true))] | length >= 1' "fresh status exposes semantic true"
    assert_text_not_contains "${fresh_status_file}" "rebuild recommended" "fresh status stdout has no rebuild recommendation"
    assert_text_not_contains "${LAST_STDERR_FILE}" "rebuild recommended" "fresh status stderr has no rebuild recommendation"
fi

run_ee_json_env "hash_init" "${HASH_ENV[@]}" -- init --workspace "${HASH_WS}" --json
assert_jq_file "${LAST_STDOUT_FILE}" '.success == true' "hash fallback init succeeds"
remember_fact HASH_RUST_ID "hash_remember_rust" "${HASH_WS}" "Cargo workspace uses Rust nightly and lexical fallback stays useful." "${HASH_ENV[@]}"
remember_fact HASH_NOISE_ID "hash_remember_noise" "${HASH_WS}" "Design screenshots discuss onboarding art direction." "${HASH_ENV[@]}"
run_ee_json_env "hash_index_rebuild" "${HASH_ENV[@]}" -- index rebuild --workspace "${HASH_WS}" --json
assert_jq_file "${LAST_STDOUT_FILE}" '.success == true' "hash index rebuild succeeds"
assert_jq_file "${LAST_STDOUT_FILE}" '.data.embedding.semantic == false' "hash reembed summary reports semantic false"
run_ee_json_env "hash_search_lexical" "${HASH_ENV[@]}" -- \
    search "cargo workspace" --workspace "${HASH_WS}" --limit 3 --relevance-floor 0 --json
hash_search_file="${LAST_STDOUT_FILE}"
assert_jq_file "${hash_search_file}" '.success == true' "hash fallback search succeeds"
assert_jq_file_arg "${hash_search_file}" target "${HASH_RUST_ID}" '[.data.results[]? | (.memoryId // .memory_id // .docId)] | index($target) != null' "hash fallback returns lexical target"
assert_jq_file_arg "${hash_search_file}" noise "${HASH_NOISE_ID}" '(.data.results[0].memoryId // .data.results[0].memory_id // .data.results[0].docId) != $noise' "hash fallback top result is not unrelated noise"
assert_jq_file "${hash_search_file}" '[.data.degraded[]? | select(.code == "embed_model_unavailable")] | length >= 1' "hash fallback emits embed_model_unavailable"
assert_jq_file tests/fixtures/failure_modes/embed_model_unavailable.json '.schema == "ee.failure_mode_fixture.v1" and .code == "embed_model_unavailable"' "embed_model_unavailable fixture exists"

run_ee_json_env "eval_bundled_embeddings" "${HASH_ENV[@]}" -- \
    eval run fx.bundled_embeddings.v1 --json
eval_file="${LAST_STDOUT_FILE}"
assert_jq_file "${eval_file}" '.schema == "ee.response.v2"' "eval response schema"
assert_jq_file "${eval_file}" '.success == true' "eval run succeeds"
assert_jq_file "${eval_file}" '.data.report.fixture_id == "fx.bundled_embeddings.v1"' "eval fixture id"
assert_jq_file "${eval_file}" '.data.report.semantic_recall.schema == "ee.eval.semantic_recall_report.v1"' "eval semantic recall schema"
assert_jq_file "${eval_file}" '.data.report.semantic_recall.passed == true' "eval semantic recall passes"
assert_jq_file "${eval_file}" '.data.report.semantic_recall.hash_baseline_recall_at_k == 0' "eval hash baseline recall pinned"
assert_jq_file "${eval_file}" '.data.report.semantic_recall.semantic_recall_at_k == 1' "eval semantic recall pinned"
assert_jq_file "${eval_file}" '.data.report.semantic_recall.recall_gain > 0' "eval semantic recall gain positive"
log_note "eval_semantic_recall" "$(jq -c '.data.report.semantic_recall' "${eval_file}")"

if [[ "${EE_E2E_ALLOW_REAL_DOWNLOAD:-0}" == "1" ]]; then
    DOWNLOAD_WS="$(workspace_dir real_download)"
    DOWNLOAD_MODEL_DIR="${ROOT}/real_download_model_cache"
    DOWNLOAD_ENV=(
        "EE_EMBED_DOWNLOAD=auto"
        "EE_EMBED_MODEL_DIR=${DOWNLOAD_MODEL_DIR}"
        "FRANKENSEARCH_MODEL_DIR=${DOWNLOAD_MODEL_DIR}"
    )
    run_ee_json_env "download_init" "${DOWNLOAD_ENV[@]}" -- init --workspace "${DOWNLOAD_WS}" --json
    assert_jq_file "${LAST_STDOUT_FILE}" '.success == true' "download workspace init succeeds"
    run_ee_json_env "download_fetch_first" "${DOWNLOAD_ENV[@]}" -- model fetch embedding-default --workspace "${DOWNLOAD_WS}" --json
    assert_jq_file "${LAST_STDOUT_FILE}" '.success == true' "first embedding fetch succeeds"
    assert_jq_file "${LAST_STDOUT_FILE}" '.data.modelId == "potion-multilingual-128M"' "download fetch model id"
    assert_jq_file "${LAST_STDOUT_FILE}" '.data.hashSha256 | type == "string" and length > 0' "download fetch sha256 present"
    assert_text_not_contains "${LAST_STDOUT_FILE}" "Downloading" "download progress does not contaminate JSON stdout"
    run_ee_json_env "download_fetch_cached" "${DOWNLOAD_ENV[@]}" -- model fetch embedding-default --workspace "${DOWNLOAD_WS}" --json
    assert_jq_file "${LAST_STDOUT_FILE}" '.success == true' "cached embedding fetch succeeds"
    assert_jq_file "${LAST_STDOUT_FILE}" '.data.copied == false' "second fetch is local cached"
else
    log_note "real_download_gated" "$(jq -cn '{enabled:false,gate:"EE_E2E_ALLOW_REAL_DOWNLOAD=1",default_ci_policy:"no 531MB download"}')"
fi
