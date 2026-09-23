#!/usr/bin/env bash
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=tests/doctor_fixtures/lib.sh
. "$SCRIPT_DIR/../lib.sh"

# NOT-DETECTED, PINNED GAP (bd-2oh15 survey c9983; measured on a stamped
# 02524267e build): a valid workspace config.toml that search accepts is MOVED
# into the baseline and replaced by syntactically invalid TOML (an unclosed
# table header and a key without a value). search then fails with a
# configuration error, while doctor swallows the parse error and reports ok.
FM="fm-workspace_config-config-toml-malformed"
target="$(doctor_fixture_target)"
ee_bin="${EE_DOCTOR_FIXTURE_BINARY:-ee}"
command -v "$ee_bin" >/dev/null
export EE_EMBED_DOWNLOAD="${EE_EMBED_DOWNLOAD:-off}"
doctor_fixture_prepare_target "$target"
doctor_fixture_healthy_store "$FM" "$target" "$ee_bin"
base="$target/.fixture_baseline"

# A comment-only config is valid TOML and changes no setting. The baseline
# search must succeed, so the config is accepted before it is broken.
printf '# workspace configuration (doctor fixture baseline)\n' > "$target/.ee/config.toml"
"$ee_bin" search "source memory" --workspace "$target" --json > "$base/search-clean-config.json"
jq -e '.schema == "ee.response.v2" and .success == true' "$base/search-clean-config.json" >/dev/null
"$ee_bin" doctor --workspace "$target" --json > "$base/doctor-clean-config.json"
doctor_fixture_assert_health_report "$FM" "$base/doctor-clean-config.json"

mv "$target/.ee/config.toml" "$base/config.toml.clean"
{
    cat "$base/config.toml.clean"
    printf '[search\nbroken =\n'
} > "$target/.ee/config.toml"

doctor_fixture_corrupt "$FM" "P1" "workspace_config"
set +e
"$ee_bin" search "source memory" --workspace "$target" --json > "$base/search-malformed.json" 2>&1
rc=$?
set -e
if [ "$rc" -eq 0 ] || ! jq -e '.schema == "ee.error.v2" and .error.code == "configuration"' \
    "$base/search-malformed.json" >/dev/null; then
    printf 'config-malformed fixture: search did not fail with a configuration error (exit %s)\n' "$rc" >&2
    exit 1
fi
printf 'real corruption confirmed: invalid TOML in .ee/config.toml (search exit %s, configuration)\n' "$rc" >&2
