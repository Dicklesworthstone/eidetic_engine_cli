# Durable memory subscriptions

`ee --workspace . subscribe poll --cursor 0 --filter TAG=release --json`
reads committed memory changes without creating a store, migrating its schema,
or queuing work. A database override must contain the requested workspace
binding. A workspace filter only narrows that binding; it never grants access
to another workspace in a shared database.

## Consume a complete page before saving its cursor

The `ee.subscribe.poll.v1` response includes two ordered arrays:

- `deltas`: ordinary `ee.memory.delta.v1` events whose current metadata matches
  the filter. These remain bodyless notifications, not permission to consume a
  memory or a reconstruction of its historical state.
- `invalidations`: `ee.memory.invalidation.v1` identity-only notices for
  non-creation events that do not match current membership filters but could
  invalidate something the consumer previously retained. They include the
  memory/workspace/audit IDs, event cursor and timestamp, changed fields,
  affected filter dimensions, and `reason=prior_filter_membership_unknown`.
  They do not disclose the actor, tags, level, kind, trust, or source body.

Process both arrays before acknowledging `nextCursor`. Evict any cached entry
named by an invalidation and refetch through the normal scoped, lifecycle- and
trust-admitted read path when needed. The two arrays share one audit ordering;
merge by `cursor` when processing individual events chronologically. Persist
`nextCursor` even when both arrays are empty.

`hasMore=true` means the bounded raw page has a continuation, even when every
row was excluded by the filter. Continue from `nextCursor` rather than treating
`deltaCount=0` as end-of-stream. The lookahead event is not acknowledged until
the next page. When `hasMore=false`, `nextCursor` advances through the proven
empty tail up to `highWatermark`, including unrelated audit actions in that
workspace. Another workspace's audit traffic does not set this watermark.
New commits after the pinned snapshot remain visible to the next poll.

## Why invalidations are separate

A tag removal can make a memory stop matching `TAG=release`; a trust downgrade
can make it stop matching `TRUST_CLASS=human_explicit`. Matching only the new
state would silently leave old cached entries behind. Audit history does not
supply a complete old combination of tags, kind, level, and trust, so the
subscription does not fabricate one. Instead, a non-creation event outside
those membership filters produces a conservative invalidation. Overnotification
is possible and intentional. An unrelated creation is still excluded.

Workspace, timestamp, and changed-field routing constraints apply to both
arrays. Invalidations do not claim prior filter membership, grant trust, or
bypass source admission. A tombstone can invalidate an identity even when its
current memory row or filter metadata is unavailable. Consumers needing all
revocations should not restrict `CHANGED_FIELDS` to unrelated fields or exclude
relevant events with a moving `SINCE_MS` cutoff.

## Recovery and limits

The binding, watermark, audit rows, and live metadata are read in one owned
snapshot. A read failure returns no acknowledged cursor. Negative or
unrepresentable `SINCE_MS` values fail with `subscribe_filter_invalid`; they do
not silently disable the time filter.

Reuse a numeric cursor only with the same store, workspace, and filter. An
ahead-of-store cursor emits `subscribe_cursor_stale`: resynchronize from
current authoritative state before accepting its reset `nextCursor`, or start
at zero to replay retained audit history. Numeric v1 cursors do not detect every
store replacement or rollback, especially when replacement rowids have already
caught up. This interface is an audit-backed invalidation feed, not exactly-once
delivery, a complete historical event store, or a substitute for a recovery
checkpoint that binds a store generation.
