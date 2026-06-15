#!/usr/bin/env bash
# Read-only RCH lane doctor for the Mac checkout (bd-2qpgn).
#
# Classifies the host-environment state that decides whether
# scripts/rch_verify.sh can reach remote Cargo at all, and emits the
# known-good env override when the USB-detached dual blocker is active.
#
# Background (bd-2qpgn, 2026-06-12): with the external build drive
# detached, /data dangles and the sibling path-dependency roots that are
# symlinks into the user dp dir escape the default canonical project
# root. Both Cargo.toml path forms are then refused before Cargo:
#   - absolute /data/projects/<dep> passes the planner's textual
#     allowed-root check but dies in sibling rsync (ENOENT);
#   - relative ../<dep> canonicalizes through the symlink to a path
#     outside the canonical root and the dependency planner refuses
#     with RCH-E327.
# The verified remediation is broadening the topology roots for the
# dispatch only:
#   RCH_CANONICAL_PROJECT_ROOT=$HOME RCH_ALIAS_PROJECT_ROOT=/dp
# which keeps every canonicalized sibling under the canonical root and
# lets the sync closure ship the dp checkouts to the worker.

set -uo pipefail

usage() {
    cat <<'EOF'
Usage: scripts/rch_lane_doctor.sh [--json] [--emit-env] [--worker-canary]

Modes:
  --json                         Emit the full ee.rch_lane_doctor.v1 report on stdout (default).
  --emit-env                     Emit eval-able export lines for the broadened-root override
                                 when (and only when) the usb_detached_dual_blocker state is
                                 detected; emits nothing when the lane is healthy.
  --worker-canary                Emit ee.rch.worker_root_canary.v1 without running Cargo.
  --worker-canary-fixture CASE   Emit deterministic canary fixture JSON for CASE:
                                 healthy, missing-root, outer-workspace-shadowed,
                                 timeout, permission-denied.
  --recurrence-evidence          Emit ee.rch.topology_recurrence_evidence.v1 by
                                 combining the worker canary, topology-audit
                                 surface, local Cargo tripwire, and br cycle check.
  --recurrence-proof PATH        RCH proof JSON to classify for recurrence evidence
                                 (default: tests/fixtures/verify_ledger/rch_e327_topology_recurrence.json).
  --recurrence-manifest PATH     Manifest path passed to topology-audit
                                 (default: Cargo.toml).
  --recurrence-canary-fixture CASE
                                 Use a deterministic worker canary fixture while
                                 building recurrence evidence.
  --self-test                    Run fixture-backed canary assertions; no Cargo, no writes.

Exit codes:
  0  lane healthy (default roots work; no override needed)
  2  usb_detached_dual_blocker detected (override available and emitted)
  3  indeterminate (unexpected layout; inspect the JSON report)

The script is read-only: no Cargo and no writes. --worker-canary may inspect
the local RCH daemon status socket with a bounded timeout.
Remediation tracking bead: bd-2qpgn.
EOF
}

MODE="json"
CANARY_FIXTURE=""
RECURRENCE_PROOF_JSON="tests/fixtures/verify_ledger/rch_e327_topology_recurrence.json"
RECURRENCE_MANIFEST="Cargo.toml"
RECURRENCE_CANARY_FIXTURE=""
while [ "$#" -gt 0 ]; do
    case "${1:-}" in
        --json ) MODE="json" ;;
        --emit-env ) MODE="emit-env" ;;
        --worker-canary|--canary ) MODE="worker-canary" ;;
        --recurrence-evidence ) MODE="recurrence-evidence" ;;
        --worker-canary-fixture )
            MODE="worker-canary-fixture"
            shift
            CANARY_FIXTURE="${1:-}"
            [ -n "$CANARY_FIXTURE" ] || { usage >&2; exit 64; }
            ;;
        --recurrence-proof )
            shift
            RECURRENCE_PROOF_JSON="${1:-}"
            [ -n "$RECURRENCE_PROOF_JSON" ] || { usage >&2; exit 64; }
            ;;
        --recurrence-manifest )
            shift
            RECURRENCE_MANIFEST="${1:-}"
            [ -n "$RECURRENCE_MANIFEST" ] || { usage >&2; exit 64; }
            ;;
        --recurrence-canary-fixture )
            shift
            RECURRENCE_CANARY_FIXTURE="${1:-}"
            [ -n "$RECURRENCE_CANARY_FIXTURE" ] || { usage >&2; exit 64; }
            ;;
        --self-test ) MODE="self-test" ;;
        -h|--help ) usage; exit 0 ;;
        * ) usage >&2; exit 64 ;;
    esac
    shift
done

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd -P)"
DEFAULT_CANONICAL_ROOT="$(dirname "$REPO_ROOT")"

