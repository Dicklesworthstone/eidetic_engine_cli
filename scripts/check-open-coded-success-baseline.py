#!/usr/bin/env python3
"""Local rail for `open_coded_success_assertions_do_not_grow`, which needs a dispatch.

WHY THIS EXISTS. That arm is the ratchet keeping incomplete success assertions
from growing under `tests/` -- an assertion that reports THAT a command failed
and discards what says why. It lives in `tests/verification_drift_guard.rs`,
which `Cargo.toml`'s `autotests = false` leaves reachable only through a
`#[path]` shard, so a verdict costs a full `cargo test --test
verification_drift_guard` dispatch. On the maintainer's host there is no cargo
at all.

It went RED and stayed red for 21 days. The baseline was last written
2026-10-18 at 80a66c411; by 2026-11-08 ten sites had grown and four had been
repaired without the baseline being retired, and nothing on this host could
see any of it (bd-mv9j7). This check answers the same question in ~0.3 s with
no toolchain.

THIS IS A SECOND COPY OF A RULE, WHICH THIS REPO NORMALLY FORBIDS, so it carries
two defences against the drift that makes second copies dangerous:

  1. A PINNED SOURCE HASH. The Rust detector's own source span is hashed, and a
     change to it makes this script exit 3 -- CANNOT ANSWER -- naming the span,
     rather than reporting a verdict under rules that have moved. Fail closed,
     never a confident answer from a stale copy. See
     scripts/check-pinned-definitions.sh for the same idea.
  2. THE BASELINE IS A CONTINUOUS ANSWER KEY. The recorded TSV is what the Rust
     detector produced over this same tree, so agreement is not a coincidence
     that needs checking separately -- it IS the verdict. A transliteration that
     had drifted would disagree with the baseline on hundreds of rows, not on
     the handful a real regression moves, and the summary prints both totals so
     that difference is visible rather than inferred.

Calibrated 2026-11-08: this scanner reproduced the baseline recorded at
80a66c411 key-for-key and count-for-count over that revision's tests/ --
591 keys, 866 sites, zero differences in either direction.

WHAT IT DOES NOT DO. It never writes the baseline. Regeneration stays
`UPDATE_GOLDEN=1 cargo test --test verification_drift_guard
open_coded_success_assertions_do_not_grow`, because a golden belongs to its
generator. When this check reports repaired-but-still-listed rows it names them
so the regeneration can be reviewed rather than trusted.

It is NOT a verify.sh stage. The non-benchmark budget had 3 s of headroom after
the Degraded Codes Doc Fingerprint stage landed, and the Rust arm is already a
gated stage on the dispatch rail. This belongs to the `scripts/check-*.py`
family CLAUDE.md tells agents to run before a commit.

Usage:
  scripts/check-open-coded-success-baseline.py              compare tree to baseline
  scripts/check-open-coded-success-baseline.py --self-test  prove every direction

Exit: 0 clean, 1 grew or shrank, 2 self-test failure, 3 cannot answer.
"""

from __future__ import annotations

import hashlib
import os
import pathlib
import re
import subprocess
import sys
from collections import Counter

GUARD = "tests/verification_drift_guard.rs"
BASELINE = "tests/fixtures/golden/open_coded_success_baseline.tsv"
SELF_EXCLUSION = "verification_drift_guard"
ASSERTION_WINDOW = 14

# The detector's source span, hashed so a change to the rules this script
# mirrors stops it answering. START is the window constant; END is the first
# item AFTER the classifier, so the span is exactly
# ASSERTION_WINDOW + assertion_window + mentions_token + DiagnosticFacts +
# scan_open_coded_success_sites + scan_incomplete_success_assertions.
DETECTOR_SPAN_START = "const ASSERTION_WINDOW: usize = 14;"
DETECTOR_SPAN_END = "const OPEN_CODED_BASELINE_HEADER: &str ="
DETECTOR_SPAN_SHA256 = "525c18535b22055d17a798dcf99b3bd9f48e33b7e183139e6d1881b67a627ae4"

