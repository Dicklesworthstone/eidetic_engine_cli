# ADR 0063: Output-Token Governor and Continuation Cursors

Status: proposed
Date: 2026-06-10
Bead: bd-7lvbg.1 (epic bd-7lvbg, 2026-06 idea-wizard wave)

## Context

Agents budget in tokens, but `ee` responses range from ~630 bytes
(`swarm brief --fields minimal`) to ~156 KiB (full brief) and the caller only
learns the cost after paying it. Field presets (`--fields`) choose response
SHAPE; nothing enforces SIZE. A declared, enforced output ceiling is what
makes an `ee` call a fixed-cost operation safe to wire into tight hook paths
(recall at ~400 tokens, primer at ~600). This ADR specifies the global
`--max-output-tokens` governor, the per-schema truncation-point registry, and
the shared `ee.cursor.v1` continuation-cursor contract that generalizes the
bespoke cursor `ee audit timeline` already ships. Division of labor with
bd-kua65 (blocked) is deliberate: kua65 owns preset DEFAULTS; this governor
owns the enforcement ceiling; swarm brief adoption is deferred until kua65's
preset work lands.

## Decision

### 1. Estimator

- The governor uses the **same tiktoken-rs encoder already used for pack
  token budgeting**, so pack-content math and output math agree by
  construction. Estimation runs over the **serialized JSON string** of the
  candidate payload and is memoized per response.
- The ceiling applies to the **estimate**, with a documented tolerance: the
  contract is "estimate ≤ ceiling", not a byte guarantee. As a backstop
  against pathological tokenizer inputs, an absolute byte cap of
  `ceiling × 8` bytes is also enforced (estimate-evading payloads cannot
  blow out by more than a constant factor).
- **Zero cost when unused**: when no ceiling is set (no flag, no env), the
  estimator is never invoked and the render path is byte-identical to
  today's output. `meta.tokensEstimated` is emitted **only when a ceiling is
  set** (v1 decision; an always-on estimate was considered and deferred
  until the estimator's cost on large payloads is measured — revisit with
  bd-7lvbg.3 telemetry).

### 2. Truncation-point registry

- A static registry **in code, adjacent to the schema-id constants**, maps
  each list-like response schema to exactly one declared truncation point
  (the array whose trailing elements may be dropped). Initial entries:

  | Schema (surface) | Truncation point |
  |---|---|
  | search response | `data.results[]` |
  | memory list | `data.items[]` |
  | insights bundle | `data.sections[].items[]` (per-section, round-robin from the last section backwards) |
  | curate candidates | `data.candidates[]` |
  | audit timeline | `data.entries[]` |
  | pack (JSON) | `data.skipped[]`, then meta extras — **`data.pack.items[]` is NEVER governor-truncated** (pack content is governed solely by its own `--max-tokens` contract) |

  Wave surfaces declare their points as they land: recall `items[]`
  (bd-u875s.3), ask `nearestEvidence[]` then citation span text — never
  `answerText` (bd-169v0.3), journal list `entries[]` (bd-1pi9m.2),
  all-workspaces listing (bd-1bfwa.3), suggest-links/diff arrays
  (bd-3a1op.3/.5), curate-doctor queue and gaps report (bd-3ap2m.2/.3).
- **No mid-object truncation, ever.** The engine drops trailing whole
  elements from the declared point until the estimate fits, then appends an
  `output_truncated_budget` degraded entry carrying `droppedCount` and
  `continuationCursor`. Envelope-required fields are untouchable; debug
  builds assert a serde round-trip of the truncated payload.
- A response whose schema declares **no** truncation point either fits or
  fails closed with `output_budget_unsatisfiable` (medium) — no special
  cases, no exemptions. If a schema cannot declare a clean point, the schema
  needs redesign, not a carve-out. The schema-drift gate
  (`tests/contracts/schema_drift.rs`) asserts every list-like schema in the
  inventory has a registry entry.
- **Precedence**: explicit `--fields` projection applies FIRST, then the
  governor truncates the projected payload. `meta.tokensEstimated` always
  reflects the FINAL emitted payload.

### 3. `ee.cursor.v1` continuation cursors

- Opaque, deterministic string encoding `{schemaId, dbGeneration,
  positionKey, paramsHash}`, BLAKE3-MACed against tampering. `positionKey`
  is the stable ordered-position key of the last emitted element (every
  governed surface already has a deterministic total order); `paramsHash` is
  a BLAKE3 of the normalized query/filter parameters — cursors **never**
  embed secrets or raw query text.
- Resuming with a cursor whose `paramsHash` mismatches the new invocation, or
  whose MAC fails, yields `cursor_invalid` (low; repair: re-run without
  `--cursor`). Resuming after the DB generation advanced yields
  `cursor_stale` (low; same repair). This is deliberate: pagination across
  writes is dishonest — pages must partition one generation's result set
  exactly (no duplicates, no gaps), which is golden- and property-tested.
