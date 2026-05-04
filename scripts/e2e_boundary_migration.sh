#!/usr/bin/env bash
# Mechanical-boundary end-to-end harness with artifact-rich command dossiers.
#
# Runs representative boundary migration checks against the real ee binary in
# isolated workspaces. Each step writes docs/boundary-migration-e2e-logging.md
# compatible artifacts under target/ee-e2e/boundary-migration/<run-id>/.
#
# Usage:
#   ./scripts/e2e_boundary_migration.sh
#   ./scripts/e2e_boundary_migration.sh baseline degraded
#   ./scripts/e2e_boundary_migration.sh --list
#   ./scripts/e2e_boundary_migration.sh --self-test
#
# Environment:
#   EE_BINARY                 Path to ee binary. Default: target/debug/ee.
#   EE_E2E_ARTIFACT_ROOT      Artifact output root.
#   EE_BOUNDARY_STRICT_GOLDEN Fail on golden mismatch when a golden is listed.
#   EE_VERBOSE                Print per-command progress.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
EE_BINARY="${EE_BINARY:-${REPO_ROOT}/target/debug/ee}"
CORPUS_PATH="${REPO_ROOT}/tests/fixtures/boundary_corpus/corpus.json"
RUN_ID="${EE_E2E_RUN_ID:-$(date -u +%Y%m%dT%H%M%SZ)-$$}"
ARTIFACT_ROOT="${EE_E2E_ARTIFACT_ROOT:-${REPO_ROOT}/target/ee-e2e/boundary-migration/${RUN_ID}}"
TMP_PARENT="${TMPDIR:-/tmp}"
TEST_ROOT=""
TEST_HOME=""
WORKSPACE=""
FAILED=0
PASSED=0
RUN=0

SCENARIOS=(baseline degraded redaction skill-handoff)

log_info() {
    echo "[INFO] $*" >&2
}

log_error() {
    echo "[ERROR] $*" >&2
}

ms_now() {
    python3 - <<'PY'
import time
print(int(time.time() * 1000))
PY
}

require_tool() {
    local tool="$1"
    if ! command -v "${tool}" >/dev/null 2>&1; then
        log_error "${tool} is required"
        exit 3
    fi
}

show_help() {
    sed -n '2,27p' "$0" | sed 's/^# \{0,1\}//'
}

list_scenarios() {
    printf '%s\n' "${SCENARIOS[@]}"
}

setup_workspace() {
    mkdir -p "${ARTIFACT_ROOT}"
    TEST_ROOT=$(mktemp -d "${TMP_PARENT%/}/ee-boundary-e2e.XXXXXX")
    TEST_HOME="${TEST_ROOT}/home"
    WORKSPACE="${TEST_ROOT}/workspace"
    mkdir -p "${TEST_HOME}" "${WORKSPACE}"
    log_info "workspace=${WORKSPACE}"
    log_info "artifacts=${ARTIFACT_ROOT}"
}

check_binary() {
    if [[ -x "${EE_BINARY}" ]]; then
        return 0
    fi

    log_error "ee binary not found at ${EE_BINARY}"
    log_error "build with rch first or pass EE_BINARY=/path/to/ee"
    exit 3
}

fixture_hashes_json() {
    local fixture_id="$1"
    FIXTURE_ID="${fixture_id}" CORPUS_PATH="${CORPUS_PATH}" python3 <<'PY'
import json
import os

fixture_id = os.environ["FIXTURE_ID"]
if not fixture_id or fixture_id == "none":
    print("{}")
    raise SystemExit

with open(os.environ["CORPUS_PATH"], encoding="utf-8") as fh:
    corpus = json.load(fh)

for fixture in corpus["fixtures"]:
    if fixture["id"] == fixture_id:
        print(json.dumps({fixture_id: fixture["contentHash"]}, sort_keys=True))
        raise SystemExit

raise SystemExit(f"fixture not found: {fixture_id}")
PY
}

write_env_sanitized() {
    local output_path="$1"
    ENV_OUT="${output_path}" TEST_HOME="${TEST_HOME}" WORKSPACE="${WORKSPACE}" python3 <<'PY'
import json
import os

sensitive_tokens = ("SECRET", "TOKEN", "PASSWORD", "KEY", "CREDENTIAL")
env = {
    "HOME": os.environ["TEST_HOME"],
    "EE_WORKSPACE": os.environ["WORKSPACE"],
    "NO_COLOR": "1",
    "EE_BOUNDARY_E2E": "1",
}
for key, value in os.environ.items():
    if any(token in key.upper() for token in sensitive_tokens):
        env[key] = "[REDACTED]"
with open(os.environ["ENV_OUT"], "w", encoding="utf-8") as fh:
    json.dump(env, fh, indent=2, sort_keys=True)
    fh.write("\n")
PY
}

