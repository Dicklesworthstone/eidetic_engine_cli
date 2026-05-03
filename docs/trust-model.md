# Trust Model

This document describes how EE keeps useful memories available while preventing
low-trust, stale, legacy, or instruction-like content from becoming authoritative
context for an agent.

The canonical trust taxonomy is ADR 0009. The canonical context-pack UX contract
is ADR 0007. This page connects those decisions to the lifecycle that `ee
context`, `ee why`, curation, imports, and maintenance jobs must preserve.

## Lifecycle

Memories pass through five integration stages:

| Stage | Owner | Durable effect | Public check |
| --- | --- | --- | --- |
| Capture | `ee remember`, `ee import cass`, `ee import eidetic-legacy --dry-run` | Creates or previews memory evidence with provenance. | `ee.response.v1` or `ee.error.v1` |
| Classify | redaction, trust class, source metadata | Assigns `trust_class`, `trust_subclass`, confidence, utility, and redaction posture. | `ee.memory.v1` fields |
| Retrieve | `ee search`, `ee context` | Selects candidates without hiding degraded capabilities. | score components and `degraded[]` |
| Pack | `ee context` | Emits compact context with provenance, trust signals, and advisory notes. | pack hash and provenance footer |
| Learn | `ee outcome`, `ee curate candidates`, steward jobs | Proposes promotion, demotion, supersession, or quarantine. | audit log plus `ee why <memory-id>` |

No lifecycle step silently upgrades, deletes, consolidates, or tombstones a
memory. Mutations are explicit, audited, and explainable through `ee why` or the
curation surfaces that own the change.

## Trust Classes

Every memory has one trust class. The class affects packing priority and default
confidence; it does not replace redaction policy, maturity, or provenance.

| Class | Source | Initial confidence | Pack posture |
| --- | --- | ---: | --- |
| `human_explicit` | A human invoked `ee remember` directly. | 0.85 | Eligible for high-priority advice unless redaction blocks it. |
| `agent_validated` | Agent assertion with outcome evidence and validation. | 0.65 | Eligible for advice, with provenance and validation notes. |
| `agent_assertion` | Agent assertion without validated outcome evidence. | 0.50 | Useful but advisory; never presented as settled policy. |
| `cass_evidence` | Imported span from `coding_agent_session_search`. | 0.45 | Evidence first; promoted only through review and validation. |
| `legacy_import` | Pre-v1 Eidetic Engine artifact. | 0.30 | Low-priority historical evidence until validated. |

`trust_subclass` is optional metadata. It may describe local project categories,
but v1 scoring and routing must not key off it.

## Advisory Priority

Context packs should separate "what to do" from "why this is safe to trust".
When a selected memory is low-trust, contradicted, stale, imported, redacted, or
backed by a degraded source, the pack must surface that posture alongside the
memory instead of making the reader infer it.

Advisories are ordered by severity:

1. Blocked: secret-bearing, policy-denied, or prompt-injection-like content.
2. Quarantined: suspicious instruction text, contradicted evidence, or failed
   validation.
3. Degraded: missing CASS, stale graph/index data, semantic search disabled, or
   legacy schema uncertainty.
4. Advisory: low trust class, limited evidence count, or old evidence.
5. Clear: high-confidence memory with current provenance and no policy flags.

Blocked content must not enter a context pack. Quarantined content can appear
only as a warning or curation candidate, not as executable advice. Degraded and
advisory content can appear when useful, but the pack must include the stable
code, provenance, and next action where one exists.

## Prompt-Injection Defense

Imported sessions and legacy artifacts can contain text that looks like
instructions to the current agent. EE treats those strings as evidence, not as
authority. The trust pipeline must flag:

- role override attempts
- requests to ignore system or project instructions
- credential exfiltration cues
- destructive filesystem or Git commands presented as instructions
- unverifiable release, migration, or security claims

Flagged memories stay out of high-priority procedural sections until reviewed.
If useful context is still needed, the pack should quote or summarize the
evidence as untrusted historical material and keep the current action advice in
a separate trusted section.

## Lifecycle States

Procedural rules use maturity states to keep advice from becoming permanent too
early:

| State | Meaning |
| --- | --- |
| `draft` | Newly captured or imported, not reviewed. |
| `candidate` | Proposed for validation or promotion. |
| `validated` | Supported by positive outcomes and review. |
| `deprecated` | Retained for history but no longer recommended. |
| `superseded` | Replaced by a newer rule or procedure. |

Rule lifecycle transitions are planned by a deterministic evaluator before any
durable mutation occurs. Supported triggers are `propose_validation`,
`outcome_helpful`, `outcome_harmful`, `validation_passed`,
`validation_contradicted`, `review_approved`, `deprecate`, and `supersede`.
The evaluator emits one action: `retain`, `promote`, `demote`, `deprecate`,
`supersede`, or `reject`.

A candidate-to-validated transition requires helpful outcome evidence,
validation evidence, and explicit review. Helpful outcomes adjust confidence and
utility with no silent promotion. Harmful outcomes require a distinct-source
quorum before deprecation is proposed for curation. Supersession requires a
non-empty replacement rule id, and terminal states reject non-supersession
triggers. Every transition plan carries an audit requirement, score deltas, and
a reason string so curation can persist or reject it explicitly.

Operational lifecycle certificates cover durable maintenance events:
`import`, `index_publish`, `hook_execution`, `backup`, `shutdown`, `migration`,
and `maintenance`. These certificates prove that long-running lifecycle events
completed or failed with an idempotency key, duration, item count, and error
posture where applicable.

## Local Signing Key Policy

High-trust procedural memory needs an extra local proof before it becomes
authoritative advice. A validated procedural memory in `human_explicit` or
`agent_validated` trust must carry a local signature; otherwise the policy emits
`local_signing_key_required` and the memory stays out of authoritative
procedural sections until signed.

Draft or candidate high-trust procedural memories emit
`local_signing_key_recommended` so curation can attach a signature before
promotion. Non-procedural memories, lower-trust memories, deprecated memories,
and superseded memories emit `local_signing_key_not_required`. A signed
high-trust procedural memory emits `local_signing_key_satisfied`.

Machine consumers use `ee.local_signing_key_policy.v1` and the stable posture
values `not_required`, `recommended`, `required`, and `satisfied`. The policy is
deterministic and does not generate keys, mutate memories, or silently promote
trust; it only reports whether local signing is required for authoritative use.

## Command Integration

- `ee context "<task>" --json` must include selected memories, provenance,
  degraded capabilities, stable ordering, and a reproducible pack hash.
- `ee why <memory-id> --json` must explain storage source, retrieval score,
  trust class, confidence, utility, pack selection, and audit history.
- `ee outcome <id> --signal helpful|harmful --json` changes evidence and
  utility, not trust class by itself.
- `ee curate candidates --json` is where promotion, demotion, quarantine,
  supersession, and consolidation become explicit review items.
- `ee import cass --dry-run --json` and `ee import eidetic-legacy --source
  <path> --dry-run` must preview trust and redaction posture before durable
  import.

## Test Contract

The lifecycle documentation is part of the public contract. The
`lifecycle_docs` integration test checks that this page and the README stay
linked to the exported trust classes, rule maturity states, lifecycle events,
and machine-output schemas. If a future change renames a trust class, adds a
lifecycle event, or changes the README documentation index, the test should fail
with a direct message instead of letting documentation drift.
