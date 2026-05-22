#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/lib/mesh_e2e_outcomes.sh
source "$SCRIPT_DIR/lib/mesh_e2e_outcomes.sh"

surface="mesh_privacy_redaction_authorization"
scenarios=(
  trusted_full_body_export
  metadata_only_body_export_denied
  metadata_only_embedding_export_denied
  metadata_result_then_body_fetch_denied
  denied_peer_import_denied_without_side_effects
  stale_peer_lookup_failure_redacted
  unknown_peer_lookup_failure_redacted
  context_pack_provenance_redacted
  support_bundle_projection_redacts_mesh_audit
)

mesh_e2e_emit_note "$surface" "matrix" "privacy fixture loaded"
for scenario in "${scenarios[@]}"; do
  mesh_e2e_emit_scheduled "$surface" "$scenario"
done

rch_bin="${RCH_BIN:-rch}"
if ! command -v "$rch_bin" >/dev/null 2>&1; then
  mesh_e2e_emit_skipped "$surface" "RCH binary not found; refusing to run mesh privacy cargo test locally" "${scenarios[@]}"
  exit 2
fi

mesh_e2e_run_with_outcomes "$surface" "${scenarios[@]}" -- \
  env RCH_REQUIRE_REMOTE=1 "$rch_bin" exec -- cargo test --test mesh_privacy_redaction_authorization -- --nocapture