write_command_file() {
    local output_path="$1"
    shift
    python3 - "$@" >"${output_path}" <<'PY'
import shlex
import sys

print(shlex.join(sys.argv[1:]))
PY
}

snapshot_workspace_files() {
    local output_path="$1"
    if [[ -d "${WORKSPACE}" ]]; then
        find "${WORKSPACE}" -mindepth 1 -print | LC_ALL=C sort >"${output_path}"
    else
        : >"${output_path}"
    fi
}

write_boundary_log() {
    local step_dir="$1"
    local case_id="$2"
    local command_family="$3"
    local matrix_row="$4"
    local workflow_row="$5"
    local fixture_id="$6"
    local side_effect_class="$7"
    local mutation_summary="$8"
    local expected_schema="$9"
    local expected_exit="${10}"
    local golden_path="${11}"
    local started_ms="${12}"
    local ended_ms="${13}"
    local exit_code="${14}"
    local command_name="${15}"
    shift 15
    local argv=("$@")

    local fixture_hashes
    fixture_hashes=$(fixture_hashes_json "${fixture_id}")

    CASE_ID="${case_id}" \
    COMMAND_FAMILY="${command_family}" \
    MATRIX_ROW="${matrix_row}" \
    WORKFLOW_ROW="${workflow_row}" \
    FIXTURE_ID="${fixture_id}" \
    SIDE_EFFECT_CLASS="${side_effect_class}" \
    MUTATION_SUMMARY="${mutation_summary}" \
    EXPECTED_SCHEMA="${expected_schema}" \
    EXPECTED_EXIT="${expected_exit}" \
    GOLDEN_PATH="${golden_path}" \
    STARTED_MS="${started_ms}" \
    ENDED_MS="${ended_ms}" \
    EXIT_CODE="${exit_code}" \
    COMMAND_NAME="${command_name}" \
    STEP_DIR="${step_dir}" \
    ARTIFACT_ROOT="${ARTIFACT_ROOT}" \
    WORKSPACE="${WORKSPACE}" \
    REPO_ROOT="${REPO_ROOT}" \
    CORPUS_PATH="${CORPUS_PATH}" \
    STRICT_GOLDEN="${EE_BOUNDARY_STRICT_GOLDEN:-}" \
    FIXTURE_HASHES="${fixture_hashes}" \
    python3 - "$@" <<'PY'
import json
import os
import shlex
import sys
from pathlib import Path

argv = sys.argv[1:]
step_dir = Path(os.environ["STEP_DIR"])
stdout_path = step_dir / "stdout"
stderr_path = step_dir / "stderr"
before_worktrees = (step_dir / "git-worktrees.before").read_text(encoding="utf-8")
after_worktrees = (step_dir / "git-worktrees.after").read_text(encoding="utf-8")
before_files = set((step_dir / "workspace-files.before").read_text(encoding="utf-8").splitlines())
after_files = set((step_dir / "workspace-files.after").read_text(encoding="utf-8").splitlines())
removed_files = sorted(before_files - after_files)

stdout_text = stdout_path.read_text(encoding="utf-8", errors="replace")
stdout_json_valid = False
observed_schema = None
parsed = None
first_failure = None
degradation_codes = []

try:
    parsed = json.loads(stdout_text) if stdout_text.strip() else None
    stdout_json_valid = parsed is not None
except json.JSONDecodeError as exc:
    first_failure = f"stdout_json_invalid:{exc.msg}"

if stdout_json_valid:
    observed_schema = parsed.get("schema")
    if observed_schema == "ee.error.v1":
        code = parsed.get("error", {}).get("code")
        if code:
            degradation_codes.append(code)
    for entry in parsed.get("data", {}).get("degraded", []):
        code = entry.get("code")
        if code:
            degradation_codes.append(code)
    for entry in parsed.get("data", {}).get("issues", []):
        code = entry.get("code")
        if code:
            degradation_codes.append(code)
else:
    if stdout_text.startswith(("[INFO]", "[WARN]", "[ERROR]", "warning:", "error:")):
        first_failure = "stdout_pollution"

expected_schema = os.environ["EXPECTED_SCHEMA"]
if first_failure is None and expected_schema != "any" and observed_schema != expected_schema:
    first_failure = f"schema_mismatch:{observed_schema or 'missing'}"

expected_exit = os.environ["EXPECTED_EXIT"]
exit_code = int(os.environ["EXIT_CODE"])
if first_failure is None and expected_exit != "any" and exit_code != int(expected_exit):
    first_failure = f"exit_code_mismatch:{exit_code}"

golden_path = os.environ["GOLDEN_PATH"]
golden_validation = {"path": None, "status": "not_applicable"}
if golden_path != "none":
    golden_abs = Path(os.environ["REPO_ROOT"]) / golden_path
    golden_validation["path"] = str(golden_abs)
    if not golden_abs.exists():
        golden_validation["status"] = "missing"
    elif golden_abs.read_text(encoding="utf-8", errors="replace") == stdout_text:
        golden_validation["status"] = "match"
    else:
        golden_validation["status"] = "mismatch"
    if first_failure is None and os.environ["STRICT_GOLDEN"] and golden_validation["status"] != "match":
        first_failure = f"golden_{golden_validation['status']}"

env_sanitized = json.loads((step_dir / "env.sanitized.json").read_text(encoding="utf-8"))
for key, value in env_sanitized.items():
    upper = key.upper()
    if any(token in upper for token in ("SECRET", "TOKEN", "PASSWORD", "KEY", "CREDENTIAL")):
        if value != "[REDACTED]":
            first_failure = f"env_not_redacted:{key}"
            break

forbidden_checked = before_worktrees == after_worktrees and not removed_files
if first_failure is None and not forbidden_checked:
    first_failure = "forbidden_filesystem_operation"

fixture_hashes = json.loads(os.environ["FIXTURE_HASHES"])
fixture_metadata = {}
if os.environ["FIXTURE_ID"] not in ("", "none"):
    with open(os.environ["CORPUS_PATH"], encoding="utf-8") as fh:
        corpus = json.load(fh)
    fixture_metadata = next(
        (fixture for fixture in corpus["fixtures"] if fixture["id"] == os.environ["FIXTURE_ID"]),
        {},
    )
if first_failure is None and os.environ["FIXTURE_ID"] not in ("", "none") and not fixture_hashes:
    first_failure = f"missing_fixture_hash:{os.environ['FIXTURE_ID']}"

redaction_status = fixture_metadata.get("redactionState", {"status": "none", "classes": []})
trust_classes = [fixture_metadata.get("trustClass", "synthetic_fixture")] if fixture_metadata else []
if fixture_metadata:
    prompt_injection_quarantine_status = (
        "quarantined" if fixture_metadata.get("promptInjectionQuarantined") else "not_quarantined"
    )
else:
    prompt_injection_quarantine_status = "not_applicable"

command_name = os.environ["COMMAND_NAME"]
reproduction_command = (
    f"cd {shlex.quote(os.environ['REPO_ROOT'])} && "
    f"env HOME={shlex.quote(env_sanitized['HOME'])} "
    f"EE_WORKSPACE={shlex.quote(env_sanitized['EE_WORKSPACE'])} "
    f"NO_COLOR=1 EE_BOUNDARY_E2E=1 "
    + shlex.join([command_name, *argv])
)

record = {
    "schema": "ee.e2e.boundary_log.v1",
    "case_id": os.environ["CASE_ID"],
    "command_family": os.environ["COMMAND_FAMILY"],
    "command": command_name,
    "argv": argv,
    "cwd": os.environ["REPO_ROOT"],
    "workspace": os.environ["WORKSPACE"],
    "env_sanitized": env_sanitized,
    "started_at_unix_ms": int(os.environ["STARTED_MS"]),
    "ended_at_unix_ms": int(os.environ["ENDED_MS"]),
    "elapsed_ms": int(os.environ["ENDED_MS"]) - int(os.environ["STARTED_MS"]),
    "exit_code": exit_code,
    "stdout_artifact_path": str(stdout_path),
    "stderr_artifact_path": str(stderr_path),
    "stdout_json_valid": stdout_json_valid,
    "schema_validation": {
        "expected": expected_schema,
        "observed": observed_schema,
        "status": "match" if expected_schema in ("any", observed_schema) else "mismatch",
    },
    "golden_validation": golden_validation,
    "redaction_status": {"status": "none", "classes": []},
    "evidence_ids": [os.environ["FIXTURE_ID"]] if os.environ["FIXTURE_ID"] != "none" else [],
    "degradation_codes": sorted(set(degradation_codes)),
    "mutation_summary": os.environ["MUTATION_SUMMARY"],
    "side_effect_class": os.environ["SIDE_EFFECT_CLASS"],
    "changed_record_ids": [],
    "audit_ids": [],
    "records_rolled_back_or_audited": [],
    "filesystem_artifacts_created": [],
    "forbidden_filesystem_operations_checked": forbidden_checked,
    "evidence_bundle_path": os.environ.get("EVIDENCE_BUNDLE_PATH") or None,
    "evidence_bundle_hash": os.environ.get("EVIDENCE_BUNDLE_HASH") or None,
    "provenance_ids": [],
    "trust_classes": trust_classes,
    "prompt_injection_quarantine_status": prompt_injection_quarantine_status,
    "command_boundary_matrix_row": os.environ["MATRIX_ROW"],
    "readme_workflow_row": os.environ["WORKFLOW_ROW"] or None,
    "fixture_hashes": fixture_hashes,
    "db_generation_before": None,
    "db_generation_after": None,
    "index_generation_before": None,
    "index_generation_after": None,
    "runtime_budget": None,
    "cancellation_status": "not_requested",
    "cancellation_injection_point": None,
    "observed_outcome": "success" if exit_code == 0 else "degraded_or_error",
    "reproduction_command": reproduction_command,
    "first_failure": first_failure,
}

(step_dir / "stdout.schema.json").write_text(
    json.dumps({"schema": observed_schema, "json_valid": stdout_json_valid}, indent=2, sort_keys=True) + "\n",
    encoding="utf-8",
)
(step_dir / "degradation-report.json").write_text(
    json.dumps({"degradation_codes": record["degradation_codes"]}, indent=2, sort_keys=True) + "\n",
    encoding="utf-8",
)
(step_dir / "redaction-report.json").write_text(
    json.dumps(record["redaction_status"], indent=2, sort_keys=True) + "\n",
    encoding="utf-8",
)
(step_dir / "first-failure.md").write_text(
    "none\n" if first_failure is None else f"{first_failure}\n",
    encoding="utf-8",
)
(step_dir / "boundary-log.json").write_text(
    json.dumps(record, indent=2, sort_keys=True) + "\n",
    encoding="utf-8",
)
(step_dir / "summary.json").write_text(
    json.dumps(
        {
            "schema": "ee.e2e.boundary_log.step_summary.v1",
            "case_id": record["case_id"],
            "passed": first_failure is None,
            "first_failure": first_failure,
            "stdout_artifact_path": record["stdout_artifact_path"],
            "stderr_artifact_path": record["stderr_artifact_path"],
            "reproduction_command": reproduction_command,
        },
        indent=2,
        sort_keys=True,
    ) + "\n",
    encoding="utf-8",
)
PY
}

