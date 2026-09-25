# ADR 0087: Canonical Deterministic Response and Pack-Hash Contract

Status: accepted
Date: 2026-08-24
Updated: 2026-09-24 (pack-hash input v2)
Bead: bd-reality-core-convergence-1azkt.1
Depends-on: ADR 0084 (hotset manifest), ADR 0085 (typed pack entity identity)

## Context

The README promises byte-stable JSON and identical pack hashes for equal
state. Before v2 that promise held over a narrow slice of the true input
space, and the census on bd-reality-core-convergence-1azkt.1 (c10108, read at
a43c82613) found the v1 hash weaker than this ADR claimed:

1. Every per-item field was fed to blake3 as raw adjacent bytes, with no label
   and no length, so different field sequences could feed identical bytes.
   Under the Lean output profile, provenance `("file://a", "bc")` and
   `("file://ab", "c")` produced one `pack.hash`.
2. The composite re-fed the raw fields instead of hashing the component
   digests, so a differing composite could not name what differed.
3. Evidence-span scores entered the hash as raw f32 bytes, not Q20.12.
4. The degraded slice entered the hash in emission order with duplicates.
   Whether the wall-clock timing entry was excluded depended on call order:
   the refresh after an L2 store added a degradation hashed a slice that
   already held the timing entry.
5. Model and execution identity, reference time, tiers, authorization and
   policy epochs are absent from the pack hash.
6. Scores reach selection thresholds and ties as raw f32, and JSON item scores
   are six-decimal display copies: two score domains.

A literal, scoped contract must precede any fix (reality-check finding). v2 is
that contract for the pack hash: it states exactly which components are bound
and names the bead that owns every component that is not.

## Decision

### 1. Three-way separation

Every machine-facing response partitions into exactly three classes:

| Class | Contents | May enter hash? |
|---|---|---|
| **Canonical product payload** | selected items, hashed scores (Q20.12), order, provenance, canonical degraded posture, omissions | yes |
| **Operational telemetry** | durations, PID, queue depth, arena stats, tracing fields, the `pack_assembly_elapsed_over_budget` degradation | never |
| **State-creating artifacts** | pack record ID, persisted-at timestamps, audit sequence numbers | never |

Non-canonical telemetry degradation codes are listed once, in
`NON_CANONICAL_TELEMETRY_DEGRADATION_CODES` (`src/pack/mod.rs`). The pack hash
drops them by construction, and the volatile registry
(`src/obs/volatile_fields.rs`) reads the same list. The timing entry still
appears in the envelope's `degraded[]`. Moving it into a dedicated telemetry
field is a deferred schema decision owned by
`bd-pack-timing-telemetry-field-2pfzo`.

`pack.text` is canonical but not the hashed bytes; the hash binds its
inputs. The shipped text is rendered without non-canonical telemetry entries,
so a timing overrun changes neither its degradation bullets nor the counts
and banner derived from them (`render_context_response_markdown_with_options`,
`src/output/mod.rs`). The shared timing normalizers
(`src/obs/volatile_fields.rs`) therefore leave `pack.text` untouched, and a
timing bullet found there is rejected as a regression rather than scrubbed.
The JSON `data.pack.advisoryBanner.degradationCount` is a per-run report and
counts every entry in `degraded[]`, the timing entry included, so when an
overrun fires `pack.text` counts one fewer degraded signal than the banner
(ruled 2026-09-25; pinned by
`shipped_text_and_banner_disagree_by_exactly_the_volatile_entries` in
`tests/pack_hash_property.rs`). The `rendered_text` component hashes the pack-layer
rendering, which differs from the shipped text (the shipped text adds the
embed-backend line, the pack-DNA block and a footer carrying the hash itself).
Making the hash bind the shipped bytes is owned by
`bd-pack-hash-shipped-text-8nafb`.

State-creating commands are specified separately, owned by
`bd-state-creating-spec-8hjtj`.

### 2. Snapshot identity components

The full target identity is S1–S10:

| # | Component | Source of truth |
|---|---|---|
| S1 | store tier + DB read generation | workspace/global/team scope ids; `read_snapshot_generation` |
| S2 | immutable index manifest root / entity-revision root | hotset manifest (ADR 0084); per-entity revision |
| S3 | retrieval subsystem identities | lexical cache epoch, L2 candidate-set key, PPR/plan cache keys |
| S4 | model identity | `EmbeddingConfig{model_id, dimension, deterministic}`, reranker id when active, provider class |
| S5 | request surface | query (trimmed bytes, no NFC), profile, budget, output options, task lens, task paths |
| S6 | effective config slice | only keys that can change selection/order/scoring, each individually named and versioned |
| S7 | reference time domain | explicit `as_of` instant + lifecycle cutoffs; wall-clock absence is itself part of identity |
| S8 | authorization/redaction/trust epochs | capability set hash, redaction policy version, trust-class floor |
| S9 | execution domain | target triple class, CPU feature class relevant to declared numeric paths, binary/toolchain digest, enabled features |
| S10 | serialization versions | `ee.pack.v2`, hash input schema, canonical-hash construction |