# Lines outside that span this script also depends on.
EXTRA_PINNED_LINES = (
    'const OPEN_CODED_SELF_EXCLUSION: &str = "verification_drift_guard";',
    'const OPEN_CODED_BASELINE: &str = "tests/fixtures/golden/open_coded_success_baseline.tsv";',
)


class CannotAnswer(Exception):
    """The environment or the pinned source cannot support a verdict. Exit 3."""


def repo_root() -> pathlib.Path:
    root = os.environ.get("EE_REPO_ROOT")
    if not root:
        try:
            done = subprocess.run(
                ["git", "rev-parse", "--show-toplevel"], capture_output=True, check=True
            )
        except (OSError, subprocess.CalledProcessError) as error:
            raise CannotAnswer(f"git rev-parse failed: {error}") from error
        root = done.stdout.decode("utf-8", errors="replace").strip()
    path = pathlib.Path(root)
    if not path.is_dir():
        raise CannotAnswer(f"repo root {root!r} is not a directory")
    return path


def detector_span(root: pathlib.Path) -> str:
    """The pinned source text, or CannotAnswer if its anchors moved."""
    try:
        source = (root / GUARD).read_text(errors="replace")
    except OSError as error:
        raise CannotAnswer(f"cannot read {GUARD}: {error}") from error
    start = source.find(DETECTOR_SPAN_START)
    end = source.find(DETECTOR_SPAN_END)
    if start < 0 or end < 0 or end <= start:
        raise CannotAnswer(
            f"could not bound the detector span in {GUARD}: "
            f"start={'found' if start >= 0 else 'MISSING'} "
            f"end={'found' if end >= 0 else 'MISSING'}. The anchors moved, so "
            "this script cannot know whether it still mirrors the rules."
        )
    span = source[start:end]
    for line in EXTRA_PINNED_LINES:
        if line not in source:
            raise CannotAnswer(
                f"{GUARD} no longer contains the pinned line:\n    {line}\n"
                "This script depends on it, so it refuses to report a verdict."
            )
        span += "\n" + line
    return span


def check_pin(root: pathlib.Path) -> str:
    span = detector_span(root)
    digest = hashlib.sha256(span.encode("utf-8")).hexdigest()
    if DETECTOR_SPAN_SHA256 != digest:
        raise CannotAnswer(
            "the Rust detector's source has changed since this script was "
            f"calibrated.\n  pinned : {DETECTOR_SPAN_SHA256}\n  actual : {digest}\n"
            f"  span   : {GUARD}, from `{DETECTOR_SPAN_START}`\n"
            f"           to `{DETECTOR_SPAN_END}`\n"
            "Re-read that span, update this script to match, re-validate it "
            "against the baseline, and then update the pin. Answering now "
            "would be a confident verdict from a stale copy of the rules."
        )
    return digest


# --- the transliterated detector -------------------------------------------


def assertion_window(lines: list[str], index: int) -> str:
    start = index - 1 if index > 0 and lines[index - 1].strip().endswith("ensure(") else index
    hard_end = min(start + ASSERTION_WINDOW, len(lines))
    depth = 0
    opened = False
    end = start
    while end < hard_end:
        for ch in lines[end]:
            if ch in "({[":
                depth += 1
                opened = True
            elif ch in ")}]":
                depth -= 1
        end += 1
        if opened and depth <= 0:
            break
    return "\n".join(lines[start:end])


def _word_char(ch: str) -> bool:
    return ch.isalnum() or ch == "_"


def mentions_token(window: str, token: str) -> bool:
    """Whole word, so a helper NAMED stdout_json is not evidence of printing it."""
    at = window.find(token)
    while at != -1:
        before_ok = at == 0 or not _word_char(window[at - 1])
        after = at + len(token)
        after_ok = after >= len(window) or not _word_char(window[after])
        if before_ok and after_ok:
            return True
        at = window.find(token, at + 1)
    return False


def missing_of(code: bool, stdout: bool, stderr: bool) -> str:
    out = []
    if not code:
        out.append("code")
    if not stdout:
        out.append("stdout")
    if not stderr:
        out.append("stderr")
    return "+".join(out)


