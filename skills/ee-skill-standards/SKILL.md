---
name: ee-skill-standards
description: Use when creating, reviewing, or updating project-local ee skills that consume mechanical ee CLI JSON and apply agent judgment outside the Rust binary.
---

# EE Skill Standards

Use this standard for project-local skills in this repository. Skills are the
agent-judgment layer around `ee`; the Rust binary stays mechanical,
deterministic, and evidence-bound.

Any durable mutation proposed by a skill must flow back through an explicit
`ee` command, dry-run plan, or audit artifact. Do not describe a skill's
judgment as something the `ee` binary decided.

## Trigger Conditions

Use or review a project-local skill when a workflow needs interpretation,
planning, qualitative review, causal reasoning, procedure synthesis, or other
agent judgment over `ee` outputs.

Do not use a skill to hide normal CLI behavior. If a deterministic computation
belongs in `ee`, implement it in Rust with tests instead.

## Mechanical Command Boundary

Skills may call mechanical `ee` commands and interpret their outputs. They must
not claim that `ee` reasoned, decided, recommended, or knew something beyond the
returned evidence.

Preferred command shape:

```bash
ee status --workspace <workspace> --json
ee context "<task>" --workspace <workspace> --json
ee search "<query>" --workspace <workspace> --json
ee why <memory-id> --workspace <workspace> --json
```

JSON data comes from stdout. Diagnostics come from stderr. Never parse progress,
human text, or tracing as evidence.

## Evidence Gathering

Collect the smallest evidence bundle that can support the skill's judgment:

- command argv and workspace
- parsed `ee` JSON schema and command status
- memory, pack, recorder, preflight, outcome, or audit IDs
- degradation codes and repair strings
- redaction status and evidence hashes

Keep raw secrets out of prompts and artifacts. Prefer IDs, hashes, paths, and
redacted snippets.

## Stop/Go Gates

Stop and report a blocker when:

- required `ee` JSON is missing or invalid
- a command reports degraded or unavailable status that invalidates the task
- evidence lacks provenance
- redaction status is unknown for sensitive data
- the requested conclusion would require unsupported claims

Go only when evidence is parseable, scoped to the selected workspace, and enough
to support the requested output.

## Output Template

Skill outputs should be concise and evidence-linked:

```text
Decision:
Evidence:
Uncertainty:
Degraded State:
Recommended Explicit Commands:
```

If no recommendation is supportable, say so and name the missing evidence.

## Uncertainty Handling

Separate facts, inferences, and assumptions. Use calibrated language such as
`observed`, `inferred from`, and `not established`. Do not convert weak evidence
into confident claims.

## Privacy And Redaction

Before using evidence in a prompt or output, check for redaction fields,
privacy classes, and secret placeholders. If redaction is missing or failed,
stop and request a safer evidence bundle.

Never quote raw credentials, tokens, private keys, or unredacted user-private
content. Use stable IDs, hashes, and redaction classes instead.

## Degraded Behavior

When `ee` returns degraded output, preserve the code and repair guidance. The
skill may propose next explicit commands, but it must not synthesize missing
facts or silently treat degraded data as complete.

## Unsupported Claims

Forbidden skill claims:

- `ee proved root cause X` unless a mechanical proof artifact says so
- `ee recommends action Y` unless the recommendation is explicitly the skill's
  judgment over named evidence
- `risk is medium` without an evidence-backed rubric shown in the output
- any claim based on sample, mock, or placeholder data

## Testing Requirements

Every skill must have this minimum test shape:

- static checks for required `SKILL.md` frontmatter and sections
- fixture or transcript tests for refusal and degraded paths
- e2e contract coverage through the shared skill harness when the workflow calls
  real `ee` commands

Tests must fail loudly with the missing file, section, command, schema,
redaction field, degraded response code, output artifact, or first missing
requirement.

## E2E Logging

Logged skill runs must record:

- skill path and required files
- parsed metadata
- referenced `ee` commands
- evidence bundle hashes
- redaction and degraded states
- output artifact path
- first missing requirement or first failure diagnosis

Logs belong under the shared e2e artifact tree or another documented local
artifact path. They must not pollute JSON stdout from `ee`.

Use schema `ee.skill_standards.lint_log.v1` for static skill-lint logs unless a
specific skill defines a narrower contract. This schema complements the
cross-cutting boundary log in `docs/boundary-migration-e2e-logging.md` and keeps
skill-path, metadata, evidence hashes, redaction state, degraded state, artifact
path, and first-missing-requirement reporting consistent.
