#!/usr/bin/env bash
# check-tracing-fields.sh - Part II tracing convention gate (bd-3usjw.58).
#
# This is build-independent. It audits Beads descriptions and declared source
# file surfaces for the shared tracing field convention documented in
# docs/observability/tracing_field_convention.md. It also validates the
# dueling-wizards no-silent-cap manifest so planned subsystems cannot drop the
# cap-event vocabulary from the shell/static review gate.

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BEADS_FILE="${ROOT}/.beads/issues.jsonl"
REPORT_FILE="${EE_TRACING_FIELD_REPORT:-${ROOT}/.tracing-field-report.json}"
DOC_PATH="${ROOT}/docs/observability/tracing_field_convention.md"
BASELINE_FILE="${ROOT}/tests/fixtures/tracing_field/violation_baseline.txt"

JSON_OUTPUT=false
SELF_TEST=false
BEAD_FILTER=""

usage() {
    cat <<'USAGE'
Usage: scripts/check-tracing-fields.sh [--json] [--bead ID] [--self-test]

  --json       Emit the JSON report to stdout.
  --bead ID    Audit one bead instead of every Part II implements-surface bead.
  --self-test  Run synthetic checker tests without reading the workspace.

A real run is judged against tests/fixtures/tracing_field/violation_baseline.txt,
which records the violations that already existed when this gate was first wired.
It fails in BOTH directions: a NEW violation is an error, and a baselined bead
that now PASSES is also an error telling you to delete its line. The baseline may
only shrink. With --bead the baseline is skipped, because a single-bead audit
cannot distinguish "fixed" from "not audited".

Writes:
  .tracing-field-report.json

Exit codes:
  0  pass
  1  tracing convention violations found (new, or a stale baseline entry)
  2  usage error
  3  required tool or input missing
USAGE
}

while [ $# -gt 0 ]; do
    case "$1" in
        --json)
            JSON_OUTPUT=true
            shift
            ;;
        --self-test)
            SELF_TEST=true
            shift
            ;;
        --bead)
            if [ $# -lt 2 ] || [ -z "${2:-}" ]; then
                echo "error: --bead requires an id" >&2
                exit 2
            fi
            BEAD_FILTER="$2"
            shift 2
            ;;
        --help|-h)
            usage
            exit 0
            ;;
        *)
            echo "error: unknown argument: $1" >&2
            usage >&2
            exit 2
            ;;
    esac
done

require_tool() {
    if ! command -v "$1" >/dev/null 2>&1; then
        echo "error: required tool not found: $1" >&2
        exit 3
    fi
}

