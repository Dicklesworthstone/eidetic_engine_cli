#!/usr/bin/env bash
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=tests/doctor_fixtures/lib.sh
. "$SCRIPT_DIR/../lib.sh"

# REPAIR (bd-2oh15 survey c9983; measured on a stamped 02524267e build): a
# real indexed store whose workspace generation is advanced past the one the
# index was built from (workspace_generations.generation > meta.json
# generation), exactly what writes after the last rebuild leave behind. doctor
# reports search_index EE-E301 and --fix dispatches run_index_rebuild.
FM="fm-search_indexes-index_stale"
target="$(doctor_fixture_target)"
ee_bin="${EE_DOCTOR_FIXTURE_BINARY:-ee}"
command -v "$ee_bin" >/dev/null
command -v sqlite3 >/dev/null
export EE_EMBED_DOWNLOAD="${EE_EMBED_DOWNLOAD:-off}"
doctor_fixture_prepare_target "$target"
doctor_fixture_healthy_store "$FM" "$target" "$ee_bin"
base="$target/.fixture_baseline"

# Provision the persistent coordination lock through a healthy no-op run before
# the baseline digest, so undo is compared against a digest that includes it.
"$ee_bin" doctor --workspace "$target" --fix --json > "$base/doctor-initialize-lock.json"
jq -e '.schema == "ee.response.v2" and .success == true and .data.actionCount == 0' \
    "$base/doctor-initialize-lock.json" >/dev/null
test -f "$target/.ee/.doctor.lock"

jq -er '.generation' "$target/.ee/index/meta.json" > "$base/index-generation"
sqlite3 "$target/.ee/ee.db" "SELECT generation FROM workspace_generations;" > "$base/store-generation.before"
test "$(cat "$base/store-generation.before")" = "$(cat "$base/index-generation")"
sqlite3 "$target/.ee/ee.db" "UPDATE workspace_generations SET generation = generation + 1;"
sqlite3 "$target/.ee/ee.db" "SELECT generation FROM workspace_generations;" > "$base/store-generation.after"
test "$(cat "$base/store-generation.after")" -gt "$(cat "$base/index-generation")"

# Check the damage BEFORE recording the baseline: the first ee open after
# the sqlite3 edit settles the store's WAL, so a baseline taken earlier would
# not match the bytes undo restores (measured on 02524267e, bd-2oh15).
"$ee_bin" doctor --workspace "$target" --json > "$base/doctor-corrupt.json"
if ! jq -e '
    .schema == "ee.response.v2" and .success == true and .data.healthy == false and
    any(.data.actionable[]; .name == "search_index" and .errorCode == "EE-E301")
' "$base/doctor-corrupt.json" >/dev/null; then
    printf 'index_stale fixture did not produce search_index EE-E301; see %s\n' "$base/doctor-corrupt.json" >&2
    exit 1
fi
doctor_fixture_corrupt "$FM" "P1" "search_indexes"
printf 'real staleness confirmed: store generation %s > index generation %s (EE-E301)\n' \
    "$(cat "$base/store-generation.after")" "$(cat "$base/index-generation")" >&2
