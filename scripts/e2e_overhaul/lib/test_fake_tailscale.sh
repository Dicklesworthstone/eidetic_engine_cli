#!/usr/bin/env bash
# Self-test for the bd-36bbk.1.10 fake Tailscale harness.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/e2e_overhaul/lib/fake_tailscale.sh
source "$SCRIPT_DIR/fake_tailscale.sh"

WORK_DIR="$(mktemp -d /tmp/ee-fake-tailscale.XXXXXX)"
export EE_TEST_EVENT_DIR="$WORK_DIR/events"
export FT_EVENT_KIND="fake_tailscale_harness_self_test"
export FT_WORKSPACE_ID="workspace-alpha"
export FT_REQUEST_ID="fake-tailscale-self-test"
export FT_BEAD_ID="bd-36bbk.1.10"
export FT_SURFACE="fake_tailscale_e2e_harness"

fail() {
    ft_emit_event "assert" "false" "$1" "" || true
    echo "fake_tailscale self-test: $1" >&2
    exit 1
}

json_get() {
    local path="${1:?path required}"
    local expr="${2:?expr required}"
    python3 - "$path" "$expr" <<'PY'
import json
import sys

value = json.load(open(sys.argv[1], encoding="utf-8"))
for part in sys.argv[2].split("."):
    if part:
        value = value[part]
print(value)
PY
}

trap 'ft_teardown' EXIT

ft_init "$WORK_DIR/scenario" "healthy_3_peers_2_eligible"
ft_set_self "nodekey:selfalpha" "100.64.0.10" "tailnet-alpha" "ee-local" --platform=linux --authenticated=true
ft_add_peer "nodekey:peer0001" "100.64.0.11" "peer-one" --ee_version=0.2.0 --ee_protocol=1.0 --workspace_ids=w-alpha --tag=ee-mesh --respond=true
ft_add_peer "nodekey:peer0002" "100.64.0.12" "peer-two" --ee_version=0.2.0 --ee_protocol=1.0 --workspace_ids=w-beta --tag=ee-mesh --respond=false
ft_add_peer "nodekey:peer0003" "100.64.0.13" "peer-three" --workspace_ids= --respond=true
ft_shim_path > "$WORK_DIR/shim-dir.txt"
shim_dir="$(cat "$WORK_DIR/shim-dir.txt")"
[ -x "$shim_dir/tailscale" ] || fail "shim path did not expose executable tailscale"

version_path="$WORK_DIR/version.txt"
tailscale --version > "$version_path"
grep -q '^1\.66\.0$' "$version_path" || fail "version shim missing semver line"
grep -q 'tailscale commit:' "$version_path" || fail "version shim missing tailscale commit"
grep -q 'go version:' "$version_path" || fail "version shim missing go version"
ft_emit_event "probe" "true" "tailscale version shim returned authentic-looking output" "$(_ft_hash "$(cat "$version_path")")"

status_path="$WORK_DIR/status.json"
tailscale status --json > "$status_path"
[ "$(json_get "$status_path" BackendState)" = "Running" ] || fail "status BackendState was not Running"
[ "$(python3 - "$status_path" <<'PY'
import json, sys
print(len(json.load(open(sys.argv[1], encoding="utf-8"))["Peer"]))
PY
)" = "3" ] || fail "status did not include three peers"
ft_emit_event "probe" "true" "tailscale status shim returned deterministic peer set" "$(_ft_hash "$(cat "$status_path")")"

self_path="$WORK_DIR/status-self.json"
tailscale status --json --self=true --peers=true > "$self_path"
grep -q '"Self"' "$self_path" || fail "narrowed status missing Self"
grep -q '"Peer"' "$self_path" || fail "narrowed status missing Peer"

