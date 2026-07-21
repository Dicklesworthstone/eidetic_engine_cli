# ADR 0083: User-Global Memory Store (Separate Local Store)

Status: accepted
Date: 2026-06-17
Bead: bd-2vq2z.13 (epic bd-2vq2z, 2026-06 idea-wizard "reach" wave)
Supersedes: ADR 0069 (Global Knowledge Lane) — for the storage-model choice only

## Context

Agents work across dozens of repos, but every `ee` memory is workspace-scoped.
A user's durable conventions — "always run `cargo fmt --check` before
release", "prefer X over Y", "never use approach Z" — should follow the user
into every repository instead of being re-taught (and re-duplicated) per
workspace. The trust-lane scope machinery already exists
(`--memory-scope self|team|global|workspace|verified|swarm`); `MemoryScope::Global`
already parses. What is missing is the **storage and consumption** of a
user-global tier.

ADR 0069 proposed implementing this as a `scope` column inside the single user
store with copy-with-link promotion, and explicitly **rejected a separate
global DB**. Its storage engine (bd-1bfwa.2) is blocked and ADR 0069 remains
`proposed`. On 2026-06-17 the owner re-decided the storage model.

## Decision

**The user-global memory tier is a separate local store** at
`<user-data-root>/global/` (its own `ee.db` + index directory), with full
schema/migration parity to a workspace store, surfaced through the existing
trust-lane scope machinery. This reverses ADR 0069's separate-DB rejection.

Rationale for the reversal:

1. **Portability** — one global store follows the user across all repos and
   machines; nothing is entangled with a particular workspace's lifecycle.
2. **Clean backup/export** — the global store is a single self-contained unit
   to snapshot, restore, and ship in a handoff capsule.
3. **No per-workspace duplication** — a universal rule is stored once, not
   copy-with-link promoted into N workspaces.

### 1. Location and parity

- Default root: `<user-data-root>/global/`, i.e. `~/.local/share/ee/global/`
  with the built-in layout. The root is config-overridable
  (`[memory] global_store_path`). It is resolved from the **user data dir**,
  never a project-local `.ee/` path — the store must follow the user, not the
  repo. (`GlobalStorePaths`, `src/core/global_store.rs`.)
- The store opens with the same `DatabaseConfig` open + `migrate()` path as a
  workspace store, so schema/migration parity is automatic. No second schema.

### 2. Local-first, non-negotiable

The global store lives on the same machine. There is **no cloud**. Mesh
(machine-to-machine) is governed separately and unchanged; global rows cross
machines only by the existing mesh rules applied to them as ordinary memory
rows.

### 3. Scope, capture, and consumption

- `MemoryScope::Global` selects the global store (reused, not re-added).
- `ee remember --global "<rule>"` captures into the global store.
- `ee search` / `ee pack` include the global tier **by default, bounded**
  (`--include-global`, default on), each surfaced item labeled with
  `lane: "global"` provenance so an agent always knows a memory came from
  elsewhere. `--no-global` and `[memory] include_global = false` opt out.
- Fan-in is **budget-bounded**: the global tier gets a small bounded share of
  the pack budget (`DEFAULT_GLOBAL_FAN_IN_BASIS_POINTS` = 1500 bp = 15%,
  configurable), reusing the house-rules quota
  (`crate::core::house_rules`) so global rules never crowd out project context.
- **Curation parity (GH#23):** every `ee memory ...` verb (`list`, `show`,
  `history`, `expire`, `revise`, `level`, `tags`, `link`, `drift`) accepts
  `--global` and then operates on the global store — same schema, same audit
  trail, same workspace-id guard, just resolved against the global workspace
  row. Without this the store was write-only through the normal verbs: a
  memory `ee search` federated back could not be listed, expired, or revised.
  `--global` conflicts with `--database` (it *is* the database selector), and
  the workspace-id guard still rejects cross-store access — a workspace-scoped
  verb cannot mutate a global memory and vice versa.

### 4. Conflict surfacing (non-negotiable, not silent)

When a global rule and a workspace rule collide on the same subject:

- **Same content** → corroboration: the workspace row takes precedence, the
  global row is kept and annotated as corroboration, and the precedence
  decision is **recorded** (`workspaceOverrides`) for audit — never applied
  invisibly.
- **Divergent content** → contradiction: **both** rows stay surfaced with a
  conflict marker. Assembly must NOT resolve a cross-lane contradiction by
  rank — that would hide exactly the disagreement the user must see (e.g.
  global says rebase-never, this workspace says rebase-always).

This policy is implemented as a pure, deterministic, insertion-order-independent
function (`surface_lane_conflicts`) so it is unit-testable without a database
and produces byte-stable JSON.

### 5. Isolation, budget, and opt-out posture

`participate = false` for a workspace blocks both contribute and consume — the
hard privacy boundary from ADR 0069 §5, preserved here. The include decision is
explainable: store presence → participation → config → per-invocation flag, each
"off" reason reported distinctly and all sharing the `global_lane_disabled`
(info) posture code. (`resolve_global_inclusion`, `GlobalInclusionReason`.)

### 6. Backup / export / handoff coverage

`ee backup`, `ee export`, the support bundle, and `ee handoff` capsules MUST
cover the global store so a workspace-only backup never loses user-global
memories. (Wired in the follow-on integration leaves.)

### 7. Store metadata contract

`ee.global_memory.v1` (`docs/schemas/ee.global_memory.v1.json`) is the
redaction-safe store-metadata block (paths, enabled/participating flags, schema
version, memory census; never content) surfaced by index status / doctor /
capabilities.

## Consequences

- **Easier**: universal conventions travel to every repo with clear provenance;
  day-one value in a brand-new repo via primer/recall; backup/export is one
  self-contained unit.
- **Cost**: cross-store search/pack fan-in must stay deterministic and
  budget-bounded (handled by the house-rules quota + sorted conflict output);
  ADR 0069's copy-with-link promotion path and the blocked bd-1bfwa.2 storage
  engine are superseded by this store-model choice.
- **Reused, not broken**: `MemoryScope::Global`, the house-rules quota, and the
  `global_lane_disabled` posture code carry over unchanged.

## Rejected Alternatives

- **Scope column in the single workspace store (ADR 0069)**: entangles the
  global tier with workspace lifecycle, needs copy-with-link duplication, and
  complicates backup/export. Superseded by the owner's 2026-06-17 decision.
- **A cloud-backed global tier**: violates local-first. Rejected.
- **Rank-based cross-lane conflict resolution**: hides disagreement; rejected
  for surfaced conflict markers.
- **Deriving the global path from the workspace DB location**: would pin the
  "user-global" store to a project-local path and break portability. The path
  is resolved from the user data dir.

## Verification

- Unit (`src/core/global_store.rs` tests): path resolution + config override;
  inclusion decision matrix (every gate, distinct reasons, shared posture
  code); conflict surfacing — corroboration (workspace wins, recorded) vs
  contradiction (both surfaced, no auto-resolve) vs unrelated; determinism
  (insertion-order independent); budget-bounded fan-in (cap respected,
  opt-out → empty); store-metadata JSON shape + redaction-safety.
- Integration / E2E (follow-on `reach` test leaf bd-2vq2z.22): `ee remember
  --global` a rule; open a different workspace; assert `ee pack` there includes
  the global rule with `global` provenance; assert a conflicting workspace rule
  is surfaced (not hidden); assert `--no-global` excludes it; assert
  backup/export cover the global store. `ee.test_event.v1` logging throughout.
