#!/usr/bin/env bash
# bd-d67os.27 item 1 — no-mock multi-process single-shot write contention E2E.
#
# Spawns N concurrent OS processes doing interleaved `ee journal append` and
# `ee remember` against ONE workspace database, then asserts ZERO hard storage
# errors: every write either persisted (exit 0, success=true) or is a bug.
# This is the empirical proof that the bd-d67os.26 retry-classification fix
# (flock-gate contention timeout classified transient) plus the bd-d67os.27
# widened flock budget + per-process jitter actually eliminate dropped
# single-shot writes under real cross-process flock contention — the .26 unit
# tests only prove classification, not end-to-end behavior.
#
# Environment:
#   EE_BIN / EE_BINARY  Path to the ee binary (default: `ee` on PATH)
#   EE_E2E_TMPDIR       Temp base (default /private/tmp)
#   EE_E2E_WRITERS      Concurrent writer processes (default 6)
#   EE_E2E_WRITES       Writes per process (default 10)

set -uo pipefail

TEST_ID="single_shot_write_contention_e2e"
FAILURES=0

if ! command -v jq >/dev/null 2>&1; then
    echo "jq missing" >&2
    exit 3
fi

E2E_SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="${REPO_ROOT:-$(cd "${E2E_SCRIPT_DIR}/.." && pwd)}"
# bd-mfqa2: the macOS temp path is a PLATFORM preference, not a default for
# every host. Resolved centrally so this script does not re-implement it.
# shellcheck source=scripts/lib/e2e_tmproot.sh
# shellcheck disable=SC1091
source "${E2E_SCRIPT_DIR}/lib/e2e_tmproot.sh"

emit_event_json() {
    local label="$1" status="$2" diagnosis="${3:-}"
    jq -cn --arg test_id "${TEST_ID}" --arg label "${label}" --arg status "${status}" \
        --arg diagnosis "${diagnosis}" \
        '{schema:"ee.test_event.v1",test_id:$test_id,kind:"assert_result",fields:{label:$label,status:$status,first_failure_diagnosis:$diagnosis}}'
}

run_self_test() {
    local st_base st_root st_out st_rc
    st_base="$(e2e_temp_root)"
    st_root="$(mktemp -d "${st_base%/}/ee-write-contention-selftest.XXXXXX")"

    # Arm 1 (negative): missing EE_E2E_TMPDIR fails closed at temp_root_ok
    # before touching /ws or /logs and records the missing path in diagnosis.
    local missing_tmp="${st_root}/does-not-exist"
    st_out="$(EE_E2E_TMPDIR="${missing_tmp}" EE_SINGLE_SHOT_CONTENTION_PROBE_STOP_AFTER_INIT=1 \
        bash "${BASH_SOURCE[0]}" 2>/dev/null)"
    st_rc=$?
    if [[ "${st_rc}" -ne 3 ]] || ! jq -e --arg p "${missing_tmp}" '
        .schema == "ee.test_event.v1"
        and .fields.label == "temp_root_ok"
        and .fields.status == "fail"
        and (.fields.first_failure_diagnosis | contains($p))
    ' <<<"${st_out}" >/dev/null; then
        echo "FAIL self-test arm 1 (missing temp root): rc=${st_rc} out=${st_out}" >&2
        return 1
    fi

    # Arm 2 (negative): failing `ee init --json` that writes its error envelope
    # to stdout (init.json) with empty stderr still populates first_failure_diagnosis.
    local fake_ee_fail="${st_root}/fake-ee-fail"
    cat >"${fake_ee_fail}" <<'EOF'
#!/usr/bin/env bash
printf '{"schema":"ee.response.v2","success":false,"error":{"code":"storage_open_failed","message":"simulated stdout-only init error"}}\n'
exit 2
EOF
    chmod +x "${fake_ee_fail}"
    st_out="$(EE_E2E_TMPDIR="${st_root}" EE_BIN="${fake_ee_fail}" \
        EE_SKIP_BINARY_FRESHNESS_CHECK=1 EE_SINGLE_SHOT_CONTENTION_PROBE_STOP_AFTER_INIT=1 \
        bash "${BASH_SOURCE[0]}" 2>/dev/null)"
    st_rc=$?
    if [[ "${st_rc}" -ne 2 ]] || ! jq -s -e '
        any(.[]; .fields.label == "temp_root_ok" and .fields.status == "pass")
        and any(.[]; .fields.label == "init_ok"
            and .fields.status == "fail"
            and (.fields.first_failure_diagnosis | contains("simulated stdout-only init error")))
    ' <<<"${st_out}" >/dev/null; then
        echo "FAIL self-test arm 2 (stdout-only init failure diagnosis): rc=${st_rc} out=${st_out}" >&2
        return 1
    fi

    # Arm 3 (positive): valid temp root + succeeding init passes both guards.
    local fake_ee_ok="${st_root}/fake-ee-ok"
    cat >"${fake_ee_ok}" <<'EOF'
#!/usr/bin/env bash
printf '{"schema":"ee.response.v2","success":true}\n'
exit 0
EOF
    chmod +x "${fake_ee_ok}"
    st_out="$(EE_E2E_TMPDIR="${st_root}" EE_BIN="${fake_ee_ok}" \
        EE_SKIP_BINARY_FRESHNESS_CHECK=1 EE_SINGLE_SHOT_CONTENTION_PROBE_STOP_AFTER_INIT=1 \
        bash "${BASH_SOURCE[0]}" 2>/dev/null)"
    st_rc=$?
    if [[ "${st_rc}" -ne 0 ]] || ! jq -s -e '
        any(.[]; .fields.label == "temp_root_ok" and .fields.status == "pass")
        and any(.[]; .fields.label == "init_ok" and .fields.status == "pass")
    ' <<<"${st_out}" >/dev/null; then
        echo "FAIL self-test arm 3 (positive init guard): rc=${st_rc} out=${st_out}" >&2
        return 1
    fi

    echo "PASS: e2e_single_shot_write_contention.sh --self-test (3/3 arms)"
    return 0
}

