#!/usr/bin/env python3
"""Grade a cargo test log by reconciling each announcement with its summary.

bd-6g6s2. A log can contain more than one libtest summary, and nothing in a
summary line says which process or which target emitted it:

    test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 10060 filtered out

TWO WAYS THAT HAPPENS, and a repair for either one alone leaves the other:

  NESTED   a test re-executes its own binary via current_exe() with --exact.
           src/core/backup.rs:25986 does this with `.status()`, which INHERITS
           stdio, so the child's `running 1 test` and `test result: ok.` land in
           the parent's log on every run. (orient.rs:2771 and index.rs:3982 do
           the same spawn but with `.output()`, so their child output is
           captured and only surfaces on failure.)

  SEQUENTIAL  one invocation runs several targets, or one script runs several
           invocations into one log. scripts/verify.sh makes 3 `cargo test`
           calls across 91 stages. No child process is involved and the log
           still holds several summaries.

`grep 'test result' <log> | tail -1` is wrong for both. It can return a child's
summary while the parent is still running, and it silently discards every other
summary in the file -- so a target that failed can be graded green by a line a
different process wrote.

THE DISCRIMINATOR libtest hands you for free: it announces `running N tests`
BEFORE it runs. A summary belongs to that announcement only when
passed + failed + ignored == N. This script pairs them in order and refuses a
verdict when they do not reconcile.

IT ALSO CAPTURES WHICH TARGET each pair belongs to, from cargo's
`Running unittests src/lib.rs (target/debug/deps/ee-<hash>)` lines. That is the
thing bd-blj5n's close had to quote BY HAND to prove its target executed; doing
it here is the point of having a tool at all.

THIS FAILS CLOSED, unlike scripts/check-format.sh which fails open. The
difference is deliberate: a formatter that cannot run should not block a commit,
but a GRADER that cannot tell you what happened must never answer "green". Every
uncertain state here is non-zero.

EXIT CODES
  0  exactly one target verdict, it reconciles, and it passed
  1  a verdict of NOT-GREEN: a failure, a mismatch, a missing summary, or
     AMBIGUITY (more than one target) with no --expect-target to disambiguate
  2  the log could not be read at all

USAGE
  grade_test_log.py <logfile>
  grade_test_log.py --expect-target lib <logfile>   # pick one target by substring
  grade_test_log.py --all-targets <logfile>         # grade EVERY target together
  grade_test_log.py --require-targets a,b <logfile> # fail if a or b never reported
  grade_test_log.py --expect-total contracts=1510 <logfile>
                                                    # fail unless that target
                                                    # HOLDS 1510 tests; the only
                                                    # tell for stale source
  grade_test_log.py --self-test
"""

from __future__ import annotations

import pathlib
import re
import sys
import tempfile

ANNOUNCE = re.compile(r"^\s*running (\d+) tests?\s*$")
SUMMARY = re.compile(
    r"^\s*test result:\s*(?P<verdict>\w+)\.\s*"
    r"(?P<passed>\d+) passed;\s*(?P<failed>\d+) failed;\s*"
    r"(?P<ignored>\d+) ignored"
    # `measured` and `filtered out` are captured so the target's TOTAL test count
    # can be checked (bd-9z8ee). Both groups are OPTIONAL on purpose: the prefix
    # above is the part every libtest version prints, and a stricter regex would
    # stop matching summaries entirely rather than fail loudly -- turning a
    # grader into one that reports "nothing ran", which is the worst available
    # failure mode for this file.
    r"(?:;\s*(?P<measured>\d+) measured)?"
    r"(?:;\s*(?P<filtered>\d+) filtered out)?"
)
RUNNING_TARGET = re.compile(r"^\s*Running (?:unittests )?(?P<what>\S+)\s*\((?P<bin>[^)]+)\)")
DOCTEST_TARGET = re.compile(r"^\s*Doc-tests\s+(?P<what>\S+)")


def strip_ansi(text: str) -> str:
    return re.sub(r"\x1b\[[0-9;]*m", "", text)