run_checker() {
    local beads_path="$1"
    local root_path="$2"
    local bead_filter="$3"
    local manifest_required="${4:-true}"

    python3 - "$beads_path" "$root_path" "$bead_filter" "$manifest_required" <<'PY'
import json
import re
import sys
from pathlib import Path

beads_path = Path(sys.argv[1])
root = Path(sys.argv[2])
bead_filter = sys.argv[3]
manifest_required = sys.argv[4] == "true"

required_fields = [
    "workspace_id",
    "request_id",
    "bead_id",
    "surface",
    "phase",
    "elapsed_ms",
    "degraded_codes",
]
phase_names = {"input", "dispatch", "dependency_check", "persistence", "response"}
required_subsystems = {
    "evidence_harvester",
    "anchors_freshness",
    "error_recall",
    "read_fence",
    "write_immune",
    "gap_honesty",
    "contradiction_resolution",
    "harness_contract",
}
cap_operations = {"truncation", "sampling", "top_n", "abstention"}
cap_event_fields = {
    "cap_kind",
    "dropped_count",
    "drop_reason",
    "cap_limit",
    "retained_count",
}
observability_manifest_rel = Path(
    "tests/fixtures/contracts/dueling_wizards_observability_no_silent_cap.json"
)
mesh_redaction_manifest_rel = Path(
    "tests/fixtures/contracts/dueling_wizards_mesh_redaction.json"
)

mesh_material_lanes = {
    "metadata",
    "body",
    "embedding",
    "graphLink",
    "revisionNotice",
    "curationSignal",
}
mesh_redaction_classes = {"omit", "hash", "redact"}
mesh_export_postures = {"deny", "redact"}
mesh_anchor_forbidden_fields = {
    "anchor_value",
    "raw_anchor_value",
    "raw_path",
    "raw_symbol",
    "raw_command",
    "raw_schema",
}
mesh_peer_policy_anchors = {
    "redaction",
    "metadata",
    "body",
    "embedding",
    "graphLink",
    "revisionNotice",
    "curationSignal",
    "payloadExportAllowed",
    "rawPayloadExportAllowed",
    "redactedPayloadRequired",
}
# Keep in lockstep with REQUIRED_SHARE_PREVIEW_SOURCE_ANCHORS in
# tests/contracts/dueling_wizards_mesh_redaction.rs. The share-preview schema
# moved V1 -> V2 in 601006ae9.
mesh_share_preview_anchors = {
    "SHARE_PREVIEW_SCHEMA_V2",
    "SharePreviewCandidate",
    "redaction_class",
    "build_share_preview",
    "scan_mesh_export_subjects",
    "MESH_EXPORT_POLICY_ATTESTATION_SCHEMA_V1",
}

# Keep in lockstep with FORBIDDEN_SHARE_PREVIEW_ORACLES in
# tests/contracts/dueling_wizards_mesh_redaction.rs. cb3738a0c removed these
# content-derived oracles from the share-preview surface: an aggregate or
# per-example hash over redacted content lets a peer confirm guessed content
# without ever receiving it. They must stay absent.
mesh_share_preview_forbidden_oracles = {
    "share_preview_hash",
    "share_preview_content_hash",
    "serialization_error:",
}

def as_set(values):
    return set(values or [])

def add_manifest_violation(violations, reason, **extra):
    entry = {"reason": reason}
    entry.update(extra)
    violations.append(entry)

def validate_observability_manifest(root_path, required):
    manifest_path = root_path / observability_manifest_rel
    violations = []
    if not manifest_path.exists():
        if required:
            add_manifest_violation(
                violations,
                "missing dueling-wizards observability manifest",
                path=str(observability_manifest_rel),
            )
            status = "fail"
        else:
            status = "skipped"
        return {
            "schema": "ee.dueling_wizards.no_silent_cap_shell_check.v1",
            "status": status,
            "manifest": str(observability_manifest_rel),
            "violationCount": len(violations),
            "violations": violations,
        }

    try:
        manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
    except json.JSONDecodeError as error:
        add_manifest_violation(
            violations,
            "dueling-wizards observability manifest is not valid JSON",
            path=str(observability_manifest_rel),
            detail=str(error),
        )
        return {
            "schema": "ee.dueling_wizards.no_silent_cap_shell_check.v1",
            "status": "fail",
            "manifest": str(observability_manifest_rel),
            "violationCount": len(violations),
            "violations": violations,
        }

    expected_scalars = {
        "schema": "ee.dueling_wizards.observability_no_silent_cap.v1",
        "initiativeBead": "bd-1n0np",
        "gateBead": "bd-1n0np.15.5",
        "implementationState": "planned_contract",
    }
    for key, expected in expected_scalars.items():
        if manifest.get(key) != expected:
            add_manifest_violation(
                violations,
                "manifest scalar drifted",
                field=key,
                expected=expected,
                actual=manifest.get(key),
            )

    policy = manifest.get("policy") or {}
    for key in [
        "structuredTracingRequired",
        "noSilentCapRequired",
        "rchProofRequiredForRuntimeTests",
    ]:
        if policy.get(key) is not True:
            add_manifest_violation(
                violations,
                "manifest policy boolean must be true",
                field=f"policy.{key}",
                actual=policy.get(key),
            )
    for key, expected in {
        "capEventCompatibility": "stable_additive",
        "missingCapEventBehavior": "degraded_not_silent",
        "localCargoProof": "invalid",
    }.items():
        if policy.get(key) != expected:
            add_manifest_violation(
                violations,
                "manifest policy scalar drifted",
                field=f"policy.{key}",
                expected=expected,
                actual=policy.get(key),
            )

    expected_sets = {
        "requiredTraceFields": set(required_fields),
        "standardPhases": phase_names,
        "capOperations": cap_operations,
        "capEventFields": cap_event_fields,
    }
    for key, expected in expected_sets.items():
        actual = as_set(manifest.get(key))
        if actual != expected:
            add_manifest_violation(
                violations,
                "manifest vocabulary set drifted",
                field=key,
                missing=sorted(expected - actual),
                extra=sorted(actual - expected),
            )

    example_operations = set()
    for index, example in enumerate(manifest.get("capEventExamples") or []):
        context = f"capEventExamples[{index}]"
        surface = example.get("surface")
        phase = example.get("phase")
        operation = example.get("cap_kind")
        if surface not in required_subsystems:
            add_manifest_violation(
                violations,
                "cap event example has unknown surface",
                context=context,
                surface=surface,
            )
        if phase not in phase_names:
            add_manifest_violation(
                violations,
                "cap event example has unknown phase",
                context=context,
                phase=phase,
            )
        if operation not in cap_operations:
            add_manifest_violation(
                violations,
                "cap event example has unknown cap_kind",
                context=context,
                cap_kind=operation,
            )
        else:
            example_operations.add(operation)
        missing_fields = sorted(field for field in cap_event_fields if field not in example)
        if missing_fields:
            add_manifest_violation(
                violations,
                "cap event example is missing fields",
                context=context,
                missingFields=missing_fields,
            )
        dropped_count = example.get("dropped_count")
        cap_limit = example.get("cap_limit")
        retained_count = example.get("retained_count")
        if not isinstance(dropped_count, int) or dropped_count <= 0:
            add_manifest_violation(
                violations,
                "cap event example must report a non-zero dropped_count",
                context=context,
                dropped_count=dropped_count,
            )
        if isinstance(retained_count, int) and isinstance(cap_limit, int) and retained_count > cap_limit:
            add_manifest_violation(
                violations,
                "cap event example retained_count exceeds cap_limit",
                context=context,
                retained_count=retained_count,
                cap_limit=cap_limit,
            )
        if not str(example.get("drop_reason") or "").strip():
            add_manifest_violation(violations, "cap event example has empty drop_reason", context=context)
    if example_operations != cap_operations:
        add_manifest_violation(
            violations,
            "cap event examples must cover every cap operation",
            missing=sorted(cap_operations - example_operations),
            extra=sorted(example_operations - cap_operations),
        )

    subsystem_ids = set()
    for index, subsystem in enumerate(manifest.get("subsystems") or []):
        context = f"subsystems[{index}]"
        subsystem_id = subsystem.get("id")
        if subsystem_id in subsystem_ids:
            add_manifest_violation(violations, "duplicate subsystem id", context=context, subsystem=subsystem_id)
        subsystem_ids.add(subsystem_id)
        if subsystem_id not in required_subsystems:
            add_manifest_violation(violations, "unknown subsystem id", context=context, subsystem=subsystem_id)
        if subsystem.get("surface") != subsystem_id:
            add_manifest_violation(violations, "subsystem surface must match id", context=context, subsystem=subsystem_id)
        if "bd-1n0np.15.5" not in as_set(subsystem.get("ownerBeads")):
            add_manifest_violation(violations, "subsystem missing bd-1n0np.15.5 owner", context=context, subsystem=subsystem_id)
        if as_set(subsystem.get("requiredTraceFields")) != set(required_fields):
            add_manifest_violation(violations, "subsystem trace fields drifted", context=context, subsystem=subsystem_id)
        if as_set(subsystem.get("capOperations")) != cap_operations:
            add_manifest_violation(violations, "subsystem cap operations drifted", context=context, subsystem=subsystem_id)
        if as_set(subsystem.get("capEventFields")) != cap_event_fields:
            add_manifest_violation(violations, "subsystem cap event fields drifted", context=context, subsystem=subsystem_id)
        anchors = subsystem.get("sourceAnchors") or []
        if subsystem.get("status") == "implemented" and not anchors:
            add_manifest_violation(violations, "implemented subsystem must list source anchors", context=context, subsystem=subsystem_id)
        for anchor in anchors:
            if not (root_path / anchor).exists():
                add_manifest_violation(violations, "subsystem source anchor is missing", context=context, subsystem=subsystem_id, path=anchor)
    if subsystem_ids != required_subsystems:
        add_manifest_violation(
            violations,
            "subsystem set drifted",
            missing=sorted(required_subsystems - subsystem_ids),
            extra=sorted(subsystem_ids - required_subsystems),
        )

    matrix_ids = set()
    for index, row in enumerate(manifest.get("subsystemCoverageMatrix") or []):
        context = f"subsystemCoverageMatrix[{index}]"
        subsystem_id = row.get("subsystem")
        matrix_ids.add(subsystem_id)
        for key, expected in {
            "traceStatus": "shared_fields_declared",
            "capStatus": "no_silent_cap_declared",
            "runtimeProofPolicy": "rch_required_local_invalid",
            "complianceStatus": "declared_conformant",
        }.items():
            if row.get(key) != expected:
                add_manifest_violation(
                    violations,
                    "coverage matrix scalar drifted",
                    context=context,
                    field=key,
                    expected=expected,
                    actual=row.get(key),
                )
        if row.get("mustClauses") != 10 or row.get("tested") != 10 or row.get("passing") != 10 or row.get("divergent") != 0:
            add_manifest_violation(violations, "coverage matrix counts drifted", context=context, subsystem=subsystem_id)
        if not isinstance(row.get("scoreMilli"), int) or row["scoreMilli"] < 950:
            add_manifest_violation(violations, "coverage matrix score below threshold", context=context, subsystem=subsystem_id, scoreMilli=row.get("scoreMilli"))
    if matrix_ids != required_subsystems:
        add_manifest_violation(
            violations,
            "coverage matrix subsystem set drifted",
            missing=sorted(required_subsystems - matrix_ids),
            extra=sorted(matrix_ids - required_subsystems),
        )

    for index, anchor in enumerate(manifest.get("anchors") or []):
        context = f"anchors[{index}]"
        source = anchor.get("source")
        if not source:
            add_manifest_violation(violations, "anchor source missing", context=context)
            continue
        source_path = root_path / source
        if not source_path.exists():
            add_manifest_violation(violations, "anchor source file missing", context=context, path=source)
            continue
        try:
            source_text = source_path.read_text(encoding="utf-8")
        except UnicodeDecodeError:
            add_manifest_violation(violations, "anchor source file is not UTF-8", context=context, path=source)
            continue
        for needle in anchor.get("needles") or []:
            if needle not in source_text:
                add_manifest_violation(
                    violations,
                    "anchor source missing required needle",
                    context=context,
                    path=source,
                    needle=needle,
                )

    return {
        "schema": "ee.dueling_wizards.no_silent_cap_shell_check.v1",
        "status": "pass" if not violations else "fail",
        "manifest": str(observability_manifest_rel),
        "subsystemCount": len(subsystem_ids),
        "capOperationCount": len(cap_operations),
        "violationCount": len(violations),
        "violations": violations,
    }

def read_manifest_json(root_path, rel_path, violations, label):
    path = root_path / rel_path
    if not path.exists():
        add_manifest_violation(violations, f"missing {label} manifest", path=str(rel_path))
        return None
    try:
        return json.loads(path.read_text(encoding="utf-8"))
    except json.JSONDecodeError as error:
        add_manifest_violation(
            violations,
            f"{label} manifest is not valid JSON",
            path=str(rel_path),
            detail=str(error),
        )
        return None

def validate_mesh_redaction_manifest(root_path, required):
    violations = []
    manifest_path = root_path / mesh_redaction_manifest_rel
    if not manifest_path.exists():
        if required:
            add_manifest_violation(
                violations,
                "missing dueling-wizards mesh redaction manifest",
                path=str(mesh_redaction_manifest_rel),
            )
            status = "fail"
        else:
            status = "skipped"
        return {
            "schema": "ee.dueling_wizards.mesh_redaction_shell_check.v1",
            "status": status,
            "manifest": str(mesh_redaction_manifest_rel),
            "violationCount": len(violations),
            "violations": violations,
        }

    manifest = read_manifest_json(root_path, mesh_redaction_manifest_rel, violations, "dueling-wizards mesh redaction")
    if manifest is None:
        return {
            "schema": "ee.dueling_wizards.mesh_redaction_shell_check.v1",
            "status": "fail",
            "manifest": str(mesh_redaction_manifest_rel),
            "violationCount": len(violations),
            "violations": violations,
        }

    expected_scalars = {
        "schema": "ee.dueling_wizards.mesh_redaction.v1",
        "initiativeBead": "bd-1n0np",
        "gateBead": "bd-1n0np.23.4",
        "manifestOwner": "tests/contracts/dueling_wizards_mesh_redaction.rs",
        "doc": "docs/agent-ux/dueling-wizards/mesh-redaction.md",
        "migrationRegistry": "tests/fixtures/contracts/dueling_wizards_migration_registry.json",
        "peerPolicyDoc": "docs/mesh/peer_policy.md",
        "sharePreviewSource": "src/policy/mod.rs",
    }
    for key, expected in expected_scalars.items():
        if manifest.get(key) != expected:
            add_manifest_violation(
                violations,
                "mesh redaction manifest scalar drifted",
                field=key,
                expected=expected,
                actual=manifest.get(key),
            )

    policy = manifest.get("policy") or {}
    for key in ["requiresPeerPolicy", "redactedPayloadRequiredWhenExportable", "rchProofRequiredForRustTests"]:
        if policy.get(key) is not True:
            add_manifest_violation(
                violations,
                "mesh redaction policy boolean must be true",
                field=f"policy.{key}",
                actual=policy.get(key),
            )
    for key in ["rawBodyExportAllowed", "rawEmbeddingExportAllowed", "payloadExportAllowed", "rawPayloadExportAllowed"]:
        if policy.get(key) is not False:
            add_manifest_violation(
                violations,
                "mesh redaction policy export boolean must be false",
                field=f"policy.{key}",
                actual=policy.get(key),
            )
    for key, expected in {
        "defaultPosture": "conservative_omit_or_hash",
        "localCargoProof": "invalid",
    }.items():
        if policy.get(key) != expected:
            add_manifest_violation(
                violations,
                "mesh redaction policy scalar drifted",
                field=f"policy.{key}",
                expected=expected,
                actual=policy.get(key),
            )

    expected_sets = {
        "allowedMaterialLanes": mesh_material_lanes,
        "allowedRedactionClasses": mesh_redaction_classes,
        "allowedExportPostures": mesh_export_postures,
    }
    for key, expected in expected_sets.items():
        actual = as_set(manifest.get(key))
        if actual != expected:
            add_manifest_violation(
                violations,
                "mesh redaction vocabulary set drifted",
                field=key,
                missing=sorted(expected - actual),
                extra=sorted(actual - expected),
            )

    registry_rel = Path(manifest.get("migrationRegistry") or "")
    registry = None
    if registry_rel == Path(""):
        add_manifest_violation(violations, "mesh redaction manifest missing migrationRegistry")
    elif registry_rel != Path("tests/fixtures/contracts/dueling_wizards_migration_registry.json"):
        add_manifest_violation(
            violations,
            "mesh redaction manifest points at unexpected migration registry",
            actual=str(registry_rel),
        )
    else:
        registry = read_manifest_json(root_path, registry_rel, violations, "dueling-wizards migration registry")

    allocations_by_id = {}
    planned_shape_by_id = {}
    expected_asset_kinds = set()
    if registry is not None:
        for index, allocation in enumerate(registry.get("allocations") or []):
            context = f"migrationRegistry.allocations[{index}]"
            allocation_id = allocation.get("id")
            backup_asset_kind = allocation.get("backupAssetKind")
            owner_bead = allocation.get("ownerBead")
            if not allocation_id or not backup_asset_kind or not owner_bead:
                add_manifest_violation(
                    violations,
                    "migration allocation missing id, backupAssetKind, or ownerBead",
                    context=context,
                )
                continue
            allocations_by_id[allocation_id] = {
                "backupAssetKind": backup_asset_kind,
                "ownerBead": owner_bead,
            }
            expected_asset_kinds.add(backup_asset_kind)
            if isinstance(allocation.get("plannedShape"), dict):
                planned_shape_by_id[allocation_id] = allocation["plannedShape"]

    field_class_by_asset_kind = {}
    for index, field_class in enumerate(manifest.get("fieldClasses") or []):
        context = f"fieldClasses[{index}]"
        asset_kind = field_class.get("assetKind")
        if not asset_kind:
            add_manifest_violation(violations, "field class missing assetKind", context=context)
            continue
        if asset_kind in field_class_by_asset_kind:
            add_manifest_violation(violations, "duplicate mesh redaction assetKind", assetKind=asset_kind)
        field_class_by_asset_kind[asset_kind] = field_class

        owner_beads = as_set(field_class.get("ownerBeads"))
        allocation_ids = as_set(field_class.get("migrationAllocationIds"))
        if not owner_beads or not allocation_ids:
            add_manifest_violation(
                violations,
                "field class ownerBeads and migrationAllocationIds must be non-empty",
                assetKind=asset_kind,
            )
        for allocation_id in allocation_ids:
            allocation = allocations_by_id.get(allocation_id)
            if allocation is None:
                add_manifest_violation(
                    violations,
                    "field class references unknown migration allocation",
                    assetKind=asset_kind,
                    allocationId=allocation_id,
                )
                continue
            if allocation["backupAssetKind"] != asset_kind:
                add_manifest_violation(
                    violations,
                    "field class allocation belongs to another backup asset kind",
                    assetKind=asset_kind,
                    allocationId=allocation_id,
                    actual=allocation["backupAssetKind"],
                )
            if allocation["ownerBead"] not in owner_beads:
                add_manifest_violation(
                    violations,
                    "field class ownerBeads missing registry owner",
                    assetKind=asset_kind,
                    ownerBead=allocation["ownerBead"],
                )

        for key, allowed in {
            "meshMaterialLane": mesh_material_lanes,
            "defaultRedactionClass": mesh_redaction_classes,
            "meshExportPosture": mesh_export_postures,
            "sharePreviewClass": mesh_redaction_classes,
        }.items():
            actual = field_class.get(key)
            if actual not in allowed:
                add_manifest_violation(
                    violations,
                    "field class has unsupported mesh redaction vocabulary",
                    assetKind=asset_kind,
                    field=key,
                    actual=actual,
                )
        if field_class.get("requiresPeerPolicy") is not True:
            add_manifest_violation(violations, "field class requiresPeerPolicy must be true", assetKind=asset_kind)
        for key in ["payloadExportAllowed", "rawPayloadExportAllowed", "rawBodyExportAllowed", "rawEmbeddingExportAllowed"]:
            if field_class.get(key) is not False:
                add_manifest_violation(
                    violations,
                    "field class export boolean must be false",
                    assetKind=asset_kind,
                    field=key,
                    actual=field_class.get(key),
                )

        redaction_class = field_class.get("defaultRedactionClass")
        export_posture = field_class.get("meshExportPosture")
        share_preview_class = field_class.get("sharePreviewClass")
        redacted_required = field_class.get("redactedPayloadRequired")
        if redaction_class == "omit":
            if export_posture != "deny" or share_preview_class != "omit" or redacted_required is not False:
                add_manifest_violation(
                    violations,
                    "omit field classes must deny export, omit previews, and not require redacted payloads",
                    assetKind=asset_kind,
                )
        elif redaction_class in {"hash", "redact"}:
            if export_posture != "redact" or redacted_required is not True:
                add_manifest_violation(
                    violations,
                    "hash/redact field classes must use redacted export posture and require redacted payloads",
                    assetKind=asset_kind,
                )
        if not str(field_class.get("sourceValueHandling") or "").strip():
            add_manifest_violation(violations, "field class sourceValueHandling must not be empty", assetKind=asset_kind)
        if not str(field_class.get("reason") or "").strip():
            add_manifest_violation(violations, "field class reason must not be empty", assetKind=asset_kind)

    actual_asset_kinds = set(field_class_by_asset_kind)
    if expected_asset_kinds and actual_asset_kinds != expected_asset_kinds:
        add_manifest_violation(
            violations,
            "mesh redaction asset kind set drifted",
            missing=sorted(expected_asset_kinds - actual_asset_kinds),
            extra=sorted(actual_asset_kinds - expected_asset_kinds),
        )

    memory_anchor_class = field_class_by_asset_kind.get("memory_anchors")
    if memory_anchor_class is None:
        add_manifest_violation(violations, "mesh redaction manifest missing memory_anchors field class")
    else:
        contract = memory_anchor_class.get("anchorValueContract") or {}
        if contract.get("rawAnchorValuesAllowed") is not False:
            add_manifest_violation(violations, "memory_anchors raw anchor values must be forbidden")
        for key, expected in {
            "meshValueMaterialPolicy": "hash_or_redacted_anchor_value_only",
            "sharePreviewValue": "hash_only",
            "peerPolicyEscalation": "required_for_any_raw_value_or_payload_lane_change",
        }.items():
            if contract.get(key) != expected:
                add_manifest_violation(
                    violations,
                    "memory_anchors anchorValueContract scalar drifted",
                    field=key,
                    expected=expected,
                    actual=contract.get(key),
                )
        allowed_fields = as_set(contract.get("allowedValueFields"))
        for required_field in ["anchor_value_hash", "redacted_anchor_value"]:
            if required_field not in allowed_fields:
                add_manifest_violation(
                    violations,
                    "memory_anchors allowedValueFields missing required field",
                    field=required_field,
                )
        forbidden_fields = as_set(contract.get("forbiddenOutboundFields"))
        for forbidden_field in mesh_anchor_forbidden_fields:
            if forbidden_field not in forbidden_fields:
                add_manifest_violation(
                    violations,
                    "memory_anchors forbiddenOutboundFields missing raw field",
                    field=forbidden_field,
                )
            if forbidden_field in allowed_fields:
                add_manifest_violation(
                    violations,
                    "memory_anchors allowedValueFields includes raw field",
                    field=forbidden_field,
                )
        planned_shape = planned_shape_by_id.get("memory_anchors") or {}
        for key, expected in {
            "anchorValueStorage": "hash_required_raw_value_forbidden",
            "meshExport": "redacted_or_hashed_values_only",
        }.items():
            if planned_shape.get(key) != expected:
                add_manifest_violation(
                    violations,
                    "memory_anchors plannedShape scalar drifted",
                    field=key,
                    expected=expected,
                    actual=planned_shape.get(key),
                )

    covered_redaction_classes = set()
    covered_export_postures = set()
    example_ids = set()
    for index, example in enumerate(manifest.get("outboundDecisionExamples") or []):
        context = f"outboundDecisionExamples[{index}]"
        example_id = example.get("exampleId")
        if not str(example_id or "").strip() or example_id in example_ids:
            add_manifest_violation(violations, "outbound decision example id must be unique and non-empty", context=context)
        example_ids.add(example_id)
        asset_kind = example.get("assetKind")
        field_class = field_class_by_asset_kind.get(asset_kind)
        if field_class is None:
            add_manifest_violation(violations, "outbound decision example references unknown assetKind", context=context, assetKind=asset_kind)
            continue
        if not str(example.get("requestedMaterial") or "").strip():
            add_manifest_violation(violations, "outbound decision example requestedMaterial must not be empty", context=context)
        for key in ["defaultRedactionClass", "meshExportPosture", "sharePreviewClass"]:
            if example.get(key) != field_class.get(key):
                add_manifest_violation(
                    violations,
                    "outbound decision example must match field class scalar",
                    context=context,
                    field=key,
                    expected=field_class.get(key),
                    actual=example.get(key),
                )
        for key in ["payloadExportAllowed", "rawPayloadExportAllowed", "rawBodyExportAllowed", "rawEmbeddingExportAllowed", "redactedPayloadRequired"]:
            if example.get(key) != field_class.get(key):
                add_manifest_violation(
                    violations,
                    "outbound decision example must match field class boolean",
                    context=context,
                    field=key,
                    expected=field_class.get(key),
                    actual=example.get(key),
                )
        for key in ["payloadExportAllowed", "rawPayloadExportAllowed", "rawBodyExportAllowed", "rawEmbeddingExportAllowed"]:
            if example.get(key) is not False:
                add_manifest_violation(
                    violations,
                    "outbound decision example export boolean must be false",
                    context=context,
                    field=key,
                    actual=example.get(key),
                )
        redaction_class = example.get("defaultRedactionClass")
        export_posture = example.get("meshExportPosture")
        covered_redaction_classes.add(redaction_class)
        covered_export_postures.add(export_posture)
        decision = example.get("decision")
        if decision == "deny":
            if export_posture != "deny" or redaction_class != "omit" or example.get("redactedPayloadRequired") is not False:
                add_manifest_violation(violations, "deny examples must omit, deny, and not require redacted payloads", context=context)
        elif decision == "redact":
            if export_posture != "redact" or example.get("redactedPayloadRequired") is not True:
                add_manifest_violation(violations, "redact examples must require redacted export posture", context=context)
        else:
            add_manifest_violation(violations, "outbound decision example has unsupported decision", context=context, decision=decision)
        if asset_kind == "memory_anchors":
            contract = field_class.get("anchorValueContract") or {}
            if example.get("peerPolicyEscalation") != contract.get("peerPolicyEscalation"):
                add_manifest_violation(violations, "memory_anchors example peerPolicyEscalation drifted", context=context)

    if covered_redaction_classes != mesh_redaction_classes:
        add_manifest_violation(
            violations,
            "outbound decision examples must cover every redaction class",
            missing=sorted(mesh_redaction_classes - covered_redaction_classes),
            extra=sorted(covered_redaction_classes - mesh_redaction_classes),
        )
    if covered_export_postures != mesh_export_postures:
        add_manifest_violation(
            violations,
            "outbound decision examples must cover every export posture",
            missing=sorted(mesh_export_postures - covered_export_postures),
            extra=sorted(covered_export_postures - mesh_export_postures),
        )

    for rel_path, anchors, label in [
        (Path(manifest.get("peerPolicyDoc") or ""), mesh_peer_policy_anchors, "peer policy doc"),
        (Path(manifest.get("sharePreviewSource") or ""), mesh_share_preview_anchors, "share preview source"),
    ]:
        if rel_path == Path(""):
            add_manifest_violation(violations, f"mesh redaction manifest missing {label} path")
            continue
        target = root_path / rel_path
        if not target.exists():
            add_manifest_violation(violations, f"mesh redaction {label} is missing", path=str(rel_path))
            continue
        try:
            text = target.read_text(encoding="utf-8")
        except UnicodeDecodeError:
            add_manifest_violation(violations, f"mesh redaction {label} is not UTF-8", path=str(rel_path))
            continue
        for anchor in anchors:
            if anchor not in text:
                add_manifest_violation(
                    violations,
                    f"mesh redaction {label} missing required anchor",
                    path=str(rel_path),
                    anchor=anchor,
                )

        if label == "share preview source":
            for forbidden in sorted(mesh_share_preview_forbidden_oracles):
                if forbidden in text:
                    add_manifest_violation(
                        violations,
                        f"mesh redaction {label} must not expose a public share-preview oracle",
                        path=str(rel_path),
                        forbiddenOracle=forbidden,
                    )

    return {
        "schema": "ee.dueling_wizards.mesh_redaction_shell_check.v1",
        "status": "pass" if not violations else "fail",
        "manifest": str(mesh_redaction_manifest_rel),
        "fieldClassCount": len(field_class_by_asset_kind),
        "outboundDecisionExampleCount": len(example_ids),
        "violationCount": len(violations),
        "violations": violations,
    }

def load_beads(path):
    beads = []
    for line in path.read_text(encoding="utf-8").splitlines():
        if line.strip():
            beads.append(json.loads(line))
    return beads

def surfaces(bead):
    found = []
    for label in bead.get("labels") or []:
        if label.startswith("implements-surface:"):
            found.append(label.removeprefix("implements-surface:"))
    match = re.search(r"\[implements-surface:([^\]]+)\]", bead.get("title") or "")
    if match:
        found.append(match.group(1))
    match = re.search(r"\bimplements-surface:([A-Za-z0-9_.-]+)", bead.get("title") or "")
    if match:
        found.append(match.group(1))
    return sorted(set(found))

def is_part_ii_implementation(bead):
    bead_id = bead.get("id") or ""
    return bead_id == "bd-3usjw" or bead_id.startswith("bd-3usjw.")

def declared_file_surfaces(bead):
    text = "\n".join([bead.get("description") or "", bead.get("notes") or ""])
    paths = []
    for line in text.splitlines():
        if not line.startswith("FILE SURFACE:"):
            continue
        rest = line.split(":", 1)[1]
        for raw in rest.split(","):
            token = raw.strip().strip("`")
            token = re.split(r"\s+", token)[0].strip("`")
            if token:
                paths.append(token)
    return paths

def tracing_decl(text):
    match = re.search(r"(?im)^TRACING:\s*(.+(?:\n(?![A-Z][A-Z _-]*:).+)*)", text)
    return match.group(0) if match else ""

def missing_decl_fields(decl):
    return [field for field in required_fields if field not in decl]

# Minimum convention fields ONE emitted event must carry. Deliberately the
# same number the previous substring check used, so this change alters the
# PREDICATE (file mentions -> event carries) without silently re-calibrating
# strictness at the same time.
#
# Measured across every src file that emits events (2026-09-16), the
# population is strongly bimodal -- best-event convention-key counts are
# 0:20 files, 1:14, 2:1, 3:3, 4:0, 5:0, 6:3, 7:13. A surface either follows
# the convention (6-7) or does not (0-1); almost nothing sits between.
#
# Consequence: any threshold in 3..5 flags the SAME bd-3usjw bead set, so the
# audited result is insensitive to the exact number. It is NOT free at file
# granularity, though -- raising to 4 would newly fail src/core/search.rs,
# src/db/read_pool.rs and src/search/lexical_ram_tier.rs, which sit at exactly
# 3 (workspace_id + elapsed_ms + degraded_codes: partial but real
# conformance). They pass today only because no audited bead declares them.
# Raise this deliberately, not incidentally.
MIN_EVENT_REQUIRED_FIELDS = 3

EVENT_MACRO_NAMES = (
    "event", "info", "warn", "error", "debug", "trace",
    "info_span", "warn_span", "error_span", "debug_span", "trace_span", "span",
)
event_macro_re = re.compile(
    r"(?:tracing::)?(?:" + "|".join(EVENT_MACRO_NAMES) + r")!\s*\("
)
instrument_re = re.compile(r"#\[(?:tracing::)?instrument\s*\(")
# `key = value`, including the `%key =` / `?key =` sigil forms and dotted keys.
event_key_re = re.compile(
    r"(?:^|[,(\s])\??%?([A-Za-z_][A-Za-z0-9_]*(?:\.[A-Za-z_][A-Za-z0-9_]*)*)\s*=(?!=)"
)

def blank_rust_noise(src):
    """Blank comments and string/char literals so paren matching is safe.

    Length is preserved (every removed byte becomes a space) so offsets stay
    valid. Without this a `//` comment or a `")"` inside a message string
    would terminate an event body early and hide its fields.
    """
    out = list(src)
    index = 0
    length = len(src)
    while index < length:
        char = src[index]
        if char == "/" and index + 1 < length and src[index + 1] == "/":
            while index < length and src[index] != "\n":
                out[index] = " "
                index += 1
        elif char == "/" and index + 1 < length and src[index + 1] == "*":
            depth = 1
            out[index] = out[index + 1] = " "
            index += 2
            while index < length and depth:
                if src[index] == "/" and index + 1 < length and src[index + 1] == "*":
                    depth += 1
                    out[index] = out[index + 1] = " "
                    index += 2
                    continue
                if src[index] == "*" and index + 1 < length and src[index + 1] == "/":
                    depth -= 1
                    out[index] = out[index + 1] = " "
                    index += 2
                    continue
                if src[index] != "\n":
                    out[index] = " "
                index += 1
        elif char == "r" and index + 1 < length and src[index + 1] in "#\"":
            cursor = index + 1
            hashes = 0
            while cursor < length and src[cursor] == "#":
                hashes += 1
                cursor += 1
            if cursor < length and src[cursor] == '"':
                terminator = '"' + "#" * hashes
                end = src.find(terminator, cursor + 1)
                if end == -1:
                    end = length
                for pos in range(index, min(end + len(terminator), length)):
                    if src[pos] != "\n":
                        out[pos] = " "
                index = end + len(terminator)
                continue
            index += 1
        elif char == '"':
            out[index] = " "
            index += 1
            while index < length:
                if src[index] == "\\":
                    for pos in (index, index + 1):
                        if pos < length and src[pos] != "\n":
                            out[pos] = " "
                    index += 2
                    continue
                if src[index] == '"':
                    out[index] = " "
                    index += 1
                    break
                if src[index] != "\n":
                    out[index] = " "
                index += 1
        elif char == "'":
            # Char literal, or a lifetime like `'a` which must be left alone.
            match = re.match(r"'(?:\\.|[^\\'])'", src[index:])
            if match:
                for pos in range(index, index + match.end()):
                    out[pos] = " "
                index += match.end()
            else:
                index += 1
        else:
            index += 1
    return "".join(out)

surface_literal_re = re.compile(r'surface\s*=\s*"([A-Za-z0-9_.-]+)"')

def emitted_events(path):
    """`(field_keys, surface_literal)` for each event / span / #[instrument].

    `surface_literal` is the string literal assigned to `surface`, or None
    when the surface is passed at runtime (`surface,` shorthand over a
    function parameter, as src/core/why.rs does). Literals are read from the
    RAW text because `blank_rust_noise` deliberately erases string bodies.
    """
    try:
        raw = path.read_text(encoding="utf-8")
    except (UnicodeDecodeError, OSError):
        return []
    source = blank_rust_noise(raw)
    events = []
    for pattern in (event_macro_re, instrument_re):
        for match in pattern.finditer(source):
            open_paren = match.end() - 1
            depth = 0
            cursor = open_paren
            while cursor < len(source):
                if source[cursor] == "(":
                    depth += 1
                elif source[cursor] == ")":
                    depth -= 1
                    if depth == 0:
                        break
                cursor += 1
            body = source[open_paren + 1:cursor]
            keys = set(event_key_re.findall(body))
            # `tracing::info!(workspace_id, ...)` shorthand means
            # `workspace_id = workspace_id`. Only honoured for the convention
            # names so an unrelated positional expression cannot be miscounted.
            for token in re.split(r"[,()]", body):
                token = token.strip()
                if token in required_fields:
                    keys.add(token)
            literal = surface_literal_re.search(raw[open_paren + 1:cursor])
            events.append((keys, literal.group(1) if literal else None))
    return events

def source_has_tracing_evidence(path, expected_surface):
    """True when an event in `path` is evidence for `expected_surface`.

    Two failure modes have to be avoided at once, and neither rule alone does
    it:

      * Asking only "does this FILE contain the vocabulary" is satisfied by a
        doc comment -- src/steward/mod.rs passed while every event it emits
        carries memory_id / freshness / confidence / reason (bd-ti1zt).
      * Asking only "does SOME event here carry the fields" is satisfied by a
        neighbour. src/cli/mod.rs hosts dozens of surfaces, so one conformant
        event would silently vouch for every bead that happens to declare that
        file -- bd-3usjw.1 (db_inspect) would go green on an event belonging
        to a different subcommand entirely.
      * Requiring a literal `surface = "<name>"` match would in turn punish a
        well-factored helper that takes the surface as a parameter, which is
        exactly what src/core/why.rs:1162 does -- a false negative against
        good code.

    So: an event is evidence when it carries at least
    MIN_EVENT_REQUIRED_FIELDS convention fields AND either names this bead's
    surface literally, or the file names no surface literally at all (a
    parameterised helper, which cannot be attributed statically and is not
    penalised for it).
    """
    events = emitted_events(path)
    if not events:
        return False, required_fields
    required = set(required_fields)
    named_surfaces = {literal for _, literal in events if literal}
    attributable = [
        keys
        for keys, literal in events
        if not named_surfaces or literal == expected_surface
    ]
    if not attributable:
        return False, required_fields
    best = max(attributable, key=lambda keys: len(keys & required))
    hits = best & required
    return (
        len(hits) >= MIN_EVENT_REQUIRED_FIELDS,
        [field for field in required_fields if field not in hits],
    )

beads = load_beads(beads_path)
violations = []
audited = 0

for bead in beads:
    if bead_filter and bead.get("id") != bead_filter:
        continue
    impl_surfaces = surfaces(bead)
    if not impl_surfaces or not is_part_ii_implementation(bead):
        continue
    audited += 1
    bead_id = bead.get("id") or "<unknown>"
    text = "\n".join([bead.get("description") or "", bead.get("notes") or ""])
    decl = tracing_decl(text)
    if not decl:
        violations.append({
            "bead": bead_id,
            "surface": impl_surfaces[0],
            "reason": "missing TRACING paragraph",
        })
    else:
        missing = missing_decl_fields(decl)
        if missing:
            violations.append({
                "bead": bead_id,
                "surface": impl_surfaces[0],
                "reason": "TRACING paragraph missing required fields",
                "missingFields": missing,
            })
        if not any(phase in decl for phase in phase_names):
            violations.append({
                "bead": bead_id,
                "surface": impl_surfaces[0],
                "reason": "TRACING paragraph does not name any standard phase",
            })

    # Structured tracing is emitted at a surface's DISPATCH BOUNDARY, not in
    # every file a bead happened to touch. `FILE SURFACE:` is a change
    # manifest -- new leaf modules, extended dispatch files, schemas, tests,
    # docs -- so requiring every declared Rust file to carry tracing demanded
    # request-scoped fields (workspace_id, request_id, elapsed_ms) from two
    # kinds of file that structurally cannot supply them:
    #
    #   * tests/** and benches/** -- a benchmark has no request and no
    #     workspace; instrumenting one would measure the harness, not ee.
    #   * pure leaf helpers -- e.g. src/util/radix_ulid_sort.rs (integer-only
    #     hot-path sort) or src/core/influence.rs, which documents that it
    #     "keeps the counterfactual math independent from storage". Threading
    #     a request context into those to satisfy a grep would damage the
    #     purity their own contracts declare.
    #
    # The obligation is therefore per-BEAD, not per-file: the surface must be
    # traced somewhere in its own declared production code. A bead that
    # declares no production Rust file (a docs/CI/test-harness surface) has no
    # runtime boundary to instrument.
    runtime_candidates = []
    for declared in declared_file_surfaces(bead):
        if not declared.endswith(".rs") or "*" in declared or "?" in declared:
            continue
        if not declared.startswith("src/"):
            continue
        path = root / declared
        if not path.exists():
            continue
        runtime_candidates.append((declared, path))

    if runtime_candidates:
        evidence = [
            (name, source_has_tracing_evidence(path, impl_surfaces[0]))
            for name, path in runtime_candidates
        ]
        if not any(ok for _, (ok, _) in evidence):
            # Report the file that came CLOSEST, not the union of every miss.
            # A bead declaring both src/cli/mod.rs (0 convention keys) and
            # src/db/mod.rs (carries `phase`) should point at db/mod.rs and
            # name the six fields actually outstanding there, otherwise the
            # report sends the reader to the wrong file with a wrong list.
            closest_name, (_, closest_missing) = min(
                evidence, key=lambda item: len(item[1][1])
            )
            violations.append({
                "bead": bead_id,
                "surface": impl_surfaces[0],
                "path": closest_name,
                "declaredRuntimeFiles": [name for name, _ in runtime_candidates],
                "reason": "no declared production FILE SURFACE carries structured tracing evidence",
                "missingFields": closest_missing,
            })

observability_report = validate_observability_manifest(root, manifest_required)
mesh_redaction_report = validate_mesh_redaction_manifest(root, manifest_required)
total_violations = (
    len(violations)
    + int(observability_report["violationCount"])
    + int(mesh_redaction_report["violationCount"])
)

report = {
    "schema": "ee.tracing_field_report.v1",
    "status": "pass" if total_violations == 0 else "fail",
    "auditedBeads": audited,
    "violationCount": total_violations,
    "requiredFields": required_fields,
    "standardPhases": sorted(phase_names),
    "duelingWizardsNoSilentCap": observability_report,
    "duelingWizardsMeshRedaction": mesh_redaction_report,
    "violations": violations,
}
print(json.dumps(report, sort_keys=True, separators=(",", ":")))
PY
}

