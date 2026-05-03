# ADR 0011: Mechanical CLI Boundary

Status: accepted
Date: 2026-05-03

## Context

The `ee` CLI is a durable memory layer for coding agents. Agent harnesses (Claude
Code, Codex, Cursor, etc.) already provide reasoning, planning, and judgment. If
`ee` commands simulate intelligence, fabricate qualitative conclusions, or return
mock reasoning, agents cannot trust the outputs as ground truth.

The audit (eidetic_engine_cli-i6vu) classified all 46 CLI command families and
found that 14 have names or outputs that suggest judgment work. This ADR codifies
the boundary so future development does not reintroduce fake intelligence.

## Decision

**`ee` is mechanical in execution.** The binary may:

- Compute, aggregate, and transform over explicit inputs
- Retrieve, index, pack, and rank by deterministic scoring
- Validate, hash, redact, and render
- Replay persisted events and project graphs
- Report posture, health, capability, and schema metadata

**`ee` may NOT:**

- Invent reasoning, causal claims, or procedure quality without explicit evidence
- Generate task plans, learning agendas, or experiment designs
- Emit recommendations, confidence, or risk assessments that simulate judgment
- Return mock/sample/stub data in production command output
- Claim to have "analyzed" or "decided" when the computation is a fixed formula

When a command surface requires intelligence (causal interpretation, procedure
synthesis, preflight questioning, counterfactual reasoning, active learning), the
boundary-crossing work belongs in **project-local `skills/`** workflows that
consume `ee` outputs, not in the `ee` binary itself.

## Consequences

### Allowed (Mechanical)

| Action | Example |
|--------|---------|
| Compute scores from stored feedback | `ee economy utility --json` returns weighted sums |
| Pack context by MMR + token budget | `ee context` assembles memories deterministically |
| Project graph metrics from edges | `ee graph pagerank` computes over persisted links |
| Replay recorder events | `ee recorder tail` streams persisted events |
| Report degraded state | Commands return `degraded` status, not guessed data |

### Disallowed (Requires Skill Workflow)

| Action | Why |
|--------|-----|
| "This procedure is high-quality" | Quality judgment requires intelligence |
| "Root cause is X because Y" | Causal interpretation requires reasoning |
| "Agent should do A next" | Task planning requires context synthesis |
| "Risk is medium" | Risk assessment implies judgment |
| Sample/mock data in production | Agents cannot trust fabricated outputs |

### Mixed Commands (Split Required)

Commands like `causal`, `preflight`, `situation`, `learn`, and `lab` must split:

1. **Mechanical surface in `ee`**: Compute projections, store records, return
   data with explicit provenance and scoring math.
2. **Skill surface in `skills/`**: Interpret outputs, synthesize conclusions,
   generate recommendations.

## Examples

### Good: Mechanical computation

```bash
ee context "fix release workflow" --json
# Returns: memories ranked by deterministic score + budget fit
# Does NOT return: "I recommend checking the CI config"
```

### Bad: Simulated judgment

```bash
ee preflight "deploy to prod" --json
# BAD: {"risk_level": "medium", "recommendation": "add canary"}
# The binary cannot know risk or make recommendations
```

### Correct: Degraded until honest

```bash
ee learn agenda --json
# Returns: {"status": "degraded", "reason": "learning_requires_skill_workflow"}
```

## Verification Hooks

1. **Command inventory check** (`docs/command_classification.md`): Every command
   has a target disposition. New commands must be classified before merge.

2. **No-mock-output contract**: CI fails if production command output contains
   sample/stub markers (`"[sample]"`, `"example_"`, `"mock_"`).

3. **Dependency audit**: `cargo tree` must not include LLM client crates in the
   core binary. Skill workflows live outside the binary.

4. **Skill workflow e2e**: `skills/` directory contains documented workflows
   that consume `ee` JSON outputs and apply agent reasoning.

## Rejected Alternatives

- **Let `ee` call LLMs for smart commands**: Violates local-first principle,
  introduces latency, and makes outputs non-deterministic.
- **Mock data as temporary placeholders**: Agents will build on mock outputs;
  degraded-with-schema is safer.
- **Merge skill logic into the binary later**: Boundary drift is expensive;
  keeping it clean from the start is cheaper.

## References

- `docs/command_classification.md`: Full command inventory and classification
- ADR 0001: CLI-first memory substrate
- ADR 0006: Procedural memory requires evidence
- AGENTS.md: Hard requirements section