if [[ "${1:-}" == "--self-test" ]]; then
    run_self_test
    exit $?
fi

# bd-79x8m: validate temp root and mktemp creation BEFORE resolving the binary
# or deriving /ws and /logs paths, so a missing EE_E2E_TMPDIR can never collapse
# ROOT to "" and attempt mkdir -p /ws /logs at the filesystem root.
ROOT_BASE="$(e2e_temp_root)"
if [[ -z "${ROOT_BASE}" || ! -d "${ROOT_BASE}" ]]; then
    emit_event_json temp_root_ok fail "temp root missing or not a directory: ${ROOT_BASE:-<empty>}"
    echo "cannot continue without an existing temp root (${ROOT_BASE:-<empty>})" >&2
    exit 3
fi
mktemp_err=""
if ! ROOT="$(mktemp -d "${ROOT_BASE%/}/ee-write-contention-e2e.XXXXXX" 2>&1)"; then
    mktemp_err="${ROOT}"
    ROOT=""
fi
if [[ -z "${ROOT}" || ! -d "${ROOT}" || "${ROOT}" == "/" ]]; then
    emit_event_json temp_root_ok fail "mktemp -d failed under ${ROOT_BASE}: ${mktemp_err:-empty ROOT}"
    echo "cannot continue without a writable workspace temp dir under ${ROOT_BASE}" >&2
    exit 3
fi
WS="${ROOT}/ws"
LOG_DIR="${ROOT}/logs"
EVENT_LOG="${LOG_DIR}/events.jsonl"
if ! mkdir -p "${WS}" "${LOG_DIR}" 2>/dev/null || ! : >"${EVENT_LOG}" 2>/dev/null; then
    emit_event_json temp_root_ok fail "failed to initialize workspace/log directories under ${ROOT}"
    echo "cannot continue without writable workspace/log directories under ${ROOT}" >&2
    exit 3
fi

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
if [[ "${EE_SKIP_BINARY_FRESHNESS_CHECK:-0}" != "1" ]]; then
    if ! ee_require_current_binary "${REAL_EE}" "e2e_single_shot_write_contention"; then
        exit 3
    fi
fi

WRITERS="${EE_E2E_WRITERS:-6}"
WRITES="${EE_E2E_WRITES:-10}"
RUN_TOKEN="$(basename "${ROOT}" | tr -cd '[:alnum:]')"
CONTENT_MARKER="contentionfact${RUN_TOKEN}"

