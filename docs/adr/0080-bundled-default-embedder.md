# ADR 0080: Downloaded Default Local Embedder

Status: accepted
Date: 2026-06-18
Bead: bd-1et0v.1

## Context

`ee` has long described Frankensearch as the owner of retrieval model choice
(ADR 0016), but the shipped default still lets an ordinary first run behave as
if semantic retrieval is absent: the compiled dependency set exposes hash and
storage only, `ee doctor` can be green while retrieval is non-semantic, and the
operator has to know to inspect `ee model status` or run a mutating reembed path
to discover the truth.

The product requirement for `bd-1et0v` is stronger: semantic embeddings are real
and on by default out of the box, while the project stays local-first,
franken-stack-only, deterministic, and honest about fallback.

Upstream Frankensearch already provides the mechanism we need:

- `frankensearch` exposes feature flags `hash`, `storage`, `model2vec`, and
  `download` in `/dp/frankensearch/frankensearch/Cargo.toml`.
- `EmbedderStack::auto_detect()` and `auto_detect_with(Some(model_root))` use a
  lazy download path when `download` and `model2vec` are enabled.
- The download path is implemented with `asupersync::http::h1`, not `reqwest`,
  `hyper`, or Tokio.
- The built-in potion manifest is pinned to
  `minishlab/potion-multilingual-128M` at revision
  `a28f4eebecd4dc585034f605e52d414878a0417c`, requires
  `tokenizer.json` and `model.safetensors`, verifies SHA-256 for both files,
  declares Apache-2.0, and has dimension 256.
- Download consent is explicit in Frankensearch through
  `FRANKENSEARCH_ALLOW_DOWNLOAD`, `FRANKENSEARCH_OFFLINE`, and
  `DownloadConsent`/`ConsentSource`.

This ADR is design and contract only. Runtime behavior lands in later leaves.

## Decision

`ee` will make the Frankensearch Model2Vec fast tier the default active local
semantic embedder. The default model is `potion-multilingual-128M` (256d,
Apache-2.0, pinned revision above), downloaded once into ee's model cache and
then reused from disk.

This is intentionally a **downloaded default**, not a binary-bundled model:

- normal installs do not embed roughly 531 MB of model assets in the `ee`
  executable or crate package;
- first semantic use may download the two pinned files;
- cached assets are verified and reused;
- an air-gapped bundled-model distribution remains an explicit opt-in package,
  not the default install path.

`ee` will enable Frankensearch with the feature set needed for this path:

```toml
frankensearch = { ..., features = ["hash", "storage", "model2vec", "download"] }
```

`fastembed` remains out of the default because the current quality-tier path is
not yet forbidden-dependency clean for `ee`. The quality tier is therefore
reported honestly as absent/null until a clean backend lands. The hash embedder
stays compiled and remains the fallback, but it is no longer the happy-path
default.

## Consent, Offline, And First-Run UX

Frankensearch defaults download consent to denied unless explicitly granted. For
`ee`, local semantic retrieval is the default product behavior, so the runtime
leaf must grant download consent by default for ee-owned model fetches unless
offline mode is active.

Required policy:

- `FRANKENSEARCH_OFFLINE=1` or an ee offline mode blocks network download and
  falls back to deterministic hash posture.
- If the model is already cached, no network access is needed.
- If the model is absent and downloads are allowed, first semantic use performs
  a foreground download with progress on stderr and machine-readable JSON still
  stable on stdout.
- Prefer a public Frankensearch programmatic consent API. If the facade does not
  expose it yet, the implementation may set `FRANKENSEARCH_ALLOW_DOWNLOAD=1`
  in-process for the duration of ee-owned model initialization and must document
  that shim next to the call site.

The model root for ee-owned downloads is ee's data directory, for example
`~/.local/share/ee/models`, with `FRANKENSEARCH_MODEL_DIR` honored as an
explicit override where Frankensearch already supports it.

## Determinism Contract

The local neural path is deterministic for a fixed:

- DB contents and index generation;
- ee config and environment posture;
- model id, pinned revision, file sizes, and SHA-256 digests;
- Frankensearch feature set.

The selected model fingerprint must be recorded in index/model metadata and
exposed through `ee.embedding_posture.v1`. Ranking ties must remain
deterministic. If a future backend cannot guarantee deterministic embeddings on
all supported platforms, it must set `deterministic=false` in the posture block
and cannot be used for byte-identical pack hashes without an explicit contract
update.

## Embedding Posture Schema

This ADR introduces `ee.embedding_posture.v1`, a redaction-safe block shared by
index status, doctor, capabilities, reembed, and model status surfaces. It
contains no memory content, query text, or raw vectors.

Required fields:

| Field | Meaning |
| --- | --- |
| `schema` | Always `ee.embedding_posture.v1`. |
| `mode` | Closed set: `neural_local`, `deterministic_hash`, `neural_remote_blocked`. |
| `semantic` | True only when a real semantic model is active. |
| `source` | Deterministic source label such as `registry_observed`, `frankensearch_hash_fallback`, or `download_blocked`. |
| `fast_model_id`, `fast_dimension` | Active fast-tier embedder id and dimension. |
| `quality_model_id`, `quality_dimension` | Active quality tier, or null when absent. |
| `deterministic` | Whether the selected posture is deterministic for fixed inputs/assets. |
| `registered_model_count`, `available_model_count` | Workspace embedding registry counts. |
| `selected_registry_model` | Redaction-safe selected registry row summary, or null. |
| `vector_coverage` | `{ embedded, total }` for active-vector coverage. |

`ee.embedding_posture.v1` is registered in `KNOWN_SCHEMAS`, `public_schemas()`,
and the schema-list golden. Later retrieval-truth leaves must reuse this schema
instead of cloning ad hoc posture fields.

## Relationship To Existing ADRs

- ADR 0016 still stands for ownership: `ee` must not implement a custom vector
  store, BM25, RRF, or embedding registry. Frankensearch owns those mechanics.
- ADR 0016 is narrowed by this decision: `ee` now explicitly opts into
  Frankensearch's local Model2Vec fast tier and download lifecycle as the
  product default.
- ADR 0074 remains the lifecycle-readiness contract. This ADR adds the smaller
  active-posture block that ordinary operational surfaces embed.

## Rejected Alternatives

- **Keep hash as the default and document it better.** Rejected. It keeps a
  green out-of-box posture while retrieval is not semantic.
- **Bundle the 531 MB model into every ee binary/package.** Rejected. It bloats
  installs and makes air-gapped packaging the default instead of an opt-in
  distribution.
- **Require manual model setup before semantic retrieval.** Rejected. It repeats
  the current footgun and makes the default install misleading.
- **Enable `fastembed` as the quality tier now.** Rejected until the dependency
  path is proven forbidden-dependency clean for `ee`.
- **Add an ee-specific embedding trait or custom downloader.** Rejected.
  Frankensearch already owns embedder lifecycle and uses asupersync HTTP.

## Verification

- `docs/schemas/ee.embedding_posture.v1.json` pins the posture contract.
- `tests/contracts/embedding_posture_schema.rs` verifies schema identity,
  `KNOWN_SCHEMAS`, `public_schemas()`, export parity, closed posture modes,
  required fields, and redaction-safe nested model shape.
- `tests/fixtures/golden/schema/schema_list_json.golden` includes
  `ee.embedding_posture.v1`.
- Runtime leaves must prove model download, offline fallback, first-run
  progress, and cross-surface posture equality through their own tests and e2e
  scripts.