def enclosing_name(raw: str) -> str | None:
    """The fn name when `raw` declares one AT COLUMN ZERO, else None.

    Column zero matters: the Rust tests `raw.starts_with("fn ")` before
    trimming, so an indented fn -- nested, or in an impl -- does not change the
    enclosing name. Faithful rather than improved; a "better" rule here would
    disagree with the baseline.
    """
    if not (raw.startswith("fn ") or raw.startswith("pub fn ")):
        return None
    rest = raw[len("pub fn ") :] if raw.startswith("pub fn ") else raw[len("fn ") :]
    name = ""
    for ch in rest:
        if ch.isalnum() or ch == "_":
            name += ch
        else:
            break
    return name or None


def scan_file(stem: str, body: str) -> list[tuple[str, int, str]]:
    """[(key, 1-based line, source text)] for every INCOMPLETE site in one file."""
    found: list[tuple[str, int, str]] = []
    lines = body.split("\n")
    enclosing = "<file scope>"
    for index, raw in enumerate(lines):
        name = enclosing_name(raw)
        if name:
            enclosing = name

        line = raw.strip()
        if line.startswith("//"):
            continue
        success_spelling = ".status.success()" in line
        code_spelling = ".status.code()" in line
        if not success_spelling and not code_spelling:
            continue
        # `!` alone does not mean "asserts failure": `if !out.status.success() {
        # return Err(..) }` asserts SUCCESS. The construct disambiguates.
        negated_ensure = "ensure(!" in line or (
            line.startswith("!") and index > 0 and lines[index - 1].strip().endswith("ensure(")
        )
        if negated_ensure:
            continue
        if not success_spelling and not ("Some(0)" in line or "Some(EXIT_SUCCESS)" in line):
            continue

        window = assertion_window(lines, index)
        asserts = (
            "Err(" in window
            or "ensure(" in line
            or "ensure_equal(" in line
            or (index > 0 and lines[index - 1].strip().endswith("ensure("))
        )
        if not asserts:
            continue

        code = ".code()" in window
        stdout = mentions_token(window, "stdout")
        stderr = mentions_token(window, "stderr")
        if code and stdout and stderr:
            continue
        found.append(
            (f"{stem}::{enclosing} [{missing_of(code, stdout, stderr)}]", index + 1, line[:100])
        )
    return found


def scan_tree(tests_dir: pathlib.Path) -> tuple[Counter, dict[str, list[tuple[str, int, str]]]]:
    sites: Counter[str] = Counter()
    where: dict[str, list[tuple[str, int, str]]] = {}
    try:
        files = sorted(p for p in tests_dir.iterdir() if p.suffix == ".rs")
    except OSError as error:
        raise CannotAnswer(f"cannot list {tests_dir}: {error}") from error
    if not files:
        raise CannotAnswer(
            f"no .rs files under {tests_dir}; a scan of nothing is not a clean tree"
        )
    for path in files:
        if path.stem == SELF_EXCLUSION:
            continue
        try:
            body = path.read_text(errors="replace")
        except OSError:
            continue
        for key, line, text in scan_file(path.stem, body):
            sites[key] += 1
            where.setdefault(key, []).append((path.name, line, text))
    return sites, where


def parse_baseline(text: str) -> Counter:
    recorded: Counter[str] = Counter()
    for line in text.splitlines():
        if not line.strip() or line.lstrip().startswith("#"):
            continue
        if "\t" not in line:
            continue
        key, _, count = line.partition("\t")
        try:
            recorded[key] = int(count.strip())
        except ValueError:
            continue
    return recorded


