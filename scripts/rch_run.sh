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
CAPSULE_EMITTER="$HERE/lib/emit_proof_capsule.py"

warn() { printf '[rch-run] %s\n' "$*" >&2; }
say() { printf '[rch-run] %s\n' "$*"; }

# Emit an ee.release_candidate_proof.v1 SKELETON beside the log.
#
# WHY THIS FUNCTION EXISTS AT ALL: scripts/lib/emit_proof_capsule.py has shipped
# since c76508495, and its own docstring states "this is called by
# scripts/rch_run.sh". It was not. This file had zero references to it, and
# `git log -S emit_proof_capsule -- scripts/rch_run.sh` is empty across all
# history, so it was never wired rather than un-wired. A 384-line emitter that
# nothing invokes is the same defect as a byte-comparison contract that no gate
# runs (bd-byte-compared-contracts-never-executed-feftl); the docstring was
# describing an intention as if it were a call site.
#
# IT CANNOT CHANGE A VERDICT, DELIBERATELY. A capsule is a record OF a run, not
# a judgement ON one, and a reporting artifact able to turn a red green is a
# worse defect than a missing one. This returns 0 on every path and never
# touches run_exit or grade_exit.
#
# BEST-EFFORT, BUT NEVER SILENT. A missing python3 or a refusing emitter prints
# the reason, because "no capsule line at all" is indistinguishable from "never
# attempted", which is the absence-is-not-evidence failure this repo keeps
# finding. The emitter's own stderr is surfaced rather than swallowed.
emit_proof_capsule() {
    local log="$1" base="$2" host="$3" run_exit="$4" cmd="$5"
    local out="${log}.capsule.json" dirty emit_out emit_rc

    if [ ! -f "$CAPSULE_EMITTER" ]; then
        printf 'capsule      : NOT EMITTED (emitter absent at %s)\n' "$CAPSULE_EMITTER"
        return 0
    fi
    if ! command -v python3 >/dev/null 2>&1; then
        printf 'capsule      : NOT EMITTED (python3 unavailable)\n'
        return 0
    fi

    dirty="$(git status --porcelain 2>/dev/null | wc -l | tr -d ' ')"
    [ -n "$dirty" ] || dirty=0

    if [ -n "$host" ]; then
        emit_out="$(python3 "$CAPSULE_EMITTER" --log "$log" --base "$base" \
            --dirty "$dirty" --host "$host" --run-exit "$run_exit" \
            --command "$cmd" --out "$out" 2>&1)"
    else
        emit_out="$(python3 "$CAPSULE_EMITTER" --log "$log" --base "$base" \
            --dirty "$dirty" --run-exit "$run_exit" \
            --command "$cmd" --out "$out" 2>&1)"
    fi
    # Captured EXPLICITLY. A bare `$?` after an if/else reads whichever branch
    # ran last, which is correct here by accident and stops being correct the
    # moment anyone adds a line between the fi and the test.
    emit_rc=$?

    if [ "$emit_rc" -eq 0 ]; then
        printf 'capsule      : %s\n' "$out"
    else
        printf 'capsule      : NOT EMITTED (emitter exited non-zero)\n'
        printf '%s\n' "$emit_out" | sed 's/^/capsule      : /'
    fi
    return 0
}

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

    # A TERMINATOR IS AN ABSENCE, NOT A RED.
    #
    # bd-xo0fn: 137 is 128+9 (SIGKILL), 143 is 128+15 (SIGTERM). Neither means
    # "the thing you ran reported failure"; both mean it was killed with work
    # outstanding. On 2026-09-20 a capture e2e was executing assertions when the
    # 900s cap killed it at 902s, and this block said
    # `RED (non-test run exited 137)` -- attributing a terminator to the code.
    #
    # A false red costs what a false green costs, pointed the other way: it
    # sends someone into archaeology over a change that is fine, and in a swarm
    # it gets good commits reverted. One integer cannot express "failed" and
    # "was killed", so the verdict must not pick the wrong one silently.
    #
    # The run's exit status is still printed verbatim above and still dominates
    # the return code, as this file's header requires. Only the VERDICT changes.
    case "$run_exit" in
        137|143)
            local signame='SIGTERM'
            [ "$run_exit" -eq 137 ] && signame='SIGKILL'
            printf 'verdict      : UNGRADEABLE (terminated by %s; exit %s is a kill, not a failure)\n' \
                "$signame" "$run_exit"
            if grep -qaiE 'timed? ?out|timeout|SIGKILL|killed' "$log" 2>/dev/null; then
                printf '  [terminated] log carries a kill/timeout marker -- compare elapsed time to the cap before blaming the code.\n'
            fi
            printf '===== END VERDICT BLOCK =====\n'
            warn "run was terminated (${signame}); that is an absence, not a red. Re-run with a larger cap or a bigger worker."
            return "$run_exit"
            ;;
    esac

    if [ ! -f "$GRADER" ] || ! command -v python3 >/dev/null 2>&1; then
        printf 'verdict      : UNGRADEABLE (grader unavailable)\n'
        printf '===== END VERDICT BLOCK =====\n'
        warn "grader missing or python3 absent -- NOT claiming a verdict."
        [ "$run_exit" -ne 0 ] && return "$run_exit"
        return 1
    fi

    # A COMPILE-ONLY RUN HAS NO TEST ANNOUNCEMENTS BY CONSTRUCTION.
    #
    # `cargo test --no-run` builds and stops; libtest never starts, so it never
    # prints `running N tests`. Grading that log on announcements demands
    # evidence the log cannot contain, and the wrapper then reds a run that did
    # exactly what was asked.
    #
    # MEASURED 2026-09-19, against a known positive first so that "not green"
    # was not vacuous: the grader returns 0 for a log with a real
    # announcement/summary pair, and 1 for every --no-run shape -- a clean
    # compile, a compile that failed with E0425, and a target-not-found. So the
    # defect is a FALSE RED and never a false green, in either direction, and
    # nothing already graded needs revisiting.
    #
    # It is still worth fixing. A gate that reds on correct input is training
    # data for ignoring the gate, which costs precisely what a gate that cannot
    # fail costs, arriving from the other side.
    #
    # Graded on BUILD evidence instead, and STILL FAILING CLOSED: exit 0 with no
    # `Finished` line is not a pass.
    # THE PREDICATE IS "DOES THIS COMMAND PRODUCE TEST ANNOUNCEMENTS", NOT
    # "IS IT --no-run". The first version of this fix matched `--no-run`, which
    # named a SITUATION; the property is that libtest never starts, and that is
    # equally true of `cargo clippy`, `cargo check`, `cargo build`, `cargo fmt`
    # and any `--job` shell command. Three probes on hz4 were graded RED that
    # way in one sitting -- each exited 0 and did exactly what was asked -- and
    # a grader that reds three valid runs will red the next ten.
    #
    # A fix that names the situation does not transfer. This one names the
    # property: announcements are expected ONLY from a command that actually
    # runs tests, which means `cargo test` WITHOUT `--no-run`. A `--job` whose
    # shell body itself invokes `cargo test` still matches, and should, because
    # that run really does produce announcements.
    # THE LOG DECIDES FIRST, THE COMMAND ONLY BREAKS THE TIE.
    #
    # Keying purely on the command string was my first attempt and it is
    # unsafe: any test invocation the match does not recognise -- `cargo
    # nextest`, a wrapper script, a filter spelled unusually -- would be graded
    # on EXIT STATUS ALONE, which is a false GREEN. My own self-test arms
    # caught it, because they pass a placeholder command with real
    # announcement-bearing logs and were suddenly graded the wrong way.
    #
    # So: if the log CONTAINS announcements, grade them, whatever the command
    # claims. Only when there are none does the command decide whether that
    # absence is expected (clippy, check, --no-run, a shell job) or damning (a
    # test run that announced nothing, which is the "nothing ran" case this
    # wrapper was built for).
    local has_announcements=false
    if grep -qaE '^[[:space:]]*running [0-9]+ tests?' "$log" 2>/dev/null; then
        has_announcements=true
    fi
    local expects_announcements=false
    case "$cmd" in
        *--no-run*) expects_announcements=false ;;
        *"cargo test"*) expects_announcements=true ;;
        *) expects_announcements=false ;;
    esac

    if [ "$has_announcements" = false ] && [ "$expects_announcements" = false ]; then
        # INFRASTRUCTURE FAILURE OUTRANKS EXIT 0, ON EVERY PATH.
        #
        # The non-test path never calls the grader, so without this it would
        # claim GREEN for a log full of `rsync: connection unexpectedly closed`
        # and `RCH-E104 SSH command timed out` merely because the wrapper
        # reported 0. That is a false green, and it is the exact regression the
        # pre-existing "UNGRADEABLE log, run exited 0" arm was written to catch
        # -- it caught mine.
        #
        # Widening a gate is where false greens get introduced: the new path
        # skips the checks the old path did, and nothing says so out loud.
        if grep -qaE 'RCH-E[0-9]{3}|^rsync: |connection unexpectedly closed' "$log" 2>/dev/null; then
            printf 'verdict      : UNGRADEABLE (infrastructure failure in the log)\n'
            printf '===== END VERDICT BLOCK =====\n'
            warn "log carries RCH/rsync failure markers; refusing to call this a pass despite exit ${run_exit}."
            [ "$run_exit" -ne 0 ] && return "$run_exit"
            return 1
        fi
        if [ "$run_exit" -ne 0 ]; then
            printf 'verdict      : RED (non-test run exited %s)\n' "$run_exit"
            printf '===== END VERDICT BLOCK =====\n'
            warn "non-test run exited ${run_exit}; that is the fact of record."
            return "$run_exit"
        fi
        # STRIP ANSI BEFORE ANCHORING. Cargo colourises, so `Finished` arrives
        # as ESC[0m ESC[1;32mFinished and `^ *Finished` never matches. This
        # exact blindness cost this lane a false "0 errors, exit 101" reading
        # earlier today against `^error`, and the first draft of THIS fix
        # reproduced it -- the self-test fixture had no escape codes, so it
        # passed while the real log reddened. A fixture that omits the medium's
        # noise validates nothing.
        local esc plain
        esc=$'\033'
        plain="$(sed -e "s/${esc}\[[0-9;]*m//g" "$log" 2>/dev/null)"
        # A RECORDED NON-ZERO EXIT OUTRANKS THE WRAPPER'S OWN EXIT 0.
        #
        # bd-k67bp: a shell payload's exit status is its LAST command's, so
        #     ...; ./scripts/e2e_capture.sh; echo "E2E_CAPTURE_EXIT=$?"
        # hands back the echo's 0 however the script ended. On 2026-09-20 that
        # produced `verdict: GREEN (compile-only)` for a log whose payload read
        # "refusing to run against a binary that does not exist" and
        # E2E_CAPTURE_EXIT=2. The operator wrote that echo precisely to preserve
        # the status the shell was about to discard -- the careful move -- and
        # was graded green anyway.
        #
        # So when the log records a non-zero exit for a named step, believe the
        # log over the wrapper's status. This only ever makes GREEN harder to
        # obtain: the arm is reachable solely when run_exit is already 0.
        #
        # It also catches the misclassification underneath: a run that built AND
        # THEN RAN something is not compile-only, and grading it on build
        # evidence answers a question nobody asked.
        local recorded_failure
        recorded_failure="$(printf '%s\n' "$plain" \
            | grep -aoE '^[A-Z][A-Z0-9_]*_EXIT=[1-9][0-9]*' | head -1)"
        if [ -n "$recorded_failure" ]; then
            printf 'verdict      : RED (%s recorded in the log; wrapper exit 0 was masked by a trailing command)\n' \
                "$recorded_failure"
            printf '===== END VERDICT BLOCK =====\n'
            warn "log records ${recorded_failure}; refusing to call this a pass despite exit 0."
            return 1
        fi
        if printf '%s\n' "$plain" | grep -qa '^[[:space:]]*Finished '; then
            local built
            built="$(printf '%s\n' "$plain" | grep -ca '^[[:space:]]*Executable ' 2>/dev/null || true)"
            printf '  [compile-only] Finished present; %s executable(s) built; no tests expected.\n' "${built:-0}"
            printf 'verdict      : GREEN (compile-only; graded on build evidence)\n'
            printf '===== END VERDICT BLOCK =====\n'
            return 0
        fi
        # NO `Finished`. What that means depends on whether cargo was even
        # involved, and collapsing the two is what made the first version of
        # this branch red three valid shell probes.
        #
        # A CARGO command that exits 0 without reporting Finished did not
        # complete a build, so it still fails closed -- that arm is unchanged.
        # A NON-CARGO command (a `--job` shell body, a script) has no reason to
        # emit Finished, and demanding it would be asking the log for evidence
        # it cannot contain: the same error one level down from asking a
        # compile-only run for test announcements.
        case "$cmd" in
            *cargo*)
                printf 'verdict      : RED (cargo run exited 0 with no Finished line)\n'
                printf '===== END VERDICT BLOCK =====\n'
                warn "cargo run exited 0 but never reported Finished -- refusing to call this a pass."
                return 1
                ;;
            *)
                # Exit status is the ONLY evidence here, and the block says so
                # rather than implying the log was examined and approved.
                printf '  [non-test] no cargo build or test output in this log; exit status is the only evidence.\n'
                printf 'verdict      : GREEN (graded on exit status alone)\n'
                printf '===== END VERDICT BLOCK =====\n'
                return 0
                ;;
        esac
    fi

    if [ -n "$expect" ]; then
        grade_out="$(python3 "$GRADER" --expect-target "$expect" "$log" 2>&1)"
    else
        grade_out="$(python3 "$GRADER" "$log" 2>&1)"
    fi
    grade_exit=$?

    printf '%s\n' "$grade_out" | sed 's/^/  /'
    # THE LEAD CAN BE MISREAD, and that is not the grader's fault. `run exit :
    # 0` sits several lines above the refusal text, so a reader skimming for an
    # exit code finds a 0 and never reaches the word RED. The grader is honest;
    # its output simply buries the conclusion. State the wrapper's own verdict.
    if [ "$run_exit" -ne 0 ]; then
        printf 'verdict      : RED (run exited %s)\n' "$run_exit"
    elif [ "$grade_exit" -ne 0 ]; then
        printf 'verdict      : RED (run exited 0; the log does not grade green)\n'
    else
        printf 'verdict      : GREEN\n'
    fi
    # Emitted INSIDE the block, because the block is what gets pasted into a
    # bead and a capsule nobody can find is a capsule nobody checks. Only on the
    # fully-graded path: the nine earlier exit paths return before this, and a
    # capsule bound to an UNGRADEABLE run would assert identities for a run that
    # established nothing.
    emit_proof_capsule "$log" "$base" "$host" "$run_exit" "$cmd"
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

    # Compile-only fixtures. A `--no-run` log has no announcement by
    # construction, so these are graded on build evidence instead.
    printf '   Compiling eidetic-engine v0.14.4 (/data/rch/eidetic_engine_cli)\n    Finished `test` profile [unoptimized + debuginfo] target(s) in 11m 34s\n  Executable tests/suites/integration_s_z.rs (target/debug/deps/integration_s_z-abc)\n' > "$tmp/norun_ok.log"
    printf '   Compiling eidetic-engine v0.14.4 (/data/rch/eidetic_engine_cli)\nerror[E0425]: cannot find function `ensure_command_success` in this scope\nerror: could not compile `eidetic-engine` (test "integration_s_z") due to 1 previous error\n' > "$tmp/norun_fail.log"
    printf '   Compiling eidetic-engine v0.14.4 (/data/rch/eidetic_engine_cli)\n' > "$tmp/norun_nofinish.log"
    # A --job shell body: no cargo lines, no announcements, nothing to grade
    # but the exit status. This is what the hz4 probes looked like.
    printf 'PROBE host=hz4\n  holder ready=yes\n  REAL SCRIPT -> exit=75\nPROBE done\n' > "$tmp/shelljob.log"
    # COLOURISED, as cargo actually emits it. Without this arm the anchored
    # match passes on clean fixtures and fails on every real log.
    printf '\033[1m\033[32m   Compiling\033[0m eidetic-engine v0.14.4\n\033[1m\033[32m    Finished\033[0m `test` profile [unoptimized + debuginfo] target(s) in 11m 34s\n\033[1m\033[32m  Executable\033[0m tests/suites/integration_s_z.rs (target/debug/deps/integration_s_z-abc)\n' > "$tmp/norun_ansi.log"
    # bd-k67bp, transcribed from the real hz3 log that this wrapper graded
    # GREEN: a build that Finished, then a payload that refused to run, whose
    # status survives only as a sentinel because a trailing echo ate the exit.
    # Colourised, because the real one was.
    printf '\033[1m\033[32m    Finished\033[0m `dev` profile [unoptimized + debuginfo] target(s) in 12m 30s\ne2e_capture: ee_binary=/x/target/debug/ee (missing or not executable)\ne2e_capture: refusing to run against a binary that does not exist.\nE2E_CAPTURE_EXIT=2\n' > "$tmp/masked_exit.log"
    # THE KNOWN POSITIVE. Same shape, same sentinel, exit 0. If this reds, the
    # guard is not detecting failure, it is just refusing logs that mention an
    # exit -- and a guard that blocks everything proves nothing.
    printf '\033[1m\033[32m    Finished\033[0m `dev` profile [unoptimized + debuginfo] target(s) in 12m 30s\ne2e_capture: 41 assertions passed\nE2E_CAPTURE_EXIT=0\n' > "$tmp/masked_exit_zero.log"

    # name | log | run_exit | expect-target | want wrapper exit | command
    # The command matters: --no-run in it selects the build-evidence path, so
    # these arms exercise the DETECTION as well as the grading.
    local -a cases=(
        "green run, graded green|$tmp/green.log|0||0|"
        "green run, log shows failures|$tmp/red.log|0||1|"
        "green run, ZERO tests executed|$tmp/zero.log|0||1|"
        "green run, nested child summary|$tmp/nested.log|0||1|"
        "green run, two targets, ambiguous|$tmp/multi.log|0||1|"
        "green run, two targets, disambiguated|$tmp/multi.log|0|contracts-b|0|"
        "UNGRADEABLE log, run exited 0|$tmp/ungradeable.log|0||1|"
        "run FAILED: its exit code dominates|$tmp/green.log|101||101|"
        "log missing entirely|$tmp/does-not-exist.log|0||1|"
        "--no-run that BUILT is green|$tmp/norun_ok.log|0||0|cargo test --test x --no-run"
        "--no-run that FAILED keeps its code|$tmp/norun_fail.log|101||101|cargo test --test x --no-run"
        "--no-run, exit 0, no Finished: fails closed|$tmp/norun_nofinish.log|0||1|cargo test --test x --no-run"
        "a --no-run log graded as a TEST run still reds|$tmp/norun_ok.log|0||1|cargo test --test x"
        "--no-run, COLOURISED as cargo really emits|$tmp/norun_ansi.log|0||0|cargo test --test x --no-run"
        # THE CLASS, not the situation. Every command below produces no test
        # announcements for the same reason --no-run does: libtest never runs.
        # Each was graded RED before this widened (three hz4 probes in one
        # sitting), which is why they are arms and not assumptions.
        "clippy that BUILT is green|$tmp/norun_ok.log|0||0|cargo clippy --all-targets -- -D warnings"
        "cargo check that BUILT is green|$tmp/norun_ok.log|0||0|cargo check --locked --all-targets"
        "a --job shell run with no cargo output is green on exit 0|$tmp/shelljob.log|0||0|--job -- bash -c 'echo probe'"
        "a --job shell run that FAILED keeps its code|$tmp/shelljob.log|1||1|--job -- bash -c 'exit 1'"
        "a CARGO run with no Finished still fails closed|$tmp/norun_nofinish.log|0||1|cargo check --locked"
        "a --job whose body runs cargo test IS graded on announcements|$tmp/green.log|0||0|--job -- bash -c 'cargo test --lib'"
        "...and that same --job reds when its announcements do not reconcile|$tmp/zero.log|0||1|--job -- bash -c 'cargo test --lib'"
        # bd-k67bp: a recorded non-zero exit outranks a masked wrapper 0.
        "a masked non-zero exit reds despite Finished and exit 0|$tmp/masked_exit.log|0||1|--job -- bash -c './scripts/e2e_capture.sh; echo EXIT=\$?'"
        "...and the same shape with EXIT=0 is still green|$tmp/masked_exit_zero.log|0||0|--job -- bash -c './scripts/e2e_capture.sh; echo EXIT=\$?'"
        # bd-xo0fn: a kill is an absence. The exit code still dominates the
        # return value; it is the VERDICT that must not say RED.
        "SIGKILL is UNGRADEABLE, not RED|$tmp/green.log|137||137|--job -- bash -c './x.sh'|UNGRADEABLE (terminated by SIGKILL"
        "SIGTERM is UNGRADEABLE, not RED|$tmp/green.log|143||143|--job -- bash -c './x.sh'|UNGRADEABLE (terminated by SIGTERM"
        # KNOWN POSITIVE: an ordinary non-zero exit is still a red, so the arms
        # above discriminate on the signal instead of excusing every failure.
        "an ordinary non-zero exit is still RED|$tmp/shelljob.log|75||75|--job -- bash -c './x.sh'|RED (non-test run exited 75)"
        # bd-k67bp's arms, now pinned on wording too.
        "a masked non-zero exit names the sentinel|$tmp/masked_exit.log|0||1|--job -- bash -c 'x; echo EXIT=\$?'|RED (E2E_CAPTURE_EXIT=2 recorded in the log"
    )
    # THE EXIT CODE ALONE CANNOT SEE A VERDICT CHANGE.
    #
    # An arm that only compares $? is blind to the thing most of these arms
    # exist to pin. bd-xo0fn's fix turns `RED (non-test run exited 137)` into
    # `UNGRADEABLE (terminated by SIGKILL...)` while BOTH paths return 137, so
    # its first three arms passed identically before and after the repair --
    # green on both sides, which is not evidence of anything. The optional 7th
    # field pins a substring of the verdict line, so an arm that cares about the
    # wording can fail on the wording.
    local entry name log rexit expect want cmd wantverdict out
    for entry in "${cases[@]}"; do
        IFS='|' read -r name log rexit expect want cmd wantverdict <<< "$entry"
        out="$(emit_verdict_block "$log" "$rexit" "self-test" "$expect" "${cmd:-self-test}" 2>&1)"
        got=$?
        if [ "$got" -ne "$want" ]; then
            warn "SELF-TEST FAIL ${name}: want ${want}, got ${got}"
            failures=$((failures + 1))
        elif [ -n "${wantverdict:-}" ] && ! printf '%s\n' "$out" | grep -qaF "$wantverdict"; then
            warn "SELF-TEST FAIL ${name}: exit ${got} as expected, but verdict did not contain '${wantverdict}'"
            warn "  got: $(printf '%s\n' "$out" | grep -a 'verdict' | head -1)"
            failures=$((failures + 1))
        else
            say "self-test OK   ${name}: want ${want}, got ${got}${wantverdict:+ (verdict pinned)}"
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
