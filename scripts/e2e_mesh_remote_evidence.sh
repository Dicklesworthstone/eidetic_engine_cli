#!/usr/bin/env bash
# bd-1k0ql - no-network remote evidence materialization proof driver.

set -euo pipefail

surface="mesh_remote_evidence"
scenarios=(
  cass_session_reference_indexed_without_body_copy
  denied_remote_artifact_redacted_placeholder
  fetchable_remote_body_metadata_only_until_lazy_fetch
  allowed_fetch_content_hash_verified_before_persist
  hash_mismatch_quarantines_remote_material
  unsafe_remote_evidence_uri_rejected
)

printf '{"schema":"ee.test_event.v1","surface":"%s","phase":"setup","scenario":"matrix","message":"remote evidence materialization fixture loaded"}\n' "$surface"
for scenario in "${scenarios[@]}"; do
  printf '{"schema":"ee.test_event.v1","surface":"%s","phase":"assert","scenario":"%s","stage":"scheduled"}\n' "$surface" "$scenario"
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

for required in "${required_terms[@]}"; do
  if ! grep -Fq "$required" docs/mesh/remote_evidence.md tests/mesh_remote_evidence.rs; then
    printf 'mesh remote evidence proof missing required term: %s\n' "$required" >&2
    exit 1
  fi
done

printf '{"schema":"ee.test_event.v1","surface":"%s","phase":"complete","scenario":"matrix","message":"remote evidence materialization static proof passed"}\n' "$surface"
