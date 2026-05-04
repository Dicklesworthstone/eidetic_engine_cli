# Verification Matrix Template

```yaml
schema: ee.skill.procedure_distillation.verification_matrix.v1
procedureId: "<procedure id>"
verificationStatus: missing | passed | failed | stale | degraded
requiredVerificationCommand: "ee procedure verify <procedure-id> --source-kind eval_fixture --source <fixture-id> --dry-run --json"
fixtureIds: ["<fixture id>"]
sourceRunIds: ["<run id>"]
evidenceIds: ["<evidence id>"]
redactionStatus: passed | failed | unknown
stepChecks:
  - stepId: "<step id>"
    expectedEvidence: ["<expected artifact or assertion>"]
    observedEvidence: ["<verified artifact or none>"]
    result: missing | passed | failed | stale | degraded
    firstFailureDiagnosis: "<diagnosis or null>"
promotionDecision:
  allowed: false
  reason: "promotion requires passed ee procedure verify or explicit fixture evidence"
degradedState:
  - code: "<degraded code or none>"
    repair: "<repair command or none>"
```

Promotion rules:

- Refuse promotion when verification is missing, failed, stale, or degraded.
- Refuse promotion when evidence IDs or source run IDs are absent.
- Record the first failed step before offering another verification command.
