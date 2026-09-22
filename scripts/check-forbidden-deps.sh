#!/usr/bin/env bash
# EE-012 forbidden-dependency audit (build-independent).
#
# Scans the lockfile-pinned cargo metadata for forbidden crate names. Unlike
# the Rust integration test in `tests/forbidden_deps.rs`, this script does not
# compile any code, so it produces signal even when an upstream dependency is
# temporarily broken. `--locked` is mandatory: verification must fail closed
# on dependency drift instead of rewriting Cargo.lock.
#
# Read unconditional crate-name bans from deny.toml. The original FORBIDDEN
# list remains a minimum safety floor: policy additions are enforced and
# reported; removing an existing ban is a policy error, never a weaker scan.
# Conditional/version-qualified entries need an explicit implementation, so
# unsupported entry fields fail closed instead of being silently discarded.
#
# Exit codes:
#   0 — no forbidden crates in resolved tree
#   1 — usage error
#   2 — forbidden crate(s) detected
#   3 — missing tool, invalid policy/metadata, or metadata fetch failed

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

MINIMUM_FORBIDDEN_LIST=$(printf '%s\n' "${FORBIDDEN[@]}")

usage() {
    echo "usage: $0 [--self-test]" >&2
}

load_forbidden_list() {
    MINIMUM_FORBIDDEN_LIST="${MINIMUM_FORBIDDEN_LIST}" python3 - "$1" <<'PY'
import os
import re
import sys

try:
    import tomllib
except ImportError:
    sys.exit("error: Python 3.11+ with tomllib is required to read deny.toml")

def fail(message):
    sys.exit(f"error: {sys.argv[1]}: {message}")

try:
    with open(sys.argv[1], "rb") as policy_file:
        policy = tomllib.load(policy_file)
except (OSError, UnicodeDecodeError, tomllib.TOMLDecodeError) as error:
    fail(f"cannot read TOML policy: {error}")

bans = policy.get("bans")
if not isinstance(bans, dict):
    fail("[bans] must be a table")
entries = bans.get("deny")
if not isinstance(entries, list) or not entries:
    fail("[bans].deny must be a non-empty array")

names = []
for index, entry in enumerate(entries):
    if not isinstance(entry, dict) or set(entry) != {"name"}:
        fail(f"[bans].deny[{index}] must contain only name; conditional or unknown fields are unsupported")
    name = entry["name"]
    if not isinstance(name, str) or not re.fullmatch(r"[A-Za-z0-9_-]+", name):
        fail(f"[bans].deny[{index}].name must be a non-empty crate name")
    if name in names:
        fail(f"duplicate ban: {name}")
    names.append(name)

minimum = set(os.environ["MINIMUM_FORBIDDEN_LIST"].splitlines())
missing = sorted(minimum - set(names))
additional = sorted(set(names) - minimum)
if additional:
    print("note: policy bans beyond the protected baseline: " + ", ".join(additional), file=sys.stderr)
if missing:
    fail("policy is missing protected bans: " + ", ".join(missing))

print("\n".join(names))
PY
}

scan_metadata() {
    FORBIDDEN_LIST="${FORBIDDEN_LIST}" python3 -c '
import json
import os
import sys

try:
    data = json.load(sys.stdin)
except (ValueError, UnicodeDecodeError) as error:
    sys.exit(f"error: invalid cargo metadata JSON: {error}")
packages = data.get("packages") if isinstance(data, dict) else None
if not isinstance(packages, list) or not packages:
    sys.exit("error: cargo metadata packages must be a non-empty array")
if any(not isinstance(pkg, dict) or not isinstance(pkg.get("name"), str) or not pkg["name"] for pkg in packages):
    sys.exit("error: every cargo metadata package must have a non-empty name")
forbidden = set(os.environ["FORBIDDEN_LIST"].splitlines())
if not forbidden:
    sys.exit("error: refusing to scan with an empty forbidden list")
hits = sorted({pkg["name"] for pkg in packages if pkg["name"] in forbidden})
for name in hits:
    print(name)
'
}

