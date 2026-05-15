# ADR 0033: Serve Localhost V2 Design

Status: proposed
Date: 2026-05-15

## Context

`ee` is CLI-first. ADR 0001 keeps daemon, MCP, HTTP, hook, and library surfaces
as optional adapters over the same core services, not independent products.

The v1 release still needs an honest `ee serve` surface because the feature flag
and README already mention a future localhost HTTP/SSE adapter. The path
selection memo in `docs/spikes/2026-05-serve-path-selection.md` recommends a v1
honesty stub and moves the real HTTP/SSE adapter to v2. That means the v2 shape
must be recorded now, before the v1 stub becomes an accidental permanent
answer.

Project constraints rule out the usual Rust HTTP server stack. `hyper`, `axum`,
`tower`, and `reqwest` are forbidden in the `ee` dependency tree. The adapter
must also preserve the core rule that CLI surfaces remain authoritative and that
all machine-facing output keeps the same response envelopes, schemas, exit-code
semantics, provenance, redaction, and degradation behavior.

## Decision

The real localhost adapter is deferred to v2 and will be implemented as an
in-tree loopback HTTP/1.1 and SSE adapter over existing core command services.
The v1 `ee serve --foreground` surface should return a deterministic
`serve_unavailable_v1` error that points to this ADR.

The v2 adapter must obey these rules:

1. It uses `std::net` for TCP accept/read/write and Asupersync `Scope` values
   for per-connection supervision, cancellation, and budgets. No forbidden HTTP
   crate is introduced.
2. It binds `127.0.0.1` by default. Binding a non-loopback address requires an
   explicit `--allow-non-loopback` flag and an authentication token.
3. Authentication uses `EE_SERVE_TOKEN`, which must contain at least 256 bits of
   entropy when serving mutable or non-loopback endpoints. Missing or weak
   tokens produce a policy error before accepting requests.
4. All endpoint handlers delegate to core services or CLI-equivalent service
   functions. `src/serve.rs` may adapt protocol details but must not duplicate
   search, context packing, policy, graph, import, or curation logic.
5. Every response uses existing `ee.response.v2` or `ee.error.v2` envelopes.
   Endpoint-specific payloads keep the same schemas as the corresponding CLI
   command whenever a CLI equivalent exists.
6. Request parsing is deliberately narrow: HTTP/1.1 only, explicit
   `Content-Length` for request bodies, bounded header and body sizes, no
   chunked uploads in the first v2 slice, deterministic malformed-request
   errors, and no keepalive until tests prove lifecycle behavior.
7. SSE is read-only. It may stream status, diagnostics, long-running job
   progress, or swarm brief updates, but it must not become a general workflow
   engine or agent control loop.

## Endpoint Catalog

The first v2 endpoint set is intentionally small and mirrors existing CLI
surfaces:

| Method | Path | CLI-equivalent intent |
| --- | --- | --- |
| `GET` | `/v1/status` | `ee status --json` |
| `GET` | `/v1/doctor` | `ee doctor --json` |
| `GET` | `/v1/search?q=...` | `ee search "<query>" --json` |
| `GET` | `/v1/context?task=...` | `ee context "<task>" --json` |
| `GET` | `/v1/why/{memory_id}` | `ee why <memory-id> --json` |
| `GET` | `/v1/swarm/brief` | `ee swarm brief --json` |
| `POST` | `/v1/durable-write` | Durable write envelope for future write APIs |
| `GET` | `/v1/events` | SSE stream for read-only progress events |

`POST /v1/durable-write` is a placeholder envelope, not a license to add many
write endpoints. Each concrete write surface still needs its own design, policy
checks, audit behavior, and parity tests.

## Security Policy

The server is local by default and hostile-by-default toward network exposure.
It must reject:

- Non-loopback bind addresses unless `--allow-non-loopback` is present.
- Mutable endpoints without `EE_SERVE_TOKEN`.
- Weak tokens.
- Requests with oversized headers or bodies.
- Unknown methods or paths.
- Requests that would bypass the existing policy/redaction layer.

Tokens are never logged. Diagnostics may report token posture as `missing`,
`weak`, or `configured`, but never include token bytes or hashes that can be
used as bearer material.

## Threading And Budgets

The accept loop creates one supervised unit per connection. Each request runs
with:

- A connection read timeout.
- A request handler budget.
- A response write timeout.
- A bounded body size.
- A bounded event buffer for SSE streams.

When a budget expires, the adapter returns a typed timeout error if headers have
not been sent. If a streaming response is already active, it emits a terminal
SSE error event and closes the connection.

## V2 Acceptance Contract

The real adapter is not done until all of these hold:

1. `ee serve --foreground --json` reports bind address, port, token posture,
   schema version, and readiness without implying cloud or daemon dependence.
2. Default bind is loopback-only.
3. Non-loopback bind without explicit opt-in fails with a policy error.
4. Missing or weak token fails for mutable and non-loopback modes.
5. `GET /v1/context`, `/v1/search`, `/v1/why`, `/v1/status`, `/v1/doctor`, and
   `/v1/swarm/brief` return schema-compatible payloads with their CLI
   equivalents.
6. Malformed requests produce deterministic `ee.error.v2` responses where
   possible.
7. SSE streams are bounded, read-only, and close deterministically on budget
   expiry or client disconnect.
8. Forbidden-dependency audit remains clean.
9. Parity tests prove CLI and serve outputs match modulo explicitly volatile
   transport metadata.
10. E2E tests use real `TcpStream` clients and cover startup, shutdown, bind
    failure, malformed request, unauthorized request, successful read-only
    request, and SSE disconnect behavior.

## Consequences

The v1 release can stay honest without pretending to ship a partial HTTP
server. Users get a stable unavailable response and a concrete v2 target.

The v2 implementation is more work than wiring an off-the-shelf framework, but
it preserves the forbidden-dependency boundary and keeps the CLI as the source
of truth. The adapter must prove parity instead of inventing separate semantics.

## Rejected Alternatives

- **Ship real HTTP/SSE in v1**: Too much protocol, security, and compatibility
  surface for the current release-readiness phase.
- **Use `hyper`, `axum`, `tower`, or `reqwest`**: These are forbidden by the
  project rules and ADR 0015's audit posture.
- **Expose non-loopback by default**: Local memory contents are sensitive, and
  accidental LAN exposure is not acceptable.
- **Make serve the primary product**: This violates ADR 0001. The CLI remains
  authoritative.

## Verification

- `tests/serve_localhost_v2_contract.rs` covers bind policy, token policy,
  request parsing, response envelopes, and malformed-request behavior.
- `tests/serve_cli_parity.rs` compares serve responses against CLI JSON for
  status, doctor, search, context, why, and swarm brief fixtures.
- `tests/serve_sse.rs` verifies bounded read-only event streams, disconnect
  behavior, and timeout behavior.
- `tests/forbidden_deps.rs` continues to fail if an HTTP framework enters the
  dependency tree.
- The v1 `serve_unavailable_v1` failure-mode fixture links to this ADR until
  the real v2 adapter lands.
