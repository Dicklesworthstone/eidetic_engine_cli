# Procedure Draft Template

```yaml
schema: ee.skill.procedure_distillation.draft.v1
workspace: "<workspace path>"
title: "<procedure title>"
sourceEvidence:
  sourceRunIds: ["<run id>"]
  evidenceIds: ["<evidence id>"]
  procedureIds: ["<procedure id or none>"]
  evidenceBundlePath: "<path>"
  evidenceBundleHash: "<blake3 hash>"
  redactionStatus: passed | failed | unknown
  trustClass: "<trust class>"
extractedFacts:
  - fact: "<observed fact from ee JSON>"
    evidence: "<source run, evidence, procedure, or fixture id>"
candidateSteps:
  - sequence: 1
    title: "<imperative step title>"
    instruction: "<actionable instruction>"
    expectedEvidence: ["<artifact, command, or check>"]
    assumptions: ["<assumption or none>"]
assumptions:
  - "<assumption or none>"
verificationPlan:
  requiredCommand: "ee procedure verify <procedure-id> --source-kind eval_fixture --source <fixture-id> --dry-run --json"
  fixtureIds: ["<fixture id>"]
  promotionAllowed: false
renderOnlySkillCapsule:
  artifactPath: "<path or none>"
  installMode: render_only
unsupportedClaims:
  - "<claim refused or none>"
degradedState:
  - code: "<degraded code or none>"
    repair: "<repair command or none>"
firstFailureDiagnosis: "<diagnosis or null>"
```

Draft rules:

- Do not draft without at least one source run ID or evidence ID.
- Keep extracted facts separate from candidate steps and assumptions.
- Keep promotion blocked until verification evidence is present.
- Treat render-only skill capsule content as a review artifact, not an
  installation instruction.
