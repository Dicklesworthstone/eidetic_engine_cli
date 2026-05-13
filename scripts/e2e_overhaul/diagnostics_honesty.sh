#!/usr/bin/env bash
# J3 — Epic E: diagnostics honesty e2e driver.
#
# Verifies `ee doctor`, `ee status`, and related diagnostics surfaces emit the
# three-state posture (E1) and that the banner-emission honesty work landed.
#
# Shipped (real assertions):  E1, E4
# Not yet shipped (todo):     E2, E3, E5, E6

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
#   - quiet baseline: a fresh-workspace `ee context` query produces
#     `data.degraded[] | length == 0` after the filter (the only
#     signals fired are build-time / workspace-state).
#   - verbose: the same query with --include-non-affecting-degradations
#     surfaces the full set (>= the quiet count).
#   - the deleted meta-`degraded_context` code never appears in any
#     emitted code, in either mode.
# ------------------------------------------------------------
WS_E2=$(mktemp -d /tmp/ee-e2e-banner.XXXXXX)
trap 'rm -rf "$WS_E2"' EXIT
e2e_log_note "e2_workspace=$WS_E2"

run_capture "$EE_BINARY" init --workspace "$WS_E2" --json
run_capture "$EE_BINARY" remember "Run cargo fmt --check before release." \
    --workspace "$WS_E2" --level procedural --kind rule --tags release --json

# Quiet baseline — default filter, expect zero degraded entries.
QUIET_JSON=$(run_capture "$EE_BINARY" context "prepare release" \
    --workspace "$WS_E2" --max-tokens 1000 --json)
QUIET_LEN=$(printf '%s' "$QUIET_JSON" | jq -r '.data.degraded | length' 2>/dev/null || echo "?")
e2e_log_assert_eq "$QUIET_LEN" "0" \
    "e2_quiet_baseline_degraded_array_empty"

# Verbose mode — every signal surfaces.
VERBOSE_JSON=$(run_capture "$EE_BINARY" context "prepare release" \
    --workspace "$WS_E2" --max-tokens 1000 \
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

# E3 — graph_snapshot rename + status normalization.
todo_assert "e3_graph_snapshot_rename" "bd-17c65.5.3" \
    "Graph snapshot status field naming inconsistent across surfaces."

# E5 — capabilities split into capabilities + diagnostics.
todo_assert "e5_capabilities_diagnostics_split" "bd-17c65.5.5" \
    "ee capabilities mixes static probe results with dynamic diagnostics."

# E6 — per-subsystem doctor checks.
todo_assert "e6_per_subsystem_doctor_checks" "bd-17c65.5.6" \
    "ee doctor lacks per-subsystem isolation (db/search/graph/cass/policy)."