#### 2a. What v2 binds, literally

`pack.hash` under input schema `ee.pack.hash_input.v2` is a function of
exactly these components and nothing else
(`compute_pack_hash_components`, `src/core/context.rs`):

| v2 component | Binds |
|---|---|
| `request` | query bytes (trimmed, no NFC), request profile, `budget.max_tokens`, output profile, resource profile, the five `include_*` output flags, `read_snapshot_generation` (S1, generation only), task lens id/version/hash, normalized task paths |
| `items` | `used_tokens`; every selected item (id, rank, section, content, estimated tokens, Q20.12 relevance/utility/proximity/score breakdown, attempt-family multiplicity, why, selection phase, provenance URIs and notes, diversity key, trust class and subclass, the procedural-rule posture policy, tombstone, lifecycle, redactions, freshness facets and anchors); every evidence span with Q20.12 scores |
| `omitted` | every omission (id, estimated tokens, reason, attempt-family multiplicity) |
| `degraded` | the canonical degraded set: telemetry codes dropped, sorted by (code, severity, message, repair), exact duplicates removed |
| `coordination` | the coordination snapshot, when present |
| `rendered_text` | the pack-layer markdown rendered from `request`, `items`, `omitted`, the canonical degraded set and `coordination` |

The composite is blake3 over the schema tag and the tagged component
digests, in this order: `request`, `items`, `omitted` (only when skipped items
are shown), `degraded`, `coordination`, `rendered_text` (only when the text is
shown). Equal composites therefore mean equal bound components, and a
differing composite is localized by comparing the component digests.

#### 2b. What v2 does not bind, and who owns it

| Component | Status in v2 | Owning bead |
|---|---|---|
| S1 store tiers / scope identity | not bound | `bd-pack-identity-tiers-vxx8l` |
| S2 index manifest / entity-revision root | not bound | `bd-reality-core-convergence-1azkt.2` |
| S3 retrieval subsystem identities (lexical cache, L2 candidate set, PPR/plan caches) | not bound | `bd-reality-core-convergence-1azkt.2` (lexical cache), `bd-reality-core-convergence-1azkt.3` (cache identity under concurrency) |
| S4 model identity | not bound | `bd-reality-core-convergence-1azkt.2` |
| S6 effective config slice (candidate pool, max results, sections, speed, source mode, filters, include-tombstoned/expired/future, relevance floor, seed) | not bound except through selected items | `bd-pack-identity-config-slice-188z4` |
| S7 reference time (`as_of`, or the silent `Utc::now` at `context.rs` `reference_time`) | not bound | `bd-pack-identity-asof-35viu` |
| S8 authorization / capability / agent | not bound | `bd-pack-identity-authz-ctr51` |
| S8 trust / redaction / security policy epochs | per-item results bound, policy versions not | `bd-pack-identity-trust-epochs-b7cq9` |
| S9 execution domain | not bound; see §3 | `bd-pack-identity-exec-domain-junoz` |
| S5 tokenizer identity, Unicode normalization, locale, line endings | query hashed as trimmed bytes; nothing normalized; tokenizer not bound | `bd-pack-identity-unicode-gitmy` |
| Selection thresholds/ties on Q20.12; one score domain for JSON | not done | `bd-reality-core-convergence-1azkt.11` |
| Cross-process and cross-host determinism gates | not in v2 | `bd-reality-core-convergence-1azkt.3` |

### 3. Numeric execution domain

Every score the pack hash consumes is quantized to fixed-point Q20.12 (u32)
first (`quantize_q20_12`, `src/pack/mod.rs`): relevance, utility, proximity,
score breakdown and attempt-family discount factors of selected items, and
relevance and utility of evidence spans. Sub-quantum IEEE-754 noise (below
2^-12) cannot fork `pack.hash`; negative zero and positive zero quantize alike.
Non-finite and negative inputs quantize to 0, so NaN and 0 would collide.
Relevance and utility are `UnitScore` and cannot be non-finite; proximity and
score-breakdown values are plain f32 and are not validated at this point.

Selection thresholds and tie-breaks still compare raw f32, and JSON item
scores are six-decimal display copies. Until
`bd-reality-core-convergence-1azkt.11` moves them onto the quantized domain,
two runs agree on `pack.hash` only when their raw-f32 selection agrees, which
is guaranteed within one binary on one target and is NOT claimed across
targets. That execution domain is owned by
`bd-pack-identity-exec-domain-junoz`.

### 4. Canonical serialization rules (hash input)

