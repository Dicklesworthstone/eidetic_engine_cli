#!/usr/bin/env bash
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=tests/doctor_fixtures/lib.sh
. "$SCRIPT_DIR/../lib.sh"

# NOT-DETECTED, PINNED GAP (bd-2oh15 ruling c9986): two initialized
# workspaces are both candidates for one command. --workspace names the
# target while EE_WORKSPACE names a second healthy workspace, and --workspace
# silently wins. ee workspace resolve reports
# workspace_explicit_environment_conflict; doctor reports nothing. The second
# workspace lives under .fixture_baseline (outside the content digest), and
# the environment is carried in .fixture_baseline/env.sh for assert.sh.
FM="fm-state_files-workspace-ambiguous-multiple-candidates"
target="$(doctor_fixture_target)"
ee_bin="${EE_DOCTOR_FIXTURE_BINARY:-ee}"
command -v "$ee_bin" >/dev/null
export EE_EMBED_DOWNLOAD="${EE_EMBED_DOWNLOAD:-off}"
unset EE_WORKSPACE
doctor_fixture_prepare_target "$target"
doctor_fixture_healthy_store "$FM" "$target" "$ee_bin"
base="$target/.fixture_baseline"

other="$base/other-workspace"
mkdir -p "$other"
"$ee_bin" --workspace "$other" init --skip-boilerplate --json > "$base/other-init.json"
jq -e '.schema == "ee.response.v2" and .success == true' "$base/other-init.json" >/dev/null

# Resolution is run from inside the target, so the workspace discovered from
# the current directory is the target itself and cannot add a second finding.
# Negative control: without EE_WORKSPACE there is exactly one candidate.
(cd "$target" && "$ee_bin" --workspace "$target" --json workspace resolve) > "$base/resolve-control.json"
jq -e '
    .schema == "ee.response.v2" and .success == true and
    ([.data.diagnostics[]?.code] | length == 0)
' "$base/resolve-control.json" >/dev/null

printf 'export EE_WORKSPACE=%q\n' "$other" > "$base/env.sh"
. "$base/env.sh"
(cd "$target" && "$ee_bin" --workspace "$target" --json workspace resolve) > "$base/resolve-ambiguous.json"
if ! jq -e '
    .schema == "ee.response.v2" and .success == true and
    ([.data.diagnostics[]?.code] == ["workspace_explicit_environment_conflict"])
' "$base/resolve-ambiguous.json" >/dev/null; then
    printf 'workspace-ambiguous fixture: resolve did not report exactly workspace_explicit_environment_conflict; see %s\n' \
        "$base/resolve-ambiguous.json" >&2
    exit 1
fi
doctor_fixture_corrupt "$FM" "P1" "state_files"
printf 'real ambiguity confirmed: --workspace %s vs EE_WORKSPACE %s (workspace_explicit_environment_conflict)\n' \
    "$target" "$other" >&2
