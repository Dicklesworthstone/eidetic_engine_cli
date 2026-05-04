# Candidate Memory File Template

```yaml
schema: ee.skill.session_review_memory_candidate_file.v1
candidates:
  - candidateId: "<draft id>"
    source: session_review_memory_distillation
    memoryLevel: procedural | semantic | episodic
    kind: rule | fact | failure | decision | anti_pattern
    content: "<candidate memory text>"
    confidence: <0.0-1.0>
    confidenceRationale: "<why confidence is supportable>"
    evidenceUris:
      - "<session id>#Lstart-Lend"
    sourceSessionIds:
      - "<session id>"
    trustClass: cass_evidence | agent_assertion | validated
    redactionStatus: passed | failed | unknown
    duplicateCheck:
      command: "ee search \"<candidate text>\" --workspace <workspace> --json"
      result: no_duplicate | duplicate_found | not_run
    validationPlan:
      required: true
      commands:
        - "ee curate validate <candidate-id> --workspace <workspace> --dry-run --json"
        - "ee curate apply <candidate-id> --workspace <workspace> --dry-run --json"
```
