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
assert_jq_file "$MIGRATION_MANIFEST" \
    '.currentLastCompiledMigration == 79 and .nextPlannedMigration == 80' \
    "migration registry pins compiled tail and next planned migration"
assert_jq_file "$MIGRATION_MANIFEST" \
    '([.transitionMatrix[].version] | sort) == [66,67,68,69,69,70,71,72,80,81,82] and ([.transitionMatrix[].id] | unique | length) == (.transitionMatrix | length) and ([.transitionMatrix[] | select(.status == "planned") | .version] | unique | length) == ([.transitionMatrix[] | select(.status == "planned")] | length)' \
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
assert_jq_file "$BACKUP_MANIFEST" \
    'all(.assetCoverageMatrix[]; .complianceStatus == "declared_conformant" and .scoreMilli >= 950 and .divergent == 0)' \
    "backup coverage matrix is conformant"
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

log_event \
    "note" \
    "phase" "summary" \
    "artifact_dir" "$LOG_DIR" \
    "event_schema" "ee.test_event.v1" \
    "migration_manifest" "${MIGRATION_MANIFEST#"$REPO_ROOT"/}" \
    "backup_manifest" "${BACKUP_MANIFEST#"$REPO_ROOT"/}" \
    "mesh_manifest" "${MESH_MANIFEST#"$REPO_ROOT"/}"

summary
