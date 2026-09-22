#!/usr/bin/env python3
"""Check refusal guidance without dispatching RCH or compiling Cargo.

The Rust contract tests call this same checker. Inputs are the verifier source
and tracked Beads export, never live tracker or fleet state.
"""

import argparse
import ast
import copy
import datetime as dt
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys


ROOT = Path(__file__).resolve().parent.parent
LIVE_STATUSES = {"open", "in_progress", "blocked", "deferred"}


class ContractError(ValueError):
    pass


def require(condition, message):
    if not condition:
        raise ContractError(message)


def parse_verifier(source):
    marker = "import datetime as dt\nimport fcntl\nimport hashlib\nimport json\nimport os\nimport re\nfrom pathlib import Path\n\nproof = json.loads(os.environ[\"JSON_PAYLOAD\"])"
    require(source.count(marker) == 1, "cannot locate verifier proof Python block")
    body = source.split(marker, 1)[1].split("\nPY\n", 1)[0]
    return ast.parse(marker + body)


def load_statuses(export):
    statuses = {}
    for line in export.splitlines():
        if not line.strip():
            continue
        row = json.loads(line)
        require(isinstance(row.get("id"), str), "tracker record missing id")
        require(isinstance(row.get("status"), str), "tracker record missing status")
        require(row["id"] not in statuses, f"duplicate tracker id: {row['id']}")
        statuses[row["id"]] = row["status"]
    require(len(statuses) >= 100, f"tracker parse guard: only {len(statuses)} issues")
    return statuses


def mapping_node(module):
    functions = {node.name: node for node in module.body if isinstance(node, ast.FunctionDef)}
    require("remediation_guidance_for" in functions, "guidance function missing")
    assignments = [
        node for node in functions["remediation_guidance_for"].body
        if isinstance(node, ast.Assign)
        and any(isinstance(target, ast.Name) and target.id == "mapping" for target in node.targets)
    ]
    require(len(assignments) == 1, "expected one explicit guidance table")
    require(isinstance(assignments[0].value, ast.Dict), "guidance table must be a literal dictionary")
    return assignments[0].value


