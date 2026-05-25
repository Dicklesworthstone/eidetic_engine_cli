#!/usr/bin/env bash
# Logged E2E driver for active-learning knowledge-gap recommendation posture.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/e2e_overhaul/lib/shared.sh
source "$SCRIPT_DIR/lib/shared.sh"
# shellcheck source=scripts/e2e_overhaul/lib/derivation_reflection.sh
source "$SCRIPT_DIR/lib/derivation_reflection.sh"

require_jq
epic_setup "knowledge_gaps_recommendations"

KG_COMMAND_FIXTURE="$EPIC_WORKSPACE/knowledge_gaps_recommendations.jsonl"
KG_REFLECTION_KEY="$EPIC_WORKSPACE/reflection_hmac.key"
printf '%s\n' "bd-3bsvv-knowledge-gaps-recommendations-key" >"$KG_REFLECTION_KEY"

kg_remember() {
    local __var_name="${1:?var name required}"
    local level="${2:?level required}"
    local kind="${3:?kind required}"
    local content="${4:?content required}"
    local label="${5:?label required}"
    local json memory_id
    json=$(ee_workspace remember "$content" \
        --level "$level" \
        --kind "$kind" \
        --tags knowledge-gaps,e2e \
        --no-auto-link \
        --json 2>/dev/null || true)
    assert_jq "$json" '.success // false' "true" "${label}_remember_success"
    memory_id=$(printf '%s' "$json" | jq -r '.data.memory_id // .data.memoryId // empty' 2>/dev/null || true)
    assert_jq_nonempty "$json" '.data.memory_id // .data.memoryId // empty' "${label}_memory_id"
    printf -v "$__var_name" '%s' "$memory_id"
}

kg_link() {
    local source="${1:?source memory required}"
    local target="${2:?target memory required}"
    local relation="${3:?relation required}"
    local evidence_count="${4:?evidence count required}"
    local confidence="${5:?confidence required}"
    local label="${6:?label required}"
    local json
    json=$(ee_workspace memory link "$source" "$target" \
        --relation "$relation" \
        --source agent \
        --evidence-count "$evidence_count" \
        --confidence "$confidence" \
        --undirected \
        --json 2>/dev/null || true)
    assert_jq "$json" '.success // false' "true" "${label}_link_success"
}

kg_seed_gap_graph() {
    kg_remember KG_BRIDGE_A semantic fact "KG fixture bridge endpoint A." "kg_bridge_a"
    kg_remember KG_BRIDGE_B semantic fact "KG fixture bridge articulation B." "kg_bridge_b"
    kg_remember KG_BRIDGE_C semantic fact "KG fixture bridge endpoint C." "kg_bridge_c"
    kg_remember KG_CONTRA_A semantic fact "KG fixture contradiction exemplar A." "kg_contra_a"
    kg_remember KG_CONTRA_B semantic fact "KG fixture contradiction exemplar B." "kg_contra_b"
    kg_remember KG_CONTRA_C semantic fact "KG fixture contradiction exemplar C." "kg_contra_c"
    kg_remember KG_HARM episodic fact "KG fixture harmful deployment outcome without a durable procedural rule." "kg_harm"
    kg_remember KG_HARM_NEIGHBOR semantic fact "KG fixture nearby evidence for the harmful outcome." "kg_harm_neighbor"
    kg_remember KG_CAUSAL_SOURCE semantic fact "KG fixture low-confidence causal source." "kg_causal_source"
    kg_remember KG_CAUSAL_TARGET semantic fact "KG fixture low-confidence causal target." "kg_causal_target"

    DRH_SOURCE_MEMORY_A_ID="$KG_BRIDGE_B"
    DRH_SOURCE_MEMORY_B_ID="$KG_CONTRA_A"

    kg_link "$KG_BRIDGE_A" "$KG_BRIDGE_B" supports 1 1.0 "kg_bridge_1"
    kg_link "$KG_BRIDGE_B" "$KG_BRIDGE_C" supports 1 1.0 "kg_bridge_2"
    kg_link "$KG_CONTRA_A" "$KG_CONTRA_B" contradicts 1 1.0 "kg_contra_1"
    kg_link "$KG_CONTRA_B" "$KG_CONTRA_C" contradicts 1 1.0 "kg_contra_2"
    kg_link "$KG_CONTRA_A" "$KG_CONTRA_C" contradicts 1 1.0 "kg_contra_3"
    kg_link "$KG_HARM" "$KG_HARM_NEIGHBOR" supports 1 1.0 "kg_harm_neighbor"
    kg_link "$KG_CAUSAL_SOURCE" "$KG_CAUSAL_TARGET" supports 4 0.25 "kg_causal_low_confidence"
}