def parse(lines: list[str]) -> tuple[list[dict], list[str], list[str]]:
    """Return (pairs, problems, announced_targets).

    `announced_targets` is every target that printed a `Running` line, in order.
    It is reported separately from the pairs because pair ATTRIBUTION can be
    wrong while the Running lines are still a complete, reliable record of which
    targets executed -- see the note on the FIFO below.
    """
    pairs: list[dict] = []
    problems: list[str] = []
    # Targets are consumed IN ORDER, not "whichever Running line was most
    # recent". Cargo may print every `Running` line up front when one invocation
    # names several targets (see BUFFERED_TARGETS), in which case "most recent"
    # labels every summary with the LAST target -- which silently misattributes
    # the results and defeats --expect-target, because it then matches more than
    # one pair. Cargo runs targets in the order it lists them, so the Nth
    # announcement belongs to the Nth target.
    #
    # The queue is a FIFO with a STICKY TAIL: once it is exhausted, further
    # announcements keep the last target. That preserves the nested-child case,
    # where one target emits two announcements and the second is not a new
    # target at all.
    #
    # THE FIFO IS WRONG UNDER PARALLEL TARGET EXECUTION, and the failure is
    # quiet. "Cargo runs targets in the order it lists them" holds for a
    # sequential run; with `-j N` several test binaries run CONCURRENTLY and
    # their `Running` lines, announcements and summaries interleave, so the Nth
    # announcement need not belong to the Nth target. Measured 2026-10-25 on a
    # `-j 2 --no-fail-fast --test contracts --test integration_s_z --test
    # integration_g_m` run: three targets each passed one test, and the pairs
    # came out labelled `<unknown target>`, then contracts' name on
    # integration_g_m's numbers, then integration_g_m's name on
    # integration_s_z's numbers. Every pair reconciled; every label was wrong.
    #
    # This is NOT fixed here, because nothing in the log ties a summary to a
    # target under interleaving -- the information is simply absent. What is
    # fixed is that the questions which do NOT need attribution no longer ask
    # for it: `--require-targets` and `--expect-total` read announced_targets
    # below. Treat a pair's `target` as advisory on any multi-target run, and
    # prefer one target per invocation when you need per-target attribution.
    announced_targets: list[str] = []
    consumed = 0
    pending: dict | None = None

    def current_target() -> str:
        nonlocal consumed
        if not announced_targets:
            return "<unknown target>"
        index = min(consumed, len(announced_targets) - 1)
        consumed = index + 1
        return announced_targets[index]

    for number, raw in enumerate(lines, start=1):
        line = strip_ansi(raw.rstrip("\n"))

        found_target = RUNNING_TARGET.match(line) or DOCTEST_TARGET.match(line)
        if found_target:
            announced_targets.append(
                found_target.groupdict().get("bin") or found_target.group("what")
            )
            continue

        announced = ANNOUNCE.match(line)
        if announced:
            if pending is not None:
                problems.append(
                    f"line {pending['line']}: announcement of {pending['announced']} "
                    f"test(s) on target {pending['target']} never reached a summary "
                    f"-- the run was cut off, or its summary is missing"
                )
            pending = {
                "announced": int(announced.group(1)),
                "target": current_target(),
                "line": number,
            }
            continue

        summarised = SUMMARY.match(line)
        if summarised:
            counted = (
                int(summarised.group("passed"))
                + int(summarised.group("failed"))
                + int(summarised.group("ignored"))
            )
            if pending is None:
                problems.append(
                    f"line {number}: summary with no preceding announcement "
                    f"({counted} test(s), target "
                    f"{announced_targets[-1] if announced_targets else '<unknown target>'})"
                    f" -- cannot be attributed"
                )
                continue
            pairs.append(
                {
                    "target": pending["target"],
                    "announced": pending["announced"],
                    "counted": counted,
                    "verdict": summarised.group("verdict"),
                    "passed": int(summarised.group("passed")),
                    "failed": int(summarised.group("failed")),
                    # `counted` deliberately includes IGNORED, because that is what
                    # reconciles against libtest's announcement. Kept separately so a
                    # reader can tell announced-and-EXECUTED from announced-and-SKIPPED:
                    # an ignored test reconciles perfectly and proves nothing.
                    "ignored": int(summarised.group("ignored")),
                    # THE TARGET'S WHOLE TEST COUNT (bd-9z8ee). `counted` is what
                    # this run executed; `total` is how many tests the BINARY
                    # holds, which is the one number a stale source tree cannot
                    # fake. A repeat `rch exec --job` to the same pinned worker
                    # can run an earlier dispatch's source: the announcement and
                    # the summary then reconcile PERFECTLY, because both are
                    # honest about code that is no longer on disk, and every
                    # other rule in this file passes. The total cannot stay the
                    # same across a commit that added a test.
                    #
                    # `filtered` defaults to 0 when libtest did not print it, so
                    # `total` degrades to `counted` rather than to None.
                    "filtered": int(summarised.group("filtered") or 0),
                    "total": counted + int(summarised.group("filtered") or 0),
                    "line": number,
                    "announce_line": pending["line"],
                }
            )
            pending = None

    if pending is not None:
        problems.append(
            f"line {pending['line']}: announcement of {pending['announced']} test(s) "
            f"on target {pending['target']} never reached a summary "
            f"-- the run was cut off, or its summary is missing"
        )
    return pairs, problems, announced_targets


