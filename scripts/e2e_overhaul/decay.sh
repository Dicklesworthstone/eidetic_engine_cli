#!/usr/bin/env bash
# J3 — L3 decay / forgetting e2e driver.
#
# Exercises deterministic decay lifecycle actions:
#   - dry-run previews do not write audit rows
#   - --include-decay demotes and tombstones stale memories
#   - status and learn summary report decay-derived signals
#   - curate untombstone reverses auto-forgetting without deleting rows

set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/e2e_overhaul/lib/shared.sh
source "$SCRIPT_DIR/lib/shared.sh"
require_jq
epic_setup "epic_M_decay"

AS_OF="$(python3 - <<'PYEOF'
from datetime import datetime, timedelta, timezone
print((datetime.now(timezone.utc) + timedelta(days=1000)).replace(microsecond=0).isoformat().replace("+00:00", "Z"))
PYEOF
)"

decay_step() {
    local step_id="${1:?step id required}"
    local expected_count="${2:-}"
    local observed_count="${3:-}"
    local status="${4:?status required}"
    _e2e_emit_event "decay_e2e_step" \
        "step_id" "$step_id" \
        "expected_count" "$expected_count" \
        "observed_count" "$observed_count" \
        "status" "$status"
}

cat >"$EPIC_WORKSPACE/.ee/config.toml" <<'EOF'
[learn.decay]
demote_threshold = 0.05
forget_threshold = 0.01
procedural_rule_half_life_days = 730
EOF
decay_step "workspace_config" "730" "730" "ok"

remember_decay_memory() {
    local level="${1:?level required}"
    local kind="${2:?kind required}"
    local confidence="${3:?confidence required}"
    local content="${4:?content required}"
    local json
    json=$(ee_workspace remember \
        --level "$level" \
        --kind "$kind" \
        --confidence "$confidence" \
        --tags decay,l3 \
        "$content" \
        --json 2>/dev/null || true)
    printf '%s' "$json" | jq -r '.data.memory_id // empty' 2>/dev/null || true
}

DEMOTE_ID=$(remember_decay_memory "procedural" "rule" "0.2" \
    "L3 stale procedural rule should demote to semantic.")
TOMBSTONE_ID=$(remember_decay_memory "semantic" "fact" "0.2" \
    "L3 obsolete semantic fact should be auto-forgotten.")
PRESERVE_ID=$(remember_decay_memory "procedural" "rule" "0.9" \
    "L3 high-confidence procedural rule should be preserved.")

if [ -z "$DEMOTE_ID" ] || [ -z "$TOMBSTONE_ID" ] || [ -z "$PRESERVE_ID" ]; then
    e2e_log_assert_eq "false" "true" "l3_decay_fixture_memory_ids_created"
    exit 0
fi
decay_step "seed_memories" "3" "3" "ok"

SUMMARY_BEFORE=$(ee_workspace learn summary --json 2>/dev/null || true)
SUMMARY_BEFORE_DEMOTED=$(printf '%s' "$SUMMARY_BEFORE" \
    | jq -r '.summary.memories_demoted_via_decay // 0' 2>/dev/null || echo 0)
SUMMARY_BEFORE_TOMBSTONED=$(printf '%s' "$SUMMARY_BEFORE" \
    | jq -r '.summary.memories_tombstoned_via_decay // 0' 2>/dev/null || echo 0)

DRY_RUN_JSON=$(ee_workspace maintenance run --include-decay --as-of "$AS_OF" --dry-run --json 2>/dev/null || true)
DRY_DEMOTED=$(printf '%s' "$DRY_RUN_JSON" \
    | jq -r '.data.results[0].details.decay.memoriesDemoted // 0' 2>/dev/null || echo 0)
DRY_TOMBSTONED=$(printf '%s' "$DRY_RUN_JSON" \
    | jq -r '.data.results[0].details.decay.memoriesTombstoned // 0' 2>/dev/null || echo 0)
SUMMARY_AFTER_DRY=$(ee_workspace learn summary --json 2>/dev/null || true)
SUMMARY_AFTER_DRY_DEMOTED=$(printf '%s' "$SUMMARY_AFTER_DRY" \
    | jq -r '.summary.memories_demoted_via_decay // 0' 2>/dev/null || echo 0)
