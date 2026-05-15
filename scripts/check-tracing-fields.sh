#!/usr/bin/env bash
# check-tracing-fields.sh - Part II tracing convention gate (bd-3usjw.58).
#
# This is build-independent. It audits Beads descriptions and declared source
# file surfaces for the shared tracing field convention documented in
# docs/observability/tracing_field_convention.md.

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

    python3 - "$beads_path" "$root_path" "$bead_filter" <<'PY'
import json
import re
import sys
from pathlib import Path

beads_path = Path(sys.argv[1])
root = Path(sys.argv[2])
bead_filter = sys.argv[3]

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

report = {
    "schema": "ee.tracing_field_report.v1",
    "status": "pass" if not violations else "fail",
    "auditedBeads": audited,
    "violationCount": len(violations),
    "requiredFields": required_fields,
    "standardPhases": sorted(phase_names),
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
    report=$(run_checker "$tmp_dir/issues.jsonl" "$tmp_dir" "")
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

report=$(run_checker "$BEADS_FILE" "$ROOT" "$BEAD_FILTER")
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
