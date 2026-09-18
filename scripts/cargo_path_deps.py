#!/usr/bin/env python3
"""Answer one question about a Cargo manifest: does it declare a PATH DEPENDENCY?

bd-e5h7g. `scripts/rch_verify.sh` used to answer this with a substring test over
the whole committed `Cargo.toml`:

    if "path" in text and "path =" in text: ...refuse...

That is not the same question. A manifest says `path =` for two unrelated
reasons, and only one of them is a dependency:

    [dependencies]                          <- a path DEPENDENCY. Cargo resolves
    foo = { path = "../foo" }                  this from the filesystem, so a
                                               committed tree cannot be
                                               materialised in isolation.

    [[test]]                                <- a TARGET path. It names a file
    name = "integration_a_d"                   INSIDE this repository. It says
    path = "tests/suites/integration_a_d.rs"   nothing about external sources.

This repository has 45 of the second and 0 of the first, and `autotests = false`
is exactly why it must spell out each `[[test]]` target with a `path =`. So the
substring test refused every verification lane on a property the repo does not
have, and did so in preflight, before RCH was ever contacted.

WHY THIS IS A SEPARATE FILE. It is the single source of truth for the predicate.
`rch_verify.sh` loads this module, and `tests/rch_verify_path_deps.rs` executes
THIS FILE directly, so the tested code and the shipped code cannot drift apart.
A copy of the logic inside the test would only prove the copy.

FAIL CLOSED. The repair makes a gate MORE PERMISSIVE, and more permissive is
indistinguishable from weaker unless the widening is confined to cases that are
proven. So `declares_path_dependencies` returns None — not False — whenever the
manifest cannot be parsed, and the caller must keep refusing on None. The gate
relaxes only where the absence of path dependencies is established, never where
it is merely unobserved.
"""

from __future__ import annotations

import json
import sys

SCHEMA = "ee.cargo_path_deps.v1"

#: Table names Cargo reads as dependency sets. `[patch.*]` and `[replace]` are
#: included because both can redirect a dependency to a local path, which has
#: the same consequence for materialisation as a direct path dependency.
DEPENDENCY_TABLE_KEYS = ("dependencies", "dev-dependencies", "build-dependencies")


def _dependency_tables(manifest):
    """Yield (label, table) for every table whose values are dependency specs."""
    for key in DEPENDENCY_TABLE_KEYS:
        table = manifest.get(key)
        if isinstance(table, dict):
            yield key, table

    # `[workspace.dependencies]`, inherited by members via `workspace = true`.
    workspace = manifest.get("workspace")
    if isinstance(workspace, dict):
        for key in DEPENDENCY_TABLE_KEYS:
            table = workspace.get(key)
            if isinstance(table, dict):
                yield f"workspace.{key}", table

    # `[target.'cfg(unix)'.dependencies]` and friends.
    targets = manifest.get("target")
    if isinstance(targets, dict):
        for spec, cfg_table in targets.items():
            if not isinstance(cfg_table, dict):
                continue
            for key in DEPENDENCY_TABLE_KEYS:
                table = cfg_table.get(key)
                if isinstance(table, dict):
                    yield f"target.{spec}.{key}", table

    # `[patch.crates-io] foo = { path = "..." }` redirects a registry crate to a
    # local checkout; materialisation is just as impossible as for a direct
    # path dependency, so it must not be waved through.
    patch = manifest.get("patch")
    if isinstance(patch, dict):
        for source, table in patch.items():
            if isinstance(table, dict):
                yield f"patch.{source}", table

    replace = manifest.get("replace")
    if isinstance(replace, dict):
        yield "replace", replace


def declares_path_dependencies(manifest_text: str):
    """True/False, or None when the manifest could not be parsed.

    None is not a soft False. The caller treats it exactly as it treats True,
    because an unparseable manifest is not evidence of anything.
    """
    try:
        import tomllib
    except ImportError:  # Python older than 3.11.
        return None
    try:
        manifest = tomllib.loads(manifest_text)
    except Exception:
        return None

    for _label, table in _dependency_tables(manifest):
        for spec in table.values():
            # A dependency spec is either a version string ("1.2.3"), which can
            # never be a path, or an inline table that may carry `path`.
            if isinstance(spec, dict) and "path" in spec:
                return True
    return False


def describe(manifest_text: str) -> dict:
    """The predicate plus the evidence behind it, so a caller can audit it."""
    verdict = declares_path_dependencies(manifest_text)
    offenders = []
    tables = []
    try:
        import tomllib

        manifest = tomllib.loads(manifest_text)
    except Exception:
        manifest = None

    if manifest is not None:
        for label, table in _dependency_tables(manifest):
            tables.append(label)
            for name, spec in table.items():
                if isinstance(spec, dict) and "path" in spec:
                    offenders.append(f"{label}.{name}")

    return {
        "schema": SCHEMA,
        "path_dependencies": verdict,
        "dependency_tables_inspected": sorted(tables),
        "path_dependency_names": sorted(offenders),
        "parsed": manifest is not None,
    }


def main(argv) -> int:
    if len(argv) != 2:
        print(f"usage: {argv[0]} <path-to-Cargo.toml>", file=sys.stderr)
        return 2
    try:
        with open(argv[1], "r", encoding="utf-8") as handle:
            text = handle.read()
    except OSError as error:
        print(json.dumps({"schema": SCHEMA, "error": str(error)}), file=sys.stderr)
        return 2
    print(json.dumps(describe(text), sort_keys=True, separators=(",", ":")))
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv))
