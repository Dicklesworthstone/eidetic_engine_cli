# ADR 0078: Incremental Index Intake

Status: accepted
Date: 2026-06-16
Bead: bd-d67os.5, bd-d67os.8

## Context

Today every single durable write triggers a **full workspace index rebuild and
atomic swap**. `collect_workspace_indexable_documents()` loads ALL indexable
documents (`src/core/index.rs:1319`), `build_index_sync` rebuilds the entire
index over all of them (`:1357`), and `publish_staged_index` swaps the freshly
built index in (`:1354-1360`). The hot path is honestly labelled
`single_document_as_full_rebuild` / `incremental_as_full_rebuild`
(`src/core/index.rs:1483-1490`). `SearchIndexJobType::Incremental` already exists
in the enum but is **never instantiated** — there is no delta/append path.

The batch O(n²) amplification from many writes is already mitigated by coalesced
deferral (`process_pending_index_jobs_coalesced`, `src/core/memory.rs:5529`), but
a *single* write still pays a full rebuild. As a workspace's corpus grows, that
rebuild cost dominates write latency, and under a swarm it multiplies across
concurrent writers. This is Track C of the hot-path concurrency epic
(`bd-d67os`); this leaf (`bd-d67os.5`) is the ADR + capability survey + schema
that gates the core/integration work in `bd-d67os.6`/`.7`/`.8`.

The decisive question for the whole track: **does the underlying frankensearch
index support incremental intake (append/update/delete a single document after
build), or only a full rebuild from the complete document set?** The chosen
design depends entirely on the answer.

## Capability Survey — Frankensearch (read-only source audit, 2026-06-16)

The finding is **frankensearch supports incremental intake on both tiers**; ee's
full-rebuild-per-write is a property of the *ee* wiring, not a frankensearch
limitation.

| Frankensearch surface | Incremental capability | Evidence |
| --- | --- | --- |
| **Vector tier** — `VectorIndex` (FSVI, `frankensearch-index`) | Append + soft-delete via a Write-Ahead Log sidecar; entries are searchable before compaction. | `append(doc_id, vector)` (`crates/frankensearch-index/src/lib.rs:651`), `append_batch` (`:665`), `soft_delete` (`:468`), `soft_delete_batch` (`:479`), `compact` WAL→main (`:796`), `vacuum` (`:605`); WAL dedup by doc_id (`:266`); test `wal_entries_are_searchable_before_compaction` (`tests/fsvi_roundtrip.rs:506`). |
| **Lexical tier** — `TantivyIndex` (BM25, `frankensearch-lexical`) | Full mutation: add / upsert / delete streamed through Tantivy's `IndexWriter` with no rebuild. | `index_document` upsert = `delete_term` + `add_document` (`crates/frankensearch-lexical/src/lib.rs:816`), `index_documents` (`:847`), `delete_document` (`:564`), `commit` (`:881`); tests `upsert_replaces_existing_document` (`:1035`), `delete_document_removes_from_index` (`:1203`). |
| **Composite** — `TwoTierIndex` / `TwoTierIndexBuilder` | Builder is terminal (`.finish()` consumes); the composite struct is read-only after build. BUT the persisted tier files can be reopened and mutated directly. | `TwoTierIndexBuilder::finish()` terminal (`crates/frankensearch-index/src/two_tier.rs:732`); incremental path = `VectorIndex::open()` + `append`/`soft_delete` and `TantivyIndex::open()` + `index_document`/`delete_document`, then `TwoTierIndex::open()`. |

**Classification:** Vector = `SUPPORTS_INCREMENTAL_ADD` (+ soft-delete, WAL);
Lexical = `SUPPORTS_FULL_MUTATION` (add/upsert/delete); TwoTier composite =
`FULL_REBUILD_ONLY` at the builder API, but its underlying tiers are incremental
when reopened from their persisted files.

## Decision

Because frankensearch already supports incremental intake on both tiers, Track C
does not invent a custom delta-segment + merge scheme inside ee. The upstream
capability remains useful, but capability does not by itself make in-place
mutation of ee's active generation cancellation-safe.

`ee.index_intake.v1` is the normative intake-telemetry schema. The intake path
gains three modes:

