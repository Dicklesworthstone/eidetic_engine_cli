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

# --- literal acceptance corpus: three tagged sessions, two revisit
# decisions, one next-tagged item, and a superseded stale note -------------
run_ee remember "Session A wrapped the parser refactor." \
    --workspace "${WS}" --level episodic --kind note --tags "session-20260801" --json
run_ee remember "Session A left the lexer half-done." \
    --workspace "${WS}" --level episodic --kind note --tags "session-20260801" --json
run_ee remember "Session B finished the lexer and started codegen." \
    --workspace "${WS}" --level episodic --kind note --tags "session-20260808" --json
run_ee remember "Session C prepared the resume handoff." \
    --workspace "${WS}" --level episodic --kind note --tags "session-20260809" --json
run_ee remember "Next: wire codegen into the driver." \
    --workspace "${WS}" --level episodic --kind note --tags "session-20260809,next,arc4" --json
STALE_OLD_EXIT=$LAST_EXIT
STALE_OLD_ID="$(jq -r '.data.memoryId // .data.memory_id // .data.id // empty' "${LAST_STDOUT}")"
run_ee remember "Next: codegen driver wiring landed; next is optimization passes." \
    --workspace "${WS}" --level semantic --kind note --tags "session-20260809,next,arc4" --json
STALE_NEW_EXIT=$LAST_EXIT
STALE_NEW_ID="$(jq -r '.data.memoryId // .data.memory_id // .data.id // empty' "${LAST_STDOUT}")"
run_ee remember "Next-only older control-tag candidate." \
    --workspace "${WS}" --level semantic --kind note --tags "next" --json
NEXT_ONLY_OLD_EXIT=$LAST_EXIT
NEXT_ONLY_OLD_ID="$(jq -r '.data.memoryId // .data.memory_id // .data.id // empty' "${LAST_STDOUT}")"
run_ee remember "Next-only newer control-tag candidate." \
    --workspace "${WS}" --level semantic --kind note --tags "next" --json
NEXT_ONLY_NEW_EXIT=$LAST_EXIT
NEXT_ONLY_NEW_ID="$(jq -r '.data.memoryId // .data.memory_id // .data.id // empty' "${LAST_STDOUT}")"
if [[ "${STALE_OLD_EXIT}" -eq 0 && "${STALE_NEW_EXIT}" -eq 0 \
    && "${NEXT_ONLY_OLD_EXIT}" -eq 0 && "${NEXT_ONLY_NEW_EXIT}" -eq 0 \
    && -n "${STALE_OLD_ID}" && -n "${STALE_NEW_ID}" \
    && -n "${NEXT_ONLY_OLD_ID}" && -n "${NEXT_ONLY_NEW_ID}" ]]; then
    event corpus_seeded pass
else
    event corpus_seeded fail \
        "remember exits ${STALE_OLD_EXIT}/${STALE_NEW_EXIT}/${NEXT_ONLY_OLD_EXIT}/${NEXT_ONLY_NEW_EXIT}; ids=${STALE_OLD_ID}/${STALE_NEW_ID}/${NEXT_ONLY_OLD_ID}/${NEXT_ONLY_NEW_ID}"
fi

run_ee decide record "resume e2e revisit decision" --chosen "ship now" \
    --alternative "wait for perf pass" --rationale "deadline wins" \
    --revisit-by "+30d" --workspace "${WS}" --json
DECIDE_ONE=$LAST_EXIT
run_ee decide record "resume e2e second decision" --chosen "keep sqlite" \
    --alternative "switch stores" --rationale "franken-stack rule" \
    --revisit-by "+90d" --workspace "${WS}" --json
DECIDE_TWO=$LAST_EXIT
if [[ "${DECIDE_ONE}" -eq 0 && "${DECIDE_TWO}" -eq 0 ]]; then
    event decisions_recorded pass
else
    event decisions_recorded fail "decide exits ${DECIDE_ONE}/${DECIDE_TWO}"
fi

# --- the bundle -----------------------------------------------------------
STORE_DATABASE="${WS}/.ee/ee.db"
JSON_HASH_BEFORE="$(store_fingerprint "${STORE_DATABASE}")"
run_ee resume --workspace "${WS}" --json
R="${LAST_STDOUT}"
JSON_HASH_AFTER="$(store_fingerprint "${STORE_DATABASE}")"
if [[ "${JSON_HASH_BEFORE}" == "${JSON_HASH_AFTER}" ]]; then
    event json_resume_preserves_db_wal_shm pass
