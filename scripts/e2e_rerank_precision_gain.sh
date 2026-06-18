#!/usr/bin/env bash
# bd-2vq2z.26 - hardened real-binary E2E that FAILS on a stubbed / pass-through
# reranker.
#
# The pre-existing scripts/e2e_rerank.sh accepts `fusion_only_degraded` as a
# passing outcome, so it can go green without ever exercising a real
# cross-encoder. This lane closes that gap with two assertions that a stub
# cannot satisfy:
#
#   (a) top-K order CHANGES versus a true fusion-only baseline. The baseline is
#       the SAME hybrid candidate set with `[search] rerank = "off"` (bd-2vq2z.23
#       opt-out), so any order difference is attributable to reranking alone --
#       not to a different source mode or candidate set. A reranker that passes
#       fusion order through unchanged fails here.
#
#   (b) precision_gain_at_1 > 0. The corpus is built so a lexical trap dominates
#       fusion ordering and the precise policy target is NOT rank 1 under
#       fusion-only. A real rerank must promote the precise target to rank 1,
#       yielding precision_gain_at_1 = p@1(reranked) - p@1(fusion_only) = 1.
#
# Lanes:
#   - Default (no reranker provisioned): the hybrid run honestly degrades to
#     fusion_only_degraded; the strong assertions are skipped and the lane
#     records why. This keeps the script green on a stock CI box.
#   - EE_E2E_REQUIRE_RERANK=1 (central lane with a pre-provisioned reranker
#     artifact): a non-reranked outcome is a HARD FAILURE. The lane MUST reach
#     mode=reranked and satisfy (a) and (b).
#
# Artifacts are intentionally retained. AGENTS.md forbids implicit cleanup.

set -euo pipefail

if ! command -v jq >/dev/null 2>&1; then
    printf '%s\n' '{"schema":"ee.test_event.v1","test_id":"rerank_precision_gain_e2e","kind":"assert_result","fields":{"label":"jq_available","status":"fail","first_failure_diagnosis":"jq executable missing before harness init"}}' >&2
    exit 3
fi

