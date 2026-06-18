#!/usr/bin/env bash
# bd-2vq2z.6 - embedding-native rerank E2E.
#
# Real-binary, no-Cargo E2E:
#   - seeds a fresh workspace with a lexical trap and a precise target memory;
#   - records the lexical baseline order and the hybrid post-rerank order;
#   - asserts the real `ee search --json` rerank posture envelope;
#   - accepts only two honest outcomes:
#       1. reranked: rerankScore/scoreKind are present and the precise target wins;
#       2. fusion_only_degraded: rerank_model_unavailable is explicit and replay is deterministic.
#
# Artifacts are intentionally retained. AGENTS.md forbids implicit cleanup.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"

if ! command -v jq >/dev/null 2>&1; then
    printf '%s\n' '{"schema":"ee.test_event.v1","test_id":"rerank_e2e","kind":"assert_result","fields":{"label":"jq_available","status":"fail","first_failure_diagnosis":"jq executable missing before harness init","stdout_artifact_path":"","stderr_artifact_path":"stderr","schema_validation_status":"not_run","redaction_status":"not_run"}}' >&2
    exit 3
fi

EE_BIN="${EE_BIN:-${EE_BINARY:-ee}}"
if [[ "${EE_BIN}" == */* ]]; then
    REAL_EE="${EE_BIN}"
else
    REAL_EE="$(command -v "${EE_BIN}" 2>/dev/null || true)"
fi
if [[ -z "${REAL_EE}" || ! -x "${REAL_EE}" ]]; then
    printf '%s\n' '{"schema":"ee.test_event.v1","test_id":"rerank_e2e","kind":"assert_result","fields":{"label":"ee_binary_available","status":"fail","first_failure_diagnosis":"prebuilt ee binary missing before harness init","stdout_artifact_path":"","stderr_artifact_path":"stderr","schema_validation_status":"not_run","redaction_status":"not_run"}}' >&2
    exit 3
fi

ROOT_BASE="${EE_E2E_TMPDIR:-${TMPDIR:-/tmp}}"
ROOT="$(mktemp -d "${ROOT_BASE%/}/ee-rerank-e2e.XXXXXX")"
WORKSPACE="${ROOT}/workspace"
HOME_DIR="${ROOT}/home"
LOG_DIR="${ROOT}/logs"
EVENT_LOG="${LOG_DIR}/events.jsonl"
mkdir -p "${WORKSPACE}" "${HOME_DIR}" "${LOG_DIR}"
: >"${EVENT_LOG}"

TEST_ID="rerank_e2e"
FAILURES=0
STEP=0
LAST_STDOUT_FILE=""

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

log_step() {
    local label="${1:?label required}"
    STEP=$((STEP + 1))
    printf '[%02d] %s\n' "${STEP}" "${label}" >&2
    emit_event "note" "$(jq -cn \
        --arg bead "bd-2vq2z.6" \
        --arg label "${label}" \
        --arg workspace "${WORKSPACE}" \
        --arg logDir "${LOG_DIR}" \
        '{bead_id:$bead,surface:"rerank_e2e",label:$label,workspace:$workspace,log_dir:$logDir,redaction_status:"local_workspace_artifacts_retained"}')"
}

record_failure() {
    local label="${1:?label required}"
    local detail="${2:-failed}"
    FAILURES=$((FAILURES + 1))
    emit_event "assert_fail" "$(jq -cn \
        --arg bead "bd-2vq2z.6" \
        --arg label "${label}" \
        --arg detail "${detail}" \
        '{bead_id:$bead,surface:"rerank_e2e",label:$label,detail:$detail,first_failure_diagnosis:$detail,redaction_status:"local_workspace_artifacts_retained"}')"
    printf '[FAIL] %s: %s\n' "${label}" "${detail}" >&2
}

record_pass() {
    local label="${1:?label required}"
    emit_event "assert_ok" "$(jq -cn \
        --arg bead "bd-2vq2z.6" \
        --arg label "${label}" \
        '{bead_id:$bead,surface:"rerank_e2e",label:$label,redaction_status:"local_workspace_artifacts_retained"}')"
}

run_ee_json() {
    local label="${1:?label required}"
    shift
    local stdout_file="${LOG_DIR}/step_$(printf '%02d' "$((STEP + 1))")_${label//[^A-Za-z0-9_]/_}.stdout.json"
    local stderr_file="${LOG_DIR}/step_$(printf '%02d' "$((STEP + 1))")_${label//[^A-Za-z0-9_]/_}.stderr.txt"
    local args
    args="$(args_json "$@")"

    log_step "${label}"
    jq -cn \
        --arg schema "ee.test_event.v1" \
        --arg ts "$(now_iso)" \
        --arg testId "${TEST_ID}" \
        --argjson args "${args}" \
        --arg label "${label}" \
        --arg workspace "${WORKSPACE}" \
        '{schema:$schema,ts:$ts,test_id:$testId,kind:"command_start",command:"ee",args:$args,fields:{bead_id:"bd-2vq2z.6",surface:"rerank_e2e",label:$label,workspace:$workspace,cwd:"[REPO_ROOT]",redaction_status:"local_workspace_artifacts_retained"}}' >>"${EVENT_LOG}"

    local start_ms end_ms elapsed_ms exit_code
    start_ms="$(python3 -c 'import time; print(int(time.time() * 1000))')"
    set +e
    env HOME="${HOME_DIR}" NO_COLOR=1 "${REAL_EE}" --workspace "${WORKSPACE}" "$@" >"${stdout_file}" 2>"${stderr_file}"
    exit_code=$?
    set -e
    end_ms="$(python3 -c 'import time; print(int(time.time() * 1000))')"
    elapsed_ms=$((end_ms - start_ms))

    jq -cn \
        --arg schema "ee.test_event.v1" \
        --arg ts "$(now_iso)" \
        --arg testId "${TEST_ID}" \
        --argjson args "${args}" \
        --arg label "${label}" \
        --argjson exitCode "${exit_code}" \
        --argjson elapsedMs "${elapsed_ms}" \
        --arg stdoutArtifact "${stdout_file}" \
        --arg stderrArtifact "${stderr_file}" \
        --arg stdoutHash "$(hash_file "${stdout_file}")" \
        --arg stderrHash "$(hash_file "${stderr_file}")" \
        '{schema:$schema,ts:$ts,test_id:$testId,kind:"command_end",command:"ee",args:$args,exit_code:$exitCode,elapsed_ms:$elapsedMs,stdout_hash:$stdoutHash,stderr_hash:$stderrHash,fields:{bead_id:"bd-2vq2z.6",surface:"rerank_e2e",label:$label,stdout_artifact_path:$stdoutArtifact,stderr_artifact_path:$stderrArtifact,schema_validation_status:"json_checked_below",redaction_status:"local_workspace_artifacts_retained",rch_status:"not_run_by_harness"}}' >>"${EVENT_LOG}"

    if [[ "${exit_code}" -ne 0 ]]; then
        record_failure "${label}" "ee exited ${exit_code}; stdout=${stdout_file} stderr=${stderr_file}"
    elif ! jq -e 'type == "object"' "${stdout_file}" >/dev/null 2>&1; then
        record_failure "${label}" "stdout was not JSON object; stdout=${stdout_file}"
    else
        record_pass "${label}"
    fi
    LAST_STDOUT_FILE="${stdout_file}"
}

assert_jq_file() {
    local file="${1:?file required}"
    local filter="${2:?jq filter required}"
    local want="${3:?expected value required}"
    local label="${4:?label required}"
    local got
    got="$(jq -r "${filter}" "${file}" 2>/dev/null || true)"
    if [[ "${got}" == "${want}" ]]; then
        record_pass "${label}"
    else
        record_failure "${label}" "expected=${want} actual=${got:-<empty>} file=${file}"
    fi
}

emit_search_observation() {
    local label="${1:?label required}"
    local file="${2:?file required}"
    local fields
    fields="$(jq -c \
        --arg bead "bd-2vq2z.6" \
        --arg label "${label}" \
        --arg file "${file}" \
        '{
          bead_id: $bead,
          surface: "rerank_e2e",
          label: $label,
          response_artifact_path: $file,
          command: .data.command,
          success: .success,
          rerank_mode: .data.rerank.mode,
          rerank_available: .data.rerank.available,
          rerank_score_count: .data.rerank.rerankScoreCount,
          degraded_codes: ((.data.degraded // .degraded // []) | map(.code)),
          order: ((.data.results // []) | map(.memoryId)),
          score_kinds: ((.data.results // []) | map(.scoreKind)),
          rerank_scores: ((.data.results // []) | map(.rerankScore // null)),
          redaction_status: "local_workspace_artifacts_retained"
        }' "${file}")"
    emit_event "rerank_order_observed" "${fields}"
}

finish() {
    emit_event "note" "$(jq -cn \
        --arg bead "bd-2vq2z.6" \
        --arg logDir "${LOG_DIR}" \
        --argjson failures "${FAILURES}" \
        '{bead_id:$bead,surface:"rerank_e2e",label:"summary",failures:$failures,log_dir:$logDir,redaction_status:"local_workspace_artifacts_retained"}')"
    printf 'rerank_e2e: failures=%d log_dir=%s\n' "${FAILURES}" "${LOG_DIR}" >&2
    if [[ "${FAILURES}" -gt 0 ]]; then
        exit 1
    fi
}
trap finish EXIT

emit_event "note" "$(jq -cn \
    --arg bead "bd-2vq2z.6" \
    --arg ee "${REAL_EE}" \
    --arg workspace "${WORKSPACE}" \
    --arg logDir "${LOG_DIR}" \
    '{bead_id:$bead,surface:"rerank_e2e",label:"start",ee_bin:$ee,workspace:$workspace,log_dir:$logDir,redaction_status:"local_workspace_artifacts_retained"}')"

QUERY="release format checklist cargo clippy"
PRECISE_SENTINEL="BD2VQ2Z6_PRECISE_RERANK_TARGET"
LEXICAL_TRAP="BD2VQ2Z6_LEXICAL_TRAP"

run_ee_json "init_workspace" init --json
init_file="${LAST_STDOUT_FILE}"
assert_jq_file "${init_file}" '.schema' "ee.response.v2" "init envelope schema"
assert_jq_file "${init_file}" '.success' "true" "init succeeds"

run_ee_json "remember_lexical_trap" remember "${LEXICAL_TRAP} release release release format format cargo cargo clippy clippy words repeat, but this note is only a noisy lexical trap and not the policy target." --level semantic --kind fact --tags "bd-2vq2z.6,rerank,trap" --json
trap_file="${LAST_STDOUT_FILE}"
assert_jq_file "${trap_file}" '.success' "true" "remember lexical trap succeeds"

run_ee_json "remember_precise_target" remember "${PRECISE_SENTINEL} The correct release policy target says run cargo fmt --check and cargo clippy before publishing a Rust release." --level procedural --kind rule --tags "bd-2vq2z.6,rerank,target" --json
precise_file="${LAST_STDOUT_FILE}"
assert_jq_file "${precise_file}" '.success' "true" "remember precise target succeeds"

run_ee_json "remember_unrelated_noise" remember "Design review notes discuss onboarding screenshots and do not mention release formatting gates." --level episodic --kind note --tags "bd-2vq2z.6,rerank,noise" --json
noise_file="${LAST_STDOUT_FILE}"
assert_jq_file "${noise_file}" '.success' "true" "remember unrelated noise succeeds"

run_ee_json "index_rebuild" index rebuild --json
index_file="${LAST_STDOUT_FILE}"
assert_jq_file "${index_file}" '.success' "true" "index rebuild succeeds"

run_ee_json "search_lexical_baseline" search "${QUERY}" --source-mode lexical-only --strict-source-mode --limit 3 --relevance-floor 0 --json
lexical_file="${LAST_STDOUT_FILE}"
assert_jq_file "${lexical_file}" '.schema' "ee.response.v2" "lexical search envelope schema"
assert_jq_file "${lexical_file}" '.success' "true" "lexical baseline succeeds"
assert_jq_file "${lexical_file}" '((.data.results // []) | length) >= 2' "true" "lexical baseline returns multiple results"
assert_jq_file "${lexical_file}" '.data.rerank.schema' "ee.rerank_posture.v1" "lexical baseline carries rerank posture schema"
assert_jq_file "${lexical_file}" '.data.rerank.mode' "fusion_only" "lexical baseline disables rerank"
emit_search_observation "pre_rerank_lexical_baseline" "${lexical_file}"
log_step "assert lexical baseline top memory is logged for pre/post rerank comparison"
lexical_top_memory="$(jq -r '(.data.results[0].memoryId // "")' "${lexical_file}")"
if [[ -n "${lexical_top_memory}" && "${lexical_top_memory}" != "null" ]]; then
    record_pass "lexical baseline top memory captured"
    emit_event "rerank_baseline_top_observed" "$(jq -cn \
        --arg bead "bd-2vq2z.6" \
        --arg memoryId "${lexical_top_memory}" \
        --arg responseArtifact "${lexical_file}" \
        '{bead_id:$bead,surface:"rerank_e2e",label:"lexical_baseline_top_memory",memory_id:$memoryId,response_artifact_path:$responseArtifact,redaction_status:"local_workspace_artifacts_retained"}')"
else
    record_failure "lexical baseline top memory captured" "missing memoryId in lexical baseline first result"
fi

run_ee_json "search_hybrid_rerank_or_degraded" search "${QUERY}" --limit 3 --relevance-floor 0 --json
hybrid_file="${LAST_STDOUT_FILE}"
assert_jq_file "${hybrid_file}" '.schema' "ee.response.v2" "hybrid search envelope schema"
assert_jq_file "${hybrid_file}" '.success' "true" "hybrid search succeeds"
assert_jq_file "${hybrid_file}" '((.data.results // []) | length) >= 2' "true" "hybrid search returns multiple results"
assert_jq_file "${hybrid_file}" '.data.rerank.schema' "ee.rerank_posture.v1" "hybrid search carries rerank posture schema"
emit_search_observation "post_rerank_hybrid_order" "${hybrid_file}"

rerank_mode="$(jq -r '.data.rerank.mode' "${hybrid_file}")"
case "${rerank_mode}" in
    reranked)
        assert_jq_file "${hybrid_file}" '.data.rerank.available' "true" "reranked mode reports available"
        assert_jq_file "${hybrid_file}" '(.data.rerank.rerankScoreCount | tonumber) > 0' "true" "reranked mode reports rerank scores"
        assert_jq_file "${hybrid_file}" '((.data.results // []) | map(select(.scoreKind == "reranked" and has("rerankScore"))) | length) > 0' "true" "reranked results expose scoreKind and rerankScore"
        assert_jq_file "${hybrid_file}" "(.data.results[0].content // \"\" | contains(\"${PRECISE_SENTINEL}\"))" "true" "rerank corrects top result to precise target"
        ;;
    fusion_only_degraded)
        assert_jq_file "${hybrid_file}" '.data.rerank.available' "false" "degraded mode reports unavailable"
        assert_jq_file "${hybrid_file}" '.data.rerank.degradedCode' "rerank_model_unavailable" "degraded mode names rerank_model_unavailable"
        assert_jq_file "${hybrid_file}" '((.data.degraded // .degraded // []) | map(.code) | contains(["rerank_model_unavailable"]))' "true" "degraded array includes rerank_model_unavailable"
        assert_jq_file "${hybrid_file}" '((.data.results // []) | map(select(.scoreKind == "reranked" or has("rerankScore"))) | length)' "0" "fusion-only degraded results omit rerankScore"

        run_ee_json "search_hybrid_degraded_replay" search "${QUERY}" --limit 3 --relevance-floor 0 --json
        replay_file="${LAST_STDOUT_FILE}"
        assert_jq_file "${replay_file}" '.success' "true" "hybrid degraded replay succeeds"
        emit_search_observation "post_rerank_degraded_replay_order" "${replay_file}"
        first_order="$(jq -c '(.data.results // []) | map(.memoryId)' "${hybrid_file}")"
        replay_order="$(jq -c '(.data.results // []) | map(.memoryId)' "${replay_file}")"
        if [[ "${first_order}" == "${replay_order}" ]]; then
            record_pass "fusion-only degraded replay preserves order"
        else
            record_failure "fusion-only degraded replay preserves order" "first=${first_order} replay=${replay_order}"
        fi
        ;;
    fusion_only)
        record_failure "hybrid rerank posture is decisive" "hybrid search returned fusion_only without rerank_model_unavailable"
        ;;
    *)
        record_failure "hybrid rerank posture mode known" "unexpected mode=${rerank_mode}"
        ;;
esac
