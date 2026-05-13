# ADR 0028: Counterfactual Lab And Immutable Revisions

Status: proposed
Date: 2026-05-13

## Context

`ee` already persists memories, context packs, audit rows, pack replay ledgers,
and support-bundle summaries. Those records explain what happened, but they do
not provide a disciplined way to answer counterfactual questions:

1. What would a context pack have selected if one memory, rule, or evidence span
   had been different?
2. Can an agent replay a historical pack against the exact durable inputs that
   existed at capture time?
3. Can a memory be revised without overwriting the old fact that previous packs
   and audit rows depended on?
4. Can causal-credit work use replay evidence without making the runtime
   retrieval path more complex or less deterministic?

The N15 track introduces a counterfactual lab, immutable revisioned writes,
frozen replay episodes, and pack-diff tooling. That is a major subsystem, so it
needs an ADR before implementation.

Existing decisions constrain the design:

- ADR 0001 keeps `ee` CLI-first and harness-agnostic.
- ADR 0002 makes FrankenSQLite plus SQLModel the durable source of truth.
- ADR 0007 makes context packs the primary user experience.
- ADR 0009 defines trust classes and promotion boundaries.
- ADR 0013 requires single-write-owner discipline for durable writes.
- ADR 0022 defines the causal evidence ledger.
- ADR 0025 defines replayable context-pack selection ledgers.
- ADR 0027 keeps swarm coordination read-only and advisory.

The lab must not become an agent harness, a planner, a daemon-only workflow, or
a second memory database. It must remain a CLI surface over durable source-of-
truth records and rebuildable derived assets.

## Decision

`ee` will add a counterfactual lab built on immutable memory revisions and
frozen replay episodes.

### Immutable Revision Model

Memory edits are append-only revisions. A revisioned memory keeps the logical
memory identity stable while preserving time-bounded physical versions:

- `valid_from` marks when a version became effective.
- `valid_to` is null for the current version and set when a later revision
  supersedes it.
- Revision writes create a new row or version record and close the prior
  version's validity window in one audited operation.
- Historical search, context, why, and replay can filter by `--as-of` using
  validity-window range predicates.

The chosen storage model is append-only validity windows in the memory storage
surface. It composes with the existing memory table, pack replay ledgers, and
audit trails, and FrankenSQLite can index range predicates for `valid_from` and
`valid_to`. The implementation may use a side revision table if migration risk
requires it, but the external model stays validity-window based.

### Frozen Replay Episodes

A lab capture freezes enough state to replay a context-pack decision without
copying every derived asset:

- workspace ID and database generation
- database manifest hash and WAL/checkpoint posture
- pack replay ledger ID/hash when a pack exists
- index manifest hash and graph/cache generation references
- query summary or redaction-safe query hash
- selected memory/version IDs and validity windows
- degradation, redaction, and trust posture at capture time

The preferred snapshot strategy is a manifest pointer after an explicit WAL
checkpoint posture check, not a full database copy by default. The manifest
hash makes the capture verifiable and storage-efficient, while a later replay
can prove whether the base state is still reconstructable. Full copies remain a
future escape hatch for sealed forensic bundles, not the default hot path.

### Counterfactual Operator

The first counterfactual operator is a single-input swap:

```text
ee lab counterfactual <capture-id> --swap memory:<old-id>=memory:<new-id>
```

The command reassembles the pack against the frozen episode plus exactly one
substituted input, then emits a pack diff. The diff explains added, removed,
changed, and unchanged items; score deltas; token-budget impact; redaction and
degradation deltas; and the owner hint for the observed change.

Single-input swaps are deliberately narrow. They keep interpretation local: an
agent can connect one changed premise to one changed pack. Multi-input batches
can come later as a separate analysis mode once single-swap semantics are
stable and testable.

### Causal-Credit Boundary

The lab provides replay evidence for the N3 causal-credit track, but it is not the runtime causal-credit path.
Runtime outcome updates still flow through the existing outcome, trust, curation, and causal evidence ledgers.
Lab replays can serve as ground-truth or validation artifacts for those systems, but they do not silently promote, demote, tombstone, or rewrite memory confidence.

### Command Surfaces

The implementation track should add surfaces equivalent to:

