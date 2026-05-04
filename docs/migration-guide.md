# Migration Guide: Mechanical CLI Boundary Realignment

Bead: `eidetic_engine_cli-3c93`

This guide documents the command behavior changes resulting from the mechanical CLI
boundary realignment. **No features were dropped**; they were split by responsibility
into:

1. **Mechanical CLI behavior** — deterministic, local, evidence-based Rust commands
2. **Project-local skills** — agent judgment workflows in `skills/`
3. **Honest degraded/unavailable states** — explicit abstention when evidence is missing

## Quick Reference

| Before | After | What Changed |
|--------|-------|--------------|
| Commands returned fabricated data | Commands return real evidence or degrade honestly | No more fake reasoning |
| Agent judgment embedded in CLI | Agent judgment moved to project-local skills | Separation of concerns |
| Mock/sample/seed data passed as real | Explicit degraded codes with repair commands | Transparent unavailability |
| Opaque "recommendations" | Evidence + interpretation clearly separated | Explainable outputs |

## Core Documents

- [Mechanical Boundary Command Inventory](./mechanical-boundary-command-inventory.md) — full command matrix
- [Command Classification](./command_classification.md) — disposition categories
- [Boundary Migration E2E Logging](./boundary-migration-e2e-logging.md) — test coverage
- [ADR 0011](./adr/0011-mechanical-cli-boundary.md) — architectural decision record
- [Project-local Skills](../skills/README.md) — skill directory and standards

---

## Command Family Migration Map

### Core Retrieval: `context`, `search`, `why`, `pack`

**Classification:** Mechanical CLI (keep)

**What the CLI computes:**
- Hybrid lexical+semantic search over FrankenSQLite memories
- Context pack assembly with token budgets via Frankensearch
- Provenance and score breakdown for every returned memory
- Pack record persistence with content hash

**What changed:**
- Nothing. These commands were always mechanical and remain so.
- Degraded codes added for index staleness and search unavailability.

**Degraded outputs:**
```json
{
  "schema": "ee.error.v1",
  "error": {
    "code": "search_index_unavailable",
    "message": "Search index is stale or unavailable.",
    "severity": "medium",
    "repair": "ee index rebuild --workspace ."
  }
}
```

---

### Memory Management: `memory`, `remember`, `revise`

**Classification:** Mechanical CLI (keep)

**What the CLI computes:**
- Memory record CRUD with audit trail
- Revision history and provenance linking
- Confidence scoring and decay application

**What changed:**
- `revise` internals no longer fabricate revision rationale
- Revisions require explicit user input or skill handoff
- Audit entries are always written for mutations

**Skill handoff:** None required — these are pure mechanical operations.

---

### Causal Analysis: `causal trace`, `causal estimate`, `causal compare`, `causal promote-plan`

**Classification:** Split (mechanical evidence + skill interpretation)

**What the CLI computes:**
- Causal chain extraction from evidence ledgers
- Conservative statistical estimates with sample sizes
- Comparison reports with confounder notes
- Promote-plan with action=Hold when evidence insufficient

**What moved to skills:**
- Causal credit assignment recommendations
- Confounder assessment and interpretation
- Evidence tier determination (T0-T5)
- Promote/demote/reroute decisions

**Skill:** `skills/causal-credit-review/SKILL.md`

**Before:**
```bash
ee causal promote-plan --workspace . --json
# Returned: { "action": "promote", "confidence": 0.85, ... }
# (fabricated from mock data)
```

**After:**
```bash
ee causal promote-plan --workspace . --json
# Returns: { "action": "hold", "degraded": [...], ... }
# (honest about insufficient evidence)

# Then invoke the skill for interpretation:
# The skill consumes ee causal JSON and produces recommendations
```

**Degraded outputs:**
```json
{
  "schema": "ee.causal.promote_plan.v1",
  "action": "hold",
  "degraded": [{
    "code": "causal_sample_underpowered",
    "message": "Sample size 12 below threshold 30.",
    "severity": "medium",
    "repair": "Record more outcomes with ee outcome record"
  }]
}
```

---

### Learning: `learn agenda`, `learn uncertainty`, `learn summary`, `learn experiment`

**Classification:** Split (mechanical ledgers + skill planning)

**What the CLI computes:**
- Observation ledger reads/writes with audit
- Experiment closure with feedback records
- Conservative degraded reports when ledgers empty

**What moved to skills:**
- Learning agenda prioritization
- Uncertainty interpretation
- Experiment proposal generation
- Active learning planning

**Skill:** Experiment planner skill (pending)

**Before:**
```bash
ee learn experiment run --id exp_database_contract_fixture --dry-run --json
# Returned: fabricated experiment report with fake steps and observations
```