def grade(
    path: pathlib.Path,
    expect_target: str | None,
    all_targets: bool = False,
    require_targets: list[str] | None = None,
    expect_totals: dict[str, int] | None = None,
) -> int:
    try:
        lines = path.read_text(errors="replace").splitlines()
    except OSError as error:
        print(f"[grade-test-log] cannot read {path}: {error}", file=sys.stderr)
        return 2

    pairs, problems, announced_targets = parse(lines)

    # Always print every pair. A grade whose denominator is invisible is the
    # failure this tool exists to remove.
    print(f"[grade-test-log] {path}")
    if not pairs and not problems:
        print("  NO test announcements and NO summaries -- nothing ran.")
        return 1
    for pair in pairs:
        reconciles = pair["announced"] == pair["counted"]
        mark = "OK " if reconciles else "MISMATCH"
        # Surface IGNORED in the denominator line. It reconciles against the
        # announcement exactly like a pass, so a reader scanning for "OK" cannot
        # otherwise tell that some of those tests never ran.
        skipped = f" IGNORED={pair['ignored']}" if pair.get("ignored") else ""
        print(
            f"  {mark} target={pair['target']} announced={pair['announced']} "
            f"counted={pair['counted']}{skipped} total={pair['total']} "
            f"verdict={pair['verdict']} "
            f"(announce line {pair['announce_line']}, summary line {pair['line']})"
        )
    for problem in problems:
        print(f"  PROBLEM {problem}")

    if problems:
        print("[grade-test-log] REFUSING a verdict: the log is not self-consistent.")
        return 1

    unreconciled = [p for p in pairs if p["announced"] != p["counted"]]
    if unreconciled:
        print(
            "[grade-test-log] REFUSING a verdict: an announcement and its summary "
            "disagree, so the summary belongs to something else."
        )
        return 1

    # A TARGET THAT NEVER REPORTED CANNOT FAIL ANY CHECK ABOVE (bd-mbupe). Every
    # rule in this file judges the announcement/summary pairs that are PRESENT, so
    # a target cargo never reached contributes no pair and is indistinguishable
    # from one that was never asked for. scripts/verify.sh's own stage diagnostic
    # claims this grader catches "a target that never reported"; until this block
    # it did not, and that clause was the only defence named against the gap.
    #
    # MEASURED COST: the K3 catalog gate sits in a `--tests` target behind the lib
    # target in verify.sh's cargo line, which carries no --no-fail-fast. Cargo
    # stops at the first failing target, so K3 never ran and
    # docs/degraded_codes.md drifted for three days (607 of 609 codes) with a
    # correct six-arm gate watching it.
    #
    # Checked against EVERY pair in the log rather than against `chosen`: the
    # question is whether the target reported AT ALL, which is independent of
    # --expect-target narrowing. Substring match, for the same reason
    # --expect-target is a substring -- the binary name carries a build hash.
    #
    # ORDERED BEFORE the failing-target check deliberately. The target that stops
    # a fail-fast run is usually one with CHRONIC failures, so reporting the
    # failure first and returning would hide the missing targets behind a red
    # that is already expected and already triaged -- permanently, since it is
    # chronic. The per-pair lines above still print every verdict, so nothing is
    # lost by reporting the structural omission first.
    if require_targets:
        # Checked against the `Running` lines, NOT against pair["target"]. A
        # pair's label is attribution and can be wrong under parallel target
        # execution (see parse); a Running line is a direct statement by cargo
        # that the target executed. Measured 2026-10-25: reading pair labels
        # here produced a FALSE RED -- it reported integration_s_z as never
        # having run on a log that contains its Running line and its passing
        # summary.
        reported = list(announced_targets)
        missing = [
            want for want in require_targets
            if not any(want in target for target in reported)
        ]
        if missing:
            print(
                f"[grade-test-log] NOT GREEN: {len(missing)} required target(s) "
                f"produced no `Running` line: {', '.join(missing)}"
            )
            print(
                "  targets that DID report: "
                f"{', '.join(reported) if reported else '(none)'}"
            )
            print(
                "  A target that never ran reports nothing, so no other rule here "
                "can see it. Cargo stops at the first failing target unless "
                "--no-fail-fast is passed."
            )
            return 1

    # THE TOTAL IS THE ONLY TELL FOR STALE SOURCE (bd-9z8ee), and it is the only
    # false-green class in this repo that survives announcement pairing. A repeat
    # `rch exec --job` to the same pinned worker can execute an EARLIER
    # dispatch's tree: exit 0, a `Running` line proving the target executed, and
    # an announcement and summary that reconcile exactly -- because both are
    # honest about source that is no longer on disk. Measured 2026-10-25 across
    # three consecutive dispatches: same test binary hash, same total, and
    # `-- --list` showing 8 of the file's 9 tests.
    #
    # `--job` cannot be audited any other way: it refuses both
    # --source-content-receipt and --clean-overlay, so the transferred set can
    # be neither declared nor checked. What remains is arithmetic -- the target's
    # whole test count cannot stay the same across a commit that added a test.
    #
    # FAILS CLOSED ON AN ABSENT TARGET. Naming a target that produced no summary
    # must be a refusal, never a silent pass: "the total I demanded was never
    # reported" and "the total matched" are opposite outcomes, and a flag whose
    # typo'd target quietly asserts nothing is worse than no flag.
    # TWO CLAIMS, NEITHER OF WHICH USES PAIR ATTRIBUTION. The target must have
    # printed a `Running` line, and SOME pair in the log must report the
    # expected total. That is deliberately weaker than "this target's pair holds
    # N", and the weakening is forced: under parallel target execution nothing
    # in the log ties a summary to a target, so the stronger claim is not
    # answerable and asking it produces a confident wrong answer rather than an
    # honest one. It still does this flag's job -- if a binary that should hold
    # N tests holds N-1 because the worker built an older tree, no pair reports
    # N and this fails. Use one target per invocation when you need the total
    # bound to a specific target.
    if expect_totals:
        totals_seen = sorted({p["total"] for p in pairs})
        for want_target, want_total in expect_totals.items():
            if not any(want_target in target for target in announced_targets):
                print(
                    f"[grade-test-log] NOT GREEN: --expect-total named "
                    f"{want_target!r}, which printed no `Running` line in this log. "
                    f"Targets that did: "
                    f"{', '.join(announced_targets) if announced_targets else '(none)'}"
                )
                return 1
            if want_total not in totals_seen:
                print(
                    f"[grade-test-log] NOT GREEN: expected a target holding "
                    f"{want_total} test(s) for {want_target!r}; the totals reported "
                    f"in this log are {totals_seen}."
                )
                print(
                    "  A target's total cannot change unless its SOURCE changed. "
                    "If you added a test and the total did not move, the worker "
                    "built an older tree (bd-9z8ee) and this run's green belongs "
                    "to code that is no longer on disk."
                )
                return 1
        print(
            "[grade-test-log] totals confirmed: "
            + ", ".join(f"{k}={v}" for k, v in expect_totals.items())
        )

    chosen = pairs
    if expect_target is not None:
        chosen = [p for p in pairs if expect_target in p["target"]]
        if not chosen:
            print(
                f"[grade-test-log] REFUSING a verdict: no target matched "
                f"--expect-target {expect_target!r}."
            )
            return 1

    if all_targets:
        # AGGREGATE MODE (bd-reality-core-convergence-1azkt.5, bullet 4). The
        # single-target path below refuses a multi-target log outright, which is
        # right when you are proving ONE target ran -- and it is why this grader
        # could not be pointed at `scripts/verify.sh`'s
        # `cargo test --workspace --lib --bins --tests --examples`, whose whole
        # job is to report many targets. Refusing for AMBIGUITY there would be a
        # red that says nothing about the code.
        #
        # Here every pair has already been reconciled above, so what remains is
        # to judge all of them together. ZERO IS CHECKED ON THE TOTAL, not per
        # pair: a target that legitimately announces `running 0 tests` (an empty
        # harness) is normal, while a whole invocation that announced nothing
        # anywhere is the vacuous green this tool exists to refuse.
        failing = [p for p in chosen if p["verdict"] != "ok" or p["failed"] != 0]
        total_announced = sum(p["announced"] for p in chosen)
        if failing:
            for p in failing:
                print(
                    f"[grade-test-log] NOT GREEN: target={p['target']} "
                    f"verdict={p['verdict']} failed={p['failed']}"
                )
            return 1
        if total_announced == 0:
            print(
                f"[grade-test-log] NOT GREEN: {len(chosen)} summary/summaries and "
                "ZERO tests announced in total. The invocation executed nothing -- "
                "a filter that matched no test, or every target skipped. libtest "
                "calls that `ok` and exits 0; it is not a pass."
            )
            return 1
        # AGGREGATE FORM OF THE IGNORED VACUITY. Checked on the TOTAL, like the zero
        # rule above: one target whose tests are all ignored beside a target that really
        # ran is normal and must stay green, while an invocation that ignored everything
        # it announced executed nothing at all.
        total_executed = sum(p["passed"] + p["failed"] for p in chosen)
        total_ignored = sum(p["ignored"] for p in chosen)
        if total_executed == 0 and total_ignored > 0:
            print(
                f"[grade-test-log] NOT GREEN: {total_announced} test(s) announced and "
                f"ALL {total_ignored} IGNORED across {len(chosen)} target(s). Ignored "
                "tests reconcile against their announcements and prove nothing."
            )
            return 1
        skipped = f", {total_ignored} IGNORED" if total_ignored else ""
        print(
            f"[grade-test-log] GREEN: {len(chosen)} target(s), "
            f"{total_announced} tests announced{skipped}, all reconciled."
        )
        return 0

    if len(chosen) > 1:
        print(
            f"[grade-test-log] REFUSING a verdict: {len(chosen)} targets reported "
            "summaries in this log. Name one with --expect-target; do NOT take the "
            "last. This is the nested-child and multi-target case both."
        )
        return 1

    only = chosen[0]
    # A run that executed NOTHING is not a pass. libtest reports
    # `running 0 tests` / `test result: ok. 0 passed` with exit status 0, and
    # 0 reconciles with 0, so every other check in this file waves it through.
    # That is precisely the vacuous green this tool exists to refuse: a filter
    # that matches no test (`--exact` with a short name is the common way) is
    # indistinguishable from a passing run by exit code, by verdict, and by
    # denominator. Only the announced count tells you, and it has to be read
    # as a special case.
    if only["announced"] == 0:
        print(
            f"[grade-test-log] NOT GREEN: target={only['target']} announced ZERO tests. "
            "The run executed nothing -- a filter that matched no test, or a target "
            "that was skipped. libtest calls this `ok` and exits 0; it is not a pass."
        )
        return 1
    # THE SAME VACUITY, ONE STEP LATER. `counted` includes IGNORED, so a target whose
    # every announced test is `#[ignore]`d reconciles perfectly and reads `ok`, exactly
    # like the zero-announced case above. Measured on a real log: a bead's PRIMARY
    # acceptance arm was `#[ignore]`d "requires the real potion-multilingual-128M
    # fixture" while the verdict line said GREEN, and only the per-test rows showed it.
    # Announcing tests and running none of them is not a pass.
    if only["passed"] == 0 and only["failed"] == 0 and only["ignored"] > 0:
        print(
            f"[grade-test-log] NOT GREEN: target={only['target']} announced "
            f"{only['announced']} test(s) and IGNORED all {only['ignored']} of them. "
            "Nothing executed; an ignored test reconciles against its announcement and "
            "proves nothing. Provide the fixture it needs, or cite a different target."
        )
        return 1
    if only["verdict"] != "ok" or only["failed"] != 0:
        print(
            f"[grade-test-log] NOT GREEN: target={only['target']} "
            f"verdict={only['verdict']} failed={only['failed']}"
        )
        return 1
    print(
        f"[grade-test-log] GREEN: target={only['target']} "
        f"{only['passed']} passed, announced {only['announced']}, reconciled."
    )
    return 0


