#!/usr/bin/env python3
"""Every content hash cited as evidence in this tree must be a hash that EXISTED.

WHY THIS EXISTS. `docs/adr/0030-typed-determinism-capability-token.md` records a
decision taken on the `tests/randomness_inventory.json` of 2026-05-13 and pins
that inventory by its `rows_content_hash`. A hash is exactly the kind of token a
reader trusts without re-deriving, so a citation to a hash that was never this
artifact's hash -- mistyped, or copied from the wrong artifact, or invented -- is
indistinguishable from a correct one.

IT SWEEPS THE TREE RATHER THAN A LIST OF ADRs, and the first version of this
gate did not, which is the defect this docstring is longest about. A census on
2026-11-08 found that hash cited in SIX tracked files, not the two the repair
that prompted this gate claimed:

    docs/adr/0030-typed-determinism-capability-token.md:11   prose
    src/runtime/README.md:17                                 prose
    src/runtime/determinism.rs:46                            a pub const
    tests/determinism_capability_token_unit.rs:201           asserts the const
    tests/snapshots/determinism_token_doctest_output.snap:8  snapshots the const
    tests/adr_0030_docs.rs:83                                a test literal

plus two more hashes in `.beads/issues.jsonl`, both bd-2cdum's own before/after
evidence and both real. An ADR-only table therefore covered ONE of seven
citations, and it missed the strongest anchor in the tree:
`RANDOMNESS_INVENTORY_ROWS_CONTENT_HASH` is a production `pub const` whose doc
comment calls it "the N4.1 inventory hash that drove the first
deterministic-token design". Bumping that const to the current hash is an
attractive wrong repair -- it is what a reader reaches for on seeing the
desync -- and it would make a dated production record wrong. A hand-maintained
table of ADRs cannot see it. `git grep` can see all seven, and sees a new citer
the day it is written.

WHAT THE GATE CLAIMS, exactly: every `blake3-ish:<64 hex>` in a tracked file is
the value some revision of a DECLARED artifact really carried. Growth is handled
by declaring: a legitimate citation of a second artifact is added to ARTIFACTS,
not excused.

`tests/adr_0030_docs.rs` cannot answer this, and that is not a criticism of it:
`include_str!` hands a test ONE revision, and whether a hash was ever a revision
of an artifact is a fact about git history. That test used to reach for the
question anyway, by asserting the CURRENT inventory still contained the
2026-05-13 hash, which the design makes impossible -- the inventory is a
timestamped snapshot (bd-2cdum), so the first regeneration broke the arm and it
stayed red for 25 days across three regenerations. Nothing saw it, because
verify.sh refuses before stage one on the maintainer's host, hosted Actions is
off repo-wide, and the arm reaches a runner only inside a 773-test shard.

Of the seven citations, that one was the ONLY one that compared a hash against
the live artifact, and it is the only one that was ever red. The other six are a
closed, self-consistent triangle -- const, unit assertion, snapshot -- that
never reads the inventory. The design was right; one assertion reached across
it.

This gate needs git and nothing else: no cargo, no `ee` binary, no dispatch. It
is deliberately NOT added to verify.sh, which has about six seconds of
non-benchmark headroom against a fixed 608 s ceiling; it lives in the
`scripts/check-*.py` family that CLAUDE.md tells agents to run before a commit,
which is also the host where a regeneration happens.

It also discharges bd-2cdum's first acceptance criterion, which was never
implemented: "the file is explicitly recorded as a point-in-time snapshot so
`rows_content_hash` citations carry their generation date". Every PASS line
prints the revision and date the cited hash belongs to, so a citation's age is
visible without archaeology.

WHAT IT DOES NOT CHECK, stated so the green is not over-read.

  * It does not check that a citation is CURRENT. Staleness between dated
    snapshots is the design, and asserting currency is the exact mistake it was
    written to replace.
  * It cannot catch a repoint to a hash that is real but WRONG -- bumping the
    const from the 2026-05-13 revision to the 2026-10-20 one passes here,
    because both are real revisions. What stops that is the dated prose beside
    each citation, and the ADR's own "316 sources / 261 systemtime" numbers,
    which `3c2b4e4e9` holds exactly (verified by hand 2026-11-08). Not enforced.
  * It does not attribute a hash to the citer's INTENT. It proves the hash
    existed, and names where and when.

Usage:
  scripts/check-adr-evidence-citations.py              audit every cited hash
  scripts/check-adr-evidence-citations.py --self-test  prove every failure direction

Exit: 0 clean, 1 gate failure, 2 self-test failure, 3 environment error.
"""

