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
)
RUNNING_TARGET = re.compile(r"^\s*Running (?:unittests )?(?P<what>\S+)\s*\((?P<bin>[^)]+)\)")
DOCTEST_TARGET = re.compile(r"^\s*Doc-tests\s+(?P<what>\S+)")


def strip_ansi(text: str) -> str:
    return re.sub(r"\x1b\[[0-9;]*m", "", text)


def parse(lines: list[str]) -> tuple[list[dict], list[str]]:
    """Return (pairs, problems). Each pair is one announcement and its summary."""
    pairs: list[dict] = []
    problems: list[str] = []
    target = "<unknown target>"
    pending: dict | None = None

    for number, raw in enumerate(lines, start=1):
        line = strip_ansi(raw.rstrip("\n"))

        found_target = RUNNING_TARGET.match(line) or DOCTEST_TARGET.match(line)
        if found_target:
            target = found_target.groupdict().get("bin") or found_target.group("what")
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
                "target": target,
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
                    f"({counted} test(s), target {target}) -- cannot be attributed"
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
    return pairs, problems


def grade(path: pathlib.Path, expect_target: str | None, all_targets: bool = False) -> int:
    try:
        lines = path.read_text(errors="replace").splitlines()
    except OSError as error:
        print(f"[grade-test-log] cannot read {path}: {error}", file=sys.stderr)
        return 2

    pairs, problems = parse(lines)

    # Always print every pair. A grade whose denominator is invisible is the
    # failure this tool exists to remove.
    print(f"[grade-test-log] {path}")
    if not pairs and not problems:
        print("  NO test announcements and NO summaries -- nothing ran.")
        return 1
    for pair in pairs:
        reconciles = pair["announced"] == pair["counted"]
        mark = "OK " if reconciles else "MISMATCH"
        print(
            f"  {mark} target={pair['target']} announced={pair['announced']} "
            f"counted={pair['counted']} verdict={pair['verdict']} "
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
        print(
            f"[grade-test-log] GREEN: {len(chosen)} target(s), "
            f"{total_announced} tests announced, all reconciled."
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
    total = len(cases) + len(aggregate_cases)
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
    if len(args) != 1:
        print(__doc__.split("USAGE")[-1].strip(), file=sys.stderr)
        return 2
    return grade(pathlib.Path(args[0]), expect_target, all_targets)


if __name__ == "__main__":
    sys.exit(main(sys.argv))
