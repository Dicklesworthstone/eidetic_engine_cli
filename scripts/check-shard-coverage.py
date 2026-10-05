#!/usr/bin/env python3
"""Fail when a `core::` submodule falls into no CI shard, or into two (bd-479c6).

WHY THIS EXISTS
    `core::` was one shard carrying 5992 of the lib suite's 11973 tests -- 50% in
    a single matrix entry -- and measurably could not finish: a run with
    --test-threads=1 completed 204 of 5976 announced tests inside 7200s INCLUDING
    a cold build (3.4%), against a 90m timeout in ci.yml. It is now split six ways
    by the FIRST LETTER of the submodule, because libtest filters are substrings
    (`core::a` matches only immediately after `core::`) while an alphabetical
    RANGE is not expressible as a filter at all.

    A letter-keyed split has exactly one failure mode, and it is silent: add
    `src/core/kafka_bridge.rs` and, if no shard claims `core::k`, its tests run in
    NO shard and the matrix still goes green. That is the shape this repository
    keeps paying for -- a gate that passes over an empty population -- so the
    split ships with this check rather than with a comment asking people to
    remember.

WHAT IT ASSERTS
    1. Every first letter of an existing src/core/*.rs module is claimed by
       exactly one shard arm.
    2. No letter is claimed by two arms (which would double-run tests and make
       shard timings lie).
    3. All 26 letters are claimed, so a FUTURE module cannot land unclaimed. This
       is stricter than (1) on purpose: (1) alone would pass today and fail only
       once someone adds the module, which is exactly too late.

    It does NOT check test counts or balance. Balance is a performance property
    that drifts continuously; coverage is a correctness property that must not.

USAGE
    scripts/check-shard-coverage.py
    scripts/check-shard-coverage.py --json
    scripts/check-shard-coverage.py --self-test
"""

from __future__ import annotations

import argparse
import json
import os
import re
import string
import sys
import tempfile
from pathlib import Path

WORKFLOW = ".github/workflows/ci.yml"
CORE_DIR = "src/core"
# Matches the shard arms this check governs, e.g.
#   lib-core-a)
#     run_test "..." cargo test ... -- --test-threads=1 --nocapture core::a core::i
ARM_RE = re.compile(r"^\s*(lib-core-[a-z]+)\)\s*$")
FILTER_RE = re.compile(r"core::([a-z])\b")


def shard_letter_map(workflow_text: str) -> dict[str, list[str]]:
    """{shard name: [letters it filters on]} from the case arms."""
    out: dict[str, list[str]] = {}
    lines = workflow_text.splitlines()
    for index, line in enumerate(lines):
        m = ARM_RE.match(line)
        if not m:
            continue
        name = m.group(1)
        letters: list[str] = []
        # The arm body runs until the `;;` terminator.
        for body in lines[index + 1 : index + 12]:
            if body.strip() == ";;":
                break
            # Only the filters AFTER `--` are test filters. Anything before it is
            # a cargo argument, and `core::` can legitimately appear in the
            # run_test label, so split on the separator first.
            if " -- " in body:
                tail = body.split(" -- ", 1)[1]
            else:
                continue
            letters.extend(FILTER_RE.findall(tail))
        out[name] = letters
    return out


def core_module_letters(root: Path) -> dict[str, list[str]]:
    """{first letter: [module names]} for src/core/*.rs."""
    out: dict[str, list[str]] = {}
    core = root / CORE_DIR
    if not core.is_dir():
        return out
    for entry in sorted(os.listdir(core)):
        if not entry.endswith(".rs"):
            continue
        name = entry[:-3]
        if not name:
            continue
        out.setdefault(name[0], []).append(name)
    return out


