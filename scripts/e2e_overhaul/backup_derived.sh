#!/usr/bin/env bash
# L6 - Backup --include-derived e2e coverage.
#
# This harness is intentionally no-build. It requires an existing ee binary via
# scripts/e2e_overhaul/lib/shared.sh and retains its temp workspace by default.

set -u -o pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/e2e_overhaul/lib/shared.sh
source "$SCRIPT_DIR/lib/shared.sh"

require_jq
START_SECONDS="$(python3 -c 'import time; print(time.monotonic())')"
epic_setup "backup_derived"

backup_manifest_digest() {
    local manifest_path="${1:?manifest path required}"
    jq -S '
      del(.backupId, .createdAt, .label)
      | .artifacts = ((.artifacts // []) | map(del(.hash)))
      | .derived = ((.derived // []) | map(del(.hash, .captured_at)) | sort_by(.kind, .path))
    ' "$manifest_path"
}

seed_backup_derived_fixture() {
    local index_dir lab_dir
    index_dir="$EPIC_WORKSPACE/.ee/indexes/combined"
    lab_dir="$EPIC_WORKSPACE/.ee/lab/episodes"
    mkdir -p "$index_dir" "$lab_dir"

    ee_workspace remember \
        "Backup derived e2e memory proves source records survive alongside derived assets." \
        --level procedural \
        --kind rule \
        --confidence 0.93 \
        --json >/dev/null

    printf '%s\n' \
        '{"schema":"ee.index.manifest.v1","documents":1,"fixture":"backup_derived"}' \
        > "$index_dir/manifest.json"
    printf '%s\n' \
        '{"schema":"ee.lab.frozen_episode.v1","episode_id":"ep_backup_derived","fixture":"backup_derived"}' \
        > "$lab_dir/ep_backup_derived.json"
}

create_backup_with_derived() {
    local label="${1:?label required}"
    ee_workspace backup create \
        --output-dir "$BACKUP_ROOT" \
        --label "$label" \
        --include-derived \
        --include-graph-cache \
        --json
}

seed_backup_derived_fixture

BACKUP_ROOT="$EPIC_WORKSPACE/backups"
CREATE_JSON="$(create_backup_with_derived "derived-e2e-a")"
assert_jq "$CREATE_JSON" '.schema' "ee.response.v2" "backup_derived_create_response_schema"
assert_jq "$CREATE_JSON" '.success' "true" "backup_derived_create_success"
assert_jq "$CREATE_JSON" '.data.schema' "ee.backup.create.v1" "backup_derived_create_schema"
assert_jq "$CREATE_JSON" '.data.includeDerived' "true" "backup_derived_create_include_derived"
assert_jq "$CREATE_JSON" '(.data.derived | map(.kind) | index("index_manifest") != null)' \
    "true" "backup_derived_index_manifest_included"
assert_jq "$CREATE_JSON" '(.data.derived | map(.kind) | index("lab_episode") != null)' \
    "true" "backup_derived_lab_episode_included"
assert_jq "$CREATE_JSON" '(.data.derived | map(.kind) | index("wal_holds") != null)' \
    "true" "backup_derived_wal_holds_included"

BACKUP_ID="$(printf '%s' "$CREATE_JSON" | jq -r '.data.backupId')"
BACKUP_PATH="$(printf '%s' "$CREATE_JSON" | jq -r '.data.outputPath')"
MANIFEST_PATH="$(printf '%s' "$CREATE_JSON" | jq -r '.data.manifestPath')"
e2e_log_note "backup_create_derived_included backup_id=${BACKUP_ID} path=${BACKUP_PATH}"
e2e_log_artifact_manifest "backup_create_derived" "$EE_BINARY" \
    backup create --include-derived --include-graph-cache --workspace "$EPIC_WORKSPACE"

VERIFY_JSON="$(ee_workspace backup verify "$BACKUP_ID" --output-dir "$BACKUP_ROOT" --json)"
assert_jq "$VERIFY_JSON" '.data.status' "verified" "backup_derived_verify_status"
assert_jq "$VERIFY_JSON" '(.data.checkedDerived | map(.kind) | index("lab_episode") != null)' \
    "true" "backup_derived_verify_lab_episode_checked"
e2e_log_note "backup_inspect_derived_summary backup_id=${BACKUP_ID}"

INSPECT_JSON="$(ee_workspace backup inspect "$BACKUP_ID" --output-dir "$BACKUP_ROOT" --json)"
assert_jq "$INSPECT_JSON" '(.data.derived | length) >= 3' "true" \
    "backup_derived_inspect_reports_assets"

RESTORE_SIDE_PATH="$EPIC_WORKSPACE.restore"
RESTORE_JSON="$(ee_workspace backup restore "$BACKUP_ID" \
    --output-dir "$BACKUP_ROOT" \
    --side-path "$RESTORE_SIDE_PATH" \
    --json)"
assert_jq "$RESTORE_JSON" '.data.status' "completed" "backup_derived_restore_status"
assert_jq "$RESTORE_JSON" '(.data.restoredDerived | map(.kind) | index("lab_episode") != null)' \
    "true" "backup_derived_restore_lab_episode_reported"
assert_jq "$RESTORE_JSON" \
    '(.data.restoredDerived[] | select(.kind == "lab_episode") | .labEpisodePath | length > 0)' \
    "true" "backup_derived_restore_lab_episode_materialized"
e2e_log_note "backup_restore_derived_validation backup_id=${BACKUP_ID}"

CORRUPT_CREATE_JSON="$(create_backup_with_derived "derived-e2e-corrupt")"
CORRUPT_BACKUP_ID="$(printf '%s' "$CORRUPT_CREATE_JSON" | jq -r '.data.backupId')"
CORRUPT_BACKUP_PATH="$(printf '%s' "$CORRUPT_CREATE_JSON" | jq -r '.data.outputPath')"
printf '%s\n' '{"schema":"tampered"}' > "$CORRUPT_BACKUP_PATH/derived/wal_holds.json"
CORRUPT_VERIFY_JSON="$(ee_workspace backup verify "$CORRUPT_BACKUP_ID" \
    --output-dir "$BACKUP_ROOT" \
    --json)"
assert_jq "$CORRUPT_VERIFY_JSON" '.data.status' "failed" \
    "backup_derived_corrupt_verify_fails"
assert_jq "$CORRUPT_VERIFY_JSON" \
    '(.data.issues | map(.code) | index("derived_asset_corrupt") != null)' \
    "true" "backup_derived_corrupt_code"
e2e_log_note "backup_derived_corrupt backup_id=${CORRUPT_BACKUP_ID}"

SECOND_CREATE_JSON="$(create_backup_with_derived "derived-e2e-b")"
SECOND_MANIFEST_PATH="$(printf '%s' "$SECOND_CREATE_JSON" | jq -r '.data.manifestPath')"
FIRST_DIGEST="$(backup_manifest_digest "$MANIFEST_PATH")"
SECOND_DIGEST="$(backup_manifest_digest "$SECOND_MANIFEST_PATH")"
e2e_log_assert_eq "$SECOND_DIGEST" "$FIRST_DIGEST" "backup_derived_manifest_deterministic_normalized"

ELAPSED_MS="$(python3 -c "import time; print(int((time.monotonic() - float('$START_SECONDS')) * 1000))")"
e2e_log_note "backup_derived_summary passed=${EE_TEST_LOG_ASSERTS_PASS} failed=${EE_TEST_LOG_ASSERTS_FAIL} elapsed_ms=${ELAPSED_MS}"

if [ "${EE_TEST_LOG_ASSERTS_FAIL:-0}" -gt 0 ]; then
    exit 1
fi
