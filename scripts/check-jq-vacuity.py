#!/usr/bin/env python3
"""Flag jq assertions whose `//` default or negation makes ABSENCE indistinguishable
from the asserted value, so the assertion passes on data it was written to reject.

bd-o8e1n fixed six sites and recorded the rule. This is the detector that found
them, rebuilt as a committed instrument (bd-9h1z5) because the original lived in
a scratchpad and scratchpads are wiped. The class was clean by inspection and
stayed clean only by luck.

THE THREE SHAPES, and why each is a defect
------------------------------------------
jq's alternative operator `//` fires on `false` as well as on `null`, which is
the root of all three. A literal `false` -- very often the exact value a clause
exists to assert -- is replaced by the default before the comparison runs.

    op ==  and default == want    ->  absence PASSES        (vacuous)
    op !=  and default != want    ->  absence PASSES        (vacuous)
    (X // <truthy>) == false      ->  UNSATISFIABLE, nothing reaches the test

The third is the nastiest, because it fails CONSTANTLY rather than passing
constantly: false, true, null and absent all yield the same verdict, so its
permanent red reads as a product defect and gets investigated as one.

THE LEGITIMATE IDIOM IS ADJACENT, and is deliberately not flagged

    ((.data.persisted // false) == true)     <- correct: absent -> false -> FAILS

Same operator, same shape, opposite verdict on absence. A detector that cannot
tell these apart would flag the whole corpus and be switched off in a week.

VALIDATION (--self-test, and it is a gate)
------------------------------------------
The validation is the part worth preserving, so it ships executable rather than
as a claim in a comment:

  - Known positive: the pre-fix line at
    `git show fd8e4eb4d^:scripts/e2e_capture.sh:792`. A guard that cannot find
    the bug we already fixed would mean nothing. Carried as an embedded fixture
    AND, when a .git is present, re-read from history -- best-effort, because a
    gate that assumes .git fails in a tarball for a reason unrelated to its job.
  - Six hand-written known-bad shapes, all flagged, INCLUDING four a grep
    version missed: indexed paths `.items[0]`, bracket keys `.["odd-key"]`,
    doubled parens, and a comparison split across a newline.
  - Negative arms: the legitimate `// false) == true` idiom, a `//` with no
    comparison, and a COMMENT documenting the anti-pattern. That last one is not
    hypothetical -- an earlier version of this detector reported the comment
    bd-o8e1n left behind explaining the bug as an instance of the bug.
  - A count arm, so an empty corpus cannot pass as a clean one.

Comment lines are blanked before matching. `#` starts a comment only at the
start of a word, which is the rule in both shell and jq, so `marker#not-a-key`
keeps its text while a real trailing comment loses it.

Written in Python `re`, not grep: BSD and GNU grep disagree on \\b and friends,
so a grep guard reaches different verdicts on a Mac and in CI. That is the same
environment-dependent-oracle defect class as a fixture that reads differently
per host.

KNOWN BLIND SPOTS, carried verbatim from bd-9h1z5
-------------------------------------------------
Literal defaults and comparands only; a variable or expression is invisible.
Comparisons split by an intervening pipe or function call are missed. A `//`
inside a string literal would be a false positive (none observed). A guard that
overstates its coverage recreates the defect it hunts.

POSTURE: --json REPORTS, it does not fail
-----------------------------------------
Hits are printed and the exit status stays 0, per bd-9h1z5's own suggested
shape, so a false positive cannot red the board on this check's first
appearance. Promote to failing after one clean full-suite observation -- and
when you do, ratchet on a baseline SET, never a count.

--self-test is the opposite: it fails hard. It compares the detector against
planted fixtures only, never against repository content, so it cannot red
because somebody edited an e2e script. It protects the instrument.
"""

from __future__ import annotations

import argparse
import json
import re
import subprocess
import sys
from pathlib import Path

LITERAL = r"""(?:true|false|null|-?\d+(?:\.\d+)?|"(?:[^"\\]|\\.)*"|'(?:[^'\\]|\\.)*')"""