from __future__ import annotations

import os
import re
import subprocess
import sys

# Artifacts whose git revisions supply legitimate hashes. The `why` is printed on
# failure so the next reader learns it from the failure rather than from
# archaeology -- same shape as scripts/check-pinned-definitions.sh's PINNED.
ARTIFACTS = [
    {
        "path": "tests/randomness_inventory.json",
        "field": "rows_content_hash",
        "why": (
            "ADR 0030 records the N4.1 randomness audit it was decided on and "
            "pins it by content hash; src/runtime/determinism.rs carries the "
            "same hash as a production const, and three tests and a golden "
            "snapshot assert against that const. A hash that was never this "
            "artifact's hash silently misdescribes the evidence those records "
            "rest on (bd-2cdum)"
        ),
    },
]

HASH_RE = re.compile(r"blake3-ish:[0-9a-f]{64}")

# One `git grep -n -o` output line: path, line number, match. The match CONTAINS
# A COLON (`blake3-ish:`), so splitting on the last colon lands inside the hash
# and yields a bare 64-hex string -- which then matches nothing and reports the
# whole tree as citing no hash at all. That was this gate's own first bug, and
# arm12 is what caught it. Anchoring the match at `$` and letting a greedy `.*`
# backtrack to `:<digits>:` is what lets a path containing a colon survive.
GREP_LINE_RE = re.compile(rf"^(?P<path>.*):(?P<lineno>\d+):(?P<match>{HASH_RE.pattern})$")

# A revision walk has to be bounded, and a truncated walk must SAY so rather
# than report a verdict it did not earn. No artifact here is near this.
REVISION_CAP = 200


def parse_grep_line(line: str) -> tuple[str, str, str] | None:
    """(path, line number, hash) for one `git grep -n -o` line, or None.

    Factored out so the self-test drives THIS code rather than a second copy of
    the rule. An arm that restates a parse passes while the real parse is broken,
    which is how the colon bug above survived its first self-test.
    """
    found = GREP_LINE_RE.match(line)
    if found is None:
        return None
    return found.group("path"), found.group("lineno"), found.group("match")


class CannotAnswer(Exception):
    """The environment cannot support a verdict. Exit 3, never a pass."""


def repo_root() -> str:
    root = os.environ.get("EE_REPO_ROOT")
    if not root:
        try:
            done = subprocess.run(
                ["git", "rev-parse", "--show-toplevel"],
                capture_output=True,
                check=True,
            )
        except (OSError, subprocess.CalledProcessError) as error:
            raise CannotAnswer(f"git rev-parse failed: {error}") from error
        root = done.stdout.decode("utf-8", errors="replace").strip()
    try:
        os.chdir(root)
    except OSError as error:
        # A bad EE_REPO_ROOT must not read as a gate failure. Exit 3 says the
        # environment cannot support a verdict; exit 1 would say the tree is bad.
        raise CannotAnswer(f"cannot enter repo root {root!r}: {error}") from error
    return root


def tracked_citations() -> dict[str, list[str]]:
    """{hash: [\"path:line\", ...]} over TRACKED files, via one `git grep`.

    Tracked-only on purpose: an untracked scratch file is not a record anybody
    will read later, and sweeping the worktree would make the verdict depend on
    whatever a peer happens to have lying around (the working tree here is
    shared with other agents).

    `git grep -o` is git's own matcher, so this does not inherit the BSD/GNU
    regex split that bites the shell gates in this repo.
    """
    try:
        done = subprocess.run(
            ["git", "grep", "-I", "-n", "-o", "-E", HASH_RE.pattern],
            capture_output=True,
            check=False,
        )
    except OSError as error:
        raise CannotAnswer(f"git grep failed: {error}") from error
    # git grep exits 1 for "no matches", which here is not an error but IS a
    # vacuous sweep; decide() is what refuses to pass on it.
    if done.returncode not in (0, 1):
        tail = done.stderr.decode("utf-8", errors="replace").strip()[:200]
        raise CannotAnswer(f"git grep exited {done.returncode}: {tail}")

    found: dict[str, list[str]] = {}
    unparsed = 0
    for line in done.stdout.decode("utf-8", errors="replace").splitlines():
        parsed = parse_grep_line(line)
        if parsed is None:
            unparsed += 1
            continue
        path, lineno, match = parsed
        found.setdefault(match, []).append(f"{path}:{lineno}")
    if unparsed:
        # A line the parser cannot read is an UNEXAMINED citation, not an absent
        # one, so it must not be silently dropped into a green.
        raise CannotAnswer(
            f"{unparsed} `git grep` output line(s) did not parse as path:line:hash; "
            "the sweep is incomplete, which is no verdict rather than a pass"
        )
    return found


