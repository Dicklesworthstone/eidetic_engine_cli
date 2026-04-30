# EE-281: Minimal Frankensearch Persistent Index Spike

Status: completed spike  
Recommendation: go, with a narrow facade and explicit rebuild/staleness guardrails  
Owner: SwiftBasin  
Date: 2026-04-29

## Question

Can `ee` use Frankensearch as the Phase 0 persistent retrieval index without
building custom BM25, vector storage, or fusion code?

The answer is yes, but `ee` should treat Frankensearch indexes as derived
artifacts owned through a small `search` module facade. FrankenSQLite/SQLModel
remain the source of truth for memories, packs, provenance, and audit records.
Frankensearch owns retrieval index files and scoring mechanics only.

## Recommendation

Use the root `frankensearch` crate with:

- `hash` in tests and deterministic fixtures.
- `lexical` for Phase 0 hybrid lexical + semantic search when the index is
  available.
- `storage` only after `ee-db` has its own SQLModel records, because
  `frankensearch-storage` has a separate FrankenSQLite document/queue schema.
- No `download`, `fastembed`, `model2vec`, `ann`, `durability`, `fts5`, `rerank`,
  or `graph` in the first `ee search/context` slice unless a later bead proves
  the added dependency and output contracts.

The minimal Phase 0 integration should rebuild a complete index from `ee-db`
records, open that index for search, and report degraded lexical or no-index
status through stable `ee.status` / `ee.error.v1` output. It should not attempt
incremental mutation until a later bead proves locking, staleness, and recovery.

## Source Snapshot

Local dependency source inspected:

- `/data/projects/frankensearch`, git `10383e3`, clean worktree at inspection.
- `frankensearch/README.md`
- `frankensearch/Cargo.toml`
- `frankensearch/src/index_builder.rs`
- `frankensearch/src/lib.rs`
- `crates/frankensearch-core/src/config.rs`
- `crates/frankensearch-core/src/types.rs`
- `crates/frankensearch-index/src/lib.rs`
- `crates/frankensearch-index/src/two_tier.rs`
- `crates/frankensearch-lexical/src/lib.rs`
- `crates/frankensearch-storage/src/{connection,document,fts5_adapter,index_metadata,job_queue,pipeline,schema,staleness}.rs`
- `crates/frankensearch-fusion/src/searcher.rs`

No Cargo build or dependency tree command was run for this docs-only spike.
When `Cargo.toml` changes land, the forbidden-dependency audit must run through
`rch exec -- cargo tree -e features`.

## What Frankensearch Already Provides

### Root Facade And Features

The root crate is an ergonomic facade over core, embed, index, fusion, lexical,
storage, and durability crates. Its default feature is `hash` only. The feature
shape matters:

- `hybrid = ["semantic", "lexical"]`
- `persistent = ["hybrid", "storage"]`
- `durable = ["persistent", "durability"]`
- `fts5 = ["storage", "frankensearch-storage/fts5"]`

This means "persistent" is not just "write vector files"; it also enables
Frankensearch's own FrankenSQLite storage subsystem. `ee` should not enable the
full `persistent` bundle by default until it has decided how that subsystem
relates to the SQLModel `ee-db` truth store.

For the walking skeleton, the safest features are explicit rather than bundle
aliases:

```toml
frankensearch = { path = "/data/projects/frankensearch/frankensearch", default-features = false, features = ["hash", "lexical"] }
```

`model2vec` may be used behind an explicit EE feature after dependency audit.
`fastembed` remains blocked for now: the current upstream feature pulls
`reqwest`, which brings Tokio/Hyper/Tower crates into `--all-features` and
violates the no-forbidden-dependencies gate. Do not expose an EE feature for it
until that upstream tree is clean.

### Index Build Path

`frankensearch::IndexBuilder` is the high-level entry point for a full rebuild:

```rust
IndexBuilder::new(index_dir)
    .with_embedder_stack(stack)
    .add_document(doc_id, content)
    .build(cx)
    .await
```

The builder:

- Requires at least one document.
- Auto-detects embedders unless `with_embedder_stack` is supplied.
- Embeds each document through `Embedder::embed(&Cx, text)`.
- Writes a fast vector index through `TwoTierIndexBuilder`.
- Writes a quality vector index only if a quality embedder exists.
- With `lexical` enabled, writes a Tantivy BM25 index under
  `{index_dir}/lexical`.
- Returns `IndexBuildStats` with counts and timings.

`IndexBuildStats` is useful for diagnostics, but its elapsed-time fields are not
stable contract data. `ee --json` should either omit timings from deterministic
goldens or put them behind an explicit diagnostics/artifact profile.

### Persistent Vector Artifacts

The vector index crate writes FSVI files:

- `vector.fast.idx`
- `vector.quality.idx`
- `vector.idx` as a fast-tier fallback

