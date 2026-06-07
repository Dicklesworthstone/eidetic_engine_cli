# ADR 0056: Code-anchoring substrate (Surface Memory Map + Code-Coupled Freshness)

Status: proposed
Date: 2026-06-07
Bead: bd-1n0np.3.1

## Context

Most coding-agent work is **surface-local**: the agent knows it is about
`src/core/context.rs`, `ee pack`, `cargo clippy`, or schema `ee.response.v2`. But
plain-text retrieval loses that structure, and — more dangerously — a memory
substrate's existential failure mode is confidently serving a **stale** fact.
`ee` already builds a symbol graph (ADR 0042) and can bias a pack toward
git-changed symbols (`--changed-symbols-from-git`), but it has no typed coupling
between a memory and the code/surface it describes, and no inverse signal when
that code changes. The 2026-06-07 review scored the two halves of this substrate
905 (Code-Coupled Freshness) and 795 (Surface Memory Map) and judged them the
foundation the coverage/blind-spot/bootstrap/error-recall features build on.

## Decision

Add one **anchor substrate** with two layers sharing a table.

**A. Surface Memory Map (typed anchors).** `MemoryAnchor { memory_id,
anchor_kind: path|symbol|command|env_var|schema|degraded_code|dependency|config_key,
anchor_value, confidence, source, provenance }`. Extraction is **precision-first**
(remember / CASS import / curate apply / index rebuild): exact paths, schema IDs,
`EE_*` vars, commands, degraded codes, plus explicit anchors. Anchor metadata is
added to the Frankensearch `CanonicalSearchDocument` so exact matches receive a
**deterministic boost** while Frankensearch keeps ownership of semantic ranking.
Graph projection adds `memory -> mentions_surface -> anchor` and `anchor <-> anchor`
proximity edges. `ee impact <path>|--symbol|--command|--env|--schema` is a
read-only query (exact anchors → lexical/semantic → graph neighbors), and
`ee pack --surface <hint>` converts hints to anchor boosts + a coverage-facet hook.

**B. Code-Coupled Freshness.** Capture a blake3 content-hash of the anchored
symbol's text span at anchor time. A steward job **bounded to git-changed files**
recomputes and diffs; drift writes an audited `memory.freshness_transition` row
(mirroring `memory.level_transition`) and multiplies the freshness term in
`src/search/scoring.rs` by a **rank-down penalty** (the memory falls in rank, it
does not vanish). Pack provenance attaches the symbol's **live `file:line`**.
Conservatism is the rule: flag only exact disappearance or content-hash change of
a **resolved** symbol; treat refactor ambiguity (rename/move) as `unknown`, never
`stale`.

## Consequences

- **Easier**: precise pre-edit context (`ee impact`), automatic detection of
  memory rot the way a generic vector DB structurally cannot, and a substrate the
  blind-spot/bootstrap/error-recall/contradiction features reuse.
- **Guarded**: extraction noise is contained by precision-first v1 (a noisy
  anchor table would poison everything downstream); freshness false-positives are
  contained by rank-down-not-remove + resolved-symbol-only + rename=unknown.
- **Intentionally impossible**: freshness never auto-tombstones; anchoring is
  opt-in/auto-suggested so unanchored memories are untouched; recompute never
  scans the full tree per run.

## Rejected Alternatives

- **Auto-demote/auto-remove on drift**: violates no-silent-mutation; rejected for
  rank-down + revalidate candidate.
- **Treat any symbol change incl. rename as stale**: refactor-fragile; rejected
  for resolved-symbol-only + rename=unknown.
- **A free-form tag convention instead of typed anchors**: loses queryability and
  graph edges; rejected.

## Verification

- Unit + property tests (bd-1n0np.3.9): no false anchors on adversarial prose;
  drift = rank-down on resolved change/disappearance, unknown on rename; anchor
  values redacted.
- e2e `scripts/e2e_anchors_freshness.sh` (bd-1n0np.3.x): anchor → `ee impact` →
  symbol change → `symbol_drift` rank-down → revalidate candidate → live
  `file:line` → rename → unknown.
- Determinism gate covers `ee.impact` + freshness fields; perf budget covers the
  search anchor-boost + scoring freshness-penalty hot path (bd-1n0np.3.12).
