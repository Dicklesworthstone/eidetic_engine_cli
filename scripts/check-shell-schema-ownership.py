#!/usr/bin/env python3
"""Shell schema-id shape-compatibility check (bd-92lvw).

Detects incompatible JSON shapes emitted under the same `ee.<name>.v<N>` schema
ID across shell scripts in `scripts/**/*.sh`, while permitting legitimate reuse
of shared schemas (such as `ee.test_event.v1`, `ee.export.*.v1`,
`ee.e2e.retention_manifest.v1`, `ee.perf.v1`, `ee.response.v2`, `ee.error.v2`).

Why producer-count alone is wrong:
  14 schema IDs are legitimately emitted by multiple shell scripts (for example,
  `ee.test_event.v1` by 60+ harnesses and `ee.export.header.v1` by 6 fixtures).
  The defect found in bd-ldypi (`ee.rch.worker_root_canary.v1`) was two scripts
  emitting incompatible shapes under one domain schema ID: one with snake_case
  keys (`generated_at`, `worker_count`, `canary_status`, ...) and one with
  camelCase keys (`generatedAtMs`, `workerHost`, `probeVerdict`, ...) sharing
  only `schema` and `status`.

Usage:
    scripts/check-shell-schema-ownership.py              # gate
    scripts/check-shell-schema-ownership.py --json
    scripts/check-shell-schema-ownership.py --self-test  # planted arms
"""

from __future__ import annotations

import argparse
import json
import re
import sys
from collections import defaultdict
from pathlib import Path

SCHEMA_RE = re.compile(
    r'(?:"schema"\s*:\s*"(ee\.[a-zA-Z0-9_.-]+\.v\d+)"'
    r'|schema\s*:\s*"(ee\.[a-zA-Z0-9_.-]+\.v\d+)"'
    r'|--arg\s+schema\s+"(ee\.[a-zA-Z0-9_.-]+\.v\d+)")'
)

KEY_RE = re.compile(
    r'(?:"([a-zA-Z_][a-zA-Z0-9_]*)"\s*:'
    r'|\b([a-zA-Z_][a-zA-Z0-9_]*)\s*:\s*[\$\"\[\{a-z0-9_])'
)

# Open-extension event/envelope schemas whose payload fields intentionally vary
# by scenario or command while sharing a common envelope header.
OPEN_EXTENSION_SCHEMAS = {
    "ee.test_event.v1",
    "ee.perf.v1",
    "ee.e2e.retention_manifest.v1",
    "ee.response.v2",
    "ee.error.v2",
}

GENERIC_ENVELOPE_KEYS = {"schema", "status", "kind"}
IGNORED_TOKENS = {
    "http",
    "https",
    "blake3",
    "sha256",
    "file",
    "true",
    "false",
    "null",
}


def is_snake_multiword(key: str) -> bool:
    return "_" in key and key == key.lower()


def is_camel_multiword(key: str) -> bool:
    return "_" not in key and any(c.isupper() for c in key[1:]) and key[0].islower()


def extract_file_schema_keys(text: str) -> dict[str, set[str]]:
    """Return {schema_id: union_of_keys_emitted_in_file} for a shell script."""
    by_schema: dict[str, set[str]] = defaultdict(set)
    lines = text.splitlines()
    for idx, line in enumerate(lines):
        stripped = line.lstrip()
        if stripped.startswith("#"):
            continue
        if "grep " in line or "assert_jq" in line or ".schema ==" in line:
            continue
        for match in SCHEMA_RE.finditer(line):
            sid = match.group(1) or match.group(2) or match.group(3)
            window = "\n".join(lines[max(0, idx - 3) : min(len(lines), idx + 35)])
            keys: set[str] = {"schema"}
            for km in KEY_RE.finditer(window):
                k = km.group(1) or km.group(2)
                if k and k not in IGNORED_TOKENS:
                    keys.add(k)
            if len(keys) >= 2:
                by_schema[sid].update(keys)
    return dict(by_schema)


def check_pair_compatibility(
    sid: str,
    file_a: str,
    keys_a: set[str],
    file_b: str,
    keys_b: set[str],
) -> str | None:
    """Return an incompatibility diagnosis if two producers of `sid` collide."""
    if sid in OPEN_EXTENSION_SCHEMAS:
        return None
    domain_a = keys_a - GENERIC_ENVELOPE_KEYS
    domain_b = keys_b - GENERIC_ENVELOPE_KEYS
    if len(domain_a) < 3 or len(domain_b) < 3:
        return None

    shared_domain = domain_a & domain_b

    # Check 1: Pure naming-convention inversion (bd-ldypi pattern: one producer
    # is purely snake_case multi-word keys and the other is purely camelCase
    # multi-word keys with zero shared domain keys).
    snake_a = {k for k in domain_a if is_snake_multiword(k)}
    camel_a = {k for k in domain_a if is_camel_multiword(k)}
    snake_b = {k for k in domain_b if is_snake_multiword(k)}
    camel_b = {k for k in domain_b if is_camel_multiword(k)}
    if (
        not shared_domain
        and ((snake_a and not camel_a and camel_b and not snake_b)
             or (camel_a and not snake_a and snake_b and not camel_b))
    ):
        return (
            f"{sid}: incompatible casing/shape between {file_a} ({sorted(domain_a)[:5]}) "
            f"and {file_b} ({sorted(domain_b)[:5]}): 0 shared domain keys"
        )

    # Check 2: Disjoint domain shapes (0 shared domain keys across two non-trivial
    # domain schemas).
    if len(shared_domain) == 0:
        return (
            f"{sid}: disjoint shapes between {file_a} ({sorted(domain_a)[:5]}) "
            f"and {file_b} ({sorted(domain_b)[:5]}): 0 shared domain keys"
        )

    return None


