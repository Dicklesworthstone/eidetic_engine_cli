# ADR 0006: Procedural Memory Requires Evidence

Status: accepted
Date: 2026-04-29

## Context

Procedural memories and rules are powerful because they change future agent
behavior. Low-quality rules, unsupported assertions, stale advice, or
prompt-injection-like content can pollute context packs and cause repeated
mistakes.

## Decision

Procedural memory requires evidence. New rules start with bounded confidence
unless backed by human-explicit input, source sessions, outcomes, validation, or
other provenance. Promotion, consolidation, retirement, and tombstoning are
audited. Instruction-like or secret-bearing evidence is quarantined or rendered
as evidence, not authoritative advice.

## Consequences

Rules become trustworthy enough to surface in high-priority pack sections.
Harmful feedback can demote or invert bad rules into anti-patterns without
silently deleting historical evidence.

The system may propose procedural rules, but durable promotion needs explicit
curation, validation, or a future audited apply path.

## Rejected Alternatives

- Promoting agent assertions directly to high-confidence rules.
- Treating repeated text as proof without provenance.
- Silent mutation of procedural memory by a background steward.
- Deleting harmful rules instead of preserving audit context.

## Verification

- Curation tests cover specificity, duplication, evidence requirements,
  redaction, and prompt-injection flags.
- Outcome tests prove helpful and harmful feedback update confidence
  asymmetrically.
- Context pack golden tests prove low-confidence or quarantined rules do not
  render as authoritative procedure.

