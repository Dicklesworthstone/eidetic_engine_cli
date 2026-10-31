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