def revisions_of(path: str, field: str) -> tuple[list[tuple[str, str, str]], bool]:
    """[(short sha, date, field value)] oldest first, plus whether it truncated.

    Reads the field with a regex rather than a JSON parse ON PURPOSE: an older
    revision of an artifact may predate the current schema, and a parse failure
    there would be read as a missing hash -- a false gate failure caused by the
    instrument. A regex over the raw blob answers the only question asked.
    """
    try:
        log = subprocess.run(
            ["git", "log", "--reverse", "--format=%h %ad", "--date=short", "--", path],
            capture_output=True,
            check=True,
        ).stdout.decode("utf-8", errors="replace")
    except (OSError, subprocess.CalledProcessError) as error:
        raise CannotAnswer(f"git log failed for {path}: {error}") from error

    lines = [line for line in log.splitlines() if line.strip()]
    truncated = len(lines) > REVISION_CAP
    field_re = re.compile(rf'"{re.escape(field)}"\s*:\s*"([^"]+)"')
    out: list[tuple[str, str, str]] = []
    for line in lines[:REVISION_CAP]:
        sha, _, date = line.partition(" ")
        try:
            blob = subprocess.run(
                ["git", "show", f"{sha}:{path}"], capture_output=True, check=True
            ).stdout.decode("utf-8", errors="replace")
        except (OSError, subprocess.CalledProcessError):
            # A revision where the path did not exist under this name is not an
            # environment failure; it simply offers no hash.
            continue
        found = field_re.search(blob)
        if found:
            out.append((sha, date.strip(), found.group(1)))
    return out, truncated


def decide(
    cited: list[str], revisions: list[tuple[str, str, str, str]]
) -> tuple[bool, list[tuple[str, str, str, str]], list[str]]:
    """Pure. (ok, matched as [(hash, sha, date, artifact)], unmatched hashes).

    Pure so the self-test can drive every branch without a repository, which is
    the same reason scripts/hooks/pre-commit/45-symbol-orphans.py factored its
    `decide`. In particular the VACUOUS branch is reachable here: a sweep that
    found no citation at all is `ok=False`, because "nothing to check" is no
    verdict rather than a pass, and that is the one direction a gate fails
    silently in.

    `revisions` is oldest first, and the FIRST revision carrying a hash wins, so
    a hash that reappears later (a revert) is dated to when it first became the
    artifact's hash -- which is the date a decision citing it was taken.
    """
    by_hash: dict[str, tuple[str, str, str]] = {}
    for sha, date, value, artifact in revisions:
        by_hash.setdefault(value, (sha, date, artifact))

    matched: list[tuple[str, str, str, str]] = []
    unmatched: list[str] = []
    for hash_value in cited:
        where = by_hash.get(hash_value)
        if where is None:
            unmatched.append(hash_value)
        else:
            matched.append((hash_value, where[0], where[1], where[2]))
    return bool(cited) and not unmatched, matched, unmatched


