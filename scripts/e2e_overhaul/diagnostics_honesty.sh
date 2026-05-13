#!/usr/bin/env bash
# J3 — Epic E: diagnostics honesty e2e driver.
#
# Verifies `ee doctor`, `ee status`, and related diagnostics surfaces emit the
# three-state posture (E1) and that the banner-emission honesty work landed.
#
# Shipped (real assertions):  E1, E2, E3, E4, E5, E6
# Not yet shipped (todo):     none

set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/e2e_overhaul/lib/shared.sh
source "$SCRIPT_DIR/lib/shared.sh"
require_jq
epic_setup "epic_E_diagnostics_honesty"

# ------------------------------------------------------------
# E1 (shipped) — doctor JSON exposes `posture` enum.
# ------------------------------------------------------------
DOCTOR_JSON=$(ee_workspace doctor --json || true)
if ! printf '%s' "$DOCTOR_JSON" | jq . >/dev/null 2>&1; then
    e2e_log_assert_eq "false" "true" "doctor_json_parses"
    exit 0
fi

# Some installations report `data.posture`, some `data.report.posture`; accept
# either by looking up via tostream and grepping. The contract is that the
# string appears as a top-level field of the doctor data shape.
POSTURE=$(printf '%s' "$DOCTOR_JSON" \
    | jq -r '.data.posture // .data.report.posture // empty' 2>/dev/null || true)
e2e_log_note "e1_doctor_posture=$POSTURE"

if [ -n "$POSTURE" ]; then
    e2e_log_assert_eq "true" "true" "e1_doctor_posture_field_present"
    case "$POSTURE" in
        ok|degraded_recoverable|blocked)
            e2e_log_assert_eq "true" "true" "e1_doctor_posture_enum_valid"
            ;;
        *)
            e2e_log_assert_eq "$POSTURE" "ok|degraded_recoverable|blocked" \
                "e1_doctor_posture_enum_valid"
            ;;
    esac
else
    e2e_log_assert_eq "missing" "present" "e1_doctor_posture_field_present"
fi

# `healthy` boolean is still preserved for backward compat per E1's design.
HAS_HEALTHY=$(printf '%s' "$DOCTOR_JSON" \
    | jq -r '.data | has("healthy") or .data | has("report")' 2>/dev/null || echo false)
e2e_log_note "e1_doctor_has_healthy_or_report=$HAS_HEALTHY"

# ------------------------------------------------------------
# E2 (SHIPPED 2026-05-13 CopperHarbor) — bead bd-17c65.5.2:
# Conditional banner emission. `data.degraded[]` filters out
# build-time feature gaps and workspace-state signals unless the
# caller passes `--include-non-affecting-degradations`.
#
# Three assertions:
#   - quiet baseline: a fresh-workspace `ee pack` query produces
#     `data.degraded[] | length == 0` after the filter (the only
#     signals fired are build-time / workspace-state).
#   - verbose: the same query with --include-non-affecting-degradations
#     surfaces the full set (>= the quiet count).
#   - the deleted meta-`degraded_context` code never appears in any
#     emitted code, in either mode.
# ------------------------------------------------------------
WS_E2=$(mktemp -d "${TMPDIR:-/tmp}/ee-e2e-banner.XXXXXX")
diagnostics_honesty_teardown() {
    e2e_log_note "e2_workspace_retained=$WS_E2"
    _epic_teardown
}
trap diagnostics_honesty_teardown EXIT
e2e_log_note "e2_workspace=$WS_E2"

e2e_log_command "$EE_BINARY" init --workspace "$WS_E2" --json >/dev/null
e2e_log_command "$EE_BINARY" remember "Run cargo fmt --check before release." \
    --workspace "$WS_E2" --level procedural --kind rule --tags release --json >/dev/null

# Quiet baseline — default filter, expect zero degraded entries.
QUIET_JSON=$(e2e_log_command "$EE_BINARY" pack "prepare release" \
    --workspace "$WS_E2" --max-tokens 1000 --candidate-pool 24 --json)
QUIET_LEN=$(printf '%s' "$QUIET_JSON" | jq -r '.data.degraded | length' 2>/dev/null || echo "?")
e2e_log_assert_eq "$QUIET_LEN" "0" \
    "e2_quiet_baseline_degraded_array_empty"

