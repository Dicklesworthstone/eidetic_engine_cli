#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"

if ! command -v cargo >/dev/null 2>&1; then
    echo "error: cargo is required for package artifact leak check" >&2
    exit 1
fi

if ! command -v python3 >/dev/null 2>&1; then
    echo "error: python3 is required for package artifact leak check" >&2
    exit 1
fi

python3 - "$REPO_ROOT/Cargo.toml" <<'PY'
import sys

try:
    import tomllib
except ModuleNotFoundError:
    print("error: python3 tomllib is required to parse Cargo.toml", file=sys.stderr)
    sys.exit(1)

manifest_path = sys.argv[1]
required = {
    "tests/artifacts/*",
    ".beads.snapshot_*",
    ".beads.snapshot_*/*",
    "perf-target/*",
    "perf.data",
    "*.profraw",
    "*.json,bak",
    "tmp/*",
    "temp/*",
    ".tmp/*",
    ".temp/*",
}

with open(manifest_path, "rb") as handle:
    manifest = tomllib.load(handle)

exclude = set(manifest.get("package", {}).get("exclude", []))
missing = sorted(required - exclude)
if missing:
    print("error: Cargo.toml [package].exclude is missing generated-artifact deny patterns:", file=sys.stderr)
    for item in missing:
        print(f"  {item}", file=sys.stderr)
    sys.exit(1)
PY

cd "$REPO_ROOT"
if ! package_list="$(cargo package --list --allow-dirty)"; then
    echo "error: cargo package --list --allow-dirty failed" >&2
    exit 1
fi

forbidden_regex='(^|/)(\.beads\.snapshot_[^/]*|tests/artifacts|perf-target|tmp|temp|\.tmp|\.temp)(/|$)|(^|/)perf\.data$|\.profraw$|\.json,bak$'
violations="$(printf '%s\n' "$package_list" | grep -E "$forbidden_regex" || true)"
if [ -n "$violations" ]; then
    echo "error: cargo package would include generated/local artifact paths:" >&2
    printf '%s\n' "$violations" >&2
    exit 1
fi

package_file_count="$(printf '%s\n' "$package_list" | sed '/^[[:space:]]*$/d' | wc -l | tr -d '[:space:]')"
echo "ok: cargo package file list is free of generated artifact paths (${package_file_count} files)"
