#!/usr/bin/env python3
"""Flag a src/graph/ file named after a README graph metric that computes it with
zero calls into the fnx-* crates.

bd-pmgg0's "Additionally" clause. AGENTS.md states the graph layer is
franken_networkx and that hand-rolled graph algorithms are not acceptable there,
and README advertises a specific set of graph-aware capabilities. The observed
defect that motivates this gate: src/graph/skyline.rs carried the README name
"skyline" while computing an onion-layer x trust-class grid of cell means with
hand-rolled decile bucketing and no dominance test of any kind, for months. A
skyline query IS the Pareto frontier, so the output did not mean what its name
said. That was repaired in 552f7ba4f.

WHAT THIS GATE DOES NOT CATCH, stated because the gap is the interesting part.
The predicate is keyed on the FILE BASENAME matching a metric token. It therefore
catches the skyline.rs shape -- a file named for a metric that hand-rolls it --
and does NOT catch a README metric hand-rolled inside a differently-named file.
PageRank is the live example: README advertises it, there is no src/graph/pagerank.rs,
and the computation lives in other modules. A name-keyed gate cannot see that.
Widening it to "any zero-fnx file in src/graph/" was considered and rejected:
that flags anchor_projection.rs, numa_pin.rs, result_cache_keys.rs and
scale_policy.rs, none of which computes a README-named metric, and a gate whose
output is mostly noise gets switched off.

WHY fnx IS COUNTED IN CODE AND NOT IN TEXT. anchor_projection.rs mentions `fnx`
twice and both mentions are doc comments describing what happens downstream; it
makes zero fnx calls. A gate that counted raw occurrences would read that file as
routed through fnx on the strength of its own prose. The same trap was already
hit once on this bead: skyline.rs matched "pareto|dominat|frontier" 62 times, and
the obvious way for that number to lie was 62 comments explaining why it was NOT
a frontier. Excluding comment lines is the difference between measuring code and
measuring claims.

Usage:
    scripts/check-graph-metric-fnx-routing.py              # gate
    scripts/check-graph-metric-fnx-routing.py --json       # machine-readable
    scripts/check-graph-metric-fnx-routing.py --self-test  # planted arms

The baseline (scripts/graph-metric-fnx-baseline.txt) is SHRINK-ONLY: a listed
file may stop being listed, but a file not listed may never start being flagged.
It is empty at introduction because every metric-named file routes through fnx
today; it exists so that an intentional, reviewed exception can be recorded
without disabling the gate.
"""

from __future__ import annotations

import argparse
import json
import os
import sys
import tempfile
from pathlib import Path

# Derived from README.md: the "Graph-aware" capability row (PageRank, HITS, PPR,
# Gomory-Hu proximity, dominance, causal paths, structural health, Pack DNA,
# skyline views) plus the documented `ee graph <subcommand>` surfaces. Tokens are
# matched as substrings of the file stem, so "ppr" also claims ppr_prefetch_cache.
METRIC_TOKENS: tuple[str, ...] = (
    "skyline",
    "pagerank",
    "hits",
    "ppr",
    "gomory_hu",
    "dominance",
    "causal",
    "structural",
    "pack_dna",
    "betweenness",
    "centrality",
    "communities",
    "louvain",
    "articulation",
    "k_core",
    "proximity",
    "neighborhood",
)

BASELINE_NAME = "graph-metric-fnx-baseline.txt"


def code_fnx_count(text: str) -> int:
    """Count lines mentioning fnx that are not comment lines.

    Deliberately crude: a line-oriented check, not a parser. It is enough to tell
    a call from a doc comment, which is the only distinction this gate needs, and
    a crude check that is obviously right beats a clever one nobody audits.
    """
    total = 0
    for line in text.splitlines():
        stripped = line.lstrip()
        if stripped.startswith("//") or stripped.startswith("/*") or stripped.startswith("*"):
            continue
        if "fnx" in line:
            total += 1
    return total