- Every field is fed as `len(label) u64 LE ‖ label ‖ len(value) u64 LE ‖ value`
  (`hash_labeled_bytes`). Optional fields add a labeled presence flag, and
  repeated fields a labeled count.
- Every component opens with the labeled schema tag `ee.pack.hash_input.v2`
  and its component name.
- The composite is `blake3` over the labeled schema tag and the labeled
  component digests (§2a).
- Unicode: bytes are hashed as stored. The query is trimmed and hashed as
  bytes with no NFC; memory content is never normalized (normalization would
  break content identity). Line endings are not normalized. Both are owned by
  `bd-pack-identity-unicode-gitmy`.
- Degraded entries: canonicalized in the hash input as in §2a. Emission order
  in `degraded[]` is a separate, presentational contract.
- No timestamps are hash inputs in v2.

### 5. Volatile-data firewall

The hash consumes only the fields listed in §2a, built from typed pack
structs; telemetry never reaches it. The degraded-slice firewall is enforced
inside the hash function (§1), not by call order, so every
`refresh_context_pack_hash` call site is covered, including the refresh after
an L2 store, whose slice already holds the timing entry.

### 6. Redaction posture

`data.pack.snapshotIdentity` exposes the composite digest and the per-component
DIGESTS (`blake3:<hex>`), never preimages. Component preimages can contain
absolute provenance paths (`file://` URIs are hashed verbatim) and query
text; a digest reveals neither.

### 7. Differing-state diagnostics

`snapshotIdentity.components` names six components. Comparing two responses
field by field names the component that differs; `renderedText` is derived
from the others and moves exactly when a rendered input moves. No CLI
comparator is part of v2.

### 8. Versioning and migration

`data.pack.snapshotIdentity` on `ee.pack.v2`:

```json
{
  "version": 2,
  "inputSchema": "ee.pack.hash_input.v2",
  "digest": "<pack.hash>",
  "numericDomain": "q20.12",
  "componentDigestsAvailableLocally": true,
  "components": {
    "request": "blake3:…", "items": "blake3:…", "omitted": "blake3:…",
    "degraded": "blake3:…", "coordination": "blake3:…", "renderedText": "blake3:…"
  }
}
```

- `componentDigestsAvailableLocally` is `false`, and `components` is absent,
  for a hand-built response whose hash no pack run computed. An L2 cache hit
  replays the stored response JSON, including the snapshot identity stored
  with it.
- Self-identification: `pack.hash` keeps its `blake3:<64 hex>` shape, because
  `ee.pack.diff.v2` and `ee.pack.replay.v2` publish `packHash` with the pattern
  `^blake3:[0-9a-f]{64}$`. The version travels beside the hash in every emitted
  pack (`version`, `inputSchema`), and the schema tag is bound into the
  preimage, so a v1 and a v2 digest cannot coincide by construction.
- `pack_records.pack_hash` has no version column. Rows written before v2 carry
  v1 hashes and are not comparable with v2 hashes; nothing in the row says
  which it is. This is accepted and documented; no migration.
- The L2 pack cache key schema moved to `ee.pack.l2_cache_key.v7`, so a
  response cached under v1 misses rather than replaying a v1 identity.
- Bump rule: any change to §2a membership, §3 quantization or §4 encoding
  bumps `snapshotIdentity.version` and the input schema tag, and forks
  digests. No compatibility shim, no dual hash.

### 9. Verification (all RCH-only)

| Layer | Harness | Asserts |
|---|---|---|
| Unit | `src/core/context_test_module.rs` `pack_hash_v2_*` | the flat-feed provenance collision is separated (red first against v1); each differing input moves its own component and the composite only; timing, order and repetition move nothing; evidence scores quantize |
| Property | `tests/pack_hash_property.rs` (in `integration_property`) | no elapsed reading moves `pack.hash`, nor one byte of the shipped `pack.text` (red first against the phase-1 tip); the timing entry stays in the envelope's `degraded[]`; degraded order and repetition never move the hash; sub-quantum noise never does and a one-quantum step always does; a pinned v2 digest vector, checked on two RCH workers |
| Existing | `determinism_unit`, `property_query_and_pack`, `pack_envelope_byte_identical_*` | unchanged determinism evidence |

None of these tests uses a test-side normalizer.

## Consequences

- Every pack hash changes once (v1 to v2). Goldens pinning `pack.hash` or
  `snapshotIdentity` move, and their diffs are limited to those fields.
- Cross-machine pack equality is a declared, bounded claim (§3), not an
  accident.
- The excluded components have owners (§2b); none is silently claimed.

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
- **A self-identifying hash prefix** (e.g. `blake3-lp1:` or
  `ee.pack.v2:blake3:`) — would break the published `packHash` pattern in
  `ee.pack.diff.v2` and `ee.pack.replay.v2`.