SUMMARY_AFTER_DRY_TOMBSTONED=$(printf '%s' "$SUMMARY_AFTER_DRY" \
    | jq -r '.summary.memories_tombstoned_via_decay // 0' 2>/dev/null || echo 0)
e2e_log_assert_eq "$SUMMARY_AFTER_DRY_DEMOTED" "$SUMMARY_BEFORE_DEMOTED" \
    "l3_decay_dry_run_writes_no_demote_audit_rows"
e2e_log_assert_eq "$SUMMARY_AFTER_DRY_TOMBSTONED" "$SUMMARY_BEFORE_TOMBSTONED" \
    "l3_decay_dry_run_writes_no_tombstone_audit_rows"
e2e_log_assert_num "$DRY_DEMOTED" -ge 1 "l3_decay_dry_run_reports_demotions"
e2e_log_assert_num "$DRY_TOMBSTONED" -ge 1 "l3_decay_dry_run_reports_tombstones"
decay_step "dry_run_preview" "2" "$((DRY_DEMOTED + DRY_TOMBSTONED))" "ok"

NO_DECAY_JSON=$(ee_workspace maintenance run --as-of "$AS_OF" --dry-run --json 2>/dev/null || true)
NO_DECAY_ENABLED=$(printf '%s' "$NO_DECAY_JSON" \
    | jq -r '.data.results[0].details.decay.enabled // false' 2>/dev/null || echo false)
e2e_log_assert_eq "$NO_DECAY_ENABLED" "false" "l3_decay_default_maintenance_skips_lifecycle_actions"
decay_step "default_skip" "false" "$NO_DECAY_ENABLED" "ok"

RUN_JSON=$(ee_workspace maintenance run --include-decay --as-of "$AS_OF" --json 2>/dev/null || true)
RUN_DEMOTED=$(printf '%s' "$RUN_JSON" \
    | jq -r '.data.results[0].details.decay.memoriesDemoted // 0' 2>/dev/null || echo 0)
RUN_TOMBSTONED=$(printf '%s' "$RUN_JSON" \
    | jq -r '.data.results[0].details.decay.memoriesTombstoned // 0' 2>/dev/null || echo 0)
HALF_LIVES=$(printf '%s' "$RUN_JSON" \
    | jq -r '.data.results[0].details.decay.halfLivesApplied // false' 2>/dev/null || echo false)
PROCEDURAL_HALF_LIFE=$(printf '%s' "$RUN_JSON" \
    | jq -r '.data.results[0].details.decay.halfLifeDays.proceduralRule // empty' 2>/dev/null || true)
DEMOTE_THRESHOLD=$(printf '%s' "$RUN_JSON" \
    | jq -r '.data.results[0].details.decay.thresholdDemote // empty' 2>/dev/null || true)
e2e_log_assert_num "$RUN_DEMOTED" -ge 1 "l3_decay_real_run_demotes"
e2e_log_assert_num "$RUN_TOMBSTONED" -ge 1 "l3_decay_real_run_tombstones"
e2e_log_assert_eq "$HALF_LIVES" "true" "l3_decay_real_run_applies_half_lives"
e2e_log_assert_eq "$PROCEDURAL_HALF_LIFE" "730" "l3_decay_reads_workspace_half_life_config"
e2e_log_assert_eq "$DEMOTE_THRESHOLD" "0.05" "l3_decay_reads_workspace_threshold_config"
decay_step "apply_decay" "2" "$((RUN_DEMOTED + RUN_TOMBSTONED))" "ok"

MEMORY_LIST_JSON=$(ee_workspace memory list --json 2>/dev/null || true)
DEMOTED_LEVEL=$(printf '%s' "$MEMORY_LIST_JSON" \
    | jq -r --arg id "$DEMOTE_ID" '.data.memories[]? | select(.id == $id) | .level' 2>/dev/null || true)
TOMBSTONE_STATE=$(printf '%s' "$MEMORY_LIST_JSON" \
    | jq -r --arg id "$TOMBSTONE_ID" '.data.memories[]? | select(.id == $id) | if .is_tombstoned then "tombstoned" else "active" end' 2>/dev/null || true)
