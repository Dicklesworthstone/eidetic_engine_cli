#!/usr/bin/env bash
# bd-2vq2z.18 - Trust freshness lifecycle e2e (real binary, no mocks).
#
# This is the cross-track real-binary proof for drift/provenance/calibration
# trust signals. It exercises the current public surfaces for real:
#   - `ee why` provenance freshness degradations for present -> moved -> missing.
#   - `ee diag provenance` for present/moved/missing/unverifiable rollups.
#   - `ee verify provenance` audited mutation/candidate accounting.
#   - `ee memory drift` read-only posture after provenance verification.
#   - `ee trust report` calibration error, reliability leaders, and proposals.
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

git_fixture() {
    log_event "trust_freshness_git" command "git -C <workspace> $*"
    git -C "$WS" "$@"
}

with_temp_workspace WS

step "seed workspace files for provenance freshness"
mkdir -p "$WS/src" "$WS/docs"
git_fixture init -q -b main
git_fixture config user.email ee-e2e@example.test
git_fixture config user.name "ee e2e"
printf 'pub fn trust_probe_seed() -> &'\''static str {\n    "seed"\n}\n' \
    >"$WS/src/trust_probe.rs"
git_fixture add src/trust_probe.rs
git_fixture -c commit.gpgsign=false commit -q -m "seed trust freshness"
base_commit="$(git -C "$WS" rev-parse --verify HEAD)"
memory_text="trusted-freshness-v1 stale anchor ee-anchor:path:src/trust_probe.rs ee-anchor:symbol:trust_probe Captured at commit $base_commit"
printf '// %s\npub fn trust_probe() -> &'\''static str {\n    "trusted-freshness-v1"\n}\n' \
    "$memory_text" >"$WS/src/trust_probe.rs"
git_fixture add src/trust_probe.rs
git_fixture -c commit.gpgsign=false commit -q -m "add trust freshness memory source"
printf 'calibration evidence fixture\n' >"$WS/docs/calibration.md"
log_event "trust_freshness_fixture" \
    workspaceHash "$(printf '%s' "$WS" | shasum -a 256 | awk '{print $1}')" \
    source "src/trust_probe.rs" \
    capturedCommit "$base_commit" \
    bead "bd-2vq2z.18"

step "init isolated workspace"
init_out="$(ee_json init --workspace "$WS" --json)"
assert_jq "$init_out" '.schema == "ee.response.v2" and .success == true' \
    "ee init returns a success response envelope"

step "remember a file-backed memory for provenance freshness"
remember_out="$(ee_json remember \
    "$memory_text" \
    --workspace "$WS" --level episodic --kind fact \
    --source "file://src/trust_probe.rs#L1-L1" --json)"
assert_jq "$remember_out" '.schema == "ee.response.v2" and .success == true' \
    "remember with file provenance succeeds"
mem_id="$(json_scalar "$remember_out" '.data.memory_id // .data.memoryId // empty')"
assert_eq "$([ -n "$mem_id" ] && echo present || echo missing)" "present" \
    "remember returns a memory id"
log_event "trust_freshness_memory" memoryId "$mem_id" provenance "file://src/trust_probe.rs#L1-L1"

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
diag_moved="$(ee_json diag provenance --workspace "$WS" --json)"
assert_jq "$diag_moved" '.schema == "ee.response.v2" and .success == true and .data.schema == "ee.provenance_health.v1"' \
    "diag provenance returns a success provenance-health envelope for moved source"
assert_jq "$diag_moved" '
    any(.data.entries[]?; .memoryId == "'"$mem_id"'" and .health == "moved"
        and any(.pointers[]?; .status == "moved"))
' "diag provenance flags moved file-backed provenance"

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
diag_restored="$(ee_json diag provenance --workspace "$WS" --json)"
assert_jq "$diag_restored" '
    .schema == "ee.response.v2" and .success == true
    and any(.data.entries[]?; .memoryId == "'"$mem_id"'" and .health == "present")
' "diag provenance returns file-backed memory to present after restore"

step "change the cited evidence; why and verify provenance expose drift/missing trust"
printf 'pub fn trust_probe() -> &'\''static str {\n    "trusted-freshness-v2"\n}\n' \
    >"$WS/src/trust_probe.rs"
git_fixture add src/trust_probe.rs
git_fixture -c commit.gpgsign=false commit -q -m "change trust freshness source"
current_commit="$(git -C "$WS" rev-parse --verify HEAD)"
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
assert_jq "$drift_out" '.schema == "ee.response.v2" and .success == true and .data.schema == "ee.memory_drift.report.v1"' \
    "memory drift emits enveloped report schema"
assert_jq "$drift_out" '.data.mode == "one_memory" and .data.summary.totalMemories == 1' \
    "memory drift reports the requested memory only"
assert_jq "$drift_out" '
    any(.data.items[]?; .memoryId == "'"$mem_id"'" and (.driftStatus == "changed" or .driftStatus == "missing_source" or .driftStatus == "unverifiable"))
