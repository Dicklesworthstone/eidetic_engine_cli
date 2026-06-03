#!/usr/bin/env bash
# P6 no-deletion disk-pressure e2e harness.
#
# This script is deliberately non-destructive. It creates a synthetic workspace
# under TMPDIR, exercises disk-pressure diagnostics, artifact retention,
# artifact relocation, fake Agent Mail archive discovery, and build-admission
# dry-runs. It verifies the synthetic files are preserved and does not delete
# the workspace.

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
DESTINATION="$SCRATCH_ROOT/$RUN_ID/external-artifacts"
MANIFEST_DIR="$SCRATCH_ROOT/$RUN_ID/manifests"
PLAN_MANIFEST="$MANIFEST_DIR/plan.json"
APPLY_MANIFEST="$MANIFEST_DIR/apply.json"
RESTORE_MANIFEST="$MANIFEST_DIR/restore.json"
FAKE_HOME="$WORKSPACE/home"
FAKE_CODEX_LOG_DIR="$FAKE_HOME/.codex/log"
FAKE_BIN="$WORKSPACE/bin"
ACTIVE_HARNESS_LOG="$FAKE_CODEX_LOG_DIR/active-open.log"
CLOSED_HARNESS_LOG="$FAKE_CODEX_LOG_DIR/closed-rotate.log"
EXTERNAL_HARNESS_LOG_ROOT="$DESTINATION/external-volume-log-case"
EXTERNAL_HARNESS_LOG="$EXTERNAL_HARNESS_LOG_ROOT/closed-external.log"
HARNESS_LOG_BYTES=1073741824
HARNESS_LOG_SENTINEL="EE_HARNESS_LOG_CONTENT_SHOULD_NOT_LEAK"

_e2e_emit_event "disk_pressure_e2e_start" \
    "workspace" "$WORKSPACE" \
    "scratch_root" "$SCRATCH_ROOT"

file_size_bytes() {
    stat -f '%z' "$1" 2>/dev/null || stat -c '%s' "$1"
}

make_sparse_harness_log() {
    local path="${1:?path required}"
    local label="${2:?label required}"
    printf '%s:%s\n' "$HARNESS_LOG_SENTINEL" "$label" > "$path"
    truncate -s "$HARNESS_LOG_BYTES" "$path"
}

harness_log_metadata_snapshot() {
    {
        printf '%s %s\n' "$(file_size_bytes "$ACTIVE_HARNESS_LOG")" "$ACTIVE_HARNESS_LOG"
        printf '%s %s\n' "$(file_size_bytes "$CLOSED_HARNESS_LOG")" "$CLOSED_HARNESS_LOG"
        printf '%s %s\n' "$(file_size_bytes "$EXTERNAL_HARNESS_LOG")" "$EXTERNAL_HARNESS_LOG"
    } | sort | shasum -a 256 | awk '{print $1}'
}

mkdir -p "$WORKSPACE/.ee" "$WORKSPACE/tests/audit_artifacts" "$WORKSPACE/target/debug" \
    "$WORKSPACE/target/ee-e2e/run-a" "$WORKSPACE/target/ee-golden-artifacts" \
    "$WORKSPACE/target/ee-bench" "$WORKSPACE/.ee/support-bundles" "$WORKSPACE/tmp" \
    "$WORKSPACE/target/restored" "$DESTINATION" "$MANIFEST_DIR" \
    "$DESTINATION/ee-relocated-artifacts/target/restored" \
    "$FAKE_HOME/.local/share/mcp_agent_mail/messages/2026/05" \
    "$FAKE_CODEX_LOG_DIR" "$FAKE_BIN" "$EXTERNAL_HARNESS_LOG_ROOT"
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
printf 'agent-mail-archive-message\n' \
    > "$FAKE_HOME/.local/share/mcp_agent_mail/messages/2026/05/message.md"
printf 'restored artifact bytes\n' \
    > "$DESTINATION/ee-relocated-artifacts/target/restored/missing.o"
