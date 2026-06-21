# Migration plan: normalized producer identities for outcome attribution (bd-pxi7f)

## Purpose

`src/policy/producer_normalization.rs` introduces `normalize_producer_id(raw) -> NormalizedProducerId` so that harmful-feedback statistics, quarantine, and trust decay attribute outcomes to the **same** producer key regardless of casing or separator drift.

Before this change, audit and derivation-metadata payloads carried producer strings in inconsistent shapes:

| Source                                   | Raw value examples                                       |
| ---------------------------------------- | -------------------------------------------------------- |
| Agent Mail registration                  | `PinkOriole`, `FoggyBison`, `CyanCardinal`              |
| tmux pane id                             | `cc_1`, `cod_2`, `%4`, `CC_1`                            |
| Harness program                          | `claude-code`, `Claude Code`, `claude_code`, `codex-cli` |
| Human reviewer                           | `jeff-emanuel`, `Jeff Emanuel`, `jeff@example.com`       |
| Reflection result producer               | `reflection:gaps:claude-opus-4-7`                        |
| Workflow context                         | `review_session:bootstrap`, `swarm:idea-wizards`         |

Each variant accumulated harmful-feedback counts independently. A producer who actually deserved quarantine never reached the SPRT threshold because their outcomes were spread across three or four spellings.

## What's already landed

This commit lands only the **pure normalization helper plus inline unit tests**. No callers have been changed yet — `record_outcome`, `harmful_feedback`, audit-row writes, and quarantine logic continue to consume raw strings.

## Migration phases (downstream beads)

### Phase 1 — Producer-identity capture (this commit)

- `normalize_producer_id` available under `crate::policy`.
- `NormalizedProducerId { kind, canonical, original }` exposes the canonical key, best-effort kind metadata, and original raw string. `attribution_key()` returns the canonical key without a kind prefix so heuristic kind drift cannot fragment feedback counters; reporting adapters that need a self-describing public key should compose `<kind>:<canonical>` at that boundary.
- Inline tests cover pane ids, Agent Mail handles, harnesses, humans, reflection contexts, workflows, empty/whitespace inputs, and `attribution_key` disambiguation across kinds.

### Phase 2 — Wire normalization at the write site (follow-up bead)

When `record_outcome`, `record_harmful_feedback`, and `apply_curation_audit` write a new audit row or feedback event:

1. Read the raw producer identifier from the existing source (`actor`, `producer.id`, `derivation_metadata.producer.payload.externalProducer.id`, …).
2. Call `normalize_producer_id(raw)` and store:
   - `producer.id_raw = <original input verbatim>`
   - `producer.id_canonical = <NormalizedProducerId.attribution_key()>`
   - `producer.id_kind = <NormalizedProducerId.kind.as_str()>` when a schema or report needs explanatory metadata
3. Continue indexing harmful-feedback by `id_canonical` rather than `id_raw`.
4. Schema bump: add `producerIdCanonical` (string, non-secret, deterministic) alongside the existing `producerIdRaw`/`actor` fields in `derivation_metadata_json` and the relevant audit event schemas. Bump the schema version (`ee.audit.derived_memory_created.v2`, `ee.outcome.feedback_event.v2`, …) and add a failure-mode fixture for any newly-emitted degradation.

### Phase 3 — Read-side coalescing (follow-up bead)

Quarantine, trust-decay calculators, and outcome-report aggregations must:

- Bucket feedback events by `producer.id_canonical`. Treat the `Unknown` kind as a single bucket only when the canonical key is non-empty — empty canonicals indicate genuinely missing attribution and must NOT be collapsed with each other.
- When emitting agent-readable reports, render `<original> (-> <canonical>)` so the agent can audit normalization decisions without losing the verbatim input.

### Phase 4 — Audit-row backfill (follow-up bead)

Existing audit rows persisted before Phase 2 continue to carry raw strings only. They do not need to be rewritten — the normalization function is **idempotent and stateless**, so the read-side aggregator (Phase 3) can compute the canonical key on the fly from the stored raw string.

If a future query requires the canonical key as a SQL index (rather than a Rust-side bucket), add a derived column and a backfill job:

1. `ALTER TABLE outcome_feedback_event ADD COLUMN producer_id_canonical TEXT;`
2. Backfill in batches of 1k rows, computing the canonical key with `normalize_producer_id`. Idempotent; safe to re-run.
3. After backfill verifies (count + spot-check), add `CREATE INDEX idx_ofe_producer_canonical ON outcome_feedback_event (workspace_id, producer_id_canonical, observed_at);` and switch quarantine queries to the new column.

Until that bead lands, the column does not exist and the normalization happens in Rust at query time. Keep the backfill plan in this doc so future contributors don't reinvent it.

## Non-goals

- `normalize_producer_id` does **not** authenticate or attest. It only canonicalizes. A producer's actual trust class is decided by `policy::trust_decay`.
- `Unknown` is not a failure mode — it's a deliberate bucket for inputs that don't fit any of the structured shapes. Empty canonical strings inside `Unknown` indicate missing input, not normalization failure.
- The normalization table (`KNOWN_HARNESSES`, `KNOWN_WORKFLOW_PREFIXES`) is intentionally small. Adding a new harness should be a single-line table edit reviewed against the rest of this plan, **not** a regex or heuristic change. Heuristic drift across normalization revisions is the failure mode this whole bead is preventing.

## Verification

- Unit tests: `src/policy/producer_normalization.rs` `#[cfg(test)]` block (20 tests).
- Integration: deferred to the Phase-2 bead that wires `normalize_producer_id` into write paths. That bead must also add the audit/feedback-event golden snapshots updated to include `producerIdCanonical`.

## RCH note

Per project RCH-only policy, Cargo proof on this commit is blocked by RCH-E327 (path-dep topology). Static proofs (rustfmt, git-diff-check) gate the change; full Cargo run will land alongside the path-dep remediation tracked by bd-17c65.10.17.1.2/.1.4.
