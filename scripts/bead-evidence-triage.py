#!/usr/bin/env python3
"""Advisory triage: which OPEN beads have their work already in the tree? (bd-p9pfs)

ADVISORY ONLY. This proposes a reading order for a human or an agent. It never
closes a bead, never writes to the tracker, and exits 0 whether it finds
candidates or not. A citation is evidence worth reading, NOT proof of
completion -- a TODO cites the same id as a shipped doc comment.

WHY THE EXISTING DETECTOR CANNOT DO THIS
    scripts/bead-obsolescence.sh asks "status=open AND days_since_update > 14".
    That is a recency-of-touch test, and the failure mode it needs to catch is a
    bead that is touched OFTEN and finished anyway. Tracker sweeps refresh
    updated_at across the queue, which actively cloaks the condition: its last
    run reported staleOpenBeadCount 0 out of 266 open beads -- a clean zero from
    an instrument that cannot measure the thing. See bd-p9pfs.

THE PREDICATE HERE IS EVIDENCE-IN-TREE, AND IT CLASSIFIES THE SITE
    Treating all citations alike is what makes a citation count useless, because
    the strongest and the weakest evidence have identical spelling. Sites are
    ranked:

      doc_on_symbol  a /// or //! comment naming the bead, immediately above a
                     fn/struct/enum/impl/trait/const/static/mod declaration.
                     The strongest signal: someone shipped a symbol and wrote
                     down which bead it discharges.
      doc_comment    a /// or //! naming the bead, not above a declaration.
      code_comment   an ordinary // comment naming the bead.
      code           the id in a string literal or identifier -- fixture names
                     like "missing-orient-sfjvq" live here and are real evidence.
      test           any citation under tests/ or in a *_tests.rs file.
      fixture        a citation under fixtures/ or golden/.
      prose          a citation in Markdown. Docs describe intent, not state.
      todo           TODO / FIXME / XXX / HACK / todo!() / unimplemented!() on
                     the citing line. NEGATIVE evidence: it says NOT done.

    `todo` carries a negative weight on purpose, so a bead whose only citations
    are TODOs ranks below a bead with none at all. That is the planted negative
    in --self-test.

USAGE
    scripts/bead-evidence-triage.py                 human summary
    scripts/bead-evidence-triage.py --json          machine output
    scripts/bead-evidence-triage.py --self-test     planted positive + negative
    scripts/bead-evidence-triage.py --root DIR --tracker FILE   (testing seams)

The tracker source is .beads/issues.jsonl, which is COMMITTED, so this runs on a
CI runner with no `br` installed and no database.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import sys
import tempfile
from pathlib import Path

BEAD_RE = re.compile(r"\bbd-[a-z0-9]+(?:[-.][a-z0-9]+)*\b")
TODO_RE = re.compile(r"\b(TODO|FIXME|XXX|HACK)\b|\btodo!\(|\bunimplemented!\(")
DECL_RE = re.compile(
    r"^\s*(?:pub(?:\s*\([^)]*\))?\s+)?"
    r"(?:async\s+)?(?:unsafe\s+)?(?:extern\s+\"[^\"]*\"\s+)?"
    r"(?:fn|struct|enum|trait|impl|const|static|mod|type|union)\b"
)
ATTR_OR_COMMENT_RE = re.compile(r"^\s*(?:#\[|#!\[|//|/\*|\*|$)")

SEARCH_DIRS = ("src", "tests", "scripts", "docs", "benches")
TEXT_SUFFIXES = {
    ".rs", ".md", ".sh", ".py", ".toml", ".yml", ".yaml", ".json",
    ".golden", ".snap", ".tsv", ".txt", ".ps1",
}
MAX_FILE_BYTES = 4 * 1024 * 1024

# SELF-EXCLUSION, and it is not hygiene -- it is a measured bug fix.
# This file's own comments name bd-6oqlx three times while explaining the weak
# tier. Scanning itself scored those as evidence and promoted that bead from 2
# to 8, across the strong threshold: the tool reported a recall improvement that
# was entirely its own prose. Any file whose job is to talk ABOUT beads must be
# excluded from a predicate that reads bead mentions as evidence.
SELF_EXCLUDED = {
    "scripts/bead-evidence-triage.py",
    "scripts/bead-obsolescence.sh",
}

WEIGHTS = {
    "doc_on_symbol": 5,
    "doc_comment": 3,
    "code_comment": 2,
    "code": 2,
    "test": 2,
    "fixture": 1,
    "prose": 0,
    "todo": -3,
}


def open_bead_ids(tracker: Path) -> dict[str, dict]:
    """Non-closed beads from the committed JSONL export."""
    out: dict[str, dict] = {}
    try:
        text = tracker.read_text(encoding="utf-8", errors="replace")
    except OSError as error:
        print(f"[bead-triage] cannot read {tracker}: {error}", file=sys.stderr)
        return out
    for line in text.splitlines():
        line = line.strip()
        if not line:
            continue
        try:
            row = json.loads(line)
        except json.JSONDecodeError:
            # A single malformed row must not blind the whole report.
            continue
        if not isinstance(row, dict):
            continue
        status = row.get("status")
        if status in (None, "closed"):
            continue
        bead_id = row.get("id")
        if isinstance(bead_id, str):
            out[bead_id] = {
                "status": status,
                "priority": row.get("priority"),
                "title": (row.get("title") or "")[:110],
            }
    return out


def classify(path: Path, lines: list[str], index: int) -> str:
    """Classify ONE citation site. Order matters: todo wins over everything."""
    line = lines[index]
    rel = str(path).replace(os.sep, "/")

    if TODO_RE.search(line):
        return "todo"
    if rel.endswith(".md"):
        return "prose"
    if "/fixtures/" in rel or "/golden/" in rel or rel.endswith((".golden", ".snap")):
        return "fixture"

    stripped = line.lstrip()
    # `#` comments in Python / shell / YAML are comments too. Without this they
    # fall through to "code", which reads a tool's own prose about a bead as
    # though it were an implementation of it.
    if stripped.startswith("#") and not rel.endswith(".rs"):
        return "test" if (rel.startswith("tests/") or "/tests/" in rel) else "code_comment"
    is_doc = stripped.startswith("///") or stripped.startswith("//!")
    if is_doc and rel.endswith(".rs"):
        # Look ahead for the first line that is neither comment, attribute nor
        # blank. If it declares a symbol, this doc comment is attached to
        # shipped code rather than floating in prose.
        for look in range(index + 1, min(index + 14, len(lines))):
            nxt = lines[look]
            if ATTR_OR_COMMENT_RE.match(nxt):
                continue
            return "doc_on_symbol" if DECL_RE.match(nxt) else "doc_comment"
        return "doc_comment"
    if is_doc:
        return "doc_comment"

    in_tests = rel.startswith("tests/") or "_tests." in rel or "/tests/" in rel
    if "//" in line and line.index("//") < (line.find("bd-") if "bd-" in line else len(line)):
        return "test" if in_tests else "code_comment"
    if in_tests:
        return "test"
    return "code"


def scan(root: Path, wanted: set[str]) -> dict[str, list[dict]]:
    """ONE pass over the tree. O(tree), not O(tree x beads)."""
    found: dict[str, list[dict]] = {}
    for base in SEARCH_DIRS:
        start = root / base
        if not start.is_dir():
            continue
        for dirpath, dirnames, filenames in os.walk(start):
            dirnames[:] = [d for d in dirnames if not d.startswith(".") and d != "target"]
            for name in filenames:
                path = Path(dirpath) / name
                if path.suffix not in TEXT_SUFFIXES:
                    continue
                if os.path.relpath(path, root).replace(os.sep, "/") in SELF_EXCLUDED:
                    continue
                try:
                    if path.stat().st_size > MAX_FILE_BYTES:
                        continue
                    text = path.read_text(encoding="utf-8", errors="replace")
                except OSError:
                    continue
                if "bd-" not in text:
                    continue
                lines = text.splitlines()
                for index, line in enumerate(lines):
                    for match in BEAD_RE.finditer(line):
                        bead_id = match.group(0)
                        if bead_id not in wanted:
                            continue
                        rel = os.path.relpath(path, root).replace(os.sep, "/")
                        found.setdefault(bead_id, []).append(
                            {
                                "file": rel,
                                "line": index + 1,
                                "site": classify(Path(rel), lines, index),
                                "text": line.strip()[:160],
                            }
                        )
    return found


def score(sites: list[dict]) -> int:
    return sum(WEIGHTS.get(s["site"], 0) for s in sites)


def build_report(root: Path, tracker: Path) -> dict:
    beads = open_bead_ids(tracker)
    citations = scan(root, set(beads))
    rows = []
    for bead_id, meta in beads.items():
        sites = citations.get(bead_id, [])
        if not sites:
            continue
        kinds: dict[str, int] = {}
        for s in sites:
            kinds[s["site"]] = kinds.get(s["site"], 0) + 1
        total = score(sites)
        strong = kinds.get("doc_on_symbol", 0)
        # A bead is a CANDIDATE when something shipped cites it, or when the
        # weight of ordinary evidence is substantial. A TODO-only bead cannot
        # reach either bar, because todo weight is negative.
        candidate = strong > 0 or total >= 6
        # SECOND TIER, added after measuring recall against bd-p9pfs's five
        # known positives and finding 1 of 3 still-open ones flagged. bd-6oqlx
        # is finished work cited only from fixtures, which the strong bar misses.
        # The weak bar requires at least two citations that are neither prose nor
        # TODO, plus a positive score -- so a docs-only bead (intent, not state)
        # and a TODO-only bead are both still refused. Verified by the planted
        # negatives in --self-test.
        substantive = len(
            [s for s in sites if s["site"] not in ("prose", "todo")]
        )
        weak = (not candidate) and substantive >= 2 and total > 0
        rows.append(
            {
                "id": bead_id,
                "status": meta["status"],
                "priority": meta["priority"],
                "title": meta["title"],
                "score": total,
                "citations": len(sites),
                "sites": kinds,
                "candidate": candidate,
                "weakCandidate": weak,
                "strongest": sorted(
                    sites, key=lambda s: -WEIGHTS.get(s["site"], 0)
                )[:3],
            }
        )
    rows.sort(key=lambda r: (-r["score"], r["id"]))
    return {
        "schema": "ee.bead_evidence_triage.v1",
        "advisory": True,
        "note": "A citation is evidence worth reading, not proof of completion. "
        "This never closes a bead.",
        "predicate": "evidence-in-tree, classified by citation site",
        "denominator": {
            "nonClosedBeads": len(beads),
            "beadsWithAnyCitation": len(citations),
            "candidates": sum(1 for r in rows if r["candidate"]),
            "weakCandidates": sum(1 for r in rows if r.get("weakCandidate")),
        },
        "weights": WEIGHTS,
        "rows": rows,
    }


def human(report: dict, limit: int) -> None:
    d = report["denominator"]
    print("[bead-triage] ADVISORY. Proposes a reading order; closes nothing.")
    print(
        f"[bead-triage] predicate: {report['predicate']}  "
        f"(NOT days-since-touch, which tracker sweeps cloak)"
    )
    print(
        f"[bead-triage] denominator: {d['nonClosedBeads']} non-closed beads; "
        f"{d['beadsWithAnyCitation']} cited anywhere; {d['candidates']} candidates, "
        f"{d['weakCandidates']} weak"
    )
    cands = [r for r in report["rows"] if r["candidate"]][:limit]
    if not cands:
        print("[bead-triage] no candidates.")
        return
    print()
    print(f"{'SCORE':>5}  {'CITES':>5}  {'P':>2}  {'ID':38}  TITLE")
    for r in cands:
        pri = "-" if r["priority"] is None else str(r["priority"])
        print(f"{r['score']:>5}  {r['citations']:>5}  {pri:>2}  {r['id']:38}  {r['title'][:64]}")
    print()
    print("Strongest site per top candidate (read these first):")
    for r in cands[:8]:
        best = r["strongest"][0] if r["strongest"] else None
        if best:
            print(f"  {r['id']}")
            print(f"      [{best['site']}] {best['file']}:{best['line']}")
            print(f"      {best['text'][:110]}")


# ---------------------------------------------------------------------------
# Self-test. Every arm asserts a POLARITY. A detector that cannot be seen to
# REFUSE something is decoration, and the refusal that matters here is the
# TODO-only bead, because that is the shape a naive citation count gets wrong.
# ---------------------------------------------------------------------------

POS_ID = "bd-selftestpos"
TODO_ID = "bd-selftesttodo"
PROSE_ID = "bd-selftestprose"
ABSENT_ID = "bd-selftestabsent"
CLOSED_ID = "bd-selftestclosed"
FIXTURE_ID = "bd-selftestfixture"
SELFREF_ID = "bd-selftestselfref"


def self_test() -> int:
    arms: list[tuple[str, bool]] = []

    def arm(label: str, ok: bool) -> None:
        arms.append((label, ok))
        print(f"  [{'ok  ' if ok else 'FAIL'}] {label}")

    with tempfile.TemporaryDirectory(prefix="bead-triage-selftest-") as tmp:
        root = Path(tmp)
        (root / "src").mkdir()
        (root / "tests").mkdir()
        (root / "docs").mkdir()
        (root / ".beads").mkdir()

        # PLANTED POSITIVE: a doc comment naming a bead, directly above a
        # shipped declaration. This is the ft1z5 shape from bd-p9pfs.
        (root / "src" / "live.rs").write_text(
            "use std::fmt;\n"
            "\n"
            f"/// Discovers nearby stores for the addressed database ({POS_ID}).\n"
            "/// Kept as one identity so the decision and the candidate agree.\n"
            "#[must_use]\n"
            "pub fn discover_nearby_stores() -> u32 {\n"
            "    0\n"
            "}\n",
            encoding="utf-8",
        )
        # PLANTED NEGATIVE: the id appears ONLY in a TODO. Must not be flagged.
        (root / "src" / "pending.rs").write_text(
            "pub fn later() {\n"
            f"    // TODO({TODO_ID}): wire this up once the carrier exists\n"
            f"    // TODO({TODO_ID}): and add the fence\n"
            f"    // TODO({TODO_ID}): and the receipt\n"
            "}\n",
            encoding="utf-8",
        )
        # PLANTED WEAK: prose only. Docs state intent, not shipped state.
        (root / "docs" / "plan.md").write_text(
            f"We intend to build the carrier described in {PROSE_ID}.\n" * 6,
            encoding="utf-8",
        )
        # A CLOSED bead cited strongly must never appear: the tracker filter,
        # not the scanner, is what excludes it.
        (root / "src" / "closedwork.rs").write_text(
            f"/// Implements {CLOSED_ID}.\npub fn done() {{}}\n", encoding="utf-8"
        )
        # PLANTED WEAK-TIER POSITIVE: cited only from fixture names, which is
        # the bd-6oqlx shape the strong bar misses.
        (root / "tests").mkdir(exist_ok=True)
        (root / "tests" / "fixtures").mkdir(parents=True, exist_ok=True)
        (root / "tests" / "fixtures" / "case.json").write_text(
            f'{{"name": "scoring-{FIXTURE_ID}", "other": "stack-{FIXTURE_ID}"}}\n',
            encoding="utf-8",
        )
        # PLANTED SELF-REFERENCE: a bead-discussing tool naming a bead many
        # times. This is the self-contamination that promoted bd-6oqlx across
        # the strong threshold on the real tree before SELF_EXCLUDED existed.
        (root / "scripts").mkdir(exist_ok=True)
        (root / "scripts" / "bead-evidence-triage.py").write_text(
            f"# explaining the weak tier using {SELFREF_ID} as the example\n"
            f"# and again {SELFREF_ID}\n"
            f"# and once more {SELFREF_ID}\n"
            f'/// Implements {SELFREF_ID}.\npub fn decoy() {{}}\n',
            encoding="utf-8",
        )

        tracker = root / ".beads" / "issues.jsonl"
        tracker.write_text(
            "\n".join(
                json.dumps(r)
                for r in [
                    {"id": POS_ID, "status": "open", "priority": 1, "title": "planted positive"},
                    {"id": TODO_ID, "status": "open", "priority": 1, "title": "planted todo-only"},
                    {"id": PROSE_ID, "status": "open", "priority": 2, "title": "planted prose-only"},
                    {"id": ABSENT_ID, "status": "open", "priority": 2, "title": "planted uncited"},
                    {"id": FIXTURE_ID, "status": "open", "priority": 2, "title": "planted fixture-only"},
                    {"id": SELFREF_ID, "status": "open", "priority": 2, "title": "planted self-reference"},
                    {"id": CLOSED_ID, "status": "closed", "priority": 1, "title": "planted closed"},
                    "MALFORMED-NOT-JSON",
                ]
                if isinstance(r, dict)
            )
            + "\nMALFORMED-NOT-JSON\n",
            encoding="utf-8",
        )

        report = build_report(root, tracker)
        rows = {r["id"]: r for r in report["rows"]}
        cand = {r["id"] for r in report["rows"] if r["candidate"]}
        weak = {r["id"] for r in report["rows"] if r.get("weakCandidate")}

        arm("a doc comment above a declaration is a CANDIDATE", POS_ID in cand)
        arm(
            "that site is classified doc_on_symbol, not merely doc_comment",
            POS_ID in rows and rows[POS_ID]["sites"].get("doc_on_symbol", 0) >= 1,
        )
        arm("a TODO-only bead is NOT a candidate", TODO_ID not in cand)
        arm(
            "and its score is NEGATIVE, so it ranks below an uncited bead",
            TODO_ID in rows and rows[TODO_ID]["score"] < 0,
        )
        arm("a prose-only bead is NOT a candidate", PROSE_ID not in cand)
        # THE WEAK TIER AND ITS TWO REFUSALS. The tier exists to catch
        # fixture-only evidence (bd-6oqlx); it must not quietly readmit the
        # prose-only and TODO-only beads the strong bar already refused.
        arm("a fixture-only bead IS a weak candidate", FIXTURE_ID in weak)
        arm("a fixture-only bead is NOT a strong candidate", FIXTURE_ID not in cand)
        arm("a prose-only bead is not weak either", PROSE_ID not in weak)
        arm("a TODO-only bead is not weak either", TODO_ID not in weak)
        arm("the strong positive is not double-counted as weak", POS_ID not in weak)
        # SELF-CONTAMINATION. A tool that reads bead mentions as evidence must
        # not read its OWN mentions. Measured on the real tree: this file's three
        # comments about bd-6oqlx promoted it from score 2 to 8, across the
        # strong bar, and the reported recall gain was the tool citing itself.
        arm(
            "a bead cited ONLY by a bead-discussing tool is not detected at all",
            SELFREF_ID not in rows,
        )
        arm("an uncited bead does not appear at all", ABSENT_ID not in rows)
        arm("a CLOSED bead never appears, however strongly cited", CLOSED_ID not in rows)
        arm("a malformed tracker row does not blind the report", len(rows) >= 3)
        arm("the report declares itself advisory", report.get("advisory") is True)
        arm(
            "the denominator is reported, not just the hits",
            report["denominator"]["nonClosedBeads"] == 6,
        )

    passed = sum(1 for _, ok in arms if ok)
    print(f"\n[bead-triage] self-test: {passed}/{len(arms)} arms passed")
    return 0 if passed == len(arms) else 1


def main() -> int:
    parser = argparse.ArgumentParser(add_help=True)
    parser.add_argument("--root", default=".")
    parser.add_argument("--tracker", default=None)
    parser.add_argument("--json", action="store_true")
    parser.add_argument("--limit", type=int, default=30)
    parser.add_argument("--self-test", action="store_true")
    args = parser.parse_args()

    if args.self_test:
        return self_test()

    root = Path(args.root).resolve()
    tracker = Path(args.tracker) if args.tracker else root / ".beads" / "issues.jsonl"
    if not tracker.is_file():
        print(f"[bead-triage] tracker export not found: {tracker}", file=sys.stderr)
        print("[bead-triage] advisory tool; nothing to report.", file=sys.stderr)
        return 0

    report = build_report(root, tracker)
    if args.json:
        print(json.dumps(report, indent=2, sort_keys=True))
    else:
        human(report, args.limit)
    # ADVISORY: never a gate. Findings are not failures.
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except SystemExit:
        raise
    except BaseException as error:  # noqa: BLE001 - advisory tool, never a gate
        print(f"[bead-triage] unexpected error ({error}); reporting nothing", file=sys.stderr)
        raise SystemExit(0) from None
