#!/usr/bin/env bash
# P2 artifact relocation e2e harness.
#
# This script is deliberately non-destructive. It creates synthetic artifact
# files under TMPDIR, exercises plan/apply/restore, and never removes originals
# or retained workspaces.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
SCRATCH_ROOT="${TMPDIR:-/tmp}/ee-artifact-relocation-e2e"
RUN_ID="run-$(date -u +%Y%m%dT%H%M%SZ)-$$"
WORKSPACE="$SCRATCH_ROOT/$RUN_ID/workspace"
DESTINATION="$SCRATCH_ROOT/$RUN_ID/external"
MANIFEST_DIR="$SCRATCH_ROOT/$RUN_ID/manifests"
PLAN_MANIFEST="$MANIFEST_DIR/plan.json"
AUDIT_PLAN_MANIFEST="$MANIFEST_DIR/audit-plan.json"
APPLY_MANIFEST="$MANIFEST_DIR/apply.json"
RESTORE_MANIFEST="$MANIFEST_DIR/restore.json"

mkdir -p "$WORKSPACE/target/debug" "$WORKSPACE/tests/audit_artifacts" \
    "$DESTINATION" "$MANIFEST_DIR"
printf 'target artifact bytes\n' > "$WORKSPACE/target/debug/sample.o"
printf 'audit artifact bytes\n' > "$WORKSPACE/tests/audit_artifacts/evidence.json"

run_ee() {
    if [ -n "${EE_BIN:-}" ]; then
        "$EE_BIN" --workspace "$WORKSPACE" "$@"
    elif [ -n "${EE_BINARY:-}" ]; then
        "$EE_BINARY" --workspace "$WORKSPACE" "$@"
    else
        (cd "$REPO_ROOT" && cargo run --quiet -- --workspace "$WORKSPACE" "$@")
    fi
}

source_hash_before="$(shasum -a 256 "$WORKSPACE/target/debug/sample.o")"

plan_report="$(run_ee artifact relocate \
    --from "$WORKSPACE/target/debug/sample.o" \
    --to "$DESTINATION" \
    --manifest "$PLAN_MANIFEST" \
    --json)"

if [ -e "$PLAN_MANIFEST" ]; then
    echo "plan mode wrote a manifest" >&2
    exit 1
fi

printf '%s\n' "$plan_report" |
    jq -e '
      .schema == "ee.response.v1"
      and .success == true
      and .data.schema == "ee.artifact.relocation.v1"
      and .data.mode == "plan"
      and .data.applied == false
      and .data.manifest.entries[0].status == "planned"
      and .data.preservationPolicy == "copy_preserve_no_delete_no_overwrite"
    ' >/dev/null

audit_plan_report="$(run_ee artifact relocate \
    --from "$WORKSPACE/tests/audit_artifacts/evidence.json" \
    --to "$DESTINATION" \
    --manifest "$AUDIT_PLAN_MANIFEST" \
    --json)"

if [ -e "$AUDIT_PLAN_MANIFEST" ]; then
    echo "audit artifact plan mode wrote a manifest" >&2
    exit 1
fi

printf '%s\n' "$audit_plan_report" |
    jq -e --arg original "$WORKSPACE/tests/audit_artifacts/evidence.json" '
      .schema == "ee.response.v1"
      and .success == true
      and .data.mode == "plan"
      and .data.sourceAllowed == true
      and (.data.manifest.entries | length) == 1
      and .data.manifest.entries[0].originalPath == $original
    ' >/dev/null

apply_report="$(run_ee artifact relocate \
    --from "$WORKSPACE/target/debug/sample.o" \
    --to "$DESTINATION" \
    --manifest "$APPLY_MANIFEST" \
    --apply \
    --actor "p2-e2e" \
    --json)"

source_hash_after="$(shasum -a 256 "$WORKSPACE/target/debug/sample.o")"
if [ "$source_hash_before" != "$source_hash_after" ]; then
    echo "apply mode changed the original artifact" >&2
    exit 1
fi

if [ ! -e "$APPLY_MANIFEST" ]; then
    echo "apply mode did not write a manifest" >&2
    exit 1
fi

applied_destination="$DESTINATION/ee-relocated-artifacts/target/debug/sample.o"
if [ ! -e "$applied_destination" ]; then
    echo "apply mode did not copy the artifact" >&2
    exit 1
fi

printf '%s\n' "$apply_report" |
    jq -e --arg original "$WORKSPACE/target/debug/sample.o" \
          --arg destination "$applied_destination" '
      .schema == "ee.response.v1"
      and .success == true
      and .data.mode == "apply"
      and .data.applied == true
      and .data.restored == false
      and .data.manifestHash != null
      and (.data.manifest.entries | length) == 1
      and .data.manifest.entries[0].originalPath == $original
      and .data.manifest.entries[0].destinationPath == $destination
      and (.data.manifest.entries[0].blake3 | startswith("blake3:"))
    ' >/dev/null

restore_original="$WORKSPACE/target/restored/missing.o"
restore_destination="$DESTINATION/ee-relocated-artifacts/target/restored/missing.o"
mkdir -p "$(dirname "$restore_destination")"
printf 'restored artifact bytes\n' > "$restore_destination"

jq -n \
    --arg schema "ee.artifact.relocation.v1" \
    --arg version "p2-e2e" \
    --arg actor "p2-e2e" \
    --arg created_at "2026-05-13T00:00:00Z" \
    --arg workspace "$WORKSPACE" \
    --arg source "$WORKSPACE/target/restored" \
    --arg destination_root "$DESTINATION" \
    --arg restore_command "ee artifact relocate --restore --manifest $RESTORE_MANIFEST --json" \
    --arg original "$restore_original" \
    --arg destination "$restore_destination" \
    '{
      schema: $schema,
      commandVersion: $version,
      actor: $actor,
      createdAt: $created_at,
      workspacePath: $workspace,
      sourcePath: $source,
      destinationRoot: $destination_root,
      restorationCommand: $restore_command,
      forceWithExplicitPath: false,
      entries: [{
        originalPath: $original,
        destinationPath: $destination,
        kind: "file",
        sizeBytes: 24,
        mtimeUnixSeconds: null,
        blake3: null,
        status: "planned"
      }]
    }' > "$RESTORE_MANIFEST"

restore_report="$(run_ee artifact relocate --restore --manifest "$RESTORE_MANIFEST" --json)"

if [ "$(cat "$restore_original")" != "restored artifact bytes" ]; then
    echo "restore mode did not copy the preserved artifact back" >&2
    exit 1
fi

printf '%s\n' "$restore_report" |
    jq -e '
      .schema == "ee.response.v1"
      and .success == true
      and .data.mode == "restore"
      and .data.restored == true
      and .data.preservationPolicy == "copy_preserve_no_delete_no_overwrite"
    ' >/dev/null

jq -n \
    --arg schema "ee.artifact_relocation.e2e.v1" \
    --arg workspace "$WORKSPACE" \
    --arg manifest "$APPLY_MANIFEST" \
    --arg restore_manifest "$RESTORE_MANIFEST" \
    '{
      schema: $schema,
      success: true,
      workspace: $workspace,
      applyManifest: $manifest,
      restoreManifest: $restore_manifest,
      mutation: "copy_preserve_only",
      note: "Synthetic workspace intentionally left in place for audit."
    }'
