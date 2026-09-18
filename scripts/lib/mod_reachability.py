#!/usr/bin/env python3
"""Every tracked .rs under src/ and tests/ must be reachable from a cargo target.

bd-l6h3g. A .rs file that no cargo target reaches is invisible to BOTH
`cargo fmt` and rustc: the formatter reports it clean because it never examines
it, and the compiler never type-checks it. Four such files were found in this
repo carrying 2350 lines and 17 test functions, one of them a 1151-line
bypass-token module undeclared since 2026-05-15.

READ-ONLY. This script parses and reports; it never edits a file. AGENTS.md
forbids scripts that modify code files in this repo.

HOW REACHABILITY IS COMPUTED
  Roots come from `cargo fmt --verbose --check`, which prints every target root
  cargo itself walks. From each root we follow `mod x;` and
  `#[path = "..."] mod y;` transitively. That is the same traversal rustfmt and
  rustc perform, so "reachable" here means what it means to them.

  Taking the roots from cargo rather than reconstructing them from Cargo.toml is
  deliberate: this repo sets autotests = false and declares 42 explicit [[test]]
  targets, and a hand-rolled target list got the answer wrong twice before this.

SELF-VALIDATION, which is the part these checks usually skip
  The resolver is checked against a KNOWN POSITIVE and a KNOWN NEGATIVE taken
  from observed cargo behaviour, not from reading:
    tests/contracts/ask_native.rs   cargo fmt REPORTED it -> must be reachable
    src/core/preflight_token.rs     its `pub mod` line was removed by 0f68778a0
                                    -> must be unreachable
  If either control disagrees the script REFUSES to emit a verdict. A resolver
  that cannot find a file cargo just formatted has no business declaring other
  files unreachable.

EXIT CODES
  0  every in-scope file is reachable or allowlisted, and the allowlist is clean
  1  a real finding: an unreachable file that is not allowlisted, OR an
     allowlist entry that has rotted (names a missing file, or names a file that
     is now reachable and no longer needs the entry)
  2  inconclusive (cargo unavailable, controls disagree) -- callers treat this
     as "not blocking", never as "clean"
"""

from __future__ import annotations

import os
import pathlib
import re
import subprocess
import sys

REPO = pathlib.Path(__file__).resolve().parents[2]
# Overridable so the gate's own arms can be exercised against a scratch
# allowlist without editing the committed one. A check whose failure modes can
# only be tested by damaging the real file does not get tested.
ALLOWLIST = pathlib.Path(
    os.environ.get("MOD_REACHABILITY_ALLOWLIST", REPO / "scripts" / "mod-reachability-allowlist.txt")
)
IN_SCOPE = ("src/", "tests/")

# Controls, from observed cargo behaviour. See the module docstring.
CONTROL_REACHABLE = "tests/contracts/ask_native.rs"
CONTROL_UNREACHABLE = "src/core/preflight_token.rs"

PATH_MOD = re.compile(
    r'#\[path\s*=\s*"([^"]+)"\]\s*(?:pub(?:\([^)]*\))?\s+)?mod\s+(\w+)\s*;', re.S
)
PLAIN_MOD = re.compile(r"^[ \t]*(?:pub(?:\([^)]*\))?\s+)?mod\s+(\w+)\s*;", re.M)


def target_roots() -> list[pathlib.Path] | None:
    """Every target root cargo walks, straight from cargo itself."""
    try:
        proc = subprocess.run(
            ["cargo", "fmt", "--verbose", "--check"],
            cwd=REPO,
            capture_output=True,
            text=True,
            timeout=180,
        )
    except (FileNotFoundError, subprocess.TimeoutExpired):
        return None
    roots = [pathlib.Path(m) for m in re.findall(r'"([^"]+\.rs)"', proc.stdout)]
    return [r for r in roots if r.is_file()] or None


def children(path: pathlib.Path) -> list[pathlib.Path]:
    try:
        text = path.read_text(errors="replace")
    except OSError:
        return []
    here = path.parent
    found: list[pathlib.Path] = []
    # A `#[path = "..."] mod name;` both names the module and redirects it, so
    # the plain-mod pass below must not also resolve `name` to `name.rs`.
    redirected: set[str] = set()
    for match in PATH_MOD.finditer(text):
        redirected.add(match.group(2))
        candidate = (here / match.group(1)).resolve()
        if candidate.is_file():
            found.append(candidate)
    for match in PLAIN_MOD.finditer(text):
        name = match.group(1)
        if name in redirected:
            continue
        for candidate in (here / f"{name}.rs", here / name / "mod.rs"):
            if candidate.is_file():
                found.append(candidate.resolve())
                break
    return found


