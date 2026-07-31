# Mesh Remote Evidence

`ee.mesh.remote_evidence.v1` records fetchable references to remote evidence,
artifacts, support bundles, memory bodies, and CASS session spans without
eagerly copying those bodies into local truth. The source of truth remains local
FrankenSQLite; remote evidence rows are provenance and fetchability metadata.

`ee.mesh.remote_evidence_materialization.v1` is the planning result for a lazy
fetch attempt. It separates four decisions that must stay distinct:

- `evidence_ref_indexed`: the reference is locally searchable as provenance, but
  no body has been copied.
- `evidence_fetch_allowed`: policy and consent allow a lazy fetch. If no body is
  supplied, the plan remains fetchable with `body_persist_allowed=false`.
- `evidence_fetch_denied`: policy, redaction, size limits, or missing consent
  require a redacted placeholder instead of a body copy.
- `evidence_hash_verified`: a fetched body matched its expected content hash,
  emits reason `content_hash_verified`, and may be persisted as derived peer
  cache material.
- `evidence_body_quarantined`: fetched bytes failed an actual-size,
  declared-size, or content-hash check and may not be published.

The default rendering posture is metadata-only. Search and context may cite the
remote reference and its provenance, but body text is not shown until a separate
policy-gated fetch succeeds. Denied evidence must render as a redacted
placeholder. A fetched body whose hash does not match is quarantined with
`content_hash_mismatch`, `mesh_fetched_body_hash_mismatch`, and
`body_persist_allowed=false`.

`sizeBytes` is untrusted peer metadata. It may reject an obviously oversized
reference before a fetch, but it can never authorize one. Fetch adapters must
stream through `copy_remote_evidence_body_bounded`; the function writes no more
than `max_bytes`, consumes at most one overflow-probe byte beyond the cap, and
returns `mesh_remote_evidence_body_size_exceeds_policy` before any oversized
body can be published. The input reader must be bounded to one protocol body so
the overflow probe can never consume the next frame. Its destination is private
staging material and must be discarded after any error.

The materialization planner checks the actual body length again before hashing.
An actual body over the cap is quarantined with
`mesh_remote_evidence_body_size_exceeds_policy`. A present declaration that
does not exactly match a successfully bounded body is quarantined with
`mesh_remote_evidence_declared_size_mismatch`. Missing declarations do not
disable the actual-byte cap. Plans expose `declaredSizeBytes`,
`actualSizeBytes`, and `degradedCodes` so the decision is explainable without
including body bytes.

Session references use the `cass-session://<session-id>#L<start>-<end>` URI
shape. Artifact and evidence refs use stable IDs such as `artifact://bundle_001`
or `evidence://span_001`; local paths, `localhost` URLs, path traversal, and
query strings are rejected before indexing.

The focused proof surface is `tests/mesh_remote_evidence.rs` plus
`scripts/e2e_mesh_remote_evidence.sh`. The script is no-network by design and
emits `ee.test_event.v1` scenario rows so the SRR6 verification matrix can track
the materialization contract without requiring a real peer.
