#!/usr/bin/env bash
# bd-2vq2z.18 - Trust freshness lifecycle e2e (real binary, no mocks).
#
# This is the cross-track route pin for drift/provenance/calibration trust
# signals. It exercises the current public surfaces for real:
#   - `ee why` provenance freshness degradations for present -> moved -> missing.
#   - `ee verify provenance` audited mutation/candidate accounting.
#   - `ee memory drift` read-only posture after provenance verification.
# Pending public routes (`ee diag provenance`, `ee trust report`) are explicit
# no-silent-cap drops until their CLI/schema files are available to this lane.
#
# NOTE: no `set -e` - assert_* helpers accumulate failures and harness_summary
# decides the exit code, so a single failing assertion cannot prevent artifacts
# and the summary from being written.
set -uo pipefail

E2E_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# Avoid the shared harness's cargo-metadata fallback in code-first swarm lanes.
EE_BIN="${EE_BIN:-ee}"
export EE_BIN
if [ -d /private/tmp ]; then
    EE_E2E_TMPDIR="${EE_E2E_TMPDIR:-/private/tmp}"
    export EE_E2E_TMPDIR
fi

# shellcheck source=scripts/e2e_lib.sh
# shellcheck disable=SC1091
source "$E2E_DIR/e2e_lib.sh"

harness_init "trust_freshness"

ee_json() {
    e2e_log_command "$EE_BIN" "$@" || true
}

ee_supports() {
    "$EE_BIN" "$@" --help >/dev/null 2>&1
}

json_scalar() {
    local json="${1:?json required}"
    local filter="${2:?jq filter required}"
    printf '%s' "$json" | jq -r "$filter" 2>/dev/null || true
}

with_temp_workspace WS

step "seed workspace files for provenance freshness"
mkdir -p "$WS/src" "$WS/docs"
printf 'pub fn trust_probe() -> &'\''static str {\n    "trusted-freshness-v1"\n}\n' \
    >"$WS/src/trust_probe.rs"
printf 'calibration evidence fixture\n' >"$WS/docs/calibration.md"
log_event "trust_freshness_fixture" \
    workspaceHash "$(printf '%s' "$WS" | shasum -a 256 | awk '{print $1}')" \
    source "src/trust_probe.rs" \
    bead "bd-2vq2z.18"

step "init isolated workspace"
init_out="$(ee_json init --workspace "$WS" --json)"
assert_jq "$init_out" '.schema == "ee.response.v2" and .success == true' \
    "ee init returns a success response envelope"

step "remember a file-backed memory for provenance freshness"
remember_out="$(ee_json remember \
    "trusted-freshness-v1" \
    --workspace "$WS" --level episodic --kind fact \
    --source "file://src/trust_probe.rs#L2-L2" --json)"
assert_jq "$remember_out" '.schema == "ee.response.v2" and .success == true' \
    "remember with file provenance succeeds"
mem_id="$(json_scalar "$remember_out" '.data.memory_id // .data.memoryId // empty')"
assert_eq "$([ -n "$mem_id" ] && echo present || echo missing)" "present" \
    "remember returns a memory id"
log_event "trust_freshness_memory" memoryId "$mem_id" provenance "file://src/trust_probe.rs#L2-L2"

step "remember cass-backed provenance; absent verifier is unverifiable, not missing"
cass_out="$(ee_json remember \
    "cass-backed trust freshness pointer" \
    --workspace "$WS" --level episodic --kind fact \
    --source "cass-session://trust-freshness-fixture#L1-L2" --json)"
assert_jq "$cass_out" '.schema == "ee.response.v2" and .success == true' \
    "remember with cass provenance succeeds"
cass_id="$(json_scalar "$cass_out" '.data.memory_id // .data.memoryId // empty')"
assert_eq "$([ -n "$cass_id" ] && echo present || echo missing)" "present" \
    "cass-backed remember returns a memory id"
why_cass="$(ee_json why "$cass_id" --workspace "$WS" --json)"
assert_jq "$why_cass" '.schema == "ee.response.v2" and .success == true' \
    "why cass provenance succeeds"
