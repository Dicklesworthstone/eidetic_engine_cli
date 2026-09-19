#!/usr/bin/env bash
# bd-yytkz — identifiers that must have EXACTLY ONE definition in the tree.
#
# WHY THIS EXISTS. f9a8b5010 deduplicated RAW_TOKEN_PATTERNS into a single
# module-level const shared by the detector and the redactor, so drift between
# copies became impossible by construction. Its own doc comment then recorded
# the cost of that shape (src/policy/mod.rs):
#
#     a module-level const can be SHADOWED by a local of the same name, where
#     two function-local consts could not shadow each other. If a future edit
#     declares `RAW_TOKEN_PATTERNS` inside either of these functions again,
#     that function silently stops sharing this table and the drift this
#     change removed comes back with no compiler complaint. Do not reintroduce
#     a local of this name; extend the table here instead.
#
# That last sentence is accurate and enforced by nothing. "Do not" in a comment
# is a PRESCRIPTION WITH NO RUNNER, and this repo has now found that same shape
# in deny.toml [bans], ci.yml disabled_manually, path-gated delivery workflows,
# the failure-mode catalog walker, and `cargo clippy -D warnings`.
#
# THIS ONE IS CHEAP TO GUARD BECAUSE THE CHECK IS A GREP, NOT A COMPILE. It
# needs no cargo, no target dir and no compile budget, so it can live in CI
# Static beside forbidden-deps, migration-registry, closure-lint,
# vision-coverage and contract-drift-radar -- a job that actually executes,
# rather than a gate that exists and never runs.
#
# Usage:
#   scripts/check-pinned-definitions.sh              audit the repo
#   scripts/check-pinned-definitions.sh --self-test  prove every failure direction
#
# Exit: 0 clean, 1 gate failure, 2 self-test failure, 3 environment error.

set -uo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

# name | expected | why it is pinned (printed on failure, so the next reader
# learns the reason from the failure rather than from archaeology)
PINNED=(
    "RAW_TOKEN_PATTERNS|1|shadowing this module-level const with a function-local of the same name silently un-shares the table between the detector and the redactor, reintroducing the drift f9a8b5010 removed, with no compiler complaint (bd-yytkz)"
)

# Print every declaration of NAME under ROOT, as path:line:text.
#
# THE --include GLOB IS QUOTED. Unquoted, zsh expands *.rs against the CURRENT
# DIRECTORY before grep ever sees it; where nothing matches, the command fails
# and prints a clean "0 definitions" -- a zero from a broken instrument, which
# reads exactly like a zero from a clean tree. That is how this gate was nearly
# reported as finding the constant ABSENT.
#
# The pattern requires the trailing ':' of a const declaration and a boundary
# before `const`, so RAW_TOKEN_PATTERNS_V2 and a mention in prose do not count.
# No '\b': that is a GNU extension and CI and macOS would disagree.
scan_definitions() {
    local root="$1" name="$2"
    grep -rnE "(^|[^A-Za-z0-9_])const[[:space:]]+${name}[[:space:]]*:" \
        --include='*.rs' "$root" 2>/dev/null || true
}

# Check one pinned identifier. Prints its hits, always.
check_one() {
    local root="$1" name="$2" expected="$3" reason="$4"
    local hits count
    hits="$(scan_definitions "$root" "$name")"
    count="$(printf '%s' "$hits" | grep -c . || true)"

    # EMPTY-WORLD GUARD, and it is a DISTINCT failure from "too many".
    #
    # A gate that only fails on 2 is vacuously green at 0, and 0 is what a
    # rename, a file move, or a broken invocation produces. Those are the
    # cases where the pin has stopped protecting anything, so they must be
    # louder than the case it was written for -- not silently passing.
    if [ "$count" -eq 0 ]; then
        printf 'check-pinned-definitions: %s has ZERO definitions, expected %s.\n' \
            "$name" "$expected" >&2
        printf '  This is NOT a pass. Zero means the pin is no longer protecting\n' >&2
        printf '  anything: the identifier was renamed or moved, or this scan is\n' >&2
        printf '  broken. Re-point or remove the entry in PINNED deliberately.\n' >&2
        printf '  Why it was pinned: %s\n' "$reason" >&2
        return 1
    fi

    if [ "$count" -ne "$expected" ]; then
        printf 'check-pinned-definitions: %s has %s definitions, expected %s:\n' \
            "$name" "$count" "$expected" >&2
        printf '%s\n' "$hits" | sed 's/^/  /' >&2
        printf '  Why it is pinned: %s\n' "$reason" >&2
        return 1
    fi

    # PRINT THE HITS ON SUCCESS TOO. A gate that prints only a count on the
    # happy path cannot be distinguished, by reading its log, from a gate that
    # matched nothing and said so politely.
    printf 'check-pinned-definitions: %s = %s definition (expected %s)\n' \
        "$name" "$count" "$expected"
    printf '%s\n' "$hits" | sed 's/^/  /'
    return 0
}