def check(module, statuses):
    functions = {node.name: node for node in module.body if isinstance(node, ast.FunctionDef)}
    require("blocker_kind_for" in functions, "blocker classifier missing")
    kinds = set()
    for node in ast.walk(functions["blocker_kind_for"]):
        if isinstance(node, ast.Return):
            require(isinstance(node.value, ast.Constant), "unrecognized blocker return shape")
            require(node.value.value is None or isinstance(node.value.value, str), "invalid blocker kind")
            if isinstance(node.value.value, str):
                kinds.add(node.value.value)
    require(len(kinds) >= 8, f"blocker parse guard: only {len(kinds)} kinds")

    table = mapping_node(module)
    keys = [ast.literal_eval(key) for key in table.keys]
    require(all(isinstance(key, str) for key in keys), "guidance keys must be strings")
    require(len(keys) == len(set(keys)), "duplicate guidance kind")
    guidance = ast.literal_eval(table)
    require(kinds == set(guidance),
            f"guidance coverage: missing={sorted(kinds - set(guidance))}, dead={sorted(set(guidance) - kinds)}")

    mapped = 0
    for kind, record in guidance.items():
        require(isinstance(record, dict), f"{kind}: guidance must be a record")
        require(set(record) in ({"bead"}, {"unmapped_reason"}),
                f"{kind}: require exactly one live bead or explicit unmapped_reason")
        if "bead" in record:
            bead = record["bead"]
            require(isinstance(bead, str) and bead.startswith("bd-"), f"{kind}: invalid bead")
            require(statuses.get(bead) in LIVE_STATUSES,
                    f"{kind}: bead {bead} is {statuses.get(bead, 'missing')}, not live")
            mapped += 1
        else:
            reason = record["unmapped_reason"]
            require(isinstance(reason, str) and bool(reason.strip()), f"{kind}: empty unmapped_reason")

    # Execute only these pure entry-building functions from the actual script.
    # Do not execute its top-level code, shell wrapper, RCH, or ledger writers.
    names = ("remediation_guidance_for", "attach_remediation_guidance",
             "known_blocker_entry", "parse_time", "format_time", "csv_fingerprint")
    require(all(name in functions for name in names), "entry-building function missing")
    namespace = {"dt": dt, "hashlib": hashlib, "json": json, "os": os,
                 "proof": {"generated_at": "2026-09-21T00:00:00Z", "command": []}}
    runtime = ast.Module(body=[functions[name] for name in names], type_ignores=[])
    exec(compile(ast.fix_missing_locations(runtime), "rch_verify.sh:entry-builders", "exec"), namespace)
    emitter_source = ast.unparse(module)
    for kind, record in guidance.items():
        expected = {
            "remediation_bead": record.get("bead"),
            "remediation_bead_status": "mapped" if "bead" in record else "unmapped",
            "remediation_reason": record.get("unmapped_reason"),
        }
        entry = namespace["known_blocker_entry"](kind, [], "local-contract-check")
        require(all(entry.get(key) == value for key, value in expected.items()),
                f"{kind}: emitted entry lost or changed its guidance")
        cached = {"blocker_kind": kind, "remediation_bead": "bd-retired",
                  "retry_after": "unchanged-expiry", "blocker_fingerprint": "unchanged-fingerprint"}
        namespace["attach_remediation_guidance"](cached)
        require(all(cached.get(key) == value for key, value in expected.items()),
                f"{kind}: cached entry retained retired guidance")
        require(cached["retry_after"] == "unchanged-expiry"
                and cached["blocker_fingerprint"] == "unchanged-fingerprint",
                f"{kind}: guidance refresh altered failure evidence or retry policy")

        # Exercise the actual receipt/summary path too: a helper that nobody
        # calls must not count as fixing already-cached citations. This runs
        # only the embedded Python emitter, with no RCH or shell dispatch and
        # no inherited ledger/store paths or write-enabled environment.
        stale = {"blocker_kind": kind, "remediation_bead": "bd-retired",
                 "retry_after": "unchanged-expiry", "blocker_fingerprint": "unchanged-fingerprint"}
        payload = {"schema": "ee.rch.verify.v1", "success": False, "verdict": "failed",
                   "exit_code": 1, "generated_at": "2026-09-21T00:00:00Z",
                   "degraded_codes": ["rch_verify_known_blocker_active"], "known_blocker": stale}
        emitted = subprocess.run(
            [sys.executable, "-B", "-c", emitter_source], capture_output=True, text=True, timeout=10,
            env={"PATH": os.defpath, "JSON_PAYLOAD": json.dumps(payload),
                 "NO_WRITE": "1", "INCLUDE_SUMMARY": "1", "KNOWN_BLOCKER_ENABLED": "0"},
        )
        require(emitted.returncode == 0, f"{kind}: receipt emitter failed: {emitted.stderr}")
        receipt = json.loads(emitted.stdout)
        actual = receipt.get("known_blocker") or {}
        require(receipt.get("status") == "known_blocker_refused"
                and all(actual.get(key) == value for key, value in expected.items()),
                f"{kind}: receipt path retained retired guidance")
        require(actual.get("retry_after") == stale["retry_after"]
                and actual.get("blocker_fingerprint") == stale["blocker_fingerprint"],
                f"{kind}: receipt refresh altered failure evidence or retry policy")
        summary = receipt.get("summary_markdown") or ""
        require(f"remediation_bead: `{record.get('bead') or 'none'}`" in summary
                and f"remediation_bead_status: `{expected['remediation_bead_status']}`" in summary,
                f"{kind}: summary lost guidance status or bead")
        if "unmapped_reason" in record:
            require(f"remediation_reason: {record['unmapped_reason']}" in summary,
                    f"{kind}: summary lost unmapped reason")
    unknown = namespace["remediation_guidance_for"]("__unrecognized_cached_kind__")
    require(set(unknown) == {"unmapped_reason"} and bool(unknown["unmapped_reason"].strip()),
            "unknown cached kind must have a reason and no invented bead")
    return {"kinds": len(kinds), "mapped": mapped, "unmapped_with_reason": len(kinds) - mapped}


