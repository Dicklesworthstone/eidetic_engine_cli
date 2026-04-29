# ADR 0009: Trust Class Taxonomy

Status: accepted
Date: 2026-04-29

## Context

Three project artifacts describe the trust class taxonomy in subtly
different ways, and EE-260 (the bead that adds the runtime enum and
memory fields) is queued for implementation:

- `README.md` (the published surface) lists five classes and pins their
  initial confidences: `human_explicit (0.85)`, `agent_validated (0.65)`,
  `agent_assertion (0.50)`, `cass_evidence (0.45)`, `legacy_import (0.30)`.
- `COMPREHENSIVE_PLAN_TO_MAKE_EE__OPUS.md` §22 (the canonical plan)
  reuses the same five names but uses a different vocabulary for
  promotion and demotion transitions.
- `COMPREHENSIVE_PLAN_TO_MAKE_EE.md` (the older long plan) describes a
  richer set (validated agent, project explicit, manual import, etc.)
  with promotion/demotion triggers tied to feedback evidence and
  validation count.

If EE-260 ships before this divergence is reconciled, every downstream
consumer (retrieval scoring, packing priority, prompt-injection defense,
redaction policy, audit log shape, MCP schema, curation lifecycle) will
inherit whichever taxonomy the implementing agent picked. Two later
features could then disagree about whether a given memory is
`agent_validated` or `agent_assertion`, and the disagreement would be
silent.

The trust class field is also load-bearing for retrieval: it factors
into the scoring composition described in `COMPREHENSIVE_PLAN_TO_MAKE_EE__OPUS.md`
§13.6 and decides whether a memory is eligible for high-priority pack
sections. Ambiguity here propagates to every section of the system.

## Decision

The README five-class set is the canonical taxonomy for v1. The
classes, their sources, and their initial confidences are pinned exactly
as published; later changes require a new ADR.

| Class             | Source                                                | Initial confidence |
| ----------------- | ----------------------------------------------------- | ------------------ |
| `human_explicit`  | A human invoked `ee remember` directly                | 0.85               |
| `agent_validated` | Agent assertion + at least one validated outcome      | 0.65               |
| `agent_assertion` | Agent assertion, no outcome events yet                | 0.50               |
| `cass_evidence`   | Imported session span from `cass`                     | 0.45               |
| `legacy_import`   | Imported from a pre-v1 Eidetic Engine artifact        | 0.30               |

Extra classes from the older long plan map onto the canonical set via
an orthogonal qualifier `trust_subclass: Option<String>` (free-form,
project-tunable, not enum-typed). The qualifier is informational only;
no scoring or routing decision keys off it in v1. Subclasses do not
unlock different promotion paths.

Trust class is exposed as a stable field `trust_class` on every memory
in the public schema `ee.memory.v1`. The qualifier appears as
`trust_subclass`, always present (may be null) so consumers can rely on
its shape. Both fields are versioned through the existing `ee.response.v1`
envelope.

### Transition triggers

Exactly four trigger kinds drive class transitions. Each has a
deterministic delta against the memory's `confidence`, `utility`, and
`trust_class`:

| Trigger                  | Confidence delta | Utility delta | Class effect                                                                                          |
| ------------------------ | ---------------- | ------------- | ----------------------------------------------------------------------------------------------------- |
| `outcome_helpful`        | +0.04            | +0.08         | None directly. Counts toward `agent_assertion` → `agent_validated` promotion when validation also fires. |
| `outcome_harmful`        | −0.10            | −0.12         | After two harmful events from distinct sessions or sources within the decay window (and no intervening helpful from those same sources), the rule inverts to an anti-pattern via the curation review queue (never silently). |
| `validation_passed`      | +0.06            | +0.04         | `agent_assertion` → `agent_validated` when paired with at least one prior `outcome_helpful` and no outstanding `outcome_harmful` within the decay window. |
| `validation_contradicted`| −0.08            | −0.05         | If the memory is `agent_validated`, demote back to `agent_assertion`. If `human_explicit`, leave class unchanged but emit a `degraded` advisory.    |

`outcome_harmful` is weighted four times as heavily as `outcome_helpful`
for utility composition (matches the README harmful_weight default of
2.5 against the helpful weight of ~1.0, with the additional asymmetry
applied at scoring time, not at storage time). The numeric deltas
above are storage-time mutations to the per-memory record; the runtime
scoring formula in `docs/scoring.md` (when written) applies further
weighting.

### Promotion is never silent

Promotion to `agent_validated` is **not** automatic in v1. When the
necessary triggers fire, the steward enqueues a curation candidate
(state = `proposed_promotion`) which the human accepts via
`ee curate apply <id>`. Auto-promote stays disabled by default behind
config flag `curation.auto_promote = false`. EE-CURATE-TTL-001 owns the
candidate lifecycle around this flag.

`human_explicit` is the only class a single command can produce
(`ee remember`); all other classes require an evidenced source.

### Audit shape per transition

