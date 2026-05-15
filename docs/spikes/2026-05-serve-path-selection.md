# Serve Localhost Path Selection

Date: 2026-05-15
Owner: RubyWolf
Bead: bd-3usjw.4.1

## Decision

Recommend **Path B: honest defer-to-v2** for `bd-3usjw.4` in the v0.1 release
train.

The v1 release should make `ee serve --foreground` reachable and deterministic,
but it should return an honest `serve_unavailable_v1` error rather than shipping
a partial HTTP server. The real localhost HTTP/SSE adapter should move to the
v2 design ADR tracked by `bd-3usjw.24`.

If no human override lands by 2026-05-22, treat Path B as the default decision
for v1.

## Current Evidence

- `Cargo.toml` declares `serve = []`, but the flag is documented as reserved:
  it has no current cfg-gates and no optional dependencies.
- `docs/feature_flag_registry.md` and `docs/silent-fallback-inventory.md` both
  describe `serve` as a future localhost HTTP/SSE adapter, not an implemented
  surface.
- `src/serve.rs` is not an HTTP adapter. It currently backs foreground daemon
  job persistence, daemon status, and orphaned daemon-job recovery.
- `tests/ee_core_api_no_adapter_logic.rs` already treats `src/serve.rs` as an
  adapter boundary that must delegate business logic to `src/core`.
- The project forbids `hyper`, `axum`, `tower`, and `reqwest`, so Path A cannot
  use the standard Rust HTTP server stack.

## Path A: Real In-Tree HTTP/SSE Adapter

Path A would implement an HTTP/1.1 loopback adapter using `std::net` plus the
existing runtime/supervision model. It would expose command-equivalent endpoints
such as `/v1/context`, `/v1/search`, `/v1/why`, `/v1/status`, `/v1/doctor`, and
swarm brief surfaces.

Estimated cost: **5 to 10 focused person-days** for a production-grade first
pass, assuming no new forbidden dependency is introduced.

Required work:

- CLI shape: `ee serve --foreground`, bind/port flags, explicit non-loopback
  policy, startup JSON, and deterministic shutdown behavior.
- Protocol parser: request line, headers, body length, malformed-request errors,
  timeouts, keepalive policy, and response writing.
- Security: bearer-token enforcement from `EE_SERVE_TOKEN`, token redaction in
  traces, non-loopback denial by default, and clear policy-denied exit behavior.
- SSE: event framing, disconnect handling, bounded buffering, heartbeat policy,
  and backpressure semantics.
- Adapter mapping: every endpoint must delegate to core command handlers without
  duplicating business logic in `src/serve.rs`.
- Tests: real `TcpStream` E2E coverage, forbidden-dependency audit, port-bind
  failure semantics, non-loopback policy tests, malformed request tests, and
  snapshot coverage for startup/error payloads.

Risk surface:

- HTTP correctness is easy to underbuild. Partial parsing creates ambiguous
  security and interoperability behavior.
- Auth and logging mistakes could expose local memory contents to unintended
  callers on shared machines.
- The adapter would compete with release-readiness blockers that currently have
  higher leverage: dependency publishing, RCH verification, and graph/agent
  JSON contract closure.
- A quick implementation would likely become a compatibility commitment before
  the CLI-first contract has settled.

Strategic value:

- Useful for local dashboards, browser edition experiments, and tools that
  cannot shell out to `ee`.
- Not required for the v1 controlling idea. The core product remains the local
  CLI memory substrate that harnesses invoke explicitly.

## Path B: Honest Defer-To-V2

Path B makes the surface reachable but explicitly unavailable in v1.

Estimated cost: **0.5 to 1 focused person-day**.

Required v1 behavior:

```json
{
  "schema": "ee.error.v2",
  "success": false,
  "error": {
    "code": "serve_unavailable_v1",
    "message": "The localhost HTTP adapter is planned for v2; use the CLI surfaces directly in v1.",
    "severity": "low",
    "repair": "Track bd-3usjw.24 for the forbidden-dependency-clean v2 serve design.",
    "details": {
      "recovery": [
        {
          "priority": 1,
          "kind": "command",
          "command": "ee context \"<task>\" --workspace . --json"
        },
        {
          "priority": 2,
          "kind": "command",
          "command": "ee search \"<query>\" --workspace . --json"
        }
      ]
    }
  }
}
```

Acceptance for Path B:

- `ee serve --help` is visible.
- `ee serve --foreground --json` emits the deterministic error above and exits
  through the documented unavailable/degraded path.
- Human output explains the same fallback without implying a background service
  exists.
- The failure-mode fixture catalog includes `serve_unavailable_v1`.
- Vision coverage classifies `serve` as documented-stubbed, not missing.
- `docs/adr/0033-serve-localhost-v2-design.md` lands with `bd-3usjw.24` before
  `bd-3usjw.4` closes.

Risk surface:

- Users do not get HTTP introspection in v1.
- The reserved feature remains a known capability gap until v2.
- A future implementer must still avoid simply copying an HTTP crate into the
  dependency tree.

Strategic value:

- Keeps the v1 release honest and CLI-first.
- Preserves the adapter boundary by avoiding rushed business logic inside
  `src/serve.rs`.
- Gives v2 room to design the HTTP/SSE contract around authentication,
  backpressure, schema parity, and browser-edition needs instead of patching an
  accidental v1 protocol.

## Recommendation

Choose Path B for v1.

The decisive point is not effort alone. The project is currently converging on
release-readiness and agent-first CLI contracts. A real HTTP/SSE adapter is a
new network protocol surface, not a thin command alias. Shipping it under the
current constraints would add security, parsing, lifecycle, and compatibility
obligations that do not advance the v1 walking skeleton as much as publishing,
RCH, schema, and verification closure work.

The correct v1 move is to expose the command honestly, return a stable
machine-readable unavailable response, and force the real server design through
the v2 ADR.

## Updates Needed After This Memo

- Update `bd-3usjw.4` to pin Path B acceptance for v1.
- Keep `bd-3usjw.24` / `docs/adr/0033-serve-localhost-v2-design.md` as a
  required dependency for the eventual real adapter design.
- When Beads metadata is writable, mark `bd-3usjw.4.1` as closed with this memo
  as evidence.
