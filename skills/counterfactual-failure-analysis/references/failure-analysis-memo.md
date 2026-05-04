# Failure Analysis Memo Template

Use this template after the stop/go gates in `SKILL.md` pass.

```yaml
schema: ee.skill.counterfactual_failure_analysis.v1
failureQuestion: ""
workspace: ""
evidenceBundle:
  path: ""
  hash: ""
  redactionStatus: unknown
  trustClass: ""
observedFacts:
  - fact: ""
    evidence: ""
replayEvidence:
  status: missing
  quality: unknown
  evidenceIds: []
packDiffs:
  - item: ""
    source: ""
hypotheses:
  - claim: ""
    support: ""
    falsification: ""
assumptions:
  - "none"
agentJudgment:
  conclusion: refused
  rationale: ""
unsupportedClaims:
  - "none"
degradedState:
  - code: "none"
    effect: "none"
    repair: "none"
recommendedExplicitCommands:
  - "ee lab replay --workspace <workspace> --episode-id <episode-id> --json"
```

Required distinction:

- `observedFacts` are facts from `ee lab capture`, `ee lab replay`,
  `ee lab counterfactual`, `ee status`, or `ee.skill_evidence_bundle.v1`.
- `replayEvidence` is replay output that directly supports or contradicts the
  counterfactual question.
- `hypotheses` are possible explanations that need falsification.
- `assumptions` are required but not established.
- `agentJudgment` is the skill's bounded interpretation, not an `ee` decision.

Refuse strong success claims when replay evidence is missing, contradictory,
degraded, stale, or based only on pack presence.
