# ADR 0087: Canonical Deterministic Response and Pack-Hash Contract

Status: accepted
Date: 2026-08-24
Updated: 2026-11-16 (pack-hash input v7, concrete store and scope identity)
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

v3 adds the effective reference-time domain (S7, `bd-pack-identity-asof-35viu`).
Two requests with different explicit clocks now have different identities even
when they select exactly the same items. An omitted clock binds an explicit
wall-clock marker; it does not bind a volatile per-run clock reading.

v4 adds the quality policy now executed after authoritative search admission
(`bd-6oqlx`). It binds the resolved active coefficients even when two policies
select identical items. This owns the quality-scoring part of S6.

v5 closes `bd-pack-hash-shipped-text-8nafb`: the text component now uses the
actual output renderer after backend, consensus/conflict analysis and requested
Pack DNA are present. One finalization boundary freezes prompt text and identity
before ledger persistence, baseline recording, audit and cache publication.

v6 adds the remaining effective retrieval/assembly controls in S6
(`bd-pack-identity-config-slice-188z4`). Two policies that happen to produce the
same selected items and prompt now have distinct `effectiveConfig` digests.
Equivalent sets/defaults share an identity; telemetry and audit IDs remain out
of the policy. This is a contract change, not runtime closure evidence for that
bead or the broader cross-host determinism work.

v7 adds addressed store and scope identity in S1
(`bd-pack-identity-tiers-vxx8l`). Equal generations and selected content no longer
make different workspace/global addresses or team memberships hash alike.
The new `storeScopes` component carries only a digest. It identifies addressed
scopes and observed bindings; it does not establish immutable content roots or
an atomic snapshot spanning multiple stores.

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
(`src/obs/volatile_fields.rs`) reads the same list.

#### 1a. Phase-2 timing telemetry schema decision (`bd-pack-timing-telemetry-field-2pfzo`)

Under `ee.response.v2` and `ee.pack.v2`, `data.pack.slo` (`ee.pack.slo.v1`) is
the dedicated structured telemetry block for pack latency and budget posture
(`actuals.elapsedMs`, `elapsedStatus`, `status`, and `slo.degradations`). At
the same time, `pack_assembly_elapsed_over_budget` intentionally remains in the
response envelope's `degraded[]` array (`src/core/context.rs`) to preserve the
GH49 / `bd-jikgj` contract that any caller inspecting `degraded[]` sees an
elapsed-budget overrun without having to parse `data.pack.slo`. Because
`NON_CANONICAL_TELEMETRY_DEGRADATION_CODES` is excluded before canonical prompt
rendering, advisory-banner retrieval-signal counting (`bd-ophw7`), and pack-hash
finalization, keeping the overrun entry in `degraded[]` alongside `data.pack.slo`
requires no compatibility shim and never forks `pack.hash` or `pack.text`.

#### 1b. State-creating command specification (`bd-state-creating-spec-8hjtj`)

State-creating commands mint durable entity IDs, audit sequence/IDs, index-queue
job IDs, and creation timestamps on purpose. Their canonical registry lives in
`STATE_CREATING_COMMAND_SPECS` (`src/obs/volatile_fields.rs`), and two
consecutive runs of the same command against equivalent workspace state must
differ only in these declared fields (plus registered operational telemetry):

