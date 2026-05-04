# Session Review Memo Template

```yaml
schema: ee.skill.session_review_memory_distillation.v1
reviewQuestion: "<session or lesson being reviewed>"
workspace: "<workspace>"
evidenceBundle:
  path: "<bundle path>"
  hash: "<content hash>"
  redactionStatus: passed | failed | unknown
  trustClass: "<trust class>"
sourceSession:
  sessionIds:
    - "<session id or path>"
  cassCommandTranscript:
    - "ee import cass --workspace <workspace> --dry-run --json"
    - "cass view <session-id> --json"
  lineRanges:
    - "<session id>#Lstart-Lend"
observedSessionFacts:
  - fact: "<observed fact from CASS/ee JSON>"
    evidence:
      - "<line range, evidence id, or hash>"
candidateMemories:
  - candidateId: "<draft id>"
    memoryLevel: procedural | semantic | episodic
    kind: rule | fact | failure | decision | anti_pattern
    content: "<candidate text>"
    confidence: <0.0-1.0>
    confidenceRationale: "<evidence-linked rationale>"
    evidenceUris:
      - "<provenance URI>"
    validationPlan:
      required: true
      commands:
        - "ee curate validate <candidate-id> --workspace <workspace> --dry-run --json"
    agentGenerated: true
rejectedObservations:
  - observation: "<observation>"
    reason: "<why rejected>"
recommendedFollowUpCommands:
  - "ee curate candidates --workspace <workspace> --json"
  - "ee curate validate <candidate-id> --workspace <workspace> --dry-run --json"
```
