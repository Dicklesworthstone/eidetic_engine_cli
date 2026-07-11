# Code-anchoring substrate — Surface Memory Map + Code-Coupled Freshness

Most coding-agent work is **surface-local**: the agent already knows it is about
`src/db/mod.rs`, `ee pack`, `cargo clippy`, schema `ee.response.v2`, or env var
`EE_PACK_TRACE`. Plain-text retrieval loses that structure, and a memory
substrate's worst failure mode is confidently serving a **stale** fact. This
subsystem adds two convergent capabilities sharing one table:

- **Surface Memory Map** — typed *anchors* that bind a memory to the exact code
  surface it describes, plus `ee impact <surface>`, the safe pre-edit query.
- **Code-Coupled Freshness** — when an anchored symbol changes or disappears,
  the memory's freshness degrades and it ranks **down** (it never vanishes), so
  rot surfaces for revalidation instead of ranking silently.

Bead lineage: `bd-1n0np.3` (feature), `bd-1n0np.3.2` (anchor table + precision
extraction), `bd-1n0np.3.5` (`ee impact`), `bd-1n0np.3.7` (freshness). Design:
[ADR-0056](../adr/0056-code-anchoring-substrate-and-freshness.md); symbol
resolution reuses the symbol graph from
[ADR-0042](../adr/0042-symbol-graph-derived-index.md).

## Typed anchors

A `MemoryAnchor` binds a memory to one code surface. There are **eight** anchor
kinds:

| `anchorKind` | Example surface | Extracted from |
|---|---|---|
| `path` | `src/db/mod.rs` | repo-relative path token in a code span |
| `symbol` | `DbConnection::insert_memory` | `::`-qualified symbol in a code span |
| `command` | `cargo fmt --check` | command line in a code span |
| `env_var` | `EE_PACK_TRACE` | `EE_`-prefixed uppercase token |
| `schema` | `ee.response.v2` | `ee.*.vN` schema id |
| `degraded_code` | `index_stale` | snake_case `_unavailable`/`_stale`/… code |
| `dependency` | `frankensearch` | known franken-stack / forbidden crate name |
| `config_key` | `scoring.recency_tau_days` | dotted config key |

Extraction is **precision-first**: it fires only on high-confidence exact
matches (paths, schema ids, `EE_*` vars, commands, degraded codes) inside
backtick code spans, plus explicit `anchor:<kind>:<value>` / `ee-anchor:…`
tokens. Adversarial prose ("src slash db slash mod dot rs") is rejected. A noisy
anchor table would poison impact, coverage, and freshness downstream, so v1
favors recall loss over false anchors.

Anchors are captured at four ingestion points, recorded as the anchor `source`:
`remember`, `cass_import`, `curate_apply`, and `index_rebuild` (the index
rebuild path backfills anchors for memories created before the table existed or
through the revision write path).

### Redaction policy (load-bearing)

Anchors are **metadata-only**. A raw anchor value never becomes a durable
payload. Each anchor stores a domain-separated BLAKE3 hash
(`anchorValueHash = "blake3:<64 hex>"`) plus a short redacted display token
(`redactedValue = "<kind>:blake3:<12 hex>"`). You query by surface value and the
engine hashes it the same way to match — the raw string is never persisted and
never appears in search documents, audit rows, or impact output. Treat any
appearance of a raw path/command/env value in anchor metadata as a bug.

## `ee impact <surface>` — the safe pre-edit query

`ee impact` is read-only. Before editing a file, symbol, command, env var, or
schema, ask which durable memories, decisions, failures, and rules are anchored
to it. No git or diff scanning is required — the harness passes the surface it
already knows.

```bash
ee impact src/db/mod.rs --workspace . --json          # positional = path surface
ee impact --symbol DbConnection::insert_memory --json
ee impact --command "cargo fmt --check" --json
ee impact --env EE_PACK_TRACE --json
ee impact --schema-id ee.response.v2 --json
ee impact --degraded-code index_stale --json
ee impact --dependency frankensearch --json
ee impact --config-key scoring.recency_tau_days --json
```

Exactly one surface target is allowed (positional path **or** one typed flag).
`-n/--limit` bounds results; `--memory-scope` and `--strict-scope` apply the
usual trust lanes.

### Resolution order

1. **Exact anchor hits** — memories with an anchor whose hash equals the queried
   surface's hash. These are authoritative and come first.
2. **Search fallback** — if exact hits do not fill the limit, a lexical/semantic
   `ee search` arm backfills. Skipped (`status: skipped_limit_filled`) when exact
   hits already fill `--limit`.
3. **Graph neighbors** — anchor-proximity neighbors. Currently honest-degraded
   (`status: not_available`, `reason: anchor_graph_projection_not_wired_for_impact_yet`),
   pending the anchor graph projection (`bd-1n0np.3.4`). A non-`ok` phase status
   is an explanation gap, not a failed query.

### JSON contract

Envelope is `ee.response.v2`; `data.command` is `"impact"`:

```jsonc
{
  "schema": "ee.response.v2",
  "success": true,
  "data": {
    "command": "impact",
    "surface": {
      "schema": "ee.impact.v1",
      "kind": "path",
      "anchorValueHash": "blake3:…",
      "redactedValue": "path:blake3:…"
    },
    "request": { "limit": 10 },
    "phases": {
      "exactAnchor":    { "status": "ok", "resultCount": 1 },
      "searchFallback": { "status": "skipped_limit_filled", "resultCount": 0 },
      "graphNeighbors": { "status": "not_available", "resultCount": 0,
                          "reason": "anchor_graph_projection_not_wired_for_impact_yet" }
    },
    "scopeStats": { },
    "results": [
      { "rank": 1, "memoryId": "mem_…", "matchType": "exact_anchor",
        "score": 1.0, "memory": { } }
    ],
    "resultCount": 1,
    "elapsedMs": 3
  }
}
```

