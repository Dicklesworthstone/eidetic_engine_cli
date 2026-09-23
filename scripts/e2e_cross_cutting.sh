#!/usr/bin/env bash
# bd-1n0np.23.6 - static E2E coverage for dueling-wizards foundations.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

export EE_E2E_KEEP="${EE_E2E_KEEP:-1}"
export EE_E2E_KEEP_ARTIFACTS="${EE_E2E_KEEP_ARTIFACTS:-1}"

# shellcheck source=scripts/e2e_lib.sh
# shellcheck disable=SC1091
source "$REPO_ROOT/scripts/e2e_lib.sh"

MIGRATION_MANIFEST="$REPO_ROOT/tests/fixtures/contracts/dueling_wizards_migration_registry.json"
BACKUP_MANIFEST="$REPO_ROOT/tests/fixtures/contracts/dueling_wizards_backup_coverage.json"
DETERMINISM_MANIFEST="$REPO_ROOT/tests/fixtures/contracts/dueling_wizards_determinism_gate.json"
INGESTION_MANIFEST="$REPO_ROOT/tests/fixtures/contracts/dueling_wizards_ingestion_security.json"
MESH_MANIFEST="$REPO_ROOT/tests/fixtures/contracts/dueling_wizards_mesh_redaction.json"
WHY_PACKDNA_MANIFEST="$REPO_ROOT/tests/fixtures/contracts/dueling_wizards_why_packdna_signals.json"
OBSERVABILITY_MANIFEST="$REPO_ROOT/tests/fixtures/contracts/dueling_wizards_observability_no_silent_cap.json"

require_tool() {
    local tool="${1:?tool required}"
    if command -v "$tool" >/dev/null 2>&1; then
        _harness_pass "required tool available: $tool"
    else
        _harness_fail "required tool missing: $tool"
    fi
}

run_static_command() {
    local label="${1:?label required}"
    shift
    local exit_code
    set +e
    e2e_log_command "$@" >/dev/null
    exit_code=$?
    set -e
    if [ "$exit_code" -eq 0 ]; then
        _harness_pass "$label"
    else
        _harness_fail "$label exit $exit_code"
    fi
}

run_static_capture() {
    local __out_var="${1:?output variable required}"
    local __status_var="${2:?status variable required}"
    shift 2
    local output
    local exit_code
    set +e
    output="$(e2e_log_command "$@")"
    exit_code=$?
    set -e
    printf -v "$__out_var" '%s' "$output"
    printf -v "$__status_var" '%s' "$exit_code"
}

assert_file_exists() {
    local path="${1:?path required}"
    local label="${2:?label required}"
    if [ -f "$path" ]; then
        e2e_log_assert_eq "present" "present" "$label"
        _harness_pass "$label"
    else
        e2e_log_assert_eq "missing" "present" "$label"
        _harness_fail "$label: missing $path"
    fi
}

assert_jq_file() {
    local path="${1:?path required}"
    local filter="${2:?jq filter required}"
    local label="${3:?label required}"
    if jq -e "$filter" "$path" >/dev/null; then
        e2e_log_assert_eq "true" "true" "$label"
        _harness_pass "$label"
    else
        e2e_log_assert_eq "false" "true" "$label"
        _harness_fail "$label"
    fi
}

assert_jq_file_argjson() {
    local path="${1:?path required}"
    local arg_name="${2:?arg name required}"
    local arg_value="${3:?arg value required}"
    local filter="${4:?jq filter required}"
    local label="${5:?label required}"
    if jq -e --argjson "$arg_name" "$arg_value" "$filter" "$path" >/dev/null; then
        e2e_log_assert_eq "true" "true" "$label"
        _harness_pass "$label"
    else
        e2e_log_assert_eq "false" "true" "$label"
        _harness_fail "$label"
    fi
}

harness_init "cross_cutting"
require_tool jq
require_tool python3

step "neural-local default docs contract is wired"
run_static_command \
    "neural-local default docs contract passes" \
    env EE_BIN="$EE_BIN" "$REPO_ROOT/scripts/e2e_neural_default_docs_contract.sh"

step "cross-cutting manifests parse"
for manifest in \
    "$MIGRATION_MANIFEST" \
    "$BACKUP_MANIFEST" \
    "$DETERMINISM_MANIFEST" \
    "$INGESTION_MANIFEST" \
    "$MESH_MANIFEST" \
    "$WHY_PACKDNA_MANIFEST" \
    "$OBSERVABILITY_MANIFEST"
do
    assert_file_exists "$manifest" "manifest exists: ${manifest#"$REPO_ROOT"/}"
    run_static_command "jq parses ${manifest#"$REPO_ROOT"/}" jq empty "$manifest"
done

step "migration registry anchors downstream cross-cutting gates"
assert_jq_file "$MIGRATION_MANIFEST" \
    '.schema == "ee.dueling_wizards.migration_registry.v1" and .gateBead == "bd-1n0np.23.1"' \
    "migration registry identity"
assert_jq_file "$MIGRATION_MANIFEST" \
    '.sourceOfTruth == "src/db/mod.rs::MIGRATIONS" and .boundaryMigrationE2e == "scripts/e2e_boundary_migration.sh" and .policy.ordering == "strictly_contiguous" and .policy.idempotency == "required" and .policy.rollbackPosture == "forward_only_reversible_where_safe"' \
    "migration registry policy anchors runtime migration sequencing"
assert_jq_file "$MIGRATION_MANIFEST" \
    '.backupCoverageBead == "bd-1n0np.23.2" and (.allocations | length) >= 11' \
    "migration registry declares backup coverage owner and allocations"
assert_jq_file "$MIGRATION_MANIFEST" \
    'all(.allocations[]; (.backupAssetKind | type == "string") and (.ownerBead | startswith("bd-1n0np.")))' \
    "migration allocations have backup asset kinds and owner beads"
assert_jq_file "$MIGRATION_MANIFEST" \
    'all(.transitionMatrix[]; .proofPosture == "rch_only_no_local_fallback")' \
    "migration transition matrix keeps RCH-only proof posture"

step "migration registry stays co-committed with the compiled MIGRATIONS tail"
# bd-zs76e. The registry's own policy.runtimeUpdateRule says "Update this
# registry in the same change that adds a compiled migration", but nothing
# enforced it at write time: the contracts test catches the drift only
# afterwards, and only when its module actually reports -- bv15 timed out with
# 65 of 199 modules silent. The drift then recurred within an hour of being
# fixed, when V124 shipped against a registry just reconciled to V123. These
# two checks fail the same run instead, and need no cargo.
migration_compiled_tail="$(
    awk '
        /pub const MIGRATIONS/ { inside = 1 }
        inside && /\];/ { exit }
        inside && /^[[:space:]]*V[0-9][0-9][0-9]_/ {
            entry = $0
            sub(/^[[:space:]]*V/, "", entry)
            sub(/_.*$/, "", entry)
            last = entry
        }
        END { print last + 0 }
    ' "$REPO_ROOT/src/db/mod.rs"
)"
migration_registry_tail="$(jq -r '.currentLastCompiledMigration' "$MIGRATION_MANIFEST")"
e2e_log_assert_eq "$migration_registry_tail" "$migration_compiled_tail" \
    "migration registry tail matches compiled MIGRATIONS tail"
if [ "$migration_registry_tail" = "$migration_compiled_tail" ]; then
    _harness_pass "migration registry tail matches compiled MIGRATIONS tail"
