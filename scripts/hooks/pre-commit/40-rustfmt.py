#!/usr/bin/env python3
"""Refuse a commit whose STAGED Rust files are not rustfmt-clean (bd-gq26a).

Not installed by being present. This file is the reviewed source; installing it
is a separate, human decision -- see "INSTALLING" below and the bead.

WHY THIS EXISTS
    Nothing checks Rust formatting between an agent writing code and hosted CI
    going red. On 2026-10-05 four files sat unformatted on main for ~15 hours
    across three peer commits; `cargo fmt --check` is run by 27 of 85 workflows,
    so one missed violation reddens many independent lanes at once.

THREE THINGS THIS GUARD GETS RIGHT, EACH OF WHICH IS EASY TO GET WRONG
    1. --edition 2024. A bare `rustfmt --check` defaults to edition 2021 and
       answers a different question than CI's `cargo fmt --check`; the two
       disagree about WHICH hunks are wrong, so formatting against 2021 yields a
       file CI still rejects.
    2. --config skip_children=true. Without it rustfmt follows `mod` into child
       files and would report on files the author never staged -- including
       peers' unstaged work. That would make the guard block commits for other
       people's drift, which is exactly how a hook gets disabled within the hour.
    3. STAGED CONTENT, via `git show :path`, not the working tree. What is
       committed is the index, and on this checkout the working tree moves under
       you constantly.

    And one trap worth recording, because it silently defeats the obvious
    implementation: `rustfmt --check` reading from STDIN prints a diff but
    EXITS 0. A guard piping blobs into rustfmt and testing `$?` passes
    everything, always. Measured 2026-10-05: stdin unformatted -> exit 0;
    file-argument unformatted -> exit 1. This guard therefore writes staged
    blobs to temporary FILES and passes them as arguments. The self-test below
    pins that behaviour so it cannot regress silently.

FAIL-OPEN BY CONSTRUCTION
    This runs on a shared six-agent checkout, where a guard that wedges commits
    is worse than the drift it prevents. Every unexpected condition -- missing
    rustfmt, not a git repo, a subprocess blowing up, a timeout, an unreadable
    blob -- warns and exits 0. The ONLY exit-1 path is "rustfmt ran to
    completion and reported a staged file is unformatted".

WHAT IT DELIBERATELY DOES NOT DO
    It never rewrites a file. Auto-formatting under an author mid-commit makes
    the index and working tree diverge silently. It refuses and prints the exact
    command to fix.

INSTALLING (per-clone; .git/ is not version controlled)
    cp scripts/hooks/pre-commit/40-rustfmt.py \
       .git/hooks/hooks.d/pre-commit/40-rustfmt.py
    chmod +x .git/hooks/hooks.d/pre-commit/40-rustfmt.py
    Numbered below the existing 50-agent-mail.py so a formatting refusal costs
    nothing and happens first.

SELF-TEST
    python3 scripts/hooks/pre-commit/40-rustfmt.py --self-test
"""

from __future__ import annotations

import os
import subprocess
import sys
import tempfile
import time
from pathlib import Path

EDITION = "2024"
SKIP_CHILDREN = "skip_children=true"
RUSTFMT_TIMEOUT_SEC = 60
GIT_TIMEOUT_SEC = 20


def _warn(message: str) -> None:
    print(f"[40-rustfmt] {message}", file=sys.stderr)


def _run(argv: list[str], timeout: int, stdin_data: bytes | None = None):
    return subprocess.run(
        argv,
        capture_output=True,
        timeout=timeout,
        input=stdin_data,
        check=False,
    )


def staged_rust_paths() -> list[str] | None:
    """Staged, still-present Rust files. None means 'could not tell' -> open."""
    try:
        done = _run(
            [
                "git",
                "diff",
                "--cached",
                "--name-only",
                "--diff-filter=ACMR",
                "-z",
                "--",
                "*.rs",
            ],
            GIT_TIMEOUT_SEC,
        )
    except Exception as error:  # noqa: BLE001 - fail open on anything
        _warn(f"could not list staged files ({error}); skipping")
        return None
    if done.returncode != 0:
        _warn("git diff --cached failed; skipping")
        return None
    # -z keeps paths with spaces, quotes or non-UTF8 bytes intact.
    raw = done.stdout.split(b"\0")
    out: list[str] = []
    for item in raw:
        if not item:
            continue
        out.append(os.fsdecode(item))
    return out


def rustfmt_available() -> bool:
    try:
        done = _run(["rustfmt", "--version"], GIT_TIMEOUT_SEC)
    except Exception:  # noqa: BLE001
        return False
    return done.returncode == 0


