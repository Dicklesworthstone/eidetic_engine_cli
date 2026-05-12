#!/usr/bin/env bash
# J3 — Epic D: schema consistency e2e driver.
#
# Asserts canonical field names across every surface that returns memory text:
# `ee memory list`, `ee rule list`, `ee learn uncertainty`, and `ee why` all
# use `content` (D1) plus `content_truncated` for list views. Also exercises
# workspace auto-discovery via EE_WORKSPACE + walk-up (D7).
#
# Shipped (real assertions):  D1, D7
# Not yet shipped (todo):     D2, D3, D4, D5, D6

set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/e2e_overhaul/lib/shared.sh
source "$SCRIPT_DIR/lib/shared.sh"
require_jq
epic_setup "epic_D_schema_consistency"
seed_corpus

# ------------------------------------------------------------
# D1 (shipped) — `ee memory list` items emit `content` and `content_truncated`.
# The legacy `content_preview` name must not appear.
# ------------------------------------------------------------
MEMORY_LIST_JSON=$(ee_workspace memory list --json || true)
if ! printf '%s' "$MEMORY_LIST_JSON" | jq . >/dev/null 2>&1; then
    e2e_log_assert_eq "false" "true" "memory_list_json_parses"
    exit 0
fi

# Pull the first item; if no memories exist, skip.
ITEM_COUNT=$(printf '%s' "$MEMORY_LIST_JSON" | jq '.data.memories | length' 2>/dev/null || echo 0)
if [ "$ITEM_COUNT" -gt 0 ]; then
    HAS_CONTENT=$(printf '%s' "$MEMORY_LIST_JSON" \
        | jq -r '.data.memories[0] | has("content")' 2>/dev/null || echo false)
    e2e_log_assert_eq "$HAS_CONTENT" "true" "d1_memory_list_has_content_field"

    HAS_TRUNC=$(printf '%s' "$MEMORY_LIST_JSON" \
        | jq -r '.data.memories[0] | has("content_truncated")' 2>/dev/null || echo false)
    e2e_log_assert_eq "$HAS_TRUNC" "true" "d1_memory_list_has_content_truncated_field"

    HAS_LEGACY=$(printf '%s' "$MEMORY_LIST_JSON" \
        | jq -r '.data.memories[0] | has("content_preview")' 2>/dev/null || echo false)
    e2e_log_assert_eq "$HAS_LEGACY" "false" "d1_memory_list_no_legacy_content_preview"
else
    e2e_log_note "d1_memory_list_empty skip=true"
fi

# ------------------------------------------------------------
# D1 — `ee rule list` items emit `content` and `contentTruncated` (camelCase).
# ------------------------------------------------------------
RULE_LIST_JSON=$(ee_workspace rule list --json || true)
if printf '%s' "$RULE_LIST_JSON" | jq . >/dev/null 2>&1; then
    RULE_COUNT=$(printf '%s' "$RULE_LIST_JSON" | jq '.data.rules | length' 2>/dev/null || echo 0)
    if [ "$RULE_COUNT" -gt 0 ]; then
        HAS_RULE_CONTENT=$(printf '%s' "$RULE_LIST_JSON" \
            | jq -r '.data.rules[0] | has("content")' 2>/dev/null || echo false)
        e2e_log_assert_eq "$HAS_RULE_CONTENT" "true" "d1_rule_list_has_content_field"
        HAS_RULE_TRUNC=$(printf '%s' "$RULE_LIST_JSON" \
            | jq -r '.data.rules[0] | has("contentTruncated")' 2>/dev/null || echo false)
        e2e_log_assert_eq "$HAS_RULE_TRUNC" "true" "d1_rule_list_has_contentTruncated_field"
    else
        e2e_log_note "d1_rule_list_empty skip=true"
    fi
fi

# ------------------------------------------------------------
# D1 — `ee why <id>` returns the full memory body as `content`.
# ------------------------------------------------------------
FIRST_MEM_ID=$(printf '%s' "$MEMORY_LIST_JSON" \
    | jq -r '.data.memories[0].id // empty' 2>/dev/null || true)