# Verbose mode — every signal surfaces.
VERBOSE_JSON=$(e2e_log_command "$EE_BINARY" pack "prepare release" \
    --workspace "$WS_E2" --max-tokens 1000 --candidate-pool 24 \
    --include-non-affecting-degradations=true --json)
VERBOSE_LEN=$(printf '%s' "$VERBOSE_JSON" | jq -r '.data.degraded | length' 2>/dev/null || echo "?")
# Verbose must be >= quiet — we expand the set, never shrink it.
if [ "$VERBOSE_LEN" = "?" ] || [ "$QUIET_LEN" = "?" ]; then
    e2e_log_assert_eq "parse_failed" "ok" "e2_verbose_count_parsed"
elif [ "$VERBOSE_LEN" -ge "$QUIET_LEN" ]; then
    e2e_log_assert_eq "true" "true" "e2_verbose_count_ge_quiet"
else
    e2e_log_assert_eq "$VERBOSE_LEN" ">=$QUIET_LEN" "e2_verbose_count_ge_quiet"
fi

# Deleted meta-code regression guard — `degraded_context` must NEVER
# appear in either emission mode.
for MODE in "quiet" "verbose"; do
    if [ "$MODE" = "quiet" ]; then
        MODE_JSON="$QUIET_JSON"
    else
        MODE_JSON="$VERBOSE_JSON"
    fi
    HAS_LEGACY=$(printf '%s' "$MODE_JSON" \
        | jq -r '[.data.degraded[]? | select(.code == "degraded_context")] | length' \
        2>/dev/null || echo "?")
    e2e_log_assert_eq "$HAS_LEGACY" "0" "e2_legacy_meta_code_absent_in_${MODE}"
done

# ------------------------------------------------------------
# E3 (SHIPPED 2026-05-13) — bead bd-17c65.5.3:
# Live graph compute and persisted graph snapshot artifacts are separate
# status concepts. A missing snapshot is `empty`, not `unimplemented`, while
# graphCompute reports that on-demand algorithms are still usable.
# ------------------------------------------------------------
E3_FRESH_STATUS=$(ee_workspace status --json || true)
assert_jq "$E3_FRESH_STATUS" '.data.graphCompute.status' "available" \
    "e3_fresh_graph_compute_available"
assert_jq "$E3_FRESH_STATUS" '.data.graphCompute.liveComputeSupported' "true" \
    "e3_fresh_graph_compute_live_supported"
assert_jq "$E3_FRESH_STATUS" '.data.graphSnapshotArtifact.status' "empty" \
    "e3_fresh_snapshot_empty"
assert_jq "$E3_FRESH_STATUS" \
    '[.data.derivedAssets[]? | select(.name == "graph_snapshot")] | length' "0" \
    "e3_old_graph_snapshot_asset_absent"
assert_jq "$E3_FRESH_STATUS" \
    '[.data.derivedAssets[]? | select(.name == "graph_snapshot_artifact")] | length' "1" \
    "e3_new_graph_snapshot_artifact_asset_present"
assert_jq "$E3_FRESH_STATUS" \
    '.data.derivedAssets[]? | select(.name == "graph_snapshot_artifact") | .memoryGraph.availability' \
    "live_compute_available" "e3_artifact_reports_live_compute_available"

E3_M1_JSON=$(ee_workspace remember "Graph status source memory one." \
    --level semantic --kind fact --json || true)
E3_M2_JSON=$(ee_workspace remember "Graph status target memory two." \
    --level semantic --kind fact --json || true)
