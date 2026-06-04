# Arena-Backed Pack Assembly Contract

This document defines the contract for future arena-backed context-pack
assembly work. It is intentionally a planning artifact: no public JSON schema,
environment variable, or Rust implementation changes are introduced here.

The current pack path builds an owned `PackDraft` from sorted candidates in
`assemble_draft_with_profile_and_options_seeded`. MMR and facility-location
assembly both use deterministic candidate ordering, explicit tie-breakers, and
owned `Vec` accumulators for selected items, omissions, audit steps, signatures,
coverages, and similarity data. An arena may reduce allocation churn, but it
must not become a source of ordering, identity, or output changes.

## Arena Modes

The first implementation mode is `request_scoped`.

`request_scoped` means:

- One arena is created for one pack assembly request.
- The arena is reset or dropped after owned `PackDraft` output is materialized.
- No reference into the arena crosses the pack assembly boundary.
- `PackDraft`, `PackDraftItem`, `PackOmission`, selection audit, pack metadata,
  rendered text, and persisted pack records stay owned values.

`disabled` remains a first-class mode. Every arena implementation must support
running the same fixtures with arena allocation off and on.

`workspace_reuse` is not part of the first implementation. A later child may
reuse an arena across requests only if it adds explicit reset auditing,
poisoning on panic or failed reset, capacity caps, and a generation key that
includes the workspace, schema version, resource profile, and arena policy
version. A reused arena must still return owned output and must never expose
bytes from a previous request.

## Output Parity

Arena mode is an internal allocation strategy. It must not affect:

- `ee pack --json` response bytes after volatile fields are normalized by
  the existing determinism harness.
- Markdown, JSONL stream trailer, and any other renderer over the same pack.
- `data.pack.hash` and persisted `pack_records.pack_hash`.
- `pack_records.ledger_json` and `pack_records.ledger_hash`.
- Selected item order, omitted item order, omission reasons, token counts, and
  section quotas.
- `selectionAudit`, including algorithm id, selection steps, marginal gains,
  coverage-fill count, and objective value.
- Pack DNA or graph-explanation fields when explain mode is enabled.
- Degraded entries, severity, repair hints, and aggregation order.

If any of those values changes with arena mode enabled, the change is a pack
selection or rendering change, not an arena optimization.

## Determinism Rules

Arena addresses, allocation order, chunk boundaries, and reset timing are never
valid tie-breakers. Existing deterministic sources stay authoritative:

- candidate canonical ordering through `compare_candidates`
- MMR seed label `pack.mmr_tiebreak`
- facility-location queue ordering and profile indexes
- section quota order
- omission sorting through the existing output comparators
- context `Deterministic<Seed>` inputs

The arena may hold scratch vectors, temporary signatures, score workspaces,
coverage arrays, or pre-sized buffers. It may not reorder candidates to improve
memory locality unless the reordered form is a private mirror and public
selection still follows the existing comparator and seed semantics.

## Safety And Dependencies

The implementation must keep `#![forbid(unsafe_code)]` true. Arena support must
not introduce Tokio, rusqlite, petgraph, or any other forbidden dependency. If a
new allocation crate is proposed, the child bead must justify it against a
small in-tree request arena or standard-library owned buffers before landing
the dependency.

Arena memory is allowed to contain redacted or unredacted pack scratch data
while the request is active. It must be treated like pack assembly memory:

- no support bundle dumps arena pages
- no tracing field includes raw memory content, raw query text, or provenance
  payloads
- reset failures degrade or disable the arena path instead of producing partial
  output

## Instrumentation Plan

The first metrics are tracing or perf-artifact fields, not additions to
`ee.pack.slo.v1`:

| Field | Meaning |
| --- | --- |
| `arena_mode` | `disabled`, `request_scoped`, or future `workspace_reuse`. |
| `arena_policy_version` | Stable policy id for allocation and reset behavior. |
| `arena_bytes_reserved` | Bytes reserved or committed for the request. |
| `arena_bytes_used` | High-water bytes used by pack assembly scratch data. |
| `arena_reset_count` | Reset count for the request or reused arena generation. |
| `arena_reuse_generation` | Nullable generation id for future workspace reuse. |
| `arena_poisoned` | Whether the arena was disabled after a failed reset or panic boundary. |
| `allocation_count_delta` | Candidate minus baseline allocation count for the measured fixture. |
| `pack_assembly_latency_ms` | Measured assembly latency for this request. |
| `pack_assembly_latency_p95_ms` | Fixture-level p95 in benchmark artifacts. |
| `pack_assembly_latency_p99_ms` | Fixture-level p99 in benchmark artifacts. |
| `fixture_label` | Stable benchmark fixture label, for example `large_provenance_pack`. |
| `candidate_count` | Candidate count entering assembly. |
| `selected_count` | Selected item count. |
| `omitted_count` | Omitted item count. |
| `algorithm_id` | Existing pack selection algorithm id. |

If these fields become public JSON, the implementation must add a versioned
schema or schema update in the same change. Until then, they belong in tracing
events, perf summaries, or test artifacts.

## Verification Matrix

Code follow-ups must prove parity and bounded lifetime behavior with RCH-only
Cargo verification:

| Area | Required proof |
| --- | --- |
| Empty pack | Arena on/off produces the same empty draft, hash, ledger, and renderer output. |
| Max-size pack | Large candidate/provenance fixture stays byte-identical and within memory caps. |
| MMR | Candidate order, coverage fill, omitted order, and marginal gains match arena off. |
| Facility location | Queue order, coverage updates, selected items, and objective values match arena off. |
| Pack hash | `data.pack.hash` and persisted `pack_records.pack_hash` match arena off. |
| Replay ledger | `ledger_json` and `ledger_hash` match arena off. |
| Renderers | JSON and Markdown goldens match arena off; stream trailer pack hash matches batch. |
| Pack DNA | Explain fields match arena off for graph-rich fixtures. |
| Reset | Request-scoped arena drops or resets before any next request can observe scratch data. |
| Workspace reuse | Future reuse mode has poison/reset/cap tests before it can be enabled. |
| Perf | Allocation count and p95/p99 pack assembly latency improve or the feature stays disabled. |

Static checks such as `rustfmt --check` and `git diff --check` can supplement
this matrix, but they do not replace RCH-backed Cargo tests once Rust code
lands.

## Implementation Notes

Start by wrapping scratch allocation behind a private pack-assembly policy, not
by changing the public `PackDraft` type to borrow from an arena. The assembly
body may use arena-backed temporary buffers, but the function should still
return the same owned `PackDraft` while parity is being established.

The safest first target is scratch storage that is already bounded by candidate
count: MMR signatures, `max_selected_similarities`, facility-location coverage
arrays, selection steps, and temporary candidate profiles. Do not arena-store
rendered response JSON, persisted ledger JSON, or support-bundle material in
the first pass.

Workspace reuse is deliberately deferred. It is only justified after
request-scoped mode proves lower allocation count or better p95/p99 latency on
large deterministic fixtures.
