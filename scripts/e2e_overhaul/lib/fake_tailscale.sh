#!/usr/bin/env bash
# bd-36bbk.1.10 — deterministic fake Tailscale harness for SRR6.46 e2e tests.
#
# Source this library from SRR6.46 e2e scripts. It creates a local fixture tree,
# prepends a `tailscale` shim to PATH, and emits ee.test_event.v1 JSONL events.
# It never requires a real tailscaled process, real tailnet credentials, or
# network access.

set -euo pipefail

FT_SCENARIO_DIR="${FT_SCENARIO_DIR:-}"
FT_OLD_PATH="${PATH:-}"
FT_RESPONDER_PIDS="${FT_RESPONDER_PIDS:-}"
FT_RESPONDER_SOCKETS="${FT_RESPONDER_SOCKETS:-}"
FT_EVENT_FILE="${FT_EVENT_FILE:-}"
FT_FIXED_NOW="${EE_TEST_NOW:-2026-05-16T00:00:00Z}"

_ft_script_dir() {
    cd "$(dirname "${BASH_SOURCE[0]}")" && pwd
}

_ft_now_ms() {
    python3 - <<'PY'
import time
print(int(time.time() * 1000))
PY
}

_ft_hash() {
    python3 - "$1" <<'PY'
import hashlib
import sys
print("sha256:" + hashlib.sha256(sys.argv[1].encode("utf-8")).hexdigest())
PY
}

_ft_node_key() {
    python3 - "$1" <<'PY'
import base64
import hashlib
import sys
digest = hashlib.sha256(sys.argv[1].encode("utf-8")).digest()[:20]
print("nodekey:" + base64.b32encode(digest).decode("ascii").rstrip("=").lower())
PY
}

_ft_short_id() {
    python3 - "$1" <<'PY'
import hashlib
import sys
print(hashlib.sha256(sys.argv[1].encode("utf-8")).hexdigest()[:12])
PY
}

_ft_require_scenario() {
    if [ -z "${FT_SCENARIO_DIR:-}" ] || [ ! -d "$FT_SCENARIO_DIR" ]; then
        echo "fake_tailscale: call ft_init <scenario_dir> first" >&2
        return 2
    fi
}

_ft_state_path() {
    printf '%s/state.json' "$FT_SCENARIO_DIR"
}

_ft_status_path() {
    printf '%s/tailscale_status.json' "$FT_SCENARIO_DIR"
}

_ft_rewrite_status() {
    _ft_require_scenario
    python3 - "$(_ft_state_path)" "$(_ft_status_path)" <<'PY'
import json
import sys
from pathlib import Path

state_path, status_path = map(Path, sys.argv[1:3])
state = json.loads(state_path.read_text(encoding="utf-8"))
daemon = state["daemon_state"]
self_node = state["self"]
authenticated = bool(self_node.get("authenticated", True)) and daemon == "running"
backend = {
    "running": "Running" if authenticated else "NeedsLogin",
    "unreachable": "NoState",
    "not_authenticated": "NeedsLogin",
    "not_installed": "NoState",
}.get(daemon, "NoState")
peer_map = {}
for peer in sorted(state["peers"], key=lambda item: item["node_key"]):
    peer_map[peer["node_key"]] = {
        "ID": peer["node_key"],
        "HostName": peer["hostname"],
        "DNSName": f"{peer['hostname']}.tailnet.test.",
        "TailscaleIPs": [peer["ip"]],
        "Tags": peer.get("tags", []),
        "Online": bool(peer.get("respond", True)),
        "Capabilities": {
            "eeVersion": peer.get("ee_version", ""),
            "eeProtocol": peer.get("ee_protocol", ""),
            "workspaceIds": peer.get("workspace_ids", []),
            "respond": bool(peer.get("respond", True)),
            "latencyMs": int(peer.get("latency_ms", 0)),
        },
    }
status = {
    "Version": "fake-tailscale.v1",
    "BackendState": backend,
    "AuthURL": "" if authenticated else "https://login.tailscale.test",
    "TUN": True,
    "Self": {
        "ID": self_node["node_key"],
        "HostName": self_node["display_name"],
        "DNSName": f"{self_node['display_name']}.tailnet.test.",
        "TailscaleIPs": [self_node["ip"]],
        "Tailnet": self_node["tailnet_id"],
        "TailnetName": self_node["display_name"],
        "Authenticated": authenticated,
        "Platform": self_node.get("platform", "linux"),
    },
    "Peer": peer_map,
}
status_path.write_text(json.dumps(status, sort_keys=True, separators=(",", ":")) + "\n", encoding="utf-8")
PY
}