| Command | Minted ID fields | Minted timestamp fields | Derived integrity fields |
|---|---|---|---|
| `remember` | `id`, `memoryId`, `memory_id`, `auditId`, `audit_id`, `indexJobId`, `index_job_id`, `logicalId`, `logical_id` | `createdAt`, `created_at`, `updatedAt`, `updated_at` | `provenanceChainHash`, `provenance_chain_hash` |
| `pack` (persisted) | `packRecordId`, `pack_record_id`, `recordId`, `record_id`, `auditId`, `audit_id` | `persistedAt`, `persisted_at`, `createdAt`, `created_at` | *(none — `pack.hash` and `snapshotIdentity` remain identical)* |
| `handoff` (`create`) | `capsuleId`, `capsule_id`, `auditId`, `audit_id` | `createdAt`, `created_at`, `generatedAt`, `generated_at` | `integrity`, `capsuleHash`, `capsule_hash` |
| `journal` | `id`, `entryId`, `entry_id`, `sessionId`, `session_id`, `memoryId`, `memory_id`, `auditId`, `audit_id`, `indexJobId`, `index_job_id` | `createdAt`, `created_at`, `updatedAt`, `updated_at`, `recordedAt` | `provenanceChainHash`, `provenance_chain_hash` |
| `decide` | `id`, `decisionId`, `decision_id`, `memoryId`, `memory_id`, `auditId`, `audit_id`, `indexJobId`, `index_job_id` | `decidedAt`, `createdAt`, `created_at`, `updatedAt`, `updated_at` | `provenanceChainHash`, `provenance_chain_hash` |
| `curate` | `candidateId`, `candidate_id`, `auditId`, `audit_id`, `indexJobId`, `index_job_id` | `createdAt`, `created_at`, `updatedAt`, `updated_at`, `reviewedAt`, `reviewed_at`, `appliedAt`, `applied_at`, `ttlExpiresAt`, `ttl_expires_at` | *(none)* |
| `backup` (`create`) | `backupId`, `backup_id`, `auditId`, `audit_id` | `createdAt`, `created_at`, `completedAt` | `archivePath`, `manifestPath` |

Resource-limit and admission-refusal degradations remain canonical: an empty
pack refused admission must remain distinguishable from an admitted empty pack.
Their message and repair text bind the posture and configured limits, never
the observed queue depth, backoff interval, candidate count, or graph traversal
count (`bd-ve7c6`). Those measurements remain available in `data.pack.slo` under
`actuals` and `admission`. Equal postures under the same resource profile thus
render and hash identically even when their measurements differ; a changed
posture or selected item still changes identity. The exact-cap
`pack_assembly_slow` warning is unchanged because its condition fixes the count.

`pack.text` is the finalized output renderer's exact UTF-8 text. Its
`rendered_text` digest has one explicit self-reference exception: the value in
the terminal `<!-- pack.hash: VALUE -->` footer is rendered as the fixed
`ee.pack.hash.self.v1` placeholder while hashing. Delivery renders the actual
composite digest in that same typed footer slot. Every other byte is identical,
including the backend line, requested DNA, analysis, filtered and aggregated
degradations, schema footer and trailing newline. This is not a claim that
`pack.hash` equals raw `blake3(pack.text)`; it remains the component composite.
The footer value is an explicit renderer argument, never a global replacement
of query or memory text.

Canonical text is frozen before persistence. Markdown and JSON reuse those
bytes, including under Lean: JSON may omit its text field, but the composite
still binds the prompt that Markdown can deliver. DNA is requested through
`ContextPackOutputOptions::with_pack_dna` and reads the same held database
snapshot as selection. The standalone DNA builder helper leaves already
finalized responses unchanged.

Elapsed telemetry, cache-publication failures and later delivery notices
(such as alias or delta-fallback notices) remain visible in the response
envelope without rewriting canonical text or its persisted hash. Consequently
the JSON advisory banner can count more signals than the frozen prompt. The
timing-only case remains pinned by
`shipped_text_and_banner_disagree_by_exactly_the_volatile_entries` in
`tests/pack_hash_property.rs`. The shared timing normalizers leave `pack.text`
untouched; they do not scrub a wrong prompt into agreement.

### 2. Snapshot identity components

The full target identity is S1–S10:

| # | Component | Source of truth |
|---|---|---|
| S1 | store tier + DB read generation | workspace/global/team scope ids; `read_snapshot_generation` |
| S2 | immutable index manifest root / entity-revision root | hotset manifest (ADR 0084); per-entity revision |
| S3 | retrieval subsystem identities | lexical cache epoch, L2 candidate-set key, PPR/plan cache keys |
| S4 | model identity | `EmbeddingConfig{model_id, dimension, deterministic}`, reranker id when active, provider class |
| S5 | request surface | query (CRLF/CR normalized to LF, trimmed, Unicode NFC via `canonicalize_pack_query`), profile, budget, output options, task lens, task paths; tokenizer/Unicode/locale policy versions bound in `effective_config` |
| S6 | effective config slice | only keys that can change selection/order/scoring, each individually named and versioned |
| S7 | reference time domain | explicit `as_of` instant + lifecycle cutoffs; wall-clock absence is itself part of identity |
| S8 | authorization/redaction/trust epochs | capability policy version + self/team producer eligibility in `store_scopes`; redaction bypass/categories/level, trust-policy version, redaction-policy version, and security epoch in `effective_config` |
| S9 | execution domain | declared `PackExecutionDomain` (`ee.pack.execution_domain.v1`: numeric score domain `q20.12`, selection tie domain `ieee754_f32_total_cmp_v1`, endianness, enabled features, target arch/OS) bound in `effective_config` |
| S10 | serialization versions | `ee.pack.v2`, hash input schema, canonical-hash construction |

