#!/usr/bin/env python3
"""How many `tests/golden/*.snap` artifacts does any code actually READ? (bd-qzr0v)

WHY THIS EXISTS. `scripts/closure-lint.sh` requires an `implements-surface` bead
to have a golden artifact, and discharges that with `surface_has_golden_snapshot`
-- a basename-existence test, now with a zero-byte check. Nothing requires a test
to OPEN the file. bd-qzr0v is the bead for that gap and it has been measured three
times, each time by counting whether the surface NAME appears somewhere:

    name appears in .rs/.sh/.py/.md/.toml, whole repo      98 of 113
    name appears in code directories only                  92 of 113
    FILE IS ACTUALLY READ BY CODE                           2 of 113   <- this

A name is not a consumer. Measured 2026-10-28, exactly TWO of the 113 files in
tests/golden/ are opened by anything other than the linter's own existence check:

    tests/golden/plan-decisioning.snap   src/core/plan.rs      include_str!
    tests/golden/skyline.snap            src/graph/skyline.rs  include_str!

FOUR MORE NAME A REAL FILE BY LITERAL PATH AND STILL DO NOT READ IT, which is
the finding rather than the arithmetic. Note the census reports only TWO of them
as "named only" -- the other two sit in COMMENTS, which this predicate treats as
prose, so they land in "not named at all". That is deliberate and the difference
is the point: a comment naming a golden is not a weaker consumer, it is not a
consumer.

    demo.snap                        inside a synthetic expected-JSON string
                                     written to a temp dir by
                                     verification_drift_guard.rs -- a fixture
    doctor_capabilities_scrubbed     a JSON FIELD VALUE naming the artifact in
                                     an emitted report, not a read
    hook_preflight_bash.snap         a COMMENT, and it says what it does
                                     instead: "assert the load-bearing contract
                                     markers documented in <the snap>"
    hook_preflight_zsh.snap          a COMMENT: "Structural golden MIRRORING
                                     <the snap>"

Those last two are the sharpest case. Each test duplicates the golden's contract
INLINE rather than comparing against the file, so the file and the test can drift
apart and nothing notices. A golden nobody reads is not a contract; it is a
second copy of one.

WHAT THIS GATE ENFORCES, AND WHAT IT DELIBERATELY DOES NOT.

It does NOT require consumption. Requiring it would red 111 of 113 surfaces in a
gate that blocks, which cannot land, and bd-qzr0v says so in its own acceptance
("flipping it to an error reds 14 surfaces at once" -- and the real number is far
worse than 14). Many of these files are declarations by design.

It enforces a FLOOR THAT ONLY RISES: the number of golden artifacts read by code
must not DECREASE. Delete the `include_str!` in src/graph/skyline.rs and this
goes red. That is the one invariant worth having while the ratio stays what it
is, because the failure mode here is silent decay, not sudden growth -- the two
remaining consumers are load-bearing precisely because there are only two.

SCOPE, STATED SO IT IS NOT WIDENED BY ACCIDENT. This covers `tests/golden/*.snap`
ONLY, and that narrowness is the whole reason the measurement is possible.
`tests/fixtures/golden/**` is a different population of 316 artifacts read by the
GoldenTest harness (tests/golden.rs), which builds
`tests/fixtures/golden/<category>/<name>.golden` out of its struct fields -- so no
literal-path search can see those consumers, and applying this predicate there
would report 316 unread files that are all live. closure-lint.sh carried a comment
saying a reader clause "deliberately does NOT belong here" for exactly that
reason; the comment was right about that directory and was being read as though it
covered this one too.

Access paths into tests/golden/ were ENUMERATED rather than assumed, because an
absence claim needs one: `include_str!`, `read_to_string`, `fs::read*`,
`File::open`, a `"tests/golden"` directory literal, `.join("golden")`, `read_dir`,
a glob, and `CARGO_MANIFEST_DIR` composition. Every `.join("golden")` site in the
tree -- and there are many -- composes `tests/fixtures/golden`, not this
directory. No path-constructing consumer of tests/golden/ exists, which is what
makes a literal-path predicate complete here and incomplete there.

EXIT CODES
  0  the read floor is met (or exceeded -- raise the floor in the same commit)
  1  FEWER artifacts are read than the floor: a consumer was removed
  2  --self-test failed
  3  environment: tests/golden/ is absent or empty, so nothing was measured.
     Never reported as a pass, because a silently-empty scan would otherwise
     report "0 missing consumers" having looked at nothing.
"""

