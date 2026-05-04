# Preflight Brief Template

Use this template after parsing `ee preflight run --json`, tripwire output, and
any verified `ee.skill_evidence_bundle.v1` handoff.

```yaml
schema: ee.skill.preflight_risk_review.v1
task: "<task>"
workspace: "<workspace>"
riskSummary:
  level: low | medium | high | blocked | unknown
  rationale: "<one or two evidence-linked sentences>"
evidenceBackedTripwires:
  - id: "<tripwire id>"
    state: matched | clear | degraded | unavailable
    severity: low | medium | high | unknown
    evidence: ["<evidence id, provenance URI, or degraded code>"]
askNowQuestions:
  - question: "<question for user>"
    reason: "<decision the answer controls>"
    agentGenerated: true
mustVerifyChecks:
  - check: "<command or manual check>"
    evidence: "<evidence id or gap>"
stopConditions:
  - condition: "<blocking condition>"
    evidence: "<evidence id or degraded code>"
degradedState:
  - code: "<code>"
    effect: "<what cannot be concluded>"
    repair: "<repair command>"
followUpEeCommands:
  - "<ee ... --json command>"
unsupportedClaims:
  - "<claim refused or none>"
```

Rules:

- Evidence-backed warnings require evidence IDs, provenance URIs, or degraded
  codes.
- Ask-now questions are agent-generated and must set `agentGenerated: true`.
- No-evidence output must use `riskSummary.level: unknown` or `blocked`, never
  `low`.
- Destructive commands require a stop condition unless tripwire checks and
  user confirmation are both present.
