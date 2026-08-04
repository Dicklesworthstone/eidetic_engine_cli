# ADR 0067: Extractive Ask — Deterministic Question Answering with Citations

Status: proposed
Date: 2026-06-10
Bead: bd-169v0.1 (epic bd-169v0, 2026-06 idea-wizard wave)

## Context

`ee search` returns ranked documents; the agent must then read N bodies to
extract one fact ("which port does the daemon use?", "master or main?").
`ee ask "<question>"` composes a direct answer FROM EXTRACTED SPANS of
stored memories — no LLM, no generation: retrieval → evidence clustering →
answer-span extraction → deterministic composition with per-claim citations,
an overall confidence, and honest abstention. The trust story rests on one
invariant: **every emitted answer sentence byte-equals a cited span of a
stored memory** — machine-checkable, golden-freezable, and the honest
differentiator versus LLM-RAG sidecars. This ADR fixes the span model,
scoring, composition, confidence/abstention, and conflict behavior before
implementation (bd-169v0.2/.3).

## Decision

### 1. Span model

- Deterministic sentence segmentation with code-fence awareness (memories
  contain commands, paths, and fenced blocks; a fenced block is one span;
  URL dots and common abbreviations do not split). The segmenter is a
  shared utility (placed for reuse by primer/pack rendering later).
- Spans are addressed as `(memory_id, byte_start, byte_end)` so
  extractiveness is verifiable by byte comparison. The **extractiveness
  invariant is enforced at the output boundary in production** (not only a
  debug assert): if any composed sentence fails byte-equality against its
  cited span, `ee ask` refuses to emit the answer (internal error path) —
  this check is the contract, never downgrade it.

### 2. Span scoring

```text
span_score = w1·lexical_overlap(question, span)
           + w2·embedding_sim(question, span)
           + w3·memory_confidence_trust_tilt
  defaults: w1 = 0.45, w2 = 0.35, w3 = 0.20   (config [ask])
```

- `lexical_overlap`: normalized content-term overlap (stopword-light,
  deterministic normalization shared with the gap-mining pipeline).
- `embedding_sim`: cosine via the frankensearch embedder; when only the
  hash-embedder fallback is available the response carries
  `ask_semantic_degraded` (info) and w2 mass shifts to w1 (documented
  re-normalization, still deterministic).
- `memory_confidence_trust_tilt`: the memory's confidence scaled by trust
  class (human_explicit > peer_human_attested > agent_validated >
  agent_assertion > cass_evidence > legacy_import, using the established trust
  ordering).
- **Corroboration**: spans are clustered with the existing MI-dedup
  machinery (cosine ≥ 0.85, NMI ≥ 0.72 constants reused, not redefined);
  a cluster's representative span gains a corroboration multiplier
  `1 + 0.1·ln(cluster_size)` capped at 1.3.
- **Contradiction penalty**: when top clusters oppose each other —
  explicit `contradicts` memory-links first (precision), the shared
  polarity/negation heuristic second — the composition switches to
  conflict mode (§4) instead of silently picking a winner.

### 3. Composition, confidence, abstention

- Answer = up to N (default 3) highest-scoring non-redundant spans, ordered
  by score then memory_id, each tagged `[n]` with
  `citations[n] = {memoryId, span, provenanceUri, trustClass, confidence}`.
- Overall confidence:
  `conf = top_span_score · corroboration · (1 − contradiction_penalty)`,
  clamped to [0,1] and reported with its components.
- **Abstention is a SUCCESS response, not an error** (exit 0): when
  `conf < [ask] min_confidence` (default 0.55) the response sets
  `abstained: true` with `no_confident_answer` (info), `nearestEvidence[]`
  (top sub-threshold spans), and a counterfactual hint mirroring why-not
  phrasing ("no memory mentions X; nearest evidence: …"). Every abstention
  ALSO emits a query-miss ledger row (`ee.search.query_miss.v1` posture:
  hashed/redacted query text) tagged with origin `ask`, so demand-driven
  gap mining (bd-3ap2m.3) sees ask misses as well as search misses.
- `--require-confidence <T>`: fail-closed mode for hooks/scripts — if
  `conf < T`, exit 6 (degraded-required) with the abstention payload.
  Hooks that act on answers must not act on weak ones.

### 4. Conflict behavior

When the contradiction penalty triggers, the answer becomes `sides[]` —
each side composed extractively with its own citations — plus
`ask_conflicting_evidence` (warning) and a next-command pointer to
`ee conflict explain <a> <b>`. Ask never resolves what `ee conflict
resolve` (ADR 0066) exists to resolve explicitly; the loop is: ask surfaces
the conflict → resolve fixes it → the next ask answers cleanly.

