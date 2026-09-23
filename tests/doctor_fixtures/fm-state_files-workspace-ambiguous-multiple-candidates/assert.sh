#!/usr/bin/env bash
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=tests/doctor_fixtures/lib.sh
. "$SCRIPT_DIR/../lib.sh"
FM="fm-state_files-workspace-ambiguous-multiple-candidates"
if [ "${EE_DOCTOR_FIXTURE_RUN_EE:-0}" != "1" ]; then
    printf '%s assertion requires EE_DOCTOR_FIXTURE_RUN_EE=1; marker-only checks are insufficient\n' "$FM" >&2
    exit 2
fi
target="$(doctor_fixture_target)"
ee_bin="${EE_DOCTOR_FIXTURE_BINARY:-ee}"
base="$target/.fixture_baseline"
test -f "$(doctor_fixture_marker_dir "$target")/$FM.json"
. "$base/env.sh"
test -d "$EE_WORKSPACE/.ee"

# Independent witness: ee's own workspace resolution still sees two
# candidates for this command and reports the conflict.
(cd "$target" && "$ee_bin" --workspace "$target" --json workspace resolve) > "$base/resolve-assert.json"
if ! jq -e '
    .schema == "ee.response.v2" and .success == true and
    ([.data.diagnostics[]?.code] == ["workspace_explicit_environment_conflict"])
' "$base/resolve-assert.json" >/dev/null; then
    printf 'fixture assert: %s witness lost: resolve no longer reports workspace_explicit_environment_conflict; see %s\n' \
        "$FM" "$base/resolve-assert.json" >&2
    exit 1
fi
# doctor runs under the same ambiguous environment, from the same directory.
cd "$target"
doctor_fixture_assert_pinned_gap "$FM" "-"
