# ADR 0016: Embedding Model Choice Owned by Frankensearch

Status: accepted
Date: 2026-05-05

Update: ADR 0080 narrows this decision for the default install. Frankensearch
still owns embedding mechanics and model execution, but `ee` now opts into
Frankensearch's pinned local Model2Vec fast tier (`potion-multilingual-128M`)
and its asupersync-backed download path so semantic retrieval is neural-local
by default.

## Context

`ee` uses Frankensearch for hybrid lexical + semantic retrieval. Frankensearch
supports multiple embedding backends (Model2Vec, FastEmbed, etc.) and has
already evaluated CPU-friendly models for the default configuration.

If `ee` specifies embedding model names, it:
1. Duplicates the evaluation work already done in Frankensearch.
2. Creates configuration surface that users must understand.
3. Risks divergence if Frankensearch updates its defaults.
4. Violates the franken-stack principle: downstream projects delegate, not duplicate.

## Decision

**`ee` does not select embedding models. Frankensearch owns that choice.**

1. `Cargo.toml` depends on `frankensearch` with the audited local feature set
   needed by the product default. ADR 0080 explicitly enables `model2vec` and
   `download`; `fastembed` remains blocked until it is forbidden-dependency
   clean.
2. `ee` config exposes `[search]` options for behavior, not model selection:
   - `mode = "hybrid" | "lexical"` — whether to use embeddings at all
   - `default_speed = "instant" | "default" | "quality"` — latency/quality tradeoff
3. The speed tradeoff maps to Frankensearch's embedder stack, which internally
   selects the appropriate model.
4. Documentation may name the pinned default model selected by ADR 0080, but
   alternate model selection remains Frankensearch-owned rather than an `ee`
   configuration surface.
5. Users who want different embedding models configure Frankensearch, not `ee`.

## Consequences

- No `[embedding] fast_model = ...` config keys in `ee`.
- `ee` benefits from Frankensearch's embedder stack while pinning the default
  downloaded model for deterministic out-of-box behavior.
- Users have a single configuration point for embedding behavior.
- The search module re-exports Frankensearch types without wrapping.
- Degraded mode (lexical-only) works without any embedding model.

## Rejected Alternatives

- **Expose model names in ee config**: Fragments the choice, ignores upstream evaluation.
- **Hard-code arbitrary user-selectable models in ee**: Couples to
  Frankensearch internals. ADR 0080's pinned default is intentionally narrower:
  it chooses one local product default while leaving model mechanics upstream.
- **Abstract embedding behind ee-specific trait**: Adds indirection without benefit.

## Verification

- `src/search/mod.rs`: Re-exports `Embedder`, `EmbedderStack`, etc. from Frankensearch.
- `Cargo.toml`: `embed-fast` includes `model2vec` and `download`; no
  `fastembed` feature is exposed until that tree is forbidden-dependency clean.
- `ee config show --json`: No `embedding.model` or similar keys.
- `tests/contracts/frankensearch_local.rs`: Uses Frankensearch defaults, not overrides.