else
    _harness_fail "migration registry tail matches compiled MIGRATIONS tail: registry V${migration_registry_tail} vs compiled V${migration_compiled_tail}; update ${MIGRATION_MANIFEST#"$REPO_ROOT"/} in the same change that adds a migration"
fi

assert_jq_file_argjson "$MIGRATION_MANIFEST" \
    tail "$migration_compiled_tail" \
    '[.allocations[] | select(.status == "planned") | .version] | min > $tail' \
    "migration registry reservations stay ahead of the compiled tail"

step "the embedding fingerprint field set matches its schema version"
# bd-7hsgy. `descriptor_content_hash` (src/core/index.rs) hashes a fixed field
# set into the embedding-registry fingerprint, and that hash is compared against
# values ALREADY PERSISTED in each workspace's registry
# (`content_hash.eq_ignore_ascii_case(&expected_hash)` and the hash_matches
# path). Adding, removing or reordering a hashed field changes every fingerprint
# the code computes while the stored ones stay put, so every existing workspace
# reads as mismatched. That is not a failed check; it is a silent invalidation
# of installed state.
#
# The field set already carries a version -- EMBEDDING_REGISTRY_FINGERPRINT_SCHEMA
# -- so the enforceable rule is that the two move together. This check pins both
# and fails if either drifts without the other.
#
# It reads src/core/index.rs rather than editing it: that file is actively owned
# elsewhere, and a static check needs no stake in it.
fingerprint_drift="$(
    python3 - <<'PYEOF'
import io, re

EXPECTED_SCHEMA = "ee.embedding_registry_fingerprint.v1"
EXPECTED_FIELDS = [
    "schema", "provider", "model_id", "model_name", "dimension", "category",
    "semantic", "ready",
    "manifest_id", "manifest_version", "manifest_repo", "manifest_revision",
    "manifest_license", "manifest_dimension", "manifest_file_name",
    "manifest_file_sha256", "manifest_file_size",
]

problems = []
text = io.open("src/core/index.rs", encoding="utf-8").read()

schema = re.search(r'EMBEDDING_REGISTRY_FINGERPRINT_SCHEMA:\s*&str\s*=\s*"([^"]+)"', text)
if not schema:
    problems.append("EMBEDDING_REGISTRY_FINGERPRINT_SCHEMA not found")
elif schema.group(1) != EXPECTED_SCHEMA:
    problems.append(f"schema changed: {EXPECTED_SCHEMA} -> {schema.group(1)}")

start = text.find("fn descriptor_content_hash(")
if start < 0:
    problems.append("descriptor_content_hash not found")
else:
    body = text[start:text.index("\n}\n", start)]
    fields = re.findall(r'hash_fingerprint_field\(\s*&mut hasher,\s*"([^"]+)"', body)
    if fields != EXPECTED_FIELDS:
        added = [f for f in fields if f not in EXPECTED_FIELDS]
        removed = [f for f in EXPECTED_FIELDS if f not in fields]
        if added:
            problems.append(f"fields added: {added}")
        if removed:
            problems.append(f"fields removed: {removed}")
        if not added and not removed:
            problems.append("field ORDER changed; the hash is order-dependent")

# Drift is only a defect when the version did NOT move with it.
if problems and any(p.startswith("schema changed") for p in problems) and len(problems) > 1:
    problems = []

print("; ".join(problems))
PYEOF
)"
if [ -z "$fingerprint_drift" ]; then
    e2e_log_assert_eq "0" "0" "embedding fingerprint field set matches its schema version"
    _harness_pass "embedding fingerprint field set matches its schema version"
else
    e2e_log_assert_eq "1" "0" "embedding fingerprint field set matches its schema version"
    _harness_fail "embedding fingerprint field set matches its schema version: ${fingerprint_drift}. Every persisted registry fingerprint was computed from the pinned field set; changing it invalidates installed workspaces. Bump EMBEDDING_REGISTRY_FINGERPRINT_SCHEMA in the same change and update this guard's expectation."
fi

step "the derived mesh node id never becomes a responder principal"
# bd-mesh-no-stable-node-identity-pt7k5. `build_peer_origin_node_id`
# (src/mesh/peer.rs) derives a node id from the Tailscale node key. Its own doc
# says it "is never an admissible responder principal", because a DERIVED
# principal is guessable by anyone who can see the peer's public node key --
# production mints a RANDOM one via `generate_peer_origin_node_id`
# (getrandom::fill) instead.
#
# That invariant lived only in a doc comment. The function is `pub`, currently
# has no production caller, and nothing stopped enrollment from being wired to
# it -- which would silently make every peer's principal derivable from a public
# value. This is the check that says so out loud.
#
# Tests may call it freely: it exists for artifact compatibility, and two test
# files construct legacy ids with it. Only src/ is constrained, and the budget
# is ONE reference -- the definition itself.
derived_principal_refs="$(rg -o 'build_peer_origin_node_id' src/ 2>/dev/null | wc -l | tr -d ' ')"
# ANCHOR: the check is meaningless if the subject is gone. At zero references the
# function has been renamed or deleted and this guard would report PASS forever
# while nothing constrained the invariant -- a check that cannot fail. Require
# the definition to exist, so a rename fails here and whoever renamed it updates
# the guard deliberately.
derived_principal_defined="$(rg -c '^pub fn build_peer_origin_node_id' src/mesh/peer.rs 2>/dev/null || printf '0')"
if [ "${derived_principal_defined:-0}" -eq 0 ]; then
    e2e_log_assert_eq "0" "1" "derived mesh node id has no production caller"
    _harness_fail "derived mesh node id has no production caller: build_peer_origin_node_id is no longer defined in src/mesh/peer.rs, so this guard has nothing to constrain. If it was renamed, point the guard at the new name; if it was deleted, delete the guard with it."
elif [ "${derived_principal_refs:-0}" -eq 1 ]; then
    e2e_log_assert_eq "$derived_principal_refs" "1" \
        "derived mesh node id has no production caller"
    _harness_pass "derived mesh node id has no production caller"
else
    e2e_log_assert_eq "$derived_principal_refs" "1" \
        "derived mesh node id has no production caller"
    _harness_fail "derived mesh node id has no production caller: build_peer_origin_node_id now has ${derived_principal_refs} references under src/ (expected 1, the definition). A derived principal is guessable from the peer's public node key; mint with generate_peer_origin_node_id instead -- $(rg -n 'build_peer_origin_node_id' src/ | tr '\n' ' ')"
fi

step "no integration shard carries a property-test load"
# bd-in3xj. The shards are split by FILENAME, and every tests/property_*.rs
# sorts into the N-R range, so the split had put 98 of the suite's 105 property
# functions -- 21,832 proptest cases -- into integration_n_r alongside 771
# ordinary tests. n_r could not reach its "test result:" line, so NONE of its
# tests could be graded, including its 38 failures. 7a5ce6d30 moved them to
# integration_property; this stops the next property_*.rs from landing back in
# n_r and re-breaking it.
#
# The limit is a LOAD threshold, not a ban on the macro. Four small proptest
# blocks live legitimately outside the property shard (recall_cli_golden,
# field_selector_unit, journal_capture_property, swarm_slo_replay_parser_proptest),
# together about 320 cases with no shard over 96. A ban on presence would fail
# all four; 1000 leaves them ten times over while catching any real property
# file, which carries hundreds of cases per module.
shard_property_load="$(
    python3 - <<'PYEOF'
