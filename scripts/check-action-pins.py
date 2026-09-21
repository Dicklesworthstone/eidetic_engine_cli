#!/usr/bin/env python3
"""bd-uhml2 — every executable `uses:` reference must be pinned to a commit SHA.

WHY THIS EXISTS. A GitHub Actions reference like `actions/checkout@v4` names a
MUTABLE tag. Whoever controls that tag controls what runs in this repo's CI, and
a tag can be moved after review. Pinning to a 40-hex commit SHA makes the
reference immutable. Nothing in this repo enforced that:
scripts/check-toolchain-pins.sh enforces the TOOLCHAIN INPUT, and its own header
is explicit that the action ref and the toolchain input are different things.

MEASURED at 714c111ec, which is why this gate carries a baseline rather than a
hard fail: 264 executable `uses:` references across 43 of 51 workflow files, of
which 75 are SHA-pinned and 187 are mutable. release.yml is the only fully
pinned file (14 of 14). A gate that failed on all 187 could not be landed, and a
gate nobody can turn on protects nothing.

THE DEFECT IS A MECHANISM, NOT A COUNT. 15 files are mostly pinned and each
leaks the SAME pair -- `actions/cache/restore@v4` and `actions/cache/save@v4`.
One blind spot repeated: a pinning pass that matched `actions/cache@` and never
matched the SUB-PATH forms. This gate exists so the sixteenth cannot arrive
silently.

TWO CLASSIFICATION RULES THAT ARE ABOUT BEHAVIOUR, NOT TEXT, because a token
match here is evidence about a file and not about what runs:

  1. A `uses:` on a COMMENT line does not execute, and is not counted. Counting
     it would inflate the denominator with text.
  2. A local `./path` or `../path` reference has no upstream to pin and is
     excluded from the denominator rather than counted as a pass, which would
     quietly improve the ratio without improving anything.

IT PRINTS ITS PREDICATE AND ITS DENOMINATOR ON EVERY RUN, UNCONDITIONALLY.
The bd-1azkt.18 census that produced this gate found three mutually
incomparable counts of the same surface in one night, each using a different
predicate. A guard that states both beside its number cannot do that.

EXIT CODES
  0  every reference is pinned, or the unpinned ones are at/below baseline
  1  a file carries MORE unpinned references than its baseline row allows,
     or a baseline row is stale (forces the row down -- shrink-only)
  2  --self-test failed
  3  environment error (no .github/workflows) -- never reported as clean
"""

from __future__ import annotations

import pathlib
import re
import subprocess
import sys
import tempfile

REPO_ROOT = pathlib.Path(__file__).resolve().parent.parent
BASELINE = REPO_ROOT / "scripts" / "action-pins-baseline.txt"

SHA40 = re.compile(r"^[0-9a-f]{40}$")
DOCKER_DIGEST = re.compile(r"^docker://.+@sha256:[0-9a-f]{64}$")
USES = re.compile(r"^-?\s*uses:\s*([^\s#]+)")

PREDICATE = (
    "every executable `uses:` reference must be pinned to a 40-hex commit SHA "
    "(or a docker digest); comments and local ./ paths are excluded"
)


def classify(ref: str) -> str:
    """'skip' (unpinnable), 'pinned', or 'mutable'."""
    if ref.startswith("./") or ref.startswith("../"):
        return "skip"
    if ref.startswith("docker://"):
        return "pinned" if DOCKER_DIGEST.match(ref) else "mutable"
    if "@" not in ref:
        return "mutable"
    return "pinned" if SHA40.match(ref.rsplit("@", 1)[1]) else "mutable"


def scan_file(path: pathlib.Path) -> tuple[int, int, list[tuple[int, str]]]:
    """(counted, pinned, [(line_no, ref), ...]) for one workflow file."""
    counted = pinned = 0
    mutable: list[tuple[int, str]] = []
    for n, raw in enumerate(path.read_text(encoding="utf-8").splitlines(), 1):
        stripped = raw.strip()
        if stripped.startswith("#"):
            continue
        m = USES.match(stripped)
        if not m:
            continue
        ref = m.group(1).strip("\"'")
        verdict = classify(ref)
        if verdict == "skip":
            continue
        counted += 1
        if verdict == "pinned":
            pinned += 1
        else:
            mutable.append((n, ref))
    return counted, pinned, mutable


def read_baseline(path: pathlib.Path) -> dict[str, int]:
    rows: dict[str, int] = {}
    if not path.is_file():
        return rows
    for raw in path.read_text(encoding="utf-8").splitlines():
        line = raw.strip()
        if not line or line.startswith("#"):
            continue
        count, _, name = line.partition("\t")
        if not name:
            continue
        rows[name.strip()] = int(count.strip())
    return rows


