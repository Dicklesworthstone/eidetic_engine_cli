#!/usr/bin/env bash
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=tests/doctor_fixtures/lib.sh
. "$SCRIPT_DIR/../lib.sh"
FM="fm-graph_subsystem-snapshot-write-lock-held"
if [ "${EE_DOCTOR_FIXTURE_RUN_EE:-0}" != "1" ]; then
    printf '%s assertion requires EE_DOCTOR_FIXTURE_RUN_EE=1; marker-only checks are insufficient\n' "$FM" >&2
    exit 2
fi
target="$(doctor_fixture_target)"
ee_bin="${EE_DOCTOR_FIXTURE_BINARY:-ee}"
base="$target/.fixture_baseline"
test -f "$(doctor_fixture_marker_dir "$target")/$FM.json"

# Independent witness: the real graph writer is blocked by the held lock.
set +e
"$ee_bin" graph centrality-refresh --workspace "$target" --json > "$base/refresh-assert.json" 2>&1
rc=$?
set -e
if [ "$rc" -ne 4 ] || ! grep -q 'held by ee-graph-snapshot-1-fixture' "$base/refresh-assert.json"; then
    printf 'fixture assert: %s witness lost: centrality-refresh exit %s\n' "$FM" "$rc" >&2
    exit 1
fi
doctor_fixture_assert_pinned_gap "$FM" "-"
# The lock row survives doctor and --fix: nothing released it.
test "$(sqlite3 "$target/.ee/ee.db" "SELECT count(*) FROM ee_advisory_locks WHERE holder_id = 'ee-graph-snapshot-1-fixture';")" = "1"
