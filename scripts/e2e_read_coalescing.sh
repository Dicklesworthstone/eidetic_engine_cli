#!/usr/bin/env bash
# bd-d67os.10 (Track A, read coalescing) - real-binary E2E for process-local
# single-flight read coalescing.
#
# Read coalescing is PROCESS-LOCAL (ADR-tracked; cross-process coalescing is a
# documented future option, see docs/read_coalescing.md). Separate `ee`
# processes therefore do NOT share a single-flight group, so the honest way to
# observe coalescing under concurrency with the real binary is the in-process
# burst harness:
#
#   ee graph feature-enrichment --dry-run --singleflight-burst N \
#       --singleflight-distinct D --json
#
# That fires N concurrent IDENTICAL requests plus D concurrent DISTINCT requests
# through the live coalescer in one process and reports the leader/follower
# telemetry. This script asserts, against the real binary:
#   - identical concurrent requests collapse to exactly ONE leader computation
#     (summary.identicalLeaderCount == 1, executionCount accounts for 1 identical
#     run), and every follower coalesced (summary.identicalFollowerCount == N-1);
#   - all coalesced callers observe a result-identical value
#     (resultHashes.identicalUniqueCount == 1);
#   - distinct keys (a bumped workspace/graph generation lands in the key) BUST
#     coalescing: each distinct request is its own leader
#     (summary.distinctLeaderCount == D, resultHashes.distinctUniqueCount == D);
#   - the burst posture is byte-stable across repeated runs (determinism).
#
# Artifacts are intentionally retained. AGENTS.md forbids implicit cleanup.

set -euo pipefail

if ! command -v jq >/dev/null 2>&1; then
    printf '%s\n' '{"schema":"ee.test_event.v1","test_id":"read_coalescing_e2e","kind":"assert_result","fields":{"label":"jq_available","status":"fail","first_failure_diagnosis":"jq executable missing before harness init"}}' >&2
    exit 3
fi