ft_emit_event() {
    _ft_require_scenario
    local phase="${1:?phase required}"
    local valid="${2:?valid required}"
    local detail="${3:?detail required}"
    local artifact_hash="${4:-}"
    mkdir -p "${EE_TEST_EVENT_DIR:-$FT_SCENARIO_DIR/events}"
    FT_EVENT_FILE="${FT_EVENT_FILE:-${EE_TEST_EVENT_DIR:-$FT_SCENARIO_DIR/events}/$(basename "${BASH_SOURCE[1]:-$0}" .sh).jsonl}"
    PHASE="$phase" VALID="$valid" DETAIL="$detail" ARTIFACT_HASH="$artifact_hash" \
    WORKSPACE_ID="${FT_WORKSPACE_ID:-fake-workspace}" \
    REQUEST_ID="${FT_REQUEST_ID:-fake-tailscale}" \
    BEAD_ID="${FT_BEAD_ID:-bd-36bbk.1.10}" \
    SURFACE="${FT_SURFACE:-fake_tailscale_e2e_harness}" \
    ELAPSED_MS="${FT_ELAPSED_MS:-0}" \
    EVENT_FILE="$FT_EVENT_FILE" \
    python3 - <<'PY'
import json
import os
from pathlib import Path

event = {
    "schema": "ee.test_event.v1",
    "kind": os.environ.get("FT_EVENT_KIND", "auto_enrollment_e2e"),
    "phase": os.environ["PHASE"],
    "valid": os.environ["VALID"].lower() == "true",
    "detail": os.environ["DETAIL"],
    "workspace_id": os.environ["WORKSPACE_ID"],
    "request_id": os.environ["REQUEST_ID"],
    "bead_id": os.environ["BEAD_ID"],
    "surface": os.environ["SURFACE"],
    "elapsed_ms": int(os.environ["ELAPSED_MS"]),
    "artifactHash": os.environ["ARTIFACT_HASH"],
}
path = Path(os.environ["EVENT_FILE"])
path.parent.mkdir(parents=True, exist_ok=True)
with path.open("a", encoding="utf-8") as handle:
    handle.write(json.dumps(event, sort_keys=True, separators=(",", ":")) + "\n")
PY
}

ft_init() {
    local scenario_dir="${1:?scenario dir required}"
    local scenario_name="${2:-custom}"
    FT_SCENARIO_DIR="$scenario_dir"
    export FT_SCENARIO_DIR
    export FAKE_TAILSCALE_SCENARIO_DIR="$FT_SCENARIO_DIR"
    mkdir -p "$FT_SCENARIO_DIR" "$FT_SCENARIO_DIR/responders" "$FT_SCENARIO_DIR/logs"
    FT_EVENT_FILE="${EE_TEST_EVENT_DIR:-$FT_SCENARIO_DIR/events}/$(basename "${BASH_SOURCE[1]:-$0}" .sh).jsonl"
    export FT_EVENT_FILE
    local node_key
    node_key="$(_ft_node_key "$scenario_name:self")"
    SCENARIO_NAME="$scenario_name" NODE_KEY="$node_key" FIXED_NOW="$FT_FIXED_NOW" \
    python3 - "$(_ft_state_path)" <<'PY'
import json
import os
import sys
from pathlib import Path

state = {
    "schema": "ee.fake_tailscale.state.v1",
    "scenario": os.environ["SCENARIO_NAME"],
    "generated_at": os.environ["FIXED_NOW"],
    "daemon_state": "running",
    "self": {
        "node_key": os.environ["NODE_KEY"],
        "ip": "100.64.0.1",
        "tailnet_id": "tailnet-alpha",
        "display_name": "ee-local",
        "platform": "linux",
        "authenticated": True,
    },
    "peers": [],
}
Path(sys.argv[1]).write_text(json.dumps(state, sort_keys=True, indent=2) + "\n", encoding="utf-8")
PY
    _ft_rewrite_status
    ft_emit_event "setup" "true" "fake tailscale scenario initialized" "$(_ft_hash "$scenario_name")"
}

