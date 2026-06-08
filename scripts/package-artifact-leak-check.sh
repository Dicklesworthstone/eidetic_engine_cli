#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
SELF_TEST=false

usage() {
    cat <<'USAGE'
package-artifact-leak-check.sh

Fail if generated/local artifact paths would enter the published crate.

Usage:
  scripts/package-artifact-leak-check.sh [--self-test]

Options:
  --self-test  Run synthetic manifest and path classifier tests without Cargo.
USAGE
}

while [ "$#" -gt 0 ]; do
    case "$1" in
        --self-test)
            SELF_TEST=true
            shift
            ;;
        --help|-h)
            usage
            exit 0
            ;;
        *)
            printf 'package-artifact-leak-check: unknown argument: %s\n' "$1" >&2
            usage >&2
            exit 2
            ;;
    esac
done

REQUIRED_EXCLUDES=(
    "tests/artifacts/*"
    ".beads.snapshot_*"
    ".beads.snapshot_*/*"
    "FINAL_AUDIT_REPORT"
    "FINAL_AUDIT_REPORT.*"
    ".audit_log"
    ".audit_log/*"
    "*.audit_log"
    ".*-report.json"
    "perf-target/*"
    "perf.data"
    "*.profraw"
    "*.json,bak"
    "tmp/*"
    "temp/*"
    ".tmp/*"
    ".temp/*"
)

FORBIDDEN_PACKAGE_PATH_REGEX='(^|/)(FINAL_AUDIT_REPORT([^/]*)?|\.audit_log|\.beads\.snapshot_[^/]*|tests/artifacts|perf-target|tmp|temp|\.tmp|\.temp)(/|$)|(^|/)perf\.data$|(^|/)\.[A-Za-z0-9_.-]*-report\.json$|\.profraw$|\.json,bak$|\.audit_log$'

require_tool() {
    if ! command -v "$1" >/dev/null 2>&1; then
        echo "error: $1 is required for package artifact leak check" >&2
        exit 1
    fi
}

check_manifest_excludes() {
    local manifest_path="$1"

    python3 - "$manifest_path" "${REQUIRED_EXCLUDES[@]}" <<'PY'
import sys

try:
    import tomllib
except ModuleNotFoundError:
    print("error: python3 tomllib is required to parse Cargo.toml", file=sys.stderr)
    sys.exit(1)

manifest_path = sys.argv[1]
required = set(sys.argv[2:])

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
}

forbidden_package_paths() {
    local package_list="$1"

    printf '%s\n' "$package_list" | grep -E "$FORBIDDEN_PACKAGE_PATH_REGEX" || true
}

generated_manifest_with_excludes() {
    local skip_pattern="${1:-}"
    local pattern

    printf '[package]\n'
    printf 'name = "package-artifact-leak-self-test"\n'
    printf 'version = "0.0.0"\n'
    printf 'edition = "2024"\n'
    printf 'exclude = [\n'
    for pattern in "${REQUIRED_EXCLUDES[@]}"; do
        [ "$pattern" != "$skip_pattern" ] || continue
        printf '  "%s",\n' "$pattern"
    done
    printf ']\n'
}

assert_forbidden_path() {
    local path="$1"

    if ! printf '%s\n' "$path" | grep -Eq "$FORBIDDEN_PACKAGE_PATH_REGEX"; then
        printf 'package_artifact_leak self-test: expected forbidden path was allowed: %s\n' "$path" >&2
        exit 1
    fi
}

assert_allowed_path() {
    local path="$1"

    if printf '%s\n' "$path" | grep -Eq "$FORBIDDEN_PACKAGE_PATH_REGEX"; then
        printf 'package_artifact_leak self-test: expected allowed path was forbidden: %s\n' "$path" >&2
        exit 1
    fi
}

self_test() {
    local status
    local stderr_output

    check_manifest_excludes <(generated_manifest_with_excludes)

    set +e
    stderr_output="$(check_manifest_excludes <(generated_manifest_with_excludes ".audit_log") 2>&1)"
    status=$?
    set -e
    if [ "$status" -eq 0 ]; then
        printf 'package_artifact_leak self-test: manifest missing .audit_log should fail\n' >&2
        exit 1
    fi
    if ! printf '%s\n' "$stderr_output" | grep -Fq ".audit_log"; then
        printf 'package_artifact_leak self-test: missing exclude error did not name .audit_log\n' >&2
        exit 1
    fi

    for path in \
        "FINAL_AUDIT_REPORT" \
        "FINAL_AUDIT_REPORT.md" \
        ".audit_log" \
        ".audit_log/events.jsonl" \
        "logs/session.audit_log" \
        ".beads.snapshot_20260608/issues.jsonl" \
        "tests/artifacts/e2e.json" \
        "perf-target/report.json" \
        "tmp/file" \
        "temp/file" \
        ".tmp/file" \
        ".temp/file" \
        "perf.data" \
        ".contract-drift-radar-report.json" \
        "trace.profraw" \
        "sample.json,bak"
    do
        assert_forbidden_path "$path"
    done

    for path in \
        "Cargo.toml" \
        "src/main.rs" \
        "docs/report.json" \
        "src/tmpfile.rs" \
        "templates/file" \
        "performance/data.json" \
        "tests/artifact_notes.md" \
        "reports/contract-drift-radar-report.json"
    do
        assert_allowed_path "$path"
    done

    local violations
    violations="$(forbidden_package_paths "$(printf '%s\n' Cargo.toml .package-report.json src/main.rs)")"
    if [ "$violations" != ".package-report.json" ]; then
        printf 'package_artifact_leak self-test: classifier returned unexpected violations: %s\n' "$violations" >&2
        exit 1
    fi

    printf 'package artifact leak self-test passed\n'
}

require_tool python3

if [ "$SELF_TEST" = "true" ]; then
    self_test
    exit 0
fi

require_tool cargo

check_manifest_excludes "$REPO_ROOT/Cargo.toml"

cd "$REPO_ROOT"
if ! package_list="$(cargo package --list --allow-dirty)"; then
    echo "error: cargo package --list --allow-dirty failed" >&2
    exit 1
fi

violations="$(forbidden_package_paths "$package_list")"
if [ -n "$violations" ]; then
    echo "error: cargo package would include generated/local artifact paths:" >&2
    printf '%s\n' "$violations" >&2
    exit 1
fi

package_file_count="$(printf '%s\n' "$package_list" | sed '/^[[:space:]]*$/d' | wc -l | tr -d '[:space:]')"
echo "ok: cargo package file list is free of generated artifact paths (${package_file_count} files)"