| `intake_mode` | When | Mechanism |
| --- | --- | --- |
| `full_rebuild` | First build, recovery, forced reindex, or the current cancellation-safe production intake for queued single/coalesced changes. | Capture one authoritative DB snapshot, build and validate a complete sibling generation, then atomically swap it into the active path. The safe fallback and current production publication path. |
| `incremental` | A single-document or small-delta intake that can be applied to a private staged generation. | Copy-on-write only: seed a sibling staging generation from the immutable active base, reopen and mutate the staged vector/lexical tiers, validate the complete staged union, then atomically publish. Direct mutation of active tier files is forbidden. Until this staged delta path is complete, eligible jobs use `full_rebuild`. |
| `segment_merge` | Periodic maintenance when WAL/segment pressure crosses a bound. | Compact/vacuum a private staged generation, validate it, then atomically publish. Maintenance never compacts the reader-visible active generation in place. |

Schema fields: `intake_mode`, `docs_touched`, `base_doc_count`,
`segment_doc_count` (WAL/uncompacted), `rebuild_avoided_count`, `merge_count`,
`fallback_to_full` + reason (closed set: `index_absent`, `generation_skew`,
`corpus_revision_mismatch`, `tier_unavailable`, `forced_reindex`,
`delta_over_threshold`). Redaction-safe — counts and modes only, never document
content. Registered in `public_schemas()`, the `schema_list` golden, and
`docs/schemas/ee.index_intake.v1.json`.

### Generation / staleness contract

Incremental intake must keep existing staleness consumers
(`search_index_stale`, `ee index status`) correct and deterministic:

- The index advances a monotonic **index generation** on every committed intake
  (incremental or full). The DB advances its own write generation
  independently.
- An index is **fresh** for a given DB generation when its recorded
  `base_generation` plus all applied incremental deltas cover that DB
  generation; otherwise it is **stale** and a `full_rebuild` is owed.
- A committed `incremental` intake advances the index generation by exactly the
  documents it touched, so a reader observing generation G sees a deterministic
  union of base + applied deltas — never a torn read. WAL/segment entries are
  searchable before compaction (proven upstream), so `segment_merge` is a
  size/perf maintenance step, never a correctness gate.
- Any uncertainty (missing index file, generation skew, corpus revision
  mismatch, tier open failure)
  fails safe to `full_rebuild` with the corresponding `fallback_to_full` reason;
  silent partial intake is forbidden.

Every active generation carries a mandatory `corpusRevision` derived from the
canonical search-document schema, admitted source classes, and each source
projection/eligibility revision. An absent or mismatched revision is never
eligible for incremental mutation or crash-recovery promotion; it forces one
complete staged rebuild and atomic publication.

This ADR records the contract for the runtime behavior. The core delta path
landed in `bd-d67os.6`, integration in `.7`, and the equivalence-property plus
flat-latency perf proof in `.8`.

### Cancellation-safety amendment (2026-08-06, `bd-e90wu`)

Runtime-facing search and index operations receive the caller-owned Asupersync
`&Cx`; they do not manufacture `Cx::for_testing()` contexts or start a nested
runtime. Cancellation, deadline, and budget reasons remain typed through
Frankensearch and back to the command boundary.

Index publication is a two-phase copy-on-write effect:

1. acquire the workspace's index-publication lease;
2. capture one authoritative database generation and document corpus;
3. build every vector and lexical artifact in a uniquely named sibling staging
   directory, checking the caller context at collection and tier boundaries;
4. validate document counts, tier counts, corpus revision, and generation while
   the active generation is still untouched;
5. perform one final cancellation checkpoint;
6. enter a short masked commit tail that writes staged metadata, atomically
   swaps the staged directory into the active path, and records associated job
   completion.

Cancellation before step 6 returns the original `CancelReason`, leaves the
active generation byte-for-byte unchanged, and makes an incomplete staging
directory ineligible for recovery. RAII lease ownership releases the advisory
lock on every exit path. A claimed job has a finalizer: cancellation records a
terminal `cancelled` job state; an unrelated abnormal return records `failed`,
so no worker exit can strand a job in `running`.

Once the masked commit tail begins, cancellation is deferred until the
publication and its job-state bookkeeping finish. There is deliberately no
checkpoint between the active-directory swap and the terminal job update. This
is the smallest non-interruptible region that prevents a published generation
from being reported as cancelled or an old generation from being reported as
complete.

