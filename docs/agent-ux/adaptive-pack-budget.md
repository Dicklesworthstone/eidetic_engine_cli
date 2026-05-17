# Agent Guide: Adaptive Pack Budgets

Adaptive pack budgets let `ee context` choose a smaller or larger token budget
from deterministic task signals instead of using one fixed default for every
query. The feature is opt-in when it is wired to the public context surface; an
explicit `--max-tokens` value remains authoritative.

The budget decision block uses schema `ee.context.budget.v1`.

## What To Read

Start with these fields:

- `adaptive`: `true` means the classifier computed the budget.
- `computedTokens`: final token budget after clamping.
- `baseTokens`: starting point before multipliers. The current classifier uses
  1000 tokens unless `maxTokens` is lower.
- `maxTokens`: upper bound from config, query file, or CLI wiring.
- `multiplier`: combined contribution multiplier applied to `baseTokens`.
- `classifierContributions`: deterministic reasons for the multiplier.
- `explicitOverride`: present when a caller-provided budget bypassed adaptive
  computation.

Example:

```json
{
  "schema": "ee.context.budget.v1",
  "adaptive": true,
  "baseTokens": 1000,
  "maxTokens": 4000,
  "computedTokens": 2160,
  "multiplier": 2.16,
  "classifierContributions": {
    "retrievalEntropy": 0.8,
    "retrievalEntropyMultiplier": 0.4,
    "graphFanout": 2.0,
    "graphFanoutMultiplier": 0.6,
    "taskKeywordScore": 0.3,
    "taskKeywordMultiplier": 0.06
  }
}
```

## Contribution Semantics

`retrievalEntropy` is the normalized Shannon entropy of the top retrieval
scores. A high value means the top candidates are spread out and the task may
need a wider pack. It is capped to the top 20 positive finite scores.

`graphFanout` is the average graph fanout for the seed set, sanitized to zero
for invalid values and capped at 3.0. High fanout indicates the query touches a
larger neighborhood.

`taskKeywordScore` is a small lexical hint for complex agent tasks. Current
markers include words such as `audit`, `diagnose`, `fix`, `migrate`,
`performance`, `refactor`, `rewrite`, `security`, `test`, and `verify`.

The current pure classifier combines the values as:

```text
multiplier = 1.0
  + 0.5 * retrievalEntropy
  + 0.3 * graphFanout
  + 0.2 * taskKeywordScore

computedTokens = clamp(ceil(baseTokens * multiplier), baseTokens, maxTokens)
```

## Determinism Rules

- The classifier is pure: same query, retrieval scores, graph fanout, and
  maximum token bound produce the same decision.
- Do not include timestamps, process IDs, random seeds, host names, or live
  environment state in `ee.context.budget.v1`.
- If future wiring derives `graphFanout` from graph state, the relevant graph
  snapshot identity must be part of the surrounding context determinism key.
- Explicit `--max-tokens` must bypass adaptive selection rather than competing
  with it.

## Consumer Rules

- Treat `computedTokens` as the only budget number to enforce.
- Treat `classifierContributions` as explanation, not as separate policy.
- Reject `computedTokens > maxTokens` as a contract bug.
- Expect `adaptive: false` when adaptive budgets are disabled or explicitly
  overridden.
- Do not compare pack hashes across adaptive and non-adaptive budget modes.

## Rollout Expectations

The first implementation slice only provides the pure classifier and this
contract. Public context wiring still needs the config key, explain-output
attachment, golden fixtures, e2e logging, and RCH-only benchmark evidence before
operators should rely on adaptive budgets in normal workflows.
