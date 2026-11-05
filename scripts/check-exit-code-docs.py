#!/usr/bin/env python3
"""bd-undocumented-exit-codes-9-and-130-zop31 — every returnable exit code must
appear in every documented exit-code table.

WHY THIS EXISTS. MEASURED 2026-11-05 at 59aa07a13: `ProcessExitCode` in
src/models/mod.rs declares twelve variants, and the three prose tables that
publish the contract listed ten of them. Every table jumped 8 -> 10, so
`EvalFailure = 9` and `Cancelled = 130` were returnable and documented nowhere.
README tells machine readers to branch on exit status and AGENTS.md tells agents
to treat 8 as the signal to run `ee migrate run`, so a harness switching
exhaustively over the published table mishandles both — and 130 is the EXPECTED
status of a cooperative cancellation, which the product documents as a feature.

THE MACHINE SURFACE WAS ALREADY RIGHT, WHICH IS WHY THIS GATE READS IT. The
`EXIT_CODES` registry in src/core/agent_docs.rs carries all twelve with
descriptions, and `exit_codes_match_process_exit_code_contract` already pins
that registry to the enum. Nothing compared the registry to the MARKDOWN, so
the drift could only exist in prose. This gate closes that one edge, and it
deliberately does not re-derive the enum: duplicating that comparison here
would give two answers to one question.

WHY IT IS PYTHON AND NOT A #[test]. The equivalent Rust assertion is reachable
only through a Cargo dispatch, and on this host every Cargo subcommand but
`fmt` is tripwired while `scripts/verify.sh` refuses before stage one for want
of a source-matched native binary. A gate that can only run on the rail that is
down is how the sibling defect in docs/degraded_codes.md stayed invisible. This
runs in well under a second with no toolchain, no binary and no lane.

HOW A TABLE IS IDENTIFIED, because this is the part a token match gets wrong.
A documented exit-code table is a contiguous run of markdown table rows
containing a row whose code cell is `0` and whose description cell is
`success`. That is not decoration: README also contains a verify.sh STAGE table
whose rows include `| 8 | Boundary migration |`, so a gate keyed on "a table
with a row for 8" would read the stage list as an exit-code contract and then
report drift in a table that never promised anything about exit codes. The
stage table has no `0`/`success` row and carries decimal stage numbers, so the
predicate excludes it. --self-test arm 7 is that negative control.

A MISSING TABLE IS NOT A PASS. If a documented file contains no exit-code table
at all, that is exit 3 (cannot answer), never exit 0. A guard that silently
succeeds when its subject disappears converts a deleted contract into a green
run.

EXIT CODES
  0  every registry code appears in every documented exit-code table
  1  a table omits a registry code, or publishes a code the registry does not
  2  --self-test failed
  3  environment error (registry or a documented table unreadable/absent) --
     never reported as clean
"""

from __future__ import annotations

import pathlib
import re
import sys

REPO_ROOT = pathlib.Path(__file__).resolve().parent.parent
REGISTRY = REPO_ROOT / "src" / "core" / "agent_docs.rs"

# Files that publish the exit-code contract in prose. Each must contain at
# least one exit-code table; AGENTS.md deliberately contains two (the agent-UX
# contract and the CLI Output Rules section), and both are checked.
DOCUMENTED = ("README.md", "AGENTS.md")

REGISTRY_BLOCK = re.compile(
    r"pub const EXIT_CODES:\s*&\[ExitCodeEntry\]\s*=\s*&\[(.*?)\n\];",
    re.S,
)
REGISTRY_ENTRY = re.compile(
    r"ExitCodeEntry\s*\{\s*code:\s*(\d+)\s*,\s*name:\s*\"([^\"]+)\"",
    re.S,
)

# A markdown table row: | <cell> | <cell> ... |. The code cell may be
# backticked (README) or bare (AGENTS.md).
ROW = re.compile(r"^\|(?P<cells>.*)\|\s*$")
CODE_CELL = re.compile(r"^`?(\d+)`?$")

