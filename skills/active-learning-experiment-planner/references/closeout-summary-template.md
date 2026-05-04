# Closeout Summary Template

```yaml
schema: ee.skill.active_learning_closeout_summary.v1
experimentId: "<experiment id>"
status: confirmed | rejected | inconclusive | unsafe
decisionImpact: "<decision changed or not established>"
confidenceDelta: "<delta or unknown>"
priorityDelta: "<delta or unknown>"
sampleSize: <n>
stopConditionMet: true | false
evidence:
  observationRecordIds:
    - "<observation id>"
  evalRecordIds:
    - "<eval id>"
  bundlePath: "<bundle path>"
  bundleHash: "<content hash>"
safetyNotes:
  - "<safety note>"
unsupportedClaims:
  - "<claim refused or none>"
followUpEeCommand: "ee learn close <experiment-id> --status inconclusive --decision-impact \"<impact>\" --safety-note \"<note>\" --dry-run --json"
```