usb_mounted=false
[ -d /Volumes/USBNVME16TB ] && usb_mounted=true

resolve_dir() {
    # Print the physical path of a directory, or nothing if unresolvable.
    (cd "$1" 2>/dev/null && pwd -P) || true
}

run_worker_canary() {
    python3 - "$REPO_ROOT" "$CANARY_FIXTURE" <<'PY'
import json
import os
import re
import shutil
import subprocess
import sys
import time
from pathlib import Path

repo_root = Path(sys.argv[1])
fixture = sys.argv[2] if len(sys.argv) > 2 else ""
now_ms = int(time.time() * 1000)

FIXTURES = {
    "healthy": {
        "status": "healthy",
        "selectedWorker": "trj",
        "roots": {
            "dataProjects": {"status": "present", "resolves": True, "readable": True, "acceptedByPolicy": True, "outerWorkspaceHazard": False},
            "dp": {"status": "present", "resolves": True, "readable": True, "acceptedByPolicy": True, "outerWorkspaceHazard": False},
            "isolatedSyncParent": {"status": "present", "resolves": True, "readable": True, "acceptedByPolicy": True, "outerWorkspaceHazard": False},
        },
        "rchStatus": {"status": "ok", "timedOut": False},
    },
    "missing-root": {
        "status": "missing_root",
        "selectedWorker": "trj",
        "roots": {
            "dataProjects": {"status": "missing", "resolves": False, "readable": False, "acceptedByPolicy": False, "outerWorkspaceHazard": False},
            "dp": {"status": "present", "resolves": True, "readable": True, "acceptedByPolicy": True, "outerWorkspaceHazard": False},
            "isolatedSyncParent": {"status": "present", "resolves": True, "readable": True, "acceptedByPolicy": True, "outerWorkspaceHazard": False},
        },
        "rchStatus": {"status": "ok", "timedOut": False},
    },
    "outer-workspace-shadowed": {
        "status": "outer_workspace_shadowed",
        "selectedWorker": "trj",
        "roots": {
            "dataProjects": {"status": "present", "resolves": True, "readable": True, "acceptedByPolicy": False, "outerWorkspaceHazard": True},
            "dp": {"status": "present", "resolves": True, "readable": True, "acceptedByPolicy": True, "outerWorkspaceHazard": False},
            "isolatedSyncParent": {"status": "present", "resolves": True, "readable": True, "acceptedByPolicy": True, "outerWorkspaceHazard": False},
        },
        "rchStatus": {"status": "ok", "timedOut": False},
    },
    "timeout": {
        "status": "timeout",
        "selectedWorker": None,
        "roots": {
            "dataProjects": {"status": "unknown", "resolves": None, "readable": None, "acceptedByPolicy": None, "outerWorkspaceHazard": None},
            "dp": {"status": "unknown", "resolves": None, "readable": None, "acceptedByPolicy": None, "outerWorkspaceHazard": None},
            "isolatedSyncParent": {"status": "unknown", "resolves": None, "readable": None, "acceptedByPolicy": None, "outerWorkspaceHazard": None},
        },
        "rchStatus": {"status": "timeout", "timedOut": True},
    },
    "permission-denied": {
        "status": "permission_denied",
        "selectedWorker": "trj",
        "roots": {
            "dataProjects": {"status": "permission_denied", "resolves": True, "readable": False, "acceptedByPolicy": False, "outerWorkspaceHazard": False},
            "dp": {"status": "present", "resolves": True, "readable": True, "acceptedByPolicy": True, "outerWorkspaceHazard": False},
            "isolatedSyncParent": {"status": "present", "resolves": True, "readable": True, "acceptedByPolicy": True, "outerWorkspaceHazard": False},
        },
        "rchStatus": {"status": "ok", "timedOut": False},
    },
}

def root_probe(path_text, policy_label):
    path = Path(path_text)
    exists = path.exists()
    resolves = False
    readable = False
    outer_hazard = False
    status = "missing"
    if exists:
        try:
            path.resolve(strict=True)
            resolves = True
        except OSError:
            resolves = False
        readable = os.access(path, os.R_OK | os.X_OK)
        if not readable:
            status = "permission_denied"
        elif resolves:
            status = "present"
        else:
            status = "unresolved"
        parent = path.parent
        outer_hazard = (parent / "Cargo.toml").exists() and parent != repo_root
    accepted = bool(exists and resolves and readable and not outer_hazard)
    return {
        "label": policy_label,
        "status": status,
        "resolves": resolves,
        "readable": readable,
        "acceptedByPolicy": accepted,
        "outerWorkspaceHazard": outer_hazard,
    }

def run_probe(argv, timeout_s=3, include_stdout=False):
    try:
        proc = subprocess.run(
            argv,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            timeout=timeout_s,
            check=False,
        )
    except subprocess.TimeoutExpired:
        return {"status": "timeout", "timedOut": True, "exitCode": None}
    except OSError as exc:
        return {"status": "unavailable", "timedOut": False, "exitCode": None, "message": str(exc)}
    return {
        "status": "ok" if proc.returncode == 0 else "error",
        "timedOut": False,
        "exitCode": proc.returncode,
        "stdoutBytes": len(proc.stdout.encode()),
        "stderrBytes": len(proc.stderr.encode()),
        **({"stdout": proc.stdout} if include_stdout else {}),
    }

def first_key(node, wanted):
    if isinstance(node, dict):
        for key, value in node.items():
            if key == wanted:
                return value
            found = first_key(value, wanted)
            if found is not None:
                return found
    elif isinstance(node, list):
        for item in node:
            found = first_key(item, wanted)
            if found is not None:
                return found
    return None

def first_worker_name(node):
    workers = first_key(node, "workers")
    if not isinstance(workers, list):
        return None
    for worker in workers:
        if isinstance(worker, str) and worker:
            return worker
        if isinstance(worker, dict):
            name = worker.get("id") or worker.get("name") or worker.get("worker_id")
            if name:
                return str(name)
    return None

def runtime_summary():
    rch_bin = os.environ.get("RCH_BIN_PATH") or shutil.which("rch")
    client_version = None
    if rch_bin:
        result = run_probe([rch_bin, "--version"], timeout_s=2)
        if result.get("status") == "ok":
            try:
                proc = subprocess.run([rch_bin, "--version"], stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True, timeout=2, check=False)
                match = re.search(r"\b\d+\.\d+(?:\.\d+)?\b", proc.stdout)
                client_version = match.group(0) if match else None
            except (OSError, subprocess.TimeoutExpired):
                client_version = None
    status = run_probe([rch_bin, "status", "--json"], timeout_s=3, include_stdout=True) if rch_bin else {"status": "unavailable", "timedOut": False, "exitCode": None}
    status_payload = None
    stdout = status.pop("stdout", None)
    if stdout:
        try:
            status_payload = json.loads(stdout)
        except json.JSONDecodeError:
            status_payload = None
    daemon_version = first_key(status_payload, "version") if status_payload is not None else None
    selected_worker = first_worker_name(status_payload) if status_payload is not None else None
    return {
        "clientVersion": client_version,
        "daemonVersion": str(daemon_version) if daemon_version else None,
        "selectedWorker": selected_worker,
        "statusProbe": status,
    }

def live_report():
    roots = {
        "dataProjects": root_probe("/data/projects", "absolute_data_projects"),
        "dp": root_probe("/dp", "global_dp"),
        "isolatedSyncParent": root_probe(os.environ.get("RCH_SYNC_ROOT_PARENT", "/tmp/rch-sync"), "isolated_sync_parent"),
    }
    rch = runtime_summary()
    status = "healthy"
    if rch["statusProbe"].get("timedOut"):
        status = "timeout"
    elif any(root["status"] == "permission_denied" for root in roots.values()):
        status = "permission_denied"
    elif any(root["status"] == "missing" for root in roots.values()):
        status = "missing_root"
    elif any(root["outerWorkspaceHazard"] for root in roots.values()):
        status = "outer_workspace_shadowed"
    elif not all(root["acceptedByPolicy"] for root in roots.values()):
        status = "topology_refused"
    selected = os.environ.get("RCH_WORKER") or os.environ.get("RCH_WORKERS") or rch.get("selectedWorker")
    return {
        "status": status,
        "selectedWorker": selected,
        "roots": roots,
        "rchStatus": rch["statusProbe"],
        "runtime": {"clientVersion": rch["clientVersion"], "daemonVersion": rch["daemonVersion"]},
    }

if fixture:
    if fixture not in FIXTURES:
        raise SystemExit(f"unknown worker canary fixture: {fixture}")
    body = dict(FIXTURES[fixture])
    body["fixture"] = fixture
else:
    body = live_report()
    body["fixture"] = None

report = {
    "schema": "ee.rch.worker_root_canary.v1",
    "generatedAtMs": now_ms,
    "status": body["status"],
    "selectedWorker": body.get("selectedWorker"),
    "roots": body["roots"],
    "rchStatus": body["rchStatus"],
    "runtime": body.get("runtime", {"clientVersion": None, "daemonVersion": None}),
    "fixture": body.get("fixture"),
    "bounded": True,
    "mutatesWorkers": False,
    "runsCargo": False,
    "recoveryActions": [
        {
            "priority": 1,
            "kind": "read_only_lane_doctor",
            "command": "scripts/rch_lane_doctor.sh --json",
            "message": "Inspect local root mapping and emit dispatch-only topology overrides when appropriate.",
        }
    ],
}
print(json.dumps(report, sort_keys=True, separators=(",", ":")))
PY
}

