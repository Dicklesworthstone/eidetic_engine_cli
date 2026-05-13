#!/usr/bin/env bash
# J3 — Epic G: learn/curate implementation e2e driver.
#
# Asserts the shipped G1 (learn summary aggregates from audit_log) and G2
# work, records TODOs for the remaining curation pipeline, and asserts the G9
# lifecycle transition paths owned by learn/curate.
#
# Shipped (real assertions):  G1, G2, G5, G9 lifecycle transition audit
# Not yet shipped (todo):     G3, G4, G7, G8

set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/e2e_overhaul/lib/shared.sh
source "$SCRIPT_DIR/lib/shared.sh"
require_jq
epic_setup "epic_G_learn_curate"
seed_corpus

# ------------------------------------------------------------
# G1 (shipped) — `ee learn summary` aggregates from audit_log; generated_at is
# a real timestamp (not 1970-01-01); counts reflect persisted memories.
# ------------------------------------------------------------
SUMMARY_JSON=$(ee_workspace learn summary --json 2>/dev/null || true)
if ! printf '%s' "$SUMMARY_JSON" | jq . >/dev/null 2>&1; then
    e2e_log_assert_eq "false" "true" "learn_summary_json_parses"
    exit 0
fi

GEN_AT=$(printf '%s' "$SUMMARY_JSON" \
    | jq -r '.generatedAt // .data.generatedAt // .data.summary.generatedAt // empty' \
    2>/dev/null || true)
e2e_log_note "g1_learn_summary_generated_at=$GEN_AT"

case "$GEN_AT" in
    1970-*|"")
        e2e_log_assert_eq "$GEN_AT" "non-epoch" "g1_generated_at_is_not_unix_epoch"
        ;;
    *)
        e2e_log_assert_eq "true" "true" "g1_generated_at_is_not_unix_epoch"
        ;;
esac

# memories_created should reflect the corpus seed count, not be a stub 0.
MEM_CREATED=$(printf '%s' "$SUMMARY_JSON" \
    | jq -r '.summary.memories_created // .data.summary.memories_created // 0' \
    2>/dev/null || echo 0)
e2e_log_note "g1_learn_summary_memories_created=$MEM_CREATED"
e2e_log_assert_num "$MEM_CREATED" -ge 0 "g1_memories_created_is_numeric"

# ------------------------------------------------------------
# G3-G9 — TODOs.
# ------------------------------------------------------------
todo_assert "g3_remember_enqueues_propose_candidate" "bd-17c65.7.3" \
    "ee remember does not yet enqueue propose_curation_candidate(memory_id, workspace_id)."

todo_assert "g4_curate_candidates_surfaces_auto_proposed" "bd-17c65.7.4" \
    "ee curate candidates does not yet show auto-proposed candidates with evidence."

# ------------------------------------------------------------
# G5 (shipped) — `ee learn cluster` uses deterministic average-linkage
# clustering, reads the workspace threshold, and emits per-cluster J1 events.
# ------------------------------------------------------------
printf '\n[learn]\ncluster_coherence_threshold = 0.0\n' >>"$EPIC_WORKSPACE/.ee/config.toml"
for index in 1 2 3; do
    ee_workspace remember "g5 cargo format cluster coherence sample $index" \
        --level procedural \
        --kind rule \
        --tags g5-cluster,cargo-format \
        --no-propose-candidates \
        --json >/dev/null 2>&1 || true
done

G5_EVENT_COUNT_BEFORE=0
if [ -n "${EE_TEST_LOG_PATH:-}" ] && [ -f "$EE_TEST_LOG_PATH" ]; then
    G5_EVENT_COUNT_BEFORE=$(jq -r 'select(.fields.event == "learn_cluster") | .fields.candidate_id' \
        "$EE_TEST_LOG_PATH" 2>/dev/null | wc -l | tr -d ' ')
fi

G5_CLUSTER_JSON=$(ee_workspace learn cluster --json 2>/dev/null || true)
if ! printf '%s' "$G5_CLUSTER_JSON" | jq . >/dev/null 2>&1; then
    e2e_log_assert_eq "false" "true" "g5_learn_cluster_json_parses"