**After:**
```bash
ee learn experiment run --id exp_database_contract_fixture --dry-run --json
# Returns: UnsatisfiedDegradedMode error
{
  "schema": "ee.error.v1",
  "error": {
    "code": "unsatisfied_degraded_mode",
    "message": "Experiment execution requires persisted experiment definitions from an evaluation registry.",
    "repair": "Provide explicit input datasets or use skill workflows for experiment orchestration."
  }
}
```

---

### Preflight & Tripwire: `preflight run`, `tripwire check`

**Classification:** Split (mechanical checks + skill risk review)

**What the CLI computes:**
- Evidence matching against stored tripwire rules
- Preflight run records with provenance
- Match scores and triggered rule IDs

**What moved to skills:**
- Risk interpretation and severity assessment
- Go/no-go recommendations
- Confounder and context analysis

**Skill:** `skills/preflight-risk-review/SKILL.md`

**Degraded outputs:**
```json
{
  "schema": "ee.preflight.run.v1",
  "status": "degraded",
  "degraded": [{
    "code": "preflight_evidence_unavailable",
    "message": "No preflight evidence recorded for this workspace.",
    "repair": "ee preflight run --workspace . --json"
  }]
}
```

---

### Procedure Lifecycle: `procedure propose`, `procedure promote`

**Classification:** Split (mechanical records + skill distillation)

**What the CLI computes:**
- Procedure candidate records with evidence links
- Promotion state machine with audit
- Verification status and drift detection

**What moved to skills:**
- Procedure distillation from session patterns
- Promotion recommendations
- Evidence sufficiency assessment

**Skill:** `skills/procedure-distillation/SKILL.md`

---

### Lab & Counterfactual: `lab capture`, `lab replay`, `lab counterfactual`

**Classification:** Split (mechanical capture + skill analysis)

**What the CLI computes:**
- Episode capture with frozen inputs
- Replay artifact generation
- Counterfactual input preparation

**What moved to skills:**
- Failure analysis and interpretation
- Counterfactual reasoning
- "Would have succeeded" claims

**Skill:** `skills/counterfactual-failure-analysis/SKILL.md`

---

### Situation & Plan: `situation classify`, `plan suggest`

**Classification:** Move to skill (mostly) or static lookup

**What the CLI computes (if kept):**
- Deterministic tag features only
- Static recipe registry lookups

**What moved to skills:**
- Situation framing and classification
- Plan synthesis and recommendations
- Command sequence suggestions

**Skill:** `skills/situation-framing/SKILL.md`

**Degraded outputs:**
```json
{
  "schema": "ee.error.v1",
  "error": {
    "code": "situation_skill_required",
    "message": "Situation classification requires skill interpretation.",
    "repair": "Use skills/situation-framing/SKILL.md workflow."
  }
}
```

---

### Rehearsal: `rehearse plan`, `rehearse run`

**Classification:** Degrade/unavailable pending implementation

**What the CLI computes:**
- Nothing yet — returns honest degraded state

**What it will compute:**
- Real dry-run sandbox artifacts with isolation
- Audit and no-overwrite checks

**Degraded outputs:**
```json
{
  "schema": "ee.error.v1",
  "error": {
    "code": "rehearsal_unavailable",
    "message": "Rehearsal requires isolated sandbox implementation.",
    "repair": "Rehearsal is not yet implemented."
  }
}
```

---

### Economy & Certificate: `economy score`, `certificate verify`

**Classification:** Fix backing data

**What the CLI computes:**
- Read-only metric reports when backed by real data
- Certificate manifest verification

**What changed:**
- No longer returns mock/sample economy metrics
- Returns degraded when real metrics unavailable

---

## Side-Effect and Mutation Table

| Command Family | Mutation Class | Idempotency | Audit |
|----------------|----------------|-------------|-------|
| `memory`, `remember`, `revise` | audited_mutation | by memory ID | always |
| `curate accept/reject` | audited_mutation | by candidate ID | always |
| `rule add/protect` | audited_mutation | by rule key | always |
| `outcome record` | append_only | by event ID | always |
| `learn observe/close` | audited_mutation | by observation/outcome ID | always |
| `preflight run/close` | audited_mutation | by run ID | always |
| `procedure propose/promote` | audited_mutation | by candidate ID | always |
| `import cass` | append_only | by source hash | always |
| `backup create` | side_path_artifact | by manifest hash | always |
| `lab capture/replay` | side_path_artifact | by episode ID | always |
| `index rebuild` | derived_asset_rebuild | by generation | none |
| `graph refresh` | derived_asset_rebuild | by generation | none |
| `context`, `search`, `why` | read_only | fully_idempotent | none |
| `status`, `capabilities` | read_only | fully_idempotent | none |

---

## Skill Handoff Table

