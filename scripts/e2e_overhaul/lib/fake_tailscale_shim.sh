#!/usr/bin/env bash
# Deterministic `tailscale` shim for bd-36bbk.1.10.

set -euo pipefail

scenario_dir="${FAKE_TAILSCALE_SCENARIO_DIR:-${FT_SCENARIO_DIR:-}}"
if [ -z "$scenario_dir" ]; then
    echo "fake tailscale shim: FAKE_TAILSCALE_SCENARIO_DIR is unset" >&2
    exit 2
fi

status_json="$scenario_dir/tailscale_status.json"
up_log="$scenario_dir/tailscale_up.log"
subcommand="${1:-}"
shift || true

case "$subcommand" in
    status)
        json=false
        self_only=false
        peers_only=false
        while [ "$#" -gt 0 ]; do
            case "$1" in
                --json) json=true; shift ;;
                --self=true) self_only=true; shift ;;
                --peers=true) peers_only=true; shift ;;
                *) echo "fake tailscale shim: unsupported status arg: $1" >&2; exit 1 ;;
            esac
        done
        if [ "$json" != true ]; then
            echo "fake tailscale shim: only status --json is supported" >&2
            exit 1
        fi
        if [ "$self_only" = true ] || [ "$peers_only" = true ]; then
            python3 - "$status_json" "$self_only" "$peers_only" <<'PY'
import json
import sys
from pathlib import Path

payload = json.loads(Path(sys.argv[1]).read_text(encoding="utf-8"))
self_only = sys.argv[2] == "true"
peers_only = sys.argv[3] == "true"
out = {"Version": payload.get("Version"), "BackendState": payload.get("BackendState")}
if self_only:
    out["Self"] = payload.get("Self")
if peers_only:
    out["Peer"] = payload.get("Peer", {})
print(json.dumps(out, sort_keys=True, separators=(",", ":")))
PY
        else
            cat "$status_json"
        fi
        ;;
    up)
        {
            printf 'tailscale up'
            for arg in "$@"; do printf ' %s' "$arg"; done
            printf '\n'
        } >> "$up_log"
        ;;
    *)
        echo "fake tailscale shim: unsupported subcommand: ${subcommand:-<missing>}" >&2
        exit 1
        ;;
esac
