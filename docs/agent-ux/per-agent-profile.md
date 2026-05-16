# Agent Guide: Per-Agent Context Profiles

Per-agent context profiles let `ee context` apply a small, deterministic bias
from an agent's own outcome history. The feature is additive: it does not
change global memory confidence, and it cannot let one agent dominate another
agent's retrieval results.

When enabled, `ee context --explain --json` may include an `agentProfile` block
using schema `ee.context.agent_profile.v1`.

## What To Read

Start with these fields:

- `coldStart`: `true` means no ranking bias was applied.
- `observedOutcomes`: total profile events considered for the current agent and
  workspace.
- `biasMagnitude`: largest absolute bias applied to any selected or candidate
  memory.
- `maxBiasMagnitude`: hard cap. It must be `0.05`.
- `memoryBiasApplied`: number of memories that received non-zero bias.
- `topBiases[]`: optional redaction-safe explanation of the largest memory
  adjustments.

Example:

```json
{
  "schema": "ee.context.agent_profile.v1",
  "agentName": "CalmBridge",
  "agentNameHash": "blake3:2f4c1b8a",
  "observedOutcomes": 73,
  "helpfulCount": 52,
  "harmfulCount": 8,
  "ignoredCount": 13,
  "biasMagnitude": 0.031,
  "maxBiasMagnitude": 0.05,
  "memoryBiasApplied": 12,
  "coldStart": false,
  "coldStartThreshold": 10,
  "halfLifeDays": 365,
  "determinismKey": {
    "workspaceGeneration": 42,
    "profileGeneration": 7,
    "agentNameHash": "blake3:2f4c1b8a",
    "basePackHash": "blake3:..."
  }
}
```

## Interpretation Rules

- Treat profile bias as a small personalization hint, not as new evidence.
- A memory still needs normal provenance and trust signals.
- `coldStart: true` means the profile exists only as an explanation surface; it
  did not alter ranking.
- `biasMagnitude` must never exceed `0.05`. A larger value is a contract bug.
- Different agents may receive different rankings for the same query after
  enough outcome history. The same agent, database, index, config, and query
  must still produce byte-identical output.

## Cold Start

The default cold-start threshold is 10 outcome events per `(workspace, agent)`.
Before that threshold, `agent_bias = 0` for every memory. The context response
may still include the `agentProfile` block so agents can see why no bias was
applied.

Agents should not try to force profile learning by generating low-quality
outcomes. Bad feedback makes the profile less useful and is bounded by the
same cap anyway.

## Privacy

Profile rows are workspace-scoped. They must not leak across workspaces.

Ordinary local JSON may include `agentName` because the caller is the local
agent asking for its own context. Export and support-bundle surfaces should
prefer `agentNameHash` unless raw names are explicitly requested through an
unredacted mode.

Do not put raw query text, memory content, or private evidence into
`topBiases[]`; use memory IDs, counts, timestamps, and numeric bias only.

## Determinism

Profile state is part of the deterministic input set. If profile rows change,
pack hashes may change. If profile rows do not change, the same
`determinismKey` and query must produce the same pack hash.

Consumers should include `determinismKey.profileGeneration` in cache keys for
any cached context result that used profile bias.

## Outcome Semantics

Outcome signals should be interpreted narrowly:

- `helpful`: this memory helped the current agent complete a task.
- `harmful`: this memory actively misled or slowed the current agent.
- `ignored`: this memory was presented but not used.

The profile bias is intentionally capped so repeated feedback can tune ranking
without overriding base relevance, trust, validity windows, or policy gates.

## Consumer Checklist

- Validate the block against `ee.context.agent_profile.v1`.
- Treat `coldStart: true` as no ranking change.
- Reject or flag any `biasMagnitude > 0.05`.
- Keep profile cache entries scoped by workspace and agent hash.
- Do not compare profile-influenced pack hashes across different agents.
- Keep `topBiases[]` separate from provenance; it explains personalization, not
  truth.