#### 2a. What v7 binds, literally

`pack.hash` under input schema `ee.pack.hash_input.v7` is a function of
exactly these components and nothing else
(`compute_context_pack_hash_components`, `src/core/context.rs`):

| v7 component | Binds |
|---|---|
| `request` | query bytes (LF-normalized, trimmed, Unicode NFC via `canonicalize_pack_query`), request profile, `budget.max_tokens`, output profile, resource profile, coverage/text/skipped/meta/verbose-meta output flags, non-affecting-degradation visibility, requested Pack DNA, `read_snapshot_generation` (S1, generation only), task lens id/version/hash, normalized task paths |
| `reference_time` | S7: `mode=explicit` and the effective UTC millisecond reference instant, or `mode=wall_clock` when no explicit clock was supplied |
| `quality_scoring` | Explicit presence plus the execution-captured quality policy digest: algorithm version and the eight active resolved coefficients. Direct builders without retrieval bind absence. |
| `items` | `used_tokens`; every selected item (id, rank, section, content, estimated tokens, Q20.12 relevance/utility/proximity/score breakdown, attempt-family multiplicity, why, selection phase, provenance URIs and notes, diversity key, trust class and subclass, the procedural-rule posture policy, tombstone, lifecycle, redactions, freshness facets and anchors); every evidence span with Q20.12 scores |
| `omitted` | every omission (id, estimated tokens, reason, attempt-family multiplicity) |
| `degraded` | the degraded set at finalization: telemetry codes dropped, sorted by (code, severity, message, repair), exact duplicates removed; later delivery notices are excluded |
| `coordination` | the coordination snapshot, when present |
| `rendered_text` | exact finalized output-renderer markdown, including backend, consensus/conflict analysis and requested DNA, with only the typed terminal hash-footer value replaced by the declared placeholder (§1) |
| `effective_config` | S5/S6/S8/S9: effective capped candidate pool, optional result limit, section set, and the captured runtime policy described in §3a (including trust/redaction/security policy epochs, execution domain, and tokenizer/Unicode/locale policies); direct builders explicitly bind runtime-policy absence |
| `store_scopes` | S1/S8: authorization capability policy version (`PACK_AUTHZ_CAPABILITY_POLICY_V1`), concrete database/root addresses and actual workspace bindings; requested scope and strictness; global-tier inclusion posture; current producer for self/team scopes; canonical team membership and roster availability (§3b). Direct builders without stores bind explicit absence. |

The composite is blake3 over the schema tag and the tagged component
digests, in this order: `request`, `reference_time`, `quality_scoring`, `items`, `omitted` (only when skipped items
are shown), `degraded`, `coordination`, `rendered_text` (always, including when
JSON omits text), `effective_config`, `store_scopes`. Equal composites therefore mean equal bound components, and a
differing composite is localized by comparing the component digests.

#### 2b. What v7 does not bind, and who owns it

