#!/usr/bin/env bash
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=tests/doctor_fixtures/lib.sh
. "$SCRIPT_DIR/../lib.sh"
FM="fm-state_files-empty-or-truncated-database"
if [ "${EE_DOCTOR_FIXTURE_RUN_EE:-0}" != "1" ]; then
    printf '%s assertion requires EE_DOCTOR_FIXTURE_RUN_EE=1; marker-only checks are insufficient\n' "$FM" >&2
    exit 2
fi
target="$(doctor_fixture_target)"
ee_bin="${EE_DOCTOR_FIXTURE_BINARY:-ee}"
base="$target/.fixture_baseline"
test -f "$(doctor_fixture_marker_dir "$target")/$FM.json"

# GUIDANCE-ONLY (bd-xa6ud / bd-wswg0, fixed at 9ed78b70d; oracle measured on a
# stamped 466ee56ee build, bd-2oh15 c9944). A truncated store is reported and
# never touched: database and search_index both report EE-E202 and posture is
# blocked before and after --fix; --fix exits 0 and records only manual
# guidance (database_corrupted); every workspace byte is unchanged.
"$ee_bin" doctor --workspace "$target" --json > "$base/guidance-before.json"
jq -es '
    length == 1 and (.[0] |
        .schema == "ee.response.v2" and .success == true and .data.posture == "blocked" and
        any(.data.actionable[]; .name == "database" and .errorCode == "EE-E202") and
        any(.data.actionable[]; .name == "search_index" and .errorCode == "EE-E202"))
' "$base/guidance-before.json" >/dev/null
# A repair would request an index rebuild; guidance must not.
test ! -e "$target/.ee/index-rebuild-request.json"
doctor_fixture_content_digest "$target" > "$base/guidance-before-fix.sha256"
doctor_fixture_assert_guidance_only "$FM" "database_corrupted" "database" "EE-E202" \
    ".ee/index-rebuild-request.json"
jq -es '
    length == 1 and (.[0] |
        .data.status == "completed_ok" and
        .data.guidanceOnlyFixerCount == (.data.fixerResults | length) and
        all(.data.fixerResults[]; .operation == "manual" and .outcome == "guidance_recorded"))
' "$base/doctor-fix.json" >/dev/null
jq -es '
    length == 1 and (.[0] |
        .data.posture == "blocked" and
        any(.data.actionable[]; .name == "search_index" and .errorCode == "EE-E202"))
' "$base/doctor-after.json" >/dev/null
doctor_fixture_content_digest "$target" > "$base/guidance-after-fix.sha256"
if ! cmp -s "$base/guidance-before-fix.sha256" "$base/guidance-after-fix.sha256"; then
    printf 'fixture assert: %s guidance-only --fix changed workspace bytes\n' "$FM" >&2
    exit 1
fi
printf 'guidance-only confirmed: %s (EE-E202 blocked before and after; --fix exit 0, manual guidance only; bytes unchanged)\n' "$FM" >&2
