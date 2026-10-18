#!/usr/bin/env python3
"""Flag workflow steps where ONE failing assertion silently skips the rest (bd-lagsa).

THE DEFECT, MEASURED TWICE IN ONE DAY
    A step that runs several `cargo test` invocations, each followed by its own
    grep, under `set -euo pipefail` stops at the FIRST failing grep. Everything
    after it never executes, and the log gives no signal: one failed grep, then
    greps whose output simply never appears. An absent `test result:` line looks
    exactly like a test that was never going to run.

    doctor-repair-receipts.yml   a stale `7 passed` pin was grep 1 of 4.
                                 Fixing it resumed dispatch (28), runtime (53) and
                                 cli (12) -- 93 TESTS that had not executed since
                                 2026-09-26.
    primary-recovery-20260919    a stale `14 passed` pin was check 4 of 8. It was
                                 hiding three test groups, a suite, and a
                                 `git diff --exit-code` working-tree guard.

    So a brittle assertion is not merely noisy. In an aborting step it is a KILL
    SWITCH for every assertion downstream of it.

THE PREDICATE, AND WHY IT IS CHEAP
    A step is at risk when it contains two or more test-result assertions AND does
    not collect failures into a flag. The safe idiom is already used elsewhere in
    this repo:

        status=0; cargo test ... || status=$?
        ... || failed=1
        exit "$failed"

    promotion-recovery.yml is the proof that it works: it carried TWO stale counts
    and, because it accumulates, BOTH suites still ran and both reported
    `0 failed` -- which is how both wrong numbers were visible at once. In an
    aborting step only the first would have been.

    This check needs no test run, no knowledge of which counts are derivable, and
    no network. It is a static property of the workflow file.

SHRINK-ONLY BASELINE, FAILING IN BOTH DIRECTIONS
    The 14 existing at-risk steps are baselined rather than rewritten here:
    converting a step to an accumulator changes what it reports on failure and
    belongs with whoever owns that lane. Per the house pattern
    (scripts/action-pins-baseline.txt, tests/fixtures/e2e_invocation/
    orphan_baseline.txt, scripts/production-reachability-baseline.txt):

      * a NEW at-risk step is an error;
      * a baselined step that has since been made safe is ALSO an error, telling
        you to delete the line.

USAGE
    scripts/check-step-assertion-isolation.py
    scripts/check-step-assertion-isolation.py --json
    scripts/check-step-assertion-isolation.py --self-test
"""

from __future__ import annotations

import argparse
import glob
import json
import os
import re
import subprocess
import sys
import tempfile
from pathlib import Path

BASELINE = "scripts/step-assertion-isolation-baseline.txt"

# A test-result assertion: a grep for libtest's summary line.
ASSERT_RE = re.compile(r"test result: ok")
# Failure accumulation: the step collects into a flag instead of dying.
ACCUM_RE = re.compile(r"\bfailed=1\b|\bstatus=\$\?|\|\|\s*status=|\bfailed=\$\(")
# Abort-on-error shells.
ABORT_RE = re.compile(r"set\s+-[a-z]*e[a-z]*\b|set\s+-o\s+errexit|pipefail")
# A publication side effect performed by the step ITSELF. When a step both
# asserts and publishes, aborting is a SAFETY INTERLOCK, not an oversight:
# converting it to accumulate would run the push and only then report failure,
# landing unverified code on main. Such a step is reported separately and is
# never demanded for conversion.
#
# Deliberately order-blind. These lanes push inside a `for attempt in 1 2 3`
# retry loop and re-verify after merging origin/main, so an assertion can sit
# textually AFTER the push it gates (session-arc-20260919.yml does exactly
# this). Asking only "is there a side effect after the first assertion?" calls
# that step pure verification and recommends the unsafe change.
PUBLISH_RE = re.compile(r"\bgit\s+push\b|\bgit\s+commit\b|\bgh\s+release\b")
# A step header in a workflow job.
STEP_RE = re.compile(r"^(\s*)-\s+name:\s*(.+?)$", re.M)