import io, os, re, glob

LIMIT = 1000
BASE = "tests/suites"
offenders = []
for suite in sorted(glob.glob("tests/suites/integration_*.rs")):
    name = os.path.basename(suite)
    if name == "integration_property.rs":
        continue
    cases = 0
    registration = io.open(suite, encoding="utf-8").read()
    for rel in re.findall(r'#\[path\s*=\s*"([^"]+)"\]', registration):
        path = os.path.normpath(os.path.join(BASE, rel))
        if not os.path.exists(path):
            continue
        text = io.open(path, encoding="utf-8", errors="replace").read()
        helper = re.search(r"fn\s+config\s*\(\)\s*->\s*ProptestConfig\s*\{[^}]*with_cases\((\d+)\)", text)
        helper_cases = int(helper.group(1)) if helper else None
        for block in re.finditer(r"\bproptest!\s*\{", text):
            start = block.end() - 1
            depth = 0
            for index in range(start, len(text)):
                if text[index] == "{":
                    depth += 1
                elif text[index] == "}":
                    depth -= 1
                    if depth == 0:
                        break
            body = text[start:index]
            functions = len(re.findall(r"\bfn\s+\w+\s*\(", body))
            per_case = 256
            inline = re.search(r"proptest_config\(\s*ProptestConfig::with_cases\((\d+)\)", body)
            struct = re.search(r"ProptestConfig\s*\{[^}]*cases\s*:\s*(\d+)", body)
            via_fn = re.search(r"proptest_config\(\s*config\(\)\s*\)", body)
            if inline:
                per_case = int(inline.group(1))
            elif struct:
                per_case = int(struct.group(1))
            elif via_fn and helper_cases:
                per_case = helper_cases
            cases += functions * per_case
    if cases > LIMIT:
        offenders.append(f"{name} ({cases} proptest cases, limit {LIMIT})")
print("\n".join(offenders))
PYEOF
)"
# ANCHOR: the scan globs tests/suites/integration_*.rs. If that stops matching --
# a rename, a move, a restructure -- it inspects nothing and reports clean. Six
# shards exist; a floor of four catches the discovery breaking.
shard_population="$(ls tests/suites/integration_*.rs 2>/dev/null | wc -l | tr -d ' ')"
if [ "${shard_population:-0}" -lt 4 ]; then
    e2e_log_assert_eq "$shard_population" ">=4" "integration shards carry no property-test load"
    _harness_fail "integration shards carry no property-test load: only ${shard_population} tests/suites/integration_*.rs files found, so the scan has no population and cannot fail. The suites were probably renamed or moved; repoint the guard."
elif [ -z "$shard_property_load" ]; then
    e2e_log_assert_eq "0" "0" "integration shards carry no property-test load"
    _harness_pass "integration shards carry no property-test load"
else
    e2e_log_assert_eq "$(printf '%s\n' "$shard_property_load" | wc -l | tr -d ' ')" "0" \
        "integration shards carry no property-test load"
    _harness_fail "integration shards carry no property-test load: move these modules to tests/suites/integration_property.rs -- $(printf '%s' "$shard_property_load" | tr '\n' ' ')"
fi

step "path redactors keep one shared prefix set"
# bd-redactor-prefix-divergence-lsy52. Twenty-one redactors each carried their
# own hand-copied path-prefix list. They diverged into three incompatible
# families: /root/ reached exactly one of the twenty-one, so a provenance URI
# naming /root/.ssh/id_rsa was redacted on one surface and published verbatim by
# twenty. Every one of the twenty-one had omissions that were live leaks.
#
# The lists are now consolidated into crate::util::SENSITIVE_PATH_PREFIXES, but
# nothing stopped a twenty-second private list appearing tomorrow -- which is
# exactly how bd-zs76e recurred within an hour of being fixed. This is the guard
# that fails the same run instead.
redactor_private_lists="$(
    python3 - <<'PYEOF'
import io, re, glob, sys

# This scan used to require the private list be spelled `&[&str] = &[...]`.
# That was a hole in this very guard, and a real list walked through it: before
# 93deb9193, src/search/mod.rs carried a SIXTEEN-entry private prefix list as a
# `matches!` arm (is_case_insensitive_macos_search_path_prefix). This guard
# passed it, and that list is what silently unhooked case-insensitive matching
# for fourteen roots when its caller switched to the shared set. A guard keyed
# on how a list is SPELLED cannot see a list spelled another way, so the scan
# now keys on the shape of the DATA instead: path prefixes clustered together.
TEST_MOD = re.compile(
    r'#\[cfg\(test\)\]\s*'
    r'(?:(?://[^\n]*|#\[[^\]]*\])\s*)*'
    r'mod\s+\w+\s*\{'
)


def strip_test_modules(text):
    # Fixtures legitimately enumerate many paths: the bd-89312 mixed-case
    # regression test lists sixteen. Only production code is scanned. The
    # comment/attribute tolerance above is load-bearing -- src/search/mod.rs
    # puts both between `#[cfg(test)]` and `mod tests {`.
    out, i = [], 0
    while True:
        m = TEST_MOD.search(text, i)
        if not m:
            out.append(text[i:])
            break
        out.append(text[i:m.start()])
        depth = 0
        for j in range(m.end() - 1, len(text)):
            if text[j] == '{':
                depth += 1
            elif text[j] == '}':
                depth -= 1
                if depth == 0:
                    break
        i = j + 1
    return "".join(out)


# A sensitive filesystem prefix ends at a directory boundary ("/Users/",
# "/private/etc/ssh/") or is a Windows drive root. A JSON pointer
# ("/rch/commandHash", "/request/query") does not, which is what keeps this off
# the pointer tables in swarm_brief.rs, why.rs, recorder.rs and output/mod.rs.
PATHLIT = re.compile(r'"(/(?:[A-Za-z_][\w.-]*/)+|[A-Za-z]:[\\/][^"\n]*)"')

offenders = []
for path in sorted(glob.glob("src/**/*.rs", recursive=True)):
    if path.endswith("util/mod.rs"):
        continue
    text = io.open(path, encoding="utf-8", errors="replace").read()
    if "REDACTED_PATH" not in text:
        continue
    production = strip_test_modules(text)
    lines = [production[:m.start()].count("\n") + 1 for m in PATHLIT.finditer(production)]
    for start in range(len(lines)):
        window = [line for line in lines[start:] if line - lines[start] <= 20]
        if len(window) >= 3:
            offenders.append(f"{path} ({len(window)} clustered path prefixes)")
            break
sys.stdout.write("\n".join(offenders))
PYEOF
)"
# ANCHOR: if nothing emits [REDACTED_PATH] the scan has no population and would
# report clean forever. Twenty-one redactors used it when this guard was written;
# a floor of ten catches a placeholder rename without being brittle.
redactor_population="$(rg -l 'REDACTED_PATH' src/ 2>/dev/null | wc -l | tr -d ' ')"
if [ "${redactor_population:-0}" -lt 10 ]; then
    e2e_log_assert_eq "$redactor_population" ">=10" "path redactors share one prefix set"
    _harness_fail "path redactors share one prefix set: only ${redactor_population} files under src/ mention REDACTED_PATH, so this scan has lost its population and cannot fail. The placeholder was probably renamed; repoint the guard."
elif [ -z "$redactor_private_lists" ]; then
    e2e_log_assert_eq "0" "0" "path redactors share one prefix set"
    _harness_pass "path redactors share one prefix set"
