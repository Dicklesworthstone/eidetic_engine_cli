#!/usr/bin/env bash
# M4 — handoff stale-snapshot drift e2e coverage.

set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/e2e_overhaul/lib/shared.sh
source "$SCRIPT_DIR/lib/shared.sh"
require_jq
epic_setup "m4_handoff_stale"

remember_handoff_memory() {
    local content="${1:?content required}"
    ee_workspace remember "$content" \
        --level procedural \
        --kind rule \
        --no-propose-candidates \
        --json
}

memory_id_from_json() {
    jq -r '.data.memory_id // .memory_id // empty'
}

capsule_path() {
    local name="${1:?name required}"
    printf '%s\n' "$EPIC_WORKSPACE/$name-capsule.json"
}

create_capsule() {
    local path="${1:?capsule path required}"
    ee_workspace handoff create --out "$path" --profile resume --json >/dev/null
}

resume_capsule() {
    local path="${1:?capsule path required}"
    ee_workspace handoff resume "$path" --json
}

BASE_JSON=$(remember_handoff_memory "M4 baseline handoff memory before large drift.")
BASE_ID=$(printf '%s' "$BASE_JSON" | memory_id_from_json)
e2e_log_assert_eq "${BASE_ID:0:4}" "mem_" "m4_handoff_seed_memory_id"

ADDED_CAPSULE=$(capsule_path "added")
create_capsule "$ADDED_CAPSULE"
sleep 1
for i in $(seq 1 25); do
    remember_handoff_memory "M4 added memory $i after capsule capture." >/dev/null
done
ADDED_RESUME=$(resume_capsule "$ADDED_CAPSULE")
assert_jq "$ADDED_RESUME" '.stale_snapshot.memories_added_since' "25" \
    "m4_handoff_added_count"
assert_jq "$ADDED_RESUME" \
    '[.degradations[]? | select(.code == "handoff_snapshot_stale" and .severity == "medium")] | length' \
    "1" \
    "m4_handoff_added_degraded_medium"

EXPIRED_JSON=$(remember_handoff_memory "M4 memory that expires after handoff capture.")
EXPIRED_ID=$(printf '%s' "$EXPIRED_JSON" | memory_id_from_json)
EXPIRED_CAPSULE=$(capsule_path "expired")
create_capsule "$EXPIRED_CAPSULE"
sleep 1
ee_workspace memory expire "$EXPIRED_ID" --reason "m4 handoff stale e2e" --json >/dev/null
EXPIRED_RESUME=$(resume_capsule "$EXPIRED_CAPSULE")
assert_jq "$EXPIRED_RESUME" '.stale_snapshot.memories_expired_since' "1" \
    "m4_handoff_expired_count"
assert_jq "$EXPIRED_RESUME" \
    '[.degradations[]? | select(.code == "handoff_snapshot_stale" and .severity == "high")] | length' \
    "1" \
    "m4_handoff_expired_degraded_high"

CLEAN_CAPSULE=$(capsule_path "clean")
create_capsule "$CLEAN_CAPSULE"
CLEAN_STRICT_JSON=$(e2e_log_command "$EE_BINARY" handoff resume "$CLEAN_CAPSULE" \
    --workspace "$EPIC_WORKSPACE" --require-fresh --json)
CLEAN_STRICT_RC=$?
e2e_log_assert_eq "$CLEAN_STRICT_RC" "0" "m4_handoff_require_fresh_clean_exit"
assert_jq "$CLEAN_STRICT_JSON" '.stale_snapshot.drift_detected' "false" \
    "m4_handoff_require_fresh_clean_no_drift"
SIZE_BASELINE=$(printf '%s' "$CLEAN_STRICT_JSON" | jq -c 'del(.stale_snapshot)' | wc -c | tr -d '[:space:]')
SIZE_CURRENT=$(printf '%s' "$CLEAN_STRICT_JSON" | jq -c '.' | wc -c | tr -d '[:space:]')
SIZE_OVERHEAD=$((SIZE_CURRENT - SIZE_BASELINE))
SIZE_BUDGET=$(((SIZE_BASELINE + 11) / 12))
if [ "$SIZE_OVERHEAD" -le "$SIZE_BUDGET" ]; then
    e2e_log_assert_eq "true" "true" "m4_handoff_stale_snapshot_size_budget"
else
    e2e_log_assert_eq "$SIZE_OVERHEAD" "<=$SIZE_BUDGET" \
        "m4_handoff_stale_snapshot_size_budget"
fi
e2e_log_note \
    "handoff_resume_size_check baseline_bytes=$SIZE_BASELINE current_bytes=$SIZE_CURRENT overhead_bytes=$SIZE_OVERHEAD budget_bytes=$SIZE_BUDGET"

DRIFT_CAPSULE=$(capsule_path "strict-drift")
create_capsule "$DRIFT_CAPSULE"
sleep 1
remember_handoff_memory "M4 one-memory strict drift after capsule capture." >/dev/null
DRIFT_STRICT_JSON=$(e2e_log_command "$EE_BINARY" handoff resume "$DRIFT_CAPSULE" \
    --workspace "$EPIC_WORKSPACE" --require-fresh --json)
DRIFT_STRICT_RC=$?
e2e_log_assert_eq "$DRIFT_STRICT_RC" "6" "m4_handoff_require_fresh_drift_exit"

DET_ONE=$(resume_capsule "$ADDED_CAPSULE" | jq -S -c '.stale_snapshot | del(.computed_at)')
DET_TWO=$(resume_capsule "$ADDED_CAPSULE" | jq -S -c '.stale_snapshot | del(.computed_at)')
DET_THREE=$(resume_capsule "$ADDED_CAPSULE" | jq -S -c '.stale_snapshot | del(.computed_at)')
e2e_log_assert_eq "$DET_ONE" "$DET_TWO" "m4_handoff_stale_snapshot_deterministic_1"
e2e_log_assert_eq "$DET_ONE" "$DET_THREE" "m4_handoff_stale_snapshot_deterministic_2"

e2e_log_note "m4_handoff_strict_error_stdout_bytes=${#DRIFT_STRICT_JSON}"
