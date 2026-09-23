#!/usr/bin/env bash
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=tests/doctor_fixtures/lib.sh
. "$SCRIPT_DIR/../lib.sh"
FM="fm-schema_migrations-shard-fanout-catalog-hash-mismatch"
if [ "${EE_DOCTOR_FIXTURE_RUN_EE:-0}" != "1" ]; then
    printf '%s assertion requires EE_DOCTOR_FIXTURE_RUN_EE=1; marker-only checks are insufficient\n' "$FM" >&2
    exit 2
fi
target="$(doctor_fixture_target)"
ee_bin="${EE_DOCTOR_FIXTURE_BINARY:-ee}"
base="$target/.fixture_baseline"
test -f "$(doctor_fixture_marker_dir "$target")/$FM.json"
# shellcheck disable=SC1091
. "$base/env.sh"
shard="$target/$(cat "$base/shard-path")"

# Independent witness: the shard no longer matches what the migration wrote,
# while the catalog row that records its hashes is unchanged.
test "$(doctor_fixture_sha256 "$shard")" != "$(doctor_fixture_sha256 "$base/shard.migrated.db")"
sqlite3 "$base/shard-root/catalog.db" \
    "SELECT shard_id, source_database_hash, target_database_hash, last_verified_hashes FROM shard_fanout_catalog;" \
    > "$base/catalog-row.assert"
cmp "$base/catalog-row.before" "$base/catalog-row.assert"
# Pinned: the shard_fanout check stays ok in the full report.
"$ee_bin" doctor --workspace "$target" --full --json > "$base/doctor-full-assert.json"
jq -e 'any(.. | objects | select(.name? == "shard_fanout"); .severity == "ok")' "$base/doctor-full-assert.json" >/dev/null
doctor_fixture_assert_pinned_gap "$FM" "$(cat "$base/shard-path")"
