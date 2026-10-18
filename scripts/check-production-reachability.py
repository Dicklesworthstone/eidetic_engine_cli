#!/usr/bin/env python3
"""Fail when a declared surface function is reachable ONLY from test code.

Asked for, in the same words, by two beads:
  bd-6oqlx  "a gate that fails if a scoring surface has only test callers"
  bd-223vl  the eight doctor fixers "reachable only from their own unit tests"

A function existing is not a capability. Both beads record the same way of
hiding: the fn is written, its unit tests pass, every reader assumes it runs,
and nothing calls it outside `#[cfg(test)]`.

WHY THIS NEEDS A REAL PARSE AND NOT A GREP
    Two mistakes are easy here and I made both before writing this.

    1. EXCLUDING THE DEFINING FILE. Looking for callers "somewhere other than
       the module that defines them" reports a wired feature as dead, because
       dispatch tables live NEXT TO the functions they dispatch.
       src/core/doctor_fixers.rs has exactly that: `fix_dispatch_for_finding`
       routes ten fixers from inside the same file. A sweep that skips the file
       sees zero callers for all ten.
    2. SPLITTING ON THE FIRST `#[cfg(test)]`. A file may have several test
       modules with PRODUCTION CODE BETWEEN THEM. src/core/doctor_fixers.rs
       carries `#[cfg(test)]` at two places, so
       `source.split("#[cfg(test)]").next()` -- which an existing test in that
       very file uses -- treats everything after the first as test and cannot
       see production code that follows it.

    So: brace-match every `#[cfg(test)] mod ... { }` region, treat ONLY those
    line ranges as test, and count references everywhere else INCLUDING the
    defining file.

SHRINK-ONLY BASELINE, FAILING IN BOTH DIRECTIONS
    The known violations are baselined rather than fixed here, because wiring
    eight doctor fixers at once is a decision with its own tests and belongs to
    bd-223vl, not to a gate that merely adds visibility. Following the house
    pattern (scripts/action-pins-baseline.txt,
    tests/fixtures/e2e_invocation/orphan_baseline.txt):

      * a NEW test-only surface is an error;
      * a baselined entry that is now reachable is ALSO an error, telling you to
        delete the line.

    The second direction is what stops a baseline decaying into a permanent
    ignore-list.

USAGE
    scripts/check-production-reachability.py
    scripts/check-production-reachability.py --json
    scripts/check-production-reachability.py --self-test
"""

from __future__ import annotations

import argparse
import json
import os
import re
import sys
import tempfile
from pathlib import Path

BASELINE = "scripts/production-reachability-baseline.txt"

# Surfaces to audit: (file, regex matching the function names that are surface).
# Deliberately explicit. A gate that guesses which functions matter produces
# noise, and noise is how a gate gets disabled.
SURFACES: list[tuple[str, str]] = [
    # bd-223vl: `ee doctor --fix` fixers.
    ("src/core/doctor_fixers.rs", r"fix_[a-z0-9_]+"),
    # bd-6oqlx: the documented scoring multiplier stack.
    ("src/search/scoring.rs", r"(?:anchor_match_score|bead_affinity_score|final_score|stale_anchor_floor|freshness_drift_multiplier)"),
]

# `[ \t]*`, NOT `\s*`, after the `^`. Under re.M, `\s` matches newlines, so
# `^\s*fn` could anchor on a BLANK LINE ABOVE the definition and consume the
# newline -- making `m.start()` land one line early and `def_line` one too
# small. The consequence was a silent FALSE NEGATIVE, in the direction that
# hides findings: the `idx == def_line` guard then failed to skip the real
# definition line, so the fn counted as referencing itself from production and
# could never be reported as test-only. Any surface fn preceded by a blank
# line was exempt from this gate. It stayed invisible because Rust style puts
# a doc comment immediately above most fns, and `\s*` cannot cross a `///`
# line -- so the bug only fires on the undocumented ones.
DEF_RE_TEMPLATE = r"^[ \t]*(?:pub(?:\s*\([^)]*\))?\s+)?(?:async\s+)?(?:unsafe\s+)?fn\s+({name})\s*[(<]"
CFG_TEST_RE = re.compile(r"#\[cfg\(test\)\]")


