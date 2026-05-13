#!/usr/bin/env bash
# J11.2 — non-destructive RCH stranded verification recovery report.
#
# This helper summarizes an RCH job without killing jobs, deleting artifacts, or
# inferring success from artifact presence alone. It consumes `rch status --jobs
# --json` output plus optional local log/artifact hints and emits one JSON report.

set -euo pipefail

usage() {
    cat <<'EOF'
Usage: scripts/rch_recover_verification.sh --job-id <id> [options]

Options:
  --job-id <id>              RCH build id to inspect (required)
  --status-json <path>       Existing `rch status --jobs --json` capture
  --rch-bin <path>           rch binary to call when --status-json is omitted
  --expected-command <text>  Command expected by the bead/verification note
  --local-exit-code <code>   Local wrapper exit code, or "indeterminate"
  --rch-output-log <path>    Captured RCH stdout/stderr to scan for target-dir hints
  --artifact-list <path>     Newline-delimited artifact paths from manual inspection
  --remote-project-root <p>  Remote project root, used for target-dir glob hints
  --remote-target-dir <p>    Exact remote Cargo target dir when known
  --worker-host <host>       Override worker host for manual inspection commands
  --project-root <path>      Local project root for source hash (default: cwd)
  --json                    Accepted for symmetry; output is always JSON
  -h, --help                 Show this help
EOF
}

JOB_ID=""
STATUS_JSON=""
RCH_BIN="${RCH_BIN:-rch}"
EXPECTED_COMMAND=""
LOCAL_EXIT_CODE="indeterminate"
RCH_OUTPUT_LOG=""
ARTIFACT_LIST=""
REMOTE_PROJECT_ROOT=""
REMOTE_TARGET_DIR=""
WORKER_HOST=""
PROJECT_ROOT="$PWD"

while [ "$#" -gt 0 ]; do
    case "$1" in
        --job-id) JOB_ID="${2:?--job-id requires a value}"; shift 2 ;;
        --status-json) STATUS_JSON="${2:?--status-json requires a value}"; shift 2 ;;
        --rch-bin) RCH_BIN="${2:?--rch-bin requires a value}"; shift 2 ;;
        --expected-command) EXPECTED_COMMAND="${2:?--expected-command requires a value}"; shift 2 ;;
        --local-exit-code) LOCAL_EXIT_CODE="${2:?--local-exit-code requires a value}"; shift 2 ;;
        --rch-output-log) RCH_OUTPUT_LOG="${2:?--rch-output-log requires a value}"; shift 2 ;;
        --artifact-list) ARTIFACT_LIST="${2:?--artifact-list requires a value}"; shift 2 ;;
        --remote-project-root) REMOTE_PROJECT_ROOT="${2:?--remote-project-root requires a value}"; shift 2 ;;
        --remote-target-dir) REMOTE_TARGET_DIR="${2:?--remote-target-dir requires a value}"; shift 2 ;;
        --worker-host) WORKER_HOST="${2:?--worker-host requires a value}"; shift 2 ;;
        --project-root) PROJECT_ROOT="${2:?--project-root requires a value}"; shift 2 ;;
        --json) shift ;;
        -h|--help) usage; exit 0 ;;
        *)
            echo "rch_recover_verification: unknown argument: $1" >&2
            usage >&2
            exit 2
            ;;
    esac
done

if [ -z "$JOB_ID" ]; then
    echo "rch_recover_verification: --job-id is required" >&2
    usage >&2
    exit 2
fi

STATUS_TMP=""
if [ -z "$STATUS_JSON" ]; then
    STATUS_TMP="$(mktemp)"
    "$RCH_BIN" status --jobs --json >"$STATUS_TMP"
    STATUS_JSON="$STATUS_TMP"
fi

python3 - "$JOB_ID" "$STATUS_JSON" "$EXPECTED_COMMAND" "$LOCAL_EXIT_CODE" \
    "$RCH_OUTPUT_LOG" "$ARTIFACT_LIST" "$REMOTE_PROJECT_ROOT" "$REMOTE_TARGET_DIR" \
    "$WORKER_HOST" "$PROJECT_ROOT" "${RCH_RECOVERY_NOW:-}" <<'PY'