PREDICATE = (
    "predicate: a documented exit-code table is a contiguous run of markdown "
    "table rows containing a row whose code cell is 0 and whose description "
    "cell is 'success'; every ExitCodeEntry in src/core/agent_docs.rs must "
    "appear as a row in every such table"
)


def say(msg: str) -> None:
    print(f"[exit-code-docs] {msg}")


def parse_registry(text: str) -> dict[int, str]:
    block = REGISTRY_BLOCK.search(text)
    if block is None:
        return {}
    return {
        int(code): name for code, name in REGISTRY_ENTRY.findall(block.group(1))
    }


def _cells(line: str) -> list[str] | None:
    match = ROW.match(line.rstrip())
    if match is None:
        return None
    return [cell.strip() for cell in match.group("cells").split("|")]


def find_exit_code_tables(text: str) -> list[dict[int, str]]:
    """Return one code->description map per exit-code table in `text`."""
    tables: list[dict[int, str]] = []
    block: list[list[str]] = []

    def flush() -> None:
        if not block:
            return
        codes: dict[int, str] = {}
        anchored = False
        for cells in block:
            if len(cells) < 2:
                continue
            code_match = CODE_CELL.match(cells[0])
            if code_match is None:
                continue
            code = int(code_match.group(1))
            description = cells[1]
            codes[code] = description
            if code == 0 and description.strip().lower().startswith("success"):
                anchored = True
        if anchored:
            tables.append(codes)

    for line in text.splitlines():
        cells = _cells(line)
        if cells is None:
            flush()
            block = []
            continue
        block.append(cells)
    flush()
    return tables


def run_lint() -> int:
    say(PREDICATE)

    if not REGISTRY.is_file():
        say(f"CANNOT ANSWER: registry not found at {REGISTRY}")
        return 3
    registry = parse_registry(REGISTRY.read_text(encoding="utf-8"))
    if not registry:
        say(
            "CANNOT ANSWER: could not parse EXIT_CODES from "
            f"{REGISTRY.relative_to(REPO_ROOT)}; the declaration shape changed"
        )
        return 3

    say(
        f"registry: {len(registry)} code(s) "
        f"({', '.join(str(c) for c in sorted(registry))})"
    )

    failures: list[str] = []
    table_count = 0

    for name in DOCUMENTED:
        path = REPO_ROOT / name
        if not path.is_file():
            say(f"CANNOT ANSWER: documented file missing: {name}")
            return 3
        tables = find_exit_code_tables(path.read_text(encoding="utf-8"))
        if not tables:
            say(
                f"CANNOT ANSWER: {name} contains no exit-code table; a missing "
                "contract is not a pass"
            )
            return 3
        for index, table in enumerate(tables, start=1):
            table_count += 1
            missing = sorted(set(registry) - set(table))
            extra = sorted(set(table) - set(registry))
            say(
                f"{name} table {index}: {len(table)} row(s), "
                f"{len(missing)} missing, {len(extra)} unknown"
            )
            for code in missing:
                failures.append(
                    f"{name} table {index} omits exit code {code} "
                    f"({registry[code]}), which ProcessExitCode can return"
                )
            for code in extra:
                failures.append(
                    f"{name} table {index} publishes exit code {code} "
                    f"({table[code]!r}), which the registry does not declare"
                )

    say(f"tables examined: {table_count}")

    if failures:
        for failure in failures:
            say(f"FAIL: {failure}")
        say(
            "repair: add the missing row(s), copying the description from the "
            "EXIT_CODES registry rather than paraphrasing, so the prose and the "
            "machine surface say the same thing"
        )
        return 1

    say("OK -- every registry code is documented in every exit-code table")
    return 0


# --------------------------------------------------------------------------
# self-test
# --------------------------------------------------------------------------