run_audit() {
    local root="$1" entry name expected reason rc=0
    for entry in "${PINNED[@]}"; do
        IFS='|' read -r name expected reason <<< "$entry"
        check_one "$root" "$name" "$expected" "$reason" || rc=1
    done
    return "$rc"
}

if [[ "${1:-}" == "--self-test" ]]; then
    command -v grep >/dev/null 2>&1 || { echo "error: grep not found" >&2; exit 3; }
    tmp="$(mktemp -d "${TMPDIR:-/tmp}/pinned-definitions.XXXXXX")" || exit 3
    failures=0
    mkdir -p "$tmp/src"

    arm() { # name | expect_rc | description
        local dir="$1" want="$2" desc="$3"
        check_one "$dir" "PINNED_TABLE" 1 "self-test" >/dev/null 2>&1
        local got=$?
        if [ "$got" -eq "$want" ]; then
            echo "ok   - $desc"
        else
            echo "FAIL - $desc (wanted rc=$want, got rc=$got)"
            failures=$((failures + 1))
        fi
    }

    # THE KNOWN POSITIVE FIRST. If the scanner cannot find a definition that is
    # plainly there, every "not found" below is vacuous and proves nothing.
    printf 'const PINNED_TABLE: &[&str] = &["a"];\n' >"$tmp/src/one.rs"
    arm "$tmp" 0 "exactly one definition passes"

    # EMPTY WORLD. The arm this gate exists to have.
    empty="$(mktemp -d "${TMPDIR:-/tmp}/pinned-empty.XXXXXX")" || exit 3
    mkdir -p "$empty/src"
    printf 'fn unrelated() {}\n' >"$empty/src/other.rs"
    arm "$empty" 1 "ZERO definitions FAILS (not vacuously green)"

    # THE SHADOWING CASE ITSELF: a second, function-local declaration.
    printf 'fn shadow() {\n    const PINNED_TABLE: &[&str] = &["b"];\n}\n' >"$tmp/src/two.rs"
    arm "$tmp" 1 "a function-local shadow makes it two and FAILS"

    # A near-miss name must not count, or the pin fires on unrelated edits and
    # gets switched off.
    near="$(mktemp -d "${TMPDIR:-/tmp}/pinned-near.XXXXXX")" || exit 3
    mkdir -p "$near/src"
    printf 'const PINNED_TABLE: &[&str] = &["a"];\nconst PINNED_TABLE_V2: &[&str] = &["b"];\n' \
        >"$near/src/near.rs"
    arm "$near" 0 "a longer identifier sharing the prefix is not counted"

    # A mention in prose is not a declaration -- the substitution this repo
    # keeps finding, including in the verification of this very fix.
    prose="$(mktemp -d "${TMPDIR:-/tmp}/pinned-prose.XXXXXX")" || exit 3
    mkdir -p "$prose/src"
    printf 'const PINNED_TABLE: &[&str] = &["a"];\n// do not add a local const PINNED_TABLE here\n' \
        >"$prose/src/prose.rs"
    arm "$prose" 0 "a comment naming the identifier is not a second definition"

    # A non-.rs file must not contribute, which is what the quoted --include
    # protects. Unquoted, this arm would pass for the wrong reason.
    other="$(mktemp -d "${TMPDIR:-/tmp}/pinned-other.XXXXXX")" || exit 3
    mkdir -p "$other/src"
    printf 'const PINNED_TABLE: &[&str] = &["a"];\n' >"$other/src/only.rs"
    printf 'const PINNED_TABLE: &[&str] = &["b"];\n' >"$other/src/notes.txt"
    arm "$other" 0 "a non-.rs file naming the identifier is not counted"

    echo "self-test: $((6 - failures))/6 passed"
    [ "$failures" -eq 0 ] || exit 2
    exit 0
fi

if [[ "${1:-}" == "--help" || "${1:-}" == "-h" ]]; then
    echo "usage: $0 [--self-test]" >&2
    exit 0
fi

run_audit "$REPO_ROOT"
