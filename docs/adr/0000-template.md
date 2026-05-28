# ADR NNNN: Title

<!--
Required header block (bd-2kud8). One field per line, in this order, so
harnesses that scrape ADRs for status/date/bead linkage see a uniform shape:
  Status:  one of — proposed | accepted | superseded | deprecated |
           Deferred (research backlog)   (the canonical defer vocabulary)
  Date:    YYYY-MM-DD
  Bead:    bd-XXXXX (optional human tag) — the tracker bead; omit only for
           pre-bead ADRs
  Supersedes: ADR NNNN — optional; omit if none
-->

Status: proposed
Date: YYYY-MM-DD
Bead: bd-XXXXX

## Context

What forces made this decision necessary?

## Decision

What are we doing?

## Consequences

What becomes easier, harder, or intentionally impossible?

## Rejected Alternatives

What did we consider and reject?

## Verification

How will tests, diagnostics, or review prove the decision remains true?

<!--
For research-defer ADRs, set `Status: Deferred (research backlog)` and add a
`### Re-open Criteria` section (bd-a43a0 / bd-21lyg) listing the conditions
that re-open the tracker bead. State explicitly whether the conditions are
ALL-of (conjunction) or ANY-of (disjunction).
-->

### Re-open Criteria

(Defer-ADRs only.) Conditions that re-open the tracker bead. State ALL-of vs
ANY-of explicitly.

