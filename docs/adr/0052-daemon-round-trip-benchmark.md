# ADR 0052: Daemon round-trip benchmark and net-win gate

Status: Accepted
Date: 2026-06-02
Bead: bd-ob21s

## Context

The context below records the state at the original 2026-06-02 decision.
The October implementation and current verification surface are described in
the amendment below; the historical stub description is not current status.

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

## 2026-10-31 amendment: public pack and search measurements

`ee.daemon.context` now executes canonical packing, and the public search
benchmark already compares fresh CLI processes with one resident daemon.
Issue #87 demonstrates why transport support alone cannot establish an
acceleration claim. The round-trip benchmark now also exercises ordinary
persisted `ee pack` calls through the public CLI, using the same verified
Model2Vec fixture as search.

The pack comparison has these controls:

- Build the database and neural index before timing, with downloads disabled.
- Pin the fixture's operating profile to `portable` in workspace configuration
  before indexing, and the pack profile to `balanced` through `EE_PROFILE`.
  Assert the operating profile in rebuild/search/diagnostic responses and the
  pack profile in pack responses. Report both selections; host-adaptive budget
  changes must not alter the work between cold and warm samples. Child
  processes use private fixture XDG directories and `EE_EMBED_BACKEND=local`.
- Wait for the daemon capability `warm.posture` to be `ready`, separately from
  socket publication; report startup-to-ready and the first daemon pack.
- Warm both paths, then take 21 paired samples, alternating execution order.
  Each invocation is a new CLI process. Only the daemon process is reused.
- Require `neural_local`, a null `fallbackReason`, and an explicit serving path
  for every sample. `hybrid` means resident daemon retrieval plus canonical
  client-side pack assembly/persistence; it must not be described as whole-pack
  daemon execution. Either path must earn its speedup through measurement.
- Compare selected entity identities, order, content, trust, provenance and
  token budgets. Ordinary persisted calls may differ in record IDs, timestamps,
  time-derived scores and diagnostic hashes; raw hashes are retained in the
  report but are not substituted for content correctness assertions.
- Retain raw wall-time samples and compute cold p50 / warm p50. The hard gate
  still requires at least **2.0x** at **10,000 memories**, without a waiver.

`benches/budgets.toml` records the ratio contract in
`comparisons.daemon_pack_10k`. The benchmark exposes two modes:

```bash
# Run through the repository's RCH verification lane. EE_BENCH_EE_BIN must
# point to the release ee artifact built from the changed revision being tested,
# available with the verified local model fixture on the measuring worker.
EE_EMBED_DOWNLOAD=off \
EE_EMBED_MODEL_FIXTURE_DIR=/path/to/verified/model \
EE_BENCH_EE_BIN=/path/to/changed-revision/release/ee \
scripts/rch_verify.sh --pinned-franken-stack --treeish HEAD -- \
  cargo bench --locked --bench daemon_round_trip -- --warm-pack-gate
```

Both warm search and pack JSON reports include `cliArtifact.path`,
`cliArtifact.contentHash` (BLAKE3), and `cliArtifact.sizeBytes`. The executable
is streamed through the hasher before fixture setup and timed samples. RCH's
source attestation identifies the benchmark source; the external CLI artifact
must be attributed separately. Record which build produced the reported hash:
a version string alone does not prove that the executable contains the changed
revision.

Use `--warm-pack-observation` on variable/shared hardware; it prints the same
ratio and a truthful `passed` value without treating the observation as a
stable-runner acceptance result. Add `--fixture-memories 10`, `100`, `1000`, or
`10000` for scale observations. `--fixture-evidence 5000` adds 20% admissible
assistant spans and 80% quarantined tool spans through the canonical evidence
insertion boundary; index admission counts are asserted before timing. A
memory-only fixture cannot stand in for this evidence-heavy workload.

`--pack-read-only` is an additional observation mode. It is rejected by the
hard gate because it changes the ordinary persisted workflow reported in #87.
The benchmark shares a warmed filesystem cache between paths and does not
claim cold-disk measurements. A green dispatch microbenchmark, a socket that
accepts connections, or a machine that merely holds the model in RSS cannot
satisfy this gate.

### Feedback-ledger boundary and fallback control

The pack acceleration fixture has **zero feedback events**, asserted during
setup and recorded as `fixture.feedbackEvents: 0` in its result. This is a
material scope limit: the current pack handoff can prove freshness only for an
empty feedback ledger. Workspace generations do not cover every feedback
mutation, and `(count, max_rowid)` cannot prove that an existing event is
unchanged. Feedback can alter quality ranking and calibration before the
daemon truncates its candidate set, so rechecking only returned items would
not establish canonical parity.

A nonempty ledger therefore receives
`daemon_pack_search_feedback_unsupported` before expensive daemon retrieval,
then runs the canonical in-process pack path. Its public JSON must identify
`servingPath: in_process_fallback`, `retrievalPath: in_process`, and that
fallback reason. This avoids executing both complete retrieval paths; it does
**not** establish daemon acceleration for workspaces with feedback history.
Such a response must fail the warm benchmark's serving-path assertion even
when its wall time happens to meet the numerical threshold.

Use a disposable initialized workspace, a ready daemon bound to it, and an
existing memory as a manual negative control:

```bash
ee --workspace /path/to/disposable/workspace --json outcome MEMORY_ID \
  --signal neutral --source-id daemon-87-control \
  --reason 'Daemon handoff fallback control'
ee --workspace /path/to/disposable/workspace --json pack 'release provenance' \
  --use-daemon
```

Neutral feedback creates a ledger entry without changing memory quality. It
must still produce the explicit fallback above. Verify canonical selected
content against the same command without `--use-daemon`; do not include this
case in the accepted warm-acceleration samples. Standard daemon search does
not use the pack handoff's empty-ledger restriction. Extending pack reuse to
nonempty ledgers requires a complete freshness witness for mutable feedback
and calibration, or a handoff before feedback-dependent filtering and ranking.
