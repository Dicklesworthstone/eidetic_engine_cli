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

# --- graph diff with no snapshots reports the honest missing code --------
run_ee graph diff --workspace "${WS}" --json
if [[ "${LAST_EXIT}" -eq 0 ]] \
    && jq -e '[.degraded[]? | select(.code == "graph_diff_snapshot_missing")] | length >= 1' \
        "${LAST_STDOUT}" >/dev/null 2>&1; then
    event diff_without_snapshots_reports_missing pass
else
    event diff_without_snapshots_reports_missing fail "exit ${LAST_EXIT}; $(head -c 250 "${LAST_STDOUT}")"
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

# --- conflict resolve (bd-3a1op.4): dry-run plan, apply, stale refusal ---
# The applied contradiction-review candidate created a contradicts link
# between a and b, so the pair is now on the conflict surface.
run_ee conflict list --workspace "${WS}" --json
RESOLVE_PAIR_PRESENT="$(jq -r --arg a "${MEM_A}" --arg b "${MEM_B}" \
    '[.data.pairs[]? | select((.memoryA.id == $a and .memoryB.id == $b) or (.memoryA.id == $b and .memoryB.id == $a))] | length' \
    "${LAST_STDOUT}" 2>/dev/null)"
if [[ "${RESOLVE_PAIR_PRESENT:-0}" -ge 1 ]]; then
    event conflict_surface_lists_pair pass
else
    event conflict_surface_lists_pair fail "pair absent: $(head -c 250 "${LAST_STDOUT}")"
fi

run_ee conflict resolve "${MEM_A}" "${MEM_B}" --verb supersede --keep "${MEM_A}" \
    --reason "always-rule wins; never-rule superseded in e2e" --workspace "${WS}" --json
if [[ "${LAST_EXIT}" -eq 0 ]] \
    && jq -e '.data.dryRun == true and .data.status == "planned" and (.data.plan.actions | length) >= 1' \
        "${LAST_STDOUT}" >/dev/null 2>&1; then
    event resolve_dry_run_plans_without_mutation pass
else
    event resolve_dry_run_plans_without_mutation fail "exit ${LAST_EXIT}; $(head -c 250 "${LAST_STDOUT}")"
fi

run_ee conflict resolve "${MEM_A}" "${MEM_B}" --verb supersede --keep "${MEM_A}" \
    --reason "always-rule wins; never-rule superseded in e2e" --apply --workspace "${WS}" --json
DECISION_MEMORY="$(jq -r '[.data.results[]? | select(.createdMemoryId != null)][0].createdMemoryId // empty' "${LAST_STDOUT}" 2>/dev/null)"
if [[ "${LAST_EXIT}" -eq 0 && -n "${DECISION_MEMORY}" ]] \
    && jq -e '.data.status == "applied" and ([.data.results[]?.auditIds[]?] | length) >= 2' \
        "${LAST_STDOUT}" >/dev/null 2>&1; then
    event resolve_apply_supersede_audited pass
else
    event resolve_apply_supersede_audited fail "exit ${LAST_EXIT}; decision=${DECISION_MEMORY}; $(head -c 250 "${LAST_STDOUT}")"
fi

# The loser is expired, so the same pair is no longer resolvable: stale refusal.
run_ee conflict resolve "${MEM_A}" "${MEM_B}" --verb supersede --keep "${MEM_A}" \
    --reason "re-run" --workspace "${WS}" --json
if [[ "${LAST_EXIT}" -ne 0 ]] && grep -q 'conflict_resolve_stale_surface' "${LAST_STDOUT}" "${LOG_DIR}/step${STEP}.stderr"; then
    event resolved_pair_rerun_refused_stale pass
else
    event resolved_pair_rerun_refused_stale fail "exit ${LAST_EXIT}; $(head -c 250 "${LAST_STDOUT}")"
fi

# --- graph diff between two real snapshots (bd-3a1op.5) ------------------
# Snapshot the pre-resolve-era graph, grow it, snapshot again, then diff.
run_ee graph snapshot refresh --graph memory_links --workspace "${WS}" --json
SNAP1_EXIT=$LAST_EXIT
run_ee remember "Diff-era memory: the snapshot family gained a node." \
    --workspace "${WS}" --level semantic --kind fact --tags "diff" --json
MEM_D="$(remember_id)"
run_ee memory link "${MEM_C}" "${MEM_D}" --relation related --workspace "${WS}" --json
LINK_CD=$LAST_EXIT
run_ee graph snapshot refresh --graph memory_links --workspace "${WS}" --json
SNAP2_EXIT=$LAST_EXIT
if [[ "${SNAP1_EXIT}" -eq 0 && "${LINK_CD}" -eq 0 && "${SNAP2_EXIT}" -eq 0 && -n "${MEM_D}" ]]; then
    event diff_snapshots_prepared pass
else
    event diff_snapshots_prepared fail "snap1=${SNAP1_EXIT} link=${LINK_CD} snap2=${SNAP2_EXIT} d=${MEM_D}"
fi

run_ee graph diff --graph memory_links --workspace "${WS}" --json
if [[ "${LAST_EXIT}" -eq 0 ]] \
    && jq -e --arg d "${MEM_D}" \
        '.data.report.schema == "ee.graph.diff.v1"
         and (.data.report.summary.edgesAdded >= 1)
         and ([.data.report.nodesAdded[]? | select(. == $d)] | length == 1)
         and (.data.report.detailCap >= 1)' \
        "${LAST_STDOUT}" >/dev/null 2>&1; then
    event diff_reports_planted_growth pass
else
    event diff_reports_planted_growth fail "exit ${LAST_EXIT}; $(head -c 300 "${LAST_STDOUT}")"
fi

echo
echo "graph intel e2e: ${STEP} steps, ${FAILURES} failures; root ${ROOT}"
if [[ "${FAILURES}" -gt 0 ]]; then
    exit 2
fi
exit 0
