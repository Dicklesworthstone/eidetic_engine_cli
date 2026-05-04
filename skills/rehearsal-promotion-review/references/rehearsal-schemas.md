# Rehearsal Schema Reference

This document lists the acceptable input schemas for the rehearsal-promotion-review
skill and the expected fields used in review logic.

## Acceptable Input Schemas

The skill accepts JSON output from these `ee rehearse` subcommands:

| Schema | Source Command |
|--------|----------------|
| `ee.rehearse.plan.v1` | `ee rehearse plan --workspace <ws> --json` |
| `ee.rehearse.run.v1` | `ee rehearse run --workspace <ws> --json` |
| `ee.rehearse.inspect.v1` | `ee rehearse inspect --plan-id <id> --workspace <ws> --json` |
| `ee.rehearse.promote_plan.v1` | `ee rehearse promote-plan --plan-id <id> --workspace <ws> --dry-run --json` |
| `ee.response.v1` | Any command with degradation codes |

## Required Fields for Review

### From `ee.rehearse.plan.v1`

```json
{
  "schema": "ee.rehearse.plan.v1",
  "plan_id": "<uuid>",
  "workspace_id": "<path>",
  "created_at": "<rfc3339>",
  "dry_run": true,
  "artifacts": [
    {
      "path": "<relative-path>",
      "content_hash": "<blake3>",
      "size_bytes": 1234
    }
  ],
  "mutations": {
    "files_changed": 0,
    "records_affected": 0,
    "side_effects": []
  },
  "preconditions": [
    {
      "name": "<check-name>",
      "status": "pass|fail|skipped",
      "message": "<optional>"
    }
  ],
  "status": "planned|ready|blocked",
  "degraded": []
}
```

### From `ee.rehearse.inspect.v1`

```json
{
  "schema": "ee.rehearse.inspect.v1",
  "plan_id": "<uuid>",
  "rehearsal_id": "<uuid>",
  "workspace_id": "<path>",
  "artifacts": [...],
  "mutations": {...},
  "preconditions": [...],
  "status": "inspected",
  "redaction_status": "passed|failed|unknown",
  "degraded": []
}
```

### From `ee.response.v1` (degraded)

```json
{
  "schema": "ee.response.v1",
  "command": "rehearse",
  "status": "degraded",
  "degraded": [
    {
      "code": "rehearsal_unavailable",
      "message": "Rehearsal commands not yet wired.",
      "severity": "high",
      "repair": "ee doctor --json"
    }
  ]
}
```

## Degradation Codes

| Code | Meaning | Skill Behavior |
|------|---------|----------------|
| `rehearsal_unavailable` | ee rehearse not wired | Refuse with unavailable template |
| `rehearsal_parse_error` | Malformed artifact JSON | Refuse with diagnosis |
| `index_stale` | Search index out of sync | Needs-escalation |
| `graph_projection_unavailable` | Graph metrics unavailable | Partial review |
| `redaction_failed` | Sensitive data not redacted | Refuse |

## Redaction Status

The skill checks `redaction_status` in inspect output:

- `passed`: All sensitive content redacted; safe to review
- `failed`: Redaction incomplete; refuse to review
- `unknown`: Redaction state unclear; escalate

## Mutation Posture

Skill tracks mutation posture from command flags and artifacts:

- `read-only`: No mutations planned
- `dry-run`: Mutations simulated but not applied
- `live`: Mutations would be applied (requires user approval)
