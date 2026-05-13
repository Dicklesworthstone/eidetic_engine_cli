#!/usr/bin/env bash
# P1 disk-pressure e2e harness.
#
# This script is deliberately non-destructive. It creates a synthetic workspace
# under TMPDIR, runs `ee diag disk-pressure --json`, and verifies the diagnostic
# did not mutate the synthetic files. It does not delete the workspace.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
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

if [ -n "${EE_BIN:-}" ]; then
    report="$("$EE_BIN" --workspace "$WORKSPACE" diag disk-pressure --json \
        --top-limit 3 --consumer-depth 1 --consumer-entry-limit 100)"
else
    report="$(cd "$REPO_ROOT" && cargo run --quiet -- --workspace "$WORKSPACE" \
        diag disk-pressure --json --top-limit 3 --consumer-depth 1 \
        --consumer-entry-limit 100)"
fi

after_snapshot="$(snapshot)"

if [ "$before_snapshot" != "$after_snapshot" ]; then
    echo "disk-pressure diagnostic mutated the synthetic workspace" >&2
    exit 1
fi

printf '%s\n' "$report" |
    jq -e '
      .schema == "ee.response.v1"
      and .success == true
      and .data.schema == "ee.disk_pressure.diagnostics.v1"
      and .data.sideEffectFree == true
      and .data.mutationPolicy == "read_only_report_no_files_modified"
      and (.data.roots | map(.label) | index("workspace") != null)
      and (.data.roots | map(.label) | index("cargo_target") != null)
      and (.data.recoveryActions | all(.kind as $kind |
        ["move_preserve", "compress_preserve", "rotate_with_manifest", "ask_human", "noop"]
        | index($kind) != null))
    ' >/dev/null

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