# `// <literal>` then any run of closing parens, then a comparison to a literal.
# `\s*` throughout and no re.M anchors, so a comparison split across a newline
# matches exactly as one on a single line does.
ALT_CMP = re.compile(
    r"//\s*(?P<default>" + LITERAL + r")" r"(?:\s*\))*" r"\s*(?P<op>==|!=)\s*" r"(?P<want>" + LITERAL + r")",
    re.VERBOSE,
)

# Reported for context only; never used to decide. Handles dotted names, index
# steps `[0]` and bracket keys `["odd-key"]`.
PATH = re.compile(r"\.(?:[A-Za-z_]\w*|\[[^\]]*\])(?:\.[A-Za-z_]\w*|\[[^\]]*\])*")

TRUTHY_EXCLUDED = {"false", "null"}


def blank_comments(text: str) -> str:
    """Blank `#`-to-end-of-line, where `#` begins a word.

    Both shell and jq take `#` as a comment, and the jq programs on this surface
    are single-quoted shell strings carrying jq comments inside them -- so a
    blanker that tracked shell quoting would protect exactly the comments that
    need blanking. Word-start is the rule both languages actually use.

    Newlines are preserved so reported line numbers stay true.
    """
    out = []
    for line in text.split("\n"):
        hit = None
        for match in re.finditer(r"#", line):
            start = match.start()
            if start == 0 or line[start - 1].isspace():
                hit = start
                break
        out.append(line if hit is None else line[:hit])
    return "\n".join(out)


def classify(default: str, op: str, want: str) -> str | None:
    """Name the shape, or None when the assertion discriminates absence."""
    if op == "==" and default == want:
        return "vacuous-eq"
    if op == "==" and want == "false" and default not in TRUTHY_EXCLUDED:
        return "unsatisfiable"
    if op == "!=" and default != want:
        return "vacuous-ne"
    return None


def find_hits(text: str, label: str = "<text>") -> list[dict]:
    """Every vacuity hit in `text`, after comment blanking."""
    body = blank_comments(text)
    hits = []
    for match in ALT_CMP.finditer(body):
        shape = classify(match.group("default"), match.group("op"), match.group("want"))
        if shape is None:
            continue
        line = body.count("\n", 0, match.start()) + 1
        before = body[max(0, match.start() - 120) : match.start()]
        paths = PATH.findall(before)
        hits.append(
            {
                "file": label,
                "line": line,
                "shape": shape,
                "default": match.group("default"),
                "op": match.group("op"),
                "want": match.group("want"),
                "subject": paths[-1] if paths else None,
                "excerpt": " ".join(match.group(0).split()),
            }
        )
    return hits


# --------------------------------------------------------------------------
# JUDGED SITES. bd-o8e1n recorded its six remaining hits as judged-legitimate
# per site; this is the same discipline for the wider surface this detector
# scans (285 shell files, not just the e2e scripts).
#
# WHY A JUDGEMENT SET AND NOT A TIGHTER REGEX: the three shapes are decidable
# from the text, but their SEVERITY is directional and is not. The identical
# shape is a defect in an acceptance assertion, where it admits a weaker
# witness, and harmless-to-correct in a sentinel decode, an inclusion filter or
# an error-emitting validator, where it errs strict. A regex that tried to tell
# those apart would be guessing at intent; a reader can simply say which it is,
# once, with the reason attached.
#
# Keyed on file + shape + normalised excerpt, NOT line number, so an edit above
# a judged site does not resurface it. Several entries cover more than one
# occurrence of one idiom in one file; `occurrences` records how many were
# reviewed, so a SIXTH copy of a five-copy idiom reports as unjudged.
#
# Judged 2026-10-22 at ec3d369e2 by reading each site in context.
# --------------------------------------------------------------------------

