#!/usr/bin/env bash
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=tests/doctor_fixtures/lib.sh
. "$SCRIPT_DIR/../lib.sh"

# condition.sh <command> [args...] (contract in lib.sh, doctor_fixture_under_condition).
# Shard fan-out is only visible when EE_SHARD_FANOUT_ENABLED and EE_SHARDS_DIR
# point doctor at the fixture's shard root, so the environment is part of the
# damage. Apply it and check that the shard it names is still there.
FM="fm-schema_migrations-shard-fanout-catalog-hash-mismatch"
target="$(doctor_fixture_target)"
base="$target/.fixture_baseline"
not_applied() {
    printf 'condition: %s not applied: %s\n' "$FM" "$1" >&2
    exit "$DOCTOR_FIXTURE_CONDITION_NOT_APPLIED"
}
[ -f "$base/env.sh" ] || not_applied "no $base/env.sh"
. "$base/env.sh"
[ "${EE_SHARD_FANOUT_ENABLED:-}" = 1 ] || not_applied "EE_SHARD_FANOUT_ENABLED is not 1"
[ -d "${EE_SHARDS_DIR:-}" ] || not_applied "EE_SHARDS_DIR is not a directory"
[ -f "$base/shard-path" ] && [ -f "$target/$(cat "$base/shard-path")" ] ||
    not_applied "the damaged shard is missing"
exec "$@"