else
    event json_resume_preserves_db_wal_shm fail "before=${JSON_HASH_BEFORE//$'\n'/,}; after=${JSON_HASH_AFTER//$'\n'/,}"
fi
if [[ "${LAST_EXIT}" -eq 0 ]] \
    && jq -e '.data.report.schema == "ee.resume.v1"' "${R}" >/dev/null 2>&1; then
    event resume_returns_schema pass
else
    event resume_returns_schema fail "exit ${LAST_EXIT}; $(head -c 250 "${R}")"
fi

if jq -e '
    all(.data.report.sessions[]?.items[]?;
        .selectionReason == "recent_session_member"
        and (.provenance | has("uri") and has("trustClass") and has("verificationStatus"))
        and (.redaction | has("applied") and has("reasons")))
    and all(.data.report.openLoops.taggedItems[]?;
        .selectionReason == "open_loop_tag"
        and (.provenance | has("uri") and has("trustClass") and has("verificationStatus"))
        and (.redaction | has("applied") and has("reasons")))
' "${R}" >/dev/null 2>&1; then
    event resume_items_carry_public_posture pass
else
    event resume_items_carry_public_posture fail "$(head -c 350 "${R}")"
fi

if jq -e '[.data.report.sessions[]?.label]
          | length == 3
            and index("session-20260809") != null
            and index("session-20260808") != null
            and index("session-20260801") != null' \
    "${R}" >/dev/null 2>&1; then
    event three_tagged_sessions_grouped pass
else
    event three_tagged_sessions_grouped fail "$(jq -c '[.data.report.sessions[]?.label]' "${R}" 2>/dev/null)"
fi

if jq -e '[.data.report.openLoops.revisitDecisions[]? | select(.revisitBy != null)] | length == 2' \
    "${R}" >/dev/null 2>&1; then
    event revisit_decisions_surfaced pass
else
    event revisit_decisions_surfaced fail "$(jq -c '.data.report.openLoops.revisitDecisions' "${R}" 2>/dev/null | head -c 250)"
fi

if jq -e '.data.report.openLoops.revisitDecisionsTotal == 2
        and .data.report.openLoops.revisitDecisionsTruncated == false
        and .data.report.openLoops.taggedItemsTotal == 4
        and .data.report.openLoops.taggedItemsTruncated == false' \
    "${R}" >/dev/null 2>&1; then
    event open_loop_totals_are_exact pass
else
    event open_loop_totals_are_exact fail "$(jq -c '.data.report.openLoops' "${R}" 2>/dev/null | head -c 350)"
fi

if jq -e --arg stale_old "${STALE_OLD_ID}" --arg stale_new "${STALE_NEW_ID}" \
    --arg next_only_old "${NEXT_ONLY_OLD_ID}" --arg next_only_new "${NEXT_ONLY_NEW_ID}" \
    '[.data.report.openLoops.taggedItems[]? | select(.tags | index("next")) | .memoryId]
     | sort == ([$stale_old, $stale_new, $next_only_old, $next_only_new] | sort)' \
    "${R}" >/dev/null 2>&1; then
    event next_tagged_item_surfaced pass
else
    event next_tagged_item_surfaced fail "$(jq -c '.data.report.openLoops.taggedItems' "${R}" 2>/dev/null | head -c 250)"
fi

# The older note shares subject tag arc4 with the newer one. Its next and
# session tags are control tags. It appears in both report projections but
# staleCount counts its memory ID exactly once.
if jq -e --arg stale_old "${STALE_OLD_ID}" --arg stale_new "${STALE_NEW_ID}" \
    '.data.report.staleCount == 1
     and ([.. | objects | select(.memoryId? == $stale_old and .stale? != null)]
          | length == 2
            and all(.[];
                .stale.supersededBy == $stale_new
                and .stale.sharedTags == ["arc4"]))' \
    "${R}" >/dev/null 2>&1; then
    event superseded_note_carries_stale_marker pass
else
    event superseded_note_carries_stale_marker fail "staleCount=$(jq -r '.data.report.staleCount' "${R}" 2>/dev/null); $(head -c 250 "${R}")"
fi

