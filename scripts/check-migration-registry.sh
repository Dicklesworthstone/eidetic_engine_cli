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
MANIFEST="${EE_MIGRATION_REGISTRY_MANIFEST:-$REPO_ROOT/tests/fixtures/contracts/dueling_wizards_migration_registry.json}"
MIGRATIONS="${EE_MIGRATION_REGISTRY_SOURCE:-$REPO_ROOT/src/db/mod.rs}"
ISSUES_JSONL="${EE_MIGRATION_REGISTRY_ISSUES:-$REPO_ROOT/.beads/issues.jsonl}"

command -v jq >/dev/null 2>&1 || { printf 'check-migration-registry: jq is required\n' >&2; exit 2; }

if [ "${1:-}" = "--self-test" ]; then
    tmp_dir="$(mktemp -d "${TMPDIR:-/tmp}/ee-migration-registry.XXXXXX")"
    trap 'rm -rf "$tmp_dir"' EXIT
    cat > "$tmp_dir/mod.rs" <<'RS'
pub const MIGRATIONS: &[Migration] = &[
    V001_INIT,
    V002_SECOND,
];
RS
    cat > "$tmp_dir/issues.jsonl" <<'JSONL'
{"id":"bd-1n0np.1","status":"closed"}
{"id":"bd-1n0np.2","status":"open"}
{"id":"bd-1n0np.3","status":"closed"}
JSONL
    cat > "$tmp_dir/good.json" <<'JSON'
{
  "currentLastCompiledMigration": 2,
  "nextPlannedMigration": 3,
  "allocations": [
    {"id": "a1", "version": 1, "status": "implemented", "ownerBead": "bd-1n0np.1"},
    {"id": "a2", "version": 3, "status": "planned", "ownerBead": "bd-1n0np.2"},
    {"id": "a3", "version": 4, "status": "not_stored", "ownerBead": "bd-1n0np.3", "scopeRuling": "delivered without table"}
  ]
}
JSON
    cat > "$tmp_dir/bad_closed_owner.json" <<'JSON'
{
  "currentLastCompiledMigration": 2,
  "nextPlannedMigration": 3,
  "allocations": [
    {"id": "a1", "version": 1, "status": "implemented", "ownerBead": "bd-1n0np.1"},
    {"id": "a2", "version": 3, "status": "planned", "ownerBead": "bd-1n0np.3"}
  ]
}
JSON
    EE_MIGRATION_REGISTRY_MANIFEST="$tmp_dir/good.json" \
        EE_MIGRATION_REGISTRY_SOURCE="$tmp_dir/mod.rs" \
        EE_MIGRATION_REGISTRY_ISSUES="$tmp_dir/issues.jsonl" \
        "$0" >/dev/null
    if EE_MIGRATION_REGISTRY_MANIFEST="$tmp_dir/bad_closed_owner.json" \
        EE_MIGRATION_REGISTRY_SOURCE="$tmp_dir/mod.rs" \
        EE_MIGRATION_REGISTRY_ISSUES="$tmp_dir/issues.jsonl" \
        "$0" >/dev/null 2>&1; then
        printf 'FAIL self-test: planned allocation with closed ownerBead must be rejected\n' >&2
        exit 1
    fi
    printf 'ok   check-migration-registry self-test passed\n'
    exit 0
fi

[ -f "$MANIFEST" ] || { printf 'check-migration-registry: missing %s\n' "$MANIFEST" >&2; exit 2; }
[ -f "$MIGRATIONS" ] || { printf 'check-migration-registry: missing %s\n' "$MIGRATIONS" >&2; exit 2; }
[ -f "$ISSUES_JSONL" ] || { printf 'check-migration-registry: missing %s\n' "$ISSUES_JSONL" >&2; exit 2; }

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
    '([.allocations[] | select(.status == "planned") | .version] | (length == 0 or min > $tail)) and ([.allocations[] | select(.status == "not_stored") | .version] | (length == 0 or min > $tail))' \
    "$MANIFEST" >/dev/null 2>&1; then
    printf 'ok   planned/not_stored reservations sit ahead of the compiled tail\n'
else
    planned="$(jq -c '[.allocations[] | select(.status == "planned" or .status == "not_stored") | .version]' "$MANIFEST")"
    printf 'FAIL planned/not_stored reservations %s collide with compiled tail V%s\n' \
        "$planned" "$compiled_tail" >&2
    printf '     shift the documentation-only reservations forward past the tail\n' >&2
    failures=$((failures + 1))
fi

# bd-5p62d: every `planned` allocation must name a live (non-closed) ownerBead
# in .beads/issues.jsonl, and every `not_stored` allocation must name an
# existing ownerBead plus a non-empty scopeRuling explaining how the capability
# landed without a dedicated table.
owner_errors="$(
    jq -r -n \
        --slurpfile manifest "$MANIFEST" \
        --slurpfile issues "$ISSUES_JSONL" \
        '
        ($issues | INDEX(.id)) as $by_id
        | $manifest[0].allocations[]
        | . as $alloc
        | ($by_id[$alloc.ownerBead] // null) as $bead
        | if $bead == null then
            "\($alloc.id): ownerBead \($alloc.ownerBead) not found in .beads/issues.jsonl"
          elif $alloc.status == "planned" and $bead.status == "closed" then
            "\($alloc.id): status is planned (V\($alloc.version)) but ownerBead \($alloc.ownerBead) is closed; re-own on a live bead or mark not_stored with a scopeRuling"
          elif $alloc.status == "not_stored" and ((($alloc.scopeRuling // "") | gsub("^\\s+|\\s+$"; "")) == "") then
            "\($alloc.id): status is not_stored but scopeRuling is missing or empty"
          else
            empty
          end
        '
)"
if [ -z "$owner_errors" ]; then
    printf 'ok   allocation ownerBead liveness and not_stored scope rulings verified\n'
else
    printf 'FAIL allocation ownerBead liveness / scope ruling check failed:\n%s\n' "$owner_errors" >&2
    failures=$((failures + 1))
fi

if [ "$failures" -ne 0 ]; then
    printf '\ncheck-migration-registry: %d check(s) failed\n' "$failures" >&2
    exit 1
fi
printf 'check-migration-registry: clean\n'