def audit(root: pathlib.Path, baseline_path: pathlib.Path) -> int:
    wf_dir = root / ".github" / "workflows"
    if not wf_dir.is_dir():
        print(f"[action-pins] no .github/workflows under {root} — refusing to report clean",
              file=sys.stderr)
        return 3

    files = sorted(p for p in wf_dir.iterdir() if p.suffix in (".yml", ".yaml"))
    baseline = read_baseline(baseline_path)

    total_files = len(files)
    total_refs = total_pinned = 0
    per_file: dict[str, list[tuple[int, str]]] = {}
    for f in files:
        counted, pinned, mutable = scan_file(f)
        total_refs += counted
        total_pinned += pinned
        if mutable:
            per_file[f.name] = mutable

    total_mutable = total_refs - total_pinned

    print(f"[action-pins] predicate: {PREDICATE}")
    print(
        f"[action-pins] denominator: {total_files} workflow file(s); "
        f"{total_refs} pinnable reference(s); {total_pinned} pinned; "
        f"{total_mutable} mutable across {len(per_file)} file(s)"
    )
    accepted = sum(baseline.values())
    print(f"[action-pins] baseline: {accepted} accepted unpinned ref(s) "
          f"across {len(baseline)} file(s) (shrink-only)")

    failures = 0
    for name, mutable in sorted(per_file.items()):
        allowed = baseline.get(name, 0)
        if len(mutable) > allowed:
            failures += 1
            print(f"[action-pins] FAIL {name}: {len(mutable)} mutable ref(s), "
                  f"baseline allows {allowed}")
            for n, ref in mutable:
                print(f"    .github/workflows/{name}:{n}  {ref}")

    # Shrink-only: a baseline row larger than reality is stale and must come
    # down, otherwise the ratchet silently stops ratcheting.
    for name, allowed in sorted(baseline.items()):
        actual = len(per_file.get(name, []))
        if actual < allowed:
            failures += 1
            print(f"[action-pins] FAIL {name}: baseline allows {allowed} but only "
                  f"{actual} remain — lower the row to {actual} (shrink-only)")

    if failures:
        print(f"[action-pins] {failures} finding(s) above.")
        return 1
    print("[action-pins] OK — no mutable reference beyond the accepted baseline")
    return 0


def self_test() -> int:
    """Plant references whose correct classification is known, and assert it."""
    arms: list[str] = []
    failures: list[str] = []

    def arm(name: str, actual: object, expected: object) -> None:
        arms.append(name)
        ok = actual == expected
        print(f"  [{'ok  ' if ok else 'FAIL'}] {name}")
        if not ok:
            failures.append(f"{name}: expected {expected!r}, got {actual!r}")

    sha = "f713795cb21599bc4e5c4b58cbad1da852d7eeb9"
    arm("a 40-hex SHA ref is pinned", classify(f"actions/checkout@{sha}"), "pinned")
    arm("a tag ref is mutable", classify("actions/checkout@v4"), "mutable")
    arm("a sub-path tag ref is mutable (the 15-file blind spot)",
        classify("actions/cache/restore@v4"), "mutable")
    arm("a branch ref is mutable", classify("dtolnay/rust-toolchain@nightly"), "mutable")
    arm("a ref with no @ at all is mutable", classify("actions/checkout"), "mutable")
    arm("a local ./ path is unpinnable, not a pass", classify("./.github/actions/x"), "skip")
    arm("a docker image without a digest is mutable",
        classify("docker://alpine:3.20"), "mutable")
    arm("a docker image with a sha256 digest is pinned",
        classify("docker://alpine@sha256:" + "a" * 64), "pinned")

    with tempfile.TemporaryDirectory() as raw:
        root = pathlib.Path(raw)
        wf = root / ".github" / "workflows"
        wf.mkdir(parents=True)
        (root / "scripts").mkdir()
        bl = root / "scripts" / "action-pins-baseline.txt"

        (wf / "planted.yml").write_text(
            "jobs:\n"
            "  a:\n"
            "    steps:\n"
            f"      - uses: actions/checkout@{sha}\n"
            "      - uses: actions/cache/restore@v4\n"
            "      # - uses: actions/commented-out@v9\n"
            "      - uses: ./.github/actions/local\n",
            encoding="utf-8",
        )
        counted, pinned, mutable = scan_file(wf / "planted.yml")
        arm("comment line is not counted; local path is excluded",
            (counted, pinned, [r for _, r in mutable]),
            (2, 1, ["actions/cache/restore@v4"]))

        bl.write_text("", encoding="utf-8")
        arm("an unbaselined mutable ref FAILS", audit(root, bl), 1)

        bl.write_text("1\tplanted.yml\n", encoding="utf-8")
        arm("the same ref at its baseline PASSES", audit(root, bl), 0)

        bl.write_text("2\tplanted.yml\n", encoding="utf-8")
        arm("a STALE baseline row fails (shrink-only forces it down)",
            audit(root, bl), 1)

        # ENVIRONMENT ARM. This is what proves the 0 above means something: a
        # function that always returned 0 would pass the baseline arm and fail
        # only here.
        empty = root / "nowhere"
        empty.mkdir()
        arm("a missing .github/workflows is an environment error, not a pass",
            audit(empty, bl), 3)

    if not arms:
        print("\n[action-pins] self-test: ZERO arms ran — this is NOT a pass", file=sys.stderr)
        return 2
    if failures:
        print(f"\n[action-pins] self-test: {len(failures)} arm(s) FAILED:")
        for f in failures:
            print(f"    {f}")
        return 2
    print(f"\n[action-pins] self-test: {len(arms)}/{len(arms)} arms passed")
    return 0


def main(argv: list[str]) -> int:
    if argv[1:] == ["--self-test"]:
        return self_test()
    if argv[1:]:
        print(f"usage: {argv[0]} [--self-test]", file=sys.stderr)
        return 3
    return audit(REPO_ROOT, BASELINE)


if __name__ == "__main__":
    sys.exit(main(sys.argv))