def audit(root: pathlib.Path) -> int:
    digest = check_pin(root)
    print()
    print(f"  detector pin ok: {digest[:16]}... ({GUARD})")

    try:
        baseline_text = (root / BASELINE).read_text(errors="replace")
    except OSError as error:
        raise CannotAnswer(f"cannot read {BASELINE}: {error}") from error
    recorded = parse_baseline(baseline_text)
    if not recorded:
        raise CannotAnswer(
            f"{BASELINE} parsed to ZERO rows. An empty answer key would make any "
            "tree look like growth, which is no verdict rather than a pass."
        )

    found, where = scan_tree(root / "tests")
    print(f"  baseline : {len(recorded):>4} rows / {sum(recorded.values()):>4} sites")
    print(f"  scanned  : {len(found):>4} rows / {sum(found.values()):>4} sites")

    grew = sorted(k for k in found if found[k] > recorded.get(k, 0))
    shrank = sorted(k for k in recorded if recorded[k] > found.get(k, 0))

    # The Rust arm's own refusal: a collapse reads as improvement to a ratchet.
    ftot, rtot = sum(found.values()), sum(recorded.values())
    if ftot == 0 and rtot > 0:
        raise CannotAnswer(
            f"the scan found ZERO sites against a baseline of {rtot}. That is a "
            "broken scanner, not a repaired tree."
        )
    if ftot * 2 < rtot:
        raise CannotAnswer(
            f"the scan found {ftot} sites against a baseline of {rtot} -- a collapse "
            "of more than half in one step. A ratchet reads that as improvement, "
            "which is how a broken scanner gets ratified. REFUSING TO GRADE."
        )

    print()
    if grew:
        print(f"  GREW -- {len(grew)} row(s). These are new assertions that cannot")
        print("  explain a failure. The bracket names WHAT IS MISSING from the")
        print("  failure text, so [code+stdout] prints stderr and neither the exit")
        print("  code nor stdout.")
        for key in grew:
            print(f"    {key}: {recorded.get(key, 0)} -> {found[key]}")
            for name, line, text in where.get(key, []):
                print(f"        tests/{name}:{line}  {text}")
        print()
        print("  FIX: print all three, and for the exit code print BOTH spellings")
        print("  beside each other, not one instead of the other -- `.code()` is")
        print("  None on a signal death while Display says \"signal: 9\":")
        print('      "... failed: {} (code {:?})\\nstdout={}\\nstderr={}",')
        print("          output.status, output.status.code(), stdout, stderr")
        print("  AND PUT ANY COMMENT ABOVE THE `if`, NOT INSIDE THE BLOCK: the")
        print(f"  window is capped at {ASSERTION_WINDOW} lines from the condition, so a comment")
        print("  between the condition and the format! pushes the tokens out of it.")
        print()
    if shrank:
        print(f"  SHRANK -- {len(shrank)} row(s) were REPAIRED but the baseline still")
        print("  lists them. Good news the ratchet cannot accept silently: a stale")
        print("  baseline stops describing the tree.")
        for key in shrank:
            print(f"    {key}: {recorded[key]} -> {found.get(key, 0)}")
        print()
        print("  Regenerate with the GENERATOR, not by hand:")
        print("    UPDATE_GOLDEN=1 cargo test --test verification_drift_guard \\")
        print("        open_coded_success_assertions_do_not_grow")
        print()

    if grew or shrank:
        print(f"  FAIL: {len(grew)} grown, {len(shrank)} shrunk. The Rust arm")
        print("  `open_coded_success_assertions_do_not_grow` asserts grown-is-empty")
        print("  FIRST, so while anything has grown the shrink list is never")
        print("  reached on the dispatch rail. Both lists are printed here.")
        return 1
    print("  OK: the tree matches the baseline exactly; nothing grew and nothing")
    print("  was repaired-but-still-listed.")
    return 0