EE_BIN="${EE_BIN:-${EE_BINARY:-ee}}"
if [[ "${EE_BIN}" == */* ]]; then
    REAL_EE="${EE_BIN}"
else
    REAL_EE="$(command -v "${EE_BIN}" 2>/dev/null || true)"
fi
if [[ -z "${REAL_EE}" || ! -x "${REAL_EE}" ]]; then
    printf '%s\n' '{"schema":"ee.test_event.v1","test_id":"rerank_precision_gain_e2e","kind":"assert_result","fields":{"label":"ee_binary_available","status":"fail","first_failure_diagnosis":"prebuilt ee binary missing before harness init"}}' >&2
    exit 3
fi

REQUIRE_RERANK="${EE_E2E_REQUIRE_RERANK:-0}"

ROOT_BASE="${EE_E2E_TMPDIR:-${TMPDIR:-/tmp}}"
ROOT="$(mktemp -d "${ROOT_BASE%/}/ee-rerank-precision-e2e.XXXXXX")"
WORKSPACE="${ROOT}/workspace"
HOME_DIR="${ROOT}/home"
LOG_DIR="${ROOT}/logs"
EVENT_LOG="${LOG_DIR}/events.jsonl"
mkdir -p "${WORKSPACE}" "${HOME_DIR}" "${LOG_DIR}"
: >"${EVENT_LOG}"

TEST_ID="rerank_precision_gain_e2e"
BEAD="bd-2vq2z.26"
FAILURES=0
STEP=0
LAST_STDOUT_FILE=""

now_iso() { date -u +"%Y-%m-%dT%H:%M:%SZ"; }

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
}

record_failure() {
    local label="${1:?label required}"
    local detail="${2:-failed}"
    FAILURES=$((FAILURES + 1))
    emit_event "assert_fail" "$(jq -cn --arg bead "${BEAD}" --arg label "${label}" --arg detail "${detail}" \
        '{bead_id:$bead,surface:"rerank_precision_gain_e2e",label:$label,detail:$detail,first_failure_diagnosis:$detail,redaction_status:"local_workspace_artifacts_retained"}')"
    printf '[FAIL] %s: %s\n' "${label}" "${detail}" >&2
}

record_pass() {
    local label="${1:?label required}"
    emit_event "assert_ok" "$(jq -cn --arg bead "${BEAD}" --arg label "${label}" \
        '{bead_id:$bead,surface:"rerank_precision_gain_e2e",label:$label,redaction_status:"local_workspace_artifacts_retained"}')"
}

run_ee_json() {
    local label="${1:?label required}"
    shift
    local stdout_file stderr_file
    stdout_file="${LOG_DIR}/step_$(printf '%02d' "$((STEP + 1))")_${label//[^A-Za-z0-9_]/_}.stdout.json"
    stderr_file="${LOG_DIR}/step_$(printf '%02d' "$((STEP + 1))")_${label//[^A-Za-z0-9_]/_}.stderr.txt"
    log_step "${label}"
    local exit_code
    set +e
    env HOME="${HOME_DIR}" NO_COLOR=1 "${REAL_EE}" --workspace "${WORKSPACE}" "$@" >"${stdout_file}" 2>"${stderr_file}"
    exit_code=$?
    set -e
    emit_event "command_end" "$(jq -cn --arg bead "${BEAD}" --arg label "${label}" \
        --argjson exitCode "${exit_code}" --arg stdout "${stdout_file}" --arg stderr "${stderr_file}" \
        '{bead_id:$bead,surface:"rerank_precision_gain_e2e",label:$label,exit_code:$exitCode,stdout_artifact_path:$stdout,stderr_artifact_path:$stderr,redaction_status:"local_workspace_artifacts_retained"}')"
    if [[ "${exit_code}" -ne 0 ]]; then
        record_failure "${label}" "ee exited ${exit_code}; stdout=${stdout_file} stderr=${stderr_file}"
    elif ! jq -e 'type == "object"' "${stdout_file}" >/dev/null 2>&1; then
        record_failure "${label}" "stdout was not a JSON object; stdout=${stdout_file}"
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

write_rerank_config() {
    local mode="${1:?rerank mode required}"
    mkdir -p "${WORKSPACE}/.ee"
    {
        printf '[search]\n'
        printf 'rerank = "%s"\n' "${mode}"
    } >"${WORKSPACE}/.ee/config.toml"
    emit_event "note" "$(jq -cn --arg bead "${BEAD}" --arg mode "${mode}" \
        '{bead_id:$bead,surface:"rerank_precision_gain_e2e",label:"rerank_config_written",rerank_mode:$mode,redaction_status:"local_workspace_artifacts_retained"}')"
}

order_of() { jq -c '[ (.data.results // [])[] | .memoryId ]' "${1:?file required}"; }

rank_in_order() {
    # Echo the 0-based rank of $2 in the JSON array $1, or -1 when absent.
    printf '%s' "${1:?order json required}" | jq --arg id "${2:?id required}" '(. | index($id)) // -1'
}

finish() {
    emit_event "note" "$(jq -cn --arg bead "${BEAD}" --arg logDir "${LOG_DIR}" --argjson failures "${FAILURES}" \
        '{bead_id:$bead,surface:"rerank_precision_gain_e2e",label:"summary",failures:$failures,log_dir:$logDir,redaction_status:"local_workspace_artifacts_retained"}')"
    printf 'rerank_precision_gain_e2e: failures=%d require_rerank=%s log_dir=%s\n' "${FAILURES}" "${REQUIRE_RERANK}" "${LOG_DIR}" >&2
    if [[ "${FAILURES}" -gt 0 ]]; then exit 1; fi
}
trap finish EXIT

emit_event "note" "$(jq -cn --arg bead "${BEAD}" --arg ee "${REAL_EE}" --arg workspace "${WORKSPACE}" \
    --arg requireRerank "${REQUIRE_RERANK}" --arg logDir "${LOG_DIR}" \
    '{bead_id:$bead,surface:"rerank_precision_gain_e2e",label:"start",ee_bin:$ee,workspace:$workspace,require_rerank:$requireRerank,log_dir:$logDir,redaction_status:"local_workspace_artifacts_retained"}')"

QUERY="release format checklist cargo clippy"
PRECISE_SENTINEL="BD2VQ2Z26_PRECISE_RERANK_TARGET"
LEXICAL_TRAP="BD2VQ2Z26_LEXICAL_TRAP"

run_ee_json "init_workspace" init --json
assert_jq_file "${LAST_STDOUT_FILE}" '.success' "true" "init succeeds"

# The lexical trap keyword-stuffs the query terms so a pure lexical/fusion order
# ranks it above the precise policy target -- the precondition for a measurable
# precision gain once a real reranker reorders the candidates.
run_ee_json "remember_lexical_trap" remember \
    "${LEXICAL_TRAP} release release release format format cargo cargo clippy clippy words repeat, but this note is only a noisy lexical trap and not the policy target." \
    --level semantic --kind fact --tags "${BEAD},rerank,trap" --json
assert_jq_file "${LAST_STDOUT_FILE}" '.success' "true" "remember lexical trap succeeds"

run_ee_json "remember_precise_target" remember \
    "${PRECISE_SENTINEL} The correct release policy target says run cargo fmt --check and cargo clippy before publishing a Rust release." \
    --level procedural --kind rule --tags "${BEAD},rerank,target" --json
precise_file="${LAST_STDOUT_FILE}"
assert_jq_file "${precise_file}" '.success' "true" "remember precise target succeeds"
precise_id="$(jq -r '(.data.memory_id // .data.memoryId // "")' "${precise_file}")"
if [[ -n "${precise_id}" && "${precise_id}" != "null" ]]; then
    record_pass "precise target memory id captured"
else
    record_failure "precise target memory id captured" "missing memory id in precise remember response file=${precise_file}"
fi

run_ee_json "remember_noise" remember \
    "Design review notes discuss onboarding screenshots and do not mention release formatting gates." \
    --level episodic --kind note --tags "${BEAD},rerank,noise" --json
assert_jq_file "${LAST_STDOUT_FILE}" '.success' "true" "remember noise succeeds"

run_ee_json "index_rebuild" index rebuild --json
assert_jq_file "${LAST_STDOUT_FILE}" '.success' "true" "index rebuild succeeds"

# --- Fusion-only baseline: same hybrid candidate set, reranker explicitly off.
write_rerank_config "off"
run_ee_json "search_fusion_only_baseline" search "${QUERY}" --limit 3 --relevance-floor 0 --json
fusion_file="${LAST_STDOUT_FILE}"
assert_jq_file "${fusion_file}" '.schema' "ee.response.v2" "fusion-only baseline envelope schema"
assert_jq_file "${fusion_file}" '.success' "true" "fusion-only baseline succeeds"
assert_jq_file "${fusion_file}" '((.data.results // []) | length) >= 2' "true" "fusion-only baseline returns multiple results"
# Prove no reranking touched the baseline order so the comparison is honest.
assert_jq_file "${fusion_file}" '((.data.rerank.rerankScoreCount // 0) | tonumber) == 0' "true" "fusion-only baseline carries zero rerank scores"
assert_jq_file "${fusion_file}" '((.data.results // []) | map(select(.scoreKind == "reranked" or has("rerankScore"))) | length)' "0" "fusion-only baseline omits rerankScore"
fusion_order="$(order_of "${fusion_file}")"
fusion_precise_rank="$(rank_in_order "${fusion_order}" "${precise_id}")"
emit_event "fusion_only_baseline_observed" "$(jq -cn --arg bead "${BEAD}" --argjson order "${fusion_order:-[]}" \
    --arg precise "${precise_id}" --argjson preciseRank "${fusion_precise_rank}" \
    '{bead_id:$bead,surface:"rerank_precision_gain_e2e",label:"fusion_only_baseline",order:$order,precise_memory_id:$precise,precise_rank:$preciseRank,redaction_status:"local_workspace_artifacts_retained"}')"

# Restore auto so the next run can exercise a real cross-encoder when present.
write_rerank_config "auto"

run_ee_json "search_hybrid_rerank" search "${QUERY}" --limit 3 --relevance-floor 0 --json
hybrid_file="${LAST_STDOUT_FILE}"
assert_jq_file "${hybrid_file}" '.schema' "ee.response.v2" "hybrid search envelope schema"
assert_jq_file "${hybrid_file}" '.success' "true" "hybrid search succeeds"
assert_jq_file "${hybrid_file}" '.data.rerank.schema' "ee.rerank_posture.v1" "hybrid search carries rerank posture schema"

rerank_mode="$(jq -r '.data.rerank.mode' "${hybrid_file}")"
emit_event "rerank_mode_observed" "$(jq -cn --arg bead "${BEAD}" --arg mode "${rerank_mode}" --arg require "${REQUIRE_RERANK}" \
    '{bead_id:$bead,surface:"rerank_precision_gain_e2e",label:"rerank_mode",mode:$mode,require_rerank:$require,redaction_status:"local_workspace_artifacts_retained"}')"

case "${rerank_mode}" in
    reranked)
        assert_jq_file "${hybrid_file}" '.data.rerank.available' "true" "reranked mode reports available"
        assert_jq_file "${hybrid_file}" '((.data.rerank.rerankScoreCount // 0) | tonumber) > 0' "true" "reranked mode reports rerank scores"
        assert_jq_file "${hybrid_file}" '((.data.results // []) | map(select(.scoreKind == "reranked" and has("rerankScore"))) | length) > 0' "true" "reranked results expose scoreKind and rerankScore"

        reranked_order="$(order_of "${hybrid_file}")"
        reranked_precise_rank="$(rank_in_order "${reranked_order}" "${precise_id}")"

        # (a) Top-K order must change versus the fusion-only baseline.
        if [[ -n "${fusion_order}" && "${reranked_order}" != "${fusion_order}" ]]; then
            record_pass "rerank changes top-K order vs fusion-only baseline"
        else
            record_failure "rerank changes top-K order vs fusion-only baseline" \
                "reranked order equals fusion-only order (pass-through reranker?): ${reranked_order}"
        fi

        # (b) precision_gain_at_1 must be strictly positive.
        fusion_p1=0; [[ "${fusion_precise_rank}" == "0" ]] && fusion_p1=1
        reranked_p1=0; [[ "${reranked_precise_rank}" == "0" ]] && reranked_p1=1
        precision_gain_at_1=$(( reranked_p1 - fusion_p1 ))
        emit_event "precision_gain_measured" "$(jq -cn --arg bead "${BEAD}" --arg precise "${precise_id}" \
            --argjson fusionOrder "${fusion_order:-[]}" --argjson rerankedOrder "${reranked_order:-[]}" \
            --argjson fusionRank "${fusion_precise_rank}" --argjson rerankedRank "${reranked_precise_rank}" \
            --argjson fusionP1 "${fusion_p1}" --argjson rerankedP1 "${reranked_p1}" --argjson gain "${precision_gain_at_1}" \
            '{bead_id:$bead,surface:"rerank_precision_gain_e2e",label:"precision_gain_at_1",precise_memory_id:$precise,fusion_only_order:$fusionOrder,reranked_order:$rerankedOrder,precise_rank_fusion_only:$fusionRank,precise_rank_reranked:$rerankedRank,precision_at_1_fusion_only:$fusionP1,precision_at_1_reranked:$rerankedP1,precision_gain_at_1:$gain,redaction_status:"local_workspace_artifacts_retained"}')"
        if [[ "${precision_gain_at_1}" -gt 0 ]]; then
            record_pass "rerank yields positive precision_gain_at_1"
        else
            record_failure "rerank yields positive precision_gain_at_1" \
                "precision_gain_at_1=${precision_gain_at_1} (fusion_rank=${fusion_precise_rank} reranked_rank=${reranked_precise_rank}); a real rerank must lift the precise target to rank 1"
        fi

        # The promoted top result must be the precise policy target, not the trap.
        assert_jq_file "${hybrid_file}" "(.data.results[0].content // \"\" | contains(\"${PRECISE_SENTINEL}\"))" "true" "reranked top result is the precise target"
        assert_jq_file "${hybrid_file}" "(.data.results[0].content // \"\" | contains(\"${LEXICAL_TRAP}\") | not)" "true" "reranked top result is not the lexical trap"
        ;;
    fusion_only_degraded)
        assert_jq_file "${hybrid_file}" '.data.rerank.available' "false" "degraded mode reports unavailable"
        assert_jq_file "${hybrid_file}" '.data.rerank.degradedCode' "rerank_model_unavailable" "degraded mode names rerank_model_unavailable"
        if [[ "${REQUIRE_RERANK}" == "1" ]]; then
            record_failure "required rerank lane must not degrade" \
                "EE_E2E_REQUIRE_RERANK=1 but hybrid search returned fusion_only_degraded; provision a reranker artifact for this lane so mode=reranked and precision_gain_at_1>0 are exercised"
        else
            record_pass "fusion-only-degraded accepted in non-required lane"
            emit_event "note" "$(jq -cn --arg bead "${BEAD}" \
                '{bead_id:$bead,surface:"rerank_precision_gain_e2e",label:"rerank_skipped_no_model",detail:"no reranker provisioned; precision-gain assertions skipped. Set EE_E2E_REQUIRE_RERANK=1 with a pre-provisioned reranker to exercise them.",redaction_status:"local_workspace_artifacts_retained"}')"
        fi
        ;;
    fusion_only)
        record_failure "hybrid rerank posture is decisive" "hybrid search returned fusion_only without rerank_model_unavailable"
        ;;
    *)
        record_failure "hybrid rerank posture mode known" "unexpected mode=${rerank_mode}"
        ;;
esac
