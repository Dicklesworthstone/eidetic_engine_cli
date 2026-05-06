# ADR 0012: Outcome as Canonical Result Type

Status: accepted
Date: 2026-05-05

## Context

`ee` uses Asupersync as its runtime foundation. Asupersync's `Outcome<T, E>` type
carries richer information than Rust's `Result<T, E>`: it distinguishes success,
recoverable failure, fatal error, cancellation, and timeout. This information
matters for agents consuming `ee` output because they can distinguish between
"operation failed, retry later" and "operation was cancelled by budget exhaust."

The risk is that surface layers (CLI, MCP adapter, serve endpoint, library API)
each convert `Outcome` to `Result` at different points, discarding the
cancellation/timeout distinction and forcing all exit paths through a single
error type. This fragments error handling and makes the JSON error contract
inconsistent across surfaces.

## Decision

**`Outcome` is the canonical result type at all internal boundaries.**

1. All core functions return `Outcome<T, DomainError>`, not `Result`.
2. CLI commands preserve `Outcome` until final rendering, then map to exit codes:
   - `Outcome::ok(v)` → exit 0
   - `Outcome::err(e)` → exit code from `DomainError::exit_code()`
   - `Outcome::cancelled()` → exit 130 (SIGINT convention)
   - `Outcome::timed_out()` → exit 124 (timeout convention)
3. JSON output includes an `outcome` field when non-success: `"cancelled"`,
   `"timed_out"`, or `"error"` with the structured error payload.
4. MCP adapter maps `Outcome` to MCP error responses with matching codes.
5. Library API (`ee-core` public surface) returns `Outcome` directly; callers
   that need `Result` call `.into_result()` at their boundary.

## Consequences

- Agents can distinguish budget cancellation from storage errors.
- Exit codes are consistent with Unix conventions for signals and timeouts.
- JSON contracts are richer: `{ "outcome": "cancelled" }` vs `{ "error": {...} }`.
- Internal code never discards `Outcome` variants prematurely.
- Test harnesses can assert on specific `Outcome` variants, not just success/failure.

## Rejected Alternatives

- **Convert to Result early**: Loses cancellation/timeout distinction.
- **Custom enum per surface**: Fragments error handling, duplicates mapping logic.
- **Ignore timeouts in CLI**: Violates determinism; same budget should yield same outcome.

## Verification

- `tests/contracts/asupersync_cancellation.rs`: Asserts `Outcome::cancelled()` flows
  through core to CLI with exit 130.
- `tests/contracts/asupersync_budget.rs`: Asserts budget exhaust produces timeout
  outcome, not silent truncation.
- CLI smoke tests verify exit codes for each outcome variant.
- JSON golden tests include `outcome` field in error responses.