run_ee_case() {
    local case_id="$1"
    local command_family="$2"
    local matrix_row="$3"
    local workflow_row="$4"
    local fixture_id="$5"
    local side_effect_class="$6"
    local mutation_summary="$7"
    local expected_schema="$8"
    local expected_exit="$9"
    local golden_path="${10}"
    shift 10
    local argv=("$@")

    local step_dir="${ARTIFACT_ROOT}/${case_id}"
    mkdir -p "${step_dir}"
    write_env_sanitized "${step_dir}/env.sanitized.json"
    write_command_file "${step_dir}/command.txt" "${EE_BINARY}" "${argv[@]}"
    printf '%s\n' "${REPO_ROOT}" >"${step_dir}/cwd.txt"
    printf '%s\n' "${WORKSPACE}" >"${step_dir}/workspace.txt"
    git -C "${REPO_ROOT}" worktree list --porcelain >"${step_dir}/git-worktrees.before"
    snapshot_workspace_files "${step_dir}/workspace-files.before"

    local started_ms
    local ended_ms
    local exit_code=0
    started_ms=$(ms_now)
    env HOME="${TEST_HOME}" EE_WORKSPACE="${WORKSPACE}" NO_COLOR=1 EE_BOUNDARY_E2E=1 \
        "${EE_BINARY}" "${argv[@]}" >"${step_dir}/stdout" 2>"${step_dir}/stderr" || exit_code=$?
    ended_ms=$(ms_now)

    git -C "${REPO_ROOT}" worktree list --porcelain >"${step_dir}/git-worktrees.after"
    snapshot_workspace_files "${step_dir}/workspace-files.after"
    write_boundary_log \
        "${step_dir}" "${case_id}" "${command_family}" "${matrix_row}" "${workflow_row}" \
        "${fixture_id}" "${side_effect_class}" "${mutation_summary}" "${expected_schema}" \
        "${expected_exit}" "${golden_path}" "${started_ms}" "${ended_ms}" "${exit_code}" \
        "${EE_BINARY}" "${argv[@]}"

    RUN=$((RUN + 1))
    if [[ "$(cat "${step_dir}/first-failure.md")" == "none" ]]; then
        PASSED=$((PASSED + 1))
        if [[ -n "${EE_VERBOSE:-}" ]]; then
            log_info "PASS ${case_id}"
        fi
    else
        FAILED=$((FAILED + 1))
        log_error "FAIL ${case_id}: $(cat "${step_dir}/first-failure.md")"
    fi
}