| Component | Status in v7 | Owning bead |
|---|---|---|
| S2 index manifest / entity-revision root | not bound | `bd-reality-core-convergence-1azkt.2` |
| S3 retrieval subsystem identities (lexical cache, L2 candidate set, PPR/plan caches) | not bound | `bd-reality-core-convergence-1azkt.2` (lexical cache), `bd-reality-core-convergence-1azkt.3` (cache identity under concurrency) |
| S4 model identity | backend label is bound through text; full model/provider/weight identity is not | `bd-reality-core-convergence-1azkt.2` |
| S8 authorization / capability / agent | bound in `store_scopes` (`PACK_AUTHZ_CAPABILITY_POLICY_V1` + self/team producer eligibility) and `effective_config` (redaction bypass/categories/level); ambient `AGENT_NAME` in shared `workspace`/`swarm`/`global` scopes is intentionally excluded | closed (`bd-pack-identity-authz-ctr51`) |
| S8 trust / redaction / security policy epochs | bound in `effective_config` (`PACK_TRUST_POLICY_VERSION_V1`, `PACK_REDACTION_POLICY_VERSION_V1`, `PACK_SECURITY_EPOCH_V1`) | closed (`bd-pack-identity-trust-epochs-b7cq9`) |
| S9 execution domain | declared by `PackExecutionDomain` (`ee.pack.execution_domain.v1`) and bound in `effective_config`; see §3 | closed (`bd-pack-identity-exec-domain-junoz`) |
| S5 tokenizer identity, Unicode normalization, locale, line endings | query LF-normalized, trimmed, and NFC-normalized (`canonicalize_pack_query`); content kept byte-exact; tokenizer/Unicode/locale policies bound in `effective_config` | closed (`bd-pack-identity-unicode-gitmy`) |
| Selection thresholds/ties on Q20.12; one score domain for JSON | not done | `bd-reality-core-convergence-1azkt.11` |
| Cross-process and cross-host determinism gates | not established by v7 | `bd-reality-core-convergence-1azkt.3` |

### 2c. Executed quality policy

Frankensearch continues to own retrieval, fusion and reranking. EE scores a
bounded overretrieved pool after source, seal, scope and mesh admission, then
applies the requested final limit. The original engine relevance and relevance
floor remain separate from the quality ranking score.

The active observations are recency, confidence, utility, harmful feedback,
native rule maturity, and an authoritative scope match. Memory feedback is
read in batches from the owning database with the request cutoff. Native
rule counters are observed only when their source timestamp can support that
cutoff. Missing observations remain explicitly unavailable and have neutral
multipliers; a workspace read does not establish a separate global store's
feedback. Exact artifact anchors are derived once from the query and compared
with anchors extracted from each already admitted live memory body using the
same precision extractor as ingestion. Every distinct matching kind/hash pair
adds 0.04, up to the configured cap. Indexed content and anchor metadata do not
establish authority. Native rules and other candidates without an admitted full
memory body keep this signal unavailable. Graph centrality, redundancy, drift
and bead observations remain unconnected to this scoring pass.

The nine `[scoring]` controls are `recency_tau_days`, `confidence_floor`,
`utility_floor`, `harmful_penalty_per_hit`, `harmful_penalty_floor`,
`scope_match_bonus`, `candidate_multiplier`, `established_multiplier`, and
`anchor_match_bias_cap`. The anchor cap defaults to 0.08 and accepts finite
values from 0.0 through 0.08. Set `scoring.anchor_match_bias_cap = 0.0` to disable
the boost, or reduce the cap to keep a smaller exact-match preference. For
example, a workspace can set:

```toml
[scoring]
anchor_match_bias_cap = 0.02
```

The policy digest binds their resolved f32 bits and a versioned algorithm tag,
rather than config file bytes, Debug formatting, or inactive controls.

Pack candidates use the weighted ranking divided by a common bound from
the active scope/maturity coefficients plus the anchor cap when the query has
precision anchors. Queries without precision anchors retain their original
scores and projection bound. The same bound applies to every lane;
scores above one retain their ordering instead of collapsing at a clamp.
The raw ranking remains available for search ordering and graph seeds.
`metadata.qualityScoring` and `--explain` expose the measured components
without relabeling engine relevance as a calibrated quality estimate.
Anchor diagnostics expose the exact-match count, capped score and live-content
source without adding raw paths or query surfaces. An observed non-match is
zero; an unavailable signal is null with a neutral zero contribution.

Daemon handoffs bind the scoring policy and explicit reference clock, and
recompute trusted scores after source revalidation. Reuse requires positive
proof that the workspace feedback set was and remains empty; otherwise the
caller retrieves again. This conservative restriction avoids certifying a
stale top-K list using a feedback count that misses in-place edits.

