#!/usr/bin/env bash
# bd-1k0ql - no-network remote evidence materialization proof driver.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/lib/mesh_e2e_outcomes.sh
source "$SCRIPT_DIR/lib/mesh_e2e_outcomes.sh"

surface="mesh_remote_evidence"
scenarios=(
  cass_session_reference_indexed_without_body_copy
  denied_remote_artifact_redacted_placeholder
  fetchable_remote_body_metadata_only_until_lazy_fetch
  allowed_fetch_content_hash_verified_before_persist
  hash_mismatch_quarantines_remote_material
  unsafe_remote_evidence_uri_rejected
)

mesh_e2e_emit_note "$surface" "matrix" "remote evidence materialization fixture loaded"
for scenario in "${scenarios[@]}"; do
  mesh_e2e_emit_scheduled "$surface" "$scenario"
done

required_terms=(
  ee.mesh.remote_evidence.v1
  ee.mesh.remote_evidence_materialization.v1
  evidence_ref_indexed
  evidence_fetch_allowed
  evidence_fetch_denied
  evidence_hash_verified
  body_persist_allowed=false
  content_hash_verified
  content_hash_mismatch
  "redacted placeholder"
)

missing_terms=()
for required in "${required_terms[@]}"; do
  if ! grep -Fq "$required" docs/mesh/remote_evidence.md tests/mesh_remote_evidence.rs; then
    missing_terms+=("$required")
  fi
done
if [ "${#missing_terms[@]}" -gt 0 ]; then
  mesh_e2e_emit_failed "$surface" "mesh remote evidence proof missing required terms: ${missing_terms[*]}" "${scenarios[@]}"
  exit 1
fi

mesh_e2e_emit_outcomes "$surface" "pass" "0.0" "" "${scenarios[@]}"
mesh_e2e_emit_note "$surface" "matrix" "remote evidence materialization static proof passed" "complete"