ft_set_self() {
    _ft_require_scenario
    local node_key="${1:?node key required}"
    local ip="${2:?ip required}"
    local tailnet_id="${3:?tailnet id required}"
    local display_name="${4:?display name required}"
    shift 4
    local platform="linux"
    local authenticated="true"
    while [ "$#" -gt 0 ]; do
        case "$1" in
            --platform=*) platform="${1#*=}"; shift ;;
            --authenticated=*) authenticated="${1#*=}"; shift ;;
            *) echo "fake_tailscale: unknown ft_set_self arg: $1" >&2; return 2 ;;
        esac
    done
    NODE_KEY="$node_key" IP="$ip" TAILNET_ID="$tailnet_id" DISPLAY_NAME="$display_name" \
    PLATFORM="$platform" AUTHENTICATED="$authenticated" \
    python3 - "$(_ft_state_path)" <<'PY'
import json
import os
import sys
from pathlib import Path

path = Path(sys.argv[1])
state = json.loads(path.read_text(encoding="utf-8"))
state["self"] = {
    "node_key": os.environ["NODE_KEY"],
    "ip": os.environ["IP"],
    "tailnet_id": os.environ["TAILNET_ID"],
    "display_name": os.environ["DISPLAY_NAME"],
    "platform": os.environ["PLATFORM"],
    "authenticated": os.environ["AUTHENTICATED"].lower() == "true",
}
path.write_text(json.dumps(state, sort_keys=True, indent=2) + "\n", encoding="utf-8")
PY
    _ft_rewrite_status
}

ft_add_peer() {
    _ft_require_scenario
    local node_key="${1:?node key required}"
    local ip="${2:?ip required}"
    local hostname="${3:?hostname required}"
    shift 3
    local ee_version="0.0.0"
    local ee_protocol="0.0"
    local workspace_ids=""
    local tags=""
    local respond="true"
    local latency_ms="0"
    while [ "$#" -gt 0 ]; do
        case "$1" in
            --ee_version=*) ee_version="${1#*=}"; shift ;;
            --ee_protocol=*) ee_protocol="${1#*=}"; shift ;;
            --workspace_ids=*) workspace_ids="${1#*=}"; shift ;;
            --tag=*) tags="${tags}${tags:+,}${1#*=}"; shift ;;
            --respond=*) respond="${1#*=}"; shift ;;
            --latency_ms=*) latency_ms="${1#*=}"; shift ;;
            *) echo "fake_tailscale: unknown ft_add_peer arg: $1" >&2; return 2 ;;
        esac
    done
    NODE_KEY="$node_key" IP="$ip" HOSTNAME="$hostname" EE_VERSION="$ee_version" \
    EE_PROTOCOL="$ee_protocol" WORKSPACE_IDS="$workspace_ids" TAGS="$tags" RESPOND="$respond" \
    LATENCY_MS="$latency_ms" \
    python3 - "$(_ft_state_path)" <<'PY'
import json
import os
import sys
from pathlib import Path

path = Path(sys.argv[1])
state = json.loads(path.read_text(encoding="utf-8"))
node_key = os.environ["NODE_KEY"]
state["peers"] = [peer for peer in state["peers"] if peer["node_key"] != node_key]
state["peers"].append({
    "node_key": node_key,
    "ip": os.environ["IP"],
    "hostname": os.environ["HOSTNAME"],
    "ee_version": os.environ["EE_VERSION"],
    "ee_protocol": os.environ["EE_PROTOCOL"],
    "workspace_ids": [item for item in os.environ["WORKSPACE_IDS"].split(",") if item],
    "tags": [item for item in os.environ["TAGS"].split(",") if item],
    "respond": os.environ["RESPOND"].lower() == "true",
    "latency_ms": int(os.environ["LATENCY_MS"]),
})
state["peers"].sort(key=lambda item: item["node_key"])
path.write_text(json.dumps(state, sort_keys=True, indent=2) + "\n", encoding="utf-8")
PY
    _ft_rewrite_status
}

