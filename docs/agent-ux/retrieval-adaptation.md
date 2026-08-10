# Retrieval Adaptation (Outcome-Tuned Fusion Weights)

ADR 0070 / bd-2tehh. How `ee` learns better search fusion weights from
recorded outcomes **without breaking determinism**: adaptation is an
offline, explicitly-promoted config change, never a silent behavioral
drift.

## The safe-adaptation loop

1. **Emit outcomes as you work.** `ee outcome <memory-id> --pack <pack-id>
   --item <n> --signal helpful|harmful` ties feedback to the pack item that
   surfaced the memory (`ee.outcome.pack_item_evidence.v1`). This is the
   dense label source. Audit-log search hits hash-join in as weak labels
   (weight 0.5); labels whose query text cannot be replayed are counted in
   the honest denominator and never guessed at.
2. **Run the evaluator offline.** `ee shadow run --policy
   candidate.retrieval.outcome_tuned_weights --json` extracts labeled
   (query, memory, signal) triples (quarantined feedback excluded), replays
   the queries against the live index, and sweeps a bounded grid of fusion
   weight vectors (±0.05/±0.10 around the incumbent, lexical/semantic
   clamped to [0.2, 0.7], graph to [0.0, 0.3], at most two descent
   rounds). Nothing durable changes; the report persists to
   `<workspace>/.ee/shadow/retrieval_tuning_report.json`
   (`ee.shadow.retrieval_tuning_report.v1`).
3. **Read the verdict.** The evidence gate abstains — honestly, with
   `insufficient_outcome_evidence` — below 50 usable triples across 15
   distinct queries, or when the winner's relative margin is under 3%.
   `abstained=true` means "keep the incumbent"; it is a result, not an
   error. A promotable report names the winning weights, score, and
   margin.
4. **Promote explicitly.** `ee shadow promote [--dry-run]` validates the
   persisted report (freshness, promotability) and applies the winner as a
   `[search]` overlay in the workspace `config.toml`, writing a promotion
   audit that carries the **complete prior config bytes**. Refusals are
   typed exit-7 plans; `--dry-run` writes nothing. Determinism is restored
   the moment the config lands: retrieval is again a pure function of
   (config, corpus, query).
5. **Roll back byte-identically.** `ee shadow demote` restores the exact
   pre-promotion `config.toml` bytes from the audit (an absent prior
   restores as an empty file — never a deletion).

## When to run

- After a sustained stretch of real outcome traffic — the gate needs 50
  usable triples over 15 queries, which a busy workspace accumulates in
  days-to-weeks, not hours.
- After large corpus shifts (mass imports, major cleanups) that could make
  the incumbent weights stale.
- Not on a timer inside hooks: the evaluator replays queries and is meant
  for deliberate, reviewed adaptation (a monthly cadence is plenty; see
  the `monthly-retrieval-tuning` agent recipe).

## Reading the report

Key fields of `ee.shadow.retrieval_tuning_report.v1`:

- `abstained` / `abstentionReason` — the gate's verdict; abstention keeps
  the incumbent and is the expected result until enough evidence exists.
- `labels.usableTriples` / `labels.distinctQueries` /
  `labels.unreplayable` — the honest denominator: how much evidence the
  sweep actually had, including what could not be replayed.
- `winner.weights` / `winner.relativeMargin` — the candidate and how far
  ahead it finished; promote only reads these from the persisted report.
- `promotable` — true only when the gate passed and the winner's margin
  clears the threshold.

## What the property suite guarantees

The evaluator's contract is pinned by `tests/property_shadow_tuning.rs`:
within a single query, adding a helpful label for a memory never flips the
pairwise order of two weight vectors against the one ranking it higher;
evaluation is byte-deterministic for identical inputs; and the
cross-query dilution counterexample is kept as a regression so nobody
"fixes" the per-query normalization that stops chatty workspaces from
dominating.

## Trust interplay

Outcome quality gates adaptation quality: quarantined feedback is
excluded from label extraction, and upstream SPRT/burst-rate quarantine
plus the prompt-injection guard keep low-quality or adversarial outcome
streams from steering the weights. Garbage feedback produces abstention,
not adaptation. See [docs/trust-model.md](../trust-model.md).

## Failure modes

| Code | Meaning |
|---|---|
| `insufficient_outcome_evidence` | Evidence gate abstained (too few usable triples/queries or margin under threshold); incumbent kept. |
| `shadow_report_not_persisted` | The evaluator ran but the report could not be written; rerun before promoting. |

Both carry fixtures under `tests/fixtures/failure_modes/` and rows in the
[degraded-code taxonomy](../degraded_code_taxonomy.md).