def strip_line_comment(line: str) -> str:
    """Drop a Rust `//` line comment so a MENTION cannot pass as a CALL.

    A name written in a comment is not a caller. Without this, documenting why
    a fn is NOT wired marks it as wired, and the gate reports the opposite of
    the truth. That is not hypothetical: commit 5b09b76f9 added a comment to
    the dispatch table in src/core/doctor_fixers.rs naming
    `fix_graph_snapshot_stale` and `fix_policy_safety_inconsistent` -- to
    record that nothing dispatches them -- and this gate immediately reported
    both as "now reachable" and failed, demanding their baseline lines be
    deleted. Deleting them would have retired two true findings on the
    strength of a comment that says they are true.

    scripts/e2e_invocation_audit.sh reached the same conclusion for shell and
    states it the same way: "Invoked is NOT the same as mentioned."

    KNOWN LIMITATION, stated rather than hidden: this splits on the first `//`
    without tracking string literals, so `let u = "http://x"; fix_y();` loses
    its real call. That direction is the safe one -- the fn is then reported as
    test-only, which is a visible false finding rather than a silent miss.
    Block comments are not handled; every current surface uses `//`.
    """
    return line.split("//", 1)[0]
MOD_RE = re.compile(r"\bmod\s+[A-Za-z_][A-Za-z0-9_]*\s*\{")


def test_line_ranges(text: str) -> list[tuple[int, int]]:
    """1-based inclusive line ranges of every `#[cfg(test)] mod ... { .. }`.

    Brace-matched, so several test modules in one file are all found and
    production code BETWEEN them is not swallowed.
    """
    ranges: list[tuple[int, int]] = []
    for attr in CFG_TEST_RE.finditer(text):
        # find the `mod ... {` that follows the attribute
        m = MOD_RE.search(text, attr.end())
        if not m:
            continue
        # Only treat it as the attribute's module if nothing but attributes and
        # whitespace sits between them.
        between = text[attr.end() : m.start()]
        if between.strip().replace("\n", "") and not re.fullmatch(
            r"[\s]*(?:#\[[^\]]*\]\s*)*", between
        ):
            continue
        depth = 0
        i = m.end() - 1
        while i < len(text):
            if text[i] == "{":
                depth += 1
            elif text[i] == "}":
                depth -= 1
                if depth == 0:
                    break
            i += 1
        start_line = text.count("\n", 0, attr.start()) + 1
        end_line = text.count("\n", 0, min(i, len(text) - 1)) + 1
        ranges.append((start_line, end_line))
    return ranges


def in_ranges(line_no: int, ranges: list[tuple[int, int]]) -> bool:
    return any(lo <= line_no <= hi for lo, hi in ranges)


def rust_files(root: Path) -> list[Path]:
    out = []
    for base in ("src", "tests", "benches"):
        start = root / base
        if not start.is_dir():
            continue
        for dirpath, dirnames, filenames in os.walk(start):
            dirnames[:] = [d for d in dirnames if not d.startswith(".") and d != "target"]
            for name in filenames:
                if name.endswith(".rs"):
                    out.append(Path(dirpath) / name)
    return sorted(out)


def read(path: Path) -> str:
    try:
        return path.read_text(encoding="utf-8", errors="replace")
    except OSError:
        return ""