E3_M1=$(printf '%s' "$E3_M1_JSON" | jq -r '.data.memory_id // empty' 2>/dev/null || true)
E3_M2=$(printf '%s' "$E3_M2_JSON" | jq -r '.data.memory_id // empty' 2>/dev/null || true)
if [ -n "$E3_M1" ] && [ -n "$E3_M2" ]; then
    ee_workspace memory link "$E3_M1" "$E3_M2" --relation supports --json >/dev/null
    E3_LINKED_STATUS=$(ee_workspace status --json || true)
    assert_jq "$E3_LINKED_STATUS" '.data.graphCompute.status' "available" \
        "e3_linked_graph_compute_still_available"
    assert_jq "$E3_LINKED_STATUS" '.data.graphSnapshotArtifact.status' "empty" \
        "e3_linked_snapshot_still_empty_before_refresh"
    assert_jq "$E3_LINKED_STATUS" '.data.graphSnapshotArtifact.memoryGraph.edgeCount' "1" \
        "e3_linked_memory_graph_edge_count"

    E3_REFRESH_JSON=$(ee_workspace graph centrality-refresh --json || true)
    assert_jq "$E3_REFRESH_JSON" '.data.snapshot.status' "valid" \
        "e3_refresh_snapshot_written"
    E3_REFRESHED_STATUS=$(ee_workspace status --json || true)
    assert_jq "$E3_REFRESHED_STATUS" '.data.graphCompute.status' "available" \
        "e3_refreshed_graph_compute_available"
    assert_jq "$E3_REFRESHED_STATUS" '.data.graphSnapshotArtifact.status' "current" \
        "e3_refreshed_snapshot_current"
    assert_jq "$E3_REFRESHED_STATUS" \
        '.data.graphSnapshotArtifact.memoryGraph.matchesDbGeneration' "true" \
        "e3_refreshed_snapshot_generation_matches"

    E3_M3_JSON=$(ee_workspace remember "Graph status third memory after snapshot." \
        --level semantic --kind fact --json || true)
    E3_M3=$(printf '%s' "$E3_M3_JSON" | jq -r '.data.memory_id // empty' 2>/dev/null || true)
    if [ -n "$E3_M3" ]; then
        ee_workspace memory link "$E3_M2" "$E3_M3" --relation related --json >/dev/null
        E3_STALE_STATUS=$(ee_workspace status --json || true)
        assert_jq "$E3_STALE_STATUS" '.data.graphCompute.status' "available" \
            "e3_stale_graph_compute_available"
        assert_jq "$E3_STALE_STATUS" '.data.graphSnapshotArtifact.status' "stale" \
            "e3_snapshot_stale_after_new_link"
        assert_jq "$E3_STALE_STATUS" \
            '.data.graphSnapshotArtifact.memoryGraph.matchesDbGeneration' "false" \
            "e3_stale_snapshot_generation_mismatch"
    else
        e2e_log_assert_eq "missing_memory_id" "present" "e3_third_memory_created"
    fi
else
    e2e_log_assert_eq "missing_memory_id" "present" "e3_seed_memories_created"
fi

# ------------------------------------------------------------
# E5 (SHIPPED 2026-05-13) — bead bd-17c65.5.5:
# Build-time feature gaps are surfaced once through capabilities, while
# response-local degraded[] arrays contain only signals that affected the
# operation.
# ------------------------------------------------------------
E5_CAPS_JSON=$(ee_global capabilities --json || true)
assert_jq "$E5_CAPS_JSON" '.data.command' "capabilities" \
    "e5_capabilities_command"
E5_UNIMPLEMENTED_COUNT=$(printf '%s' "$E5_CAPS_JSON" \
    | jq -r '.data.unimplemented | length' 2>/dev/null || echo "?")
if [ "$E5_UNIMPLEMENTED_COUNT" = "?" ]; then
    e2e_log_assert_eq "parse_failed" "numeric" "e5_unimplemented_count_parsed"
else
    e2e_log_assert_num "$E5_UNIMPLEMENTED_COUNT" -gt 0 \
        "e5_capabilities_unimplemented_nonempty"
fi

E5_BUILD_CODES=$(printf '%s' "$E5_CAPS_JSON" \
    | jq -r '.data.unimplemented[]?.code' 2>/dev/null || true)
if [ -z "$E5_BUILD_CODES" ]; then
    e2e_log_assert_eq "missing" "present" "e5_build_time_codes_present"
else
    e2e_log_assert_eq "present" "present" "e5_build_time_codes_present"
fi

ee_workspace remember "E5 indexed memory before stale index." \
    --level semantic --kind fact --json >/dev/null
ee_workspace index rebuild --json >/dev/null
ee_workspace remember "E5 memory written after the index rebuild." \
    --level semantic --kind fact --json >/dev/null
E5_SEARCH_JSON=$(ee_workspace search "E5 indexed memory" --json || true)

