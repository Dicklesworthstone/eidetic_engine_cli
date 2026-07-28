# ADR 0085: Typed Pack Entity Identity

Status: accepted
Date: 2026-07-28
Bead: bd-12ubv

## Context

`ee pack` is currently memory-centric. Its candidate, selection, omission,
persistence, replay, feedback, and explanation paths identify every item with
a `MemoryId`. That is correct for durable memories but not for two other
first-class retrieval results:

- an applied procedural rule whose native identity is `RuleId`; and
- an imported evidence span whose native identity is `EvidenceId`.

Requiring either entity to have a linked memory means a sourceless rule or an
undistilled evidence span can be searchable but cannot enter a context pack.
Representing either one with an invented `MemoryId` would conflate policy,
observation, and learned interpretation. Keeping permanent
`context_rule_hit_unhydrated` or `context_evidence_hit_unhydrated` behavior
would leave the Learn → Retrieve → Pack and CASS → Retrieve → Pack loops
incomplete.

The identity decision affects more than hydration. Pack hashes, deterministic
ordering, database foreign keys, replay ledgers, omissions, outcomes, `ee why`,
streaming, renderers, support bundles, schemas, and migrations all need one
coherent representation.

## Decision

### Native typed identity

Pack-capable entities use a closed native identity:

```rust
enum PackEntityRef {
    Memory(MemoryId),
    Rule(RuleId),
    EvidenceSpan(EvidenceId),
}
```

The stable wire kinds are `memory`, `rule`, and `evidence_span`. Their
canonical kind order is:

```text
memory < rule < evidence_span
```

Every equality comparison, sort key, set or map key, cache key, omission key,
replay key, diff key, and hash input includes both kind and ID. A raw ID string
without its kind is not a pack identity.

`DocumentSource` is not reused for this purpose. Searchable sessions,
artifacts, and candidates are not automatically pack-admissible entities.
Artifacts keep their current linked-memory hydration path unless a later ADR
defines a native artifact pack contract.

### Public contract

The mixed-entity pack contract is `ee.pack.v3`. Every selected or omitted item
contains:

```json
{
  "entity": {
    "kind": "rule",
    "id": "rule_..."
  },
  "entityRevision": "blake3:..."
}
```

`memoryId` is removed from v3 items rather than retained beside `entity`.
Keeping both would create two authorities that could disagree. This is an
early-development breaking migration, not a compatibility-shim opportunity.

The coordinated contract change includes pack streaming, revision tokens,
replay and diff, JSONL and hook output, context delta, handoff and support
bundles, attestations, renderers, schema registry entries, fixtures, and
goldens. Stored v2 state is migrated once; new writers emit only v3.

### Live, fail-closed admission

Search metadata is relevance input, never authorization. Immediately before
selection and persistence, a single typed boundary loads the native row and
returns either an admitted entity or a structured denial:

```text
admit_pack_entity(entity, admission_context)
    -> AdmittedPackEntity | structured denial
```

The boundary validates the active workspace, native identity, canonical
entity revision, lifecycle, scope, trust, provenance, and redaction posture.
A generation or revision race receives at most one bounded reassembly.
Persistent drift fails explicitly; a pack is never recorded under a stale
admitted revision.

Admission rules are:

- **Memory:** retain the current memory admission behavior.
- **Rule:** load the procedural-rule row directly. Draft, deprecated,
  superseded, tombstoned, malformed, or scope-mismatched rules are denied.
  Rule content, maturity, trust, scope, tags, protection, and source-memory
  provenance come from the rule projection. A sourceless candidate rule may
  be admitted only as bounded-confidence advisory guidance under ADR 0006.
  Until rule-local signature storage and verification exist, even a validated
  high-trust rule remains advisory rather than authoritative.
  `protected` never bypasses policy. Directory and file-pattern scopes require
  a normalized workspace-relative task target and reject absolute paths,
  traversal, and symlink escape. Cross-workspace rules remain denied until a
  separate audited global-rule contract exists.