' "memory drift reports an affected or unverifiable provenance state"
assert_jq "$drift_out" '
    any(.data.items[]?;
        .memoryId == "'"$mem_id"'"
        and .freshness == "drifted"
        and .staleAnchor == true
        and .capturedAtCommit == "'"$base_commit"'"
        and .currentCommit == "'"$current_commit"'"
        and ((.commitDistance // 0) >= 1)
        and ((.changedRegions // []) | length >= 1)
        and any((.anchors // [])[]?; .staleAnchor == true and .freshness == "drifted")
    )
' "memory drift exposes code-anchor commit distance and stale-anchor details"

step "pack stale-anchor signal route pin"
pack_out="$(ee_json pack "trust freshness stale anchor" --workspace "$WS" --max-tokens 1200 --json)"
assert_jq "$pack_out" '.schema == "ee.response.v2" and .success == true' \
    "pack returns a success response envelope for stale-anchor route"
log_event "trust_freshness_stale_anchor_pack_assert" \
    memoryId "$mem_id" \
    capturedCommit "$base_commit" \
    currentCommit "$current_commit"
assert_jq "$pack_out" '
    any(.data.pack.items[]?;
        .memoryId == "'"$mem_id"'"
        and any((.freshnessFacets // [])[]?;
            .kind == "stale_anchor"
            and .freshness == "drifted"
            and .staleAnchor == true
            and .capturedAtCommit == "'"$base_commit"'"
            and .currentCommit == "'"$current_commit"'"
            and ((.commitDistance // 0) >= 1)
            and ((.changedRegions // []) | length >= 1)
            and any((.anchors // [])[]?; .staleAnchor == true and .freshness == "drifted")
        )
    )
' "pack keeps the drifted memory and exposes a stale_anchor freshness facet"

step "diag provenance reports changed file provenance and cass unverifiable"
diag_changed="$(ee_json diag provenance --workspace "$WS" --json)"
assert_jq "$diag_changed" '.schema == "ee.response.v2" and .success == true' \
    "diag provenance returns a success envelope"
assert_jq "$diag_changed" '.data.schema == "ee.provenance_health.v1"' \
    "diag provenance emits provenance health schema"
assert_jq "$diag_changed" '
    any(.data.entries[]?; .memoryId == "'"$mem_id"'" and .health == "missing"
        and any(.pointers[]?; .status == "missing"))
' "diag provenance reports mismatched file evidence as missing"
assert_jq "$diag_changed" '
    any(.data.entries[]?; .memoryId == "'"$cass_id"'" and .health == "unverifiable"
        and any(.pointers[]?; .status == "unverifiable"))
' "diag provenance reports cass-backed provenance as unverifiable, not missing"

step "seed outcome feedback for calibration/trust report"
helpful_out="$(ee_json remember \
    "low confidence trust report fixture that proved helpful" \
    --workspace "$WS" --level episodic --kind fact --confidence 0.2 \
    --source "file://docs/calibration.md#L1-L1" --json)"
assert_jq "$helpful_out" '.schema == "ee.response.v2" and .success == true' \
    "remember low-confidence helpful calibration fixture succeeds"
helpful_id="$(json_scalar "$helpful_out" '.data.memory_id // .data.memoryId // empty')"
assert_eq "$([ -n "$helpful_id" ] && echo present || echo missing)" "present" \
    "helpful calibration fixture returns a memory id"

harmful_out="$(ee_json remember \
    "high confidence trust report fixture that proved harmful" \
    --workspace "$WS" --level episodic --kind fact --confidence 0.9 \
    --source "file://docs/calibration.md#L1-L1" --json)"
assert_jq "$harmful_out" '.schema == "ee.response.v2" and .success == true' \
    "remember high-confidence harmful calibration fixture succeeds"
harmful_id="$(json_scalar "$harmful_out" '.data.memory_id // .data.memoryId // empty')"
assert_eq "$([ -n "$harmful_id" ] && echo present || echo missing)" "present" \
    "harmful calibration fixture returns a memory id"

helpful_feedback="$(ee_json outcome "$helpful_id" --workspace "$WS" --signal helpful --weight 1.0 --source-id trust-report-e2e --json)"
assert_jq "$helpful_feedback" '.schema == "ee.response.v2" and .success == true' \
    "helpful outcome feedback is recorded"
harmful_feedback="$(ee_json outcome "$harmful_id" --workspace "$WS" --signal harmful --weight 1.0 --source-id trust-report-e2e --json)"
assert_jq "$harmful_feedback" '.schema == "ee.response.v2" and .success == true' \
    "harmful outcome feedback is recorded"
log_event "trust_report_fixture" helpfulMemoryId "$helpful_id" harmfulMemoryId "$harmful_id" expectedEce "0.85"

step "calibration/trust report asserts real outcomes"
trust_out="$(ee_json trust report --workspace "$WS" --json)"
assert_jq "$trust_out" '.schema == "ee.response.v2" and .success == true' \
    "trust report returns a success envelope"
assert_jq "$trust_out" '.data.schema == "ee.trust_report.v1"' \
    "trust report emits calibration schema"
assert_jq "$trust_out" '.data.outcomeEvents.helpful == 1 and .data.outcomeEvents.harmful == 1' \
    "trust report counts scripted helpful and harmful outcomes"
assert_jq "$trust_out" '.data.calibration.expectedCalibrationError == 0.85' \
    "trust report expected calibration error matches seeded outcomes"
assert_jq "$trust_out" '.data.reliability.mostHelpful[0].memoryId == "'"$helpful_id"'"' \
    "trust report most-helpful leaderboard is outcome-backed"
assert_jq "$trust_out" '.data.reliability.mostHarmful[0].memoryId == "'"$harmful_id"'"' \
    "trust report most-harmful leaderboard is outcome-backed"
assert_jq "$trust_out" '(.data.recommendations // []) | type == "array"' \
    "trust report recommendations are proposal-only data"

harness_summary
