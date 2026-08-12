#!/usr/bin/env bash
# bd-resume-verb-v0f57 — `ee resume` E2E: the acceptance fixture from the
# bead (tagged sessions + revisit decisions + a next-tagged item + a
# superseded stale note) against the real binary. No mocks.
#
# Environment:
#   EE_BIN / EE_BINARY  Path to the ee binary (default: `ee` on PATH)
#   EE_E2E_TMPDIR       Temp base (default /private/tmp)
#   EE_EMBED_DOWNLOAD   Model download policy (default: off for deterministic E2E)
#   EE_RESUME_E2E_SCOPE functional skips the expensive shared 10k resume/orient
#                       gates; default `all` retains the standalone full script.

set -uo pipefail

TEST_ID="resume_e2e"
FAILURES=0
STEP=0
EE_RESUME_E2E_SCOPE="${EE_RESUME_E2E_SCOPE:-all}"

if ! command -v jq >/dev/null 2>&1; then
    echo "jq missing" >&2
    exit 3
fi
if ! command -v python3 >/dev/null 2>&1; then
    echo "python3 missing" >&2
    exit 3
fi

EE_BIN="${EE_BIN:-${EE_BINARY:-ee}}"
export EE_EMBED_DOWNLOAD="${EE_EMBED_DOWNLOAD:-off}"
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
export XDG_DATA_HOME="${ROOT}/xdg"
mkdir -p "${WS}" "${LOG_DIR}" "${XDG_DATA_HOME}"
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

finish() {
    echo
    echo "resume e2e (${EE_RESUME_E2E_SCOPE}): ${STEP} steps, ${FAILURES} failures; root ${ROOT}"
    if [[ "${FAILURES}" -gt 0 ]]; then
        return 2
    fi
    return 0
}

case "${EE_RESUME_E2E_SCOPE}" in
    all|functional) ;;
    *)
        event invalid_e2e_scope fail \
            "scope=${EE_RESUME_E2E_SCOPE}; expected all or functional"
        finish
        exit 3
        ;;
esac

run_ee() {
    STEP=$((STEP + 1))
    local out="${LOG_DIR}/step${STEP}.stdout" err="${LOG_DIR}/step${STEP}.stderr"
    "${REAL_EE}" "$@" >"${out}" 2>"${err}"
    LAST_EXIT=$?
    LAST_STDOUT="${out}"
    return 0
}

hash_file() {
    local path="$1"
    if [[ ! -e "${path}" ]]; then
        printf 'MISSING'
    elif command -v sha256sum >/dev/null 2>&1; then
        sha256sum "${path}" | awk '{print $1}'
    else
        shasum -a 256 "${path}" | awk '{print $1}'
    fi
}

store_fingerprint() {
    local database="$1" path
    for path in "${database}" "${database}-wal" "${database}-shm"; do
        printf '%s=%s\n' "$(basename "${path}")" "$(hash_file "${path}")"
    done
}

now_ms() {
    python3 -c 'import time; print(time.monotonic_ns() // 1000000)'
}

run_ee init --workspace "${WS}" --json
if [[ "${LAST_EXIT}" -eq 0 ]]; then
    event init_ok pass
else
    event init_ok fail "init exit ${LAST_EXIT}"
fi

# --- empty store: honest no-session-evidence degradation ------------------
run_ee resume --workspace "${WS}" --json
if [[ "${LAST_EXIT}" -eq 0 ]] \
    && jq -e '[.degraded[]? | select(.code == "resume_no_session_evidence")] | length >= 1' \
        "${LAST_STDOUT}" >/dev/null 2>&1; then
    event empty_store_reports_no_evidence pass
else
    event empty_store_reports_no_evidence fail "exit ${LAST_EXIT}; $(head -c 250 "${LAST_STDOUT}")"
fi

# --- literal acceptance corpus: three tagged sessions, every open-loop tag,
# legacy + typed revisit decisions, a planted secret tag, and stale truth ---
run_ee remember "Session A wrapped the parser refactor." \
    --workspace "${WS}" --level episodic --kind note --tags "session-20260801" --json
SESSION_A_ONE_EXIT=$LAST_EXIT
run_ee remember "Session A left the lexer half-done." \
    --workspace "${WS}" --level episodic --kind note --tags "session-20260801" --json
SESSION_A_TWO_EXIT=$LAST_EXIT
run_ee remember "Session B finished the lexer and started codegen." \
    --workspace "${WS}" --level episodic --kind note --tags "session-20260808" --json
SESSION_B_EXIT=$LAST_EXIT
run_ee remember "Session C prepared the resume handoff." \
    --workspace "${WS}" --level episodic --kind note --tags "session-20260809" --json
SESSION_C_EXIT=$LAST_EXIT
run_ee remember "Arc 4 driver wiring remains unfinished." \
    --workspace "${WS}" --level episodic --kind note --tags "session-20260809,arc4" --json
STALE_OLD_EXIT=$LAST_EXIT
STALE_OLD_ID="$(jq -r '.data.memoryId // .data.memory_id // .data.id // empty' "${LAST_STDOUT}")"
run_ee remember "Arc 4 driver wiring landed." \
    --workspace "${WS}" --level semantic --kind note --tags "arc4" --json
STALE_NEW_EXIT=$LAST_EXIT
STALE_NEW_ID="$(jq -r '.data.memoryId // .data.memory_id // .data.id // empty' "${LAST_STDOUT}")"
run_ee remember "Next: begin optimization passes." \
    --workspace "${WS}" --level semantic --kind note --tags "next,optimization" --json
NEXT_EXIT=$LAST_EXIT
NEXT_ID="$(jq -r '.data.memoryId // .data.memory_id // .data.id // empty' "${LAST_STDOUT}")"
SECRET_TAG="ghp_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
run_ee remember "Queue: review the public resume projection." \
    --workspace "${WS}" --level semantic --kind note \
    --tags "queue,resume-public,${SECRET_TAG}" --json
QUEUE_EXIT=$LAST_EXIT
QUEUE_ID="$(jq -r '.data.memoryId // .data.memory_id // .data.id // empty' "${LAST_STDOUT}")"
run_ee remember "Blocking: finish the deterministic gate." \
    --workspace "${WS}" --level semantic --kind note --tags "blocking,resume-gate" --json
BLOCKING_EXIT=$LAST_EXIT
BLOCKING_ID="$(jq -r '.data.memoryId // .data.memory_id // .data.id // empty' "${LAST_STDOUT}")"
run_ee remember "Pending: obtain the remote acceptance proof." \
    --workspace "${WS}" --level semantic --kind note --tags "pending,resume-proof" --json
PENDING_EXIT=$LAST_EXIT
PENDING_ID="$(jq -r '.data.memoryId // .data.memory_id // .data.id // empty' "${LAST_STDOUT}")"
run_ee remember "Todo: verify the bounded resume bundle." \
    --workspace "${WS}" --level semantic --kind note --tags "todo,resume-bundle" --json
TODO_EXIT=$LAST_EXIT
TODO_ID="$(jq -r '.data.memoryId // .data.memory_id // .data.id // empty' "${LAST_STDOUT}")"
run_ee remember "Revisit: re-check the store-discovery dependency." \
    --workspace "${WS}" --level semantic --kind note --tags "revisit,resume-discovery" --json
REVISIT_TAG_EXIT=$LAST_EXIT
REVISIT_TAG_ID="$(jq -r '.data.memoryId // .data.memory_id // .data.id // empty' "${LAST_STDOUT}")"
if [[ "${SESSION_A_ONE_EXIT}" -eq 0 && "${SESSION_A_TWO_EXIT}" -eq 0 \
    && "${SESSION_B_EXIT}" -eq 0 && "${SESSION_C_EXIT}" -eq 0 \
    && "${STALE_OLD_EXIT}" -eq 0 && "${STALE_NEW_EXIT}" -eq 0 \
    && "${NEXT_EXIT}" -eq 0 && "${QUEUE_EXIT}" -eq 0 \
    && "${BLOCKING_EXIT}" -eq 0 && "${PENDING_EXIT}" -eq 0 \
    && "${TODO_EXIT}" -eq 0 && "${REVISIT_TAG_EXIT}" -eq 0 \
    && -n "${STALE_OLD_ID}" \
    && -n "${STALE_NEW_ID}" && -n "${NEXT_ID}" && -n "${QUEUE_ID}" \
    && -n "${BLOCKING_ID}" && -n "${PENDING_ID}" && -n "${TODO_ID}" \
    && -n "${REVISIT_TAG_ID}" ]]; then
    event corpus_seeded pass