def audit(root: Path, surfaces: list[tuple[str, str]]) -> dict:
    files = rust_files(root)
    cache: dict[Path, tuple[str, list[str], list[tuple[int, int]]]] = {}
    for f in files:
        t = read(f)
        cache[f] = (t, t.splitlines(), test_line_ranges(t))

    findings = []
    examined = 0
    for rel, name_pat in surfaces:
        path = root / rel
        if path not in cache:
            findings.append({"surface": rel, "name": None, "issue": "surface file not found"})
            continue
        text, lines, tranges = cache[path]
        def_re = re.compile(DEF_RE_TEMPLATE.format(name=name_pat), re.M)
        for m in def_re.finditer(text):
            fname = m.group(1)
            def_line = text.count("\n", 0, m.start()) + 1
            # A fn DEFINED inside a test module is a test helper, not a surface.
            if in_ranges(def_line, tranges):
                continue
            examined += 1
            prod_refs = []
            test_refs = 0
            word = re.compile(r"\b" + re.escape(fname) + r"\b")
            for f, (ftext, flines, franges) in cache.items():
                if fname not in ftext:
                    continue
                for idx, line in enumerate(flines, start=1):
                    if not word.search(line):
                        continue
                    if f == path and idx == def_line:
                        continue  # the definition itself is not a caller
                    # A MENTION IS NOT A REFERENCE, in either direction, so
                    # this is checked before the test/production split rather
                    # than inside one branch of it.
                    if not word.search(strip_line_comment(line)):
                        continue
                    if in_ranges(idx, franges):
                        test_refs += 1
                        continue
                    prod_refs.append(
                        f"{os.path.relpath(f, root)}:{idx}".replace(os.sep, "/")
                    )
            if not prod_refs:
                findings.append(
                    {
                        "surface": rel,
                        "name": fname,
                        "defined_at": f"{rel}:{def_line}",
                        "issue": "reachable only from test code",
                        "test_references": test_refs,
                    }
                )
    return {
        "schema": "ee.production_reachability.v1",
        "surfacesExamined": examined,
        "testOnly": findings,
    }


def load_baseline(root: Path) -> set[str]:
    path = root / BASELINE
    if not path.is_file():
        return set()
    out = set()
    for line in read(path).splitlines():
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
    report = audit(root, SURFACES)
    baseline = load_baseline(root)
    found = {f["name"] for f in report["testOnly"] if f.get("name")}

    new = sorted(found - baseline)
    fixed = sorted(baseline - found)
    report["baselineSize"] = len(baseline)
    report["newViolations"] = new
    report["baselinedButNowReachable"] = fixed

    if args.json:
        print(json.dumps(report, indent=2, sort_keys=True))
    else:
        print(
            "[reachability] predicate: a declared surface fn with NO reference outside "
            "a brace-matched #[cfg(test)] module"
        )
        print(
            f"[reachability] surfaces examined: {report['surfacesExamined']}; "
            f"test-only: {len(found)}; baseline: {len(baseline)}"
        )
        for f in report["testOnly"]:
            mark = "NEW" if f.get("name") in new else "baselined"
            print(f"   [{mark}] {f.get('name')}  ({f.get('defined_at')}) "
                  f"test refs={f.get('test_references')}")

    status = 0
    if new:
        print(f"\n[reachability] FAIL: {len(new)} surface(s) reachable only from tests "
              f"and not in {BASELINE}:", file=sys.stderr)
        for n in new:
            print(f"    {n}", file=sys.stderr)
        print("  Wire it, or add it to the baseline with a reason if it is "
              "deliberately not dispatched.", file=sys.stderr)
        status = 1
    if fixed:
        print(f"\n[reachability] FAIL: {len(fixed)} baselined entr(ies) are now "
              f"reachable; delete their line(s) from {BASELINE}:", file=sys.stderr)
        for n in fixed:
            print(f"    {n}", file=sys.stderr)
        print("  A baseline that keeps wired entries is an ignore-list.", file=sys.stderr)
        status = 1
    if status == 0:
        print("[reachability] OK -- no test-only surface beyond the accepted baseline")
    return status


# ---------------------------------------------------------------------------
# Self-test. The arms that matter are the REFUSALS and the two traps in the
# docstring: a dispatch table in the defining file must count as production, and
# production code AFTER a test module must not be swallowed.
# ---------------------------------------------------------------------------


