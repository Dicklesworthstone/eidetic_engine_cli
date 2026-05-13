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
FAILURE_MODE_FILTER="${EE_FAILURE_MODE_FILTER:-}"

fixture_label() {
    printf 'j6_%s' "$1" | tr -c 'a-zA-Z0-9_' '_'
}

fixture_files() {
    find "$FIXTURE_DIR" -maxdepth 1 -type f -name '*.json' | sort
}

fixture_filter_matches() {
    local code="${1:?code required}"
    local filter="${FAILURE_MODE_FILTER//,/ }"
    local item
    [ -z "$filter" ] && return 0
    for item in $filter; do
        [ "$item" = "$code" ] && return 0
    done
    return 1
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

seed_j6_pack_slo_memories() {
    local prefix="${1:?prefix required}"
    local count="${2:?count required}"
    local base="${3:?id base required}"
    local import_dir import_file i ordinal memory_id
    import_dir="$EPIC_WORKSPACE/j6-pack-slo-import-$prefix"
    import_file="$import_dir/memories.jsonl"
    mkdir -p "$import_dir"
    printf '{"schema":"ee.export.header.v1","format_version":1,"created_at":"2026-05-13T00:00:00Z","workspace_id":"ws_j6_pack_slo","workspace_path":"/j6/pack-slo","export_scope":"memories","redaction_level":"standard","record_count":%s,"ee_version":"j6-fixture","hostname":null,"export_id":"exp_j6_%s","import_source":"native","trust_level":"validated","checksum":null,"signature":null,"source_schema_version":null}\n' \
        "$count" "$prefix" > "$import_file"
    for i in $(seq 1 "$count"); do
        ordinal=$((base + i))
        memory_id="$(printf 'mem_%026d' "$ordinal")"
        printf '{"schema":"ee.export.memory.v1","memory_id":"%s","workspace_id":"ws_j6_pack_slo","level":"procedural","kind":"rule","content":"J6 %s pack SLO marker resource saturation memory %s: keep resource-aware context assembly bounded.","importance":0.8,"confidence":0.8,"utility":0.8,"created_at":"2026-05-13T00:00:01Z","updated_at":null,"tombstoned_at":null,"tombstoned_reason":null,"valid_from":null,"valid_to":null,"expires_at":null,"source_agent":"j6-pack-slo","provenance_uri":"ee-export://j6-pack-slo/%s/%s","superseded_by":null,"supersedes":null,"redacted":false,"redaction_reason":null}\n' \
            "$memory_id" "$prefix" "$i" "$prefix" "$i" >> "$import_file"
    done
    ee_workspace import jsonl --source "$import_file" --json >/dev/null 2>&1 || true
}

hold_j6_lean_pack_slot() {
    local slot_dir slot_path ready_path holder_pid
    if ! command -v python3 >/dev/null 2>&1; then
        printf '%s\n' ""
        return 1
    fi

    slot_dir="$EPIC_WORKSPACE/.ee/pack-slots"
    slot_path="$slot_dir/lean-00.lock"
    ready_path="$slot_dir/j6-lean-holder-ready-$$"
    mkdir -p "$slot_dir"

    python3 - "$slot_path" "$ready_path" >/dev/null 2>&1 <<'PY' &
import fcntl
import pathlib
import sys
import time

slot_path = sys.argv[1]
ready_path = pathlib.Path(sys.argv[2])
with open(slot_path, "a+", encoding="utf-8") as handle:
    fcntl.flock(handle, fcntl.LOCK_EX)
    ready_path.write_text("ready\n", encoding="utf-8")
    time.sleep(30)
PY
    holder_pid=$!

    for _ in $(seq 1 100); do
        if [ -f "$ready_path" ]; then
            printf '%s\n' "$holder_pid"
            return 0
        fi
        sleep 0.05
    done

    kill "$holder_pid" 2>/dev/null || true
    wait "$holder_pid" 2>/dev/null || true
    printf '%s\n' ""
    return 1
}

release_j6_pack_slot_holder() {
    local holder_pid="${1:-}"
    if [ -n "$holder_pid" ]; then
        kill "$holder_pid" 2>/dev/null || true
        wait "$holder_pid" 2>/dev/null || true
    fi
}

write_perf_artifact() {
    local path="${1:?path required}"
    local artifact_id="${2:?artifact id required}"
    local profile_json="${3:?profile json required}"
    local fixture_tier="${4:?fixture tier required}"
    local metrics_json="${5:?metrics json required}"
    local degraded_json="${6:?degraded json required}"
    local redaction_json="${7:?redaction json required}"
    local content_hash="${8:-abc}"
    local observed_hash="${9:-$content_hash}"
    local schema="${10:-ee.perf.artifact_summary.v1}"

    cat > "$path" <<JSON
{
  "schema": "$schema",
  "artifactId": "$artifact_id",
  "artifactKind": "benchmark_report",
  "sourceSchema": "ee.bench.smoke.v1",
  "sourcePath": "redacted/$artifact_id.json",
  "contentHash": "$content_hash",
  "observedHash": "$observed_hash",
  "profile": $profile_json,
  "fixtureTier": "$fixture_tier",
  "commandFamily": "pack",
  "metrics": $metrics_json,
  "degraded": $degraded_json,
  "redaction": $redaction_json,
  "provenance": []
}
JSON
}

write_jsonl_import_memory() {
    local path="${1:?path required}"
    local export_id="${2:?export id required}"
    local memory_id="${3:?memory id required}"
    local import_source="${4:?import source required}"
    local trust_level="${5:?trust level required}"
    local content="${6:?content required}"
    local provenance_uri="${7:?provenance uri required}"
    local source_schema_version="${8:-null}"

    cat > "$path" <<JSON
{"schema":"ee.export.header.v1","format_version":1,"created_at":"2026-05-13T00:00:00Z","workspace_id":"ws_j6_source","workspace_path":"/j6/source","export_scope":"memories","redaction_level":"standard","record_count":1,"ee_version":"j6-fixture","hostname":null,"export_id":"$export_id","import_source":"$import_source","trust_level":"$trust_level","checksum":null,"signature":null,"source_schema_version":$source_schema_version}
{"schema":"ee.export.memory.v1","memory_id":"$memory_id","workspace_id":"ws_j6_source","level":"procedural","kind":"rule","content":"$content","importance":0.8,"confidence":0.8,"utility":0.8,"created_at":"2026-05-13T00:00:01Z","updated_at":null,"tombstoned_at":null,"tombstoned_reason":null,"valid_from":null,"valid_to":null,"expires_at":null,"source_agent":"j6","provenance_uri":"$provenance_uri","superseded_by":null,"supersedes":null,"redacted":false,"redaction_reason":null}
JSON
}

perf_metrics_single_elapsed='{"elapsed_ms":{"kind":"measured","value":100,"unit":"ms"}}'
perf_profile_workstation='{"profileName":"workstation","confidence":"high"}'

status_uninitialized_workspace() {
    local status_dir
    status_dir="$EPIC_WORKSPACE/j6-status-uninitialized"
    mkdir -p "$status_dir"
    ee_global --fields full status --workspace "$status_dir" --json 2>/dev/null || true
}

status_without_selected_workspace() {
    local status_dir
    status_dir="${TMPDIR:-/tmp}/ee-j6-no-workspace-status-$$"
    mkdir -p "$status_dir"
    (cd "$status_dir" && env -u EE_WORKSPACE "$EE_BINARY" status --json 2>/dev/null || true)
}

workspace_with_unopenable_database() {
    local workspace_dir="${1:?workspace dir required}"
    mkdir -p "$workspace_dir/.ee/ee.db"
}

workspace_with_quarantine_table_read_errors() {
    local workspace_dir="${1:?workspace dir required}"
    mkdir -p "$workspace_dir/.ee"
    cp "$EPIC_WORKSPACE/.ee/ee.db" "$workspace_dir/.ee/ee.db" 2>/dev/null || true
}

insert_j6_index_publish_lock() {
    ee_workspace diag advisory-lock \
        --resource-type index \
        --holder j6-index-lock-holder \
        --ttl-seconds 3600 \
        --reason "j6 index lock fixture" \
        --json >/dev/null 2>&1
}

j6_memory_id_from_json() {
    jq -r '.data.memory_id // .data.memoryId // empty' 2>/dev/null
}

create_j6_causal_memory() {
    local content="${1:?content required}"
    local kind="${2:?kind required}"
    local memory_json memory_id
    memory_json=$(remember_j6_memory "$content" episodic "$kind")
    memory_id=$(printf '%s' "$memory_json" | j6_memory_id_from_json)
    if [ -z "$memory_id" ]; then
        todo_assert "j6_causal_memory_seeded" "bd-17c65.10.6" \
            "Failed to create causal fixture memory: $content"
        return 1
    fi
    printf '%s\n' "$memory_id"
}

seed_j6_single_causal_chain() {
    local prefix="${1:?prefix required}"
    local failure_id root_id edge_id edge_json workspace_id

    failure_id=$(create_j6_causal_memory "J6 $prefix causal failure." failure) || return 1
    root_id=$(create_j6_causal_memory "J6 $prefix causal root cause." root-cause) || return 1
    edge_id="cev_j6_${prefix}_root"
    edge_json=$(ee_workspace diag causal-edge \
        --edge-id "$edge_id" \
        --failure-id "$failure_id" \
        --candidate-cause-id "$root_id" \
        --contribution-score 0.7 \
        --computed-at "2026-05-13T00:00:00Z" \
        --json 2>/dev/null) || return 1
    workspace_id=$(printf '%s' "$edge_json" | jq -r '.data.workspaceId // empty')
    if [ -z "$workspace_id" ]; then
        todo_assert "j6_causal_workspace_resolved" "bd-17c65.10.6" \
            "Failed to resolve workspace id for causal fixture memory."
        return 1
    fi
    printf '%s\t%s\t%s\n' "$failure_id" "$root_id" "$workspace_id"
}

seed_j6_pack_reference_issue() {
    ee_workspace diag pack-record \
        --pack-id pack_j6referenceissues000000000 \
        --query "j6 reference issue" \
        --profile compact \
        --max-tokens 256 \
        --used-tokens 32 \
        --item-count 1 \
        --omitted-count 0 \
        --pack-hash blake3:j6-reference-issue \
        --created-by bd-17c65.10.6 \
        --json >/dev/null 2>&1
}

seed_j6_graph_snapshot() {
    local status="${1:?status required}"
    local metrics_json="${2:?metrics json required}"
    local node_count="${3:?node count required}"
    local edge_count="${4:?edge count required}"
    ee_workspace diag graph-snapshot \
        --status "$status" \
        --metrics-json "$metrics_json" \
        --node-count "$node_count" \
        --edge-count "$edge_count" \
        --json >/dev/null 2>&1
}

seed_j6_disabled_model_registry_entry() {
    ee_workspace diag model-registry \
        --model-id mdl_j6000000000000000000000001 \
        --provider hash \
        --model-name j6-disabled-hash \
        --purpose embedding \
        --dimension 256 \
        --distance-metric cosine \
        --status disabled \
        --version j6 \
        --last-checked-at "2026-05-13T00:00:00Z" \
        --json >/dev/null 2>&1
}

seed_j6_oversized_model_registry_entry() {
    ee_workspace diag model-registry \
        --model-id mdl_j6000000000000000000000002 \
        --provider custom \
        --model-name j6-oversized-embedder \
        --purpose embedding \
        --dimension 4096 \
        --distance-metric cosine \
        --status available \
        --version j6 \
        --last-checked-at "2026-05-13T00:00:00Z" \
        --json >/dev/null 2>&1
}

seed_j6_unsupported_tripwire() {
    local tripwire_id
    tripwire_id="tw_j6_unsupported_condition"
    ee_workspace diag tripwire \
        --tripwire-id "$tripwire_id" \
        --preflight-run-id preflight_j6_unsupported \
        --tripwire-type custom \
        --condition "unsupported_condition_kind(value)" \
        --action warn \
        --state armed \
        --message "J6 unsupported condition fixture" \
        --created-at "2026-05-13T00:00:00Z" \
        --json >/dev/null 2>&1 || return 1
    printf '%s\n' "$tripwire_id"
}

seed_j6_curation_candidate() {
    local mode="${1:?mode required}"
    local variant="${2:-$mode}"
    local memory_json memory_id candidate_id status review_state ttl_policy_id state_entered_at reason
    memory_json=$(remember_j6_memory "J6 curation $mode target memory." semantic fact)
    memory_id=$(printf '%s' "$memory_json" | j6_memory_id_from_json)
    if [ -z "$memory_id" ]; then
        todo_assert "j6_curation_memory_seeded" "bd-17c65.10.6" \
            "Failed to create curation fixture target memory."
        return 1
    fi

    case "$mode" in
            harmful)
                candidate_id=$(printf 'curate_j6%024d' 1)
                status="rejected"
                review_state="rejected"
                ttl_policy_id="curation.harmful.default"
                state_entered_at="2000-01-01T00:00:00Z"
                reason="J6 harmful candidate escalation fixture"
                ;;
            missing_policy)
                if [ "$variant" = "ttl_policy_missing" ]; then
                    candidate_id=$(printf 'curate_j6%024d' 3)
                else
                    candidate_id=$(printf 'curate_j6%024d' 2)
                fi
                status="pending"
                review_state="new"
                ttl_policy_id="curation.j6.missing"
                state_entered_at="2026-05-01T00:00:00Z"
                reason="J6 missing TTL policy fixture"
                ;;
            *)
                todo_assert "j6_curation_fixture_mode_supported" "bd-17c65.10.6" \
                    "Unsupported curation fixture mode: $mode"
                return 1
                ;;
    esac

    ee_workspace diag curation-candidate \
        --candidate-id "$candidate_id" \
        --candidate-type deprecate \
        --status "$status" \
        --target-memory-id "$memory_id" \
        --source-type feedback_event \
        --source-id "j6-$variant" \
        --reason "$reason" \
        --confidence 0.8 \
        --created-at "2026-05-01T00:00:00Z" \
        --review-state "$review_state" \
        --state-entered-at "$state_entered_at" \
        --ttl-policy-id "$ttl_policy_id" \
        --json >/dev/null 2>&1 || return 1
    printf '%s\n' "$candidate_id"
}

