# Experiment Plan Template

```yaml
schema: ee.skill.active_learning_experiment_planner.v1
planningQuestion: "<question>"
workspace: "<workspace>"
evidenceBundle:
  path: "<bundle path>"
  hash: "<content hash>"
  redactionStatus: passed | failed | unknown
  trustClass: "<trust class>"
candidateExperiments:
  - experimentId: "<candidate id>"
    measurableHypothesis: "<metric, expected direction, and threshold>"
    requiredFixturesData:
      - "<fixture, eval record, observation, or context pack id>"
    stopCondition: "<sample size, confidence, safety, or time boundary>"
    costRisk:
      attentionTokens: <n>
      runtimeSeconds: <n>
      safetyBoundary: dry_run_only | ask_before_acting | human_review | denied
      risks:
        - "<risk and mitigation>"
    expectedInformationValue: high | medium | low
    expectedDecisionImpact: "<memory, retrieval, curation, policy, or profile decision that could change>"
    evidence:
      - "<observation/eval/provenance id>"
    agentGenerated: true
followUpEeCommands:
  - "ee learn observe <experiment-id> --measurement-name <name> --signal positive --evidence-id <evidence-id> --redaction-status redacted --dry-run --json"
  - "ee learn close <experiment-id> --status inconclusive --decision-impact \"<impact>\" --safety-note \"<note>\" --dry-run --json"
unsupportedClaims:
  - "<claim refused or none>"
```
