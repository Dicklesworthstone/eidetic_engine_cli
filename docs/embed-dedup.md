# Insert-Time Embedding Deduplication

Bead: `bd-1iltv`

Status: SimHash/cosine substrate is landed; remember-path integration is not
complete.

## Goal

`ee remember` should avoid recomputing and storing embeddings for duplicate or
near-duplicate memory content when there is already a confirmed equivalent
memory in the same workspace. The dedup decision is a two-stage gate:

1. Use deterministic 128-bit SimHash over canonicalized content to find cheap
   near-neighbor candidates.
2. Confirm the candidate with cosine similarity before reusing its embedding.

This keeps the write path deterministic, local-first, and bounded while avoiding
false positives from SimHash alone.

## Non-Goals

- Do not replace Frankensearch, build a custom vector store, or add custom
  ranking logic outside the write-path dedup gate.
- Do not embed first and deduplicate afterward; the value of this path is
  skipping unnecessary embedder work.
- Do not deduplicate across workspaces.
- Do not mutate memory state during dry-run, preview, or read-only commands.
- Do not treat a SimHash match as sufficient proof without cosine confirmation.

## Current Substrate

The reusable substrate lives in `src/search/simhash.rs`:

- `canonicalize_content_for_simhash` lowercases text, normalizes whitespace,
  strips punctuation, and produces stable token input.
- `simhash_128` emits a deterministic `SimHash128`; empty canonical content is
  represented as the all-zero hash.
- `ranked_simhash_candidates` ranks candidates by Hamming distance and then by
  stable candidate id for deterministic tie-breaking.
- `nearest_simhash_candidate` returns the closest candidate within a configured
  Hamming threshold.
- `confirm_cosine_similarity` validates equal-length, non-empty vectors and
  rejects zero-norm vectors instead of inventing a score.
- `first_confirmed_simhash_candidate` scans ranked SimHash candidates and keeps
  looking after cosine rejection, so a near SimHash false positive does not hide
  a later confirmed duplicate.

Property coverage lives in `tests/property_simhash.rs` and covers deterministic
hashing, Hamming-distance ranking, nearest-candidate selection, cosine edge
cases, cosine rejection, and confirmed-candidate ordering. Contract-style unit
coverage in `tests/embed_dedup_unit.rs` pins the public dedup decision scaffold:
exact-content reuse, whitespace/case variant reuse, SimHash false-positive
rejection by the cosine floor, continued candidate scan after cosine rejection,
default Hamming-threshold rejection, and 16-byte big-endian `content_simhash`
encoding.

## Write-Path Contract

The unfinished remember-path integration should implement this order:

1. Resolve the workspace and prepare the memory content exactly as the normal
   `remember` path does today.
2. If `EE_EMBED_DEDUP_ENABLED` is false, store a fresh memory and emit no dedup
   link.
3. Compute the 128-bit SimHash for the canonical content.
4. Query existing memories in the same workspace with stored `content_simhash`
   values.
5. Keep candidates whose Hamming distance is at most
   `EE_EMBED_DEDUP_HAMMING_K`, default `12`.
6. Fetch candidate embeddings and run cosine confirmation.
7. Reuse an existing embedding only when cosine similarity is at least
   `EE_EMBED_DEDUP_COSINE_FLOOR`, default `0.97`.
8. Persist the new memory with its own `content_simhash` and a dedup link to the
   reused memory when reuse occurred.
9. If no candidate confirms, run the embedder and store a fresh embedding.

The database migration should add a nullable `content_simhash` column encoded as
16 bytes. Null means the row predates this feature or was stored through a path
that could not compute a SimHash. Null rows are ignored by the SimHash lookup
but remain valid memories.

The public explanation surface should expose the link in `ee why` as
`dedupLink` when the surrounding JSON schema uses camelCase. Internal Rust
fields may use `dedup_link`, but emitted JSON must follow the schema convention
for that response.

## Configuration

The integration must register and document these env vars before source code
reads them:

| Variable | Default | Meaning |
| --- | --- | --- |
| `EE_EMBED_DEDUP_ENABLED` | `false` | Enables insert-time embedding dedup. |
| `EE_EMBED_DEDUP_HAMMING_K` | `12` | Maximum SimHash Hamming distance admitted to cosine confirmation. |
| `EE_EMBED_DEDUP_COSINE_FLOOR` | `0.97` | Minimum cosine similarity required before reusing an embedding. |

Raw `std::env::var("EE_*")` reads outside `src/config/env_registry.rs` remain
forbidden.

## Degraded Codes

The substrate itself does not emit degraded codes. When the remember-path
integration starts emitting runtime degradations, the same commit must add
failure-mode fixtures and taxonomy rows for the new codes.

Likely response-time codes are:

| Code | Category | Trigger |
| --- | --- | --- |
| `dedup_disabled` | `response_time` | The feature is explicitly disabled for this write. |
| `simhash_index_unavailable` | `response_time` | Candidate lookup could not inspect stored SimHashes. |
| `cosine_under_floor` | `response_time` | A near SimHash candidate was rejected by cosine confirmation. |

Only emit a degraded entry when the response was actually affected. Static
feature absence belongs in capabilities, not per-response `degraded[]`.

## Tracing

The runtime surface should use `surface = "embed_dedup"` and structured
snake_case tracing fields. Suggested phases:

- `input`
- `candidate_lookup`
- `cosine_confirm`
- `decision`
- `persistence`
- `response`

Common fields from `docs/observability/tracing_field_convention.md` apply:
`workspace_id`, `request_id`, `bead_id`, `surface`, `phase`, `elapsed_ms`, and
`degraded_codes`.

Embed-dedup-specific fields should be emitted when available:
`memory_id`, `candidate_memory_id`, `hamming_distance`, `cosine_similarity`,
and `decision`.

## Verification

Current evidence is limited to build-independent static review,
`tests/property_simhash.rs`, `tests/embed_dedup_unit.rs`, and the intended
chain fixture at `tests/golden/embed_dedup_chain.json`. The unit coverage
proves the reusable SimHash/cosine decision scaffold and parses the fixture to
pin the future `dedupLink` emission shape. It does not prove the persisted
remember-path feature yet. Full acceptance for `bd-1iltv` still requires:

- Unit tests for env parsing and dedup-link selection after DB/write-path
  fields exist.
- E2E tests showing identical and near-identical memories reuse embeddings.
- A semantic false-positive test showing SimHash proximity alone does not reuse
  embeddings when cosine is under the floor.
- Runtime or E2E golden replay for the dedup chain and `ee why` `dedupLink`
  output once the write path persists those fields.
- RCH-backed `cargo check --all-targets`, `cargo clippy --all-targets`,
  `cargo fmt --check`, and targeted tests, with Clippy run under `-D warnings`.

Do not use local Cargo as a fallback on the Mac dev host.

## Remaining Integration Checklist

- Add the nullable `content_simhash` migration and workspace-scoped lookup.
- Register the three `EE_EMBED_DEDUP_*` variables.
- Wire the lookup and confirmation gate into `remember_memory_inner`.
- Persist the dedup link and surface it through `ee why`.
- Add the remaining env, dedup-link, E2E, and golden tests listed above.
- Add degraded-code taxonomy rows and fixtures if the source emits new runtime
  degraded codes.
- Prove the full slice through RCH.
