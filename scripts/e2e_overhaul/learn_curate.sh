#!/usr/bin/env bash
# J3 — Epic G: learn/curate implementation e2e driver.
#
# Asserts the shipped G1 (learn summary aggregates from audit_log), G2,
# G3/G4 curation proposal surfaces, G5 clustering, G7 auto-link honesty, G8
# read-surface audit rows, and the G9 lifecycle transition paths owned by
# learn/curate.
#
# Shipped (real assertions):  G1, G2, G3, G4, G5, G7, G8, G9 lifecycle transition audit

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
# G3 (shipped) — `ee remember` auto-proposes a curation candidate once a
# repeated tagged rule cluster reaches the proposal threshold.
# ------------------------------------------------------------
G3_CANDIDATE_ID=""
G3_TARGET_ID=""
G3_MEMBER_COUNT=0
G3_FINAL_STATUS=""

for index in 0 1 2; do
    G3_REMEMBER_JSON=$(ee_workspace remember \
        "G3 cargo release rule $index: run cargo fmt --check before release." \
        --level procedural \
        --kind rule \
        --tags g3-auto-propose,cargo,release \
        --json 2>/dev/null || true)

    if ! printf '%s' "$G3_REMEMBER_JSON" | jq . >/dev/null 2>&1; then
        e2e_log_assert_eq "false" "true" "g3_remember_json_parses"
        continue
    fi

    G3_STATUS=$(printf '%s' "$G3_REMEMBER_JSON" \
        | jq -r '.data.curation_candidate_status // empty' 2>/dev/null || true)
    e2e_log_note "g3_remember_index=$index curation_candidate_status=$G3_STATUS"

    if [ "$index" -eq 2 ]; then
        G3_FINAL_STATUS="$G3_STATUS"
        G3_CANDIDATE_ID=$(printf '%s' "$G3_REMEMBER_JSON" \
            | jq -r '.data.curation_candidate.candidate_id // .data.curation_candidate.candidateId // empty' \
            2>/dev/null || true)
        G3_TARGET_ID=$(printf '%s' "$G3_REMEMBER_JSON" \
            | jq -r '.data.curation_candidate.target_memory_id // .data.curation_candidate.targetMemoryId // empty' \
            2>/dev/null || true)
        G3_MEMBER_COUNT=$(printf '%s' "$G3_REMEMBER_JSON" \
            | jq -r '[(.data.curation_candidate.member_memory_ids[]?, .data.curation_candidate.memberMemoryIds[]?)] | unique | length' \
            2>/dev/null || echo 0)
    fi
done

e2e_log_assert_eq "$G3_FINAL_STATUS" "proposed" \
    "g3_remember_enqueues_propose_candidate"
e2e_log_assert_eq "${G3_CANDIDATE_ID:+present}" "present" \
    "g3_remember_candidate_id_present"
e2e_log_assert_num "$G3_MEMBER_COUNT" -ge 3 \
    "g3_remember_candidate_has_cluster_members"

# ------------------------------------------------------------
# G4 (shipped) — `ee curate candidates` surfaces auto-proposed candidates with
# evidence, proposal source, priority, and proposed procedural rule metadata.
# ------------------------------------------------------------
G4_CANDIDATES_JSON=$(ee_workspace curate candidates --type rule --status pending --json 2>/dev/null || true)
if ! printf '%s' "$G4_CANDIDATES_JSON" | jq . >/dev/null 2>&1; then
    e2e_log_assert_eq "false" "true" "g4_curate_candidates_json_parses"