- `ee audit timeline` migrates its bespoke cursor to this codec **keeping
  the existing `--cursor` flag name and behavior**; old-format cursors are
  rejected as `cursor_invalid` with the re-run repair hint (acceptable
  break: cursors are short-lived by design).

### 4. Flag, env, and capability plumbing

- `--max-output-tokens <N>` is a **global** Cli-level flag (like
  `--fields`), threaded through the shared render path — individual command
  handlers do not reimplement governing. Env mirror `EE_MAX_OUTPUT_TOKENS`
  is registered in `src/config/env_registry.rs` + `docs/env_vars.md` in the
  implementing commit (bd-7lvbg.2). Flag wins over env.
- `ee capabilities` advertises governor availability and the per-schema
  truncation-point table so harnesses can feature-detect (bd-7lvbg.3).
- Human-format output is unaffected; the governor governs machine formats.

### 5. Degraded codes (pre-classified; files land with emission)

| Code | Severity | Class | Trigger |
|---|---|---|---|
| `output_truncated_budget` | info | response_time | elements dropped at the declared point; carries `droppedCount` + `continuationCursor` |
| `output_budget_unsatisfiable` | medium | response_time | envelope minimum (or point-less schema) exceeds the ceiling |
| `cursor_stale` | low | response_time | cursor generation < current DB generation |
| `cursor_invalid` | low | response_time | MAC failure, params mismatch, or legacy-format cursor |

Per the same-commit rule, `tests/fixtures/failure_modes/<code>.json` and
`docs/degraded_code_taxonomy.md` rows land with the first emitting commit
(bd-7lvbg.2/.3), not with this ADR.

## Consequences

- **Easier**: every governed `ee` call becomes fixed-cost; tight hook
  budgets become safe; pagination is uniform across surfaces instead of
  bespoke per command.
- **Guarded**: valid JSON always (whole-element drops + round-trip assert);
  pack content integrity preserved (items[] exempt by hard rule);
  determinism preserved (same DB + query + ceiling ⇒ byte-identical output
  including the cursor; prefix-stability: a smaller ceiling yields a strict
  prefix of a larger ceiling's elements).
- **Costs accepted**: estimator cost on very large payloads when a ceiling
  is set (mitigated by memoization + the fields-first precedence);
  one-time invalidation of in-flight legacy audit-timeline cursors.

## Rejected Alternatives

- **Byte ceilings only**: agents reason in tokens; byte caps survive only as
  the anti-pathological backstop.
- **Per-command bespoke flags**: inconsistent semantics and N
  implementations of truncation; rejected for one global flag + one engine.
- **Streaming-only answer** (NDJSON everywhere): pack streaming exists, but
  one-shot calls dominate hook paths; a governor must serve single
  responses.
- **Always-on tokensEstimated**: deferred (not rejected) pending estimator
  cost data; v1 emits only under a ceiling to keep the unused path
  zero-cost.
- **Cursors that survive generation advance**: dishonest pagination
  (duplicates/gaps); rejected for explicit `cursor_stale`.

## Verification

- Unit (bd-7lvbg.2): estimator determinism + agreement with pack budgeting;
  truncation never yields invalid JSON (proptest over arbitrary arrays);
  prefix-stability; cursor round-trip, tamper rejection, generation-stale
  rejection; zero-invocation assert when no ceiling is set
  (instrumentation counter).
- Contract (bd-7lvbg.4): schema-drift assert that every list-like inventory
  schema declares a truncation point; failure-mode fixtures for all four
  codes; J7 determinism harness extended to governed output.
- E2E (bd-7lvbg.4): `scripts/e2e_output_governor.sh` — 500-memory corpus,
  ceiling sweep (100/500/2000/none) across wired surfaces, cursor drain with
  exact-count partition checks, mid-pagination generation advance ⇒
  `cursor_stale`, env/flag equivalence; `ee.test_event.v1` logging per step.

## Appendix: `ee.cursor.v1` (normative draft)

Standalone `docs/schemas/ee.cursor.v1.json` ships with bd-7lvbg.2
(`x-ee-status` `shipped:false` until then); this draft is normative.

```text
ee.cursor.v1 (opaque wire form: base64url(payload) . base64url(blake3_mac))
payload object
  schema        const "ee.cursor.v1"
  targetSchema  string            (schema id of the governed response)
  dbGeneration  integer
  positionKey   string            (stable order key of last emitted element)
  paramsHash    string            (blake3 of normalized query/filter params)
mac: blake3 keyed over payload bytes (workspace-local key; never a secret
     leak vector — cursors are workspace-scoped and short-lived)
```
