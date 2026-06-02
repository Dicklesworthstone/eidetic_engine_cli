# ADR 0052: Daemon round-trip benchmark and net-win gate

Status: Accepted
Date: 2026-06-02
Bead: bd-ob21s

## Context

`ee daemon` exists to make future warm-loaded context calls faster by keeping
expensive state resident in a process. The daemon also adds overhead: UDS
connect, request framing, JSON serialization, per-connection dispatch, response
framing, and client-side decoding. Without a baseline, a warm-load slice could
add the daemon path and still be slower than the cold CLI path for realistic
workloads.

The daemon's live UDS accept path currently has safe same-UID peer credential
support on Linux. Other Unix targets fail closed until their peer credential
wrappers land, so the benchmark records pure dispatch everywhere and live UDS
round-trip overhead only where the daemon can authorize the client.

The current `ee.daemon.context` method is still a stub that returns
`daemon_ann_warmload_not_yet_implemented`, so the project cannot yet benchmark
the final cold-start-vs-daemon context comparison honestly.

## Decision

Add `benches/daemon_round_trip.rs` as the daemon overhead baseline. It measures:

- pure dispatch for the seed daemon methods;
- Linux UDS `client_round_trip` overhead against a live local daemon socket;
- the context stub round trip separately from the diagnostic echo path.

The first budget gate is advisory and intentionally conservative:

- `ee_daemon_dispatch`: p50 <= 1 ms, p99 <= 5 ms;
- `ee_daemon_uds_round_trip`: p50 <= 5 ms, p99 <= 25 ms on Linux.

When `ee.daemon.context` is implemented, extend the same benchmark with the
actual net-win comparison:

- cold-start `ee pack "<task>" --json` through the CLI binary;
- daemon-routed context/pack round trip over UDS on the same fixture workspace.

The warm-load slice must show at least a 2x p50 reduction at the 10k-memory
fixture scale, or it needs an explicit waiver explaining why the daemon mode is
still worth shipping.

## Rejected Alternatives

1. **Benchmark only pure dispatch.** Rejected because framing and UDS connect
   are part of the user-visible daemon path.
2. **Pretend the context stub is the final context benchmark.** Rejected because
   the stub intentionally returns a degraded error and does not exercise pack
   assembly, cache residency, or ANN warm-load behavior.
3. **Wait for warm-load before adding any benchmark.** Rejected because the
   overhead floor should exist before the warm-load implementation has a chance
   to hide regressions.

## Verification

- `cargo bench --bench daemon_round_trip` records the overhead baseline.
- `benches/budgets.toml` carries advisory budgets for dispatch and UDS
  round-trip overhead; the UDS measurement is Linux-only until peer credential
  support is implemented for other Unix targets.
- The future warm-load implementation must extend this benchmark rather than
  adding a separate one-off measurement.

## Consequences

The daemon now has a measurable overhead floor. Future warm-load work can be
judged against the floor and against the cold CLI path instead of relying on the
daemon's architectural intent.