def strip_comments(body: str) -> str:
    """Remove YAML/shell comments from a step body, respecting quotes.

    THE GATE DECIDED ON PROSE WITHOUT THIS. Every classification below matched
    over the raw body, so a step could be called "accumulating" purely by
    naming `failed=1` inside a COMMENT while still dying on its first
    assertion -- and a comment mentioning `git push` could mark a step
    publish-gated that pushes nothing.

    This is the same hole scripts/check-production-reachability.py had, fixed
    there at 24f097272 after it fired for real: a comment added to the doctor
    dispatch table to record that two fixers are NOT wired made that gate
    report them as wired and instruct the operator to delete two accurate
    baseline rows.

    Quote tracking is load-bearing in both directions: a '#' inside
    "sha256:ab#cd" must not truncate real content, and `failed=1  # why`
    must keep its assignment.
    """
    out = []
    for line in body.split("\n"):
        stripped = line.lstrip()
        if stripped.startswith("#"):
            continue
        in_s = None
        esc = False
        cut = len(line)
        for i, ch in enumerate(line):
            if esc:
                esc = False
            elif ch == "\\":
                esc = True
            elif in_s:
                if ch == in_s:
                    in_s = None
            elif ch in "\"'":
                in_s = ch
            elif ch == "#":
                cut = i
                break
        out.append(line[:cut])
    return "\n".join(out)


def steps(text: str):
    """Yield (name, body) for each step that has a `run:` block."""
    marks = [(m.start(), m.group(1), m.group(2).strip()) for m in STEP_RE.finditer(text)]
    for index, (start, _indent, name) in enumerate(marks):
        end = marks[index + 1][0] if index + 1 < len(marks) else len(text)
        body = text[start:end]
        if "run:" in body:
            yield name, body


def audit(root: Path) -> dict:
    at_risk = []
    safe = []
    publish_gated = []
    examined = 0
    for path in sorted(glob.glob(str(root / ".github" / "workflows" / "*.yml"))):
        base = os.path.basename(path)
        try:
            text = Path(path).read_text(encoding="utf-8", errors="replace")
        except OSError:
            continue
        for name, raw_body in steps(text):
            # EVERY predicate reads the de-commented body. Counting assertions
            # in prose would inflate `count`; classifying on prose decides a
            # step's posture from a sentence about the posture.
            body = strip_comments(raw_body)
            count = len(ASSERT_RE.findall(body))
            if count < 2:
                continue
            examined += 1
            accumulates = bool(ACCUM_RE.search(body))
            aborts = bool(ABORT_RE.search(body)) and not accumulates
            publishes = bool(PUBLISH_RE.search(body))
            key = f"{base}::{name}"
            row = {"step": key, "assertions": count, "atRisk": aborts and not publishes}
            if aborts and publishes:
                # Correct by design: the assertions gate this step's own push.
                row["publishGated"] = True
                publish_gated.append(row)
            elif aborts:
                # n-1 assertions sit downstream of the first failure.
                row["assertionsAtRisk"] = count - 1
                at_risk.append(row)
            else:
                safe.append(row)
    return {
        "schema": "ee.step_assertion_isolation.v2",
        "multiAssertionSteps": examined,
        "atRisk": sorted(at_risk, key=lambda r: (-r["assertions"], r["step"])),
        "publishGated": sorted(publish_gated, key=lambda r: r["step"]),
        "safe": sorted(safe, key=lambda r: r["step"]),
        "assertionsAtRisk": sum(r["assertionsAtRisk"] for r in at_risk),
    }