# ---------------------------------------------------------------------------
# --self-test: a grader that cannot fail is the defect it exists to find.
# ---------------------------------------------------------------------------
CLEAN = """     Running unittests src/lib.rs (target/debug/deps/ee-aaa)
running 4 tests
test a ... ok
test result: ok. 4 passed; 0 failed; 0 ignored; 0 measured; 10 filtered out
"""

NESTED_CHILD = """     Running unittests src/lib.rs (target/debug/deps/ee-aaa)
running 1200 tests
running 1 test
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 10060 filtered out
test result: ok. 1200 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
"""

SEQUENTIAL = """     Running unittests src/lib.rs (target/debug/deps/ee-aaa)
running 4 tests
test result: ok. 4 passed; 0 failed; 0 ignored; 0 measured; 10 filtered out
     Running tests/contracts.rs (target/debug/deps/contracts-bbb)
running 7 tests
test result: ok. 7 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
"""

MISMATCH = """     Running unittests src/lib.rs (target/debug/deps/ee-aaa)
running 9 tests
test result: ok. 4 passed; 0 failed; 0 ignored; 0 measured; 10 filtered out
"""

TRUNCATED = """     Running unittests src/lib.rs (target/debug/deps/ee-aaa)
running 1472 tests
test a ... ok
"""