### 3. Numeric execution domain

Structured selected-item and evidence score fields are quantized to Q20.12
(u32) first (`quantize_q20_12`, `src/pack/mod.rs`): relevance, utility, proximity,
score breakdown and attempt-family discount factors of selected items, and
relevance and utility of evidence spans. Sub-quantum IEEE-754 noise (below
2^-12) cannot fork `pack.hash`; negative zero and positive zero quantize alike.
Non-finite and negative inputs quantize to 0, so NaN and 0 would collide.
Relevance and utility are `UnitScore` and cannot be non-finite; proximity and
score-breakdown values are plain f32 and are not validated at this point.
The `numericDomain` label describes those structured score components. The
rendered-text component binds displayed numeric strings exactly, including
DNA explanations; it does not normalize or quantize already-rendered text.

Selection thresholds and tie-breaks still compare raw f32, and JSON item
scores are six-decimal display copies. `PackExecutionDomain`
(`ee.pack.execution_domain.v1`, `src/pack/mod.rs`, `bd-pack-identity-exec-domain-junoz`)
declares the execution domain literally: `numeric_score_domain = "q20.12"`,
`selection_tie_domain = "ieee754_f32_total_cmp_v1"`, `endianness`, sorted
`enabled_features` (`embed-fast`, `fts5`, `graph`, `json`, `lexical-bm25`),
`target_arch`, and `target_os`. Its `portable_domain_key()` is bound into
`effectiveConfig` (`execution.domain`), so two little-endian targets compiled
with the same feature set claim equality when their raw-f32 selection agrees,
while any change to the numeric domain, tie domain, endianness, feature set, or
explicit `target_domain_key()` moves `effectiveConfig` and is named by
`PackHashComponentDigests::differing_components`. Until
`bd-reality-core-convergence-1azkt.11` moves selection thresholds onto the
quantized domain, cross-target equality is conditional on equal raw-f32
selection.

### 3a. Effective retrieval and assembly policy

The `ee.pack.effective_config.v1` runtime slice has individually labeled fields:

| Control | Effective representation |
|---|---|
| Retrieval | speed, requested/applied source mode, strict source, memory scope and strict scope; configured rerank mode and active top-K; actual normalized lexical/semantic RRF weights only when hybrid retrieval executes |
| Visibility | tombstone, expired, future and stale inclusion after query-validity posture is applied |
| Filters | conjunctive metadata field/operator/typed-value atoms; tag sets; trust minimum/exclusions/posture; redaction bypass and allowed categories; temporal row bounds and validity posture; enabled graph seeds/direction/hops/link types/orphan policy |
| Assembly | explicit relevance-floor presence and value; output redaction level and enablement; fresh-sentinel requirement; normalized changed-symbol set and git selection; pagination limit/offset; enabled coordination staleness limit |
| Resolved workspace controls | memory-tier admission, PPR/HITS/proximity gates, active PPR weight, LOD enablement and reduced full/preview/link ratios |

The request component already binds the actual capped/adaptive token budget,
profile, output controls, task lens and normalized task paths. The quality
component binds the executed scoring coefficients. Runtime controls are captured
before L2 admission; the same captured graph/admission/LOD values are used by
selection. Fusion values are carried from actual retrieval, including validated
daemon handoffs, rather than reread from a potentially changed config file.

Sets are sorted and deduplicated; disabled graph/coordination/LOD subsettings
and disabled rerank top-K are absent. Equivalent LOD ratios and available fusion
arm ratios canonicalize. Configuration thresholds retain their effective IEEE
bits rather than score quantization, with negative zero folded into zero.
`None` relevance floor means automatic behavior and differs from explicit zero.
Temporal row bounds retain their actual precision; the lifecycle clock is bound
separately in `referenceTime`. Explicit defaults and omitted defaults bind alike
when their executed behavior is alike.

The deterministic token currently supplies audit/persistence IDs and trace
labels; pack ties and equivalent search scores use stable item comparisons,
not seeded random choice. Consequently audit seed, cache eligibility/hit state,
elapsed time, record paths, and persistence/baseline requests are excluded.
Introducing seeded selection requires binding its effective seed and a version
bump. This slice does not claim model/authorization identities owned in §2b,
nor does it emit raw configuration or filter values. Addressed stores are
bound separately below.

