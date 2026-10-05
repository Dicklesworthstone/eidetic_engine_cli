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
# A step header in a workflow job.
STEP_RE = re.compile(r"^(\s*)-\s+name:\s*(.+?)$", re.M)


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
    examined = 0
    for path in sorted(glob.glob(str(root / ".github" / "workflows" / "*.yml"))):
        base = os.path.basename(path)
        try:
            text = Path(path).read_text(encoding="utf-8", errors="replace")
        except OSError:
            continue
        for name, body in steps(text):
            count = len(ASSERT_RE.findall(body))
            if count < 2:
                continue
            examined += 1
            accumulates = bool(ACCUM_RE.search(body))
            aborts = bool(ABORT_RE.search(body)) and not accumulates
            key = f"{base}::{name}"
            row = {"step": key, "assertions": count, "atRisk": aborts}
            if aborts:
                # n-1 assertions sit downstream of the first failure.
                row["assertionsAtRisk"] = count - 1
                at_risk.append(row)
            else:
                safe.append(row)
    return {
        "schema": "ee.step_assertion_isolation.v1",
        "multiAssertionSteps": examined,
        "atRisk": sorted(at_risk, key=lambda r: (-r["assertions"], r["step"])),
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
            f"at risk: {len(found)}; safe: {len(result['safe'])}; "
            f"assertions downstream of a first failure: {result['assertionsAtRisk']}; "
            f"baseline: {len(baseline)}"
        )
        for row in result["atRisk"]:
            mark = "NEW" if row["step"] in new else "baselined"
            print(f"   [{mark}] {row['assertions']} assertions "
                  f"({row['assertionsAtRisk']} at risk)  {row['step']}")

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
        print(
            f"\n[step-isolation] FAIL: {len(fixed)} baselined step(s) now accumulate; "
            f"delete their line(s) from {BASELINE}:",
            file=sys.stderr,
        )
        for n in fixed:
            print(f"    {n}", file=sys.stderr)
        status = 1
    if status == 0:
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
