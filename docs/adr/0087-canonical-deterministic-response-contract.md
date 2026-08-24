# ADR 0087: Canonical Deterministic Response and Pack-Hash Contract

Status: proposed
Date: 2026-08-24
Bead: bd-reality-core-convergence-1azkt.1
Depends-on: ADR 0084 (hotset manifest), ADR 0085 (typed pack entity identity)

## Context

The README promises byte-stable JSON and identical pack hashes for equal
state. Today that promise is enforced only over a narrow slice of the true
input space. The production hash path is
`compute_pack_hash_components` (`src/core/context.rs`) which composes five
named sub-hashes (`PackHashComponents`):

| Component | Fed by |
|---|---|
| `pack_request_hash` | query, profile, budget max_tokens, output options, `read_snapshot_generation`, task lens |
| `draft_items_hash` | draft items + `used_tokens` |
| `degraded_summary_hash` | degradation rows |
| `rendered_text_hash` | full markdown render |
| `composite_hash` | the four above |

Gaps against the promise:

1. **Model and execution identity are absent from the pack hash.** The index
   manifest already hashes embedder identity (`hash_embedding_config_str_field`
   over `EmbeddingConfig{model_id, dimension, deterministic}`,
   `src/search/mod.rs`), but two packs assembled under different embedders,
   CPU feature classes, or toolchains can still collide in `pack_hash`.
2. **No declared numeric execution domain.** Scores flow through f32/f64
   platform paths before thresholds and ties; cross-target bit-identity is
   implied but never specified.
3. **Volatile telemetry adjacency.** Durations, PIDs, queue depths live in the
   same response envelope as canonical payload; nothing structurally prevents
   them perturbing hashed fields.
4. **State-creating writes are not separated** from reads: persisting a pack
   mints IDs and timestamps whose generation must never feed the hash.
5. **Partial canonicalization.** `revision_hash` (`src/pack/mod.rs`) already
   does length-prefixed NUL-delimited BLAKE3 (correct encoding discipline),
   but map ordering, Unicode policy, float formatting, negative zero, and
   timestamp precision are conventions, not contract.

A literal, scoped contract must precede any fix (reality-check finding).

## Decision

### 1. Three-way separation

Every machine-facing response partitions into exactly three classes:

| Class | Contents | May enter hash? |
|---|---|---|
| **Canonical product payload** | selected items, scores (post-quantization), order, provenance, degraded posture, omissions | yes |
| **Operational telemetry** | durations, PID, queue depth, arena stats, tracing fields | never |
| **State-creating artifacts** | pack record ID, persisted-at timestamps, audit sequence numbers | never |

Telemetry and state-creating fields MUST live outside every hashed
serialization. Enforcement is structural: the canonical serialization is built
from a closed component list (below), not by stripping a full response.

### 2. Snapshot identity components

Equal snapshot identity is defined as equality of ALL of the following.
Each names its owning source; implementations derive digests from these and
nothing else.

| # | Component | Source of truth |
|---|---|---|
| S1 | store tier + DB read generation | workspace/global/team scope ids; `read_snapshot_generation` |
| S2 | immutable index manifest root / entity-revision root | hotset manifest (ADR 0084); per-entity revision |
| S3 | retrieval subsystem identities | lexical cache epoch, L2 candidate-set key, PPR/plan cache keys |
| S4 | model identity | `EmbeddingConfig{model_id, dimension, deterministic}`, reranker id when active, provider class |
| S5 | request surface | query (NFC bytes), profile, budget, output options, task lens, seed |
| S6 | effective config slice | only keys that can change selection/order/scoring, each individually named and versioned |
| S7 | reference time domain | explicit `as_of` instant + lifecycle cutoffs; wall-clock absence is itself part of identity |
| S8 | authorization/redaction/trust epochs | capability set hash, redaction policy version, trust-class floor |
| S9 | execution domain | target triple class, CPU feature class relevant to declared numeric paths, binary/toolchain digest, enabled features |
| S10 | serialization versions | `ee.pack.v2`, binary format version, canonical-hash algorithm tag |

S4/S9 make cross-machine equality an EXPLICIT claim: two executions agree iff
their declared domains match, or their score paths quantize identically
(§3). Vague "same results everywhere" wording is forbidden by the bead.

### 3. Numeric execution domain

Decision: **quantize decision scores to fixed-point Q20.12 (u32) at the single
choke point where selection thresholds, tie-breaks, and hash inputs consume
them**, before any comparison or serialization. Raw float scores remain
available as telemetry but are excluded from canonical payload and hash.
Tie-break after quantization: lexicographic `(entity_ref, revision)` per
ADR 0085 ordering.

Rationale: quantize-first keeps the strong promise ("identical bytes") without
requiring bit-identical libm across targets; the residual platform variance is
absorbed below the quantum (≤ 2^-12 relative), which is far below every
selection threshold in use. If a future scoring path proves too sensitive for
Q20.12, the fallback is narrowing S9 (declare provider+target inside the
domain) — never silently weakening §5.

