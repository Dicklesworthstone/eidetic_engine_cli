#!/usr/bin/env bash
# Fail if the migration registry has drifted from the compiled MIGRATIONS list.
#
# bd-zs76e. The manifest's own policy.runtimeUpdateRule says "Update this
# registry in the same change that adds a compiled migration." That rule was
# declared and did not bind: V124 and V125 each shipped without it, the second
# eighteen seconds after the enforcing check landed.
#
# The check itself was correct both times. It simply never ran: verify.sh has no
# automated consumer, no CI workflow or git hook invokes it, and this swarm
# commits with --no-verify. "The guard passed it" and "the guard was never run"
# look identical from outside.
#
# So this exists to make running it cost one line. It takes under a second, needs
# no cargo, and is safe to run on a dirty tree. The same two checks also run
# inside scripts/e2e_cross_cutting.sh; this is the standalone entry point.
#
#   scripts/check-migration-registry.sh   # exit 0 clean, 1 on drift
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
MANIFEST="$REPO_ROOT/tests/fixtures/contracts/dueling_wizards_migration_registry.json"
MIGRATIONS="$REPO_ROOT/src/db/mod.rs"

command -v jq >/dev/null 2>&1 || { printf 'check-migration-registry: jq is required\n' >&2; exit 2; }
[ -f "$MANIFEST" ] || { printf 'check-migration-registry: missing %s\n' "$MANIFEST" >&2; exit 2; }
[ -f "$MIGRATIONS" ] || { printf 'check-migration-registry: missing %s\n' "$MIGRATIONS" >&2; exit 2; }

# Compiled tail: the last V###_ entry between `pub const MIGRATIONS` and its
# closing `];`. Mirrors the contract test's own parser. POSIX awk only, so it
# runs on the macOS workers too.
compiled_tail="$(
    awk '
        /pub const MIGRATIONS/ { inside = 1 }
        inside && /\];/ { exit }
        inside && /^[[:space:]]*V[0-9][0-9][0-9]_/ {
            entry = $0
            sub(/^[[:space:]]*V/, "", entry)
            sub(/_.*$/, "", entry)
            last = entry
        }
        END { print last + 0 }
    ' "$MIGRATIONS"
)"
registry_tail="$(jq -r '.currentLastCompiledMigration' "$MANIFEST")"

failures=0

if [ "$registry_tail" != "$compiled_tail" ]; then
    printf 'FAIL registry tail V%s does not match compiled tail V%s\n' \
        "$registry_tail" "$compiled_tail" >&2
    printf '     update currentLastCompiledMigration (and nextPlannedMigration) in\n' >&2
    printf '     tests/fixtures/contracts/dueling_wizards_migration_registry.json\n' >&2
    failures=$((failures + 1))
else
    printf 'ok   registry tail matches compiled tail (V%s)\n' "$compiled_tail"
fi

if jq -e --argjson tail "$compiled_tail" \
    '[.allocations[] | select(.status == "planned") | .version] | min > $tail' \
    "$MANIFEST" >/dev/null 2>&1; then
    printf 'ok   planned reservations sit ahead of the compiled tail\n'
else
    planned="$(jq -c '[.allocations[] | select(.status == "planned") | .version]' "$MANIFEST")"
    printf 'FAIL planned reservations %s collide with compiled tail V%s\n' \
        "$planned" "$compiled_tail" >&2
    printf '     shift the documentation-only reservations forward past the tail\n' >&2
    failures=$((failures + 1))
fi

if [ "$failures" -ne 0 ]; then
    printf '\ncheck-migration-registry: %d check(s) failed\n' "$failures" >&2
    exit 1
fi
printf 'check-migration-registry: clean\n'
