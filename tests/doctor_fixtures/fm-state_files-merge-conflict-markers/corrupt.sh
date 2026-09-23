#!/usr/bin/env bash
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=tests/doctor_fixtures/lib.sh
. "$SCRIPT_DIR/../lib.sh"

# NOT-DETECTED, PINNED GAP (bd-2oh15 c9863): a valid workspace config.toml that
# search accepts is MOVED into the baseline and replaced by the same content
# plus git merge-conflict markers. search/pack then fail with a configuration
# error while doctor and status report ok.
FM="fm-state_files-merge-conflict-markers"
target="$(doctor_fixture_target)"
ee_bin="${EE_DOCTOR_FIXTURE_BINARY:-ee}"
command -v "$ee_bin" >/dev/null
export EE_EMBED_DOWNLOAD="${EE_EMBED_DOWNLOAD:-off}"
doctor_fixture_prepare_target "$target"
doctor_fixture_healthy_store "$FM" "$target" "$ee_bin"
base="$target/.fixture_baseline"

# A comment-only config is valid TOML and changes no setting. (An earlier
# baseline used a [search] key this binary rejects, so the "clean" config was
# itself invalid -- bd-2oh15 c9863 correction.) The baseline search below must
# succeed, proving the config is accepted before any markers are added.
printf '# workspace configuration (doctor fixture baseline)\n' > "$target/.ee/config.toml"
"$ee_bin" search "source memory" --workspace "$target" --json > "$base/search-clean-config.json"
jq -e '.schema == "ee.response.v2" and .success == true' "$base/search-clean-config.json" >/dev/null
"$ee_bin" doctor --workspace "$target" --json > "$base/doctor-clean-config.json"
doctor_fixture_assert_health_report "$FM" "$base/doctor-clean-config.json"

mv "$target/.ee/config.toml" "$base/config.toml.clean"
{
    cat "$base/config.toml.clean"
    printf '<<<<<<< HEAD\n# ours\n=======\n# theirs\n>>>>>>> branch\n'
} > "$target/.ee/config.toml"

doctor_fixture_corrupt "$FM" "P0" "state_files"
set +e
"$ee_bin" search "source memory" --workspace "$target" --json > "$base/search-conflicted.json" 2>&1
rc=$?
set -e
if [ "$rc" -eq 0 ] || ! jq -e '.schema == "ee.error.v2" and .error.code == "configuration"' \
    "$base/search-conflicted.json" >/dev/null; then
    printf 'merge-conflict fixture: search did not fail with a configuration error (exit %s)\n' "$rc" >&2
    exit 1
fi
printf 'real corruption confirmed: conflict markers in .ee/config.toml (search exit %s, configuration)\n' "$rc" >&2
