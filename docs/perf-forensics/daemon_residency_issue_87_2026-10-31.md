# Daemon residency investigation for issue #87

Date: 2026-10-31. Source review began at
`0a626d91277c08df3f5aff472cffe0c1cee1d41b`.

This change repairs actual resource-lifetime gaps and exposes daemon fallback.
It does **not** establish an updated-binary speedup or satisfy ADR 0052's
10,000-memory, at-least-twofold p50 gate. Rust execution was blocked by the
repository-required RCH lane being unavailable. The measurements below are
observations of the published **0.18.0 release**, whose source commit is
`969f0cbf19a5fdaa367328350c937cf566063efc`, not a build of the reviewed main or
the new changes.

## Evidence and scope

- [Issue #87](https://github.com/Dicklesworthstone/eidetic_engine_cli/issues/87)
  describes a ready, approximately 700 MB daemon on a 31 GB Linux host, with
  pack requests still taking 17.5–27.5 seconds and daemon search taking
  12.3 seconds.
- [ADR 0052](../adr/0052-daemon-round-trip-benchmark.md) remains the acceptance
  contract. Its June stub description is historical; the current context RPC
  already executes canonical packing.
- The comprehensive plan, close-the-gap plan, and relevant work records were
  reviewed: `bd-ob21s`, `bd-search-warm-latency-0bh05`, `bd-16pwc.1`,
  `bd-5oa5m`, `bd-snkcp`, and the known pack-hash issue `bd-wubqf`. No bead was
  closed on static evidence.
- The release archive and real multilingual Model2Vec files were verified
  against their published/frozen SHA-256 digests. Downloads were disabled
  during measurement. The exact upstream `frankensearch-index 0.4.0` and
  `frankensearch-core 0.4.0` sources were inspected against `Cargo.lock`
  checksums; the facade dependency is named `frankensearch 0.7.1`.

The compact [measurement receipt](daemon_issue_87_observations_2026-10-31.json)
contains raw wall-time samples, artifact identity, scope, and explicit
unmeasured cases. It contains no private worker or machine identifiers.

## What the public request actually does

CLI parsing carries `--use-daemon` through `pack`, its `context` alias, and
query-file requests. Workspace selection binds the daemon socket and request
authority. Capabilities and the method request share the bounded daemon
attempt deadline. Warming, unsupported options, protocol drift, unavailable
sockets, method errors, and failed handoff admission retain canonical local
fallback.

Public `ee search --use-daemon` dispatches `ee.daemon.search`. Public pack uses
`ee.daemon.pack_search`: it obtains read-only retrieval from the resident
daemon, then performs canonical assembly, audit, and persistence in the client.
This is reported as **hybrid** execution. The separate `ee.daemon.context`
method executes the full context request in the daemon and is reported as
**daemon** execution.

Routing a persisted public pack directly to a mutating context RPC would need
an additional exactly-once protocol. A daemon can continue after a client
loses the response; blindly rerunning locally would risk duplicate persistence.
This change retains the read-only handoff and canonical single local writer.

The handoff still validates the source snapshot/generation and scoring
admissibility. Neither the resident reader cache nor JSON serving metadata
replaces those checks. Pack serving metadata is outside the canonical pack
tree and its hash.

## Measured release baseline

Both columns below are **before-change release measurements**. Each fixture
used five paired fresh CLI processes per path, one already-ready daemon, a
shared warm filesystem cache, a real local neural model, and ordinary persisted
packs with a 400-token request budget. No outliers were removed. These are
shared-container observations, not stable-runner acceptance results.

| Memories | Operation | Fresh in-process p50 | `--use-daemon` p50 | Cold / daemon |
| ---: | --- | ---: | ---: | ---: |
| 10 | Search | 802.616 ms | 369.952 ms | 2.17x |
| 10 | Pack | 1,057.126 ms | 645.113 ms | 1.64x |
| 1,000 | Search | 836.739 ms | 375.480 ms | 2.23x |
| 1,000 | Pack | 1,039.719 ms | 754.389 ms | 1.38x |

The host had approximately 9.73 GiB RAM; it was not the reporter's host. All
measured search samples reported the constrained operating profile. Pack
selected entities, order, content, trust, provenance, and budget agreed across
the paired observations; hashes were not used as a substitute for those
checks. The fixtures contained no feedback or evidence spans. The new
machine-readable serving fields were not available in the released binary;
no daemon fallback diagnostic appeared in these samples, and separate
performance probes identified daemon execution/retrieval.

These observations do not reproduce the reporter's absolute latency. They do
show that the release already reuses model weights and that pack has a much
smaller net benefit than search on these fixtures.

### Cost attribution

In a separate traced ten-memory run, the daemon opened the tokenizer and model
weights once each, during startup. It made 109 source-database `openat` calls
across startup and repeated requests. Those are whole-run counts, not a claim
of 109 opens per request. Traced wall times were excluded from the table.

Single untraced warm-search probes gave:

| Memories | Client RPC time | Daemon total | Inner search total | Model preparation | Index open | Query collection |
| ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 10 | 305.164 ms | 301.666 ms | 24.467 ms | 0.005 ms | 0.033 ms | 0.563 ms |
| 1,000 | 330.772 ms | 325.814 ms | 111.715 ms | 0.012 ms | 0.038 ms | 2.190 ms |

Transport overhead was a small part of these probes. Much of the warm request
was outside the old inner-search trace. Source setup, admission, and audit
work require separate attribution; it would be incorrect to assign the entire
gap to model loading or vector quantization. Cold model preparation took
333–649 ms in the separate explain probes. The vector collection span was only
about 2–3 ms at 1,000 memories, so the larger-scale benefit of retaining its
derived corpus remains unmeasured.

The new trace preserves `search::total` and adds request validation,
reconciliation, pool lookup, empty-generation probing, embedder admission,
snapshot acquisition/release, audit connection opening, and
`search::requestTotal`. Audit workspace/payload/flush spans now survive in the
returned trace instead of being written to a discarded temporary trace.

### Reproduced late fallback after neutral feedback

On the ten-memory fixture, a ready daemon served a pack in 616.619 ms. After
one canonical `ee outcome ... --signal neutral`, the next daemon-routed pack
took 1,168.820 ms and emitted:

```text
Warm daemon search unavailable (workspace changed after daemon retrieval);
used canonical in-process search.
```

The recorded confidence stayed at 0.5. A separate explain probe after the
event showed a 482.978 ms local model preparation. This is a one-call control,
not a statistical speedup measurement or an updated-code comparison.

The source explains the refusal: `quality_feedback_absent` rejects every
nonempty feedback ledger, even when no change occurred between daemon and
client snapshots. A positive feedback preflight now refuses this existing
unsupported state before expensive remote retrieval and supplies
`daemon_pack_search_feedback_unsupported`. Cold cached-model refusal retains
its existing precedence, and the final snapshot check remains necessary for
a feedback insertion racing with the preflight.

Nonempty feedback is **still served locally**. Relaxing the guard using only
`count`/`max_rowid` would be unsafe: same-count feedback changes and calibration
state can alter scores, floor filtering, and truncated candidates. The warm
benchmark explicitly seeds and reports an empty ledger and rejects fallback.

## Resource changes

The process read-pool registry previously stored weak references. A completed
warm-up or request could drop its only strong owner. The daemon now owns
bounded residency leases: eight stores per server, four configurations per
store. Pool reuse respects the effective configuration and physical database
identity. Replaced files obtain new readers, in-flight old snapshots remain
observable for checkpoint attribution, and shutdown revokes retention.
Residency owns connections, never an active source snapshot. Short model
registry inspections borrow the unpinned pool with the existing immediate
ad-hoc fallback under contention.

The new bounded vector cache retains eight verified immutable generations.
`TwoTierIndex::open` previously created fresh readers for each semantic query.
The exact upstream fast reader lazily creates an int8 slab for its entire F16
corpus; reopening discards that slab as well as mmap/alignment state. The cache
preserves this work and keys it by the full configuration, canonical path,
directory identity, metadata/manifest contents, and vector file identity and
modification details.

WAL-bearing, unidentified, unsafe, unsupported, or transiently degraded
quality-tier opens retain uncached behavior. Canonical source/producer/tier
admission still runs per request, and publication leases remain request-scoped.
That independent admission work is a remaining cost; the change does not
claim to remove all index opening or database work.

Search's strict method response is versioned as v4. New clients also accept
the exact historical v3 contract from an already-running older daemon.
Unknown versions and mismatched version/reuse descriptions are rejected.

## Regression and benchmark coverage

Added or extended regressions cover physical reader reuse with fresh committed
rows, no retained transaction, configuration changes, database replacement,
bounded retention/weak-entry cleanup, vector generation/file replacement,
WAL bypass, optional quality-tier recovery, workspace isolation, startup
readiness, repeated calls, shutdown/restart, fallback, and exactly-once audit
and pack persistence behavior. The UDS lifecycle regression uses deterministic
lexical retrieval at 10 and 100 memories; it does not pretend that hash
embeddings exercise the neural vector path. Direct vector-cache tests and
the real-model benchmark cover the semantic-specific work.

The extended `daemon_round_trip` benchmark uses verified neural fixtures,
separately records cold initialization and startup-to-ready, runs three
warmups and 21 measured pairs, retains raw samples, checks canonical selected
content/budget, and requires explicit daemon retrieval with null fallback.
It fixes the operating profile to `portable` and the pack profile to
`balanced`, and records the supplied CLI artifact's BLAKE3 digest separately
from the benchmark source revision.
It supports evidence-heavy observations and 10/100/1,000/10,000-memory scales.
Its ordinary persisted-pack gate still requires at least 2.0x p50 at 10,000;
read-only observations cannot satisfy that gate. See ADR 0052 for commands.

## Verification status and remaining work

Passed static checks include focused nightly Rust formatting, `git diff
--check`, staged commit hygiene, Contract Drift Radar, JSON parsing, and
Draft 2020-12 validation of full current/legacy response examples plus negative
version/field controls. The original v3 schema remains byte-for-byte intact.
These checks do not constitute Rust execution.

The RCH verification wrapper refused before dispatch because the required
sibling dependency repositories were absent. There was also no RCH client or
configured worker. `AGENTS.md` prohibits a local Cargo fallback without
explicit authorization, so no local build/test/bench was substituted. No
GitHub Actions run appeared for the first main commit checked in this task.

The relevant remaining remote runs are the daemon/read-pool/vector-cache unit
tests, daemon contract and CLI routing tests, `integration_a_d` UDS lifecycle
regressions, the audit coverage contract, `bench_contracts`, and both public
warm search/pack performance modes. Run the evidence-heavy scenario separately.
There is no 10,000-memory waiver and no measured after-change result in this
report. Issue #87 should not be considered performance-verified from these
commits alone.

## 2026-11-05 follow-up: correctness and qualification, without new measurements

The November 4 issue update and main at
`8b370dd8044f106945795f1f50455cef71740dd4` were reviewed against the comprehensive
plan, ADR 0052, and the relevant work records. Serving-path provenance, retained
daemon readers/index generations, feedback-aware single retrieval, and the
existing query-reuse changes were already present. This follow-up builds on
those changes and on `benches/daemon_round_trip.rs`; it does not replace them.

### Round-trip failures and exact-result regressions

Two repeated-search tests compared complete results while allowing each request
to choose a new scoring reference time. Recency and final quality scores can
therefore differ even when the source and resident process are unchanged.
`daemon_search_reuses_one_process_and_returns_stable_results` and
`daemon_repeated_retrieval_refreshes_after_rebuild_and_restart_at_two_scales`
now supply one `asOf` value for each equality sequence. Exact array comparisons,
rebuild/restart checks and transport deadlines remain intact. A negative control
advances only `asOf` by one day and requires both recency and final score to
decrease, so the repair does not disable time-sensitive scoring.

The retained evidence does not identify or reproduce all five failures reported
in the November 4 update. The other three remain unclassified here. In particular,
`bd-0kpjq`'s description of a 400k-row gap is not supported by its fixture:
`plant_large_index_gap(..., 400_000, ...)` uses 400,000 as an ID offset and inserts
`SEARCH_INDEX_LARGE_GAP_THRESHOLD + 1` rows, currently 51. Its deadline failure
still needs a native reproduction; neither a larger deadline nor a
production-scale conclusion follows from that description.

Source inspection also found two independent pack-parity defects. These are not
claimed to be the three unclassified failures:

- Successful local source reads controlled admission and scoring but did not
  replace a same-ID global memory already in the request preload. Pack assembly
  could consume the global body for the local row. Batched and point-read
  hydration and the scope collector now preserve local precedence in the
  returned preload as well. A successfully read local row also clears a stale
  global-store lane marker and its synthetic scope labels, so it is not
  incorrectly excluded from the local persistence ledger. Regressions compare
  complete stored rows, an empty preload, malformed-neighbor fallback, source
  classification, and genuine global-only membership.
- Native rule/evidence rehydration replaced metadata from a validated daemon
  handoff, losing calibration fields used by the pack's default relevance
  filter. The client now retains the already validated calibration with the
  request's feedback snapshot and restores calibration for native results that
  carried it before rehydration. It also retains an existing native scope
  annotation only when it equals the already matched request scope. It does
  not reload feedback, duplicate calibration degradations, or change previously
  uncalibrated scope behavior.
  Regressions compare native result metadata and canonical local/handoff packs,
  including an uncalibrated selection control.

Non-passthrough native-rule scope admission can also drop calibration before
the canonical result is produced. That separate behavior remains unchanged;
the new Verified/Global comparisons require the handoff to preserve the current
canonical result rather than introduce a new scope-specific selection policy.

### Attributing the next realistic run

The initial writable connection for an ordinary pack is opened before search
and reused for persistence. Previously its cost was included in `search`, while
`pack.persistence.subspans.connectionOpen` reported zero for that reuse path.
The timings now charge initial and retry writer opens to persistence and exclude
the initial open from retrieval. This changes accounting, not database opens,
write ordering, audit ownership or exactly-once persistence. Pack performance
output also reports the actual search `embedBackend`.

The existing benchmark retains its 10,000-memory, mixed-feedback, 21-pair,
at-least-2.0x p50 contract. It now runs two separate public pack performance
probes after the 50 ordinary invocations. Diagnostic rendering is outside the
p50 sample set. Each probe requires explicit retrieval provenance, a verified
local neural backend, the same workload settings, required phase/storage
timings, and the expected persistence transition. Persisted diagnostic records
must have integrity-checked selections matching the ordinary baseline. Full
selected-content equivalence remains checked on all ordinary calls; redacted
read-only diagnostic output can establish only counts and budgets.

Use the probes to distinguish process wall time, startup-to-ready, first calls,
model preparation, source admission, handoff validation, candidate construction,
assembly and storage. `daemonRetrieval` still includes transport and daemon
search together; the handoff does not carry the daemon's internal timing trace.
The separate search diagnostic workload is useful context, not a substitute for
an ordinary persisted pack measurement.

Two source-established costs remain candidates for measurement, not proven
dominant bottlenecks. Local candidate rows are independently hydrated for
tombstone admission, scope and quality scoring. Safely sharing them would need
to retain snapshot identity, local ownership, authoritative absence and read
failure distinctions; a plain map is insufficient. Complete feedback-ledger
loading and hashing also remain per-request work under the existing content
witness. Replacing that witness with a count or last timestamp would miss
in-place changes and weaken correctness. Writer initialization, ledger
serialization, transactions and audit must likewise be measured before
claiming that resident retrieval eliminates ordinary pack latency.

### Verification boundary

No native test, benchmark, formatter or after-change latency measurement was
run for this follow-up. The available environment has no Rust toolchain,
RCH/DSR client, configured worker credentials or built `ee` binary. The
repository requires its pinned RCH verification lane and forbids local Cargo
fallback; no GitHub Actions run was used. Static diff and commit-hygiene checks
are recorded separately from the pending native results.

A bounded invocation of the required wrapper targeted
`integration_a_d`'s
`daemon_uds_rpc_round_trip::daemon_failed_delivery_does_not_consume_stale_warning_episode`.
It exited 1 before RCH dispatch: the wrapper's committed-source inspection
(`git ls-tree -r -l -z --full-tree` at
`62d9d624f5fe4a7b8cbda54a5503c9e06e01088b`) exceeded its own ten-second subprocess
timeout. The outer 55-second bound did not fire. This is a preflight failure,
not a failed or successful Rust test, and produced no completed verification
proof. An earlier attempt with an incorrect test-target name was interrupted
without executing native tests.

Required native coverage includes `bench_contracts`, the focused pack/search
regressions, and `integration_a_d`'s daemon UDS tests. Then build the matching
release CLI and run ADR 0052's mixed-feedback 10,000-memory gate with the
verified model fixture, followed by the separate evidence-heavy and larger
workspace observations. Attribute the CLI artifact hash and source revision.
The published-release measurements earlier in this report remain historical;
none establish the performance of these changes. No remaining performance bead
or issue #87 is closed on this evidence.