event() {
    local label="$1" status="$2" diagnosis="${3:-}"
    emit_event_json "${label}" "${status}" "${diagnosis}" | tee -a "${EVENT_LOG}"
    if [[ "${status}" == "fail" ]]; then
        FAILURES=$((FAILURES + 1))
    fi
}

event temp_root_ok pass

if ! "${REAL_EE}" init --workspace "${WS}" --json >"${LOG_DIR}/init.json" 2>"${LOG_DIR}/init.err"; then
    init_rc=$?
    init_err_head="$(head -c 240 "${LOG_DIR}/init.err" 2>/dev/null || true)"
    init_out_head="$(head -c 240 "${LOG_DIR}/init.json" 2>/dev/null || true)"
    event init_ok fail "init failed (rc=${init_rc}): stderr=${init_err_head:-<empty>}; stdout=${init_out_head:-<empty>}"
    echo "cannot continue without a workspace" >&2
    exit 2
fi
event init_ok pass
if [[ "${EE_SINGLE_SHOT_CONTENTION_PROBE_STOP_AFTER_INIT:-0}" == "1" ]]; then
    exit 0
fi

# --- concurrent interleaved writers -------------------------------------
# Each worker alternates journal append / remember. Every command's exit
# code and JSON success flag land in a per-worker results file.
worker() {
    local worker_id="$1"
    local results="${LOG_DIR}/worker${worker_id}.results"
    : >"${results}"
    local j out rc kind
    for ((j = 1; j <= WRITES; j++)); do
        if (((worker_id + j) % 2 == 0)); then
            kind="journal_append"
            out="$("${REAL_EE}" --workspace "${WS}" journal append \
                "contention observation w${worker_id}-${j}" \
                --kind observation --session "e2e-contention-w${worker_id}" \
                --json 2>>"${results}.stderr")"
            rc=$?
        else
            kind="remember"
            out="$("${REAL_EE}" --workspace "${WS}" remember \
                "${CONTENT_MARKER} w${worker_id}-${j}" --json 2>>"${results}.stderr")"
            rc=$?
        fi
        local success schema persisted memory_id label
        success="$(jq -r '.success // false' <<<"${out}" 2>/dev/null || echo parse_error)"
        schema="$(jq -r '.schema // empty' <<<"${out}" 2>/dev/null || true)"
        label="w${worker_id}-${j}"
        echo "${kind} ${j} rc=${rc} success=${success} schema=${schema}" >>"${results}"
        if [[ "${rc}" -ne 0 || "${success}" != "true" || "${schema}" != "ee.response.v2" ]]; then
            echo "FAILED ${kind} ${label}: rc=${rc} success=${success} schema=${schema} out=$(head -c 300 <<<"${out}")" >>"${results}"
        elif [[ "${kind}" == "remember" ]]; then
            persisted="$(jq -r '.data.persisted // false' <<<"${out}")"
            memory_id="$(jq -r '.data.memoryId // empty' <<<"${out}")"
            if [[ "${persisted}" != "true" || -z "${memory_id}" ]]; then
                echo "FAILED remember ${label}: persisted=${persisted} memoryId=${memory_id:-missing}" >>"${results}"
            else
                printf 'MEMORY %s %s\n' "${label}" "${memory_id}" >>"${results}"
            fi
        fi
    done
}

pids=()
for ((w = 1; w <= WRITERS; w++)); do
    worker "${w}" &
    pids+=($!)
done
for pid in "${pids[@]}"; do
    wait "${pid}"
done

# --- zero hard failures --------------------------------------------------
# Journal appends and remembers must both survive the shared-workspace writer
# queue. bd-rs4cm made remember's advisory-lock wait progress-aware, so any
# dropped remember is now a hard regression rather than an accepted branch.
journal_failed="$(grep -hc '^FAILED journal_append' "${LOG_DIR}"/worker*.results 2>/dev/null | awk '{ total += $1 } END { print total + 0 }')"
remember_failed="$(grep -hc '^FAILED remember' "${LOG_DIR}"/worker*.results 2>/dev/null | awk '{ total += $1 } END { print total + 0 }')"
if [[ "${journal_failed}" -eq 0 ]]; then
    event zero_dropped_journal_appends pass
else
    diagnosis="$(grep -h '^FAILED journal_append' "${LOG_DIR}"/worker*.results | head -2)"
    event zero_dropped_journal_appends fail "${journal_failed} journal appends dropped; first: ${diagnosis}"