### 3b. Addressed store and scope identity

The `ee.pack.store_scopes.v1` slice records the primary database address, addressed
workspace root and resolved workspace row ID. It uses the held primary read
connection, including custom workspace bindings. Distinct roots that have no
row in a shared database remain distinct. A read failure is never substituted
for a successfully observed empty binding.

Database and root addresses are domain-separated digests of canonical path
bytes; inaccessible paths fall back to their absolute spelling. Resolvable
symlink and dot-segment aliases canonicalize. Size, modification time, inode
and ordinary writes do not change address identity. Relocating a store to a
different canonical address changes this component, even if its rows and
generations match. Replacing contents at the same address with the same binding
does not establish a new content identity: immutable roots remain S2 work.
The local L2 key retains its independent file-replacement guard.

Global participation follows the same scope and inclusion gates used by
retrieval: swarm/workspace/global scope, store presence, participation and
`include_global` policy. Disabled or out-of-scope tiers are not opened or named.
An included tier binds its address and actual existing workspace row, including
a custom ID. Unavailable path/policy/binding states, absence and opt-out reasons
have separate markers. Global binding metadata is observed before assembly and
L2 admission; this extra read does not attest the earlier global source snapshot,
its content generation, or an atomic snapshot spanning both stores.

Current producer identity enters only self/team scopes. Team scope also binds
the sorted, deduplicated roster and whether it was successfully measured.
The captured roster is reused by final candidate and native-rule admission.
Inactive rosters and producer variables do not change unrelated scopes.
This binds scope eligibility, not the complete capability or authorization
epoch contract in S8.

Only the resulting `storeScopes` digest is emitted or logged. Canonical paths,
workspace IDs, producer names and error-message preimages are not added to
public pack metadata. Direct builders that do not address stores bind absence
instead of manufacturing a runtime identity.

### 4. Canonical serialization rules (hash input)

- Every field is fed as `len(label) u64 LE ‖ label ‖ len(value) u64 LE ‖ value`
  (`hash_labeled_bytes`). Optional fields add a labeled presence flag, and
  repeated fields a labeled count.
- Every component opens with the labeled schema tag `ee.pack.hash_input.v7`
  and its component name.
- The composite is `blake3` over the labeled schema tag and the labeled
  component digests (§2a).
- Unicode, line endings, tokenizer, and locale (`bd-pack-identity-unicode-gitmy`):
  the query is canonicalized by `canonicalize_pack_query` (`src/pack/mod.rs`),
  which normalizes CRLF (`\r\n`) and CR (`\r`) line endings to LF (`\n`), trims
  outer whitespace, and applies Unicode Canonical Composition (NFC,
  `PACK_QUERY_UNICODE_POLICY_V1 = "nfc_lf_trimmed_v1"`). Candidate memory
  `content` is never Unicode- or line-ending-normalized
  (`PACK_CONTENT_UNICODE_POLICY_V1 = "byte_exact_utf8_v1"`), because normalizing
  stored memory bodies would break content-addressed memory identity (two
  byte-distinct memories must stay distinct). Canonical rendered prompt text
  (`pack.text`) always uses LF (`\n`) line endings. The tokenizer identity
  (`PACK_TOKENIZER_IDENTITY_V1 = "tiktoken_cl100k_base_or_char4_v1"`) and
  locale-independent C policy (`PACK_LOCALE_POLICY_V1 = "c_locale_independent_v1"`)
  are bound into `effectiveConfig`.
- Degraded entries: canonicalized in the hash input as in §2a. Emission order
  in `degraded[]` is a separate, presentational contract.
- Reference-time precedence is explicit CLI `as_of`, then query-file
  `temporal.validity.reference_time`, then query-file `temporal.as_of`.
  The resolved instant is truncated toward the start of its millisecond and
  serialized as UTC RFC 3339 with exactly three fractional digits and `Z`.
  Equivalent offsets and sub-millisecond spellings bind identically. Search,
  candidate validity checks and lifecycle explanations use that same
  millisecond domain: they evaluate at the millisecond's last nanosecond, so
  every lifecycle event stamped inside it (stored timestamps keep
  nanoseconds) has happened -- a revision is current at its own revision
  instant (bd-s6f7o). The independent query-file row-history cutoff remains
  a row-history filter; when present, it is also truncated to milliseconds.
  Existing whole-second row-bound inclusion semantics are retained.