def audit() -> int:
    print()
    revisions: list[tuple[str, str, str, str]] = []
    for entry in ARTIFACTS:
        path, field = entry["path"], entry["field"]
        found, truncated = revisions_of(path, field)
        if not found:
            # An artifact with no history at all means the table names a path
            # that does not exist -- a declaration bug, which is a FAILURE the
            # author must fix, not an environment the gate cannot read.
            print(f"  FAIL: no revision of {path} carries a `{field}`.")
            print("        ARTIFACTS names a path or field that does not exist.")
            return 1
        if truncated:
            print(
                f"  (walk of {path} capped at {REVISION_CAP} revisions; a cited "
                "hash older than that would read as unmatched -- raise the cap, "
                "do not trust this)"
            )
        print(f"  {path} has {len(found)} revision(s) carrying `{field}`")
        revisions.extend((sha, date, value, path) for sha, date, value in found)

    citations = tracked_citations()
    print(f"  tracked files cite {len(citations)} distinct hash(es)")
    print()

    ok, matched, unmatched = decide(sorted(citations), revisions)

    for hash_value, sha, date, artifact in matched:
        where = citations[hash_value]
        print(f"  ok   {hash_value[:22]}... is {artifact} at {sha}, {date}")
        # Every site, never a "+N more": the census IS the output here, and the
        # sites this gate exists for -- a production const, a golden snapshot --
        # are the ones an elided tail would hide.
        print(f"       cited by {len(where)} site(s):")
        for site in where:
            print(f"         {site}")
    for hash_value in unmatched:
        print(f"  FAIL {hash_value[:22]}... matches NO declared artifact revision")
        print(f"       cited by {', '.join(citations[hash_value])}")
        for entry in ARTIFACTS:
            print(f"       {entry['path']} ever carried:")
            for sha, date, value, artifact in revisions:
                if artifact == entry["path"]:
                    print(f"         {sha}  {date}  {value[:22]}...")
            print(f"       why it is pinned: {entry['why']}")

    print()
    if not citations:
        print("  FAIL: the sweep found no cited hash anywhere in the tree.")
        print("  That is not a pass -- it is a verdict about nothing, and the most")
        print("  likely cause is that this gate's own pattern or `git grep` broke.")
        return 1
    if not ok:
        print(f"  FAIL: {len(unmatched)} cited hash(es) were never a declared")
        print("  artifact's hash. Two legitimate causes, and they need opposite fixes:")
        print("    * the citation is of a DIFFERENT artifact -- declare that artifact")
        print("      in ARTIFACTS, do not change the citation;")
        print("    * the hash is wrong -- mistyped, or copied from the wrong run.")
        print("  What is NOT a cause: the artifact having moved since. Snapshots are")
        print("  dated (bd-2cdum), so an old citation matching an old revision is")
        print("  exactly right and prints `ok` above.")
        return 1
    sites = sum(len(where) for where in citations.values())
    print(f"  OK: {len(matched)} distinct hash(es) at {sites} citation site(s)")
    print(f"  each match a real revision of {len(ARTIFACTS)} declared artifact(s).")
    return 0