run_skill_handoff_case() {
    local case_id="skill_handoff_prompt_injection"
    local step_dir="${ARTIFACT_ROOT}/${case_id}"
    mkdir -p "${step_dir}"
    write_env_sanitized "${step_dir}/env.sanitized.json"
    write_command_file "${step_dir}/command.txt" "skill-handoff-fixture" "boundary.prompt_injection_session.v1"
    printf '%s\n' "${REPO_ROOT}" >"${step_dir}/cwd.txt"
    printf '%s\n' "${WORKSPACE}" >"${step_dir}/workspace.txt"
    git -C "${REPO_ROOT}" worktree list --porcelain >"${step_dir}/git-worktrees.before"
    snapshot_workspace_files "${step_dir}/workspace-files.before"

    local bundle_path="${step_dir}/skill-evidence-bundle.json"
    CORPUS_PATH="${CORPUS_PATH}" BUNDLE_PATH="${bundle_path}" python3 <<'PY'
import hashlib
import json
import os

with open(os.environ["CORPUS_PATH"], encoding="utf-8") as fh:
    corpus = json.load(fh)
fixture = next(f for f in corpus["fixtures"] if f["id"] == "boundary.prompt_injection_session.v1")
bundle = {
    "schema": "ee.skill_evidence_bundle.v1",
    "fixtureMode": True,
    "allowedSkillAction": "review_only",
    "durableMutation": "must_call_ee_cli_explicitly",
    "evidence": [
        {
            "id": fixture["id"],
            "provenanceUris": fixture["provenanceUris"],
            "redactionState": fixture["redactionState"],
            "trustClass": fixture["trustClass"],
            "promptInjectionQuarantined": fixture["promptInjectionQuarantined"],
            "normalWorkspaceLeakageForbidden": fixture["normalWorkspaceLeakageForbidden"],
        }
    ],
}
text = json.dumps(bundle, indent=2, sort_keys=True) + "\n"
with open(os.environ["BUNDLE_PATH"], "w", encoding="utf-8") as fh:
    fh.write(text)
PY
    local bundle_hash
    bundle_hash=$(CORPUS_PATH="${CORPUS_PATH}" BUNDLE_PATH="${bundle_path}" python3 <<'PY'
import hashlib
import os
text = open(os.environ["BUNDLE_PATH"], encoding="utf-8").read()
print("sha256:" + hashlib.sha256(text.encode("utf-8")).hexdigest())
PY
)
    cp "${bundle_path}" "${step_dir}/stdout"
    : >"${step_dir}/stderr"
    git -C "${REPO_ROOT}" worktree list --porcelain >"${step_dir}/git-worktrees.after"
    snapshot_workspace_files "${step_dir}/workspace-files.after"

    local started_ms
    local ended_ms
    started_ms=$(ms_now)
    ended_ms="${started_ms}"
    EVIDENCE_BUNDLE_PATH="${bundle_path}" EVIDENCE_BUNDLE_HASH="${bundle_hash}" \
        write_boundary_log \
        "${step_dir}" "${case_id}" "project-local-skill-handoff" \
        "handoff/review/preflight skill handoff rows" \
        "skill workflow: review_only evidence bundle" \
        "boundary.prompt_injection_session.v1" "class=read_only" "read_only" \
        "ee.skill_evidence_bundle.v1" "0" "none" "${started_ms}" "${ended_ms}" "0" \
        "skill-handoff-fixture" "boundary.prompt_injection_session.v1"

    RUN=$((RUN + 1))
    if [[ "$(cat "${step_dir}/first-failure.md")" == "none" ]]; then
        PASSED=$((PASSED + 1))
        if [[ -n "${EE_VERBOSE:-}" ]]; then
            log_info "PASS ${case_id}"
        fi
    else
        FAILED=$((FAILED + 1))
        log_error "FAIL ${case_id}: $(cat "${step_dir}/first-failure.md")"
    fi
}

