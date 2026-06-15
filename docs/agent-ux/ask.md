# Ask

`ee ask "<question>" --workspace . --json` returns a direct extractive answer
from stored memories. It is for narrow factual questions where the agent needs
the answer and the cited evidence, not a full context pack.

Use `ee ask` when:

- the question can be answered by a small number of stored evidence spans
- the caller needs `data.citations[]` with byte ranges and memory IDs
- low confidence should produce an abstention instead of a synthesized answer
- hooks need fail-closed behavior through `--require-confidence <threshold>`

The JSON payload uses `data.schema == "ee.ask.v1"`. Important fields:

| Field | Meaning |
|---|---|
| `data.answerText` | Extractive answer text, or null when abstained/conflicted |
| `data.citations[]` | Cited memory IDs, source byte ranges, span text, trust class, and confidence |
| `data.confidence` | Overall confidence after corroboration and contradiction handling |
| `data.confidenceComponents` | Top span score, corroboration multiplier, and contradiction penalty |
| `data.sides[]` | Opposing cited answers when conflicting evidence is detected |
| `data.nearestEvidence[]` | Best sub-threshold spans when the command abstains |
| `data.counterfactualHint` | What evidence was missing or too weak for an answer |

Known degraded rows:

- `ask_semantic_degraded`: semantic span scoring is unavailable and the weight
  is renormalized into lexical scoring.
- `ask_conflicting_evidence`: top evidence clusters oppose each other and the
  payload emits `sides[]`.
- `no_confident_answer`: no span reached the confidence threshold; use
  `nearestEvidence[]` to decide what source memory to add or inspect.

For automation, treat `--require-confidence T` as the strict hook mode. It exits
with code 6 and an `ee.error.v2` envelope when the answer confidence is below
`T`.