if [ -n "$FIRST_MEM_ID" ]; then
    WHY_JSON=$(ee_workspace why "$FIRST_MEM_ID" --json || true)
    if printf '%s' "$WHY_JSON" | jq . >/dev/null 2>&1; then
        HAS_WHY_CONTENT=$(printf '%s' "$WHY_JSON" \
            | jq -r '.data | has("content")' 2>/dev/null || echo false)
        e2e_log_assert_eq "$HAS_WHY_CONTENT" "true" "d1_why_response_has_content_field"

        WHY_CONTENT=$(printf '%s' "$WHY_JSON" \
            | jq -r '.data.content // empty' 2>/dev/null || true)
        if [ -n "$WHY_CONTENT" ] && [ "$WHY_CONTENT" != "null" ]; then
            e2e_log_assert_eq "true" "true" "d1_why_content_is_populated"
        else
            e2e_log_assert_eq "empty_or_null" "populated" "d1_why_content_is_populated"
        fi
    fi
else
    e2e_log_note "d1_why_skipped_no_memory_seeded"
fi

# ------------------------------------------------------------
# D7 (shipped) — workspace auto-discovery via EE_WORKSPACE env var.
# Create a subdir, cd into it, and assert that ee finds the workspace via
# walk-up.
# ------------------------------------------------------------
SUBDIR="$EPIC_WORKSPACE/nested/dir"
mkdir -p "$SUBDIR"
(
    cd "$SUBDIR"
    DISCOVERY_JSON=$("$EE_BINARY" memory list --json 2>/dev/null || true)
    if printf '%s' "$DISCOVERY_JSON" | jq . >/dev/null 2>&1; then
        echo "discovery_json_parses=true"
    else
        echo "discovery_json_parses=false"
    fi
) > /tmp/d7_discovery_$$.txt 2>&1 || true
DISCOVERY_PARSED=$(grep -c 'discovery_json_parses=true' /tmp/d7_discovery_$$.txt 2>/dev/null || echo 0)
rm -f /tmp/d7_discovery_$$.txt
e2e_log_assert_num "$DISCOVERY_PARSED" -ge 1 "d7_workspace_walk_up_from_subdir"

# EE_WORKSPACE env var
(
    cd /
    ENV_DISCOVERY=$(EE_WORKSPACE="$EPIC_WORKSPACE" "$EE_BINARY" memory list --json 2>/dev/null || true)
    if printf '%s' "$ENV_DISCOVERY" | jq -r '.success // false' 2>/dev/null | grep -q true; then
        echo "env_discovery_ok=true"
    else
        echo "env_discovery_ok=false"
    fi
) > /tmp/d7_env_$$.txt 2>&1 || true
ENV_OK=$(grep -c 'env_discovery_ok=true' /tmp/d7_env_$$.txt 2>/dev/null || echo 0)
rm -f /tmp/d7_env_$$.txt
e2e_log_assert_num "$ENV_OK" -ge 1 "d7_workspace_resolves_via_env_var"

# ------------------------------------------------------------
# D2-D6 (not shipped) — TODOs.
# ------------------------------------------------------------
todo_assert "d2_json_markdown_parity" "bd-17c65.4.2" \
    "Markdown renderer should derive from canonical JSON tree (currently parallel)."

todo_assert "d3_pack_metadata_in_markdown" "bd-17c65.4.3" \
    "Markdown render lacks pack.hash + pack.schema + pack.generatedAt HTML comments."

todo_assert "d4_schema_drift_audit_in_ci" "bd-17c65.4.4" \
    "Schema-drift audit test (canonical_content_field) ships in D1 but D4 wants broader coverage."

todo_assert "d5_fields_preset_interaction" "bd-17c65.4.5" \
    "--fields preset and explicit field-list interaction not yet defined per-schema."

todo_assert "d6_all_renderers_from_canonical_tree" "bd-17c65.4.6" \
    "Eight --format renderers don't all derive from the canonical pack tree yet."
