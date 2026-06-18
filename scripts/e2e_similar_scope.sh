#!/usr/bin/env bash
# bd-2vq2z.25 - real-binary regression: `ee similar` must NOT bypass workspace /
# `--memory-scope` filtering for its SEED memory.
#
# Repro shape from the review bead: one database holding memories from two
# workspaces A and B. `ee similar --workspace A <seed-from-B>` must NOT load the
# B seed globally, must NOT use B's content as the query, and must NOT surface
# B's content or metadata. The seed has to pass the SAME workspace + trust-lane
# (`--memory-scope`) gating every other search candidate passes; an out-of-scope
# seed yields a scoped not-found error, not a leak.
#
# Assertions that FAIL on the pre-fix seed bypass:
#   (neg-1) `ee similar --workspace A <B-seed> --memory-scope self --strict-scope`
#           returns a `not_found` error envelope and the B secret sentinel never
#           appears in stdout (redaction / no-leak honored).
#   (neg-2) Same with the default (swarm) scope: still rejected, because the
#           SEED is in workspace B and A is the active workspace. This catches a
#           stub that only checks the trust lane but skips the workspace
#           predicate.
#   (pos)   `ee similar --workspace B <B-seed>` succeeds and echoes the seed as
#           targetMemoryId -- proving the negatives are scope enforcement, not a
#           broken fixture or unknown id.
#
# Both workspaces share ONE database via EE_DATABASE_PATH; workspace identity is
# driven by the per-invocation `--workspace` path. Artifacts are retained.

set -euo pipefail

if ! command -v jq >/dev/null 2>&1; then
    printf '%s\n' '{"schema":"ee.test_event.v1","test_id":"similar_scope_e2e","kind":"assert_result","fields":{"label":"jq_available","status":"fail","first_failure_diagnosis":"jq executable missing before harness init"}}' >&2
    exit 3
fi

