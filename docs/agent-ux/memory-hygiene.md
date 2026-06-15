# Memory Hygiene

Memory hygiene is the content-health workflow for a long-lived `ee` store. It
does not replace `ee doctor`: doctor checks environment posture, while hygiene
checks whether the stored memories are still useful, trusted, and answering the
questions agents actually ask.

Use it on a weekly cadence, after a large import, or before relying on an old
workspace for a high-risk task.

## Weekly Workflow

```bash
ee curate doctor --workspace . --limit 5 --trend --json
ee learn gaps --workspace . --limit 10 --json
```

First read the `curate doctor` queue. Each item is read-only and carries a
suggested action that routes through an existing audited surface such as
`ee outcome`, `ee curate disposition`, `ee recall`, or conflict resolution.
Execute only the actions you have reviewed.

Then read `learn gaps`. A gap is demand-driven: agents asked a question that
search or ask could not answer confidently. Each cluster includes redacted
representative queries, nearest sub-threshold evidence, and a
`rememberTemplate` with suggested level, kind, tags, and a content skeleton.
Capture the missing knowledge with `ee remember` only when you can supply a real
source or durable lesson.

Finally rerun `ee curate doctor --trend --json` after steward snapshots exist.
Improving trends show that the queue is shrinking after audited repairs; flat or
worsening trends mean the store is accumulating more debt than the hygiene loop
is removing.

## Debt Classes

| Class | What it means | Typical repair |
| --- | --- | --- |
| `stale_anchor` | An anchored memory points at code or docs whose freshness is suspect or stale. | Review with recall or rebuild the relevant index before relying on it. |
| `contradicted_unresolved` | A conflict pair has aged without an explicit resolution. | Explain and resolve the conflict through the audited conflict/curation surface. |
| `never_retrieved` | An old memory has no recent search or pack inclusion evidence. | Review with `ee curate disposition`; keep, snooze, or tombstone deliberately. |
| `orphan` | A low-utility memory has no links and no recent retrievals. | Create a real link/outcome if it matters, otherwise disposition it. |
| `low_trust_high_rank` | A low-trust memory keeps winning packs without outcome validation. | Grade it with `ee outcome` or add corroborating evidence. |
| `decay_imminent_high_utility` | A useful memory is close to demotion or tombstone under decay settings. | Review decay half-lives before the memory loses prominence. |

The queue ordering is deterministic: severity first, then impact proxy, then
memory id. Report rows are evidence, not mutations.

## Boundary Table

| Surface | Evidence stream | Use it for |
| --- | --- | --- |
| `ee doctor` | Environment, DB, index, and capability checks | Fixing local readiness problems. |
| `ee curate doctor` | Persisted content, links, feedback, audit rows, and decay projections | Prioritizing memory-debt repairs. |
| `ee insights --section knowledgeGaps` | Graph-structural gaps | Finding weak topology or missing graph support. |
| `ee learn gaps` | Query-miss and ask-abstention demand | Capturing knowledge agents keep asking for. |
| `ee learn agenda` | Learning and experiment ledgers | Planning experiments or uncertainty reduction. |

## Retention And Privacy

`ee learn gaps` reads retained miss demand under
`[search].query_miss_retention_days`, mirrored by
`EE_QUERY_MISS_RETENTION_DAYS`. The default window is 30 days so weekly reviews
do not lose the signal. The retained payload is still hash-only/redacted demand;
raw query text is not promoted into the report.

`learn_gaps_no_miss_data` is an honest empty result. It means no retained demand
was available in the window, not that the workspace has no missing knowledge.
`learn_gaps_retention_short` means the requested `--since` is older than the
configured retention bound.

## Trend Snapshots

The steward job `memory-debt-snapshot` writes append-only `debt_snapshots` rows.
Run it after normal maintenance so the trend reflects post-maintenance content
health:

```bash
ee steward run --job memory_debt_snapshot --workspace . --json
ee curate doctor --workspace . --trend --json
```

Trend rows are durable telemetry. They should be preserved across migrations and
support bundles, subject to the normal redaction policy.
