#!/usr/bin/env python3
"""Static ratchet for ADR 0088 degraded/error repair actionability (bd-9zfku).

Per ADR 0088 (`docs/adr/0088-degraded-repair-actionability.md`):
1. `degraded[].repair` stays human-facing prose and is NOT required to name a
   runnable command.
2. `degraded[].repairCommand` (`Option<String>`) is the optional declared field
   for a complete, runnable command when one exists alongside prose.
3. A repair literal in product code (`src/**/*.rs`, excluding inline
   `#[cfg(test)]` blocks) that names a CLI flag (`--<flag>`) or environment
   variable (`EE_*`, `CARGO_*`, `RCH_*`, etc.) while neither `repair` nor
   `repair_command` carries a complete tool invocation is a defect class gated
   against the shrink-only baseline at
   `tests/fixtures/contracts/degraded_repair_flag_only_baseline.txt`.
4. An advisory prose repair (with no flag and no command) is explicitly VALID
   and must never be handed a synthetic command to satisfy this gate.

Exit codes:
  0  Check or --self-test passed.
  1  New unbaselined flag/env-only repair, empty repair, or stale baseline row.
  2  Usage error.
  3  CannotAnswer: source tree or baseline missing/empty.
"""

from __future__ import annotations

from collections import Counter
from dataclasses import dataclass
from pathlib import Path
import argparse
import json
import re
import sys
import tempfile

EXIT_OK = 0
EXIT_FAIL = 1
EXIT_USAGE = 2
EXIT_CANNOT = 3

BASELINE_REL_PATH = Path(
    "tests/fixtures/contracts/degraded_repair_flag_only_baseline.txt"
)

CFG_TEST_RE = re.compile(r"#\s*\[\s*cfg\s*\(\s*test\s*\)\s*\]")
REPAIR_FIELD_RE = re.compile(
    r"\brepair\s*:\s*(?:Some\s*\(\s*)?(?:format!\s*\(\s*)?\"((?:[^\"\\]|\\.)*)\"",
    re.DOTALL,
)
REPAIR_COMMAND_SIBLING_RE = re.compile(
    r"(?:\brepair_command\s*:\s*Some\s*\(\s*(?:format!\s*\(\s*)?\"((?:[^\"\\]|\\.)*)\""
    r"|\.with_repair_command\s*\(\s*(?:format!\s*\(\s*)?\"((?:[^\"\\]|\\.)*)\")",
    re.DOTALL,
)
TOOL_RE = re.compile(
    r"(?:^|[\s`\"\x27(\[])(?:\./)?(?:"
    r"ee|br|bv|cargo|rch|git|sqlite3|chmod|cass|tailscale|curl|ollama|"
    r"python3|pytest|rustup|dsr|jq|rg|scripts/[A-Za-z0-9_./-]+\.(?:sh|py)"
    r")\s+\S"
)
FLAG_OR_ENV_RE = re.compile(
    r"(?:^|[\s`\"\x27(\[])--[a-z0-9][a-z0-9_-]*|"
    r"\b(?:EE|CARGO|RCH|RUST|SQLITE|TAILSCALE|TMPDIR)_[A-Z0-9_]+\b"
)


class CannotAnswer(Exception):
    """Raised when the source tree or baseline cannot be inspected."""


@dataclass(frozen=True)
class Problem:
    kind: str
    path: str
    repair: str
    detail: str

    def to_dict(self) -> dict[str, str]:
        return {
            "kind": self.kind,
            "path": self.path,
            "repair": self.repair,
            "detail": self.detail,
        }