def audit(root: Path) -> dict:
    wf = root / WORKFLOW
    workflow_text = wf.read_text(encoding="utf-8", errors="replace") if wf.is_file() else ""
    shards = shard_letter_map(workflow_text)
    modules = core_module_letters(root)

    claimed: dict[str, list[str]] = {}
    for shard, letters in shards.items():
        for letter in letters:
            claimed.setdefault(letter, []).append(shard)

    unclaimed_live = sorted(
        letter for letter in modules if letter not in claimed
    )
    duplicated = sorted(
        letter for letter, owners in claimed.items() if len(owners) > 1
    )
    unclaimed_alphabet = sorted(
        letter for letter in string.ascii_lowercase if letter not in claimed
    )
    return {
        "schema": "ee.shard_coverage.v1",
        "shards": {k: sorted(v) for k, v in sorted(shards.items())},
        "coreModuleLetters": sorted(modules),
        "unclaimedLiveLetters": unclaimed_live,
        "duplicatedLetters": {
            letter: sorted(claimed[letter]) for letter in duplicated
        },
        "unclaimedAlphabetLetters": unclaimed_alphabet,
        "modulesAtRisk": sorted(
            name for letter in unclaimed_live for name in modules[letter]
        ),
    }


def report(result: dict) -> int:
    shards = result["shards"]
    print(
        f"[shard-coverage] {len(shards)} lib-core shard(s); "
        f"{len(result['coreModuleLetters'])} distinct first letters among "
        f"src/core/*.rs modules"
    )
    for name, letters in shards.items():
        print(f"    {name:16} {' '.join('core::' + l for l in letters)}")

    status = 0
    if not shards:
        print(
            f"[shard-coverage] FAIL: no lib-core-* arms found in {WORKFLOW}. "
            "Either the split was reverted or the arm naming changed; this check "
            "must be updated in the same commit.",
            file=sys.stderr,
        )
        return 1
    if result["unclaimedLiveLetters"]:
        print(
            "[shard-coverage] FAIL: these core submodules fall into NO shard, so "
            "their tests would run nowhere while the matrix stayed green:",
            file=sys.stderr,
        )
        for letter in result["unclaimedLiveLetters"]:
            print(f"    core::{letter}*  -> ", file=sys.stderr, end="")
            print(
                ", ".join(
                    n for n in result["modulesAtRisk"] if n.startswith(letter)
                ),
                file=sys.stderr,
            )
        status = 1
    if result["duplicatedLetters"]:
        print(
            "[shard-coverage] FAIL: these letters are claimed by more than one "
            "shard, which double-runs their tests and makes shard timings lie:",
            file=sys.stderr,
        )
        for letter, owners in result["duplicatedLetters"].items():
            print(f"    core::{letter}  claimed by {', '.join(owners)}", file=sys.stderr)
        status = 1
    if result["unclaimedAlphabetLetters"]:
        print(
            "[shard-coverage] FAIL: these letters are claimed by no shard, so a "
            "FUTURE src/core module starting with one would run in no shard: "
            + " ".join(result["unclaimedAlphabetLetters"]),
            file=sys.stderr,
        )
        print(
            "    Add them to an arm now. Catching this when the module lands is "
            "too late -- the matrix goes green either way.",
            file=sys.stderr,
        )
        status = 1
    if status == 0:
        print(
            "[shard-coverage] OK -- every letter claimed exactly once, including "
            "the ones no module uses yet"
        )
    return status


# ---------------------------------------------------------------------------
# Self-test. Each arm asserts a POLARITY. The three refusals are the point: a
# check that cannot be seen to fail is decoration.
# ---------------------------------------------------------------------------

GOOD_ARMS = """
            lib-core-a)
              run_test "x" cargo test --lib -- --test-threads=1 core::a core::i
              ;;
            lib-core-rest)
              run_test "y" cargo test --lib -- --test-threads=1 core::b core::c core::d core::e core::f core::g core::h core::j core::k core::l core::m core::n core::o core::p core::q core::r core::s core::t core::u core::v core::w core::x core::y core::z
              ;;
"""