The amendment supersedes the earlier interpretation that reopening and
mutating the active persisted tiers was an acceptable production incremental
path. Frankensearch's WAL and Tantivy mutation APIs are still the intended
building blocks for a future incremental optimization, but ee may invoke them
only against a private staged generation and publish the result with the same
validation/checkpoint/masked-tail protocol as a full rebuild.

## Relationship To Existing Work

- **Group-commit write intake** (Track B, ADR 0077) reduces `fsync`s on the
  commit side; incremental intake reduces index work on the same write. They are
  independent and compose: a coalesced write batch can drive a single
  incremental intake of the batch's touched documents.
- **Scale envelope** (`ee.scale_envelope.v1`) reports index posture
  (freshness, generation, lag). Incremental intake makes that lag shrink per
  write instead of resetting via full rebuild; the envelope may cite
  `ee.index_intake.v1` counters.
- **Coalesced deferral** (`process_pending_index_jobs_coalesced`,
  `memory.rs:5529`) already groups pending jobs; incremental intake changes what
  each coalesced job *does* (delta instead of full rebuild), not when it runs.

## Constraints

- Franken-stack only: no `tokio`, `rusqlite`, or `petgraph`; runtime-facing async
  takes `&Cx` and returns `Outcome<T>` with budget/cancellation preserved;
  `#![forbid(unsafe_code)]`.
- Determinism: same DB + config + query yields stable JSON and a stable pack
  hash. A reader at index generation G observes a deterministic base+delta union.
- No silent staleness: every fallback to `full_rebuild` emits its closed-set
  reason; intake never pretends freshness it does not have.
- Local Cargo is not part of the verification contract on this Mac swarm lane;
  schema and unit tests are proven RCH-only per the epic constraint.

## Rejected Alternatives

- **Full rebuild per single write (status quo):** rejected — it is the
  amplification this track removes; rebuild cost grows with corpus size and
  multiplies under swarm load.
- **Custom ee-side delta-segment + merge scheme:** rejected — unnecessary.
  Frankensearch already provides the vector WAL and Tantivy's streaming
  writer/segment merge; re-implementing a parallel segment store in ee would
  duplicate durable-index machinery and risk diverging from the upstream
  searchable-before-compaction semantics.
- **Async background re-indexer (tokio):** rejected — `tokio` is forbidden, and a
  detached indexer would break the deterministic generation/staleness contract
  and the audited, synchronous write path.
- **Mutating the active `TwoTierIndex` or its persisted tiers in place:**
  rejected — the composite builder is terminal, and reopening the reader-visible
  vector/Tantivy files would expose partial mutation if cancellation lands
  between tier commits. Reopen/mutate APIs are permitted only on a private
  copy-on-write staging generation followed by validation and atomic publish.

## Verification

- A contract test (mirroring `tests/contracts/scale_envelope_schema.rs`) pins
  `ee.index_intake.v1` identity, `public_schemas()` registration, `schema_list`
  golden membership, the required fields, and the closed `intake_mode` /
  `fallback_to_full` sets.
- A `tests/fixtures/failure_modes/*` fixture documents each `fallback_to_full`
  reason with trigger shape and the `full_rebuild` path as the safe fallback.
- The core/index tests carry an equivalence property: an index built by N
  incremental add/update/delete intakes must answer queries identically to the
  same final documents built by one full rebuild (deterministic ranking,
  identical result sets, identical rounded scores).
- Deterministic Asupersync contract tests inject caller cancellation/deadline at
  search collection and index build/publication boundaries. They assert exact
  `CancelReason` propagation, runtime quiescence, no leaked publication lease,
  terminal `cancelled` job state, and byte-identical active metadata/generation
  after a pre-commit cancellation. A static contract also rejects
  `Cx::for_testing()` in the production portions of search, pack, and index
  sources.
- The perf leaf (`bd-d67os.8`) carries the no-mock
  `scripts/e2e_incremental_index.sh` proof. It writes a growing corpus through
  `ee remember`, emits `ee.test_event.v1` `bench_iteration` rows, compares
  search ordering before and after `ee index rebuild`, and writes an
  `ee.perf.artifact_summary.v1` summary. The committed fixture is
  `tests/fixtures/golden/perf_artifact/incremental_index_intake.json`.
- All Cargo verification for the schema and unit tests is RCH-only on this Mac
  lane.
