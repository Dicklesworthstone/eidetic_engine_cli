# ADR 0005: CASS Is The Raw Session Source

Status: accepted
Date: 2026-04-29

## Context

Coding-agent session history already exists in `coding_agent_session_search`
(`cass`). `ee` should learn from that history without duplicating its raw store
or binding to unstable internal database details.

## Decision

`ee` consumes CASS through stable robot/JSON commands. CASS remains the raw
session source. `ee` imports evidence spans, provenance, candidates, and derived
memories, but it does not duplicate the raw session store or depend on bare
interactive CASS output.

## Consequences

Session ingestion is adapter-driven and testable. Imported memory can point back
to exact session provenance. If CASS is unavailable, explicit `ee remember`,
`ee search`, and `ee context` workflows continue in degraded mode.

The CASS adapter must preserve budgets, cancellation, schema-version checks, and
subprocess cleanup.

## Rejected Alternatives

- Directly reading CASS internals without a stable robot contract.
- Maintaining a second raw session database inside `ee`.
- Treating CASS as mandatory for all memory workflows.
- Parsing human-oriented CASS TUI output.

## Verification

- CASS contract fixtures pin `capabilities`, `search --robot`, `view --json`,
  and `expand --json` outputs.
- Unknown CASS schema versions return `external_adapter_schema_mismatch`.
- Degradation tests prove CASS absence is reported and explicit memories still
  work.

