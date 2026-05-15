# Feature Flag Registry

This file is the canonical registry of every Cargo feature flag declared in
`Cargo.toml` `[features]`. Each entry records the flag's status, the surface
it gates, the bead that owns it, and the version at which it was introduced.

The registry is enforced by `tests/feature_flag_registry_in_sync.rs`: every
flag in `Cargo.toml` must have an entry here, and every entry here must
correspond to a flag in `Cargo.toml`. CI fails on drift.

`status` values:

- **active** — the flag gates real code and is exercised by current
  workflows. Toggling it changes behavior.
- **reserved** — the flag exists in `Cargo.toml` but has no current
  cfg-gates. Reserved for a future subsystem; the flag's name is
  documented here so it stays load-bearing.
- **deprecated** — the flag will be removed in a future release. A
  follow-up bead tracks the removal.

`[]` as a Cargo flag value means "no optional Cargo dependencies are
pulled in." It does **not** mean the flag is empty — the flag can still
gate code via `#[cfg(feature = NAME)]`. The `status` column reflects
whether the gate is exercised.

## Registry

| Flag                 | Status     | Cargo value                              | Gate surface                                                                                                              | Owner bead             | Since version |
| -------------------- | ---------- | ---------------------------------------- | ------------------------------------------------------------------------------------------------------------------------- | ---------------------- | ------------- |
| `default`            | active     | `["fts5", "json", "embed-fast", "lexical-bm25", "graph"]` | Composite — enables `fts5`, `json`, `embed-fast`, `lexical-bm25`, `graph` by default.                                     | core build             | 0.1.0         |
| `fts5`               | active     | `["frankensearch/fts5"]`                 | Enables the Frankensearch FTS5 lexical fallback dependency.                                                               | `bd-17c65.2` (B-series) | 0.1.0         |
| `json`               | reserved   | `[]`                                     | No cfg-gates in `src/`. Reserved for a future "JSON-only minimal output" build profile; today JSON output is unconditional. | `bd-17c65.11.7` (K7)   | 0.1.0         |
| `embed-fast`         | active     | `["frankensearch/model2vec"]`            | Enables the Frankensearch `model2vec` fast semantic embedder.                                                             | `bd-17c65.2` (B-series) | 0.1.0         |
| `lexical-bm25`       | active     | `["frankensearch/lexical"]`              | Enables the Frankensearch BM25 lexical scorer.                                                                            | `bd-17c65.2` (B-series) | 0.1.0         |
| `graph`              | active     | `[]`                                     | Gates `src/graph/mod.rs` graph analytics surface (42 cfg-gated functions including PageRank, centrality refresh, neighborhood expansion). Default-on so `ee graph *` works out of the box. | `bd-17c65` (graph subsystem) | 0.1.0   |
| `mcp`                | active     | `[]`                                     | Gates the optional stdio/server adapter module at `src/lib.rs:24`. The CLI discovery surfaces `ee mcp manifest` and `ee mcp validate` remain available in default builds and report `mcp_feature_disabled` as a capability gap when the adapter is not compiled. Opt-in via `cargo install eidetic-engine --features mcp` per README §Agent Harness Integration. | `bd-17c65` (MCP adapter) | 0.1.0       |
| `serve`              | reserved   | `[]`                                     | No cfg-gates in `src/`. Reserved for the future localhost HTTP/SSE adapter described in AGENTS.md §Module Layout (`src/serve/`). Not implemented in v0.1. | `bd-17c65.11.7` (K7) | 0.1.0         |
| `science-analytics`  | reserved   | `[]`                                     | One cfg-gate at `src/science/mod.rs:1999`. The corresponding CLI surface `ee analyze science-status` is `CommandEffect::degraded_unavailable` per `src/core/effect.rs`. Reserved for the future analytics subsystem (EE-171). | `bd-17c65.11.7` (K7) | 0.1.0         |

## Reserved-flag contract

A `status: reserved` flag carries an implicit promise: the flag's name
will not be reused for a different purpose, and the flag will either be
shipped to `active` or marked `deprecated` (with a removal bead) by the
next minor version after a 90-day reservation window. Reservations
without a tracking bead or a removal plan accrue technical debt.

## Cross-references

- `docs/silent-fallback-inventory.md` — the `science-analytics` flag is
  cross-referenced there because the `ee analyze science-status` CLI
  surface is currently `CommandEffect::degraded_unavailable`.
- `bd-17c65.11.7` (K7) — this bead owns the registry. Updates to flag
  status are tracked through it until a successor bead is filed.
- `Cargo.toml` `[features]` section — the source of truth for flag
  *names*. This file is the source of truth for flag *intent*. CI
  enforces 1:1 correspondence.
