# Claim Review Template

```yaml
schema: ee.skill.claim_certificate_review.v1
workspace: "<workspace path or unknown>"
reviewQuestion: "<claim posture being reviewed>"
evidenceBundle:
  path: "<path>"
  hash: "<content hash>"
  redactionStatus: passed | failed | unknown
  trustClass: "<trust class>"
sourceVerification:
  claimIds: ["<claim id>"]
  certificateIds: ["<certificate id or none>"]
  manifestPaths: ["<claims.yaml>", "<artifact manifest>"]
  commandTranscript:
    - "ee --workspace <workspace> --json claim verify <claim-id> --claims-file <path> --artifacts-dir <path> --fail-fast"
verifiedFacts:
  - fact: "<mechanically verified claim fact>"
    evidence: ["<manifest path, report path, evidence id, or hash>"]
failedStaleChecks:
  - check: hash | schema | manifest | degraded | redaction
    status: passed | failed | stale | missing | degraded | unknown
    evidence: ["<error code or evidence id>"]
assumptions:
  - assumption: "<assumption required by the claim>"
    status: passed | failed | unverified | unknown
overclaimRisks:
  - risk: "<where wording outruns evidence>"
    evidence: ["<mechanical report or missing evidence>"]
missingEvidence:
  - item: "<missing artifact, manifest, or validation>"
    repair: "<explicit repair command>"
reviewJudgment:
  posture: supported | stale | overbroad | blocked | unsupported | unknown
  rationale: "<evidence-linked judgment>"
  mayStrengthenClaim: true | false
unsupportedClaims:
  - "<claim refused or none>"
degradedState:
  - code: "<degraded code>"
    effect: "<what cannot be concluded>"
    repair: "<explicit repair command>"
followUpCommands:
  - "ee --workspace <workspace> --json claim verify <claim-id> --claims-file <path> --artifacts-dir <path> --fail-fast"
```