run_recurrence_evidence() {
    python3 - "$REPO_ROOT" "$RECURRENCE_PROOF_JSON" "$RECURRENCE_MANIFEST" "$RECURRENCE_CANARY_FIXTURE" <<'PY'
import hashlib
import json
import os
import shutil
import subprocess
import sys
import time
from pathlib import Path

repo_root = Path(sys.argv[1]).resolve()
proof_arg = sys.argv[2]
manifest_arg = sys.argv[3]
canary_fixture = sys.argv[4]
script_path = repo_root / "scripts" / "rch_lane_doctor.sh"


def resolve_repo_path(value):
    path = Path(value)
    if not path.is_absolute():
        path = repo_root / path
    return path.resolve(strict=False)


def repo_relative(path):
    try:
        return str(path.resolve(strict=False).relative_to(repo_root))
    except ValueError:
        return f"<outside-repo>/{path.name}"


def sha256_file(path):
    if not path.exists() or not path.is_file():
        return None
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return "sha256:" + digest.hexdigest()


def redact_text(value):
    text = str(value)
    replacements = {
        str(repo_root): "<repo>",
        str(Path.home()): "<home>",
        "/Users/jemanuel": "<home>",
    }
    for needle, replacement in replacements.items():
        text = text.replace(needle, replacement)
    if len(text) > 400:
        text = text[:397] + "..."
    return text


def redact_json(value):
    if isinstance(value, dict):
        return {str(key): redact_json(item) for key, item in value.items()}
    if isinstance(value, list):
        return [redact_json(item) for item in value]
    if isinstance(value, str):
        return redact_text(value)
    return value


def run_probe(argv, timeout_s):
    try:
        proc = subprocess.run(
            argv,
            cwd=repo_root,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            timeout=timeout_s,
            check=False,
        )
        payload = None
        if proc.stdout.strip():
            try:
                payload = json.loads(proc.stdout)
            except json.JSONDecodeError:
                payload = None
        return {
            "exitCode": proc.returncode,
            "timedOut": False,
            "stdoutBytes": len(proc.stdout.encode()),
            "stderrBytes": len(proc.stderr.encode()),
            "payload": payload,
            "stderrPreview": redact_text(proc.stderr.strip().splitlines()[0]) if proc.stderr.strip() else None,
        }
    except subprocess.TimeoutExpired as exc:
        stdout = exc.stdout if isinstance(exc.stdout, str) else ""
        stderr = exc.stderr if isinstance(exc.stderr, str) else ""
        return {
            "exitCode": None,
            "timedOut": True,
            "stdoutBytes": len(stdout.encode()),
            "stderrBytes": len(stderr.encode()),
            "payload": None,
            "stderrPreview": "timeout",
        }
    except OSError as exc:
        return {
            "exitCode": None,
            "timedOut": False,
            "stdoutBytes": 0,
            "stderrBytes": 0,
            "payload": None,
            "stderrPreview": redact_text(exc),
        }


def first_key(node, wanted):
    if isinstance(node, dict):
        for key, value in node.items():
            if key == wanted:
                return value
            found = first_key(value, wanted)
            if found is not None:
                return found
    elif isinstance(node, list):
        for item in node:
            found = first_key(item, wanted)
            if found is not None:
                return found
    return None


def load_json_file(path):
    if not path.exists():
        return None
    try:
        with path.open("r", encoding="utf-8") as handle:
            return json.load(handle)
    except (OSError, json.JSONDecodeError):
        return None


def summarize_worker_canary(payload, probe):
    if not isinstance(payload, dict):
        return {
            "status": "timeout" if probe["timedOut"] else "unavailable",
            "schema": None,
            "runsCargo": False,
            "mutatesWorkers": False,
        }
    return {
        "schema": payload.get("schema"),
        "status": payload.get("status"),
        "fixture": payload.get("fixture"),
        "selectedWorker": payload.get("selectedWorker"),
        "roots": payload.get("roots", {}),
        "runtime": payload.get("runtime", {"clientVersion": None, "daemonVersion": None}),
        "rchStatus": payload.get("rchStatus", {}),
        "bounded": payload.get("bounded") is True,
        "runsCargo": payload.get("runsCargo") is True,
        "mutatesWorkers": payload.get("mutatesWorkers") is True,
    }


def summarize_tripwire(payload, probe):
    if not isinstance(payload, dict):
        return {
            "schema": None,
            "status": "timeout" if probe["timedOut"] else "unavailable",
            "count": None,
            "forbiddenWorktreeCount": None,
            "localBuildPolicyStatus": "unknown",
            "worktreePolicyStatus": "unknown",
            "runsCargo": False,
        }
    local_policy = payload.get("localBuildPolicy") if isinstance(payload.get("localBuildPolicy"), dict) else {}
    worktree_policy = payload.get("worktreePolicy") if isinstance(payload.get("worktreePolicy"), dict) else {}
    return {
        "schema": payload.get("schema"),
        "status": payload.get("status"),
        "count": payload.get("count"),
        "forbiddenWorktreeCount": payload.get("forbiddenWorktreeCount"),
        "localBuildPolicyStatus": local_policy.get("status"),
        "worktreePolicyStatus": worktree_policy.get("status"),
        "detectedLocalBuildCount": len(payload.get("detectedLocalBuilds", [])) if isinstance(payload.get("detectedLocalBuilds"), list) else payload.get("count"),
        "runsCargo": False,
    }


def summarize_cycles(payload, probe):
    if not isinstance(payload, dict):
        return {
            "status": "timeout" if probe["timedOut"] else "unavailable",
            "count": None,
            "cycles": [],
        }
    count = payload.get("count")
    cycles = payload.get("cycles") if isinstance(payload.get("cycles"), list) else []
    if count is None:
        count = len(cycles)
    return {
        "status": "clear" if count == 0 else "cycles_detected",
        "count": count,
        "cycles": redact_json(cycles),
    }


def summarize_topology_audit(probe):
    payload = probe.get("payload")
    if probe.get("timedOut"):
        return {
            "status": "timeout",
            "schema": None,
            "exitCode": None,
            "runsCargo": False,
            "pathClosureStatus": "unknown",
        }
    if not isinstance(payload, dict):
        return {
            "status": "unavailable",
            "schema": None,
            "exitCode": probe.get("exitCode"),
            "runsCargo": False,
            "pathClosureStatus": "unknown",
            "message": probe.get("stderrPreview"),
        }
    if payload.get("schema") == "ee.error.v2":
        error = payload.get("error") if isinstance(payload.get("error"), dict) else {}
        message = str(error.get("message", ""))
        code = error.get("code")
        if code == "usage" and "topology-audit" in message:
            status = "ee_surface_unavailable"
        else:
            status = "reported_error"
        return {
            "status": status,
            "schema": payload.get("schema"),
            "exitCode": probe.get("exitCode"),
            "runsCargo": False,
            "pathClosureStatus": "not_collected",
            "error": {
                "code": code,
                "message": redact_text(message),
                "severity": error.get("severity"),
            },
        }
    unresolved = first_key(payload, "unresolvedTopologyEdges")
    closure_hash = first_key(payload, "pathClosureHash") or first_key(payload, "closureHash")
    closure_status = first_key(payload, "pathClosureStatus") or first_key(payload, "status")
    return {
        "status": "completed" if probe.get("exitCode") == 0 else "reported_error",
        "schema": payload.get("schema"),
        "exitCode": probe.get("exitCode"),
        "runsCargo": False,
        "pathClosureStatus": redact_json(closure_status) if closure_status is not None else "reported",
        "pathClosureHash": closure_hash,
        "unresolvedTopologyEdges": redact_json(unresolved) if unresolved is not None else [],
    }


def classify_proof(payload):
    if not isinstance(payload, dict):
        return {
            "status": "proof_unavailable",
            "sourceVerdict": "no_rust_verdict",
            "remoteSourceMaterialized": None,
            "unresolvedTopologyEdge": None,
        }
    error_codes = payload.get("error_codes", [])
    degraded_codes = payload.get("degraded_codes", [])
    selector = payload.get("selector_admission_probe") if isinstance(payload.get("selector_admission_probe"), dict) else {}
    known = payload.get("known_blocker") if isinstance(payload.get("known_blocker"), dict) else {}
    stderr_tail = str(payload.get("stderr_tail", ""))
    topology_blocked = (
        "RCH-E327" in error_codes
        or "rch_verify_topology_blocked" in degraded_codes
        or selector.get("selection_failure_reason") == "topology_blocked"
        or "Path dependency topology policy failed" in stderr_tail
    )
    active_project_exclusion = (
        selector.get("selection_failure_reason") == "active_project_exclusion"
        or "active_project_exclusion" in degraded_codes
        or known.get("blocker_kind") == "active_project_exclusion"
    )
    edge = None
    if topology_blocked:
        edge = {
            "kind": "path_dependency_topology_policy_failed",
            "errorCode": "RCH-E327" if "RCH-E327" in error_codes else None,
            "selectorFailureReason": selector.get("selection_failure_reason"),
            "remediationBead": known.get("remediation_bead"),
            "blockerFingerprint": known.get("blocker_fingerprint"),
            "retryAfter": known.get("retry_after"),
        }
    status = "topology_blocked" if topology_blocked else "active_project_exclusion" if active_project_exclusion else "not_recurrent"
    source_state = payload.get("source_state") if isinstance(payload.get("source_state"), dict) else {}
    return {
        "status": status,
        "beadId": payload.get("bead_id"),
        "commandText": payload.get("command_text"),
        "commandKind": payload.get("command_kind"),
        "remoteRequired": payload.get("remote_required"),
        "remoteSourceMaterialized": source_state.get("remote_source_materialized", payload.get("remote_source_materialized")),
        "errorCodes": error_codes,
        "degradedCodes": degraded_codes,
        "selectorAdmissionProbe": {
            "schema": selector.get("schema"),
            "status": selector.get("status"),
            "selectionFailureReason": selector.get("selection_failure_reason"),
            "remoteRequired": selector.get("remote_required"),
            "localFallbackRefused": selector.get("local_fallback_refused"),
        },
        "knownBlocker": {
            "blockerFingerprint": known.get("blocker_fingerprint"),
            "blockerKind": known.get("blocker_kind"),
            "remediationBead": known.get("remediation_bead"),
            "retryAfter": known.get("retry_after"),
        },
        "unresolvedTopologyEdge": edge,
        "sourceVerdict": "no_rust_verdict",
        "stderrClassifier": "path_dependency_topology_policy_failed" if topology_blocked else "not_classified",
    }


proof_path = resolve_repo_path(proof_arg)
manifest_path = resolve_repo_path(manifest_arg)
proof_payload = load_json_file(proof_path)
proof_summary = classify_proof(proof_payload)

canary_args = [str(script_path)]
if canary_fixture:
    canary_args.extend(["--worker-canary-fixture", canary_fixture])
else:
    canary_args.append("--worker-canary")
canary_probe = run_probe(canary_args, 10)
worker_canary = summarize_worker_canary(canary_probe.get("payload"), canary_probe)

tripwire_path = repo_root / "scripts" / "check-local-cargo-tripwire.sh"
if tripwire_path.exists():
    tripwire_probe = run_probe([str(tripwire_path), "--probe-processes", "--json"], 20)
else:
    tripwire_probe = {"payload": None, "timedOut": False, "exitCode": None, "stderrPreview": "script unavailable"}
local_tripwire = summarize_tripwire(tripwire_probe.get("payload"), tripwire_probe)

br_bin = shutil.which("br")
if br_bin:
    cycles_probe = run_probe([br_bin, "dep", "cycles", "--json"], 20)
else:
    cycles_probe = {"payload": None, "timedOut": False, "exitCode": None, "stderrPreview": "br unavailable"}
beads_cycles = summarize_cycles(cycles_probe.get("payload"), cycles_probe)

topology_command = [
    "ee",
    "verify",
    "rch",
    "topology-audit",
    "--from-json",
    repo_relative(proof_path),
    "--manifest",
    repo_relative(manifest_path),
    "--json",
]
ee_bin = shutil.which("ee")
if ee_bin:
    topology_probe = run_probe([ee_bin, "verify", "rch", "topology-audit", "--from-json", str(proof_path), "--manifest", str(manifest_path), "--json"], 20)
else:
    topology_probe = {"payload": None, "timedOut": False, "exitCode": None, "stderrPreview": "ee unavailable"}
topology_audit = summarize_topology_audit(topology_probe)

local_cargo_clean = (
    local_tripwire.get("status") == "ok"
    and local_tripwire.get("count") == 0
    and local_tripwire.get("forbiddenWorktreeCount") == 0
)
cycles_clear = beads_cycles.get("count") == 0
surface_gaps = []
if topology_audit.get("status") == "ee_surface_unavailable":
    surface_gaps.append("ee_verify_topology_audit_unavailable")
if local_tripwire.get("status") in {"unavailable", "timeout"}:
    surface_gaps.append("local_cargo_tripwire_unavailable")
if beads_cycles.get("status") == "unavailable":
    surface_gaps.append("br_dep_cycles_unavailable")

if proof_summary.get("unresolvedTopologyEdge"):
    final_outcome = "environment_blocked_actionable"
elif topology_audit.get("status") == "completed" and not topology_audit.get("unresolvedTopologyEdges"):
    final_outcome = "topology_closure_proven"
else:
    final_outcome = "evidence_incomplete"

report = {
    "schema": "ee.rch.topology_recurrence_evidence.v1",
    "generatedAtMs": int(time.time() * 1000),
    "status": final_outcome,
    "sourceVerdict": "no_rust_verdict",
    "proof": {
        "path": repo_relative(proof_path),
        "exists": proof_path.exists(),
        "contentSha256": sha256_file(proof_path),
        "classifier": proof_summary,
    },
    "manifest": {
        "path": repo_relative(manifest_path),
        "exists": manifest_path.exists(),
        "contentSha256": sha256_file(manifest_path),
    },
    "workerCanary": worker_canary,
    "topologyAudit": topology_audit,
    "pathClosureAudit": {
        "status": topology_audit.get("pathClosureStatus", "unknown"),
        "hash": topology_audit.get("pathClosureHash"),
        "command": " ".join(topology_command),
        "runsCargo": False,
    },
    "localCargoTripwire": local_tripwire,
    "beadsCycles": beads_cycles,
    "focusedRchRetry": {
        "attempted": False,
        "beadId": proof_summary.get("beadId"),
        "commandText": proof_summary.get("commandText"),
        "preconditions": [
            "topology evidence changed or RCH owner requests override",
            "localCargoTripwire.status == ok",
            "beadsCycles.count == 0",
            "RCH_REQUIRE_REMOTE=1",
        ],
    },
    "surfaceGaps": surface_gaps,
    "proofDiscipline": {
        "sourceBeadClosePolicy": "do_not_close_application_source_on_topology_blocked_evidence_alone",
        "topologyBlockedEvidenceIsEnvironmentProof": True,
        "remoteCargoReached": proof_summary.get("remoteSourceMaterialized") is True,
        "localCargoTripwireClean": local_cargo_clean,
        "beadsCyclesClear": cycles_clear,
    },
    "nextCommands": [
        {"kind": "worker_canary", "command": "scripts/rch_lane_doctor.sh --worker-canary"},
        {"kind": "topology_audit", "command": " ".join(topology_command)},
        {"kind": "local_cargo_tripwire", "command": "scripts/check-local-cargo-tripwire.sh --probe-processes --json"},
        {"kind": "cycle_check", "command": "br dep cycles --json"},
    ],
    "redaction": {
        "rawStderrEmbedded": False,
        "hostPrivatePathsRedacted": True,
        "rawWorkerLogsEmbedded": False,
    },
    "runsCargo": False,
    "mutatesWorkers": False,
    "mutatesState": False,
    "bounded": True,
}

print(json.dumps(report, sort_keys=True, separators=(",", ":")))
PY
}

