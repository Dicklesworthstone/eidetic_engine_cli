# Multi-Agent Store Integrity — Read Fences + Write Immune System

When many agents share one `ee` checkout, two failure modes erode trust in
memory: a context-producing command can serve results from a **derived asset**
(search index, graph snapshot) that lags the FrankenSQLite DB, and a single
misbehaving source can **flood** the store with low-evidence or near-duplicate
writes. E8 (`bd-1n0np.8`) addresses both with pure, deterministic decision logic
that *reports and proposes* rather than silently mutating.

Bead lineage: `bd-1n0np.8` (feature), `8.3` (read-fence model), `8.4`
(read-fence unit/property tests), `8.6` (write-immune quarantine tier + audit +
`ee curate quarantine`), `8.8` (write-immune unit/contract/golden tests).

## Read fences — "coherent as of generation N"

A read fence states the coherence a caller wants from a `search` / `pack` /
`why` response, and the response carries back a verdict
(`src/core/read_fence.rs`, schema `ee.read_fence.consistency.v1`).

| Fence | Meaning | On lag |
|---|---|---|
| **`Eventual`** (default) | Fast path; derived assets may trail the DB. | Lag is **reported** as a `warning`, never enforced. |
| **`Latest`** | High-stakes opt-in; every derived asset must be ≥ the DB generation. | Lag escalates to `high`; in `--strict` it fails closed. |
| **`Snapshot(n)`** | Replay a pinned workspace generation. | N/A — always an informational pinned replay. |

`evaluate_consistency(fence, db_generation, asset_generations, strict)` returns a
`ConsistencyBlock { schema, mode, db_generation, asset_generations, verdict,
severity, repair, strict_failed }`. The verdict is one of:

- `Coherent` — no asset trails the DB (`info`, no repair).
- `AssetsBehind { max_lag, behind_assets }` — `max_lag` is the largest gap;
  `behind_assets` names the lagging assets, **sorted**. Carries the repair
  `ee index rebuild --workspace .`.
- `PinnedSnapshot { generation }` — always `info`, never fails.

`strict_failed` is `true` **only** under `Latest` + `strict` with a lagging
asset; the caller should fail closed (exit code 6 / `degraded_required`).

### Two hard invariants

1. **`Eventual` never fails closed** — even under `strict`. The fast common path
   is never slowed; lag is advisory only.
2. **Deterministic + stable-ordered** — the same inputs always yield the same
   block, and `asset_generations` is sorted by name. This keeps the block
   cleanly-additive so wiring it onto responses is a single coordinated golden
   update (the threading/emission is the follow-on; the model itself is pure and
   golden-free).

## Write immune system — per-source advisory quarantine

The write chokepoint accumulates **per-source rolling stats** over an explicit
window and flags abusive sources for *advisory* quarantine
(`src/core/write_owner.rs`).

- `WriteStreamObservation::memory_create(source_id, content, trust_class,
  provenance_uri, observed_at_ms)` records one write, deriving an exact
  `content_hash`, a `SimHash128`, a deterministic embedding (for cosine
  confirmation after a SimHash match), the normalized `trust_class`, and an
  `evidence_present` flag.
- `compute_source_write_stats(observations, WriteStreamStatsConfig)` →
  `Vec<SourceWriteStats>`: per source, the write count, exact-duplicate and
  near-duplicate counts/ratio, trust-class distribution, and
  evidence-presence/absence ratios — all over the explicit
  `[window_start_ms, window_end_ms]` window with a configurable near-duplicate
  Hamming threshold.
- `evaluate_write_immune_quarantine(stats, WriteImmuneQuarantineConfig)` →
  `WriteImmuneQuarantineDecision { action, reasons, whitelisted, ... }`.

A decision trips `action = "quarantine"` when any threshold is exceeded, each as
a stable reason code:

| Reason code | Threshold |
|---|---|
| `writes_per_window_exceeded` | `max_writes_per_window` |
| `near_duplicate_ratio_exceeded` | `max_near_duplicate_ratio` |
| `missing_evidence_ratio_exceeded` | `max_missing_evidence_ratio` |
| `high_trust_missing_evidence_ratio_exceeded` | `max_high_trust_missing_evidence_ratio` |

### Three hard invariants

1. **Never a global write stall.** Decisions are strictly per-source: an abusive
   burst source is quarantined while every clean source stays `allow`. There is
   no global lock — one bad actor cannot block the swarm.
2. **Orchestrator whitelist bypass.** `WriteImmuneQuarantineConfig::
   with_whitelisted_source(id)` lets an orchestrator-approved source bypass
   advisory quarantine; the decision still records the tripped `reasons` with
   `whitelisted = true` and `action = "allow"` for auditability.
3. **Generous-threshold false-positive guard.** A legitimate prolific source —
   distinct content, full evidence, under the count limit — is *never* falsely
   quarantined. Quarantine is advisory: `ee` proposes a curation candidate and
   an audit row; it never deletes or hard-rejects on a write-immune verdict.

## Verifying

- **Read fence** — in-module unit tests (`src/core/read_fence.rs`) plus the
  cross-grid property tests in `tests/read_fence_properties.rs`
  (Eventual-never-fails, Latest-fails-iff-lag, exact `max_lag`, deterministic,
  sorted assets).
- **Write immune** — in-module unit tests (determinism, threshold-trip,
  whitelist bypass) in `src/core/write_owner.rs`, plus
  `tests/write_immune_quarantine.rs` (false-positive guard, per-source
  isolation / no-global-stall, high-trust-without-evidence reason).

## See also

- [`dueling-wizards-anchors-freshness.md`](dueling-wizards-anchors-freshness.md) — the freshness/staleness signals sentinels and write-immune both lean on.
- [`dueling-wizards-why-not.md`](dueling-wizards-why-not.md) — read-side explainability for what a fenced read did and did not return.