require_tool python3
require_tool jq

# Read the accepted-violation baseline: one bead id per line, `#` comments and
# blank lines ignored. A MISSING file yields an empty baseline, which is strictly
# STRICTER (every violation counts as new) rather than silently permissive -- a
# baseline that vanishes must not turn the gate green.
read_violation_baseline() {
    local file="$1"
    [ -f "$file" ] || return 0
    sed -e 's/#.*//' "$file" | tr -d '[:blank:]' | sed '/^$/d' | sort -u
}

# The bead ids carrying at least one tracing violation in a report.
report_violation_beads() {
    printf '%s\n' "$1" | jq -r '.violations[]?.bead' | sort -u
}

# Pure classifier: baseline vs current, both newline-separated. Emits `new <id>`
# for a violation nobody accepted and `stale <id>` for an accepted entry that now
# passes. Kept string-in/string-out so the self-test can prove it without a
# workspace.
classify_violation_baseline() {
    local baseline current
    baseline="$(printf '%s\n' "$1" | sed '/^$/d' | sort -u)"
    current="$(printf '%s\n' "$2" | sed '/^$/d' | sort -u)"
    comm -13 <(printf '%s\n' "$baseline") <(printf '%s\n' "$current") \
        | sed '/^$/d' | sed 's/^/new /'
    comm -23 <(printf '%s\n' "$baseline") <(printf '%s\n' "$current") \
        | sed '/^$/d' | sed 's/^/stale /'
}

