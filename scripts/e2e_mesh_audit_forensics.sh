#!/usr/bin/env bash
set -euo pipefail

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
  printf '{"schema":"ee.test_event.v1","surface":"%s","phase":"assert","scenario":"%s","stage":"scheduled"}\n' "$surface" "$scenario"
done

for required in \
  peer_enrollment \
  preview_consent \
  denied_body_fetch \
  support_bundle_projection \
  mesh_audit_ledger_missing \
  mesh_audit_ledger_corrupt
do
  if ! grep -Fq "$required" docs/mesh/audit_forensics.md tests/mesh_audit_forensics.rs; then
    printf 'mesh audit forensics coverage missing required term: %s\n' "$required" >&2
    exit 1
  fi
done

rch_bin="${RCH_BIN:-rch}"
if ! command -v "$rch_bin" >/dev/null 2>&1; then
  printf 'RCH binary not found; refusing to run mesh audit forensics cargo test locally\n' >&2
  exit 2
fi

RCH_REQUIRE_REMOTE=1 "$rch_bin" exec -- cargo test --test mesh_audit_forensics -- --nocapture