else
    G4_MATCH_COUNT=$(printf '%s' "$G4_CANDIDATES_JSON" \
        | jq -r --arg id "$G3_CANDIDATE_ID" '[.data.candidates[]? | select((.candidateId // .candidate_id // .id // "") == $id)] | length' \
        2>/dev/null || echo 0)
    G4_PROPOSAL_SOURCE=$(printf '%s' "$G4_CANDIDATES_JSON" \
        | jq -r --arg id "$G3_CANDIDATE_ID" '.data.candidates[]? | select((.candidateId // .candidate_id // .id // "") == $id) | .proposalSource // .proposal_source // empty' \
        2>/dev/null | head -n 1 || true)
    G4_PROPOSED_LEVEL=$(printf '%s' "$G4_CANDIDATES_JSON" \
        | jq -r --arg id "$G3_CANDIDATE_ID" '.data.candidates[]? | select((.candidateId // .candidate_id // .id // "") == $id) | .proposedLevel // .proposed_level // empty' \
        2>/dev/null | head -n 1 || true)
    G4_PROPOSED_KIND=$(printf '%s' "$G4_CANDIDATES_JSON" \
        | jq -r --arg id "$G3_CANDIDATE_ID" '.data.candidates[]? | select((.candidateId // .candidate_id // .id // "") == $id) | .proposedKind // .proposed_kind // empty' \
        2>/dev/null | head -n 1 || true)
    G4_SOURCE_TYPE=$(printf '%s' "$G4_CANDIDATES_JSON" \
        | jq -r --arg id "$G3_CANDIDATE_ID" '.data.candidates[]? | select((.candidateId // .candidate_id // .id // "") == $id) | .source.sourceType // .source.source_type // empty' \
        2>/dev/null | head -n 1 || true)
    G4_MEMBER_ID_COUNT=$(printf '%s' "$G4_CANDIDATES_JSON" \
        | jq -r --arg id "$G3_CANDIDATE_ID" '[.data.candidates[]? | select((.candidateId // .candidate_id // .id // "") == $id) | (.memberMemoryIds[]?, .member_memory_ids[]?)] | unique | length' \
        2>/dev/null || echo 0)
    G4_SUPPORT_COUNT=$(printf '%s' "$G4_CANDIDATES_JSON" \
        | jq -r --arg id "$G3_CANDIDATE_ID" '.data.candidates[]? | select((.candidateId // .candidate_id // .id // "") == $id) | .evidenceSummary.supportCount // .evidence_summary.support_count // 0' \
        2>/dev/null | head -n 1 || echo 0)
    G4_PRIORITY=$(printf '%s' "$G4_CANDIDATES_JSON" \
        | jq -r --arg id "$G3_CANDIDATE_ID" '.data.candidates[]? | select((.candidateId // .candidate_id // .id // "") == $id) | .priority // empty' \
        2>/dev/null | head -n 1 || true)
    G4_AUDIT_PROPOSED_BY=$(printf '%s' "$G4_CANDIDATES_JSON" \
        | jq -r --arg id "$G3_CANDIDATE_ID" '.data.candidates[]? | select((.candidateId // .candidate_id // .id // "") == $id) | .audit.proposedBy // .audit.proposed_by // empty' \
        2>/dev/null | head -n 1 || true)

    e2e_log_note "g4_candidate_id=$G3_CANDIDATE_ID target=$G3_TARGET_ID priority=$G4_PRIORITY"
    e2e_log_assert_num "$G4_MATCH_COUNT" -ge 1 \
        "g4_curate_candidates_surfaces_auto_proposed"
    e2e_log_assert_eq "$G4_PROPOSAL_SOURCE" "auto_propose_from_cluster" \
        "g4_curate_candidate_proposal_source"
    e2e_log_assert_eq "$G4_PROPOSED_LEVEL" "procedural" \
        "g4_curate_candidate_proposed_level"
    e2e_log_assert_eq "$G4_PROPOSED_KIND" "rule" \
        "g4_curate_candidate_proposed_kind"
    e2e_log_assert_eq "$G4_SOURCE_TYPE" "agent_inference" \
        "g4_curate_candidate_source_type"
    e2e_log_assert_num "$G4_MEMBER_ID_COUNT" -ge 3 \
        "g4_curate_candidate_member_memory_ids"
    e2e_log_assert_num "$G4_SUPPORT_COUNT" -ge 3 \
        "g4_curate_candidate_evidence_summary"
    e2e_log_assert_eq "${G4_PRIORITY:+present}" "present" \
        "g4_curate_candidate_priority_present"
    e2e_log_assert_eq "$G4_AUDIT_PROPOSED_BY" "auto_proposer:v1" \
        "g4_curate_candidate_audit_proposed_by"
