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
   rounds). The evaluator scores the actual live incumbent, even when its
   weights are outside the candidate bounds. Default persistence writes the
   report to `<workspace>/.ee/shadow/retrieval_tuning_report.json`
   (`ee.shadow.retrieval_tuning_report.v1`), a private
   `retrieval_tuning_authority.json` beside it, and an evaluation attestation
   in the addressed database's audit log. Use `--no-persist` for an evaluation
   that saves none of those records. Evaluation does not change live weights.
3. **Read the verdict.** The evidence gate abstains — honestly, with
   `insufficient_outcome_evidence` — below 50 usable triples across 15
   distinct queries, or when the winner's relative margin is under 3%.
   `abstained=true` means "keep the incumbent"; it is a result, not an
   error. A promotable report names the winning weights, score, and
   margin.
4. **Promote explicitly.** `ee shadow promote [--dry-run]` requires the
   evaluator's matching report, source receipt, and database attestation.
   It rechecks candidate bounds, evidence thresholds, strict improvement,
   margin, corpus generation, current outcome labels, effective configuration,
   and the addressed workspace/database before changing the three `[search]`
   fusion weights. Editing the JSON or recomputing its hashes cannot create
   evaluation authority. Refusals are typed exit-7 plans; `--dry-run` writes
   nothing. A durable audit intent, including complete before/after config
   bytes, precedes atomic config publication and its completion audit.
5. **Roll back without losing later settings.** `ee shadow demote` restores
   `search.lexical_weight`, `search.semantic_weight`, and `search.graph_weight`
   from the promotion audit. If the config is unchanged since promotion,
   restoration is byte-identical. Otherwise unrelated settings and comments
   survive, while conflicting manual edits to the three owned weights cause
   a refusal. Repeated demotion is a no-op. An absent prior config restores
   as an empty file, never a deletion.

If publication is interrupted, retry the same promotion or demotion. The
recorded intent can complete the audit for an already-published change even
when later outcomes or corpus changes make the original evaluation stale.
Demotion can recover a published promotion even after a newer evaluation
replaces the report files. On completion-audit failure, rollback only replaces
the exact bytes this operation published, preserving any intervening edit.
Database audit records and workspace files are separate durability boundaries;
the intent makes that boundary recoverable.

Reports produced before evaluator authority was introduced must be regenerated
with `ee shadow run`. Historical promotion audits without source binding and
owned-field snapshots cannot authorize automatic demotion.

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
- `labelSet.triples` / `labelSet.distinctQueries` / `labelSet.hash` — the
  replayable evidence count and its reproducible identity. The report also
  retains label-extraction diagnostics for evidence that could not be replayed.
- `winner.weights` / `winner.relativeMargin` — the candidate and how far
  ahead it finished; promotion verifies these against the full attested
  candidate evaluation and its policy gates.
- `promotable` — true only when the gate passed and the winner's margin
  clears the threshold.

## Regression coverage

The evaluator's contract is pinned by `tests/property_shadow_tuning.rs`:
within a single query, adding a helpful label for a memory never flips the
pairwise order of two weight vectors against the one ranking it higher;
evaluation is byte-deterministic for identical inputs; and the
cross-query dilution counterexample is kept as a regression so nobody
"fixes" the per-query normalization that stops chatty workspaces from
dominating. Core tuning regressions cover authentic promotion and rollback,
forged reports and receipts, changed evidence/configuration, database binding,
failed intent/completion audits, interrupted publication, and preservation of
unrelated edits. `scripts/e2e_shadow_retrieval_tuning.sh` exercises the sparse
CLI loop and proves that editing an abstained report cannot authorize either
dry-run or live promotion. The sparse fixture does not claim a learned winner.

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
