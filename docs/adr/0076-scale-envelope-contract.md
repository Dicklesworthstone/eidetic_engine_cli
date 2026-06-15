# ADR 0076: Scale-Envelope Contract

Status: accepted
Date: 2026-06-15
Bead: bd-ssoco.1

## Context

Swarm-scale work needs a shared answer to a narrow question: is this workspace
small enough for ordinary command assumptions, merely warming derived assets, or
large enough that cache, WAL, and index posture must shape command SLOs? Today
that information is scattered across hotset manifests, resource-admission
diagnostics, CASS prefetch posture, swarm replay scorecards, DB size probes, and
failure-mode fixtures.

`bd-ssoco` does not introduce a daemon-first steward. The first contract is a
redaction-safe envelope that later probes, fixtures, and replay tools can emit
from normal CLI commands. The envelope records counts, states, SLO observations,
degraded codes, and recovery commands. It never stores memory bodies, session
text, mail bodies, or raw query text.

## Decision

`ee.scale_envelope.v1` is the normative scale-envelope schema:
[`docs/schemas/ee.scale_envelope.v1.json`](../schemas/ee.scale_envelope.v1.json).

The envelope contains:

| Area | Purpose |
| --- | --- |
| Corpus profile | Memory/link/pack/search-document counts, DB bytes, estimated content bytes, and optional deterministic fixture profile id. |
| Store posture | FrankenSQLite size and page posture plus read-pool and write-spool states. |
| Page-cache/WAL posture | Cold/warming/warm/thrashing cache state, WAL checkpoint posture, amplification estimates, and bounded checkpoint age. |
| Index posture | Lexical, semantic, and graph freshness, generation, lag, and last-build timestamps. |
| Command SLOs | Per-surface p50/p95/p99 observations against a budget, with scale-specific degraded code linkage. |
| Recovery actions | Ordered, machine-readable commands for recapture, cache warming, WAL checkpointing, index rebuild, probe scoping, or support-bundle inspection. |
| Provenance | Bounded schema, fixture, probe, bead, and ADR refs with optional BLAKE3 hashes. |

The shipped degraded-code vocabulary for this envelope is:

| Code | Severity | Meaning |
| --- | --- | --- |
| `scale_posture_warming` | `low` | The corpus or derived assets are cold but moving toward an expected warm state. |
| `scale_posture_thrashing` | `high` | Cache, WAL, or index churn is high enough to invalidate ordinary SLO assumptions. |
| `scale_fixture_unavailable` | `medium` | A requested deterministic large-corpus fixture profile is missing or unreadable. |
| `scale_probe_budget_exceeded` | `warning` | A bounded probe stopped before full coverage and returned partial posture. |

## Relationship To Existing Work

- **Hotset manifests** (`ee.cache.hotset.v1`) describe which redaction-safe
  search and pack entries should be warmed. Scale envelope describes whether
  the workspace is warm enough, or whether the hotset should be recaptured or
  prewarmed before measuring SLOs.
- **Resource admission** remains the claim-gate and workload-pressure advisor.
  Scale envelope supplies corpus and cache posture evidence that admission can
  consume without taking over claims.
- **CASS prefetch** still decides scoped prefetch safety. Scale envelope records
  prefetch-visible index and cache posture as evidence; it does not prefetch
  sessions itself.
- **Swarm replay** stays replayable and side-effect-free. Scale envelope gives
  replay fixtures and live scorecards a common SLO vocabulary so downstream
  work can compare fixture and live posture without bespoke fields.

## Constraints

- The schema is local-first and CLI-first. No field requires daemon mode.
- Local Cargo is not part of the verification contract on this Mac swarm lane.
- The envelope is redaction-safe by construction:
  `redactionStatus = counts_hashes_paths_no_content`.
- Probes may return partial envelopes with `scale_probe_budget_exceeded`;
  they must not silently pretend full coverage.
- Deterministic large-corpus generators in `bd-ssoco.2` must use
  `fixtureProfileId` rather than embedding raw generated rows.

## Rejected Alternatives

- **Daemon-only steward status:** rejected because the first deliverable must
  work from ordinary CLI commands and deterministic fixtures.
- **Embedding this into hotset manifests:** rejected because hotsets explain
  cache candidates, not store, WAL, index, and command SLO posture.
- **Unbounded live probes:** rejected because swarm agents need predictable
  proof cost; bounded probes can surface `scale_probe_budget_exceeded`.
- **Raw per-query timing logs:** rejected because the scale surface must be
  support-bundle safe and cannot store user task or memory content.

## Verification

- `tests/contracts/scale_envelope_schema.rs` pins schema identity,
  `KNOWN_SCHEMAS` registration, public schema-catalog registration, required
  posture blocks, SLO and recovery closed sets, redaction posture, no-daemon
  status, and degraded-code fixture coverage.
- `tests/fixtures/failure_modes/scale_*.json` documents the four scale
  degraded codes with severity, trigger shape, message substrings, and repairs.
- Later implementation beads (`bd-ssoco.2`, `bd-ssoco.3`, and `bd-ssoco.4`)
  should emit or consume this schema rather than introducing new SLO fields.
