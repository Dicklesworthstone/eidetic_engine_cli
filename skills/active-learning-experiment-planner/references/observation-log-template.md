# Observation Log Template

```yaml
schema: ee.skill.active_learning_observation_log.v1
experimentId: "<experiment id>"
observationRecordIds:
  - "<observation id>"
evalRecordIds:
  - "<eval id>"
measurement:
  name: "<measurement name>"
  signal: positive | negative | neutral | safety
  value: "<numeric or categorical value>"
  observedAt: "<RFC3339 or unknown>"
evidence:
  ids:
    - "<evidence id>"
  bundlePath: "<bundle path>"
  bundleHash: "<content hash>"
  redactionStatus: passed | failed | unknown
degradedCodes:
  - "<code or none>"
nextCommand: "ee learn observe <experiment-id> --measurement-name <name> --signal neutral --evidence-id <evidence-id> --redaction-status redacted --dry-run --json"
```
