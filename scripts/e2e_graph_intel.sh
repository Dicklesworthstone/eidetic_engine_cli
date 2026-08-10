#!/usr/bin/env bash
# bd-3a1op.6 — graph-intelligence E2E (ADR 0066): suggest-links prediction,
# --propose candidate emission with dedup, and the curate validate/apply
# lifecycle creating the real typed link. Real binary, no mocks.
#
# Environment:
#   EE_BIN / EE_BINARY  Path to the ee binary (default: `ee` on PATH)
#   EE_E2E_TMPDIR       Temp base (default /private/tmp)

set -uo pipefail

TEST_ID="graph_intel_e2e"
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
ROOT="$(mktemp -d "${ROOT_BASE%/}/ee-graph-intel-e2e.XXXXXX")"
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

remember_id() {
    jq -r '.data.memoryId // .data.memory_id // .data.id // empty' "${LAST_STDOUT}"
}

run_ee init --workspace "${WS}" --json
[[ "${LAST_EXIT}" -eq 0 ]] && event init_ok pass || event init_ok fail "init exit ${LAST_EXIT}"

# --- empty graph reports the honest insufficient-graph code --------------
run_ee graph suggest-links --workspace "${WS}" --json
if [[ "${LAST_EXIT}" -eq 0 ]] \
    && jq -e '[.degraded[]? | select(.code == "suggest_links_insufficient_graph")] | length >= 1' \
        "${LAST_STDOUT}" >/dev/null 2>&1; then
    event empty_graph_reports_insufficient pass
else
    event empty_graph_reports_insufficient fail "exit ${LAST_EXIT}; $(head -c 250 "${LAST_STDOUT}")"
fi

# --- corpus: a hub pattern (a-c, b-c linked; a,b share tags) -------------
run_ee remember "Always run the schema drift gate before altering public JSON contracts." \
    --workspace "${WS}" --level procedural --kind rule --tags "contracts,gates" --json
MEM_A="$(remember_id)"
run_ee remember "Never alter public JSON contracts without running the schema drift gate first." \
    --workspace "${WS}" --level procedural --kind rule --tags "contracts,gates" --json
MEM_B="$(remember_id)"
run_ee remember "The schema drift gate lives in the contracts test harness." \
    --workspace "${WS}" --level semantic --kind fact --tags "contracts" --json
MEM_C="$(remember_id)"
if [[ -n "${MEM_A}" && -n "${MEM_B}" && -n "${MEM_C}" ]]; then
    event corpus_seeded pass
else
    event corpus_seeded fail "ids: a=${MEM_A} b=${MEM_B} c=${MEM_C}"
fi

run_ee memory link "${MEM_A}" "${MEM_C}" --relation related --workspace "${WS}" --json
LINK_AC=$LAST_EXIT
run_ee memory link "${MEM_B}" "${MEM_C}" --relation related --workspace "${WS}" --json
LINK_BC=$LAST_EXIT
[[ "${LINK_AC}" -eq 0 && "${LINK_BC}" -eq 0 ]] && event hub_links_created pass \
    || event hub_links_created fail "link exits ${LINK_AC}/${LINK_BC}"

# --- suggestion: (a,b) share neighbor c and tags; opposed polarity -------
run_ee graph suggest-links --workspace "${WS}" --json
PAIR_FILTER='.data.report.suggestions[]? | select((.memoryA == $a and .memoryB == $b) or (.memoryA == $b and .memoryB == $a))'
if jq -e --arg a "${MEM_A}" --arg b "${MEM_B}" "[${PAIR_FILTER}] | length >= 1" \
    "${LAST_STDOUT}" >/dev/null 2>&1; then
    event pair_suggested pass
    SUGGESTED_RELATION="$(jq -r --arg a "${MEM_A}" --arg b "${MEM_B}" "[${PAIR_FILTER}][0].suggestedRelation" "${LAST_STDOUT}")"
    if [[ "${SUGGESTED_RELATION}" == "contradicts" ]]; then
        event opposed_polarity_types_contradicts pass
    else
        event opposed_polarity_types_contradicts fail "always/never pair typed ${SUGGESTED_RELATION}"
    fi
    if jq -e --arg a "${MEM_A}" --arg b "${MEM_B}" "[${PAIR_FILTER}][0].signals | has(\"adamicAdar\") and has(\"ppr\")" \
        "${LAST_STDOUT}" >/dev/null 2>&1; then
        event signals_carried_per_row pass
    else
        event signals_carried_per_row fail "missing per-signal values"
    fi
else
    event pair_suggested fail "exit ${LAST_EXIT}; $(head -c 300 "${LAST_STDOUT}")"
    event opposed_polarity_types_contradicts fail "no pair suggestion"
    event signals_carried_per_row fail "no pair suggestion"
fi

# --- propose writes candidates; re-propose dedups ------------------------
run_ee graph suggest-links --workspace "${WS}" --propose --json
FIRST_CANDIDATE="$(jq -r --arg a "${MEM_A}" --arg b "${MEM_B}" "[${PAIR_FILTER}][0].proposedCandidateId // empty" "${LAST_STDOUT}")"
if [[ "${LAST_EXIT}" -eq 0 && -n "${FIRST_CANDIDATE}" ]]; then
    event propose_writes_candidate pass
else
    event propose_writes_candidate fail "exit ${LAST_EXIT}; candidate=${FIRST_CANDIDATE}"
fi
run_ee graph suggest-links --workspace "${WS}" --propose --json
SECOND_CANDIDATE="$(jq -r --arg a "${MEM_A}" --arg b "${MEM_B}" "[${PAIR_FILTER}][0].proposedCandidateId // empty" "${LAST_STDOUT}")"
if [[ -n "${FIRST_CANDIDATE}" && "${FIRST_CANDIDATE}" == "${SECOND_CANDIDATE}" ]]; then
    event repropose_dedups_to_existing pass
else
    event repropose_dedups_to_existing fail "first=${FIRST_CANDIDATE} second=${SECOND_CANDIDATE}"
fi

# --- curate lifecycle: validate then apply creates the typed link --------
if [[ -n "${FIRST_CANDIDATE}" ]]; then
    run_ee curate validate "${FIRST_CANDIDATE}" --workspace "${WS}" --json
    VALIDATE_EXIT=$LAST_EXIT
    run_ee curate apply "${FIRST_CANDIDATE}" --workspace "${WS}" --json
    if [[ "${VALIDATE_EXIT}" -eq 0 && "${LAST_EXIT}" -eq 0 ]]; then
        event candidate_lifecycle_applies pass
    else
        event candidate_lifecycle_applies fail "validate=${VALIDATE_EXIT} apply=${LAST_EXIT}: $(head -c 250 "${LAST_STDOUT}")"
    fi
    run_ee graph explain-link "${MEM_A}" "${MEM_B}" --workspace "${WS}" --json
    if [[ "${LAST_EXIT}" -eq 0 ]] && grep -q 'contradicts' "${LAST_STDOUT}"; then
        event applied_link_visible_in_graph pass
    else
        event applied_link_visible_in_graph fail "exit ${LAST_EXIT}; $(head -c 250 "${LAST_STDOUT}")"
    fi
else
    event candidate_lifecycle_applies fail "no candidate id from propose"
    event applied_link_visible_in_graph fail "no candidate id from propose"
fi

echo
echo "graph intel e2e: ${STEP} steps, ${FAILURES} failures; root ${ROOT}"
if [[ "${FAILURES}" -gt 0 ]]; then
    exit 2
fi
exit 0
