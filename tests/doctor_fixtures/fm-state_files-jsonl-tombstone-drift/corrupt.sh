#!/usr/bin/env bash
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=tests/doctor_fixtures/lib.sh
. "$SCRIPT_DIR/../lib.sh"

# NOT-DETECTED, PINNED GAP (bd-2oh15 ruling c9986; FM-SF-02 declares
# fix_beads_jsonl_drift, but no doctor check reaches it): a real br tracker in
# the workspace deletes an issue, so the database holds its tombstone, and the
# export .beads/issues.jsonl is then replaced by the export taken BEFORE the
# delete. The export has lost the tombstone and carries the deleted issue as
# live, which an import would resurrect. The tombstone-bearing export is MOVED
# into the baseline; nothing is deleted.
FM="fm-state_files-jsonl-tombstone-drift"
target="$(doctor_fixture_target)"
ee_bin="${EE_DOCTOR_FIXTURE_BINARY:-ee}"
command -v "$ee_bin" >/dev/null
command -v br >/dev/null
command -v sqlite3 >/dev/null
export EE_EMBED_DOWNLOAD="${EE_EMBED_DOWNLOAD:-off}" RUST_LOG=error
doctor_fixture_prepare_target "$target"
doctor_fixture_healthy_store "$FM" "$target" "$ee_bin"
base="$target/.fixture_baseline"
beads="$target/.beads"

(cd "$target" && br init --json) > "$base/br-init.json"
(cd "$target" && br create "Kept issue for $FM" --actor doctor-fixture --json) > "$base/br-create-kept.json"
(cd "$target" && br create "Deleted issue for $FM" --actor doctor-fixture --json) > "$base/br-create-deleted.json"
jq -er '.id // .[0].id' "$base/br-create-deleted.json" > "$base/tombstoned-id"
gone="$(cat "$base/tombstoned-id")"
(cd "$target" && br sync --flush-only) > "$base/br-flush-1.out" 2>&1
cp -p "$beads/issues.jsonl" "$base/issues.before-delete.jsonl"
(cd "$target" && br delete "$gone" --actor doctor-fixture --json) > "$base/br-delete.json"
(cd "$target" && br sync --flush-only) > "$base/br-flush-2.out" 2>&1

# Negative control: after the flush the export carries the tombstone, the
# database agrees, and br's own doctor reports the tracker healthy.
jq -es --arg id "$gone" 'map(select(.id == $id)) | length == 1 and .[0].status == "tombstone"' \
    "$beads/issues.jsonl" >/dev/null
test "$(sqlite3 -readonly "$beads/beads.db" "SELECT status FROM issues WHERE id = '$gone';")" = tombstone
set +e
(cd "$target" && br doctor --json) > "$base/br-doctor-clean.json" 2>/dev/null
set -e
jq -e '.ok == true' "$base/br-doctor-clean.json" >/dev/null

mv "$beads/issues.jsonl" "$base/issues.with-tombstone.jsonl"
cp "$base/issues.before-delete.jsonl" "$beads/issues.jsonl"

if ! jq -es --arg id "$gone" 'map(select(.id == $id)) | length == 1 and .[0].status != "tombstone"' \
    "$beads/issues.jsonl" >/dev/null; then
    printf 'jsonl-tombstone fixture: the export still carries the tombstone for %s\n' "$gone" >&2
    exit 1
fi
set +e
(cd "$target" && br doctor --json) > "$base/br-doctor-drift.json" 2>/dev/null
set -e
if ! jq -e '.ok == false' "$base/br-doctor-drift.json" >/dev/null; then
    printf 'jsonl-tombstone fixture: br doctor did not see the drift; see %s\n' "$base/br-doctor-drift.json" >&2
    exit 1
fi
# The baseline is recorded last, after every br run has settled the tracker.
doctor_fixture_corrupt "$FM" "P1" "state_files"
printf 'real drift confirmed: %s is a tombstone in beads.db but live in issues.jsonl (br doctor degraded)\n' \
    "$gone" >&2
