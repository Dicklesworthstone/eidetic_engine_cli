#!/usr/bin/env bash
# J3/J6 — failure-mode fixture catalog e2e driver.
#
# Reads every fixture under tests/fixtures/failure_modes and exercises the
# documented emission when the trigger is executable through public CLI
# surfaces. Fixtures that still document placeholder-only triggers are recorded
# as TODOs instead of being silently skipped.

set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/e2e_overhaul/lib/shared.sh
source "$SCRIPT_DIR/lib/shared.sh"
require_jq
epic_setup "j6_failure_modes"
seed_corpus

FIXTURE_DIR="$REPO_ROOT/tests/fixtures/failure_modes"

fixture_label() {
    printf 'j6_%s' "$1" | tr -c 'a-zA-Z0-9_' '_'
}

fixture_files() {
    find "$FIXTURE_DIR" -maxdepth 1 -type f -name '*.json' | sort
}

remember_j6_memory() {
    local content="${1:?content required}"
    local level="semantic"
    local kind="fact"
    shift
    if [ $# -gt 0 ]; then
        level="$1"
        shift
    fi
    if [ $# -gt 0 ]; then
        kind="$1"
        shift
    fi
    ee_workspace remember "$content" \
        --level "$level" \
        --kind "$kind" \
        --no-propose-candidates \
        "$@" \
        --json 2>/dev/null || true
}

run_fixture_scenario() {
    local code="${1:?code required}"
    SCENARIO_OUTPUT=""

    case "$code" in
        no_relevant_results)
            remember_j6_memory "J6 no relevant results seed: apples are red." semantic fact >/dev/null
            SCENARIO_OUTPUT=$(ee_workspace search \
                "zyxw unrelated impossible query" \
                --relevance-floor 0.99 \
                --json 2>/dev/null || true)
            ;;
        weak_query_recall)
            remember_j6_memory "J6 weak recall database connection pooling guide." semantic fact >/dev/null
            SCENARIO_OUTPUT=$(ee_workspace search "connection" --json 2>/dev/null || true)
            ;;
        low_recall_after_floor)
            remember_j6_memory "J6 low recall use cargo fmt before release." procedural rule >/dev/null
            remember_j6_memory "J6 low recall database connection pooling guide." semantic fact >/dev/null
            remember_j6_memory "J6 low recall migration added user email column." episodic decision >/dev/null
            remember_j6_memory "J6 low recall run clippy all targets before push." procedural rule >/dev/null
            SCENARIO_OUTPUT=$(ee_workspace search \
                "cargo fmt clippy" \
                --relevance-floor 0.05 \
                --json 2>/dev/null || true)
            ;;
        lexical_unavailable)
            local empty_index_dir
            empty_index_dir="$EPIC_WORKSPACE/j6-empty-index-lexical"
            mkdir -p "$empty_index_dir"
            SCENARIO_OUTPUT=$(ee_workspace search \
                "query" \
                --index-dir "$empty_index_dir" \
                --source-mode lexical_only \
                --json 2>/dev/null || true)
            ;;
        source_mode_fallback)
            local empty_index_dir
            empty_index_dir="$EPIC_WORKSPACE/j6-empty-index-hybrid"
            mkdir -p "$empty_index_dir"
            SCENARIO_OUTPUT=$(ee_workspace search \
                "query" \
                --index-dir "$empty_index_dir" \
                --source-mode hybrid \
                --json 2>/dev/null || true)
            ;;
        duplicates_collapsed)
            remember_j6_memory "J6 duplicate cargo fmt release marker." procedural rule >/dev/null
            remember_j6_memory "J6 duplicate cargo fmt release marker." procedural rule >/dev/null
            SCENARIO_OUTPUT=$(ee_workspace search \
                "J6 duplicate cargo fmt release marker" \
                --relevance-floor 0.0 \
                --json 2>/dev/null || true)
            ;;
        expired_filtered)
            remember_j6_memory \
                "J6 expired working note that should be filtered." \
                working \
                fact \
                --valid-to "2000-01-01T00:00:00Z" >/dev/null
            SCENARIO_OUTPUT=$(ee_workspace search \
                "J6 expired working note" \
                --relevance-floor 0.0 \
                --json 2>/dev/null || true)
            ;;
        profile_search_limit_capped)
            remember_j6_memory "J6 project memory for profile limit cap." procedural rule >/dev/null
            SCENARIO_OUTPUT=$(ee_workspace search "project" --limit 10000 --json 2>/dev/null || true)
            ;;
        tombstoned_in_results)
            local tombstone_json memory_id
            tombstone_json=$(remember_j6_memory "J6 old rule kept for audit." procedural rule)
            memory_id=$(printf '%s' "$tombstone_json" | jq -r '.data.memory_id // empty' 2>/dev/null || true)
            if [ -n "$memory_id" ]; then
                ee_workspace curate tombstone "$memory_id" \
                    --reason "j6 fixture superseded" \
                    --actor "failure_modes_e2e" \
                    --json >/dev/null 2>&1 || true
            fi
            SCENARIO_OUTPUT=$(ee_workspace search \
                "J6 old rule kept for audit" \
                --include-tombstoned \
                --relevance-floor 0.0 \
                --json 2>/dev/null || true)
            ;;
        tombstoned_filtered)
            local tombstone_json memory_id
            tombstone_json=$(remember_j6_memory "J6 old rule filtered by default." procedural rule)
            memory_id=$(printf '%s' "$tombstone_json" | jq -r '.data.memory_id // empty' 2>/dev/null || true)
            if [ -n "$memory_id" ]; then
                ee_workspace curate tombstone "$memory_id" \
                    --reason "j6 fixture superseded" \
                    --actor "failure_modes_e2e" \
                    --json >/dev/null 2>&1 || true
            fi
            SCENARIO_OUTPUT=$(ee_workspace search \
                "J6 old rule filtered by default" \
                --relevance-floor 0.0 \
                --json 2>/dev/null || true)
            ;;
        index_missing)
            local missing_dir
            missing_dir="$EPIC_WORKSPACE/j6-index-missing"
            mkdir -p "$missing_dir"
            SCENARIO_OUTPUT=$(ee_workspace search \
                "any query" \
                --index-dir "$missing_dir" \
                --json 2>/dev/null || true)
            ;;
        index_corrupt)
            local corrupt_dir
            corrupt_dir="$EPIC_WORKSPACE/j6-index-corrupt"
            mkdir -p "$corrupt_dir"
            printf '{ not-json' > "$corrupt_dir/meta.json"
            SCENARIO_OUTPUT=$(ee_workspace search \
                "any query" \
                --index-dir "$corrupt_dir" \
                --json 2>/dev/null || true)
            ;;
        index_stale)
            todo_assert "j6_${code}_fixture_exercised" "bd-17c65.10.6" \
                "Fixture documents index-status degradationCode=index_stale, not a degraded[] entry with severity; keep as catalog-only until an index-status e2e assertion path is defined."
            return 1
            ;;
        search_index_stale)
            local stale_dir
            remember_j6_memory "J6 index stale seed memory." semantic fact >/dev/null
            stale_dir="$EPIC_WORKSPACE/j6-index-stale"
            mkdir -p "$stale_dir"
            printf '{"generation":0,"lastRebuildAt":"2000-01-01T00:00:00Z"}' \
                > "$stale_dir/meta.json"
            printf 'marker\n' > "$stale_dir/document.marker"
            SCENARIO_OUTPUT=$(ee_workspace search \
                "any query" \
                --index-dir "$stale_dir" \
                --json 2>/dev/null || true)
            ;;
        policy_secret_detected_with_offsets)
            SCENARIO_OUTPUT=$(ee_workspace remember \
                "Document API_KEY=sk-FAKEabc123def456ghi789jkl012." \
                --level procedural \
                --kind rule \
                --json 2>/dev/null || true)
            ;;
        policy_tag_rejected_with_details)
            SCENARIO_OUTPUT=$(ee_workspace remember \
                "Tag rejection should be recoverable." \
                --level semantic \
                --kind fact \
                --tags "bad tag" \
                --json 2>/dev/null || true)
            ;;
        policy_bypass_used)
            SCENARIO_OUTPUT=$(ee_workspace remember \
                "Document API_KEY=sk-FAKEabc123def456ghi789jkl012." \
                --level procedural \
                --kind rule \
                --allow-secret-mention \
                --json 2>/dev/null || true)
            ;;
        model_registry_empty)
            SCENARIO_OUTPUT=$(ee_workspace model status --json 2>/dev/null || true)
            ;;
        integrity_database_missing)
            local missing_workspace
            missing_workspace="$EPIC_WORKSPACE/j6-missing-integrity-db"
            mkdir -p "$missing_workspace"
            SCENARIO_OUTPUT=$(ee_global diag integrity \
                --workspace "$missing_workspace" \
                --json 2>/dev/null || true)
            ;;
        graph_snapshot_missing)
            SCENARIO_OUTPUT=$(ee_workspace graph export --json 2>/dev/null || true)
            ;;
        mcp_feature_disabled)
            SCENARIO_OUTPUT=$(ee_global mcp manifest --json 2>/dev/null || true)
            ;;
        no_filters)
            SCENARIO_OUTPUT=$(ee_workspace causal estimate --json 2>/dev/null || true)
            ;;
        no_sources)
            SCENARIO_OUTPUT=$(ee_workspace causal compare \
                --artifact-id artifact-1 \
                --json 2>/dev/null || true)
            ;;
        causal_sample_underpowered)
            SCENARIO_OUTPUT=$(ee_workspace causal estimate \
                --artifact-id artifact-1 \
                --json 2>/dev/null || true)
            ;;
        causal_confounders_unavailable)
            SCENARIO_OUTPUT=$(ee_workspace causal estimate \
                --artifact-id artifact-1 \
                --include-confounders \
                --json 2>/dev/null || true)
            ;;
        causal_comparison_evidence_unavailable)
            SCENARIO_OUTPUT=$(ee_workspace causal compare \
                --fixture-replay-id fixture-1 \
                --json 2>/dev/null || true)
            ;;
        unknown_method)
            SCENARIO_OUTPUT=$(ee_workspace causal compare \
                --fixture-replay-id fixture-1 \
                --method made-up \
                --json 2>/dev/null || true)
            ;;
        context_evidence_freshness_changed_source)
            todo_assert "j6_${code}_fixture_exercised" "bd-17c65.10.6" \
                "Fixture trigger still requires pack replay plus direct memory mutation; no public safe e2e setup yet."
            return 1
            ;;
        *)
            todo_assert "j6_${code}_fixture_exercised" "bd-17c65.10.6" \
                "No executable scenario registered for this fixture code yet."
            return 1
            ;;
    esac
}