FAILING = """     Running unittests src/lib.rs (target/debug/deps/ee-aaa)
running 4 tests
test result: FAILED. 3 passed; 1 failed; 0 ignored; 0 measured; 10 filtered out
"""

# Verbatim shape of a real vacuous run from 2026-09-18: `--exact` with a short
# test name matched nothing, libtest reported `ok`, and the process exited 0.
# It reconciles perfectly (0 == 0), so it is the one case every other check in
# this file passes.
ZERO_TESTS = """     Running unittests src/lib.rs (target/debug/deps/ee-aaa)
running 0 tests
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 10076 filtered out
"""


# Several targets, one of which FAILED. Aggregate mode must not let a passing
# neighbour carry it: this is the case the old `tail -1` heuristic got wrong.
SEQUENTIAL_ONE_FAILED = """     Running unittests src/lib.rs (target/debug/deps/ee-aaa)
running 4 tests
test result: ok. 4 passed; 0 failed; 0 ignored; 0 measured; 10 filtered out
     Running tests/contracts.rs (target/debug/deps/contracts-bbb)
running 7 tests
test result: FAILED. 6 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out
"""

# Every target announced ZERO. Each pair reconciles (0 == 0) and libtest calls
# them all `ok`, so only the TOTAL distinguishes this from a real run.
ALL_ZERO = """     Running unittests src/lib.rs (target/debug/deps/ee-aaa)
running 0 tests
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 10076 filtered out
     Running tests/contracts.rs (target/debug/deps/contracts-bbb)
running 0 tests
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 900 filtered out
"""

# One empty harness beside a real one. This MUST be green: a target with no
# tests is normal, and failing it would make the zero check unusable in
# aggregate mode -- the arm that stops the fix over-correcting.
ONE_EMPTY_ONE_REAL = """     Running unittests src/lib.rs (target/debug/deps/ee-aaa)
running 0 tests
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
     Running tests/contracts.rs (target/debug/deps/contracts-bbb)
running 7 tests
test result: ok. 7 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
"""

# REAL CARGO OUTPUT THAT NO FIXTURE ABOVE REPRODUCES. When one `cargo test`
# invocation names several `--test` targets, cargo can print EVERY `Running`
# line up front, before any test binary has produced output -- not interleaved
# with it the way SEQUENTIAL and ONE_EMPTY_ONE_REAL assume. Taken verbatim in
# shape from .ntm/logs/b9_premise.log (`--test integration_a_d --test
# harness_hook_install_audit`), where this cost a correct verdict: 7 of 7 tests
# passed and the log was refused.
#
# Note the `filtered out` counts, which are what prove the attribution: 9 belong
# to the target with no matching tests, 756 to the one that ran the 7. Under a
# "most recent Running line" rule BOTH summaries are labelled with the LAST
# target, so --expect-target matches two pairs and cannot disambiguate -- the
# grader's own documented escape hatch stops working for the case its refusal
# message tells you to use it on.
BUFFERED_TARGETS = """     Running tests/harness_hook_install_audit.rs (target/debug/deps/harness_hook_install_audit-aaa)
     Running tests/suites/integration_a_d.rs (target/debug/deps/integration_a_d-bbb)

running 0 tests

test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 9 filtered out


running 7 tests
test daemon_start_lifecycle::one ... ok
test result: ok. 7 passed; 0 failed; 0 ignored; 0 measured; 756 filtered out
"""

