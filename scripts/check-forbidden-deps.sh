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

# bd-iuybr. A forbidden crate can be absent from the graph for two very
# different reasons: nothing wants it, or something wants it and an upper bound
# somewhere is the only thing holding it back. The scan below cannot tell those
# apart -- it reports a clean tree either way -- so the second kind is invisible
# until a routine `cargo update` trips the gate, with no indication of which pin
# had been doing the work.
#
# Each entry here names a cap that is load-bearing: remove it and a forbidden
# crate enters the resolved graph. The check asserts the cap is still declared
# AND that the resolved version honours it, so the cap cannot be deleted, or
# quietly rendered ineffective, without this gate saying which crate it was
# keeping out and why.
#
# The cap is located by searching every dependency table in the manifest rather
# than by a hardcoded table path, so moving it (host vs target-gated, normal vs
# dev) does not silently disable this check -- only deleting it does.
#
# Format: crate | required bound | why it exists
LOAD_BEARING_CAPS=(
    'wasm-bindgen-futures|<0.4.79|0.4.79 adds a tokio dependency; ee reaches this crate through asupersync, which requires only "0.4" (bd-iuybr)'
)

# Assert every load-bearing cap is still declared in the manifest and still
# honoured by the resolved graph. Exits non-zero via its caller on any failure:
# a cap that has been removed is a policy regression, not a clean scan.
check_load_bearing_caps() {
    CAPS_SPEC="$(printf '%s\n' "${LOAD_BEARING_CAPS[@]}")" \
    CAPS_MANIFEST="$1" \
    CAPS_METADATA="$2" \
    python3 -c '
import json
import os
import sys

try:
    import tomllib
except ImportError:
    sys.exit("error: Python 3.11+ with tomllib is required to verify load-bearing caps")


def parse_version(text):
    parts = []
    for chunk in text.split("."):
        digits = ""
        for char in chunk:
            if not char.isdigit():
                break
            digits += char
        parts.append(int(digits) if digits else 0)
    while len(parts) < 3:
        parts.append(0)
    return tuple(parts[:3])


try:
    with open(os.environ["CAPS_MANIFEST"], "rb") as handle:
        manifest = tomllib.load(handle)
except (OSError, UnicodeDecodeError, tomllib.TOMLDecodeError) as error:
    sys.exit(f"error: cannot read manifest for cap verification: {error}")

try:
    with open(os.environ["CAPS_METADATA"], "r", encoding="utf-8") as handle:
        packages = json.load(handle).get("packages") or []
except (OSError, ValueError, UnicodeDecodeError) as error:
    sys.exit(f"error: cannot read metadata for cap verification: {error}")

resolved = {}
for package in packages:
    name = package.get("name")
    version = package.get("version")
    if isinstance(name, str) and isinstance(version, str):
        resolved[name] = version

DEPENDENCY_TABLES = ("dependencies", "dev-dependencies", "build-dependencies")


def requirements_for(crate):
    """Every version requirement the manifest declares on crate, with where."""
    found = []

    def visit(table, where):
        if not isinstance(table, dict) or crate not in table:
            return
        entry = table[crate]
        if isinstance(entry, str):
            found.append((where, entry))
        elif isinstance(entry, dict) and isinstance(entry.get("version"), str):
            found.append((where, entry["version"]))

    for name in DEPENDENCY_TABLES:
        visit(manifest.get(name), f"[{name}]")
    targets = manifest.get("target")
    if isinstance(targets, dict):
        for cfg, tables in targets.items():
            if not isinstance(tables, dict):
                continue
            for name in DEPENDENCY_TABLES:
                visit(tables.get(name), f"[target.{cfg!r}.{name}]")
    return found


failures = []
reported = []
for line in os.environ["CAPS_SPEC"].splitlines():
    if not line.strip():
        continue
    crate, bound, why = line.split("|", 2)

    declarations = requirements_for(crate)
    capped = [(where, req) for where, req in declarations if bound in req.replace(" ", "")]

    if not declarations:
        failures.append(
            f"{crate}: the {bound} cap is GONE -- the manifest no longer declares this crate "
            f"anywhere.\n"
            f"    That cap was the only reason a forbidden crate stayed out of the graph: {why}\n"
            f"    Re-declaring the cap, or confirming upstream dropped the dependency, is the fix."
        )
        continue
    if not capped:
        failures.append(
            f"{crate}: declared as "
            + ", ".join(f"{req!r} in {where}" for where, req in declarations)
            + f", none of which carries the {bound} bound.\n    {why}"
        )
        continue
    where, requirement = capped[0]

    actual = resolved.get(crate)
    if actual is None:
        reported.append(f"  {crate} {requirement} in {where} (not in the resolved graph) -- {why}")
        continue
    limit = bound.lstrip("<=")
    if parse_version(actual) >= parse_version(limit):
        failures.append(
            f"{crate}: cap says {bound} but the graph resolved {actual}, so the cap is declared "
            f"but not effective.\n    {why}"
        )
        continue
    reported.append(f"  {crate} {requirement} in {where} -> resolved {actual} -- {why}")

if failures:
    print("error: a load-bearing version cap is missing or ineffective:", file=sys.stderr)
    for failure in failures:
        print(f"  - {failure}", file=sys.stderr)
    sys.exit(2)

print("ok: load-bearing caps holding forbidden crates out of the graph:")
for line in reported:
    print(line)
'
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

    # bd-iuybr. Controls for the load-bearing cap check. A guard whose failing
    # arm has never been observed is decoration, and this one guards a cap whose
    # removal is silent by construction: the forbidden scan reports a clean tree
    # either way. One passing arm and three distinct failing arms -- cap deleted,
    # cap present but unbounded, cap declared but not honoured by the resolution.
    caps_manifest="${fixture_dir}/caps-ok.toml"
    cat > "${caps_manifest}" <<'CAPFIXTURE'
[package]
name = "fixture"
version = "0.0.0"

[target.'cfg(target_arch = "wasm32")'.dependencies]
wasm-bindgen-futures = { version = ">=0.4, <0.4.79", default-features = false }
CAPFIXTURE
    printf '{"packages":[{"name":"wasm-bindgen-futures","version":"0.4.78"}]}\n' \
        > "${fixture_dir}/caps-ok.json"
    if ! check_load_bearing_caps "${caps_manifest}" "${fixture_dir}/caps-ok.json" \
        > "${fixture_dir}/caps-ok.out" 2>&1; then
        cat "${fixture_dir}/caps-ok.out" >&2
        echo "error: self-test rejected a manifest whose cap is present and honoured" >&2
        exit 2
    fi
    if ! grep -Fq 'wasm-bindgen-futures' "${fixture_dir}/caps-ok.out"; then
        echo "error: cap check passed without naming the capped crate" >&2
        exit 2
    fi
    echo "ok: load-bearing cap accepted when present and honoured"

    expect_cap_failure() {
        local label="$1" expected="$2" manifest="$3" metadata="$4"
        if check_load_bearing_caps "${manifest}" "${metadata}" \
            > "${fixture_dir}/${label}.out" 2>&1; then
            echo "error: self-test accepted a broken cap: ${label}" >&2
            exit 2
        fi
        if ! grep -Fq -- "${expected}" "${fixture_dir}/${label}.out"; then
            cat "${fixture_dir}/${label}.out" >&2
            echo "error: cap self-test ${label} failed for an unexpected reason" >&2
            exit 2
        fi
        echo "ok: rejected cap ${label}"
    }

    cat > "${fixture_dir}/caps-deleted.toml" <<'CAPFIXTURE'
[package]
name = "fixture"
version = "0.0.0"
CAPFIXTURE
    expect_cap_failure cap_deleted 'cap is GONE' \
        "${fixture_dir}/caps-deleted.toml" "${fixture_dir}/caps-ok.json"

    cat > "${fixture_dir}/caps-unbounded.toml" <<'CAPFIXTURE'
[package]
name = "fixture"
version = "0.0.0"

[target.'cfg(target_arch = "wasm32")'.dependencies]
wasm-bindgen-futures = "0.4"
CAPFIXTURE
    expect_cap_failure cap_unbounded 'none of which carries' \
        "${fixture_dir}/caps-unbounded.toml" "${fixture_dir}/caps-ok.json"

    printf '{"packages":[{"name":"wasm-bindgen-futures","version":"0.4.79"}]}\n' \
        > "${fixture_dir}/caps-violated.json"
    expect_cap_failure cap_not_effective 'not effective' \
        "${caps_manifest}" "${fixture_dir}/caps-violated.json"

    # The cap may legitimately move between dependency tables; only deleting it
    # is a regression. If this arm ever fails, the search narrowed by accident.
    cat > "${fixture_dir}/caps-moved.toml" <<'CAPFIXTURE'
[package]
name = "fixture"
version = "0.0.0"

[dependencies]
wasm-bindgen-futures = { version = ">=0.4, <0.4.79" }
CAPFIXTURE
    if ! check_load_bearing_caps "${fixture_dir}/caps-moved.toml" "${fixture_dir}/caps-ok.json" \
        > "${fixture_dir}/caps-moved.out" 2>&1; then
        cat "${fixture_dir}/caps-moved.out" >&2
        echo "error: cap check did not find the cap after it moved tables" >&2
        exit 2
    fi
    echo "ok: cap still found when declared in a different dependency table"

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

report_hits() {
    local label="$1" hits="$2"
    echo "error: forbidden dependencies present in the ${label} resolved tree:" >&2
    while IFS= read -r hit; do
        printf '  - %s\n' "${hit}" >&2
    done <<<"${hits}"
    echo >&2
    echo "Fix: remove the dependency, or quarantine it behind an explicit feature" >&2
    echo "that is disabled by default. See AGENTS.md \`Forbidden Dependencies" >&2
    echo "(Hard Rule, Audited By CI)\` for the canonical list and rationale." >&2
}

# Audit one manifest's resolved graph. Returns 0 clean, 2 on a hit, 3 on an
# infrastructure failure, so a broken toolchain can never be mistaken for a pass.
audit_manifest() {
    local label="$1" manifest="$2" stale_is_fatal="${3:-yes}" metadata hits stderr_file status
    if [[ ! -f "${manifest}" ]]; then
        echo "error: ${label} manifest not found at ${manifest}" >&2
        return 1
    fi
    stderr_file=$(mktemp)
    metadata=$(cargo metadata --locked --format-version=1 --manifest-path "${manifest}" 2>"${stderr_file}")
    status=$?
    if (( status != 0 )); then
        # A lockfile that no longer matches its manifest is a DIFFERENT defect
        # from a forbidden dependency, and conflating them would make this gate
        # red for a reason it is not named for. Distinguish exactly that one
        # signature; anything else is still a hard infrastructure failure, so a
        # broken toolchain can never be mistaken for a pass.
        if [[ "${stale_is_fatal}" == "no" ]] && grep -q 'because --locked was passed' "${stderr_file}"; then
            echo "warning: ${label} lockfile is out of sync with its manifest, so its graph could not be" >&2
            echo "         audited. This is NOT a forbidden-dependency finding." >&2
            echo "         Regenerate it with: (cd fuzz && cargo generate-lockfile)" >&2
            echo "         Tolerated rather than fatal because fuzz depends on the root by path, so ANY" >&2
            echo "         change to the root manifest staleness-marks this lock; making that fatal would" >&2
            echo "         red the gate for dependency edits that have nothing to do with fuzz. The" >&2
            echo "         earlier reason for tolerating it -- that regenerating pulled in tokio via" >&2
            echo "         wasm-bindgen-futures 0.4.79 -- no longer applies: the root now caps that" >&2
            echo "         crate below 0.4.79 (bd-iuybr) and fuzz inherits the cap through the path" >&2
            echo "         dependency, so a regenerated lock resolves 0.4.78 and stays tokio-free." >&2
            rm -f "${stderr_file}"
            return 0
        fi
        cat "${stderr_file}" >&2
        rm -f "${stderr_file}"
        echo "error: cargo metadata failed for ${label}; its lockfile was left unchanged" >&2
        return 3
    fi
    rm -f "${stderr_file}"
    # Keep the resolved graph for the load-bearing cap check, which has to compare
    # the manifest's declared bound against what was actually resolved.
    LAST_AUDIT_METADATA_FILE=$(mktemp)
    printf '%s\n' "${metadata}" > "${LAST_AUDIT_METADATA_FILE}"
    if ! hits=$(printf '%s\n' "${metadata}" | scan_metadata); then
        echo "error: dependency metadata scan failed for ${label}" >&2
        return 3
    fi
    if [[ -n "${hits}" ]]; then
        report_hits "${label}" "${hits}"
        return 2
    fi
    echo "ok: no forbidden dependencies detected in the ${label} resolved tree"
    return 0
}

LAST_AUDIT_METADATA_FILE=""
audit_manifest "root" "${MANIFEST}" || exit $?

# A clean scan above means "no forbidden crate is in the graph", which is exactly
# as true when a cap is holding one out as when nothing wants it. Say which,
# while the tree is still green, instead of leaving the next person to discover
# it from a red gate after their `cargo update`.
if [[ -n "${LAST_AUDIT_METADATA_FILE}" && -f "${LAST_AUDIT_METADATA_FILE}" ]]; then
    caps_status=0
    check_load_bearing_caps "${MANIFEST}" "${LAST_AUDIT_METADATA_FILE}" || caps_status=$?
    rm -f "${LAST_AUDIT_METADATA_FILE}"
    if (( caps_status != 0 )); then
        exit "${caps_status}"
    fi
else
    echo "error: root audit produced no metadata to verify caps against" >&2
    exit 3
fi

# bd-mj18x: fuzz/ is EXCLUDED from the workspace and resolves its own lockfile, so
# the root audit above never saw it. That blind spot is not theoretical -- while
# regenerating fuzz/Cargo.lock on 2026-09-28, free resolution selected
# wasm-bindgen-futures 0.4.79, which depends on tokio, and nothing would have
# reported it. The root escapes only because it pins 0.4.78 (bd-iuybr).
#
# Audited with the same policy and the same scanner as the root: one forbidden
# list, two graphs. A missing fuzz manifest is not an error -- the subproject is
# optional -- but a fuzz manifest that FAILS to resolve is, because a silent skip
# is exactly how this surface went unwatched.
FUZZ_MANIFEST="${REPO_ROOT}/fuzz/Cargo.toml"
if [[ -f "${FUZZ_MANIFEST}" ]]; then
    audit_manifest "fuzz" "${FUZZ_MANIFEST}" "no" || exit $?
else
    echo "note: no fuzz manifest at ${FUZZ_MANIFEST}; nothing to audit there"
fi

echo "checked: ${FORBIDDEN_LIST//$'\n'/ }"
exit 0
