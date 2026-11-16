#!/usr/bin/env python3
"""Shrink-only ratchet for shell E2E `ee` data-dir isolation (bd-xuznt).

Every shell E2E script under `scripts/e2e*.sh` and `scripts/e2e_overhaul/*.sh`
that invokes `ee` must isolate `HOME` and the `XDG_*` data/config/cache/state
directories (plus `EE_EMBED_DOWNLOAD=off` by default) so running a shell E2E
never reads or mutates the operator's live `~/.local/share/ee` store, global
event log, or embedding cache.

A script is classified as isolated if it either:
  1. Sources a shared harness library whose setup/init function isolates
     `HOME` + `XDG_*` (`lib/e2e_harness.sh`, `e2e_overhaul/lib/shared.sh`,
     `lib/fake_tailscale.sh`, `e2e_lib/agent_ergonomics_lib.sh`),
  2. Calls `ee_isolate_data_dirs` (from `scripts/lib/ee_binary_resolution.sh`
     or `scripts/lib/e2e_tmproot.sh`), or
  3. Explicitly sets `XDG_DATA_HOME=` and (`HOME=` or `XDG_CONFIG_HOME=`).

Usage:
    scripts/check-shell-ee-spawn-isolation.py              # gate
    scripts/check-shell-ee-spawn-isolation.py --json
    scripts/check-shell-ee-spawn-isolation.py --self-test  # planted arms
"""

from __future__ import annotations

import argparse
import json
import re
import sys
from pathlib import Path

BASELINE = "tests/fixtures/ee_shell_spawn_isolation_baseline.tsv"

SPAWN_PATTERNS = (
    re.compile(r"\$\{?EE_BIN(?:ARY)?\b"),
    re.compile(r"\bharness_(?:init|ee(?:_with_env|_expect_error|_timed)?)\b"),
    re.compile(r"\bepic_setup\b"),
    re.compile(r"\brun_capture\b"),
    re.compile(r"\brun_ee(?:_json)?\b"),
    re.compile(r"\bcapture_ee\b"),
    re.compile(r"\bee_capture\b"),
    re.compile(r"\bee_assert_(?:ok|fail|json|exit)\b"),
    re.compile(r"\binvoke_ee\b"),
    re.compile(r"\bee_resolve_(?:fixture_)?binary\b"),
)

ISOLATED_SHARED_LIBS = (
    "lib/e2e_harness.sh",
    "e2e_lib.sh",
    "lib/shared.sh",
    "lib/fake_tailscale.sh",
    "e2e_lib/agent_ergonomics_lib.sh",
)


def strip_comments(text: str) -> str:
    lines: list[str] = []
    for line in text.splitlines():
        stripped = line.lstrip()
        if stripped.startswith("#"):
            continue
        lines.append(line)
    return "\n".join(lines)


def spawns_ee(text: str) -> bool:
    code = strip_comments(text)
    return any(pattern.search(code) for pattern in SPAWN_PATTERNS)


def is_isolated_shell_script(text: str) -> bool:
    code = strip_comments(text)
    if any(lib in code for lib in ISOLATED_SHARED_LIBS):
        return True
    if "ee_isolate_data_dirs" in code:
        return True
    has_xdg_data = "XDG_DATA_HOME=" in code
    has_home_or_config = ("HOME=" in code) or ("XDG_CONFIG_HOME=" in code)
    return has_xdg_data and has_home_or_config


def unisolated_shell_spawn(text: str) -> int:
    if not spawns_ee(text):
        return 0
    return 0 if is_isolated_shell_script(text) else 1


def candidate_scripts(root: Path) -> list[Path]:
    scripts_dir = root / "scripts"
    found: list[Path] = []
    if scripts_dir.is_dir():
        found.extend(sorted(scripts_dir.glob("e2e*.sh")))
        overhaul_dir = scripts_dir / "e2e_overhaul"
        if overhaul_dir.is_dir():
            found.extend(sorted(overhaul_dir.glob("*.sh")))
    return found


def census(root: Path) -> tuple[dict[str, int], int]:
    counts: dict[str, int] = {}
    spawn_files = 0
    for path in candidate_scripts(root):
        try:
            text = path.read_text(encoding="utf-8", errors="replace")
        except OSError:
            continue
        if not spawns_ee(text):
            continue
        spawn_files += 1
        found = unisolated_shell_spawn(text)
        if found > 0:
            rel = str(path.relative_to(root)).replace("\\", "/")
            counts[rel] = found
    return counts, spawn_files


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
    findings: list[str] = []
    for file in sorted(set(actual) | set(baseline)):
        now = actual.get(file, 0)
        allowed = baseline.get(file, 0)
        if now > allowed:
            findings.append(
                f"{file}: {now} un-isolated shell ee script(s), baseline allows {allowed}. "
                "Call `ee_isolate_data_dirs <root>` (from scripts/lib/ee_binary_resolution.sh "
                "or scripts/lib/e2e_tmproot.sh), source scripts/lib/e2e_harness.sh, or export "
                "isolated HOME and XDG_DATA_HOME."
            )
        elif now < allowed:
            findings.append(
                f"{file}: baseline allows {allowed} but only {now} remain; "
                f"lower the row to {now} (shrink-only)."
            )
    return findings


