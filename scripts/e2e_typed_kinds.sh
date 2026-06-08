#!/usr/bin/env bash
# bd-1n0np.12.5 — Typed memory kinds end-to-end coverage.
#
# Scenario:
#   1. init a temp workspace.
#   2. remember a failure whose body contains explicit typed markers.
#   3. prove extraction via --kind plus --field searches for family, cause, and
#      reverted_at_sha.
#   4. remember a decision that supersedes an existing memory and prove the
#      projection-time typed graph edge through graph path.
#   5. remember a bare failure body and prove it remains untyped by showing the
#      typed field filter does not match it.
#
# No set -e: harness assertions accumulate failures and harness_summary owns
# the exit code.
set -uo pipefail

E2E_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/e2e_lib.sh
# shellcheck disable=SC1091
source "$E2E_DIR/e2e_lib.sh"

harness_init "typed_kinds"

ee_json() {
    e2e_log_command "$EE_BIN" "$@" || true
}

json_value() {
    local json="$1" filter="$2"
    printf '%s' "$json" | jq -r "$filter" 2>/dev/null || true
}

memory_id_from_remember() {
    json_value "$1" '.data.memoryId // .data.memory_id // empty'
}

assert_search_returns_memory() {
    local json="$1" memory_id="$2" label="$3"
    local result
    result="$(printf '%s' "$json" \
        | jq -e --arg memory_id "$memory_id" \
            '[.data.results[]? | select((.memoryId // .docId) == $memory_id)] | length == 1' \
            >/dev/null 2>&1 && printf true || printf false)"
    e2e_log_assert_eq "$result" "true" "$label" || true
    if [ "$result" = "true" ]; then
        _harness_pass "$label"
    else
        _harness_fail "$label: search results did not contain $memory_id"
    fi
}

assert_search_omits_memory() {
    local json="$1" memory_id="$2" label="$3"
    local result
    result="$(printf '%s' "$json" \
        | jq -e --arg memory_id "$memory_id" \
            '[.data.results[]? | select((.memoryId // .docId) == $memory_id)] | length == 0' \
            >/dev/null 2>&1 && printf true || printf false)"
    e2e_log_assert_eq "$result" "true" "$label" || true
    if [ "$result" = "true" ]; then
        _harness_pass "$label"
    else
        _harness_fail "$label: search results unexpectedly contained $memory_id"
    fi
}

with_temp_workspace WS

step "init typed-kinds workspace"
init_out="$(ee_json --workspace "$WS" init --json)"
assert_jq "$init_out" '.success == true' "ee init succeeds"

step "remember failure with extraction-first body markers"
failure_body="Tried page-level cache prefetch. Result: -8% on small-N reads. Reverted at SHA 9af3c21. Family: aggressive prefetch. Cause: cache pollution. Regression surface: small-N reads."
failure_out="$(ee_json --workspace "$WS" remember "$failure_body" \
    --level episodic --kind failure \
    --tags negative-evidence,prefetch \
    --source "bench-run://typed-kinds/failure-prefetch" \
    --json)"
assert_jq "$failure_out" '.success == true and .data.kind == "failure" and .data.persisted == true' \
    "failure memory persisted"
failure_id="$(memory_id_from_remember "$failure_out")"
assert_jq "$failure_out" '.data.memoryId != null or .data.memory_id != null' "failure memory id returned"
e2e_log_note "typed_fields memory=$failure_id kind=failure family=aggressive prefetch cause=cache pollution reverted_at_sha=9af3c21"

step "search filters prove extracted failure typed fields"
family_search="$(ee_json --workspace "$WS" search "prefetch regression" \
    --kind failure --field "family=aggressive prefetch" --json)"
assert_jq "$family_search" '.success == true' "family field search succeeds"
assert_search_returns_memory "$family_search" "$failure_id" "family field search returns the failure"

cause_search="$(ee_json --workspace "$WS" search "cache pollution" \
    --kind failure --field "cause=cache pollution" --json)"
assert_jq "$cause_search" '.success == true' "cause field search succeeds"
assert_search_returns_memory "$cause_search" "$failure_id" "cause field search returns the failure"

revert_search="$(ee_json --workspace "$WS" search "reverted prefetch" \
    --kind failure --field "reverted-at-sha=9af3c21" --json)"
assert_jq "$revert_search" '.success == true' "reverted_at_sha field search succeeds"
assert_search_returns_memory "$revert_search" "$failure_id" "reverted_at_sha field search returns the failure"

step "bare --kind failure body remains untyped"
bare_body="A plain failure note with no field labels or structured markers."
bare_out="$(ee_json --workspace "$WS" remember "$bare_body" \
    --level episodic --kind failure \
    --source "bench-run://typed-kinds/bare-failure" \
    --json)"