else
    event corpus_seeded fail \
        "remember exits ${SESSION_A_ONE_EXIT}/${SESSION_A_TWO_EXIT}/${SESSION_B_EXIT}/${SESSION_C_EXIT}/${STALE_OLD_EXIT}/${STALE_NEW_EXIT}/${NEXT_EXIT}/${QUEUE_EXIT}/${BLOCKING_EXIT}/${PENDING_EXIT}/${TODO_EXIT}/${REVISIT_TAG_EXIT}"
fi

run_ee decide record "resume e2e revisit decision" --chosen "ship now" \
    --alternative "wait for perf pass" --rationale "deadline wins" \
    --revisit-by "+30d" --workspace "${WS}" --json
DECIDE_ONE=$LAST_EXIT
DECIDE_ONE_ID="$(jq -r '.data.decision.memoryId // empty' "${LAST_STDOUT}")"
run_ee decide record "resume e2e second decision" --chosen "keep sqlite" \
    --alternative "switch stores" --rationale "franken-stack rule" \
    --revisit-by "+90d" --workspace "${WS}" --json
DECIDE_TWO=$LAST_EXIT
DECIDE_TWO_ID="$(jq -r '.data.decision.memoryId // empty' "${LAST_STDOUT}")"
run_ee remember "typed sidecar resume decision" --workspace "${WS}" \
    --level semantic --kind decision --field "chosen=consume canonical fields" \
    --field "revisit_by=2026-12-31T00:00:00Z" --json
DECIDE_TYPED=$LAST_EXIT
DECIDE_TYPED_ID="$(jq -r '.data.memoryId // .data.memory_id // .data.id // empty' "${LAST_STDOUT}")"
if [[ "${DECIDE_ONE}" -eq 0 && "${DECIDE_TWO}" -eq 0 \
    && "${DECIDE_TYPED}" -eq 0 && -n "${DECIDE_ONE_ID}" \
    && -n "${DECIDE_TWO_ID}" && -n "${DECIDE_TYPED_ID}" ]]; then
    event decisions_recorded pass
else
    event decisions_recorded fail \
        "decision exits ${DECIDE_ONE}/${DECIDE_TWO}/${DECIDE_TYPED}; ids=${DECIDE_ONE_ID}/${DECIDE_TWO_ID}/${DECIDE_TYPED_ID}"
fi

# --- the bundle -----------------------------------------------------------
STORE_DATABASE="${WS}/.ee/ee.db"
JSON_HASH_BEFORE="$(store_fingerprint "${STORE_DATABASE}")"
run_ee resume --workspace "${WS}" --sessions 3 --json
MAIN_RESUME_EXIT=$LAST_EXIT
R="${LAST_STDOUT}"
cp "${R}" "${LOG_DIR}/resume-report.json"
JSON_HASH_AFTER="$(store_fingerprint "${STORE_DATABASE}")"
if [[ "${JSON_HASH_BEFORE}" == "${JSON_HASH_AFTER}" ]]; then
    event json_resume_preserves_db_wal_shm pass
else
    event json_resume_preserves_db_wal_shm fail "before=${JSON_HASH_BEFORE//$'\n'/,}; after=${JSON_HASH_AFTER//$'\n'/,}"
fi
if [[ "${MAIN_RESUME_EXIT}" -eq 0 ]] \
    && jq -e '.data.report.schema == "ee.resume.v1"' "${R}" >/dev/null 2>&1; then
    event resume_returns_schema pass
else
    event resume_returns_schema fail "exit ${MAIN_RESUME_EXIT}; $(head -c 250 "${R}")"
fi

if jq -e '
    all(.data.report.sessions[]?.items[]?;
        .selectionReason == "recent_session_member"
        and (.provenance
             | (.uri | type) == "string" and (.uri | length) > 0
               and has("trustClass") and has("verificationStatus"))
        and (.redaction | has("applied") and has("reasons")))
    and all(.data.report.openLoops.taggedItems[]?;
        .selectionReason == "open_loop_tag"
        and (.provenance
             | (.uri | type) == "string" and (.uri | length) > 0
               and has("trustClass") and has("verificationStatus"))
        and (.redaction | has("applied") and has("reasons")))
    and all(.data.report.openLoops.revisitDecisions[]?;
        (.provenance
         | (.uri | type) == "string" and (.uri | length) > 0
           and has("trustClass") and has("verificationStatus"))
        and (.redaction | has("applied") and has("reasons")))
' "${R}" >/dev/null 2>&1; then
    event resume_items_carry_public_posture pass
else
    event resume_items_carry_public_posture fail "$(head -c 350 "${R}")"
fi

if jq -e '.data.report.episodicTotal == 5
        and [.data.report.sessions[]?.label]
            == ["session-20260809", "session-20260808", "session-20260801"]
        and ([.data.report.sessions[]?.items[]?.content
              | select(contains("Session A"))] | length) == 2' \
    "${R}" >/dev/null 2>&1; then
    event all_three_tagged_sessions_publicly_surfaced pass
else
    event all_three_tagged_sessions_publicly_surfaced fail \
        "$(jq -c '{total:.data.report.episodicTotal, sessions:[.data.report.sessions[]? | {label,items:[.items[]?.content]}]}' "${R}" 2>/dev/null)"
fi

run_ee resume --workspace "${WS}" --sessions 2 --json
TWO_SESSION_EXIT=$LAST_EXIT
TWO_SESSION_JSON="${LAST_STDOUT}"
if [[ "${TWO_SESSION_EXIT}" -eq 0 ]] \
    && jq -e '[.data.report.sessions[]?.label]
            == ["session-20260809", "session-20260808"]
        and ([.data.report.sessions[]?.items[]?.content
              | select(contains("Session A"))] | length) == 0' \
        "${TWO_SESSION_JSON}" >/dev/null 2>&1; then
    event requested_two_newest_sessions_only pass
else
    event requested_two_newest_sessions_only fail \
        "exit=${TWO_SESSION_EXIT}; $(head -c 400 "${TWO_SESSION_JSON}")"
fi

if jq -e --arg decision_one "${DECIDE_ONE_ID}" --arg decision_two "${DECIDE_TWO_ID}" \
    --arg decision_typed "${DECIDE_TYPED_ID}" \
    '(.data.report.openLoops.revisitDecisions
      | all(.revisitBy != null))
     and ([.data.report.openLoops.revisitDecisions[]? | .memoryId] | sort)
         == ([$decision_one, $decision_two, $decision_typed] | sort)
     and ([.data.report.openLoops.revisitDecisions[]?
           | select(.memoryId == $decision_typed
                    and .topic == "typed sidecar resume decision"
                    and .chosen == "consume canonical fields"
                    and .revisitBy == "2026-12-31T00:00:00Z")]
          | length) == 1' \
    "${R}" >/dev/null 2>&1; then
    event canonical_typed_revisit_decision_surfaced pass
else
    event canonical_typed_revisit_decision_surfaced fail \
        "$(jq -c '.data.report.openLoops.revisitDecisions' "${R}" 2>/dev/null | head -c 350)"
fi

if jq -e '.data.report.openLoops.revisitDecisionsTotal == 3
        and .data.report.openLoops.revisitDecisionsTruncated == false
        and (.data.report.openLoops.revisitDecisions | length) == 3
        and .data.report.openLoops.taggedItemsTotal == 6
        and .data.report.openLoops.taggedItemsTruncated == false
        and (.data.report.openLoops.taggedItems | length) == 6' \
    "${R}" >/dev/null 2>&1; then
    event open_loop_totals_are_exact pass
else
    event open_loop_totals_are_exact fail "$(jq -c '.data.report.openLoops' "${R}" 2>/dev/null | head -c 350)"
fi

if jq -e --arg next_id "${NEXT_ID}" --arg queue_id "${QUEUE_ID}" \
    --arg blocking_id "${BLOCKING_ID}" --arg pending_id "${PENDING_ID}" \
    --arg todo_id "${TODO_ID}" --arg revisit_id "${REVISIT_TAG_ID}" \
    '([.data.report.openLoops.taggedItems[]? | .memoryId] | sort)
        == ([$next_id, $queue_id, $blocking_id, $pending_id, $todo_id, $revisit_id] | sort)
     and ([.data.report.openLoops.taggedItems[]?
           | select(.memoryId == $next_id and (.tags | index("next")) != null)] | length) == 1
     and ([.data.report.openLoops.taggedItems[]?
           | select(.memoryId == $queue_id and (.tags | index("queue")) != null)] | length) == 1
     and ([.data.report.openLoops.taggedItems[]?
           | select(.memoryId == $blocking_id and (.tags | index("blocking")) != null)] | length) == 1
     and ([.data.report.openLoops.taggedItems[]?
           | select(.memoryId == $pending_id and (.tags | index("pending")) != null)] | length) == 1
     and ([.data.report.openLoops.taggedItems[]?
           | select(.memoryId == $todo_id and (.tags | index("todo")) != null)] | length) == 1
     and ([.data.report.openLoops.taggedItems[]?
           | select(.memoryId == $revisit_id and (.tags | index("revisit")) != null)] | length) == 1
     and all(.data.report.openLoops.taggedItems[]?; .stale == null)' \
    "${R}" >/dev/null 2>&1; then
    event all_six_open_loop_tags_surfaced pass
