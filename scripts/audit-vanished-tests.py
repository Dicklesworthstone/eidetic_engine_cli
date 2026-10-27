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

NOT WIRED INTO verify.sh, DELIBERATELY, and now with the price measured so the
decision is cheap rather than a judgement call. Both of
scripts/verify-budget.toml's guard arms are at EXACTLY zero headroom:
`verify_budget_manifest_has_p50_and_regression_factor_for_every_stage` allows 27
unmeasured stages and there are 27, and the readiness ceiling is 608 s with the
manifest documenting `total_expected_seconds = 608`. A full run of this tool
costs about 160 s, of which 154 s is `git log --numstat` over the range and only
3.4 s is scanning the tree (measured 2026-10-25 -- the tree scan is NOT the
bottleneck, which is the opposite of the obvious guess). So wiring it means
moving a declared ceiling by ~160 s AND the Rust constant beside it, which
bd-xh5zd holds open as a decision. It also wants a commit RANGE, a different
shape from the per-tree gates verify.sh runs. It stays an operator tool, in the
same category as scripts/audit-set-e-dead-handlers.py.

FINDINGS NO LONGER EXPIRE (bd-0rkti). Two changes, and they only work together:
the window's lower bound is a COMMIT recorded in
scripts/vanished-tests-dispositions.txt, so it can only grow as HEAD advances;
and every surfaced name must carry a disposition in that file, so this exits
NONZERO on an untriaged name. Before, `--since` defaulted to a rolling
`60 days ago` and the tool exited 0 while printing findings -- so a name nobody
triaged aged out of the window and the report got cleaner than the tree. Six
`capabilities_report_*` names were four days from expiring that way.

The file is SHRINK-ONLY in both directions: an undispositioned name fails, and
so does a row whose name is no longer surfaced, because with a growing window
the only way that happens is that the test came back.

USAGE
    scripts/audit-vanished-tests.py                     scan from the recorded
                                                        baseline commit
    scripts/audit-vanished-tests.py --since 2026-09-01  scan from a date instead
    scripts/audit-vanished-tests.py --since <sha>        scan from a commit
    scripts/audit-vanished-tests.py --net 200            only bigger deletions
    scripts/audit-vanished-tests.py --advisory           report without failing
    scripts/audit-vanished-tests.py --json               machine-readable
    scripts/audit-vanished-tests.py --self-test          16 arms, pure functions
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

# Dispositions, and the baseline commit the window starts from (bd-0rkti).
#
# WHY THIS FILE EXISTS. Before it, this tool's findings EXPIRED. `--since`
# defaulted to a rolling `60 days ago`, nothing recorded which findings had been
# triaged, and so an untriaged deletion did not stay surfaced -- it aged out and
# the report got CLEANER while the tree did not. Measured 2026-10-25: a run on
# 2026-10-19 with `--since 2026-09-05` reported 12 names, and a default run that
# day reported 11; the two windows are not nested, so the sets differed in BOTH
# directions, and `05020689c` was about to leave the window entirely on
# 2026-10-27 with six names nobody had ever dispositioned.
#
# A detector whose output shrinks with the CLOCK rather than with REPAIR is not
# a ratchet. Two changes make it one: the window's lower bound is a recorded
# commit, so it can only grow; and every surfaced name must carry a
# disposition, so an untriaged name is the only thing that stands out.
DISPOSITIONS_PATH = "scripts/vanished-tests-dispositions.txt"
BASELINE_DIRECTIVE = "# baseline-commit:"
VALID_DISPOSITIONS = ("retired", "replaced", "owed")


def parse_dispositions(text: str) -> tuple[dict[str, tuple[str, str]], str | None]:
    """(name -> (disposition, reason), baseline_commit_or_None).

    Format is one `name<TAB>disposition<TAB>reason` row per line, `#` comments,
    and one `# baseline-commit: <sha>` directive. Raises ValueError on an
    unknown disposition or a row missing its reason, because a disposition
    without a stated why is the thing this file exists to prevent.
    """
    dispositions: dict[str, tuple[str, str]] = {}
    baseline: str | None = None
    for lineno, raw in enumerate(text.split("\n"), start=1):
        line = raw.rstrip()
        if line.startswith(BASELINE_DIRECTIVE):
            baseline = line[len(BASELINE_DIRECTIVE):].strip() or None
            continue
        if not line.strip() or line.lstrip().startswith("#"):
            continue
        parts = line.split("\t")
        if len(parts) < 3 or not parts[2].strip():
            raise ValueError(
                f"{DISPOSITIONS_PATH}:{lineno}: expected "
                f"name<TAB>disposition<TAB>reason, got {line!r}"
            )
        name, disposition, reason = parts[0].strip(), parts[1].strip(), "\t".join(parts[2:]).strip()
        if disposition not in VALID_DISPOSITIONS:
            raise ValueError(
                f"{DISPOSITIONS_PATH}:{lineno}: disposition {disposition!r} is not one of "
                f"{', '.join(VALID_DISPOSITIONS)}"
            )
        dispositions[name] = (disposition, reason)
    return dispositions, baseline