import hashlib
import json
import os
import re
import sys
from datetime import datetime, timezone
from pathlib import Path

(
    job_id_text,
    status_json_path,
    expected_command,
    local_exit_code,
    rch_output_log,
    artifact_list_path,
    remote_project_root,
    remote_target_dir,
    worker_host_override,
    project_root,
    generated_at_override,
) = sys.argv[1:]


def now_iso() -> str:
    if generated_at_override:
        return generated_at_override
    return datetime.now(timezone.utc).isoformat(timespec="microseconds").replace("+00:00", "Z")


def hash_text(value: str) -> str | None:
    if not value:
        return None
    return "sha256:" + hashlib.sha256(value.encode("utf-8")).hexdigest()


def hash_file(path: Path) -> str | None:
    try:
        digest = hashlib.sha256()
        with path.open("rb") as handle:
            for chunk in iter(lambda: handle.read(1024 * 1024), b""):
                digest.update(chunk)
        return "sha256:" + digest.hexdigest()
    except OSError:
        return None


def all_jobs(status: dict) -> tuple[list[dict], list[dict]]:
    daemon = status.get("data", {}).get("daemon", {})
    return daemon.get("active_builds", []) or [], daemon.get("recent_builds", []) or []


def find_job(active: list[dict], recent: list[dict], job_id: int) -> tuple[str | None, dict | None]:
    for job in active:
        if job.get("id") == job_id:
            return "active", job
    for job in recent:
        if job.get("id") == job_id:
            return "recent", job
    return None, None


def read_lines(path: str) -> list[str]:
    if not path:
        return []
    try:
        with open(path, "r", encoding="utf-8") as handle:
            return [line.rstrip("\n") for line in handle if line.strip()]
    except OSError:
        return []


def target_dirs_from_log(path: str) -> list[str]:
    if not path:
        return []
    try:
        text = Path(path).read_text(encoding="utf-8")
    except OSError:
        return []
    matches = re.findall(r"(/[^\s'\"`]*\.rch-target-[^\s'\"`]*)", text)
    return sorted(dict.fromkeys(matches))


def source_hash(root: str) -> str | None:
    root_path = Path(root)
    candidates = [root_path / "Cargo.lock", root_path / "Cargo.toml", root_path / "rust-toolchain.toml"]
    digest = hashlib.sha256()
    observed = False
    for path in candidates:
        if path.exists():
            observed = True
            digest.update(path.name.encode("utf-8"))
            digest.update(b"\0")
            digest.update(path.read_bytes())
            digest.update(b"\0")
    if not observed:
        return None
    return "sha256:" + digest.hexdigest()


try:
    job_id = int(job_id_text)
except ValueError:
    print(json.dumps({
        "schema": "ee.rch.recovery_report.v1",
        "success": False,
        "error": {"code": "invalid_job_id", "message": "--job-id must be an integer"},
    }, indent=2, sort_keys=True))
    sys.exit(2)

with open(status_json_path, "r", encoding="utf-8") as handle:
    status_json = json.load(handle)

active, recent = all_jobs(status_json)
record_state, record = find_job(active, recent, job_id)
artifact_paths = read_lines(artifact_list_path)
candidate_target_dirs = []
if remote_target_dir:
    candidate_target_dirs.append(remote_target_dir)
candidate_target_dirs.extend(target_dirs_from_log(rch_output_log))
candidate_binaries = [
    path for path in artifact_paths
    if re.search(r"(/debug/deps/|/release/|/debug/)", path) and not path.endswith((".d", ".rlib", ".rmeta", ".o"))
]

manual_commands: list[str] = []
worker_id = None
worker_host = worker_host_override or None
command = None
exit_code = None
started_at = None
completed_at = None
heartbeat_age = None
progress_age = None
record_command_hash = None
expected_command_hash = hash_text(expected_command)
unsafe_ambiguity = False
ambiguities: list[str] = []
status_value = "missing_job_record"
confidence = "none"

