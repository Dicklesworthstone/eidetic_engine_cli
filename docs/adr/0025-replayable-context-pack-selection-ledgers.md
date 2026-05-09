# ADR 0025: Replayable Context Pack Selection Ledgers

Status: proposed
Date: 2026-05-09

## Context

ADR 0007 makes context packs the primary `ee` user experience. A pack already
contains a hash, provenance, a selection certificate, per-item explanations,
and degradation records. The database also persists pack records and pack items
so `ee why` can connect a memory back to the packs that selected it.

That is enough to inspect a pack, but not enough to replay or compare the
decision after the system changes. Agents still need answers to questions like:

1. Why did this memory appear in yesterday's pack but not today's?
2. Did the change come from the query, profile, token budget, redaction policy,
   index generation, graph snapshot, trust score, or freshness of evidence?
3. Can a support bundle include enough non-secret evidence for another agent to
   debug the pack without re-running retrieval on a changed workspace?
4. Are old packs honest when the replay data did not exist yet?

Existing decisions constrain the answer:

- ADR 0001 keeps `ee` CLI-first and harness-agnostic.
- ADR 0002 makes FrankenSQLite plus SQLModel the durable source of truth.
- ADR 0004 delegates retrieval to Frankensearch.
- ADR 0007 gives context packs product priority.
- ADR 0008 treats graph metrics as derived features.
- ADR 0009 defines trust classes and prompt-injection posture.
- ADR 0013 requires a single write owner for durable writes.
- ADR 0017 requires swarm-scale resource governance to be measured and
  redaction-safe.
- ADR 0024 defines read-only comparison of already-produced performance
  artifacts.

The replay problem must not turn `ee` into a profiler, a scheduler, a second
memory store, a custom retrieval engine, or a daemon-first service.

## Decision

`ee` will add replayable context pack selection ledgers. A ledger is a
schema-versioned audit artifact attached to a persisted pack record. It records
the safe inputs and derived-asset references needed to reconstruct the pack
selection explanation for a previously emitted pack.

The ledger is not a second source of truth for memories. FrankenSQLite memory
records remain the source of truth, and Frankensearch indexes, graph snapshots,
and caches remain derived assets. The ledger records what the pack builder saw
and how it decided, so a future command can explain or diff the decision without
silently reinterpreting the historical pack.

The initial ledger schema is `ee.pack_replay_ledger.v1`. It contains:

- `packId`, `packHash`, `workspaceId`, `createdAt`, and `createdBy`.
- The command surface, such as `context` or `pack`, and the canonical request
  mode.
- A redaction-safe query record: exact query text when policy permits it,
  otherwise a stable hash plus redaction metadata.
- A query-file digest and normalized query summary when the pack came from
  `ee pack --query-file`.
- Profile, token budget, candidate pool, requested format, and effective
  settings after config and CLI precedence are applied.
- Database generation and schema version.
- Derived-asset references by generation or manifest hash: search index,
  graph snapshot, cache report, and profile evidence when present.
- Candidate counts by stage, omitted counts, and deterministic selection-step
  summaries.
- Selected item IDs, ranks, sections, token estimates, score components,
  `why` strings, diversity keys, trust class, provenance summaries, redaction
  classes applied, and freshness state if available.
- Omitted item summaries when omission affected the final explanation.
- Sorted degradation records with stable codes and repair hints.
- A ledger hash computed over the normalized redaction-safe ledger payload.

The ledger must use stable ordering. Ties are sorted by rank, memory ID, section,
score key, provenance URI, and degradation code as applicable. Rendering formats
such as Markdown and TOON are adapters over the same canonical ledger data; they
must not change selection, redaction, freshness, or degradation decisions.

Old pack records that lack a ledger stay readable. Replay commands report
`ledger_unavailable` as a stable degradation or error depending on the requested
mode. They must not fabricate a ledger by re-running live retrieval and calling
it historical replay.

### Replay And Diff Surfaces

The implementation track should add CLI surfaces equivalent to:

- `ee pack replay <pack-id> --json`
- `ee pack diff <old-pack-id> <new-pack-id> --json`

Exact command shape may follow existing CLI conventions, but the behavior is
fixed:

- Replay mode reconstructs the historical selection explanation from the stored
  ledger.
- Live re-retrieval is a separate comparison mode, not replay.
- Diff mode compares two ledgers and reports added, removed, changed, and
  unchanged items; score deltas; degradation deltas; redaction deltas;
  freshness deltas; and likely owner hints.
- Owner hints are explanations, not automatic assignments. Valid owners include
  `request`, `retrieval`, `packing`, `storage`, `indexing`, `graph`, `cache`,
  `trust`, `redaction`, `freshness`, `profile`, `runtime`, and `unknown`.
- JSON stdout is the public contract. Human repair text and progress belong on
  stderr.

The replay JSON schema is `ee.pack_replay.v1`. The diff JSON schema is
`ee.pack_diff.v1`.

