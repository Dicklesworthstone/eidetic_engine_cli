#!/usr/bin/env bash
# bd-3ap2m.4 — memory-debt E2E (slice 1): aging-corpus planting for the
# age-independent debt classes, curate doctor detection with Actionable
# suggested commands, the resolve-shrinks-queue loop through the real CLI,
# and learn-gaps query-miss clustering. Real binary, no mocks.
#
# Age-gated classes are exercised through the doctor's frozen --now clock
# (contradicted_unresolved past the 14-day window, never_retrieved past the
# 60-day window). stale_anchor and decay_imminent still need planted anchors
# / decay projections and live in the unit suites — an honest scope note,
# not silent coverage.
#
# Environment:
#   EE_BIN / EE_BINARY  Path to the ee binary (default: `ee` on PATH)
#   EE_E2E_TMPDIR       Temp base (default /private/tmp)

set -uo pipefail

TEST_ID="memory_debt_e2e"
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
ROOT="$(mktemp -d "${ROOT_BASE%/}/ee-memory-debt-e2e.XXXXXX")"
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

# --- corpus: healthy control pair (linked) + a planted orphan -------------
run_ee remember "Healthy control: the parser owns tokenization." \
    --workspace "${WS}" --level semantic --kind fact --tags "control" --json
CTRL_A="$(remember_id)"
run_ee remember "Healthy control: the lexer feeds the parser." \
    --workspace "${WS}" --level semantic --kind fact --tags "control" --json
CTRL_B="$(remember_id)"
run_ee memory link "${CTRL_A}" "${CTRL_B}" --relation related --workspace "${WS}" --json
CTRL_LINK=$LAST_EXIT
run_ee remember "Planted orphan: an isolated note nothing references." \
    --workspace "${WS}" --level semantic --kind fact --tags "planted-orphan" --json
ORPHAN="$(remember_id)"
if [[ -n "${CTRL_A}" && -n "${CTRL_B}" && -n "${ORPHAN}" && "${CTRL_LINK}" -eq 0 ]]; then
    event corpus_seeded pass
else
    event corpus_seeded fail "a=${CTRL_A} b=${CTRL_B} orphan=${ORPHAN} link=${CTRL_LINK}"
fi

# --- curate doctor: planted orphan found, control pair clean --------------
run_ee curate doctor --workspace "${WS}" --json
D1="${LAST_STDOUT}"
ORPHANS_BEFORE="$(jq -r '.data.summary.classCounts.orphan // 0' "${D1}")"
if [[ "${LAST_EXIT}" -eq 0 ]] \
    && jq -e '.data.schema == "ee.curate.doctor.v1"' "${D1}" >/dev/null 2>&1 \
    && jq -e --arg id "${ORPHAN}" \
        '[.data.queue[]? | select(.memoryId == $id and .class == "orphan")] | length == 1' \
        "${D1}" >/dev/null 2>&1; then
    event doctor_finds_planted_orphan pass
else
    event doctor_finds_planted_orphan fail "exit ${LAST_EXIT}; $(head -c 300 "${D1}")"
fi
if jq -e --arg a "${CTRL_A}" \
    '[.data.queue[]? | select(.memoryId == $a and .class == "orphan")] | length == 0' \
    "${D1}" >/dev/null 2>&1; then
    event linked_control_not_orphan pass
else
    event linked_control_not_orphan fail "control pair flagged orphan"
fi
if jq -e --arg id "${ORPHAN}" \
    '[.data.queue[]? | select(.memoryId == $id)][0].suggestedAction
     | (.classifier == "Actionable" and (.command | contains($id)))' \
    "${D1}" >/dev/null 2>&1; then
    event suggested_command_actionable pass
else
    event suggested_command_actionable fail "$(jq -c --arg id "${ORPHAN}" '[.data.queue[]? | select(.memoryId == $id)][0].suggestedAction' "${D1}" 2>/dev/null)"
fi

# --- resolving the debt strictly shrinks the class count ------------------
run_ee memory link "${ORPHAN}" "${CTRL_A}" --relation related --workspace "${WS}" --json
RESOLVE_EXIT=$LAST_EXIT
run_ee curate doctor --workspace "${WS}" --json
D2="${LAST_STDOUT}"
ORPHANS_AFTER="$(jq -r '.data.summary.classCounts.orphan // 0' "${D2}")"
if [[ "${RESOLVE_EXIT}" -eq 0 && "${ORPHANS_AFTER}" -lt "${ORPHANS_BEFORE}" ]]; then
    event resolving_orphan_shrinks_queue pass
else
    event resolving_orphan_shrinks_queue fail "link=${RESOLVE_EXIT} before=${ORPHANS_BEFORE} after=${ORPHANS_AFTER}"
fi