assert_jq "$why_cass" '
    any((.data.degraded // [])[]?; .code == "why_provenance_freshness_unverifiable")
' "cass-backed provenance is unverifiable when cass verifier is absent"
assert_jq "$why_cass" '
    [(.data.degraded // [])[]? | select(.code == "why_provenance_freshness_missing")]
    | length == 0
' "cass-backed provenance is not misclassified as missing"

step "why reports present provenance with no provenance-freshness degradation"
why_present="$(ee_json why "$mem_id" --workspace "$WS" --json)"
assert_jq "$why_present" '.schema == "ee.response.v2" and .success == true' \
    "why present provenance succeeds"
assert_jq "$why_present" '
    [(.data.degraded // [])[]? | select((.code // "") | startswith("why_provenance_freshness_"))]
    | length == 0
' "why has no provenance freshness degradation while source matches"

step "move the cited file; why must report moved provenance, not silent trust"
mv "$WS/src/trust_probe.rs" "$WS/src/trust_probe_moved.rs"
log_event "trust_freshness_transition" memoryId "$mem_id" transition "moved"
why_moved="$(ee_json why "$mem_id" --workspace "$WS" --json)"
assert_jq "$why_moved" '.schema == "ee.response.v2" and .success == true' \
    "why moved provenance succeeds"
assert_jq "$why_moved" '
    any((.data.degraded // [])[]?; .code == "why_provenance_freshness_moved")
' "why flags moved provenance"

step "restore the file; why returns to present provenance"
mv "$WS/src/trust_probe_moved.rs" "$WS/src/trust_probe.rs"
log_event "trust_freshness_transition" memoryId "$mem_id" transition "restored"
why_restored="$(ee_json why "$mem_id" --workspace "$WS" --json)"
assert_jq "$why_restored" '.schema == "ee.response.v2" and .success == true' \
    "why restored provenance succeeds"
assert_jq "$why_restored" '
    [(.data.degraded // [])[]? | select((.code // "") | startswith("why_provenance_freshness_"))]
    | length == 0
' "why clears provenance freshness degradation after restore"

step "change the cited evidence; why and verify provenance expose drift/missing trust"
printf 'pub fn trust_probe() -> &'\''static str {\n    "trusted-freshness-v2"\n}\n' \
    >"$WS/src/trust_probe.rs"
log_event "trust_freshness_transition" memoryId "$mem_id" transition "content_changed"
why_missing="$(ee_json why "$mem_id" --workspace "$WS" --json)"
assert_jq "$why_missing" '.schema == "ee.response.v2" and .success == true' \
    "why changed provenance succeeds"
assert_jq "$why_missing" '
    any((.data.degraded // [])[]?; .code == "why_provenance_freshness_missing")
' "why flags missing/mismatched provenance"

verify_out="$(ee_json verify provenance --workspace "$WS" --json)"
assert_jq "$verify_out" '.schema == "ee.response.v2" and .success == true' \
    "verify provenance succeeds"
assert_jq "$verify_out" 'any(.data.referents[]?; .memoryId == "'"$mem_id"'" and (.status == "evidence_drift" or .status == "evidence_missing"))' \
    "verify provenance classifies the changed source as drift or missing"
assert_jq "$verify_out" '.data.auditCount >= 1 and .data.mutationCount >= 1' \
    "verify provenance records audited trust mutation evidence"
assert_jq "$verify_out" 'all(.data.referents[]?; .status != "removed")' \
    "verify provenance never removes memories"

step "memory drift read-only report reflects provenance verification status"
drift_out="$(ee_json memory drift "$mem_id" --workspace "$WS" --json)"
assert_jq "$drift_out" '.schema == "ee.memory_drift.report.v1"' \
    "memory drift emits report schema"
assert_jq "$drift_out" '.mode == "one_memory" and .summary.totalMemories == 1' \
    "memory drift reports the requested memory only"
assert_jq "$drift_out" '
    any(.items[]?; .memoryId == "'"$mem_id"'" and (.driftStatus == "changed" or .driftStatus == "missing_source" or .driftStatus == "unverifiable"))
' "memory drift reports an affected or unverifiable provenance state"

step "pack stale-anchor signal route pin"
pack_out="$(ee_json pack "trust freshness stale anchor" --workspace "$WS" --max-tokens 1200 --json)"
if printf '%s' "$pack_out" | jq -e '.schema == "ee.response.v2" and .success == true' >/dev/null 2>&1; then
    if printf '%s' "$pack_out" | jq -e '
        any(.. | objects;
            (((.freshness? // "") | tostring | test("stale|drift|anchor"))
             or (((.degradedCode? // "") | tostring) | test("stale_anchor|memory_drift")))
        )
    ' >/dev/null 2>&1; then
        _harness_pass "pack exposes a stale-anchor or drift freshness signal"
    else
        log_drop 1 "pack succeeded but stale_anchor freshness facet is pending: assert stale_anchor once pack exposes anchor freshness in JSON"
    fi
else
    log_drop 1 "pack route unavailable for stale_anchor assertion; keep this route pin until pack freshness JSON is wired"
fi

step "diag provenance route pin"
if ee_supports diag provenance; then
    diag_out="$(ee_json diag provenance --workspace "$WS" --json)"
    assert_jq "$diag_out" '.schema == "ee.response.v2" and .success == true' \
        "diag provenance returns a success envelope"
    assert_jq "$diag_out" '.data.schema == "ee.provenance_health.v1"' \
        "diag provenance emits provenance health schema"
    assert_jq "$diag_out" '(.data.summary.movedCount + .data.summary.missingCount + .data.summary.unverifiableCount) >= 1' \
        "diag provenance reports at least one non-present pointer"
else
    log_drop 1 "ee diag provenance CLI is pending/blocked: when wired, assert ee.provenance_health.v1 moved/missing/unverifiable summary for this memory"
fi

step "calibration/trust report route pin"
if ee_supports trust report; then
    trust_out="$(ee_json trust report --workspace "$WS" --json)"
    assert_jq "$trust_out" '.schema == "ee.response.v2" and .success == true' \
        "trust report returns a success envelope"
    assert_jq "$trust_out" '.data.schema == "ee.trust_report.v1"' \
        "trust report emits calibration schema"
    assert_jq "$trust_out" '(.data.recommendations // []) | type == "array"' \
        "trust report recommendations are proposal-only data"
else
    log_drop 1 "ee trust report CLI is pending/blocked: when wired, seed outcomes and assert calibration error, reliability leaderboard, and proposal-only recommendations"
fi

harness_summary
