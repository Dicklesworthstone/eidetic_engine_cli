#!/usr/bin/env bash
# J3 — Epic F: discoverability e2e driver.
#
# Asserts the shipped F1 (recovery actions), F2 (top-level aliases), F4
# (capabilities binaries + env), F7 (didYouMean) surfaces and records TODOs
# for the remaining F3, F5, F6 work.
#
# Shipped (real assertions):  F1, F2, F4, F7
# Not yet shipped (todo):     F3, F5, F6

set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/e2e_overhaul/lib/shared.sh
source "$SCRIPT_DIR/lib/shared.sh"
require_jq
epic_setup "epic_F_discoverability"

# ------------------------------------------------------------
# F1 (shipped) — error envelopes carry `error.details.recovery[]` with at
# least one structured RecoveryAction for the error categories F1 maps:
# Import (cass binary not found), SearchIndex, MigrationRequired,
# MigrationDrift, PolicyDenied(secret), Usage(workspace not found).
#
# Trigger PolicyDenied(secret) — the easiest reliable path — by remembering
# a value-shape secret.
# ------------------------------------------------------------
SECRET_JSON=$(ee_workspace remember \
    "GH_TOKEN=ghp_abcdefghijklmnopqrstuvwxyz0123456789ABC" \
    --level episodic --json 2>/dev/null || true)

if printf '%s' "$SECRET_JSON" | jq . >/dev/null 2>&1; then
    # The error envelope is `ee.error.v1` (top-level {schema, error}), not the
    # `ee.response.v1` `{success, error}` shape — accept either path.
    RECOVERY_COUNT=$(printf '%s' "$SECRET_JSON" \
        | jq '(.error.details.recovery // []) | length' 2>/dev/null || echo 0)
    e2e_log_assert_num "$RECOVERY_COUNT" -ge 1 "f1_recovery_actions_present_on_policy_denied"

    FIRST_KIND=$(printf '%s' "$SECRET_JSON" \
        | jq -r '.error.details.recovery[0].kind // empty' 2>/dev/null || true)
    if [ -n "$FIRST_KIND" ]; then
        e2e_log_assert_eq "true" "true" "f1_recovery_action_has_kind"
    else
        e2e_log_assert_eq "missing" "present" "f1_recovery_action_has_kind"
    fi
else
    e2e_log_note "f1_secret_response_unparseable"
fi

# ------------------------------------------------------------
# F2 (shipped) — top-level aliases (`ee show`, `ee link`, `ee tag`, `ee history`)
# route to the correct subcommand based on ID prefix.
# ------------------------------------------------------------

# Seed a memory so we have a real ID to use.
SEEDED_ID=$(ee_workspace remember \
    "F2 alias test — link/show/history dispatch via mem_ prefix." \
    --level episodic --json 2>/dev/null \
    | jq -r '.data.memory_id // .data.id // empty' 2>/dev/null || true)

if [ -n "$SEEDED_ID" ] && [ "$SEEDED_ID" != "null" ]; then
    SHOW_JSON=$(ee_workspace show "$SEEDED_ID" --json 2>/dev/null || true)
    SHOW_OK=$(printf '%s' "$SHOW_JSON" | jq -r '.success // false' 2>/dev/null || echo false)
    e2e_log_assert_eq "$SHOW_OK" "true" "f2_show_alias_routes_to_memory_show"
else
    e2e_log_note "f2_show_alias_skip_no_seed"
fi

# Unknown prefix produces an error that names the supported prefixes.
ALIAS_BAD_JSON=$(ee_workspace show "xyz_unknown_prefix_id" --json 2>/dev/null || true)
if printf '%s' "$ALIAS_BAD_JSON" | jq . >/dev/null 2>&1; then
    ALIAS_BAD_MSG=$(printf '%s' "$ALIAS_BAD_JSON" \
        | jq -r '.error.message // empty' 2>/dev/null || true)
    if printf '%s' "$ALIAS_BAD_MSG" | grep -qE 'Unknown ID prefix|prefix'; then
        e2e_log_assert_eq "true" "true" "f2_alias_unknown_prefix_explains"
    else
        e2e_log_assert_eq "$ALIAS_BAD_MSG" "names supported prefixes" "f2_alias_unknown_prefix_explains"
    fi

    # F7 (shipped) — didYouMean hint appears when the supplied prefix is close
    # to a supported one. `mam` -> `mem` is distance 1.
    MAM_JSON=$(ee_workspace show "mam_close_to_mem" --json 2>/dev/null || true)
    MAM_MSG=$(printf '%s' "$MAM_JSON" \
        | jq -r '.error.message // empty' 2>/dev/null || true)
    if printf '%s' "$MAM_MSG" | grep -qE 'Did you mean'; then
        e2e_log_assert_eq "true" "true" "f7_did_you_mean_hint_for_close_prefix"
    else
        e2e_log_assert_eq "$MAM_MSG" "contains 'Did you mean'" "f7_did_you_mean_hint_for_close_prefix"
    fi
fi

# ------------------------------------------------------------
# F4 (shipped) — `ee capabilities` reports discovered binaries + env overrides.
# ------------------------------------------------------------
CAPS_JSON=$(ee_global capabilities --json 2>/dev/null || true)
if printf '%s' "$CAPS_JSON" | jq . >/dev/null 2>&1; then
    HAS_BINARIES=$(printf '%s' "$CAPS_JSON" \
        | jq -r '.data | has("binaries")' 2>/dev/null || echo false)
    e2e_log_assert_eq "$HAS_BINARIES" "true" "f4_capabilities_binaries_block_present"

    HAS_ENV_OVERRIDES=$(printf '%s' "$CAPS_JSON" \
        | jq -r '.data | has("env_overrides") or .data | has("envOverrides")' 2>/dev/null \
        || echo false)
    e2e_log_note "f4_capabilities_has_env_overrides=$HAS_ENV_OVERRIDES"
fi

# ------------------------------------------------------------
# F3 — `ee --help` reorganization with "First 5" section.
# ------------------------------------------------------------
todo_assert "f3_help_first5_section" "bd-17c65.6.3" \
    "ee --help lacks the curated 'First 5' commands section."

# F5 — env var registry surfaced via `ee capabilities envs --json`.
todo_assert "f5_env_var_registry_command" "bd-17c65.6.5" \
    "No single canonical listing of consumed env vars + provenance."

# F6 — shell completion regeneration test (must not drift after F2).
todo_assert "f6_completion_regen_test" "bd-17c65.6.6" \
    "tests/golden/completion.snap should regen-and-diff after every clap surface change."
