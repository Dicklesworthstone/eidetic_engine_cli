#!/usr/bin/env bash
# bd-pnj3s / bd-6g6s2 — run a remote job and hand back a graded, pasteable verdict.
#
# WHY THIS EXISTS. Every remote verdict in this swarm reaches a bead through an
# agent reading a log file. There is no repo-owned code on that path, so the
# reading rule this repo keeps rediscovering -- MATCH AN ANNOUNCEMENT TO ITS
# SUMMARY, NEVER TAKE THE LAST -- is enforced by attention alone. On 2026-09-18
# the person enforcing that rule applied it to three pieces of work and still
# missed it on a fourth. A rule that lives only in attention fails at the rate
# attention fails.
#
# WHAT IT IS NOT
#   It does NOT replace, wrap-around, or hide `rch exec`. Bare `rch exec` keeps
#   working exactly as before, for everyone, unchanged. A chokepoint that becomes
#   the ONLY path is a single point of failure on the one capability this swarm
#   cannot lose -- and today five of thirteen workers are healthy. If this script
#   is broken, deleted, or simply not used, verification is unaffected.
#
# WHY YOU WOULD USE IT ANYWAY, which is the only adoption model that survives
# someone being in a hurry: it hands you the verdict block you would otherwise
# assemble by hand -- announcement, summary, reconciliation, target, host, base,
# command, log path -- already checked and ready to paste into a bead. That
# assembly is what cost this lane the most time today, and it is the part that
# gets skipped under pressure.
#
# GRADING NEVER BLOCKS EXECUTION. The run's own exit status is always printed
# verbatim and always dominates. If the grader cannot parse the log, the script
# says so loudly and still reports what the run did. Fail closed on the VERDICT;
# never on the EXECUTION.
#
# USAGE
#   scripts/rch_run.sh [--log <path>] [--expect-target <substr>] -- <rch args...>
#   scripts/rch_run.sh --self-test
#
# EXAMPLES
#   scripts/rch_run.sh -- --base "$SHA" --clean-overlay --no-overlay \
#       -- cargo test --locked --lib -- some_filter
#   scripts/rch_run.sh --expect-target contracts -- --job -- bash -c './scripts/e2e_x.sh'
#
# EXIT
#   the run's exit code, when the run itself failed (execution dominates)
#   0  run succeeded AND the log grades green
#   1  run succeeded but the verdict is not green, or could not be graded

set -uo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
GRADER="$HERE/lib/grade_test_log.py"

warn() { printf '[rch-run] %s\n' "$*" >&2; }
say() { printf '[rch-run] %s\n' "$*"; }

# Assemble the verdict block and decide the wrapper's exit.
#   $1 log path   $2 the run's exit code   $3 base label   $4 expect-target ("" for none)
#   $5 the command, for the record
# Kept separate from the run so --self-test can drive it with fixtures; a
# wrapper whose reporting half cannot be tested without a fleet would not be.
emit_verdict_block() {
    local log="$1" run_exit="$2" base="$3" expect="$4" cmd="$5"
    local host grade_out grade_exit

    printf '\n===== VERDICT BLOCK (paste into the bead) =====\n'
    printf 'command      : %s\n' "$cmd"
    printf 'base         : %s\n' "$base"

    # The host is the field this lane omitted twice today. Report it, or report
    # that it could not be found -- never leave it silently absent.
    host="$(grep -aoE '\b(hz[0-9]+|vmi[0-9]+)\b' "$log" 2>/dev/null | head -1)"
    printf 'worker host  : %s\n' "${host:-NOT RECORDED IN LOG}"
    printf 'run exit     : %s\n' "$run_exit"
    printf 'log          : %s\n' "$log"

    if [ ! -r "$log" ]; then
        printf 'verdict      : UNGRADEABLE (log unreadable)\n'
        printf '===== END VERDICT BLOCK =====\n'
        warn "log is unreadable; the run's own exit status above is the only fact here."
        [ "$run_exit" -ne 0 ] && return "$run_exit"
        return 1
    fi

    if [ ! -f "$GRADER" ] || ! command -v python3 >/dev/null 2>&1; then
        printf 'verdict      : UNGRADEABLE (grader unavailable)\n'
        printf '===== END VERDICT BLOCK =====\n'
        warn "grader missing or python3 absent -- NOT claiming a verdict."
        [ "$run_exit" -ne 0 ] && return "$run_exit"
        return 1
    fi

    if [ -n "$expect" ]; then
        grade_out="$(python3 "$GRADER" --expect-target "$expect" "$log" 2>&1)"
    else
        grade_out="$(python3 "$GRADER" "$log" 2>&1)"
    fi
    grade_exit=$?

    printf '%s\n' "$grade_out" | sed 's/^/  /'
    printf '===== END VERDICT BLOCK =====\n'

    # EXECUTION DOMINATES. A failed run is a failed run whatever the log says,
    # and its code is surfaced verbatim rather than flattened to 1.
    if [ "$run_exit" -ne 0 ]; then
        warn "run exited ${run_exit}; that is the fact of record. Grade above is context."
        return "$run_exit"
    fi
    if [ "$grade_exit" -ne 0 ]; then
        warn "run exited 0 but the log does NOT grade green. Refusing to call this a pass."
        return 1
    fi
    return 0
}