def load_baseline(root: Path) -> set[str]:
    path = root / BASELINE
    if not path.is_file():
        return set()
    out = set()
    for line in path.read_text(encoding="utf-8", errors="replace").splitlines():
        line = line.strip()
        if line and not line.startswith("#"):
            out.add(line)
    return out


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--root", default=".")
    parser.add_argument("--json", action="store_true")
    parser.add_argument("--self-test", action="store_true")
    args = parser.parse_args()
    if args.self_test:
        return self_test()

    root = Path(args.root).resolve()
    result = audit(root)
    baseline = load_baseline(root)
    found = {r["step"] for r in result["atRisk"]}
    new = sorted(found - baseline)
    fixed = sorted(baseline - found)
    result["baselineSize"] = len(baseline)
    result["newAtRisk"] = new
    result["baselinedButNowSafe"] = fixed

    if args.json:
        print(json.dumps(result, indent=2, sort_keys=True))
    else:
        print(
            "[step-isolation] predicate: a step with 2+ test-result assertions that "
            "aborts on the first failure instead of accumulating into a flag"
        )
        print(
            f"[step-isolation] multi-assertion steps: {result['multiAssertionSteps']}; "
            f"at risk: {len(found)}; publish-gated: {len(result['publishGated'])}; "
            f"safe: {len(result['safe'])}; "
            f"assertions downstream of a first failure: {result['assertionsAtRisk']}; "
            f"baseline: {len(baseline)}"
        )
        for row in result["atRisk"]:
            mark = "NEW" if row["step"] in new else "baselined"
            print(f"   [{mark}] {row['assertions']} assertions "
                  f"({row['assertionsAtRisk']} at risk)  {row['step']}")
        for row in result["publishGated"]:
            print(f"   [publish-gated, aborting is CORRECT] {row['assertions']} "
                  f"assertions  {row['step']}")

    status = 0
    if new:
        print(
            f"\n[step-isolation] FAIL: {len(new)} step(s) can silently skip their "
            f"later assertions and are not in {BASELINE}:",
            file=sys.stderr,
        )
        for n in new:
            print(f"    {n}", file=sys.stderr)
        print(
            "  Collect failures into a flag (status=$?; ... || failed=1; exit \"$failed\") "
            "so one stale assertion cannot disable the rest.",
            file=sys.stderr,
        )
        status = 1
    if fixed:
        gated = {r["step"] for r in result["publishGated"]}
        print(
            f"\n[step-isolation] FAIL: {len(fixed)} baselined step(s) are no longer "
            f"at risk; delete their line(s) from {BASELINE}:",
            file=sys.stderr,
        )
        for n in fixed:
            why = ("now publishes in-step, so aborting is correct"
                   if n in gated else "now accumulates")
            print(f"    {n}  ({why})", file=sys.stderr)
        status = 1
    if status == 0 and not args.json:
        # Never on the --json path: a trailing prose line makes the document
        # unparseable ("Extra data"), which is how this was found.
        print("[step-isolation] OK -- no at-risk step beyond the accepted baseline")
    return status


# ---------------------------------------------------------------------------
# Self-test. The refusals are the value: a check nobody has seen fail is
# decoration.
# ---------------------------------------------------------------------------

ABORTING = """jobs:
  verify:
    steps:
      - name: Aborting multi-assert
        run: |
          set -euo pipefail
          cargo test --lib a | tee a.log
          grep -Fq 'test result: ok. 3 passed' a.log
          cargo test --lib b | tee b.log
          grep -Fq 'test result: ok. 4 passed' b.log
"""

# bd-step-isolation-mention-counts-2zj71: aborts for real, only TALKS about
# accumulating. The comment deliberately spells both idioms ACCUM_RE looks for.
COMMENT_ONLY_ACCUM = """jobs:
  verify:
    steps:
      - name: Aborting step whose comments mention the flag
        run: |
          set -euo pipefail
          # historical note: this used to accumulate with failed=1
          # and read status=$? after each suite before it was simplified
          cargo test --lib a > a.log
          grep -Fq 'test result: ok. 3 passed' a.log
          cargo test --lib b > b.log
          grep -Fq 'test result: ok. 4 passed' b.log
"""

# The converse: a REAL accumulator whose lines carry trailing comments, plus a
# '#' inside a quoted assertion so the stripper cannot be line-blanking.
ACCUM_WITH_TRAILING_COMMENT = """jobs:
  verify:
    steps:
      - name: Accumulating with trailing comments
        run: |
          set -uo pipefail
          failed=0
          cargo test --lib a > a.log || failed=1  # first suite
          grep -Fq 'test result: ok. 3 passed' a.log || failed=1  # assert a
          echo 'marker#not-a-comment'
          cargo test --lib b > b.log || failed=1  # second suite
          grep -Fq 'test result: ok. 4 passed' b.log || failed=1  # assert b
          exit "$failed"
"""

ACCUMULATING = """jobs:
  verify:
    steps:
      - name: Accumulating multi-assert
        run: |
          set -uo pipefail
          failed=0
          cargo test --lib a > a.log || failed=1
          grep -Fq 'test result: ok. 3 passed' a.log || failed=1
          cargo test --lib b > b.log || failed=1
          grep -Fq 'test result: ok. 4 passed' b.log || failed=1
          exit "$failed"
"""