if [ "$SELF_TEST" = true ]; then
    tmp_dir=$(mktemp -d "${TMPDIR:-/tmp}/ee-tracing-fields.XXXXXX")
    cat > "$tmp_dir/issues.jsonl" <<'JSONL'
{"id":"bd-3usjw.good","title":"[implements-surface:good_surface] example","labels":["implements-surface:good_surface"],"description":"FILE SURFACE: src/good.rs\nTRACING: surface=good_surface, phases=input|dispatch|response, fields=workspace_id,request_id,bead_id,surface,phase,elapsed_ms,degraded_codes."}
{"id":"bd-3usjw.bad","title":"[implements-surface:bad_surface] example","labels":["implements-surface:bad_surface"],"description":"FILE SURFACE: src/bad.rs"}
{"id":"bd-3usjw.dispatch","title":"[implements-surface:dispatch_surface] example","labels":["implements-surface:dispatch_surface"],"description":"FILE SURFACE: src/leaf.rs, src/dispatch.rs\nTRACING: surface=dispatch_surface, phases=input|dispatch|response, fields=workspace_id,request_id,bead_id,surface,phase,elapsed_ms,degraded_codes."}
{"id":"bd-3usjw.harness","title":"[implements-surface:harness_surface] example","labels":["implements-surface:harness_surface"],"description":"FILE SURFACE: tests/harness.rs, benches/harness.rs\nTRACING: surface=harness_surface, phases=input|dispatch|response, fields=workspace_id,request_id,bead_id,surface,phase,elapsed_ms,degraded_codes."}
{"id":"bd-3usjw.untraced","title":"[implements-surface:untraced_surface] example","labels":["implements-surface:untraced_surface"],"description":"FILE SURFACE: src/untraced.rs\nTRACING: surface=untraced_surface, phases=input|dispatch|response, fields=workspace_id,request_id,bead_id,surface,phase,elapsed_ms,degraded_codes."}
{"id":"bd-3usjw.mentions","title":"[implements-surface:mentions_surface] example","labels":["implements-surface:mentions_surface"],"description":"FILE SURFACE: src/mentions.rs\nTRACING: surface=mentions_surface, phases=input|dispatch|response, fields=workspace_id,request_id,bead_id,surface,phase,elapsed_ms,degraded_codes."}
{"id":"bd-3usjw.neighbour","title":"[implements-surface:neighbour_surface] example","labels":["implements-surface:neighbour_surface"],"description":"FILE SURFACE: src/shared_host.rs\nTRACING: surface=neighbour_surface, phases=input|dispatch|response, fields=workspace_id,request_id,bead_id,surface,phase,elapsed_ms,degraded_codes."}
{"id":"bd-3usjw.parameterised","title":"[implements-surface:parameterised_surface] example","labels":["implements-surface:parameterised_surface"],"description":"FILE SURFACE: src/parameterised.rs\nTRACING: surface=parameterised_surface, phases=input|dispatch|response, fields=workspace_id,request_id,bead_id,surface,phase,elapsed_ms,degraded_codes."}
JSONL
    mkdir -p "$tmp_dir/src"
    cat > "$tmp_dir/src/good.rs" <<'RS'
