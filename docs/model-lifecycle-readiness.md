# Model Lifecycle Readiness

`ee.model_lifecycle.v1` is the read-only contract agents use to decide whether
semantic retrieval is ready, degraded, or honestly limited to lexical fallback.
It is reported inside `ee model status --json` as `data.modelLifecycle` and is
also reusable by search and recall code paths that already hold a workspace
database connection.

Default builds are expected to report semantic readiness through the
Frankensearch Model2Vec fast tier once the pinned `potion-multilingual-128M`
asset is cached or the first-use download is pending/complete. Deterministic
hash embedding remains the fallback posture for offline, blocked, corrupt, or
fault-injected model-loading paths; it is no longer the normal happy path.

## Offline Local Readiness

A workspace is semantically ready only when the registry, model asset, and
derived index agree:

- the selected registry row is an available embedding model;
- any local `sourceUri` resolves to a regular file and its `contentHash`
  matches the observed asset hash;
- embedding metadata records the same dimension, distance metric, and vector
  dtype as the semantic index metadata;
- the index health check is ready for the current workspace generation.

The report never emits raw absolute model or workspace paths. Local model
assets are rendered workspace-relative when they are inside the workspace and
as `hashed:<12-hex>` identifiers otherwise. The `redactionStatus` is pinned to
`paths_workspace_relative_or_hashed_no_content`.

## Agent Rules

- Treat `semanticReadiness.state == "available"` and
  `semanticReadiness.mode == "semantic"` as the only semantic-ready posture.
- Treat `lexical_fallback` as usable retrieval, not semantic readiness; for a
  default install it means the bundled local model path needs attention.
- Run `ee index reembed --workspace .` when the state is
  `dimension_mismatch` or `stale_index_model`.
- Run `ee model status --json` before weakening search or recall behavior; the
  lifecycle report should name the model/index evidence that caused the
  degradation.

## Golden Fixture

`tests/fixtures/golden/model_lifecycle/offline_local_readiness.json.golden`
pins a no-mock smoke fixture for the ready path. The test builds a real temp
workspace, database, model registry row, local model asset, and index metadata,
then canonicalizes only the volatile report timestamp and workspace
fingerprint before comparing JSON. Hashes, workspace-relative asset paths,
index model metadata, and degraded arrays remain real fixture evidence.
