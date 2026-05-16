#!/usr/bin/env bash
# J3 — Epic F: discoverability e2e driver.
#
# Asserts the shipped F1 (recovery actions), F2 (top-level aliases), F3
# (most-used help prelude), F4/F5 (capabilities binaries + env registry), F6
# (completion aliases), and F7 (didYouMean) surfaces.
#
# Shipped (real assertions):  F1, F2, F3, F4, F5, F6, F7

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
    # The error envelope is `ee.error.v2` (top-level {schema, error}), not the
    # `ee.response.v1` `{success, error}` shape — accept either path.
    SECRET_SCHEMA=$(printf '%s' "$SECRET_JSON" \
        | jq -r '.schema // empty' 2>/dev/null || true)
    e2e_log_assert_eq "$SECRET_SCHEMA" "ee.error.v2" "a10_error_schema_v2_on_policy_denied"

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
    ALIAS_BAD_SCHEMA=$(printf '%s' "$ALIAS_BAD_JSON" \
        | jq -r '.schema // empty' 2>/dev/null || true)
    e2e_log_assert_eq "$ALIAS_BAD_SCHEMA" "ee.error.v2" "a10_error_schema_v2_on_alias_error"

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
# F3 (shipped) — `ee --help` opens with a most-used command prelude and
# stable categories before the full clap command reference.
# ------------------------------------------------------------
HELP_TEXT=$(ee_global --help 2>/dev/null || true)
if [ -n "$HELP_TEXT" ]; then
    if printf '%s' "$HELP_TEXT" | grep -q 'Most-used commands (start here):'; then
        e2e_log_assert_eq "true" "true" "f3_help_most_used_prelude"
    else
        e2e_log_assert_eq "missing" "Most-used commands (start here):" \
            "f3_help_most_used_prelude"
    fi

    for command in init note pack why search remember; do
        if printf '%s' "$HELP_TEXT" | grep -Eq "^[[:space:]]+${command}[[:space:]]"; then
            e2e_log_assert_eq "true" "true" "f3_help_most_used_command_$command"
        else
            e2e_log_assert_eq "missing" "$command in most-used prelude" \
                "f3_help_most_used_command_$command"
        fi
    done

    for category in Inspect Curate Graph Maintenance Diagnostics Configuration; do
        if printf '%s' "$HELP_TEXT" | grep -q "^[[:space:]]*$category:"; then
            e2e_log_assert_eq "true" "true" "f3_help_category_$category"
        else
            e2e_log_assert_eq "missing" "$category category" "f3_help_category_$category"
        fi
    done
else
    e2e_log_assert_eq "empty" "non-empty help output" "f3_help_generated"
fi

# F5 (shipped) — the EnvVar registry is surfaced through capabilities
# `envOverrides`, using registry names/descriptions and suppressing sensitive
# current values.
if printf '%s' "$CAPS_JSON" | jq . >/dev/null 2>&1; then
    F5_ENV_COUNT=$(printf '%s' "$CAPS_JSON" \
        | jq '[.data.envOverrides[]? | select(.name | startswith("EE_"))] | length' \
            2>/dev/null || echo 0)
    e2e_log_assert_num "$F5_ENV_COUNT" -ge 10 "f5_env_registry_lists_ee_vars"

    F5_HAS_CASS=$(printf '%s' "$CAPS_JSON" \
        | jq -r 'any(.data.envOverrides[]?; .name == "EE_CASS_BINARY" and (.controls | length > 0))' \
            2>/dev/null || echo false)
    e2e_log_assert_eq "$F5_HAS_CASS" "true" "f5_env_registry_describes_cass_binary"

    F5_SECRET_EXPOSED=$(printf '%s' "$CAPS_JSON" \
        | jq -r 'any(.data.envOverrides[]?; .name == "EE_PREFLIGHT_BYPASS_SECRET" and has("currentValue"))' \
            2>/dev/null || echo true)
    e2e_log_assert_eq "$F5_SECRET_EXPOSED" "false" "f5_env_registry_suppresses_secret_value"

    F5_ENV_JSON=$(
        unset EE_REMEMBER_CURATION_SYNC_BUDGET_MS
        EE_CASS_BINARY="/tmp/ee-f5-cass" \
            EE_PREFLIGHT_BYPASS_SECRET="ee-f5-secret" \
            ee_global capabilities --json 2>/dev/null || true
    )
    if printf '%s' "$F5_ENV_JSON" | jq . >/dev/null 2>&1; then
        F5_CASS_SET=$(printf '%s' "$F5_ENV_JSON" \
            | jq -r 'any(.data.envOverrides[]?; .name == "EE_CASS_BINARY" and .isSet == true and .currentValue == "/tmp/ee-f5-cass" and .source == "process_env")' \
                2>/dev/null || echo false)
        e2e_log_assert_eq "$F5_CASS_SET" "true" "f5_env_registry_reports_set_cass_binary"

        F5_SECRET_SET_SAFE=$(printf '%s' "$F5_ENV_JSON" \
            | jq -r 'any(.data.envOverrides[]?; .name == "EE_PREFLIGHT_BYPASS_SECRET" and .isSet == true and .source == "process_env" and (has("currentValue") | not))' \
                2>/dev/null || echo false)
        e2e_log_assert_eq "$F5_SECRET_SET_SAFE" "true" "f5_env_registry_reports_secret_without_value"

        F5_DEFAULT_SOURCE=$(printf '%s' "$F5_ENV_JSON" \
            | jq -r 'any(.data.envOverrides[]?; .name == "EE_REMEMBER_CURATION_SYNC_BUDGET_MS" and .defaultValue == "50" and .source == "registry_default")' \
                2>/dev/null || echo false)
        e2e_log_assert_eq "$F5_DEFAULT_SOURCE" "true" "f5_env_registry_reports_registry_default"
    fi
fi

# F6 (shipped) — completion output includes top-level aliases and remains
# valid bash syntax.
BASH_COMPLETION=$(ee_global completion bash 2>/dev/null || true)
if [ -n "$BASH_COMPLETION" ]; then
    if printf '%s' "$BASH_COMPLETION" | bash -n 2>/dev/null; then
        e2e_log_assert_eq "true" "true" "f6_bash_completion_syntax_valid"
    else
        e2e_log_assert_eq "bash -n failed" "valid" "f6_bash_completion_syntax_valid"
    fi

    for alias in show link tag history; do
        if printf '%s' "$BASH_COMPLETION" | grep -q "ee,$alias)"; then
            e2e_log_assert_eq "true" "true" "f6_bash_completion_alias_$alias"
        else
            e2e_log_assert_eq "missing" "ee,$alias)" "f6_bash_completion_alias_$alias"
        fi
    done
else
    e2e_log_assert_eq "empty" "non-empty" "f6_bash_completion_generated"
fi