fn demo() {
    tracing::info!(
        workspace_id = "wsp",
        request_id = "req",
        surface = "good_surface",
        phase = "response",
        elapsed_ms = 1_u64,
        degraded_codes = ?Vec::<String>::new(),
        "done"
    );
}
RS
    cat > "$tmp_dir/src/bad.rs" <<'RS'
fn demo() {}
RS
    # A surface traced at its dispatch boundary: the pure leaf carries no
    # tracing and must not be required to.
    cat > "$tmp_dir/src/leaf.rs" <<'RS'
pub fn pure_helper(values: &mut [u8]) {
    values.sort_unstable();
}
RS
    cat > "$tmp_dir/src/dispatch.rs" <<'RS'
fn demo() {
    tracing::info!(
        workspace_id = "wsp",
        request_id = "req",
        surface = "dispatch_surface",
        phase = "response",
        elapsed_ms = 1_u64,
        degraded_codes = ?Vec::<String>::new(),
        "done"
    );
}
RS
    # A docs/CI/test-harness surface: no production Rust file at all.
    mkdir -p "$tmp_dir/tests" "$tmp_dir/benches"
    cat > "$tmp_dir/tests/harness.rs" <<'RS'
#[test]
fn harness() {}
RS
    cat > "$tmp_dir/benches/harness.rs" <<'RS'
