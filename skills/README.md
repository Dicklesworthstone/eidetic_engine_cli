# Project-Local Skills

This directory contains agent-facing workflows that sit beside `ee` without
becoming part of the Rust binary. Skills may interpret mechanical `ee` JSON,
ask for human judgment, and synthesize next steps. Durable state changes still
go through explicit `ee` commands.

The boundary is intentional: skills may use agent judgment, but the Rust `ee`
binary remains mechanical. If a skill wants durable mutation, it must produce
or run an explicit `ee` command, dry-run plan, or audited repair command that
the user or harness can inspect.

## Directory Contract

Each skill lives in one folder:

```text
skills/
  skill-name/
    SKILL.md
    references/
    scripts/
    assets/
```

`SKILL.md` is required. Optional resources are loaded only when the skill needs
them. Do not add README files inside individual skill folders; keep discoverable
instructions in `SKILL.md` or a referenced file.

## Required `SKILL.md` Sections

Every project-local skill must cover:

- Trigger Conditions
- Mechanical Command Boundary
- Evidence Gathering
- Stop/Go Gates
- Output Template
- Uncertainty Handling
- Privacy And Redaction
- Degraded Behavior
- Unsupported Claims
- Testing Requirements
- E2E Logging

These sections keep skill workflows honest about what `ee` computed versus what
the agent inferred, including degraded states and redaction requirements.

## Minimum Test Shape

Every skill must have this minimum test shape before it is treated as
implemented:

- static or unit checks for required `SKILL.md` frontmatter, sections, command
  references, and output template labels
- fixture or transcript tests for refusal paths, degraded `ee` outputs,
  redaction failures, and unsupported-claim handling
- e2e contract coverage through the shared skill harness when the workflow
  calls real `ee` commands

Tests must fail loudly with the missing file, section, metadata field, command,
schema, redaction state, degraded code, output artifact, or first missing
requirement.

## Calling `ee`

Skills should prefer machine-facing command modes:

```bash
ee status --workspace <path> --json
ee context "<task>" --workspace <path> --json
ee why <memory-id> --workspace <path> --json
```

JSON data comes from stdout. Treat stderr as diagnostics. If a command returns
a degraded or unavailable response, the skill must surface the degraded code,
name the missing capability, and avoid filling the gap with invented evidence.

## Durable Mutation

Skills may recommend or prepare explicit commands, but they must not imply that
the Rust binary performed reasoning. Any durable mutation must be visible as an
`ee` command, dry-run plan, or audit record.

## E2E Logging Contract

Skill harness logs must follow the boundary logging discipline in
`docs/boundary-migration-e2e-logging.md`: stdout remains machine JSON from `ee`,
while skill diagnostics and artifacts stay outside that stream. A skill e2e or
lint log records:

- skill path
- required files
- parsed metadata
- referenced `ee` commands
- evidence bundle hashes
- redaction and degraded states
- output artifact path
- first missing requirement or first failure diagnosis

The project-local lint schema is `ee.skill_standards.lint_log.v1`. It exists to
prove the skill folder contract and to give future skill beads a stable place to
record missing requirements without claiming unsupported behavior.
