# Drift Review Template

```yaml
schema: ee.skill.procedure_distillation.drift_review.v1
procedureId: "<procedure id>"
checkedAt: "<RFC3339 timestamp>"
sourceEvidence:
  sourceRunIds: ["<run id>"]
  evidenceIds: ["<evidence id>"]
  procedureIds: ["<procedure id>"]
driftSignals:
  - signal: "<failed verification, stale evidence, dependency drift, or none>"
    evidence: "<verification id, fixture id, or degraded code>"
    severity: low | medium | high | unknown
verificationStatus: passed | failed | stale | degraded | missing
redactionStatus: passed | failed | unknown
degradedState:
  - code: "<degraded code or none>"
    repair: "<repair command or none>"
recommendedExplicitCommands:
  - "ee procedure drift <procedure-id> --workspace <workspace> --json"
  - "ee procedure verify <procedure-id> --source-kind eval_fixture --source <fixture-id> --dry-run --json"
firstFailureDiagnosis: "<diagnosis or null>"
```

Drift rules:

- Do not retire, promote, or mutate a procedure from this review alone.
- Preserve degraded codes and repair commands.
- Treat stale evidence as a blocker until the verification matrix is rerun.
