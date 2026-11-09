#!/usr/bin/env python3
"""An ADR that cites evidence by content hash must cite a hash that EXISTED.

WHY THIS EXISTS. `docs/adr/0030-typed-determinism-capability-token.md` records
a decision taken on the `tests/randomness_inventory.json` of 2026-05-13 and
pins that inventory by its `rows_content_hash`. A hash is exactly the kind of
token a reader trusts without re-deriving, so a citation to a hash that was
never this file's hash -- mistyped, or copied from the wrong artifact, or
invented -- is indistinguishable from a correct one.

`tests/adr_0030_docs.rs` cannot answer that question, and this is not a
criticism of it: `include_str!` hands a test ONE revision of the artifact, and
whether a hash was ever a revision of it is a fact about git history. That test
used to reach for the question anyway, by asserting the CURRENT inventory still
contained the 2026-05-13 hash, which the design makes impossible -- the
inventory is a timestamped snapshot (bd-2cdum), so the first regeneration broke
the arm and it stayed red for 25 days across three regenerations. Nothing saw
it, because verify.sh refuses before stage one on the maintainer's host, hosted
Actions is off repo-wide, and the arm reaches a runner only inside a 773-test
shard.

So this gate exists to answer the question that is actually answerable, on a
rail that actually runs. It needs git and nothing else: no cargo, no `ee`
binary, no dispatch. It is deliberately NOT added to verify.sh, which has about
six seconds of non-benchmark headroom against a fixed 608 s ceiling; it is in
the `scripts/check-*.py` family that CLAUDE.md tells agents to run before a
commit, which is also the host where a regeneration happens.

It also discharges bd-2cdum's first acceptance criterion, which was never
implemented: "the file is explicitly recorded as a point-in-time snapshot so
`rows_content_hash` citations carry their generation date". Every PASS line
below prints the revision and date the cited hash belongs to, so the citation's
age is visible without archaeology.

WHAT IT DOES NOT CHECK, stated so the green is not over-read. It does not check
that a citation is CURRENT -- staleness between dated snapshots is the design,
and asserting currency is the exact mistake it was written to replace. It does
not check the ADR's prose numbers against the revision it names; that the ADR's
"316 sources" and "261 systemtime" match `3c2b4e4e9` was verified by hand on
2026-11-08 and is recorded in the Rust test's docstring, not enforced here.

Usage:
  scripts/check-adr-evidence-citations.py              audit the declared citations
  scripts/check-adr-evidence-citations.py --self-test  prove every failure direction

Exit: 0 clean, 1 gate failure, 2 self-test failure, 3 environment error.
"""

from __future__ import annotations

import os
import re
import subprocess
import sys

# Each declared citation, with the reason it is pinned printed on failure so the
# next reader learns it from the failure rather than from archaeology. Same
# shape as scripts/check-pinned-definitions.sh's PINNED table.
CITATIONS = [
    {
        "adr": "docs/adr/0030-typed-determinism-capability-token.md",
        "artifact": "tests/randomness_inventory.json",
        "field": "rows_content_hash",
        "why": (
            "ADR 0030 records the N4.1 randomness audit it was decided on and "
            "pins it by content hash; N4.2's bead body cites the same hash, so "
            "a hash that was never this artifact's hash silently misdescribes "
            "the evidence three decisions rest on (bd-2cdum)"
        ),
    },
]

HASH_RE = re.compile(r"blake3-ish:[0-9a-f]{64}")

# A revision walk has to be bounded, and a truncated walk must SAY so rather
# than report a verdict it did not earn. No artifact here is near this.
REVISION_CAP = 200


class CannotAnswer(Exception):
    """The environment cannot support a verdict. Exit 3, never a pass."""


def repo_root() -> str:
    root = os.environ.get("EE_REPO_ROOT")
    if root:
        return root
    try:
        done = subprocess.run(
            ["git", "rev-parse", "--show-toplevel"],
            capture_output=True,
            check=True,
        )
    except (OSError, subprocess.CalledProcessError) as error:
        raise CannotAnswer(f"git rev-parse failed: {error}") from error
    return done.stdout.decode("utf-8", errors="replace").strip()


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


def classify(
    cited: list[str], revisions: list[tuple[str, str, str]]
) -> tuple[list[tuple[str, str, str]], list[str]]:
    """Pure. (matched as [(hash, sha, date)], unmatched hashes).

    Pure so the self-test can drive every branch without a repository, which is
    the same reason scripts/hooks/pre-commit/45-symbol-orphans.py factored its
    `decide`.
    """
    by_hash = {value: (sha, date) for sha, date, value in revisions}
    matched, unmatched = [], []
    for hash_value in cited:
        where = by_hash.get(hash_value)
        if where is None:
            unmatched.append(hash_value)
        else:
            matched.append((hash_value, where[0], where[1]))
    return matched, unmatched


