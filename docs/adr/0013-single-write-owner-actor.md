# ADR 0013: Single-Write-Owner Actor

Status: accepted
Date: 2026-05-05

## Context

SQLite allows only one writer at a time. When multiple `ee` processes or threads
attempt concurrent writes, they contend for the write lock. This causes:

1. SQLITE_BUSY errors that surface as confusing storage failures.
2. Non-deterministic write ordering when multiple agents push memories.
3. Risk of partial writes if a process dies mid-transaction.

The franken-stack (FrankenSQLite via SQLModel) handles connection pooling and
WAL mode, but does not coordinate multiple processes attempting writes.

## Decision

**All durable writes flow through a single-writer actor.**

1. A `WriteOwner` actor (in `src/core/write_owner.rs`) holds exclusive write
   capability for the database.
2. Write requests are submitted to a channel; the actor processes them serially.
3. Read operations bypass the actor and go directly to the read pool.
4. CLI commands that write (`remember`, `import`, `curate apply`) acquire a
   write slot from the actor before proceeding.
5. If another writer is active, the command either waits (with timeout) or
   returns a `write_owner_busy` error with repair guidance.

## Consequences

- No SQLITE_BUSY races between concurrent `ee` invocations.
- Write ordering is deterministic: first-in-first-out through the channel.
- Agents can check write availability before committing to a mutation.
- Long-running imports hold the write slot; other commands see backpressure.
- The actor is optional in single-threaded CLI mode but mandatory for daemon/serve.

## Rejected Alternatives

- **Rely on SQLite WAL alone**: WAL improves concurrency but does not eliminate
  writer contention across processes.
- **Distributed lock file**: Adds filesystem state outside the database; harder
  to reason about and recover from.
- **Optimistic locking with retries**: Non-deterministic; agents cannot predict
  which write wins.

## Verification

- `tests/contracts/single_writer_stress.rs`: Two competing writers; one waits,
  no BUSY errors.
- `ee diag locks --json`: Reports current write owner, queue depth, wait time.
- CLI integration tests verify `write_owner_busy` error shape and exit code.
