# Decide

`ee decide` records lightweight ADRs as ordinary `kind=decision` memories with
typed fields. Use it when the choice should stop coming back as an open question.
Use raw `ee remember --kind decision` for imported or historical decisions where
you are preserving evidence, not asserting a new live chain head.

## Commands

| Command | Use |
|---|---|
| `ee decide record "<topic>" --chosen <choice> --alternative <other> --rationale "<why>" --json` | Create a current decision head |
| `ee decide record ... --revisit-by <RFC3339|+ND>` | Schedule the decision for future review |
| `ee decide record ... --supersedes <memory-id>` | Replace a prior decision for the same normalized topic |
| `ee decide list --about <text> --json` | Inspect current heads before proposing architecture or workflow changes |
| `ee decide list --include-superseded --json` | Inspect the full supersede history |
| `ee decide revisit [--warning-days N] --json` | List decisions due or near due for review |

`record` is a durable mutation. `list` and `revisit` are read-only.

## Supersede Discipline

Decision topics are normalized to prevent accidental forks. If a live decision
already exists for the same normalized topic, a second `record` without
`--supersedes` fails with a usage error and includes the prior memory id plus a
suggested command.

Agents should follow this loop:

1. Run `ee decide list --about <topic> --workspace . --json`.
2. If a current head exists, read it before proposing a replacement.
3. Replace it only with `ee decide record ... --supersedes <prior-id> --json`.
4. Use `ee decide list --include-superseded --about <topic> --json` when the
   chain history matters.

Do not create a new topic spelling to bypass fork refusal.

## Revisit Hygiene

Use `--revisit-by +90d` for relative day intervals or an explicit RFC3339
timestamp for calendar commitments. `ee decide revisit` uses the workspace
`[decide] revisit_warning_days` setting unless `--warning-days` overrides it.

Returned decision items include:

| Field | Meaning |
|---|---|
| `memoryId` | Decision memory id |
| `topic` / `normalizedTopic` | Human topic and deterministic chain key |
| `chosen` | Current choice |
| `alternatives` / `options` | Rejected alternatives and all options |
| `rationale` | Why the chosen option won |
| `supersedes` | Prior memory id when this item replaces one |
| `chainDepth` | Number of predecessors in the supersede chain |
| `revisitBy` | RFC3339 revisit timestamp, if scheduled |
| `revisitStatus` | `none`, `future`, `near_due`, `due`, or `overdue` |
| `superseded` / `validTo` | Whether this item is no longer a live head |

The response data schemas are `ee.decide.record.v1`, `ee.decide.list.v1`, and
`ee.decide.revisit.v1`, all wrapped by `ee.response.v2`.

## Ask And Conflict Resolution

Decision-kind typed fields use the same registry as ordinary memories:
`options`, `chosen`, `rationale`, `supersedes`, and `revisit_by`.

This matters for agents:

- `ee ask` can cite decision memories and expose conflict sides without needing a
  separate decision store.
- Conflict-resolution rationale should use the same field names so later
  `ee search --kind decision --field chosen=<choice> --json` works.
- `ee pack` and `ee orient` can surface due decision reviews as memory evidence,
  not as a hidden planner state.

## Failure Branches

| Signal | Next action |
|---|---|
| `decision_topic_requires_supersedes` | Inspect `priorMemoryId`, then rerun with `--supersedes` only if replacing it is intentional |
| `decision_supersedes_topic_mismatch` | Pick a predecessor from the same normalized topic or create a new decision topic |
| Invalid revisit timestamp | Use RFC3339 or `+ND`, for example `+90d` |
| Missing predecessor id | Run `ee decide list --include-superseded --json` and choose an existing decision memory |

Automation should key on `error.details.failureModeCode` when present, not on
human text.
