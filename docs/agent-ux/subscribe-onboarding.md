# `ee subscribe` Onboarding

`ee subscribe` lets agent harnesses replace repeated memory searches with a
cursor-based memory delta feed. The v1 backend reads the durable audit table and
uses the SQLite rowid as the monotonic cursor until the audit MPSC lane lands.

## One-shot Poll

```bash
ee subscribe poll --cursor 0 --filter LEVEL=procedural,TAG=release --json
```

The response is an `ee.response.v1` envelope. Persist `data.nextCursor` after
every successful poll and pass it back as `--cursor` on the next call. The
command advances the cursor across inspected audit rows even when a filter
excludes them, so non-matching changes do not replay forever.

## Foreground Stream

```bash
ee subscribe stream --cursor 0 --filter LEVEL=procedural,TAG=release --json
```

The stream emits one `ee.memory.delta.v1` JSON object per line until the process
is interrupted. Harness tests can use `--max-events N` to bound the process.

## Filter Keys

- `LEVEL=working|episodic|semantic|procedural`
- `KIND=rule|fact|decision|failure|...`
- `TAG=release+ci`
- `TAG_MODE=all|any`
- `WORKSPACE_ID=wsp_...`
- `TRUST_CLASS=agent_assertion`
- `CHANGED_FIELDS=level+tags`
- `SINCE_MS=60000`

Filters are advisory and local-only. They do not create a queue or reserve
events for a subscriber; reconnecting agents reconcile by polling from their
last persisted cursor.
