#!/usr/bin/env bash
set -euo pipefail

# SRR6.46.18 documentation-set smoke gate. This script is intentionally
# read-only: it validates that the ADR, agent onboarding guide, migration
# guide, README index, and Rust doc-consistency test are present and mutually
# discoverable. Cargo execution belongs to RCH; this script only emits the
# commands a verifier should run.

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT"

emit_event() {
  local name="$1"
  local status="$2"
  local detail="$3"
  printf '{"schema":"ee.test_event.v1","surface":"auto_enroll_documentation_set","name":"%s","status":"%s","detail":"%s"}\n' \
    "$name" "$status" "$detail"
}

require_file() {
  local path="$1"
  test -f "$path" || {
    emit_event "$path" "fail" "missing file"
    exit 1
  }
  emit_event "$path" "pass" "file exists"
}

require_text() {
  local path="$1"
  local text="$2"
  grep -Fq -- "$text" "$path" || {
    emit_event "$path" "fail" "missing text: $text"
    exit 1
  }
}

require_file "docs/adr/0038-auto-enrollment-zero-touch.md"
require_file "docs/agent-ux/auto_enrollment_onboarding.md"
require_file "docs/migration-guide.md"
require_file "tests/auto_enroll_documentation_consistency.rs"

for section in "Status:" "Date:" "## Context" "## Decision" "## Invariants" "## Rejected Alternatives"; do
  require_text "docs/adr/0038-auto-enrollment-zero-touch.md" "$section"
done
emit_event "adr_sections" "pass" "ADR 0038 required sections present"

for section in "## TL;DR" "## Required Preconditions" "### Common Degraded Codes" "## Safety Patterns" "## Common Workflows"; do
  require_text "docs/agent-ux/auto_enrollment_onboarding.md" "$section"
done
emit_event "onboarding_sections" "pass" "agent onboarding required sections present"

for env_var in \
  "EE_MESH_DISCOVERY_CACHE_TTL_SECONDS" \
  "EE_MESH_DRIFT_SOFT_STALE_AFTER" \
  "EE_MESH_DRIFT_SOFT_STALE_AFTER_SECONDS" \
  "EE_MESH_DRIFT_HARD_STALE_AFTER" \
  "EE_MESH_DRIFT_HARD_STALE_AFTER_SECONDS" \
  "EE_MESH_ENABLED" \
  "EE_MESH_MODE" \
  "EE_TAILSCALE_BINARY_OVERRIDE" \
  "EE_TAILSCALE_PROBE_SOCKET_OVERRIDE" \
  "EE_TAILSCALE_PROBE_TIMEOUT_MS" \
  "EE_TAILSCALE_DISCOVERY_MODE" \
  "EE_TAILSCALE_RESPOND_MODE"; do
  require_text "docs/migration-guide.md" "\`$env_var\`"
done
emit_event "migration_env_vars" "pass" "migration guide lists registered mesh env vars"

require_text "README.md" "docs/agent-ux/auto_enrollment_onboarding.md"
require_text "README.md" "docs/adr/0038-auto-enrollment-zero-touch.md"
emit_event "readme_index" "pass" "README indexes auto-enrollment docs"

emit_event "rch_command" "info" "rch exec -- cargo test --test auto_enroll_documentation_consistency"
