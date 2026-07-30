# Indexing

`ee` treats Frankensearch indexes as derived assets. FrankenSQLite remains the
source of truth, and `ee index rebuild --workspace . --json` can reconstruct the
search index when generation, file, or tier integrity checks fail.

## Incremental Intake

The production write path uses incremental intake for single-document memory
writes when an active index already exists. The path updates the persisted
Frankensearch tiers directly: vector rows are appended or soft-deleted through
the vector tier, and lexical rows are upserted or deleted through Tantivy. A full
rebuild remains the safe fallback for first build, generation skew, missing
index files, corpus-revision mismatch, unavailable tiers, forced reindex, or
deltas that exceed the bounded incremental threshold.

`ee.index_intake.v1` is the redaction-safe telemetry contract for this behavior.
It records modes and counts only: no memory content, query text, or provenance
body is emitted.

## Correctness Contract

Incremental intake is correct only when the resulting search index is equivalent
to a full rebuild of the same final document set. The load-bearing proof is in
`src/core/index.rs`: randomized add/update/delete sequences are applied through
the incremental path and then compared against a full rebuild using deterministic
hash embeddings and stable search-result snapshots.

The equivalence requirement covers:

- same result document IDs,
- same ranking,
- same rounded scores,
- tombstone and update handling,
- fallback-to-full reason stability.

Each active `meta.json` uses `ee.index_metadata.v2` and records a deterministic
`corpusRevision`, exact memory/session/artifact/rule/evidence counts, and
per-tier counts. Missing legacy revisions fail closed as stale. Full rebuild,
re-embed, incremental intake, and interrupted-publish recovery verify those
counts before publishing a current generation; a per-document build failure can
never be published as a complete corpus.

## E2E And Perf Proof

`scripts/e2e_incremental_index.sh` exercises the real CLI path:

```bash
EE_BINARY=/path/to/ee EE_E2E_TMPDIR=/private/tmp scripts/e2e_incremental_index.sh
```

The script requires `EE_BINARY` to point at a prebuilt executable. It does not
build the binary itself. It writes a growing corpus through `ee remember`, records
per-write `ee.test_event.v1` `bench_iteration` events, compares search result
ordering before and after `ee index rebuild`, and emits a normalized
`ee.perf.artifact_summary.v1` summary. The committed fixture is
`tests/fixtures/golden/perf_artifact/incremental_index_intake.json`.

This proof is intended for RCH/orchestrated verification lanes. Local interactive
agent sessions on the Mac swarm lane should not run local Rust compilation for
this proof.
