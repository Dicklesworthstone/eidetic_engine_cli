#!/usr/bin/env bash
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=tests/doctor_fixtures/lib.sh
. "$SCRIPT_DIR/../lib.sh"
FM="fm-workspace_config-nested-ee-markers"
if [ "${EE_DOCTOR_FIXTURE_RUN_EE:-0}" != "1" ]; then
    printf '%s assertion requires EE_DOCTOR_FIXTURE_RUN_EE=1; marker-only checks are insufficient\n' "$FM" >&2
    exit 2
fi
target="$(doctor_fixture_target)"
ee_bin="${EE_DOCTOR_FIXTURE_BINARY:-ee}"
base="$target/.fixture_baseline"
test -f "$(doctor_fixture_marker_dir "$target")/$FM.json"
unset EE_WORKSPACE
child="$target/$(cat "$base/child-path")"

# Independent witness: from inside the nested store, ee's own resolution still
# sees two markers in the ancestry, and the nearest one (the child) wins.
(cd "$child" && "$ee_bin" --json workspace resolve) > "$base/resolve-assert.json"
if ! jq -e '[.data.diagnostics[]?.code] == ["workspace_nested_markers"]' \
    "$base/resolve-assert.json" >/dev/null; then
    printf 'fixture assert: %s witness lost: resolve from the child no longer reports workspace_nested_markers; see %s\n' \
        "$FM" "$base/resolve-assert.json" >&2
    exit 1
fi
# doctor runs on the OUTER workspace, from inside the nested one: the most
# favourable position for it to notice the nesting.
cd "$child"
doctor_fixture_assert_pinned_gap "$FM" "-"
test -d "$child/.ee"