fi
if [[ "${remember_failed}" -eq 0 ]]; then
    event zero_dropped_remembers pass
else
    diagnosis="$(grep -h '^FAILED remember' "${LOG_DIR}"/worker*.results | head -2)"
    event zero_dropped_remembers fail "${remember_failed} remembers dropped; first: ${diagnosis}"
fi

# --- persisted counts match what was written -----------------------------
expected_total=$((WRITERS * WRITES))
expected_journal=0
for ((w = 1; w <= WRITERS; w++)); do
    for ((j = 1; j <= WRITES; j++)); do
        if (((w + j) % 2 == 0)); then
            expected_journal=$((expected_journal + 1))
        fi
    done
done
expected_memories=$((expected_total - expected_journal))

journal_stderr="${LOG_DIR}/journal-list.stderr"
journal_json="$("${REAL_EE}" --workspace "${WS}" journal list --limit 500 --json 2>"${journal_stderr}")"
journal_rc=$?
journal_count="$(jq -r '[.data.entries // .data.journalEntries // [] | length] | first' <<<"${journal_json}" 2>/dev/null || echo -1)"
if [[ "${journal_rc}" -eq 0 && "${journal_count}" == "${expected_journal}" ]]; then
    event journal_rows_persisted pass
else
    event journal_rows_persisted fail \
        "expected ${expected_journal} journal entries, rc=${journal_rc}, listed=${journal_count}; stderr=$(head -c 240 "${journal_stderr}"); stdout=$(head -c 240 <<<"${journal_json}")"
fi

memory_rows="${LOG_DIR}/remembered.rows"
grep -h '^MEMORY ' "${LOG_DIR}"/worker*.results | sort >"${memory_rows}" || true
recorded_memory_count="$(wc -l <"${memory_rows}" | tr -d ' ')"
unique_memory_count="$(awk '{print $3}' "${memory_rows}" | sort -u | wc -l | tr -d ' ')"
if [[ "${recorded_memory_count}" == "${expected_memories}" && "${unique_memory_count}" == "${expected_memories}" ]]; then
    event remembered_ids_are_complete_and_unique pass
else
    event remembered_ids_are_complete_and_unique fail "expected ${expected_memories} response IDs, recorded=${recorded_memory_count}, unique=${unique_memory_count}"
fi

show_failures=0
show_diagnosis=""
while read -r _ label memory_id; do
    [[ -n "${memory_id:-}" ]] || continue
    show_stderr="${LOG_DIR}/memory-show-${label}.stderr"
    show_json="$("${REAL_EE}" --workspace "${WS}" memory show "${memory_id}" --json 2>"${show_stderr}")"
    show_rc=$?
    expected_content="${CONTENT_MARKER} ${label}"
    if [[ "${show_rc}" -ne 0 ]] || ! jq -e --arg memory_id "${memory_id}" --arg content "${expected_content}" '
        .schema == "ee.response.v2"
        and .success == true
        and .data.found == true
        and .data.memoryId == $memory_id
        and .data.memory.content == $content
    ' <<<"${show_json}" >/dev/null; then
        show_failures=$((show_failures + 1))
        if [[ -z "${show_diagnosis}" ]]; then
            show_diagnosis="${label}/${memory_id}: rc=${show_rc}; stderr=$(head -c 240 "${show_stderr}"); stdout=$(head -c 240 <<<"${show_json}")"
        fi
    fi
done <"${memory_rows}"
if [[ "${show_failures}" -eq 0 && "${recorded_memory_count}" == "${expected_memories}" ]]; then
    event every_remember_response_is_durably_readable pass
else
    event every_remember_response_is_durably_readable fail "showFailures=${show_failures}; first=${show_diagnosis:-missing response IDs}"
fi

