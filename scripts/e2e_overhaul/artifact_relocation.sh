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

# THE REASON THIS SUITE COULD EXIT 1 HAVING PRINTED ZERO BYTES (bd-63mua).
#
# `ee` writes its error envelope to STDOUT, not stderr. Measured 2026-10-19
# against ee 0.14.2:
#
#     ee --workspace /nonexistent search q --json
#       exit 10, STDOUT 2116 bytes ({"schema":"ee.error.v2", ... "repair": ...}),
#       STDERR 0 bytes
#
# So `report="$(run_ee ... --json)"` under the `set -euo pipefail` at the top of
# this file captures the ENTIRE diagnostic into a variable, leaves stderr empty,
# and -e aborts at the assignment before any line can print what was captured.
# The result is a nonzero exit with no output anywhere -- a failure nobody can
# triage, which is exactly what this suite was reported doing.
#
# The product's error envelopes are good. They are simply invisible at the one
# moment they are needed, because the capture that swallows them is also the
# thing that kills the script.
#
# capture_ee prints the swallowed envelope to stderr before failing. The `exit`
# inside it ends the command-substitution SUBSHELL, so the outer assignment
# still fails and -e still aborts the suite -- but the diagnostic has already
# reached stderr by then.
capture_ee() {
    local label="$1"
    shift
    local out
    local rc=0
    out="$(run_ee "$@")" || rc=$?
    if [ "$rc" -ne 0 ]; then
        echo "artifact_relocation: $label failed (ee exit $rc)." >&2
        echo "artifact_relocation: ee writes ee.error.v2 to STDOUT, which the" \
            "capture swallowed; reproducing it here:" >&2
        printf '%s\n' "$out" >&2
        exit 1
    fi
    printf '%s' "$out"
}

# THE OTHER HALF OF THE ZERO-BYTE FAILURE (bd-63mua), and the one that actually
# fired. Each assertion in this suite was a bare pipeline:
#
#     printf '%s\n' "$report" | jq -e '<filter>' >/dev/null
#
# `jq -e` exits 1 when its result is `false`, prints only `false` -- and that
# went to /dev/null. As a bare pipeline under `set -euo pipefail` the suite then
# aborted having emitted NOTHING: no clause name, no report, no jq verdict.
#
# Traced 2026-10-19: the clause that fails is
# `.data.manifest.entries[0].originalPath == $original`, because `ee` REDACTS
# that field. At this surface's default redaction level a value whose entropy
# is at or above the threshold becomes `[REDACTED:<class>]`, and a macOS
# TMPDIR (`/var/folders/vt/n2xyn_s51b97.../T/`) plus a random run id clears it
# easily -- the observed value was `[REDACTED_PATH]`, with sibling fields
# reading `[REDACTED:high_entropy_secret]`. So a path-equality assertion here
# is unsatisfiable, and whether it is depends on how random TMPDIR looks, which
# is why this reads as environment flakiness rather than a stale assertion.
#
# Fixing the ASSERTION needs a product flag I cannot exercise: `--redaction
# <level>` is documented in docs/redaction_levels.md but is not present in the
# only binary runnable here (0.14.2 rejects it as an unexpected argument), and
# the current tree cannot be built on this host. So that half is reported on the
# bead rather than guessed at.
#
# This half is unambiguous and is what the bead asks for: the suite must be able
# to say what failed.
assert_json() {
    local label="$1"
    local report="$2"
    shift 2
    local rc=0
    printf '%s\n' "$report" | jq -e "$@" >/dev/null || rc=$?
    if [ "$rc" -ne 0 ]; then
        echo "artifact_relocation: ASSERTION FAILED: $label (jq exit $rc)" >&2
        echo "artifact_relocation: jq filter was: $*" >&2
        echo "artifact_relocation: the report it checked:" >&2
        printf '%s\n' "$report" | jq -S '.' >&2 2>/dev/null \
            || printf '%s\n' "$report" >&2
        exit 1
    fi
}