def run_gate(root: Path, as_json: bool) -> int:
    actual, spawn_files = census(root)
    total = sum(actual.values())
    baseline_path = root / BASELINE
    baseline = (
        parse_baseline(baseline_path.read_text(encoding="utf-8"))
        if baseline_path.exists()
        else {}
    )
    baseline_total = sum(baseline.values())
    findings = ratchet_findings(actual, baseline)

    report = {
        "schema": "ee.shell_spawn_isolation_ratchet.v1",
        "censusTotal": total,
        "censusFiles": len(actual),
        "spawnFiles": spawn_files,
        "baselineTotal": baseline_total,
        "baselineFiles": len(baseline),
        "findings": findings,
    }

    if spawn_files == 0 or (baseline_total > 0 and total == 0):
        report["status"] = "fail"
        report["reason"] = "census found no shell ee spawn scripts; the scan is broken"
        if as_json:
            print(json.dumps(report, indent=2))
        else:
            print(f"FAIL: {report['reason']}", file=sys.stderr)
        return 1

    report["status"] = "fail" if findings else "pass"
    if as_json:
        print(json.dumps(report, indent=2))
    else:
        print(f"  census {total} un-isolated shell script(s) across {spawn_files} spawning script(s)")
        print(f"  baseline {baseline_total} across {len(baseline)} file(s)")
        if findings:
            print("\nFAIL: shell spawn-isolation ratchet:", file=sys.stderr)
            for finding in findings:
                print(f"  {finding}", file=sys.stderr)
        else:
            print("PASS: all shell e2e scripts isolate HOME/XDG data dirs, and baseline matches reality.")
    return 1 if findings else 0


def self_test() -> int:
    failures: list[str] = []

    def arm(label: str, ok: bool) -> None:
        print(f"  [{'ok  ' if ok else 'FAIL'}] {label}")
        if not ok:
            failures.append(label)

    cases = [
        (
            "bare $EE_BIN invocation is un-isolated",
            '#!/usr/bin/env bash\n"$EE_BIN" init --workspace "$WS" --json\n',
            1,
        ),
        (
            "sourcing scripts/lib/e2e_harness.sh isolates",
            '#!/usr/bin/env bash\nsource "$SCRIPT_DIR/lib/e2e_harness.sh"\nharness_ee status --json\n',
            0,
        ),
        (
            "sourcing scripts/e2e_overhaul/lib/shared.sh isolates",
            '#!/usr/bin/env bash\nsource "$SCRIPT_DIR/e2e_overhaul/lib/shared.sh"\n"$EE_BINARY" status --json\n',
            0,
        ),
        (
            "calling ee_isolate_data_dirs isolates",
            '#!/usr/bin/env bash\nsource "$SCRIPT_DIR/lib/ee_binary_resolution.sh"\nee_isolate_data_dirs "$TMP_ROOT"\n"$EE_BIN" status --json\n',
            0,
        ),
        (
            "explicit HOME + XDG_DATA_HOME isolates",
            '#!/usr/bin/env bash\nexport HOME="$TMP/home"\nexport XDG_DATA_HOME="$TMP/data"\n"$EE_BIN" status --json\n',
            0,
        ),
        (
            "EE_EMBED_DOWNLOAD alone is NOT isolation",
            '#!/usr/bin/env bash\nexport EE_EMBED_DOWNLOAD=off\n"$EE_BIN" status --json\n',
            1,
        ),
        (
            "dispatcher script without ee spawn is not counted",
            '#!/usr/bin/env bash\nbash "$SCRIPT_DIR/e2e_test.sh"\n',
            0,
        ),
    ]
    for label, text, want in cases:
        got = unisolated_shell_spawn(text)
        arm(f"{label} (expected {want}, got {got})", got == want)

    arm(
        "a file ABOVE its baseline row fails",
        len(ratchet_findings({"scripts/e2e_a.sh": 1}, {})) == 1,
    )
    arm(
        "a baseline row ABOVE reality fails (must be lowered)",
        len(ratchet_findings({}, {"scripts/e2e_a.sh": 1})) == 1,
    )
    arm(
        "equal counts produce no finding",
        ratchet_findings({}, {}) == [],
    )

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
    return run_gate(root, args.json)


if __name__ == "__main__":
    sys.exit(main())
