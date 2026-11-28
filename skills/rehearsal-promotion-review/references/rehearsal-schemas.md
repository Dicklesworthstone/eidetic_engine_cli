# Rehearsal Schema Reference

This document lists the acceptable input schemas for the rehearsal-promotion-review
skill and the expected fields used in review logic.

## Acceptable Input Schemas

The skill accepts JSON output from these `ee rehearse` subcommands:

| Schema | Source Command |
|--------|----------------|
| `ee.rehearse.plan.v1` | `ee rehearse plan --commands <spec.json> --workspace <ws> --json` |
| `ee.rehearse.run.v1` | `ee rehearse run --commands <spec.json> --workspace <ws> --json` |
| `ee.rehearse.inspect.v1` | `ee rehearse inspect <run-id> --workspace <ws> --json` |
| `ee.rehearse.promote_plan.v1` | `ee rehearse promote-plan <run-id> --workspace <ws> --json` |
| `ee.response.v2` | Any command with degradation codes |

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

### Degraded rehearsal output

Rehearse reports carry their own `degradation_codes` array. A run whose
sandbox had no `ee` binary to execute plans the commands without running them:

```json
{
  "schema": "ee.rehearse.run.v1",
  "degradation_codes": ["executor_unavailable"]
}
```

## Degradation Codes

Every row whose source names an `ee` surface is a code that surface emits.
`rehearsal_unavailable` and `rehearsal_parse_error` are this skill's own
labels for states it detects itself; no `ee` command emits them.

| Code | Source | Meaning | Skill Behavior |
|------|--------|---------|----------------|
| `unsupported_rehearsal_command` | `ee rehearse plan` | Plan has `can_proceed: false` | Refuse with diagnosis |
| `external_io` | `ee rehearse run` | A command needs external I/O or is not rehearsable | No-go for that command; its real `--dry-run` is the fallback |
| `workspace_escape` | `ee rehearse run` | Arguments point workspace, database, index or output outside the sandbox | Refuse |
| `executor_unavailable` | `ee rehearse run` | No `ee` binary in the sandbox; commands were planned, not run | Refuse with unavailable template |
| `index_stale` | `ee index status` | Search index generation is behind the database | Needs-escalation |
| `graph_snapshot_stale` | graph export, feature enrichment | Graph-derived output may lag memory links | Needs-escalation |
| `rehearsal_unavailable` | this skill | Installed `ee` has no `rehearse` command | Refuse with unavailable template |
| `rehearsal_parse_error` | this skill | Malformed artifact JSON | Refuse with diagnosis |

## Redaction Status

`ee rehearse run` writes `redaction_status`: `redacted` under
`--profile privacy`, otherwise `none`. `ee rehearse inspect` repeats it as
`redaction_summary.policy`. The skill classifies each artifact from that:

- `passed`: All sensitive content redacted; safe to review
- `failed`: Redaction incomplete; refuse to review
- `unknown`: Redaction state unclear; escalate

## Mutation Posture

Skill tracks mutation posture from command flags and artifacts:

- `read-only`: No mutations planned
- `dry-run`: Mutations simulated but not applied
- `live`: Mutations would be applied (requires user approval)
