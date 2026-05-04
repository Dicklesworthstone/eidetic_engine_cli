---
name: rehearsal-promotion-review
description: Review rehearsal dry-run artifacts and produce conservative promotion checklists; requires real ee rehearse output or explicit unavailable degradation.
---

# Rehearsal and Promotion Review

Use this skill to review rehearsal dry-run artifacts and decide whether to
promote changes. The skill interprets `ee rehearse` output but never authorizes
destructive commands directly.

## Trigger Conditions

Invoke when:

- A rehearsal plan or dry-run artifact needs human/agent review
- Promotion decisions require checking preconditions and mutation risks
- Rollback or verification steps need to be identified before committing

Do not invoke to run rehearsals. Use `ee rehearse plan/run/inspect` directly
first, then bring the JSON output to this skill for review.

## Required Evidence

Before producing any recommendation:

1. Run `ee status --workspace <workspace> --json` to verify workspace readiness
2. Run `ee rehearse plan --workspace <workspace> --json` or equivalent
3. If unavailable, the skill must refuse with explicit degraded status
4. Parse the rehearsal JSON for artifacts, mutations, preconditions, and status

Acceptable input schemas:

- `ee.rehearse.plan.v1`
- `ee.rehearse.run.v1`
- `ee.rehearse.inspect.v1`
- `ee.rehearse.promote_plan.v1`
- `ee.response.v1` with degraded codes

Do not proceed if:

- Rehearsal JSON is missing or malformed
- Command returned `rehearsal_unavailable` degradation
- Evidence lacks workspace or artifact provenance
- Redaction status is unknown for sensitive artifacts

## Stop/Go Gates

**Stop** when:

- `ee rehearse` is unavailable or degraded without repair path
- Artifacts reference files/data that were not captured in the dry run
- Preconditions are unmet (missing dependencies, failed checks)
- Mutation would affect protected/safety-critical resources without explicit user approval

**Go** when:

- Rehearsal JSON is valid and complete
- All preconditions pass or are explicitly waived
- Mutation risks are documented and acceptable
- Verification steps are identified

## Evidence Gathering

Collect from rehearsal output:

- rehearsal_id, plan_id, workspace_id
- artifact paths and content hashes
- mutation summary (files changed, records affected, side effects)
- precondition check results
- degradation codes and repair suggestions
- redaction status of any sensitive content
- timestamp and dry_run flag

## Output Template

```text
## Rehearsal Review Summary

**Rehearsal ID:** <id>
**Workspace:** <path>
**Status:** <pass/fail/partial/degraded>

### Artifact Summary
- Files: <count> files, <total_size>
- Records: <count> mutations planned
- Side Effects: <list or "none declared">

### Preconditions
- [ ] <precondition 1>: <status>
- [ ] <precondition 2>: <status>

### Mutation Risks
- <risk 1>: <severity>, <mitigation>
- <risk 2>: <severity>, <mitigation>

### Verification Steps (post-promotion)
1. <verification command 1>
2. <verification command 2>

### Rollback Considerations
- <rollback option 1>
- <rollback option 2>

### Go/No-Go Recommendation
**Decision:** <go/no-go/needs-escalation>
**Rationale:** <evidence-based reasoning>
**Escalation Required:** <yes/no> - <reason if yes>

### Recommended Explicit Commands
- `ee rehearse promote <plan_id> --workspace <workspace> --dry-run --json` (preview)
- `ee rehearse promote <plan_id> --workspace <workspace> --json` (execute, requires user approval)
```

## Uncertainty Handling

- Label uncertain preconditions as "inferred" or "not verified"
- List assumptions explicitly under a separate section if needed
- Never convert partial dry-run results into confident promotion claims

## Destructive Command Escalation

This skill **never authorizes destructive commands directly**. When a promotion
would:

- Delete files or database records
- Overwrite existing production data
- Modify security-sensitive configuration
- Affect other workspaces or shared resources

The skill must:

1. List the destructive operations explicitly
2. Mark the decision as "needs-escalation"
3. Recommend the user run the command manually after review
4. Include the exact command for user copy-paste

## Degraded Behavior

When `ee rehearse` returns degraded output:

- Preserve the degradation code and repair command in the review
- Do not synthesize missing artifact data
- Report what is available and what is missing
- Recommend repair commands before promotion review can proceed

## Unavailable Handling

If `ee rehearse` is entirely unavailable (nd65 not yet wired):

```text
## Rehearsal Review Unavailable

**Status:** rehearsal_unavailable

Rehearsal commands are not yet wired to real dry-run isolation.
This skill cannot review artifacts that do not exist.

**Repair:** Wait for ee rehearse implementation or check ee doctor --json
**Follow-up Bead:** eidetic_engine_cli-nd65
```

## Testing Requirements

Tests must cover:

- Valid rehearsal plan with all preconditions passing (go)
- Rehearsal with failed precondition (no-go)
- Partial failure in dry run (needs-escalation)
- Unavailable rehearsal (explicit refusal)
- Unsupported command type (explicit refusal)
- Redacted artifact evidence (preserve redaction)
- Malformed rehearsal JSON (parse error with diagnosis)
- Degraded CLI output (preserve degradation)

## E2E Logging

Log schema: `ee.skill_standards.lint_log.v1`

Required fields:

- skill_path: `skills/rehearsal-promotion-review`
- fixture_id and fixture_hash
- rehearsal_artifact_ids and rehearsal_artifact_paths
- evidence_bundle_path and evidence_bundle_hash
- mutation_posture (read-only/dry-run/live)
- redaction_status and degraded_codes
- go_no_go_class (go/no-go/needs-escalation/unavailable)
- output_artifact_path
- required_section_check (pass/fail with missing sections)
- first_failure_diagnosis (if any)