def strip_cfg_test_blocks(source: str) -> str:
    """Exclude inline #[cfg(test)] items by brace/statement counting."""
    out: list[str] = []
    pos = 0
    n = len(source)
    for match in CFG_TEST_RE.finditer(source):
        if match.start() < pos:
            continue
        out.append(source[pos : match.start()])
        i = match.end()
        in_str = False
        escape = False
        brace_start = -1
        while i < n:
            ch = source[i]
            if in_str:
                if escape:
                    escape = False
                elif ch == "\\":
                    escape = True
                elif ch == '"':
                    in_str = False
            else:
                if ch == '"':
                    in_str = True
                elif ch in ("{", ";"):
                    brace_start = i
                    break
            i += 1
        if brace_start == -1:
            pos = match.end()
            continue
        if source[brace_start] == ";":
            out.append("\n" * source[match.start() : brace_start + 1].count("\n"))
            pos = brace_start + 1
            continue
        depth = 1
        i = brace_start + 1
        while i < n and depth > 0:
            ch = source[i]
            if in_str:
                if escape:
                    escape = False
                elif ch == "\\":
                    escape = True
                elif ch == '"':
                    in_str = False
            else:
                if ch == "/" and i + 1 < n and source[i + 1] == "/":
                    nl = source.find("\n", i)
                    i = n if nl == -1 else nl
                    continue
                if ch == '"':
                    in_str = True
                elif ch == "{":
                    depth += 1
                elif ch == "}":
                    depth -= 1
            i += 1
        out.append("\n" * source[match.start() : i].count("\n"))
        pos = i
    out.append(source[pos:])
    return "".join(out)


def normalize_repair_literal(raw: str) -> str:
    collapsed = re.sub(r"\\\n\s*", " ", raw).replace('\\"', '"').strip()
    return re.sub(r"\s+", " ", collapsed)


def classify_repair_site(repair_text: str, sibling_repair_command: str | None) -> str:
    if not repair_text:
        return "empty"
    if sibling_repair_command and TOOL_RE.search(sibling_repair_command):
        return "runnable"
    if TOOL_RE.search(repair_text):
        return "runnable"
    if FLAG_OR_ENV_RE.search(repair_text):
        return "flag_or_env_only"
    return "advisory_prose"


def load_baseline(baseline_path: Path) -> dict[tuple[str, str], int]:
    if not baseline_path.is_file():
        raise CannotAnswer(f"missing baseline file: {baseline_path}")
    entries: dict[tuple[str, str], int] = {}
    for lineno, raw_line in enumerate(
        baseline_path.read_text(encoding="utf-8").splitlines(), 1
    ):
        line = raw_line.strip()
        if not line or line.startswith("#"):
            continue
        parts = raw_line.split("\t", 2)
        if len(parts) != 3:
            raise CannotAnswer(
                f"{baseline_path}:{lineno}: expected <path>\\t<count>\\t<repair>"
            )
        rel_path, count_str, repair_val = parts[0].strip(), parts[1].strip(), parts[2].strip()
        try:
            count = int(count_str)
        except ValueError as exc:
            raise CannotAnswer(
                f"{baseline_path}:{lineno}: invalid count {count_str!r}"
            ) from exc
        if count <= 0:
            raise CannotAnswer(
                f"{baseline_path}:{lineno}: baseline count must be >= 1, got {count}"
            )
        key = (rel_path, repair_val)
        if key in entries:
            raise CannotAnswer(
                f"{baseline_path}:{lineno}: duplicate baseline key {key!r}"
            )
        entries[key] = count
    return entries


