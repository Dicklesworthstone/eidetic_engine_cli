#!/usr/bin/env bash
# bd-2vq2z.17 - real-binary E2E for coverage-gap capture demand.
#
# Scenario: start with a thin memory store, prove the gap report names missing
# kinds and capture templates, capture one demanded release rule, then prove
# the next pack gap report improves. The script runs a prebuilt ee binary only.

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# Avoid the shared harness's cargo-metadata fallback in code-first swarm lanes.
EE_BIN="${EE_BIN:-ee}"
export EE_BIN

# shellcheck source=scripts/e2e_lib.sh
# shellcheck disable=SC1091
source "$SCRIPT_DIR/e2e_lib.sh"

harness_init "coverage_gap"

ee_json() {
    e2e_log_command "$EE_BIN" "$@" || true
}

json_scalar() {
    local json="${1:?json required}"
    local filter="${2:?jq filter required}"
    printf '%s' "$json" | jq -r "$filter" 2>/dev/null | head -n 1
}

assert_int_lt() {
    local actual="${1:?actual required}"
    local expected_upper="${2:?expected upper required}"
    local label="${3:?label required}"
    local result="false"
    if [ "$actual" -lt "$expected_upper" ] 2>/dev/null; then
        result="true"
    fi
    e2e_log_assert_eq "$result" "true" "$label"
    if [ "$result" = "true" ]; then
        _harness_pass "$label ($actual < $expected_upper)"
    else
        _harness_fail "$label: expected $actual < $expected_upper"
    fi
}

with_temp_workspace WS

step "initialize isolated workspace"
init_out="$(ee_json init --workspace "$WS" --json)"
assert_jq "$init_out" '.schema == "ee.response.v2" and .success == true' \
    "ee init returns a success response envelope"

step "seed a thin store with insufficient generic guidance"
generic_out="$(
    ee_json remember "Run cargo fmt before editing code." \
        --workspace "$WS" \
        --level procedural \
        --kind rule \
        --source "file://AGENTS.md#L1" \
        --json
)"
assert_jq "$generic_out" '.schema == "ee.response.v2" and .success == true' \
    "generic memory capture succeeds"
log_event "coverage_gap_seed" \
    memoryKind "rule" \
    level "procedural" \
    purpose "thin-store-control"

step "why-not --gaps reports missing kinds and templates"
why_not_gaps="$(
    ee_json why-not \
        --task "prepare release" \
        --gaps \
        --workspace "$WS" \
        --json
)"
assert_jq "$why_not_gaps" '.schema == "ee.response.v2" and .success == true' \
    "why-not gaps returns a success envelope"
assert_jq "$why_not_gaps" '.data.schema == "ee.coverage_gap.v1"' \
    "why-not gaps emits coverage-gap schema"
assert_jq "$why_not_gaps" '.data.posture == "capture_required"' \
    "thin store requires capture"
assert_jq "$why_not_gaps" '
    [.data.missingKinds[]?.kind] | index("release_rule") != null
' "thin store names release rule gap"
assert_jq "$why_not_gaps" '
    [.data.missingKinds[]?.kind] | index("decision") != null and index("anti_pattern") != null
' "thin store names baseline decision and anti-pattern gaps"
assert_jq "$why_not_gaps" '
    [.data.captureTemplates[]?.kind] | index("release_rule") != null
' "thin store includes release rule capture template"
assert_jq "$why_not_gaps" '
    [.data.captureTemplates[]? | select(.kind == "release_rule") | .command] |
    any(contains("ee remember") and contains("--level procedural") and contains("--kind rule"))
' "release rule template contains an executable remember command"
assert_jq "$why_not_gaps" '(.data.nearestInsufficient | length) >= 1' \
    "gap report names nearest insufficient evidence"
before_count="$(json_scalar "$why_not_gaps" '.data.missingKinds | length')"
log_event "coverage_gap_before_capture" \
    missingKinds "$before_count" \
    posture "$(json_scalar "$why_not_gaps" '.data.posture')" \
    firstMissingKind "$(json_scalar "$why_not_gaps" '.data.missingKinds[0].kind')"

step "pack --explain-gaps returns the same coverage surface"
pack_gaps="$(
    ee_json pack "prepare release" \
        --workspace "$WS" \
        --max-tokens 360 \
        --read-only \
        --explain-gaps \
        --json
)"
assert_jq "$pack_gaps" '.schema == "ee.response.v2" and .success == true' \
    "pack explain-gaps returns a success envelope"
assert_jq "$pack_gaps" '.data.schema == "ee.coverage_gap.v1"' \
    "pack explain-gaps emits coverage-gap schema"
assert_jq "$pack_gaps" '
    [.data.captureTemplates[]?.kind] | index("release_rule") != null
' "pack explain-gaps carries release rule capture template"
assert_eq "$(json_scalar "$pack_gaps" '.data.taskHash')" \
    "$(json_scalar "$why_not_gaps" '.data.taskHash')" \
    "why-not and pack gap reports use the same task hash"

step "capture a demanded release rule"
release_rule_out="$(
    ee_json remember "Release rule: before release run verify, confirm rollback, then tag." \
        --workspace "$WS" \
        --level procedural \
        --kind rule \
        --source "file://docs/release.md#L1" \
        --json
)"
assert_jq "$release_rule_out" '.schema == "ee.response.v2" and .success == true' \
    "release rule capture succeeds"
log_event "coverage_gap_capture" \
    memoryKind "rule" \
    level "procedural" \
    capturedKind "release_rule"

step "next pack improves and clears release rule demand"
after_gaps="$(
    ee_json pack "prepare release" \
        --workspace "$WS" \
        --max-tokens 360 \
        --read-only \
        --explain-gaps \
        --json
)"
assert_jq "$after_gaps" '.schema == "ee.response.v2" and .success == true' \
    "after-capture pack explain-gaps returns a success envelope"
assert_jq "$after_gaps" '.data.schema == "ee.coverage_gap.v1"' \
    "after-capture pack keeps coverage-gap schema"
assert_jq "$after_gaps" '
    [.data.missingKinds[]?.kind] | index("release_rule") == null
' "after-capture report clears release rule gap"
assert_jq "$after_gaps" '
    [.data.captureTemplates[]?.kind] | index("release_rule") == null
' "after-capture report removes release rule capture template"
after_count="$(json_scalar "$after_gaps" '.data.missingKinds | length')"
assert_int_lt "$after_count" "$before_count" \
    "capturing demanded evidence reduces missing-kind count"
log_event "coverage_gap_after_capture" \
    missingKinds "$after_count" \
    posture "$(json_scalar "$after_gaps" '.data.posture')" \
    clearedKind "release_rule"

harness_summary