def check_blob(path: str, workdir: Path) -> bool | None:
    """True = clean, False = unformatted, None = undetermined (fail open)."""
    try:
        blob = _run(["git", "show", f":{path}"], GIT_TIMEOUT_SEC)
    except Exception as error:  # noqa: BLE001
        _warn(f"could not read staged {path} ({error}); skipping it")
        return None
    if blob.returncode != 0:
        _warn(f"could not read staged {path}; skipping it")
        return None

    # A FILE argument, never stdin: see the stdin exit-0 trap in the docstring.
    scratch = workdir / (str(abs(hash(path))) + ".rs")
    try:
        scratch.write_bytes(blob.stdout)
        done = _run(
            [
                "rustfmt",
                "--edition",
                EDITION,
                "--check",
                "--config",
                SKIP_CHILDREN,
                str(scratch),
            ],
            RUSTFMT_TIMEOUT_SEC,
        )
    except Exception as error:  # noqa: BLE001
        _warn(f"rustfmt could not run on {path} ({error}); skipping it")
        return None

    if done.returncode == 0:
        return True
    # rustfmt exits non-zero for a parse error too. A file that does not parse
    # is the author's problem at compile time, not a formatting refusal, and
    # blocking on it would be a false positive.
    stderr = done.stderr.decode("utf-8", "replace")
    if "error[" in stderr or "error:" in stderr:
        _warn(f"{path} did not parse; leaving it to the compiler")
        return None
    return False


def extract_staged(paths: list[str], workdir: Path) -> list[Path] | None:
    """Write every staged blob to a temp file. None = undetermined (fail open)."""
    out: list[Path] = []
    for path in paths:
        try:
            blob = _run(["git", "show", f":{path}"], GIT_TIMEOUT_SEC)
            if blob.returncode != 0:
                _warn(f"could not read staged {path}; skipping it")
                continue
            scratch = workdir / f"batch_{abs(hash(path))}.rs"
            scratch.write_bytes(blob.stdout)
            out.append(scratch)
        except Exception as error:  # noqa: BLE001 - fail open
            _warn(f"could not stage-read {path} ({error}); skipping it")
            continue
    return out


def batch_is_clean(files: list[Path]) -> bool | None:
    """True = every file clean, False = at least one is not, None = unknown."""
    try:
        done = _run(
            [
                "rustfmt",
                "--edition",
                EDITION,
                "--check",
                "--config",
                SKIP_CHILDREN,
                *[str(f) for f in files],
            ],
            RUSTFMT_TIMEOUT_SEC,
        )
    except Exception as error:  # noqa: BLE001 - fail open
        _warn(f"rustfmt batch could not run ({error}); skipping the check")
        return None
    return done.returncode == 0


def main() -> int:
    started = time.monotonic()
    paths = staged_rust_paths()
    if paths is None:
        return 0
    if not paths:
        return 0
    if not rustfmt_available():
        _warn("rustfmt not found on PATH; skipping the formatting check")
        return 0

    offenders: list[str] = []
    with tempfile.TemporaryDirectory(prefix="rustfmt-staged-") as tmp:
        workdir = Path(tmp)
        # FAST PATH: one rustfmt invocation over every staged blob. Process
        # spawn dominates the cost (~75ms/file when run per-file: 0.90s for 12
        # real source files, measured 2026-10-05), and the overwhelmingly common
        # answer is "all clean", which needs no attribution. Only when something
        # IS unformatted -- rare -- do we pay the per-file pass to name it.
        staged = extract_staged(paths, workdir)
        if staged is None:
            return 0
        if staged and batch_is_clean(staged) is False:
            for path in paths:
                if check_blob(path, workdir) is False:
                    offenders.append(path)

    elapsed = time.monotonic() - started
    if not offenders:
        print(
            f"[40-rustfmt] {len(paths)} staged Rust file(s) formatted "
            f"({elapsed:.2f}s)"
        )
        return 0

    print("", file=sys.stderr)
    print(
        "[40-rustfmt] COMMIT REFUSED: staged Rust file(s) are not "
        f"rustfmt-clean ({elapsed:.2f}s)",
        file=sys.stderr,
    )
    for path in offenders:
        print(f"    {path}", file=sys.stderr)
    print("", file=sys.stderr)
    print("  Fix, then re-stage:", file=sys.stderr)
    print(f"      rustfmt --edition {EDITION} " + " ".join(offenders), file=sys.stderr)
    print("      git add " + " ".join(offenders), file=sys.stderr)
    print("", file=sys.stderr)
    print(
        "  Checked the STAGED content, not the working tree, and only files "
        "you staged.",
        file=sys.stderr,
    )
    print(
        "  This guard never rewrites your files. To bypass once: "
        "git commit --no-verify",
        file=sys.stderr,
    )
    return 1