else
    event all_six_open_loop_tags_surfaced fail \
        "$(jq -c '.data.report.openLoops.taggedItems' "${R}" 2>/dev/null | head -c 400)"
fi

if jq -e --arg decision_typed "${DECIDE_TYPED_ID}" \
    --arg next_id "${NEXT_ID}" --arg queue_id "${QUEUE_ID}" \
    --arg blocking_id "${BLOCKING_ID}" --arg pending_id "${PENDING_ID}" \
    --arg todo_id "${TODO_ID}" --arg revisit_id "${REVISIT_TAG_ID}" '
        (.data.report.sessions | length) == 3
        and [.data.report.sessions[].label]
            == ["session-20260809", "session-20260808", "session-20260801"]
        and ([.data.report.openLoops.revisitDecisions[]?.memoryId]
             | index($decision_typed)) != null
        and ([.data.report.openLoops.taggedItems[]?.memoryId] | sort)
            == ([$next_id, $queue_id, $blocking_id, $pending_id, $todo_id, $revisit_id] | sort)
    ' "${R}" >/dev/null 2>&1; then
    event requested_sessions_plus_every_open_loop_in_one_resume pass
else
    event requested_sessions_plus_every_open_loop_in_one_resume fail \
        "$(jq -c '.data.report | {sessions,openLoops}' "${R}" 2>/dev/null | head -c 500)"
fi

if ! grep -Fq "${SECRET_TAG}" "${R}" \
    && jq -e --arg queue_id "${QUEUE_ID}" '
        [.data.report.openLoops.taggedItems[]?
         | select(.memoryId == $queue_id
                  and .redaction.applied == true
                  and ([.redaction.reasons[]? | startswith("tag:")] | any))]
        | length == 1
    ' "${R}" >/dev/null 2>&1; then
    event resume_json_redacts_planted_secret pass
else
    event resume_json_redacts_planted_secret fail "JSON secret leak or missing redaction posture"
fi

EXPECTED_NEXT_COMMANDS='["ee decide list --json  # open decisions incl. revisit conditions","ee orient \"<current task>\" --json  # task-conditioned pack once you know the task","ee conflict list --json  # anything contradictory left behind"]'
if [[ "$(jq -c '.data.report.nextCommands' "${R}" 2>/dev/null)" == "${EXPECTED_NEXT_COMMANDS}" ]]; then
    event canonical_next_commands_preserved pass
else
    event canonical_next_commands_preserved fail \
        "$(jq -c '.data.report.nextCommands' "${R}" 2>/dev/null)"
fi

NEXT_COMMANDS_EXIT=0
while IFS= read -r command; do
    STEP=$((STEP + 1))
    COMMAND_STDOUT="${LOG_DIR}/step${STEP}.stdout"
    COMMAND_STDERR="${LOG_DIR}/step${STEP}.stderr"
    (cd "${WS}" && PATH="$(dirname "${REAL_EE}"):${PATH}" bash -c "${command}") \
        >"${COMMAND_STDOUT}" 2>"${COMMAND_STDERR}"
    COMMAND_EXIT=$?
    if [[ "${COMMAND_EXIT}" -ne 0 ]]; then
        NEXT_COMMANDS_EXIT="${COMMAND_EXIT}"
        break
    fi
    if ! jq -e '.schema == "ee.response.v2" and .success == true' \
        "${COMMAND_STDOUT}" >/dev/null 2>&1; then
        NEXT_COMMANDS_EXIT=2
        break
    fi
done < <(jq -r '.data.report.nextCommands[]' "${R}")
if [[ "${NEXT_COMMANDS_EXIT}" -eq 0 ]]; then
    event canonical_next_commands_execute pass
else
    event canonical_next_commands_execute fail "exit=${NEXT_COMMANDS_EXIT}"
fi

# The older session note and newer semantic note share only the non-control
# subject tag arc4. The stale item appears in the session lane only.
if jq -e --arg stale_old "${STALE_OLD_ID}" --arg stale_new "${STALE_NEW_ID}" \
    '.data.report.staleCount == 1
     and ([.. | objects | select(.memoryId? == $stale_old and .stale? != null)]
          | length == 1
            and .[0].selectionReason == "recent_session_member"
            and .[0].stale.supersededBy == $stale_new
            and .[0].stale.sharedTags == ["arc4"])
     and ([.data.report.openLoops.taggedItems[]?
           | select(.memoryId == $stale_old or .memoryId == $stale_new)] | length == 0)' \
    "${R}" >/dev/null 2>&1; then
    event superseded_note_carries_stale_marker pass
else
    event superseded_note_carries_stale_marker fail "staleCount=$(jq -r '.data.report.staleCount' "${R}" 2>/dev/null); $(head -c 250 "${R}")"
fi

# --- human contract: every declared section remains visible ---------------
HUMAN_HASH_BEFORE="$(store_fingerprint "${STORE_DATABASE}")"
run_ee resume --workspace "${WS}" --sessions 3
H="${LAST_STDOUT}"
HUMAN_HASH_AFTER="$(store_fingerprint "${STORE_DATABASE}")"
if [[ "${HUMAN_HASH_BEFORE}" == "${HUMAN_HASH_AFTER}" ]]; then
    event human_resume_preserves_db_wal_shm pass
else
    event human_resume_preserves_db_wal_shm fail "before=${HUMAN_HASH_BEFORE//$'\n'/,}; after=${HUMAN_HASH_AFTER//$'\n'/,}"
fi
if [[ "${LAST_EXIT}" -eq 0 ]] \
    && grep -Fq "Recent end-state:" "${H}" \
    && grep -Fq "Open loops:" "${H}" \
    && grep -Fq "Next commands:" "${H}"; then
    event human_declared_sections_visible pass
else
    event human_declared_sections_visible fail "exit ${LAST_EXIT}; $(head -c 250 "${H}")"
fi

STALE_NEW_CREATED_AT="$(jq -r --arg stale_old "${STALE_OLD_ID}" \
    '[.. | objects | select(.memoryId? == $stale_old and .stale? != null)][0].stale.supersededByCreatedAt // empty' \
    "${R}" 2>/dev/null)"
if grep -Fq "resume: 5 episodic memories, 3 sessions shown, 3/3 open decisions, 6/6 queued items, 1 stale flags" "${H}" \
    && grep -Fq "[session-20260809]" "${H}" \
    && grep -Fq "[session-20260808]" "${H}" \
    && grep -Fq "[session-20260801]" "${H}" \
    && grep -Fq "Session A wrapped the parser refactor." "${H}" \
    && grep -Fq "Session A left the lexer half-done." "${H}" \
    && grep -Fq "decision ${DECIDE_ONE_ID}: resume e2e revisit decision (revisit " "${H}" \
    && grep -Fq "decision ${DECIDE_TWO_ID}: resume e2e second decision (revisit " "${H}" \
    && grep -Fq "decision ${DECIDE_TYPED_ID}: typed sidecar resume decision (revisit 2026-12-31T00:00:00Z)" "${H}" \
    && grep -Fq "queued ${NEXT_ID}: Next: begin optimization passes." "${H}" \
    && grep -Fq "queued ${QUEUE_ID} [REDACTED]: Queue: review the public resume projection." "${H}" \
    && grep -Fq "queued ${TODO_ID}: Todo: verify the bounded resume bundle." "${H}" \
    && grep -Fq "${STALE_OLD_ID} [STALE]: Arc 4 driver wiring remains unfinished. [superseded by ${STALE_NEW_ID} at ${STALE_NEW_CREATED_AT}; shared tags: arc4]" "${H}" \
    && ! grep -Fq "${SECRET_TAG}" "${H}"; then
    event human_open_loop_and_staleness_visible pass
else
    event human_open_loop_and_staleness_visible fail "$(head -c 500 "${H}")"
fi

if grep -Fq "queued ${QUEUE_ID} [REDACTED]" "${H}" \
    && ! grep -Fq "${SECRET_TAG}" "${H}"; then
    event resume_human_redacts_planted_secret pass
else
    event resume_human_redacts_planted_secret fail "human secret leak or missing redaction marker"
fi