- **Evidence span:** load both the evidence row and its session. Require exact
  workspace agreement, canonical excerpt-hash verification, a recognized
  producer, kind and role, positive search and pack eligibility, and the
  current evidence-security policy epoch. Instruction-like, system, tool,
  malformed, hash-drifted, cross-workspace, legacy-unknown, quarantined, and
  denied spans fail closed. Re-run egress redaction. Admitted evidence uses a
  fixed `cass_evidence` advisory trust class and a named neutral utility prior
  of `0.5`; retrieval relevance is not copied into utility. Public provenance
  uses the `ev_` identity and
  `cass-session://<stable-session-id>#L<start>-<end>`; raw CASS identifiers,
  source paths, and host-private metadata are never emitted.

An evidence span's linked memory and a rule's source memories are provenance,
not replacement identities.

### Pack model and algorithms

`PackEntityRef` replaces `memory_id` in candidates, draft items, selected
items, omissions, selection steps, impressions, persistence inputs, replay
ledger entries, provenance-footer entries, validation errors, and caches.

The following behavior is entity-generic:

- redaction and token estimation;
- budget enforcement;
- deterministic tie-breaking;
- MMR and facility-location selection;
- similarity-based redundancy handling;
- omissions and explanation;
- provenance requirements; and
- canonical hashing.

Memory-only features remain explicitly filtered to
`PackEntityRef::Memory` until they have a genuine typed projection. This
includes graph and PPR features, proximity, focus, memory tier, sentinels and
freshness anchors, contradiction and consensus analysis, Bayesian trust,
agent-profile bias, memory debt and drift, changed-symbol evidence, and memory
cache warming. Rules and evidence receive explicit neutral or
`not_applicable` values; they never receive fabricated memory semantics.

A rule and one of its source memories may both be candidates. Similarity and
budgeting handle redundancy; identity is not collapsed.

### Persistence

The next pack-schema migration rebuilds selected-item, omission, and
candidate-impression storage with exactly one native foreign key:

```text
memory_id | rule_id | evidence_span_id
```

Database constraints enforce:

- exactly one non-null native foreign key;
- correct typed-ID prefix;
- one selected entity per pack;
- unique selected rank per pack;
- valid section for each kind (`rule` → `procedural_rules`,
  `evidence_span` → `evidence`);
- workspace integrity for rules and evidence; and
- restricted physical source deletion.

There is no unconstrained `(kind, raw_id)` authority column. The typed identity
is derived from the one populated foreign key.

Entity-to-pack foreign keys do not use `ON DELETE CASCADE`. Native entities use
soft lifecycle transitions, and a future hard purge must explicitly purge or
anonymize historical pack records. Deleting an entire pack record may still
cascade to its owned item rows.

Existing item, omission, and impression rows backfill as `memory`. Compressed
and uncompressed replay ledgers migrate to a typed replay-ledger schema.
Historical `pack_hash` values remain the digest that was originally emitted;
records also carry their pack schema and hash algorithm. Migrated ledger
hashes are recomputed from the migrated ledger representation.

### Deterministic hash encoding

New pack hashes use a domain-separated, labeled, length-delimited encoding of
stable wire values. Each selected and omitted identity contributes:

```text
entity kind
entity id
entity revision
section/rank or omission position
```

Rust enum discriminants, memory addresses, map iteration order, and debug
representations never enter the hash. Changing only an entity kind changes the
hash. Mixed-kind ordering follows the canonical kind order when all prior sort
fields tie.

### Provenance, feedback, and explanation

Direct provenance forms are:

- `ee://memory/<id>`
- `ee://rule/<id>`
- `ee://evidence/<id>`

Evidence additionally carries its redacted CASS session and line provenance.
Pack footers report `entityCount` plus deterministic per-kind counts.

`ee outcome --pack --item` resolves pack, rank, typed entity, and entity
revision. Helpful or harmful selection feedback updates a typed pack-item
impression. It does not automatically mutate a linked memory or apply memory
posterior semantics to a rule or evidence span.

Entity-specific semantic feedback is routed explicitly:

- direct memory feedback retains current Bayesian and trust behavior;
- direct rule feedback uses rule lifecycle evidence; and
- evidence content remains immutable.

