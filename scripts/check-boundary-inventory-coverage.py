#!/usr/bin/env python3
"""Fast local precheck: does every CLI command path hold a row of its own?

THIS IS A PRECHECK, NOT THE AUTHORITY. The authoritative assertion is
`mechanical_boundary_inventory_covers_all_cli_command_paths` in
tests/mechanical_boundary_inventory.rs. This script replicates three of its
predicates in Python so the check is available where the authors actually work.

WHY IT EXISTS (bd-no-compile-gate-before-a-commit-lands-qdxc0).
Measured 2026-10-26: that assertion was RED at HEAD and had been for three
features. Six command paths -- `config explain`, `curate candidates --propose`,
`evidence withdraw`, `index backfill-anchors`, `memory anchors`,
`workspace rebind` -- held no cell anywhere in the inventory while the document
declared "Unmapped command count: 0". They arrived with 614581b78
feat(evidence), f5de143e0 feat(anchors) and 3b8c751d7 feat(workspace), and in
each case the author could not have learned: ./scripts/verify.sh exits 1 before
any stage on this host, hosted Actions is off repo-wide, and nobody dispatches a
thirty-minute pinned lane to check a markdown table.

The gate was correct and unrunnable, which is the same thing as absent. These
predicates are string matching over two tracked files and need no toolchain, so
there is no reason for the feedback to cost a remote build.

DRIFT IS THE REAL RISK of a second implementation, and it is handled two ways:
the self-test carries a MUTANT arm for each predicate (a rule that cannot fail
is not a rule), and this file names the Rust functions it mirrors so a reader
can diff them:

    command_paths_from_extract_function   tests/mechanical_boundary_inventory.rs
    first_cell_of                          (same file)
    the `Unmapped command count: 0` requirement

If this script and the Rust disagree, the RUST IS RIGHT and this file is the
bug. Nothing here may be used to argue a verdict against the test.
"""

import pathlib
import sys

REPO = pathlib.Path(__file__).resolve().parent.parent
CLI_PATH = REPO / "src/cli/mod.rs"
INVENTORY_PATH = REPO / "docs/mechanical-boundary-command-inventory.md"

# A live CLI cannot plausibly have fewer paths than this. The number is a FLOOR
# against the vacuous zero, not a census: if the start/end markers below ever
# stop matching -- a rename, a moved doc comment -- the extractor returns an
# empty list and every downstream check passes while verifying nothing. That is
# the failure this project keeps rediscovering, so it is asserted rather than
# assumed. At the time of writing the real count is 460.
MINIMUM_PLAUSIBLE_COMMAND_PATHS = 300

START_MARKER = "fn extract_command_path(cli: &Cli) -> String {"
END_MARKER = "\n    /// Returns a stable identifier"
UNMAPPED_DECLARATION = "Unmapped command count: 0"


def command_paths(source: str) -> list[str]:
    """Every lowercase string literal in `extract_command_path`, sorted unique.

    Mirrors the Rust byte walk, including its backslash skip, rather than using
    a regex: a regex over Rust string literals gets escapes wrong in a way that
    silently drops or merges paths.
    """
    if START_MARKER not in source:
        raise LookupError(f"{START_MARKER!r} not found in src/cli/mod.rs")
    body = source.split(START_MARKER, 1)[1]
    if END_MARKER not in body:
        raise LookupError(f"{END_MARKER!r} not found after the start marker")
    body = body.split(END_MARKER, 1)[0]

    found: list[str] = []
    index = 0
    while index < len(body):
        if body[index] != '"':
            index += 1
            continue
        index += 1
        start = index
        while index < len(body) and body[index] != '"':
            index += 2 if body[index] == "\\" else 1
        value = body[start:index]
        if value[:1].isascii() and value[:1].islower():
            found.append(value)
        index += 1
    return sorted(set(found))


def first_cells(inventory: str) -> list[str]:
    """The FIRST cell of every markdown table row in the document.

    Mirrors `first_cell_of`: strip leading whitespace, require a leading pipe,
    take the text before the next pipe. Naming a path in a later cell documents
    nothing about its own boundary (bd-6hp3w), so later cells are not collected.
    """
    cells = []
    for line in inventory.splitlines():
        stripped = line.lstrip()
        if stripped.startswith("|"):
            cells.append(stripped[1:].split("|")[0])
    return cells


def audit(
    cli_source: str,
    inventory: str,
    minimum: int = MINIMUM_PLAUSIBLE_COMMAND_PATHS,
) -> tuple[list[str], list[str]]:
    """(paths with no row of their own, other contract failures).

    `minimum` is a parameter so the self-test's synthetic fixtures -- which hold
    two command paths, not four hundred -- can exercise the row predicates
    without every arm also tripping the plausibility floor. The first version
    hard-coded the floor here and two arms failed for that reason alone, which
    is the self-test catching a defect in this file rather than in the tree.
    Live callers never pass it.
    """
    paths = command_paths(cli_source)
    problems: list[str] = []
    if len(paths) < minimum:
        problems.append(
            f"extracted only {len(paths)} command paths, below the plausibility "
            f"floor of {minimum}. The extractor is probably broken, NOT the "
            f"inventory -- check {START_MARKER!r}."
        )
    cells = first_cells(inventory)
    missing = [p for p in paths if not any(f"`{p}`" in cell for cell in cells)]
    if UNMAPPED_DECLARATION not in inventory:
        problems.append(
            f"the inventory must record {UNMAPPED_DECLARATION!r}; that line is absent"
        )
    return missing, problems