# Zero is invalid, must return the standard structured error, and must not
# mutate the addressed store while refusing the request.
ZERO_HASH_BEFORE="$(store_fingerprint "${STORE_DATABASE}")"
run_ee resume --workspace "${WS}" --sessions 0 --json
ZERO_JSON="${LAST_STDOUT}"
ZERO_HASH_AFTER="$(store_fingerprint "${STORE_DATABASE}")"
if [[ "${LAST_EXIT}" -ne 0 && "${ZERO_HASH_BEFORE}" == "${ZERO_HASH_AFTER}" ]] \
    && jq -e '.schema == "ee.error.v2"
        and .error.code == "usage"
        and (.error.message | contains("--sessions") and contains("at least 1"))
        and (.error.repair | contains("--sessions 1"))' \
        "${ZERO_JSON}" >/dev/null 2>&1; then
    event zero_sessions_structured_nonzero_no_mutation pass
else
    event zero_sessions_structured_nonzero_no_mutation fail \
        "exit=${LAST_EXIT}; before=${ZERO_HASH_BEFORE//$'\n'/,}; after=${ZERO_HASH_AFTER//$'\n'/,}; $(head -c 300 "${ZERO_JSON}")"
fi

CAP_HASH_BEFORE="$(store_fingerprint "${STORE_DATABASE}")"
run_ee resume --workspace "${WS}" --sessions 65 --json
CAP_JSON="${LAST_STDOUT}"
CAP_HASH_AFTER="$(store_fingerprint "${STORE_DATABASE}")"
if [[ "${LAST_EXIT}" -ne 0 && "${CAP_HASH_BEFORE}" == "${CAP_HASH_AFTER}" ]] \
    && jq -e '.schema == "ee.error.v2"
        and .error.code == "usage"
        and (.error.message | contains("cannot exceed 64"))
        and (.error.repair | contains("--sessions 64"))' \
        "${CAP_JSON}" >/dev/null 2>&1; then
    event sessions_above_public_cap_structured_nonzero_no_mutation pass
else
    event sessions_above_public_cap_structured_nonzero_no_mutation fail \
        "exit=${LAST_EXIT}; before=${CAP_HASH_BEFORE//$'\n'/,}; after=${CAP_HASH_AFTER//$'\n'/,}; $(head -c 300 "${CAP_JSON}")"
fi

# Untagged memories exercise the literal four-hour boundary. The records are
# created through the real binary; deterministic timestamps are then planted
# in that real store before the compiled resume binary reads it.
BOUNDARY_WS="${ROOT}/untagged-four-hour-boundary"
mkdir -p "${BOUNDARY_WS}"
run_ee init --workspace "${BOUNDARY_WS}" --json
BOUNDARY_INIT_EXIT=$LAST_EXIT
BOUNDARY_CONTENTS=(
    "Boundary newest at noon."
    "Boundary exactly four hours older."
    "Boundary four hours and one minute older."
    "Boundary oldest within second session."
)
BOUNDARY_SEED_EXIT=0
for content in "${BOUNDARY_CONTENTS[@]}"; do
    run_ee remember "${content}" --workspace "${BOUNDARY_WS}" \
        --level episodic --kind note --json
    if [[ "${LAST_EXIT}" -ne 0 ]]; then
        BOUNDARY_SEED_EXIT=$LAST_EXIT
        break
    fi
done
BOUNDARY_DATABASE="${BOUNDARY_WS}/.ee/ee.db"
python3 - "${BOUNDARY_DATABASE}" <<'PY'
import sqlite3
import sys

database = sys.argv[1]
timestamps = {
    "Boundary newest at noon.": "2026-08-10T12:00:00Z",
    "Boundary exactly four hours older.": "2026-08-10T08:00:00Z",
    "Boundary four hours and one minute older.": "2026-08-10T03:59:00Z",
    "Boundary oldest within second session.": "2026-08-10T00:00:00Z",
}
with sqlite3.connect(database) as connection:
    for content, timestamp in timestamps.items():
        connection.execute(
            "UPDATE memories SET created_at = ?, updated_at = ? WHERE content = ?",
            (timestamp, timestamp, content),
        )
PY
BOUNDARY_TIME_EXIT=$?
BOUNDARY_HASH_BEFORE="$(store_fingerprint "${BOUNDARY_DATABASE}")"
run_ee resume --workspace "${BOUNDARY_WS}" --sessions 1 --json
BOUNDARY_ONE_EXIT=$LAST_EXIT
BOUNDARY_ONE_JSON="${LAST_STDOUT}"
BOUNDARY_HASH_AFTER="$(store_fingerprint "${BOUNDARY_DATABASE}")"
if [[ "${BOUNDARY_INIT_EXIT}" -eq 0 && "${BOUNDARY_SEED_EXIT}" -eq 0 \
    && "${BOUNDARY_TIME_EXIT}" -eq 0 && "${BOUNDARY_ONE_EXIT}" -eq 0 \
    && "${BOUNDARY_HASH_BEFORE}" == "${BOUNDARY_HASH_AFTER}" ]] \
    && jq -e '.data.report.episodicTotal == 4
        and (.data.report.sessions | length) == 1
        and .data.report.sessions[0].memberCount == 2
        and .data.report.sessions[0].newestAt == "2026-08-10T12:00:00Z"
        and .data.report.sessions[0].oldestAt == "2026-08-10T08:00:00Z"
        and ([.data.report.sessions[0].items[].content]
             == ["Boundary newest at noon.", "Boundary exactly four hours older."])
        and ([.. | strings | select(contains("four hours and one minute"))] | length) == 0
        and ([.. | strings | select(contains("oldest within second session"))] | length) == 0' \
        "${BOUNDARY_ONE_JSON}" >/dev/null 2>&1; then
    event untagged_four_hour_boundary_and_sessions_one_truncation pass
else
    event untagged_four_hour_boundary_and_sessions_one_truncation fail \
        "init/seed/time/resume=${BOUNDARY_INIT_EXIT}/${BOUNDARY_SEED_EXIT}/${BOUNDARY_TIME_EXIT}/${BOUNDARY_ONE_EXIT}; $(head -c 500 "${BOUNDARY_ONE_JSON}")"
fi

run_ee resume --workspace "${BOUNDARY_WS}" --sessions 2 --json
if [[ "${LAST_EXIT}" -eq 0 ]] \
    && jq -e '(.data.report.sessions | length) == 2
        and [.data.report.sessions[].memberCount] == [2, 2]
        and [.data.report.sessions[].newestAt]
            == ["2026-08-10T12:00:00Z", "2026-08-10T03:59:00Z"]' \
        "${LAST_STDOUT}" >/dev/null 2>&1; then
    event untagged_sessions_two_returns_both_real_groups pass
else
    event untagged_sessions_two_returns_both_real_groups fail \
        "exit=${LAST_EXIT}; $(head -c 500 "${LAST_STDOUT}")"
fi

# --- genuinely cold workspace: discovery + executable database retarget ---
COLD_WS="${ROOT}/cold start"
NEARBY_WS="${COLD_WS}/nearby campaign root"
export XDG_DATA_HOME="${ROOT}/xdg-cold"
mkdir -p "${XDG_DATA_HOME}"
mkdir -p "${COLD_WS}" "${NEARBY_WS}"
if [[ ! -e "${COLD_WS}/.ee" ]]; then
    event cold_root_starts_uninitialized pass
else
    event cold_root_starts_uninitialized fail "unexpected ${COLD_WS}/.ee before resume"
fi

run_ee init --workspace "${NEARBY_WS}" --json
NEARBY_INIT_EXIT=$LAST_EXIT
run_ee remember "Nearby campaign evidence survives the cold-root retarget." \
    --workspace "${NEARBY_WS}" --level episodic --kind note \
    --tags "session-nearby-campaign" --json
NEARBY_SEED_EXIT=$LAST_EXIT
CAMPAIGN_STORE="${NEARBY_WS}/.ee-campaign"
if [[ "${NEARBY_INIT_EXIT}" -eq 0 && "${NEARBY_SEED_EXIT}" -eq 0 \
    && ! -e "${CAMPAIGN_STORE}" ]]; then
    mv "${NEARBY_WS}/.ee" "${CAMPAIGN_STORE}"
    CAMPAIGN_MOVE_EXIT=$?
else
    CAMPAIGN_MOVE_EXIT=1
fi
CAMPAIGN_DATABASE="${CAMPAIGN_STORE}/ee.db"
if [[ "${CAMPAIGN_MOVE_EXIT}" -eq 0 && -f "${CAMPAIGN_DATABASE}" \
    && ! -e "${COLD_WS}/.ee" ]]; then
    event nearby_store_seeded pass
else
    event nearby_store_seeded fail \
        "init/seed/move exits ${NEARBY_INIT_EXIT}/${NEARBY_SEED_EXIT}/${CAMPAIGN_MOVE_EXIT}; cold .ee exists=$([[ -e "${COLD_WS}/.ee" ]] && printf yes || printf no)"
fi

