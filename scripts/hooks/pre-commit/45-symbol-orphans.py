#!/usr/bin/env python3
"""Pre-commit: report a staged deletion of a `fn` whose callers survive.

A thin front end for `scripts/check-deleted-symbol-orphans.py --staged`. It
holds no predicate of its own on purpose -- two copies of a rule drift apart,
and the script is the one carrying 21 self-test arms and real-history controls.

WHY IT EXISTS. bd-1ks7e left main uncompilable for 24 hours because
`f9ecec213` deleted a function and left a caller in a `#[path]` sibling, which
no `mod` grep reaches. The `scripts/check-*.py` family loop catches that on its
next run; this catches it in the terminal of the agent creating it, at the
moment they create it, which is the only moment the repair is cheap.

INSTALL (the convention this directory follows, same as 40-rustfmt.py):

    cp scripts/hooks/pre-commit/45-symbol-orphans.py \
       .git/hooks/hooks.d/pre-commit/45-symbol-orphans.py
    chmod +x .git/hooks/hooks.d/pre-commit/45-symbol-orphans.py
    python3 scripts/hooks/pre-commit/45-symbol-orphans.py --self-test

It must sort after `40-rustfmt.py`, which rewrites and re-stages formatted
files, so this reads the index as it will actually be committed.

### IT DOES NOT BLOCK THE COMMIT, AND THAT IS A DELIBERATE CHOICE ###

`decide()` returns 0 for every input, and `never_blocks_for_any_exit_code`
pins that. The reason is not timidity about gates, it is a measured property of
this particular predicate: its false-positive set was wrong TWICE within an
hour of being written, each time found by widening the measurement rather than
by reasoning about it.

    120 commits, bare-word predicate    2 fires, 1 of them a `///` doc comment
                                        naming the deleted symbol
    1200 commits, prose stripped        1 more, `let near_duplicate = ...`,
                                        a name that used to be a function and
                                        is now a local binding

Both are fixed and both have arms. But a predicate with that history probably
has a third unmeasured class, and `.git/hooks` is SHARED by every agent working
in this checkout. A blocking gate whose false-positive rate is still being
discovered does not block one commit, it blocks a swarm -- and AGENTS.md is
explicit that another agent's work is never to be disturbed. The asymmetry is
the argument: a missed orphan costs one build and the family loop still catches
it, while a false block costs every agent until somebody diagnoses a hook.

TO MAKE IT BLOCK, once the rate has been observed over real use rather than
over history, return `rc` from `decide()` for the finding case and say on
bd-1ks7e what the observation was. Do not make that change on the strength of
the numbers above; they are retrospective.
"""

import subprocess
import sys
from pathlib import Path

CHECK = "scripts/check-deleted-symbol-orphans.py"
TAG = "[45-symbol-orphans]"


def decide(rc: int, stdout: str, stderr: str) -> tuple[int, list[str]]:
    """Map the gate's result to this plugin's output and exit code.

    Pure, so the self-test can cover every branch without staging anything in
    a real repository. ALWAYS returns 0 -- see the module docstring.
    """
    if rc == 0:
        return 0, []
    if rc == 3:
        lines = [f"{TAG} could not answer; nothing was checked."]
        lines += [f"{TAG}   {line}" for line in stderr.strip().splitlines() if line]
        return 0, lines
    if rc == 1:
        lines = [f"{TAG} *** A STAGED DELETION LEAVES A CALLER BEHIND ***"]
        lines += [f"{TAG} {line}" for line in stdout.strip().splitlines()]
        lines.append(f"{TAG} This is rustc E0425 and nothing else here catches it")
        lines.append(f"{TAG} until someone pays for a remote build. NOT blocked.")
        return 0, lines
    # An unexpected code means this plugin called the gate wrongly, which is a
    # plugin bug and still must not stop a commit. Say so instead of silently
    # treating it as clean -- a front end that swallows its own misuse reports
    # green for a gate it never ran.
    lines = [f"{TAG} gate exited {rc} unexpectedly; treating as NO VERDICT."]
    lines += [f"{TAG}   {line}" for line in (stderr or stdout).strip().splitlines()[:5]]
    return 0, lines


def main() -> int:
    try:
        root = subprocess.run(
            ["git", "rev-parse", "--show-toplevel"],
            capture_output=True,
            text=True,
            check=False,
        ).stdout.strip()
        if not root:
            return 0
        script = Path(root) / CHECK
        if not script.exists():
            # The gate is tracked, so this is a checkout without it. Silence is
            # correct: a hook complaining about its own absence is noise.
            return 0
        proc = subprocess.run(
            [sys.executable, str(script), "--staged"],
            capture_output=True,
            text=True,
            cwd=root,
            check=False,
        )
    except Exception as exc:  # never let this plugin break a commit
        print(f"{TAG} skipped: {exc}")
        return 0

    code, lines = decide(proc.returncode, proc.stdout, proc.stderr)
    for line in lines:
        print(line)
    return code


def self_test() -> int:
    arms: list[tuple[str, bool]] = []

    def arm(name: str, ok: bool) -> None:
        arms.append((name, ok))

    code, lines = decide(0, "", "")
    arm("clean_is_silent", code == 0 and lines == [])

    code, lines = decide(1, "ORPHANED: 1 removed definition(s) still called.", "")
    arm(
        "finding_is_reported",
        code == 0
        and any("LEAVES A CALLER BEHIND" in line for line in lines)
        and any("ORPHANED" in line for line in lines),
    )

    code, lines = decide(3, "", "CANNOT ANSWER: git cannot resolve 'x'")
    arm(
        "cannot_answer_is_distinguished",
        code == 0
        and any("could not answer" in line for line in lines)
        and not any("LEAVES A CALLER" in line for line in lines),
    )

    code, lines = decide(2, "", "usage: ...")
    arm(
        "unexpected_code_is_not_silent",
        code == 0 and any("NO VERDICT" in line for line in lines),
    )

    # THE CONTRACT ARM. Report-only is the whole design, and a future edit that
    # makes this blocking must fail a test rather than surprise a swarm.
    arm(
        "never_blocks_for_any_exit_code",
        all(decide(rc, "out", "err")[0] == 0 for rc in range(-8, 130)),
    )

    # Every tagged line must carry the tag, so hook output stays attributable
    # when several plugins print in one commit.
    tagged = all(
        line.startswith(TAG)
        for rc in (1, 2, 3)
        for line in decide(rc, "some out", "some err")[1]
    )
    arm("every_line_is_tagged", tagged)

    width = max(len(name) for name, _ in arms)
    for name, ok in arms:
        print(f"  [{'PASS' if ok else 'FAIL'}] {name.ljust(width)}")
    passed = sum(1 for _, ok in arms if ok)
    print(f"\nself-test: {passed}/{len(arms)} passed")
    return 0 if passed == len(arms) else 1


if __name__ == "__main__":
    if "--self-test" in sys.argv:
        sys.exit(self_test())
    sys.exit(main())
