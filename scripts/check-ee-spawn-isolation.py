#!/usr/bin/env python3
"""Hosted mirror of tests/ee_spawn_isolation_contract.rs (bd-rvrj2 / bd-gpik0).

WHY A SECOND IMPLEMENTATION EXISTS, stated plainly because duplication is a cost.

The Rust contract is the AUTHORITY. This script exists only because that contract
is not reachable from any hosted lane:

  - scripts/verify.sh:2072 runs `cargo test --workspace --lib --bins --tests
    --examples`, which builds the whole `integration_e_f` target and therefore DOES
    execute the contract. So the ratchet is not inert locally.
  - But every hosted lane that touches that target passes a FILTER --
    `e2e_doctor_concise_default::`, `focus_suggest_phase2_e2e::`,
    `e2e_cass_import_redaction::ingestion_boundaries::`, `eval_report_e2e::` -- and a
    filter excludes `ee_spawns_do_not_add_unisolated_sites`. No hosted lane runs it.

Running the Rust contract in a hosted lane would mean compiling integration_e_f,
which is minutes of build for a check that reads text files. The predicate is pure
static analysis: it opens sources and a TSV and spawns nothing. So it ports.

DRIFT IS THE RISK AND IT IS NOT FULLY MITIGATED. If someone edits the Rust
classifier -- adds a fifth isolation marker, changes how a fn body is delimited --
this file does not follow automatically and the two can disagree. Mitigations: the
constants below are copied verbatim with their Rust names noted, the self-test uses
the SAME planted fixtures as the contract's `classifier_controls_both_directions`,
and a disagreement shows up as this gate failing while the Rust test passes, which
is the safe direction. RETIRE THIS FILE when the Rust contract gains a hosted lane,
or when the baseline reaches zero.

Usage:
    scripts/check-ee-spawn-isolation.py              # gate
    scripts/check-ee-spawn-isolation.py --json
    scripts/check-ee-spawn-isolation.py --self-test  # planted arms
"""

from __future__ import annotations

import argparse
import json
import os
import sys
from pathlib import Path

# Mirrors SPAWN_TOKEN in the Rust contract, which builds it with concat! so the
# guard does not count itself as a spawn site. Same trick, same reason.
SPAWN_TOKEN = "CARGO_BIN_" + "EXE_ee"
# Mirrors ISOLATION_MARKERS.
ISOLATION_MARKERS = ('"HOME"', '"XDG_DATA_HOME"', ".env_clear()", "isolated_ee_command(")
BASELINE = "tests/fixtures/ee_spawn_isolation_baseline.tsv"
HELPER = "tests/support/isolated_ee.rs"
SCAN_ROOTS = ("tests", "src")


def is_fn_header(line: str) -> bool:
    """Mirrors `is_fn_header`: [pub[(..)]] [async] [const] [unsafe] fn <name>."""
    rest = line.lstrip()
    if rest.startswith("pub"):
        after = rest[3:]
        if after.startswith("("):
            close = after.find(")")
            if close < 0:
                return False
            after = after[close + 1 :]
        if not after[:1].isspace():
            return False
        rest = after.lstrip()
    for keyword in ("async", "const", "unsafe"):
        if rest.startswith(keyword) and rest[len(keyword) : len(keyword) + 1].isspace():
            rest = rest[len(keyword) :].lstrip()
    if not rest.startswith("fn"):
        return False
    after = rest[2:]
    head = after.lstrip()[:1]
    return bool(after[:1].isspace()) and (head.isalnum() or head == "_")


def enclosing_fn(lines: list[str], idx: int) -> tuple[int, str]:
    """Mirrors `enclosing_fn`: brace-matched body, 40-line window fallback."""
    for start in range(idx, -1, -1):
        if not is_fn_header(lines[start]):
            continue
        depth = 0
        opened = False
        body: list[str] = []
        for j in range(start, len(lines)):
            line = lines[j]
            body.append(line)
            depth += line.count("{") - line.count("}")
            if "{" in line:
                opened = True
            if opened and depth <= 0:
                if j >= idx:
                    return start, "\n".join(body)
                break
    low = max(0, idx - 40)
    high = min(len(lines), idx + 40)
    return idx, "\n".join(lines[low:high])


