#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT"

emit() {
  local status="$1"
  local name="$2"
  local detail="${3:-}"
  printf '{"schema":"ee.test_event.v1","test":"hello_responder_lifecycle","status":"%s","name":"%s","detail":"%s"}\n' \
    "$status" "$name" "$detail"
}

require_grep() {
  local pattern="$1"
  local path="$2"
  if ! grep -Eq "$pattern" "$path"; then
    emit fail "missing_pattern" "$path:$pattern"
    exit 1
  fi
}

require_grep 'HELLO_RESPONDER_STATUS_SCHEMA_V1' src/mesh/hello_responder.rs
require_grep 'DEFAULT_HELLO_RESPONDER_RATE_LIMIT_PER_PEER: u32 = 16' src/mesh/hello_responder.rs
require_grep 'HELLO_RESPONDER_STARTED_EVENT.*mesh\.hello_responder_started' src/mesh/hello_responder.rs
require_grep 'HelloResponder\(MeshHelloResponderArgs\)' src/cli/mesh.rs
require_grep 'EE_MESH_HELLO_PORT' src/config/env_registry.rs
require_grep 'EE_MESH_HELLO_RESPONDER_DISABLED' docs/env_vars.md
require_grep 'hello_responder_not_running' docs/degraded_code_taxonomy.md

for code in \
  hello_responder_not_running \
  hello_responder_port_in_use \
  hello_responder_no_tailscale_ip \
  hello_responder_crash_loop \
  hello_responder_rate_limited_storm
do
  require_grep "\"code\": \"$code\"" "tests/fixtures/failure_modes/$code.json"
done

if [[ -n "${EE_BIN:-}" && -x "${EE_BIN:-}" ]]; then
  tmpdir="$(mktemp -d "${TMPDIR:-/tmp}/ee-hello-responder.XXXXXX")"
  "$EE_BIN" init --workspace "$tmpdir" --json >/dev/null
  output="$(EE_MESH_ENABLED=1 "$EE_BIN" mesh hello-responder status --workspace "$tmpdir" --json)"
  printf '%s\n' "$output" | grep -q '"schema":"ee.response.v2"'
  printf '%s\n' "$output" | grep -q '"schema":"ee.mesh.hello_responder.status.v1"'
  printf '%s\n' "$output" | grep -q '"hello_responder_not_running"'
  emit pass "cli_probe" "EE_BIN"
else
  emit skip "cli_probe" "set EE_BIN to run the compiled ee binary"
fi

emit pass "static_contracts" "hello responder lifecycle files present"