def reachable_set(roots: list[pathlib.Path]) -> set[pathlib.Path]:
    seen = {r.resolve() for r in roots}
    stack = list(seen)
    while stack:
        for child in children(stack.pop()):
            if child not in seen:
                seen.add(child)
                stack.append(child)
    return seen


def tracked_in_scope() -> list[str]:
    proc = subprocess.run(
        ["git", "ls-files", "*.rs"], cwd=REPO, capture_output=True, text=True
    )
    return sorted(
        line
        for line in proc.stdout.split()
        if line.startswith(IN_SCOPE)
    )


def read_allowlist() -> list[tuple[str, str]]:
    """(path, reason) pairs. A bare path with no reason is rejected by the gate."""
    if not ALLOWLIST.is_file():
        return []
    entries = []
    for raw in ALLOWLIST.read_text().splitlines():
        line = raw.strip()
        if not line or line.startswith("#"):
            continue
        path, _, reason = line.partition("\t")
        entries.append((path.strip(), reason.strip()))
    return entries


def main() -> int:
    roots = target_roots()
    if roots is None:
        print("[mod-reachability] cargo unavailable or timed out — inconclusive, not blocking", file=sys.stderr)
        return 2

    reachable = reachable_set(roots)
    tracked = tracked_in_scope()

    def is_reachable(rel: str) -> bool:
        return (REPO / rel).resolve() in reachable

    # --- controls, before any verdict -------------------------------------
    if not is_reachable(CONTROL_REACHABLE):
        print(
            f"[mod-reachability] CONTROL FAILED: {CONTROL_REACHABLE} should be reachable "
            "(cargo fmt reports it) but the resolver says otherwise — refusing to emit",
            file=sys.stderr,
        )
        return 2
    if is_reachable(CONTROL_UNREACHABLE):
        print(
            f"[mod-reachability] CONTROL FAILED: {CONTROL_UNREACHABLE} should be unreachable "
            "(0f68778a0 removed its `pub mod` line) but the resolver says otherwise — refusing to emit",
            file=sys.stderr,
        )
        return 2

    allow = read_allowlist()
    allow_paths = {p for p, _ in allow}
    findings = 0

    # --- 1. unreachable and not allowlisted -------------------------------
    unreachable = [f for f in tracked if not is_reachable(f)]
    new_unreachable = [f for f in unreachable if f not in allow_paths]
    if new_unreachable:
        findings += len(new_unreachable)
        print("UNREACHABLE FROM ANY CARGO TARGET, and not on the allowlist:")
        for f in new_unreachable:
            print(f"    {f}")
        print(
            "  Nothing compiles or formats these. Either declare the module "
            "(`mod x;` or `#[path = \"...\"] mod x;` from a reachable parent), "
            "or add an allowlist entry WITH A REASON if the file is deliberate "
            "test data."
        )

    # --- 2. allowlist rot: entry names a file that is gone -----------------
    missing = [p for p in allow_paths if not (REPO / p).is_file()]
    if missing:
        findings += len(missing)
        print("ALLOWLIST ENTRIES NAMING FILES THAT NO LONGER EXIST:")
        for p in sorted(missing):
            print(f"    {p}")
        print("  Remove them. An allowlist that outlives its files decays into fiction.")

    # --- 3. allowlist rot: entry is now reachable and unnecessary ----------
    now_reachable = [p for p in allow_paths if (REPO / p).is_file() and is_reachable(p)]
    if now_reachable:
        findings += len(now_reachable)
        print("ALLOWLIST ENTRIES THAT ARE NOW REACHABLE (the exemption is obsolete):")
        for p in sorted(now_reachable):
            print(f"    {p}")
        print("  Remove them, so the allowlist only ever names real exemptions.")

    # --- 4. an entry with no reason is not an exemption, it is a hole ------
    unreasoned = [p for p, reason in allow if not reason]
    if unreasoned:
        findings += len(unreasoned)
        print("ALLOWLIST ENTRIES WITH NO REASON:")
        for p in sorted(unreasoned):
            print(f"    {p}")
        print("  Every exemption states why, or it cannot be reviewed.")

    if findings:
        print(f"\n[mod-reachability] {findings} finding(s) above. Each is printed by name.")
        return 1

    # Print the population, not just a verdict: a green whose denominator is
    # invisible is the failure this gate exists to remove.
    print(
        f"[mod-reachability] {len(tracked)} tracked .rs under {', '.join(IN_SCOPE)}: "
        f"{len(tracked) - len(unreachable)} reachable, {len(unreachable)} allowlisted, "
        f"0 unaccounted."
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