def metric_tokens_for(stem: str) -> list[str]:
    return [token for token in METRIC_TOKENS if token in stem]


def load_baseline(root: Path) -> set[str]:
    path = root / "scripts" / BASELINE_NAME
    if not path.exists():
        return set()
    entries: set[str] = set()
    for line in path.read_text(encoding="utf-8").splitlines():
        line = line.strip()
        if line and not line.startswith("#"):
            entries.add(line)
    return entries


def audit(root: Path) -> dict:
    graph_dir = root / "src" / "graph"
    examined: list[dict] = []
    flagged: list[str] = []
    for path in sorted(graph_dir.glob("*.rs")):
        stem = path.stem
        tokens = metric_tokens_for(stem)
        if not tokens:
            continue
        fnx = code_fnx_count(path.read_text(encoding="utf-8", errors="replace"))
        examined.append({"file": path.name, "metrics": tokens, "fnxCodeReferences": fnx})
        if fnx == 0:
            flagged.append(path.name)
    return {
        "schema": "ee.graph_metric_fnx_routing.v1",
        "metricNamedFiles": len(examined),
        "examined": examined,
        "flagged": sorted(flagged),
    }


def run_gate(root: Path, as_json: bool) -> int:
    report = audit(root)
    baseline = load_baseline(root)
    flagged = set(report["flagged"])
    new = sorted(flagged - baseline)
    recovered = sorted(baseline - flagged)
    report["baseline"] = sorted(baseline)
    report["newlyFlagged"] = new
    report["baselinedButNowRouted"] = recovered

    if report["metricNamedFiles"] == 0:
        # EMPTY-WORLD GUARD. This gate reports a negative, so a discovery that
        # finds no metric-named files at all would report "nothing flagged" having
        # checked nothing. That happens if src/graph/ moves or METRIC_TOKENS stops
        # matching, and it must fail rather than pass.
        message = (
            "no metric-named files found under src/graph/ -- the gate examined "
            "nothing, which is a gate failure and not a clean result"
        )
        if as_json:
            report["status"] = "fail"
            report["reason"] = message
            print(json.dumps(report, indent=2))
        else:
            print(f"FAIL: {message}", file=sys.stderr)
        return 1

    if as_json:
        report["status"] = "fail" if (new or recovered) else "pass"
        print(json.dumps(report, indent=2))
    else:
        for entry in report["examined"]:
            mark = "FLAG" if entry["fnxCodeReferences"] == 0 else "ok  "
            print(
                f"  {mark} {entry['file']:<28} metrics={','.join(entry['metrics']):<24} "
                f"fnx(code)={entry['fnxCodeReferences']}"
            )
        print(f"\n{report['metricNamedFiles']} metric-named files examined.")
        if new:
            print(
                "\nFAIL: these compute a README-named graph metric with zero fnx calls:",
                file=sys.stderr,
            )
            for name in new:
                print(f"  src/graph/{name}", file=sys.stderr)
            print(
                "\nAGENTS.md routes graph analytics through franken_networkx. Either call "
                "into fnx-*, or record a reviewed exception in "
                f"scripts/{BASELINE_NAME}.",
                file=sys.stderr,
            )
        if recovered:
            print(
                "\nFAIL: baselined files now route through fnx; remove them from "
                f"scripts/{BASELINE_NAME} (it is shrink-only):",
                file=sys.stderr,
            )
            for name in recovered:
                print(f"  src/graph/{name}", file=sys.stderr)
        if not new and not recovered:
            print("PASS: every metric-named file routes through fnx.")
    return 1 if (new or recovered) else 0


