#!/usr/bin/env bash
# EE-012 forbidden-dependency audit (build-independent).
#
# Scans the resolved cargo metadata for forbidden crate names. Unlike the
# Rust integration test in `tests/forbidden_deps.rs`, this script does not
# compile any code, so it produces signal even when an upstream dependency
# is temporarily broken.
#
# Forbidden list comes from AGENTS.md `Forbidden Dependencies (Hard Rule,
# Audited By CI)`. Keep this list in sync with `deny.toml` and
# `tests/forbidden_deps.rs`.
#
# Exit codes:
#   0 — no forbidden crates in resolved tree
#   1 — usage error
#   2 — forbidden crate(s) detected
#   3 — required tool missing (cargo, python3) or metadata fetch failed

set -euo pipefail

FORBIDDEN=(
    tokio
    tokio-util
    async-std
    smol
    rusqlite
    sqlx
    diesel
    sea-orm
    petgraph
    hyper
    axum
    tower
    reqwest
)

usage() {
    echo "usage: $0 [--self-test]" >&2
}

scan_metadata() {
    FORBIDDEN_LIST="${FORBIDDEN_LIST}" python3 -c '
import json
import os
import sys

data = json.load(sys.stdin)
forbidden = {line.strip() for line in os.environ["FORBIDDEN_LIST"].splitlines() if line.strip()}
hits = sorted({pkg["name"] for pkg in data.get("packages", []) if pkg.get("name") in forbidden})
for name in hits:
    print(name)
'
}

FORBIDDEN_LIST=$(printf '%s\n' "${FORBIDDEN[@]}")

if [[ $# -gt 1 ]]; then
    usage
    exit 1
fi

if [[ "${1:-}" == "--self-test" ]]; then
    if ! command -v python3 >/dev/null 2>&1; then
        echo "error: python3 is required but was not found on PATH" >&2
        exit 3
    fi

    synthetic='{"packages":[{"name":"eidetic-engine"},{"name":"tokio"},{"name":"serde"}]}'
    if ! hits=$(printf '%s\n' "${synthetic}" | scan_metadata); then
        echo "error: synthetic forbidden-dependency scan failed" >&2
        exit 3
    fi
    if [[ "${hits}" != "tokio" ]]; then
        echo "error: self-test expected tokio hit, got: ${hits:-<none>}" >&2
        exit 2
    fi

    clean='{"packages":[{"name":"eidetic-engine"},{"name":"serde"}]}'
    if ! hits=$(printf '%s\n' "${clean}" | scan_metadata); then
        echo "error: synthetic clean dependency scan failed" >&2
        exit 3
    fi
    if [[ -n "${hits}" ]]; then
        echo "error: self-test expected clean tree, got: ${hits}" >&2
        exit 2
    fi

    echo "ok: forbidden dependency scanner self-test passed"
    exit 0
elif [[ -n "${1:-}" ]]; then
    usage
    exit 1
fi

if ! command -v cargo >/dev/null 2>&1; then
    echo "error: cargo is required but was not found on PATH" >&2
    exit 3
fi

if ! command -v python3 >/dev/null 2>&1; then
    echo "error: python3 is required but was not found on PATH" >&2
    exit 3
fi

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
MANIFEST="${REPO_ROOT}/Cargo.toml"

if [[ ! -f "${MANIFEST}" ]]; then
    echo "error: manifest not found at ${MANIFEST}" >&2
    exit 1
fi

if ! HITS=$(cargo metadata --format-version=1 --manifest-path "${MANIFEST}" |
    scan_metadata); then
    echo "error: cargo metadata or dependency scan failed" >&2
    exit 3
fi

if [[ -n "${HITS}" ]]; then
    echo "error: forbidden dependencies present in the resolved tree:" >&2
    echo "${HITS}" | sed 's/^/  - /' >&2
    echo >&2
    echo "Fix: remove the dependency, or quarantine it behind an explicit feature" >&2
    echo "that is disabled by default. See AGENTS.md \`Forbidden Dependencies" >&2
    echo "(Hard Rule, Audited By CI)\` for the canonical list and rationale." >&2
    exit 2
fi

echo "ok: no forbidden dependencies detected in the resolved tree"
echo "checked: ${FORBIDDEN[*]}"
exit 0
