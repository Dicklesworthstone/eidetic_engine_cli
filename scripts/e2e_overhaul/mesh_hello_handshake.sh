#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT"

emit() {
  local status="$1"
  local name="$2"
  local detail="${3:-}"
  printf '{"schema":"ee.test_event.v1","test":"mesh_hello_handshake","status":"%s","name":"%s","detail":"%s"}\n' \
    "$status" "$name" "$detail"
}

fail() {
  emit fail "$1" "${2:-}"
  exit 1
}

require_file() {
  local path="$1"
  [ -f "$path" ] || fail "missing_file" "$path"
}

require_grep() {
  local pattern="$1"
  local path="$2"
  if ! grep -Eq "$pattern" "$path"; then
    fail "missing_pattern" "$path:$pattern"
  fi
}

require_file src/mesh/hello.rs
require_file docs/schemas/ee.mesh.hello.v1.json
require_file docs/schemas/ee.mesh.hello.response.v1.json
require_file docs/schemas/ee.mesh.hello.error.v1.json

require_grep 'HELLO_PAYLOAD_BUDGET_BYTES: usize = 4096' src/mesh/hello.rs
require_grep 'pub const HELLO_REQUEST_SCHEMA_V1: &str = "ee\.mesh\.hello\.v1"' src/mesh/hello.rs
require_grep 'pub const HELLO_RESPONSE_SCHEMA_V1: &str = "ee\.mesh\.hello\.response\.v1"' src/mesh/hello.rs
require_grep 'pub const HELLO_ERROR_SCHEMA_V1: &str = "ee\.mesh\.hello\.error\.v1"' src/mesh/hello.rs
require_grep 'ResponderShieldsUp' src/mesh/hello.rs
require_grep 'ResponderUnauthenticatedTailscale' src/mesh/hello.rs
require_grep 'assert_no_responder_metadata_leak' src/mesh/hello.rs
require_grep 'handler_returns_responder_mesh_disabled_when_env_false' src/mesh/hello.rs
require_grep 'handler_returns_responder_shields_up_when_set' src/mesh/hello.rs
require_grep 'handler_returns_responder_unauthenticated_tailscale_when_probe_says_so' src/mesh/hello.rs
require_grep 'decline_response_omits_responder_metadata' src/mesh/hello.rs

for schema in \
  docs/schemas/ee.mesh.hello.v1.json \
  docs/schemas/ee.mesh.hello.response.v1.json \
  docs/schemas/ee.mesh.hello.error.v1.json
do
  require_grep '"additionalProperties": false' "$schema"
done

for fixture in \
  consent_granted \
  consent_denied \
  mesh_disabled \
  shields_up_decline \
  unauth_decline \
  version_skew_minor \
  version_skew_major \
  unknown_fields \
  decline_no_metadata_leak
do
  require_file "tests/fixtures/mesh_hello/${fixture}.json"
done

python3 - <<'PY'
import json
from pathlib import Path

root = Path("tests/fixtures/mesh_hello")
forbidden = {
    "responderNodeKey",
    "responderEeVersion",
    "responderEeProtocolVersion",
    "responderWorkspaceIds",
    "responderCapabilities",
    "responderAdvertisedTags",
    "responseElapsedMicros",
}

expected_errors = {
    "consent_denied": "discovery_consent_denied",
    "mesh_disabled": "responder_mesh_disabled",
    "shields_up_decline": "responder_shields_up",
    "unauth_decline": "responder_unauthenticated_tailscale",
    "version_skew_major": "unsupported_protocol_version",
    "decline_no_metadata_leak": "discovery_consent_denied",
}

for path in sorted(root.glob("*.json")):
    payload = json.loads(path.read_text(encoding="utf-8"))
    scenario = payload["scenario"]
    request = payload["request"]
    expected = payload["expected"]
    assert request["schema"] == "ee.mesh.hello.v1", path
    assert request["requestId"], path
    assert request["requesterEeProtocolVersion"].count(".") == 1, path
    if expected["kind"] == "granted":
        response = expected["response"]
        assert response["schema"] == "ee.mesh.hello.response.v1", path
        assert response["requestId"] == request["requestId"], path
        assert response["discoveryConsent"] is True, path
        assert response["responderEeProtocolVersion"].count(".") == 1, path
    else:
        error = expected["error"]
        assert error["schema"] == "ee.mesh.hello.error.v1", path
        assert error["requestId"] == request["requestId"], path
        assert error["discoveryConsent"] is False, path
        assert not (forbidden & set(error)), path
        if scenario in expected_errors:
            assert error["code"] == expected_errors[scenario], path

minor = json.loads((root / "version_skew_minor.json").read_text(encoding="utf-8"))
assert minor["expected"]["kind"] == "granted"
assert minor["request"]["requesterEeProtocolVersion"].startswith("1.")

unknown = json.loads((root / "unknown_fields.json").read_text(encoding="utf-8"))
assert "futureRequesterHint" in unknown["request"], unknown

print("mesh hello fixtures valid")
PY

emit pass "static_contracts" "mesh hello protocol schemas and fixtures present"