from __future__ import annotations

import os
import pathlib
import re
import sys

REPO_ROOT = pathlib.Path(__file__).resolve().parent.parent

#: The floor. Raise it in the same commit that adds a consumer; never lower it.
#:
#: 2 on 2026-10-28, the measured count. This is NOT a target -- 113 would be the
#: wrong target, since most of these artifacts are declarations. It is a tripwire
#: against losing the two that are real.
READ_FLOOR = 2

#: Directories searched for consumers. Kept explicit rather than "the whole
#: repo": a mention in docs/ or in a bead is not a consumer, and counting one
#: would re-create the overcount this gate exists to correct.
CODE_DIRS = ("src", "tests", "scripts")
CODE_SUFFIXES = (".rs", ".sh", ".py")

#: A literal path into the directory under test.
GOLDEN_PATH = re.compile(r"tests/golden/([A-Za-z0-9_.-]+\.snap)")

#: A READ, as opposed to a mention. These are the constructs that actually open
#: a file; the enumeration in this module's docstring says why the list is
#: complete for THIS directory.
READ_CONSTRUCTS = (
    re.compile(r"include_str!\s*\(\s*\"[^\"]*tests/golden/([A-Za-z0-9_.-]+\.snap)"),
    re.compile(r"include_bytes!\s*\(\s*\"[^\"]*tests/golden/([A-Za-z0-9_.-]+\.snap)"),
    re.compile(r"read_to_string\s*\(\s*\"[^\"]*tests/golden/([A-Za-z0-9_.-]+\.snap)"),
    re.compile(r"File::open\s*\(\s*\"[^\"]*tests/golden/([A-Za-z0-9_.-]+\.snap)"),
)

#: A comment line, in any of the three languages scanned. A path inside a comment
#: is prose. Two of the four false consumers found on 2026-10-28 were comments,
#: and both comments described asserting the contract INLINE instead of reading
#: the file -- so counting them would have credited the exact arrangement this
#: gate is meant to surface.
COMMENT = re.compile(r"^\s*(//|#|/\*|\*)")


def code_files(root: pathlib.Path) -> list[pathlib.Path]:
    out = []
    for d in CODE_DIRS:
        base = root / d
        if not base.is_dir():
            continue
        for path in base.rglob("*"):
            if path.is_file() and path.suffix in CODE_SUFFIXES:
                out.append(path)
    return sorted(out)


def census(root: pathlib.Path) -> dict:
    golden_dir = root / "tests" / "golden"
    artifacts = sorted(
        p.name for p in golden_dir.glob("*.snap") if p.is_file()
    ) if golden_dir.is_dir() else []

    read: dict[str, list[str]] = {}
    mentioned: dict[str, list[str]] = {}
    self_path = pathlib.Path(__file__).resolve()
    for path in code_files(root):
        if path.resolve() == self_path:
            # THIS FILE NAMES EXAMPLE PATHS IN ITS OWN DOCSTRING. Scanning it
            # would credit artifacts because a comment here mentions them, which
            # is the precise error the COMMENT rule above exists to avoid.
            continue
        try:
            text = path.read_text(encoding="utf-8", errors="replace")
        except OSError:
            continue
        rel = str(path.relative_to(root))
        for line in text.splitlines():
            for rx in READ_CONSTRUCTS:
                for name in rx.findall(line):
                    read.setdefault(name, []).append(rel)
            if COMMENT.match(line):
                continue
            for name in GOLDEN_PATH.findall(line):
                mentioned.setdefault(name, []).append(rel)

    present = set(artifacts)
    return {
        "artifacts": artifacts,
        "read": {k: sorted(set(v)) for k, v in read.items() if k in present},
        # "mentioned" is an UPPER BOUND on consumption, not consumption. It is
        # printed so the gap between the two numbers is visible in one place.
        "mentioned": {
            k: sorted(set(v))
            for k, v in mentioned.items()
            if k in present and k not in read
        },
    }


