#!/usr/bin/env bash
# P1 disk-pressure e2e harness.
#
# This script is deliberately non-destructive. It creates a synthetic workspace
# under TMPDIR, runs `ee diag disk-pressure --json`, and verifies the diagnostic
# did not mutate the synthetic files. It does not delete the workspace.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/e2e_overhaul/lib/shared.sh
# shellcheck disable=SC1091
source "$SCRIPT_DIR/lib/shared.sh"
require_jq

e2e_log_start "disk_pressure"
trap e2e_log_end EXIT

SCRATCH_ROOT="${TMPDIR:-/tmp}/ee-disk-pressure-e2e"
RUN_ID="run-$(date -u +%Y%m%dT%H%M%SZ)-$$"
WORKSPACE="$SCRATCH_ROOT/$RUN_ID/workspace"

mkdir -p "$WORKSPACE/.ee" "$WORKSPACE/tests/audit_artifacts" "$WORKSPACE/target/debug" \
    "$WORKSPACE/tmp"
printf 'workspace-state\n' > "$WORKSPACE/.ee/ee.db.placeholder"
printf 'audit-artifact\n' > "$WORKSPACE/tests/audit_artifacts/sample.json"
printf 'build-artifact\n' > "$WORKSPACE/target/debug/sample.o"
printf 'scratch\n' > "$WORKSPACE/tmp/sample.tmp"

snapshot() {
    find "$WORKSPACE" -type f -print0 |
        sort -z |
        xargs -0 shasum -a 256
}

before_snapshot="$(snapshot)"

EE_UNDER_TEST="${EE_BIN:-${EE_BINARY:-}}"
if [ -n "$EE_UNDER_TEST" ]; then
    report="$("$EE_UNDER_TEST" --workspace "$WORKSPACE" diag disk-pressure --json \
        --top-limit 3 --consumer-depth 1 --consumer-entry-limit 100)"
else
    report="$(cd "$REPO_ROOT" && cargo run --quiet -- --workspace "$WORKSPACE" \
        diag disk-pressure --json --top-limit 3 --consumer-depth 1 \
        --consumer-entry-limit 100)"
fi

after_snapshot="$(snapshot)"

e2e_log_assert_eq "$after_snapshot" "$before_snapshot" "disk_pressure_no_mutation"

assert_jq "$report" '.schema' "ee.response.v1" "disk_pressure_response_schema"
assert_jq "$report" '.success' "true" "disk_pressure_response_success"
assert_jq "$report" '.data.schema' "ee.disk_pressure.diagnostics.v1" \
    "disk_pressure_data_schema"
assert_jq "$report" '.data.sideEffectFree' "true" "disk_pressure_side_effect_free"
assert_jq "$report" '.data.mutationPolicy' "read_only_report_no_files_modified" \
    "disk_pressure_mutation_policy"
assert_jq "$report" '(.data.roots | map(.label) | index("workspace") != null)' \
    "true" "disk_pressure_workspace_root"
assert_jq "$report" '(.data.roots | map(.label) | index("cargo_target") != null)' \
    "true" "disk_pressure_cargo_target_root"
# shellcheck disable=SC2016
assert_jq "$report" '(.data.recoveryActions | all(.kind as $kind |
    ["move_preserve", "compress_preserve", "rotate_with_manifest", "ask_human", "noop"]
    | index($kind) != null))' "true" "disk_pressure_recovery_actions_preserve_only"

jq -n \
    --arg schema "ee.disk_pressure.e2e.v1" \
    --arg workspace "$WORKSPACE" \
    --arg posture "$(printf '%s\n' "$report" | jq -r '.data.posture')" \
    --arg mutation "none" \
    '{
      schema: $schema,
      success: true,
      workspace: $workspace,
      posture: $posture,
      mutation: $mutation,
      note: "Synthetic workspace intentionally left in place for audit."
    }'
