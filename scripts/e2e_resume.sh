#!/usr/bin/env bash
# bd-resume-verb-v0f57 — `ee resume` E2E: the acceptance fixture from the
# bead (tagged sessions + revisit decisions + a next-tagged item + a
# superseded stale note) against the real binary. No mocks.
#
# Environment:
#   EE_BIN / EE_BINARY  Path to the ee binary (default: `ee` on PATH)
#   EE_E2E_TMPDIR       Temp base (default /private/tmp)

set -uo pipefail

TEST_ID="resume_e2e"
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
ROOT="$(mktemp -d "${ROOT_BASE%/}/ee-resume-e2e.XXXXXX")"
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

run_ee init --workspace "${WS}" --json
[[ "${LAST_EXIT}" -eq 0 ]] && event init_ok pass || event init_ok fail "init exit ${LAST_EXIT}"

# --- empty store: honest no-session-evidence degradation ------------------
run_ee resume --workspace "${WS}" --json
if [[ "${LAST_EXIT}" -eq 0 ]] \
    && jq -e '[.degraded[]? | select(.code == "resume_no_session_evidence")] | length >= 1' \
        "${LAST_STDOUT}" >/dev/null 2>&1; then
    event empty_store_reports_no_evidence pass
else
    event empty_store_reports_no_evidence fail "exit ${LAST_EXIT}; $(head -c 250 "${LAST_STDOUT}")"
fi

# --- corpus: two tagged sessions, a queued item, a stale note -------------
run_ee remember "Session A wrapped the parser refactor." \
    --workspace "${WS}" --level episodic --kind note --tags "session-20260801" --json
run_ee remember "Session A left the lexer half-done." \
    --workspace "${WS}" --level episodic --kind note --tags "session-20260801" --json
run_ee remember "Session B finished the lexer and started codegen." \
    --workspace "${WS}" --level episodic --kind note --tags "session-20260808" --json
run_ee remember "Next: wire codegen into the driver." \
    --workspace "${WS}" --level episodic --kind note --tags "next,codegen" --json
STALE_OLD_EXIT=$LAST_EXIT
run_ee remember "Next: codegen driver wiring landed; next is optimization passes." \
    --workspace "${WS}" --level episodic --kind note --tags "next,codegen" --json
STALE_NEW_EXIT=$LAST_EXIT
[[ "${STALE_OLD_EXIT}" -eq 0 && "${STALE_NEW_EXIT}" -eq 0 ]] \
    && event corpus_seeded pass || event corpus_seeded fail "remember exits ${STALE_OLD_EXIT}/${STALE_NEW_EXIT}"

run_ee decide record "resume e2e revisit decision" --chosen "ship now" \
    --alternative "wait for perf pass" --rationale "deadline wins" \
    --revisit-by "+30d" --workspace "${WS}" --json
DECIDE_ONE=$LAST_EXIT
run_ee decide record "resume e2e second decision" --chosen "keep sqlite" \
    --alternative "switch stores" --rationale "franken-stack rule" \
    --revisit-by "+90d" --workspace "${WS}" --json
DECIDE_TWO=$LAST_EXIT
[[ "${DECIDE_ONE}" -eq 0 && "${DECIDE_TWO}" -eq 0 ]] \
    && event decisions_recorded pass || event decisions_recorded fail "decide exits ${DECIDE_ONE}/${DECIDE_TWO}"

# --- the bundle -----------------------------------------------------------
run_ee resume --workspace "${WS}" --json
R="${LAST_STDOUT}"
if [[ "${LAST_EXIT}" -eq 0 ]] \
    && jq -e '.data.report.schema == "ee.resume.v1"' "${R}" >/dev/null 2>&1; then
    event resume_returns_schema pass
else
    event resume_returns_schema fail "exit ${LAST_EXIT}; $(head -c 250 "${R}")"
fi

if jq -e '[.data.report.sessions[]?.label] | index("session-20260808") != null and index("session-20260801") != null' \
    "${R}" >/dev/null 2>&1; then
    event tagged_sessions_grouped pass
else
    event tagged_sessions_grouped fail "$(jq -c '[.data.report.sessions[]?.label]' "${R}" 2>/dev/null)"
fi

if jq -e '[.data.report.openLoops.revisitDecisions[]? | select(.revisitBy != null)] | length == 2' \
    "${R}" >/dev/null 2>&1; then
    event revisit_decisions_surfaced pass
else
    event revisit_decisions_surfaced fail "$(jq -c '.data.report.openLoops.revisitDecisions' "${R}" 2>/dev/null | head -c 250)"
fi

if jq -e '[.data.report.openLoops.taggedItems[]? | select(.tags | index("next"))] | length >= 1' \
    "${R}" >/dev/null 2>&1; then
    event next_tagged_item_surfaced pass
else
    event next_tagged_item_surfaced fail "$(jq -c '.data.report.openLoops.taggedItems' "${R}" 2>/dev/null | head -c 250)"
fi

# The older next-note shares kind + 2 tags with the newer one -> stale flag.
if jq -e '.data.report.staleCount >= 1
          and ([.. | objects | select(has("stale")) | select(.stale != null)
                | select(.content | contains("wire codegen into the driver"))] | length >= 1)' \
    "${R}" >/dev/null 2>&1; then
    event superseded_note_carries_stale_marker pass
else
    event superseded_note_carries_stale_marker fail "staleCount=$(jq -r '.data.report.staleCount' "${R}" 2>/dev/null); $(head -c 250 "${R}")"
fi

echo
echo "resume e2e: ${STEP} steps, ${FAILURES} failures; root ${ROOT}"
if [[ "${FAILURES}" -gt 0 ]]; then
    exit 2
fi
exit 0
