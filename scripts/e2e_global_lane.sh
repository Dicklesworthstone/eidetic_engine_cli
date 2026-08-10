#!/usr/bin/env bash
# bd-1bfwa.4 — global knowledge lane E2E (three real workspaces, one
# hermetic user-global store; NO MOCKS).
#
# Covers the deterministic acceptance paths implementable against the
# shipped surface (bd-1bfwa.2/.3):
#   - evidence gate: promoting an under-trusted workspace memory refuses
#     with exit 7 and structured reasons (or, when the memory's trust
#     class is already promotable, the dry-run plan + apply path work and
#     write audit rows) — the branch taken is asserted from the actual
#     trust class, never assumed;
#   - direct global authorship: `ee remember --global` rows surface in a
#     DIFFERENT workspace labeled storeLane=global;
#   - isolation: a workspace with memory.participate=false sees no global
#     rows and reports global_lane_disabled honestly;
#   - demote-global tombstones the row and it stops surfacing.
#
# Environment:
#   EE_BIN / EE_BINARY  Path to the ee binary (default: `ee` on PATH)
#   EE_E2E_TMPDIR       Temp base (default /private/tmp)

set -uo pipefail

TEST_ID="global_lane_e2e"
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
ROOT="$(mktemp -d "${ROOT_BASE%/}/ee-global-lane-e2e.XXXXXX")"
WS_A="${ROOT}/ws_a"
WS_B="${ROOT}/ws_b"
WS_C="${ROOT}/ws_c"
LOG_DIR="${ROOT}/logs"
EVENT_LOG="${LOG_DIR}/events.jsonl"
mkdir -p "${WS_A}" "${WS_B}" "${WS_C}" "${LOG_DIR}"
: >"${EVENT_LOG}"

# Hermetic user-global store: the global tier resolves under XDG_DATA_HOME.
export XDG_DATA_HOME="${ROOT}/xdg"
mkdir -p "${XDG_DATA_HOME}"

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

# --- setup: three workspaces --------------------------------------------
for ws in "${WS_A}" "${WS_B}" "${WS_C}"; do
    run_ee init --workspace "${ws}" --json
    [[ "${LAST_EXIT}" -eq 0 ]] || event "init_$(basename "${ws}")" fail "init exit ${LAST_EXIT}"
done
event workspaces_initialized pass

# --- evidence gate: promote from workspace A ----------------------------
run_ee remember "Always run the schema drift gate before altering public JSON." \
    --workspace "${WS_A}" --level procedural --kind rule --json
MEMORY_A="$(jq -r '.data.memoryId // .data.memory_id // .data.id // empty' "${LAST_STDOUT}")"
if [[ -z "${MEMORY_A}" ]]; then
    event remember_workspace_rule fail "no memory id (exit ${LAST_EXIT})"
else
    event remember_workspace_rule pass
fi

TRUST_A="$(
    run_ee memory show "${MEMORY_A}" --workspace "${WS_A}" --json >/dev/null 2>&1
    jq -r '.. | .trustClass? // .trust_class? // empty' "${LAST_STDOUT}" 2>/dev/null | head -1
)"
run_ee memory promote-global "${MEMORY_A}" --workspace "${WS_A}" --dry-run --json
if [[ "${TRUST_A}" == "human_explicit" || "${TRUST_A}" == "agent_validated" ]]; then
    if [[ "${LAST_EXIT}" -eq 0 ]]; then
        event promote_dry_run_plans pass
    else
        event promote_dry_run_plans fail "promotable class ${TRUST_A} but dry-run exit ${LAST_EXIT}: $(head -c 200 "${LAST_STDOUT}")"
    fi
else
    if [[ "${LAST_EXIT}" -eq 7 ]] && grep -qi 'trust' "${LAST_STDOUT}"; then
        event promote_undervalidated_refuses_exit7 pass
    else
        event promote_undervalidated_refuses_exit7 fail "trust ${TRUST_A:-unknown}; exit ${LAST_EXIT} (wanted 7 + trust-naming reasons): $(head -c 250 "${LAST_STDOUT}")"
    fi
fi