def unisolated_spawn_fns(text: str) -> int:
    """Mirrors `unisolated_spawn_fns`: distinct un-isolated spawning fns."""
    lines = text.split("\n")
    seen: set[int] = set()
    count = 0
    for idx, line in enumerate(lines):
        if SPAWN_TOKEN not in line:
            continue
        start, body = enclosing_fn(lines, idx)
        if start in seen:
            continue
        seen.add(start)
        if not any(marker in body for marker in ISOLATION_MARKERS):
            count += 1
    return count


def census(root: Path) -> dict[str, int]:
    counts: dict[str, int] = {}
    for scan in SCAN_ROOTS:
        for dirpath, _dirs, files in os.walk(root / scan):
            for name in files:
                if not name.endswith(".rs"):
                    continue
                path = Path(dirpath) / name
                try:
                    text = path.read_text(encoding="utf-8", errors="replace")
                except OSError:
                    continue
                if SPAWN_TOKEN not in text:
                    continue
                found = unisolated_spawn_fns(text)
                if found > 0:
                    counts[str(path.relative_to(root)).replace("\\", "/")] = found
    return counts


def parse_baseline(text: str) -> dict[str, int]:
    rows: dict[str, int] = {}
    for line in text.splitlines():
        line = line.strip()
        if not line or line.startswith("#"):
            continue
        count, _, path = line.partition("\t")
        rows[path.strip()] = int(count.strip())
    return rows


def ratchet_findings(actual: dict[str, int], baseline: dict[str, int]) -> list[str]:
    """Mirrors `ratchet_findings`: shrink-only, fails in BOTH directions."""
    findings = []
    for file in sorted(set(actual) | set(baseline)):
        now = actual.get(file, 0)
        allowed = baseline.get(file, 0)
        if now > allowed:
            findings.append(
                f"{file}: {now} un-isolated ee spawn fn(s), baseline allows {allowed}. "
                f"Spawn ee through isolated_ee_command ({HELPER}) or set HOME/XDG_DATA_HOME."
            )
        elif now < allowed:
            findings.append(
                f"{file}: baseline allows {allowed} but only {now} remain; "
                f"lower the row to {now} (shrink-only)."
            )
    return findings


def run_gate(root: Path, as_json: bool) -> int:
    actual = census(root)
    total = sum(actual.values())
    baseline_path = root / BASELINE
    baseline = parse_baseline(baseline_path.read_text(encoding="utf-8")) if baseline_path.exists() else {}
    findings = ratchet_findings(actual, baseline)

    report = {
        "schema": "ee.spawn_isolation_ratchet.v1",
        "censusTotal": total,
        "censusFiles": len(actual),
        "baselineTotal": sum(baseline.values()),
        "baselineFiles": len(baseline),
        "findings": findings,
    }

    # EMPTY-WORLD GUARD, mirroring the contract's own assertion: a census that
    # finds nothing means the scan broke, not that the repo is clean.
    if total == 0:
        report["status"] = "fail"
        report["reason"] = "census found no un-isolated ee spawns; the scan is broken or the baseline is obsolete"
        if as_json:
            print(json.dumps(report, indent=2))
        else:
            print(f"FAIL: {report['reason']}", file=sys.stderr)
        return 1

    # POSITIVE CONTROL, mirroring the contract: the isolated helper DOES spawn ee
    # and must NOT be counted. If it ever is, the classifier has inverted.
    helper_path = root / HELPER
    if helper_path.exists():
        helper_text = helper_path.read_text(encoding="utf-8")
        if SPAWN_TOKEN not in helper_text:
            report["status"] = "fail"
            report["reason"] = f"{HELPER} must spawn ee for the positive control to mean anything"
            if as_json:
                print(json.dumps(report, indent=2))
            else:
                print(f"FAIL: {report['reason']}", file=sys.stderr)
            return 1
        if unisolated_spawn_fns(helper_text) != 0:
            report["status"] = "fail"
            report["reason"] = f"{HELPER} is the isolated helper and must not be counted un-isolated"
            if as_json:
                print(json.dumps(report, indent=2))
            else:
                print(f"FAIL: {report['reason']}", file=sys.stderr)
            return 1

    report["status"] = "fail" if findings else "pass"
    if as_json:
        print(json.dumps(report, indent=2))
    else:
        print(f"  census {total} un-isolated spawn fn(s) across {len(actual)} file(s)")
        print(f"  baseline {sum(baseline.values())} across {len(baseline)} file(s)")
        if findings:
            print("\nFAIL: spawn-isolation ratchet:", file=sys.stderr)
            for finding in findings:
                print(f"  {finding}", file=sys.stderr)
        else:
            print("PASS: no new un-isolated ee spawns, and no baseline row is above reality.")
    return 1 if findings else 0