- An absent explicit reference time is encoded as `mode=wall_clock`, with no
  `instant` field. Wall-clock runs are not historical replays; selected items
  and their lifecycle facets still bind any outcome changes. The L2 cache
  continues to bypass implicit-clock requests.
- Budget capping and adaptive budgeting preserve the resolved clock and task
  paths. A budget change cannot silently turn an explicit clock into wall-clock
  mode.

### 5. Volatile-data firewall

The hash consumes only the fields listed in §2a. Both its degraded component
and shared text renderer drop registered telemetry. Finalization happens after
canonical attachments and before persistence; later envelope notices do not
trigger rehashing. If persistence fails before any record commits, the returned
nonpersisted response is finalized again with that failure posture. A cache
write failure cannot change the hash already committed to the pack ledger.

### 6. Redaction posture

`data.pack.snapshotIdentity` exposes the composite digest and the per-component
DIGESTS (`blake3:<hex>`), never preimages. Component preimages can contain
absolute provenance paths (`file://` URIs are hashed verbatim) and query
text; a digest reveals neither.

### 7. Differing-state diagnostics

`snapshotIdentity.components` names ten components. Comparing two responses
field by field names the component that differs; the typed
`PackHashComponentDigests::differing_components` comparator uses those public
field names, including `referenceTime`, `qualityScoring`, `effectiveConfig` and `storeScopes`. `renderedText` moves when a rendered
input moves; changing only the clock can move `referenceTime` and the composite
while leaving the rendered text and selected items identical. No CLI comparator
is part of v7.

### 8. Versioning and migration

`data.pack.snapshotIdentity` on `ee.pack.v2`:

```json
{
  "version": 7,
  "inputSchema": "ee.pack.hash_input.v7",
  "digest": "<pack.hash>",
  "numericDomain": "q20.12",
  "componentDigestsAvailableLocally": true,
  "components": {
    "request": "blake3:…", "referenceTime": "blake3:…",
    "qualityScoring": "blake3:…",
    "items": "blake3:…", "omitted": "blake3:…",
    "degraded": "blake3:…", "coordination": "blake3:…", "renderedText": "blake3:…",
    "effectiveConfig": "blake3:…", "storeScopes": "blake3:…"
  }
}
```

- `componentDigestsAvailableLocally` is `false`, and `components` is absent,
  for a hand-built response whose hash no pack run computed. An L2 cache hit
  replays the stored response JSON, including the snapshot identity stored
  with it.
- Self-identification: `pack.hash` keeps its `blake3:<64 hex>` shape, because
  `ee.pack.diff.v3` and `ee.pack.replay.v2` publish `packHash` with the pattern
  `^blake3:[0-9a-f]{64}$`. The version travels beside the hash in every emitted
  pack (`version`, `inputSchema`), and the schema tag is bound into the
  preimage, so versions use separate hash domains.
- `pack_records.pack_hash` has no version column. Rows written before v7 carry
  earlier hash domains and are not comparable with v7 hashes; nothing in the
  row identifies that domain. Historical ledgers and hashes are not rewritten.
  `context show` and `pack replay` retain their redacted selection-projection
  contract and preserve the original hash; they do not claim to reproduce the
  original prompt bytes or relabel historical hashes as v7. No raw prompt is
  added to the durable ledger and no SQL migration is required.
- The L2 pack cache key schema moved to `ee.pack.l2_cache_key.v12`, so older
  responses miss. Lookup follows retrieval and adaptive budget resolution.
  Its key binds store/scope identity, the captured quality/effective configuration
  policies and returned candidate IDs, order,
  source lanes, and exact ranking/pack-score bits. New harmful feedback can
  therefore invalidate assembly reuse even when no memory row changed.
  Retrieval and authoritative admission still execute on an L2 hit.
