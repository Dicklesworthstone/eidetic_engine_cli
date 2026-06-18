#!/usr/bin/env bash
# bd-2vq2z.14 - Health scorecard e2e (real binary, no mocks).
#
# Proves `ee health scorecard --json` returns the stable scorecard schema,
# reads existing memory-debt snapshots for trend, and declines after introducing
# duplicate, low-provenance memory debt. The script intentionally does not
# build; central RCH verify provides the binary.
set -uo pipefail

E2E_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

if [ -d /private/tmp ]; then
    EE_E2E_TMPDIR="${EE_E2E_TMPDIR:-/private/tmp}"
    export EE_E2E_TMPDIR
fi

# shellcheck source=scripts/e2e_lib.sh
# shellcheck disable=SC1091
source "$E2E_DIR/e2e_lib.sh"

harness_init "health_scorecard"

ee_json() {
    e2e_log_command "$EE_BIN" "$@" || true
}

json_scalar() {
    local json="${1:?json required}"
    local filter="${2:?jq filter required}"
    printf '%s' "$json" | jq -r "$filter" 2>/dev/null || true
}

assert_number_lt() {
    local actual="$1"
    local upper="$2"
    local label="$3"
    local result
    result="$(
        python3 - "$actual" "$upper" <<'PY'
import sys
try:
    actual = float(sys.argv[1])
    upper = float(sys.argv[2])
except Exception:
    print("false")
else:
    print("true" if actual < upper else "false")
PY
    )"
    e2e_log_assert_eq "$result" "true" "$label"
    if [ "$result" = "true" ]; then
        _harness_pass "$label ($actual < $upper)"
    else
        _harness_fail "$label: expected $actual < $upper"
    fi
}

with_temp_workspace WS

step "init isolated workspace"
init_out="$(ee_json init --workspace "$WS" --json)"
assert_jq "$init_out" '.schema == "ee.response.v2" and .success == true' \
    "ee init returns a success response envelope"
log_event "health_scorecard_workspace" \
    workspaceHash "$(printf '%s' "$WS" | shasum -a 256 | awk '{print $1}')" \
    bead "bd-2vq2z.14"

step "seed one sourced baseline memory"
mkdir -p "$WS/docs"
printf '%s\n' "health scorecard baseline evidence" >"$WS/docs/scorecard.md"
baseline_remember="$(ee_json remember \
    "health scorecard baseline evidence" \
    --workspace "$WS" --level procedural --kind rule \
    --source "file://docs/scorecard.md#L1-L1" --json)"
assert_jq "$baseline_remember" '.schema == "ee.response.v2" and .success == true' \
    "baseline remember succeeds"
baseline_mem="$(json_scalar "$baseline_remember" '.data.memory_id // .data.memoryId // empty')"
assert_eq "$([ -n "$baseline_mem" ] && echo present || echo missing)" "present" \
    "baseline remember returns memory id"
log_event "health_scorecard_seed" memoryId "$baseline_mem" phase "baseline"

step "record baseline trend snapshot through scorecard"
baseline_scorecard="$(ee_json health scorecard --workspace "$WS" --record-snapshot --json)"
assert_jq "$baseline_scorecard" '.schema == "ee.response.v2" and .success == true' \
    "baseline scorecard returns success envelope"
assert_jq "$baseline_scorecard" '.data.schema == "ee.health_scorecard.v1"' \
    "baseline scorecard emits health scorecard schema"
assert_jq "$baseline_scorecard" '.data.snapshot.requested == true and (.data.snapshot.status | type == "string")' \
    "baseline scorecard records or recognizes a trend snapshot"
assert_jq "$baseline_scorecard" '(.data.subScores | length) >= 5 and (.data.topActions | type == "array")' \
    "baseline scorecard includes subscores and top actions"
baseline_score="$(json_scalar "$baseline_scorecard" '.data.score')"
assert_eq "$([ -n "$baseline_score" ] && echo present || echo missing)" "present" \
    "baseline score is present"
log_event "health_scorecard_baseline" score "$baseline_score" snapshotStatus "$(json_scalar "$baseline_scorecard" '.data.snapshot.status')"

step "introduce duplicate low-provenance memory debt"
for idx in 1 2 3 4 5 6; do
    dup_out="$(ee_json remember \
        "duplicate scorecard regression payload" \
        --workspace "$WS" --level episodic --kind fact \
        --confidence 0.2 --json)"
    assert_jq "$dup_out" '.schema == "ee.response.v2" and .success == true' \
        "duplicate remember $idx succeeds"
    dup_mem="$(json_scalar "$dup_out" '.data.memory_id // .data.memoryId // empty')"
    log_event "health_scorecard_duplicate_seed" index "$idx" memoryId "$dup_mem"
done

step "scorecard declines and prioritizes repair actions"
declined_scorecard="$(ee_json health scorecard --workspace "$WS" --json)"
assert_jq "$declined_scorecard" '.schema == "ee.response.v2" and .success == true' \
    "declined scorecard returns success envelope"
assert_jq "$declined_scorecard" '.data.schema == "ee.health_scorecard.v1"' \
    "declined scorecard emits stable data schema"
assert_jq "$declined_scorecard" '.data.trend.source == "debt_snapshots" and .data.trend.snapshotCount >= 1' \
    "declined scorecard reads the baseline debt snapshot"
assert_jq "$declined_scorecard" '.data.trend.direction == "declining" and .data.trend.delta < 0' \
    "declined scorecard reports a negative trend"
declined_score="$(json_scalar "$declined_scorecard" '.data.score')"
assert_number_lt "$declined_score" "$baseline_score" \
    "declined score is lower than baseline score"
assert_jq "$declined_scorecard" '.data.evidence.exactDuplicateGroupCount >= 1 and .data.evidence.exactDuplicateMemoryCount >= 2' \
    "scorecard evidence counts duplicate memories"
assert_jq "$declined_scorecard" '.data.evidence.missingProvenanceCount >= 1 and .data.evidence.unverifiedProvenanceCount >= 1' \
    "scorecard evidence counts freshness and provenance debt"
assert_jq "$declined_scorecard" '
    any(.data.topActions[]?; .subScore == "redundancy" or .subScore == "freshness" or .subScore == "trust")
' "scorecard top actions target redundancy, freshness, or trust debt"
log_event "health_scorecard_decline" \
    baselineScore "$baseline_score" \
    declinedScore "$declined_score" \
    trend "$(json_scalar "$declined_scorecard" '.data.trend.direction')" \
    duplicateGroups "$(json_scalar "$declined_scorecard" '.data.evidence.exactDuplicateGroupCount')"

step "scorecard is deterministic across identical read-only invocations"
repeat_scorecard="$(ee_json health scorecard --workspace "$WS" --json)"
first_hash="$(printf '%s' "$declined_scorecard" | shasum -a 256 | awk '{print $1}')"
repeat_hash="$(printf '%s' "$repeat_scorecard" | shasum -a 256 | awk '{print $1}')"
assert_eq "$repeat_hash" "$first_hash" \
    "read-only health scorecard output is byte-identical for same DB and query"
log_event "health_scorecard_determinism" firstHash "$first_hash" repeatHash "$repeat_hash"

harness_summary