e2e_log_assert_eq "$DEMOTED_LEVEL" "semantic" "l3_decay_demoted_procedural_to_semantic"
e2e_log_assert_eq "$TOMBSTONE_STATE" "tombstoned" "l3_decay_tombstoned_below_forget_threshold"

DEMOTE_HISTORY_JSON=$(ee_workspace memory history "$DEMOTE_ID" --json 2>/dev/null || true)
TOMBSTONE_HISTORY_JSON=$(ee_workspace memory history "$TOMBSTONE_ID" --json 2>/dev/null || true)
DEMOTE_TRANSITION_COUNT=$(printf '%s' "$DEMOTE_HISTORY_JSON" \
    | jq '[.data.entries[]? | select(.action == "memory.level_transition" and .details.previousLevel == "procedural" and .details.newLevel == "semantic" and .details.sourceAction == "memory.decay_demote")] | length' \
    2>/dev/null || echo 0)
TOMBSTONE_TRANSITION_COUNT=$(printf '%s' "$TOMBSTONE_HISTORY_JSON" \
    | jq '[.data.entries[]? | select(.action == "memory.level_transition" and .details.newLevel == "tombstoned" and .details.sourceAction == "memory.decay_tombstone")] | length' \
    2>/dev/null || echo 0)
e2e_log_assert_num "$DEMOTE_TRANSITION_COUNT" -ge 1 "g9_l3_decay_demote_writes_level_transition_audit"
e2e_log_assert_num "$TOMBSTONE_TRANSITION_COUNT" -ge 1 "g9_l3_decay_tombstone_writes_level_transition_audit"

STATUS_JSON=$(ee_workspace status --json 2>/dev/null || true)
assert_jq "$STATUS_JSON" '.data.memoryHealth.scoreComponents.sourcedFrom' \
    "decay_v1" "l3_status_freshness_source_decay_v1"

SUMMARY_JSON=$(ee_workspace learn summary --json 2>/dev/null || true)
SUMMARY_DEMOTED=$(printf '%s' "$SUMMARY_JSON" \
    | jq -r '.summary.memories_demoted_via_decay // 0' 2>/dev/null || echo 0)
SUMMARY_TOMBSTONED=$(printf '%s' "$SUMMARY_JSON" \
    | jq -r '.summary.memories_tombstoned_via_decay // 0' 2>/dev/null || echo 0)
e2e_log_assert_num "$SUMMARY_DEMOTED" -ge 1 "l3_learn_summary_decay_demotions"
e2e_log_assert_num "$SUMMARY_TOMBSTONED" -ge 1 "l3_learn_summary_decay_tombstones"
decay_step "audit_rows" "2" "$((SUMMARY_DEMOTED + SUMMARY_TOMBSTONED))" "ok"
decay_step "learn_summary" "2" "$((SUMMARY_DEMOTED + SUMMARY_TOMBSTONED))" "ok"

UNTOMBSTONE_JSON=$(ee_workspace curate untombstone "$TOMBSTONE_ID" \
    --reason "restore after L3 e2e auto-forgetting" \
    --actor decay_e2e \
    --json 2>/dev/null || true)
UNTOMBSTONE_PERSISTED=$(printf '%s' "$UNTOMBSTONE_JSON" \
    | jq -r '.persisted // false' 2>/dev/null || echo false)
MEMORY_LIST_RESTORED_JSON=$(ee_workspace memory list --no-tombstoned --json 2>/dev/null || true)
RESTORED_STATE=$(printf '%s' "$MEMORY_LIST_RESTORED_JSON" \
    | jq -r --arg id "$TOMBSTONE_ID" 'if ([.data.memories[]?.id] | index($id)) == null then "missing" else "active" end' 2>/dev/null || echo missing)
e2e_log_assert_eq "$UNTOMBSTONE_PERSISTED" "true" "l3_curate_untombstone_persisted"
e2e_log_assert_eq "$RESTORED_STATE" "active" "l3_curate_untombstone_restores_memory"
decay_step "untombstone" "active" "$RESTORED_STATE" "ok"