scenario_baseline() {
    run_ee_case baseline_agent_docs "baseline-infrastructure" \
        "help/version/introspect/schema/model/agent-docs row" \
        "README agent command discovery" "boundary.empty_workspace.v1" \
        "class=read_only" "read_only" "ee.response.v1" "0" \
        "tests/fixtures/golden/agent_docs/agent_docs_json.golden" \
        agent-docs --json

    run_ee_case baseline_status_empty "baseline-infrastructure" \
        "capabilities/check/health/status row" \
        "README status workflow" "boundary.empty_workspace.v1" \
        "class=read_only" "read_only" "ee.response.v1" "0" "none" \
        status --workspace "${WORKSPACE}" --json

    run_ee_case baseline_diag_dependencies "diagnostics-eval-ops" \
        "diag/doctor row" "README diagnostics workflow" "boundary.empty_workspace.v1" \
        "class=read_only" "read_only" "ee.response.v1" "0" "none" \
        diag dependencies --json
}

scenario_degraded() {
    run_ee_case degraded_search_missing_index "core-retrieval-pack-explain" \
        "context/search/why row" "README search workflow" \
        "boundary.search_index_missing_degraded.v1" "class=read_only" "read_only" \
        "ee.error.v1" "4" "tests/fixtures/golden/agent/search_unavailable.json.golden" \
        search "format before release" --workspace "${WORKSPACE}" --json

    run_ee_case degraded_context_missing_db "core-retrieval-pack-explain" \
        "context/search/why row" "README context workflow" \
        "boundary.search_index_missing_degraded.v1" "class=mixed" "failed_before_mutation" \
        "ee.error.v1" "3" "none" \
        context "prepare release" --workspace "${WORKSPACE}" --json

    run_ee_case degraded_why_missing_db "core-retrieval-pack-explain" \
        "context/search/why row" "README why workflow" \
        "boundary.search_index_missing_degraded.v1" "class=read_only" "read_only" \
        "ee.error.v1" "3" "none" \
        why mem_00000000000000000000000001 --workspace "${WORKSPACE}" --json
}

