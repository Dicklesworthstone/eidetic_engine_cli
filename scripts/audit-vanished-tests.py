#!/usr/bin/env python3
"""READ-ONLY census of `#[test]` functions that a commit deleted and nothing
restored (bd-z1dus).

WHAT THE DEFECT IS. A deleted test is the only coverage loss that leaves NO
signal anywhere. The build stays green, clippy stays green, the suite passes,
and the count of passing tests even RISES as other work lands. Nothing in this
tree compares one commit's test surface against its parent's, so a test can
disappear in a large refactor and never be noticed again.

MEASURED 2026-10-18, which is why this exists rather than being speculative:
eight `#[test]` functions were found deleted and never restored --

    src/core/learn.rs      7   lost when 5c3392996 truncated the file
                               mid-expression; 1679e0fa4 restored the tail
                               WITHOUT them. Among them a redaction assertion
                               (sensitive evidence ids must not appear in
                               proposal JSON) and two refusal-path tests.
    src/core/context.rs    1   3870f412d extracted 157 tests into
                               context_test_module.rs and 156 arrived.

RELOCATION IS NOT LOSS, and conflating them is the easy mistake: the same
sweep first reported 157 lost from context.rs because it only looked in the
file the commit touched. A test that moved to a sibling module is present.
So the question this asks is "does this name exist ANYWHERE in the tree now",
not "is it still in that file".

REPLACEMENT IS NOT LOSS EITHER, and this tool CANNOT decide that for you.
86380c6e8 removed two Rust contract tests and, in the same commit, added
scripts/check-rch-remediation-guidance.py covering both invariants with more
cases than the tests it replaced. A name-based sweep calls those lost; a human
reading the commit calls them consolidated. So every finding below prints what
its removing commit ADDED, and the verdict stays with the reader.

NOT WIRED INTO verify.sh, DELIBERATELY. scripts/verify-budget.toml's
non-benchmark stages total essentially all of a fixed ceiling, so a new stage
would have to displace an existing one; and this check wants a commit RANGE,
which is a different shape from the per-tree gates verify.sh runs. It is an
operator tool, in the same category as scripts/audit-set-e-dead-handlers.py.

USAGE
    scripts/audit-vanished-tests.py                    scan the last 60 days
    scripts/audit-vanished-tests.py --since 2026-09-01  scan from a date
    scripts/audit-vanished-tests.py --net 200           only bigger deletions
    scripts/audit-vanished-tests.py --json              machine-readable
    scripts/audit-vanished-tests.py --self-test         9 arms, pure functions
"""
import argparse
import glob
import json
import os
import re
import subprocess
import sys

# `#[test]` followed by an optional `async` and the fn name. Tolerates
# attributes between the marker and the fn (#[should_panic], #[ignore]).
TEST_RE = re.compile(r"#\[test\]\s*(?:#\[[^\]]*\]\s*)*(?:pub\s+)?(?:async\s+)?fn\s+([a-z_0-9]+)")
ANY_FN_RE = re.compile(r"fn\s+([a-z_0-9]+)")


def strip_line_comments(text: str) -> str:
    """Drop `//` comment tails, respecting string literals.

    A `#[test]` inside a doc comment or a commented-out block is not a test.
    This repo has had three gates decide on prose for want of this
    (check-production-reachability, check-step-assertion-isolation,
    check-shard-coverage), so it is handled here from the start rather than
    added after the first wrong answer.
    """
    out = []
    for line in text.split("\n"):
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
            elif line.startswith("//", i):
                cut = i
                break
        out.append(line[:cut])
    return "\n".join(out)


def test_names(text: str) -> set[str]:
    """`#[test]` fn names in `text`, comments excluded."""
    return set(TEST_RE.findall(strip_line_comments(text)))


def all_fn_names(text: str) -> set[str]:
    """Every fn name, comments excluded. Used for the PRESENT set, because a
    relocated test may have lost its `#[test]` marker to a macro or a harness
    wrapper and is still not missing."""
    return set(ANY_FN_RE.findall(strip_line_comments(text)))


def vanished(before: set[str], present: set[str]) -> list[str]:
    """Names in `before` that no longer appear in `present`.

    EMPTY-WORLD GUARD: an empty `present` means the tree scan produced
    nothing, not that every test was deleted. Returning `before` there would
    report total loss from an instrument failure, which is the worst possible
    direction for this particular check -- it would look like a catastrophe
    and be a bug. Callers get an exception instead.
    """
    if not present:
        raise ValueError(
            "present-name set is empty: the tree scan found no functions at all, "
            "so no loss can be inferred (instrument failure, not total deletion)"
        )
    return sorted(before - present)


def _git(args: list[str]) -> str:
    return subprocess.run(["git", *args], capture_output=True, text=True).stdout


def present_names(root: str) -> set[str]:
    names: set[str] = set()
    for pattern in ("src/**/*.rs", "tests/**/*.rs", "benches/**/*.rs", "crates/**/*.rs"):
        for path in glob.glob(os.path.join(root, pattern), recursive=True):
            try:
                names |= all_fn_names(open(path, encoding="utf-8", errors="replace").read())
            except OSError:
                continue
    return names


def big_deletions(since: str, net: int) -> list[tuple[int, str, str]]:
    """(net_deleted, sha, path) for commits deleting >= net lines from one .rs."""
    log = _git(["log", f"--since={since}", "--numstat", "--format=@@%H"])
    out: list[tuple[int, str, str]] = []
    sha = None
    for line in log.split("\n"):
        if line.startswith("@@"):
            sha = line[2:]
            continue
        parts = line.split("\t")
        if len(parts) != 3 or not parts[2].endswith(".rs"):
            continue
        try:
            added, deleted = int(parts[0]), int(parts[1])
        except ValueError:
            continue
        if deleted - added >= net and sha:
            out.append((deleted - added, sha, parts[2]))
    out.sort(reverse=True)
    return out


