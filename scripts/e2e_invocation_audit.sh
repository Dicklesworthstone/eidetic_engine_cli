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

# Print the basenames of e2e scripts in <dir> that nothing else references.
# Pure function of the tree so the self-test can drive it over a fixture.
e2e_orphans() {
    local root="$1"
    local script_dir="$root/scripts"
    [ -d "$script_dir" ] || return 0
    local path base
    for path in "$script_dir"/e2e_*.sh; do
        [ -e "$path" ] || continue
        base="$(basename "$path")"
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
        if ! rg -l "^[^#]*$(printf '%s' "$base" | sed 's/\./\\./g')" "$root" \
            --glob '!target' --glob '!.git' --glob "!scripts/$base" \
            --glob '!tests/fixtures/e2e_invocation/**' \
            >/dev/null 2>&1; then
            printf '%s\n' "$base"
        fi
    done
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

    echo "self-test: $((9 - failures))/9 passed"
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
TOTAL=$(( $(ls "$REPO_ROOT"/scripts/e2e_*.sh 2>/dev/null | wc -l | tr -d ' ') - 1 ))
echo "e2e_invocation_audit: $(printf '%s\n' "$CURRENT_UNRETIRED" | grep -vc '^$') orphaned of ${TOTAL} e2e scripts; baseline holds $(printf '%s\n' "$BASE" | grep -vc '^$'); retired $(printf '%s\n' "$RETIRED_NAMES" | grep -vc '^$')" >&2

exit "$rc"