fn bench() {}
RS
    # A genuinely untraced production surface must STILL fail.
    cat > "$tmp_dir/src/untraced.rs" <<'RS'
pub fn handler() {}
RS
    # bd-ti1zt regression guard, in miniature: this file MENTIONS every
    # convention field in prose and calls tracing::, so the old substring
    # predicate passed it -- but the event it actually emits carries none of
    # them. This is src/steward/mod.rs reduced to eight lines. It must FAIL.
    cat > "$tmp_dir/src/mentions.rs" <<'RS'
//! Emits workspace_id, request_id, bead_id, surface, phase, elapsed_ms and
//! degraded_codes. (It does not. That is the point of this fixture.)
pub fn handler() {
    tracing::info!(
        memory_id = "mem",
        freshness = 1.0,
        confidence = 0.5,
        "memory demoted by decay"
    );
}
RS
    # A file hosting MANY surfaces: it emits a fully conformant event, but for
    # someone else's surface. bd-3usjw.neighbour must NOT be able to claim it.
    # This is src/cli/mod.rs in miniature.
    cat > "$tmp_dir/src/shared_host.rs" <<'RS'
pub fn other_subcommand() {
    tracing::info!(
        workspace_id = "wsp",
        request_id = "req",
        bead_id = "bd-other",
        surface = "some_other_surface",
        phase = "response",
        elapsed_ms = 1_u64,
        degraded_codes = ?Vec::<String>::new(),
        "a neighbouring surface, not neighbour_surface"
    );
}
RS
    # A well-factored helper that takes the surface as a PARAMETER (what
    # src/core/why.rs does). Nothing can attribute it statically, so it must
    # NOT be punished for that: bd-3usjw.parameterised passes.
    cat > "$tmp_dir/src/parameterised.rs" <<'RS'
