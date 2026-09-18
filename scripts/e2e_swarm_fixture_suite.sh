#!/usr/bin/env bash
# Driver for the swarm/ownership fixture e2e suites (bd-udjrq).
#
# These five were committed and wired into nothing. Each was executed for the
# first time on 2026-09-18 and passed on its own; this driver is what makes
# them run again. They are grouped into one verify.sh stage rather than five
# because the budget ceiling admits measured seconds, not stage slots, and
# five 0.4s scripts do not each deserve an entry.
#
# It collects EVERY failure and fails once with the full list. Returning on
# the first mismatch is how a second defect stays hidden behind the first --
# the whole reason these suites went years without executing.

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
# `|| exit 1`, not a bare cd: this script runs under `set -uo pipefail` with
# no -e, so a failed cd would leave it resolving every scripts/... path from
# the wrong directory and reporting all five members "not executable" -- a
# false report from the one script whose job is not to make false reports.
cd "$REPO_ROOT" || exit 1

SCRIPTS=(
    e2e_overhaul/ownership_posture.sh
    e2e_overhaul/swarm_incident_recovery_actions.sh
    e2e_overhaul/swarm_incidents.sh
    e2e_overhaul/swarm_next_action_profile.sh
    e2e_overhaul/swarm_schemas.sh
)

SCRIPT_BUDGET_SECONDS="${EE_SWARM_FIXTURE_SUITE_SCRIPT_BUDGET_SECONDS:-120}"

failures=()
for script in "${SCRIPTS[@]}"; do
    path="scripts/$script"
    if [ ! -x "$path" ]; then
        # A committed script with no execute bit can never have run. One of
        # this set's siblings was in exactly that state (9b022ce19), so the
        # driver checks rather than assuming.
        printf '  FAIL %-46s not executable\n' "$script"
        failures+=("$script (not executable)")
        continue
    fi
    started="$(date +%s)"
    if timeout "$SCRIPT_BUDGET_SECONDS" "./$path" >/dev/null 2>&1; then
        printf '  ok   %-46s %ss\n' "$script" "$(( $(date +%s) - started ))"
    else
        status=$?
        printf '  FAIL %-46s exit %s after %ss\n' "$script" "$status" "$(( $(date +%s) - started ))"
        failures+=("$script (exit $status)")
    fi
done

if [ "${#failures[@]}" -ne 0 ]; then
    printf 'swarm fixture suite: %s of %s failed:\n' \
        "${#failures[@]}" "${#SCRIPTS[@]}" >&2
    printf '  - %s\n' "${failures[@]}" >&2
    exit 1
fi

printf 'swarm fixture suite: %s of %s passed\n' "${#SCRIPTS[@]}" "${#SCRIPTS[@]}"