# ANNOUNCED AND IGNORED IS NOT A PASS, and it is harder to see than announcing zero.
# `counted` must include IGNORED to reconcile against libtest's announcement, so a
# target whose every test is `#[ignore]`d reconciles PERFECTLY and prints `ok`.
#
# Taken from a real log: bd-lgp7z's PRIMARY acceptance arm reported
# "ignored, requires the real potion-multilingual-128M fixture" while the verdict line
# said GREEN, and only the per-test rows revealed it. The zero-announced rule below
# could not catch it, because one test WAS announced.
ALL_IGNORED = """     Running unittests src/lib.rs (target/debug/deps/ee-aaa)
running 1 test
test needs_a_506mb_model ... ignored, requires the real fixture
test result: ok. 0 passed; 0 failed; 1 ignored; 0 measured; 8 filtered out
"""

# NEGATIVE CONTROL for the rule above, and the reason it is checked on the TOTAL in
# aggregate mode. One target skipping everything beside a target that really ran is
# normal -- a fixture-gated suite next to a live one -- and must stay GREEN, or the
# rule is unusable on any repo that has an `#[ignore]` anywhere.
ONE_IGNORED_ONE_REAL = """     Running unittests src/lib.rs (target/debug/deps/ee-aaa)
running 1 test
test needs_a_506mb_model ... ignored, requires the real fixture
test result: ok. 0 passed; 0 failed; 1 ignored; 0 measured; 8 filtered out
     Running tests/contracts.rs (target/debug/deps/contracts-bbb)
running 4 tests
test really_ran ... ok
test result: ok. 4 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
"""


# PARALLEL TARGET EXECUTION, taken verbatim in shape from a real `-j 2
# --no-fail-fast --test contracts --test integration_s_z --test integration_g_m`
# run on 2026-10-25 (worker identifiers removed per AGENTS.md). Three targets,
# three passing tests, and the log CANNOT be attributed: the first announcement
# arrives BEFORE any `Running` line, so the FIFO is already off by one, and the
# three Running lines appear in a different order from the three summaries.
#
# Note what still works: the test NAMES disambiguate perfectly
# (failure_mode_fixtures:: is contracts, handoff_no_mocks_e2e:: is
# integration_g_m, volatile_field_registry_consistency_test:: is
# integration_s_z), and every `Running` line is present. That is why
# --require-targets and --expect-total read announced_targets rather than pair
# labels; reading pair labels here reported integration_s_z as never having run.
PARALLEL_INTERLEAVED = """running 1 test
     Running tests/contracts.rs (target/debug/deps/contracts-aaa)
     Running tests/suites/integration_g_m.rs (target/debug/deps/integration_g_m-bbb)
test failure_mode_fixtures::handoff_prefix_family_fixtures_declare_the_severity_the_envelope_emits ... ok
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 1509 filtered out; finished in 0.31s

running 1 test
     Running tests/suites/integration_s_z.rs (target/debug/deps/integration_s_z-ccc)
test handoff_no_mocks_e2e::no_mocks_handoff_cli_cycle_verifies_hmac_and_rejects_tampering ... ok
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 507 filtered out; finished in 104.97s

running 1 test
test volatile_field_registry_consistency_test::graph_determinism_volatile_list_does_not_drift_further_from_the_registry ... ok
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 545 filtered out; finished in 0.01s
"""