pub fn trace_checkpoint(workspace_id: &str, surface: &'static str, phase: &'static str) {
    tracing::info!(
        workspace_id = %workspace_id,
        request_id = "req",
        bead_id = "bd-param",
        surface,
        phase,
        elapsed_ms = 1_u64,
        degraded_codes = ?Vec::<String>::new(),
        "parameterised surface checkpoint"
    );
}
RS
    report=$(run_checker "$tmp_dir/issues.jsonl" "$tmp_dir" "" "false")
    printf '%s\n' "$report" > "$tmp_dir/self-test-report.json"
    if ! printf '%s\n' "$report" | jq -e '.status == "fail" and .violationCount == 5' >/dev/null; then
        echo "error: self-test expected five violations" >&2
        printf '%s\n' "$report" >&2
        exit 1
    fi
    # bd-3usjw.dispatch: leaf untraced but dispatch boundary traced -> clean.
    if ! printf '%s\n' "$report" | jq -e '[.violations[] | select(.bead == "bd-3usjw.dispatch")] | length == 0' >/dev/null; then
        echo "error: a surface traced at its dispatch boundary must not be flagged for its pure leaf module" >&2
        printf '%s\n' "$report" >&2
        exit 1
    fi
    # bd-3usjw.harness: tests/** and benches/** are not runtime surfaces.
    if ! printf '%s\n' "$report" | jq -e '[.violations[] | select(.bead == "bd-3usjw.harness")] | length == 0' >/dev/null; then
        echo "error: tests/benches must not be required to emit request-scoped tracing fields" >&2
        printf '%s\n' "$report" >&2
        exit 1
    fi
    # bd-3usjw.untraced: the rule must still catch a real gap.
    if ! printf '%s\n' "$report" | jq -e '[.violations[] | select(.bead == "bd-3usjw.untraced")] | length == 1' >/dev/null; then
        echo "error: a production surface with no tracing evidence must still fail" >&2
        printf '%s\n' "$report" >&2
        exit 1
    fi
    # bd-ti1zt: mentioning the vocabulary must NOT count as emitting it.
    if ! printf '%s\n' "$report" | jq -e '[.violations[] | select(.bead == "bd-3usjw.mentions")] | length == 1' >/dev/null; then
        echo "error: a file that only MENTIONS the convention fields must not pass; the predicate has regressed to a substring check" >&2
        printf '%s\n' "$report" >&2
        exit 1
    fi
    # A conformant event for a DIFFERENT surface must not vouch for this bead.
    if ! printf '%s\n' "$report" | jq -e '[.violations[] | select(.bead == "bd-3usjw.neighbour")] | length == 1' >/dev/null; then
        echo "error: a bead must not claim a conformant event belonging to another surface in the same file" >&2
        printf '%s\n' "$report" >&2
        exit 1
    fi
    # A helper taking the surface as a parameter must still count as evidence.
    if ! printf '%s\n' "$report" | jq -e '[.violations[] | select(.bead == "bd-3usjw.parameterised")] | length == 0' >/dev/null; then
        echo "error: a parameterised tracing helper must not be penalised for not naming its surface literally" >&2
        printf '%s\n' "$report" >&2
        exit 1
    fi
    # bd-c79fk: the baseline ratchet is itself gated. A baseline that only
    # suppressed findings would reproduce this bead in a new place, so the
    # stale direction is pinned as hard as the new direction.
    baseline_case() {
        local label="$1" baseline="$2" current="$3" expected="$4"
        local actual
        actual="$(classify_violation_baseline "$baseline" "$current" | tr '\n' ' ' | sed 's/ $//')"
        if [ "$actual" = "$expected" ]; then
            echo "ok   - $label"
        else
            echo "FAIL - $label: got '$actual', wanted '$expected'" >&2
            exit 2
        fi
    }
    baseline_case "an accepted violation stays accepted" \
        "bd-a"$'\n'"bd-b" "bd-a"$'\n'"bd-b" ""
    baseline_case "a NEW violation is reported" \
        "bd-a" "bd-a"$'\n'"bd-c" "new bd-c"
    baseline_case "a baselined bead that now passes is reported as stale" \
        "bd-a"$'\n'"bd-b" "bd-a" "stale bd-b"
    baseline_case "both directions report together" \
        "bd-a"$'\n'"bd-b" "bd-a"$'\n'"bd-c" "new bd-c stale bd-b"
    baseline_case "an empty baseline makes every violation new" \
        "" "bd-a" "new bd-a"
    baseline_case "a clean tree against an empty baseline is silent" \
        "" "" ""

    echo "ok: tracing field checker self-test passed"
    exit 0