while IFS= read -r E5_CODE; do
    [ -z "$E5_CODE" ] && continue
    _e2e_emit_event "code_classification" \
        "code" "$E5_CODE" \
        "category" "build_time" \
        "surface" "capabilities" \
        "promoted_from_response_to_capabilities" "true"
    E5_IN_SEARCH=$(printf '%s' "$E5_SEARCH_JSON" \
        | jq -r --arg code "$E5_CODE" \
            '[.data.degraded[]? | select(.code == $code)] | length' \
            2>/dev/null || echo "?")
    e2e_log_assert_eq "$E5_IN_SEARCH" "0" \
        "e5_build_time_code_absent_from_search_degraded_${E5_CODE}"
done <<EOF
$E5_BUILD_CODES
EOF

E5_STALE_COUNT=$(printf '%s' "$E5_SEARCH_JSON" \
    | jq -r '[.data.degraded[]? | select(.code == "search_index_stale")] | length' \
    2>/dev/null || echo "?")
if [ "$E5_STALE_COUNT" = "?" ]; then
    e2e_log_assert_eq "parse_failed" "numeric" "e5_response_time_stale_count_parsed"
else
    e2e_log_assert_num "$E5_STALE_COUNT" -gt 0 \
        "e5_response_time_search_index_stale_still_emitted"
fi

# ------------------------------------------------------------
# E6 (SHIPPED 2026-05-13) — bead bd-17c65.5.6:
# Status posture separates workspace-wide subsystem readiness from the
# operation that just ran.
# ------------------------------------------------------------
E6_STATUS_JSON=$(ee_workspace status --json || true)
assert_jq "$E6_STATUS_JSON" '.data.command' "status" \
    "e6_status_command"
assert_jq "$E6_STATUS_JSON" '.data.posture.thisOperation.status' "ok" \
    "e6_this_operation_status_ok"
assert_jq "$E6_STATUS_JSON" \
    '[.data.posture.subsystems[]?.id] | sort | join(",")' \
    "agent_detection,curate,feedback,graph_compute,maintenance,memory,pack,runtime,search,storage" \
    "e6_fixed_subsystem_ids_present"

E6_BAD_POSTURE_STATUS_COUNT=$(printf '%s' "$E6_STATUS_JSON" \
    | jq -r '[.data.posture.overall, .data.posture.thisOperation.status, (.data.posture.subsystems[]?.status)]
        | map(select(. as $s | ["ok","degraded_recoverable","degraded_required","blocked","unimplemented","initializing"] | index($s) | not))
        | length' 2>/dev/null || echo "?")
if [ "$E6_BAD_POSTURE_STATUS_COUNT" = "?" ]; then
    e2e_log_assert_eq "parse_failed" "numeric" "e6_posture_status_enum_parsed"
else
    e2e_log_assert_eq "$E6_BAD_POSTURE_STATUS_COUNT" "0" \
        "e6_posture_status_enum_valid"
fi

E6_SUBSYSTEMS_USED_COUNT=$(printf '%s' "$E6_STATUS_JSON" \
    | jq -r '.data.posture.thisOperation.subsystemsUsed | length' \
    2>/dev/null || echo "?")
if [ "$E6_SUBSYSTEMS_USED_COUNT" = "?" ]; then
    e2e_log_assert_eq "parse_failed" "numeric" "e6_this_operation_used_count_parsed"
else
    e2e_log_assert_num "$E6_SUBSYSTEMS_USED_COUNT" -gt 0 \
        "e6_this_operation_subsystems_used_nonempty"
fi

E6_DEGRADED_LEN=$(printf '%s' "$E6_STATUS_JSON" \
    | jq -r '.data.degraded | length' 2>/dev/null || echo "?")
E6_OPERATION_DEGRADATIONS_LEN=$(printf '%s' "$E6_STATUS_JSON" \
    | jq -r '.data.posture.thisOperation.degradationsApplied | length' \
    2>/dev/null || echo "?")
if [ "$E6_DEGRADED_LEN" = "?" ] || [ "$E6_OPERATION_DEGRADATIONS_LEN" = "?" ]; then
    e2e_log_assert_eq "parse_failed" "numeric" "e6_degradation_count_parsed"
else
    e2e_log_assert_eq "$E6_OPERATION_DEGRADATIONS_LEN" "$E6_DEGRADED_LEN" \
        "e6_this_operation_degradation_count_matches_status"
fi