make_sparse_harness_log "$ACTIVE_HARNESS_LOG" "active-open"
make_sparse_harness_log "$CLOSED_HARNESS_LOG" "closed-rotate"
make_sparse_harness_log "$EXTERNAL_HARNESS_LOG" "external-volume"

cat > "$FAKE_BIN/lsof" <<'EOF'
#!/usr/bin/env bash
target="${!#}"
case "$target" in
    *active-open.log)
        printf 'p4242\ncCodex\n'
        exit 0
        ;;
    *closed-rotate.log|*closed-external.log)
        exit 1
        ;;
    *)
        exit 1
        ;;
esac
EOF
chmod +x "$FAKE_BIN/lsof"

_e2e_emit_event "synthetic_tree_created" \
    "workspace" "$WORKSPACE" \
    "destination" "$DESTINATION" \
    "fake_home" "$FAKE_HOME"
_e2e_emit_event "harness_log_fixture_created" \
    "active_log" "$ACTIVE_HARNESS_LOG" \
    "closed_log" "$CLOSED_HARNESS_LOG" \
    "external_volume_log" "$EXTERNAL_HARNESS_LOG" \
    "fake_lsof" "$FAKE_BIN/lsof" \
    "bytes_per_log" "$HARNESS_LOG_BYTES"