- `ee memory revise <memory-id> --content <text> --reason <reason> --json`
- `ee lab capture --pack-id <pack-id> --json`
- `ee lab replay <capture-id> --json`
- `ee lab counterfactual <capture-id> --swap <single-swap> --json`

Exact flags may follow existing CLI conventions, but the behavioral contract is
fixed:

- Revision writes are audited and append-only.
- Lab capture/replay/counterfactual commands are deterministic for the same
  capture, query, config, and swap.
- JSON stdout is schema-versioned and stable.
- Human diagnostics and progress go to stderr.
- Missing frozen state is reported with stable degraded codes; replay must not
  silently fall back to live retrieval and call it historical replay.

## Consequences

Agents gain a principled way to inspect memory changes and pack sensitivity
without overwriting history. A memory can be corrected while preserving the
exact version earlier packs saw, and a later agent can ask whether a proposed
change would have altered selection.

The design also creates new implementation obligations:

- Storage migrations must preserve existing memories and make validity-window
  queries deterministic.
- Pack builders and search paths need `--as-of` style filtering before they can
  claim historical replay.
- Capture artifacts must share support-bundle redaction rules.
- Counterfactual diffs must be read-only and must not mutate trust or curation
  state.
- E2E and benchmark gates must run through RCH because replay and pack assembly
  can be CPU intensive.

The design intentionally increases storage and audit complexity. That cost is
accepted because mutable in-place memory edits would make historical pack
explanations and causal validation unreliable.

## Rejected Alternatives

- **Overwrite memories in place.** This is simpler, but it breaks historical
  pack replay, auditability, and any causal evidence that depends on the old
  value.
- **Use a separate revision table as the public model.** A side table can be an
  implementation tactic, but exposing revisions as a separate identity graph
  would force every search, pack, why, and curation path to join two concepts
  that are logically one memory over time.
- **Use vector clocks for every revision.** Vector clocks are attractive for
  distributed writes, but V1 explicitly avoids multi-process concurrent SQLite
  writers as a correctness dependency. Validity windows are simpler and match
  the single-write-owner model.
- **Copy the full database and derived indexes for every capture.** Full copies
  are straightforward to replay, but they create storage blowup and duplicate
  derived assets that ADR 0002 and ADR 0004 treat as rebuildable.
- **Allow multi-input counterfactual batches first.** Batch swaps make the
  resulting diff harder to interpret. Single-input swaps give agents a clear
  cause-and-effect explanation.
- **Just persist every pack and skip frozen episodes.** Persisted packs answer
  what was emitted, not what would have happened under a controlled input swap.
  They lose the counterfactual framing and do not validate revised inputs.
- **Make replay part of the runtime causal-credit updater.** Runtime credit
  updates must remain fast and auditable. Lab replay is evidence for later
  validation, not a hidden mutation path.

## Verification

The decision remains true when the N15 track proves all of the following:

1. `bd-17c65.14.15.1` lands this ADR and static tests before N15.1 through
   N15.5 implementation work starts.
2. `tests/adr_0028_docs.rs` asserts the ADR file exists, is indexed, has the
   required sections, and documents at least three rejected alternatives with
   reasoning.
3. N15.1 adds migration and query tests for `valid_from`/`valid_to`, including
   current, historical, tombstoned, and overlapping-window rejection cases.
4. N15.2 proves `ee memory revise` is append-only, audited, and does not
   overwrite the superseded version.
5. N15.3 proves `ee lab capture` records database, pack, index, graph/cache,
   redaction, and degradation manifest hashes without raw secret-bearing
   content.
6. N15.4 proves `ee lab replay` is byte-deterministic for the same frozen
   episode and reports missing state instead of live-retrieval masquerade.
7. N15.5 proves `ee lab counterfactual` supports exactly one swap, emits a
   stable pack diff, and refuses multi-swap input until that mode has its own
   ADR or amendment.
8. N3 causal-credit tests consume lab replay artifacts as validation evidence
   without making replay a runtime trust-mutation path.
9. RCH-offloaded verification includes the relevant static doc tests, focused
   lab/revision tests, and any pack/replay benchmarks added by the track.
10. Forbidden-dependency audits continue to reject Tokio, rusqlite, petgraph,
    and other banned crates.
