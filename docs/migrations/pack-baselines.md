# V078 — `pack_baselines` (per-agent `--since last` ledger)

Migration `V078_PACK_BASELINES` (bd-7lvbg.6) creates the per-agent
pack-baseline ledger backing `ee pack --since last` / `ee context
--since last`. No manual step is required; the table is created
idempotently by `ee init` / `ee migrate run`.

## Shape

One row per (workspace, agent, task key, pack):

| Column | Meaning |
|---|---|
| `workspace_id` | FK → `workspaces(id)`, `ON DELETE CASCADE` |
| `agent_name` | Identity from `EE_AGENT_NAME` at pack time |
| `task_key` | Optional `--task-key` scope; `''` = any-task baseline |
| `pack_id` | FK → `pack_records(id)`, `ON DELETE CASCADE` |
| `pack_hash` | The hash `--since last` resolves to |
| `created_at` | RFC 3339 write time (resolution recency key) |

## Semantics

- **Written** whenever a pack persists with an agent identity set.
  `--read-only` / `--no-persist` / `--no-baseline-write` paths never
  write. A ledger write failure is warn-and-continue: it can never
  unwind the persisted pack record.
- **Resolved** newest-first for the exact task key when one is given,
  falling back to the agent's newest any-key row; ties break by
  `created_at` then `pack_id`, both descending. No baseline resolves →
  the honest `context_delta_no_baseline` full-pack fallback.
- **Bounded** per (workspace, agent) by `[pack]
  baseline_ledger_max_rows` (default 32). Oldest rows past the cap are
  evicted inside the insert transaction with one
  `pack.baseline_evicted` audit row naming the evicted pack ids — the
  ledger never shrinks silently.
- **GC-coherent**: pack-record deletion cascades, so a baseline can
  never name a pack whose ledger is gone (resolution would otherwise
  produce `context_delta_prior_unknown` on a hash the workspace can no
  longer verify).

## Rollback

The table is derived coordination state, not provenance: dropping every
row only costs agents one full (non-delta) pack each. There is no
rollback command; `--since <explicit-hash>` keeps working regardless.