# WHY THIS IS A FUNCTION AND NOT A BARE `shasum` (bd-63mua).
#
# This suite was measured exiting 1 in 0s having printed ZERO BYTES -- a
# failure nobody, including its author, could triage from its own output. Both
# hash sites were bare assignments from a command substitution under the
# `set -euo pipefail` at the top of this file, which is the dead-handler shape:
# -e aborts AT the assignment, so no line after it runs and the script has no
# opportunity to say what happened. Whatever the first failure was, it could
# not announce itself.
#
# Two separate problems, both fixed here:
#
#   1. PORTABILITY. `shasum` is a perl tool present by default on macOS; the
#      graders for this suite are Linux workers, where `sha256sum` is the
#      standard name. Five scripts in this repo already carry the
#      `command -v` fallback (bridge-staleness.sh:591 among them), so the
#      idiom is established -- these two sites simply did not use it.
#
#   2. SELF-DIAGNOSIS. Every failure path now prints a named reason to stderr
#      before exiting, so this script can never again hand back a bare exit
#      code. `|| rc=$?` keeps -e suspended through the call so the handler
#      actually runs; a bare `hash_file ...` would abort first and reproduce
#      the original defect.
hash_file() {
    local path="$1"
    if [ ! -f "$path" ]; then
        echo "artifact_relocation: cannot hash missing file: $path" >&2
        return 1
    fi
    if command -v shasum >/dev/null 2>&1; then
        shasum -a 256 "$path" | awk '{print $1}'
    elif command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$path" | awk '{print $1}'
    else
        echo "artifact_relocation: no sha256 tool found (tried shasum, sha256sum)" >&2
        return 1
    fi
}

source_hash_before=""
hash_rc=0
source_hash_before="$(hash_file "$WORKSPACE/target/debug/sample.o")" || hash_rc=$?
if [ "$hash_rc" -ne 0 ] || [ -z "$source_hash_before" ]; then
    echo "artifact_relocation: failed to hash the source artifact before relocation (rc=$hash_rc)" >&2
    exit 1
fi

plan_report="$(capture_ee "plan mode" artifact relocate \
    --from "$WORKSPACE/target/debug/sample.o" \
    --to "$DESTINATION" \
    --manifest "$PLAN_MANIFEST" \
    --json)"

if [ -e "$PLAN_MANIFEST" ]; then
    echo "plan mode wrote a manifest" >&2
    exit 1
fi

assert_json "plan mode report shape" "$plan_report" '
      .schema == "ee.response.v2"
      and .success == true
      and .data.schema == "ee.artifact.relocation.v1"
      and .data.mode == "plan"
      and .data.applied == false
      and .data.manifest.entries[0].status == "planned"
      and .data.preservationPolicy == "copy_preserve_no_delete_no_overwrite"
    '

audit_plan_report="$(capture_ee "audit artifact plan mode" artifact relocate \
    --from "$WORKSPACE/tests/audit_artifacts/evidence.json" \
    --to "$DESTINATION" \
    --manifest "$AUDIT_PLAN_MANIFEST" \
    --json)"

if [ -e "$AUDIT_PLAN_MANIFEST" ]; then
    echo "audit artifact plan mode wrote a manifest" >&2
    exit 1
fi

assert_json "audit artifact plan mode report shape" "$audit_plan_report" \
    --arg original "$WORKSPACE/tests/audit_artifacts/evidence.json" '
      .schema == "ee.response.v2"
      and .success == true
      and .data.mode == "plan"
      and .data.sourceAllowed == true
      and (.data.manifest.entries | length) == 1
      and .data.manifest.entries[0].originalPath == $original
    '

apply_report="$(capture_ee "apply mode" artifact relocate \
    --from "$WORKSPACE/target/debug/sample.o" \
    --to "$DESTINATION" \
    --manifest "$APPLY_MANIFEST" \
    --apply \
    --actor "p2-e2e" \
    --json)"

source_hash_after=""
hash_rc=0
source_hash_after="$(hash_file "$WORKSPACE/target/debug/sample.o")" || hash_rc=$?
if [ "$hash_rc" -ne 0 ] || [ -z "$source_hash_after" ]; then
    echo "artifact_relocation: failed to hash the source artifact after apply (rc=$hash_rc)" >&2
    exit 1
fi
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

assert_json "apply mode report shape" "$apply_report" \
    --arg original "$WORKSPACE/target/debug/sample.o" \
    --arg destination "$applied_destination" '
      .schema == "ee.response.v2"
      and .success == true
      and .data.mode == "apply"
      and .data.applied == true
      and .data.restored == false
      and .data.manifestHash != null
      and (.data.manifest.entries | length) == 1
      and .data.manifest.entries[0].originalPath == $original
      and .data.manifest.entries[0].destinationPath == $destination
      and (.data.manifest.entries[0].blake3 | startswith("blake3:"))
    '

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

restore_report="$(capture_ee "restore mode" artifact relocate --restore --manifest "$RESTORE_MANIFEST" --json)"

if [ "$(cat "$restore_original")" != "restored artifact bytes" ]; then
    echo "restore mode did not copy the preserved artifact back" >&2
    exit 1
fi

assert_json "restore mode report shape" "$restore_report" '
      .schema == "ee.response.v2"
      and .success == true
      and .data.mode == "restore"
      and .data.restored == true
      and .data.preservationPolicy == "copy_preserve_no_delete_no_overwrite"
    '

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