ft_remove_peer() {
    _ft_require_scenario
    local node_key="${1:?node key required}"
    NODE_KEY="$node_key" python3 - "$(_ft_state_path)" <<'PY'
import json
import os
import sys
from pathlib import Path

path = Path(sys.argv[1])
state = json.loads(path.read_text(encoding="utf-8"))
state["peers"] = [peer for peer in state["peers"] if peer["node_key"] != os.environ["NODE_KEY"]]
path.write_text(json.dumps(state, sort_keys=True, indent=2) + "\n", encoding="utf-8")
PY
    _ft_rewrite_status
}

ft_swap_tailnet() {
    _ft_require_scenario
    local new_tailnet_id="${1:?new tailnet id required}"
    local new_display_name="${2:?new display name required}"
    TAILNET_ID="$new_tailnet_id" DISPLAY_NAME="$new_display_name" \
    python3 - "$(_ft_state_path)" <<'PY'
import json
import os
import sys
from pathlib import Path

path = Path(sys.argv[1])
state = json.loads(path.read_text(encoding="utf-8"))
state["self"]["tailnet_id"] = os.environ["TAILNET_ID"]
state["self"]["display_name"] = os.environ["DISPLAY_NAME"]
path.write_text(json.dumps(state, sort_keys=True, indent=2) + "\n", encoding="utf-8")
PY
    _ft_rewrite_status
}

ft_set_daemon_state() {
    _ft_require_scenario
    local state_value="${1:?state required}"
    STATE_VALUE="$state_value" python3 - "$(_ft_state_path)" <<'PY'
import json
import os
import sys
from pathlib import Path

path = Path(sys.argv[1])
state = json.loads(path.read_text(encoding="utf-8"))
state["daemon_state"] = os.environ["STATE_VALUE"]
path.write_text(json.dumps(state, sort_keys=True, indent=2) + "\n", encoding="utf-8")
PY
    _ft_rewrite_status
}

ft_corrupt_status_json() {
    _ft_require_scenario
    local kind="${1:?kind required}"
    case "$kind" in
        truncated) printf '{"Version":"fake-tailscale.v1","Peer":' > "$(_ft_status_path)" ;;
        invalid_utf8) python3 - "$(_ft_status_path)" <<'PY'
import sys
from pathlib import Path
Path(sys.argv[1]).write_bytes(b'{"Version":"fake"}\xff\n')
PY
            ;;
        wrong_schema) printf '{"schema":"wrong","Peer":[]}\n' > "$(_ft_status_path)" ;;
        unknown_fields) python3 - "$(_ft_status_path)" <<'PY'
import json
import sys
from pathlib import Path
path = Path(sys.argv[1])
status = json.loads(path.read_text(encoding="utf-8"))
status["UnexpectedFakeField"] = {"why": "corrupt_status_json"}
path.write_text(json.dumps(status, sort_keys=True, separators=(",", ":")) + "\n", encoding="utf-8")
PY
            ;;
        *) echo "fake_tailscale: unknown corrupt kind: $kind" >&2; return 2 ;;
    esac
}

ft_shim_path() {
    _ft_require_scenario
    local shim_dir="$FT_SCENARIO_DIR/bin"
    mkdir -p "$shim_dir"
    local shim_path="$shim_dir/tailscale"
    if [ ! -e "$shim_path" ]; then
        ln -s "$(_ft_script_dir)/fake_tailscale_shim.sh" "$shim_path"
    fi
    PATH="$shim_dir:$PATH"
    export PATH
    printf '%s\n' "$shim_dir"
}