def self_test(module, statuses):
    results = []

    def rejected(name, mutate, expected):
        candidate = copy.deepcopy(module)
        mutate(candidate)
        try:
            check(candidate, statuses)
        except ContractError as error:
            require(expected in str(error), f"{name}: wrong failure: {error}")
            results.append({"case": name, "rejected": True, "error": str(error)})
        else:
            raise ContractError(f"{name}: planted defect was accepted")

    def set_first(candidate, value):
        mapping_node(candidate).values[0] = ast.parse(repr(value), mode="eval").body

    def remove_first(candidate):
        table = mapping_node(candidate)
        table.keys.pop(0)
        table.values.pop(0)

    def add_kind(candidate):
        classifier = next(node for node in candidate.body
                          if isinstance(node, ast.FunctionDef) and node.name == "blocker_kind_for")
        classifier.body.insert(0, ast.Return(value=ast.Constant(value="planted_without_guidance")))

    rejected("existing_kind_has_neither_bead_nor_reason", lambda node: set_first(node, {}), "require exactly one")
    rejected("new_kind_has_no_guidance", add_kind, "planted_without_guidance")
    rejected("missing_existing_kind", remove_first, "guidance coverage")
    rejected("blank_unmapped_reason", lambda node: set_first(node, {"unmapped_reason": " \t"}), "empty unmapped_reason")
    rejected("closed_bead", lambda node: set_first(node, {"bead": "bd-17c65.10.17"}), "not live")
    rejected("missing_bead", lambda node: set_first(node, {"bead": "bd-nonexistent-contract-probe"}), "not live")
    rejected("bead_and_reason_ambiguous", lambda node: set_first(node, {"bead": "bd-k2fkz", "unmapped_reason": "ambiguous"}), "require exactly one")

    def empty_classifier(candidate):
        classifier = next(node for node in candidate.body
                          if isinstance(node, ast.FunctionDef) and node.name == "blocker_kind_for")
        classifier.body = [ast.Return(value=ast.Constant(value=None))]

    rejected("empty_kind_parse", empty_classifier, "blocker parse guard")

    def disconnect_cache_refresh(candidate):
        calls = [node for node in ast.walk(candidate)
                 if isinstance(node, ast.Expr) and isinstance(node.value, ast.Call)
                 and isinstance(node.value.func, ast.Name)
                 and node.value.func.id == "attach_remediation_guidance"]
        require(len(calls) == 1, "expected one receipt cache-refresh call")
        calls[0].value = ast.Constant(value=None)

    rejected("cache_refresh_not_called", disconnect_cache_refresh, "receipt path retained retired guidance")

    def drop_emitted_reason(candidate):
        helper = next(node for node in candidate.body
                      if isinstance(node, ast.FunctionDef) and node.name == "attach_remediation_guidance")
        helper.body.insert(-1, ast.parse('entry.pop("remediation_reason", None)').body[0])

    rejected("entry_omits_reason", drop_emitted_reason, "emitted entry lost or changed its guidance")
    try:
        load_statuses("")
    except ContractError as error:
        require("tracker parse guard" in str(error), str(error))
        results.append({"case": "empty_tracker", "rejected": True, "error": str(error)})
    else:
        raise ContractError("empty tracker was accepted")

    # Intentional absence is legal even when there are ZERO bead citations.
    all_unmapped = copy.deepcopy(module)
    table = mapping_node(all_unmapped)
    table.values = [ast.parse(repr({"unmapped_reason": "No current remediation owner is established."}), mode="eval").body
                    for _ in table.keys]
    check(all_unmapped, statuses)
    results.append({"case": "all_kinds_explicitly_unmapped", "accepted": True})
    return results


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--script", type=Path, default=ROOT / "scripts/rch_verify.sh")
    parser.add_argument("--beads", type=Path, default=ROOT / ".beads/issues.jsonl")
    parser.add_argument("--self-test", action="store_true")
    args = parser.parse_args()
    try:
        module = parse_verifier(args.script.read_text())
        statuses = load_statuses(args.beads.read_text())
        report = {"schema": "ee.rch.remediation_guidance_check.v1", "status": "pass",
                  **check(module, statuses)}
        if args.self_test:
            report["cases"] = self_test(module, statuses)
            report["passed"] = len(report["cases"])
            report["failed"] = 0
        print(json.dumps(report, sort_keys=True))
        return 0
    except (ContractError, OSError, SyntaxError, ValueError, KeyError, TypeError, subprocess.TimeoutExpired) as error:
        print(json.dumps({"schema": "ee.rch.remediation_guidance_check.v1",
                          "status": "fail", "error": str(error)}, sort_keys=True))
        return 1


if __name__ == "__main__":
    sys.exit(main())
