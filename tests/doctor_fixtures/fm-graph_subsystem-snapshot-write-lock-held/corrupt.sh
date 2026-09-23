#!/usr/bin/env bash
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=tests/doctor_fixtures/lib.sh
. "$SCRIPT_DIR/../lib.sh"

# NOT-DETECTED, PINNED GAP (bd-2oh15 c9853): the graph snapshot write lock is a
# row in ee_advisory_locks. A holder that is not PID-reclaimable, expiring in
# 2099, keeps it held. `ee graph centrality-refresh` then fails with exit 4
# (graph_snapshot_lock_held); doctor never reads the lock table. A byte copy of
# the pre-lock database is kept in the baseline.
FM="fm-graph_subsystem-snapshot-write-lock-held"
target="$(doctor_fixture_target)"
ee_bin="${EE_DOCTOR_FIXTURE_BINARY:-ee}"
command -v "$ee_bin" >/dev/null
command -v sqlite3 >/dev/null
export EE_EMBED_DOWNLOAD="${EE_EMBED_DOWNLOAD:-off}"
doctor_fixture_prepare_target "$target"
doctor_fixture_healthy_store "$FM" "$target" "$ee_bin"
base="$target/.fixture_baseline"

cp -p "$target/.ee/ee.db" "$base/ee.db.pre-lock"
wsp="$(sqlite3 "$target/.ee/ee.db" "SELECT id FROM workspaces LIMIT 1;")"
test -n "$wsp"
sqlite3 "$target/.ee/ee.db" "INSERT INTO ee_advisory_locks (resource_key, resource_type, resource_id, holder_id, acquired_at, expires_at, reason) VALUES ('graph_snapshot:$wsp:memory_links', 'graph_snapshot', '$wsp:memory_links', 'ee-graph-snapshot-1-fixture', '2026-09-23T00:00:00Z', '2099-01-01T00:00:00Z', 'bd-2oh15 doctor fixture');"

doctor_fixture_corrupt "$FM" "P0" "graph_subsystem"
set +e
"$ee_bin" graph centrality-refresh --workspace "$target" --json > "$base/refresh-corrupt.json" 2>&1
rc=$?
set -e
if [ "$rc" -ne 4 ] || ! grep -q 'held by ee-graph-snapshot-1-fixture' "$base/refresh-corrupt.json"; then
    printf 'graph-lock fixture: centrality-refresh did not fail on the held lock (exit %s)\n' "$rc" >&2
    exit 1
fi
printf 'real lock confirmed: graph snapshot write lock held (centrality-refresh exit 4)\n' >&2