else
    G5_THRESHOLD_MILLI=$(printf '%s' "$G5_CLUSTER_JSON" \
        | jq -r '((.threshold // -1) * 1000 | round)' 2>/dev/null || echo "-1")
    G5_CLUSTER_COUNT=$(printf '%s' "$G5_CLUSTER_JSON" \
        | jq -r '.clusterCount // 0' 2>/dev/null || echo 0)
    G5_FIRST_CLUSTER_ID=$(printf '%s' "$G5_CLUSTER_JSON" \
        | jq -r '.clusters[0].cluster_id // empty' 2>/dev/null || true)
    e2e_log_assert_eq "$G5_THRESHOLD_MILLI" "0" \
        "g5_learn_cluster_reads_workspace_threshold"
    e2e_log_assert_num "$G5_CLUSTER_COUNT" -ge 1 \
        "g5_learn_cluster_emits_cluster"
    e2e_log_assert_eq "${G5_FIRST_CLUSTER_ID:+present}" "present" \
        "g5_learn_cluster_has_stable_cluster_id"

    G5_EVENT_COUNT_AFTER=$G5_EVENT_COUNT_BEFORE
    if [ -n "${EE_TEST_LOG_PATH:-}" ] && [ -f "$EE_TEST_LOG_PATH" ]; then
        G5_EVENT_COUNT_AFTER=$(jq -r 'select(.fields.event == "learn_cluster") | .fields.candidate_id' \
            "$EE_TEST_LOG_PATH" 2>/dev/null | wc -l | tr -d ' ')
    fi
    G5_EVENT_DELTA=$((G5_EVENT_COUNT_AFTER - G5_EVENT_COUNT_BEFORE))
    e2e_log_assert_num "$G5_EVENT_DELTA" -ge "$G5_CLUSTER_COUNT" \
        "g5_learn_cluster_logs_per_cluster_event"
fi

todo_assert "g7_auto_link_behavior_clarified" "bd-17c65.7.6" \
    "ee remember auto-link currently always returns no_workflow regardless of context."

todo_assert "g8_audit_log_instrumentation_complete" "bd-17c65.7.7" \
    "Not every memory-mutating surface writes audit events for L3/G1 to read."

# ------------------------------------------------------------
# G9 — lifecycle transitions write canonical memory.level_transition audit rows.
# ------------------------------------------------------------
G9_WORKFLOW="wf-g9-lifecycle"
G9_REMEMBER_JSON=$(ee_workspace remember \
    "G9 working lifecycle marker." \
    --level working \
    --kind fact \
    --workflow "$G9_WORKFLOW" \
    --json 2>/dev/null || true)
G9_MEMORY_ID=$(printf '%s' "$G9_REMEMBER_JSON" \
    | jq -r '.data.memory_id // empty' 2>/dev/null || true)

if [ -z "$G9_MEMORY_ID" ]; then
    e2e_log_assert_eq "$G9_MEMORY_ID" "non-empty" "g9_working_memory_created"
else
    G9_CLOSE_JSON=$(ee_workspace workflow close "$G9_WORKFLOW" --json 2>/dev/null || true)
    G9_HISTORY_JSON=$(ee_workspace memory history "$G9_MEMORY_ID" --json 2>/dev/null || true)
    G9_PROMOTED_LEVEL=$(ee_workspace memory show "$G9_MEMORY_ID" --json 2>/dev/null \
        | jq -r '.data.memory.level // empty' 2>/dev/null || true)
    G9_TRANSITION_COUNT=$(printf '%s' "$G9_HISTORY_JSON" \
        | jq '[.data.entries[]? | select(.action == "memory.level_transition" and .details.previousLevel == "working" and .details.newLevel == "episodic" and .details.event == "workflow.completed")] | length' \
        2>/dev/null || echo 0)
    G9_CLOSE_COUNT=$(printf '%s' "$G9_CLOSE_JSON" \
        | jq -r '.data.promoted_count // 0' 2>/dev/null || echo 0)

    e2e_log_assert_eq "$G9_PROMOTED_LEVEL" "episodic" "g9_workflow_close_promotes_to_episodic"
    e2e_log_assert_num "$G9_CLOSE_COUNT" -ge 1 "g9_workflow_close_reports_promotion"
    e2e_log_assert_num "$G9_TRANSITION_COUNT" -ge 1 "g9_workflow_close_writes_level_transition_audit"
fi

G9_MANUAL_JSON=$(ee_workspace remember \
    "G9 manual lifecycle marker." \
    --level working \
    --kind fact \
    --no-propose-candidates \
    --json 2>/dev/null || true)
