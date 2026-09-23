#!/usr/bin/env bash
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=tests/doctor_fixtures/lib.sh
. "$SCRIPT_DIR/../lib.sh"

# REPORT-ONLY, FULL MODE ONLY (bd-2oh15 c9850): the failure is environmental --
# cass is not resolvable on PATH. This records a PATH with every directory that
# holds a `cass` executable removed; assert.sh runs doctor under it. doctor
# --full reports cass EE-E506; concise doctor shows nothing; EE-E506 has no
# dispatch.
FM="fm-cass_integration-cass_not_found"
target="$(doctor_fixture_target)"
ee_bin="$(command -v "${EE_DOCTOR_FIXTURE_BINARY:-ee}")"
export EE_EMBED_DOWNLOAD="${EE_EMBED_DOWNLOAD:-off}"
doctor_fixture_prepare_target "$target"
doctor_fixture_healthy_store "$FM" "$target" "$ee_bin"
base="$target/.fixture_baseline"

path_without_cass=""
IFS=':' read -r -a entries <<< "$PATH"
for dir in "${entries[@]}"; do
    [ -n "$dir" ] || continue
    [ -x "$dir/cass" ] && continue
    path_without_cass="${path_without_cass:+$path_without_cass:}$dir"
done
printf '%s\n' "$path_without_cass" > "$base/path-without-cass"
for tool in jq shasum; do
    if ! PATH="$path_without_cass" command -v "$tool" >/dev/null; then
        printf 'cass_not_found fixture cannot isolate cass: %s shares a directory with it\n' "$tool" >&2
        exit 2
    fi
done

doctor_fixture_corrupt "$FM" "P0" "cass_integration"
PATH="$path_without_cass" "$ee_bin" doctor --workspace "$target" --full --json > "$base/doctor-corrupt.json"
if ! jq -e 'any(.. | objects | select(has("name") and has("errorCode")); .name == "cass" and .errorCode == "EE-E506")' \
    "$base/doctor-corrupt.json" >/dev/null; then
    printf 'cass_not_found fixture: doctor --full did not report cass EE-E506; see %s\n' "$base/doctor-corrupt.json" >&2
    exit 1
fi
printf 'real environment confirmed: cass absent from PATH (doctor --full: cass EE-E506)\n' >&2