### Evidence Freshness

Replay ledgers create a natural place to surface evidence freshness, but they do
not make freshness a silent mutation mechanism. The first freshness states are:

- `fresh`
- `missing_source`
- `changed_source`
- `unreachable_source`
- `unsupported_source`
- `unknown`

Freshness checks warn and explain. They do not silently tombstone, delete,
rewrite, demote, or promote memories. Any durable trust or lifecycle mutation
still goes through the existing curation, audit, and policy pathways.

### Redaction And Support Bundles

Replay ledgers are support artifacts. They must be safe to include in a support
bundle after policy redaction. Raw secret-like spans, prompt-injection-looking
source text, private query fragments, and sensitive provenance payloads are
redacted or hashed before they enter the ledger.

Support bundles may include ledger summaries, ledger hashes, pack IDs, pack
hashes, generation references, redaction posture, freshness summaries, and
degradation records. They must not include raw secret-bearing memory content.

## Consequences

Agents gain an audit trail for the most important workflow. A later agent can
debug pack drift without guessing whether the cause was query shape, budget,
ranking, stale evidence, graph availability, redaction, or trust changes.

This also makes pack-related regressions easier to test. Golden fixtures can pin
ledger shape, E2E tests can replay a real pack, and performance tests can
measure ledger overhead before enabling richer audit paths by default.

The design adds stricter implementation obligations:

- Pack persistence needs schema-versioned ledger storage or a linked ledger
  table with migration coverage.
- Ledger construction must share the same redaction policy as rendered outputs
  and support bundles.
- Replay/diff commands must be read-only.
- Freshness checks must be visible but non-mutating.
- Large fixtures and RCH-offloaded checks are needed before this becomes a hot
  default path for swarm-scale deployments.

## Rejected Alternatives

- **Re-run live retrieval and call it replay.** Live re-retrieval answers a
  useful comparison question, but it cannot prove what a historical pack saw.
- **Persist raw candidate documents and unredacted query text.** That would
  violate the no-secrets-in-context and support-bundle safety requirements.
- **Make replay depend on a daemon.** Ordinary replay and diff must work as
  one-shot CLI commands.
- **Copy search indexes or graph snapshots into the ledger.** Indexes and graph
  snapshots are derived assets. Ledgers should reference generations and
  manifests, not duplicate them.
- **Use replay as a hidden trust mutation path.** Stale evidence can inform
  later curation, but replay itself is read-only inspection.
- **Emit best-effort ad hoc JSON.** The output must use versioned schemas and
  golden coverage before agents consume it.
- **Fold this into the performance-forensics comparator.** ADR 0024 compares
  performance artifacts. This ADR explains semantic pack selection and can later
  feed safe summaries into performance or support-bundle artifacts.

## Verification

The decision remains true when the `eidetic_engine_cli-w2ts` track proves all of
the following:

1. `eidetic_engine_cli-mn78` lands this ADR and any schema-contract notes needed
   before implementation starts.
2. `eidetic_engine_cli-zn8i` persists deterministic
   `ee.pack_replay_ledger.v1` ledgers for `ee context` and `ee pack`, with unit
   tests for empty packs, lexical-only degradation, graph-unavailable
   degradation, redacted items, and deterministic tie ordering.
3. `eidetic_engine_cli-v454` exposes read-only replay and diff CLI surfaces with
   stable `ee.pack_replay.v1` and `ee.pack_diff.v1` JSON stdout, stderr-only
   diagnostics, missing-ledger degradation, and golden fixtures for no-change,
   ranking-change, redaction-change, and derived-asset-degraded cases.
4. `eidetic_engine_cli-aft1` threads evidence freshness into context and why
   explanations with stable states, repair hints, deterministic ordering, and no
   silent memory mutation.
5. `eidetic_engine_cli-rynf` adds a redaction egress matrix covering context,
   search, why, pack/replay, and support-bundle style outputs. Failures identify
   the leaked pattern class and artifact path without printing the secret.
6. `eidetic_engine_cli-dmu0` adds logged no-mock E2E coverage using real `ee`
   binaries and isolated local workspaces. The dossier records command, cwd,
   sanitized environment, elapsed time, exit code, stdout/stderr artifact paths,
   schema/golden validation, redaction status, degradation status, and first
   failure diagnosis.
7. `eidetic_engine_cli-dcub` measures ledger and freshness overhead through
   RCH-offloaded benchmark or smoke profiles over deterministic large fixtures,
   then records budgets or follow-up beads for any unacceptable overhead.
8. `eidetic_engine_cli-65lu` updates support-bundle and user-facing docs only
   after the behavior works, and freezes any docs-visible JSON contract with
   schema or golden coverage.
9. `br dep cycles --json` remains empty for the planning track.
10. Forbidden-dependency audits continue to reject Tokio, rusqlite, petgraph,
    and other banned crates.
