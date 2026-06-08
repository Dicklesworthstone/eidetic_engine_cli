# House Rules — Cross-Workspace Global Memory Tier

A "house rule" is a memory that should apply across *every* workspace, not just
the one it was written in — a forbidden-dependency policy, a release ritual, a
hard-won convention. House Rules add a **Global** memory tier above the
per-workspace lane so these rules travel, while strict gates keep the global tier
from filling with noise or crowding out project-specific context.

Bead lineage: `bd-1n0np.10` (feature), `10.1` (Global scope lane + candidate-load
union), `10.2` (audited promotion gate), `10.3` (capped pack quota + opt-out +
insights section), `10.4` (tests), `10.5` (e2e), `10.6` (docs/capabilities/help).

## The Global scope lane (landed)

`MemoryScope::Global` sits above `Workspace` (`src/core/memory_scope.rs`). There
is **no new storage** — one shared DB. A memory joins the global tier by carrying
a `global` (or `house_rule`) tag; `memory_in_scope_with_tags` makes scope
filtering tag-aware. Candidate loading unions the active workspace's candidates
with global-scope memories via `list_memories_for_retrieval_with_global`
(deterministic `ORDER BY m.id ASC`), wired through context fallback and index
rebuild/reembed/job loading.

## The audited promotion gate (`core::house_rules`, landed)

A memory reaches the global tier only with justification —
`evaluate_global_promotion_gate`:

- **Explicit human marking** always promotes (human authority).
- Otherwise the memory must carry evidence from **at least N distinct
  workspaces** (ADR-0006, "procedural memory requires evidence"; default N = 3).
- A zero / misconfigured threshold **never** auto-promotes.

The decision records its `basis` (`ExplicitHumanMarking` or
`CrossWorkspaceEvidence { distinct_workspaces }`) for the audit trail, and
promotion is meant to route through strict redaction so a workspace-local secret
never leaks into the global tier.

## The capped house-rules quota (`core::house_rules`, landed)

Global rules must never crowd out project context. `house_rules_quota` resolves a
**bounded share** of the pack budget (default 2000 bp = 20%, clamped to 100%) as
a hard cap for the house-rules section, and a **per-workspace opt-out** disables
the section entirely (cap 0). `select_within_house_rules_quota` greedily fills the
cap in priority order, where a single oversize rule never blocks later rules from
filling the remaining room, and the section never overflows its quota.

## Two hard invariants

1. **Promotion is evidence-gated, never automatic.** Only explicit human marking
   or N-distinct-workspace evidence promotes; the gate is pure and deterministic
   and records its basis for audit.
2. **Global rules are quota-bounded and opt-outable.** The house-rules section is
   capped to a fixed budget share and can be turned off per workspace, so the
   global tier can never starve project-specific context.

## Status

- **Landed + verified:** the Global scope lane + candidate-load union (`10.1`)
  and the promotion-gate + quota decision cores (`core::house_rules`, `10.2` /
  `10.3`), with unit tests.
- **Follow-on (CLI / golden-gated):** surfacing the cores through
  `ee remember --scope global`, `ee rule promote-global <id>` (audited curate
  transition + redaction), the capped house-rules `PackSection` in pack assembly,
  and `ee insights --section houseRules --json`, plus opt-out/quota config
  registration. These add commands/sections that change command-reference,
  agent-docs, pack, and insights goldens, so they land with a coordinated golden
  regeneration.

## See also

- [`dueling-wizards-store-integrity.md`](dueling-wizards-store-integrity.md) — write-immune + read-fence integrity for the shared store the global tier lives in.
- [`dueling-wizards-why-not.md`](dueling-wizards-why-not.md) — explains when a global rule was or was not selected into a pack.