# What the verdict is actually bound to, from the rch arguments.
#   $@  the rch arguments, exactly as they will be passed through
# MEASURED DEFECT, 2026-09-18: this used to print `git rev-parse HEAD` under the
# label "base" even when --base pinned the run to a different commit. A run at
# --base 0226dbeafd reported "base: b5ca17108" because a peer pushed mid-run.
# That is worse than omitting the field: the one thing the verdict block exists
# to bind was reported wrong, confidently, in the pasteable block. When --base
# is given, the local checkout is not what was built and must not be named.
derive_base() {
    local arg prev=""
    for arg in "$@"; do
        if [ "$prev" = "--base" ]; then
            printf '%s (pinned by --base; local HEAD is NOT what ran)\n' "$arg"
            return 0
        fi
        case "$arg" in
            --base=*)
                printf '%s (pinned by --base; local HEAD is NOT what ran)\n' "${arg#--base=}"
                return 0
                ;;
        esac
        prev="$arg"
    done
    if git rev-parse --short HEAD >/dev/null 2>&1; then
        printf '%s (+%s dirty) UNPINNED: bound to the working tree\n' \
            "$(git rev-parse --short HEAD)" \
            "$(git status --porcelain | wc -l | tr -d ' ')"
    else
        printf 'not a git repository\n'
    fi
}