# Sharing only the open-loop control tag `next` never establishes subject
# identity, even when the candidate is same-kind and strictly newer.
if jq -e --arg next_only_old "${NEXT_ONLY_OLD_ID}" --arg next_only_new "${NEXT_ONLY_NEW_ID}" \
    '([.. | objects | select(.memoryId? == $next_only_old)]
      | length == 1 and all(.[]; .stale == null))
     and ([.. | objects | select(.stale?.supersededBy? == $next_only_new)] | length == 0)' \
    "${R}" >/dev/null 2>&1; then
    event next_only_overlap_does_not_mark_stale pass
else
    event next_only_overlap_does_not_mark_stale fail \
        "$(jq -c --arg id "${NEXT_ONLY_OLD_ID}" '[.. | objects | select(.memoryId? == $id)]' "${R}" 2>/dev/null | head -c 350)"
fi

# --- human contract: every declared section remains visible ---------------
HUMAN_HASH_BEFORE="$(store_fingerprint "${STORE_DATABASE}")"
run_ee resume --workspace "${WS}"
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

if grep -Fq "queued " "${H}" \
    && grep -Fq "Next: wire codegen into the driver." "${H}" \
    && grep -Fq "[STALE]" "${H}"; then
    event human_open_loop_and_staleness_visible pass
else
    event human_open_loop_and_staleness_visible fail "$(head -c 500 "${H}")"
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
COLD_JSON_HASH_AFTER="$(store_fingerprint "${COLD_DATABASE}")"
BEST_ROOT="$(jq -r '.data.report.nearbyStores.stores[0].workspaceRoot // empty' \
    "${COLD_JSON}" 2>/dev/null)"
BEST_STORE="$(jq -r '.data.report.nearbyStores.stores[0].storeDir // empty' \
    "${COLD_JSON}" 2>/dev/null)"
BEST_DOCS="$(jq -r '.data.report.nearbyStores.stores[0].documents // 0' \
    "${COLD_JSON}" 2>/dev/null)"
BEST_LAST_WRITE="$(jq -r '.data.report.nearbyStores.stores[0].lastWrite // empty' \
    "${COLD_JSON}" 2>/dev/null)"
FIRST_COMMAND="$(jq -r '.data.report.nextCommands[0] // empty' "${COLD_JSON}" 2>/dev/null)"
EXPECTED_COMMAND="ee resume --workspace '${NEARBY_WS}' --database '${CAMPAIGN_DATABASE}' --json"
if [[ "${LAST_EXIT}" -eq 0 && "${BEST_ROOT}" == "${NEARBY_WS}" \
    && "${BEST_STORE}" == "${CAMPAIGN_STORE}" && "${BEST_DOCS}" -gt 0 \
    && -n "${BEST_LAST_WRITE}" \
    && "${FIRST_COMMAND}" == "${EXPECTED_COMMAND}" ]]; then
    event nearby_store_prepends_quoted_database_resume pass
else
    event nearby_store_prepends_quoted_database_resume fail \
        "exit=${LAST_EXIT}; root=${BEST_ROOT}; store=${BEST_STORE}; docs=${BEST_DOCS}; lastWrite=${BEST_LAST_WRITE}; command=${FIRST_COMMAND}"
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
SCALE_FAST_HUMAN_RELEVANT="$(awk '
    /^Task-relevant memories:$/ { capture = 1; next }
    capture && /^(Degraded|Next commands):$/ { exit }
    capture { print }
' "${SCALE_FAST_HUMAN}")"
if [[ "${LAST_EXIT}" -eq 0 && "${SCALE_FAST_HUMAN_ELAPSED_MS}" -lt 1000 ]] \
    && grep -Fq "Recent memories:" "${SCALE_FAST_HUMAN}" \
    && grep -Fq "Task-relevant memories:" "${SCALE_FAST_HUMAN}" \
    && grep -Fq "Resume scale row 09999." <<<"${SCALE_FAST_HUMAN_RELEVANT}" \
    && grep -Fq "orient_doctor_skipped" "${SCALE_FAST_HUMAN}" \
    && ! grep -Fq "orient_pack_skipped" "${SCALE_FAST_HUMAN}"; then
    event scale_10k_fast_human_orient_under_1s_with_queried_content pass
else
    event scale_10k_fast_human_orient_under_1s_with_queried_content fail \
        "exit=${LAST_EXIT}; elapsedMs=${SCALE_FAST_HUMAN_ELAPSED_MS}; relevant=$(head -c 300 <<<"${SCALE_FAST_HUMAN_RELEVANT}")"
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