assert_jq "$bare_out" '.success == true and .data.kind == "failure" and .data.persisted == true' \
    "bare failure memory persisted"
bare_id="$(memory_id_from_remember "$bare_out")"
bare_search="$(ee_json --workspace "$WS" search "plain failure note" \
    --kind failure --field "family=aggressive prefetch" --json)"
assert_search_omits_memory "$bare_search" "$bare_id" \
    "bare failure is not fabricated into the family field"
e2e_log_note "typed_fields memory=$bare_id kind=failure bare_body=no_typed_fields_expected"

step "decision supersedes field creates a typed graph projection edge"
base_decision_out="$(ee_json --workspace "$WS" remember \
    "Decision: keep local cache as the baseline for typed graph edge tests." \
    --level semantic --kind decision \
    --source "decision://typed-kinds/base" \
    --json)"
assert_jq "$base_decision_out" '.success == true and .data.kind == "decision" and .data.persisted == true' \
    "base decision memory persisted"
base_decision_id="$(memory_id_from_remember "$base_decision_out")"

superseding_body="Options: local cache or RCH remote. Chosen: RCH remote. Rationale: avoids local Cargo. Supersedes: ${base_decision_id}."
decision_out="$(ee_json --workspace "$WS" remember "$superseding_body" \
    --level semantic --kind decision \
    --source "decision://typed-kinds/superseding" \
    --json)"
assert_jq "$decision_out" '.success == true and .data.kind == "decision" and .data.persisted == true' \
    "superseding decision memory persisted"
decision_id="$(memory_id_from_remember "$decision_out")"
e2e_log_note "typed_fields memory=$decision_id kind=decision chosen=RCH remote supersedes=$base_decision_id"

decision_search="$(ee_json --workspace "$WS" search "RCH remote decision" \
    --kind decision --field "chosen=RCH remote" --json)"
assert_jq "$decision_search" '.success == true' "decision chosen field search succeeds"
assert_search_returns_memory "$decision_search" "$decision_id" "decision chosen field search returns the superseding decision"

path_out="$(ee_json --workspace "$WS" graph path "$decision_id" "$base_decision_id" --json)"
assert_jq "$path_out" '.success == true' "graph path command succeeds"
if printf '%s' "$path_out" | jq -e '.data.status == "path_found" and .data.pathLength == 1' >/dev/null 2>&1; then
    _harness_pass "typed supersedes graph projection creates a one-edge path"
else
    _harness_fail "typed supersedes graph projection missing one-edge path"
fi

step "persisted contradiction snapshot and export include typed failure-family edge"
family_peer_out="$(ee_json --workspace "$WS" remember \
    "Second failure in the same family. Family: aggressive prefetch. Cause: cache pollution. Regression surface: small-N reads." \
    --level episodic --kind failure \
    --source "bench-run://typed-kinds/failure-prefetch-peer" \
    --json)"
assert_jq "$family_peer_out" '.success == true and .data.kind == "failure" and .data.persisted == true' \
    "failure peer memory persisted"
family_peer_id="$(memory_id_from_remember "$family_peer_out")"

family_path_out="$(ee_json --workspace "$WS" graph path "$failure_id" "$family_peer_id" --json)"
assert_jq "$family_path_out" '.success == true and .data.status == "path_found" and .data.pathLength == 1' \
    "live graph path sees typed failure-family edge"

refresh_out="$(ee_json --workspace "$WS" graph snapshot refresh --graph contradictions --json)"
assert_jq "$refresh_out" '.success == true' "contradiction snapshot refresh succeeds"
assert_jq "$refresh_out" \
    'any(.data.reports[]?; .graphType == "contradiction_subgraph" and .status == "refreshed" and .graph.nodeCount >= 2 and .graph.edgeCount >= 2 and .snapshot.graphType == "contradiction_subgraph")' \
    "contradiction snapshot persists typed failure-family edge"

export_out="$(ee_json --workspace "$WS" graph export --graph-type contradiction_subgraph --json)"
assert_jq "$export_out" '.success == true and .data.status == "exported" and .data.graphType == "contradiction_subgraph" and .data.graph.edgeCount >= 2' \
    "contradiction graph export uses persisted typed-edge snapshot"
if printf '%s' "$export_out" | jq -e '(.data.artifact.content // "") | contains("failure_family")' >/dev/null 2>&1; then
    _harness_pass "contradiction graph export labels typed failure-family edge"
else
    _harness_fail "contradiction graph export did not label typed failure-family edge"
fi

end_temp_workspace
summary_rc=0
harness_summary || summary_rc=$?
printf 'Artifacts: %s\n' "$LOG_DIR" >&2
exit "$summary_rc"