def self_test() -> int:
    arms: list[tuple[str, bool]] = []

    def arm(label: str, ok: bool) -> None:
        arms.append((label, ok))
        print(f"  [{'ok  ' if ok else 'FAIL'}] {label}")

    with tempfile.TemporaryDirectory(prefix="reach-selftest-") as tmp:
        root = Path(tmp)
        (root / "src").mkdir()
        (root / "src" / "surface.rs").write_text(
            # A: called only from the test module -> VIOLATION
            "fn fix_only_tested() -> u32 { 1 }\n"
            # B: called by a dispatch table in THIS SAME FILE -> production
            "fn fix_in_table() -> u32 { 2 }\n"
            "pub fn dispatch(which: &str) -> u32 {\n"
            "    match which { \"t\" => fix_in_table(), _ => 0 }\n"
            "}\n"
            "\n"
            "#[cfg(test)]\n"
            "mod tests {\n"
            "    use super::*;\n"
            "    #[test]\n"
            "    fn t1() { assert_eq!(fix_only_tested(), 1); }\n"
            "    #[test]\n"
            "    fn t2() { assert_eq!(fix_in_table(), 2); }\n"
            "    #[test]\n"
            "    fn t3() { assert_eq!(fix_after_tests(), 3); }\n"
            "}\n"
            "\n"
            # C: defined AFTER a test module and called from production after it.
            # The split-on-first-cfg(test) bug would call this test-only.
            "fn fix_after_tests() -> u32 { 3 }\n"
            "pub fn later_production() -> u32 { fix_after_tests() }\n"
            "\n"
            "#[cfg(test)]\n"
            "mod more_tests {\n"
            "    #[test]\n"
            "    fn t4() { assert!(true); }\n"
            "}\n"
            "\n"
            # D: called only from tests, but NAMED in a production comment.
            # The comment is the shape that broke this gate for real: a note
            # explaining that the fn is not dispatched. A mention must not
            # count as a call, or documenting a hole closes the report of it.
            "fn fix_mentioned_in_comment() -> u32 { 4 }\n"
            "pub fn table_v2(which: &str) -> u32 {\n"
            "    // fix_mentioned_in_comment would fit here but nothing emits\n"
            "    // its finding, so an arm would be dead code.\n"
            "    match which { \"x\" => 0, _ => 1 }\n"
            "}\n"
            "\n"
            "#[cfg(test)]\n"
            "mod comment_tests {\n"
            "    use super::*;\n"
            "    #[test]\n"
            "    fn t5() { assert_eq!(fix_mentioned_in_comment(), 4); }\n"
            "}\n"
            "\n"
            # E: a real call that happens to carry a trailing comment. The
            # comment strip must not eat the call sitting before it.
            "fn fix_called_with_trailing_comment() -> u32 { 5 }\n"
            "pub fn table_v3() -> u32 { fix_called_with_trailing_comment() } // see above\n",
            encoding="utf-8",
        )
        surfaces = [("src/surface.rs", r"fix_[a-z0-9_]+")]
        rep = audit(root, surfaces)
        names = {f["name"] for f in rep["testOnly"]}

        arm("a fn called only from #[cfg(test)] is flagged", "fix_only_tested" in names)
        arm(
            "a fn dispatched from a table IN THE SAME FILE is NOT flagged",
            "fix_in_table" not in names,
        )
        arm(
            "production code AFTER a test module is seen (no split-on-first bug)",
            "fix_after_tests" not in names,
        )
        arm(
            "a fn only MENTIONED in a production comment is still flagged",
            "fix_mentioned_in_comment" in names,
        )
        arm(
            "a real call with a trailing comment is NOT flagged",
            "fix_called_with_trailing_comment" not in names,
        )
        arm("all five surface fns were examined", rep["surfacesExamined"] == 5)

        ranges = test_line_ranges(read(root / "src" / "surface.rs"))
        arm("every test module is found, not just the first", len(ranges) == 3)
        arm(
            "the ranges do not overlap the production tail",
            all(not (lo <= 1 <= hi) for lo, hi in ranges),
        )

        # BASELINE DIRECTION 2: an entry that is now reachable must be reported.
        (root / "scripts").mkdir()
        (root / BASELINE).write_text(
            "# planted\nfix_only_tested\nfix_in_table\n", encoding="utf-8"
        )
        base = load_baseline(root)
        found = names
        arm("a baselined-but-now-reachable entry is detected",
            "fix_in_table" in (base - found))
        arm("a genuinely test-only baselined entry is NOT reported as fixed",
            "fix_only_tested" not in (base - found))

        # A surface file that does not exist must be reported, not silently skipped.
        rep2 = audit(root, [("src/nope.rs", r"fix_[a-z0-9_]+")])
        arm(
            "a missing surface file is reported, not silently passed",
            any(f.get("issue") == "surface file not found" for f in rep2["testOnly"]),
        )

    passed = sum(1 for _, ok in arms if ok)
    print(f"\n[reachability] self-test: {passed}/{len(arms)} arms passed")
    return 0 if passed == len(arms) else 1


if __name__ == "__main__":
    raise SystemExit(main())