| Command Family | Skill Path | Evidence Bundle Schema |
|----------------|------------|------------------------|
| `causal` | `skills/causal-credit-review/SKILL.md` | `ee.skill_evidence_bundle.v1` |
| `lab` | `skills/counterfactual-failure-analysis/SKILL.md` | `ee.skill_evidence_bundle.v1` |
| `preflight` | `skills/preflight-risk-review/SKILL.md` | `ee.skill_evidence_bundle.v1` |
| `procedure` | `skills/procedure-distillation/SKILL.md` | `ee.skill_evidence_bundle.v1` |
| `situation` | `skills/situation-framing/SKILL.md` | `ee.skill_evidence_bundle.v1` |
| `review` | `skills/session-review/SKILL.md` (pending) | `ee.skill_evidence_bundle.v1` |
| `learn experiment` | Experiment planner skill (pending) | `ee.skill_evidence_bundle.v1` |

---

## Degraded Code Reference

| Code | Severity | Meaning | Repair Command |
|------|----------|---------|----------------|
| `storage` | high | Database unavailable | `ee init --workspace .` |
| `search_index_unavailable` | medium | Index stale or missing | `ee index rebuild --workspace .` |
| `context_unavailable` | medium | Context pack failed | Check storage and index |
| `causal_evidence_unavailable` | high | No causal evidence | `ee causal trace --workspace . --json` |
| `causal_sample_underpowered` | medium | Sample too small | Record more outcomes |
| `learning_records_unavailable` | high | Learning ledgers empty | `ee learn observe --json` |
| `experiment_registry_unavailable` | high | No experiment definitions | Use skill workflows |
| `preflight_evidence_unavailable` | medium | No preflight records | `ee preflight run --workspace . --json` |
| `procedure_evidence_unavailable` | medium | No procedure candidates | `ee procedure propose --json` |
| `lab_evidence_unavailable` | medium | No lab episodes | `ee lab capture --workspace . --json` |
| `rehearsal_unavailable` | high | Not implemented | Wait for implementation |
| `situation_skill_required` | medium | Skill interpretation needed | Use situation-framing skill |

---

## Workflow Examples

### Before: Opaque "AI recommendation"

```bash
# Old behavior - fabricated recommendations
$ ee causal promote-plan --target mem_001 --workspace . --json
{
  "action": "promote",
  "confidence": 0.85,
  "rationale": "Strong causal signal detected..."
}
```

### After: Mechanical evidence + skill interpretation

```bash
# Step 1: Get mechanical evidence
$ ee causal trace --workspace . --json > /tmp/causal_trace.json
$ ee causal estimate --workspace . --json > /tmp/causal_estimate.json
$ ee causal promote-plan --workspace . --json > /tmp/promote_plan.json

# Step 2: Check if evidence is sufficient
$ jq '.degraded' /tmp/promote_plan.json
# If degraded, follow repair commands

# Step 3: Invoke skill for interpretation
# The skill reads the JSON evidence and produces recommendations
# with explicit evidence tiers, confounder assessment, and risks
```

### Handling Degraded Output

```bash
# Check for degraded state
$ ee learn experiment run --id my_exp --dry-run --json 2>&1 | jq '.error.code'
"unsatisfied_degraded_mode"

# Get repair command
$ ee learn experiment run --id my_exp --dry-run --json 2>&1 | jq '.error.repair'
"Provide explicit input datasets or use skill workflows for experiment orchestration."

# Follow the repair guidance
# Either provide real data or use the appropriate skill workflow
```

---

## Testing and Validation

### Unit/Static Tests

Tests validate:
- Deprecation-map structure and required sections
- Command references match actual Clap surface
- Workflow IDs exist in documentation
- Matrix-row links resolve correctly
- Skill paths exist in `skills/` directory
- Degraded code examples are accurate
- No stale references to removed/renamed commands

### E2E Tests

E2E scripts exercise representative examples and log:
- Docs path and checked anchors
- Command examples exercised with actual `ee` binary
- Skill paths verified to exist
- Workflow rows validated against README
- stdout/stderr captured for each example
- Schema/golden result comparison
- First-failure diagnosis for debugging

See `tests/migration_guide.rs` for the test harness.

---

## No Features Dropped

This migration **splits responsibilities**, it does not remove functionality:

| Functionality | Before | After |
|---------------|--------|-------|
| Causal credit assignment | Fake data in CLI | Real evidence in CLI + skill interpretation |
| Learning experiments | Hard-coded templates | Degraded CLI + skill orchestration |
| Preflight risk review | Mock assessments | Evidence matching + skill interpretation |
| Procedure distillation | Embedded in CLI | Evidence records + skill workflow |
| Situation framing | Fake classification | Skill workflow with explicit evidence |

Every workflow that previously "worked" (with fake data) now either:
1. Works with real data (mechanical CLI)
2. Degrades honestly and directs to a skill (skill handoff)
3. Reports unavailable with a repair path (degraded state)

The result is a more honest, maintainable, and explainable system.
