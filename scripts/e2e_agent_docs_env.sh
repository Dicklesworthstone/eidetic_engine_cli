#!/usr/bin/env bash
# bd-n7gig — real-binary E2E pin for `ee agent-docs env --json`.
#
# This is a shell-only route pin: it never builds the binary. The script uses
# the installed/preprovided ee binary and proves the env-registry docs surface
# stays machine-readable without exposing live process environment values.

set -uo pipefail

E2E_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# Avoid the shared harness's cargo-metadata fallback in RCH-only agent sessions.
EE_BIN="${EE_BIN:-ee}"
export EE_BIN

# shellcheck source=scripts/lib/e2e_harness.sh
# shellcheck disable=SC1091
source "$E2E_DIR/lib/e2e_harness.sh"

harness_init "agent_docs_env"

ee_json() { "$EE_BIN" "$@" 2>/dev/null || true; }
ee_text() { "$EE_BIN" "$@" 2>/dev/null || true; }

SECRET_SENTINEL="ee-agent-docs-env-secret-sentinel-17391"
WORKSPACE_SENTINEL="/tmp/ee-agent-docs-env-workspace-sentinel-17391"

step "agent-docs env emits a response envelope"
env_json="$(
    EE_PREFLIGHT_BYPASS_SECRET="$SECRET_SENTINEL" \
    EE_WORKSPACE="$WORKSPACE_SENTINEL" \
    ee_json agent-docs env --json
)"

assert_jq "$env_json" '.schema == "ee.response.v2" and .success == true' \
    "agent-docs env returns success envelope"
assert_jq "$env_json" '.data.command == "agent-docs" and .data.topic == "env"' \
    "agent-docs env identifies command and topic"
assert_jq "$env_json" '(.data.envVars | type) == "array" and (.data.envVars | length) > 20' \
    "agent-docs env returns a populated envVars array"

step "env registry entries keep stable names, categories, and defaults"
# shellcheck disable=SC2016
assert_jq "$env_json" '
    def by_name($name): .data.envVars[] | select(.name == $name);
    (by_name("EE_WORKSPACE").category == "paths")
    and (by_name("EE_DATABASE_PATH").category == "paths")
    and (by_name("EE_OUTPUT_FORMAT").category == "output")
    and (by_name("EE_JSON").category == "output")
    and (by_name("EE_PREFLIGHT_BYPASS_SECRET").category == "policy")
' "core env vars expose expected categories"
# shellcheck disable=SC2016
assert_jq "$env_json" '
    def by_name($name): .data.envVars[] | select(.name == $name);
    (by_name("EE_FLIGHT_RECORDER").default == "false")
' "boolean default is documented as a string"
assert_jq "$env_json" '
    all(.data.envVars[]; (.name | startswith("EE_")) and ((.description // "") | length > 0) and ((.category // "") | length > 0))
' "every env var entry has name, description, and category"

step "env docs do not expose live process environment values"
assert_jq "$env_json" '
    (tostring | contains("ee-agent-docs-env-secret-sentinel-17391") | not)
    and (tostring | contains("/tmp/ee-agent-docs-env-workspace-sentinel-17391") | not)
' "agent-docs env redacts by omission: no raw env values"

step "human env topic remains discoverable"
env_text="$(ee_text agent-docs env)"
assert_contains "$env_text" "Environment variables:" "human env topic heading"
assert_contains "$env_text" "EE_WORKSPACE" "human env topic lists EE_WORKSPACE"

harness_summary