run_ee resume --workspace "${NEARBY_WS}" --json
if [[ "${LAST_EXIT}" -eq 0 ]] \
    && jq -e '.data.report.episodicTotal >= 1
        and ([.data.report.sessions[]?.items[]?.content
              | select(contains("Nearby campaign evidence survives"))] | length) >= 1' \
        "${LAST_STDOUT}" >/dev/null 2>&1 \
    && [[ ! -e "${NEARBY_WS}/.ee" ]]; then
    event implicit_resume_resolves_campaign_store pass
else
    event implicit_resume_resolves_campaign_store fail \
        "exit=${LAST_EXIT}; $(head -c 400 "${LAST_STDOUT}")"
fi

CAMPAIGN_HUMAN_HASH_BEFORE="$(store_fingerprint "${CAMPAIGN_DATABASE}")"
CAMPAIGN_HUMAN_STARTED_MS="$(now_ms)"
run_ee resume --workspace "${NEARBY_WS}"
CAMPAIGN_HUMAN_ELAPSED_MS=$(( $(now_ms) - CAMPAIGN_HUMAN_STARTED_MS ))
CAMPAIGN_HUMAN="${LAST_STDOUT}"
CAMPAIGN_HUMAN_HASH_AFTER="$(store_fingerprint "${CAMPAIGN_DATABASE}")"
if [[ "${LAST_EXIT}" -eq 0 && "${CAMPAIGN_HUMAN_ELAPSED_MS}" -lt 2000 ]] \
    && grep -Fq "Recent end-state:" "${CAMPAIGN_HUMAN}" \
    && grep -Fq "Nearby campaign evidence survives the cold-root retarget." \
        "${CAMPAIGN_HUMAN}" \
    && [[ ! -e "${NEARBY_WS}/.ee" ]]; then
    event implicit_human_resume_resolves_campaign_store_under_2s pass
else
    event implicit_human_resume_resolves_campaign_store_under_2s fail \
        "exit=${LAST_EXIT}; elapsedMs=${CAMPAIGN_HUMAN_ELAPSED_MS}; $(head -c 400 "${CAMPAIGN_HUMAN}")"
fi
if [[ "${CAMPAIGN_HUMAN_HASH_BEFORE}" == "${CAMPAIGN_HUMAN_HASH_AFTER}" ]]; then
    event implicit_human_campaign_resume_preserves_db_wal_shm pass
else
    event implicit_human_campaign_resume_preserves_db_wal_shm fail \
        "before=${CAMPAIGN_HUMAN_HASH_BEFORE//$'\n'/,}; after=${CAMPAIGN_HUMAN_HASH_AFTER//$'\n'/,}"
fi

COLD_DATABASE="${COLD_WS}/.ee/ee.db"
COLD_JSON_HASH_BEFORE="$(store_fingerprint "${COLD_DATABASE}")"
run_ee resume --workspace "${COLD_WS}" --json
COLD_JSON="${LAST_STDOUT}"
cp "${COLD_JSON}" "${LOG_DIR}/resume-cold-report.json"
COLD_JSON_HASH_AFTER="$(store_fingerprint "${COLD_DATABASE}")"
BEST_ROOT="$(jq -r '.data.report.nearbyStores.stores[0].workspaceRoot // empty' \
    "${COLD_JSON}" 2>/dev/null)"
BEST_STORE="$(jq -r '.data.report.nearbyStores.stores[0].storeDir // empty' \
    "${COLD_JSON}" 2>/dev/null)"
BEST_DOCS="$(jq -r '.data.report.nearbyStores.stores[0].documents // 0' \
    "${COLD_JSON}" 2>/dev/null)"
BEST_LAST_WRITE="$(jq -r '.data.report.nearbyStores.stores[0].lastWrite // empty' \
    "${COLD_JSON}" 2>/dev/null)"
BEST_SCAN_OUTCOME="$(jq -r '.data.report.nearbyStores.outcome // empty' \
    "${COLD_JSON}" 2>/dev/null)"
FIRST_COMMAND="$(jq -r '.data.report.nextCommands[0] // empty' "${COLD_JSON}" 2>/dev/null)"
EXPECTED_COMMAND="ee resume --workspace '${NEARBY_WS}' --database '${CAMPAIGN_DATABASE}' --json"
if [[ "${LAST_EXIT}" -eq 0 && "${BEST_ROOT}" == "${NEARBY_WS}" \
    && "${BEST_STORE}" == "${CAMPAIGN_STORE}" && "${BEST_DOCS}" -gt 0 \
    && -n "${BEST_LAST_WRITE}" && "${BEST_SCAN_OUTCOME}" == "complete" \
    && "${FIRST_COMMAND}" == "${EXPECTED_COMMAND}" ]]; then
    event nearby_store_prepends_quoted_database_resume pass
else
    event nearby_store_prepends_quoted_database_resume fail \
        "exit=${LAST_EXIT}; root=${BEST_ROOT}; store=${BEST_STORE}; docs=${BEST_DOCS}; lastWrite=${BEST_LAST_WRITE}; outcome=${BEST_SCAN_OUTCOME}; command=${FIRST_COMMAND}"
fi
if [[ "${BEST_SCAN_OUTCOME}" == "complete" ]] \
    && ! jq -e '.data.report.nearbyStores | has("truncated")' \
        "${COLD_JSON}" >/dev/null 2>&1; then
    event nearby_store_exact_complete_outcome_surfaced pass
else
    event nearby_store_exact_complete_outcome_surfaced fail \
        "outcome=${BEST_SCAN_OUTCOME}; $(jq -c '.data.report.nearbyStores' "${COLD_JSON}" 2>/dev/null)"
fi
if [[ "${LAST_EXIT}" -eq 0 \
    && "$(jq -r '.data.report.schema // empty' "${COLD_JSON}" 2>/dev/null)" == "ee.resume.v1" \
    && "$(jq -r '.data.report.episodicTotal // -1' "${COLD_JSON}" 2>/dev/null)" -eq 0 \
    && "${COLD_JSON_HASH_BEFORE}" == "${COLD_JSON_HASH_AFTER}" \
    && ! -e "${COLD_WS}/.ee" ]]; then
    event missing_db_returns_empty_resume_without_initializing pass
else
    event missing_db_returns_empty_resume_without_initializing fail \
        "exit=${LAST_EXIT}; before=${COLD_JSON_HASH_BEFORE//$'\n'/,}; after=${COLD_JSON_HASH_AFTER//$'\n'/,}; cold .ee exists=$([[ -e "${COLD_WS}/.ee" ]] && printf yes || printf no)"
fi

COLD_HUMAN_HASH_BEFORE="$(store_fingerprint "${COLD_DATABASE}")"
run_ee resume --workspace "${COLD_WS}"
COLD_HUMAN="${LAST_STDOUT}"
COLD_HUMAN_HASH_AFTER="$(store_fingerprint "${COLD_DATABASE}")"
if [[ "${LAST_EXIT}" -eq 0 \
    && "${COLD_HUMAN_HASH_BEFORE}" == "${COLD_HUMAN_HASH_AFTER}" ]] \
    && grep -Fq "Nearby populated stores:" "${COLD_HUMAN}" \
    && grep -Fq "Nearby-store discovery outcome: complete." "${COLD_HUMAN}" \
    && ! grep -Fq "outcome: unavailable" "${COLD_HUMAN}"; then
    event nearby_store_human_complete_outcome_is_explicit pass
else
    event nearby_store_human_complete_outcome_is_explicit fail \
        "exit=${LAST_EXIT}; $(head -c 500 "${COLD_HUMAN}")"
fi

STEP=$((STEP + 1))
EMITTED_STDOUT="${LOG_DIR}/step${STEP}.stdout"
EMITTED_STDERR="${LOG_DIR}/step${STEP}.stderr"
PATH="$(dirname "${REAL_EE}"):${PATH}" bash -c "${FIRST_COMMAND}" \
    >"${EMITTED_STDOUT}" 2>"${EMITTED_STDERR}"
EMITTED_EXIT=$?
if [[ "${EMITTED_EXIT}" -eq 0 ]] \
    && jq -e '.data.report.episodicTotal >= 1
        and ([.data.report.sessions[]?.items[]?.content
              | select(contains("Nearby campaign evidence survives"))] | length) >= 1' \
        "${EMITTED_STDOUT}" >/dev/null 2>&1; then
    event emitted_database_resume_executes_campaign_evidence pass
else
    event emitted_database_resume_executes_campaign_evidence fail \
        "exit=${EMITTED_EXIT}; command=${FIRST_COMMAND}; $(head -c 400 "${EMITTED_STDOUT}")"
fi
if [[ ! -e "${COLD_WS}/.ee" ]]; then
    event emitted_resume_leaves_cold_root_uninitialized pass
