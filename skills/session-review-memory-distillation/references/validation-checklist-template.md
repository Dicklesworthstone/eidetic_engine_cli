# Validation Checklist Template

```yaml
schema: ee.skill.session_review_validation_checklist.v1
candidateId: "<candidate id>"
checks:
  provenance:
    passed: true | false
    evidenceUris:
      - "<uri>"
  redaction:
    passed: true | false
    status: passed | failed | unknown
  duplicate:
    passed: true | false
    command: "ee search \"<candidate text>\" --workspace <workspace> --json"
  specificity:
    passed: true | false
  promptInjection:
    quarantined: true | false
  validation:
    command: "ee curate validate <candidate-id> --workspace <workspace> --dry-run --json"
    status: passed | failed | not_run
recommendedAction: validate | reject | request_more_evidence | apply_dry_run
```