# ---------------------------------------------------------------------------
# Self-test. Every arm asserts a POLARITY, not just an absence of crashes: the
# point is to prove the guard can say no, not merely that it can say nothing.
# ---------------------------------------------------------------------------

BAD = b'fn main(){let x=1;println!("{}",x);}\n'
GOOD = b'fn main() {\n    println!("hi");\n}\n'


def self_test() -> int:
    arms: list[tuple[str, bool]] = []

    def arm(label: str, ok: bool) -> None:
        arms.append((label, ok))
        print(f"  [{'ok  ' if ok else 'FAIL'}] {label}")

    if not rustfmt_available():
        print("[40-rustfmt] self-test cannot run: rustfmt is not on PATH")
        return 1

    with tempfile.TemporaryDirectory(prefix="rustfmt-selftest-") as tmp:
        workdir = Path(tmp)
        bad = workdir / "bad.rs"
        good = workdir / "good.rs"
        bad.write_bytes(BAD)
        good.write_bytes(GOOD)

        def fmt_file(target: Path):
            return _run(
                [
                    "rustfmt",
                    "--edition",
                    EDITION,
                    "--check",
                    "--config",
                    SKIP_CHILDREN,
                    str(target),
                ],
                RUSTFMT_TIMEOUT_SEC,
            )

        # POSITIVE CONTROL: the guard's core predicate must reject bad input.
        arm("an unformatted file argument exits non-zero", fmt_file(bad).returncode != 0)
        # NEGATIVE CONTROL: and must accept good input, or it rejects everything.
        arm("a formatted file argument exits zero", fmt_file(good).returncode == 0)

        # --check must never rewrite. If it did, the guard would silently mutate
        # the author's staged content.
        arm("--check leaves the file byte-identical", bad.read_bytes() == BAD)

        # THE TRAP THIS GUARD EXISTS TO AVOID. If a future rustfmt starts
        # exiting non-zero on stdin, this arm fails loudly and the docstring's
        # rationale can be revisited -- it is pinned deliberately, not assumed.
        piped = _run(
            ["rustfmt", "--edition", EDITION, "--check", "--config", SKIP_CHILDREN],
            RUSTFMT_TIMEOUT_SEC,
            stdin_data=BAD,
        )
        arm(
            "stdin --check still exits 0 on unformatted input "
            "(why this guard uses file arguments)",
            piped.returncode == 0 and bool(piped.stdout),
        )

        # THE BATCH FAST PATH. It decides the common case, so it needs both
        # polarities: a batch containing one bad file must not be reported
        # clean, and an all-good batch must not be reported dirty.
        arm(
            "a batch containing one unformatted file is NOT clean",
            batch_is_clean([good, bad, good]) is False,
        )
        arm(
            "an all-formatted batch IS clean",
            batch_is_clean([good, good]) is True,
        )

        # Edition matters: pin that we are asking the question CI asks.
        e2024 = _run(
            ["rustfmt", "--edition", "2024", "--check", "--config", SKIP_CHILDREN, str(bad)],
            RUSTFMT_TIMEOUT_SEC,
        )
        arm("edition 2024 is accepted by this rustfmt", e2024.returncode == 1)

        # A file that does not parse must NOT be reported as a formatting
        # refusal -- otherwise the guard blocks on broken-but-honest WIP.
        broken = workdir / "broken.rs"
        broken.write_bytes(b"fn main( {\n")
        done = fmt_file(broken)
        stderr = done.stderr.decode("utf-8", "replace")
        arm(
            "a non-parsing file is distinguishable from an unformatted one",
            done.returncode != 0 and ("error" in stderr.lower()),
        )

    passed = sum(1 for _, ok in arms if ok)
    print(f"\n[40-rustfmt] self-test: {passed}/{len(arms)} arms passed")
    return 0 if passed == len(arms) else 1


if __name__ == "__main__":
    if "--self-test" in sys.argv:
        raise SystemExit(self_test())
    try:
        raise SystemExit(main())
    except SystemExit:
        raise
    except BaseException as error:  # noqa: BLE001 - never wedge a commit
        _warn(f"unexpected error ({error}); allowing the commit")
        raise SystemExit(0) from None
