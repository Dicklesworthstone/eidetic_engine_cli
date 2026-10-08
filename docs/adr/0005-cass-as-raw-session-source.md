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

## Screening upgrades and retained history (2026-10-08, issues #65 and #66)

A screened excerpt is a representation of source evidence. Its content hash
authenticates that representation, not the bytes removed by screening. New CASS
imports therefore compute a BLAKE3 commitment and byte length over the complete
`cass view` line before decoding, redaction, or bounding. The storage boundary
retains these fixed-shape values and the excerpt-producer version in the optional
`cassSource` member of canonical evidence security metadata. It validates the
producer, schema, field types, digest and bounds, and copies no raw source or
source path. Existing 13-field security metadata remains valid.

Refresh still requires the same session, source locator, line range, kind, and
role, and verifies the retained excerpt's own hash. If an import-time source
commitment exists, the new complete source must match it even when the screened
excerpts are identical. This detects edits hidden by credential redaction or
text truncation. A matching source may have a different screened representation;
refresh preserves the original evidence identity, excerpt, security posture,
links, timestamps, and provenance. It adds a stable
`cass.evidence.screening_reconciled` audit naming both representation hashes and
the actual basis of the source proof. This audit, added evidence, session
checkpoint and publication job commit atomically. Repeated imports reuse the
same audit and pending publication work.

v0.17.0 did not retain complete-source commitments for ordinary rows. A clean
excerpt can prove the complete retained source only when it cannot be an old
truncation or normalization result. The old byte cap and its three-byte UTF-8
retreat interval are excluded. Canonically serialized JSON objects and
newline-joined object streams are also excluded: v0.17.0 could compress an
oversized object containing insignificant whitespace or escaped characters
into a short canonical object without adding a truncation marker; later
producers on main could do the same for JSONL windows. A source that was
originally canonical has the same stored representation and cannot be
distinguished after the fact. Malformed
or noncanonical JSON and ordinary text below the ambiguous byte interval can
still provide a complete-source proof when clean and unmarked. A strict
importer-generated withholding envelope with its matching redaction class can
also retain a complete-source digest. These are the only legacy reconciliation
exceptions. The audit distinguishes these recovered proofs from an import-time
commitment; neither is written back into historical provenance.

Old redacted excerpts and truncated prefixes cannot prove discarded bytes.
Replaying the old screener, comparing normalized JSON, trusting a session hash
derived from path/mtime/size, or installing today's digest as an old commitment
would conceal this gap. Such sessions are explicitly refused with
`cass_refresh_history_unverifiable`. Confirmed source edits or missing retained
lines use `cass_refresh_history_changed` and `cass_refresh_history_missing`.
No evidence, session checkpoint, publication job or reconciliation audit is
written for a refused session.

These three history refusals do not prevent later sessions from importing.
The existing report schema adds the per-session status `refused`, with
`refusalCode` and `repair` present only for refused sessions. The run reports
`completed_with_refusals` and a `cass_import_history_refused` degradation;
`sessionsSkipped` includes those refusals. The import ledger remains `failed`
with the first refusal and records the successfully committed counts. Other
storage failures remain fatal. Review original source backups before retrying
unverifiable history; `--since` can scope subsequent work to newer sessions.

The malformed encoded-JSON withholding envelope carries an explicit
`[REDACTED:<reason>]` marker alongside its reason and source digest. This satisfies
the inherited-class storage contract without restoring any rejected content.

Verification lives in `policy::ingestion::store_tests`, the CASS import and
refresh library tests, and `db::tests::cass_source_commitment_*`: real database
and subprocess imports cover old/new representations, subsequent sessions,
hidden source edits, duplicate-free retries, privacy and transaction rollback.

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

