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
    ee_global status --workspace "$status_dir" --json 2>/dev/null || true
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
            todo_assert "j6_${code}_fixture_exercised" "bd-17c65.10.6" \
                "Fixture requires an indexed memory with stale validity_status; no safe public CLI setup creates that state yet."
            return 1
            ;;
        malformed_validity_filtered)
            todo_assert "j6_${code}_fixture_exercised" "bd-17c65.10.6" \
                "Fixture requires malformed persisted validity timestamps; keep catalog-only until a safe fixture harness can inject them."
            return 1
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
            SCENARIO_OUTPUT=$(ee_workspace pack \
                "J6 advisory imported memory marker" \
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
                --time-limit-ms 0 \
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
            local unresolved_decay_workspace unresolved_decay_database
            unresolved_decay_workspace="$EPIC_WORKSPACE/j6-decay-workspace-unresolved"
            unresolved_decay_database="$EPIC_WORKSPACE/j6-decay-workspace-unresolved.db"
            mkdir -p "$unresolved_decay_workspace"
            cp "$EPIC_WORKSPACE/.ee/ee.db" "$unresolved_decay_database" 2>/dev/null || true
            SCENARIO_OUTPUT=$(ee_global job run decay_sweep \
                --workspace "$unresolved_decay_workspace" \
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
        maintenance_job_skipped)
            SCENARIO_OUTPUT=$(ee_workspace job run custom --json 2>/dev/null || true)
            ;;
        maintenance_job_history_write_failed)
            local history_blocking_path
            history_blocking_path="$EPIC_WORKSPACE/.ee/maintenance-jobs.jsonl"
            mkdir -p "$history_blocking_path"
            SCENARIO_OUTPUT=$(ee_workspace job run custom --json 2>/dev/null || true)
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
            todo_assert "j6_${code}_fixture_exercised" "bd-17c65.10.6" \
                "Current public surfaces expose this as dependency-contract metadata or usage errors, not a structured degraded[] emission with severity and repair."
            return 1
            ;;
        agent_status_unavailable)
            todo_assert "j6_${code}_fixture_exercised" "bd-17c65.10.6" \
                "Swarm brief only emits this when agent inventory collection errors; public CLI options do not create that failure without an injected detector."
            return 1
            ;;
        auto_link_disabled)
            SCENARIO_OUTPUT=$(ee_workspace remember \
                "J6 workflow-less auto-link fixture." \
                --level procedural \
                --kind rule \
                --json 2>/dev/null || true)
            ;;
        auto_propose_failed)
            todo_assert "j6_${code}_fixture_exercised" "bd-17c65.10.6" \
                "Requires a remember-time proposal engine failure after storage succeeds; no safe public CLI setup injects that failure."
            return 1
            ;;
        auto_propose_search_neighbor_lookup_failed)
            todo_assert "j6_${code}_fixture_exercised" "bd-17c65.10.6" \
                "Requires search-neighbor lookup failure during remember auto-propose; index corruption attempts do not currently surface this code."
            return 1
            ;;
        cass_unavailable)
            todo_assert "j6_${code}_fixture_exercised" "bd-17c65.10.6" \
                "Import cass currently returns an import error and doctor returns warning checks, not degraded[] with cass_unavailable severity and repair."
            return 1
            ;;
        causal_chain_id_required)
            todo_assert "j6_${code}_fixture_exercised" "bd-17c65.10.6" \
                "The public causal estimate path without a chain ID routes to no_filters; the chain-id-required helper is not directly exposed."
            return 1
            ;;
        causal_chain_pair_required)
            todo_assert "j6_${code}_fixture_exercised" "bd-17c65.10.6" \
                "The public causal compare path with one missing chain routes through chain lookup or artifact filters instead of the pair-required helper."
            return 1
            ;;
        causal_database_migration_failed)
            todo_assert "j6_${code}_fixture_exercised" "bd-17c65.10.6" \
                "Requires a causal store migration failure injected below the public workspace database opener."
            return 1
            ;;
        causal_database_missing)
            todo_assert "j6_${code}_fixture_exercised" "bd-17c65.10.6" \
                "Public causal commands open or migrate the workspace database before reaching the lower-level missing-database diagnostic."
            return 1
            ;;
        causal_database_open_failed)
            todo_assert "j6_${code}_fixture_exercised" "bd-17c65.10.6" \
                "Public causal commands convert unopenable database state before the lower-level causal open-failed diagnostic is emitted."
            return 1
            ;;
        causal_evidence_table_missing)
            todo_assert "j6_${code}_fixture_exercised" "bd-17c65.10.6" \
                "Requires a migrated-skew database where the causal evidence table is absent while the rest of the store opens."
            return 1
            ;;
        causal_insufficient_chains)
            todo_assert "j6_${code}_fixture_exercised" "bd-17c65.10.6" \
                "Requires exactly one matching persisted causal chain; public CLI has no safe fixture seeding path for that ledger state."
            return 1
            ;;
        causal_no_matching_chains)
            todo_assert "j6_${code}_fixture_exercised" "bd-17c65.10.6" \
                "Requires causal ledger rows that do not match the selected artifact or decision; no public seeding path exists."
            return 1
            ;;
        causal_trace_store_failed)
            todo_assert "j6_${code}_fixture_exercised" "bd-17c65.10.6" \
                "Requires trace row reads to fail after the causal store opens; no public CLI setup can inject that partial read failure."
            return 1
            ;;
        causal_workspace_id_required)
            todo_assert "j6_${code}_fixture_exercised" "bd-17c65.10.6" \
                "Workspace ID is always resolved by public causal commands before lower-level causal helpers run."
            return 1
            ;;
        clustering_insufficient_data)
            SCENARIO_OUTPUT=$(ee_workspace learn cluster --json 2>/dev/null || true)
            ;;
        clustering_no_embeddings)
            todo_assert "j6_${code}_fixture_exercised" "bd-17c65.10.6" \
                "Requires science clustering candidates without embeddings; no public fixture seeding path creates that state."
            return 1
            ;;
        clustering_threshold_too_strict)
            remember_j6_memory "J6 strict cluster singleton seed."
            SCENARIO_OUTPUT=$(ee_workspace learn cluster --json 2>/dev/null || true)
            ;;
        curation_harmful_candidate_escalated)
            todo_assert "j6_${code}_fixture_exercised" "bd-17c65.10.6" \
                "Requires persisted curation candidates past escalation TTL; public CLI does not seed that candidate state deterministically."
            return 1
            ;;
        curation_ttl_blocked)
            todo_assert "j6_${code}_fixture_exercised" "bd-17c65.10.6" \
                "Requires candidate rows whose TTL policy evaluation is blocked; no public CLI setup creates that malformed policy relationship."
            return 1
            ;;
        curation_ttl_policy_missing)
            todo_assert "j6_${code}_fixture_exercised" "bd-17c65.10.6" \
                "Requires candidate disposition against a missing TTL policy row; no public CLI fixture seeding path exists."
            return 1
            ;;
        curation_ttl_policy_unavailable)
            todo_assert "j6_${code}_fixture_exercised" "bd-17c65.10.6" \
                "Requires curation candidates to read while TTL policy row reads fail, which needs targeted store skew."
            return 1
            ;;
        decay_sweep_database_unresolved)
            todo_assert "j6_${code}_fixture_exercised" "bd-17c65.10.6" \
                "Current public decay-sweep errors resolve to missing, open-failed, or workspace-unresolved; no distinct unresolved database path is exposed."
            return 1
            ;;
        decay_sweep_handler_failed)
            todo_assert "j6_${code}_fixture_exercised" "bd-17c65.10.6" \
                "Requires an internal decay-sweep handler failure after setup; public cases currently map to more specific detail codes."
            return 1
            ;;
        decay_sweep_migration_failed)
            todo_assert "j6_${code}_fixture_exercised" "bd-17c65.10.6" \
                "Requires a database that opens but fails decay-sweep migration; no safe public fixture creates that migration skew."
            return 1
            ;;
        diagram_backend_unavailable)
            SCENARIO_OUTPUT=$(ee_workspace diag dependencies --json 2>/dev/null || true)
            ;;
        drift_analysis_unavailable)
            todo_assert "j6_${code}_fixture_exercised" "bd-17c65.10.6" \
                "Science status is currently available in this build; drift fallback emits specific snapshot errors instead of this capability code."
            return 1
            ;;
        feedback_protected_rules_unavailable)
            todo_assert "j6_${code}_fixture_exercised" "bd-17c65.10.6" \
                "Requires protected-rule counts to fail after earlier feedback health queries succeed; no public CLI setup creates that partial failure."
            return 1
            ;;
        feedback_quarantine_unavailable)
            todo_assert "j6_${code}_fixture_exercised" "bd-17c65.10.6" \
                "Requires feedback quarantine rows to fail after harmful-feedback counts succeed; no public CLI setup creates that partial failure."
            return 1
            ;;
        graph_feature_disabled)
            todo_assert "j6_${code}_fixture_exercised" "bd-17c65.10.6" \
                "Requires running a binary built without the default graph feature; no such binary is available in this no-Cargo pass."
            return 1
            ;;
        graph_snapshot_scores_unavailable)
            todo_assert "j6_${code}_fixture_exercised" "bd-17c65.10.6" \
                "Requires a persisted graph snapshot whose metrics_json cannot be parsed for centrality scores."
            return 1
            ;;
        graph_snapshot_stale)
            todo_assert "j6_${code}_fixture_exercised" "bd-17c65.10.6" \
                "Requires marking a persisted graph snapshot stale; no public graph command exposes that state transition."
            return 1
            ;;
        graph_snapshot_topology_unavailable)
            todo_assert "j6_${code}_fixture_exercised" "bd-17c65.10.6" \
                "Requires a persisted snapshot with malformed export topology while the snapshot row remains otherwise readable."
            return 1
            ;;
        graph_snapshot_unusable)
            todo_assert "j6_${code}_fixture_exercised" "bd-17c65.10.6" \
                "Requires a persisted graph snapshot marked invalid or archived; no public graph command creates that state."
            return 1
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
            todo_assert "j6_${code}_fixture_exercised" "bd-17c65.10.6" \
                "Requires an active index publish advisory lock row; public CLI exposes no lock acquisition fixture."
            return 1
            ;;
        index_publish_lock_contention)
            todo_assert "j6_${code}_fixture_exercised" "bd-17c65.10.6" \
                "Requires holding the index publish advisory lock before rebuild; public CLI exposes no lock holder setup."
            return 1
            ;;
        integrity_provenance_sample_unavailable)
            todo_assert "j6_${code}_fixture_exercised" "bd-17c65.10.6" \
                "Requires provenance sample queries to fail after schema and reference checks are readable; needs targeted database skew."
            return 1
            ;;
        integrity_reference_check_unavailable)
            todo_assert "j6_${code}_fixture_exercised" "bd-17c65.10.6" \
                "Requires reference-integrity query failure after the database opens; no public setup creates that partial failure."
            return 1
            ;;
        integrity_reference_issues)
            todo_assert "j6_${code}_fixture_exercised" "bd-17c65.10.6" \
                "Requires dangling link or pack references; public CLI prevents creating the inconsistent reference rows."
            return 1
            ;;
        integrity_schema_check_unavailable)
            todo_assert "j6_${code}_fixture_exercised" "bd-17c65.10.6" \
                "Requires migration metadata reads to fail while the database opens; no public setup creates that state."
            return 1
            ;;
        integrity_schema_migration_required)
            todo_assert "j6_${code}_fixture_exercised" "bd-17c65.10.6" \
                "Requires a behind-current database fixture; public init immediately creates the current schema."
            return 1
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
            todo_assert "j6_${code}_fixture_exercised" "bd-17c65.10.6" \
                "Public tiny-budget job runs currently emit cancelled, failed, or skipped rather than timed_out."
            return 1
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
            todo_assert "j6_${code}_fixture_exercised" "bd-17c65.10.6" \
                "Requires a model_registry row with no available entries; no public CLI can insert disabled registry rows."
            return 1
            ;;
        preflight_evidence_stale)
            todo_assert "j6_${code}_fixture_exercised" "bd-17c65.10.6" \
                "The constructor exists but no current public preflight path appears to emit this code."
            return 1
            ;;
        remember_auto_link_failed)
            todo_assert "j6_${code}_fixture_exercised" "bd-17c65.10.6" \
                "Requires remember storage success followed by auto-link failure; no public CLI setup injects that partial failure."
            return 1
            ;;
        remember_link_suggestion_failed)
            todo_assert "j6_${code}_fixture_exercised" "bd-17c65.10.6" \
                "Requires remember storage success followed by link-suggestion failure; no public CLI setup injects that partial failure."
            return 1
            ;;
        runtime_unavailable)
            todo_assert "j6_${code}_fixture_exercised" "bd-17c65.10.6" \
                "Requires Asupersync runtime initialization failure; current public binary initializes the runtime successfully."
            return 1
            ;;
        science_backend_unavailable)
            todo_assert "j6_${code}_fixture_exercised" "bd-17c65.10.6" \
                "Science backend is available in this build; no public environment flag forces backend-unavailable diagnostics."
            return 1
            ;;
        science_budget_exceeded)
            todo_assert "j6_${code}_fixture_exercised" "bd-17c65.10.6" \
                "No current public science command exposes a configurable tiny budget that emits this structured code."
            return 1
            ;;
        science_input_too_large)
            todo_assert "j6_${code}_fixture_exercised" "bd-17c65.10.6" \
                "No current public science command exposes a deterministic oversized-input fixture path for this structured code."
            return 1
            ;;
        science_not_compiled)
            todo_assert "j6_${code}_fixture_exercised" "bd-17c65.10.6" \
                "Requires a binary built without science analytics; the available binary reports science available."
            return 1
            ;;
        search_not_inspected)
            todo_assert "j6_${code}_fixture_exercised" "bd-17c65.10.6" \
                "The public status command always resolves a workspace path, so the no-workspace not-inspected branch is not reachable."
            return 1
            ;;
        search_unavailable)
            todo_assert "j6_${code}_fixture_exercised" "bd-17c65.10.6" \
                "Current public paths expose search dependency contract or index status, not this structured degraded[] capability code."
            return 1
            ;;
        search_unimplemented)
            todo_assert "j6_${code}_fixture_exercised" "bd-17c65.10.6" \
                "Requires a binary built without search support; this build has search compiled."
            return 1
            ;;
        semantic_dimension_exceeds_budget)
            todo_assert "j6_${code}_fixture_exercised" "bd-17c65.10.6" \
                "Requires a model registry or semantic-admissibility fixture with oversized dimensions; no public seed path exists."
            return 1
            ;;
        situation_decisioning_unavailable)
            SCENARIO_OUTPUT=$(ee_workspace situation classify \
                "fix failing release workflow" \
                --json 2>/dev/null || true)
            ;;
        storage_not_inspected)
            todo_assert "j6_${code}_fixture_exercised" "bd-17c65.10.6" \
                "The public status command always resolves a workspace path, so the no-workspace not-inspected branch is not reachable."
            return 1
            ;;
        storage_unavailable)
            todo_assert "j6_${code}_fixture_exercised" "bd-17c65.10.6" \
                "Current public paths expose storage_degraded/not_initialized or dependency-contract metadata, not this capability code."
            return 1
            ;;
        storage_unimplemented)
            todo_assert "j6_${code}_fixture_exercised" "bd-17c65.10.6" \
                "Requires a binary built without storage support; this build has storage compiled."
            return 1
            ;;
        toon_unavailable)
            todo_assert "j6_${code}_fixture_exercised" "bd-17c65.10.6" \
                "Currently represented in dependency-contract diagnostics, not as a structured degraded[] emission from status."
            return 1
            ;;
        unsupported_condition)
            todo_assert "j6_${code}_fixture_exercised" "bd-17c65.10.6" \
                "Requires a persisted tripwire with an unsupported condition; public preflight-generated tripwires use supported condition forms."
            return 1
            ;;
        why_pack_selection_unavailable)
            todo_assert "j6_${code}_fixture_exercised" "bd-17c65.10.6" \
                "Requires pack-selection ledger reads to fail after memory lookup succeeds; no public setup creates that partial failure."
            return 1
            ;;
        write_owner_busy)
            todo_assert "j6_${code}_fixture_exercised" "bd-17c65.10.6" \
                "Requires holding the database write-owner lock from another process; no public fixture command acquires it without performing writes."
            return 1
            ;;
        write_spool_backpressure)
            todo_assert "j6_${code}_fixture_exercised" "bd-17c65.10.6" \
                "Requires saturating the write spool queue; no safe public CLI setup creates deterministic backpressure."
            return 1
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
        elif ([.. | objects | select((.details.code? // empty) == $code)] | length) > 0 then
            [.. | objects | select((.code? // empty) == "maintenance_job_failed") | .severity?]
            | map(select(. != ""))
            | first // empty
        else
            [.. | objects | select((.code? // empty) == $code) |
                if ((.severity? // "") != "") then
                    .severity
                elif ((.description? // "") != "" and (.impact? // "") != "") then
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