def check_repo(
    repo_root: Path,
    baseline_path: Path | None = None,
) -> tuple[dict[str, int], list[Problem]]:
    src_root = repo_root / "src"
    if not src_root.is_dir():
        raise CannotAnswer(f"missing src directory under {repo_root}")
    rs_files = sorted(
        p for p in src_root.rglob("*.rs") if not p.name.startswith("._")
    )
    if not rs_files:
        raise CannotAnswer(f"enumerated 0 Rust source files under {src_root}")

    resolved_baseline = baseline_path or (repo_root / BASELINE_REL_PATH)
    baseline = load_baseline(resolved_baseline)

    counts = {
        "runnable": 0,
        "flag_or_env_only": 0,
        "advisory_prose": 0,
        "empty": 0,
    }
    observed_flag_only: Counter[tuple[str, str]] = Counter()
    problems: list[Problem] = []

    for rs_path in rs_files:
        rel_path = rs_path.relative_to(repo_root).as_posix()
        prod_source = strip_cfg_test_blocks(rs_path.read_text(encoding="utf-8"))
        for match in REPAIR_FIELD_RE.finditer(prod_source):
            repair_val = normalize_repair_literal(match.group(1))
            lookahead = prod_source[match.end() : match.end() + 320]
            sibling_match = REPAIR_COMMAND_SIBLING_RE.search(lookahead)
            sibling_cmd = None
            if sibling_match:
                raw_cmd = sibling_match.group(1) or sibling_match.group(2) or ""
                sibling_cmd = normalize_repair_literal(raw_cmd)

            category = classify_repair_site(repair_val, sibling_cmd)
            counts[category] += 1
            if category == "empty":
                problems.append(
                    Problem(
                        kind="empty_repair_literal",
                        path=rel_path,
                        repair="",
                        detail="Emitted repair literal is empty after trimming.",
                    )
                )
            elif category == "flag_or_env_only":
                observed_flag_only[(rel_path, repair_val)] += 1

    total_repairs = sum(counts.values())
    if total_repairs == 0:
        raise CannotAnswer(f"enumerated 0 repair literals under {src_root}")

    # New arm: any observed flag/env-only repair absent from baseline or above count
    for key, obs_count in sorted(observed_flag_only.items()):
        rel_path, repair_val = key
        allowed = baseline.get(key, 0)
        if obs_count > allowed:
            problems.append(
                Problem(
                    kind="new_flag_only_repair_without_command",
                    path=rel_path,
                    repair=repair_val,
                    detail=(
                        f"observed {obs_count} flag/env-only repair site(s) without a tool "
                        f"invocation, baseline allows {allowed}. Populate repairCommand / "
                        f"with_repair_command or include a complete runnable command."
                    ),
                )
            )

    # Stale arm: any baseline entry whose observed flag/env-only count shrank
    for key, allowed in sorted(baseline.items()):
        rel_path, repair_val = key
        obs_count = observed_flag_only.get(key, 0)
        if obs_count < allowed:
            problems.append(
                Problem(
                    kind="stale_baseline_entry_acquired_command_or_removed",
                    path=rel_path,
                    repair=repair_val,
                    detail=(
                        f"baseline declares {allowed} flag/env-only site(s), but observed "
                        f"{obs_count}. Lower or delete this row in "
                        f"{BASELINE_REL_PATH.as_posix()}."
                    ),
                )
            )

    return counts, problems