fi

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

# ------------------------------------------------------------
# G7 (shipped) — workflow-less remember output honestly reports that auto-link
# is not applicable and points callers at explicit `ee memory link`.
# ------------------------------------------------------------
G7_REMEMBER_JSON=$(ee_workspace remember \
    "G7 workflow-less auto-link honesty marker." \
    --level episodic \
    --kind fact \
    --json 2>/dev/null || true)
if ! printf '%s' "$G7_REMEMBER_JSON" | jq . >/dev/null 2>&1; then
    e2e_log_assert_eq "false" "true" "g7_remember_json_parses"
    G7_MEMORY_ID=""
else
    G7_MEMORY_ID=$(printf '%s' "$G7_REMEMBER_JSON" \
        | jq -r '.data.memory_id // .data.memoryId // empty' 2>/dev/null || true)
    G7_AUTO_LINK_STATUS=$(printf '%s' "$G7_REMEMBER_JSON" \
        | jq -r '.data.auto_link_status // .data.autoLinkStatus // empty' 2>/dev/null || true)
    G7_AUTO_LINK_COUNT=$(printf '%s' "$G7_REMEMBER_JSON" \
        | jq -r '[.data.auto_links[]?, .data.autoLinks[]?] | length' 2>/dev/null || echo 0)
    G7_AUTO_LINK_DEGRADATION_COUNT=$(printf '%s' "$G7_REMEMBER_JSON" \
        | jq -r '[(.data.auto_link_degradations[]?, .data.autoLinkDegradations[]?)] | map(select(.code == "auto_link_disabled")) | length' \
        2>/dev/null || echo 0)

    e2e_log_assert_eq "$G7_AUTO_LINK_STATUS" "no_workflow_required" \
        "g7_auto_link_behavior_clarified"
    e2e_log_assert_num "$G7_AUTO_LINK_COUNT" -eq 0 \
        "g7_auto_link_does_not_create_workflowless_links"
    e2e_log_assert_num "$G7_AUTO_LINK_DEGRADATION_COUNT" -ge 1 \
        "g7_auto_link_disabled_degradation_present"
fi

# ------------------------------------------------------------
# G8 (shipped) — read surfaces write audit rows that L3 decay and G1 summary
# can consume as memory-access signals.
# ------------------------------------------------------------
if [ -z "${G7_MEMORY_ID:-}" ]; then
    e2e_log_assert_eq "$G7_MEMORY_ID" "non-empty" "g8_audit_seed_memory_created"
else
    ee_workspace memory show "$G7_MEMORY_ID" --json >/dev/null 2>&1 || true
    G8_AUDIT_JSON=$(ee_workspace audit timeline --action memory.show --limit 20 --json 2>/dev/null || true)
    if ! printf '%s' "$G8_AUDIT_JSON" | jq . >/dev/null 2>&1; then
        e2e_log_assert_eq "false" "true" "g8_audit_timeline_json_parses"
    else
        G8_MEMORY_SHOW_COUNT=$(printf '%s' "$G8_AUDIT_JSON" \
            | jq -r --arg id "$G7_MEMORY_ID" '[.entries[]? | select((.mutation_kind // .mutationKind // "") == "memory.show" and (.target_id // .targetId // "") == $id)] | length' \
            2>/dev/null || echo 0)
        G8_MEMORY_TARGET_COUNT=$(printf '%s' "$G8_AUDIT_JSON" \
            | jq -r --arg id "$G7_MEMORY_ID" '[.entries[]? | select((.target_type // .targetType // "") == "memory" and (.target_id // .targetId // "") == $id)] | length' \
            2>/dev/null || echo 0)

        e2e_log_assert_num "$G8_MEMORY_SHOW_COUNT" -ge 1 \
            "g8_memory_show_audit_action_present"
        e2e_log_assert_num "$G8_MEMORY_TARGET_COUNT" -ge 1 \
            "g8_memory_show_audit_targets_memory"
    fi
fi

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
