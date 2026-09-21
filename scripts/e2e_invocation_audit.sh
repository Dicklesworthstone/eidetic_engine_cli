#!/usr/bin/env bash
# bd-smxdr follow-on — every scripts/e2e_*.sh must be invoked by something.
#
# This session found six separate pieces of test machinery that existed,
# were committed, and ran nowhere: a binary-resolution guard unwired since
# May, 59 mcp unit tests in no CI job, and four whole e2e suites referenced
# by nothing. The repo's real coverage problem is not missing tests, it is
# written tests that nothing invokes.
#
# The Rust side already solved this: tests/suites/inventory.rs asserts every
# tests/*.rs is registered in exactly one shard, and carries its own self-test
# proving it catches unwired, duplicate and stale entries. There was no shell
# equivalent. This is it.
#
# WHAT COUNTS AS INVOKED: any reference to the script's basename anywhere in
# the repo outside target/ and .git/, other than the script itself. That
# deliberately includes tests/*.rs, because 32 e2e scripts are driven from
# Rust harnesses rather than from verify.sh. An earlier version of this audit
# scanned only scripts/ .github/ Makefile and reported 41 orphans where the
# true number is 19 -- more than double, because it was asking a narrower
# question than the one that matters.
#
# BASELINE: 19 scripts are already orphaned. Failing on them would wedge the
# gate, so they are recorded in the baseline file and the audit fails only on
# CHANGE. Critically it fails in BOTH directions:
#   - a NEW orphan appears            -> someone added a suite and wired it
#                                        nowhere; the thing this exists to stop
#   - a BASELINED orphan is now wired -> the baseline is stale; delete the line
# The second arm is what stops the baseline decaying into a permanent
# ignore-list. It can only shrink.
#
# RETIRED — the third category (2026-09-16). Some scripts should be neither
# wired nor counted as debt: wrappers whose assertions are already run by
# something else. tests/fixtures/e2e_invocation/retired.txt holds those, one
# per line, as
#     <script> | <covering command> | <assertion coverage lost>
#
# This is deliberately NOT a softer baseline. An entry re-earns the category on
# every run: the script must still be un-invoked, must not also sit in the
# orphan baseline, must declare `none` coverage lost, and its covering command
# must be a literal that ACTUALLY APPEARS in ci.yml or verify.sh. That last
# check is the point — a retired entry naming a command that runs nowhere is
# precisely the failure this audit exists to catch, so it is verified rather
# than believed. If you cannot name a real covering command, the script stays
# orphaned and the gate stays red.
#
# Usage:
#   scripts/e2e_invocation_audit.sh              audit the repo
#   scripts/e2e_invocation_audit.sh --self-test  prove every failure direction
#   scripts/e2e_invocation_audit.sh --list       print the current orphan set

set -uo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BASELINE="${REPO_ROOT}/tests/fixtures/e2e_invocation/orphan_baseline.txt"
RETIRED="${REPO_ROOT}/tests/fixtures/e2e_invocation/retired.txt"