json_has_fixture_code() {
    local json="${1:-}"
    local code="${2:?code required}"
    printf '%s' "$json" | jq -e --arg code "$code" '
        [.. | objects | ((.code? // empty), (.detailCode? // empty))]
        | any(. == $code)
    ' >/dev/null 2>&1
}

json_fixture_severity() {
    local json="${1:-}"
    local code="${2:?code required}"
    printf '%s' "$json" | jq -r --arg code "$code" '
        if (.error.details.detailCode? // empty) == $code then
            .error.severity // empty
        else
            [.. | objects | select((.code? // empty) == $code) | (.severity? // empty)]
            | map(select(. != ""))
            | first // empty
        end
    ' 2>/dev/null || true
}

json_messages() {
    printf '%s' "${1:-}" | jq -r '[.. | objects | (.message? // empty)] | join("\n")' \
        2>/dev/null || true
}

json_repairs() {
    printf '%s' "${1:-}" | jq -r '[.. | objects | (.repair? // empty)] | map(tostring) | join("\n")' \
        2>/dev/null || true
}

assert_fixture_emission() {
    local fixture="${1:?fixture required}"
    local json="${2:-}"
    local code expected_severity label messages repairs repair_present repair_contains
    code=$(jq -r '.code' "$fixture")
    expected_severity=$(jq -r '.expected_emission.severity' "$fixture")
    repair_present=$(jq -r '.repair_present' "$fixture")
    repair_contains=$(jq -r '.expected_emission.repair_contains // empty' "$fixture")
    label=$(fixture_label "$code")

    if ! printf '%s' "$json" | jq . >/dev/null 2>&1; then
        e2e_log_assert_eq "unparseable" "json" "${label}_json_parses"
        return 0
    fi
    e2e_log_assert_eq "json" "json" "${label}_json_parses"

    if json_has_fixture_code "$json" "$code"; then
        e2e_log_assert_eq "present" "present" "${label}_code_present"
    else
        e2e_log_assert_eq "missing:$code" "present" "${label}_code_present"
    fi

    local actual_severity
    actual_severity=$(json_fixture_severity "$json" "$code")
    e2e_log_assert_eq "$actual_severity" "$expected_severity" "${label}_severity"

    messages=$(json_messages "$json")
    while IFS= read -r fragment; do
        [ -z "$fragment" ] && continue
        case "$messages" in
            *"$fragment"*) e2e_log_assert_eq "present" "present" "${label}_message_contains" ;;
            *) e2e_log_assert_eq "missing:$fragment" "present" "${label}_message_contains" ;;
        esac
    done < <(jq -r '.expected_emission.message_contains[]?' "$fixture")

    if [ "$repair_present" = "true" ] && [ -n "$repair_contains" ]; then
        repairs=$(json_repairs "$json")
        case "$repairs" in
            *"$repair_contains"*) e2e_log_assert_eq "present" "present" "${label}_repair_contains" ;;
            *) e2e_log_assert_eq "missing:$repair_contains" "present" "${label}_repair_contains" ;;
        esac
    fi
}

FIXTURE_COUNT=0
EXERCISED_COUNT=0
TODO_COUNT=0

for fixture in $(fixture_files); do
    code=$(jq -r '.code' "$fixture")
    FIXTURE_COUNT=$((FIXTURE_COUNT + 1))
    e2e_log_note "j6_fixture_start code=$code path=$fixture"
    if run_fixture_scenario "$code"; then
        EXERCISED_COUNT=$((EXERCISED_COUNT + 1))
        assert_fixture_emission "$fixture" "$SCENARIO_OUTPUT"
    else
        TODO_COUNT=$((TODO_COUNT + 1))
    fi
done

e2e_log_note "failure_mode_catalog fixtures_total=$FIXTURE_COUNT fixtures_exercised=$EXERCISED_COUNT fixtures_todo=$TODO_COUNT"