def run_self_test() -> int:
    with tempfile.TemporaryDirectory() as tmp:
        root = Path(tmp)
        src_dir = root / "src"
        src_dir.mkdir(parents=True, exist_ok=True)
        baseline_file = root / "baseline.txt"

        # 1. CannotAnswer on empty src/
        baseline_file.write_text("", encoding="utf-8")
        try:
            check_repo(root, baseline_file)
            print("SELF-TEST FAIL: empty src/ did not raise CannotAnswer", file=sys.stderr)
            return EXIT_FAIL
        except CannotAnswer:
            pass

        # 2. CannotAnswer on 0 repair literals
        sample_rs = src_dir / "lib.rs"
        sample_rs.write_text("pub fn noop() {}\n", encoding="utf-8")
        try:
            check_repo(root, baseline_file)
            print("SELF-TEST FAIL: 0 repairs did not raise CannotAnswer", file=sys.stderr)
            return EXIT_FAIL
        except CannotAnswer:
            pass

        # 3. Positive control (new arm): repair naming only a flag FAILS when unbaselined
        sample_rs.write_text(
            'pub fn bad() { let _ = Entry { repair: "Re-run with --limit <= 10" }; }\n',
            encoding="utf-8",
        )
        _, probs = check_repo(root, baseline_file)
        if not any(p.kind == "new_flag_only_repair_without_command" for p in probs):
            print("SELF-TEST FAIL: unbaselined flag-only repair did not fail new arm", file=sys.stderr)
            return EXIT_FAIL

        # 4. Negative control (new arm resolved in repair): same repair carrying a command PASSES
        sample_rs.write_text(
            'pub fn good_cmd() { let _ = Entry { repair: "Re-run `ee search --limit 10 --json`." }; }\n',
            encoding="utf-8",
        )
        _, probs = check_repo(root, baseline_file)
        if probs:
            print(f"SELF-TEST FAIL: runnable command in repair failed: {probs}", file=sys.stderr)
            return EXIT_FAIL

        # 5. Negative control (new arm resolved via repair_command): flag-only repair + repair_command PASSES
        sample_rs.write_text(
            'pub fn good_sibling() {\n'
            '    let _ = Entry {\n'
            '        repair: "Re-run with --limit <= 10",\n'
            '        repair_command: Some("ee search --limit 10 --json".to_owned()),\n'
            '    };\n'
            '}\n',
            encoding="utf-8",
        )
        _, probs = check_repo(root, baseline_file)
        if probs:
            print(f"SELF-TEST FAIL: sibling repair_command did not satisfy gate: {probs}", file=sys.stderr)
            return EXIT_FAIL

        # 6. Positive control (stale arm): baseline entry whose site now has a command FAILS
        baseline_file.write_text(
            "src/lib.rs\t1\tRe-run with --limit <= 10\n",
            encoding="utf-8",
        )
        _, probs = check_repo(root, baseline_file)
        if not any(
            p.kind == "stale_baseline_entry_acquired_command_or_removed" for p in probs
        ):
            print("SELF-TEST FAIL: stale baseline row did not fail stale arm", file=sys.stderr)
            return EXIT_FAIL
        baseline_file.write_text("", encoding="utf-8")

        # 7. Negative control (advisory prose): prose with no flag and no command PASSES
        sample_rs.write_text(
            'pub fn advisory() {\n'
            '    let _ = Entry { repair: "Explicitly re-enable mesh after containment review." };\n'
            '}\n',
            encoding="utf-8",
        )
        counts, probs = check_repo(root, baseline_file)
        if probs or counts["advisory_prose"] != 1:
            print(f"SELF-TEST FAIL: advisory prose failed: {counts} {probs}", file=sys.stderr)
            return EXIT_FAIL

        # 8. Positive control (DOTALL line continuation) + Negative control (#[cfg(test)] exclusion & extended tools)
        sample_rs.write_text(
            'pub fn tools_and_continuation() {\n'
            '    let _ = Entry { repair: "Run `br sync --flush-only` then `tailscale status`." };\n'
            '    let _ = Entry { repair: "Use \\\n        --port 41888." };\n'
            '}\n'
            '#[cfg(test)]\n'
            'mod tests {\n'
            '    fn test_only() {\n'
            '        let _ = Entry { repair: "Use --test-only-flag" };\n'
            '    }\n'
            '}\n',
            encoding="utf-8",
        )
        counts, probs = check_repo(root, baseline_file)
        if (
            counts["runnable"] != 1
            or counts["flag_or_env_only"] != 1
            or len(probs) != 1
            or probs[0].repair != "Use --port 41888."
        ):
            print(
                f"SELF-TEST FAIL: DOTALL / cfg(test) / tool check unexpected: {counts} {probs}",
                file=sys.stderr,
            )
            return EXIT_FAIL

    print("check-degraded-repair-actionability.py --self-test: 8/8 arms passed")
    return EXIT_OK


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--self-test", action="store_true", help="Run self-tests")
    parser.add_argument("--json", action="store_true", help="Emit JSON report")
    parser.add_argument(
        "--repo-root",
        type=Path,
        default=Path(__file__).resolve().parent.parent,
        help="Repository root",
    )
    args = parser.parse_args(argv)

    if args.self_test:
        return run_self_test()

    try:
        counts, problems = check_repo(args.repo_root)
    except CannotAnswer as exc:
        if args.json:
            print(
                json.dumps(
                    {
                        "schema": "ee.degraded_repair_actionability_gate.v1",
                        "verdict": "cannot_answer",
                        "reason": str(exc),
                    },
                    indent=2,
                )
            )
        else:
            print(f"CannotAnswer: {exc}", file=sys.stderr)
        return EXIT_CANNOT

    verdict = "pass" if not problems else "fail"
    if args.json:
        print(
            json.dumps(
                {
                    "schema": "ee.degraded_repair_actionability_gate.v1",
                    "verdict": verdict,
                    "counts": counts,
                    "totalRepairs": sum(counts.values()),
                    "problemCount": len(problems),
                    "problems": [p.to_dict() for p in problems],
                },
                indent=2,
            )
        )
    else:
        print(
            f"degraded-repair-actionability: verdict={verdict} "
            f"runnable={counts['runnable']} "
            f"flag_or_env_only={counts['flag_or_env_only']} "
            f"advisory_prose={counts['advisory_prose']} "
            f"empty={counts['empty']} "
            f"problems={len(problems)}"
        )
        for prob in problems:
            print(
                f"  [{prob.kind}] {prob.path}: {prob.repair!r} -- {prob.detail}",
                file=sys.stderr,
            )

    return EXIT_OK if not problems else EXIT_FAIL


if __name__ == "__main__":
    sys.exit(main())
