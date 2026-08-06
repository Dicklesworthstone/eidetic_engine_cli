# Indexing

`ee` treats Frankensearch indexes as derived assets. FrankenSQLite remains the
source of truth, and `ee index rebuild --workspace . --json` can reconstruct the
search index when generation, file, or tier integrity checks fail.

## Cancellation-Safe Intake

Production intake never mutates the active Frankensearch tiers in place. A
single-document or coalesced job captures one writer-fenced database snapshot,
builds a complete generation in a sibling staging directory, validates every
tier and count, and reaches a caller-owned Asupersync cancellation checkpoint
before publication. The previous active generation remains readable throughout
embedding, lexical construction, validation, and cancellation.

Publication uses a short masked in-process tail. Renaming the complete staged
generation is the filesystem linearization point; the associated database job
transition follows in the same masked tail. An ordinary transition error or
panic triggers rollback: `ee` restores the previous active generation and moves
the unpublished generation into a rejected quarantine for inspection. Recovery
never promotes quarantined generations, while `ee index vacuum` still reports
them as reclaimable derived assets. Cooperative cancellation is checked before
the tail, so it produces no partial active index, leaves no running job or
advisory lock behind, and preserves the exact caller reason for the CLI's typed
`cancelled` response and exit code 130.

The filesystem rename and database transition are not a crash-atomic
cross-store transaction. Abrupt process termination can leave a fully validated
staged or active generation alongside an orphaned `running` job row; directory
rename semantics still prevent that process termination from exposing a
half-built active generation. Power-loss durability is not established by this
protocol because the publication directories are not fsynced. FrankenSQLite
remains the source of truth, and generation health marks older derived indexes
stale. Durable publish-intent, directory-fsync ordering, and orphan-job
reconciliation require a separate hard-crash protocol and are not claimed by
this cancellation contract.

Job types named `incremental` and `single_document` remain intake and telemetry
contracts, not permission to edit active files. They may be coalesced into one
staged full-generation build. Generation skew, missing files,
corpus-revision mismatch, unavailable tiers, forced reindex, and large deltas
remain explicit fallback reasons in `ee.index_intake.v1` telemetry.

`ee.index_intake.v1` is the redaction-safe telemetry contract for this behavior.
It records modes and counts only: no memory content, query text, or provenance
body is emitted.

## Correctness Contract

Every intake result must be equivalent to a full rebuild of the same final
document set. The load-bearing proof is in `src/core/index.rs`: randomized
add/update/delete sequences exercise the historical incremental model and are
compared against full rebuilds using deterministic hash embeddings and stable
search-result snapshots. Production additionally has deterministic LabRuntime
coverage for cancellation during construction and after validation but before
publication.

The equivalence requirement covers:

- same result document IDs,
- same ranking,
- same rounded scores,
- tombstone and update handling,
- fallback-to-full reason stability.

Each active `meta.json` uses `ee.index_metadata.v2` and records a deterministic
`corpusRevision`, exact memory/session/artifact/rule/evidence counts, and
per-tier counts. Missing legacy revisions fail closed as stale. Full rebuild,
re-embed, staged intake, and interrupted-publish recovery verify those counts
before publishing a current generation; a per-document build failure can never
be published as a complete corpus.

## E2E And Perf Proof

`scripts/e2e_incremental_index.sh` exercises the real CLI intake path (the
historical script name and artifact schema are retained):

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