def self_test() -> int:
    cases = [
        ("clean single target", CLEAN, None, 0),
        ("nested child summary", NESTED_CHILD, None, 1),
        ("sequential second target", SEQUENTIAL, None, 1),
        ("announced/counted mismatch", MISMATCH, None, 1),
        ("truncated: announced, never summarised", TRUNCATED, None, 1),
        ("a genuine failure", FAILING, None, 1),
        ("zero tests executed, reported ok", ZERO_TESTS, None, 1),
        # The positive control for --expect-target: the SAME sequential log that
        # is refused above must grade green once a target is named. Without this
        # arm, "refuses everything" would pass every other arm.
        ("sequential, disambiguated by target", SEQUENTIAL, "contracts-bbb", 0),
        # THE TWIN OF THE ARM ABOVE, on the shape real cargo emits when several
        # --test targets are named at once. --expect-target must select ONE pair
        # here exactly as it does for interleaved output; if both summaries carry
        # the last target's name it selects two and refuses, which is the defect
        # this arm pins. Named the empty target deliberately: it must grade exit
        # 1 for announcing ZERO tests, not because it was ambiguous.
        ("buffered targets, disambiguated to the real one", BUFFERED_TARGETS, "integration_a_d-bbb", 0),
        (
            "buffered targets, the empty one is refused for announcing nothing",
            BUFFERED_TARGETS,
            "harness_hook_install_audit-aaa",
            1,
        ),
        # A target that announced a test and IGNORED it executed nothing. Reconciles
        # perfectly and prints `ok`, which is exactly why it needs its own arm.
        ("announced but all ignored is not a pass", ALL_IGNORED, None, 1),
    ]
    # AGGREGATE MODE (1azkt.5 bullet 4). Same fixtures, graded together.
    aggregate_cases = [
        ("all-targets: multi-target log is graded, not refused", SEQUENTIAL, 0),
        ("all-targets: one failing target fails the whole log", SEQUENTIAL_ONE_FAILED, 1),
        ("all-targets: every target announced zero is NOT a pass", ALL_ZERO, 1),
        # NEGATIVE CONTROL for the zero rule: an empty harness beside a real one
        # is normal and must stay green, or the rule is unusable.
        ("all-targets: one empty harness beside a real one is green", ONE_EMPTY_ONE_REAL, 0),
        ("all-targets: a genuine failure still fails", FAILING, 1),
        ("all-targets: a mismatch still refuses", MISMATCH, 1),
        # NESTED still REFUSES in aggregate mode, and that is correct: the
        # child's `running 1 test` lands between the parent's announcement and
        # the parent's summary, so neither can be attributed. Aggregate mode
        # widens WHICH targets are judged, never the fails-closed rule. I first
        # wrote this arm expecting 0 and the harness caught the expectation.
        ("all-targets: nested child is still refused, not attributed", NESTED_CHILD, 1),
        # The ignored rule, aggregated: everything announced was skipped -> not a pass.
        ("all-targets: every announced test ignored is NOT a pass", ALL_IGNORED, 1),
        # NEGATIVE CONTROL. A fixture-gated target beside a live one must stay GREEN, or
        # the rule above is unusable on any repo containing a single #[ignore].
        (
            "all-targets: one fully-ignored target beside a real one is green",
            ONE_IGNORED_ONE_REAL,
            0,
        ),
    ]
    # --require-targets (bd-mbupe). SEQUENTIAL reports exactly two targets,
    # `ee-aaa` and `contracts-bbb`, and every arm below is graded in aggregate
    # mode because that is how scripts/verify.sh invokes this grader.
    #
    # The POSITIVE CONTROL is the second arm. Without it the flag could be wired
    # to nothing at all and the other arms would still pass, which is the exact
    # shape of defect this flag was added to fix -- a defence nobody has seen
    # fail. The first and last arms are its negative controls: the check must not
    # fire when the target did report, or it is unusable.
    require_cases = [
        ("require: a target that DID report is green", SEQUENTIAL, ["contracts"], 0),
        ("require: a target that never reported FAILS", SEQUENTIAL, ["integration_a_d"], 1),
        (
            "require: one present and one absent still FAILS",
            SEQUENTIAL,
            ["contracts", "integration_a_d"],
            1,
        ),
        (
            "require: several targets all present is green",
            SEQUENTIAL,
            ["contracts", "ee-aaa"],
            0,
        ),
        # The flag must not rescue a log that is bad for another reason, and must
        # not mask one either: SEQUENTIAL_ONE_FAILED has both its targets present,
        # so the require check passes and the FAILURE still decides the verdict.
        (
            "require: a satisfied requirement does not mask a real failure",
            SEQUENTIAL_ONE_FAILED,
            ["contracts", "ee-aaa"],
            1,
        ),
    ]
    # --expect-total (bd-9z8ee). SEQUENTIAL's two targets hold 14 tests
    # (4 counted + 10 filtered) and 7 (7 + 0). Graded in aggregate mode.
    #
    # The POSITIVE CONTROL is the second arm: a total that does NOT match must
    # fail. The fourth is the one that matters most in practice -- a total
    # demanded of a target that never reported must FAIL, because the stale-source
    # class this flag defends against often shows up as a target whose filter
    # matched nothing, and a flag that passes vacuously on a typo'd name would
    # reproduce exactly the false green it exists to catch.
    total_cases = [
        ("expect-total: matching total is green", SEQUENTIAL, {"contracts": 7}, 0),
        ("expect-total: a total that moved FAILS", SEQUENTIAL, {"contracts": 8}, 1),
        (
            "expect-total: counted+filtered is the total, not counted",
            SEQUENTIAL,
            {"ee-aaa": 14},
            0,
        ),
        (
            "expect-total: a target that never reported FAILS, not passes",
            SEQUENTIAL,
            {"integration_a_d": 99},
            1,
        ),
        (
            "expect-total: two targets pinned at once, both right, is green",
            SEQUENTIAL,
            {"contracts": 7, "ee-aaa": 14},
            0,
        ),
        (
            "expect-total: a correct total does not mask a real failure",
            SEQUENTIAL_ONE_FAILED,
            {"contracts": 7},
            1,
        ),
        # THE DOCUMENTED WEAKENING, pinned so nobody "fixes" it unknowingly.
        # 14 is ee-aaa's total, not contracts', and this is accepted: the check
        # is "the target ran AND some pair holds N". Tightening it back to
        # per-target requires attribution, which does not exist under parallel
        # execution. One target per invocation is the way to get the strong form.
        (
            "expect-total: a cross-target total is ACCEPTED (deliberate, see parse)",
            SEQUENTIAL,
            {"contracts": 14},
            0,
        ),
    ]
    # PARALLEL INTERLEAVING (measured 2026-10-25). These are the regression arms
    # for a FALSE RED that reached a real verdict: reading pair labels reported
    # integration_s_z as never having run, on a log holding its Running line and
    # its passing summary.
    parallel_cases = [
        (
            "parallel: all three targets are seen despite broken attribution",
            PARALLEL_INTERLEAVED,
            ["contracts", "integration_s_z", "integration_g_m"],
            {},
            0,
        ),
        (
            "parallel: a genuinely absent target still FAILS",
            PARALLEL_INTERLEAVED,
            ["contracts", "integration_n_r"],
            {},
            1,
        ),
        (
            "parallel: the 1510 total is found despite broken attribution",
            PARALLEL_INTERLEAVED,
            [],
            {"contracts": 1510},
            0,
        ),
        (
            "parallel: a total no target holds still FAILS",
            PARALLEL_INTERLEAVED,
            [],
            {"contracts": 1509},
            1,
        ),
    ]
    failures = 0
    with tempfile.TemporaryDirectory() as directory:
        for name, body, expect_target, want in cases:
            path = pathlib.Path(directory) / "log.txt"
            path.write_text(body)
            got = grade(path, expect_target)
            status = "OK  " if got == want else "FAIL"
            if got != want:
                failures += 1
            print(f"  [self-test] {status} {name}: want exit {want}, got {got}\n")
        for name, body, want in aggregate_cases:
            path = pathlib.Path(directory) / "log.txt"
            path.write_text(body)
            got = grade(path, None, all_targets=True)
            status = "OK  " if got == want else "FAIL"
            if got != want:
                failures += 1
            print(f"  [self-test] {status} {name}: want exit {want}, got {got}\n")
        for name, body, required, want in require_cases:
            path = pathlib.Path(directory) / "log.txt"
            path.write_text(body)
            got = grade(path, None, all_targets=True, require_targets=required)
            status = "OK  " if got == want else "FAIL"
            if got != want:
                failures += 1
            print(f"  [self-test] {status} {name}: want exit {want}, got {got}\n")
        for name, body, totals, want in total_cases:
            path = pathlib.Path(directory) / "log.txt"
            path.write_text(body)
            got = grade(path, None, all_targets=True, expect_totals=totals)
            status = "OK  " if got == want else "FAIL"
            if got != want:
                failures += 1
            print(f"  [self-test] {status} {name}: want exit {want}, got {got}\n")
        for name, body, required, totals, want in parallel_cases:
            path = pathlib.Path(directory) / "log.txt"
            path.write_text(body)
            got = grade(
                path,
                None,
                all_targets=True,
                require_targets=required,
                expect_totals=totals,
            )
            status = "OK  " if got == want else "FAIL"
            if got != want:
                failures += 1
            print(f"  [self-test] {status} {name}: want exit {want}, got {got}\n")
    total = (
        len(cases)
        + len(aggregate_cases)
        + len(require_cases)
        + len(total_cases)
        + len(parallel_cases)
    )
    if failures:
        print(f"[grade-test-log] SELF-TEST FAILED: {failures} of {total} arms")
        return 1
    print(f"[grade-test-log] self-test: {total} of {total} arms passed")
    return 0