seed_j6_changed_source_pack() {
    local marker source_path source_uri remember_json memory_id context_json pack_json pack_id
    marker="J6 changed source freshness marker"
    source_path="$EPIC_WORKSPACE/j6-changed-source.md"
    source_uri="file://$source_path#L1"

    printf '%s\n' "$marker original source evidence." > "$source_path"
    remember_json=$(remember_j6_memory \
        "$marker original source evidence." \
        procedural \
        rule \
        --tags j6,changed-source \
        --source "$source_uri")
    memory_id=$(printf '%s' "$remember_json" | j6_memory_id_from_json)
    if [ -z "$memory_id" ]; then
        todo_assert "j6_context_freshness_memory_seeded" "bd-17c65.10.6" \
            "Failed to create file-backed memory for context freshness fixture."
        return 1
    fi

    ee_workspace index rebuild --json >/dev/null 2>&1 || true
    ee_workspace context "$marker" --max-tokens 2000 --json >/dev/null 2>&1 || true

    printf '%s\n' "$marker changed source evidence." > "$source_path"
    context_json=$(ee_workspace context "$marker" --max-tokens 2000 --json 2>/dev/null || true)
    if ! json_has_fixture_code "$context_json" "context_evidence_freshness_changed_source"; then
        todo_assert "j6_context_freshness_changed_source_context_emitted" "bd-17c65.10.6" \
            "Context did not emit changed-source freshness degradation after source mutation."
        return 1
    fi

    pack_json=$(ee_workspace diag pack-latest --query "$marker" --json 2>/dev/null || true)
    pack_id=$(printf '%s' "$pack_json" | jq -r '.data.packId // empty')
    if [ -z "$pack_id" ]; then
        todo_assert "j6_context_freshness_pack_record_persisted" "bd-17c65.10.6" \
            "Failed to resolve context pack id for changed-source fixture."
        return 1
    fi

    printf '%s\n' "$pack_id"
}

