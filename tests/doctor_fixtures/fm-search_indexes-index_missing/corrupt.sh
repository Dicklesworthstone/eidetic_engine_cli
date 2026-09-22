#!/usr/bin/env bash
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=tests/doctor_fixtures/lib.sh
. "$SCRIPT_DIR/../lib.sh"

# A missing derived index is meaningful only after a real store and healthy
# index existed. Refuse reuse rather than overwriting somebody else's state.
target="$(doctor_fixture_target)"
ee_bin="${EE_DOCTOR_FIXTURE_BINARY:-ee}"
if [ -L "$target" ]; then
    printf 'index_missing fixture refuses a symlink target: %s\n' "$target" >&2
    exit 2
fi
mkdir -p "$target"
existing="$(find "$target" -mindepth 1 -maxdepth 1 -print -quit)"
if [ -n "$existing" ]; then
    printf 'index_missing fixture requires an empty target: %s\n' "$target" >&2
    exit 2
fi
command -v "$ee_bin" >/dev/null
mkdir -p "$target/.fixture_baseline"
"$ee_bin" --workspace "$target" init --skip-boilerplate --json \
    > "$target/.fixture_baseline/init.json"
"$ee_bin" doctor --workspace "$target" --json \
    > "$target/.fixture_baseline/doctor-healthy.json"
doctor_fixture_assert_health_report "fm-search_indexes-index_missing" \
    "$target/.fixture_baseline/doctor-healthy.json"
test -f "$target/.ee/ee.db"
test -f "$target/.ee/index/meta.json"
mv "$target/.ee/index" "$target/.fixture_baseline/healthy-index"

# Capture the actual corrupted pre-fix state, preserving the displaced index
# as evidence. The marker is bookkeeping, never the corruption itself.
doctor_fixture_corrupt "fm-search_indexes-index_missing" "P1" "search_indexes"
"$ee_bin" doctor --workspace "$target" --json \
    > "$target/.fixture_baseline/doctor-corrupt.json"
if ! jq -e '
    .schema == "ee.response.v2" and .success == true and
    .data.healthy == false and .data.posture == "degraded_recoverable" and
    any(.data.actionable[]; .name == "search_index" and .tier == "core" and
        .severity == "warning" and .errorCode == "EE-E300") and
    all(.data.coreChecks[]; .name == "search_index" or .severity == "ok")
' "$target/.fixture_baseline/doctor-corrupt.json" >/dev/null; then
    printf 'index_missing fixture did not isolate EE-E300; inspect %s\n' \
        "$target/.fixture_baseline/doctor-corrupt.json" >&2
    exit 1
fi
printf 'real corruption confirmed: index_missing (EE-E300); source database preserved\n' >&2
