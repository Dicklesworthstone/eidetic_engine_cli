#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/lib/mesh_e2e_outcomes.sh
source "$SCRIPT_DIR/lib/mesh_e2e_outcomes.sh"

surface="mesh_audit_forensics"
scenarios=(
  peer_enrollment
  preview_consent
  policy_decision
  export
  import
  denied_body_fetch
  withdrawal
  quarantine
  revision
  support_bundle_projection
  missing_ledger_failure_mode
  corrupt_ledger_failure_mode
)

printf '{"schema":"ee.test_event.v1","surface":"%s","phase":"setup","scenario":"matrix","message":"mesh audit forensics fixture loaded"}\n' "$surface"
for scenario in "${scenarios[@]}"; do
  mesh_e2e_emit_scheduled "$surface" "$scenario"
done

missing_terms=()
for required in \
  peer_enrollment \
  preview_consent \
  denied_body_fetch \
  support_bundle_projection \
  mesh_audit_ledger_missing \
  mesh_audit_ledger_corrupt
do
  if ! grep -Fq "$required" docs/mesh/audit_forensics.md tests/mesh_audit_forensics.rs; then
    missing_terms+=("$required")
  fi
done
if [ "${#missing_terms[@]}" -gt 0 ]; then
  mesh_e2e_emit_failed "$surface" "mesh audit forensics coverage missing required terms: ${missing_terms[*]}" "${scenarios[@]}"
  exit 1
fi

rch_bin="${RCH_BIN:-rch}"
if ! command -v "$rch_bin" >/dev/null 2>&1; then
  mesh_e2e_emit_skipped "$surface" "RCH binary not found; refusing to run mesh audit forensics cargo test locally" "${scenarios[@]}"
  exit 2
fi

mesh_e2e_run_with_outcomes "$surface" "${scenarios[@]}" -- \
  env RCH_REQUIRE_REMOTE=1 "$rch_bin" exec -- cargo test --test mesh_audit_forensics -- --nocapture