else
    e2e_log_assert_eq "$(printf '%s\n' "$redactor_private_lists" | wc -l | tr -d ' ')" "0" \
        "path redactors share one prefix set"
    _harness_fail "path redactors share one prefix set: these files define a private path-prefix list instead of using crate::util::SENSITIVE_PATH_PREFIXES -- $(printf '%s' "$redactor_private_lists" | tr '\n' ' ')"
fi
assert_jq_file "$MIGRATION_MANIFEST" \
    '.currentLastCompiledMigration == 125 and .nextPlannedMigration == 126 and (.policy.nonInitiativeCompiledMigrations.versions.V100 | startswith("V100_PACK_EVIDENCE_ITEMS")) and (.policy.nonInitiativeCompiledMigrations.versions.V101 | startswith("V101_ATTEMPT_FAMILY_IMMUTABILITY_REPAIR")) and (.policy.nonInitiativeCompiledMigrations.versions.V121 | startswith("V121_EVIDENCE_FEEDBACK_TARGETS")) and (.policy.nonInitiativeCompiledMigrations.versions.V122 | startswith("V122_TYPED_PACK_ITEM_IDENTITY")) and (.policy.nonInitiativeCompiledMigrations.versions.V123 | startswith("V123_MEMORY_SUPERSEDED_AT")) and (.policy.nonInitiativeCompiledMigrations.versions.V124 | startswith("V124_TIMESTAMP_SPELLING_REPAIR")) and (.policy.nonInitiativeCompiledMigrations.versions.V125 | startswith("V125_SUPERSESSION_REDERIVE"))' \
    "migration registry pins compiled tail and next planned migration"
assert_jq_file "$MIGRATION_MANIFEST" \
    '([.transitionMatrix[].version] | sort) == [66,67,68,69,69,70,71,72,126,127,128] and ([.transitionMatrix[].id] | unique | length) == (.transitionMatrix | length) and ([.transitionMatrix[] | select(.status == "planned") | .version] | unique | length) == ([.transitionMatrix[] | select(.status == "planned")] | length)' \
    "migration transition versions match the implemented/planned layout with unique ids and planned slots"
assert_jq_file "$MIGRATION_MANIFEST" \
    '([.transitionMatrix[] | {id, version, status}] | sort_by(.id)) == ([.allocations[] | {id, version, status}] | sort_by(.id))' \
    "migration allocations mirror transition ids, versions, and statuses"
# shellcheck disable=SC2016
assert_jq_file "$MIGRATION_MANIFEST" \
    '.currentLastCompiledMigration as $tail | .nextPlannedMigration as $next | all(.transitionMatrix[]; if .status == "implemented" then (.version <= $tail and .runtimeRule == "compiled_migration_present" and (.migrationConstant | test("^V[0-9]{3}_[A-Z0-9_]+$")) and .boundaryMigrationEvidence == "required_and_current" and .backupCoverageEvidence == "required_and_current") elif .status == "planned" then (.version >= $next and .runtimeRule == "planned_allocation_only" and .migrationConstant == "required_before_implemented" and .boundaryMigrationEvidence == "required_before_implemented" and .backupCoverageEvidence == "required_before_implemented") else false end)' \
    "migration transition status controls implemented vs planned evidence"
assert_jq_file "$MIGRATION_MANIFEST" \
    'all(.allocations[]; (.migrationName | test("^V[0-9]{3}_[A-Z0-9_]+$")) and (.tables | length > 0) and ((.idempotency // "") | length > 0) and ([.reversibleClass] | inside(["reversible_where_safe","forward_only"])))' \
    "migration allocations name tables, migration constants, reversibility, and idempotency"
# shellcheck disable=SC2016
assert_jq_file "$MIGRATION_MANIFEST" \
    '(.allocations[] | select(.id == "memory_anchors") | .plannedShape) as $shape | ($shape.anchorValueStorage == "hash_required_raw_value_forbidden" and $shape.meshExport == "redacted_or_hashed_values_only" and $shape.freshnessMutation == "rank_down_only_no_tombstone" and $shape.writePosture == "append_or_upsert_by_generation" and (($shape.columns | sort) == ["anchor_kind","anchor_value_hash","captured_span_hash","confidence","created_at","freshness_state","generation","memory_id","provenance","redacted_anchor_value","source","updated_at"]) and (($shape.indexes | sort) == ["anchor_kind_value_hash_lookup","freshness_state_generation_lookup","memory_id_anchor_kind_value_hash_unique"]))' \
    "migration memory-anchor shape forbids raw anchor values and pins indexes"

step "backup coverage mirrors migration allocation asset kinds"
# shellcheck disable=SC2016
run_static_command \
    "backup coverage asset set mirrors migration registry" \
    jq -e -n \
    --slurpfile registry "$MIGRATION_MANIFEST" \
    --slurpfile backup "$BACKUP_MANIFEST" \
    '($registry[0].allocations | map(.backupAssetKind) | sort) as $expected
     | ($backup[0].assets | map(.assetKind) | sort) as $actual
     | $expected == $actual'
# shellcheck disable=SC2016
run_static_command \
    "backup assets mirror migration allocation ids and owner beads" \
    jq -e -n \
    --slurpfile registry "$MIGRATION_MANIFEST" \
    --slurpfile backup "$BACKUP_MANIFEST" \
    'all($registry[0].allocations[];
       . as $allocation
       | any($backup[0].assets[];
           .assetKind == $allocation.backupAssetKind
           and (.migrationAllocationIds | index($allocation.id))
           and (.ownerBeads | index($allocation.ownerBead))
           and .hashPolicy == "blake3_required"
           and .missingAssetFailure == "degraded_not_silent_loss"))'
assert_jq_file "$BACKUP_MANIFEST" \
    '.schema == "ee.dueling_wizards.backup_coverage.v1" and .gateBead == "bd-1n0np.23.2"' \
    "backup coverage identity"
assert_jq_file "$BACKUP_MANIFEST" \
    '.policy.missingAssetFailure == "degraded_not_silent_loss" and .policy.hashPolicy == "blake3_required"' \
    "backup coverage fail-visible hash policy"
assert_jq_file "$BACKUP_MANIFEST" \
    '.coverageSurfaces == ["backup_create","backup_inspect","backup_verify","backup_restore","manifest_rehash","roundtrip_e2e"] and all(.assets[]; .hashPolicy == "blake3_required" and .missingAssetFailure == "degraded_not_silent_loss" and .coverageSurfaces == ["backup_create","backup_inspect","backup_verify","backup_restore","manifest_rehash","roundtrip_e2e"] and ((.roundTripEvidence // "") | length > 0))' \
    "backup assets declare full fail-visible coverage surfaces"
# complianceStatus has two legal values (bd-nwyir). Conformance needs declared
# runtime round-trip evidence. A row may admit it is not conformant only while
# its round-trip evidence is still planned, and it must name the bead the
# evidence is pending on.
assert_jq_file "$BACKUP_MANIFEST" \
    'all(.assetCoverageMatrix[]; (.complianceStatus == "declared_conformant" and .roundTripEvidenceStatus == "runtime_evidence_declared" and .scoreMilli >= 950 and .divergent == 0) or (.complianceStatus == "not_conformant_evidence_pending" and .roundTripEvidenceStatus == "planned_contract_only" and ((.evidencePendingOn // "") | startswith("bd-"))))' \
    "backup coverage matrix claims are consistent with their evidence"
