# ADR 0016: Embedding Model Choice Owned by Frankensearch

Status: accepted
Date: 2026-05-05

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

1. `Cargo.toml` depends on `frankensearch` with default features only.
   Do not enable specific embedder features like `model2vec` or `fastembed`.
2. `ee` config exposes `[search]` options for behavior, not model selection:
   - `mode = "hybrid" | "lexical"` — whether to use embeddings at all
   - `default_speed = "instant" | "default" | "quality"` — latency/quality tradeoff
3. The speed tradeoff maps to Frankensearch's embedder stack, which internally
   selects the appropriate model.
4. Documentation refers to "Frankensearch's default embedders" rather than
   naming specific models.
5. Users who want different embedding models configure Frankensearch, not `ee`.

## Consequences

- No `[embedding] fast_model = ...` config keys in `ee`.
- `ee` benefits from Frankensearch's model updates without code changes.
- Users have a single configuration point for embedding behavior.
- The search module re-exports Frankensearch types without wrapping.
- Degraded mode (lexical-only) works without any embedding model.

## Rejected Alternatives

- **Expose model names in ee config**: Fragments the choice, ignores upstream evaluation.
- **Hard-code specific models in ee**: Couples to Frankensearch internals.
- **Abstract embedding behind ee-specific trait**: Adds indirection without benefit.

## Verification

- `src/search/mod.rs`: Re-exports `Embedder`, `EmbedderStack`, etc. from Frankensearch.
- `Cargo.toml`: No explicit `model2vec` or `fastembed` feature flags.
- `ee config show --json`: No `embedding.model` or similar keys.
- `tests/contracts/frankensearch_local.rs`: Uses Frankensearch defaults, not overrides.
