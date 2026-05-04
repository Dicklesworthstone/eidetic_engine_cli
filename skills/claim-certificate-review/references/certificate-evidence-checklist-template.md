# Certificate Evidence Checklist Template

```yaml
schema: ee.skill.claim_certificate_review.checklist.v1
workspace: "<workspace path or unknown>"
certificateIds: ["<certificate id>"]
manifestPaths: ["<certificates.json>", "<payload path>"]
evidenceBundle:
  path: "<path>"
  hash: "<content hash>"
hashStatus: passed | failed | stale | missing | degraded | unknown
schemaStatus: passed | stale | unsupported | missing | degraded | unknown
expiryStatus: valid | expired | revoked | invalid | pending | unknown
assumptionStatus: passed | failed | unverified | unknown
redactionStatus: passed | failed | unknown
degradedState:
  - code: "<degraded code>"
    repair: "<explicit repair command>"
missingEvidence:
  - item: "<missing manifest, payload, hash, schema, or assumption>"
    repair: "<explicit repair command>"
followUpCommands:
  - "ee --workspace <workspace> --json certificate verify <certificate-id> --manifest <path>"
  - "ee --workspace <workspace> --json certificate show <certificate-id> --manifest <path>"
```