def self_test() -> int:
    """Planted arms.

    The case that INSPIRED this gate can no longer validate it: skyline.rs routes
    through fnx since 552f7ba4f, so the gate is green on the very file it was
    written for. A control nobody has seen fail is decoration, so the positive
    arms are planted here instead.
    """
    failures: list[str] = []

    def arm(label: str, ok: bool) -> None:
        print(f"  {'ok  ' if ok else 'FAIL'} {label}")
        if not ok:
            failures.append(label)

    def quiet_gate(root: Path) -> int:
        """Run the gate with its own output captured.

        The arms below deliberately drive the gate into FAILING states, so its
        real stderr ("FAIL: no metric-named files found ...") would interleave
        with the arm results and read as a broken self-test. Capturing it keeps
        the arm list the only thing a reader has to interpret.
        """
        import contextlib
        import io

        sink = io.StringIO()
        with contextlib.redirect_stdout(sink), contextlib.redirect_stderr(sink):
            return run_gate(root, as_json=False)

    with tempfile.TemporaryDirectory(prefix="graph-fnx-selftest-") as tmp:
        root = Path(tmp)
        graph = root / "src" / "graph"
        graph.mkdir(parents=True)
        (root / "scripts").mkdir()

        # POSITIVE: a metric-named file with no fnx call must be flagged.
        (graph / "skyline.rs").write_text(
            "pub fn compute() -> f64 {\n    let decile = 3.0_f64;\n    decile\n}\n",
            encoding="utf-8",
        )
        report = audit(root)
        arm("a metric-named file with zero fnx calls IS flagged", report["flagged"] == ["skyline.rs"])

        # POSITIVE, AND THE ONE THAT MATTERS MOST: fnx mentioned ONLY in comments
        # must still be flagged. This is anchor_projection.rs's real shape, and a
        # raw-occurrence gate passes it.
        (graph / "dominance.rs").write_text(
            "//! This module feeds fnx graph analysis downstream.\n"
            "// fnx_algorithms would be the right home for this one day.\n"
            "pub fn compute() -> f64 {\n    1.0\n}\n",
            encoding="utf-8",
        )
        report = audit(root)
        arm(
            "fnx mentioned only in COMMENTS does not count as routed",
            "dominance.rs" in report["flagged"],
        )

        # NEGATIVE: a real fnx call clears the file.
        (graph / "dominance.rs").write_text(
            "//! This module feeds fnx graph analysis downstream.\n"
            "use fnx_classes::Graph;\n"
            "pub fn compute(g: &Graph) -> f64 {\n    1.0\n}\n",
            encoding="utf-8",
        )
        report = audit(root)
        arm("a real fnx call clears the file", "dominance.rs" not in report["flagged"])

        # NEGATIVE: a file that is NOT metric-named is ignored even with zero fnx.
        # Without this arm the gate would flag numa_pin.rs, result_cache_keys.rs
        # and scale_policy.rs, become noise, and get switched off.
        (graph / "numa_pin.rs").write_text("pub fn pin() {}\n", encoding="utf-8")
        report = audit(root)
        arm(
            "a non-metric file with zero fnx is IGNORED, not flagged",
            "numa_pin.rs" not in report["flagged"],
        )

        # The baseline suppresses a reviewed exception, and only that one.
        (root / "scripts" / BASELINE_NAME).write_text("skyline.rs\n", encoding="utf-8")
        code = quiet_gate(root)
        arm("a baselined file does not fail the gate", code == 0)

        # SHRINK-ONLY: a baselined file that starts routing through fnx must fail,
        # so the baseline cannot accumulate stale entries that hide a later
        # regression under the same name.
        (graph / "skyline.rs").write_text(
            "use fnx_classes::Graph;\npub fn compute(g: &Graph) -> f64 {\n    1.0\n}\n",
            encoding="utf-8",
        )
        code = quiet_gate(root)
        arm("a baselined file that now routes through fnx FAILS (shrink-only)", code == 1)

    with tempfile.TemporaryDirectory(prefix="graph-fnx-empty-") as tmp:
        root = Path(tmp)
        (root / "src" / "graph").mkdir(parents=True)
        (root / "scripts").mkdir()
        code = quiet_gate(root)
        arm("an EMPTY src/graph/ fails instead of passing vacuously", code == 1)

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
