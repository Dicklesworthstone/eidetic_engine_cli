#!/usr/bin/env bash
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=tests/doctor_fixtures/lib.sh
. "$SCRIPT_DIR/../lib.sh"

# NOT-DETECTED, PINNED GAP (bd-2oh15 rulings c9985/c9986; t2250 NEXT): a
# second initialized workspace nested INSIDE the target, at nested/child. From
# the child directory the ancestry holds two .ee markers: the nearest wins
# unless --workspace is explicit, so a command meant for one store can land in
# the other. ee workspace resolve reports workspace_nested_markers; doctor,
# run on the outer workspace, reports nothing. The nested store is ordinary
# workspace content, so the damage is in the target's own bytes.
FM="fm-workspace_config-nested-ee-markers"
target="$(doctor_fixture_target)"
ee_bin="${EE_DOCTOR_FIXTURE_BINARY:-ee}"
command -v "$ee_bin" >/dev/null
export EE_EMBED_DOWNLOAD="${EE_EMBED_DOWNLOAD:-off}"
unset EE_WORKSPACE
doctor_fixture_prepare_target "$target"
doctor_fixture_healthy_store "$FM" "$target" "$ee_bin"
base="$target/.fixture_baseline"
child="$target/nested/child"

# Negative control: before the nested store exists, resolving from a
# directory inside the target finds one marker and reports nothing.
mkdir -p "$child"
(cd "$child" && "$ee_bin" --json workspace resolve) > "$base/resolve-control.json"
jq -e --arg target "$target" '
    .schema == "ee.response.v2" and .success == true and
    ([.data.diagnostics[]?.code] | length == 0)
' "$base/resolve-control.json" >/dev/null

"$ee_bin" --workspace "$child" init --skip-boilerplate --json > "$base/child-init.json"
jq -e '.schema == "ee.response.v2" and .success == true' "$base/child-init.json" >/dev/null
test -d "$child/.ee"
printf '%s\n' "nested/child" > "$base/child-path"

(cd "$child" && "$ee_bin" --json workspace resolve) > "$base/resolve-nested.json"
if ! jq -e '[.data.diagnostics[]?.code] == ["workspace_nested_markers"]' \
    "$base/resolve-nested.json" >/dev/null; then
    printf 'nested-markers fixture: resolve from the child did not report exactly workspace_nested_markers; see %s\n' \
        "$base/resolve-nested.json" >&2
    exit 1
fi
doctor_fixture_corrupt "$FM" "P1" "workspace_config"
printf 'real nesting confirmed: %s inside %s (workspace_nested_markers)\n' "$child" "$target" >&2
