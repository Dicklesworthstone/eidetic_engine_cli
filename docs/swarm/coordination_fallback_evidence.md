# Coordination Fallback Evidence

`ee.coordination_fallback_evidence.v1` is the planned record shape for
`bd-1zb7k.13.2`. It captures coordination-substrate incidents that would
otherwise survive only in chat: Agent Mail unavailable, Beads stale, file
reservation uncertainty, or RCH remote verification blocked before Cargo can
run.

The record is evidence, not a repair action. It says which substrate was
`available`, `unavailable`, `stale`, `blocked`, or `unknown`, gives a stable
reason code, stores a redacted summary plus content hash, and links optional
Beads, verification, and support-bundle IDs.

## Redaction

Records must store summaries and hashes, not raw inboxes, raw logs, or full
local paths. The `redaction` block explicitly declares whether raw inbox/log
content is present, whether secret scanning was applied, and how paths were
handled. The intended default is labels and counts only.

## Intended Surfaces

- `ee coordination evidence ingest --file/--stdin --json` or an equivalent
  agreed CLI should ingest records idempotently by `summary.contentHash`.
- `ee why` reports redaction-safe linked coordination evidence. The collector
  reads `.ee/coordination-fallback-evidence.jsonl` and links records by memory
  workflow/bead IDs or by verification IDs already linked to the memory.
- Support bundles include `coordination_fallback_summary.json`, a redacted
  summary of the local `.ee/coordination-fallback-evidence.jsonl` ledger. It
  carries status/source counts, reason codes, evidence IDs, linked IDs, and
  content hashes only; it does not include raw inboxes, raw logs, full paths, or
  fallback summary text.

## Fixture

The canonical example is duplicated in
`tests/fixtures/swarm/coordination_fallback_evidence.json` and
`tests/fixtures/swarm_schemas/all_examples.json`. Keep both in lockstep with
the first schema example until the ingest command has executable fixtures.

## Non-goals

- Do not replace Agent Mail, Beads, RCH, or the agent harness.
- Do not send messages, claim Beads, reserve files, run Cargo, or repair
  workers during ingest.
- Do not store raw inbox bodies, secrets, or unredacted local filesystem paths.
- Do not treat fallback evidence as successful coordination or successful
  verification.