else
    event emitted_resume_leaves_cold_root_uninitialized fail \
        "unexpected ${COLD_WS}/.ee after executing ${FIRST_COMMAND}"
fi

# --- isolated staleness proofs: preserve the literal main corpus counts -----
# Sharing only the open-loop control tag `next` must not establish subject
# identity, even for a same-kind replacement written strictly later. Both
# memories are episodic so the isolated store has session evidence and does not
# invoke nearby-store discovery during this focused production-path check.
CONTROL_WS="${ROOT}/isolated-control-tags"
mkdir -p "${CONTROL_WS}"
run_ee init --workspace "${CONTROL_WS}" --json
CONTROL_INIT_EXIT=$LAST_EXIT
run_ee remember "Next-only older control-tag candidate." \
    --workspace "${CONTROL_WS}" --level episodic --kind note --tags "next" --json
CONTROL_OLD_EXIT=$LAST_EXIT
CONTROL_OLD_ID="$(jq -r '.data.memoryId // .data.memory_id // .data.id // empty' "${LAST_STDOUT}")"
sleep 1
run_ee remember "Next-only newer control-tag candidate." \
    --workspace "${CONTROL_WS}" --level episodic --kind note --tags "next" --json
CONTROL_NEW_EXIT=$LAST_EXIT
CONTROL_NEW_ID="$(jq -r '.data.memoryId // .data.memory_id // .data.id // empty' "${LAST_STDOUT}")"
run_ee resume --workspace "${CONTROL_WS}" --json
CONTROL_RESUME_EXIT=$LAST_EXIT
CONTROL_JSON="${LAST_STDOUT}"
if [[ "${CONTROL_INIT_EXIT}" -eq 0 && "${CONTROL_OLD_EXIT}" -eq 0 \
    && "${CONTROL_NEW_EXIT}" -eq 0 && "${CONTROL_RESUME_EXIT}" -eq 0 \
    && -n "${CONTROL_OLD_ID}" && -n "${CONTROL_NEW_ID}" ]] \
    && jq -e --arg old "${CONTROL_OLD_ID}" --arg new "${CONTROL_NEW_ID}" '
        def epoch:
            (if contains(".") then split(".")[0] + "Z"
             else sub("\\+00:00$"; "Z") end)
            | fromdateiso8601;
        .data.report.staleCount == 0
        and .data.report.openLoops.taggedItemsTotal == 2
        and .data.report.openLoops.taggedItemsTruncated == false
        and ([.. | objects | select(.memoryId? == $old)]
             | length == 2
               and all(.[];
                   .kind == "note" and .tags == ["next"] and .stale == null))
        and ([.. | objects | select(.memoryId? == $new)]
             | length == 2
               and all(.[];
                   .kind == "note" and .tags == ["next"] and .stale == null))
        and (([.. | objects | select(.memoryId? == $old) | .createdAt]
              | unique | .[0] | epoch)
             < ([.. | objects | select(.memoryId? == $new) | .createdAt]
                | unique | .[0] | epoch))
        and ([.. | objects | select(.stale?.supersededBy? == $new)] | length == 0)
    ' "${CONTROL_JSON}" >/dev/null 2>&1; then
    event next_only_overlap_does_not_mark_stale pass
else
    event next_only_overlap_does_not_mark_stale fail \
        "init/old/new/resume exits=${CONTROL_INIT_EXIT}/${CONTROL_OLD_EXIT}/${CONTROL_NEW_EXIT}/${CONTROL_RESUME_EXIT}; ids=${CONTROL_OLD_ID}/${CONTROL_NEW_ID}; $(head -c 400 "${CONTROL_JSON}")"
fi

# A stale open-loop session member is rendered in both production projections,
# but staleCount counts its memory ID once and both flags identify the same
# strictly newer same-kind replacement through the non-control `arc4` tag.
DEDUP_WS="${ROOT}/isolated-stale-dedup"
mkdir -p "${DEDUP_WS}"
run_ee init --workspace "${DEDUP_WS}" --json
DEDUP_INIT_EXIT=$LAST_EXIT
run_ee remember "Arc 4 dedup candidate remains unfinished." \
    --workspace "${DEDUP_WS}" --level episodic --kind note \
    --tags "session-stale-dedup,next,arc4" --json
DEDUP_OLD_EXIT=$LAST_EXIT
DEDUP_OLD_ID="$(jq -r '.data.memoryId // .data.memory_id // .data.id // empty' "${LAST_STDOUT}")"
sleep 1
run_ee remember "Arc 4 dedup candidate landed." \
    --workspace "${DEDUP_WS}" --level semantic --kind note --tags "arc4" --json
DEDUP_NEW_EXIT=$LAST_EXIT
DEDUP_NEW_ID="$(jq -r '.data.memoryId // .data.memory_id // .data.id // empty' "${LAST_STDOUT}")"
run_ee resume --workspace "${DEDUP_WS}" --json
DEDUP_RESUME_EXIT=$LAST_EXIT
DEDUP_JSON="${LAST_STDOUT}"
if [[ "${DEDUP_INIT_EXIT}" -eq 0 && "${DEDUP_OLD_EXIT}" -eq 0 \
    && "${DEDUP_NEW_EXIT}" -eq 0 && "${DEDUP_RESUME_EXIT}" -eq 0 \
    && -n "${DEDUP_OLD_ID}" && -n "${DEDUP_NEW_ID}" ]] \
    && jq -e --arg old "${DEDUP_OLD_ID}" --arg new "${DEDUP_NEW_ID}" '
        def epoch:
            (if contains(".") then split(".")[0] + "Z"
             else sub("\\+00:00$"; "Z") end)
            | fromdateiso8601;
        .data.report.staleCount == 1
        and .data.report.openLoops.taggedItemsTotal == 1
        and ([.. | objects | select(.memoryId? == $old and .stale? != null)]
             | length == 2
               and ([.[].selectionReason] | sort)
                   == ["open_loop_tag", "recent_session_member"]
               and all(.[];
                   .kind == "note"
                   and .stale.supersededBy == $new
                   and .stale.sharedTags == ["arc4"]
                   and ((.stale.supersededByCreatedAt | epoch)
                        > (.createdAt | epoch))))
        and ([.. | objects | select(.stale?.supersededBy? == $new)] | length == 2)
    ' "${DEDUP_JSON}" >/dev/null 2>&1; then
    event stale_count_deduplicates_open_loop_and_session_projections pass
else
    event stale_count_deduplicates_open_loop_and_session_projections fail \
        "init/old/new/resume exits=${DEDUP_INIT_EXIT}/${DEDUP_OLD_EXIT}/${DEDUP_NEW_EXIT}/${DEDUP_RESUME_EXIT}; ids=${DEDUP_OLD_ID}/${DEDUP_NEW_ID}; $(head -c 400 "${DEDUP_JSON}")"
fi

if [[ "${EE_RESUME_E2E_SCOPE}" == "functional" ]]; then
    finish
    exit $?
fi

# --- bounded 10k corpus: real store, bounded output, <2s command ----------
SCALE_WS="${ROOT}/scale-10k"
mkdir -p "${SCALE_WS}"
run_ee init --workspace "${SCALE_WS}" --json
SCALE_INIT_EXIT=$LAST_EXIT
SCALE_BATCH_LIMIT=512
SCALE_TOTAL=10000
SCALE_STORED=0
SCALE_SEED_EXIT=0
SCALE_BATCH_START=1
while [[ "${SCALE_BATCH_START}" -le "${SCALE_TOTAL}" ]]; do
    SCALE_BATCH_COUNT="${SCALE_BATCH_LIMIT}"
    SCALE_REMAINING=$((SCALE_TOTAL - SCALE_BATCH_START + 1))
    if [[ "${SCALE_REMAINING}" -lt "${SCALE_BATCH_COUNT}" ]]; then
        SCALE_BATCH_COUNT="${SCALE_REMAINING}"
    fi

    STEP=$((STEP + 1))
    SCALE_SEED_STDOUT="${LOG_DIR}/step${STEP}.stdout"
    SCALE_SEED_STDERR="${LOG_DIR}/step${STEP}.stderr"
    awk -v first="${SCALE_BATCH_START}" -v count="${SCALE_BATCH_COUNT}" 'BEGIN {
        for (i = first; i < first + count; i++) {
            printf "{\"content\":\"Resume scale row %05d.\",\"level\":\"episodic\",\"kind\":\"note\",\"tags\":[\"session-scale-10k\"]}\n", i
        }
    }' | "${REAL_EE}" --workspace "${SCALE_WS}" remember --batch --stdin --json \
        >"${SCALE_SEED_STDOUT}" 2>"${SCALE_SEED_STDERR}"
    SCALE_BATCH_EXIT=${PIPESTATUS[1]}
    if [[ "${SCALE_BATCH_EXIT}" -ne 0 ]]; then
        SCALE_SEED_EXIT="${SCALE_BATCH_EXIT}"
        break
    fi

    SCALE_BATCH_STORED="$(jq -r '.data.storedCount // 0' "${SCALE_SEED_STDOUT}" 2>/dev/null)"
    if [[ ! "${SCALE_BATCH_STORED}" =~ ^[0-9]+$ ]]; then
        SCALE_SEED_EXIT=2
        break
    fi
    SCALE_STORED=$((SCALE_STORED + SCALE_BATCH_STORED))
    SCALE_BATCH_START=$((SCALE_BATCH_START + SCALE_BATCH_COUNT))