EE_BIN="${EE_BIN:-${EE_BINARY:-ee}}"
if [[ "${EE_BIN}" == */* ]]; then
    REAL_EE="${EE_BIN}"
else
    REAL_EE="$(command -v "${EE_BIN}" 2>/dev/null || true)"
fi
if [[ -z "${REAL_EE}" || ! -x "${REAL_EE}" ]]; then
    printf '%s\n' '{"schema":"ee.test_event.v1","test_id":"read_coalescing_e2e","kind":"assert_result","fields":{"label":"ee_binary_available","status":"fail","first_failure_diagnosis":"prebuilt ee binary missing before harness init"}}' >&2
    exit 3
fi

# bd-2vq2z / e2e harness rule: ExFAT TMPDIR breaks DB opens; default to
# /private/tmp on macOS unless the caller pins EE_E2E_TMPDIR.
ROOT_BASE="${EE_E2E_TMPDIR:-/private/tmp}"
ROOT="$(mktemp -d "${ROOT_BASE%/}/ee-read-coalescing-e2e.XXXXXX")"
WORKSPACE="${ROOT}/workspace"
HOME_DIR="${ROOT}/home"
LOG_DIR="${ROOT}/logs"
EVENT_LOG="${LOG_DIR}/events.jsonl"
mkdir -p "${WORKSPACE}" "${HOME_DIR}" "${LOG_DIR}"
: >"${EVENT_LOG}"

TEST_ID="read_coalescing_e2e"
BEAD="bd-d67os.10"
FAILURES=0
STEP=0
LAST_STDOUT_FILE=""
LAST_EXIT=0

BURST_IDENTICAL=8
BURST_DISTINCT=3

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
        '{bead_id:$bead,surface:"read_coalescing_e2e",label:$label,detail:$detail,first_failure_diagnosis:$detail,redaction_status:"local_workspace_artifacts_retained"}')"
    printf '[FAIL] %s: %s\n' "${label}" "${detail}" >&2
}

record_pass() {
    local label="${1:?label required}"
    emit_event "assert_ok" "$(jq -cn --arg bead "${BEAD}" --arg label "${label}" \
        '{bead_id:$bead,surface:"read_coalescing_e2e",label:$label,redaction_status:"local_workspace_artifacts_retained"}')"
}

run_ee() {
    local label="${1:?label required}"
    shift
    local outfile errfile
    outfile="${LOG_DIR}/step_$(printf '%02d' "$((STEP + 1))")_${label//[^A-Za-z0-9_]/_}.json"
    errfile="${outfile%.json}.stderr.txt"
    log_step "${label}"
    set +e
    env HOME="${HOME_DIR}" NO_COLOR=1 "${REAL_EE}" --workspace "${WORKSPACE}" "$@" >"${outfile}" 2>"${errfile}"
    LAST_EXIT=$?
    set -e
    LAST_STDOUT_FILE="${outfile}"
    emit_event "command_end" "$(jq -cn --arg bead "${BEAD}" --arg label "${label}" \
        --argjson exitCode "${LAST_EXIT}" --arg stdout "${outfile}" --arg stderr "${errfile}" \
        '{bead_id:$bead,surface:"read_coalescing_e2e",label:$label,exit_code:$exitCode,stdout_artifact_path:$stdout,stderr_artifact_path:$stderr,redaction_status:"local_workspace_artifacts_retained"}')"
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

finish() {
    emit_event "note" "$(jq -cn --arg bead "${BEAD}" --arg logDir "${LOG_DIR}" --argjson failures "${FAILURES}" \
        '{bead_id:$bead,surface:"read_coalescing_e2e",label:"summary",failures:$failures,log_dir:$logDir,redaction_status:"local_workspace_artifacts_retained"}')"
    printf 'read_coalescing_e2e: failures=%d log_dir=%s\n' "${FAILURES}" "${LOG_DIR}" >&2
    if [[ "${FAILURES}" -gt 0 ]]; then exit 1; fi
}
trap finish EXIT

emit_event "note" "$(jq -cn --arg bead "${BEAD}" --arg ee "${REAL_EE}" --arg workspace "${WORKSPACE}" --arg logDir "${LOG_DIR}" \
    '{bead_id:$bead,surface:"read_coalescing_e2e",label:"start",ee_bin:$ee,workspace:$workspace,log_dir:$logDir,redaction_status:"local_workspace_artifacts_retained"}')"

run_ee "init_workspace" init --json
assert_jq_file "${LAST_STDOUT_FILE}" '.success' "true" "init succeeds"

# --- Coalescing burst: N identical + D distinct concurrent requests, one process.
run_ee "coalescing_burst" graph feature-enrichment --dry-run \
    --singleflight-burst "${BURST_IDENTICAL}" --singleflight-distinct "${BURST_DISTINCT}" --json
burst_file="${LAST_STDOUT_FILE}"
assert_jq_file "${burst_file}" '.schema' "ee.response.v2" "burst envelope schema"
assert_jq_file "${burst_file}" '.success' "true" "burst harness passes"
assert_jq_file "${burst_file}" '.data.command' "graph feature-enrichment --singleflight-burst" "burst command stable"
assert_jq_file "${burst_file}" '.data.requested.identical' "${BURST_IDENTICAL}" "requested identical count echoed"
assert_jq_file "${burst_file}" '.data.requested.distinct' "${BURST_DISTINCT}" "requested distinct count echoed"

# (1) Identical concurrent requests collapse to ONE leader computation.
assert_jq_file "${burst_file}" '.data.summary.identicalLeaderCount' "1" "identical requests share exactly one leader"
# (2) Every other identical caller coalesced as a follower (leader_count==1, follower_count>0).
assert_jq_file "${burst_file}" '.data.summary.identicalFollowerCount' "$((BURST_IDENTICAL - 1))" "identical followers all coalesced"
assert_jq_file "${burst_file}" '(.data.summary.identicalFollowerCount > 0)' "true" "at least one follower coalesced"
# (3) All coalesced callers observed a result-identical value.
assert_jq_file "${burst_file}" '.data.resultHashes.identicalUniqueCount' "1" "coalesced callers share one result hash"
# (4) No follower timeouts / leader failures / poisoned state on the happy path.
assert_jq_file "${burst_file}" '.data.summary.timeoutCount' "0" "no follower timeouts"
assert_jq_file "${burst_file}" '.data.summary.leaderFailureCount' "0" "no leader failures"
assert_jq_file "${burst_file}" '.data.summary.statePoisonedCount' "0" "no poisoned single-flight state"

# (5) Distinct keys (a bumped workspace/graph generation lands in the key) BUST
# coalescing: each distinct request runs its own leader computation.
assert_jq_file "${burst_file}" '.data.summary.distinctLeaderCount' "${BURST_DISTINCT}" "distinct keys each get their own leader (coalescing busted)"
assert_jq_file "${burst_file}" '.data.summary.distinctFollowerCount' "0" "distinct keys do not coalesce into followers"
assert_jq_file "${burst_file}" '.data.resultHashes.distinctUniqueCount' "${BURST_DISTINCT}" "distinct keys produce distinct result hashes"

# Execution accounting: exactly one identical computation plus one per distinct key.
assert_jq_file "${burst_file}" '.data.summary.executionCount' "$((1 + BURST_DISTINCT))" "execution count == 1 identical + per-distinct computations"

burst_summary="$(jq -c '.data.summary' "${burst_file}" 2>/dev/null || printf '{}')"
emit_event "coalescing_observed" "$(jq -cn --arg bead "${BEAD}" --argjson summary "${burst_summary:-{}}" \
    '{bead_id:$bead,surface:"read_coalescing_e2e",label:"burst_summary",summary:$summary,redaction_status:"local_workspace_artifacts_retained"}')"

# --- Determinism: a second identical burst produces the same coalescing posture.
run_ee "coalescing_burst_replay" graph feature-enrichment --dry-run \
    --singleflight-burst "${BURST_IDENTICAL}" --singleflight-distinct "${BURST_DISTINCT}" --json
replay_file="${LAST_STDOUT_FILE}"
assert_jq_file "${replay_file}" '.success' "true" "replay burst passes"
first_posture="$(jq -S -c '{summary:.data.summary, resultHashes:.data.resultHashes}' "${burst_file}" 2>/dev/null || printf '')"
replay_posture="$(jq -S -c '{summary:.data.summary, resultHashes:.data.resultHashes}' "${replay_file}" 2>/dev/null || printf '')"
if [[ -n "${first_posture}" && "${first_posture}" == "${replay_posture}" ]]; then
    record_pass "coalescing posture is deterministic across runs"
else
    record_failure "coalescing posture is deterministic across runs" "first=${first_posture} replay=${replay_posture}"
fi
