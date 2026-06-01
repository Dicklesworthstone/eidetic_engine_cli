# ADR 0049: Memory-Mapped Frankensearch Index for Zero-Copy Hot Reads — Deferred to Research Backlog

Status: Deferred (research backlog)
Date: 2026-05-27
Bead: bd-17c65.14.9 (N9)

## Context

bd-17c65.14.9 (N9) asks whether `ee` should ship a memory-mapped variant
of the Frankensearch lexical index so hot-path retrieval can read posting
lists directly from a page-cache-backed mapping instead of round-tripping
through a deserialized in-memory copy.

The motivation is concrete:

- **Eliminate page-cache double-buffering on the BM25 inverted index.**
  Today the lexical tier loads posting lists into a `Vec<Posting>` per
  query, which means the kernel page cache holds the on-disk bytes and
  `ee`'s heap holds a parsed copy. On a 256 GiB host with a 10 GiB
  lexical corpus this doubles steady-state RSS for the search hot path.
- **Skip serde round-trips on the hot path.** Each query currently
  re-decodes posting bytes into structured values. With a stable
  on-disk layout that matches the in-memory access pattern, a query can
  iterate posting bytes directly without an allocator hit.
- **Pair with bd-21xbi (lexical RAM-tier pinning) and ADR 0040
  (per-workspace shard fan-out).** Zero-copy reads compose with the
  RAM-tier `mlock`/`MADV_HUGEPAGE` work and with the per-shard write
  ownership story; together they would let a 64-core host run many
  concurrent `ee context` calls against the same lexical index without
  contending on either the allocator or the global write gate.

The same motivation is described in N9's tracker text and in
[`docs/agent-ux/insights-onboarding.md`](../agent-ux/insights-onboarding.md)
as a longer-term lever for swarm-scale responsiveness.

## Decision

**N9 is deferred to the research backlog. `ee` will not ship a
memory-mapped Frankensearch index in the current release wave.**

The deferral is recorded here so that future agents picking up
bd-17c65.14.9 can immediately tell why the work has not landed and what
must change for it to land cleanly.

### Why defer rather than build

1. **Upstream owns the on-disk format.** `/data/projects/frankensearch`
   is the canonical home of the posting-list serializer; `ee` consumes
   its public reader API. A zero-copy mmap path requires either a stable
   on-disk layout commitment from upstream Frankensearch (so a mapped
   view does not silently break on the next minor release) OR a forked
   downstream serializer that `ee` maintains. Both options have to be
   negotiated upstream first; doing the work downstream-only would leave
   `ee` carrying a serializer fork that drifts every Frankensearch
   release.
2. **Unsafe-policy collision with `#![forbid(unsafe_code)]`.** The same
   blocker that stalls bd-21xbi.2 (safe Linux mmap + mlock + madvise)
   applies here. `memmap2`, `rustix`, and the obvious safe-mmap crates
   all require `unsafe` at the call boundary; the project policy forbids
   unsafe in the main crate. N9 cannot ship without either an approved
   safe-mmap dependency OR an internal adapter crate that audits unsafe
   outside the main forbid boundary. Both are open architectural
   decisions tracked under bd-21xbi.2's blocker chain.
3. **Spike scope is not small.** A credible N9 spike is ~3 weeks of
   work: ~1 week to negotiate and ship an on-disk-format stability
   contract upstream in Frankensearch, ~1 week to land the safe-mmap
   adapter and the downstream reader change behind a feature flag, and
   ~1 week to land a 1 M-document p99 latency bench on a 256 GiB Linux
   host (the same host class bd-21xbi.3 targets). The spike's value
   depends on a measurable end-to-end p99 win; without that, the work
   is theoretical.
4. **No current consumer crosses the staying-power threshold.** Today
   `ee` retrieval p99 on the canonical fixture set is well under the
   `pack.max_tokens` assembly budget, and `ee context` end-to-end is
   dominated by pack assembly + provenance rendering rather than by
   lexical IO. Until a real consumer profile shows lexical IO as the
   top contributor, the optimisation cannot meet the ADR 0024 "stays
   in the production hot path" bar.
5. **Priority is P3.** Bead priority signals "research-grade, ship
   when displaced from the backlog by evidence" not "ship in the next
   release wave." Treating P3 + upstream-blocked + no-consumer-profile
   as anything other than deferred would burn agent time on
   speculative infrastructure.

### What "deferred" means for tracking