seed_j6_untrusted_conflict_memory() {
    local import_dir import_file
    import_dir="$EPIC_WORKSPACE/j6-conflict-trust-mismatch"
    import_file="$import_dir/memories.jsonl"
    mkdir -p "$import_dir"

    printf '%s\n' \
        '{"schema":"ee.export.header.v1","format_version":1,"created_at":"2026-05-13T00:00:00Z","workspace_id":"ws_j6_conflict_trust","workspace_path":"/j6/conflict-trust","export_scope":"memories","redaction_level":"standard","record_count":3,"ee_version":"j6-fixture","hostname":null,"export_id":"exp_j6_conflict_trust","import_source":"native","trust_level":"untrusted","checksum":null,"signature":null,"source_schema_version":null}' \
        '{"schema":"ee.export.memory.v1","memory_id":"mem_00000000000000000000110001","workspace_id":"ws_j6_conflict_trust","level":"procedural","kind":"rule","content":"Never use HTTPS for callbacks.","importance":0.8,"confidence":0.8,"utility":0.8,"created_at":"2026-05-13T00:00:01Z","updated_at":null,"tombstoned_at":null,"tombstoned_reason":null,"valid_from":null,"valid_to":null,"expires_at":null,"source_agent":"j6-agent-assertion","provenance_uri":"file://j6/conflict-trust-mismatch.jsonl#L2","superseded_by":null,"supersedes":null,"redacted":false,"redaction_reason":null}' \
        '{"schema":"ee.export.tag.v1","memory_id":"mem_00000000000000000000110001","tag":"transport-https","created_at":"2026-05-13T00:00:02Z"}' \
        '{"schema":"ee.export.footer.v1","export_id":"exp_j6_conflict_trust","completed_at":"2026-05-13T00:00:03Z","total_records":4,"memory_count":1,"link_count":0,"tag_count":1,"audit_count":0,"checksum":null,"success":true,"error_message":null}' \
        >"$import_file"

    ee_workspace import jsonl --source "$import_file" --json >/dev/null 2>&1 || true
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
        future_validity_filtered)
            remember_j6_memory \
                "J6 future validity marker." \
                semantic \
                fact \
                --valid-from "2999-01-01T00:00:00Z" >/dev/null
            SCENARIO_OUTPUT=$(ee_workspace search \
                "future validity marker" \
                --relevance-floor 0.0 \
                --json 2>/dev/null || true)
            ;;
        validity_filtered_significant_recall_drop)
            remember_j6_memory \
                "J6 future validity marker." \
                semantic \
                fact \
                --valid-from "2999-01-01T00:00:00Z" >/dev/null
            SCENARIO_OUTPUT=$(ee_workspace search \
                "future validity marker" \
                --relevance-floor 0.0 \
                --json 2>/dev/null || true)
            ;;
        stale_validity_filtered)
            local stale_validity_json stale_validity_memory_id
            stale_validity_json=$(remember_j6_memory \
                "J6 stale validity marker." \
                semantic \
                fact \
                --valid-to "2000-01-01T00:00:00Z") || return 1
            stale_validity_memory_id=$(printf '%s' "$stale_validity_json" | j6_memory_id_from_json)
            if [ -z "$stale_validity_memory_id" ]; then
                todo_assert "j6_stale_validity_memory_seeded" "bd-17c65.10.6" \
                    "Failed to create stale validity fixture memory."
                return 1
            fi
            ee_workspace diag memory-validity \
                --memory-id "$stale_validity_memory_id" \
                --clear-valid-to \
                --json >/dev/null 2>&1 || return 1
            SCENARIO_OUTPUT=$(ee_workspace search \
                "stale validity marker" \
                --relevance-floor 0.0 \
                --json 2>/dev/null || true)
            ;;
        malformed_validity_filtered)
            local malformed_validity_json malformed_validity_memory_id
            malformed_validity_json=$(remember_j6_memory "J6 malformed validity marker." semantic fact) || return 1
            malformed_validity_memory_id=$(printf '%s' "$malformed_validity_json" | j6_memory_id_from_json)
            if [ -z "$malformed_validity_memory_id" ]; then
                todo_assert "j6_malformed_validity_memory_seeded" "bd-17c65.10.6" \
                    "Failed to create malformed validity fixture memory."
                return 1
            fi
            ee_workspace diag memory-validity \
                --memory-id "$malformed_validity_memory_id" \
                --valid-to "not-a-time" \
                --json >/dev/null 2>&1 || return 1
            SCENARIO_OUTPUT=$(ee_workspace search \
                "malformed validity marker" \
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
        tombstone_visibility_unavailable)
            local tombstone_visibility_database
            tombstone_visibility_database="$EPIC_WORKSPACE/j6-tombstone-visibility-db"
            mkdir -p "$tombstone_visibility_database"
            remember_j6_memory "J6 tombstone visibility search target." semantic fact >/dev/null
            SCENARIO_OUTPUT=$(ee_workspace search \
                "J6 tombstone visibility search target" \
                --database "$tombstone_visibility_database" \
                --index-dir "$EPIC_WORKSPACE/.ee/index" \
                --relevance-floor 0.0 \
                --json 2>/dev/null || true)
            ;;
        auto_propose_skipped_too_few_neighbors)
            SCENARIO_OUTPUT=$(ee_workspace remember \
                "One isolated fact." \
                --level semantic \
                --kind fact \
                --json 2>/dev/null || true)
            ;;
        auto_propose_deferred_to_maintenance)
            ee_workspace remember \
                "J6 deferred proposal cluster alpha." \
                --level semantic \
                --kind fact \
                --tags j6-deferred-proposal \
                --json >/dev/null 2>&1 || true
            ee_workspace remember \
                "J6 deferred proposal cluster beta." \
                --level semantic \
                --kind fact \
                --tags j6-deferred-proposal \
                --json >/dev/null 2>&1 || true
            SCENARIO_OUTPUT=$(ee_workspace remember \
                "J6 deferred proposal cluster gamma." \
                --level semantic \
                --kind fact \
                --tags j6-deferred-proposal \
                --json 2>/dev/null || true)
            ;;
        auto_propose_skipped_existing_rule_covers)
            local covering_rule_a_json covering_rule_b_json
            local covering_rule_a covering_rule_b
            covering_rule_a_json=$(ee_workspace remember \
                "Cargo release rule 0: run cargo fmt --check before release." \
                --level procedural \
                --kind rule \
                --tags cargo,release \
                --json 2>/dev/null || true)
            covering_rule_b_json=$(ee_workspace remember \
                "Cargo release rule 1: run cargo fmt --check before release." \
                --level procedural \
                --kind rule \
                --tags cargo,release \
                --json 2>/dev/null || true)
            covering_rule_a=$(printf '%s' "$covering_rule_a_json" | jq -r '.data.memory_id // empty' 2>/dev/null || true)
            covering_rule_b=$(printf '%s' "$covering_rule_b_json" | jq -r '.data.memory_id // empty' 2>/dev/null || true)
            ee_workspace rule add \
                "Run cargo fmt --check before release work." \
                --tag cargo \
                --tag release \
                --source-memory "$covering_rule_a" \
                --source-memory "$covering_rule_b" \
                --confidence 0.9 \
                --json >/dev/null 2>&1 || true
            SCENARIO_OUTPUT=$(ee_workspace remember \
                "Cargo release rule 2: run cargo fmt --check before release." \
                --level procedural \
                --kind rule \
                --tags cargo,release \
                --json 2>/dev/null || true)
            ;;
        deprecated_alias)
            SCENARIO_OUTPUT=$(ee_workspace remember \
                "Deprecated alias fixture." \
                --level semantic \
                --kind fact \
                --json 2>/dev/null || true)
            ;;
        usage_unknown_field)
            SCENARIO_OUTPUT=$(ee_global \
                --fields missingField \
                --json \
                status 2>/dev/null || true)
            ;;
        usage_conflicting_presets)
            SCENARIO_OUTPUT=$(ee_global \
                --fields minimal,summary \
                --json \
                status 2>/dev/null || true)
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
            local stale_status_dir
            remember_j6_memory "J6 index status stale seed memory." semantic fact >/dev/null
            stale_status_dir="$EPIC_WORKSPACE/j6-index-status-stale"
            mkdir -p "$stale_status_dir"
            printf '{"generation":0,"lastRebuildAt":"2000-01-01T00:00:00Z"}' \
                > "$stale_status_dir/meta.json"
            printf 'marker\n' > "$stale_status_dir/document.marker"
            SCENARIO_OUTPUT=$(ee_workspace index status \
                --index-dir "$stale_status_dir" \
                --json 2>/dev/null || true)
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
        storage_not_initialized)
            SCENARIO_OUTPUT=$(status_uninitialized_workspace)
            ;;
        search_waiting_for_storage)
            SCENARIO_OUTPUT=$(status_uninitialized_workspace)
            ;;
        memory_health_unavailable)
            SCENARIO_OUTPUT=$(status_uninitialized_workspace)
            ;;
        curation_health_unavailable)
            SCENARIO_OUTPUT=$(status_uninitialized_workspace)
            ;;
        feedback_health_unavailable)
            SCENARIO_OUTPUT=$(status_uninitialized_workspace)
            ;;
        storage_not_ready)
            local health_dir
            health_dir="$EPIC_WORKSPACE/j6-health-not-ready"
            mkdir -p "$health_dir"
            SCENARIO_OUTPUT=$(ee_global health \
                --workspace "$health_dir" \
                --json 2>/dev/null || true)
            ;;
        search_not_ready)
            local health_dir
            health_dir="$EPIC_WORKSPACE/j6-health-not-ready"
            mkdir -p "$health_dir"
            SCENARIO_OUTPUT=$(ee_global health \
                --workspace "$health_dir" \
                --json 2>/dev/null || true)
            ;;
        storage_degraded)
            local bad_storage_workspace
            bad_storage_workspace="$EPIC_WORKSPACE/j6-storage-degraded"
            workspace_with_unopenable_database "$bad_storage_workspace"
            SCENARIO_OUTPUT=$(ee_global status \
                --workspace "$bad_storage_workspace" \
                --json 2>/dev/null || true)
            ;;
        search_index_degraded)
            SCENARIO_OUTPUT=$(ee_workspace status --json 2>/dev/null || true)
            ;;
        workspace_nested_markers)
            local nested_dir
            nested_dir="$EPIC_WORKSPACE/j6-nested-workspace"
            mkdir -p "$nested_dir"
            ee_global init --workspace "$nested_dir" --json >/dev/null 2>&1 || true
            SCENARIO_OUTPUT=$(
                cd "$nested_dir" && ee_global status --json 2>/dev/null || true
            )
            ;;
        context_profile_budget_capped)
            remember_j6_memory "J6 context task memory." semantic fact >/dev/null
            SCENARIO_OUTPUT=$(ee_workspace context \
                "J6 context task" \
                --json 2>/dev/null || true)
            ;;
        degraded_context)
            remember_j6_memory "J6 context task memory." semantic fact >/dev/null
            SCENARIO_OUTPUT=$(ee_workspace context \
                "J6 context task" \
                --json 2>/dev/null || true)
            ;;
        pack_assembly_slow)
            ee_global profile config apply \
                --workspace "$EPIC_WORKSPACE" \
                --profile swarm \
                --json >/dev/null 2>&1 || true
            seed_j6_pack_slo_memories "slow" 80 7000
            SCENARIO_OUTPUT=$(ee_workspace context \
                "J6 slow pack SLO marker resource saturation" \
                --resource-profile lean \
                --candidate-pool 80 \
                --json 2>/dev/null || true)
            ;;
        pack_assembly_budget_exceeded)
            ee_global profile config apply \
                --workspace "$EPIC_WORKSPACE" \
                --profile swarm \
                --json >/dev/null 2>&1 || true
            seed_j6_pack_slo_memories "budget" 81 8000
            SCENARIO_OUTPUT=$(ee_workspace context \
                "J6 budget pack SLO marker resource saturation" \
                --resource-profile lean \
                --candidate-pool 81 \
                --json 2>/dev/null || true)
            ;;
        pack_concurrent_limit_reached)
            ee_global profile config apply \
                --workspace "$EPIC_WORKSPACE" \
                --profile swarm \
                --json >/dev/null 2>&1 || true
            seed_j6_pack_slo_memories "concurrent" 20 9000
            J6_PACK_SLOT_HOLDER_PID="$(hold_j6_lean_pack_slot)"
            if [ -n "$J6_PACK_SLOT_HOLDER_PID" ]; then
                SCENARIO_OUTPUT=$(ee_workspace context \
                    "J6 concurrent pack SLO marker resource saturation" \
                    --resource-profile lean \
                    --candidate-pool 20 \
                    --json 2>/dev/null || true)
                release_j6_pack_slot_holder "$J6_PACK_SLOT_HOLDER_PID"
            else
                SCENARIO_OUTPUT=""
            fi
            ;;
        daemon_background_mode_unimplemented)
            SCENARIO_OUTPUT=$(ee_global daemon --json 2>/dev/null || true)
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
        cass_evidence_not_available)
            SCENARIO_OUTPUT=$(ee_workspace review workspace \
                --include-cass \
                --json 2>/dev/null || true)
            ;;
        profile_mismatch)
            SCENARIO_OUTPUT=$(ee_global perf compare \
                --baseline "$REPO_ROOT/tests/fixtures/golden/perf_artifact/baseline_smoke.json" \
                --candidate "$REPO_ROOT/tests/fixtures/golden/perf_artifact/swarm_profile.json" \
                --json 2>/dev/null || true)
            ;;
        metric_missing)
            SCENARIO_OUTPUT=$(ee_global perf compare \
                --baseline "$REPO_ROOT/tests/fixtures/golden/perf_artifact/baseline_smoke.json" \
                --candidate "$REPO_ROOT/tests/fixtures/golden/perf_artifact/swarm_profile.json" \
                --json 2>/dev/null || true)
            ;;
        profile_missing)
            local perf_dir report
            perf_dir="$EPIC_WORKSPACE/j6-perf-profile-missing"
            mkdir -p "$perf_dir"
            report="$perf_dir/no-profile.json"
            write_perf_artifact \
                "$report" \
                "j6-no-profile" \
                "null" \
                "smoke" \
                "$perf_metrics_single_elapsed" \
                "[]" \
                '"clean"'
            SCENARIO_OUTPUT=$(ee_global perf budget check \
                --profile workstation \
                --report "$report" \
                --json 2>/dev/null || true)
            ;;
        missing_metric)
            local perf_dir report
            perf_dir="$EPIC_WORKSPACE/j6-perf-missing-metric"
            mkdir -p "$perf_dir"
            report="$perf_dir/no-metrics.json"
            write_perf_artifact \
                "$report" \
                "j6-no-metrics" \
                "$perf_profile_workstation" \
                "smoke" \
                "{}" \
                "[]" \
                '"clean"'
            SCENARIO_OUTPUT=$(ee_global perf budget check \
                --profile workstation \
                --report "$report" \
                --json 2>/dev/null || true)
            ;;
        fixture_tier_mismatch)
            local perf_dir baseline candidate
            perf_dir="$EPIC_WORKSPACE/j6-perf-tier-mismatch"
            mkdir -p "$perf_dir"
            baseline="$perf_dir/baseline.json"
            candidate="$perf_dir/candidate.json"
            write_perf_artifact \
                "$baseline" \
                "j6-tier-baseline" \
                "$perf_profile_workstation" \
                "smoke" \
                "$perf_metrics_single_elapsed" \
                "[]" \
                '"clean"'
            write_perf_artifact \
                "$candidate" \
                "j6-tier-candidate" \
                "$perf_profile_workstation" \
                "stress" \
                "$perf_metrics_single_elapsed" \
                "[]" \
                '"clean"'
            SCENARIO_OUTPUT=$(ee_global perf compare \
                --baseline "$baseline" \
                --candidate "$candidate" \
                --json 2>/dev/null || true)
            ;;
        tampered_hash)
            local perf_dir baseline candidate
            perf_dir="$EPIC_WORKSPACE/j6-perf-tampered"
            mkdir -p "$perf_dir"
            baseline="$perf_dir/baseline.json"
            candidate="$perf_dir/candidate.json"
            write_perf_artifact \
                "$baseline" \
                "j6-tamper-baseline" \
                "$perf_profile_workstation" \
                "smoke" \
                "$perf_metrics_single_elapsed" \
                "[]" \
                '"clean"'
            write_perf_artifact \
                "$candidate" \
                "j6-tamper-candidate" \
                "$perf_profile_workstation" \
                "smoke" \
                "$perf_metrics_single_elapsed" \
                "[]" \
                '"clean"' \
                "abc" \
                "def"
            SCENARIO_OUTPUT=$(ee_global perf compare \
                --baseline "$baseline" \
                --candidate "$candidate" \
                --json 2>/dev/null || true)
            ;;
        redaction_uncertain)
            local perf_dir baseline candidate
            perf_dir="$EPIC_WORKSPACE/j6-perf-redaction"
            mkdir -p "$perf_dir"
            baseline="$perf_dir/baseline.json"
            candidate="$perf_dir/candidate.json"
            write_perf_artifact \
                "$baseline" \
                "j6-redaction-baseline" \
                "$perf_profile_workstation" \
                "smoke" \
                "$perf_metrics_single_elapsed" \
                "[]" \
                '"clean"'
            write_perf_artifact \
                "$candidate" \
                "j6-redaction-candidate" \
                "$perf_profile_workstation" \
                "smoke" \
                "$perf_metrics_single_elapsed" \
                '[{"code":"redaction_uncertain","severity":"medium","message":"Field request.query may contain sensitive data","affectedField":"request.query"}]' \
                '"uncertain"'
            SCENARIO_OUTPUT=$(ee_global perf compare \
                --baseline "$baseline" \
                --candidate "$candidate" \
                --json 2>/dev/null || true)
            ;;
        unsupported_artifact_kind)
            local perf_dir baseline candidate
            perf_dir="$EPIC_WORKSPACE/j6-perf-unsupported-kind"
            mkdir -p "$perf_dir"
            baseline="$perf_dir/baseline.json"
            candidate="$perf_dir/candidate.json"
            write_perf_artifact \
                "$baseline" \
                "j6-unsupported-kind-baseline" \
                "$perf_profile_workstation" \
                "smoke" \
                "$perf_metrics_single_elapsed" \
                "[]" \
                '"clean"'
            write_perf_artifact \
                "$candidate" \
                "j6-unsupported-kind-candidate" \
                "$perf_profile_workstation" \
                "smoke" \
                "$perf_metrics_single_elapsed" \
                '[{"code":"unsupported_artifact_kind","severity":"high","message":"Artifact kind legacy_profiler_dump is not supported for comparison"}]' \
                '"clean"'
            SCENARIO_OUTPUT=$(ee_global perf compare \
                --baseline "$baseline" \
                --candidate "$candidate" \
                --json 2>/dev/null || true)
            ;;
        unsupported_schema)
            local perf_dir baseline candidate
            perf_dir="$EPIC_WORKSPACE/j6-perf-unsupported-schema"
            mkdir -p "$perf_dir"
            baseline="$perf_dir/baseline.json"
            candidate="$perf_dir/candidate.json"
            write_perf_artifact \
                "$baseline" \
                "j6-unsupported-schema-baseline" \
                "$perf_profile_workstation" \
                "smoke" \
                "$perf_metrics_single_elapsed" \
                "[]" \
                '"clean"'
            write_perf_artifact \
                "$candidate" \
                "j6-unsupported-schema-candidate" \
                "$perf_profile_workstation" \
                "smoke" \
                "$perf_metrics_single_elapsed" \
                "[]" \
                '"clean"' \
                "abc" \
                "abc" \
                "ee.perf.artifact_summary.v0"
            SCENARIO_OUTPUT=$(ee_global perf compare \
                --baseline "$baseline" \
                --candidate "$candidate" \
                --json 2>/dev/null || true)
            ;;
        stale_schema_version)
            local perf_dir baseline candidate
            perf_dir="$EPIC_WORKSPACE/j6-perf-stale-schema"
            mkdir -p "$perf_dir"
            baseline="$perf_dir/baseline.json"
            candidate="$perf_dir/candidate.json"
            write_perf_artifact \
                "$baseline" \
                "j6-stale-schema-baseline" \
                "$perf_profile_workstation" \
                "smoke" \
                "$perf_metrics_single_elapsed" \
                "[]" \
                '"clean"'
            write_perf_artifact \
                "$candidate" \
                "j6-stale-schema-candidate" \
                "$perf_profile_workstation" \
                "smoke" \
                "$perf_metrics_single_elapsed" \
                '[{"code":"stale_schema_version","severity":"medium","message":"Artifact uses older schema version: ee.perf.artifact_summary.v0","repair":"Re-export artifact with current ee version"}]' \
                '"clean"'
            SCENARIO_OUTPUT=$(ee_global perf compare \
                --baseline "$baseline" \
                --candidate "$candidate" \
                --json 2>/dev/null || true)
            ;;
        model_registry_empty)
            SCENARIO_OUTPUT=$(ee_workspace model status --json 2>/dev/null || true)
            ;;
        advisory_memory)
            local advisory_import_dir advisory_import_file
            advisory_import_dir="$EPIC_WORKSPACE/j6-advisory-import"
            advisory_import_file="$advisory_import_dir/advisory.jsonl"
            mkdir -p "$advisory_import_dir"
            write_jsonl_import_memory \
                "$advisory_import_file" \
                "exp_j6_advisory" \
                "mem_01KRFR9EB0E61T8WV4Y952ZY13" \
                "native" \
                "untrusted" \
                "J6 advisory imported memory marker." \
                "cass-session://j6-advisory"
            ee_workspace import jsonl --source "$advisory_import_file" --json >/dev/null 2>&1 || true
            ee_workspace index rebuild --json >/dev/null 2>&1 || true
            SCENARIO_OUTPUT=$(ee_workspace pack \
                "J6 advisory imported memory marker." \
                --json 2>/dev/null || true)
            ;;
        legacy_memory)
            local legacy_import_dir legacy_import_file
            legacy_import_dir="$EPIC_WORKSPACE/j6-legacy-import"
            legacy_import_file="$legacy_import_dir/legacy.jsonl"
            mkdir -p "$legacy_import_dir"
            write_jsonl_import_memory \
                "$legacy_import_file" \
                "exp_j6_legacy" \
                "mem_01KRFR9EB0E61T8WV4Y952ZY14" \
                "legacy_scan" \
                "untrusted" \
                "J6 legacy imported memory marker." \
                "cass-session://j6-legacy" \
                '"pre-v1"'
            ee_workspace import jsonl --source "$legacy_import_file" --json >/dev/null 2>&1 || true
            SCENARIO_OUTPUT=$(ee_workspace pack \
                "J6 legacy imported memory marker" \
                --json 2>/dev/null || true)
            ;;
        cass_unavailable)
            SCENARIO_OUTPUT=$(EE_CASS_BINARY=/missing/cass ee_workspace import cass \
                --dry-run \
                --json 2>/dev/null || true)
            ;;
        integrity_database_missing)
            local missing_workspace
            missing_workspace="$EPIC_WORKSPACE/j6-missing-integrity-db"
            mkdir -p "$missing_workspace"
            SCENARIO_OUTPUT=$(ee_global diag integrity \
                --workspace "$missing_workspace" \
                --json 2>/dev/null || true)
            ;;
        integrity_database_open_failed)
            local bad_integrity_workspace
            bad_integrity_workspace="$EPIC_WORKSPACE/j6-integrity-open-failed"
            workspace_with_unopenable_database "$bad_integrity_workspace"
            SCENARIO_OUTPUT=$(ee_global diag integrity \
                --workspace "$bad_integrity_workspace" \
                --json 2>/dev/null || true)
            ;;
        graph_snapshot_missing)
            SCENARIO_OUTPUT=$(ee_workspace graph export --json 2>/dev/null || true)
            ;;
        mcp_feature_disabled)
            SCENARIO_OUTPUT=$(ee_global mcp manifest --json 2>/dev/null || true)
            ;;
        git_unavailable)
            SCENARIO_OUTPUT=$(PATH=/nonexistent ee_global swarm brief \
                --sources git \
                --workspace "$REPO_ROOT" \
                --json 2>/dev/null || true)
            ;;
        beads_unavailable)
            SCENARIO_OUTPUT=$(PATH=/usr/bin:/bin ee_global swarm brief \
                --sources beads \
                --workspace "$REPO_ROOT" \
                --json 2>/dev/null || true)
            ;;
        bv_unavailable)
            SCENARIO_OUTPUT=$(PATH=/usr/bin:/bin ee_global swarm brief \
                --sources bv \
                --workspace "$REPO_ROOT" \
                --json 2>/dev/null || true)
            ;;
        rch_unavailable)
            SCENARIO_OUTPUT=$(PATH=/usr/bin:/bin ee_global swarm brief \
                --sources rch \
                --workspace "$REPO_ROOT" \
                --json 2>/dev/null || true)
            ;;
        agent_mail_unavailable)
            SCENARIO_OUTPUT=$(ee_global swarm brief \
                --sources agent-mail \
                --workspace "$REPO_ROOT" \
                --json 2>/dev/null || true)
            ;;
        no_filters)
            SCENARIO_OUTPUT=$(ee_workspace causal estimate --json 2>/dev/null || true)
            ;;
        no_sources)
            SCENARIO_OUTPUT=$(ee_workspace causal compare \
                --artifact-id artifact-1 \
                --method replay \
                --json 2>/dev/null || true)
            ;;
        causal_sample_underpowered)
            SCENARIO_OUTPUT=$(ee_workspace causal promote-plan \
                --artifact-id artifact-1 \
                --json 2>/dev/null || true)
            ;;
        dry_run_recommended)
            SCENARIO_OUTPUT=$(ee_workspace causal promote-plan \
                --artifact-id artifact-1 \
                --json 2>/dev/null || true)
            ;;
        action_override_not_actionable)
            SCENARIO_OUTPUT=$(ee_workspace causal promote-plan \
                --artifact-id artifact-1 \
                --action promote \
                --json 2>/dev/null || true)
            ;;
        causal_ledger_empty)
            SCENARIO_OUTPUT=$(ee_workspace causal estimate \
                --artifact-id artifact-1 \
                --json 2>/dev/null || true)
            ;;
        stable_unit)
            SCENARIO_OUTPUT=$(ee_workspace causal estimate \
                --artifact-id artifact-1 \
                --method naive \
                --include-assumptions \
                --json 2>/dev/null || true)
            ;;
        no_confounders)
            SCENARIO_OUTPUT=$(ee_workspace causal estimate \
                --artifact-id artifact-1 \
                --method naive \
                --include-assumptions \
                --json 2>/dev/null || true)
            ;;
        conditional_independence)
            SCENARIO_OUTPUT=$(ee_workspace causal estimate \
                --artifact-id artifact-1 \
                --method matching \
                --include-assumptions \
                --json 2>/dev/null || true)
            ;;
        replay_fidelity)
            SCENARIO_OUTPUT=$(ee_workspace causal estimate \
                --artifact-id artifact-1 \
                --method replay \
                --include-assumptions \
                --json 2>/dev/null || true)
            ;;
        proper_randomization)
            SCENARIO_OUTPUT=$(ee_workspace causal estimate \
                --artifact-id artifact-1 \
                --method experiment \
                --include-assumptions \
                --json 2>/dev/null || true)
            ;;
        causal_chain_not_found)
            SCENARIO_OUTPUT=$(ee_workspace causal promote-plan \
                trace_missing_chain \
                --json 2>/dev/null || true)
            ;;
        causal_failure_id_required)
            SCENARIO_OUTPUT=$(ee_workspace causal trace --json 2>/dev/null || true)
            ;;
        causal_evidence_unavailable)
            SCENARIO_OUTPUT=$(ee_workspace causal trace \
                failure-memory-1 \
                --json 2>/dev/null || true)
            ;;
        causal_chain_id_required)
            SCENARIO_OUTPUT=$(ee_workspace causal estimate \
                --chain-id "" \
                --json 2>/dev/null || true)
            ;;
        causal_chain_pair_required)
            SCENARIO_OUTPUT=$(ee_workspace causal compare \
                chain-a \
                "" \
                --json 2>/dev/null || true)
            ;;
        drift_no_evaluation_snapshots)
            SCENARIO_OUTPUT=$(ee_workspace analyze drift --json 2>/dev/null || true)
            ;;
        drift_no_comparable_metrics)
            local snapshot_dir baseline_snapshot current_snapshot
            snapshot_dir="$EPIC_WORKSPACE/j6-drift-snapshots"
            mkdir -p "$snapshot_dir"
            baseline_snapshot="$snapshot_dir/baseline.json"
            current_snapshot="$snapshot_dir/current.json"
            printf '{"id":"baseline","note":"no numeric metrics"}\n' > "$baseline_snapshot"
            printf '{"id":"current","note":"still no numeric metrics"}\n' > "$current_snapshot"
            SCENARIO_OUTPUT=$(ee_workspace analyze drift \
                --baseline "$baseline_snapshot" \
                --current "$current_snapshot" \
                --json 2>/dev/null || true)
            ;;
        clustering_no_candidates)
            SCENARIO_OUTPUT=$(ee_workspace analyze clustering --json 2>/dev/null || true)
            ;;
        maintenance_job_cancelled)
            SCENARIO_OUTPUT=$(ee_workspace job run decay_sweep \
                --item-limit 0 \
                --json 2>/dev/null || true)
            ;;
        maintenance_job_failed)
            SCENARIO_OUTPUT=$(ee_workspace job run decay_sweep \
                --database "$EPIC_WORKSPACE/j6-missing-maintenance.db" \
                --dry-run \
                --json 2>/dev/null || true)
            ;;
        decay_sweep_database_missing)
            SCENARIO_OUTPUT=$(ee_workspace job run decay_sweep \
                --database "$EPIC_WORKSPACE/j6-missing-decay-sweep.db" \
                --dry-run \
                --json 2>/dev/null || true)
            ;;
        decay_sweep_database_open_failed)
            local bad_decay_workspace
            bad_decay_workspace="$EPIC_WORKSPACE/j6-decay-open-failed"
            workspace_with_unopenable_database "$bad_decay_workspace"
            SCENARIO_OUTPUT=$(ee_global job run decay_sweep \
                --workspace "$bad_decay_workspace" \
                --dry-run \
                --json 2>/dev/null || true)
            ;;
        decay_sweep_workspace_unresolved)
            local unresolved_decay_database
            unresolved_decay_database="$EPIC_WORKSPACE/j6-decay-workspace-unresolved.db"
            ee_workspace diag database-skew \
                --output-database "$unresolved_decay_database" \
                --skew workspaces-table-unavailable \
                --json >/dev/null 2>&1 || return 1
            SCENARIO_OUTPUT=$(ee_global job run decay_sweep \
                --database "$unresolved_decay_database" \
                --dry-run \
                --json 2>/dev/null || true)
            ;;
        decay_sweep_item_limit_too_large)
            SCENARIO_OUTPUT=$(ee_workspace job run decay_sweep \
                --item-limit 4294967296 \
                --dry-run \
                --json 2>/dev/null || true)
            ;;
        learn_decay_config_invalid)
            local invalid_decay_config_workspace
            invalid_decay_config_workspace="$EPIC_WORKSPACE/j6-learn-decay-config-invalid"
            ee_global init --workspace "$invalid_decay_config_workspace" --json >/dev/null
            printf '[learn.decay\n' > "$invalid_decay_config_workspace/.ee/config.toml"
            SCENARIO_OUTPUT=$(ee_global maintenance run \
                --workspace "$invalid_decay_config_workspace" \
                --include-decay \
                --json 2>/dev/null || true)
            ;;
        learn_decay_config_read_failed)
            local unreadable_decay_config_workspace
            unreadable_decay_config_workspace="$EPIC_WORKSPACE/j6-learn-decay-config-read-failed"
            ee_global init --workspace "$unreadable_decay_config_workspace" --json >/dev/null
            mkdir -p "$unreadable_decay_config_workspace/.ee/config.toml"
            SCENARIO_OUTPUT=$(ee_global maintenance run \
                --workspace "$unreadable_decay_config_workspace" \
                --include-decay \
                --json 2>/dev/null || true)
            ;;
        maintenance_job_skipped)
            SCENARIO_OUTPUT=$(ee_workspace job run custom --json 2>/dev/null || true)
            ;;
        maintenance_job_history_write_failed)
            local history_blocking_workspace history_blocking_path
            history_blocking_workspace="$EPIC_WORKSPACE/j6-maintenance-history-write-failed"
            mkdir -p "$history_blocking_workspace/.ee"
            history_blocking_path="$history_blocking_workspace/.ee/maintenance-jobs.jsonl"
            mkdir -p "$history_blocking_path"
            SCENARIO_OUTPUT=$(ee_global job run custom \
                --workspace "$history_blocking_workspace" \
                --json 2>/dev/null || true)
            ;;
        maintenance_job_lock_open_failed)
            local lock_open_workspace
            lock_open_workspace="$EPIC_WORKSPACE/j6-maintenance-lock-open-failed"
            mkdir -p "$lock_open_workspace"
            printf 'not a directory' > "$lock_open_workspace/.ee"
            SCENARIO_OUTPUT=$(ee_global job run custom \
                --workspace "$lock_open_workspace" \
                --json 2>/dev/null || true)
            ;;
        maintenance_job_lock_busy)
            local lock_busy_workspace lock_path ready_path locker_pid
            if ! command -v perl >/dev/null 2>&1; then
                todo_assert "j6_${code}_requires_perl_flock_holder" "bd-17c65.10.6" \
                    "Fixture requires a tiny Perl flock holder to keep the maintenance lock busy."
                return 1
            fi
            lock_busy_workspace="$EPIC_WORKSPACE/j6-maintenance-lock-busy"
            mkdir -p "$lock_busy_workspace/.ee"
            lock_path="$lock_busy_workspace/.ee/maintenance-job.lock"
            ready_path="$lock_busy_workspace/.ee/maintenance-job-lock-ready"
            perl -MFcntl=:flock -e \
                'open my $fh, ">>", $ARGV[0] or die $!; flock($fh, LOCK_EX) or die $!; print "ready\n"; sleep 10' \
                "$lock_path" > "$ready_path" &
            locker_pid=$!
            for _ in 1 2 3 4 5 6 7 8 9 10; do
                [ -s "$ready_path" ] && break
                sleep 0.1
            done
            SCENARIO_OUTPUT=$(ee_global job run custom \
                --workspace "$lock_busy_workspace" \
                --json 2>/dev/null || true)
            kill "$locker_pid" >/dev/null 2>&1 || true
            wait "$locker_pid" 2>/dev/null || true
            ;;
        maintenance_job_since_invalid)
            SCENARIO_OUTPUT=$(ee_workspace job list \
                --since not-a-date \
                --json 2>/dev/null || true)
            ;;
        maintenance_job_not_found)
            SCENARIO_OUTPUT=$(ee_workspace job show \
                missing-job-id \
                --json 2>/dev/null || true)
            ;;
        verification_evidence_not_found)
            local verify_json verify_memory_id
            verify_json=$(remember_j6_memory \
                "J6 verification-sensitive claim." \
                semantic \
                fact \
                --tags verification-required)
            verify_memory_id=$(printf '%s' "$verify_json" | jq -r '.data.memory_id // empty' 2>/dev/null || true)
            SCENARIO_OUTPUT=$(ee_workspace why \
                "$verify_memory_id" \
                --json 2>/dev/null || true)
            ;;
        why_result_target_unsupported_source)
            SCENARIO_OUTPUT=$(ee_workspace why \
                "result:not-memory-doc-id" \
                --json 2>/dev/null || true)
            ;;
        graph_query_relative_features_unavailable)
            local why_json memory_id
            why_json=$(remember_j6_memory "J6 graph why target." semantic fact)
            memory_id=$(printf '%s' "$why_json" | jq -r '.data.memory_id // empty' 2>/dev/null || true)
            SCENARIO_OUTPUT=$(ee_workspace why \
                "$memory_id" \
                --json 2>/dev/null || true)
            ;;
        graph_memory_not_in_snapshot)
            local graph_memory_a_json graph_memory_b_json graph_memory_new_json
            local graph_memory_a graph_memory_b graph_memory_new
            graph_memory_a_json=$(remember_j6_memory "J6 graph snapshot seed alpha." semantic fact)
            graph_memory_b_json=$(remember_j6_memory "J6 graph snapshot seed beta." semantic fact)
            graph_memory_a=$(printf '%s' "$graph_memory_a_json" | jq -r '.data.memory_id // empty' 2>/dev/null || true)
            graph_memory_b=$(printf '%s' "$graph_memory_b_json" | jq -r '.data.memory_id // empty' 2>/dev/null || true)
            ee_workspace memory link \
                "$graph_memory_a" \
                "$graph_memory_b" \
                --relation related \
                --undirected \
                --json >/dev/null 2>&1 || true
            ee_workspace graph centrality-refresh --json >/dev/null 2>&1 || true
            graph_memory_new_json=$(remember_j6_memory "J6 memory created after graph snapshot." semantic fact)
            graph_memory_new=$(printf '%s' "$graph_memory_new_json" | jq -r '.data.memory_id // empty' 2>/dev/null || true)
            SCENARIO_OUTPUT=$(ee_workspace why \
                "$graph_memory_new" \
                --json 2>/dev/null || true)
            ;;
        preflight_evidence_unavailable)
            SCENARIO_OUTPUT=$(ee_workspace preflight run \
                "risky task" \
                --json 2>/dev/null || true)
            ;;
        quarantine_workspace_unavailable)
            local missing_quarantine_workspace
            missing_quarantine_workspace="$EPIC_WORKSPACE/j6-missing-quarantine-workspace"
            SCENARIO_OUTPUT=$(ee_global diag quarantine list \
                --workspace "$missing_quarantine_workspace" \
                --json 2>/dev/null || true)
            ;;
        quarantine_database_missing)
            local missing_quarantine_db_workspace
            missing_quarantine_db_workspace="$EPIC_WORKSPACE/j6-missing-quarantine-db"
            mkdir -p "$missing_quarantine_db_workspace"
            SCENARIO_OUTPUT=$(ee_global diag quarantine list \
                --workspace "$missing_quarantine_db_workspace" \
                --json 2>/dev/null || true)
            ;;
        quarantine_database_unreadable)
            local unreadable_quarantine_db_workspace
            unreadable_quarantine_db_workspace="$EPIC_WORKSPACE/j6-unreadable-quarantine-db"
            mkdir -p "$unreadable_quarantine_db_workspace/.ee"
            printf 'not sqlite' > "$unreadable_quarantine_db_workspace/.ee/ee.db"
            SCENARIO_OUTPUT=$(ee_global diag quarantine list \
                --workspace "$unreadable_quarantine_db_workspace" \
                --json 2>/dev/null || true)
            ;;
        quarantine_feedback_events_unreadable)
            local unreadable_feedback_events_workspace
            unreadable_feedback_events_workspace="$EPIC_WORKSPACE/j6-unreadable-feedback-events"
            workspace_with_quarantine_table_read_errors "$unreadable_feedback_events_workspace"
            SCENARIO_OUTPUT=$(ee_global diag quarantine list \
                --workspace "$unreadable_feedback_events_workspace" \
                --json 2>/dev/null || true)
            ;;
        quarantine_rows_unreadable)
            local unreadable_quarantine_rows_workspace
            unreadable_quarantine_rows_workspace="$EPIC_WORKSPACE/j6-unreadable-quarantine-rows"
            workspace_with_quarantine_table_read_errors "$unreadable_quarantine_rows_workspace"
            SCENARIO_OUTPUT=$(ee_global diag quarantine list \
                --workspace "$unreadable_quarantine_rows_workspace" \
                --json 2>/dev/null || true)
            ;;
        trust_quarantine_rows_unreadable)
            local unreadable_trust_quarantine_workspace
            unreadable_trust_quarantine_workspace="$EPIC_WORKSPACE/j6-unreadable-trust-quarantine"
            workspace_with_quarantine_table_read_errors "$unreadable_trust_quarantine_workspace"
            SCENARIO_OUTPUT=$(ee_global diag quarantine list \
                --workspace "$unreadable_trust_quarantine_workspace" \
                --json 2>/dev/null || true)
            ;;
        tripwire_inputs_incomplete)
            SCENARIO_OUTPUT=$(ee_workspace tripwire check \
                tripwire-1 \
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
            SCENARIO_OUTPUT=$(ee_workspace causal promote-plan \
                --artifact-id artifact-1 \
                --method made-up \
                --json 2>/dev/null || true)
            ;;
        agent_detection_unavailable)
            SCENARIO_OUTPUT=$(ee_workspace agent status \
                --only not-a-real-agent \
                --json 2>/dev/null || true)
            ;;
        agent_status_unavailable)
            SCENARIO_OUTPUT=$(ee_workspace swarm brief \
                --sources agent-inventory \
                --agent-inventory-only not-a-real-agent \
                --json 2>/dev/null || true)
            ;;
        auto_link_disabled)
            SCENARIO_OUTPUT=$(ee_workspace remember \
                "J6 workflow-less auto-link fixture." \
                --level procedural \
                --kind rule \
                --json 2>/dev/null || true)
            ;;
        auto_propose_failed)
            local auto_propose_skew_workspace
            auto_propose_skew_workspace="$EPIC_WORKSPACE/j6-auto-propose-failed"
            ee_global init --workspace "$auto_propose_skew_workspace" --json >/dev/null
            ee_global remember "J6 auto propose seed one." \
                --workspace "$auto_propose_skew_workspace" \
                --level semantic \
                --kind fact \
                --tags j6-auto-propose \
                --no-auto-link \
                --no-propose-candidates \
                --json >/dev/null 2>&1 || true
            ee_global remember "J6 auto propose seed two." \
                --workspace "$auto_propose_skew_workspace" \
                --level semantic \
                --kind fact \
                --tags j6-auto-propose \
                --no-auto-link \
                --no-propose-candidates \
                --json >/dev/null 2>&1 || true
            printf '[learn]\ncluster_coherence_threshold = 2.0\n' \
                >"$auto_propose_skew_workspace/.ee/config.toml"
            SCENARIO_OUTPUT=$(EE_REMEMBER_CURATION_SYNC_BUDGET_MS=100000 \
                ee_global remember "J6 auto propose triggering memory." \
                    --workspace "$auto_propose_skew_workspace" \
                    --level semantic \
                    --kind fact \
                    --tags j6-auto-propose \
                    --no-auto-link \
                    --json 2>/dev/null || true)
            ;;
        auto_propose_search_neighbor_lookup_failed)
            local auto_propose_neighbor_workspace
            auto_propose_neighbor_workspace="$EPIC_WORKSPACE/j6-auto-propose-neighbor-unavailable"
            ee_global init --workspace "$auto_propose_neighbor_workspace" --json >/dev/null
            ee_global remember "J6 auto propose neighbor seed." \
                --workspace "$auto_propose_neighbor_workspace" \
                --level semantic \
                --kind fact \
                --tags j6-auto-propose-neighbor \
                --no-auto-link \
                --no-propose-candidates \
                --json >/dev/null 2>&1 || true
            SCENARIO_OUTPUT=$(EE_DISABLE_REMEMBER_SEARCH_NEIGHBORS=1 \
                ee_global remember "J6 auto propose neighbor triggering memory." \
                    --workspace "$auto_propose_neighbor_workspace" \
                    --level semantic \
                    --kind fact \
                    --tags j6-auto-propose-neighbor \
                    --no-auto-link \
                    --json 2>/dev/null || true)
            ;;
        causal_database_migration_failed)
            local causal_migration_skew_database causal_migration_skew_json causal_migration_workspace_id
            causal_migration_skew_database="$EPIC_WORKSPACE/j6-causal-migration-skew.db"
            causal_migration_skew_json=$(ee_workspace diag database-skew \
                --output-database "$causal_migration_skew_database" \
                --skew migration-checksum-column-missing \
                --json 2>/dev/null) || return 1
            causal_migration_workspace_id=$(printf '%s' "$causal_migration_skew_json" |
                jq -r '.data.workspaceId // empty')
            SCENARIO_OUTPUT=$(ee_workspace causal trace failure-1 \
                --database "$causal_migration_skew_database" \
                --database-workspace-id "$causal_migration_workspace_id" \
                --json 2>/dev/null || true)
            ;;
        causal_database_missing)
            SCENARIO_OUTPUT=$(ee_workspace causal trace failure-1 \
                --database "$EPIC_WORKSPACE/j6-missing-causal.db" \
                --database-workspace-id "wsp_j6_causal_replay" \
                --json 2>/dev/null || true)
            ;;
        causal_database_open_failed)
            local causal_bad_database
            causal_bad_database="$EPIC_WORKSPACE/j6-unopenable-causal.db"
            mkdir -p "$causal_bad_database"
            SCENARIO_OUTPUT=$(ee_workspace causal trace failure-1 \
                --database "$causal_bad_database" \
                --database-workspace-id "wsp_j6_causal_replay" \
                --json 2>/dev/null || true)
            ;;
        causal_evidence_table_missing)
            local causal_missing_table_database causal_missing_table_json causal_missing_table_workspace_id
            causal_missing_table_database="$EPIC_WORKSPACE/j6-causal-missing-table.db"
            causal_missing_table_json=$(ee_workspace diag database-skew \
                --output-database "$causal_missing_table_database" \
                --skew causal-evidence-table-missing \
                --json 2>/dev/null) || return 1
            causal_missing_table_workspace_id=$(printf '%s' "$causal_missing_table_json" |
                jq -r '.data.workspaceId // empty')
            SCENARIO_OUTPUT=$(ee_workspace causal trace failure-1 \
                --database "$causal_missing_table_database" \
                --database-workspace-id "$causal_missing_table_workspace_id" \
                --json 2>/dev/null || true)
            ;;
        causal_insufficient_chains)
            local causal_pair causal_root_id
            causal_pair=$(seed_j6_single_causal_chain "insufficient") || return 1
            causal_root_id=$(printf '%s' "$causal_pair" | awk '{print $2}')
            SCENARIO_OUTPUT=$(ee_workspace causal compare \
                --artifact-id "$causal_root_id" \
                --json 2>/dev/null || true)
            ;;
        causal_no_matching_chains)
            seed_j6_single_causal_chain "no_matching" >/dev/null || return 1
            SCENARIO_OUTPUT=$(ee_workspace causal estimate \
                --artifact-id mem_00000000000000000000000000 \
                --json 2>/dev/null || true)
            ;;
        causal_trace_store_failed)
            local causal_cycle_pair causal_cycle_failure_id causal_cycle_root_id causal_cycle_workspace_id
            causal_cycle_pair=$(seed_j6_single_causal_chain "trace_store_failed") || return 1
            causal_cycle_failure_id=$(printf '%s' "$causal_cycle_pair" | awk '{print $1}')
            causal_cycle_root_id=$(printf '%s' "$causal_cycle_pair" | awk '{print $2}')
            causal_cycle_workspace_id=$(printf '%s' "$causal_cycle_pair" | awk '{print $3}')
            ee_workspace diag causal-edge \
                --edge-id cev_j6_trace_store_cycle \
                --failure-id "$causal_cycle_root_id" \
                --candidate-cause-id "$causal_cycle_failure_id" \
                --contribution-score 0.4 \
                --computed-at "2026-05-13T00:00:01Z" \
                --json >/dev/null 2>&1 || return 1
            SCENARIO_OUTPUT=$(ee_workspace causal trace \
                "$causal_cycle_failure_id" \
                --database "$EPIC_WORKSPACE/.ee/ee.db" \
                --database-workspace-id "$causal_cycle_workspace_id" \
                --json 2>/dev/null || true)
            ;;
        causal_workspace_id_required)
            SCENARIO_OUTPUT=$(ee_workspace causal trace failure-1 \
                --database "$EPIC_WORKSPACE/.ee/ee.db" \
                --json 2>/dev/null || true)
            ;;
        clustering_insufficient_data)
            local clustering_empty_workspace
            clustering_empty_workspace="$EPIC_WORKSPACE/j6-clustering-insufficient-data"
            ee_global init --workspace "$clustering_empty_workspace" --json >/dev/null
            SCENARIO_OUTPUT=$(ee_global learn cluster \
                --workspace "$clustering_empty_workspace" \
                --json 2>/dev/null || true)
            ;;
        clustering_no_embeddings)
            local clustering_workspace
            clustering_workspace="$EPIC_WORKSPACE/j6-clustering-no-embeddings"
            ee_global init --workspace "$clustering_workspace" --json >/dev/null
            ee_global diag curation-candidate \
                --workspace "$clustering_workspace" \
                --allow-missing-target \
                --candidate-type rule \
                --status pending \
                --json >/dev/null 2>&1 || true
            SCENARIO_OUTPUT=$(ee_global analyze clustering \
                --workspace "$clustering_workspace" \
                --candidate-type rule \
                --status pending \
                --json 2>/dev/null || true)
            ;;
        clustering_threshold_too_strict)
            remember_j6_memory "J6 strict cluster singleton seed."
            SCENARIO_OUTPUT=$(ee_workspace learn cluster --json 2>/dev/null || true)
            ;;
        curation_harmful_candidate_escalated)
            seed_j6_curation_candidate harmful >/dev/null || return 1
            SCENARIO_OUTPUT=$(ee_workspace curate disposition \
                --now "2026-05-13T00:00:00Z" \
                --json 2>/dev/null || true)
            ;;
        curation_ttl_blocked)
            seed_j6_curation_candidate missing_policy ttl_blocked >/dev/null || return 1
            SCENARIO_OUTPUT=$(ee_workspace --fields full status --json 2>/dev/null || true)
            ;;
        curation_ttl_policy_missing)
            seed_j6_curation_candidate missing_policy ttl_policy_missing >/dev/null || return 1
            SCENARIO_OUTPUT=$(ee_workspace curate disposition \
                --now "2026-05-13T00:00:00Z" \
                --json 2>/dev/null || true)
            ;;
        curation_ttl_policy_unavailable)
            local curation_ttl_skew_workspace
            curation_ttl_skew_workspace="$EPIC_WORKSPACE/j6-curation-ttl-policy-unavailable"
            mkdir -p "$curation_ttl_skew_workspace/.ee"
            ee_workspace diag database-skew \
                --output-database "$curation_ttl_skew_workspace/.ee/ee.db" \
                --skew curation-ttl-policies-table-unavailable \
                --json >/dev/null 2>&1 || return 1
            SCENARIO_OUTPUT=$(ee_global --fields full status \
                --workspace "$curation_ttl_skew_workspace" \
                --json 2>/dev/null || true)
            ;;
        decay_sweep_database_unresolved)
            local unresolved_job_workspace
            unresolved_job_workspace="$EPIC_WORKSPACE/j6-decay-database-unresolved"
            mkdir -p "$unresolved_job_workspace"
            SCENARIO_OUTPUT=$(
                cd "$unresolved_job_workspace" &&
                    env -u EE_WORKSPACE "$EE_BINARY" job run decay_sweep --json 2>/dev/null || true
            )
            ;;
        decay_sweep_handler_failed)
            local decay_handler_skew_database
            decay_handler_skew_database="$EPIC_WORKSPACE/j6-decay-handler-skew.db"
            ee_workspace diag database-skew \
                --output-database "$decay_handler_skew_database" \
                --skew decay-memories-table-unavailable \
                --json >/dev/null 2>&1 || return 1
            SCENARIO_OUTPUT=$(ee_workspace job run decay_sweep \
                --database "$decay_handler_skew_database" \
                --json 2>/dev/null || true)
            ;;
        decay_sweep_migration_failed)
            local decay_migration_skew_database
            decay_migration_skew_database="$EPIC_WORKSPACE/j6-decay-migration-skew.db"
            ee_workspace diag database-skew \
                --output-database "$decay_migration_skew_database" \
                --skew migration-checksum-column-missing \
                --json >/dev/null 2>&1 || return 1
            SCENARIO_OUTPUT=$(ee_workspace job run decay_sweep \
                --database "$decay_migration_skew_database" \
                --json 2>/dev/null || true)
            ;;
        diagram_backend_unavailable)
            SCENARIO_OUTPUT=$(ee_workspace diag dependencies --json 2>/dev/null || true)
            ;;
        drift_analysis_unavailable)
            SCENARIO_OUTPUT=$(EE_SCIENCE_BACKEND_PATH="$EPIC_WORKSPACE/j6-missing-science-backend" \
                ee_workspace analyze drift --json 2>/dev/null || true)
            ;;
        feedback_protected_rules_unavailable)
            local feedback_protected_skew_workspace
            feedback_protected_skew_workspace="$EPIC_WORKSPACE/j6-feedback-protected-rules-unavailable"
            mkdir -p "$feedback_protected_skew_workspace/.ee"
            ee_workspace diag database-skew \
                --output-database "$feedback_protected_skew_workspace/.ee/ee.db" \
                --skew procedural-rules-table-unavailable \
                --json >/dev/null 2>&1 || return 1
            SCENARIO_OUTPUT=$(ee_global --fields full status \
                --workspace "$feedback_protected_skew_workspace" \
                --json 2>/dev/null || true)
            ;;
        feedback_quarantine_unavailable)
            local feedback_quarantine_skew_workspace
            feedback_quarantine_skew_workspace="$EPIC_WORKSPACE/j6-feedback-quarantine-unavailable"
            mkdir -p "$feedback_quarantine_skew_workspace/.ee"
            ee_workspace diag database-skew \
                --output-database "$feedback_quarantine_skew_workspace/.ee/ee.db" \
                --skew feedback-quarantine-table-unavailable \
                --json >/dev/null 2>&1 || return 1
            SCENARIO_OUTPUT=$(ee_global --fields full status \
                --workspace "$feedback_quarantine_skew_workspace" \
                --json 2>/dev/null || true)
            ;;
        graph_feature_disabled)
            SCENARIO_OUTPUT=$(EE_DIAG_FORCE_CAPABILITY_GAP=graph \
                ee_workspace status --fields full --json 2>/dev/null || true)
            ;;
        graph_snapshot_scores_unavailable)
            seed_j6_graph_snapshot valid '{"nodes":[],"edges":[]}' 0 0 || return 1
            SCENARIO_OUTPUT=$(ee_workspace graph feature-enrichment --json 2>/dev/null || true)
            ;;
        graph_snapshot_stale)
            seed_j6_graph_snapshot stale '{"nodes":[{"id":"mem_j6graphnode00000000000000","label":"J6 node","pagerank":1.0,"betweenness":0.0}],"edges":[]}' 1 0 || return 1
            SCENARIO_OUTPUT=$(ee_workspace graph export --json 2>/dev/null || true)
            ;;
        graph_snapshot_topology_unavailable)
            seed_j6_graph_snapshot valid '{"nodes":[],"edges":[]}' 0 0 || return 1
            SCENARIO_OUTPUT=$(ee_workspace graph export --json 2>/dev/null || true)
            ;;
        graph_snapshot_unusable)
            seed_j6_graph_snapshot invalid '{"nodes":[{"id":"mem_j6graphbad000000000000000","label":"J6 invalid","pagerank":1.0,"betweenness":0.0}],"edges":[]}' 1 0 || return 1
            SCENARIO_OUTPUT=$(ee_workspace graph export --json 2>/dev/null || true)
            ;;
        graph_unavailable)
            SCENARIO_OUTPUT=$(ee_workspace diag dependencies --json 2>/dev/null || true)
            ;;
        heavy_gates_skipped)
            SCENARIO_OUTPUT=$(ee_workspace profile config plan \
                --profile constrained \
                --json 2>/dev/null || true)
            ;;
        index_locked)
            insert_j6_index_publish_lock
            SCENARIO_OUTPUT=$(ee_workspace index vacuum --json 2>/dev/null || true)
            ;;
        index_publish_lock_contention)
            remember_j6_memory "J6 index publish lock contention seed." semantic fact >/dev/null
            insert_j6_index_publish_lock || return 1
            SCENARIO_OUTPUT=$(
                export EE_INDEX_PUBLISH_LOCK_RETRY_ATTEMPTS=1
                ee_workspace index rebuild --json 2>/dev/null || true
            )
            ;;
        integrity_provenance_sample_unavailable)
            local provenance_skew_database
            provenance_skew_database="$EPIC_WORKSPACE/j6-integrity-provenance-skew.db"
            ee_workspace diag database-skew \
                --output-database "$provenance_skew_database" \
                --skew memories-provenance-chain-hash-column-unavailable \
                --json >/dev/null 2>&1 || return 1
            SCENARIO_OUTPUT=$(ee_workspace diag integrity \
                --database "$provenance_skew_database" \
                --json 2>/dev/null || true)
            ;;
        integrity_reference_check_unavailable)
            local reference_skew_database
            reference_skew_database="$EPIC_WORKSPACE/j6-integrity-reference-skew.db"
            ee_workspace diag database-skew \
                --output-database "$reference_skew_database" \
                --skew memory-links-table-unavailable \
                --json >/dev/null 2>&1 || return 1
            SCENARIO_OUTPUT=$(ee_workspace diag integrity \
                --database "$reference_skew_database" \
                --json 2>/dev/null || true)
            ;;
        integrity_reference_issues)
            seed_j6_pack_reference_issue || return 1
            SCENARIO_OUTPUT=$(ee_workspace diag integrity --json 2>/dev/null || true)
            ;;
        integrity_schema_check_unavailable)
            local schema_check_skew_database
            schema_check_skew_database="$EPIC_WORKSPACE/j6-integrity-schema-check-skew.db"
            ee_workspace diag database-skew \
                --output-database "$schema_check_skew_database" \
                --skew migration-checksum-column-missing \
                --json >/dev/null 2>&1 || return 1
            SCENARIO_OUTPUT=$(ee_workspace diag integrity \
                --database "$schema_check_skew_database" \
                --json 2>/dev/null || true)
            ;;
        integrity_schema_migration_required)
            local schema_required_skew_database
            schema_required_skew_database="$EPIC_WORKSPACE/j6-integrity-schema-required-skew.db"
            ee_workspace diag database-skew \
                --output-database "$schema_required_skew_database" \
                --skew schema-migration-required \
                --json >/dev/null 2>&1 || return 1
            SCENARIO_OUTPUT=$(ee_workspace diag integrity \
                --database "$schema_required_skew_database" \
                --json 2>/dev/null || true)
            ;;
        lab_replay_unavailable)
            SCENARIO_OUTPUT=$(ee_workspace lab replay \
                missing-episode-id \
                --json 2>/dev/null || true)
            ;;
        maintenance_job_history_read_failed)
            local bad_history_workspace
            bad_history_workspace="$EPIC_WORKSPACE/j6-job-history-read-failed"
            mkdir -p "$bad_history_workspace/.ee/maintenance-jobs.jsonl"
            SCENARIO_OUTPUT=$(ee_global job list \
                --workspace "$bad_history_workspace" \
                --json 2>/dev/null || true)
            ;;
        maintenance_job_timed_out)
            SCENARIO_OUTPUT=$(ee_workspace job run decay_sweep \
                --time-limit-ms 0 \
                --json 2>/dev/null || true)
            ;;
        manual_heavy_strategy)
            SCENARIO_OUTPUT=$(ee_workspace profile config plan \
                --profile constrained \
                --json 2>/dev/null || true)
            ;;
        mcp_unavailable)
            SCENARIO_OUTPUT=$(ee_workspace diag dependencies --json 2>/dev/null || true)
            ;;
        model_registry_no_available_entry)
            seed_j6_disabled_model_registry_entry || return 1
            SCENARIO_OUTPUT=$(ee_workspace model status --json 2>/dev/null || true)
            ;;
        preflight_evidence_stale)
            local preflight_stale_workspace preflight_stale_store preflight_stale_tmp
            preflight_stale_workspace="$EPIC_WORKSPACE/j6-preflight-evidence-stale"
            preflight_stale_store="$preflight_stale_workspace/.ee/preflight_runs.json"
            preflight_stale_tmp="$EPIC_WORKSPACE/j6-preflight-evidence-stale.json"
            ee_global init --workspace "$preflight_stale_workspace" --json >/dev/null
            ee_global preflight run \
                "J6 stale preflight evidence task." \
                --workspace "$preflight_stale_workspace" \
                --json >/dev/null 2>&1 || true
            jq '
                .runs[0].report.started_at = "2000-01-01T00:00:00Z"
                | .runs[0].report.completed_at = "2000-01-01T00:00:01Z"
            ' "$preflight_stale_store" > "$preflight_stale_tmp"
            cp "$preflight_stale_tmp" "$preflight_stale_store"
            SCENARIO_OUTPUT=$(ee_global preflight run \
                "J6 stale preflight evidence task." \
                --workspace "$preflight_stale_workspace" \
                --json 2>/dev/null || true)
            ;;
        remember_auto_link_failed)
            local auto_link_skew_workspace
            auto_link_skew_workspace="$EPIC_WORKSPACE/j6-remember-auto-link-failed"
            ee_global init --workspace "$auto_link_skew_workspace" --json >/dev/null
            ee_global remember "J6 workflow auto-link seed." \
                --workspace "$auto_link_skew_workspace" \
                --workflow j6-auto-link \
                --no-propose-candidates \
                --json >/dev/null 2>&1 || true
            ee_global diag database-skew \
                --workspace "$auto_link_skew_workspace" \
                --in-place \
                --skew memory-links-table-unavailable \
                --json >/dev/null 2>&1 || return 1
            SCENARIO_OUTPUT=$(ee_global remember "J6 workflow auto-link trigger." \
                --workspace "$auto_link_skew_workspace" \
                --workflow j6-auto-link \
                --no-propose-candidates \
                --json 2>/dev/null || true)
            ;;
        remember_link_suggestion_failed)
            local link_suggestion_skew_workspace
            link_suggestion_skew_workspace="$EPIC_WORKSPACE/j6-remember-link-suggestion-failed"
            ee_global init --workspace "$link_suggestion_skew_workspace" --json >/dev/null
            ee_global remember "J6 link suggestion seed." \
                --workspace "$link_suggestion_skew_workspace" \
                --level semantic \
                --kind fact \
                --tags j6-link-suggestion \
                --no-auto-link \
                --no-propose-candidates \
                --json >/dev/null 2>&1 || true
            ee_global diag database-skew \
                --workspace "$link_suggestion_skew_workspace" \
                --in-place \
                --skew memory-links-table-unavailable \
                --json >/dev/null 2>&1 || return 1
            SCENARIO_OUTPUT=$(ee_global remember "J6 link suggestion trigger." \
                --workspace "$link_suggestion_skew_workspace" \
                --level semantic \
                --kind fact \
                --tags j6-link-suggestion \
                --no-auto-link \
                --no-propose-candidates \
                --json 2>/dev/null || true)
            ;;
        runtime_unavailable)
            SCENARIO_OUTPUT=$(EE_DIAG_FORCE_CAPABILITY_GAP=runtime \
                ee_workspace status --json 2>/dev/null || true)
            ;;
        science_backend_unavailable)
            SCENARIO_OUTPUT=$(EE_SCIENCE_BACKEND_PATH="$EPIC_WORKSPACE/j6-missing-science-backend" \
                ee_workspace analyze science-status --json 2>/dev/null || true)
            ;;
        science_budget_exceeded)
            local science_budget_dir science_budget_baseline science_budget_current
            science_budget_dir="$EPIC_WORKSPACE/j6-science-budget"
            mkdir -p "$science_budget_dir"
            science_budget_baseline="$science_budget_dir/baseline.json"
            science_budget_current="$science_budget_dir/current.json"
            printf '{"snapshotId":"science-budget-baseline","scenariosRun":2,"scenariosPassed":2,"scienceMetrics":{"f1Score":1.0}}\n' > "$science_budget_baseline"
            printf '{"snapshotId":"science-budget-current","scenariosRun":2,"scenariosPassed":1,"scienceMetrics":{"f1Score":0.5}}\n' > "$science_budget_current"
            SCENARIO_OUTPUT=$(ee_workspace analyze drift \
                --baseline "$science_budget_baseline" \
                --current "$science_budget_current" \
                --metric-budget 0 \
                --json 2>/dev/null || true)
            ;;
        science_input_too_large)
            local science_input_dir science_input_baseline science_input_current
            science_input_dir="$EPIC_WORKSPACE/j6-science-input"
            mkdir -p "$science_input_dir"
            science_input_baseline="$science_input_dir/baseline.json"
            science_input_current="$science_input_dir/current.json"
            printf '{"snapshotId":"science-input-baseline","scenariosRun":2,"scenariosPassed":2,"scienceMetrics":{"f1Score":1.0}}\n' > "$science_input_baseline"
            printf '{"snapshotId":"science-input-current","scenariosRun":2,"scenariosPassed":1,"scienceMetrics":{"f1Score":0.5}}\n' > "$science_input_current"
            SCENARIO_OUTPUT=$(ee_workspace analyze drift \
                --baseline "$science_input_baseline" \
                --current "$science_input_current" \
                --max-input-bytes 1 \
                --json 2>/dev/null || true)
            ;;
        science_not_compiled)
            SCENARIO_OUTPUT=$(EE_DIAG_FORCE_CAPABILITY_GAP=science \
                ee_workspace analyze science-status --json 2>/dev/null || true)
            ;;
        search_not_inspected)
            SCENARIO_OUTPUT=$(status_without_selected_workspace)
            ;;
        search_unavailable)
            SCENARIO_OUTPUT=$(ee_workspace status --json 2>/dev/null || true)
            ;;
        search_unimplemented)
            SCENARIO_OUTPUT=$(EE_DIAG_FORCE_CAPABILITY_GAP=search \
                ee_workspace status --json 2>/dev/null || true)
            ;;
        semantic_dimension_exceeds_budget)
            seed_j6_oversized_model_registry_entry || return 1
            SCENARIO_OUTPUT=$(ee_workspace model status --json 2>/dev/null || true)
            ;;
        situation_decisioning_unavailable)
            SCENARIO_OUTPUT=$(ee_workspace situation classify \
                "fix failing release workflow" \
                --json 2>/dev/null || true)
            ;;
        storage_not_inspected)
            SCENARIO_OUTPUT=$(status_without_selected_workspace)
            ;;
        storage_unavailable)
            local unavailable_storage_workspace
            unavailable_storage_workspace="$EPIC_WORKSPACE/j6-storage-unavailable"
            workspace_with_unopenable_database "$unavailable_storage_workspace"
            SCENARIO_OUTPUT=$(ee_global status \
                --workspace "$unavailable_storage_workspace" \
                --json 2>/dev/null || true)
            ;;
        storage_unimplemented)
            SCENARIO_OUTPUT=$(EE_DIAG_FORCE_CAPABILITY_GAP=storage \
                ee_workspace status --json 2>/dev/null || true)
            ;;
        toon_unavailable)
            SCENARIO_OUTPUT=$(EE_DISABLE_TOON=1 ee_workspace status --json 2>/dev/null || true)
            ;;
        unsupported_condition)
            local unsupported_tripwire_id
            unsupported_tripwire_id=$(seed_j6_unsupported_tripwire) || return 1
            SCENARIO_OUTPUT=$(ee_workspace tripwire check \
                "$unsupported_tripwire_id" \
                --dry-run \
                --json 2>/dev/null || true)
            ;;
        why_pack_selection_unavailable)
            local why_skew_workspace why_memory_json why_memory_id
            why_skew_workspace="$EPIC_WORKSPACE/j6-why-pack-selection-unavailable"
            ee_global init --workspace "$why_skew_workspace" --json >/dev/null
            why_memory_json=$(ee_global remember \
                "J6 why pack selection fixture memory." \
                --workspace "$why_skew_workspace" \
                --level semantic \
                --kind fact \
                --no-auto-link \
                --no-propose-candidates \
                --json 2>/dev/null || true)
            why_memory_id=$(printf '%s' "$why_memory_json" | j6_memory_id_from_json)
            if [ -z "$why_memory_id" ]; then
                todo_assert "j6_why_pack_selection_memory_seeded" "bd-17c65.10.6" \
                    "Failed to create memory for why pack-selection fixture."
                return 1
            fi
            ee_global diag database-skew \
                --workspace "$why_skew_workspace" \
                --output-database "$why_skew_workspace/.ee/ee-skew.db" \
                --skew pack-items-table-unavailable \
                --json >/dev/null 2>&1 || return 1
            SCENARIO_OUTPUT=$(ee_global why "$why_memory_id" \
                --workspace "$why_skew_workspace" \
                --database "$why_skew_workspace/.ee/ee-skew.db" \
                --json 2>/dev/null || true)
            ;;
        write_owner_busy)
            SCENARIO_OUTPUT=$(ee_workspace diag write-owner \
                --capacity 1 \
                --enqueue 2 \
                --json 2>/dev/null || true)
            ;;
        write_spool_backpressure)
            SCENARIO_OUTPUT=$(ee_workspace diag write-spool \
                --max-pending 1 \
                --enqueue 2 \
                --json 2>/dev/null || true)
            ;;
        context_evidence_freshness_changed_source)
            local changed_source_pack_id
            changed_source_pack_id=$(seed_j6_changed_source_pack) || return 1
            SCENARIO_OUTPUT=$(ee_workspace pack replay \
                "$changed_source_pack_id" \
                --json 2>/dev/null || true)
            ;;
        consensus_no_clusters)
            local consensus_workspace
            consensus_workspace="$EPIC_WORKSPACE/j6-consensus-no-clusters"
            mkdir -p "$consensus_workspace"
            "$EE_BINARY" init --workspace "$consensus_workspace" --json >/dev/null 2>&1 || true
            SCENARIO_OUTPUT=$("$EE_BINARY" context \
                "sparse consensus subject" \
                --workspace "$consensus_workspace" \
                --json 2>/dev/null || true)
            ;;
        coordination_source_stale)
            SCENARIO_OUTPUT=$(ee_workspace pack \
                "coordination snapshot fixture" \
                --coordination-snapshot "$REPO_ROOT/tests/fixtures/coordination_snapshots/${code}.json" \
                --json 2>/dev/null || true)
            ;;
        coordination_source_unavailable)
            SCENARIO_OUTPUT=$(ee_workspace pack \
                "coordination snapshot fixture" \
                --coordination-snapshot "$REPO_ROOT/tests/fixtures/coordination_snapshots/${code}.json" \
                --json 2>/dev/null || true)
            ;;
        swarm_scale_budget_exceeded)
            SCENARIO_OUTPUT=$(EE_SWARM_SCALE_FORCE_FAILURE=budget_exceeded \
                "$SCRIPT_DIR/swarm_scale.sh" 2>/dev/null || true)
            ;;
        swarm_scale_nondeterminism)
            SCENARIO_OUTPUT=$(EE_SWARM_SCALE_FORCE_FAILURE=nondeterminism \
                "$SCRIPT_DIR/swarm_scale.sh" 2>/dev/null || true)
            ;;
        scope_agent_unavailable)
            remember_j6_memory "J6 scope agent unavailable fixture." semantic fact >/dev/null
            ee_workspace index rebuild --json >/dev/null 2>&1 || true
            SCENARIO_OUTPUT=$(env -u EE_AGENT_NAME "$EE_BINARY" search \
                "J6 scope agent unavailable fixture" \
                --workspace "$EPIC_WORKSPACE" \
                --memory-scope self \
                --json 2>/dev/null || true)
            ;;
        scope_excluded_evidence)
            remember_j6_memory "J6 scope excluded evidence fixture." semantic fact >/dev/null
            ee_workspace index rebuild --json >/dev/null 2>&1 || true
            SCENARIO_OUTPUT=$(env -u EE_AGENT_NAME "$EE_BINARY" search \
                "J6 scope excluded evidence fixture" \
                --workspace "$EPIC_WORKSPACE" \
                --memory-scope self \
                --json 2>/dev/null || true)
            ;;
        scope_metadata_unavailable)
            remember_j6_memory "J6 scope metadata unavailable fixture." semantic fact >/dev/null
            ee_workspace index rebuild --json >/dev/null 2>&1 || true
            SCENARIO_OUTPUT=$("$EE_BINARY" search \
                "J6 scope metadata unavailable fixture" \
                --workspace "$EPIC_WORKSPACE" \
                --database "$EPIC_WORKSPACE/j6-scope-metadata-empty.db" \
                --memory-scope verified \
                --json 2>/dev/null || true)
            ;;
        scope_strict_excluded_evidence)
            remember_j6_memory "J6 scope strict excluded evidence fixture." semantic fact >/dev/null
            ee_workspace index rebuild --json >/dev/null 2>&1 || true
            SCENARIO_OUTPUT=$(env -u EE_AGENT_NAME "$EE_BINARY" search \
                "J6 scope strict excluded evidence fixture" \
                --workspace "$EPIC_WORKSPACE" \
                --memory-scope self \
                --strict-scope \
                --json 2>/dev/null || true)
            ;;
        level_transition_requires_evidence)
            local level_memory_json level_memory_id
            level_memory_json=$(remember_j6_memory \
                "Evidence-free promotion candidate." \
                episodic \
                fact)
            level_memory_id=$(printf '%s' "$level_memory_json" | j6_memory_id_from_json)
            if [ -z "$level_memory_id" ]; then
                todo_assert "j6_level_transition_requires_evidence_memory_seeded" "bd-17c65.10.6" \
                    "Failed to create the memory-level transition fixture memory."
                return 1
            fi
            SCENARIO_OUTPUT=$(ee_workspace memory level \
                "$level_memory_id" \
                --to semantic \
                --json 2>/dev/null || true)
            ;;
        level_transition_tombstoned_rejected)
            local tombstone_memory_json tombstone_memory_id
            tombstone_memory_json=$(remember_j6_memory \
                "Retired lifecycle fact." \
                semantic \
                fact)
            tombstone_memory_id=$(printf '%s' "$tombstone_memory_json" | j6_memory_id_from_json)
            if [ -z "$tombstone_memory_id" ]; then
                todo_assert "j6_level_transition_tombstone_memory_seeded" "bd-17c65.10.6" \
                    "Failed to create the tombstoned level-transition fixture memory."
                return 1
            fi
            ee_workspace curate tombstone \
                "$tombstone_memory_id" \
                --reason "superseded" \
                --json >/dev/null 2>&1 || true
            SCENARIO_OUTPUT=$(ee_workspace memory level \
                "$tombstone_memory_id" \
                --to procedural \
                --reason "promote retired fact" \
                --json 2>/dev/null || true)
            ;;
        conflict_direct)
            remember_j6_memory \
                "Always use HTTPS for callbacks." \
                procedural \
                rule \
                --tags transport-https >/dev/null
            remember_j6_memory \
                "Never use HTTPS for callbacks." \
                procedural \
                rule \
                --tags transport-https >/dev/null
            ee_workspace index rebuild --json >/dev/null 2>&1 || true
            SCENARIO_OUTPUT=$(ee_workspace context \
                "HTTPS callbacks" \
                --profile submodular \
                --candidate-pool 20 \
                --max-tokens 4000 \
                --json 2>/dev/null || true)
            ;;
        conflict_trust_mismatch)
            seed_j6_untrusted_conflict_memory
            remember_j6_memory \
                "Always use HTTPS for callbacks." \
                procedural \
                rule \
                --tags transport-https >/dev/null
            ee_workspace index rebuild --json >/dev/null 2>&1 || true
            SCENARIO_OUTPUT=$(ee_workspace context \
                "trusted callback transport HTTPS callbacks" \
                --profile submodular \
                --candidate-pool 20 \
                --max-tokens 4000 \
                --json 2>/dev/null || true)
            ;;
        level_transition_concurrent_conflict)
            local conflict_memory_json conflict_memory_id
            conflict_memory_json=$(remember_j6_memory \
                "Concurrent lifecycle marker." \
                working \
                fact \
                --workflow wf-conflict)
            conflict_memory_id=$(printf '%s' "$conflict_memory_json" | j6_memory_id_from_json)
            if [ -z "$conflict_memory_id" ]; then
                todo_assert "j6_level_transition_concurrent_conflict_memory_seeded" "bd-17c65.10.6" \
                    "Failed to create the concurrent level-transition fixture memory."
                return 1
            fi
            ee_workspace memory level \
                "$conflict_memory_id" \
                --from working \
                --to episodic \
                --reason "first writer completed the workflow" \
                --json >/dev/null 2>&1 || true
            SCENARIO_OUTPUT=$(ee_workspace memory level \
                "$conflict_memory_id" \
                --from working \
                --to episodic \
                --reason "stale worker retries the planned transition" \
                --json 2>/dev/null || true)
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
        [
            .. | objects |
            ((.code // ""), (.detailCode // ""), (.details.code // ""))
        ]
        | any(. == $code)
    ' >/dev/null 2>&1
}

json_fixture_severity() {
    local json="${1:-}"
    local code="${2:?code required}"
    printf '%s' "$json" | jq -r --arg code "$code" '
        if (.error.details.detailCode // "") == $code then
            .error.severity // empty
        else
            [.. | objects | select(
                ((.code // "") == $code)
                or ((.detailCode // "") == $code)
                or ((.details.code // "") == $code)
            ) |
                if ((.severity // "") != "") then
                    .severity
                elif (((.error | objects | .severity) // "") != "") then
                    (.error | objects | .severity)
                elif ((.description // "") != "" and (.impact // "") != "") then
                    "info"
                else
                    empty
                end
            ]
            | map(select(. != ""))
            | first // empty
        end
    ' 2>/dev/null || true
}

json_messages() {
    printf '%s' "${1:-}" | jq -r '
        [.. | objects | ((.message? // empty), (.description? // empty))]
        | join("\n")
    ' \
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
    code=$(jq -r '.code // empty' "$fixture")
    if [ -z "$code" ]; then
        code="$(basename "$fixture" .json)"
    fi
    if ! fixture_filter_matches "$code"; then
        continue
    fi
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