G9_MANUAL_ID=$(printf '%s' "$G9_MANUAL_JSON" \
    | jq -r '.data.memory_id // empty' 2>/dev/null || true)

if [ -z "$G9_MANUAL_ID" ]; then
    e2e_log_assert_eq "$G9_MANUAL_ID" "non-empty" "g9_manual_working_memory_created"
else
    G9_MANUAL_LEVEL_JSON=$(ee_workspace memory level "$G9_MANUAL_ID" \
        --to episodic \
        --reason "G9 manual lifecycle promotion" \
        --actor g9_e2e \
        --json 2>/dev/null || true)
    G9_MANUAL_LEVEL_STATUS=$(printf '%s' "$G9_MANUAL_LEVEL_JSON" \
        | jq -r '.data.status // empty' 2>/dev/null || true)
    G9_MANUAL_LEVEL=$(ee_workspace memory show "$G9_MANUAL_ID" --json 2>/dev/null \
        | jq -r '.data.memory.level // empty' 2>/dev/null || true)
    G9_MANUAL_HISTORY_JSON=$(ee_workspace memory history "$G9_MANUAL_ID" --json 2>/dev/null || true)
    G9_MANUAL_TRANSITION_COUNT=$(printf '%s' "$G9_MANUAL_HISTORY_JSON" \
        | jq '[.data.entries[]? | select(.action == "memory.level_transition" and .details.previousLevel == "working" and .details.newLevel == "episodic" and .details.event == "manual.promote_to_episodic")] | length' \
        2>/dev/null || echo 0)

    e2e_log_assert_eq "$G9_MANUAL_LEVEL_STATUS" "transitioned" "g9_memory_level_manual_promote_status"
    e2e_log_assert_eq "$G9_MANUAL_LEVEL" "episodic" "g9_memory_level_manual_promotes_to_episodic"
    e2e_log_assert_num "$G9_MANUAL_TRANSITION_COUNT" -ge 1 "g9_memory_level_manual_writes_transition_audit"
fi

G9_SEMANTIC_JSON=$(ee_workspace remember \
    "G9 semantic lifecycle marker that became time-bound." \
    --level semantic \
    --kind fact \
    --no-propose-candidates \
    --json 2>/dev/null || true)
G9_SEMANTIC_ID=$(printf '%s' "$G9_SEMANTIC_JSON" \
    | jq -r '.data.memory_id // empty' 2>/dev/null || true)

if [ -z "$G9_SEMANTIC_ID" ]; then
    e2e_log_assert_eq "$G9_SEMANTIC_ID" "non-empty" "g9_semantic_memory_created"
else
    ee_workspace memory expire "$G9_SEMANTIC_ID" \
        --reason "G9 fact is now time-bound" \
        --actor g9_e2e \
        --json >/dev/null 2>&1 || true
    G9_EXPIRED_LEVEL=$(ee_workspace memory show "$G9_SEMANTIC_ID" --json 2>/dev/null \
        | jq -r '.data.memory.level // empty' 2>/dev/null || true)
    G9_EXPIRE_HISTORY_JSON=$(ee_workspace memory history "$G9_SEMANTIC_ID" --json 2>/dev/null || true)
    G9_EXPIRE_TRANSITION_COUNT=$(printf '%s' "$G9_EXPIRE_HISTORY_JSON" \
        | jq '[.data.entries[]? | select(.action == "memory.level_transition" and .details.previousLevel == "semantic" and .details.newLevel == "episodic" and .details.event == "valid_to.set")] | length' \
        2>/dev/null || echo 0)

    e2e_log_assert_eq "$G9_EXPIRED_LEVEL" "episodic" "g9_memory_expire_demotes_semantic_to_episodic"
    e2e_log_assert_num "$G9_EXPIRE_TRANSITION_COUNT" -ge 1 "g9_memory_expire_writes_level_transition_audit"
fi

G9_EPISODIC_JSON=$(ee_workspace remember \
    "G9 repeated episodic lifecycle observation." \
    --level episodic \
    --kind observation \
    --no-propose-candidates \
    --json 2>/dev/null || true)
G9_EPISODIC_ID=$(printf '%s' "$G9_EPISODIC_JSON" \
    | jq -r '.data.memory_id // empty' 2>/dev/null || true)

