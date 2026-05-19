#!/usr/bin/env bash
set -euo pipefail

scenarios=(
  metadata_only_embedding_denied
  metadata_only_lexical_metadata_reused
  two_node_incompatible_version_lexical_fallback
  two_node_incompatible_model_recomputed
  content_hash_mismatch_recomputed
)

for scenario in "${scenarios[@]}"; do
  printf '{"schema":"ee.test_event.v1","surface":"mesh_search_surrogate_audit","scenario":"%s","stage":"scheduled"}\n' "$scenario"
done

rch_bin="${RCH_BIN:-rch}"
if ! command -v "$rch_bin" >/dev/null 2>&1; then
  printf 'RCH binary not found; refusing to run mesh surrogate audit cargo test locally\n' >&2
  exit 2
fi

RCH_REQUIRE_REMOTE=1 "$rch_bin" exec -- cargo test --test mesh_surrogate_audit -- --nocapture