def main(argv: list[str]) -> int:
    args = list(argv[1:])
    if "--self-test" in args:
        return self_test()
    all_targets = "--all-targets" in args
    if all_targets:
        args.remove("--all-targets")
    expect_target = None
    if "--expect-target" in args:
        index = args.index("--expect-target")
        try:
            expect_target = args[index + 1]
        except IndexError:
            print("[grade-test-log] --expect-target needs a value", file=sys.stderr)
            return 2
        del args[index : index + 2]
    require_targets: list[str] = []
    if "--require-targets" in args:
        index = args.index("--require-targets")
        try:
            raw = args[index + 1]
        except IndexError:
            print("[grade-test-log] --require-targets needs a value", file=sys.stderr)
            return 2
        del args[index : index + 2]
        require_targets = [item.strip() for item in raw.split(",") if item.strip()]
        # An empty or all-whitespace list would silently assert nothing, which is
        # the decoration this flag exists to remove. Refuse rather than pass.
        if not require_targets:
            print(
                "[grade-test-log] --require-targets was given no non-empty names",
                file=sys.stderr,
            )
            return 2
    # Repeatable, so several targets can be pinned in one invocation:
    #   --expect-total contracts=1510 --expect-total integration_s_z=812
    expect_totals: dict[str, int] = {}
    while "--expect-total" in args:
        index = args.index("--expect-total")
        try:
            raw = args[index + 1]
        except IndexError:
            print(
                "[grade-test-log] --expect-total needs <target>=<count>",
                file=sys.stderr,
            )
            return 2
        del args[index : index + 2]
        name, _, count = raw.partition("=")
        name = name.strip()
        if not name or not count.strip().isdigit():
            print(
                f"[grade-test-log] --expect-total wants <target>=<count>, got {raw!r}",
                file=sys.stderr,
            )
            return 2
        expect_totals[name] = int(count.strip())
    if len(args) != 1:
        print(__doc__.split("USAGE")[-1].strip(), file=sys.stderr)
        return 2
    return grade(
        pathlib.Path(args[0]),
        expect_target,
        all_targets,
        require_targets,
        expect_totals,
    )


if __name__ == "__main__":
    sys.exit(main(sys.argv))