def self_test() -> int:
    arms: list[tuple[str, bool]] = []

    def arm(name: str, ok: bool) -> None:
        arms.append((name, ok))

    art = "tests/randomness_inventory.json"
    revs = [
        ("aaaaaaa", "2026-05-13", "blake3-ish:" + "1" * 64, art),
        ("bbbbbbb", "2026-10-14", "blake3-ish:" + "2" * 64, art),
        ("ccccccc", "2026-10-20", "blake3-ish:" + "3" * 64, art),
    ]

    ok, matched, unmatched = decide(["blake3-ish:" + "1" * 64], revs)
    arm(
        "arm01 a hash from the OLDEST revision passes, and names that revision",
        ok
        and not unmatched
        and matched == [("blake3-ish:" + "1" * 64, "aaaaaaa", "2026-05-13", art)],
    )

    ok, matched, unmatched = decide(["blake3-ish:" + "3" * 64], revs)
    arm("arm02 a hash from the CURRENT revision passes too", ok and matched[0][1] == "ccccccc")

    ok, matched, unmatched = decide(["blake3-ish:" + "9" * 64], revs)
    arm(
        "arm03 a hash matching no revision FAILS and is reported, not waved through",
        not ok and unmatched == ["blake3-ish:" + "9" * 64] and not matched,
    )

    ok, matched, unmatched = decide([], revs)
    arm(
        "arm04 a sweep that found NOTHING is a failure, not a vacuous pass",
        not ok and not matched and not unmatched,
    )

    ok, matched, unmatched = decide(["blake3-ish:" + "1" * 64], [])
    arm(
        "arm05 with no revisions to compare, a real hash is NOT silently matched",
        not ok and unmatched == ["blake3-ish:" + "1" * 64],
    )

    ok, _, unmatched = decide(["blake3-ish:" + "1" * 64, "blake3-ish:" + "9" * 64], revs)
    arm(
        "arm06 one bad hash among good ones still fails the whole gate",
        not ok and unmatched == ["blake3-ish:" + "9" * 64],
    )

    # A hash that appears at two revisions must be dated to the FIRST, because
    # that is when a decision citing it was taken.
    reverted = revs + [("ddddddd", "2026-11-01", "blake3-ish:" + "1" * 64, art)]
    _, matched, _ = decide(["blake3-ish:" + "1" * 64], reverted)
    arm(
        "arm07 a hash present at two revisions is dated to the OLDEST, not the newest",
        matched == [("blake3-ish:" + "1" * 64, "aaaaaaa", "2026-05-13", art)],
    )

    # Two artifacts: a hash legitimately belonging to the second must pass, and
    # be attributed to the artifact it really came from rather than the first.
    multi = revs + [("eeeeeee", "2026-06-01", "blake3-ish:" + "4" * 64, "tests/other.json")]
    ok, matched, _ = decide(["blake3-ish:" + "4" * 64], multi)
    arm(
        "arm08 with two declared artifacts, a match is attributed to the right one",
        ok and matched[0][3] == "tests/other.json",
    )

    # The pattern is the other half of the instrument, and a loose one matches
    # prose. Both directions.
    arm(
        "arm09 the hash pattern matches a real citation in prose",
        HASH_RE.findall("Its hash is:\n\n`blake3-ish:" + "a" * 64 + "`\n")
        == ["blake3-ish:" + "a" * 64],
    )
    arm(
        "arm10 the pattern rejects a short, upper-case or unprefixed digest",
        not HASH_RE.findall("blake3-ish:" + "a" * 63)
        and not HASH_RE.findall("blake3-ish:" + "A" * 64)
        and not HASH_RE.findall("sha256:" + "a" * 64),
    )

    # The field reader must not be fooled by a different key whose name CONTAINS
    # the field name, which is the substring trap this repo keeps hitting.
    field_re = re.compile(r'"rows_content_hash"\s*:\s*"([^"]+)"')
    arm(
        "arm11 the field reader does not match a longer key containing the name",
        field_re.search('{"prev_rows_content_hash": "blake3-ish:x"}') is None
        and field_re.search('{"rows_content_hash": "blake3-ish:y"}') is not None,
    )

    # The sweep parses `git grep -n -o` output. Drive the REAL parser, not a copy
    # of it: the first version of this arm restated the rule and so agreed with
    # a parse that split inside `blake3-ish:` and found nothing in the tree.
    arm(
        "arm12 an ordinary grep line parses into path, line and the FULL hash",
        parse_grep_line("src/runtime/determinism.rs:46:blake3-ish:" + "a" * 64)
        == ("src/runtime/determinism.rs", "46", "blake3-ish:" + "a" * 64),
    )
    arm(
        "arm13 a colon inside the PATH survives the parse",
        parse_grep_line("docs/a:b/c.md:17:blake3-ish:" + "a" * 64)
        == ("docs/a:b/c.md", "17", "blake3-ish:" + "a" * 64),
    )
    arm(
        "arm14 a line that is not path:line:hash is rejected, never half-parsed",
        parse_grep_line("blake3-ish:" + "a" * 64) is None
        and parse_grep_line("docs/x.md:notanumber:blake3-ish:" + "a" * 64) is None
        and parse_grep_line("docs/x.md:17:sha256:" + "a" * 64) is None,
    )

    # This script is itself tracked, so a literal 64-hex hash in its own source
    # would be swept up as a citation and fail the gate against its own test
    # data. Every hash above is built by concatenation for exactly that reason.
    try:
        with open(__file__, encoding="utf-8") as handle:
            own_source = handle.read()
        arm(
            "arm15 this script's own source holds no literal hash for the sweep to find",
            not HASH_RE.findall(own_source),
        )
    except OSError as error:
        arm(f"arm15 could not read own source: {error}", False)

    arm(
        "arm16 every declared artifact names a path, a field and a reason",
        all(entry["path"] and entry["field"] and entry["why"] for entry in ARTIFACTS),
    )

    width = max(len(name) for name, _ in arms)
    for name, ok in arms:
        print(f"  {'ok  ' if ok else 'FAIL'} {name.ljust(width)}")
    passed = sum(1 for _, ok in arms if ok)
    print(f"\ncheck-adr-evidence-citations self-test: {passed}/{len(arms)} arms passed")
    return 0 if passed == len(arms) else 2


def main() -> int:
    if "--self-test" in sys.argv[1:]:
        return self_test()
    try:
        repo_root()
        return audit()
    except CannotAnswer as error:
        print(f"\n  CANNOT ANSWER: {error}")
        return 3


if __name__ == "__main__":
    sys.exit(main())