# shellcheck disable=SC2016
assert_jq_file "$BACKUP_MANIFEST" \
    '(.assets[] | select(.assetKind == "memory_anchors") | .privacyContract) as $p | $p.rawAnchorValuesAllowed == false and $p.valueMaterialPolicy == "hash_or_redacted_only" and $p.manifestRedactionClass == "hash" and $p.restoreValidation == "hashes_roundtrip_without_raw_values" and (($p.forbiddenFields | sort) == ["anchor_value","raw_anchor_value","raw_command","raw_path","raw_schema","raw_symbol"]) and ($p.serializedFields | index("anchor_value") == null) and ($p.serializedFields | index("raw_anchor_value") == null) and ($p.serializedFields | index("raw_path") == null)' \
    "backup memory-anchor privacy forbids raw anchor values"
assert_jq_file "$BACKUP_MANIFEST" \
    '([.failureScenarios[].scenario] | sort) == ["corrupt_derived_asset_hash","missing_derived_asset","raw_anchor_value_present","restore_manifest_rehash_mismatch"] and all(.failureScenarios[]; .expectedFailure == "degraded_not_silent_loss" and .hashPolicy == "blake3_required" and ((.roundTripEvidence // "") | length > 0))' \
    "backup failure scenarios keep required ids and fail-visible posture"
# shellcheck disable=SC2016
assert_jq_file "$BACKUP_MANIFEST" \
    '(.runtimeAnchors) as $anchors | all(.failureScenarios[]; .expectedRuntimeAnchor as $anchor | $anchors | index($anchor))' \
    "backup failure scenarios name runtime anchors"
run_static_command \
    "backup runtime source exposes derived-asset missing anchor" \
    grep -q "derived_asset_missing" "$REPO_ROOT/src/core/backup.rs"
run_static_command \
    "backup runtime source exposes restored derived anchor" \
    grep -q "restoredDerived" "$REPO_ROOT/src/core/backup.rs"

step "static checker covers manifest-only cross-cutting gates"
checker_output=""
checker_status=""
run_static_capture checker_output checker_status \
    "$REPO_ROOT/scripts/check-tracing-fields.sh" \
    --bead __no_such_bead__ \
    --json
assert_eq "$checker_status" "0" "tracing checker manifest-only invocation exits 0"
assert_json "$checker_output" '.duelingWizardsNoSilentCap.schema' "ee.dueling_wizards.no_silent_cap_shell_check.v1" "no-silent-cap shell block schema is stable"
assert_json "$checker_output" '.duelingWizardsNoSilentCap.status' "pass" "no-silent-cap shell block passes"
assert_json "$checker_output" '.duelingWizardsNoSilentCap.violationCount' "0" "no-silent-cap shell block has no violations"
assert_json "$checker_output" '.duelingWizardsNoSilentCap.subsystemCount' "8" "no-silent-cap shell block sees all subsystems"
assert_json "$checker_output" '.duelingWizardsNoSilentCap.capOperationCount' "4" "no-silent-cap shell block sees all cap operations"
assert_json "$checker_output" '.duelingWizardsMeshRedaction.status' "pass" "mesh-redaction shell block passes"
assert_json "$checker_output" '.duelingWizardsMeshRedaction.violationCount' "0" "mesh-redaction shell block has no violations"

step "remaining cross-cutting manifests pin conservative review posture"
assert_jq_file "$DETERMINISM_MANIFEST" \
    '.schema == "ee.dueling_wizards.determinism_gate.v1" and .policy.localCargoProof == "invalid"' \
    "determinism manifest keeps local cargo proof invalid"
assert_jq_file "$DETERMINISM_MANIFEST" \
    '.initiativeBead == "bd-1n0np" and .gateBead == "bd-1n0np.15.2" and .implementationState == "planned_contract" and .determinismHarness == "scripts/e2e_overhaul/determinism.sh" and .determinismUnit == "tests/determinism_unit.rs" and .surfaceContract == "docs/agent-ux/dueling-wizards/surface-contract.md" and .migrationRegistry == "tests/fixtures/contracts/dueling_wizards_migration_registry.json"' \
    "determinism manifest anchors harness, unit, surface, and migration contracts"
assert_jq_file "$DETERMINISM_MANIFEST" \
    '.policy.runCount == 3 and .policy.canonicalization == "explicit_volatile_field_removal" and .policy.byteStableJsonRequired == true and .policy.packHashReproRequiredWhenPackEmitted == true and .policy.stdoutMachineOnly == true and .policy.rchProofRequiredForRuntimeTests == true' \
    "determinism policy keeps three-run byte-stable RCH proof posture"
assert_jq_file "$DETERMINISM_MANIFEST" \
    '(.requiredAssertions | sort) == ["byte_identical_json","stable_ordering","stderr_or_artifact_diagnostics","volatile_fields_explicit"] and (.packAssertions | sort) == ["pack_hash_absence_is_failure_not_skip","pack_hash_reproducible"]' \
    "determinism shared assertion vocabularies are complete"
# shellcheck disable=SC2016
assert_jq_file "$DETERMINISM_MANIFEST" \
    '["why_not","harvest","calibration","impact","error_recall","blind_spots","conflict","read_fence_consistency","pack_lod","feedback_roi"] as $surfaces | ([.surfaces[].id] | sort) == ($surfaces | sort) and ([.determinismMatrix[].surface] | sort) == ($surfaces | sort) and ([.surfaceCoverageMatrix[].surface] | sort) == ($surfaces | sort)' \
    "determinism surfaces, matrix, and coverage rows stay in lockstep"
# shellcheck disable=SC2016
assert_jq_file "$DETERMINISM_MANIFEST" \
    '["byte_identical_json","volatile_fields_explicit","stable_ordering","stderr_or_artifact_diagnostics"] as $required | ["pack_hash_reproducible","pack_hash_absence_is_failure_not_skip"] as $pack | all(.surfaces[]; (.ownerBeads | index("bd-1n0np.15.2")) and ((.command // "") | length > 0) and ((.schemaRefs // []) | length > 0) and ((.assertions | sort) == ($required | sort)) and (if (.id == "read_fence_consistency" or .id == "pack_lod") then ((.packAssertions | sort) == ($pack | sort)) else (.packAssertions == []) end))' \
    "determinism surfaces declare owners, commands, schemas, assertions, and pack hash rows"
# shellcheck disable=SC2016
assert_jq_file "$DETERMINISM_MANIFEST" \
    '.policy as $policy | ["byte_identical_json","volatile_fields_explicit","stable_ordering","stderr_or_artifact_diagnostics"] as $required | all(.determinismMatrix[]; .runCount == $policy.runCount and .canonicalization == $policy.canonicalization and .stdoutMachineOnly == $policy.stdoutMachineOnly and .diagnosticsChannel == "stderr_or_artifact" and .runtimeProof == "rch_only" and ((.requiredAssertions | sort) == ($required | sort)))' \
    "determinism matrix rows mirror policy and RCH-only runtime proof"
assert_jq_file "$DETERMINISM_MANIFEST" \
    '([.determinismMatrix[] | select(.packHashExpected) | .surface] | sort) == ["pack_lod","read_fence_consistency"] and all(.determinismMatrix[]; if .packHashExpected then (.packHashAbsenceFailure == true and .packHashField == "data.pack.hash") else (.packHashAbsenceFailure == false and .packHashField == null) end)' \
    "determinism pack hash absence is failure only for pack surfaces"