`TwoTierIndex::open(dir, config)` loads `vector.fast.idx` first and then falls
back to `vector.idx`. It treats `vector.quality.idx` as optional.

`VectorIndexWriter::finish()` sorts pending records by `doc_id_hash` and then
`doc_id`, writes a temporary file, fsyncs it, renames it into place, and syncs
the parent directory. That gives a good full-rebuild atomicity baseline for
Phase 0.

Important caveat: `VectorIndex::open()` maps files with `MmapMut` and opens them
read/write even for search. WAL and soft-delete paths exist. `ee` should avoid
multi-process mutation of Frankensearch index files in Phase 0 and use
rebuild-then-atomic-swap semantics from a single command path.

### Search Path

`TwoTierSearcher` coordinates:

- Fast embedder query vector.
- `TwoTierIndex::search_fast_with_params`.
- Optional lexical search.
- RRF fusion.
- Optional quality refinement.
- Optional reranking.

Its result type already carries retrieval provenance:

- `ScoredResult.doc_id`
- `score`
- `source`
- `index`
- `fast_score`
- `quality_score`
- `lexical_score`
- `rerank_score`
- optional `explanation`
- optional metadata

`FusedHit` has deterministic ranking tie-breaks: score, both-source presence,
lexical score, then lexicographic `doc_id`. That aligns with `ee`'s
determinism requirements as long as `ee` supplies stable document IDs and a
deterministic embedder in tests.

`TwoTierConfig::validate()` now exists and rejects degenerate values such as
`candidate_multiplier < 1`, non-positive `rrf_k`, out-of-range
`quality_weight`, and too-small quality timeouts. The constructor logs a warning
on invalid config instead of returning an error, so `ee` should call
`validate()` itself and convert failures to stable config errors.

### Lexical Options

Tantivy BM25 is the current build path used by `IndexBuilder` when the `lexical`
feature is active. It stores fields for `id`, `content`, `title`, and
`metadata_json`, supports upsert-by-delete-then-add, and commits via an
Asupersync mutex-protected writer.

The storage crate also has an FTS5 adapter. It is useful evidence that
Frankensearch can offer a FrankenSQLite lexical backend, but the current
`IndexBuilder` path writes Tantivy, not that adapter. For Phase 0, use
Tantivy through Frankensearch instead of writing custom FTS/BM25 code. A later
bead can evaluate `fts5` when the EE storage/index boundary is stable.

### Storage Subsystem

`frankensearch-storage` is not just index metadata. It owns:

- A `Storage` connection using `fsqlite`.
- Document metadata tables.
- Embedding status and job queue tables.
- Content hash deduplication.
- Search history and bookmarks.
- Index metadata and build history.
- Staleness checks.
- A storage-backed ingestion and embedding job runner.

This subsystem uses FrankenSQLite directly, not SQLModel. That is acceptable
inside Frankensearch, but `ee` should not adopt it as the durable memory store.
Doing so would create a second truth store beside `ee-db`.

The useful parts for EE are concepts and future references:

- `IndexMetadata` fields for index name, type, embedder, dimension, record
  count, file path, size, hash, schema version, build time, source doc count,
  config, FEC sidecar, and verification timestamps.
- `StalenessReport` shape for comparing index build state to source documents.
- The job queue model for future daemon/steward work, not Phase 0 CLI commands.

## Minimal EE Shape

Recommended `search` facade responsibilities:

```text
ee-db memory records
  -> search::IndexDocument rows with stable doc_id/content/metadata
  -> Frankensearch IndexBuilder full rebuild
  -> index manifest written by EE
  -> TwoTierIndex::open + TantivyIndex::open
  -> TwoTierSearcher::search_collect
  -> ee.search.result.v1 / context provenance
```

Do not let CLI or core code depend directly on `IndexBuilder`,
`TwoTierSearcher`, `TantivyIndex`, or `VectorIndex`. Keep those behind a module
that returns EE domain/output structs.

Suggested Phase 0 files when implementation starts:

- `src/search/mod.rs`
- `src/search/index.rs`
- `src/search/query.rs`
- `src/search/manifest.rs`
- `src/search/errors.rs`

The manifest should be EE-owned even if Frankensearch later persists more
metadata. Suggested schema:

```json
{
  "schema": "ee.search_manifest.v1",
  "workspaceId": "workspace:...",
  "indexGeneration": 12,
  "sourceDbGeneration": 12,
  "frankensearchRevision": "10383e3",
  "features": ["hash", "lexical"],
  "fastEmbedderId": "hash/fnv1a:256",
  "qualityEmbedderId": null,
  "documentCount": 42,
  "artifactHashes": {
    "vector.fast.idx": "sha256:...",
    "lexical/": "sha256-tree:..."
  },
  "builtAt": "2026-04-29T00:00:00Z"
}
```

`builtAt` is operational metadata and should not be part of deterministic
golden comparisons unless fixtures freeze it.