if record is not None:
    worker_id = record.get("worker_id")
    worker_host = worker_host or worker_id
    command = record.get("command")
    record_command_hash = hash_text(command or "")
    exit_code = record.get("exit_code")
    started_at = record.get("started_at")
    completed_at = record.get("completed_at")
    heartbeat_age = record.get("heartbeat_age_secs")
    progress_age = record.get("progress_age_secs")
    if remote_project_root and worker_host:
        manual_commands.append(
            f"ssh {worker_host} 'cd {remote_project_root} && find . -maxdepth 3 -path \"./.rch-target-*\" -type d -print'"
        )
    if remote_project_root and worker_host and not candidate_target_dirs:
        safe_worker = re.sub(r"[^A-Za-z0-9_.-]", "-", str(worker_id or "worker")).strip("-") or "worker"
        candidate_target_dirs.append(f"{remote_project_root}/.rch-target-{safe_worker}-job-{job_id}-*")
    for target_dir in candidate_target_dirs:
        if worker_host:
            manual_commands.append(
                f"ssh {worker_host} 'find {target_dir}/debug/deps {target_dir}/release {target_dir}/debug -maxdepth 1 -type f 2>/dev/null | sort'"
            )

    if expected_command and command != expected_command:
        unsafe_ambiguity = True
        ambiguities.append("record command does not match --expected-command")
        status_value = "ambiguous_command_mismatch"
        confidence = "blocked"
    elif record_state == "recent" and isinstance(exit_code, int):
        if exit_code == 0:
            status_value = "pass"
            confidence = "explicit_remote_exit_zero"
        else:
            status_value = "fail"
            confidence = "explicit_remote_nonzero_exit"
    elif candidate_binaries or artifact_paths:
        status_value = "indeterminate_recovered_artifact"
        confidence = "artifact_only_no_exit_status"
    elif record_state == "active" and heartbeat_age is None and progress_age is None:
        status_value = "stale_no_heartbeat"
        confidence = "active_record_without_heartbeat"
    else:
        status_value = "indeterminate"
        confidence = "record_without_terminal_evidence"

if local_exit_code not in ("", "0", "indeterminate") and status_value == "pass":
    ambiguities.append("local wrapper exit was non-zero while remote record reports pass")

report = {
    "schema": "ee.rch.recovery_report.v1",
    "generated_at": now_iso(),
    "job_id": job_id,
    "status": status_value,
    "confidence": confidence,
    "safe_for_closure_evidence": status_value in ("pass", "fail") and not unsafe_ambiguity,
    "unsafe_ambiguity": unsafe_ambiguity,
    "ambiguities": ambiguities,
    "local_wrapper": {
        "exit_code": local_exit_code or "indeterminate",
        "indeterminate": local_exit_code in ("", "indeterminate", "-1"),
    },
    "rch_record": {
        "state": record_state,
        "worker_id": worker_id,
        "worker_host": worker_host,
        "command": command,
        "command_hash": record_command_hash,
        "expected_command_hash": expected_command_hash,
        "started_at": started_at,
        "completed_at": completed_at,
        "exit_code": exit_code,
        "heartbeat_age_secs": heartbeat_age,
        "progress_age_secs": progress_age,
    },
    "artifacts": {
        "remote_project_root": remote_project_root or None,
        "candidate_target_dirs": sorted(dict.fromkeys(candidate_target_dirs)),
        "candidate_paths": artifact_paths,
        "candidate_binaries": candidate_binaries,
        "artifact_count": len(artifact_paths),
    },
    "hashes": {
        "source_hash": source_hash(project_root),
        "status_json_hash": hash_file(Path(status_json_path)),
    },
    "manual_inspection_commands": sorted(dict.fromkeys(manual_commands)),
    "notes": [
        "Artifact presence alone is not pass evidence.",
        "Use safe_for_closure_evidence=true only when an explicit terminal RCH record exists and command ambiguity is false.",
    ],
}

print(json.dumps(report, indent=2, sort_keys=True))
PY