assert_jq_file "$DETERMINISM_MANIFEST" \
    'all(.surfaceCoverageMatrix[]; .mustClauses == 9 and .tested == 9 and .passing == 9 and .divergent == 0 and .scoreMilli == 1000 and .determinismStatus == "three_run_contract_declared" and .runtimeProofPolicy == "rch_required_local_invalid" and .complianceStatus == "declared_conformant" and (if (.surface == "read_fence_consistency" or .surface == "pack_lod") then .packHashStatus == "pack_hash_required" else .packHashStatus == "not_applicable" end))' \
    "determinism coverage matrix is conformant and fail-closed on pack hashes"
# shellcheck disable=SC2016
assert_jq_file "$DETERMINISM_MANIFEST" \
    '(.surfaces[] | select(.id == "impact") | .anchorDeterminism) as $anchor | (.determinismMatrix[] | select(.surface == "impact") | .volatileFields | sort) == ($anchor.volatileFields | sort) and $anchor.storageAssetKind == "memory_anchors" and $anchor.ownerBead == "bd-1n0np.3.2" and $anchor.hashInputMaterial == "normalized_anchor_value_with_anchor_kind_and_source_class" and $anchor.rawAnchorValueExcluded == true and $anchor.redactedValueDeterministic == true and $anchor.generationSource == "workspace_generation_not_wall_clock" and (($anchor.requiredAssertions | sort) == ["generation_not_wall_clock","raw_anchor_value_absent","stable_anchor_value_hash","stable_ordering","stable_redacted_anchor_value"])' \
    "determinism impact anchor contract forbids raw values and mirrors volatile fields"
assert_jq_file "$INGESTION_MANIFEST" \
    '.schema == "ee.dueling_wizards.ingestion_security.v1" and .gateBead == "bd-1n0np.23.3"' \
    "ingestion security manifest identity"
assert_jq_file "$INGESTION_MANIFEST" \
    '.policy.externalTextDefault == "untrusted_until_guarded" and .policy.rawExternalTextStorage == "forbidden_by_default" and .policy.flaggedInputBehavior == "quarantine_not_store" and .policy.auditEventRequired == true and .policy.localCargoProof == "invalid"' \
    "ingestion security policy is fail-closed and RCH-only"
assert_jq_file "$INGESTION_MANIFEST" \
    '.requiredPipeline == ["source_classification","secret_redaction","prompt_injection_guard","quarantine_not_store","audit_event","regression_corpus"]' \
    "ingestion security guard pipeline order is stable"
assert_jq_file "$INGESTION_MANIFEST" \
    '([.surfaces[].surface] | sort) == ["docs_bootstrap","error_log_diagnosis","sandbox_import"]' \
    "ingestion security surface set is complete"
assert_jq_file "$INGESTION_MANIFEST" \
    'all(.surfaces[]; .ownerBead == "bd-1n0np.23.3" and .externalText == true and .redaction == "crate::policy::redact_secret_like_content" and .promptInjectionGuard == "crate::policy::detect_instruction_like_content" and .flaggedBehavior == "quarantine_not_store" and .rawStorage == "forbidden" and (.requiredPipeline == ["source_classification","secret_redaction","prompt_injection_guard","quarantine_not_store","audit_event","regression_corpus"]) and ((.requiredRegressionPayloadClasses | sort) == ["destructive_command_coercion","ignore_previous_instructions","mixed_benign_and_malicious","role_markup","secret_like_token"]))' \
    "ingestion surfaces require redaction, prompt guard, quarantine, and corpus coverage"
assert_jq_file "$INGESTION_MANIFEST" \
    '([.guardOrderMatrix[].surface] | sort) == ([.surfaces[].surface] | sort) and all(.guardOrderMatrix[]; .redactionBeforePromptGuard == true and .promptGuardBeforeStorage == true and .rawStorageBeforeGuards == "forbidden" and .flaggedStorageDisposition == "quarantine_not_store" and .auditAfterDisposition == true)' \
    "ingestion guard-order matrix keeps raw text out of storage"
assert_jq_file "$INGESTION_MANIFEST" \
    '([.regressionPayloadExamples[].payloadClass] | sort) == (.regressionPayloadClasses | sort) and all(.regressionPayloadExamples[]; .mustRunPromptInjectionGuard == true and .mustQuarantineWhenFlagged == true and .rawStorage == "forbidden" and (.expectedAuditEvent | endswith("_ingestion_security")))' \
    "ingestion regression payload examples require quarantine and audit"
run_static_command \
    "ingestion policy source exposes external-text screen" \
    grep -q "screen_external_text_for_ingestion" "$REPO_ROOT/src/policy/mod.rs"
assert_jq_file "$MESH_MANIFEST" \
    '.schema == "ee.dueling_wizards.mesh_redaction.v1" and .policy.rawPayloadExportAllowed == false' \
    "mesh manifest forbids raw payload export"
assert_jq_file "$WHY_PACKDNA_MANIFEST" \
    '.schema == "ee.dueling_wizards.why_packdna_signals.v1" and .gateBead == "bd-1n0np.23.5"' \
    "why/PackDna manifest identity"
assert_jq_file "$WHY_PACKDNA_MANIFEST" \
    '([.requiredSignals[].id] | sort) == ["anchor_file_line_provenance","causal_ancestry_path","contradiction_suppressed","freshness_symbol_drift","sentinel_state","task_lens"]' \
    "why/PackDna required signal set is complete"
assert_jq_file "$WHY_PACKDNA_MANIFEST" \
    'all(.requiredSignals[]; (.ownerBeads | index("bd-1n0np.23.5")) and (.whyFields | length > 0) and (.packDnaFields | length > 0) and (.schemaRefs | index("ee.why.v1")) and (.schemaRefs | index("ee.context.pack_dna.v1")) and ((.agentQuestion // "") | length > 0) and ((.decisionImpact // "") | length > 0))' \
    "why/PackDna signals declare owners, fields, schemas, and agent decisions"
assert_jq_file "$WHY_PACKDNA_MANIFEST" \
    'all(.requiredSignals[] | select(.id == "causal_ancestry_path"); .schemaRefs | index("ee.why.causal.v1"))' \
    "why/PackDna causal signal references causal schema"
assert_jq_file "$WHY_PACKDNA_MANIFEST" \
    '([.requiredSignals[].id] | sort) == ([.signalCoverageMatrix[].signal] | sort)' \
    "why/PackDna coverage matrix mirrors required signals"
assert_jq_file "$WHY_PACKDNA_MANIFEST" \
    'all(.signalCoverageMatrix[]; .compatibility == "stable_additive" and .redactionStatus == "redaction_safe" and .degradedHandlingStatus == "degraded_not_silent" and .runtimeProofPolicy == "rch_required_local_invalid" and .complianceStatus == "planned_conformant" and .scoreMilli >= 950 and .divergent == 0)' \
    "why/PackDna coverage matrix keeps conservative proof posture"
assert_jq_file "$OBSERVABILITY_MANIFEST" \
    '.schema == "ee.dueling_wizards.observability_no_silent_cap.v1" and .initiativeBead == "bd-1n0np" and .gateBead == "bd-1n0np.15.5" and .manifestOwner == "tests/contracts/dueling_wizards_observability_no_silent_cap.rs" and .doc == "docs/agent-ux/dueling-wizards/observability-no-silent-cap.md" and .implementationState == "planned_contract"' \
    "observability manifest identity and owner are stable"