- L2 payload schema `ee.pack.l2_context_response.v4` stores the finalized
  `canonicalText` separately from `responseJson`, including for Lean JSON.
  Admission validates v7 identity, requires all ten component digests, recomposes them with the
  requested output policy, verifies the typed footer and text digest, and
  checks JSON text presence/type against that policy. Hits hydrate the frozen
  Markdown without adding a text field to Lean JSON. DNA requests bypass L2
  so graph attachments always come from the live held selection snapshot.
- Bump rule: any change to §2a membership, §3 quantization or §4 encoding
  bumps `snapshotIdentity.version` and the input schema tag, and forks
  digests. No compatibility shim, no dual hash.

### 9. Verification (all RCH-only)

| Layer | Harness | Asserts |
|---|---|---|
| Unit | `src/core/context_test_module.rs` `pack_hash_v2_*` | the flat-feed provenance collision is separated (red first against v1); each differing input moves its own component and the composite only; timing, order and repetition move nothing; evidence scores quantize |
| Reference time | `context_reference_time_uses_one_precedence_and_millisecond_domain`, `pack_hash_v3_names_reference_time_without_changing_other_components`, `context_pack_validity_window_honors_as_of_and_include_future`, `pack_l2_cache_key_tracks_canonical_inputs` | precedence and actual selection share millisecond precision; wall-clock and explicit modes differ; offsets/sub-millisecond values canonicalize; identical selected items still distinguish different clocks; adaptive budgeting preserves identity; the comparator names `referenceTime`; L2 keys distinguish clocks |
| Effective configuration | `pack_hash_v6_*`, `pack_l2_cache_key_tracks_canonical_inputs` | equal selected items/text still distinguish controls in `effectiveConfig`; sets, signed zero, proportional ratios and observational changes canonicalize; L2 keys bind the captured policy |
| Store scopes | `pack_hash_v7_*`, `pack_store_scope_identity_*`, `pack_global_scope_identity_*`, `pack_store_address_identity_*`, `pack_l2_cache_key_tracks_canonical_inputs` | equal content distinguishes concrete stores/bindings, global tier sets and team membership; comparator names `storeScopes`; aliases and unordered memberships canonicalize; disabled stores remain unopened; only digests are public; L2 binds scope changes |
| Property | `tests/pack_hash_property.rs` (in `integration_property`) | timing stays outside prompt identity but visible in the envelope; order/repetition and sub-quantum noise properties remain; independently derived v7 composite and text-component vectors pin exact renderer bytes with the declared footer exception |
| Finalization and replay | `src/core/context_test_module.rs` | backend/DNA changes move the text component; literal placeholder text is preserved; late notices do not rewrite finalized text; Lean still binds Markdown; cache tampering and forged composites are rejected; Lean cache hits retain canonical text; real seeded persistence records the finalized DNA hash |
| State-creating commands | `src/obs/volatile_fields.rs` `state_creating_command_registry_covers_all_mutating_surfaces_and_converges_bd_state_creating_spec_8hjtj` | every state-creating command surface (`remember`, `pack`, `handoff`, `journal`, `decide`, `curate`, `backup`) is registered in `STATE_CREATING_COMMAND_SPECS`, consecutive runs differ only in declared state-creating and operational-telemetry fields, and undeclared canonical drift fails convergence |
| Existing | `determinism_unit`, `property_query_and_pack`, `pack_envelope_byte_identical_*` | unchanged determinism evidence |

Tests compare product output directly. The v7 text-leaf oracle substitutes only
the declared terminal footer value; it does not scrub any other content.
Its independent encoder reproduced the previous v6 vectors before deriving v7.
The prior 1,022-byte text oracle escaped the banner's `ee doctor` backticks;
the current renderer appends that summary verbatim, so the v7 fixture pins its
actual 1,020 bytes without changing renderer behavior.

## Consequences

- Every new pack hash changes once (v6 to v7). Addressed stores and scopes now
  have their own component, even when they do not change selected items or the
  rendered prompt. Historical hashes remain opaque references to their original
  ledger records and remain available through historical inspection/replay.
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
  `ee.pack.diff.v3` and `ee.pack.replay.v2`.