run_self_test() {
    local case_name output
    for case_name in healthy missing-root outer-workspace-shadowed timeout permission-denied; do
        output="$("$0" --worker-canary-fixture "$case_name")"
        python3 - "$case_name" "$output" <<'PY'
import json
import sys

case_name = sys.argv[1]
report = json.loads(sys.argv[2])
expected_status = {
    "healthy": "healthy",
    "missing-root": "missing_root",
    "outer-workspace-shadowed": "outer_workspace_shadowed",
    "timeout": "timeout",
    "permission-denied": "permission_denied",
}[case_name]
assert report["schema"] == "ee.rch.worker_root_canary.v1"
assert report["fixture"] == case_name
assert report["status"] == expected_status
assert report["runsCargo"] is False
assert report["mutatesWorkers"] is False
for root_name in ("dataProjects", "dp", "isolatedSyncParent"):
    assert root_name in report["roots"], root_name
PY
    done
    output="$("$0" --recurrence-evidence --recurrence-canary-fixture missing-root)"
    python3 - "$output" <<'PY'
import json
import sys

report = json.loads(sys.argv[1])
assert report["schema"] == "ee.rch.topology_recurrence_evidence.v1"
assert report["runsCargo"] is False
assert report["mutatesWorkers"] is False
assert report["mutatesState"] is False
assert report["workerCanary"]["schema"] == "ee.rch.worker_root_canary.v1"
assert report["workerCanary"]["fixture"] == "missing-root"
assert report["proof"]["classifier"]["sourceVerdict"] == "no_rust_verdict"
assert report["proofDiscipline"]["sourceBeadClosePolicy"] == "do_not_close_application_source_on_topology_blocked_evidence_alone"
assert report["redaction"]["rawStderrEmbedded"] is False
assert report["status"] in {
    "environment_blocked_actionable",
    "topology_closure_proven",
    "evidence_incomplete",
}
PY
    printf 'rch_lane_doctor worker-canary and recurrence-evidence self-test passed\n'
}