def load_dispositions(root: str, path: str | None = None) -> tuple[dict[str, tuple[str, str]], str | None]:
    # `--dispositions` exists so the POSITIVE CONTROL is repeatable by anyone:
    # point it at a copy of the real file with one row removed and the gate must
    # fail on that name, against the real tree and real git history. Without the
    # override the only way to run that control is to edit the tracked file and
    # remember to put it back, which is how a control becomes a thing nobody
    # reruns.
    path = path or os.path.join(root, DISPOSITIONS_PATH)
    try:
        with open(path, encoding="utf-8") as handle:
            return parse_dispositions(handle.read())
    except FileNotFoundError:
        return {}, None


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


def is_commit(ref: str) -> bool:
    """True when `ref` names a commit this repository holds.

    Decides whether `--since` is a REVISION or a DATE, and it has to be decided
    rather than guessed: `git log --since=<sha>` does not error -- it parses the
    sha as an (absurd) date and silently scans everything. That is the failure
    mode that would make a baseline commit look like it was working while the
    window had actually become the whole history.
    """
    if not ref:
        return False
    probe = subprocess.run(
        ["git", "rev-parse", "--verify", "--quiet", f"{ref}^{{commit}}"],
        capture_output=True,
        text=True,
    )
    return probe.returncode == 0 and bool(probe.stdout.strip())


def big_deletions(since: str, net: int) -> list[tuple[int, str, str]]:
    """(net_deleted, sha, path) for commits deleting >= net lines from one .rs.

    `since` is either a COMMIT, scanned as `<since>..HEAD` -- a window that can
    only grow as HEAD advances -- or a DATE, scanned as `--since=<date>`, the
    rolling window that lets findings expire.
    """
    selector = [f"{since}..HEAD"] if is_commit(since) else [f"--since={since}"]
    log = _git(["log", *selector, "--numstat", "--format=@@%H"])
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


