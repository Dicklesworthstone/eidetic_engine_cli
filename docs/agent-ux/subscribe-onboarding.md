# `ee subscribe` Onboarding

`ee subscribe` lets agent harnesses replace repeated memory searches with a
cursor-based memory delta feed. The v1 backend reads the durable audit table and
uses the SQLite rowid as the monotonic cursor until the audit MPSC lane lands.

## One-shot Poll

```bash
ee subscribe poll --cursor 0 --filter LEVEL=procedural,TAG=release --json
```

The response is an `ee.response.v2` envelope. Consume **both** `data.deltas` and
`data.invalidations` before persisting `data.nextCursor`, then pass that cursor
back as `--cursor`. A successful page advances across inspected audit rows even
when filters exclude them. Follow `data.hasMore` even when the page contains no
matching deltas. A failed page acknowledges nothing; retry the previous cursor.
Keep cursors bound to the same database, workspace, and filter.

`invalidations` are identity-only cache eviction notices, not authorization to
read a body or evidence of prior filter membership. Reconcile those identities
against current authoritative state. They include filter exits, immutable
revision predecessors, and the two endpoints of a visible `memory.link.create`
event (`reason: memory_link_changed`, `changedFields: ["links"]`). Link endpoint
notices share one audit cursor and are never split across poll pages, including
with `--limit 1`. The limit counts inspected audit events, not expanded notices.

Link endpoints are resolved in bounded batches from the durable relationship
and memory tables in the same source snapshot as the audit. Both endpoints must
belong to the requested workspace, and graph visibility policy still applies.
The notices contain no body, tags, actor, edge annotations, or relationship
labels. Missing link authority fails the page rather than inventing endpoints
from free-form audit details. Explicit time, workspace, and changed-field
routing applies; current level/tag/trust filters do not silently suppress a
cache eviction whose earlier membership is unknown.

Read-only audits such as `memory.show` do not create change notifications or
consume change-page slots. Their audit records and watermark progress remain
intact, preventing fetch-on-notification loops without losing audit history.

## Foreground Stream

```bash
ee subscribe stream --cursor 0 --filter LEVEL=procedural,TAG=release --json
```

The current foreground stream emits ordinary `ee.memory.delta.v1` objects only;
it does **not** forward the poll response's invalidations. Use the poll API above
for complete cache reconciliation, including revisions, filter exits, and graph
changes. Harness tests can use `--max-events N` to bound ordinary stream output.

## Filter Keys

- `LEVEL=working|episodic|semantic|procedural`
- `KIND=rule|fact|decision|failure|...`
- `TAG=release+ci`
- `TAG_MODE=all|any`
- `WORKSPACE_ID=wsp_...`
- `TRUST_CLASS=agent_assertion`
- `CHANGED_FIELDS=level+tags` (all listed fields must match)
- `CHANGED_FIELDS=links` (graph endpoint invalidations)
- `SINCE_MS=60000`

Filters are advisory and local-only. They do not create a queue or reserve
events for a subscriber; reconnecting agents reconcile by polling from their
last persisted cursor.