self_test() {
    local tmp failures=0 got
    tmp="$(mktemp -d)" || { warn "self-test could not make a temp dir"; return 1; }

    printf 'running 4 tests\ntest result: ok. 4 passed; 0 failed; 0 ignored; 0 measured; 1 filtered out\n' > "$tmp/green.log"
    printf 'running 4 tests\ntest result: FAILED. 3 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out\n' > "$tmp/red.log"
    printf 'running 0 tests\ntest result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 99 filtered out\n' > "$tmp/zero.log"
    printf 'running 9 tests\nrunning 1 test\ntest result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 50 filtered out\ntest result: ok. 9 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out\n' > "$tmp/nested.log"
    printf '     Running unittests src/lib.rs (target/debug/deps/ee-a)\nrunning 2 tests\ntest result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out\n     Running tests/contracts.rs (target/debug/deps/contracts-b)\nrunning 3 tests\ntest result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out\n' > "$tmp/multi.log"
    printf 'rsync: connection unexpectedly closed\n[RCH] remote hz4 failed [RCH-E104] SSH command timed out\n' > "$tmp/ungradeable.log"

    # name | log | run_exit | expect-target | want wrapper exit
    local -a cases=(
        "green run, graded green|$tmp/green.log|0||0"
        "green run, log shows failures|$tmp/red.log|0||1"
        "green run, ZERO tests executed|$tmp/zero.log|0||1"
        "green run, nested child summary|$tmp/nested.log|0||1"
        "green run, two targets, ambiguous|$tmp/multi.log|0||1"
        "green run, two targets, disambiguated|$tmp/multi.log|0|contracts-b|0"
        "UNGRADEABLE log, run exited 0|$tmp/ungradeable.log|0||1"
        "run FAILED: its exit code dominates|$tmp/green.log|101||101"
        "log missing entirely|$tmp/does-not-exist.log|0||1"
    )
    local entry name log rexit expect want
    for entry in "${cases[@]}"; do
        IFS='|' read -r name log rexit expect want <<< "$entry"
        emit_verdict_block "$log" "$rexit" "self-test" "$expect" "self-test" >/dev/null 2>&1
        got=$?
        if [ "$got" -eq "$want" ]; then
            say "self-test OK   ${name}: want ${want}, got ${got}"
        else
            warn "SELF-TEST FAIL ${name}: want ${want}, got ${got}"
            failures=$((failures + 1))
        fi
    done

    # derive_base arms. The reporting half is only trustworthy if the base it
    # prints is the base the run used, so each arm asserts the printed string
    # rather than asserting that the function merely succeeded.
    local -a base_cases=(
        "--base spelled separately|--base|0226dbeafd|--clean-overlay|0226dbeafd"
        "--base= spelled joined|--base=0226dbeafd|--clean-overlay||0226dbeafd"
        "a later --base still found|--clean-overlay|--base|0226dbeafd|0226dbeafd"
    )
    local bname a1 a2 a3 want_base got_base total=$((${#cases[@]} + ${#base_cases[@]} + 1))
    for entry in "${base_cases[@]}"; do
        IFS='|' read -r bname a1 a2 a3 want_base <<< "$entry"
        got_base="$(derive_base "$a1" "$a2" "$a3")"
        case "$got_base" in
            "$want_base "*|"$want_base")
                say "self-test OK   ${bname}: base=${want_base}" ;;
            *)
                warn "SELF-TEST FAIL ${bname}: want base ${want_base}, got '${got_base}'"
                failures=$((failures + 1)) ;;
        esac
    done

    # The negative partner. It asserts the UNPINNED branch's own marker, not
    # merely the absence of the pinned one: a derive_base that blindly echoed
    # its first argument would print "--clean-overlay", which also lacks the
    # string "pinned by --base", and would pass a bare absence check while
    # being completely wrong. Requiring "UNPINNED" means only the git branch
    # can satisfy this arm.
    got_base="$(derive_base --clean-overlay -- cargo test)"
    case "$got_base" in
        *"pinned by --base"*)
            warn "SELF-TEST FAIL unpinned run must not claim a pin: got '${got_base}'"
            failures=$((failures + 1)) ;;
        *UNPINNED*)
            say "self-test OK   unpinned run reports the working tree: ${got_base}" ;;
        *)
            warn "SELF-TEST FAIL unpinned run reached neither branch: got '${got_base}'"
            failures=$((failures + 1)) ;;
    esac

    if [ "$failures" -ne 0 ]; then
        warn "SELF-TEST FAILED: ${failures} of ${total} arms"
        return 1
    fi
    say "self-test: ${total} of ${total} arms passed"
    return 0
}

main() {
    local log="" expect="" base
    while [ $# -gt 0 ]; do
        case "$1" in
            --self-test) self_test; exit $? ;;
            --log) log="${2:-}"; shift 2 ;;
            --expect-target) expect="${2:-}"; shift 2 ;;
            --) shift; break ;;
            *) warn "unknown wrapper argument: $1"; exit 2 ;;
        esac
    done

    if [ $# -eq 0 ]; then
        warn "no rch arguments given. See the header for usage."
        exit 2
    fi
    if ! command -v rch >/dev/null 2>&1; then
        warn "rch not on PATH -- this wrapper adds nothing without it. Use rch directly."
        exit 2
    fi

    if [ -z "$log" ]; then
        log="${TMPDIR:-/tmp}/rch-run-$(date +%Y%m%dT%H%M%SZ)-$$.log"
    fi

    # Record what the verdict is bound to: the --base the run pins to when there
    # is one, and otherwise the working tree with its dirty count.
    base="$(derive_base "$@")"

    say "running: rch exec $*"
    say "log: $log"
    rch exec "$@" > "$log" 2>&1
    local run_exit=$?

    emit_verdict_block "$log" "$run_exit" "$base" "$expect" "rch exec $*"
    exit $?
}

main "$@"