# The writer path must converge the derived index before any search can
# trigger read repair. Numeric/non-null generations plus exact corpus counts
# rule out null-equality and partial-index false positives.
index_stderr="${LOG_DIR}/index-status.stderr"
index_json="$("${REAL_EE}" --workspace "${WS}" index status --json 2>"${index_stderr}")"
index_rc=$?
if [[ "${index_rc}" -eq 0 ]] && jq -e --argjson expected_memories "${expected_memories}" '
    .schema == "ee.response.v2"
    and .success == true
    and .data.health == "ready"
    and .data.indexExists == true
    and (.data.dbGeneration | type) == "number"
    and (.data.indexGeneration | type) == "number"
    and .data.indexGeneration == .data.dbGeneration
    and .data.dbMemoryCount == $expected_memories
    and .data.indexDocumentCount == $expected_memories
    and .data.indexDocumentCounts.memories == $expected_memories
    and (.data.actualCorpusRevision | type) == "string"
    and (.data.expectedCorpusRevision | type) == "string"
    and .data.actualCorpusRevision == .data.expectedCorpusRevision
    and (
        .data.degradationCode == null
        or (.data.degradationCode == "embed_model_unavailable"
            and .data.embedding.semantic == false)
        or (.data.degradationCode == "search_lexical_only"
            and .data.embedding.semantic == true
            and .data.embedding.vector_coverage.total > 0
            and .data.embedding.vector_coverage.embedded == 0)
    )
    and (.data.degraded | type) == "array"
    and ([.data.degraded[].code]
        == (if .data.degradationCode == null then [] else [.data.degradationCode] end))
    and .data.repairHint == null
    and .data.lastCheckError == null
' <<<"${index_json}" >/dev/null; then
    event search_index_fresh_before_search pass
else
    event search_index_fresh_before_search fail \
        "expected ready generation-matched ${expected_memories}-memory index before search, rc=${index_rc}; stderr=$(head -c 240 "${index_stderr}"); stdout=$(head -c 400 <<<"${index_json}")"
fi

search_stderr="${LOG_DIR}/search.stderr"
memory_json="$("${REAL_EE}" --workspace "${WS}" search "${CONTENT_MARKER}" --limit "${expected_memories}" \
    --source-mode lexical_only --strict-source-mode --json 2>"${search_stderr}")"
search_rc=$?
memory_count="$(jq -r '.data.resultCount // (.data.results | length) // -1' <<<"${memory_json}" 2>/dev/null || echo -1)"
expected_ids="${LOG_DIR}/expected-memory-ids.sorted"
actual_ids="${LOG_DIR}/actual-search-doc-ids.sorted"
awk '{print $3}' "${memory_rows}" | LC_ALL=C sort >"${expected_ids}"
search_shape_ok=false
if jq -e '
    .schema == "ee.response.v2"
    and .success == true
    and (.data.results | type) == "array"
    and all(.data.results[]; (.docId | type) == "string" and (.docId | length) > 0)
    and all((.degraded // [])[];
        .code as $code
        | (["search_index_stale", "index_stale", "stale_index", "search_index_large_gap"]
            | index($code) | not))
    and all((.data.degraded // [])[];
        .code as $code
        | (["search_index_stale", "index_stale", "stale_index", "search_index_large_gap"]
            | index($code) | not))
' <<<"${memory_json}" >/dev/null 2>&1; then
    search_shape_ok=true
    jq -r '.data.results[].docId' <<<"${memory_json}" | LC_ALL=C sort >"${actual_ids}"
else
    : >"${actual_ids}"
fi
actual_id_count="$(wc -l <"${actual_ids}" | tr -d ' ')"
if [[ "${search_rc}" -eq 0 \
    && "${search_shape_ok}" == "true" \
    && "${memory_count}" == "${expected_memories}" \
    && "${actual_id_count}" == "${expected_memories}" ]] \
    && cmp -s "${expected_ids}" "${actual_ids}"; then
    event search_doc_ids_equal_remember_response_ids pass
else
    id_diff="$(diff -u "${expected_ids}" "${actual_ids}" 2>/dev/null | head -c 400 || true)"
    event search_doc_ids_equal_remember_response_ids fail \
        "expected exact ${expected_memories}-ID search set, rc=${search_rc}, shape=${search_shape_ok}, resultCount=${memory_count}, actualIds=${actual_id_count}; diff=${id_diff}; stderr=$(head -c 240 "${search_stderr}"); stdout=$(head -c 240 <<<"${memory_json}")"
fi

echo
echo "single-shot write contention e2e: ${WRITERS} writers x ${WRITES} writes, ${FAILURES} assertion failures; workspace ${ROOT}"
if [[ "${FAILURES}" -gt 0 ]]; then
    exit 2
fi
exit 0
