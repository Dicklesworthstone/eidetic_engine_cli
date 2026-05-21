#!/usr/bin/env bash
# bd-3qs2i.2.6 — F2.6 e2e for `pack_budget_too_small`.
#
# Exercises the full agent flow against a real ee binary in a fresh workspace.
# The script pins the regression path where retrieval finds candidates but the
# pack budget is too small to include any item.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"

if [ -z "${WORKSPACE:-}" ]; then
    workspace_root="$REPO_ROOT/tests/logs/agent_ergonomics_workspaces"
    mkdir -p "$workspace_root"
    WORKSPACE="$(mktemp -d "$workspace_root/ee-e2e-f2-XXXX")"
fi
export WORKSPACE

# shellcheck source=scripts/e2e_lib/agent_ergonomics_lib.sh
# shellcheck disable=SC1091
source "$SCRIPT_DIR/agent_ergonomics_lib.sh"

ee_workspace() {
    "$EE_BIN" --workspace "$WORKSPACE" "$@"
}

has_pack_budget_too_small='((.data.degraded // []) | map(.code) | contains(["pack_budget_too_small"]))'
has_no_relevant_results='((.data.degraded // []) | map(.code) | contains(["no_relevant_results"]))'

remember_release_memory() {
    local index="${1:?index required}"
    ee_workspace remember \
        "Release ritual memory $index: before publishing, preserve provenance, inspect structured degraded recovery actions, run remote-only RCH verification, keep stdout machine-readable, and cite remediation beads when preflight blocks remote proof." \
        --level semantic \
        --kind fact \
        --json
}

# ---------------------------------------------------------------------------
log_step "Initialize fresh workspace"
init_out=$(ee_workspace init --json)
assert_jq "$init_out" '.success' "true" "init returns success=true"
assert_jq "$init_out" \
    '(.data.status == "created" or .data.status == "already_exists" or .data.status == "revalidated")' \
    "true" "init produced a usable workspace"

# ---------------------------------------------------------------------------
log_step "Verify workspace is fresh"
fresh_out=$(ee_workspace memory list --json)
assert_jq "$fresh_out" '(.data.memories // []) | length' "0" \
    "workspace has zero memories"

# ---------------------------------------------------------------------------
log_step "Populate release-ritual memories"
for i in 1 2 3 4 5 6 7 8 9 10; do
    remember_out=$(remember_release_memory "$i")
    assert_jq "$remember_out" '.success' "true" "remember $i succeeds"
done

# ---------------------------------------------------------------------------
log_step "Tight budget trips pack_budget_too_small"
tight_out=$(ee_workspace context "release ritual" --max-tokens 1 --json)
assert_jq "$tight_out" '.success' "true" "tight context call succeeds"
assert_jq "$tight_out" "$has_pack_budget_too_small" "true" \
    "pack_budget_too_small appears in degraded[]"
assert_jq "$tight_out" \
    '(.data.degraded[] | select(.code == "pack_budget_too_small") | .severity)' \
    "warning" "severity is warning"
assert_jq "$tight_out" \
    '(.data.degraded[] | select(.code == "pack_budget_too_small") | .details.recovery | length)' \
    "3" "recovery has three actions"
assert_jq "$tight_out" \
    '(.data.degraded[] | select(.code == "pack_budget_too_small") | .details.recovery[0].kind)' \
    "flag" "first recovery action is a flag"
assert_jq "$tight_out" \
    '(.data.degraded[] | select(.code == "pack_budget_too_small") | .details.recovery[0].flagName)' \
    "--max-tokens" "first recovery flag is --max-tokens"
assert_jq "$tight_out" \
    '(.data.degraded[] | select(.code == "pack_budget_too_small") | .details.recovery[1].flagName)' \
    "--profile" "second recovery flag is --profile"
assert_jq "$tight_out" \
    '(.data.degraded[] | select(.code == "pack_budget_too_small") | .details.recovery[2].kind)' \
    "broaden" "third recovery action is broaden"

# ---------------------------------------------------------------------------
log_step "Items array is empty when budget code fires"
assert_jq "$tight_out" '(.data.pack.items // []) | length' "0" \
    "items empty under too-small budget"

# ---------------------------------------------------------------------------
log_step "Wide budget selects items and suppresses pack_budget_too_small"
wide_out=$(ee_workspace context "release ritual" --max-tokens 4000 --json)
assert_jq "$wide_out" "$has_pack_budget_too_small" "false" \
    "pack_budget_too_small absent on wide budget"
assert_jq "$wide_out" '((.data.pack.items // []) | length) > 0' "true" \
    "wide budget selects at least one item"

# ---------------------------------------------------------------------------
log_step "Empty retrieval pool remains no_relevant_results only"
empty_workspace="$WORKSPACE-empty"
mkdir -p "$empty_workspace"
empty_init=$("$EE_BIN" --workspace "$empty_workspace" init --json)
assert_jq "$empty_init" '.success' "true" "empty workspace init succeeds"
empty_out=$("$EE_BIN" --workspace "$empty_workspace" context "release ritual" --max-tokens 1 --json)
assert_jq "$empty_out" "$has_no_relevant_results" "true" \
    "no_relevant_results fires for empty pool"
assert_jq "$empty_out" "$has_pack_budget_too_small" "false" \
    "pack_budget_too_small absent for empty pool"

# ---------------------------------------------------------------------------
log_step "Recovery advice to raise --max-tokens fixes the pack"
raised_out=$(ee_workspace context "release ritual" --max-tokens 8000 --json)
assert_jq "$raised_out" "$has_pack_budget_too_small" "false" \
    "raised budget clears pack_budget_too_small"
assert_jq "$raised_out" '((.data.pack.items // []) | length) > 0' "true" \
    "raised budget selects items"

# ---------------------------------------------------------------------------
log_step "Compact profile retry completes"
compact_out=$(ee_workspace context "release ritual" --max-tokens 1 --profile compact --json)
assert_jq "$compact_out" '.success' "true" "compact profile retry succeeds"

# ---------------------------------------------------------------------------
log_step "Tracing target fires on budget-exhausted pack"
trace_log="$LOG_DIR/trace.log"
RUST_LOG=ee::pack::budget_exhausted=warn EE_LOG_JSON=1 \
    "$EE_BIN" --workspace "$WORKSPACE" context "release ritual" \
    --max-tokens 1 \
    --json >"$LOG_DIR/trace_context.json" 2>"$trace_log"
assert_contains "$(cat "$trace_log")" "ee::pack::budget_exhausted" \
    "tracing target ee::pack::budget_exhausted emitted"
assert_contains "$(cat "$trace_log")" "pack budget too small" \
    "trace event describes budget exhaustion"