# --- age-gated classes via the frozen doctor clock ------------------------
# A contradicts pair ages past the 14-day window under --now, and the whole
# corpus ages past the 60-day never-retrieved window under a later --now.
run_ee remember "Contradiction side A: always vendor the lockfile." \
    --workspace "${WS}" --level semantic --kind fact --tags "planted-conflict" --json
CONF_A="$(remember_id)"
run_ee remember "Contradiction side B: never vendor the lockfile." \
    --workspace "${WS}" --level semantic --kind fact --tags "planted-conflict" --json
CONF_B="$(remember_id)"
run_ee memory link "${CONF_A}" "${CONF_B}" --relation contradicts --workspace "${WS}" --json
CONF_LINK=$LAST_EXIT

FUTURE_30D="$(date -u -v+30d +%Y-%m-%dT%H:%M:%SZ 2>/dev/null || date -u -d "+30 days" +%Y-%m-%dT%H:%M:%SZ)"
run_ee curate doctor --workspace "${WS}" --now "${FUTURE_30D}" --json
D3="${LAST_STDOUT}"
if [[ "${CONF_LINK}" -eq 0 && "${LAST_EXIT}" -eq 0 ]] \
    && jq -e --arg a "${CONF_A}" --arg b "${CONF_B}" \
        '[.data.queue[]? | select(.class == "contradicted_unresolved" and (.memoryId == $a or .memoryId == $b))] | length >= 1' \
        "${D3}" >/dev/null 2>&1; then
    event aged_contradiction_detected_under_frozen_clock pass
else
    event aged_contradiction_detected_under_frozen_clock fail "link=${CONF_LINK} exit=${LAST_EXIT}; $(jq -c '.data.summary.classCounts' "${D3}" 2>/dev/null)"
fi

FUTURE_90D="$(date -u -v+90d +%Y-%m-%dT%H:%M:%SZ 2>/dev/null || date -u -d "+90 days" +%Y-%m-%dT%H:%M:%SZ)"
run_ee curate doctor --workspace "${WS}" --now "${FUTURE_90D}" --json
D4="${LAST_STDOUT}"
if [[ "${LAST_EXIT}" -eq 0 ]] \
    && jq -e '(.data.summary.classCounts.never_retrieved // 0) >= 1' "${D4}" >/dev/null 2>&1; then
    event aged_corpus_reports_never_retrieved pass
else
    event aged_corpus_reports_never_retrieved fail "exit=${LAST_EXIT}; $(jq -c '.data.summary.classCounts' "${D4}" 2>/dev/null)"
fi

# --- learn gaps: repeated missed searches cluster -------------------------
for _ in 1 2 3; do
    run_ee search "zebra hovercraft docking protocol" --workspace "${WS}" --json
done
run_ee learn gaps --workspace "${WS}" --json
G="${LAST_STDOUT}"
if [[ "${LAST_EXIT}" -eq 0 ]] \
    && jq -e '(.clusterCount // .data.clusterCount // 0) >= 1' "${G}" >/dev/null 2>&1; then
    event repeated_misses_form_gap_cluster pass
else
    event repeated_misses_form_gap_cluster fail "exit ${LAST_EXIT}; $(head -c 300 "${G}")"
fi

# --- trend: two steward snapshots around a fix show the direction ---------
run_ee remember "Second planted orphan for the trend arc." \
    --workspace "${WS}" --level semantic --kind fact --tags "planted-orphan-2" --json
ORPHAN2="$(remember_id)"
run_ee job run memory_debt_snapshot --workspace "${WS}" --json
SNAP1=$LAST_EXIT
run_ee memory link "${ORPHAN2}" "${CTRL_A}" --relation related --workspace "${WS}" --json
FIX2=$LAST_EXIT
run_ee job run memory_debt_snapshot --workspace "${WS}" --json
SNAP2=$LAST_EXIT
run_ee curate doctor --workspace "${WS}" --trend --json
T="${LAST_STDOUT}"
if [[ "${SNAP1}" -eq 0 && "${FIX2}" -eq 0 && "${SNAP2}" -eq 0 && "${LAST_EXIT}" -eq 0 ]] \
    && jq -e '(.data.trend.snapshots | length) >= 2
              and (.data.trend.snapshots[0].totalScore < .data.trend.snapshots[1].totalScore)' \
        "${T}" >/dev/null 2>&1; then
    event trend_shows_debt_decreasing_after_fix pass
else
    event trend_shows_debt_decreasing_after_fix fail "snap1=${SNAP1} fix=${FIX2} snap2=${SNAP2} exit=${LAST_EXIT}; $(jq -c '[.data.trend.snapshots[]? | {generation, totalScore}]' "${T}" 2>/dev/null)"
fi

echo
echo "memory debt e2e: ${STEP} steps, ${FAILURES} failures; root ${ROOT}"
if [[ "${FAILURES}" -gt 0 ]]; then
    exit 2
fi
exit 0