done
if [[ "${SCALE_INIT_EXIT}" -eq 0 && "${SCALE_SEED_EXIT}" -eq 0 \
    && "${SCALE_STORED}" -eq 10000 ]]; then
    event scale_10k_seeded pass
else
    event scale_10k_seeded fail "init=${SCALE_INIT_EXIT}; seed=${SCALE_SEED_EXIT}; stored=${SCALE_STORED}"
fi

SCALE_DATABASE="${SCALE_WS}/.ee/ee.db"
SCALE_HASH_BEFORE="$(store_fingerprint "${SCALE_DATABASE}")"
SCALE_STARTED_MS="$(now_ms)"
run_ee resume --workspace "${SCALE_WS}" --json
SCALE_ELAPSED_MS=$(( $(now_ms) - SCALE_STARTED_MS ))
SCALE_JSON="${LAST_STDOUT}"
SCALE_HASH_AFTER="$(store_fingerprint "${SCALE_DATABASE}")"
if [[ "${LAST_EXIT}" -eq 0 && "${SCALE_ELAPSED_MS}" -lt 2000 ]] \
    && jq -e '.data.report.episodicTotal == 10000
        and (.data.report.sessions | length) == 1
        and .data.report.sessions[0].memberCount == 10000
        and (.data.report.sessions[0].items | length) == 20' \
        "${SCALE_JSON}" >/dev/null 2>&1; then
    event scale_10k_resume_under_2s_and_bounded pass
else
    event scale_10k_resume_under_2s_and_bounded fail "exit=${LAST_EXIT}; elapsedMs=${SCALE_ELAPSED_MS}; $(head -c 300 "${SCALE_JSON}")"
fi
if [[ "${SCALE_HASH_BEFORE}" == "${SCALE_HASH_AFTER}" ]]; then
    event scale_10k_resume_preserves_db_wal_shm pass
else
    event scale_10k_resume_preserves_db_wal_shm fail "before=${SCALE_HASH_BEFORE//$'\n'/,}; after=${SCALE_HASH_AFTER//$'\n'/,}"
fi

# The bead's latency claim covers both machine and human output. Reuse the
# exact same 10k store so the human renderer cannot pass on a smaller fixture
# while only JSON carries the scale proof.
SCALE_HUMAN_HASH_BEFORE="$(store_fingerprint "${SCALE_DATABASE}")"
SCALE_HUMAN_STARTED_MS="$(now_ms)"
run_ee resume --workspace "${SCALE_WS}"
SCALE_HUMAN_ELAPSED_MS=$(( $(now_ms) - SCALE_HUMAN_STARTED_MS ))
SCALE_HUMAN="${LAST_STDOUT}"
SCALE_HUMAN_HASH_AFTER="$(store_fingerprint "${SCALE_DATABASE}")"
SCALE_HUMAN_ITEM_COUNT="$(grep -Ec '^  - mem_[^:]+:' "${SCALE_HUMAN}" 2>/dev/null || true)"
if [[ "${LAST_EXIT}" -eq 0 && "${SCALE_HUMAN_ELAPSED_MS}" -lt 2000 \
    && "${SCALE_HUMAN_ITEM_COUNT}" -eq 20 ]] \
    && grep -Fq "resume: 10000 episodic memories, 1 sessions shown" "${SCALE_HUMAN}" \
    && grep -Fq "[session-scale-10k] 10000 memories" "${SCALE_HUMAN}" \
    && grep -Fq "Open loops:" "${SCALE_HUMAN}" \
    && grep -Fq "Next commands:" "${SCALE_HUMAN}"; then
    event scale_10k_human_resume_under_2s_and_bounded pass
else
    event scale_10k_human_resume_under_2s_and_bounded fail \
        "exit=${LAST_EXIT}; elapsedMs=${SCALE_HUMAN_ELAPSED_MS}; items=${SCALE_HUMAN_ITEM_COUNT}; $(head -c 300 "${SCALE_HUMAN}")"
fi
if [[ "${SCALE_HUMAN_HASH_BEFORE}" == "${SCALE_HUMAN_HASH_AFTER}" ]]; then
    event scale_10k_human_resume_preserves_db_wal_shm pass
else
    event scale_10k_human_resume_preserves_db_wal_shm fail \
        "before=${SCALE_HUMAN_HASH_BEFORE//$'\n'/,}; after=${SCALE_HUMAN_HASH_AFTER//$'\n'/,}"
fi

# Reuse the same real 10k corpus for bd-orient-fast-content-iubub's literal
# latency guard. Index construction is deliberately outside the timed region:
# --fast promises a bounded read path, not a hidden benchmark of derived-index
# creation. The timed command must still return both useful content sections,
# keep only the honest doctor-skip degradation, and leave the source-of-truth
# store byte-identical.
run_ee --workspace "${SCALE_WS}" index rebuild --json
SCALE_INDEX_JSON="${LAST_STDOUT}"
if [[ "${LAST_EXIT}" -eq 0 ]] \
    && jq -e '.schema == "ee.response.v2" and .success == true' \
        "${SCALE_INDEX_JSON}" >/dev/null 2>&1; then
    event scale_10k_index_ready_for_fast_orient pass
else
    event scale_10k_index_ready_for_fast_orient fail \
        "exit=${LAST_EXIT}; $(head -c 300 "${SCALE_INDEX_JSON}")"
fi

SCALE_FAST_HASH_BEFORE="$(store_fingerprint "${SCALE_DATABASE}")"
SCALE_FAST_STARTED_MS="$(now_ms)"
run_ee --workspace "${SCALE_WS}" orient --fast "Resume scale row 09999" --json
SCALE_FAST_ELAPSED_MS=$(( $(now_ms) - SCALE_FAST_STARTED_MS ))
SCALE_FAST_JSON="${LAST_STDOUT}"
SCALE_FAST_HASH_AFTER="$(store_fingerprint "${SCALE_DATABASE}")"
if [[ "${LAST_EXIT}" -eq 0 && "${SCALE_FAST_ELAPSED_MS}" -lt 1000 ]] \
    && jq -e '
        .schema == "ee.response.v2"
        and .success == true
        and .data.mode == "fast"
        and .data.pack == null
        and (.data.fastContent.recent | length) >= 1
        and (.data.fastContent.relevant | length) >= 1
        and (.data.fastContent.recent | length) <= 5
        and (.data.fastContent.relevant | length) <= 5
        and all(.data.fastContent.recent[];
            (.id | length) > 0 and (.snippet | length) > 0
            and ((.snippet | split("\n") | length) <= 2)
            and (.createdAt | length) > 0
            and (.tags | type) == "array"
            and (.provenance | length) > 0)
        and all(.data.fastContent.relevant[];
            (.id | length) > 0 and (.snippet | length) > 0
            and ((.snippet | split("\n") | length) <= 2)
            and (.createdAt | length) > 0
            and (.tags | type) == "array"
            and (.provenance | length) > 0)
        and ([.data.fastContent.relevant[]?.snippet
              | select(contains("Resume scale row 09999."))] | length) >= 1
        and ([.degraded[]? | select(.code == "orient_doctor_skipped")] | length) == 1
        and ([.degraded[]? | select(.code == "orient_pack_skipped")] | length) == 0
    ' "${SCALE_FAST_JSON}" >/dev/null 2>&1; then
    event scale_10k_fast_orient_under_1s_with_content pass
else
    event scale_10k_fast_orient_under_1s_with_content fail \
        "exit=${LAST_EXIT}; elapsedMs=${SCALE_FAST_ELAPSED_MS}; $(head -c 400 "${SCALE_FAST_JSON}")"
fi
if [[ "${SCALE_FAST_HASH_BEFORE}" == "${SCALE_FAST_HASH_AFTER}" ]]; then
    event scale_10k_fast_orient_preserves_db_wal_shm pass
else
    event scale_10k_fast_orient_preserves_db_wal_shm fail \
        "before=${SCALE_FAST_HASH_BEFORE//$'\n'/,}; after=${SCALE_FAST_HASH_AFTER//$'\n'/,}"
fi

