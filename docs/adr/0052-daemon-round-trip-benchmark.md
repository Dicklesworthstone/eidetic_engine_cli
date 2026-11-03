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
- After each timed child exits, inspect the fixture ledger read-only. Require
  exactly one new pack record for every ordinary invocation, including first
  calls and warmups, and verify its response hash, workspace, query, profile,
  token budget, selected count and integrity-checked selection ledger. Compare
  persisted entity identities, revisions, ranks, sections, tokens, trust and
  provenance with the response. Reject
  `context_pack_persist_failed` even when the command reports success. Record
  identity, not hash uniqueness, establishes exactly-once persistence.
- Retain raw wall-time samples and compute cold p50 / warm p50. The hard gate
  still requires at least **2.0x** at **10,000 memories**, without a waiver.

`benches/budgets.toml` records the ratio contract in
`comparisons.daemon_pack_10k`. The benchmark exposes two modes:

```bash
# Run through the repository's RCH verification lane. EE_BENCH_EE_BIN must
# point to the release ee artifact built from the changed revision being tested,
# available with the verified local model fixture on the measuring worker.
scripts/rch_verify.sh --pinned-franken-stack --treeish HEAD \
  --env EE_EMBED_DOWNLOAD=off \
  --env EE_EMBED_MODEL_FIXTURE_DIR=/path/to/verified/model \
  --env EE_BENCH_EE_BIN=/path/to/changed-revision/release/ee -- \
  cargo bench --locked --bench daemon_round_trip -- \
  --warm-pack-gate --fixture-memories 10000 --fixture-feedback mixed
```

Use the wrapper's `--env` arguments to transport these settings to the measuring
worker; the model and CLI paths must exist on that worker. Build the release
binary with `cargo build --locked --release --bin ee` through the same pinned
RCH lane and revision before qualification.

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

### 2026-11-01 amendment: feedback-aware handoff and qualification

The v2 pack-search handoff replaces the empty-ledger restriction with a content
witness for the entire workspace feedback ledger, the calibration input actually
consumed by retrieval, and the scoring reference time. The client recomputes
the witness on its pinned source snapshot and retains the source-generation,
quality-scoring-policy and authority checks. This covers changes before ranking,
calibration, floor filtering and candidate truncation; comparing row counts or
only returned candidates would not establish parity. Daemon retrieval remains
read-only, and the client remains the sole owner of pack persistence.

`--fixture-feedback empty|neutral|mixed` selects the benchmark history; pack
measurements default to **mixed**. The supplied CLI records one neutral outcome
and three helpful updates on the planted memory, plus two harmful updates on
a distractor (the same memory only for a one-memory observation). Setup verifies
that events were recorded, authorized and applied, with unchanged confidence
for neutral and directional confidence changes for helpful/harmful outcomes.
The index is published after these updates and before timing. The report retains
the actual total in `fixture.feedbackEvents` and per-signal counts, scenario,
confidence-change count and setup time in `fixture.feedback`. Search's fixture
retains its empty-ledger baseline.

Run the command above with `--fixture-feedback neutral` or
`--fixture-feedback empty` for separate controls, and with `mixed` for the
default qualification. Report each scenario separately: an empty-ledger result
does not establish the mixed-history result. All scenarios still require
ordinary persisted packs, 10,000 memories, 21 measured pairs, canonical selected
content and explicit daemon retrieval with no fallback for the hard gate.
Changed or unverifiable handoff state retains canonical local fallback with
`daemon_handoff_rejected`; that response must fail the warm serving-path
assertion even when its latency happens to meet the numerical threshold.

This amendment adds executable qualification coverage; it does **not** establish
a speedup. Native RCH correctness runs and the 10,000-memory at-least-2.0x gate
remain unverified where the required execution lane is unavailable. The October
31 investigation's published-release measurements remain historical evidence.

### 2026-11-02 amendment: retrieval policy, persistence and fallback diagnostics

The v3 pack-search response additionally requires the actual fusion weights
consumed by retrieval. Admission compares those weights and the reported
reranking mode/top-k with the caller's effective configuration, and compares
the daemon's operating profile and budgets with the profile captured by the
canonical pack request. Configuration-only changes do not necessarily change
the workspace generation or feedback witness. Candidates ranked or truncated
under a different policy must trigger local fallback; rescoring the returned
subset cannot recover omitted candidates. Older v2 responses fail schema
negotiation explicitly instead of being treated as this stronger contract.

The persistence check covers all 50 pack invocations: two first calls, six
warmups and 42 measured calls. The report retains the verified record IDs for
first and measured calls and the total verified/persisted counts. Validation
occurs outside the wall timer and does not replace ordinary pack writes with
read-only operations. Explicit read-only observations instead require zero new
records and remain ineligible for the hard gate. Missing, duplicate, replaced
or corrupt records fail qualification even if the response content is usable.

A handoff rejected after retrieval must retain the rejected attempt's timing
and database-open counts when it retries canonical local admission. Performance
output includes `daemonHandoffValidation`, `daemonHandoffRejected` and the
fallback explanation; `total` includes both attempts. Repeated phase names
accumulate their elapsed durations. The fallback notice is delivery metadata
and must not retroactively change the finalized pack hash or persisted ledger.
Ordinary persisted retries compute their elapsed SLO from the original request
start, with the existing one-time model-load exclusion.
Cached read-only JSON delivery merges newly appended fallback diagnostics into
both degraded arrays without changing its cached pack payload or hash.

Public search uses explicit local fallback for `self` and `team` memory scopes
until caller identity is carried into canonical daemon scope admission. Direct
search RPCs independently refuse those scopes instead of consulting the
daemon's own agent identity. Search `--full` also uses local rendering while the
daemon protocol contains only preview renderings; performance-only output is
unaffected. These responses cannot count as warm qualification samples.

These checks and corrections do not establish a performance result. Native
correctness execution and the current-source mixed-feedback 10,000-memory gate
still require an available RCH worker, pinned dependencies, a matching release
CLI artifact and the verified local model fixture.