def report(root: pathlib.Path) -> tuple[int, str]:
    data = census(root)
    artifacts = data["artifacts"]
    lines = []
    if not artifacts:
        lines.append(
            "[golden-consumption] INCONCLUSIVE: tests/golden/ holds no *.snap "
            "files, so nothing was measured. This is an environment result, not "
            "a pass."
        )
        return 3, "\n".join(lines)

    n_read = len(data["read"])
    n_mentioned = len(data["mentioned"])
    lines.append(
        "[golden-consumption] predicate: an artifact is READ when a literal "
        "tests/golden/<name>.snap appears inside include_str!/include_bytes!/"
        "read_to_string/File::open in src|tests|scripts (*.rs|*.sh|*.py); a path "
        "in a COMMENT is prose, not a consumer"
    )
    lines.append(
        f"[golden-consumption] denominator: {len(artifacts)} artifact(s) in "
        f"tests/golden/; {n_read} READ; {n_mentioned} named by literal path but "
        f"NOT read; {len(artifacts) - n_read - n_mentioned} not named at all"
    )
    for name, srcs in sorted(data["read"].items()):
        lines.append(f"    READ       {name}  <- {', '.join(srcs)}")
    for name, srcs in sorted(data["mentioned"].items()):
        lines.append(f"    named only {name}  <- {', '.join(srcs)}")

    if n_read < READ_FLOOR:
        lines.append(
            f"[golden-consumption] FAIL: {n_read} artifact(s) are read, below the "
            f"floor of {READ_FLOOR}. A consumer was removed. This floor only "
            f"rises: restore the consumer, or lower READ_FLOOR in the same commit "
            f"with the reason the artifact no longer needs reading."
        )
        return 1, "\n".join(lines)

    if n_read > READ_FLOOR:
        lines.append(
            f"[golden-consumption] OK, and the floor is STALE: {n_read} read "
            f"against a floor of {READ_FLOOR}. Raise READ_FLOOR to {n_read} in "
            f"the commit that added the consumer, so the gain cannot be lost "
            f"silently."
        )
        return 0, "\n".join(lines)

    lines.append(
        f"[golden-consumption] OK — {n_read} artifact(s) read, floor {READ_FLOOR} "
        f"met. NOT a coverage statement: {len(artifacts) - n_read} artifacts in "
        f"this directory are opened by nothing, and this gate deliberately does "
        f"not require them to be (see the module docstring)."
    )
    return 0, "\n".join(lines)


