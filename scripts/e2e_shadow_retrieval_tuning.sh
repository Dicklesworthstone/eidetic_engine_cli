#!/usr/bin/env bash
# shellcheck disable=SC2015
# bd-2tehh.4 — shadow retrieval-tuning E2E (ADR 0070).
#
# Real-binary, no-Cargo E2E for the ee shadow surface:
#   - a sparse corpus abstains with insufficient_outcome_evidence and still
#     persists the report;
#   - promote on an abstained report is a typed exit-7 refusal;
#   - a promotable report (persisted report edited in place — promote trusts
#     the local report file by design; same trust domain as config.toml)
#     dry-runs without writing, then applies the [search] overlay;
#   - demote restores the pre-promotion config bytes exactly.
#
# Environment:
#   EE_BIN / EE_BINARY  Path to the ee binary (default: `ee` on PATH)
#   EE_E2E_TMPDIR       Temp base (default /private/tmp)

set -uo pipefail

TEST_ID="shadow_retrieval_tuning_e2e"
POLICY="candidate.retrieval.outcome_tuned_weights"
FAILURES=0
STEP=0

if ! command -v jq >/dev/null 2>&1; then
    echo "jq missing" >&2
    exit 3
fi

EE_BIN="${EE_BIN:-${EE_BINARY:-ee}}"
if [[ "${EE_BIN}" == */* ]]; then
    REAL_EE="${EE_BIN}"
else
    REAL_EE="$(command -v "${EE_BIN}" 2>/dev/null || true)"
fi
if [[ -z "${REAL_EE}" || ! -x "${REAL_EE}" ]]; then
    echo "ee binary missing (set EE_BIN)" >&2
    exit 3
fi

ROOT_BASE="${EE_E2E_TMPDIR:-/private/tmp}"
ROOT="$(mktemp -d "${ROOT_BASE%/}/ee-shadow-tuning-e2e.XXXXXX")"
WS="${ROOT}/ws"
LOG_DIR="${ROOT}/logs"
EVENT_LOG="${LOG_DIR}/events.jsonl"
mkdir -p "${WS}" "${LOG_DIR}"
: >"${EVENT_LOG}"

event() {
    local label="$1" status="$2" diagnosis="${3:-}"
    jq -cn --arg test_id "${TEST_ID}" --arg label "${label}" --arg status "${status}" \
        --arg diagnosis "${diagnosis}" \
        '{schema:"ee.test_event.v1",test_id:$test_id,kind:"assert_result",fields:{label:$label,status:$status,first_failure_diagnosis:$diagnosis}}' \
        | tee -a "${EVENT_LOG}"
    if [[ "${status}" == "fail" ]]; then
        FAILURES=$((FAILURES + 1))
    fi
}

run_ee() {
    STEP=$((STEP + 1))
    local out="${LOG_DIR}/step${STEP}.stdout" err="${LOG_DIR}/step${STEP}.stderr"
    "${REAL_EE}" "$@" >"${out}" 2>"${err}"
    LAST_EXIT=$?
    LAST_STDOUT="${out}"
    return 0
}

# --- corpus: two memories, one pack, one pack-item outcome (sparse) ---
run_ee init --workspace "${WS}" --json
[[ "${LAST_EXIT}" -eq 0 ]] && event init_ok pass || event init_ok fail "init exit ${LAST_EXIT}"

run_ee remember "Run cargo fmt --check before release." --workspace "${WS}" --level procedural --kind rule --json
[[ "${LAST_EXIT}" -eq 0 ]] && event remember_ok pass || event remember_ok fail "remember exit ${LAST_EXIT}"
MEMORY_ID="$(jq -r '.data.memoryId // .data.memory_id // .data.id // empty' "${LAST_STDOUT}")"

run_ee pack "prepare release" --workspace "${WS}" --max-tokens 800 --json
[[ "${LAST_EXIT}" -eq 0 ]] && event pack_ok pass || event pack_ok fail "pack exit ${LAST_EXIT}"
PACK_ID="$(jq -r '.data.pack.packId // .data.pack.pack_id // .data.packId // empty' "${LAST_STDOUT}")"

if [[ -n "${MEMORY_ID}" && -n "${PACK_ID}" ]]; then
    run_ee outcome "${MEMORY_ID}" --pack "${PACK_ID}" --item 1 --signal helpful --workspace "${WS}" --json
    [[ "${LAST_EXIT}" -eq 0 ]] && event outcome_ok pass || event outcome_ok fail "outcome exit ${LAST_EXIT}"
else
    event outcome_ok fail "memory or pack id missing (memory=${MEMORY_ID} pack=${PACK_ID})"
fi

# --- sparse run: abstains, persists the report ---
run_ee shadow run --policy "${POLICY}" --workspace "${WS}" --json
if [[ "${LAST_EXIT}" -eq 0 ]] \
    && [[ "$(jq -r '.data.report.abstained' "${LAST_STDOUT}")" == "true" ]] \
    && [[ "$(jq -r '.degraded[0].code // empty' "${LAST_STDOUT}")" == "insufficient_outcome_evidence" ]]; then
    event sparse_run_abstains pass
else
    event sparse_run_abstains fail "exit ${LAST_EXIT}; $(head -c 200 "${LAST_STDOUT}")"
fi
REPORT_FILE="${WS}/.ee/shadow/retrieval_tuning_report.json"
[[ -f "${REPORT_FILE}" ]] && event report_persisted pass || event report_persisted fail "missing ${REPORT_FILE}"

# --- promote on an abstained report refuses with exit 7 ---
run_ee shadow promote --workspace "${WS}" --json
[[ "${LAST_EXIT}" -eq 7 ]] && event promote_abstained_refuses_exit7 pass \
    || event promote_abstained_refuses_exit7 fail "exit ${LAST_EXIT} (wanted 7)"

# --- make the persisted report promotable (in place; promote trusts the
# --- local report file by design) and exercise dry-run + apply + demote ---
if [[ -f "${REPORT_FILE}" ]]; then
    jq '.abstained = false
        | .abstentionReason = null
        | .promotable = true
        | .winner = {weights: {lexical: 0.55, semantic: 0.35, graph: 0.10}, score: 1.0, origin: "grid", relativeMargin: 0.5}' \
        "${REPORT_FILE}" >"${REPORT_FILE}.tmp" && mv "${REPORT_FILE}.tmp" "${REPORT_FILE}"
fi
CONFIG_FILE="${WS}/.ee/config.toml"
PRIOR_CONFIG=""
[[ -f "${CONFIG_FILE}" ]] && PRIOR_CONFIG="$(cat "${CONFIG_FILE}")"

run_ee shadow promote --dry-run --workspace "${WS}" --json
if [[ "${LAST_EXIT}" -eq 0 ]] && [[ "$(jq -r '.data.applied' "${LAST_STDOUT}")" == "false" ]]; then
    CURRENT=""
    [[ -f "${CONFIG_FILE}" ]] && CURRENT="$(cat "${CONFIG_FILE}")"
    [[ "${CURRENT}" == "${PRIOR_CONFIG}" ]] && event promote_dry_run_writes_nothing pass \
        || event promote_dry_run_writes_nothing fail "config changed under --dry-run"
else
    event promote_dry_run_writes_nothing fail "exit ${LAST_EXIT}; $(head -c 200 "${LAST_STDOUT}")"
fi

run_ee shadow promote --workspace "${WS}" --json
if [[ "${LAST_EXIT}" -eq 0 ]] && grep -q 'lexical_weight' "${CONFIG_FILE}" 2>/dev/null \
    && grep -q '\[search\]' "${CONFIG_FILE}" 2>/dev/null; then
    event promote_applies_overlay pass
else
    event promote_applies_overlay fail "exit ${LAST_EXIT}; config: $(head -c 200 "${CONFIG_FILE}" 2>/dev/null || echo missing)"
fi

run_ee shadow demote --workspace "${WS}" --json
RESTORED=""
[[ -f "${CONFIG_FILE}" ]] && RESTORED="$(cat "${CONFIG_FILE}")"
if [[ "${LAST_EXIT}" -eq 0 ]] && [[ "${RESTORED}" == "${PRIOR_CONFIG}" ]]; then
    event demote_restores_bytes pass
else
    event demote_restores_bytes fail "exit ${LAST_EXIT}; restored bytes differ from prior"
fi

echo
echo "shadow tuning e2e: $((STEP)) steps, ${FAILURES} failures; workspace ${ROOT}"
if [[ "${FAILURES}" -gt 0 ]]; then
    exit 2
fi
exit 0