fi

if [ ! -f "$BEADS_FILE" ]; then
    echo "error: missing $BEADS_FILE" >&2
    exit 3
fi

if [ ! -f "$DOC_PATH" ]; then
    echo "error: missing $DOC_PATH" >&2
    exit 3
fi

report=$(run_checker "$BEADS_FILE" "$ROOT" "$BEAD_FILTER" "true")
printf '%s\n' "$report" > "$REPORT_FILE"

if [ "$JSON_OUTPUT" = true ]; then
    printf '%s\n' "$report"
else
    status=$(printf '%s\n' "$report" | jq -r '.status')
    audited=$(printf '%s\n' "$report" | jq -r '.auditedBeads')
    violations=$(printf '%s\n' "$report" | jq -r '.violationCount')
    echo "Tracing field report -> .tracing-field-report.json"
    echo "  raw_status: $status"
    echo "  audited_beads: $audited"
    echo "  violations: $violations"
    # `raw_status` is the checker's own verdict over the tree and stays `fail`
    # while ANY debt exists. The gate's verdict is the baseline comparison
    # below, printed after it is computed, so a reader never sees a bare
    # "status: fail" next to an exit code of 0.
    echo "  baseline: tests/fixtures/tracing_field/violation_baseline.txt"
fi

# Violations outside the tracing block (the dueling-wizards manifest checks)
# are never baselined here and still fail outright.
total_violations=$(printf '%s\n' "$report" | jq -r '.violationCount // 0')
tracing_violations=$(printf '%s\n' "$report" | jq -r '.violations | length')
other_violations=$((total_violations - tracing_violations))
if [ "$other_violations" -gt 0 ]; then
    echo "check-tracing-fields: $other_violations non-tracing violation(s); see $REPORT_FILE" >&2
    exit 1
fi

# A single-bead audit cannot tell "fixed" from "not audited", so it keeps the
# original all-or-nothing verdict rather than consulting the baseline.
if [ -n "$BEAD_FILTER" ]; then
    if printf '%s\n' "$report" | jq -e '.status == "pass"' >/dev/null; then
        exit 0
    fi
    exit 1
fi

classification="$(classify_violation_baseline \
    "$(read_violation_baseline "$BASELINE_FILE")" \
    "$(report_violation_beads "$report")")"
new_violations="$(printf '%s\n' "$classification" | sed -n 's/^new //p' | sed '/^$/d')"
stale_baseline="$(printf '%s\n' "$classification" | sed -n 's/^stale //p' | sed '/^$/d')"

status=0
if [ -n "$new_violations" ]; then
    echo "check-tracing-fields: NEW tracing violation(s) not in the baseline:" >&2
    printf '  %s\n' $new_violations >&2
    echo "  Instrument the surface, or justify it and add the id to" >&2
    echo "  tests/fixtures/tracing_field/violation_baseline.txt with a reason." >&2
    status=1
fi
if [ -n "$stale_baseline" ]; then
    echo "check-tracing-fields: baseline entr(ies) that now PASS — delete the line(s):" >&2
    printf '  %s\n' $stale_baseline >&2
    echo "  The baseline may only shrink; a stale entry is how it decays into a" >&2
    echo "  permanent ignore-list." >&2
    status=1
fi
if [ "$status" -eq 0 ] && [ "$JSON_OUTPUT" != true ]; then
    accepted=$(read_violation_baseline "$BASELINE_FILE" | wc -l | tr -d ' ')
    echo "  gate: PASS — all $accepted violation(s) are accepted baseline debt," \
        "none new, none stale"
fi
exit "$status"
