#!/usr/bin/env bash
# bd-resume-verb-v0f57 — `ee resume` E2E: the acceptance fixture from the
# bead (tagged sessions + revisit decisions + a next-tagged item + a
# superseded stale note) against the real binary. No mocks.
#
# Environment:
#   EE_BIN / EE_BINARY  Path to the ee binary (default: `ee` on PATH)
#   EE_E2E_TMPDIR       Temp base (default /private/tmp)
#   EE_EMBED_DOWNLOAD   Model download policy (default: off for deterministic E2E)

set -uo pipefail

TEST_ID="resume_e2e"
FAILURES=0
STEP=0

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
    --workspace "${WS}" --level semantic --kind note --tags "next,codegen" --json
STALE_OLD_EXIT=$LAST_EXIT
run_ee remember "Next: codegen driver wiring landed; next is optimization passes." \
    --workspace "${WS}" --level semantic --kind note --tags "next,codegen" --json
STALE_NEW_EXIT=$LAST_EXIT
if [[ "${STALE_OLD_EXIT}" -eq 0 && "${STALE_NEW_EXIT}" -eq 0 ]]; then
    event corpus_seeded pass
else
    event corpus_seeded fail "remember exits ${STALE_OLD_EXIT}/${STALE_NEW_EXIT}"
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

# --- cold workspace: nearby populated child retargets resume safely -------
COLD_WS="${ROOT}/cold start"
NEARBY_WS="${COLD_WS}/nearby campaign root"
mkdir -p "${COLD_WS}" "${NEARBY_WS}"
run_ee init --workspace "${COLD_WS}" --json
COLD_INIT_EXIT=$LAST_EXIT
run_ee init --workspace "${NEARBY_WS}" --json
NEARBY_INIT_EXIT=$LAST_EXIT
run_ee remember "Nearby campaign has durable state." --workspace "${NEARBY_WS}" \
    --level semantic --kind note --json
NEARBY_SEED_EXIT=$LAST_EXIT
if [[ "${COLD_INIT_EXIT}" -eq 0 && "${NEARBY_INIT_EXIT}" -eq 0 && "${NEARBY_SEED_EXIT}" -eq 0 ]]; then
    event nearby_store_seeded pass
else
    event nearby_store_seeded fail "init/seed exits ${COLD_INIT_EXIT}/${NEARBY_INIT_EXIT}/${NEARBY_SEED_EXIT}"
fi

COLD_DATABASE="${COLD_WS}/.ee/ee.db"
COLD_JSON_HASH_BEFORE="$(store_fingerprint "${COLD_DATABASE}")"
run_ee resume --workspace "${COLD_WS}" --json
COLD_JSON="${LAST_STDOUT}"
COLD_JSON_HASH_AFTER="$(store_fingerprint "${COLD_DATABASE}")"
BEST_ROOT="$(jq -r '.data.report.nearbyStores.stores[0].workspaceRoot // empty' "${COLD_JSON}" 2>/dev/null)"
BEST_DOCS="$(jq -r '.data.report.nearbyStores.stores[0].documents // 0' "${COLD_JSON}" 2>/dev/null)"
BEST_LAST_WRITE="$(jq -r '.data.report.nearbyStores.stores[0].lastWrite // empty' "${COLD_JSON}" 2>/dev/null)"
FIRST_COMMAND="$(jq -r '.data.report.nextCommands[0] // empty' "${COLD_JSON}" 2>/dev/null)"
EXPECTED_COMMAND="ee resume --workspace '${NEARBY_WS}' --json"
if [[ "${LAST_EXIT}" -eq 0 && "${BEST_ROOT}" == "${NEARBY_WS}" \
    && "${BEST_DOCS}" -gt 0 && -n "${BEST_LAST_WRITE}" \
    && "${FIRST_COMMAND}" == "${EXPECTED_COMMAND}" ]]; then
    event nearby_store_prepends_quoted_resume pass
else
    event nearby_store_prepends_quoted_resume fail "exit=${LAST_EXIT}; root=${BEST_ROOT}; docs=${BEST_DOCS}; lastWrite=${BEST_LAST_WRITE}; command=${FIRST_COMMAND}"
fi
if [[ "${COLD_JSON_HASH_BEFORE}" == "${COLD_JSON_HASH_AFTER}" ]]; then
    event nearby_json_resume_preserves_db_wal_shm pass
else
    event nearby_json_resume_preserves_db_wal_shm fail "before=${COLD_JSON_HASH_BEFORE//$'\n'/,}; after=${COLD_JSON_HASH_AFTER//$'\n'/,}"
fi

COLD_HUMAN_HASH_BEFORE="$(store_fingerprint "${COLD_DATABASE}")"
run_ee resume --workspace "${COLD_WS}"
COLD_HUMAN="${LAST_STDOUT}"
COLD_HUMAN_HASH_AFTER="$(store_fingerprint "${COLD_DATABASE}")"
if [[ "${LAST_EXIT}" -eq 0 ]] \
    && grep -Fq "${NEARBY_WS}/.ee" "${COLD_HUMAN}" \
    && grep -Fq "${BEST_DOCS} documents" "${COLD_HUMAN}" \
    && grep -Fq "last write ${BEST_LAST_WRITE}" "${COLD_HUMAN}"; then
    event nearby_human_shows_path_docs_last_write pass
else
    event nearby_human_shows_path_docs_last_write fail "exit ${LAST_EXIT}; $(head -c 500 "${COLD_HUMAN}")"
fi
if [[ "${COLD_HUMAN_HASH_BEFORE}" == "${COLD_HUMAN_HASH_AFTER}" ]]; then
    event nearby_human_resume_preserves_db_wal_shm pass
else
    event nearby_human_resume_preserves_db_wal_shm fail "before=${COLD_HUMAN_HASH_BEFORE//$'\n'/,}; after=${COLD_HUMAN_HASH_AFTER//$'\n'/,}"
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
        and all(.data.fastContent.recent[]; (.id | length) > 0 and (.snippet | length) > 0)
        and all(.data.fastContent.relevant[]; (.id | length) > 0 and (.snippet | length) > 0)
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

echo
echo "resume e2e: ${STEP} steps, ${FAILURES} failures; root ${ROOT}"
if [[ "${FAILURES}" -gt 0 ]]; then
    exit 2
fi
exit 0
