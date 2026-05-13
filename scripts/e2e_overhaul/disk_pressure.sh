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
    "$WORKSPACE/target/ee-e2e/run-a" "$WORKSPACE/target/ee-golden-artifacts" \
    "$WORKSPACE/target/ee-bench" "$WORKSPACE/.ee/support-bundles" "$WORKSPACE/tmp"
printf 'workspace-state\n' > "$WORKSPACE/.ee/ee.db.placeholder"
printf 'audit-artifact\n' > "$WORKSPACE/tests/audit_artifacts/sample.json"
printf 'build-artifact\n' > "$WORKSPACE/target/debug/sample.o"
printf 'e2e-artifact\n' > "$WORKSPACE/target/ee-e2e/run-a/stdout.txt"
printf 'golden-artifact\n' > "$WORKSPACE/target/ee-golden-artifacts/context.json"
printf 'bench-artifact\n' > "$WORKSPACE/target/ee-bench/bench.json"
printf 'support-bundle\n' > "$WORKSPACE/.ee/support-bundles/bundle.json"
printf '{"schema":"ee.e2e.retention_manifest.v1"}\n' \
    > "$WORKSPACE/tmp/e2e_retention_manifest.json"
printf '{"schema":"ee.test_event.v1","kind":"note"}\n' > "$WORKSPACE/tmp/j1.jsonl"
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
    artifacts_report="$(CARGO_TARGET_DIR="$WORKSPACE/target" \
        TMPDIR="$WORKSPACE/tmp" \
        EE_TEST_LOG_PATH="$WORKSPACE/tmp/j1.jsonl" \
        EE_E2E_RETENTION_MANIFEST="$WORKSPACE/tmp/e2e_retention_manifest.json" \
        "$EE_UNDER_TEST" --workspace "$WORKSPACE" diag artifacts --json \
        --top-limit 3 --consumer-depth 1 --consumer-entry-limit 100)"
else
    report="$(cd "$REPO_ROOT" && cargo run --quiet -- --workspace "$WORKSPACE" \
        diag disk-pressure --json --top-limit 3 --consumer-depth 1 \
        --consumer-entry-limit 100)"
    artifacts_report="$(cd "$REPO_ROOT" && CARGO_TARGET_DIR="$WORKSPACE/target" \
        TMPDIR="$WORKSPACE/tmp" \
        EE_TEST_LOG_PATH="$WORKSPACE/tmp/j1.jsonl" \
        EE_E2E_RETENTION_MANIFEST="$WORKSPACE/tmp/e2e_retention_manifest.json" \
        cargo run --quiet -- --workspace "$WORKSPACE" diag artifacts --json \
        --top-limit 3 --consumer-depth 1 --consumer-entry-limit 100)"
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

assert_jq "$artifacts_report" '.schema' "ee.response.v1" "artifact_retention_response_schema"
assert_jq "$artifacts_report" '.success' "true" "artifact_retention_response_success"
assert_jq "$artifacts_report" '.data.schema' "ee.artifact_retention.diagnostics.v1" \
    "artifact_retention_data_schema"
assert_jq "$artifacts_report" '.data.sideEffectFree' "true" \
    "artifact_retention_side_effect_free"
assert_jq "$artifacts_report" '.data.mutationPolicy' \
    "read_only_report_no_files_modified_no_cleanup" \
    "artifact_retention_mutation_policy"
assert_jq "$artifacts_report" '.data.summary.j1LogConfigured' "true" \
    "artifact_retention_j1_log_configured"
assert_jq "$artifacts_report" '.data.summary.retentionManifestConfigured' "true" \
    "artifact_retention_manifest_configured"
assert_jq "$artifacts_report" '(.data.roots | map(.label) |
    index("tests_audit_artifacts") != null
    and index("cargo_target_e2e") != null
    and index("golden_artifacts") != null
    and index("bench_artifacts") != null
    and index("support_bundles") != null
    and index("j1_current_log") != null
    and index("current_retention_manifest") != null)' "true" \
    "artifact_retention_expected_roots"
# shellcheck disable=SC2016
assert_jq "$artifacts_report" '(.data.actions | all(.kind as $kind |
    ["keep", "move_preserve", "compress_preserve", "eligible_for_human_cleanup"]
    | index($kind) != null))' "true" "artifact_retention_preserve_only_actions"
assert_jq "$artifacts_report" '(.data.roots | all(
    (.retentionReason | length > 0)
    and (.budget.warningBytes >= 0)
    and (.budget.degradedBytes >= .budget.warningBytes)))' "true" \
    "artifact_retention_budget_metadata"

jq -n \
    --arg schema "ee.disk_pressure.e2e.v1" \
    --arg workspace "$WORKSPACE" \
    --arg posture "$(printf '%s\n' "$report" | jq -r '.data.posture')" \
    --arg artifact_roots "$(printf '%s\n' "$artifacts_report" | jq -r '.data.summary.rootCount')" \
    --arg mutation "none" \
    '{
      schema: $schema,
      success: true,
      workspace: $workspace,
      posture: $posture,
      artifactRoots: ($artifact_roots | tonumber),
      mutation: $mutation,
      note: "Synthetic workspace intentionally left in place for audit."
    }'