if [[ $# -gt 1 ]]; then
    usage
    exit 1
fi

if [[ "${1:-}" == "--self-test" ]]; then
    if ! command -v python3 >/dev/null 2>&1; then
        echo "error: python3 is required but was not found on PATH" >&2
        exit 3
    fi

    fixture_dir=$(mktemp -d "${TMPDIR:-/tmp}/ee-forbidden-deps.XXXXXX")
    echo "self-test: fixtures retained at ${fixture_dir}"

    write_policy_fixture() {
        local name
        printf '[bans]\ndeny = [\n'
        for name in "${FORBIDDEN[@]}"; do
            printf '  { name = "%s" },\n' "${name}"
        done
        printf '%s\n]\n' "${1:-}"
    }

    expect_bad_policy() {
        local label="$1" expected="$2" fixture="${fixture_dir}/$1.toml"
        cat > "${fixture}"
        if load_forbidden_list "${fixture}" > "${fixture}.out" 2> "${fixture}.err"; then
            echo "error: self-test accepted invalid policy: ${label}" >&2
            exit 2
        fi
        if ! grep -Fq -- "${expected}" "${fixture}.err"; then
            cat "${fixture}.err" >&2
            echo "error: self-test ${label} failed for an unexpected reason" >&2
            exit 2
        fi
        echo "ok: rejected policy ${label}"
    }

    write_policy_fixture > "${fixture_dir}/base.toml"
    FORBIDDEN_LIST=$(load_forbidden_list "${fixture_dir}/base.toml") || exit 3
    for name in "${FORBIDDEN[@]}"; do
        synthetic=$(printf '{"packages":[{"name":"%s"}]}' "${name}")
        if ! hits=$(printf '%s\n' "${synthetic}" | scan_metadata) || [[ "${hits}" != "${name}" ]]; then
            echo "error: self-test did not catch protected ban: ${name}" >&2
            exit 2
        fi
    done
    echo "ok: all ${#FORBIDDEN[@]} protected bans detected"

    clean='{"packages":[{"name":"eidetic-engine"},{"name":"serde"}]}'
    if ! hits=$(printf '%s\n' "${clean}" | scan_metadata); then
        echo "error: synthetic clean dependency scan failed" >&2
        exit 3
    fi
    if [[ -n "${hits}" ]]; then
        echo "error: self-test expected clean tree, got: ${hits}" >&2
        exit 2
    fi

    write_policy_fixture '  { name = "serde" },' > "${fixture_dir}/added.toml"
    FORBIDDEN_LIST=$(load_forbidden_list "${fixture_dir}/added.toml") || exit 3
    if ! hits=$(printf '%s\n' "${clean}" | scan_metadata) || [[ "${hits}" != "serde" ]]; then
        echo "error: self-test did not enforce the policy-only serde ban" >&2
        exit 2
    fi
    echo "ok: policy-only serde ban detected"

    expect_bad_policy malformed "cannot read TOML policy" <<<'[bans'
    expect_bad_policy missing_table "[bans] must be a table" <<<'[other]'
    expect_bad_policy missing_deny "[bans].deny must be a non-empty array" <<<'[bans]'
    expect_bad_policy wrong_type "[bans].deny must be a non-empty array" <<<$'[bans]\ndeny = "tokio"'
    expect_bad_policy empty "[bans].deny must be a non-empty array" <<<$'[bans]\ndeny = []'
    expect_bad_policy string_entry "must contain only name" <<<$'[bans]\ndeny = ["tokio"]'
    expect_bad_policy missing_name "must contain only name" <<<$'[bans]\ndeny = [{}]'
    expect_bad_policy wrong_name_type "must be a non-empty crate name" <<<$'[bans]\ndeny = [{ name = 1 }]'
    expect_bad_policy blank_name "must be a non-empty crate name" <<<$'[bans]\ndeny = [{ name = "" }]'
    expect_bad_policy conditional "must contain only name" <<<$'[bans]\ndeny = [{ name = "tokio", version = "1" }]'
    expect_bad_policy misspelled "must contain only name" <<<$'[bans]\ndeny = [{ nmae = "tokio" }]'
    expect_bad_policy missing_protected "policy is missing protected bans" <<<$'[bans]\ndeny = [{ name = "serde" }]'
    write_policy_fixture '  { name = "tokio" },' | expect_bad_policy duplicate "duplicate ban: tokio"
    if load_forbidden_list "${fixture_dir}/absent.toml" > "${fixture_dir}/absent.out" 2> "${fixture_dir}/absent.err"; then
        echo "error: self-test accepted a missing policy file" >&2
        exit 2
    fi
    grep -Fq 'cannot read TOML policy' "${fixture_dir}/absent.err" || exit 2
    echo "ok: rejected missing policy file"

    for invalid in 'not json' '[]' '{}' '{"packages":[]}' '{"packages":{}}' '{"packages":[{}]}' '{"packages":[{"name":1}]}'; do
        if printf '%s\n' "${invalid}" | scan_metadata > "${fixture_dir}/metadata.out" 2> "${fixture_dir}/metadata.err"; then
            echo "error: self-test accepted malformed metadata: ${invalid}" >&2
            exit 2
        fi
    done
    echo "ok: malformed metadata rejected"

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
POLICY="${REPO_ROOT}/deny.toml"

if ! FORBIDDEN_LIST=$(load_forbidden_list "${POLICY}"); then
    echo "error: forbidden-dependency policy could not be enforced" >&2
    exit 3
fi

if [[ ! -f "${MANIFEST}" ]]; then
    echo "error: manifest not found at ${MANIFEST}" >&2
    exit 1
fi

if ! METADATA=$(cargo metadata --locked --format-version=1 --manifest-path "${MANIFEST}"); then
    echo "error: cargo metadata failed; Cargo.lock was left unchanged" >&2
    exit 3
fi

if ! HITS=$(printf '%s\n' "${METADATA}" | scan_metadata); then
    echo "error: dependency metadata scan failed" >&2
    exit 3
fi

if [[ -n "${HITS}" ]]; then
    echo "error: forbidden dependencies present in the resolved tree:" >&2
    while IFS= read -r hit; do
        printf '  - %s\n' "${hit}" >&2
    done <<<"${HITS}"
    echo >&2
    echo "Fix: remove the dependency, or quarantine it behind an explicit feature" >&2
    echo "that is disabled by default. See AGENTS.md \`Forbidden Dependencies" >&2
    echo "(Hard Rule, Audited By CI)\` for the canonical list and rationale." >&2
    exit 2
fi

echo "ok: no forbidden dependencies detected in the resolved tree"
echo "checked: ${FORBIDDEN_LIST//$'\n'/ }"
exit 0
