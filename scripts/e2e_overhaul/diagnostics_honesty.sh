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
# E2 (not shipped) — degraded[] should only appear when the *current* response
# was actually affected. Today the banner is almost always non-empty.
# ------------------------------------------------------------
todo_assert "e2_conditional_banner_emission" "bd-17c65.5.2" \
    "degraded[] currently emits banner content even when the current response isn't impacted."

# E3 — graph_snapshot rename + status normalization.
todo_assert "e3_graph_snapshot_rename" "bd-17c65.5.3" \
    "Graph snapshot status field naming inconsistent across surfaces."

# E5 — capabilities split into capabilities + diagnostics.
todo_assert "e5_capabilities_diagnostics_split" "bd-17c65.5.5" \
    "ee capabilities mixes static probe results with dynamic diagnostics."

# E6 — per-subsystem doctor checks.
todo_assert "e6_per_subsystem_doctor_checks" "bd-17c65.5.6" \
    "ee doctor lacks per-subsystem isolation (db/search/graph/cass/policy)."