def analyze_producers(
    producers_by_sid: dict[str, list[tuple[str, set[str]]]]
) -> list[str]:
    findings: list[str] = []
    for sid, entries in sorted(producers_by_sid.items()):
        if len(entries) < 2:
            continue
        for i in range(len(entries)):
            for j in range(i + 1, len(entries)):
                file_a, keys_a = entries[i]
                file_b, keys_b = entries[j]
                diag = check_pair_compatibility(sid, file_a, keys_a, file_b, keys_b)
                if diag:
                    findings.append(diag)
    return findings


def scan_repo(root: Path) -> tuple[dict[str, list[tuple[str, set[str]]]], int]:
    scripts_dir = root / "scripts"
    producers_by_sid: dict[str, list[tuple[str, set[str]]]] = defaultdict(list)
    files_scanned = 0
    for path in sorted(scripts_dir.rglob("*.sh")):
        files_scanned += 1
        try:
            text = path.read_text(encoding="utf-8", errors="replace")
        except OSError:
            continue
        rel = str(path.relative_to(root)).replace("\\", "/")
        for sid, keys in extract_file_schema_keys(text).items():
            producers_by_sid[sid].append((rel, keys))
    return dict(producers_by_sid), files_scanned


def run_gate(root: Path, as_json: bool) -> int:
    producers_by_sid, files_scanned = scan_repo(root)
    distinct_ids = len(producers_by_sid)
    shared_ids = {
        sid: [p for p, _ in entries]
        for sid, entries in sorted(producers_by_sid.items())
        if len(entries) > 1
    }
    findings = analyze_producers(producers_by_sid)

    report = {
        "schema": "ee.shell_schema_ownership.v1",
        "bead": "bd-92lvw",
        "filesScanned": files_scanned,
        "distinctSchemaIds": distinct_ids,
        "sharedSchemaIdCount": len(shared_ids),
        "sharedSchemaIds": {sid: len(paths) for sid, paths in shared_ids.items()},
        "findings": findings,
        "status": "fail" if (findings or distinct_ids == 0) else "pass",
    }

    if as_json:
        print(json.dumps(report, indent=2))
    else:
        print(
            f"  scanned {files_scanned} shell script(s): "
            f"{distinct_ids} distinct schema id(s), {len(shared_ids)} shared id(s)"
        )
        if findings:
            print("\nFAIL: shell schema-id shape incompatibility detected:", file=sys.stderr)
            for f in findings:
                print(f"  {f}", file=sys.stderr)
        else:
            print("PASS: all shared shell schema IDs have compatible shapes.")
    return 0 if report["status"] == "pass" else 1


def self_test() -> int:
    failures: list[str] = []

    def arm(label: str, ok: bool) -> None:
        print(f"  [{'ok  ' if ok else 'FAIL'}] {label}")
        if not ok:
            failures.append(label)

    # 1. Negative control: bd-ldypi collision (snake_case vs camelCase under one id)
    ldypi_producers = {
        "ee.rch.worker_root_canary.v1": [
            (
                "scripts/rch-worker-root-canary.sh",
                {"schema", "status", "generated_at", "worker_count", "canary_status", "writable_roots"},
            ),
            (
                "scripts/rch_verify.sh",
                {"schema", "status", "generatedAtMs", "workerHost", "probeVerdict", "canaryPath"},
            ),
        ]
    }
    arm(
        "planted bd-ldypi snake_case vs camelCase collision fails",
        len(analyze_producers(ldypi_producers)) == 1,
    )

    # 2. Negative control: disjoint domain shapes under one custom schema id
    disjoint_producers = {
        "ee.custom_report.v1": [
            (
                "scripts/a.sh",
                {"schema", "alpha_count", "beta_count", "gamma_count", "delta_count"},
            ),
            (
                "scripts/b.sh",
                {"schema", "widget_id", "gadget_name", "sprocket_hash", "flange_mode"},
            ),
        ]
    }
    arm(
        "planted disjoint domain shapes under one schema id fails",
        len(analyze_producers(disjoint_producers)) == 1,
    )

    # 3. Positive control: shared domain schema with matching keys passes
    compatible_producers = {
        "ee.export.header.v1": [
            (
                "scripts/e2e_overhaul/pack_slo.sh",
                {"schema", "export_id", "created_at", "ee_version", "workspace_id"},
            ),
            (
                "scripts/e2e_overhaul/swarm_hot_path.sh",
                {"schema", "export_id", "created_at", "ee_version", "workspace_id"},
            ),
        ]
    }
    arm(
        "shared domain schema with compatible shape passes",
        analyze_producers(compatible_producers) == [],
    )

    # 4. Positive control: open-extension ee.test_event.v1 passes across varied event fields
    test_event_producers = {
        "ee.test_event.v1": [
            ("scripts/a.sh", {"schema", "ts", "test_id", "kind", "fields"}),
            ("scripts/b.sh", {"schema", "beadId", "surface", "phase", "status", "detail"}),
        ]
    }
    arm(
        "ee.test_event.v1 open-extension schema passes",
        analyze_producers(test_event_producers) == [],
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