EE_BIN="${EE_BIN:-${EE_BINARY:-ee}}"
if [[ "${EE_BIN}" == */* ]]; then
    REAL_EE="${EE_BIN}"
else
    REAL_EE="$(command -v "${EE_BIN}" 2>/dev/null || true)"
fi
if [[ -z "${REAL_EE}" || ! -x "${REAL_EE}" ]]; then
    printf '%s\n' '{"schema":"ee.test_event.v1","test_id":"similar_scope_e2e","kind":"assert_result","fields":{"label":"ee_binary_available","status":"fail","first_failure_diagnosis":"prebuilt ee binary missing before harness init"}}' >&2
    exit 3
fi

ROOT_BASE="${EE_E2E_TMPDIR:-${TMPDIR:-/tmp}}"
ROOT="$(mktemp -d "${ROOT_BASE%/}/ee-similar-scope-e2e.XXXXXX")"
SHARED_DB="${ROOT}/shared/ee.db"
WS_A="${ROOT}/ws_a"
WS_B="${ROOT}/ws_b"
IDX_A="${ROOT}/idx_a"
IDX_B="${ROOT}/idx_b"
HOME_DIR="${ROOT}/home"
LOG_DIR="${ROOT}/logs"
EVENT_LOG="${LOG_DIR}/events.jsonl"
mkdir -p "${ROOT}/shared" "${WS_A}" "${WS_B}" "${IDX_A}" "${IDX_B}" "${HOME_DIR}" "${LOG_DIR}"
: >"${EVENT_LOG}"

TEST_ID="similar_scope_e2e"
BEAD="bd-2vq2z.25"
FAILURES=0
STEP=0
LAST_STDOUT_FILE=""
LAST_EXIT=0

# A distinctive secret that lives ONLY in the workspace-B seed content. Any
# appearance of it in an A-scoped `ee similar` response is a leak.
B_SECRET="BD2VQ2Z25_WS_B_SECRET_TOKEN_cross_workspace_release_runbook"

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
        '{bead_id:$bead,surface:"similar_scope_e2e",label:$label,detail:$detail,first_failure_diagnosis:$detail,redaction_status:"local_workspace_artifacts_retained"}')"
    printf '[FAIL] %s: %s\n' "${label}" "${detail}" >&2
}

record_pass() {
    local label="${1:?label required}"
    emit_event "assert_ok" "$(jq -cn --arg bead "${BEAD}" --arg label "${label}" \
        '{bead_id:$bead,surface:"similar_scope_e2e",label:$label,redaction_status:"local_workspace_artifacts_retained"}')"
}

# Run ee against the SHARED database. Never fails the harness on a non-zero exit
# (the negative cases expect a not_found exit); callers assert explicitly.
run_ee() {
    local label="${1:?label required}"
    shift
    local outfile errfile
    outfile="${LOG_DIR}/step_$(printf '%02d' "$((STEP + 1))")_${label//[^A-Za-z0-9_]/_}.json"
    errfile="${outfile%.json}.stderr.txt"
    log_step "${label}"
    set +e
    env HOME="${HOME_DIR}" NO_COLOR=1 EE_DATABASE_PATH="${SHARED_DB}" "${REAL_EE}" "$@" >"${outfile}" 2>"${errfile}"
    LAST_EXIT=$?
    set -e
    LAST_STDOUT_FILE="${outfile}"
    emit_event "command_end" "$(jq -cn --arg bead "${BEAD}" --arg label "${label}" \
        --argjson exitCode "${LAST_EXIT}" --arg stdout "${outfile}" --arg stderr "${errfile}" \
        '{bead_id:$bead,surface:"similar_scope_e2e",label:$label,exit_code:$exitCode,stdout_artifact_path:$stdout,stderr_artifact_path:$stderr,redaction_status:"local_workspace_artifacts_retained"}')"
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

assert_success_zero() {
    local label="${1:?label required}"
    if [[ "${LAST_EXIT}" -eq 0 ]]; then
        record_pass "${label} exit 0"
    else
        record_failure "${label} exit 0" "exit=${LAST_EXIT} file=${LAST_STDOUT_FILE}"
    fi
}

# Assert a needle is ABSENT from a file (no content leak / redaction honored).
assert_absent() {
    local file="${1:?file required}"
    local needle="${2:?needle required}"
    local label="${3:?label required}"
    if grep -Fq -- "${needle}" "${file}"; then
        record_failure "${label}" "out-of-scope content leaked: '${needle}' found in ${file}"
    else
        record_pass "${label}"
    fi
}

remember_capture_id() {
    local __id_var="${1:?id var required}"
    local label="${2:?label required}"
    local workspace="${3:?workspace required}"
    local content="${4:?content required}"
    run_ee "${label}" --workspace "${workspace}" remember "${content}" \
        --level semantic --kind fact --tags "${BEAD},similar,scope" --json
    assert_success_zero "${label}"
    assert_jq_file "${LAST_STDOUT_FILE}" '.success' "true" "${label} succeeds"
    local mid
    mid="$(jq -r '(.data.memory_id // .data.memoryId // "")' "${LAST_STDOUT_FILE}")"
    if [[ -n "${mid}" && "${mid}" != "null" ]]; then
        record_pass "${label} returns memory id"
    else
        record_failure "${label} returns memory id" "no memory id in ${LAST_STDOUT_FILE}"
    fi
    printf -v "${__id_var}" '%s' "${mid}"
}

finish() {
    emit_event "note" "$(jq -cn --arg bead "${BEAD}" --arg logDir "${LOG_DIR}" --argjson failures "${FAILURES}" \
        '{bead_id:$bead,surface:"similar_scope_e2e",label:"summary",failures:$failures,log_dir:$logDir,redaction_status:"local_workspace_artifacts_retained"}')"
    printf 'similar_scope_e2e: failures=%d log_dir=%s\n' "${FAILURES}" "${LOG_DIR}" >&2
    if [[ "${FAILURES}" -gt 0 ]]; then exit 1; fi
}
trap finish EXIT

emit_event "note" "$(jq -cn --arg bead "${BEAD}" --arg ee "${REAL_EE}" --arg db "${SHARED_DB}" --arg logDir "${LOG_DIR}" \
    '{bead_id:$bead,surface:"similar_scope_e2e",label:"start",ee_bin:$ee,shared_db:$db,log_dir:$logDir,redaction_status:"local_workspace_artifacts_retained"}')"

# --- Fixture: two workspaces (A, B) registered into one shared database.
run_ee "init_workspace_a" --workspace "${WS_A}" init --json
assert_success_zero "init workspace A"
assert_jq_file "${LAST_STDOUT_FILE}" '.success' "true" "init workspace A succeeds"

run_ee "init_workspace_b" --workspace "${WS_B}" init --json
assert_success_zero "init workspace B"
assert_jq_file "${LAST_STDOUT_FILE}" '.success' "true" "init workspace B succeeds"

# Seed lives in workspace B and carries the secret sentinel.
B_SEED_ID=""
remember_capture_id B_SEED_ID "remember_workspace_b_seed" "${WS_B}" \
    "${B_SECRET} workspace B release runbook with cross-workspace deployment steps that must stay scoped to B."
# A second B memory so an in-workspace similar query has a real neighbor pool.
remember_capture_id _B_NEIGHBOR_ID "remember_workspace_b_neighbor" "${WS_B}" \
    "Workspace B follow-up note about the same release runbook and deployment cadence."
# Workspace A gets its own unrelated memory + index so A is a live workspace.
remember_capture_id _A_MEMORY_ID "remember_workspace_a_memory" "${WS_A}" \
    "Workspace A onboarding checklist covering editor setup and local tooling, unrelated to release runbooks."

run_ee "index_rebuild_a" --workspace "${WS_A}" index rebuild --index-dir "${IDX_A}" --json
assert_success_zero "index rebuild A"
assert_jq_file "${LAST_STDOUT_FILE}" '.success' "true" "index rebuild A succeeds"

run_ee "index_rebuild_b" --workspace "${WS_B}" index rebuild --index-dir "${IDX_B}" --json
assert_success_zero "index rebuild B"
assert_jq_file "${LAST_STDOUT_FILE}" '.success' "true" "index rebuild B succeeds"

if [[ -z "${B_SEED_ID}" || "${B_SEED_ID}" == "null" ]]; then
    record_failure "workspace B seed id captured" "cannot run scope regression without a B seed id"
else
    # --- neg-1: A workspace, strict self-scope -> must reject the B seed, no leak.
    run_ee "similar_a_scope_self_strict" --workspace "${WS_A}" similar "${B_SEED_ID}" \
        --memory-scope self --strict-scope --index-dir "${IDX_A}" --json
    neg1="${LAST_STDOUT_FILE}"
    if [[ "${LAST_EXIT}" -ne 0 ]]; then
        record_pass "cross-workspace strict-self similar fails closed (non-zero exit)"
    else
        record_failure "cross-workspace strict-self similar fails closed (non-zero exit)" \
            "expected non-zero exit for out-of-scope seed, got exit=0 file=${neg1}"
    fi
    assert_jq_file "${neg1}" '.error.code' "not_found" "cross-workspace strict-self similar returns not_found error"
    assert_jq_file "${neg1}" '(.success // false)' "false" "cross-workspace strict-self similar is not a success envelope"
    assert_absent "${neg1}" "${B_SECRET}" "neg-1 does not leak workspace-B seed content"
    # The B seed must never appear as a returned neighbor either.
    assert_jq_file "${neg1}" "((.data.results // []) | map(select(.memoryId == \"${B_SEED_ID}\")) | length)" "0" "neg-1 does not return the B seed as a neighbor"

    # --- neg-2: A workspace, default (swarm) scope -> still rejected (workspace gate).
    run_ee "similar_a_scope_default" --workspace "${WS_A}" similar "${B_SEED_ID}" \
        --index-dir "${IDX_A}" --json
    neg2="${LAST_STDOUT_FILE}"
    if [[ "${LAST_EXIT}" -ne 0 ]]; then
        record_pass "cross-workspace default-scope similar fails closed (non-zero exit)"
    else
        record_failure "cross-workspace default-scope similar fails closed (non-zero exit)" \
            "expected non-zero exit for out-of-workspace seed, got exit=0 file=${neg2}"
    fi
    assert_jq_file "${neg2}" '.error.code' "not_found" "cross-workspace default-scope similar returns not_found error"
    assert_absent "${neg2}" "${B_SECRET}" "neg-2 does not leak workspace-B seed content"

    # --- pos: B workspace, default scope -> seed admitted in its own workspace.
    run_ee "similar_b_scope_default" --workspace "${WS_B}" similar "${B_SEED_ID}" \
        --index-dir "${IDX_B}" --json
    pos="${LAST_STDOUT_FILE}"
    assert_success_zero "in-workspace similar"
    assert_jq_file "${pos}" '.schema' "ee.response.v2" "in-workspace similar returns success envelope"
    assert_jq_file "${pos}" '.success' "true" "in-workspace similar succeeds"
    assert_jq_file "${pos}" '.data.targetMemoryId' "${B_SEED_ID}" "in-workspace similar echoes the B seed as target"
fi