Legacy learning consumers explicitly filter memory impressions.

`ee why` generalizes to a typed entity response covering storage, retrieval,
selection, provenance, trust, lifecycle, feedback, entity details, and
degradation. Memory-only posterior, link, graph, and sentinel data remain
inside memory details. Rules expose maturity, scope, and source memories.
Evidence exposes session, line range, producer, screening, redaction, and
admission posture.

## Consequences

- Fresh, safe CASS evidence and sourceless advisory rules can enter packs
  without synthetic memories.
- Identity and feedback remain honest: memory, policy, and observation keep
  distinct lifecycles.
- The pack contract requires a deliberate v3 migration across all consumers.
- Pack algorithms become more explicit because memory-only behavior must be
  gated instead of accidentally applied to every result.
- Live admission adds bounded source-of-truth reads before persistence, which
  is accepted in exchange for workspace, revision, and security integrity.
- The implementation must coordinate with evidence security, rule/index
  freshness, corpus-generation, and single-document publication fencing.

## Rejected Alternatives

- **Eager synthetic memories.** Rejected because they duplicate source truth,
  launder trust and provenance, split maturity and decay lifecycles, and can
  create one memory per transcript span.
- **Lazy pack-time memory materialization.** Rejected because a read-only pack
  would mutate storage and pack hashes would depend on write timing.
- **Permanent honest degradation.** Rejected because it leaves both blocking
  P0 workflows incomplete.
- **Unchecked `(kind, raw_id)` strings.** Rejected because the database cannot
  enforce referential or workspace integrity.
- **Reuse `DocumentSource`.** Rejected because search source and pack
  admissibility are different domains.
- **Keep `memoryId` beside `entity`.** Rejected because dual identities drift.
- **Apply memory graph, Bayesian, or lifecycle semantics to all kinds.**
  Rejected because it fabricates meaning.
- **Resolve rule or evidence outcomes through linked memories.** Rejected
  because a provenance link is not feedback authority.
- **Widen v2 schemas without version bumps.** Rejected because existing
  consumers and stored hashes need an explicit contract boundary.
- **Cascade source deletion into historical packs.** Rejected because it
  silently destroys audit and replay evidence.

## Implementation Sequence

1. Add `PackEntityRef`, canonical entity revisions, and entity-specific
   live-database admission.
2. Convert pack algorithms, persistence, v3 output, typed replay/diff/stream
   contracts, provenance, renderers, caches, schemas, fixtures, and goldens as
   one contract-coherent migration.
3. Generalize pack-item outcomes, impressions, and `ee why`; keep memory-only
   learning consumers explicitly filtered.
4. Wire safe procedural rules and evidence spans through search hydration into
   typed pack candidates.
5. Run public no-mock acceptance from native creation/import through index,
   search, pack, persistence, replay, diff, why, and outcome grading.

## Verification

Required proof includes:

- stable mixed Memory/Rule/EvidenceSpan ordering and hashing;
- a hash change when only entity kind changes;
- typed omissions in deterministic hashes and replay;
- exactly-one-FK, prefix, duplicate-rank, duplicate-entity, wrong-section,
  cross-workspace, stale-revision, and physical-deletion rejection;
- direct packing of a sourceless advisory rule under its `RuleId`;
- direct packing of an undistilled safe span under its `EvidenceId`;
- deterministic denial of ineligible rule lifecycle and scope states;
- deterministic denial of evidence integrity, role, screening, provenance, and
  redaction failures with no raw path or secret escape;
- explicit exclusion of rule/evidence from memory-only graph, trust, profile,
  sentinel, and drift logic;
- mixed-identity round trips through replay, diff, context-show, streaming,
  hooks, JSONL, handoff, and support bundles;
- type-safe pack-item feedback that cannot mutate memory confidence for a rule
  or evidence selection;
- typed `ee why` output for all three kinds;
- deterministic migration of memory-only rows and compressed or uncompressed
  replay ledgers; and
- full schema, contract, golden, property, metamorphic, no-mock E2E, Clippy,
  formatting, and RCH-only verification gates.