def self_test() -> int:
    arms: list[tuple[str, bool]] = []

    def arm(name: str, ok: bool) -> None:
        arms.append((name, ok))

    # The window is bracket-bounded, not a fixed line count. The comment-inside-
    # the-block trap is a real failure from bd-mv9j7, frozen here as a probe:
    # a 14-line comment between the condition and the format! pushes `.code()`
    # out of the window, and the repair silently fails to register.
    tight = [
        "    if !output.status.success() {",
        "        return Err(format!(",
        '            "failed: {} (code {:?})\\nstdout={}\\nstderr={}",',
        "            output.status, output.status.code(), out, err",
        "        ));",
        "    }",
    ]
    win = assertion_window(tight, 0)
    arm("arm01 a compact site's window reaches its format! arguments", ".code()" in win)

    padded = tight[:2] + ["        // filler"] * 14 + tight[2:]
    win = assertion_window(padded, 0)
    arm(
        "arm02 a comment INSIDE the block pushes .code() out of the window",
        ".code()" not in win,
    )

    arm(
        "arm03 the window stops at the construct's closing brace, not 14 lines on",
        assertion_window(tight + ["    let x = y.code();"], 0).count("\n") == 5,
    )

    arm(
        "arm04 mentions_token is whole-word: a helper NAMED stdout is not a print",
        not mentions_token('ensure(stdout_json(&out), "x")', "stdout")
        and mentions_token('format!("stdout={}", s)', "stdout"),
    )

    # The classifier's own documented exclusions, both directions.
    def keys(src: str) -> list[str]:
        return [k for k, _, _ in scan_file("f", src)]

    arm(
        "arm05 `ensure(!x.status.success(), ..)` asserts FAILURE and is not a site",
        keys('    ensure(!out.status.success(), "should fail");') == [],
    )
    arm(
        "arm06 `if !x.status.success() { Err(..) }` asserts SUCCESS and IS a site",
        keys(
            "fn t() {\n"
            "    if !out.status.success() {\n"
            '        return Err("boom".into());\n'
            "    }\n"
            "}"
        )
        == ["f::t [code+stdout+stderr]"],
    )
    arm(
        "arm07 a comparison against a NON-success code is a different shape",
        keys('    ensure_equal(&out.status.code(), &Some(10), "rejected");') == [],
    )
    arm(
        "arm08 control flow with no Err( in its window is not an assertion",
        keys("fn t() {\n    if out.status.success() {\n        happy();\n    }\n}") == [],
    )
    arm(
        "arm09 a commented-out specimen is not a site",
        keys('    // ensure(out.status.success(), "quoted in prose");') == [],
    )
    arm(
        "arm10 a COMPLETE site is not recorded at all",
        keys(
            "fn t() {\n"
            "    if !out.status.success() {\n"
            '        return Err(format!("{} {:?} stdout={} stderr={}",\n'
            "            out.status, out.status.code(), a, b));\n"
            "    }\n"
            "}"
        )
        == [],
    )
    arm(
        "arm11 the missing-fact bracket is part of the key, so a partial repair shows",
        keys(
            "fn t() {\n"
            "    if !out.status.success() {\n"
            '        return Err(format!("stdout={}", a));\n'
            "    }\n"
            "}"
        )
        == ["f::t [code+stderr]"],
    )
    arm(
        "arm12 only a COLUMN-ZERO fn changes the enclosing name",
        enclosing_name("fn outer() {") == "outer"
        and enclosing_name("    fn inner() {") is None,
    )

    # The baseline parser must not read the header as rows, and must not accept
    # an empty answer key as agreement -- that is audit()'s CannotAnswer branch.
    parsed = parse_baseline("# a comment\n\nfoo::bar [code]\t3\nnot a row\n")
    arm(
        "arm13 the baseline parser reads rows and ignores header and junk",
        dict(parsed) == {"foo::bar [code]": 3},
    )
    arm("arm14 an all-comment baseline parses to ZERO rows", not parse_baseline("# only\n"))

    width = max(len(name) for name, _ in arms)
    for name, ok in arms:
        print(f"  {'ok  ' if ok else 'FAIL'} {name.ljust(width)}")
    passed = sum(1 for _, ok in arms if ok)
    print(f"\ncheck-open-coded-success-baseline self-test: {passed}/{len(arms)} arms passed")
    return 0 if passed == len(arms) else 2


def main() -> int:
    if "--self-test" in sys.argv[1:]:
        return self_test()
    if "--print-pin" in sys.argv[1:]:
        # Calibration aid: print the digest of the current detector span so the
        # pin can be set deliberately rather than pasted from a failure.
        try:
            root = repo_root()
            span = detector_span(root)
        except CannotAnswer as error:
            print(f"  CANNOT ANSWER: {error}")
            return 3
        print(hashlib.sha256(span.encode("utf-8")).hexdigest())
        return 0
    try:
        return audit(repo_root())
    except CannotAnswer as error:
        print(f"\n  CANNOT ANSWER: {error}")
        return 3


if __name__ == "__main__":
    sys.exit(main())