JUDGED: dict[tuple[str, str, str], dict] = {
    (
        "scripts/check-local-cargo-tripwire.sh",
        "vacuous-eq",
        '// "-") == "-"',
    ): {
        "occurrences": 5,
        "reason": "SENTINEL DECODE, not an assertion: `if (.[12] // \"-\") == \"-\" then null "
        "else .[12] end` maps both an absent field and the literal \"-\" placeholder to "
        "null. Absence and the sentinel are MEANT to be indistinguishable here -- that is "
        "the decode's whole job -- so the shape is correct by construction.",
    },
    (
        "scripts/audit_install_pipeline.sh",
        "vacuous-eq",
        "// false) == false",
    ): {
        "occurrences": 2,
        "reason": "A COMPUTED JSON FIELD (latest_release_published), not an acceptance "
        "assertion. `((.isDraft // false) == false)` treats a release whose isDraft the "
        "GitHub API did not report as not-a-draft, which is a deliberate default, and the "
        "expression still rejects isDraft:true.",
    },
    (
        "scripts/closure-lint.sh",
        "vacuous-ne",
        '// "") != "closed"',
    ): {
        "occurrences": 1,
        "reason": "AN INCLUSION FILTER that errs strict. `select((.status // \"\") != "
        '"closed")` admits a bead carrying no status field, so an unknown-status bead gets '
        "LINTED rather than skipped. Absence widening the population checked is the safe "
        "direction for a linter.",
    },
    (
        "scripts/plan-drift.sh",
        "vacuous-ne",
        '// "") != "closed"',
    ): {
        "occurrences": 1,
        "reason": "Same inclusion-filter shape as closure-lint.sh and the same strict "
        "direction: a bead with no status is treated as open and therefore checked.",
    },
    (
        "scripts/e2e_overhaul/swarm_incident_recovery_actions.sh",
        "vacuous-ne",
        '// "") != "observe"',
    ): {
        "occurrences": 1,
        "reason": "AN ERROR-EMITTING VALIDATOR, so the polarity is inverted: the condition "
        "guards a failure message, and an absent `.kind` yields \"\" != \"observe\" -> true "
        "-> the fixture is REJECTED. Absence fails closed, which is what the check wants.",
    },
    (
        "scripts/e2e_output_governor.sh",
        "vacuous-eq",
        "// null) == null",
    ): {
        "occurrences": 2,
        "reason": "AN INTENTIONAL ABSENCE ASSERTION: the message is \"zero-cost path leaves "
        "envelope unstamped\", so absence IS the asserted property and `(X // null) == null` "
        "is the ordinary jq idiom for it. The residual hole -- `//` also firing on `false` -- "
        "is unreachable for a token count.",
    },
    (
        "scripts/e2e_overhaul/swarm_incident_recovery_actions.sh",
        "vacuous-eq",
        "// null) == null",
    ): {
        "occurrences": 1,
        "reason": "Intentional absence assertion: the validator requires an action to carry "
        "a command or a manualStep, so it must detect that manualStep is absent.",
    },
    (
        "scripts/e2e_overhaul/sx7_adaptive_budget.sh",
        "vacuous-eq",
        "// null) == null",
    ): {
        "occurrences": 1,
        "reason": "Intentional absence assertion: \"explicit --max-tokens disables the "
        "adaptive override\" is asserted by the budget schema field being absent.",
    },
    (
        "scripts/e2e_recall_hooks.sh",
        "vacuous-ne",
        '// "") != "src/graph/mod.rs"',
    ): {
        "occurrences": 1,
        "reason": "The default is CORRECT rather than merely strict: an item carrying no "
        "anchor path genuinely is not the graph-only memory this assertion excludes.",
    },
    (
        "scripts/e2e_reach.sh",
        "vacuous-eq",
        '// "advisory") == "advisory"',
    ): {
        "occurrences": 1,
        "verdict": "GENUINE, MILD",
        "reason": "THE ONE REAL FINDING of this pass, left in place rather than fixed "
        "silently. `all(...; (.severity? // \"advisory\") == \"advisory\")` is an acceptance "
        "assertion whose purpose is to constrain severity, and a finding that emits NO "
        "severity field satisfies it -- so were the product to stop emitting severity "
        "altogether, the assertion would still pass. Mild because the surrounding `jq -e` "
        "guard proves at least one config_ finding exists, so the `all()` is not vacuous over "
        "an empty list, and because advisory is in fact the product default. Fixing it means "
        "asserting the field is PRESENT and equal, which is a change to an e2e assertion and "
        "wants its own bead rather than a drive-by edit inside a detector commit.",
    },
}


def judge(hit: dict) -> dict | None:
    return JUDGED.get((hit["file"], hit["shape"], hit["excerpt"]))