prefs_path="$WORK_DIR/prefs.json"
tailscale debug localapi /localapi/v0/prefs > "$prefs_path"
[ "$(json_get "$prefs_path" ShieldsUp)" = "False" ] || fail "prefs did not default ShieldsUp false"
ft_set_shields_up true
tailscale debug localapi /localapi/v0/prefs > "$prefs_path"
[ "$(json_get "$prefs_path" ShieldsUp)" = "True" ] || fail "prefs did not reflect ShieldsUp true"
tailscale status --json --self=true > "$self_path"
[ "$(json_get "$self_path" Self.ShieldsUp)" = "True" ] || fail "status self did not reflect ShieldsUp true"
ft_set_shields_up false
ft_emit_event "prefs" "true" "tailscale debug localapi prefs reflected shields-up state" "$(_ft_hash "$(cat "$prefs_path")")"

tailscale up --accept-dns=false
[ -s "$WORK_DIR/scenario/tailscale_up.log" ] || fail "tailscale up shim did not log"
ft_emit_event "auto_enroll" "true" "tailscale up shim logged non-network call" "$(_ft_hash "$(cat "$WORK_DIR/scenario/tailscale_up.log")")"

ft_swap_tailnet "tailnet-beta" "ee-local-renamed"
tailscale status --json > "$status_path"
[ "$(json_get "$status_path" Self.Tailnet)" = "tailnet-beta" ] || fail "tailnet swap was not reflected"
ft_emit_event "status" "true" "tailnet swap reflected in status fixture" "$(_ft_hash "$(cat "$status_path")")"

ft_corrupt_status_json wrong_schema
if tailscale status --json > "$WORK_DIR/corrupt.json"; then
    grep -q '"schema":"wrong"' "$WORK_DIR/corrupt.json" || fail "wrong_schema corruption not visible"
else
    fail "wrong_schema corruption should still be readable JSON"
fi
ft_emit_event "probe" "true" "wrong_schema corruption injected" "$(_ft_hash "$(cat "$WORK_DIR/corrupt.json")")"
_ft_rewrite_status

ft_corrupt_status_json invalid_utf8
tailscale status --json > "$WORK_DIR/corrupt-invalid-utf8.json"
if python3 - "$WORK_DIR/corrupt-invalid-utf8.json" 2>/dev/null <<'PY'
import sys
from pathlib import Path
Path(sys.argv[1]).read_text(encoding="utf-8")
PY
then
    fail "invalid_utf8 corruption decoded as utf-8"
fi
ft_emit_event "probe" "true" "invalid_utf8 corruption injected" "$(_ft_hash "$WORK_DIR/corrupt-invalid-utf8.json")"
_ft_rewrite_status

socket_path="$(ft_run_responder "nodekey:peer0001")"
for _ in 1 2 3 4 5; do
    [ -S "$socket_path" ] && break
    sleep 0.1
done
[ -S "$socket_path" ] || fail "hello responder socket was not created"
hello_path="$WORK_DIR/hello.json"
python3 - "$socket_path" > "$hello_path" <<'PY'
import socket
import sys

sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
sock.connect(sys.argv[1])
sock.sendall(b'{"schema":"ee.mesh.hello.request.v1","workspaceId":"w-alpha"}')
print(sock.recv(65536).decode("utf-8").strip())
PY
[ "$(json_get "$hello_path" schema)" = "ee.mesh.hello.v1" ] || fail "hello response schema mismatch"
[ "$(json_get "$hello_path" accepted)" = "True" ] || fail "hello responder did not accept configured peer"
ft_emit_event "hello" "true" "fake responder returned hello response" "$(_ft_hash "$(cat "$hello_path")")"
ft_stop_responder "nodekey:peer0001"

bad_events="$WORK_DIR/bad-events.jsonl"
old_event_file="$FT_EVENT_FILE"
FT_EVENT_FILE="$bad_events"
ft_emit_event "rollback" "false" "intentional invalid event for assertion coverage" ""
FT_EVENT_FILE="$old_event_file"
if ft_assert_no_invalid_events "$bad_events" 2> "$WORK_DIR/bad-events.err"; then
    fail "ft_assert_no_invalid_events accepted an invalid event"
fi
ft_emit_event "rollback" "true" "invalid-event assertion rejected bad event log" "$(_ft_hash "$bad_events")"

ft_assert_no_invalid_events
ft_emit_event "summary" "true" "fake tailscale harness self-test passed" "$(_ft_hash "$WORK_DIR")"
printf '%s\n' "$FT_EVENT_FILE"