# --- promotable-trust arcs: apply, twin-merge, secret refusal -----------
# These need a promotable trust class on plain `ee remember` rows; the
# branch is taken from the ACTUAL class read back above, mirroring the
# dry-run assertion.
if [[ "${TRUST_A}" == "human_explicit" || "${TRUST_A}" == "agent_validated" ]]; then
    run_ee memory promote-global "${MEMORY_A}" --workspace "${WS_A}" --json
    PROMOTED_GID="$(jq -r '.data.report.globalMemoryId // empty' "${LAST_STDOUT}")"
    if [[ "${LAST_EXIT}" -eq 0 && -n "${PROMOTED_GID}" ]]; then
        event promote_apply_creates_global_row pass
    else
        event promote_apply_creates_global_row fail "exit ${LAST_EXIT}; $(head -c 250 "${LAST_STDOUT}")"
    fi

    # Same content promoted from a DIFFERENT workspace must merge into the
    # existing global row, not create a duplicate.
    run_ee remember "Always run the schema drift gate before altering public JSON." \
        --workspace "${WS_B}" --level procedural --kind rule --json
    MEMORY_B="$(jq -r '.data.memoryId // .data.memory_id // .data.id // empty' "${LAST_STDOUT}")"
    run_ee memory promote-global "${MEMORY_B}" --workspace "${WS_B}" --dry-run --json
    if jq -e '[.. | objects | select(.action? == "merge_into")] | length >= 1' \
        "${LAST_STDOUT}" >/dev/null 2>&1; then
        event twin_promotion_plans_merge_not_duplicate pass
    else
        event twin_promotion_plans_merge_not_duplicate fail "exit ${LAST_EXIT}; no merge_into action: $(head -c 250 "${LAST_STDOUT}")"
    fi

    # A planted secret-like string must refuse promotion outright.
    run_ee remember "Deploy rule: export AWS_SECRET_ACCESS_KEY=AKIAIOSFODNN7EXAMPLEKEY99 before pushing." \
        --workspace "${WS_A}" --level procedural --kind rule --allow-secret-mention --json
    SECRET_MEMORY="$(jq -r '.data.memoryId // .data.memory_id // .data.id // empty' "${LAST_STDOUT}")"
    if [[ -n "${SECRET_MEMORY}" ]]; then
        run_ee memory promote-global "${SECRET_MEMORY}" --workspace "${WS_A}" --dry-run --json
        if [[ "${LAST_EXIT}" -eq 7 ]] && grep -qiE 'redact|secret' "${LAST_STDOUT}"; then
            event secret_promotion_refused pass
        else
            event secret_promotion_refused fail "exit ${LAST_EXIT} (wanted 7 + redaction reasons): $(head -c 250 "${LAST_STDOUT}")"
        fi
    else
        event secret_promotion_refused fail "secret-bearing remember did not store (exit ${LAST_EXIT})"
    fi
else
    event promotable_trust_arcs_skipped pass "trust class ${TRUST_A:-unknown} is not promotable; apply/merge/secret arcs exercised only via the refusal branch above"
fi

# --- direct global authorship + cross-workspace surfacing ---------------
run_ee remember "Global house rule: never git reset --hard in shared trees." \
    --global --level procedural --kind rule --json
GLOBAL_ID="$(jq -r '.data.memoryId // .data.memory_id // .data.id // empty' "${LAST_STDOUT}")"
if [[ "${LAST_EXIT}" -eq 0 && -n "${GLOBAL_ID}" ]]; then
    event remember_global_rule pass
else
    event remember_global_rule fail "exit ${LAST_EXIT}; id=${GLOBAL_ID}"
fi

run_ee search "never git reset hard shared trees" --workspace "${WS_B}" --json
if jq -e '[.data.results[]? | select((.metadata.storeLane // .storeLane // "") == "global")] | length >= 1' \
    "${LAST_STDOUT}" >/dev/null 2>&1; then
    event global_row_surfaces_in_other_workspace pass
else
    event global_row_surfaces_in_other_workspace fail "no storeLane=global hit in ws_b: $(head -c 250 "${LAST_STDOUT}")"
fi

# --- isolation: participate=false sees nothing, honestly ----------------
run_ee config set memory.participate false --workspace "${WS_C}" --json
[[ "${LAST_EXIT}" -eq 0 ]] && event participate_false_set pass \
    || event participate_false_set fail "config set exit ${LAST_EXIT}: $(head -c 200 "${LAST_STDOUT}")"

run_ee search "never git reset hard shared trees" --workspace "${WS_C}" --json
GLOBAL_HITS_C="$(jq -r '[.data.results[]? | select((.metadata.storeLane // .storeLane // "") == "global")] | length' "${LAST_STDOUT}" 2>/dev/null || echo -1)"
HAS_DISABLED_CODE="$(jq -r '[.degraded[]? , .data.degraded[]? | select(.code == "global_lane_disabled")] | length' "${LAST_STDOUT}" 2>/dev/null || echo 0)"
if [[ "${GLOBAL_HITS_C}" == "0" && "${HAS_DISABLED_CODE}" != "0" ]]; then
    event participate_false_isolated_and_honest pass
elif [[ "${GLOBAL_HITS_C}" == "0" ]]; then
    event participate_false_isolated_and_honest fail "isolated but no global_lane_disabled degraded entry"
else
    event participate_false_isolated_and_honest fail "global rows leaked into participate=false workspace (${GLOBAL_HITS_C})"
fi

# --- demote-global tombstones and stops surfacing -----------------------
run_ee memory demote-global "${GLOBAL_ID}" --workspace "${WS_A}" --json
[[ "${LAST_EXIT}" -eq 0 ]] && event demote_global_succeeds pass \
    || event demote_global_succeeds fail "exit ${LAST_EXIT}: $(head -c 200 "${LAST_STDOUT}")"

run_ee search "never git reset hard shared trees" --workspace "${WS_B}" --json
if jq -e '[.data.results[]? | select((.metadata.storeLane // .storeLane // "") == "global")] | length == 0' \
    "${LAST_STDOUT}" >/dev/null 2>&1; then
    event demoted_row_stops_surfacing pass
else
    event demoted_row_stops_surfacing fail "tombstoned global row still surfaces: $(head -c 250 "${LAST_STDOUT}")"
fi

echo
echo "global lane e2e: ${STEP} steps, ${FAILURES} failures; root ${ROOT}"
if [[ "${FAILURES}" -gt 0 ]]; then
    exit 2
fi
exit 0