_REGISTRY_FIXTURE = """
pub const EXIT_CODES: &[ExitCodeEntry] = &[
    ExitCodeEntry {
        code: 0,
        name: "success",
        description: "Command completed successfully",
    },
    ExitCodeEntry {
        code: 9,
        name: "eval_failure",
        description: "Evaluation completed and found regressions",
    },
    ExitCodeEntry {
        code: 130,
        name: "cancelled",
        description: "Operation was cancelled",
    },
];
"""

_COMPLETE_TABLE = """
| Code | Meaning |
| ---- | ------- |
| 0 | success |
| 9 | evaluation completed and found regressions |
| 130 | operation was cancelled |
"""

_MISSING_TABLE = """
| Code | Meaning |
| ---- | ------- |
| 0 | success |
| 9 | evaluation completed and found regressions |
"""

_BACKTICKED_TABLE = """
| `0` | success |
| `9` | evaluation completed and found regressions |
| `130` | operation was cancelled |
"""

# The verify.sh stage table: decimal stage numbers, a row for 8, and NO
# 0/success row. This must NOT be read as an exit-code table.
_STAGE_TABLE = """
| Stage | Gate |
|---:|---|
| 5.5 | Proof verification |
| 8 | Boundary migration |
| 9 | Benchmarks when `--include-bench` is passed |
"""


def self_test() -> int:
    failures = 0

    def check(label: str, got: object, want: object) -> None:
        nonlocal failures
        if got == want:
            print(f"ok   {label}")
        else:
            print(f"FAIL {label}: got {got!r}, want {want!r}")
            failures += 1

    registry = parse_registry(_REGISTRY_FIXTURE)
    check("arm1 registry parses to its codes", sorted(registry), [0, 9, 130])

    check(
        "arm2 a complete table yields no missing codes",
        sorted(set(registry) - set(find_exit_code_tables(_COMPLETE_TABLE)[0])),
        [],
    )

    check(
        "arm3 a table omitting 130 reports it missing (the RED direction)",
        sorted(set(registry) - set(find_exit_code_tables(_MISSING_TABLE)[0])),
        [130],
    )

    check(
        "arm4 backticked code cells parse (README style)",
        sorted(find_exit_code_tables(_BACKTICKED_TABLE)[0]),
        [0, 9, 130],
    )

    check(
        "arm5 an unparseable registry yields no codes, not a silent pass",
        parse_registry("pub const SOMETHING_ELSE: &[u8] = &[];"),
        {},
    )

    check(
        "arm6 two tables in one file are both found",
        len(find_exit_code_tables(_COMPLETE_TABLE + "\ntext\n" + _BACKTICKED_TABLE)),
        2,
    )

    # The negative control. Over-matching here would make the gate report drift
    # in a table that never promised anything about exit codes.
    check(
        "arm7 NEGATIVE CONTROL: the verify.sh stage table is not an exit-code table",
        find_exit_code_tables(_STAGE_TABLE),
        [],
    )

    check(
        "arm8 a table with an unknown code reports it as unknown",
        sorted(
            set(find_exit_code_tables(_COMPLETE_TABLE + "| 77 | invented |\n")[0])
            - set(registry)
        ),
        [77],
    )

    # Direction control on the anchor itself: a table that has a 0 row whose
    # description is NOT 'success' is not an exit-code table either, so the
    # anchor cannot be satisfied by any table that merely starts at zero.
    check(
        "arm9 ANCHOR CONTROL: a 0 row that does not say success does not anchor",
        find_exit_code_tables("| 0 | zeroth stage |\n| 9 | ninth |\n"),
        [],
    )

    print(f"check-exit-code-docs self-test: {9 - failures}/9 arms passed")
    return 0 if failures == 0 else 2


def main(argv: list[str]) -> int:
    if len(argv) > 1 and argv[1] == "--self-test":
        return self_test()
    if len(argv) > 1:
        print(__doc__)
        print("usage: check-exit-code-docs.py [--self-test]", file=sys.stderr)
        return 2
    return run_lint()


if __name__ == "__main__":
    sys.exit(main(sys.argv))
