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

METADATA_FILE=$(mktemp -t ee-forbidden-deps.XXXXXX.json)
ERR_FILE=$(mktemp -t ee-forbidden-deps.XXXXXX.err)
trap 'rm -f "${METADATA_FILE}" "${ERR_FILE}"' EXIT

if ! cargo metadata --format-version=1 --manifest-path "${MANIFEST}" >"${METADATA_FILE}" 2>"${ERR_FILE}"; then
    echo "error: cargo metadata failed:" >&2
    cat "${ERR_FILE}" >&2
    exit 3
fi

FORBIDDEN_LIST=$(printf '%s\n' "${FORBIDDEN[@]}")

HITS=$(METADATA_FILE="${METADATA_FILE}" FORBIDDEN_LIST="${FORBIDDEN_LIST}" python3 <<'PY'
import json, os

with open(os.environ["METADATA_FILE"], encoding="utf-8") as fh:
    data = json.load(fh)
forbidden = {line.strip() for line in os.environ["FORBIDDEN_LIST"].splitlines() if line.strip()}
hits = sorted({pkg["name"] for pkg in data.get("packages", []) if pkg.get("name") in forbidden})
for name in hits:
    print(name)
PY
)

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
