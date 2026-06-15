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
while [ "$#" -gt 0 ]; do
    case "${1:-}" in
        --json ) MODE="json" ;;
        --emit-env ) MODE="emit-env" ;;
        --worker-canary|--canary ) MODE="worker-canary" ;;
        --worker-canary-fixture )
            MODE="worker-canary-fixture"
            shift
            CANARY_FIXTURE="${1:-}"
            [ -n "$CANARY_FIXTURE" ] || { usage >&2; exit 64; }
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
    printf 'rch_lane_doctor worker-canary self-test passed\n'
}

case "$MODE" in
    worker-canary|worker-canary-fixture)
        run_worker_canary
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