- `bd-17c65.14.9` is closed with this ADR as the documented decision.
- The research backlog stays open in `docs/research/`-style locations
  (or in the bead's reopen-on-evidence trigger comment) so a future
  agent can surface the idea again when one of the deferral conditions
  flips.
- The deferral does NOT block sibling work. bd-21xbi (RAM-tier pinning),
  ADR 0040 (shard fan-out), and ADR 0042 (symbol graph derived index)
  all proceed on their own lanes; this ADR exists so a future agent
  reading bd-17c65.14.9 understands that the dependency chain N9 needs
  is rooted in bd-21xbi.2's unsafe-policy decision plus an upstream
  Frankensearch contract, not in `ee`-side code that just needs to be
  written.

### Re-open Criteria

1. **Frankensearch on-disk-format stability ADR lands upstream.** A
   stable layout commitment with an `#[repr(C)]` or zerocopy-validated
   posting struct, plus an upstream reader/writer pair that round-trips
   it, makes the downstream mmap path safe to write.
2. **Approved safe-mmap dependency or unsafe-carve-out ADR lands
   downstream.** Resolving bd-21xbi.2's blocker also unblocks N9; the
   same adapter that pins lexical files in RAM is the natural seam for
   zero-copy posting reads.
3. **A profiled `ee context` run on a representative host class shows
   lexical-tier IO as the dominant p99 contributor.** Per the ADR 0024
   forensics contract, the recorded bench artifact stays in the
   verification ledger as evidence that the optimisation now meets the
   staying-power threshold.
4. **A consumer crosses the swarm-scale responsiveness threshold.** If
   a real-tailnet swarm running >= 32 concurrent `ee context` invocations
   begins to bottleneck on lexical-IO contention (RSS doubling + page-
   fault counts), the optimisation moves from research to production.

## Consequences

- The hot-path read in `src/search/lexical_ram_tier.rs` continues to
  emit `LEXICAL_RAM_TIER_NOT_IMPLEMENTED_CODE` on Linux and the
  existing degraded codes on macOS / Windows; N9 will not change that
  surface until the reopen conditions above flip.
- Future agents auditing N9 against the Bayesian-trust-class-promotion
  evidence in ADR 0032 can cite this ADR as the documented reason the
  optimisation is research-backlog rather than active work, instead of
  rediscovering the unsafe-policy + upstream-format blockers from
  scratch.
- The release wave is unaffected. N9 was never on the critical path for
  any open `implements-surface:*` bead; ADRs 0046 (v0.1.0 tag recovery)
  and 0047 (publish_flip Cargo.toml gate) cover the release blockers
  N9 might be confused with.
- The research-backlog framing matches how N1-N8 and the alien-artifact
  family in `docs/research/` are tracked: ADR records the decision, the
  bead carries the reopen trigger, and no in-repo `src/` code goes into
  speculative scaffolding while the decision stays deferred.

## Rejected Alternatives

- **Ship a downstream-only mmap reader against the current
  Frankensearch on-disk layout.** Rejected. Frankensearch does not
  commit to a stable on-disk layout; the next minor release could
  reshape posting bytes and silently break a downstream zero-copy
  reader without a compile-time signal. The fix-forward path is to
  negotiate the contract upstream first, not to carry the fork
  downstream.
- **Use an `unsafe` block locally with a "narrow blast radius" comment.**
  Rejected on policy grounds. The crate-level
  `#![forbid(unsafe_code)]` is a load-bearing rule; carving out a
  single block sets the precedent for "this one mmap is fine" which
  multiplies. The right path is the bd-21xbi.2 unsafe-policy decision
  (approve a safe-mmap dependency OR ship an internal adapter crate
  outside the forbid boundary).
- **Build the spike against a synthetic 1 M-document corpus today and
  defer the production landing.** Rejected. The spike's value is
  conditional on the reopen conditions above; running it now generates
  a measurement that goes stale before it influences a decision. The
  bench harness from bd-21xbi.3 already exists as the host-class proof
  shape, so the spike can be designed quickly when the reopen
  conditions flip.
- **Treat N9 as `implements-surface:mmap_frankensearch_zero_copy` and
  add a feature flag for opt-in.** Rejected. Feature flags for
  unshipped work invite "ship a placeholder and call it implemented"
  closures of the kind ADR 0026 + the honesty-only/implements-surface
  bead taxonomy were created to prevent. Deferral with an ADR is the
  honest tracking shape.

## Verification

How a future reviewer can confirm this decision remains valid:

- `br show bd-17c65.14.9` reports the N9 tracker item as deferred or
  closed with ADR 0049 named as the documented decision.
- `docs/adr/README.md` lists ADR 0049 in the ADR index as deferred to
  the research backlog.
- `rg -n "LEXICAL_RAM_TIER_NOT_IMPLEMENTED_CODE" src/search/lexical_ram_tier.rs`
  still finds the current scaffold emission path for Linux builds where
  the mmap / pinning adapter has not landed.
- `rg -n "MmapFrankensearch|memmap2|zerocopy|mmap_friendly" src/search Cargo.toml`
  returns no matches unless a superseding ADR names the upstream layout
  contract and unsafe-policy decision.
- The dependency path remains rooted in the bd-21xbi.2 unsafe-policy
  decision plus an upstream Frankensearch on-disk-format contract; if
  either dependency lands, the reviewer should re-run the reopen
  criteria instead of silently treating this ADR as obsolete.

If any of those checks fail without a superseding ADR, the deferral has
drifted. Restore the deferred state or open a new ADR with the upstream
format, safe-mmap, and representative p99 evidence required above.