snapshot() {
    find "$WORKSPACE" "$DESTINATION" -type f -print0 |
        while IFS= read -r -d '' path; do
            case "$path" in
                "$FAKE_CODEX_LOG_DIR"/*|"$EXTERNAL_HARNESS_LOG_ROOT"/*) continue ;;
            esac
            printf '%s\0' "$path"
        done |
        sort -z |
        xargs -0 shasum -a 256
}

assert_snapshot_preserved() {
    local snapshot_text="${1:?snapshot text required}"
    local line expected_hash path actual_hash
    while IFS= read -r line; do
        [ -n "$line" ] || continue
        expected_hash="${line%% *}"
        path="${line#*  }"
        if [ ! -f "$path" ]; then
            echo "disk_pressure: synthetic file disappeared: $path" >&2
            exit 1
        fi
        actual_hash="$(shasum -a 256 "$path" | awk '{print $1}')"
        if [ "$actual_hash" != "$expected_hash" ]; then
            echo "disk_pressure: synthetic file changed: $path" >&2
            exit 1
        fi
    done <<< "$snapshot_text"
}

before_snapshot="$(snapshot)"
harness_logs_before="$(harness_log_metadata_snapshot)"
source_hash_before="$(shasum -a 256 "$WORKSPACE/target/debug/sample.o")"

if [ -n "${EE_BIN:-}" ]; then
    EE_BINARY="$EE_BIN"
fi
if [ ! -x "$EE_BINARY" ]; then
    echo "disk_pressure: ee binary not executable at $EE_BINARY" >&2
    echo "    pass EE_BINARY/EE_BIN for an existing ee binary; this no-build harness will not run cargo" >&2
    exit 2
fi

IMPOSSIBLE_MIN_FREE_BYTES="18446744073709551615"
report="$(PATH="$FAKE_BIN:$PATH" HOME="$FAKE_HOME" "$EE_BINARY" --workspace "$WORKSPACE" diag disk-pressure --json \
    --top-limit 3 --consumer-depth 1 --consumer-entry-limit 100)"
artifacts_report="$(CARGO_TARGET_DIR="$WORKSPACE/target" \
    TMPDIR="$WORKSPACE/tmp" \
    HOME="$FAKE_HOME" \
    EE_TEST_LOG_PATH="$WORKSPACE/tmp/j1.jsonl" \
    EE_E2E_RETENTION_MANIFEST="$WORKSPACE/tmp/e2e_retention_manifest.json" \
    "$EE_BINARY" --workspace "$WORKSPACE" diag artifacts --json \
    --top-limit 3 --consumer-depth 1 --consumer-entry-limit 100)"
build_admission_report="$(CARGO_TARGET_DIR="$WORKSPACE/target" \
    TMPDIR="$WORKSPACE/tmp" \
    HOME="$FAKE_HOME" \
    "$EE_BINARY" --workspace "$WORKSPACE" diag build-admission --json \
    --min-free-bytes "$IMPOSSIBLE_MIN_FREE_BYTES" \
    --artifact-destination "$WORKSPACE/target/ee-e2e/sync-down")"
plan_report="$(HOME="$FAKE_HOME" "$EE_BINARY" --workspace "$WORKSPACE" artifact relocate \
    --from "$WORKSPACE/target/debug/sample.o" \
    --to "$DESTINATION" \
    --manifest "$PLAN_MANIFEST" \
    --json)"
apply_report="$(HOME="$FAKE_HOME" "$EE_BINARY" --workspace "$WORKSPACE" artifact relocate \
    --from "$WORKSPACE/target/debug/sample.o" \
    --to "$DESTINATION" \
    --manifest "$APPLY_MANIFEST" \
    --apply \
    --actor "p6-e2e" \
    --json)"

restore_original="$WORKSPACE/target/restored/missing.o"
restore_destination="$DESTINATION/ee-relocated-artifacts/target/restored/missing.o"
restore_size_bytes="$(wc -c < "$restore_destination" | tr -d ' ')"
jq -n \
    --arg schema "ee.artifact.relocation.v1" \
    --arg version "p6-e2e" \
    --arg actor "p6-e2e" \
    --arg created_at "2026-05-21T00:00:00Z" \
    --arg workspace "$WORKSPACE" \
    --arg source "$WORKSPACE/target/restored" \
    --arg destination_root "$DESTINATION" \
    --arg restore_command "ee artifact relocate --restore --manifest $RESTORE_MANIFEST --json" \
    --arg original "$restore_original" \
    --arg destination "$restore_destination" \
    --argjson size_bytes "$restore_size_bytes" \
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
        sizeBytes: $size_bytes,
        mtimeUnixSeconds: null,
        blake3: null,
        status: "planned"
      }]
    }' > "$RESTORE_MANIFEST"
restore_report="$(HOME="$FAKE_HOME" "$EE_BINARY" --workspace "$WORKSPACE" artifact relocate \
    --restore \
    --manifest "$RESTORE_MANIFEST" \
    --json)"

assert_snapshot_preserved "$before_snapshot"
harness_logs_after="$(harness_log_metadata_snapshot)"
source_hash_after="$(shasum -a 256 "$WORKSPACE/target/debug/sample.o")"

e2e_log_assert_eq "$source_hash_after" "$source_hash_before" \
    "disk_pressure_relocation_preserves_original"
e2e_log_assert_eq "$harness_logs_after" "$harness_logs_before" \
    "agent_harness_log_metadata_preserved_no_delete"

assert_jq "$report" '.schema' "ee.response.v2" "disk_pressure_response_schema"
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
assert_jq "$report" '(.data.roots | any(.label == "agent_mail_archive"
    and .role == "agent_mail_archive_root"
    and .exists == true
    and (.path | startswith("'"$FAKE_HOME"'"))))' \
    "true" "disk_pressure_fake_agent_mail_archive_root"
# shellcheck disable=SC2016
assert_jq "$report" '(.data.recoveryActions | all(.kind as $kind |
    ["move_preserve", "compress_preserve", "preserve_tail_copy", "rotate_with_manifest", "ask_human", "noop"]
    | index($kind) != null))' "true" "disk_pressure_recovery_actions_preserve_only"
assert_jq "$report" '(.data.agentHarnessLogs | length >= 2)' \
    "true" "agent_harness_logs_detected"
assert_jq "$report" \
    '(.data.agentHarnessLogs | any(.entry.path | endswith("active-open.log")))' \
    "true" "agent_harness_active_log_detected"
assert_jq "$report" \
    '(.data.agentHarnessLogs | any(.entry.path | endswith("closed-rotate.log")))' \
    "true" "agent_harness_closed_log_detected"
assert_jq "$report" \
    '(.data.agentHarnessLogs | any((.entry.path | endswith("active-open.log"))
        and .entry.activity == "active_open"
        and .entry.owningProcessSummary == "pid=4242 command=Codex"
        and .repairKind == "preserve_tail_copy"
        and .mutationPolicy == "preservation_only"
        and .sideEffectFree == true))' \
    "true" "active_log_plan_checked"
assert_jq "$report" \
    '(.data.agentHarnessLogs | any((.entry.path | endswith("closed-rotate.log"))
        and .entry.activity == "closed"
        and .repairKind == "rotate_with_manifest"
        and .mutationPolicy == "preservation_only"
        and .sideEffectFree == true))' \
    "true" "closed_log_rotation_plan_checked"
assert_jq "$report" \
    '(.data.agentHarnessLogs | all((.reason + " " + .suggestion) |
        contains("EE_HARNESS_LOG_CONTENT_SHOULD_NOT_LEAK") | not))' \
    "true" "agent_harness_log_contents_not_leaked"
assert_jq "$report" \
    '(.data.recoveryActions | any(.target == "agent_harness_log"
        and .kind == "preserve_tail_copy"))' \
    "true" "agent_harness_preserve_tail_copy_recovery"
assert_jq "$report" \
    '(.data.recoveryActions | any(.target == "agent_harness_log"
        and .kind == "rotate_with_manifest"))' \
    "true" "agent_harness_rotate_with_manifest_recovery"
assert_jq "$report" \
    '(.data.guidance | any(.code == "agent_harness_log_pressure"))' \
    "true" "agent_harness_pressure_guidance"

_e2e_emit_event "active_log_plan_checked" \
    "active_log" "$ACTIVE_HARNESS_LOG" \
    "metadata_hash" "$harness_logs_after" \
    "repair_kind" "preserve_tail_copy"
_e2e_emit_event "no_delete_policy_checked" \
    "active_log" "$ACTIVE_HARNESS_LOG" \
    "closed_log" "$CLOSED_HARNESS_LOG" \
    "external_volume_log" "$EXTERNAL_HARNESS_LOG" \
    "policy" "metadata_preserved_no_delete"

_e2e_emit_event "repair_plan_checked" \
    "workspace" "$WORKSPACE" \
    "posture" "$(printf '%s\n' "$report" | jq -r '.data.posture')" \
    "recovery_action_count" "$(printf '%s\n' "$report" | jq -r '.data.recoveryActions | length')"

assert_jq "$artifacts_report" '.schema' "ee.response.v2" "artifact_retention_response_schema"
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

assert_jq "$build_admission_report" '.schema' "ee.response.v2" \
    "build_admission_response_schema"
assert_jq "$build_admission_report" '.success' "true" \
    "build_admission_response_success"
assert_jq "$build_admission_report" '.data.schema' "ee.build_admission.diagnostics.v1" \
    "build_admission_data_schema"
assert_jq "$build_admission_report" '.data.sideEffectFree' "true" \
    "build_admission_side_effect_free"
assert_jq "$build_admission_report" '.data.mutationPolicy' \
    "read_only_report_no_files_modified" \
    "build_admission_mutation_policy"
assert_jq "$build_admission_report" '.data.admitted' "false" \
    "build_admission_denied"
assert_jq "$build_admission_report" \
    '(.data.degraded | map(.code) | index("build_admission_denied") != null)' \
    "true" "build_admission_denied_degradation"
assert_jq "$build_admission_report" \
    '(.data.recoveryActions | any(.target == "build_admission"
        and .kind == "ask_human"))' \
    "true" "build_admission_ask_human_recovery"
assert_jq "$build_admission_report" \
    '(.data.checks | map(.label) |
        index("workspace") != null
        and index("cargo_target") != null
        and index("tmpdir") != null
        and index("artifact_destination") != null)' \
    "true" "build_admission_expected_checks"

if [ -e "$PLAN_MANIFEST" ]; then
    echo "disk_pressure: artifact relocation plan mode wrote a manifest" >&2
    exit 1
fi
applied_destination="$DESTINATION/ee-relocated-artifacts/target/debug/sample.o"
if [ ! -f "$applied_destination" ]; then
    echo "disk_pressure: artifact relocation apply did not copy artifact" >&2
    exit 1
fi
if [ ! -f "$APPLY_MANIFEST" ]; then
    echo "disk_pressure: artifact relocation apply did not write manifest" >&2
    exit 1
fi
if [ "$(cat "$restore_original")" != "restored artifact bytes" ]; then
    echo "disk_pressure: artifact relocation restore did not copy preserved artifact back" >&2
    exit 1
fi
assert_jq "$plan_report" '.schema' "ee.response.v2" "relocation_plan_response_schema"
assert_jq "$plan_report" '.success' "true" "relocation_plan_response_success"
assert_jq "$plan_report" '.data.mode' "plan" "relocation_plan_mode"
assert_jq "$plan_report" '.data.applied' "false" "relocation_plan_not_applied"
assert_jq "$plan_report" '.data.preservationPolicy' \
    "copy_preserve_no_delete_no_overwrite" "relocation_plan_preservation_policy"
assert_jq "$apply_report" '.schema' "ee.response.v2" "relocation_apply_response_schema"
assert_jq "$apply_report" '.success' "true" "relocation_apply_response_success"
assert_jq "$apply_report" '.data.mode' "apply" "relocation_apply_mode"
assert_jq "$apply_report" '.data.applied' "true" "relocation_apply_applied"
assert_jq "$apply_report" '.data.restored' "false" "relocation_apply_not_restored"
assert_jq "$apply_report" '.data.manifestHash != null' \
    "true" "relocation_apply_manifest_hash"
assert_jq "$apply_report" '.data.preservationPolicy' \
    "copy_preserve_no_delete_no_overwrite" "relocation_apply_preservation_policy"
assert_jq "$restore_report" '.schema' "ee.response.v2" "relocation_restore_response_schema"
assert_jq "$restore_report" '.success' "true" "relocation_restore_response_success"
assert_jq "$restore_report" '.data.mode' "restore" "relocation_restore_mode"
assert_jq "$restore_report" '.data.restored' "true" "relocation_restore_restored"
assert_jq "$restore_report" '.data.preservationPolicy' \
    "copy_preserve_no_delete_no_overwrite" "relocation_restore_preservation_policy"

_e2e_emit_event "relocation_manifest_checked" \
    "plan_manifest" "$PLAN_MANIFEST" \
    "apply_manifest" "$APPLY_MANIFEST" \
    "restore_manifest" "$RESTORE_MANIFEST" \
    "applied_destination" "$applied_destination"

_e2e_emit_event "disk_pressure_e2e_summary" \
    "workspace" "$WORKSPACE" \
    "destination" "$DESTINATION" \
    "mutation" "copy_preserve_only" \
    "asserts_pass" "$EE_TEST_LOG_ASSERTS_PASS" \
    "asserts_fail" "$EE_TEST_LOG_ASSERTS_FAIL"

jq -n \
    --arg schema "ee.disk_pressure.e2e.v1" \
    --arg workspace "$WORKSPACE" \
    --arg destination "$DESTINATION" \
    --arg apply_manifest "$APPLY_MANIFEST" \
    --arg restore_manifest "$RESTORE_MANIFEST" \
    --arg posture "$(printf '%s\n' "$report" | jq -r '.data.posture')" \
    --arg artifact_roots "$(printf '%s\n' "$artifacts_report" | jq -r '.data.summary.rootCount')" \
    --arg build_admitted "$(printf '%s\n' "$build_admission_report" | jq -r '.data.admitted')" \
    --arg mutation "copy_preserve_only" \
    '{
      schema: $schema,
      success: true,
      workspace: $workspace,
      destination: $destination,
      applyManifest: $apply_manifest,
      restoreManifest: $restore_manifest,
      posture: $posture,
      artifactRoots: ($artifact_roots | tonumber),
      buildAdmissionAdmitted: ($build_admitted == "true"),
      mutation: $mutation,
      note: "Synthetic workspace intentionally left in place for audit."
    }'