def self_test() -> int:
    import tempfile

    arms: list[bool] = []

    def arm(label: str, ok: bool) -> None:
        arms.append(ok)
        print(f"  [{'ok  ' if ok else 'FAIL'}] {label}")

    def tree(tmp: str, snaps: list[str], files: dict[str, str]) -> pathlib.Path:
        root = pathlib.Path(tmp)
        (root / "tests" / "golden").mkdir(parents=True)
        for s in snaps:
            (root / "tests" / "golden" / s).write_text("body\n")
        for rel, body in files.items():
            p = root / rel
            p.parent.mkdir(parents=True, exist_ok=True)
            p.write_text(body)
        return root

    # POSITIVE: an include_str! of the artifact counts as READ.
    with tempfile.TemporaryDirectory() as tmp:
        root = tree(tmp, ["a.snap", "b.snap"], {
            "src/x.rs": 'let e = include_str!("../tests/golden/a.snap");\n',
        })
        d = census(root)
        arm("include_str! of an artifact is READ", list(d["read"]) == ["a.snap"])
        arm("an artifact nothing names is neither read nor mentioned",
            "b.snap" not in d["read"] and "b.snap" not in d["mentioned"])

    # NEGATIVE, AND IT IS THE ARM THAT MATTERS. A path in a COMMENT is prose.
    # Both real-tree false consumers found on 2026-10-28 were comments whose own
    # text said the test asserts the contract inline instead of reading the file.
    with tempfile.TemporaryDirectory() as tmp:
        root = tree(tmp, ["c.snap"], {
            "tests/y.rs": "// Structural golden mirroring tests/golden/c.snap\n",
            "scripts/z.sh": "# see tests/golden/c.snap for the contract\n",
        })
        d = census(root)
        arm("a path in a // comment is NOT a consumer", "c.snap" not in d["read"])
        arm("a path in a # comment is NOT a consumer",
            "c.snap" not in d["mentioned"])

    # A literal path in CODE but not in a read construct is MENTIONED, not READ.
    # This is the doctor_capabilities_scrubbed shape: a JSON field value.
    with tempfile.TemporaryDirectory() as tmp:
        root = tree(tmp, ["d.snap"], {
            "tests/y.rs": 'json!({"artifact": "tests/golden/d.snap"});\n',
        })
        d = census(root)
        arm("a literal path in a data string is MENTIONED, not READ",
            "d.snap" in d["mentioned"] and "d.snap" not in d["read"])

    # THE FLOOR, BOTH POLARITIES. Without the failing arm the floor could be
    # removed and every run would still pass.
    with tempfile.TemporaryDirectory() as tmp:
        root = tree(tmp, ["a.snap", "b.snap"], {
            "src/x.rs": 'include_str!("../tests/golden/a.snap");\n'
                        'include_str!("../tests/golden/b.snap");\n',
        })
        code, text = report(root)
        arm("meeting the floor exactly exits 0",
            code == 0 and "floor is STALE" not in text)
    # EXCEEDING the floor needs READ_FLOOR + 1 consumers, not READ_FLOOR. The
    # first version of this arm reused the fixture above and asserted "stale" on
    # a run that MET the floor, so the arm failed on correct code -- the test was
    # wrong, not the gate.
    with tempfile.TemporaryDirectory() as tmp:
        root = tree(tmp, ["a.snap", "b.snap", "c.snap"], {
            "src/x.rs": 'include_str!("../tests/golden/a.snap");\n'
                        'include_str!("../tests/golden/b.snap");\n'
                        'include_str!("../tests/golden/c.snap");\n',
        })
        code, text = report(root)
        arm("exceeding the floor says the floor is STALE and still exits 0",
            code == 0 and "floor is STALE" in text)
    with tempfile.TemporaryDirectory() as tmp:
        root = tree(tmp, ["a.snap"], {"src/x.rs": "fn main() {}\n"})
        code, text = report(root)
        arm("falling BELOW the floor exits 1", code == 1)
        arm("the failure names the floor and says it only rises",
            "below the floor" in text and "only" in text)

    # EMPTY-WORLD GUARD: no artifacts must be INCONCLUSIVE, never a pass.
    with tempfile.TemporaryDirectory() as tmp:
        root = pathlib.Path(tmp)
        (root / "tests" / "golden").mkdir(parents=True)
        code, text = report(root)
        arm("an empty tests/golden is exit 3 INCONCLUSIVE, not a pass",
            code == 3 and "INCONCLUSIVE" in text)

    # The live tree must report a non-zero denominator, or the predicate has
    # stopped matching and a clean answer would mean nothing.
    live = census(REPO_ROOT)
    arm("the live tree reports a non-empty denominator",
        len(live["artifacts"]) > 50)
    arm("the live tree's read set is exactly the two measured consumers",
        sorted(live["read"]) == ["plan-decisioning.snap", "skyline.snap"])

    failed = arms.count(False)
    print()
    if failed:
        print(f"[golden-consumption] self-test: {failed} of {len(arms)} arm(s) FAILED")
        return 2
    print(f"[golden-consumption] self-test: {len(arms)}/{len(arms)} arms passed")
    return 0


def main() -> int:
    if "--self-test" in sys.argv[1:]:
        return self_test()
    root = REPO_ROOT
    if "--root" in sys.argv[1:]:
        root = pathlib.Path(sys.argv[sys.argv.index("--root") + 1])
    code, text = report(root)
    stream = sys.stdout if code == 0 else sys.stderr
    print(text, file=stream)
    return code


if __name__ == "__main__":
    sys.exit(main())