`results[].matchType` is `exact_anchor` for anchor hits; search-fallback items
carry their search-derived match type. The report omits raw anchor values; use
`ee memory show <id>` to read a memory's body. Output is deterministic for a
fixed DB + query (the only non-deterministic field is `elapsedMs`).

## Code-Coupled Freshness

`ee` builds a symbol graph (ADR-0042) and can bias a pack toward git-changed
symbols. Freshness adds the **inverse**: when a memory is anchored to a symbol
and that symbol's content changes or disappears, the memory should auto-degrade
and surface for revalidation rather than keep ranking silently.

### Freshness states

Every anchor carries a freshness state, ordered by staleness:

| State | Meaning |
|---|---|
| `current` | The anchored surface is unchanged (or freshness was never checked). |
| `suspect` | Drift is **ambiguous** — e.g. the symbol could not be resolved exactly (a rename/move). Advisory only. |
| `stale` | A **resolved** symbol's content changed (`memory_drift_source_changed`) or it disappeared (`memory_drift_source_missing`). |

### The conservatism contract (read this before trusting drift)

Two invariants make the signal safe to act on:

1. **Rank down, never remove.** A drifted memory's freshness term is multiplied
   by a bounded penalty clamped to `[floor, 1.0]` (default floor `0.4`):
   `current → 1.0`, `suspect → partial`, `stale → floor`. A stale memory falls
   in rank but **remains retrievable** and is never auto-tombstoned. No silent
   mutation: every transition is audited.
2. **rename = unknown, never stale.** Drift is asserted `stale` only on exact
   disappearance or content-hash change of a **resolved** symbol. Refactor
   ambiguity (rename/move that the symbol graph cannot resolve exactly) maps to
   `suspect` (advisory) — never `stale`. False positives on refactors would be
   worse than a missed drift.

### Audited transitions

Each freshness change writes a `memory.freshness_transition` audit row
(mirroring `memory.level_transition`), schema
`ee.audit.memory_anchor_freshness_transition.v1`:

```jsonc
{
  "schema": "ee.audit.memory_anchor_freshness_transition.v1",
  "memoryId": "mem_…",
  "anchorKind": "symbol",
  "anchorValueHash": "blake3:…",   // hashed identity, never the raw symbol
  "previousState": "current",
  "newState": "stale",
  "driftCode": "memory_drift_source_changed",
  "fileLine": "src/db/mod.rs:42",  // live location when the symbol resolved
  "reason": "source_evidence_changed",
  "automatic": true,
  "detectedAt": "2026-06-07T00:00:00Z",
  "detailsHash": "blake3:…"
}
```

The row is redaction-safe (hashed anchor identity) and deterministic for a fixed
transition. `fileLine` carries the symbol's **live** `file:line` when it
resolved and stays absent under rename ambiguity.

Drift is inspected today through `ee memory drift` (read-only); the bounded
steward job that recomputes drift over git-changed files, the live ranking
penalty, and the per-pack `symbol_drift` facet build on the primitives above and
are tracked under `bd-1n0np.3.7`/`bd-1n0np.3.8`.

For claim-gate use, `--mode recent-pack-items` scans a bounded pack-record and
integrity-ledger selection window and fails closed if that authoritative scan truncates. It validates each
pack's replay-ledger hash/status and requires the ledger's integrity-bound
`createdAt` to match the record before applying a fixed seven-day horizon. A
selection exactly seven days old is still in scope; only strictly older,
integrity-valid selections are ignored for claim authorization. Malformed,
future, missing-ledger, or mismatched timestamps fail closed. The report emits
its decision clock as `generatedAt` so the boundary is reproducible. Pack
identities are admitted first and capped ledger bodies are loaded one at a time;
oversized compressed or uncompressed declarations fail before decompression.

Within the bound database snapshot, the collector uses the integrity-validated
ledger `createdAt` to decide which selection is newer, with database-local
admission order only as a deterministic tie-break. It does not claim hidden
SQLite `rowid` is a durable export/import sequence. It also resolves every selected
memory through its `logical_id` revision chain: exactly one live,
non-tombstoned revision must exist. A recent pack that selected a superseded
revision stays blocking until a later pack selects the live revision. Routine
new-memory `unverified` provenance remains visible but advisory; structural
pack or lineage uncertainty emits `memory_drift_source_unverifiable`. Ordinary
`ee search`, `ee why`, and historical replay still expose older evidence for
diagnosis.

## Determinism & graceful degradation

- Same DB + indexes + query → byte-identical `ee impact` JSON (modulo
  `elapsedMs`) and byte-identical audit-detail payloads.
- No symbol graph → no drift signal, but retrieval and `ee impact` are
  unaffected. Unanchored memories are never touched.

## See also

- [`insights-onboarding.md`](insights-onboarding.md) — `ee why` / `ee pack --explain` graph surfaces.
- [`surface-contract.md`](surface-contract.md) — the new-surface registration contract.
- [ADR-0056](../adr/0056-code-anchoring-substrate-and-freshness.md) — the anchoring + freshness decision record.