def audit(root: str, since: str, net: int,
          dispositions: dict[str, tuple[str, str]] | None = None) -> dict:
    dispositions = dispositions or {}
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
    surfaced = {name for f in findings for name in f["vanished"]}
    undispositioned = sorted(surfaced - set(dispositions))
    # SHRINK-ONLY, the MESH_EMITTED_CODES_WITHOUT_FIXTURE idiom. A disposition
    # for a name the scan no longer surfaces is STALE and must fail, or the file
    # becomes a graveyard nobody revisits. With a baseline COMMIT the window
    # only grows, so the sole way a name stops being surfaced is that it came
    # back -- in which case its disposition is a statement about the past.
    stale = sorted(set(dispositions) - surfaced)
    return {
        "schema": "ee.audit.vanished_tests.v1",
        "status": "pass" if not undispositioned and not stale else "findings",
        "since": since,
        "sinceIsCommit": is_commit(since),
        "netThreshold": net,
        "presentFnNames": len(present),
        "findings": findings,
        # OCCURRENCES, not names: two commits can each delete the same name, and
        # `surfaced` is a SET. Reporting only the sum put two numbers on one line
        # that cannot be subtracted from each other, which invites a reader to
        # try. Both are printed, labelled.
        "vanishedTotal": sum(len(f["vanished"]) for f in findings),
        "distinctNames": len(surfaced),
        "dispositioned": {
            name: {"disposition": dispositions[name][0], "reason": dispositions[name][1]}
            for name in sorted(surfaced & set(dispositions))
        },
        "undispositioned": undispositioned,
        "staleDispositions": stale,
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

    # --- bd-0rkti: the dispositions file and the non-expiring window ---

    # 10. the format parses, comments and blanks tolerated
    parsed, baseline = parse_dispositions(
        "# a comment\n"
        "# baseline-commit: deadbeef\n"
        "\n"
        "alpha\tretired\tsuperseded by beta\n"
        "gamma\towed\tnothing asserts this yet\n"
    )
    arm("dispositions parse, with the baseline directive read separately",
        parsed == {"alpha": ("retired", "superseded by beta"),
                   "gamma": ("owed", "nothing asserts this yet")}
        and baseline == "deadbeef")

    # 11. A DISPOSITION WITHOUT A REASON IS REFUSED. The whole point of the file
    #     is that a reader can tell a triaged name from an untriaged one; a bare
    #     `name<TAB>retired` would let someone clear the gate without saying why,
    #     which is the gate's own failure mode written into its input.
    try:
        parse_dispositions("alpha\tretired\n")
        reasonless_ok = False
    except ValueError:
        reasonless_ok = True
    arm("a disposition with no reason is refused", reasonless_ok)

    # 12. ...and an unknown disposition word is refused, so the vocabulary
    #     cannot be extended by typo.
    try:
        parse_dispositions("alpha\tprobably-fine\tlooks ok\n")
        vocab_ok = False
    except ValueError:
        vocab_ok = True
    arm("an unknown disposition word is refused", vocab_ok)

    # 13. THE POSITIVE CONTROL FOR THE GATE. An undispositioned name must be
    #     reported as such. Without this arm the dispositions feature could be
    #     "everything is dispositioned" and every other arm would still pass.
    arm("a surfaced name with no row is undispositioned",
        sorted({"alpha", "beta"} - set({"alpha": ("retired", "r")})) == ["beta"])

    # 14. SHRINK-ONLY, the other direction: a row for a name that is no longer
    #     surfaced is STALE. Without this a cleared finding's row stays forever
    #     and the file stops describing the tree.
    arm("a row for a name no longer surfaced is stale",
        sorted(set({"alpha": ("retired", "r"), "ghost": ("retired", "r")}) - {"alpha"})
        == ["ghost"])

    # 15. THE REVISION/DATE DISCRIMINATION, which is the whole non-expiring
    #     mechanism. `git log --since=<sha>` does NOT error -- it reads the sha
    #     as a date and scans everything -- so a baseline commit mistaken for a
    #     date would look like it worked while measuring the full history.
    head_is_commit = is_commit("HEAD")
    date_is_not = not is_commit("60 days ago")
    arm("HEAD is recognised as a commit and a date is not",
        head_is_commit and date_is_not)

    # 16. ...and the empty string is not a commit, so a blank baseline
    #     directive falls back to the date path rather than scanning nothing.
    arm("an empty ref is not a commit", not is_commit(""))

    passed = sum(1 for _, ok in arms if ok)
    print(f"\n[vanished-tests] self-test: {passed}/{len(arms)} arms passed")
    return 0 if passed == len(arms) else 1


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--since", default=None,
                        help="commit or date; defaults to the recorded baseline commit")
    parser.add_argument("--net", type=int, default=120)
    parser.add_argument("--json", action="store_true")
    parser.add_argument("--self-test", action="store_true")
    parser.add_argument("--root", default=".")
    parser.add_argument("--advisory", action="store_true",
                        help="report undispositioned names without failing")
    parser.add_argument("--dispositions", default=None,
                        help=f"dispositions file (default: {DISPOSITIONS_PATH} under --root)")
    args = parser.parse_args()

    if args.self_test:
        return self_test()

    try:
        dispositions, baseline = load_dispositions(args.root, args.dispositions)
    except ValueError as error:
        print(f"[vanished-tests] BLOCKED: {error}")
        return 2

    # `60 days ago` survives only as the fallback for a tree with no recorded
    # baseline. It is the expiring behaviour this bead was filed about, so it is
    # announced rather than taken silently.
    since = args.since or baseline or "60 days ago"
    if args.since is None and baseline is None:
        print("[vanished-tests] NO BASELINE COMMIT RECORDED in "
              f"{DISPOSITIONS_PATH}; falling back to a ROLLING 60-day window, "
              "from which findings can expire")

    report = audit(args.root, since, args.net, dispositions)
    if args.json:
        print(json.dumps(report, sort_keys=True))
    else:
        print(f"[vanished-tests] since={report.get('since')}"
              f"{' (commit)' if report.get('sinceIsCommit') else ' (date)'} "
              f"net>={report.get('netThreshold')} "
              f"present_fn_names={report.get('presentFnNames')}")
        if report.get("status") == "blocked":
            print(f"[vanished-tests] BLOCKED: {report.get('error')}")
            return 2
        dispositioned = report.get("dispositioned") or {}
        for f in report.get("findings", []):
            print(f"\n  {f['commit']}  -{f['netDeleted']} lines  {f['path']}")
            print(f"    {f['subject']}")
            for name in f["vanished"]:
                record = dispositioned.get(name)
                if record:
                    print(f"      {record['disposition']:<9} {name}")
                    print(f"                  {record['reason']}")
                else:
                    print(f"      UNDISPOSITIONED  {name}")
            if f["commitAddedPaths"]:
                print("    that commit ADDED (check for a replacement):")
                for p in f["commitAddedPaths"]:
                    print(f"      + {p}")
        total = report.get("vanishedTotal", 0)
        print(f"\n[vanished-tests] {total} deletion(s) of "
              f"{report.get('distinctNames')} distinct name(s) across "
              f"{len(report.get('findings', []))} commit(s); of the distinct "
              f"names {len(dispositioned)} dispositioned, "
              f"{len(report.get('undispositioned') or [])} NOT")
        for name in report.get("undispositioned") or []:
            print(f"  UNDISPOSITIONED  {name}")
        for name in report.get("staleDispositions") or []:
            print(f"  STALE DISPOSITION  {name}  (no longer surfaced; the name "
                  f"is present again, so delete this row)")

    # A GATE now, not a census. Before bd-0rkti this returned 0 while printing
    # findings, so nothing ever forced the triage and names expired untriaged.
    undispositioned = report.get("undispositioned") or []
    stale = report.get("staleDispositions") or []
    if (undispositioned or stale) and not args.advisory:
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
