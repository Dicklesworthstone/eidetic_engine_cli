#!/usr/bin/env bash
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=tests/doctor_fixtures/lib.sh
. "$SCRIPT_DIR/../lib.sh"
FM="fm-workspace_config-config-toml-malformed"
if [ "${EE_DOCTOR_FIXTURE_RUN_EE:-0}" != "1" ]; then
    printf '%s assertion requires EE_DOCTOR_FIXTURE_RUN_EE=1; marker-only checks are insufficient\n' "$FM" >&2
    exit 2
fi
target="$(doctor_fixture_target)"
ee_bin="${EE_DOCTOR_FIXTURE_BINARY:-ee}"
base="$target/.fixture_baseline"
test -f "$(doctor_fixture_marker_dir "$target")/$FM.json"

# Independent witness: a real command fails on the malformed config.
set +e
"$ee_bin" search "source memory" --workspace "$target" --json > "$base/search-assert.json" 2>&1
rc=$?
set -e
if [ "$rc" -eq 0 ] || ! jq -e '.schema == "ee.error.v2" and .error.code == "configuration"' \
    "$base/search-assert.json" >/dev/null; then
    printf 'fixture assert: %s witness lost: search exit %s without a configuration error\n' "$FM" "$rc" >&2
    exit 1
fi
doctor_fixture_assert_pinned_gap "$FM" ".ee/config.toml"
