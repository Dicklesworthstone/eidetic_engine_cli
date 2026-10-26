# Withdraw a session's retained evidence

To stop using all currently retained CASS evidence from one session, select
its canonical `sess_...` ID. A session ID is not a transcript filename or a
CASS upstream reference.

```bash
ee evidence withdraw --session <session-id> --workspace . \
  --reason "These session observations are no longer suitable for retrieval" \
  --dry-run --json

ee evidence withdraw --session <session-id> --workspace . \
  --reason "These session observations are no longer suitable for retrieval" \
  --json
```

Use either `--session` or the existing positional evidence ID, never both.
The reason remains required. `--database <path>` selects an existing custom
store; `--actor <name>` records the operator. JSON, JSONL, compact, TOON, human
and Markdown renderers use the same operation. The JSON payload is
[`ee.evidence.withdraw_session.v1`](schemas/ee.evidence.withdraw_session.v1.json)
inside the ordinary response envelope, with field presets including `minimal`.

## Atomic selection and receipts

The operation selects the session and its retained evidence in one addressed
workspace snapshot. It validates all selected CASS security metadata before
changing any row, sets both retrieval eligibility columns and their canonical
metadata to `denied`, and commits one state-bound audit plus one session reindex
job. A source, audit or job failure aborts the transaction, not just that row.
Known non-CASS producers are excluded and counted rather than being silently
withdrawn with the imported conversation.

The receipt distinguishes `retainedEvidenceCount`, `cassEvidenceCount`,
`excludedProducerCount`, `alreadyWithdrawnCount`, `wouldWithdrawCount` and
`withdrawnCount`. The last count is zero in a preview or no-op. A successful
commit reports `withdrawn`, `persisted: true`, `changed: true` and
`indexStatus: queued`. Queuing does not claim that index publication finished.
Live admission excludes the denied sources before the existing index worker
publishes their removal.

Preview opens the existing store read-only and reports `would_withdraw` without
writing sources, audit rows or jobs. Repeating the operation after all selected
CASS rows are denied reports `already_withdrawn` with no new audit or job.
An empty session or a session containing only other producers reports
`no_cass_sources`. Both no-op cases use `indexStatus: not_scheduled`, which says
nothing about an earlier job's progress. Missing stores, pending migrations,
foreign workspace ownership and inconsistent source metadata are refused;
the command does not initialize or migrate a store.

## Deliberately limited scope

This withdraws **currently retained CASS sources**, including retained derived
incident cards. It does not erase transcripts or alter historical packs,
backups, independently curated memories/rules, or other producers. The audit
contains redacted reason/actor information, counts and before/after state
commitments; the response contains no transcript or reason text.

It is **not a persistent ban on future imports**. Existing denials survive the
normal CASS refresh path, but newly appended evidence can still be imported
under its normal admission policy. The receipt states `scope: retained_sources`
and `futureImports: unchanged`. Running withdrawal again after additional
sources arrive selects those new sources and leaves already-denied rows intact.

The core API is `withdraw_cass_session_evidence`; it shares the addressed-store,
read-only preview, transaction and metadata-validation boundaries with
single-source withdrawal. Regression tests cover database rollback, mixed
producers, source preservation, previews, repeats and complete CLI dispatch.