kg_validate_section() {
    local json="${1:?knowledgeGaps JSON required}"
    local command_fixture="${2:?command fixture path required}"
    local response_fixture="$EPIC_WORKSPACE/knowledge_gaps_response.json"
    printf '%s' "$json" >"$response_fixture"
    python3 - "$command_fixture" "$response_fixture" <<'PY'
import json
import sys

fixture_path = sys.argv[1]
response_path = sys.argv[2]
with open(response_path, "r", encoding="utf-8") as response_handle:
    payload = json.load(response_handle)
data = payload.get("data") or {}
sections = data.get("sections") or []
if data.get("selectedSection") != "knowledgeGaps":
    raise SystemExit("selectedSection was not knowledgeGaps")
if not sections or sections[0].get("section") != "knowledgeGaps":
    raise SystemExit("knowledgeGaps section marker missing")
items = sections[0].get("items") or []
recommendations = sections[0].get("recommendations") or []
expected = [
    "thin_evidence_bridge",
    "unresolved_contradiction_cluster",
    "harmful_neighborhood_without_rule",
    "underdetermined_causal_chain",
]
categories = [item.get("category") for item in items]
if categories != expected:
    raise SystemExit(f"category order mismatch: {categories!r}")
if len(recommendations) != len(items):
    raise SystemExit("recommendation count did not match item count")
with open(fixture_path, "w", encoding="utf-8") as handle:
    for item, recommendation in zip(items, recommendations):
        for field in ("category", "sourceMemoryIds", "metricEvidence", "explanation", "confidence", "priority", "recommendation"):
            if field not in item:
                raise SystemExit(f"gap missing {field}: {item}")
        metric = item["metricEvidence"]
        if metric.get("schema") != "ee.graph.knowledge_gap.v1" or not metric.get("signal"):
            raise SystemExit(f"bad metric evidence: {metric}")
        rec = item["recommendation"]
        if rec.get("kind") != "reflect_propose":
            raise SystemExit(f"bad recommendation kind: {rec}")
        command = rec.get("command") or ""
        if "reflect ingest" in command or "curate apply" in command or "--dry-run" not in command:
            raise SystemExit(f"unsafe recommendation command: {command}")
        source_ids = item["sourceMemoryIds"]
        command_sources = []
        parts = command.split()
        for index, part in enumerate(parts):
            if part == "--source-memory":
                try:
                    command_sources.append(parts[index + 1])
                except IndexError as exc:
                    raise SystemExit(f"--source-memory without value: {command}") from exc
        if command_sources != source_ids:
            raise SystemExit(f"command sources {command_sources!r} did not match item sources {source_ids!r}")
        if recommendation.get("id") != item.get("gapId"):
            raise SystemExit("compact recommendation id did not match gap id")
        if recommendation.get("recommendation_kind") != "reflect_propose":
            raise SystemExit("compact recommendation kind was not reflect_propose")
        if recommendation.get("suggested_query") != command:
            raise SystemExit("compact suggested_query did not match command")
        if any(word in json.dumps(item).lower() for word in ("placeholder", "todo", "fake")):
            raise SystemExit(f"placeholder text leaked into gap item: {item}")
        handle.write(json.dumps({"command": command, "sourceMemoryIds": source_ids}, sort_keys=True))
        handle.write("\n")
PY
}

kg_run_recommendation_commands() {
    local fixture_path="${1:?fixture path required}"
    while IFS= read -r entry; do
        [ -n "$entry" ] || continue
        local command expected actual propose_json
        command=$(printf '%s' "$entry" | jq -r '.command')
        # The generated command is a deterministic ee argv string with no shell metacharacters.
        # shellcheck disable=SC2086
        set -- $command
        if [ "${1:-}" != "ee" ]; then
            e2e_log_assert_eq "${1:-missing}" "ee" "knowledge_gaps_recommendation_command_prefix"
            continue
        fi
        shift
        propose_json=$(cd "$EPIC_WORKSPACE" && \
            EE_REFLECTION_HMAC_KEY_ID="bd-3bsvv-e2e-key" \
            EE_REFLECTION_HMAC_KEY_PATH="$KG_REFLECTION_KEY" \
            "$EE_BINARY" "$@" 2>/dev/null || true)
        assert_jq "$propose_json" '.success // false' "true" "knowledge_gaps_recommendation_reflect_propose_success"
        assert_jq "$propose_json" '.data.schema // empty' "ee.reflect.propose.v1" "knowledge_gaps_recommendation_reflect_propose_schema"
        assert_jq "$propose_json" '.data.reflectionKind // empty' "gaps" "knowledge_gaps_recommendation_reflect_kind"
        assert_jq "$propose_json" '.data.dryRun // false' "true" "knowledge_gaps_recommendation_reflect_dry_run"
        assert_jq "$propose_json" '.data.request.schema // empty' "ee.reflect.request.v1" "knowledge_gaps_recommendation_request_schema"
        expected=$(printf '%s' "$entry" | jq -c '.sourceMemoryIds')
        actual=$(printf '%s' "$propose_json" | jq -c '[.data.request.sourcePackage.sources[]?.id]')
        e2e_log_assert_eq "$actual" "$expected" "knowledge_gaps_recommendation_request_sources"
    done <"$fixture_path"
}