ft_run_responder() {
    _ft_require_scenario
    local node_key="${1:?node key required}"
    local node_id
    node_id="$(_ft_short_id "$node_key")"
    local run_id
    run_id="$(_ft_now_ms).$$"
    local socket_path
    socket_path="$FT_SCENARIO_DIR/responders/r-${node_id}.${run_id}.sock"
    local pid_path="$FT_SCENARIO_DIR/responders/r-${node_id}.${run_id}.pid"
    local responder_stdout="$FT_SCENARIO_DIR/logs/responder-${node_id}.${run_id}.stdout.log"
    local responder_stderr="$FT_SCENARIO_DIR/logs/responder-${node_id}.${run_id}.stderr.log"
    NODE_KEY="$node_key" SOCKET_PATH="$socket_path" STATE_PATH="$(_ft_state_path)" \
    python3 - <<'PY' > "$responder_stdout" 2> "$responder_stderr" &
import json
import os
import socket
from pathlib import Path

node_key = os.environ["NODE_KEY"]
socket_path = os.environ["SOCKET_PATH"]
state_path = Path(os.environ["STATE_PATH"])
server = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
server.bind(socket_path)
server.listen(1)
while True:
    conn, _ = server.accept()
    with conn:
        request = conn.recv(65536)
        state = json.loads(state_path.read_text(encoding="utf-8"))
        peer = next((item for item in state["peers"] if item["node_key"] == node_key), None)
        if peer is None:
            response = {"schema": "ee.mesh.hello.v1", "accepted": False, "reason": "unknown_peer"}
        else:
            response = {
                "schema": "ee.mesh.hello.v1",
                "accepted": bool(peer.get("respond", True)),
                "nodeKey": node_key,
                "eeVersion": peer.get("ee_version", ""),
                "eeProtocol": peer.get("ee_protocol", ""),
                "workspaceIds": peer.get("workspace_ids", []),
                "requestHash": "sha256:" + __import__("hashlib").sha256(request).hexdigest(),
            }
        conn.sendall(json.dumps(response, sort_keys=True, separators=(",", ":")).encode("utf-8") + b"\n")
PY
    local pid=$!
    printf '%s\n' "$pid" > "$pid_path"
    FT_RESPONDER_PIDS="${FT_RESPONDER_PIDS}${FT_RESPONDER_PIDS:+ }$pid"
    FT_RESPONDER_SOCKETS="${FT_RESPONDER_SOCKETS}${FT_RESPONDER_SOCKETS:+ }$node_key=$socket_path"
    export FT_RESPONDER_PIDS
    export FT_RESPONDER_SOCKETS
    printf '%s\n' "$socket_path"
}

ft_stop_responder() {
    local node_key="${1:-}"
    if [ -n "$node_key" ] && [ -n "${FT_SCENARIO_DIR:-}" ]; then
        local node_id
        node_id="$(_ft_short_id "$node_key")"
        local pid_file
        for pid_file in "$FT_SCENARIO_DIR/responders/r-${node_id}".*.pid "$FT_SCENARIO_DIR/responders/r-${node_id}.pid"; do
            [ -f "$pid_file" ] || continue
            local pid
            pid="$(cat "$pid_file")"
            case "$pid" in
                ''|*[!0-9]*) continue ;;
                *) kill "$pid" >/dev/null 2>&1 || true ;;
            esac
        done
    fi
    if [ -n "${FT_RESPONDER_PIDS:-}" ]; then
        # shellcheck disable=SC2086
        kill $FT_RESPONDER_PIDS >/dev/null 2>&1 || true
        FT_RESPONDER_PIDS=""
        FT_RESPONDER_SOCKETS=""
        export FT_RESPONDER_PIDS
        export FT_RESPONDER_SOCKETS
    fi
}

ft_assert_no_invalid_events() {
    _ft_require_scenario
    local event_file="${1:-$FT_EVENT_FILE}"
    python3 - "$event_file" <<'PY'
import json
import sys
from pathlib import Path

path = Path(sys.argv[1])
if not path.exists():
    raise SystemExit(f"missing event log: {path}")
for line_number, line in enumerate(path.read_text(encoding="utf-8").splitlines(), start=1):
    if not line.strip():
        continue
    event = json.loads(line)
    if event.get("valid") is False:
        raise SystemExit(f"invalid fake-tailscale event line {line_number}: {event.get('detail')}")
PY
}

ft_teardown() {
    if [ -n "${FT_RESPONDER_PIDS:-}" ]; then
        ft_stop_responder || true
    fi
    PATH="$FT_OLD_PATH"
    export PATH
    if [ -n "${FT_SCENARIO_DIR:-}" ] && [ -d "$FT_SCENARIO_DIR" ]; then
        ft_emit_event "teardown" "true" "no_delete_by_policy: retained fake tailscale scenario directory" "$(_ft_hash "$FT_SCENARIO_DIR")"
    fi
}
