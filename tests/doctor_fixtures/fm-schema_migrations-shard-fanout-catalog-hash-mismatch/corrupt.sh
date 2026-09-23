#!/usr/bin/env bash
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=tests/doctor_fixtures/lib.sh
. "$SCRIPT_DIR/../lib.sh"

# NOT-DETECTED, PINNED GAP (bd-2oh15 c9863): a REAL shard fan-out migration
# writes catalog.db and the workspace shard; one memory row in the shard is then
# changed. The catalog's recorded hashes no longer describe the shard, but no
# detector computes or compares them (last_verified_hashes is always None), so
# doctor keeps reporting shard fan-out ok. A byte copy of the migrated shard is
# kept in the baseline.
FM="fm-schema_migrations-shard-fanout-catalog-hash-mismatch"
target="$(doctor_fixture_target)"
ee_bin="${EE_DOCTOR_FIXTURE_BINARY:-ee}"
command -v "$ee_bin" >/dev/null
command -v sqlite3 >/dev/null
export EE_EMBED_DOWNLOAD="${EE_EMBED_DOWNLOAD:-off}"
doctor_fixture_prepare_target "$target"
doctor_fixture_healthy_store "$FM" "$target" "$ee_bin"
base="$target/.fixture_baseline"

mkdir -p "$base/shard-root/shards"
printf 'export EE_SHARD_FANOUT_ENABLED=1\nexport EE_SHARDS_DIR=%q\n' "$base/shard-root/shards" > "$base/env.sh"
# shellcheck disable=SC1091
. "$base/env.sh"
# Pass the shard root explicitly: measured at ab83e3f23, `migrate shard-fanout`
# wrote to the XDG default instead of the exported EE_SHARDS_DIR. doctor reads
# EE_SHARDS_DIR, so both must name the same root.
"$ee_bin" migrate shard-fanout --workspace "$target" --shards-dir "$EE_SHARDS_DIR" --json > "$base/shard-migrate.json"
jq -e --arg root "$EE_SHARDS_DIR" '
    .schema == "ee.response.v2" and .success == true and
    .data.apply.outcome == "applied" and .data.apply.shardRoot == $root
' "$base/shard-migrate.json" >/dev/null
catalog="$base/shard-root/catalog.db"
shard="$(find "$EE_SHARDS_DIR" -maxdepth 1 -type f -name '*.db' | head -n 1)"
test -f "$catalog"
test -n "$shard"
printf '%s\n' "${shard#"$target/"}" > "$base/shard-path"
sqlite3 "$catalog" "SELECT shard_id, source_database_hash, target_database_hash, last_verified_hashes FROM shard_fanout_catalog;" \
    > "$base/catalog-row.before"
cp -p "$shard" "$base/shard.migrated.db"
sqlite3 "$shard" "UPDATE memories SET content = content || ' [tampered after migration]' WHERE rowid = (SELECT MIN(rowid) FROM memories);"

doctor_fixture_corrupt "$FM" "P0" "schema_migrations"
if [ "$(doctor_fixture_sha256 "$shard")" = "$(doctor_fixture_sha256 "$base/shard.migrated.db")" ]; then
    printf 'shard fixture: the shard bytes did not change after the tamper\n' >&2
    exit 1
fi
printf 'real mismatch confirmed: shard changed after migration; catalog row unchanged\n' >&2