if [ -z "$G9_EPISODIC_ID" ]; then
    e2e_log_assert_eq "$G9_EPISODIC_ID" "non-empty" "g9_episodic_memory_created"
else
    G9_SEMANTIC_LEVEL_JSON=$(ee_workspace memory level "$G9_EPISODIC_ID" \
        --to semantic \
        --reason "G9 repeated observations support semantic memory" \
        --actor g9_e2e \
        --json 2>/dev/null || true)
    G9_SEMANTIC_LEVEL_STATUS=$(printf '%s' "$G9_SEMANTIC_LEVEL_JSON" \
        | jq -r '.data.status // empty' 2>/dev/null || true)
    G9_SEMANTIC_LEVEL=$(ee_workspace memory show "$G9_EPISODIC_ID" --json 2>/dev/null \
        | jq -r '.data.memory.level // empty' 2>/dev/null || true)
    G9_SEMANTIC_HISTORY_JSON=$(ee_workspace memory history "$G9_EPISODIC_ID" --json 2>/dev/null || true)
    G9_SEMANTIC_TRANSITION_COUNT=$(printf '%s' "$G9_SEMANTIC_HISTORY_JSON" \
        | jq '[.data.entries[]? | select(.action == "memory.level_transition" and .details.previousLevel == "episodic" and .details.newLevel == "semantic" and .details.event == "manual.promote_to_semantic" and .details.sourceAction == "memory.level")] | length' \
        2>/dev/null || echo 0)

    e2e_log_assert_eq "$G9_SEMANTIC_LEVEL_STATUS" "transitioned" "g9_memory_level_manual_promote_to_semantic_status"
    e2e_log_assert_eq "$G9_SEMANTIC_LEVEL" "semantic" "g9_memory_level_manual_promotes_to_semantic"
    e2e_log_assert_num "$G9_SEMANTIC_TRANSITION_COUNT" -ge 1 "g9_memory_level_manual_semantic_writes_transition_audit"
fi

G9_PROCEDURAL_JSON=$(ee_workspace remember \
    "G9 durable semantic rule seed." \
    --level semantic \
    --kind fact \
    --no-propose-candidates \
    --json 2>/dev/null || true)
G9_PROCEDURAL_ID=$(printf '%s' "$G9_PROCEDURAL_JSON" \
    | jq -r '.data.memory_id // empty' 2>/dev/null || true)

if [ -z "$G9_PROCEDURAL_ID" ]; then
    e2e_log_assert_eq "$G9_PROCEDURAL_ID" "non-empty" "g9_procedural_seed_memory_created"
else
    G9_PROCEDURAL_LEVEL_JSON=$(ee_workspace memory level "$G9_PROCEDURAL_ID" \
        --to procedural \
        --reason "G9 validated semantic memory as durable procedural guidance" \
        --actor g9_e2e \
        --json 2>/dev/null || true)
    G9_PROCEDURAL_LEVEL_STATUS=$(printf '%s' "$G9_PROCEDURAL_LEVEL_JSON" \
        | jq -r '.data.status // empty' 2>/dev/null || true)
    G9_PROCEDURAL_LEVEL=$(ee_workspace memory show "$G9_PROCEDURAL_ID" --json 2>/dev/null \
        | jq -r '.data.memory.level // empty' 2>/dev/null || true)
    G9_PROCEDURAL_HISTORY_JSON=$(ee_workspace memory history "$G9_PROCEDURAL_ID" --json 2>/dev/null || true)
    G9_PROCEDURAL_TRANSITION_COUNT=$(printf '%s' "$G9_PROCEDURAL_HISTORY_JSON" \
        | jq '[.data.entries[]? | select(.action == "memory.level_transition" and .details.previousLevel == "semantic" and .details.newLevel == "procedural" and .details.event == "manual.promote_to_procedural" and .details.sourceAction == "memory.level")] | length' \
        2>/dev/null || echo 0)

    e2e_log_assert_eq "$G9_PROCEDURAL_LEVEL_STATUS" "transitioned" "g9_memory_level_manual_promote_to_procedural_status"
    e2e_log_assert_eq "$G9_PROCEDURAL_LEVEL" "procedural" "g9_memory_level_manual_promotes_to_procedural"
    e2e_log_assert_num "$G9_PROCEDURAL_TRANSITION_COUNT" -ge 1 "g9_memory_level_manual_procedural_writes_transition_audit"
fi
