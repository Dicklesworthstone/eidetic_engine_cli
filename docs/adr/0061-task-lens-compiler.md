# ADR 0061: Task Lens Compiler

Status: proposed
Date: 2026-06-07
Bead: bd-1n0np.17.1

## Context

`ee` has a large, expert flag/profile surface — great for control, hard for
routine use. The 2026-06-07 review (score 725) proposed named, inspectable task
*lenses* that compile into existing pack/search/redaction/output knobs. The
defended value (vs "this is just richer profiles") is **auditability**: a lens is
a compiled policy whose **hash is persisted in the pack record**, so `ee why`,
pack replay, and future regression analysis can explain not just the selected
memories but the policy that made them eligible.

## Decision

- `TaskLens { id, version, description, ContextPackOptions overlay, SearchOptions
  overlay, coverage_facet_requirements, allowed/deprioritized kinds, redaction +
  output profile, stable lens_hash }`. A lens **compiles into existing knobs** —
  it is not a planner and never decides what the agent does next.
- Built-in lenses (boring + high-value): `bugfix`, `code-review`,
  `release-readiness`, `dependency-update`, `schema-contract`,
  `performance-investigation`, `coordination-handoff`. Workspace-local overrides
  in `.ee/config.toml`, schema-validated + size-capped.
- Surfaces: `ee lens list`, `ee lens explain <name>` (renders effective options),
  `ee pack --lens <name>`. The lens id/version/hash is recorded in the pack
  ledger; `--no-lens` / explicit flags override.

## Consequences

- **Easier**: one stable flag for agents; intuitiveness for humans; and pack
  decisions become explainable down to the policy that shaped eligibility.
- **Guarded**: opinionated defaults could hide relevant memories — mitigated by
  always surfacing the effective policy via `ee lens explain`, persisting the
  hash, and honoring explicit overrides.
- **Intentionally impossible**: a lens never plans or sequences work; it only
  compiles retrieval/pack/redaction/output policy.

## Rejected Alternatives

- **Treat lenses as plain presets**: loses the audit trail; rejected for the
  pack-record lens hash.
- **Let a lens drive next-action selection**: violates the controlling idea;
  rejected (compiler-not-planner).

## Verification

- Unit + golden (bd-1n0np.17.4): lens compiles to expected effective options;
  explicit flags override; lens hash deterministic + recorded; bad override
  rejected by schema validation.
- e2e `scripts/e2e_task_lens.sh`: `lens explain` → `pack --lens` applies +
  records id/version/hash → `ee why` cites the lens → `--no-lens` diverges.
- Composes with coverage facets (gap-honesty) and the pack-record consumers
  (read-fence consistency, attestation).