def _plant(root: Path, arms: str, modules: list[str]) -> None:
    (root / ".github" / "workflows").mkdir(parents=True, exist_ok=True)
    (root / WORKFLOW).write_text("jobs:\n  verify-tests:\n" + arms, encoding="utf-8")
    (root / CORE_DIR).mkdir(parents=True, exist_ok=True)
    for name in modules:
        (root / CORE_DIR / f"{name}.rs").write_text("#[test]\nfn t() {}\n", encoding="utf-8")


def self_test() -> int:
    arms: list[tuple[str, bool]] = []

    def arm(label: str, ok: bool) -> None:
        arms.append((label, ok))
        print(f"  [{'ok  ' if ok else 'FAIL'}] {label}")

    with tempfile.TemporaryDirectory(prefix="shardcov-") as tmp:
        root = Path(tmp)
        _plant(root, GOOD_ARMS, ["alpha", "index", "backup", "search"])
        clean = audit(root)
        arm("a complete partition passes", report_silent(clean) == 0)
        arm("it reads both arms", len(clean["shards"]) == 2)

        # REFUSAL 1: a live module whose letter no arm claims.
        root2 = Path(tempfile.mkdtemp(dir=tmp))
        _plant(root2, GOOD_ARMS.replace(" core::k", ""), ["kafka_bridge", "alpha"])
        missing = audit(root2)
        arm(
            "a live module in NO shard is caught",
            "k" in missing["unclaimedLiveLetters"]
            and "kafka_bridge" in missing["modulesAtRisk"],
        )
        arm("and it fails", report_silent(missing) == 1)

        # REFUSAL 2: a letter claimed twice.
        root3 = Path(tempfile.mkdtemp(dir=tmp))
        _plant(root3, GOOD_ARMS.replace("core::b ", "core::b core::a "), ["alpha"])
        dup = audit(root3)
        arm("a letter claimed by two shards is caught", "a" in dup["duplicatedLetters"])
        arm("and it fails", report_silent(dup) == 1)

        # REFUSAL 3: an unused letter left unclaimed -- the FUTURE-module hole.
        root4 = Path(tempfile.mkdtemp(dir=tmp))
        _plant(root4, GOOD_ARMS.replace(" core::z", ""), ["alpha"])
        future = audit(root4)
        arm(
            "an unclaimed letter with no module today is still caught",
            "z" in future["unclaimedAlphabetLetters"],
        )
        arm("and it fails", report_silent(future) == 1)

        # REFUSAL 4: the arms disappearing entirely must not pass silently.
        root5 = Path(tempfile.mkdtemp(dir=tmp))
        _plant(root5, "\n", ["alpha"])
        gone = audit(root5)
        arm("no lib-core arms at all fails rather than passing", report_silent(gone) == 1)

        # A `core::` mention in the run_test LABEL must not count as a filter.
        root6 = Path(tempfile.mkdtemp(dir=tmp))
        label_only = GOOD_ARMS.replace('run_test "x"', 'run_test "cargo test --lib core::zz"')
        _plant(root6, label_only, ["alpha"])
        lab = audit(root6)
        arm(
            "a core:: mention before `--` is not read as a filter",
            "z" in lab["shards"]["lib-core-rest"] and lab["duplicatedLetters"] == {},
        )

    passed = sum(1 for _, ok in arms if ok)
    print(f"\n[shard-coverage] self-test: {passed}/{len(arms)} arms passed")
    return 0 if passed == len(arms) else 1


def report_silent(result: dict) -> int:
    """report() without the printing, for self-test arms."""
    if not result["shards"]:
        return 1
    return (
        1
        if result["unclaimedLiveLetters"]
        or result["duplicatedLetters"]
        or result["unclaimedAlphabetLetters"]
        else 0
    )


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--root", default=".")
    parser.add_argument("--json", action="store_true")
    parser.add_argument("--self-test", action="store_true")
    args = parser.parse_args()
    if args.self_test:
        return self_test()
    result = audit(Path(args.root).resolve())
    if args.json:
        print(json.dumps(result, indent=2, sort_keys=True))
        return report_silent(result)
    return report(result)


if __name__ == "__main__":
    raise SystemExit(main())