def audit(root: str, since: str, net: int) -> dict:
    present = present_names(root)
    findings = []
    for net_deleted, sha, path in big_deletions(since, net):
        before = test_names(_git(["show", f"{sha}^:{path}"]))
        if not before:
            continue
        try:
            lost = vanished(before, present)
        except ValueError as error:
            return {"schema": "ee.audit.vanished_tests.v1", "status": "blocked",
                    "error": str(error)}
        if not lost:
            continue
        added_paths = [
            l for l in _git(["show", "--name-only", "--diff-filter=A", "--format=", sha]).split("\n") if l
        ]
        findings.append({
            "commit": sha[:9],
            "path": path,
            "netDeleted": net_deleted,
            "testsBefore": len(before),
            "vanished": lost,
            "commitAddedPaths": added_paths[:12],
            "subject": _git(["log", "-1", "--format=%s", sha]).strip()[:90],
        })
    return {
        "schema": "ee.audit.vanished_tests.v1",
        "status": "pass" if not findings else "findings",
        "since": since,
        "netThreshold": net,
        "presentFnNames": len(present),
        "findings": findings,
        "vanishedTotal": sum(len(f["vanished"]) for f in findings),
    }


def self_test() -> int:
    arms: list[tuple[str, bool]] = []

    def arm(label: str, ok: bool) -> None:
        arms.append((label, ok))
        print(f"  [{'ok  ' if ok else 'FAIL'}] {label}")

    # 1. the basic shape
    before = test_names("#[test]\nfn alpha() {}\n#[test]\nfn beta() {}\n")
    arm("two #[test] fns are found", before == {"alpha", "beta"})

    # 2. RELOCATION IS NOT LOSS -- the mistake that first reported 157 lost
    #    from context.rs when 156 had moved to a sibling module.
    arm("a test that moved elsewhere is not reported",
        vanished({"alpha", "beta"}, {"alpha", "beta", "unrelated"}) == [])

    # 3. a real deletion IS reported
    arm("a test absent from the whole tree is reported",
        vanished({"alpha", "beta"}, {"alpha", "unrelated"}) == ["beta"])

    # 4. EMPTY-WORLD GUARD. An empty present-set means the scan broke; it must
    #    raise rather than report every test lost.
    try:
        vanished({"alpha"}, set())
        empty_ok = False
    except ValueError:
        empty_ok = True
    arm("an empty present-set raises instead of reporting total loss", empty_ok)

    # 5. COMMENTS ARE NOT CODE. A commented-out test must not count as having
    #    existed, or removing dead commented code reads as deleting a test.
    commented = test_names("// #[test]\n// fn ghost() {}\n#[test]\nfn real() {}\n")
    arm("a commented-out #[test] is not counted", commented == {"real"})

    # 6. ...and the converse: a trailing comment must not hide a real test.
    trailing = test_names("#[test]  // bd-1234: keep\nfn kept() {}\n")
    arm("a real test with a trailing comment is still counted",
        trailing == {"kept"})

    # 7. async and intervening attributes
    fancy = test_names("#[test]\n#[should_panic]\nasync fn spicy() {}\n")
    arm("async fns and intervening attributes are found", fancy == {"spicy"})

    # 8. the PRESENT set uses any fn name, so a relocated test that lost its
    #    marker to a macro wrapper still counts as present.
    arm("present-set matching does not require the #[test] marker",
        "wrapped" in all_fn_names("fn wrapped() {}"))

    # 9. a '#' inside a string must not truncate the line
    arm("a quoted // does not eat the line",
        test_names('#[test]\nfn urls() { let _ = "https://x"; }\n') == {"urls"})

    passed = sum(1 for _, ok in arms if ok)
    print(f"\n[vanished-tests] self-test: {passed}/{len(arms)} arms passed")
    return 0 if passed == len(arms) else 1


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--since", default="60 days ago")
    parser.add_argument("--net", type=int, default=120)
    parser.add_argument("--json", action="store_true")
    parser.add_argument("--self-test", action="store_true")
    parser.add_argument("--root", default=".")
    args = parser.parse_args()

    if args.self_test:
        return self_test()

    report = audit(args.root, args.since, args.net)
    if args.json:
        print(json.dumps(report, sort_keys=True))
    else:
        print(f"[vanished-tests] since={report.get('since')} "
              f"net>={report.get('netThreshold')} "
              f"present_fn_names={report.get('presentFnNames')}")
        if report.get("status") == "blocked":
            print(f"[vanished-tests] BLOCKED: {report.get('error')}")
            return 2
        for f in report.get("findings", []):
            print(f"\n  {f['commit']}  -{f['netDeleted']} lines  {f['path']}")
            print(f"    {f['subject']}")
            for name in f["vanished"]:
                print(f"      VANISHED  {name}")
            if f["commitAddedPaths"]:
                print("    that commit ADDED (check for a replacement):")
                for p in f["commitAddedPaths"]:
                    print(f"      + {p}")
        total = report.get("vanishedTotal", 0)
        print(f"\n[vanished-tests] {total} name(s) vanished across "
              f"{len(report.get('findings', []))} commit(s)")
    # A census, not a gate: exit 0 whenever the measurement completed.
    return 0


if __name__ == "__main__":
    sys.exit(main())