scenario_redaction() {
    local had_openai_key=0
    local previous_openai_key=""
    if [[ -v OPENAI_API_KEY ]]; then
        had_openai_key=1
        previous_openai_key="${OPENAI_API_KEY}"
    fi

    export OPENAI_API_KEY="not-a-real-key"
    run_ee_case redacted_status_env "privacy-trust" \
        "privacy/trust/redacted support and handoff rows" \
        "README privacy and trust workflow" "boundary.redacted_secret_placeholder.v1" \
        "class=read_only" "read_only" "ee.response.v1" "0" "none" \
        status --workspace "${WORKSPACE}" --json

    if [[ "${had_openai_key}" -eq 1 ]]; then
        export OPENAI_API_KEY="${previous_openai_key}"
    else
        unset OPENAI_API_KEY
    fi
}

scenario_skill_handoff() {
    run_skill_handoff_case
}

write_summary() {
    ARTIFACT_ROOT="${ARTIFACT_ROOT}" RUN_ID="${RUN_ID}" python3 <<'PY'
import json
import os
from collections import Counter, defaultdict
from pathlib import Path

root = Path(os.environ["ARTIFACT_ROOT"])
logs = []
for path in sorted(root.glob("*/boundary-log.json")):
    with open(path, encoding="utf-8") as fh:
        logs.append(json.load(fh))

by_family = defaultdict(lambda: {"passed": 0, "failed": 0})
by_fixture = defaultdict(lambda: {"passed": 0, "failed": 0})
by_side_effect = defaultdict(lambda: {"passed": 0, "failed": 0})
by_workflow = defaultdict(lambda: {"passed": 0, "failed": 0})
degraded = Counter()
evidence_artifacts = []
repro = []
first_failure = None

for record in logs:
    passed = record["first_failure"] is None
    bucket = "passed" if passed else "failed"
    by_family[record["command_family"]][bucket] += 1
    for evidence_id in record["evidence_ids"] or ["none"]:
        by_fixture[evidence_id][bucket] += 1
    by_side_effect[record["side_effect_class"]][bucket] += 1
    by_workflow[record["readme_workflow_row"] or "none"][bucket] += 1
    degraded.update(record["degradation_codes"])
    if record["evidence_bundle_path"]:
        evidence_artifacts.append(
            {
                "path": record["evidence_bundle_path"],
                "hash": record["evidence_bundle_hash"],
                "case_id": record["case_id"],
            }
        )
    repro.append({"case_id": record["case_id"], "command": record["reproduction_command"]})
    if first_failure is None and not passed:
        first_failure = {"case_id": record["case_id"], "diagnosis": record["first_failure"]}

summary = {
    "schema": "ee.e2e.boundary_log.summary.v1",
    "run_id": os.environ["RUN_ID"],
    "artifact_root": str(root),
    "totals": {
        "steps": len(logs),
        "passed": sum(1 for r in logs if r["first_failure"] is None),
        "failed": sum(1 for r in logs if r["first_failure"] is not None),
    },
    "by_command_family": dict(sorted(by_family.items())),
    "by_fixture": dict(sorted(by_fixture.items())),
    "by_side_effect_class": dict(sorted(by_side_effect.items())),
    "by_workflow": dict(sorted(by_workflow.items())),
    "degraded_states_observed": dict(sorted(degraded.items())),
    "evidence_artifacts": evidence_artifacts,
    "reproduction_commands": repro,
    "first_failure": first_failure,
}

(root / "summary.json").write_text(json.dumps(summary, indent=2, sort_keys=True) + "\n", encoding="utf-8")
print(root / "summary.json")
PY
}

