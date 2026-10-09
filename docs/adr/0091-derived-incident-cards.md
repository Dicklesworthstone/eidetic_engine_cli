# ADR 0091: Derived Incident Cards

Status: accepted
Date: 2026-10-07
Bead: bd-reality-core-convergence-1azkt.59
Amends: [ADR 0085](0085-typed-pack-entity-identity.md) (evidence-span admission)

## Context

Imported CASS evidence enters packs one transcript turn at a time. After the
transcript projection (.45) a turn is readable, but it is still a fragment: a
failing tool result without its fix, or a fix without the error it fixed. An
agent preparing to repeat work needs the incident: what broke, which command
failed, what fixed it, and the run that proved the fix.

The CASS failure-arc derivation (.60) already pairs each failing tool result
with the admitted assistant turns that explained its fix and the later run of
the same command family that verified it, and records those links under the
error's layered fingerprint key.

## Decision

### A card is a derived evidence span, not a new pack entity

An incident card is written as an evidence span of the session it summarizes:

- `producer_kind = cass_import`, `span_kind = summary`, no role;
- the line range runs from the failing command's tool call to the verifying
  run, so its public provenance is the ordinary
  `cass-session://<session>#L<start>-<end>`;
- the excerpt is plain text beginning with the fixed header
  `Incident card (derived by ee from lines <start>-<end>):`;
- its id is an `ev_` id derived from
  `blake3(incident_card.v1 ‖ workspace ‖ failing span id)`, so re-deriving a
  session never duplicates or rewrites a card.

A card therefore passes the unchanged ADR 0085 evidence admission boundary
(recognized producer, indexable kind and role, excerpt-hash verification,
instruction-risk and secret screening, policy epoch), is indexed, searched,
packed, persisted, replayed, graded and explained through the existing
`evidence_span` paths, and carries the fixed `cass_evidence` trust class with
the subclass `derived_incident_card`.

Recognition (`is_derived_incident_card`) requires all four of: producer
`cass_import`, kind `summary`, no role, and the fixed header. An imported
transcript record is a JSON line, so an upstream line cannot take that shape.

### Card text is extractive

Facet labels are fixed; nothing else is generated.

| Facet | Source | Policy class |
|---|---|---|
| header | failing command reduced to program + subcommand | A, reduced |
| `Symptom:` | first rustc error line (+ `-->` location), failing test line, or typed EE/RCH error code with its masked message | A, redacted, screened; falls back to the masked template, then the error class |
| `Fix:` | sentences of the admitted assistant turns between failure and proof, chosen greedily by fix/anchor score under the remaining token budget, shown in transcript order | admitted turns only |
| `Verified:` | verifying command reduced to program + subcommand | A, reduced |

A card is at most 120 estimated tokens. A card is not written when the arc is
unresolved, when no admitted turn explains the fix, or when the assembled text
contains a redaction marker or fails instruction screening. No class-B
(instruction-risk) record feeds any facet.

### Links

The card is recorded as a `repair`/`helpful` link under each fingerprint key of
its failure, with the failing span as `evidence_ref` and
`created_by = "ee import cass (incident_card.v1)"`. Error recall
(`ee diagnose-error`, `ee pack --error-log`) therefore surfaces the card, ahead
of the raw repair turns and in full. `ee why <card>` reads the same links to
list the failing span, error classes, repair turns and verifying run.

### Packing

In the direct evidence lane:

- a matched transcript turn inside an admitted card's line range is replaced
  by the narrowest such card, at the turn's rank; a turn whose card is already
  a candidate is dropped;
- each card's `why` states how many incident cards of its error class the
  workspace holds; sharing an error class does not establish that repairs are
  equivalent, so distinct cards remain eligible for ranking and budgeting;
- duplicate compression retains the first ranked representative only when
  complete projected text, source role and source kind match exactly. Numbers,
  command operand order, case, negation and qualifications remain significant.

The 2026-10-09 amendment replaces the original rule that kept only one card per
error class. Different failures with the same code can require different fixes.
The amendment changes candidate selection without rewriting cards, their source
identities, recorded derivations, or historical pack ledgers.

### Lifecycle

Cards are derived during `ee import cass` with the failure-arc derivation. The
per-session derivation marker names the extractor version, so a store derived
under an older extractor (or before cards existed) is derived once more on the
next import. CASS refresh and backfill reconcile only transcript lines and skip
cards; curation, session review and focus suggestions read transcript turns
only. Cards are never memories: they become durable knowledge only through the
existing review → curate path.

The `cass_error_recall.v2` extractor also recognizes complete `ee.error.v2`
errors and failed `ee.rch.verify.v1` reports with a typed known blocker or a
recognized blocker code. It shares canonicalization with `pack --error-log`
and `diagnose-error --tool ee|rch`, so subsequent errors recall the same
imported repair. Schema, code, and status fields must be unambiguous; nested
examples and truncated or duplicate-field documents do not create error
classes. RCH abstention and contradictory completion statuses cannot verify a
repair. Recall extraction has its own version in the per-session derivation
marker, independent of the card renderer, so unchanged older imports acquire
the additional error classes on their next import.

## Rejected alternatives

- **A fourth `PackEntityRef` kind with its own table.** Correct in the long
  run, but it would duplicate every evidence path (index projection,
  admission, persistence, replay, diff, outcome, why, backup) for an entity
  whose admission requirements are exactly those of evidence. Revisit if cards
  ever need non-evidence semantics.
- **A new `producer_kind`.** The producer column has a CHECK constraint; a new
  value requires rebuilding `evidence_spans`, which `pack_evidence_items`
  references. The fixed header plus kind/role/producer shape identifies cards
  without a migration.
- **Generated (abstractive) summaries.** Not deterministic, not auditable, and
  a new instruction-injection surface.
- **Rendering cards at pack time only.** Cards would be invisible to search and
  to error recall, and replay would have to re-run the extractor.

## Verification

- `core::cass_error_recall::tests` — one bounded, admitted card per resolved
  arc; facets; idempotence; no secrets or unadmitted fix text; no memory rows.
- `core::context::incident_card_tests` — card substitution, direct matches,
  distinct repairs for one error class, preservation of qualified facts and
  plain source roles, and exact duplicate compression in the pack lane.
- `core::context::exact_evidence_duplicate_tests` — complete-text equality
  preserving numbers, command order, case, negation, roles and long-tail
  qualifications.
- `incident_card_cli` in the `integration_g_m` suite — real import, packing,
  persisted ledger verification, replay and repeated-import source preservation.
- `cass::refresh::canonical_reference_tests::derived_incident_cards_neither_block_nor_join_transcript_refresh`.
- `core::incident_card::tests` — extraction and recognition.