def self_test() -> int:
    """Planted arms, using the SAME fixtures as the Rust contract's
    `classifier_controls_both_directions`. Agreement with those fixtures is the
    only thing keeping this mirror honest."""
    failures: list[str] = []

    def arm(label: str, ok: bool) -> None:
        print(f"  [{'ok  ' if ok else 'FAIL'}] {label}")
        if not ok:
            failures.append(label)

    spawn = f"env!(\"{SPAWN_TOKEN}\")"
    cases = [
        ("a bare spawn is un-isolated",
         f'fn run(ws: &Path) -> Output {{\n    Command::new({spawn}).arg("--workspace").arg(ws).output().unwrap()\n}}\n', 1),
        ("setting HOME isolates (pub fn, multi-line)",
         f'pub fn run(home: &Path) -> Output {{\n    Command::new({spawn})\n        .env("HOME", home)\n        .output()\n        .unwrap()\n}}\n', 0),
        ("setting XDG_DATA_HOME isolates (separate statement)",
         f'fn command(data: &Path) -> Command {{\n    let mut command = Command::new({spawn});\n    command.env("XDG_DATA_HOME", data);\n    command\n}}\n', 0),
        ("env_clear isolates (async fn)",
         f'async fn run() {{\n    let _ = Command::new({spawn}).env_clear().status();\n}}\n', 0),
        ("routing through the helper isolates",
         'fn run(root: &Path) -> Output {\n    isolated_ee_command(root).unwrap().output().unwrap()\n}\n', 0),
        ("EE_EMBED_DOWNLOAD alone is NOT isolation",
         f'fn run(ws: &Path) -> Output {{\n    Command::new({spawn}).env("EE_EMBED_DOWNLOAD", "off").arg(ws).output().unwrap()\n}}\n', 1),
    ]
    for label, text, want in cases:
        got = unisolated_spawn_fns(text)
        arm(f"{label} (expected {want}, got {got})", got == want)

    # Two spawns in ONE fn count once: the contract dedupes by header line.
    two_in_one = (
        f'fn run() {{\n    let _ = Command::new({spawn}).status();\n'
        f'    let _ = Command::new({spawn}).status();\n}}\n'
    )
    arm("two spawns in one fn count as ONE un-isolated fn",
        unisolated_spawn_fns(two_in_one) == 1)

    # Shrink-only, both directions.
    arm("a file ABOVE its row fails",
        len(ratchet_findings({"a.rs": 2}, {"a.rs": 1})) == 1)
    arm("a row ABOVE reality fails (must be lowered)",
        len(ratchet_findings({"a.rs": 1}, {"a.rs": 2})) == 1)
    arm("equal counts produce no finding",
        ratchet_findings({"a.rs": 1}, {"a.rs": 1}) == [])
    arm("a NEW file with no row fails",
        len(ratchet_findings({"new.rs": 1}, {})) == 1)

    if failures:
        print(f"\n{len(failures)} self-test arm(s) failed", file=sys.stderr)
        return 1
    print("\nAll self-test arms passed.")
    return 0


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--json", action="store_true")
    parser.add_argument("--self-test", action="store_true")
    parser.add_argument("--root", default=None)
    args = parser.parse_args()
    if args.self_test:
        return self_test()
    root = Path(args.root) if args.root else Path(__file__).resolve().parent.parent
    return run_gate(root, as_json=args.json)


if __name__ == "__main__":
    sys.exit(main())