self_test() {
    require_tool python3
    require_tool git
    setup_workspace
    OPENAI_API_KEY="not-a-real-key" write_env_sanitized "${ARTIFACT_ROOT}/self-test-env.json"
    python3 - "${ARTIFACT_ROOT}/self-test-env.json" <<'PY'
import json
import sys
env = json.load(open(sys.argv[1], encoding="utf-8"))
assert env["OPENAI_API_KEY"] == "[REDACTED]"
PY
    run_skill_handoff_case
    local summary
    summary=$(write_summary)
    python3 - "${summary}" <<'PY'
import json
import sys
summary = json.load(open(sys.argv[1], encoding="utf-8"))
assert summary["schema"] == "ee.e2e.boundary_log.summary.v1"
assert summary["totals"]["failed"] == 0
assert summary["evidence_artifacts"][0]["hash"].startswith("sha256:")
assert "skill-handoff-fixture" in summary["reproduction_commands"][0]["command"]
PY
    log_info "self-test summary=${summary}"
}

main() {
    local targets=()
    while [[ $# -gt 0 ]]; do
        case "$1" in
            --help|-h)
                show_help
                exit 0
                ;;
            --list|-l)
                list_scenarios
                exit 0
                ;;
            --self-test)
                self_test
                exit 0
                ;;
            *)
                targets+=("$1")
                ;;
        esac
        shift
    done

    require_tool python3
    require_tool git
    check_binary
    setup_workspace

    if [[ ${#targets[@]} -eq 0 ]]; then
        targets=("${SCENARIOS[@]}")
    fi

    for target in "${targets[@]}"; do
        case "${target}" in
            baseline) scenario_baseline ;;
            degraded) scenario_degraded ;;
            redaction) scenario_redaction ;;
            skill-handoff) scenario_skill_handoff ;;
            *)
                log_error "unknown scenario: ${target}"
                list_scenarios >&2
                exit 1
                ;;
        esac
    done

    local summary
    summary=$(write_summary)
    echo "Boundary migration e2e summary: ${summary}"
    echo "Steps: ${RUN}; passed: ${PASSED}; failed: ${FAILED}"

    if [[ "${FAILED}" -ne 0 ]]; then
        exit 2
    fi
}

main "$@"