Every transition produces one append-only entry in the existing
`audit_log` table with the following minimum fields:

| Field             | Source                                                       |
| ----------------- | ------------------------------------------------------------ |
| `event_kind`      | `trust_transition`                                           |
| `memory_id`       | The affected memory's public id                              |
| `prior_class`     | One of the canonical five classes                            |
| `new_class`       | One of the canonical five classes                            |
| `trigger`         | One of the four trigger kinds                                |
| `evidence_uri`    | Provenance pointer for the trigger event                     |
| `confidence_before` | Pre-transition value                                       |
| `confidence_after`  | Post-transition value                                      |
| `utility_before`    | Pre-transition value                                       |
| `utility_after`     | Post-transition value                                      |
| `actor`           | Source identity (human user, agent name, or `cass:<session>`) |
| `protected`       | Boolean — see EE-FEEDBACK-RATE-001 protected-rule guard      |

No transition silently mutates a memory: the pair of (prior, new)
class names is always written, even when only confidence changes within
the same class.

### Cross-cutting boundaries

- **Trust class is not redaction class.** Redaction class
  (`Public | Project | Private | Secret | Blocked`) is orthogonal and
  decides storage and rendering; trust class decides scoring and
  packing priority. They never collapse into one field.
- **Trust class is not maturity.** Maturity is a separate scalar
  (access count, decay window) maintained by the curation pipeline.
- **Trust class affects packing quotas, not retrieval inclusion.** A
  low-trust memory can still match a query; it just lands in a
  lower-priority pack section.

## Consequences

- EE-260 (trust-class enum and memory fields) becomes unambiguous to
  implement. The enum has exactly five variants in this order:
  `HumanExplicit`, `AgentValidated`, `AgentAssertion`, `CassEvidence`,
  `LegacyImport`. Reordering breaks the schema-drift gate
  (EE-SCHEMA-DRIFT-001) and is forbidden.
- EE-CURATE-SPEC-001 (specificity validation) and EE-CURATE-TTL-001
  (candidate TTL) gain a stable contract: "promotion to
  `agent_validated` requires (helpful, validation_passed) plus
  specificity above threshold plus no harmful within decay window."
- EE-FEEDBACK-RATE-001 (harmful-feedback rate-limit and protected
  rules) gains a stable contract for the inversion threshold: "two
  distinct sources, decay window, no intervening helpful." The
  protected-rule guard layers on top of these rules.
- The README and OPUS plan §22 cross-link to this ADR. Future drift
  between either document and this ADR is a doc bug, not a design
  question; this ADR wins.
- The richer taxonomy from the older long plan is preserved as
  free-form subclass metadata. No information is lost; the canonical
  scoring path simply does not key off it.

## Rejected Alternatives

- **Free-text trust labels.** Forces every consumer to do its own
  classification. Causes silent divergence as soon as two implementing
  agents pick slightly different label normalisation rules.
- **Unifying trust class and redaction class into one field.** They
  serve different purposes (scoring vs. storage gates) and need
  independent lifecycles (a memory can become more trusted without
  becoming less private, and vice versa).
- **Adopting the older long plan's nine-class taxonomy verbatim.** The
  five published classes are already user-visible in the README;
  changing them after publication is a breaking change. The richer
  long-plan classes are recoverable as subclasses without breaking the
  published surface.
- **Automatic LLM-driven classification in v1.** Open question §29.1
  in the OPUS plan defers this. A later ADR can layer it on once the
  evaluation harness exists, but v1 stays deterministic.
- **Promoting on a single helpful outcome.** Single-outcome promotion
  is too easy to weaponise (an agent confirming its own output) and
  too easy to misroute. The dual-trigger requirement (helpful +
  validation_passed) preserves the "evidence over vibes" promise.

## Verification

- A pinned fixture at `tests/contracts/trust_class.json` enumerates
  every class, the initial confidence, the promotion source, and the
  audit shape. EE-SCHEMA-DRIFT-001 will pull this fixture into the
  schema-drift gate so a change to the variant order or initial
  confidences fails CI with a readable diff.
- A property test asserts that `outcome_harmful` followed by
  `outcome_helpful` from the same source within the decay window does
  not net-promote the memory; the harmful event always wins on the
  utility axis.
- A property test asserts that a `human_explicit` memory cannot be
  silently downgraded; it can only be flagged as `degraded` when
  contradicted, not class-demoted by automatic transitions.
- An integration test seeds the four trigger kinds in known order and
  asserts the audit log records the (prior, new) pair for every
  transition, including no-op transitions where only confidence shifts.
- The README trust class table and `COMPREHENSIVE_PLAN_TO_MAKE_EE__OPUS.md`
  §22 link to this ADR. CI does not enforce the back-link yet (no
  bead), but the ADR's `status: accepted` line is the canonical
  reference.
- EE-260, EE-CURATE-SPEC-001, EE-CURATE-TTL-001, and
  EE-FEEDBACK-RATE-001 cite this ADR in their close-out evidence.