# Publishes from inside the step, assertions BEFORE the push. Aborting is the
# interlock that keeps unverified code off main, so this must NOT be demanded
# for conversion.
PUBLISH_GATED = """jobs:
  verify:
    steps:
      - name: Verify then publish in one step
        run: |
          set -euo pipefail
          cargo test --lib a | tee a.log
          grep -Fq 'test result: ok. 3 passed' a.log
          cargo test --lib b | tee b.log
          grep -Fq 'test result: ok. 4 passed' b.log
          git add -- src/x.rs
          git commit -m 'feat: x'
          git push origin HEAD:main
"""

# The ordering trap. The push comes textually FIRST; the assertions live in the
# retry loop and gate the NEXT attempt. A predicate that only looks for a side
# effect AFTER the first assertion calls this pure verification and recommends
# the unsafe conversion. session-arc-20260919.yml is this shape in production.
PUBLISH_GATED_IN_LOOP = """jobs:
  verify:
    steps:
      - name: Publish with merge-retry reverification
        run: |
          set -euo pipefail
          git commit -m 'feat: x'
          for attempt in 1 2 3; do
            if git push origin HEAD:main; then break; fi
            test "$attempt" -lt 3
            git merge --no-edit origin/main
            cargo test --lib a > a.log
            grep -Fq 'test result: ok. 3 passed' a.log
            cargo test --lib b > b.log
            grep -Fq 'test result: ok. 4 passed' b.log
          done
"""

SINGLE = """jobs:
  verify:
    steps:
      - name: One assertion only
        run: |
          set -euo pipefail
          cargo test --lib a | tee a.log
          grep -Fq 'test result: ok. 3 passed' a.log
"""


def _plant(root: Path, name: str, content: str) -> None:
    d = root / ".github" / "workflows"
    d.mkdir(parents=True, exist_ok=True)
    (d / name).write_text(content, encoding="utf-8")