# The scripts this audit is responsible for, as paths relative to scripts/.
#
# TWO directories, not one (bd-1cn5o). The original glob was scripts/e2e_*.sh
# alone, which is 93 files -- while scripts/e2e_overhaul/ holds 115 more that
# the gate never looked at. Measured before widening: 36 of those 115 had no
# reference outside docs/ anywhere in the tree, so the audit was asserting
# "written tests actually execute" over less than half its own subject.
#
# Overhaul scripts are identified by their path relative to scripts/
# (e2e_overhaul/foo.sh) rather than by basename. There are zero basename
# collisions between the two directories today, so bare names would work -- but
# scripts/e2e_overhaul/tiered_recall.sh sitting next to a registered
# tiered_recall_e2e.sh is exactly the near-miss this gate exists to catch, and a
# bare-name scheme would silently merge a future collision into one entry.
e2e_audit_paths() {
    local script_dir="$1"
    local path
    for path in "$script_dir"/e2e_*.sh; do
        [ -e "$path" ] && printf '%s\n' "${path##*/}"
    done
    for path in "$script_dir"/e2e_overhaul/*.sh "$script_dir"/e2e_overhaul/lib/*.sh; do
        [ -e "$path" ] && printf 'e2e_overhaul/%s\n' "${path#"$script_dir"/e2e_overhaul/}"
    done
    return 0
}

# Print the identifiers of e2e scripts that nothing else references.
# Pure function of the tree so the self-test can drive it over a fixture.
e2e_orphans() {
    local root="$1"
    local script_dir="$root/scripts"
    [ -d "$script_dir" ] || return 0
    local path base needle
    while IFS= read -r base; do
        [ -n "$base" ] || continue
        path="$script_dir/$base"
        # This audit is a tool, not an e2e suite, despite matching the glob.
        [ "$base" = "e2e_invocation_audit.sh" ] && continue
        # The baseline file lists orphans BY NAME, so it must be excluded from
        # the reference scan. Without this every baselined script appears
        # "referenced" by the baseline itself and the audit reports zero
        # orphans forever -- a gate made vacuous by its own bookkeeping.
        # "Invoked" is NOT the same as "mentioned". Naming a script inside a
        # COMMENT -- e.g. a verify.sh note explaining why it is deliberately
        # NOT wired -- would otherwise mark it invoked and silently retire the
        # baseline entry. Found by running this audit against its own triage
        # commit, which is the only way that ambiguity shows up.
        #
        # So require the name on a line with no preceding '#': comment lines
        # and trailing-comment mentions do not count as invocation.
        #
        # DOCUMENTATION IS EXCLUDED FOR THE SAME REASON (bd-q2nq9). Markdown
        # cannot invoke anything, and a Markdown line almost never begins with
        # '#' unless it is a heading -- so an ordinary English sentence or a
        # table cell naming a script satisfied the old predicate. Twelve
        # scripts passed this gate on prose alone:
        #     "`scripts/e2e_read_coalescing.sh` asserts this"   docs/read_coalescing.md:73
        #     "| e2e | `scripts/e2e_sandbox.sh` | 21.5 |"       docs/agent-ux/.../sandbox.md:31
        # Each was verified by hand to have NO reference in scripts/, tests/ or
        # .github/. The comment exclusion above and this one are the same rule
        # applied to two spellings of the same mistake: a mention standing in
        # for an invocation.
        #
        # THE NEEDLE IS THE BASENAME, THE IDENTIFIER IS THE PATH. Callers name
        # these scripts by basename -- e2e_overhaul.sh's EPIC_SCRIPTS registry
        # holds `pack_format.sh`, not `e2e_overhaul/pack_format.sh`. Searching
        # for the path-qualified form would find zero references for all 23
        # registered epics and report them as orphans, which is a false
        # positive of exactly the size that would get this gate disabled.
        needle="${base##*/}"
        # TWO MORE SPELLINGS OF "A MENTION STANDING IN FOR AN INVOCATION",
        # found 2026-09-19 by reconciling this audit against an independent
        # closure walk. They disagreed on exactly six scripts, and the audit
        # was wrong on all six -- in the direction that HIDES orphans.
        #
        # (a) TEST FIXTURES ARE DATA, NOT INVOKERS. Five suites were "invoked"
        #     solely by tests/fixtures/contracts/dueling_wizards_verify_wiring.json
        #     and tests/fixtures/failure_modes/*.json -- files that DESCRIBE
        #     wiring, including wiring that does not exist. A fixture asserting
        #     "this script is wired" was accepted as proof that it is.
        #     Narrowing from tests/fixtures/e2e_invocation/** to tests/fixtures/**
        #     orphans exactly those five and nothing else: the only .sh under
        #     fixtures names scripts/lib/e2e_logger.sh, a library outside this
        #     population that has 11 other referrers.
        #
        # (b) THE COMMENT RULE ONLY KNEW '#'. `^[^#]*` rejects a shell comment
        #     but a `//` comment contains no '#', so a Rust line counted:
        #         src/core/search.rs:23188
        #         // exercised by scripts/e2e_rerank.sh, not unit tests.
        #     A sentence explaining that a script PROVIDES the coverage was
        #     read as the coverage. tests/*.rs must keep counting -- 32 suites
        #     are genuinely driven from there -- so the file cannot be excluded
        #     the way docs/ was; only its comment lines can.
        #
        # (c) THE GLOBS ARE `**/`-PREFIXED BECAUSE THE SELF-TEST WAS TESTING A
        #     DIFFERENT PREDICATE THAN PRODUCTION. A ripgrep --glob containing
        #     '/' anchors to the CWD, not to the search root. In production
        #     root IS the cwd, so `!tests/fixtures/e2e_invocation/**` worked;
        #     in --self-test root is a temp dir, so the same glob matched
        #     nothing and the fixture arm below passed against an exclusion
        #     that was silently inert. `!**/tests/fixtures/**` matches under
        #     both roots. The docs arm never surfaced this because `!**/*.md`
        #     caught its fixture regardless of whether `!docs/**` applied.
        # (d) `--no-require-git` IS LOAD-BEARING, NOT TIDINESS (bd-05i9j).
        #     ripgrep applies .gitignore ONLY inside a git repository. The RCH
        #     clean-overlay tree is SYNCED rather than cloned and has no .git
        #     (measured: HAS_DOT_GIT=no), so without this flag every
        #     gitignored-but-synced directory becomes searchable there. The one
        #     that matters is beads_compliance_audit/ -- 8397 files on the
        #     worker, gitignored at .gitignore:322, not excluded by .rchignore --
        #     whose bead JSON mentions script paths in TITLES:
        #
        #       {"title":"sandbox: e2e script scripts/e2e_sandbox.sh (...)"}
        #
        #     which is the same "a sentence about coverage read AS coverage"
        #     substitution that (b) above already fixed for docs/ and .md,
        #     arriving through a channel nobody anticipated. Measured effect:
        #     52 orphans on macOS, 6 on two Linux workers, SAME COMMIT, same
        #     ripgrep 15.1.0 -- 46 scripts silently reclassified as invoked.
        #
        #     Two exclusions here are explicit (`target`, `.git`) and the rest
        #     were left to ripgrep's implicit filtering. The explicit half
        #     survives a clean overlay and the implicit half does not. This
        #     flag makes the filtering a property of the GATE rather than of
        #     the tree it happens to run in, which is the invariant that was
        #     missing. It changes nothing where .git exists, so the baseline
        #     stays valid.
        if ! rg --no-require-git --no-heading -N "^[^#]*$(printf '%s' "$needle" | sed 's/\./\\./g')" "$root" \
            --glob '!target' --glob '!.git' --glob "!scripts/$base" \
            --glob '!**/tests/fixtures/**' \
            --glob '!**/*.md' --glob '!**/docs/**' 2>/dev/null \
            | awk 'BEGIN { found = 0 }
                   { body = $0; sub(/^[^:]*:/, "", body)
                     if (body !~ /^[[:space:]]*\/\//) { found = 1 } }
                   END { exit(found ? 0 : 1) }'; then
            printf '%s\n' "$base"
        fi
    done < <(e2e_audit_paths "$script_dir")
}

read_baseline() {
    local file="$1"
    [ -f "$file" ] || return 0
    sed -e 's/#.*//' -e 's/[[:space:]]//g' "$file" | grep -v '^$' | sort -u
}

# Basenames listed in the retired ledger. Field 1 of each non-comment line.
read_retired_names() {
    local file="$1"
    [ -f "$file" ] || return 0
    grep -v '^[[:space:]]*#' "$file" | grep -v '^[[:space:]]*$' \
        | awk -F'|' '{gsub(/[[:space:]]/,"",$1); if ($1 != "") print $1}' | sort -u
}

# Validate every retired entry. Prints one diagnostic per violation and returns
# 1 if any fired. A retired entry has to EARN the category on every run:
# a named covering command that genuinely exists, and zero coverage lost.
validate_retired() {
    local root="$1" file="$2" orphans="$3"
    [ -f "$file" ] || return 0
    local bad=0 line script covering lost
    while IFS= read -r line; do
        case "$line" in ''|\#*) continue ;; esac
        script="$(printf '%s' "$line" | awk -F'|' '{gsub(/^[[:space:]]+|[[:space:]]+$/,"",$1); print $1}')"
        covering="$(printf '%s' "$line" | awk -F'|' '{gsub(/^[[:space:]]+|[[:space:]]+$/,"",$2); print $2}')"
        lost="$(printf '%s' "$line" | awk -F'|' '{gsub(/^[[:space:]]+|[[:space:]]+$/,"",$3); print $3}')"

        if [ -z "$script" ] || [ -z "$covering" ] || [ -z "$lost" ]; then
            echo "  ${script:-<no script>}: needs all three fields — <script> | <covering command> | <coverage lost>" >&2
            bad=1
            continue
        fi

        # Rule 4: retiring something with real coverage loss is deleting
        # coverage, not retiring a wrapper.
        if [ "$lost" != "none" ]; then
            echo "  $script: assertion coverage lost is '$lost', must be 'none'" >&2
            bad=1
        fi

        # Rule 1: if something invokes it again, it is not retired.
        if ! printf '%s\n' "$orphans" | grep -qx "$script"; then
            echo "  $script: is invoked again, so it is not retired — delete this line" >&2
            bad=1
        fi

        # Rule 3: the covering command must actually exist somewhere that runs.
        # Naming a command that runs nowhere is the exact failure this audit
        # exists to catch, so it is checked rather than believed.
        if ! grep -Fq -- "$covering" "$root/.github/workflows/ci.yml" 2>/dev/null \
            && ! grep -Fq -- "$covering" "$root/scripts/verify.sh" 2>/dev/null; then
            echo "  $script: covering command is in neither ci.yml nor verify.sh: '$covering'" >&2
            bad=1
        fi
    done <"$file"
    return "$bad"
}

if [[ "${1:-}" == "--list" ]]; then
    e2e_orphans "$REPO_ROOT" | sort -u
    exit 0
fi

if [[ "${1:-}" == "--self-test" ]]; then
    tmp=$(mktemp -d "${TMPDIR:-/private/tmp}/e2e-invocation-audit.XXXXXX")
    failures=0
    mkdir -p "$tmp/fixture/scripts" "$tmp/fixture/tests"

    # wired.sh is referenced from a Rust harness -- the path the first version
    # of this audit missed. It must NOT be reported.
    printf '#!/bin/sh\nexit 0\n' >"$tmp/fixture/scripts/e2e_wired.sh"
    printf 'fn drive() { run("scripts/e2e_wired.sh"); }\n' >"$tmp/fixture/tests/driver.rs"
    # lonely.sh is referenced by nothing.
    printf '#!/bin/sh\nexit 0\n' >"$tmp/fixture/scripts/e2e_lonely.sh"

    got="$(e2e_orphans "$tmp/fixture" | sort -u | tr '\n' ' ')"
    if [[ "$got" == "e2e_lonely.sh " ]]; then
        echo "ok   - detects the unreferenced script"
    else
        echo "FAIL - orphan set was '$got', wanted 'e2e_lonely.sh '"
        failures=$((failures + 1))
    fi
    if [[ "$got" != *"e2e_wired.sh"* ]]; then
        echo "ok   - a script driven from tests/*.rs is NOT an orphan"
    else
        echo "FAIL - misreported a Rust-driven script as orphaned"
        failures=$((failures + 1))
    fi

    # A script named ONLY in documentation is still an orphan (bd-q2nq9).
    # Paired with the e2e_wired.sh arm above, which proves a real reference in
    # tests/*.rs still counts -- a predicate that rejected everything would
    # pass this arm and fail that one.
    mkdir -p "$tmp/fixture/docs"
    printf '#!/bin/sh\nexit 0\n' >"$tmp/fixture/scripts/e2e_documented.sh"
    printf 'The suite `scripts/e2e_documented.sh` asserts the invariant.\n' \
        >"$tmp/fixture/docs/note.md"
    got_docs="$(e2e_orphans "$tmp/fixture" | sort -u | tr '\n' ' ')"
    if [[ "$got_docs" == *"e2e_documented.sh"* ]]; then
        echo "ok   - a script mentioned only in docs is still an orphan"
    else
        echo "FAIL - a docs-only mention counted as invocation, got '$got_docs'"
        failures=$((failures + 1))
    fi
    if [[ "$got_docs" != *"e2e_wired.sh"* ]]; then
        echo "ok   - excluding docs did not break the Rust-driven case"
    else
        echo "FAIL - excluding docs broke detection of a real reference"
        failures=$((failures + 1))
    fi

    # A FIXTURE IS DATA. A JSON file under tests/fixtures/ that NAMES a script
    # -- even in a field called "script" -- describes wiring rather than
    # performing it, and five real suites passed this gate on exactly that.
    mkdir -p "$tmp/fixture/tests/fixtures/contracts"
    printf '#!/bin/sh\nexit 0\n' >"$tmp/fixture/scripts/e2e_fixtured.sh"
    printf '{ "stages": [ { "script": "scripts/e2e_fixtured.sh" } ] }\n' \
        >"$tmp/fixture/tests/fixtures/contracts/wiring.json"
    got_fix="$(e2e_orphans "$tmp/fixture" | sort -u | tr '\n' ' ')"
    if [[ "$got_fix" == *"e2e_fixtured.sh"* ]]; then
        echo "ok   - a script named only in a tests/fixtures JSON is still an orphan"
    else
        echo "FAIL - a fixture mention counted as invocation, got '$got_fix'"
        failures=$((failures + 1))
    fi
    if [[ "$got_fix" != *"e2e_wired.sh"* ]]; then
        echo "ok   - excluding fixtures did not break the Rust-driven case"
    else
        echo "FAIL - excluding fixtures broke detection of a real reference"
        failures=$((failures + 1))
    fi

    # A '//' COMMENT IS STILL A COMMENT. The original rule rejected '#' only,
    # so a Rust doc line naming a script counted as running it. tests/*.rs has
    # to keep counting, which is what the paired arm below protects: the
    # distinction is the COMMENT, not the file type.
    printf '#!/bin/sh\nexit 0\n' >"$tmp/fixture/scripts/e2e_slashcommented.sh"
    printf 'fn note() {\n    // exercised by scripts/e2e_slashcommented.sh, not here.\n}\n' \
        >"$tmp/fixture/tests/commented.rs"
    got_slash="$(e2e_orphans "$tmp/fixture" | sort -u | tr '\n' ' ')"
    if [[ "$got_slash" == *"e2e_slashcommented.sh"* ]]; then
        echo "ok   - a script named only in a // comment is still an orphan"
    else
        echo "FAIL - a // comment counted as invocation, got '$got_slash'"
        failures=$((failures + 1))
    fi
    if [[ "$got_slash" != *"e2e_wired.sh"* ]]; then
        echo "ok   - rejecting // comments did not break the Rust-driven case"
    else
        echo "FAIL - rejecting // comments broke a real tests/*.rs reference"
        failures=$((failures + 1))
    fi

    # scripts/e2e_overhaul/ is in the population too (bd-1cn5o), and its
    # entries are identified by path while the SEARCH still uses the basename.
    # Both halves need an arm, because getting either one wrong is silent:
    # a missing population means 115 files go unexamined, and a path-qualified
    # search term would report all 23 registered epics as orphans at once.
    mkdir -p "$tmp/fixture/scripts/e2e_overhaul"
    printf '#!/bin/sh\nexit 0\n' >"$tmp/fixture/scripts/e2e_overhaul/lonely_epic.sh"
    printf '#!/bin/sh\nexit 0\n' >"$tmp/fixture/scripts/e2e_overhaul/wired_epic.sh"
    # Referenced the way e2e_overhaul.sh's EPIC_SCRIPTS registry does it: by
    # BASENAME, with no directory prefix.
    printf 'declare -A EPIC_SCRIPTS=(\n    [A]="wired_epic.sh"\n)\n' \
        >"$tmp/fixture/scripts/driver_registry.sh"
    got_overhaul="$(e2e_orphans "$tmp/fixture" | sort -u | tr '\n' ' ')"
    if [[ "$got_overhaul" == *"e2e_overhaul/lonely_epic.sh"* ]]; then
        echo "ok   - an unreferenced scripts/e2e_overhaul script is an orphan"
    else
        echo "FAIL - e2e_overhaul is outside the population, got '$got_overhaul'"
        failures=$((failures + 1))
    fi
    if [[ "$got_overhaul" != *"wired_epic"* ]]; then
        echo "ok   - an overhaul script referenced by BASENAME is not an orphan"
    else
        echo "FAIL - basename reference missed; a path-qualified search term would"
        echo "       report every registered epic as orphaned, got '$got_overhaul'"
        failures=$((failures + 1))
    fi

    # Direction 1: a new orphan against an empty baseline must fail.
    printf '# empty\n' >"$tmp/baseline_empty.txt"
    new_orphans="$(comm -23 <(printf 'e2e_lonely.sh\n') <(read_baseline "$tmp/baseline_empty.txt"))"
    if [[ -n "$new_orphans" ]]; then
        echo "ok   - a new orphan is caught against the baseline"
    else
        echo "FAIL - new orphan not caught"
        failures=$((failures + 1))
    fi

    # Direction 2: a baselined entry that is now wired must ALSO fail, or the
    # baseline decays into a permanent ignore-list.
    printf 'e2e_lonely.sh\ne2e_wired.sh\n' >"$tmp/baseline_stale.txt"
    stale="$(comm -13 <(printf 'e2e_lonely.sh\n') <(read_baseline "$tmp/baseline_stale.txt"))"
    if [[ "$stale" == "e2e_wired.sh" ]]; then
        echo "ok   - a stale baseline entry is caught (baseline can only shrink)"
    else
        echo "FAIL - stale baseline entry not caught, got '$stale'"
        failures=$((failures + 1))
    fi

    # ---- retired-ledger arms. Every rule gets a negative control, because a
    # validator that has only ever been shown a passing input is untested.
    mkdir -p "$tmp/fixture/.github/workflows"
    printf 'jobs:\n  run: cargo test --workspace --lib real::\n' \
        >"$tmp/fixture/.github/workflows/ci.yml"
    printf '#!/usr/bin/env bash\nexit 0\n' >"$tmp/fixture/scripts/verify.sh"
    orphan_set="$(printf 'e2e_lonely.sh\n')"

    # 5. A well-formed entry naming a command CI really runs must pass.
    printf 'e2e_lonely.sh | cargo test --workspace --lib real:: | none\n' >"$tmp/retired_ok.txt"
    if validate_retired "$tmp/fixture" "$tmp/retired_ok.txt" "$orphan_set" 2>/dev/null; then
        echo "ok   - a valid retired entry is accepted"
    else
        echo "FAIL - valid retired entry was rejected"
        failures=$((failures + 1))
    fi

    # 6. A covering command that exists nowhere must FAIL. This is the arm that
    #    stops "retired" becoming prose: the named command is checked, not read.
    printf 'e2e_lonely.sh | cargo test --workspace --lib imaginary:: | none\n' >"$tmp/retired_fake.txt"
    if validate_retired "$tmp/fixture" "$tmp/retired_fake.txt" "$orphan_set" 2>/dev/null; then
        echo "FAIL - accepted a covering command that runs nowhere"
        failures=$((failures + 1))
    else
        echo "ok   - rejects a covering command that runs nowhere"
    fi

    # 7. Retiring something that loses real coverage is deleting coverage.
    printf 'e2e_lonely.sh | cargo test --workspace --lib real:: | the redaction cases\n' \
        >"$tmp/retired_lossy.txt"
    if validate_retired "$tmp/fixture" "$tmp/retired_lossy.txt" "$orphan_set" 2>/dev/null; then
        echo "FAIL - accepted a retired entry that admits losing coverage"
        failures=$((failures + 1))
    else
        echo "ok   - rejects a retired entry whose coverage loss is not none"
    fi

    # 8. A script that something invokes again is not retired.
    printf 'e2e_wired.sh | cargo test --workspace --lib real:: | none\n' >"$tmp/retired_stale.txt"
    if validate_retired "$tmp/fixture" "$tmp/retired_stale.txt" "$orphan_set" 2>/dev/null; then
        echo "FAIL - accepted a retired entry for a script that is invoked again"
        failures=$((failures + 1))
    else
        echo "ok   - rejects a retired entry whose script is invoked again"
    fi

    # 9. Missing fields must fail rather than silently retiring on a blank.
    printf 'e2e_lonely.sh | | none\n' >"$tmp/retired_blank.txt"
    if validate_retired "$tmp/fixture" "$tmp/retired_blank.txt" "$orphan_set" 2>/dev/null; then
        echo "FAIL - accepted a retired entry with an empty covering command"
        failures=$((failures + 1))
    else
        echo "ok   - rejects a retired entry with an empty field"
    fi

    echo "self-test: $((17 - failures))/17 passed"
    [[ "$failures" -eq 0 ]] || exit 2
    exit 0
fi

if ! command -v rg >/dev/null 2>&1; then
    echo "e2e_invocation_audit: ripgrep (rg) is required" >&2
    exit 3
fi

CURRENT="$(e2e_orphans "$REPO_ROOT" | sort -u)"
BASE="$(read_baseline "$BASELINE")"
RETIRED_NAMES="$(read_retired_names "$RETIRED")"

rc=0

# Retired entries are validated BEFORE they are allowed to suppress anything.
# An invalid entry does not quietly fall back to "orphaned" — it fails the
# audit, so a malformed justification cannot be used to park a script.
if [[ -n "$RETIRED_NAMES" ]]; then
    RETIRED_DIAGNOSTICS="$(validate_retired "$REPO_ROOT" "$RETIRED" "$CURRENT" 2>&1)" || true
    if [[ -n "$RETIRED_DIAGNOSTICS" ]]; then
        echo "e2e_invocation_audit: RETIRED entr(ies) failed validation:" >&2
        printf '%s\n' "$RETIRED_DIAGNOSTICS" >&2
        echo "  File: tests/fixtures/e2e_invocation/retired.txt" >&2
        rc=1
    fi
fi

# A validated retired script is neither a new orphan nor a baseline entry.
CURRENT_UNRETIRED="$(comm -23 <(printf '%s\n' "$CURRENT" | grep -v '^$') <(printf '%s\n' "$RETIRED_NAMES" | grep -v '^$'))"

# Rule 2: no double-booking. An entry in both ledgers lets a shrinking baseline
# look like progress while the script simply moved sideways.
DOUBLE_BOOKED="$(comm -12 <(printf '%s\n' "$BASE" | grep -v '^$') <(printf '%s\n' "$RETIRED_NAMES" | grep -v '^$'))"
if [[ -n "$DOUBLE_BOOKED" ]]; then
    echo "e2e_invocation_audit: script(s) in BOTH orphan_baseline.txt and retired.txt — pick one:" >&2
    printf '  %s\n' $DOUBLE_BOOKED >&2
    rc=1
fi

NEW_ORPHANS="$(comm -23 <(printf '%s\n' "$CURRENT_UNRETIRED" | grep -v '^$') <(printf '%s\n' "$BASE" | grep -v '^$'))"
STALE_ENTRIES="$(comm -13 <(printf '%s\n' "$CURRENT_UNRETIRED" | grep -v '^$') <(printf '%s\n' "$BASE" | grep -v '^$'))"

if [[ -n "$NEW_ORPHANS" ]]; then
    echo "e2e_invocation_audit: NEW orphaned e2e script(s) — written but invoked by nothing:" >&2
    printf '  %s\n' $NEW_ORPHANS >&2
    echo "  Wire each into scripts/verify.sh or a tests/*.rs harness, or retire it." >&2
    rc=1
fi

if [[ -n "$STALE_ENTRIES" ]]; then
    echo "e2e_invocation_audit: STALE baseline entr(ies) — now invoked, so remove from the baseline:" >&2
    printf '  %s\n' $STALE_ENTRIES >&2
    echo "  File: tests/fixtures/e2e_invocation/orphan_baseline.txt" >&2
    rc=1
fi

# Denominator excludes this audit script, which matches the glob but is a
# tool rather than a suite -- the same exclusion the scan loop makes.
#
# It is derived from e2e_audit_paths, the SAME function the scan loop walks, so
# the reported denominator cannot drift from the population actually examined.
# It used to be an independent `ls` of one directory, which is how the gate came
# to report "of 92 e2e scripts" while 115 more sat outside it (bd-1cn5o).
TOTAL=$(( $(e2e_audit_paths "$REPO_ROOT/scripts" | grep -vc '^$') - 1 ))
echo "e2e_invocation_audit: $(printf '%s\n' "$CURRENT_UNRETIRED" | grep -vc '^$') orphaned of ${TOTAL} e2e scripts; baseline holds $(printf '%s\n' "$BASE" | grep -vc '^$'); retired $(printf '%s\n' "$RETIRED_NAMES" | grep -vc '^$')" >&2

exit "$rc"
