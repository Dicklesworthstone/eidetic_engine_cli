#!/usr/bin/env bash
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=tests/doctor_fixtures/lib.sh
. "$SCRIPT_DIR/../lib.sh"

# condition.sh <command> [args...] (contract in lib.sh, doctor_fixture_under_condition).
# The damage is the environment: EE_WORKSPACE names a second workspace while
# --workspace names the target. Apply it, check it with the same witness
# assert.sh uses, then run the command from inside the target.
FM="fm-state_files-workspace-ambiguous-multiple-candidates"
target="$(doctor_fixture_target)"
ee_bin="${EE_DOCTOR_FIXTURE_BINARY:-ee}"
base="$target/.fixture_baseline"
not_applied() {
    printf 'condition: %s not applied: %s\n' "$FM" "$1" >&2
    exit "$DOCTOR_FIXTURE_CONDITION_NOT_APPLIED"
}
[ -f "$base/env.sh" ] || not_applied "no $base/env.sh"
. "$base/env.sh"
[ -d "${EE_WORKSPACE:-}/.ee" ] || not_applied "EE_WORKSPACE is not an initialized workspace"
cd "$target"
"$ee_bin" --workspace "$target" --json workspace resolve > "$base/condition-resolve.json" 2>/dev/null ||
    not_applied "workspace resolve failed"
jq -e '[.data.diagnostics[]?.code] == ["workspace_explicit_environment_conflict"]' \
    "$base/condition-resolve.json" >/dev/null ||
    not_applied "resolve does not report workspace_explicit_environment_conflict"
exec "$@"
