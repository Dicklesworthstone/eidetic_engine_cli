# What-If Memory Sandbox

> Epic `bd-1n0np.21`. Simulate proposed memory changes (new memories, retirements,
> edits) and see their would-be effect on retrieval/packs **before** committing —
> a read-only hypothesis that never mutates the durable store.

## Model (21.1 — `core::sandbox`, landed)

A `SandboxOverlay` is a set of proposed, NON-DURABLE changes layered over a
baseline `memory_id -> content_hash` view:

- `upsert(id, content_hash)` — add or modify a memory hypothetically.
- `remove(id)` — hypothetically tombstone a memory.
- `apply(&baseline) -> overlaid` — returns a **fresh** overlaid view; the baseline
  is never touched (the core no-durable-write guarantee).
- `overlay_hash()` — a stable `blake3:` hash of the canonical (sorted) change set,
  so the same hypothetical always identifies the same scratch namespace
  (order-independent, change-sensitive).

`diff_overlay(&baseline, &overlay) -> SandboxDiffReport { added, modified, removed,
unchanged, overlay_hash }` reports baseline-vs-overlay changes, deterministically.

## Surfaces

| Concern | Surface | Status |
|---------|---------|--------|
| Overlay model + diff | `core::sandbox` (`SandboxOverlay`, `diff_overlay`) | 21.1 ✅ |
| Retrieval fidelity | temp `TwoTierIndex` under a cache namespace keyed by `overlay_hash` (derived scratch, never truth) | 21.2 |
| CLI | `ee sandbox remember/import/curate/diff/apply` (no durable write until apply) | 21.3 |
| Tests | `tests/sandbox_contracts.rs` + unit; `SandboxDiffReport` goldens | 21.4 |
| e2e | `scripts/e2e_sandbox.sh` | 21.5 ✅ |

## Invariants

- **No durable mutation.** Sandbox `remember`/`import`/`curate`/`diff` never write
  the durable store; `apply()` returns a fresh view, the baseline is unchanged.
- **Honest retrieval fidelity.** For new-memory retrieval truthfulness, a temporary
  `TwoTierIndex` is built under a cache namespace keyed by `overlay_hash` (treated
  as derived scratch, never truth). When that temp index is not built, the report
  carries an explicit `sandbox_approximation` marker — never a silent
  best-effort. (Resolves the conceded fidelity caveat, 21.2.)
- **Deterministic.** Same baseline + overlay → identical `SandboxDiffReport` and
  `overlay_hash`.
- **Apply routes through the normal path.** Committing a sandboxed proposal goes
  through the ordinary `remember` / `curate` / `import` flow with full audit — the
  sandbox proposes, it does not write a back door.