kg_seed_gap_graph
drh_log_state "knowledge_gaps_graph_fixture_seeded" "graph_fixture_seeded" "ok"

KNOWLEDGE_GAPS_JSON=$(ee_workspace insights --section knowledgeGaps --limit 10 --json 2>/dev/null || true)
drh_extract_degraded_codes "$KNOWLEDGE_GAPS_JSON"
drh_extract_recovery_actions "$KNOWLEDGE_GAPS_JSON"

if printf '%s' "$KNOWLEDGE_GAPS_JSON" | jq . >/dev/null 2>&1; then
    assert_jq "$KNOWLEDGE_GAPS_JSON" '.success // false' "true" "knowledge_gaps_success"
    assert_jq "$KNOWLEDGE_GAPS_JSON" '.data.selectedSection // empty' "knowledgeGaps" "knowledge_gaps_selected"
    assert_jq "$KNOWLEDGE_GAPS_JSON" '.data.sections[0].section // empty' "knowledgeGaps" "knowledge_gaps_section_field"
    ITEM_COUNT=$(printf '%s' "$KNOWLEDGE_GAPS_JSON" | jq '.data.sections[0].items | length' 2>/dev/null || echo 0)
    RECOMMENDATION_COUNT=$(printf '%s' "$KNOWLEDGE_GAPS_JSON" | jq '.data.sections[0].recommendations | length' 2>/dev/null || echo 0)
    e2e_log_assert_num "$ITEM_COUNT" -eq 4 "knowledge_gaps_fixture_item_count"
    e2e_log_assert_num "$RECOMMENDATION_COUNT" -eq "$ITEM_COUNT" "knowledge_gaps_recommendation_count"
    if kg_validate_section "$KNOWLEDGE_GAPS_JSON" "$KG_COMMAND_FIXTURE"; then
        e2e_log_assert_eq "ok" "ok" "knowledge_gaps_fixture_contract"
        kg_run_recommendation_commands "$KG_COMMAND_FIXTURE"
    else
        e2e_log_assert_eq "knowledgeGaps-contract" "valid" "knowledge_gaps_fixture_contract"
    fi
else
    e2e_log_assert_eq "parseable-json" "invalid-json" "knowledge_gaps_parseable"
fi

EMPTY_WORKSPACE="$EPIC_WORKSPACE-empty"
EMPTY_JSON=$("$EE_BINARY" --workspace "$EMPTY_WORKSPACE" --json insights --section knowledgeGaps 2>/dev/null || true)
if printf '%s' "$EMPTY_JSON" | jq . >/dev/null 2>&1; then
    assert_jq "$EMPTY_JSON" '.success // false' "true" "knowledge_gaps_empty_success"
    assert_jq "$EMPTY_JSON" '.data.sections[0].items | length' "0" "knowledge_gaps_empty_item_count"
    assert_jq "$EMPTY_JSON" '.data.sections[0].recommendations | length' "0" "knowledge_gaps_empty_recommendation_count"
    assert_jq "$EMPTY_JSON" '.data.degradedSignals[0].code // empty' "graph.workspace_empty" "knowledge_gaps_empty_degradation"
    DRH_RECOVERY_ACTIONS='["empty graph: no graph memories are available, so no reflection recommendation is emitted"]'
    drh_log_state "knowledge_gaps_empty_graph_no_recommendation" "graph.workspace_empty" "ok"
else
    e2e_log_assert_eq "parseable-json" "invalid-json" "knowledge_gaps_empty_parseable"
fi

drh_log_state "knowledge_gaps_recommendation_probe" "insights_knowledge_gaps_probe" "recorded"
drh_assert_test_log_contract "knowledge_gaps_recommendations_log_contract"

if [ "${EE_TEST_LOG_ASSERTS_FAIL:-0}" -gt 0 ]; then
    exit 1
fi
