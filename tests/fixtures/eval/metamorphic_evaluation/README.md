# metamorphic_evaluation Evaluation Fixture

Fixture ID: `fx.metamorphic_evaluation.v1`

Scenario:

- `usr_eval_metamorphic_memory_regressions`

This fixture pins metamorphic evaluation behavior for paired memory states. It
covers five deterministic relation families:

- positive feedback strengthens the same selected rule
- contradictory evidence raises review risk instead of becoming authoritative
- supersession prefers the latest procedure and excludes the old procedure
- tighter token budgets preserve the highest-priority memory while trimming detail
- semantic fallback emits `semantic_disabled` and preserves lexical top results

## Trust classes: the ORDER is what this fixture measures

Two of the families above depend on a trust gradient rather than on any
particular trust class. Budget trimming needs the background detail to rank
below the rules, and the contradiction family needs the counterclaim not to
become authoritative. So these memories are assigned three distinct stored
classes with strictly decreasing `TrustClass::initial_confidence`:

| role in the fixture | trust class | initial confidence |
| --- | --- | --- |
| release rules, procedures, confirmed positive outcome | `agent_validated` | 0.65 |
| incidental background detail (trimmed first) | `agent_assertion` | 0.50 |
| the skip-verification counterclaim (must not win) | `legacy_import` | 0.30 |

**If you change one of these, preserve the strict ordering.** Flattening two
roles into the same class, or inverting a pair, silently changes what the
budget-trimming and contradiction families measure while every shape check
still passes.

These replaced `verified`, `observed` and `untrusted` (bd-e9zcn, 2026-10-07).
Those three spellings are not stored `TrustClass` values, so seeding this family
would have aborted; nothing caught it because the fixture is contract-checked
for shape and is not in `RETRIEVAL_WORKLOADS`, so it is never seeded. The
mapping above was chosen to preserve the gradient the old labels implied, not to
re-rank anything.

Generated run artifacts belong under
`target/ee-e2e/metamorphic_evaluation/<run-id>/`.
