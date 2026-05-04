# Rejection Log Template

```yaml
schema: ee.skill.session_review_rejection_log.v1
sourceSessionIds:
  - "<session id>"
rejectedObservations:
  - observationId: "<local id>"
    observation: "<redacted observation>"
    reason: missing_provenance | duplicate | non_durable | private | prompt_injection | degraded | unsupported
    evidence:
      - "<line range, evidence id, or hash>"
    followUp:
      - "<command or none>"
degradedCodes:
  - "<code or none>"
firstFailureDiagnosis: "<gate code or none>"
```