def self_test() -> int:
    """Every predicate gets a mutant that must fail it."""
    arms: list[tuple[str, bool]] = []

    def arm(name: str, passed: bool) -> None:
        arms.append((name, passed))

    cli = (
        'fn extract_command_path(cli: &Cli) -> String {\n'
        '    let _ = "alpha beta";\n'
        '    let _ = "gamma";\n'
        '    let _ = "Uppercase ignored";\n'
        '}\n    /// Returns a stable identifier\n'
    )
    paths = command_paths(cli)
    arm("extracts lowercase literals only", paths == ["alpha beta", "gamma"])

    good = (
        f"{UNMAPPED_DECLARATION}\n"
        "| `alpha beta` | anchor | shape | keep mechanical |\n"
        "| `gamma` | anchor | shape | keep mechanical |\n"
    )
    missing, problems = audit(cli, good, minimum=0)
    arm("clean document reports nothing", missing == [] and problems == [])

    # MUTANT 1: a path with no row at all.
    missing, _ = audit(
        cli, f"{UNMAPPED_DECLARATION}\n| `alpha beta` | a | s | k |\n", minimum=0
    )
    arm("MUTANT a missing path is caught", missing == ["gamma"])

    # MUTANT 2: the path named only in a LATER cell (bd-6hp3w discrimination).
    later = (
        f"{UNMAPPED_DECLARATION}\n"
        "| `alpha beta` | anchor | see also `gamma` | keep mechanical |\n"
    )
    missing, _ = audit(cli, later, minimum=0)
    arm("MUTANT a later-cell mention does not count", missing == ["gamma"])

    # MUTANT 3: the declaration line removed. Asserts the problem is THIS one
    # and not merely that something was reported, so the floor cannot stand in
    # for it -- which is precisely how this arm first passed-by-accident.
    _, problems = audit(
        cli,
        good.replace(UNMAPPED_DECLARATION, "Unmapped command count: 6"),
        minimum=0,
    )
    arm(
        "MUTANT a non-zero declaration is caught",
        len(problems) == 1 and UNMAPPED_DECLARATION in problems[0],
    )

    # MUTANT 4: a broken extractor must report ITSELF, not blame the inventory.
    # This arm keeps the real floor, since the floor is what it exercises.
    _, problems = audit(
        'fn extract_command_path(cli: &Cli) -> String {\n    let _ = "solo";\n}'
        "\n    /// Returns a stable identifier\n",
        good,
    )
    arm("MUTANT a tiny extraction trips the plausibility floor", bool(problems))

    # MUTANT 5: a renamed start marker must raise, never return an empty list.
    try:
        command_paths("fn something_else() {}")
        arm("MUTANT a renamed marker raises", False)
    except LookupError:
        arm("MUTANT a renamed marker raises", True)

    # The real tree must pass, which is what makes this runnable as a gate.
    missing, problems = audit(
        CLI_PATH.read_text(encoding="utf-8", errors="replace"),
        INVENTORY_PATH.read_text(encoding="utf-8", errors="replace"),
    )
    arm("the real tree is clean", missing == [] and problems == [])

    width = max(len(name) for name, _ in arms)
    for name, passed in arms:
        print(f"  {'ok  ' if passed else 'FAIL'}  {name:<{width}}")
    failed = [name for name, passed in arms if not passed]
    print()
    print(f"self-test: {len(arms) - len(failed)}/{len(arms)} arms passed")
    return 1 if failed else 0


def main(argv: list[str]) -> int:
    if "--self-test" in argv:
        return self_test()

    try:
        missing, problems = audit(
            CLI_PATH.read_text(encoding="utf-8", errors="replace"),
            INVENTORY_PATH.read_text(encoding="utf-8", errors="replace"),
        )
    except LookupError as error:
        print(f"boundary-inventory precheck CANNOT RUN: {error}")
        print("This is a fault in the precheck, not a verdict on the inventory.")
        return 2

    for problem in problems:
        print(f"boundary inventory: {problem}")
    if missing:
        print(
            f"boundary inventory: {len(missing)} command path(s) hold no row of "
            f"their own in docs/mechanical-boundary-command-inventory.md:"
        )
        for path in missing:
            print(f"  {path}")
        print()
        print(
            "Add each to the Full Command Inventory table with the path in the "
            "row's FIRST cell. A mention in a later cell of another command's "
            "row does not count (bd-6hp3w). Note that clearing this does NOT "
            "raise twelve-column enforcement (bd-o74n4)."
        )
    if missing or problems:
        return 1
    print("boundary inventory: every CLI command path holds a row of its own")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