assert_jq_file "$OBSERVABILITY_MANIFEST" \
    '.policy.structuredTracingRequired == true and .policy.noSilentCapRequired == true and .policy.capEventCompatibility == "stable_additive" and .policy.missingCapEventBehavior == "degraded_not_silent" and .policy.localCargoProof == "invalid" and .policy.rchProofRequiredForRuntimeTests == true' \
    "observability manifest keeps no-silent-cap and RCH-only proof policy"
assert_jq_file "$OBSERVABILITY_MANIFEST" \
    '(.requiredTraceFields | sort) == ["bead_id","degraded_codes","elapsed_ms","phase","request_id","surface","workspace_id"] and (.standardPhases | sort) == ["dependency_check","dispatch","input","persistence","response"] and (.capOperations | sort) == ["abstention","sampling","top_n","truncation"] and (.capEventFields | sort) == ["cap_kind","cap_limit","drop_reason","dropped_count","retained_count"]' \
    "observability manifest shared trace and cap vocabularies are complete"
assert_jq_file "$OBSERVABILITY_MANIFEST" \
    '([.capEventExamples[].cap_kind] | sort) == ["abstention","sampling","top_n","truncation"] and ([.capEventExamples[].drop_reason] | sort) == ["fixture_sample_limit","ranked_output_limit","required_dependency_unavailable","token_budget_exceeded"] and all(.capEventExamples[]; .surface == "harness_contract" and (.phase | IN("dependency_check","persistence","response")) and (.dropped_count | type == "number" and . > 0) and (.cap_limit | type == "number") and (.retained_count | type == "number") and .retained_count <= .cap_limit and ((.drop_reason // "") | length > 0))' \
    "observability cap-event examples cover all operations without silent drops"
# shellcheck disable=SC2016
assert_jq_file "$OBSERVABILITY_MANIFEST" \
    '["evidence_harvester","anchors_freshness","error_recall","read_fence","write_immune","gap_honesty","contradiction_resolution","harness_contract"] as $subsystems | ["workspace_id","request_id","bead_id","surface","phase","elapsed_ms","degraded_codes"] as $trace | ["truncation","sampling","top_n","abstention"] as $ops | ["cap_kind","dropped_count","drop_reason","cap_limit","retained_count"] as $cap_fields | ([.subsystems[].id] | sort) == ($subsystems | sort) and all(.subsystems[]; .surface == .id and (.ownerBeads | index("bd-1n0np.15.5")) and ((.requiredTraceFields | sort) == ($trace | sort)) and ((.capOperations | sort) == ($ops | sort)) and ((.capEventFields | sort) == ($cap_fields | sort)) and (if .status == "implemented" then (.sourceAnchors | length) > 0 else true end))' \
    "observability subsystems carry shared fields, cap vocabulary, owners, and anchors"
assert_jq_file "$OBSERVABILITY_MANIFEST" \
    'all(.subsystemCoverageMatrix[]; .traceFieldCount == 7 and .capOperationCount == 4 and .capEventFieldCount == 5 and .mustClauses == 10 and .tested == 10 and .passing == 10 and .divergent == 0 and .scoreMilli == 1000 and .traceStatus == "shared_fields_declared" and .capStatus == "no_silent_cap_declared" and .runtimeProofPolicy == "rch_required_local_invalid" and .complianceStatus == "declared_conformant" and (if .status == "implemented" then .anchorEvidenceStatus == "source_anchors_required" else .anchorEvidenceStatus == "planned_contract_only" end))' \
    "observability subsystem coverage matrix is conformant and fail-visible"

step "event-contract radar recognizes the cross-cutting driver"
run_static_command \
    "event radar scans cross-cutting e2e driver" \
    "$REPO_ROOT/scripts/e2e_event_contract_radar.sh" \
    --quiet \
    --output "$LOG_DIR/e2e_cross_cutting_radar.json" \
    "$REPO_ROOT/scripts/e2e_cross_cutting.sh"

step "validity columns are never written from a bookkeeping timestamp"
# bd-o22r0. `valid_from`, `valid_to` and `superseded_at` are compared LEXICALLY
# in SQL and are written in the SecondsFormat::Secs `Z` spelling; `created_at`,
# `updated_at` and `tombstoned_at` use the offset form. 'Z' is 0x5A and '+' is
# 0x2B, so at the same instant a `Z` value sorts ABOVE a `+00:00` one. Mixing
# spellings inside one column breaks that column's ordering -- which is how
# V123's supersession backfill left two live heads in one revision chain.
#
# The specific regression this catches is an UPDATE that binds ONE parameter to
# both a validity column and a bookkeeping column. That shipped twice:
# expire_memory_valid_to and mark_memory_superseded each wrote `?1` into both
# `valid_to`/`superseded_at` AND `updated_at`, so every expired or revised
# memory carried a `Z`-spelled `updated_at` with no import involved. Both are
# fixed; nothing stopped a third from appearing. This is that guard.
shared_bind_updates="$(
    python3 - <<'PYEOF'
import io, re, sys

VALIDITY = ("valid_from", "valid_to", "superseded_at")
BOOKKEEPING = ("created_at", "updated_at", "tombstoned_at")
offenders = []
text = io.open("src/db/mod.rs", encoding="utf-8", errors="replace").read()
for match in re.finditer(r'"(UPDATE\s+memories\s+SET\s[^"]{0,400})"', text, re.S):
    body = " ".join(match.group(1).split())
    line = text[: match.start()].count("\n") + 1
    for validity in VALIDITY:
        assign = re.search(rf"\b{validity}\s*=\s*(\?\d+)", body)
        if not assign:
            continue
        param = assign.group(1)
        for book in BOOKKEEPING:
            shared = re.search(rf"\b{book}\s*=\s*{re.escape(param)}\b", body)
            if shared:
                offenders.append(
                    f"src/db/mod.rs:{line} binds {param} to both {validity} and {book}"
                )
sys.stdout.write("\n".join(sorted(set(offenders))))
PYEOF
)"
if [ -z "$shared_bind_updates" ]; then
    e2e_log_assert_eq "0" "0" "validity and bookkeeping timestamps never share a bind"
    _harness_pass "validity and bookkeeping timestamps never share a bind"
else
    e2e_log_assert_eq "$(printf '%s\n' "$shared_bind_updates" | wc -l | tr -d ' ')" "0" \
        "validity and bookkeeping timestamps never share a bind"
    _harness_fail "validity and bookkeeping timestamps never share a bind: ${shared_bind_updates}"
fi

step "validity comparison bounds use the validity canon"
# bd-o22r0 / bd-60tq7. normalize_validity_timestamp's doc comment states the rule
# -- "every writer and every comparison bound must use this exact spelling" --
# and until now that rule lived only in prose. It was broken twice: resume.rs and
# context.rs both passed a bare to_rfc3339() (`+00:00`, variable fractional
# digits) as the as_of bound into a reader whose SQL compares it lexically
# against valid_from/valid_to, stored as SecondsFormat::Secs `Z`. A comparison
# that mixes the two misorders at the boundary instant.
#
# A prose rule is one a future edit breaks silently. This fails the run instead.
bare_validity_bounds="$(
    python3 - <<'PYEOF'
import io, re, glob, sys