### 4. Canonical serialization rules

- Encoding: UTF-8, no BOM, LF newlines; Unicode content preserved byte-wise
  (no NFC normalization — normalization would break memory-content identity);
  all case folding forbidden in canonical output.
- Maps/sets serialized with lexicographic key sort; duplicate keys impossible
  by construction.
- Degraded arrays sorted by `(severity_rank, code, worker_id, message)` —
  already the emission order; contract pins it.
- Timestamps that legitimately appear in canonical payload (e.g., memory
  `created_at` facts) truncated to millisecond precision, UTC, RFC 3339.
- Floats cannot appear (§3 removes them); integers only, negative zero N/A.
- Hash encoding: the existing `revision_hash` length-prefixed NUL-delimited
  BLAKE3 scheme, tagged `blake3-lp1`. Composite = `blake3-lp1` over the
  ordered component-digest list, each component itself tagged with its
  component name string.
- Algorithm tag travels inside S10 so any future change forks identities
  instead of colliding.

### 5. Volatile-data firewall

The canonical serializer consumes ONLY typed structs enumerating §2
components. Telemetry structs are different types and cannot be passed where
canonical input is expected. Any new response field must declare its class at
type level (module convention: `Canonical*` vs `*Telemetry`). A volatile value
that must be visible to agents (e.g., elapsed_ms) appears solely under
`telemetry` envelope siblings, outside `data.pack`.

### 6. Redaction posture

The snapshot identity surfaces ONE field:
`snapshot_identity.digest` (hex, `blake3-lp1`). The full component vector is
logged to the flight recorder (local-only) and NEVER serialized into
agent-facing responses: components contain config-slice and toolchain digests
that are safe as hashes but whose preimages could name private paths. `ee why`
may expose component DIGESTS plus the differing-component NAME on mismatch
(§7), never preimages.

### 7. Differing-state diagnostics

Because components are named and ordered, inequality localizes: comparing two
digests yields the first diverging component name (`store`, `index`,
`retrieval`, `model`, `request`, `config`, `time`, `authz`, `execution`,
`serialization`). `compute_pack_hash_components` already produces this shape;
the contract requires the comparator to be total (all ten S-components) rather
than today's five.

### 8. Versioning and migration

- New optional object on `ee.pack.v2` data: `snapshotIdentity: {version: 1,
  digest, numericDomain: "q20.12", componentDigestsAvailableLocally: true}`.
- Additive; old consumers ignore it; no compatibility shim. Migration note
  appended to `docs/migration_v0_1_to_v0_2.md` lineage.
- Pack-record persistence stores `{digest, component_digests}` so replay
  (bd-…replay surfaces) can re-derive and compare without the original env.
- Hash-input version bump rule: ANY change to §2 membership, §3 quantum, or §4
  rules bumps `snapshotIdentity.version` and forks digests. Golden fixtures
  pin one version explicitly.

### 9. Verification plan (all RCH-only)

| Layer | Harness | Asserts |
|---|---|---|
| Unit | extend `tests/determinism_unit.rs` | per-component digests stable under re-order-independent construction; quantization ties resolve by §3 |
| Property | new `tests/pack_hash_property.rs` | perturbation matrix: mutate each S-component → digest changes AND comparator names it; mutate ONLY telemetry → digest unchanged (negative test) |
| Cross-process | extend `scripts/e2e_overhaul/determinism.sh` | same fixture, two processes, same digest bytes |
| Golden | fixture under `tests/fixtures/` | pinned `snapshotIdentity.version=1` digest vector |
| State-creation separation | golden | two consecutive runs differ ONLY in state-creating fields |

## Consequences

- `compute_pack_hash_*` gains S1–S10 component derivation; five-component
  `PackHashComponents` becomes the leaf layer under a ten-component snapshot
  tree. Callers unaffected (same composite entry point).
- Selection choke point gains one quantization step; perf impact bounded to
  one integer conversion per scored candidate.
- Cross-machine pack equality becomes a DECLARED claim scoped by S4+S9 rather
  than an accident; support bundles gain a one-line equality answer via §7.
- Implementation is deliberately staged: this ADR is the contract;
  `-azkt.5` implements the verification manifest + pinned runner;
  `-azkt.10` builds the regression oracle; `-azkt.18` closes hermeticity.

## Rejected alternatives

- **Declare target+provider inside the domain instead of quantizing** — kept
  as documented fallback; rejected as primary because it weakens the README
  promise to "identical per machine" and makes every CPU difference a hash
  fork.
- **Normalize Unicode in canonical payload** — breaks content-addressed
  memory identity (two byte-distinct memories must stay distinct).
- **Strip telemetry from the existing response serializer** — stripping is
  retroactive and provably incomplete; closed-component construction is the
  enforceable form.