case "$MODE" in
    worker-canary|worker-canary-fixture)
        run_worker_canary
        exit 0
        ;;
    recurrence-evidence)
        run_recurrence_evidence
        exit 0
        ;;
    self-test)
        run_self_test
        exit 0
        ;;
esac

data_target=""
data_resolves=false
if [ -L /data ]; then
    data_target="$(readlink /data)"
    [ -n "$(resolve_dir /data)" ] && data_resolves=true
elif [ -d /data ]; then
    data_target="/data"
    data_resolves=true
fi

dp_target=""
dp_resolves=false
if [ -L /dp ]; then
    dp_target="$(readlink /dp)"
    [ -n "$(resolve_dir /dp)" ] && dp_resolves=true
elif [ -d /dp ]; then
    dp_target="/dp"
    dp_resolves=true
fi

# Collect path-dependency roots from the [patch.crates-io] table. Each
# unique first path component (relative to the repo) is one sibling root.
sibling_json=""
escaped_sibling_count=0
unresolvable_sibling_count=0
sibling_total=0
while IFS= read -r root_decl; do
    canonical="$(resolve_dir "$root_decl")"
    sibling_total=$((sibling_total + 1))
    under_root=false
    resolvable=false
    if [ -n "$canonical" ]; then
        resolvable=true
        case "$canonical" in
            "$DEFAULT_CANONICAL_ROOT"/* ) under_root=true ;;
        esac
    else
        unresolvable_sibling_count=$((unresolvable_sibling_count + 1))
    fi
    if [ "$resolvable" = true ] && [ "$under_root" = false ]; then
        escaped_sibling_count=$((escaped_sibling_count + 1))
    fi
    entry=$(printf '{"declaredPath":"%s","canonicalPath":"%s","resolvable":%s,"underDefaultCanonicalRoot":%s}' \
        "$root_decl" "${canonical:-}" "$resolvable" "$under_root")
    if [ -n "$sibling_json" ]; then
        sibling_json="$sibling_json,$entry"
    else
        sibling_json="$entry"
    fi
done < <(sed -n '/^\[patch\.crates-io\]/,/^\[/p' "$REPO_ROOT/Cargo.toml" \
    | sed -n 's/.*path = "\([^"]*\)".*/\1/p' \
    | while IFS= read -r dep_path; do
        case "$dep_path" in
            /* ) declared="$dep_path" ;;
            *  ) declared="$REPO_ROOT/$dep_path" ;;
        esac
        # Reduce to the sibling root (the directory above crates/<crate>).
        case "$declared" in
            */crates/* ) declared="${declared%%/crates/*}" ;;
        esac
        printf '%s\n' "$declared"
    done | sort -u)

lane_state="indeterminate"
exit_code=3
if [ "$sibling_total" -gt 0 ] && [ "$escaped_sibling_count" -eq 0 ] && [ "$unresolvable_sibling_count" -eq 0 ]; then
    lane_state="healthy"
    exit_code=0
elif [ "$usb_mounted" = false ] && { [ "$escaped_sibling_count" -gt 0 ] || [ "$unresolvable_sibling_count" -gt 0 ]; }; then
    lane_state="usb_detached_dual_blocker"
    exit_code=2
fi

override_canonical="$HOME"
override_alias="/dp"
[ "$dp_resolves" = true ] || override_alias="/data"

if [ "$MODE" = "emit-env" ]; then
    if [ "$lane_state" = "usb_detached_dual_blocker" ]; then
        printf 'export RCH_CANONICAL_PROJECT_ROOT=%s\n' "$override_canonical"
        printf 'export RCH_ALIAS_PROJECT_ROOT=%s\n' "$override_alias"
    fi
    exit "$exit_code"
fi

printf '{"schema":"ee.rch_lane_doctor.v1","laneState":"%s","remediationBead":"bd-2qpgn","usbVolumeMounted":%s,"dataSymlink":{"target":"%s","resolves":%s},"dpSymlink":{"target":"%s","resolves":%s},"defaultCanonicalRoot":"%s","siblingRoots":[%s],"escapedSiblingCount":%s,"unresolvableSiblingCount":%s,"recommendation":{"envOverride":{"RCH_CANONICAL_PROJECT_ROOT":"%s","RCH_ALIAS_PROJECT_ROOT":"%s"},"appliesWhen":"usb_detached_dual_blocker","exampleCommand":"eval \\"$(scripts/rch_lane_doctor.sh --emit-env)\\" && TMPDIR=/private/tmp RCH_VISIBILITY=summary RCH_TEST_TIMEOUT_SEC=3600 scripts/rch_verify.sh --summary -- cargo test --lib","note":"override broadens the dispatch-local topology roots only; project hash changes (fresh lane, cold first build - set RCH_TEST_TIMEOUT_SEC=3600 or the remote 1800s test budget times out mid-compile with RCH-E104). Verified 2026-06-12 on bd-2qpgn."}}\n' \
    "$lane_state" "$usb_mounted" "$data_target" "$data_resolves" "$dp_target" "$dp_resolves" \
    "$DEFAULT_CANONICAL_ROOT" "$sibling_json" "$escaped_sibling_count" "$unresolvable_sibling_count" \
    "$override_canonical" "$override_alias"
exit "$exit_code"