### 5. Degradation vocabulary

| Code | Severity | Class | Trigger |
|---|---|---|---|
| `no_confident_answer` | info | response_time | confidence below threshold; abstention payload (an ANSWER about the corpus, not an error) |
| `ask_semantic_degraded` | info | response_time | hash-embedder fallback in play; w2 re-normalized into w1 |
| `ask_conflicting_evidence` | warning | response_time | top clusters oppose; sides[] emitted |

Envelope truncation uses the ADR 0063 governor (`output_truncated_budget`);
ask's declared truncation point is `nearestEvidence[]` first, then citation
span text — **never `answerText`**. Fixture/taxonomy files land with the
emitting commits (bd-169v0.2/.3).

### 6. Output and quality gates

- `ee ask "<q>" [--limit-evidence K] [--source-mode …] [--require-confidence T]
  --json | --format markdown`; markdown renders a compact answer block with
  footnote citations (prepend-safe). Read-only effect class.
- Quality is a CI contract, not a vibe: `ee eval` gains ask fixtures
  (bd-169v0.4) with metrics — citation precision, answer exactness,
  abstention calibration (no confident answers on unanswerables, no
  abstention on easy facts), conflict recall — on the
  advisory→blocking maturation path the perf budgets use.
- Performance target: p50 < 150 ms warm at default K (bench in
  bd-169v0.5); abstention must not be the slow path — it is the common
  case on sparse corpora.

## Consequences

- **Easier**: a 3-call read-and-skim workflow (search → memory show × N)
  becomes one bounded call whose output drops directly into reasoning, with
  abstention honest enough to wire into fail-closed hooks.
- **Guarded**: extractive-only with a production-enforced byte invariant;
  deterministic (same DB + question ⇒ byte-identical answer); conflicts
  surfaced, never silently resolved; weak corpora abstain instead of
  hallucinate-by-retrieval.
- **Costs accepted**: answers are limited to sentences someone stored —
  ask cannot synthesize across spans (by design; synthesis is the
  harness's job, with ask's citations as ground truth).

## Rejected Alternatives

- **Generative composition** (LLM): violates determinism + no-paid-API +
  the extractiveness trust story. Rejected.
- **Answering from packs**: packs optimize task coverage, not question
  precision, and are task-conditioned; rejected for direct span retrieval.
- **Silent best-side selection on conflict**: hides exactly what the
  conflict surface exists to fix. Rejected for sides[].
- **Abstention as an error exit**: abstention is information about the
  corpus; rejected for exit 0 + payload (exit 6 only under explicit
  `--require-confidence`).
- **Bespoke truncation code**: superseded by the ADR 0063 governor
  vocabulary, same as recall (ADR 0064 precedent).

## Verification

- Unit (bd-169v0.2): segmentation goldens (code fences, URLs,
  abbreviations); score determinism + tie-breaks; corroboration math;
  contradiction trigger precision on fixture pairs; abstention threshold
  edges; extractiveness invariant (mutate a span ⇒ assert refusal);
  query-miss row emission on abstention with origin=ask.
- Eval (bd-169v0.4): fixture corpus (~60 memories, seeded hash embeddings)
  + question set covering direct facts, corroboration lift, planted
  contradictions, unanswerables, validity-window-sensitive facts; metric
  computations unit-tested against hand-computed values; wired into
  `scripts/verify.sh`.
- E2E (bd-169v0.5): `scripts/e2e_ask.sh` — factual / corroborated /
  conflicting / unanswerable / fail-closed / lexical-degraded paths with
  `ee.test_event.v1` logging per step.

## Appendix: `ee.ask.v1` (normative draft)

Standalone `docs/schemas/ee.ask.v1.json` ships with bd-169v0.3
(`x-ee-status` `shipped:false` until then); this draft is normative.

```text
object ee.ask.v1 (under ee.response.v2 data.answer)
  schema          const "ee.ask.v1"
  question        string (echoed)
  abstained       boolean
  answerText      string | null      ([n]-marked extracted sentences)
  confidence      number
  confidenceComponents {topSpanScore, corroboration, contradictionPenalty}
  citations[]
    index         integer            (the [n] marker)
    memoryId      string
    span          {byteStart: integer, byteEnd: integer}
    text          string             (byte-equal to the span)
    provenanceUri string | null
    trustClass    string
    confidence    number
  sides[] | null                     (conflict mode)
    label         string
    answerText    string
    citations[]   (as above)
  nearestEvidence[] | null           (abstention mode; governor truncation point)
    memoryId, span, text, score
  counterfactualHint string | null
```
