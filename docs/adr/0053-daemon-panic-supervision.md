# ADR 0053: Daemon Panic Supervision Boundary

Status: Accepted
Date: 2026-06-02
Bead: bd-375u1

## Context

The Unix-domain socket daemon is still a narrow CLI-first acceleration surface:
one accepted connection maps to one framed daemon request, and the current
dispatch table only includes `ee.daemon.echo` and the `ee.daemon.context` stub.
The implementation intentionally avoids Tokio, HTTP stacks, and daemon-first
control flow.

The original skeleton spawned one thread per accepted connection and called
`dispatch` directly. A panic inside dispatch or a future warm-load handler would
kill only that worker thread, leave the accept loop running, and give the
client a torn-down connection instead of a response envelope. That behavior was
not acceptable for an agent-facing machine contract: clients need a stable
error code, operators need a bounded diagnostic signal, and future warm-load
methods need to know where panic boundaries live.

The daemon module still plans to move long-lived accept-loop supervision under
Asupersync once daemon context execution becomes substantial. That future
supervision tree will own process-level lifecycle, cancellation budgets, and
restart policy. It does not replace the need for a per-RPC panic boundary on
the wire protocol.

## Decision

Every daemon connection handler wraps the dispatch call in
`std::panic::catch_unwind` before writing the response frame. If dispatch or a
future method handler panics:

- the worker converts the panic into a structured `daemon_handler_panic`
  response envelope,
- the response keeps the original `request_id`, `agent_id`, and `workspace_id`,
- the wire message is fixed and generic, never the raw panic payload,
- the raw panic payload is sanitized to one line, capped, and logged only to
  daemon stderr,
- the accept loop remains alive and continues serving later connections.

This is the daemon's stable per-RPC supervision boundary until an Asupersync
region wraps the accept loop. When that refactor lands, the Asupersync region
may supervise accept-loop lifetime and worker cancellation, but it must preserve
the same wire-level `daemon_handler_panic` behavior for handler panics. A
supervisor restart without a framed response would regress the client contract.

The chosen degraded code is `daemon_handler_panic` with high severity. It is a
response-time daemon failure, not a build-time capability flag.

## Rejected Alternatives

1. **Let worker panics tear down the connection.** Rejected because a bare
   connection reset is indistinguishable from daemon death, wrong socket path,
   overload, or peer disconnect.

2. **Wait for the Asupersync region before adding any panic boundary.**
   Rejected because the daemon already accepts untrusted local framed JSON, and
   future handlers can land incrementally. The wire contract should be pinned
   before those handlers grow.

3. **Return the panic payload to the client for debugging.** Rejected because a
   panic may contain workspace paths, memory contents, or attacker-controlled
   strings. The client gets a fixed message; operators get a sanitized stderr
   line.

4. **Abort the entire daemon process on handler panic.** Rejected for the
   current opt-in daemon surface. A single malformed request should not evict
   unrelated in-flight or subsequent local agent requests.

## Verification Hooks

- Unit: `handle_connection_panicking_method_returns_structured_envelope_not_connection_reset`
  exercises the `catch_unwind` plus `build_panic_response` composition and pins
  the generic wire message.
- Unit: `sanitize_panic_message_strips_control_chars_and_truncates` prevents
  log-line injection and unbounded panic logging.
- Unit: `extract_panic_payload_str_handles_str_string_and_other` pins panic
  payload extraction without `Debug` formatting unknown types.
- Fixture: `tests/fixtures/failure_modes/daemon_handler_panic.json` documents
  the degraded-code trigger, severity, and expected message.
- Taxonomy: `docs/degraded_code_taxonomy.md` classifies
  `daemon_handler_panic` as high severity.

## Consequences

- Daemon clients can treat handler panics as ordinary structured daemon errors
  and fall back to the in-process CLI path.
- The daemon's current std-thread implementation has a clear panic boundary
  while the Asupersync supervision slice remains deferred.
- Future daemon method handlers must not bypass `handle_connection` or write
  their own panic responses.
- The future Asupersync region must preserve this wire contract even if it
  changes worker scheduling, cancellation, or restart behavior.