def audit(root: Path) -> dict:
    files = sorted(
        set(root.glob("scripts/**/*.sh")) | set(root.glob("scripts/**/*.bash")),
        key=lambda p: str(p),
    )
    hits: list[dict] = []
    for path in files:
        try:
            text = path.read_text(encoding="utf-8", errors="replace")
        except OSError:
            continue
        hits.extend(find_hits(text, str(path.relative_to(root))))
    by_shape: dict[str, int] = {}
    seen: dict[tuple[str, str, str], int] = {}
    for hit in hits:
        by_shape[hit["shape"]] = by_shape.get(hit["shape"], 0) + 1
        key = (hit["file"], hit["shape"], hit["excerpt"])
        seen[key] = seen.get(key, 0) + 1
        judgement = JUDGED.get(key)
        hit["judged"] = judgement is not None
        if judgement:
            hit["reason"] = judgement["reason"]
            if "verdict" in judgement:
                hit["verdict"] = judgement["verdict"]

    # A judged idiom that gained a copy. The reviewed count is part of the
    # judgement: five decodes were read, so a sixth is unreviewed text even
    # though its excerpt matches.
    overflowing = [
        {"site": list(key), "reviewed": JUDGED[key]["occurrences"], "found": count}
        for key, count in sorted(seen.items())
        if key in JUDGED and count > JUDGED[key]["occurrences"]
    ]
    # A judgement whose site is gone -- fixed, moved or deleted. Reported as
    # data, never as a failure: rot in the baseline is worth seeing, and a
    # judgement going stale because somebody fixed the site is good news.
    stale = [list(key) for key in sorted(JUDGED) if key not in seen]

    unjudged = [hit for hit in hits if not hit["judged"]]
    return {
        "files_scanned": len(files),
        "hits": hits,
        "hit_count": len(hits),
        "by_shape": by_shape,
        "judged_count": len(hits) - len(unjudged),
        "unjudged": unjudged,
        "unjudged_count": len(unjudged),
        "overflowing_judgements": overflowing,
        "stale_judgements": stale,
    }


# --------------------------------------------------------------------------
# Fixtures. The six hand-written shapes, the real pre-fix line, and the
# negatives. Kept beside the self-test so a reader can see what "validated"
# means without running anything.
# --------------------------------------------------------------------------

KNOWN_POSITIVE_PREFIX = "fd8e4eb4d^:scripts/e2e_capture.sh"
KNOWN_POSITIVE_LINE = """        and ((.data.dry_run // true) == false)"""

POSITIVES: list[tuple[str, str]] = [
    ("the real pre-fix line (bd-o8e1n, e2e_capture.sh:792)", KNOWN_POSITIVE_LINE),
    ("indexed path", """    assert_jq "$out" '((.items[0].ok // true) == true)' "indexed" """),
    ("bracket key", """    assert_jq "$out" '((.["odd-key"] // true) == true)' "bracket" """),
    ("doubled parens", """    assert_jq "$out" '(((.a.b // true)) == true)' "doubled" """),
    (
        "comparison split across a newline",
        """    assert_jq "$out" '
        (.a.b // true)
          == true
    ' "split" """,
    ),
    ("negation, default != want", """    assert_jq "$out" '((.a.b // "x") != "y")' "negated" """),
    # A positive on purpose. This fixture is genuinely vacuous (default == want)
    # AND carries a word-internal `#`, so it is the arm that proves blanking did
    # not eat the expression: were the hash treated as a comment, the text would
    # be truncated mid-literal and the hit would VANISH. As a negative arm it
    # proved nothing -- silence would have been the verdict either way, which is
    # the shape of a control that cannot fail.
    (
        "a word-internal hash survives blanking, so a vacuous payload stays flagged",
        """    assert_jq "$out" '(.data.marker // "a#b") == "a#b"' "hash-in-string" """,
    ),
]

NEGATIVES: list[tuple[str, str]] = [
    (
        "the legitimate idiom: absent -> false -> fails",
        """    assert_jq "$out" '((.data.persisted // false) == true)' "legit" """,
    ),
    ("a default with no comparison at all", """    assert_jq "$out" '(.data.rows // empty)' "nocmp" """),
    (
        "a COMMENT documenting the anti-pattern",
        """    assert_jq "$out" '
        ((.data.persisted // false) == true)
        # NOT (.data.dry_run // true) == false (bd-o8e1n). The jq alternative
        # operator fires on false as well as null, so a literal false was
        # replaced by the true default before the comparison.
    ' "documented" """,
    ),
]


