# ADR 0030: Typed Determinism Capability Token

Status: accepted
Date: 2026-05-13

## Context

The N4.1 audit (`tests/randomness_inventory.json`) found 316 sources of
ambient nondeterminism. Its `rows_content_hash` is:

`blake3-ish:51a8854727a5768008ba8269596e8666cc9ffdd88e8ac3f13101ad36434a3bfc`

The largest bucket is clock usage (`systemtime`, 261 rows), followed by
filesystem order, environment reads, unsorted `HashMap` iteration, and direct
randomness. Existing golden tests catch some drift after the fact, but they do
not make nondeterminism impossible to introduce in the retrieval and context
pack hot paths.

N4 needs a substrate that code can require before consuming randomness-like
state. N4.2 owns that substrate; N4.3 and later beads thread it through the
existing paths.

## Decision

Introduce `ee::runtime::determinism::Deterministic<Seed>`, a move-only runtime
capability token.

The token:

1. Carries a stable `Seed`.
2. Records its `SeedSource`: explicit, persistent workspace, timestamp
   truncated to second precision, environment value, or child scope.
3. Has an internal monotonic counter.
4. Cannot be cloned.
5. Is `Send` but intentionally not `Sync`.
6. Splits with `child(label)` so independent subsystems receive deterministic
   sub-token streams instead of sharing one mutable stream accidentally.

The token is the only constructor for deterministic consumers introduced in
N4.2:

- `DeterministicClock`, which returns UUIDv7-compatible timestamps and UUIDv7
  values without calling `SystemTime`.
- `DeterministicRng`, a small deterministic byte stream for bounded local
  consumers that would otherwise reach for process randomness.
- `DeterministicOrder`, a token-constructed sorting helper for collections
  whose native iteration order is not stable enough for machine-facing output.

The UUIDv7 clock uses the seed plus the token's monotonic counter as the
timestamp/counter source. Within one token scope, emitted UUIDv7 values are
monotonically increasing. With the same seed and child-label sequence, emitted
UUIDv7 values are byte-identical across runs. Across different root seeds,
ordering is deterministic but arbitrary: it follows seed precedence, not
wall-clock time.

## Consequences

What becomes easier:

- Entry points can make determinism explicit before calling pack assembly,
  retrieval, scoring, or ID generation.
- Tests can assert deterministic byte output by constructing tokens from a fixed
  seed without patching global clocks or process randomness.
- Future lints can look for functions marked as determinism-required and ensure
  consumers are constructed through the token.

What becomes harder:

- Callers must split child tokens deliberately when crossing subsystem
  boundaries.
- Existing functions that assume `Uuid::now_v7`, `SystemTime::now`, or direct
  randomness will need N4.3 mechanical edits before they can participate in the
  typed deterministic path.

## Rejected Alternatives

### Global deterministic mode

A process-global deterministic mode would be easy to turn on, but it would
silently affect unrelated code and make concurrent test runs hard to reason
about. The token keeps the deterministic budget lexical and explicit.

### Cloneable seed handle

A cloneable handle would be convenient, but two consumers could accidentally
reuse the same stream and couple their output. `child(label)` is more verbose
and easier to audit.

### Wall-clock UUIDv7 with sorted output

Sorting after generation can stabilize some arrays, but it does not fix the
underlying fact that IDs and timestamps differ across runs. The deterministic
clock makes the source stable instead of normalizing drift after the fact.

## Verification

N4.2 ships with these verification hooks:

1. `tests/determinism_capability_token_unit.rs` covers construction from all
   supported seed sources, child split determinism, deterministic byte streams,
   deterministic ordering, monotonic UUIDv7 emission, cross-seed UUID ordering,
   and `Send` typing.
2. Compile-fail doctests in `src/runtime/determinism.rs` prove the token is not
   cloneable and not `Sync`.
3. `tests/adr_0030_docs.rs` asserts this ADR is indexed, cites the N4.1
   inventory hash, documents the move-only and `Send + !Sync` decisions, and
   names the N4.3 threading boundary.
4. `tests/snapshots/determinism_token_doctest_output.snap` pins a compact
   deterministic sequence summary for future drift checks.