## Proposed Phase 0 Rebuild Algorithm

1. Read memory/searchable records from `ee-db` in stable order.
2. Map each record to `IndexableDocument`:
   - stable `doc_id` equals the EE memory ID or artifact ID.
   - content is redacted/searchable text only.
   - metadata contains type, level, workspace, trust, and provenance IDs.
3. Build in a temporary sibling directory with `IndexBuilder`.
4. Write an EE manifest into the temporary directory.
5. Verify `TwoTierIndex::open(temp, config)` and lexical open/search smoke.
6. Rename/swap the completed directory into place.
7. Persist an EE pack/search-index record in `ee-db`.
8. `ee status --json` compares source DB generation to manifest generation.

The first implementation can require explicit rebuild after writes. Automatic
incremental indexing should be a later steward/daemon bead.

## Degraded Modes

Required stable degradation states:

| Condition | Command behavior |
| --- | --- |
| No index directory | `ee search` exits with search/index error and repair `ee index rebuild --workspace .`; `ee status` reports `index.exists=false`. |
| Manifest source generation behind DB | `ee search` can fail closed or run with explicit `--allow-stale`; default should be fail closed for context packs. |
| Vector index missing, lexical present | report `semantic_unavailable`; allow lexical-only only if command permits degraded search. |
| Lexical index missing, vector present | run semantic-only and mark `lexical_unavailable`. |
| Embedder/config mismatch | fail with repair to rebuild; do not silently reuse. |
| Invalid `TwoTierConfig` | configuration error before constructing `TwoTierSearcher`. |
| Search result has doc ID not in `ee-db` | exclude it and report `orphaned_index_hit` in diagnostics/status. |

## Test Requirements For Implementation Beads

When this spike turns into code, add tests with RCH-offloaded Cargo commands:

- Unit tests for manifest hashing and staleness comparison.
- Unit tests for `TwoTierConfig::validate()` mapping into EE errors.
- Integration test: build a temporary hash+lexical index from three EE records,
  open it, search, and prove deterministic doc ordering.
- Integration test: missing vector index with lexical present produces the
  chosen degraded JSON shape.
- Integration test: stale manifest/database generation fails closed for
  `ee context --json`.
- Golden test for `ee search --json` with provenance fields and no diagnostics
  on stdout.
- Golden test for `ee status --json` reporting DB/index/degraded capabilities.
- Forbidden dependency audit after features land:

```bash
rch exec -- cargo tree -e features
```

The audit must still reject `tokio`, `tokio-util`, `async-std`, `smol`,
`rusqlite`, `sqlx`, `diesel`, `sea-orm`, `petgraph`, `hyper`, `axum`, `tower`,
and `reqwest`.

## Risks And Guardrails

### Duplicate Storage Stack

Risk: Enabling `frankensearch/persistent` brings in `frankensearch-storage`,
which has its own document and embedding queue schema.

Guardrail: `ee-db` stays the source of truth. If `frankensearch-storage` is used
later, it must be treated as derived index metadata/queue state, not memory
truth.

### Hidden Non-Determinism

Risk: build/search stats include elapsed times; model-based embeddings can vary
with model availability and revision.

Guardrail: tests use `HashEmbedder`; production manifests pin embedder IDs and
revisions; stable JSON excludes wall-clock durations unless explicitly
diagnostic.

### Concurrent Mutation

Risk: vector indexes are memory-mapped read/write, and mutation paths have WAL
and soft-delete behavior.

Guardrail: Phase 0 only performs full rebuilds from a single CLI command. No
daemon, watcher, or multi-writer mutation until later tests prove it.

### Bundle Features Pull Too Much

Risk: `persistent`, `durable`, and `full` are convenient but broad.

Guardrail: spell out minimal features in `Cargo.toml`, then audit the resolved
tree with RCH before closing the implementation bead.

## Go / No-Go

Go for Phase 0 with the following scope:

- Full rebuild only.
- Hash embedder for tests.
- `lexical` feature for BM25 through Frankensearch/Tantivy.
- EE-owned manifest and DB generation comparison.
- Search facade returns EE output structs with provenance and degradation codes.

No-go for the following in Phase 0:

- Enabling `full`, `durable`, `download`, or model-backed embeddings by default.
- Using `frankensearch-storage` as the EE source of truth.
- Implementing custom RRF, vector search, or BM25 in `ee`.
- Multi-process incremental mutation of Frankensearch artifacts.

## Follow-Up Beads

- Gate 0 integration foundation: prove the search/index smoke path.
- EE-120/EE-121 class search beads: implement the facade and rebuild/open path.
- EE-127: expose `ee search --json` from the facade.
- EE-314: add FTS5 smoke and lexical fallback parity tests after the baseline
  search path exists.
- A future manifest/staleness bead: formalize `ee.search_manifest.v1` and
  fail-closed behavior.
