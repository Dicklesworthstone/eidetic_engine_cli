#!/usr/bin/env bash
# shellcheck disable=SC2015
# bd-2tehh.4 — shadow retrieval-tuning E2E (ADR 0070).
#
# Real-binary, no-Cargo E2E for the ee shadow surface:
#   - a sparse corpus abstains with insufficient_outcome_evidence and still
#     persists the report;
#   - promote on an abstained report is a typed exit-7 refusal;
#   - editing an abstained report into a purported winner cannot authorize
#     either dry-run or live promotion;
#   - rejected reports leave config bytes and demotion authority unchanged;
#   - a fresh evaluator run restores an authentic, still-abstained report.
# Positive promotion and selective rollback use evaluator-backed receipts in
# the core shadow_tuning tests; this sparse CLI corpus proves the refusal path.
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

E2E_SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="${REPO_ROOT:-$(cd "${E2E_SCRIPT_DIR}/.." && pwd)}"
# shellcheck source=scripts/lib/ee_binary_resolution.sh
# shellcheck disable=SC1091
source "${REPO_ROOT}/scripts/lib/ee_binary_resolution.sh"

# bd-z6g1s: the default was bare `ee`, i.e. PATH. verify.sh passes
# EE_BIN/EE_BINARY to every stage, so that was dormant there and live for any
# standalone run -- including rch exec --job, where a sibling suite silently
# graded the installed binary instead of the one the job built. Resolve from
# Cargo's effective target directory; EE_BIN/EE_BINARY still win.
EE_BIN="${EE_BIN:-${EE_BINARY:-$(ee_resolve_binary debug)}}"
if [[ "${EE_BIN}" == */* ]]; then
    REAL_EE="${EE_BIN}"
else
    REAL_EE="$(command -v "${EE_BIN}" 2>/dev/null || true)"
fi
if [[ -z "${REAL_EE}" || ! -x "${REAL_EE}" ]]; then
    echo "ee binary missing (set EE_BIN)" >&2
    exit 3
fi
# Resolving a path is not enough: a resolved binary can be stale, or foreign to
# this host (an RCH run can leave a Linux ELF in a shared target dir, which
# satisfies -x and dies with exec format error). Refuse rather than report
# assertion failures with one environmental cause.
if ! ee_require_current_binary "${REAL_EE}" "e2e_shadow_retrieval_tuning"; then
    exit 3
fi

# bd-mfqa2: the macOS temp path is a PLATFORM preference, not a default for
# every host. Resolved centrally so this script does not re-implement it.
# shellcheck source=scripts/lib/e2e_tmproot.sh
# shellcheck disable=SC1091
source "$(dirname "${BASH_SOURCE[0]}")/lib/e2e_tmproot.sh"
ROOT_BASE="$(e2e_temp_root)"
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

# --- a changed report cannot manufacture evidence or authorize promotion ---
if [[ -f "${REPORT_FILE}" ]]; then
    jq '.abstained = false
        | .abstentionReason = null
        | .promotable = true
        | .winner = {weights: {lexical: 0.55, semantic: 0.35, graph: 0.10}, score: 1.0, origin: "grid", relativeMargin: 0.5}' \
        "${REPORT_FILE}" >"${REPORT_FILE}.tmp" && mv "${REPORT_FILE}.tmp" "${REPORT_FILE}"
fi
CONFIG_FILE="${WS}/.ee/config.toml"
PRIOR_CONFIG_FILE="${LOG_DIR}/config.before.toml"
PRIOR_CONFIG_PRESENT=false
if [[ -f "${CONFIG_FILE}" ]]; then
    PRIOR_CONFIG_PRESENT=true
    cp "${CONFIG_FILE}" "${PRIOR_CONFIG_FILE}"
fi

config_unchanged() {
    if [[ "${PRIOR_CONFIG_PRESENT}" == true ]]; then
        [[ -f "${CONFIG_FILE}" ]] && cmp -s "${PRIOR_CONFIG_FILE}" "${CONFIG_FILE}"
    else
        [[ ! -e "${CONFIG_FILE}" ]]
    fi
}

run_ee shadow promote --dry-run --workspace "${WS}" --json
if [[ "${LAST_EXIT}" -eq 7 ]] && config_unchanged; then
    event tampered_report_dry_run_refused pass
else
    event tampered_report_dry_run_refused fail "exit ${LAST_EXIT}; $(head -c 200 "${LAST_STDOUT}")"
fi

run_ee shadow promote --workspace "${WS}" --json
if [[ "${LAST_EXIT}" -eq 7 ]] && config_unchanged; then
    event tampered_report_promotion_refused pass
else
    event tampered_report_promotion_refused fail "exit ${LAST_EXIT}; $(head -c 200 "${LAST_STDOUT}")"
fi

run_ee shadow demote --workspace "${WS}" --json
if [[ "${LAST_EXIT}" -eq 7 ]] && config_unchanged; then
    event rejected_report_grants_no_demotion_authority pass
else
    event rejected_report_grants_no_demotion_authority fail "exit ${LAST_EXIT}; $(head -c 200 "${LAST_STDOUT}")"
fi

run_ee shadow run --policy "${POLICY}" --workspace "${WS}" --json
if [[ "${LAST_EXIT}" -eq 0 ]] \
    && [[ "$(jq -r '.data.report.abstained' "${LAST_STDOUT}")" == true ]] \
    && [[ "$(jq -r '.promotable' "${REPORT_FILE}")" == false ]] \
    && config_unchanged; then
    event rerun_restores_authentic_abstention pass
else
    event rerun_restores_authentic_abstention fail "exit ${LAST_EXIT}; $(head -c 200 "${LAST_STDOUT}")"
fi

echo
echo "shadow tuning e2e: $((STEP)) steps, ${FAILURES} failures; workspace ${ROOT}"
if [[ "${FAILURES}" -gt 0 ]]; then
    exit 2
fi
exit 0
