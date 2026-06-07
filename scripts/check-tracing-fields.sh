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

JSON_OUTPUT=false
SELF_TEST=false
BEAD_FILTER=""

usage() {
    cat <<'USAGE'
Usage: scripts/check-tracing-fields.sh [--json] [--bead ID] [--self-test]

  --json       Emit the JSON report to stdout.
  --bead ID    Audit one bead instead of every Part II implements-surface bead.
  --self-test  Run synthetic checker tests without reading the workspace.

Writes:
  .tracing-field-report.json

Exit codes:
  0  pass
  1  tracing convention violations found
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

def source_has_tracing_evidence(path):
    try:
        content = path.read_text(encoding="utf-8")
    except UnicodeDecodeError:
        return False, required_fields
    has_tracing_call = "tracing::" in content or "#[instrument" in content or "#[tracing::instrument" in content
    field_hits = [field for field in required_fields if field in content]
    if not has_tracing_call:
        return False, required_fields
    return len(field_hits) >= 3, [field for field in required_fields if field not in field_hits]

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

    for declared in declared_file_surfaces(bead):
        if not declared.endswith(".rs") or "*" in declared or "?" in declared:
            continue
        path = root / declared
        if not path.exists():
            continue
        ok, missing = source_has_tracing_evidence(path)
        if not ok:
            violations.append({
                "bead": bead_id,
                "surface": impl_surfaces[0],
                "path": declared,
                "reason": "Rust FILE SURFACE lacks structured tracing evidence",
                "missingFields": missing,
            })

observability_report = validate_observability_manifest(root, manifest_required)
total_violations = len(violations) + int(observability_report["violationCount"])

report = {
    "schema": "ee.tracing_field_report.v1",
    "status": "pass" if total_violations == 0 else "fail",
    "auditedBeads": audited,
    "violationCount": total_violations,
    "requiredFields": required_fields,
    "standardPhases": sorted(phase_names),
    "duelingWizardsNoSilentCap": observability_report,
    "violations": violations,
}
print(json.dumps(report, sort_keys=True, separators=(",", ":")))
PY
}

require_tool python3
require_tool jq

if [ "$SELF_TEST" = true ]; then
    tmp_dir=$(mktemp -d "${TMPDIR:-/tmp}/ee-tracing-fields.XXXXXX")
    cat > "$tmp_dir/issues.jsonl" <<'JSONL'
{"id":"bd-3usjw.good","title":"[implements-surface:good_surface] example","labels":["implements-surface:good_surface"],"description":"FILE SURFACE: src/good.rs\nTRACING: surface=good_surface, phases=input|dispatch|response, fields=workspace_id,request_id,bead_id,surface,phase,elapsed_ms,degraded_codes."}
{"id":"bd-3usjw.bad","title":"[implements-surface:bad_surface] example","labels":["implements-surface:bad_surface"],"description":"FILE SURFACE: src/bad.rs"}
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
    report=$(run_checker "$tmp_dir/issues.jsonl" "$tmp_dir" "" "false")
    printf '%s\n' "$report" > "$tmp_dir/self-test-report.json"
    if ! printf '%s\n' "$report" | jq -e '.status == "fail" and .violationCount == 2' >/dev/null; then
        echo "error: self-test expected two violations" >&2
        printf '%s\n' "$report" >&2
        exit 1
    fi
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
    echo "  status: $status"
    echo "  audited_beads: $audited"
    echo "  violations: $violations"
fi

if printf '%s\n' "$report" | jq -e '.status == "pass"' >/dev/null; then
    exit 0
fi
exit 1
