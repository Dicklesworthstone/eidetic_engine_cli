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

harness_init "cross_cutting"
require_tool jq
require_tool python3

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
    '.backupCoverageBead == "bd-1n0np.23.2" and (.allocations | length) >= 11' \
    "migration registry declares backup coverage owner and allocations"
assert_jq_file "$MIGRATION_MANIFEST" \
    'all(.allocations[]; (.backupAssetKind | type == "string") and (.ownerBead | startswith("bd-1n0np.")))' \
    "migration allocations have backup asset kinds and owner beads"
assert_jq_file "$MIGRATION_MANIFEST" \
    'all(.transitionMatrix[]; .proofPosture == "rch_only_no_local_fallback")' \
    "migration transition matrix keeps RCH-only proof posture"

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
assert_jq_file "$BACKUP_MANIFEST" \
    '.schema == "ee.dueling_wizards.backup_coverage.v1" and .gateBead == "bd-1n0np.23.2"' \
    "backup coverage identity"
assert_jq_file "$BACKUP_MANIFEST" \
    '.policy.missingAssetFailure == "degraded_not_silent_loss" and .policy.hashPolicy == "blake3_required"' \
    "backup coverage fail-visible hash policy"
assert_jq_file "$BACKUP_MANIFEST" \
    'all(.assetCoverageMatrix[]; .complianceStatus == "declared_conformant" and .scoreMilli >= 950 and .divergent == 0)' \
    "backup coverage matrix is conformant"

step "static checker covers manifest-only cross-cutting gates"
checker_output=""
checker_status=""
run_static_capture checker_output checker_status \
    "$REPO_ROOT/scripts/check-tracing-fields.sh" \
    --bead __no_such_bead__ \
    --json
assert_eq "$checker_status" "0" "tracing checker manifest-only invocation exits 0"
assert_json "$checker_output" '.duelingWizardsNoSilentCap.status' "pass" "no-silent-cap shell block passes"
assert_json "$checker_output" '.duelingWizardsMeshRedaction.status' "pass" "mesh-redaction shell block passes"
assert_json "$checker_output" '.duelingWizardsMeshRedaction.violationCount' "0" "mesh-redaction shell block has no violations"

step "remaining cross-cutting manifests pin conservative review posture"
assert_jq_file "$DETERMINISM_MANIFEST" \
    '.schema == "ee.dueling_wizards.determinism_gate.v1" and .policy.localCargoProof == "invalid"' \
    "determinism manifest keeps local cargo proof invalid"
assert_jq_file "$INGESTION_MANIFEST" \
    '.schema == "ee.dueling_wizards.ingestion_security.v1" and .gateBead == "bd-1n0np.23.3"' \
    "ingestion security manifest identity"
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
    '.schema == "ee.dueling_wizards.observability_no_silent_cap.v1" and .policy.noSilentCapRequired == true' \
    "observability manifest keeps no-silent-cap required"

step "event-contract radar recognizes the cross-cutting driver"
run_static_command \
    "event radar scans cross-cutting e2e driver" \
    "$REPO_ROOT/scripts/e2e_event_contract_radar.sh" \
    --quiet \
    --output "$LOG_DIR/e2e_cross_cutting_radar.json" \
    "$REPO_ROOT/scripts/e2e_cross_cutting.sh"

log_event \
    "note" \
    "phase" "summary" \
    "artifact_dir" "$LOG_DIR" \
    "event_schema" "ee.test_event.v1" \
    "migration_manifest" "${MIGRATION_MANIFEST#"$REPO_ROOT"/}" \
    "backup_manifest" "${BACKUP_MANIFEST#"$REPO_ROOT"/}" \
    "mesh_manifest" "${MESH_MANIFEST#"$REPO_ROOT"/}"

summary