def audit(root: str) -> int:
    failures: list[str] = []
    print()
    for entry in CITATIONS:
        adr, artifact = entry["adr"], entry["artifact"]
        field, why = entry["field"], entry["why"]
        print(f"  {adr}")
        print(f"    cites {artifact} by {field}")
        try:
            with open(os.path.join(root, adr), encoding="utf-8") as handle:
                text = handle.read()
        except OSError as error:
            raise CannotAnswer(f"cannot read {adr}: {error}") from error

        cited = sorted(set(HASH_RE.findall(text)))
        if not cited:
            # A declared citation with no hash in it cannot pass by finding
            # nothing: that is the vacuous direction, and it is a failure.
            print(f"    FAIL: no {field}-shaped hash found in the ADR at all")
            print(f"          why this is pinned: {why}")
            failures.append(adr)
            continue

        revisions, truncated = revisions_of(artifact, field)
        if not revisions:
            raise CannotAnswer(
                f"no revision of {artifact} carries a `{field}`; the walk found "
                f"nothing to compare against, which is no verdict rather than a pass"
            )
        if truncated:
            print(
                f"    (walk capped at {REVISION_CAP} revisions; a cited hash older "
                f"than that would read as unmatched -- raise the cap, do not trust this)"
            )

        matched, unmatched = classify(cited, revisions)
        for hash_value, sha, date in matched:
            current = " (the current revision)" if (sha, date) == (
                revisions[-1][0],
                revisions[-1][1],
            ) else ""
            print(f"    ok   {hash_value[:22]}... is {artifact} at {sha}, {date}{current}")
        for hash_value in unmatched:
            print(f"    FAIL: {hash_value[:22]}... matches NO revision of {artifact}")
            print(f"          {len(revisions)} revision(s) carry a `{field}`:")
            for sha, date, value in revisions:
                print(f"            {sha}  {date}  {value[:22]}...")
            print(f"          why this is pinned: {why}")
            failures.append(adr)

    print()
    if failures:
        print(f"  FAIL: {len(failures)} citation failure(s). An ADR must cite a hash")
        print("  that was really this artifact's hash at some revision. If the ADR is")
        print("  right and the artifact moved, that is EXPECTED -- snapshots are dated")
        print("  (bd-2cdum) -- and nothing here needs changing. Fix the ADR only when")
        print("  its hash was never real.")
        return 1
    total = sum(len(set(HASH_RE.findall(open(os.path.join(root, c["adr"]), encoding="utf-8").read()))) for c in CITATIONS)
    print(f"  OK: {total} cited hash(es) across {len(CITATIONS)} ADR(s) each match a real revision.")
    return 0


def self_test() -> int:
    arms: list[tuple[str, bool]] = []

    def arm(name: str, ok: bool) -> None:
        arms.append((name, ok))

    revs = [
        ("aaaaaaa", "2026-05-13", "blake3-ish:" + "1" * 64),
        ("bbbbbbb", "2026-10-14", "blake3-ish:" + "2" * 64),
        ("ccccccc", "2026-10-20", "blake3-ish:" + "3" * 64),
    ]

    matched, unmatched = classify(["blake3-ish:" + "1" * 64], revs)
    arm(
        "arm1 a hash from the OLDEST revision matches, and names that revision",
        not unmatched and matched == [("blake3-ish:" + "1" * 64, "aaaaaaa", "2026-05-13")],
    )

    matched, unmatched = classify(["blake3-ish:" + "3" * 64], revs)
    arm(
        "arm2 a hash from the CURRENT revision matches too",
        not unmatched and matched[0][1] == "ccccccc",
    )

    matched, unmatched = classify(["blake3-ish:" + "9" * 64], revs)
    arm(
        "arm3 a hash matching no revision is REPORTED, not waved through",
        unmatched == ["blake3-ish:" + "9" * 64] and not matched,
    )

    matched, unmatched = classify([], revs)
    arm(
        "arm4 citing nothing yields nothing matched -- the caller must fail it",
        not matched and not unmatched,
    )

    matched, unmatched = classify(["blake3-ish:" + "1" * 64], [])
    arm(
        "arm5 with no revisions to compare, a real hash is NOT silently matched",
        unmatched == ["blake3-ish:" + "1" * 64],
    )

    # The regex is the other half of the instrument, and a too-loose one would
    # match prose. Both directions.
    arm(
        "arm6 the hash pattern matches a real citation",
        HASH_RE.findall("Its hash is:\n\n`blake3-ish:" + "a" * 64 + "`\n")
        == ["blake3-ish:" + "a" * 64],
    )
    arm(
        "arm7 the pattern rejects a short, upper-case or unprefixed digest",
        not HASH_RE.findall("blake3-ish:" + "a" * 63)
        and not HASH_RE.findall("blake3-ish:" + "A" * 64)
        and not HASH_RE.findall("sha256:" + "a" * 64),
    )

    # And the field reader must not be fooled by a different key whose name
    # CONTAINS the field name, which is the substring trap this repo keeps
    # hitting.
    field_re = re.compile(r'"rows_content_hash"\s*:\s*"([^"]+)"')
    arm(
        "arm8 the field reader does not match a longer key containing the name",
        field_re.search('{"prev_rows_content_hash": "blake3-ish:x"}') is None
        and field_re.search('{"rows_content_hash": "blake3-ish:y"}') is not None,
    )

    # A declared citation whose ADR holds no hash must be a FAILURE and not a
    # vacuous pass. That branch lives in audit(), so assert the predicate it
    # uses rather than restating the branch.
    arm(
        "arm9 an ADR with no hash at all is detected as citing nothing",
        not HASH_RE.findall("# ADR 9999\n\nNo evidence is cited here.\n"),
    )

    arm(
        "arm10 every declared citation names an ADR and an artifact under the repo",
        all(
            entry["adr"].startswith("docs/")
            and entry["artifact"]
            and entry["field"]
            and entry["why"]
            for entry in CITATIONS
        ),
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
        root = repo_root()
        os.chdir(root)
        return audit(root)
    except CannotAnswer as error:
        print(f"\n  CANNOT ANSWER: {error}")
        return 3


if __name__ == "__main__":
    sys.exit(main())
