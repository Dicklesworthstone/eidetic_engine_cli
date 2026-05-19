# Mesh Search Surrogates

SRR6.45 treats embeddings, summaries, minhashes, lexical metadata, and query
fingerprints as separate mesh surrogate lanes. All lanes are default-deny until
peer policy explicitly allows export for the matching surrogate type.

## Compatibility

Remote surrogates carry:

- `surrogateType`
- `modelFingerprint.modelId`
- `modelFingerprint.modelVersion`
- `modelFingerprint.featureFlags`
- `contentHash`
- optional `validUntil`

A local node reuses a remote surrogate only when policy allows export, the model
fingerprint exactly matches the local model fingerprint, the source content hash
matches, and the surrogate has not expired. Otherwise it recomputes from a
locally fetched body when available, or falls back to lexical metadata.

## Privacy

Metadata-only policy permits `lexical_metadata` reuse but denies `embedding`,
`summary`, `minhash`, and `query_fingerprint` export. Denied or incompatible
surrogates emit structured degraded codes rather than raw memory body text.

## Structured Evidence

The audit surface uses these stable codes:

- `surrogate_denied`
- `surrogate_incompatible`
- `surrogate_recomputed`
- `lexical_fallback_used`

The RCH-routed e2e proof is `scripts/e2e_mesh_surrogate_audit.sh`; it emits
`ee.test_event.v1` lines for the metadata-only denial, lexical metadata reuse,
incompatible-version fallback, incompatible-model recomputation, and stale
content-hash recomputation scenarios.