def self_test() -> int:
    failures: list[str] = []
    checked = 0

    def arm(label: str, ok: bool) -> None:
        nonlocal checked
        checked += 1
        print(f"  {'ok  ' if ok else 'FAIL'}  {label}")
        if not ok:
            failures.append(label)

    print("POSITIVE arms -- each must be flagged:")
    for label, fixture in POSITIVES:
        arm(label, len(find_hits(fixture)) >= 1)

    print("NEGATIVE arms -- each must be silent:")
    for label, fixture in NEGATIVES:
        hits = find_hits(fixture)
        arm(f"{label}{'' if not hits else f'  (got {hits})'}", not hits)

    print("COUNT arm -- an empty corpus must not read as a clean one:")
    corpus = "\n".join(body for _, body in POSITIVES)
    found = len(find_hits(corpus))
    arm(f"the {len(POSITIVES)} positive fixtures yield {found} hits in one pass", found == len(POSITIVES))

    print("SHAPE arm -- all three shapes are reachable, not just the easy one:")
    shapes = {hit["shape"] for hit in find_hits(corpus)}
    arm(f"shapes exercised: {sorted(shapes)}", shapes == {"vacuous-eq", "vacuous-ne", "unsatisfiable"})

    print("BLANKING arms -- both directions, since one alone cannot catch a no-op:")
    commented = NEGATIVES[2][1]
    arm("a real comment is removed", blank_comments(commented) != commented)
    arm('a word-internal hash is KEPT', 'a#b' in blank_comments('(.a // "a#b") == "c"'))
    arm(
        "a trailing comment is removed while the code before it survives",
        blank_comments('(.a // 1) == 1   # why') .strip() == '(.a // 1) == 1',
    )

    print(f"HISTORY arm -- re-read the known positive from git ({KNOWN_POSITIVE_PREFIX}):")
    try:
        blob = subprocess.run(
            ["git", "show", KNOWN_POSITIVE_PREFIX],
            capture_output=True,
            text=True,
            timeout=30,
            check=False,
        )
    except (OSError, subprocess.SubprocessError) as exc:
        print(f"  skip  git unavailable ({exc}); the embedded fixture above carries this case")
    else:
        if blob.returncode != 0:
            print("  skip  object not in this clone; the embedded fixture above carries this case")
        else:
            lines = blob.stdout.split("\n")
            window = "\n".join(lines[789:793])
            arm("the historical pre-fix line is flagged as it stands in history", len(find_hits(window)) >= 1)

    print()
    if failures:
        print(f"SELF-TEST FAILED: {len(failures)} of {checked} arms")
        for label in failures:
            print(f"  - {label}")
        return 1
    print(f"SELF-TEST PASSED: {checked} arms")
    return 0


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--self-test", action="store_true", help="validate the detector against planted fixtures")
    parser.add_argument("--json", action="store_true", help="machine-readable report")
    parser.add_argument("--root", default=".", help="repository root to scan")
    args = parser.parse_args()

    if args.self_test:
        return self_test()

    report = audit(Path(args.root).resolve())
    if args.json:
        print(json.dumps(report, indent=2, sort_keys=True))
    else:
        print(
            f"scanned {report['files_scanned']} shell files; {report['hit_count']} hit(s), "
            f"{report['judged_count']} judged, {report['unjudged_count']} unjudged"
        )
        for hit in report["hits"]:
            mark = hit.get("verdict") or ("judged" if hit["judged"] else "UNJUDGED")
            print(f"  {hit['file']}:{hit['line']}  [{hit['shape']}]  {hit['excerpt']}   <- {mark}")
        if not report["hits"]:
            print("  no vacuous jq comparisons found")
        for row in report["overflowing_judgements"]:
            print(f"  OVERFLOW: {row['site']} reviewed {row['reviewed']}, found {row['found']}")
        for row in report["stale_judgements"]:
            print(f"  STALE JUDGEMENT (site gone, which may mean fixed): {row}")
    # Deliberately 0 on hits: see POSTURE in the module docstring.
    return 0


if __name__ == "__main__":
    sys.exit(main())