SCALE_FAST_HUMAN_HASH_BEFORE="$(store_fingerprint "${SCALE_DATABASE}")"
SCALE_FAST_HUMAN_STARTED_MS="$(now_ms)"
run_ee --workspace "${SCALE_WS}" orient --fast "Resume scale row 09999"
SCALE_FAST_HUMAN_ELAPSED_MS=$(( $(now_ms) - SCALE_FAST_HUMAN_STARTED_MS ))
SCALE_FAST_HUMAN="${LAST_STDOUT}"
SCALE_FAST_HUMAN_HASH_AFTER="$(store_fingerprint "${SCALE_DATABASE}")"
SCALE_FAST_HUMAN_RECENT="$(awk '
    /^Recent memories:$/ { capture = 1; next }
    capture && /^Task-relevant memories:$/ { exit }
    capture { print }
' "${SCALE_FAST_HUMAN}")"
SCALE_FAST_HUMAN_RELEVANT="$(awk '
    /^Task-relevant memories:$/ { capture = 1; next }
    capture && /^(Degraded|Next commands):$/ { exit }
    capture { print }
' "${SCALE_FAST_HUMAN}")"
SCALE_FAST_HUMAN_RECENT_SNIPPETS="$(awk '
    /^    / { sub(/^    /, ""); if (length > 0) print }
' <<<"${SCALE_FAST_HUMAN_RECENT}")"
SCALE_FAST_HUMAN_RELEVANT_SNIPPETS="$(awk '
    /^    / { sub(/^    /, ""); if (length > 0) print }
' <<<"${SCALE_FAST_HUMAN_RELEVANT}")"
SCALE_FAST_HUMAN_RECENT_COUNT="$(awk 'NF { count++ } END { print count + 0 }' \
    <<<"${SCALE_FAST_HUMAN_RECENT_SNIPPETS}")"
SCALE_FAST_HUMAN_RELEVANT_COUNT="$(awk 'NF { count++ } END { print count + 0 }' \
    <<<"${SCALE_FAST_HUMAN_RELEVANT_SNIPPETS}")"
if [[ "${LAST_EXIT}" -eq 0 && "${SCALE_FAST_HUMAN_ELAPSED_MS}" -lt 1000 ]] \
    && [[ "${SCALE_FAST_HUMAN_RECENT_COUNT}" -ge 1 \
        && "${SCALE_FAST_HUMAN_RECENT_COUNT}" -le 5 ]] \
    && [[ "${SCALE_FAST_HUMAN_RELEVANT_COUNT}" -ge 1 \
        && "${SCALE_FAST_HUMAN_RELEVANT_COUNT}" -le 5 ]] \
    && grep -Fxq "Resume scale row 09999." <<<"${SCALE_FAST_HUMAN_RELEVANT_SNIPPETS}" \
    && grep -Fq "orient_doctor_skipped" "${SCALE_FAST_HUMAN}" \
    && ! grep -Fq "orient_pack_skipped" "${SCALE_FAST_HUMAN}"; then
    event scale_10k_fast_human_orient_under_1s_with_queried_content pass
else
    event scale_10k_fast_human_orient_under_1s_with_queried_content fail \
        "exit=${LAST_EXIT}; elapsedMs=${SCALE_FAST_HUMAN_ELAPSED_MS}; recentCount=${SCALE_FAST_HUMAN_RECENT_COUNT}; relevantCount=${SCALE_FAST_HUMAN_RELEVANT_COUNT}; recent=$(head -c 180 <<<"${SCALE_FAST_HUMAN_RECENT_SNIPPETS}"); relevant=$(head -c 180 <<<"${SCALE_FAST_HUMAN_RELEVANT_SNIPPETS}")"
fi
if [[ "${SCALE_FAST_HUMAN_HASH_BEFORE}" == "${SCALE_FAST_HUMAN_HASH_AFTER}" ]]; then
    event scale_10k_fast_human_orient_preserves_db_wal_shm pass
else
    event scale_10k_fast_human_orient_preserves_db_wal_shm fail \
        "before=${SCALE_FAST_HUMAN_HASH_BEFORE//$'\n'/,}; after=${SCALE_FAST_HUMAN_HASH_AFTER//$'\n'/,}"
fi

# --- functional fast-orient probes on the same real 10k store --------------
# These probes are deliberately UNTIMED. The <1s events above remain the sole
# measured latency subject for the bead's bench guard; nothing below may be
# read as latency evidence.
run_ee --workspace "${SCALE_WS}" orient --fast "Resume scale row 09999" --json
FAST_REPEAT_ONE_EXIT=$LAST_EXIT
FAST_REPEAT_ONE="$(jq -cS '.data.fastContent' "${LAST_STDOUT}" 2>/dev/null)"
run_ee --workspace "${SCALE_WS}" orient --fast "Resume scale row 09999" --json
FAST_REPEAT_TWO_EXIT=$LAST_EXIT
FAST_REPEAT_TWO="$(jq -cS '.data.fastContent' "${LAST_STDOUT}" 2>/dev/null)"
if [[ "${FAST_REPEAT_ONE_EXIT}" -eq 0 && "${FAST_REPEAT_TWO_EXIT}" -eq 0 \
    && -n "${FAST_REPEAT_ONE}" && "${FAST_REPEAT_ONE}" != "null" \
    && "${FAST_REPEAT_ONE}" == "${FAST_REPEAT_TWO}" ]]; then
    event scale_10k_fast_content_repeat_bytes_identical pass
else
    event scale_10k_fast_content_repeat_bytes_identical fail \
        "exits=${FAST_REPEAT_ONE_EXIT}/${FAST_REPEAT_TWO_EXIT}; one=$(head -c 200 <<<"${FAST_REPEAT_ONE}"); two=$(head -c 200 <<<"${FAST_REPEAT_TWO}")"
fi

# Planted secret-shaped tag: remember-time policy scans content only, so the
# tag persists; the fast-orient egress must apply the shared public-replay
# redaction policy instead of leaking it, in JSON and human alike.
SECRET_TAG="ghp_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
run_ee --workspace "${SCALE_WS}" remember "Planted secret-shaped tag probe for fast orientation." \
    --level episodic --kind note --tags "fastprobe,${SECRET_TAG}" --json
SECRET_PLANT_EXIT=$LAST_EXIT
SECRET_MEMORY_ID="$(jq -r \
    '.data.memory_id // .data.memoryId // .data.memory.id // .data.id // empty' \
    "${LAST_STDOUT}" 2>/dev/null)"
run_ee --workspace "${SCALE_WS}" orient --fast "Planted secret-shaped tag probe" --json
SECRET_FAST_JSON="${LAST_STDOUT}"
SECRET_FAST_EXIT=$LAST_EXIT
if [[ "${SECRET_PLANT_EXIT}" -eq 0 && "${SECRET_FAST_EXIT}" -eq 0 ]] \
    && [[ -n "${SECRET_MEMORY_ID}" ]] \
    && ! grep -Fq "${SECRET_TAG}" "${SECRET_FAST_JSON}" \
    && jq -e --arg id "${SECRET_MEMORY_ID}" '
        ([.data.fastContent.recent[]?
          | select(.id == $id)
          | select(any(.tags[]?; . == "[REDACTED:github_token]"))]
         | length == 1)
        and
        ([.data.fastContent.relevant[]?
          | select(.id == $id)
          | select(any(.tags[]?; . == "[REDACTED:github_token]"))]
         | length == 1)
    ' "${SECRET_FAST_JSON}" >/dev/null 2>&1; then
    event fast_orient_json_redacts_secret_shaped_tag pass
else
    event fast_orient_json_redacts_secret_shaped_tag fail \
        "plant=${SECRET_PLANT_EXIT}; fast=${SECRET_FAST_EXIT}; $(head -c 300 "${SECRET_FAST_JSON}")"
fi

run_ee --workspace "${SCALE_WS}" orient --fast "Planted secret-shaped tag probe"
SECRET_FAST_HUMAN="${LAST_STDOUT}"
if [[ "${LAST_EXIT}" -eq 0 ]] \
    && [[ -n "${SECRET_MEMORY_ID}" ]] \
    && grep -Fq "${SECRET_MEMORY_ID}" "${SECRET_FAST_HUMAN}" \
    && grep -Fq "Planted secret-shaped tag probe for fast orientation." \
        "${SECRET_FAST_HUMAN}" \
    && grep -Fq "[REDACTED:github_token]" "${SECRET_FAST_HUMAN}" \
    && ! grep -Fq "${SECRET_TAG}" "${SECRET_FAST_HUMAN}"; then
    event fast_orient_human_never_leaks_secret_shaped_tag pass
else
    event fast_orient_human_never_leaks_secret_shaped_tag fail \
        "exit=${LAST_EXIT}; $(head -c 300 "${SECRET_FAST_HUMAN}")"
fi

finish
exit $?