# readers whose trailing timestamp argument is compared against a validity column
READERS = (
    "list_recent_current_memories_for_retrieval",
    "list_memories_valid_at",
    "list_memories_by_tag_valid_at",
    "list_all_tags_valid_at",
    "get_tag_counts_valid_at",
)
BARE = re.compile(r"to_rfc3339\s*\(\s*\)")


def has_bare(fragment):
    """True when `fragment` calls to_rfc3339() rather than to_rfc3339_opts(..)."""
    for hit in BARE.finditer(fragment):
        if not fragment[max(0, hit.start() - 5) : hit.start()].endswith("_opts"):
            return True
    return False


offenders = []
for path in sorted(glob.glob("src/**/*.rs", recursive=True)):
    text = io.open(path, encoding="utf-8", errors="replace").read()
    # Strip line comments BEFORE scanning. The fixed call sites carry a comment
    # explaining the old defect, and that prose contains the literal
    # `to_rfc3339()` -- matching it would fail the run on correct code and invite
    # someone to delete the explanation to get green.
    lines = [re.sub(r"//.*$", "", raw) for raw in text.splitlines()]

    # Variables bound to a bare to_rfc3339(). The defect ships in TWO shapes and
    # an earlier version of this guard only caught one: resume.rs passed
    # `&now.to_rfc3339()` inline, but context.rs bound
    # `let reference_time_text = reference_time.to_rfc3339();` NINE lines above
    # its call. A forward-only window missed it entirely -- the guard would have
    # passed the very defect it was written for.
    tainted = {
        match.group(1)
        for offset, raw in enumerate(lines)
        for match in [re.match(r"\s*let\s+(?:mut\s+)?(\w+)\s*=\s*(.+);\s*$", raw)]
        if match and has_bare(match.group(2))
    }

    for index, line in enumerate(lines):
        if not any(reader in line for reader in READERS):
            continue
        if "fn " in line:  # the definition, not a call
            continue
        window = " ".join(lines[index : min(index + 8, len(lines))])
        if has_bare(window):
            offenders.append(
                f"{path}:{index + 1} passes a bare to_rfc3339() as a validity bound"
            )
            continue
        for name in sorted(tainted):
            if re.search(rf"\b{re.escape(name)}\b", window):
                offenders.append(
                    f"{path}:{index + 1} passes `{name}`, bound from a bare to_rfc3339(), as a validity bound"
                )
                break
sys.stdout.write("\n".join(sorted(set(offenders))))
PYEOF
)"
if [ -z "$bare_validity_bounds" ]; then
    e2e_log_assert_eq "0" "0" "validity bounds use normalize_validity_timestamp"
    _harness_pass "validity bounds use normalize_validity_timestamp"
else
    e2e_log_assert_eq "$(printf '%s\n' "$bare_validity_bounds" | wc -l | tr -d ' ')" "0" \
        "validity bounds use normalize_validity_timestamp"
    _harness_fail "validity bounds use normalize_validity_timestamp: ${bare_validity_bounds}"
fi

step "liveness is decided from superseded_at, never from valid_to"
# bd-o22r0 / bd-tmv70. `valid_to` is overloaded: one nullable timestamp expresses
# two of four states (S0 live/in-force, S1 live/expired, S2 history/unexpired,
# S3 history/expired). So `valid_to IS NULL` is NOT "this is the current
# revision" -- it is "this revision has no expiry", which a superseded row can
# also satisfy. Identity belongs to `superseded_at`; `valid_to` only ever
# answers applicability.
#
# Reading identity off `valid_to` shipped twice: backup.rs chain-head selection
# and verify.rs liveness both passed superseded revisions through as live. Both
# are fixed -- backup.rs via filter_current_memory_ids, verify.rs via
# get_memory_superseded_at -- and I repaired seven call sites of this class by
# hand while enforcing nothing. This is the enforcement.
#
# Two shapes are deliberately NOT offences:
#   * paired with `valid_from` -- "no validity bracket was declared at all"
#     (context.rs, search.rs), a question about the bracket, not about identity;
#   * a writer asking "did the caller supply a value to write" (CLI args,
#     update_memory_validity).
#
# Known holes, named rather than papered over: the pairing test is a +/-2 line
# window, so it clears update_memory_validity for proximity to an unrelated
# `valid_from` rather than because it is a writer; it reads only `src/**/*.rs`;
# and it sees the `.is_none()`/`.is_some()` spelling, not an equivalent
# `match`/`if let` on the same field.
valid_to_liveness="$(
    python3 - <<'PYEOF'
import io, re, glob, sys

LIVENESS = re.compile(r"\bvalid_to\s*(?:\.\s*as_ref\s*\(\s*\)\s*)?\.\s*is_(none|some)\s*\(\s*\)")
# a writer asking whether the caller supplied a bound, not a liveness test
ARGFIELD = re.compile(r"\b(?:args|options|opts|input|request|params)\s*\.\s*valid_to\b")

offenders = []
for path in sorted(glob.glob("src/**/*.rs", recursive=True)):
    text = io.open(path, encoding="utf-8", errors="replace").read()
    # Strip line comments first: the repaired sites carry comments naming the old
    # defect, and that prose contains the literal `valid_to.is_none()`. Matching
    # it would fail the run on correct code and invite deleting the explanation
    # to get green -- the same trap the validity-bound guard already hit.
    lines = [re.sub(r"//.*$", "", raw) for raw in text.splitlines()]

    # Test modules are excluded by brace depth, not by "first #[cfg(test)]":
    # these files carry several test modules, and a first-hit rule would silence
    # every production line after the earliest one.
    spans, depth, pending, start, sdepth = [], 0, False, None, 0
    for index, line in enumerate(lines):
        if "#[cfg(test)]" in line:
            pending = True
        if pending and re.search(r"\bmod\s+\w+", line) and "{" in line:
            pending, start, sdepth = False, index, depth
        depth += line.count("{") - line.count("}")
        if start is not None and depth <= sdepth and index > start:
            spans.append((start, index))
            start = None
    if start is not None:
        spans.append((start, len(lines)))

    for index, line in enumerate(lines):
        if not LIVENESS.search(line):
            continue
        if any(lo <= index <= hi for lo, hi in spans):
            continue
        if "valid_from" in " ".join(lines[max(0, index - 2) : index + 3]):
            continue
        if ARGFIELD.search(line):
            continue
        offenders.append(
            f"{path}:{index + 1} decides liveness from valid_to; identity lives in superseded_at"
        )
sys.stdout.write("\n".join(sorted(set(offenders))))
PYEOF
)"
if [ -z "$valid_to_liveness" ]; then
    e2e_log_assert_eq "0" "0" "liveness is keyed on superseded_at"
    _harness_pass "liveness is keyed on superseded_at"
else
    e2e_log_assert_eq "$(printf '%s\n' "$valid_to_liveness" | wc -l | tr -d ' ')" "0" \
        "liveness is keyed on superseded_at"
    _harness_fail "liveness is keyed on superseded_at: ${valid_to_liveness}"
fi

log_event \
    "note" \
    "phase" "summary" \
    "artifact_dir" "$LOG_DIR" \
    "event_schema" "ee.test_event.v1" \
    "migration_manifest" "${MIGRATION_MANIFEST#"$REPO_ROOT"/}" \
    "backup_manifest" "${BACKUP_MANIFEST#"$REPO_ROOT"/}" \
    "mesh_manifest" "${MESH_MANIFEST#"$REPO_ROOT"/}"

summary