def self_test() -> int:
    arms: list[tuple[str, bool]] = []

    def arm(label: str, ok: bool) -> None:
        arms.append((label, ok))
        print(f"  [{'ok  ' if ok else 'FAIL'}] {label}")

    with tempfile.TemporaryDirectory(prefix="stepiso-") as tmp:
        # POSITIVE: an aborting multi-assertion step is flagged.
        r1 = Path(tempfile.mkdtemp(dir=tmp))
        _plant(r1, "a.yml", ABORTING)
        a1 = audit(r1)
        arm("an aborting step with 2 assertions is flagged", len(a1["atRisk"]) == 1)
        arm("and it reports 1 assertion downstream of the first failure",
            a1["assertionsAtRisk"] == 1)

        # NEGATIVE 1: the accumulator idiom is NOT flagged.
        r2 = Path(tempfile.mkdtemp(dir=tmp))
        _plant(r2, "b.yml", ACCUMULATING)
        a2 = audit(r2)
        arm("an accumulating step is NOT flagged", a2["atRisk"] == [])
        arm("and it is counted as safe", len(a2["safe"]) == 1)

        # NEGATIVE 2: a single-assertion step is out of scope entirely.
        r3 = Path(tempfile.mkdtemp(dir=tmp))
        _plant(r3, "c.yml", SINGLE)
        a3 = audit(r3)
        arm("a single-assertion step is not examined at all",
            a3["multiAssertionSteps"] == 0 and a3["atRisk"] == [])

        # Steps must be attributed separately, not merged.
        r4 = Path(tempfile.mkdtemp(dir=tmp))
        _plant(r4, "d.yml", ABORTING + ACCUMULATING.split("steps:\n")[1])
        a4 = audit(r4)
        arm("two steps in one file are attributed separately",
            a4["multiAssertionSteps"] == 2 and len(a4["atRisk"]) == 1)

        # PUBLISH GATE: aborting is correct, so it is NOT an at-risk row.
        r6 = Path(tempfile.mkdtemp(dir=tmp))
        _plant(r6, "e.yml", PUBLISH_GATED)
        a6 = audit(r6)
        arm("a step that publishes in-step is NOT demanded for conversion",
            a6["atRisk"] == [] and len(a6["publishGated"]) == 1)
        arm("and it is not silently counted as already safe either",
            a6["safe"] == [] and a6["assertionsAtRisk"] == 0)

        # The ordering trap: push textually before the assertions it gates.
        r7 = Path(tempfile.mkdtemp(dir=tmp))
        _plant(r7, "f.yml", PUBLISH_GATED_IN_LOOP)
        a7 = audit(r7)
        arm("a push inside a retry loop still counts as publish-gated, "
            "though it precedes its assertions",
            a7["atRisk"] == [] and len(a7["publishGated"]) == 1)

        # CONTROL for the arm above: the same assertions WITHOUT a publish verb
        # must still be flagged, proving the new branch narrows nothing else.
        r8 = Path(tempfile.mkdtemp(dir=tmp))
        _plant(r8, "g.yml", PUBLISH_GATED_IN_LOOP.replace(
            "git commit -m 'feat: x'", "echo no-publish").replace(
            "if git push origin HEAD:main; then break; fi", "if false; then break; fi"))
        a8 = audit(r8)
        arm("CONTROL: the same step with the publish verbs removed is flagged "
            "again", len(a8["atRisk"]) == 1)

        # COMMENT HANDLING (bd-step-isolation-mention-counts-2zj71).
        #
        # THE POSITIVE CONTROL. This step ABORTS on its first failure and only
        # MENTIONS the accumulation idiom, in comments. Before strip_comments
        # it was classified "accumulating" and reported safe, so an aborting
        # step hid behind a sentence about accumulating. This arm fails on the
        # pre-fix code, which is what makes it a control rather than decoration.
        r10 = Path(tempfile.mkdtemp(dir=tmp))
        _plant(r10, "h.yml", COMMENT_ONLY_ACCUM)
        a10 = audit(r10)
        arm("a step that names the failure flag ONLY in a comment is still "
            "reported at risk", len(a10["atRisk"]) == 1)

        # THE CONVERSE, so the fix cannot be an over-broad silencer: a GENUINE
        # accumulator whose lines carry trailing comments must still read as
        # accumulating. A stripper that blanked whole lines would satisfy the
        # arm above and break this one.
        r11 = Path(tempfile.mkdtemp(dir=tmp))
        _plant(r11, "i.yml", ACCUM_WITH_TRAILING_COMMENT)
        a11 = audit(r11)
        arm("a real accumulation carrying trailing comments is NOT flagged",
            a11["atRisk"] == [] and len(a11["safe"]) == 1)

        # And a '#' inside a quoted string must not truncate the line it sits
        # on: the assertion pattern itself has to survive.
        arm("quoted '#' does not eat the assertion count",
            a11["multiAssertionSteps"] == 1)

        # --json must be a parseable document on BOTH exit paths. It was not:
        # the trailing "OK" line made the clean run fail with "Extra data".
        r9 = Path(tempfile.mkdtemp(dir=tmp))
        _plant(r9, "b.yml", ACCUMULATING)
        for label, extra in (("clean", None), ("failing", ABORTING)):
            if extra:
                _plant(r9, "a.yml", extra)
            proc = subprocess.run(
                [sys.executable, str(Path(__file__).resolve()),
                 "--root", str(r9), "--json"],
                capture_output=True, text=True,
            )
            ok = False
            try:
                ok = isinstance(json.loads(proc.stdout), dict)
            except json.JSONDecodeError:
                ok = False
            arm(f"--json stdout parses as one JSON document on the {label} path", ok)

        # The baseline must fail in BOTH directions.
        r5 = Path(tempfile.mkdtemp(dir=tmp))
        _plant(r5, "a.yml", ABORTING)
        (r5 / "scripts").mkdir(parents=True, exist_ok=True)
        key = audit(r5)["atRisk"][0]["step"]
        (r5 / BASELINE).write_text(f"# planted\n{key}\n", encoding="utf-8")
        base = load_baseline(r5)
        arm("a baselined at-risk step is accepted", {key} == base & {key})
        (r5 / BASELINE).write_text("# planted\na.yml::Some step that no longer exists\n",
                                   encoding="utf-8")
        base2 = load_baseline(r5)
        found2 = {r["step"] for r in audit(r5)["atRisk"]}
        arm("a baselined step that is no longer at risk is reported",
            bool(base2 - found2))
        arm("and the genuinely at-risk step is then reported as NEW",
            bool(found2 - base2))

    passed = sum(1 for _, ok in arms if ok)
    print(f"\n[step-isolation] self-test: {passed}/{len(arms)} arms passed")
    return 0 if passed == len(arms) else 1


if __name__ == "__main__":
    raise SystemExit(main())
