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
restores only a compatible prior active generation: a retained directory, or
on Unix the former live inode stranded at its attested staging name after an
atomic exchange. A structurally complete build is not evidence of publication,
so ordinary staging and rejected generations are never promoted by recovery.
Their files remain available for inspection and explicit vacuum. Cooperative
cancellation is checked before
the tail, so it produces no partial active index, leaves no running job or
advisory lock behind, and preserves the exact caller reason for the CLI's typed
`cancelled` response and exit code 130.

The filesystem rename and database transition are not a crash-atomic
cross-store transaction. Abrupt process termination can leave a fully validated
staged or active generation alongside an orphaned `running` job row. Publication
flushes tier files and the generation and parent directories, with rollback
covering durability-barrier failures. On Linux, Android, and Apple platforms,
atomic directory exchange keeps the active pathname present while replacing
an existing generation; Unix readers hold an OS lease across their generation
reads, and publication holds the exclusive lease through commit and rollback.
These mechanisms do not make the filesystem and database job transition one
transaction. FrankenSQLite remains the source of truth, and generation health
marks older derived indexes stale. A durable database pointer and full orphan-job
reconciliation remain separate parts of the hard-crash protocol.

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

## Reads Before Deferred Index Publication

When a usable published generation is behind the caller's source snapshot,
read-only retrieval can search the complete current corpus in an ephemeral
Frankensearch lexical index. It uses the same memory, session, artifact, native
rule and admitted-evidence projections as publication, then applies the ordinary
live revision, seal, validity, rule, workspace and relevance checks. New and
revised content can therefore be retrieved before its queued job is published.
The request does not claim or finish jobs, backfill anchors, change the database,
write an index, or download a model.

This replacement is bounded to 4,096 source rows and 16 MiB of source bodies and
projected document bytes. It collects the complete bounded result pool before
live admission and the requested result limit; it never substitutes a truncated
corpus. Cancellation remains attached to the caller's request. The response
retains the persisted index's stale status and reports
`search_live_snapshot_lexical`, lexical-only retrieval, and no semantic or
reranking execution. Strict source modes that require semantic retrieval,
explicit reference times, and tombstone inspection keep their existing indexed
view. If the complete replacement exceeds a bound or cannot
run, `search_live_snapshot_unavailable` explicitly warns that newly committed
content may be absent. Background publication or an explicit index rebuild is
still needed to restore full indexed retrieval.

## Session Generation Invalidation

`V097_SESSION_INDEX_GENERATIONS` makes the session source family obey the same
workspace-generation fence as other first-class search documents. Committed
session inserts and deletes advance the owning workspace; material updates
advance the new workspace and also the old workspace when ownership moves. A
null-safe predicate suppresses exact-row no-ops, and transaction rollback rolls
back both the session mutation and its generation advance. Migration advances
each workspace that already contains sessions once, monotonically invalidating
pre-V097 index metadata without treating generation as a row count.

A CASS intake transaction may insert one session, its positively admitted
evidence spans, and the stable `single_document` session job atomically. Both
the ordinary single-job processor and the limited/coalesced processor consume a
complete writer-fenced source snapshot, so draining that session job publishes
the session and all admitted evidence committed beside it without an operator
running a manual rebuild. Evidence projection retains only screened content and
canonical `cass-session://...#L...` provenance; raw paths and upstream span IDs
do not enter the index.

Session import and evidence-attachment now share the same writer-fenced
snapshot publisher: draining the durable job that covers the mutation
republishes the complete corpus, including refreshed `memory_id` metadata on
an attached evidence document, without an operator running a manual rebuild.
Public no-mock coverage lives in
`ordinary_session_job_indexes_atomic_admitted_evidence_without_manual_rebuild`,
`limited_coalesced_session_job_indexes_atomic_admitted_evidence_without_manual_rebuild`,
`ordinary_memory_job_refreshes_attached_evidence_without_manual_rebuild`, and
`limited_coalesced_memory_job_refreshes_attached_evidence_without_manual_rebuild`.
The CASS CLI path is `scripts/e2e_capture.sh`. Direct `EvidenceSpan` packing
is owned by `bd-16imy`; remaining `bd-3k1mg` follow-up is the
failure-retry-crash matrix for the attachment job itself.

## Refreshing Previously Imported Sessions

A known CASS session is not assumed to be finished. With evidence capture
selected, `ee import cass` reads the complete bounded transcript on subsequent
imports and extends the same stored `SessionId`. This also backfills a session
that was first imported without spans or by an older first-window importer.

Refresh preserves every retained CASS evidence ID, memory attachment, redaction
record, and search/pack admission decision. New spans pass through the normal
screening and insertion path. Missing or changed retained excerpts, conflicting
references, and scope mismatches refuse the entire session refresh rather than
rewriting historical pack provenance or silently choosing an upstream version.
This checks retained screened excerpts, not the omitted tails of truncated raw
lines; it is not an authenticated upstream snapshot protocol.

Session metadata, additional spans, redaction/refresh audits, and the new
revision-specific index job commit together under the import writer fence.
The existing complete-corpus publisher consumes that job, so newly captured
conversation evidence does not require a manual rebuild. Repeating the same
snapshot writes no new evidence or refresh audit and returns any unfinished
publication job for that revision. A completed original-import job cannot hide
a later refresh whose publication failed.

The existing response schema and status vocabulary are retained:
`sessionsImported` includes sessions whose stored snapshot materially changed,
`sessionsSkipped` counts unchanged sessions, and `spansImported` counts only
newly captured spans. Metadata-only refresh can therefore report one imported
session and zero new spans. Imports without evidence capture retain their
existing skip/reconciliation behavior. Dry-run still performs no view capture
or database mutation.

Database regression coverage lives in `src/cass/refresh_tests.rs`. It covers
growth, backfill, unchanged/reordered retries, failed publication reconciliation,
metadata-only changes, history rewrites/truncation, denied evidence, redaction,
scope isolation, and rollback when index-job insertion fails.

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
