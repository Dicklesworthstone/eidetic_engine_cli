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

## Response Schema Compatibility Window

`ee.response.v2` is the default success envelope:

```json
{"schema":"ee.response.v2","success":true,"data":{},"degraded":[]}
```

For agents pinned to the previous envelope shape, use the documented migration
surface for that release line. Older `0.1.x` builds accepted `--schema-version
v0` or `--legacy-schema` to emit `ee.response.v0`:

<!-- legacy-example: pre-0.2 envelope, no live schema file by design -->
```json
{"schema":"ee.response.v0","ok":true,"result":{}}
```

New integrations should consume the default v2 envelope and read structured
recovery actions from `error.details.recovery[]`.

## Status Graph Fields

`ee status --json` no longer reports the persisted graph cache as
`graph_snapshot`. The live graph algorithms and durable snapshot artifact are
separate surfaces:

- `data.graphCompute` reports whether on-demand graph algorithms such as
  PageRank and betweenness are available.
- `data.graphSnapshotArtifact` reports whether a persisted
  `memory_links` snapshot has been built, is current, or is stale.
- `data.derivedAssets[]` now uses `name: "graph_snapshot_artifact"` with
  `kind: "persisted_snapshot"`; the old `graph_snapshot` asset name is removed.

An empty snapshot artifact is normal on a fresh workspace. It means no
`ee graph centrality-refresh --workspace .` run has produced a persisted
snapshot yet; it does not mean live graph compute is unavailable.

## Code-Anchored Recall Index

Migration V076 adds `memory_anchor_index`, the derived reverse lookup table
used by `ee recall`. It maps a workspace, memory id, and normalized anchor
surface to one of the hot selectors recall can answer quickly:
workspace-relative paths and exact symbol names. The durable source of truth is
still the memory plus anchor records; this table exists only to make pre-edit
lookups fast and deterministic.

`ee migrate status --workspace . --json` reports whether the local database has
applied V076 and later migrations. If the database is older than the binary,
machine-facing commands fail with exit code 8 and an `ee.error.v2` envelope
whose `error.code` is `migration_required`. The repair remains:

```bash
ee migrate run --workspace . --json
```

The table is rebuilt through the normal derived-index path:

```bash
ee index rebuild --workspace . --json
```

Rollback posture is derived-asset safe: losing or dropping
`memory_anchor_index` does not lose memories, anchors, provenance, or feedback.
Do not repair it with manual SQL. Re-run migrations for schema drift and
`ee index rebuild` for stale or missing derived rows so generation accounting
and recall degraded codes stay honest.

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
  "schema": "ee.error.v2",
  "error": {
    "code": "search_index_unavailable",
    "message": "Search index is stale or unavailable.",
    "severity": "medium",
    "repair": "ee index rebuild --workspace .",
    "details": {
      "recovery": [
        {
          "priority": 0,
          "kind": "rebuild",
          "rationale": "Rebuild the derived search index from the database.",
          "command": "ee index rebuild --workspace .",
          "resultsIn": "Search and context commands can satisfy indexed retrieval again."
        }
      ]
    }
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
- Extraction-first typed sidecars for structured failure, decision, command,
  and risk memories when the body contains explicit field markers

**What changed:**
- `revise` internals no longer fabricate revision rationale
- Revisions require explicit user input or skill handoff
- Audit entries are always written for mutations
- `ee remember --kind failure|decision|command|rule|convention|risk|anti-pattern`
  can populate typed fields from explicit body text. Bare or unstructured
  memories remain unchanged; the extractor does not fabricate missing fields.
- The canonical typed sidecar is now `ee.memory.typed_fields.v2`. Existing v1
  sidecars, including failure sidecars from the negative-evidence ledger, remain
  valid input and canonicalize to v2 when revalidated. No data rewrite is
  required for old memories.
- `MAX_TYPED_MEMORY_FIELDS` increased from 4 to 8. This is an additive
  validation change, not a migration hazard: decision memories now need five
  fields (`options`, `chosen`, `rationale`, `supersedes`, `revisit_by`) and the
  extra headroom stays bounded.
- `ee search` accepts `--kind <kind>` plus repeatable `--field name=value`
  filters for typed sidecar fields, plus `name~value` contains and `name^value`
  prefix operators. For example:
  `ee search "prefetch" --kind failure --field family=aggressive-prefetch --json`.
- Negative-evidence ledger tag prefixes such as `family-*`, `cause-*`, and
  `reverted-at-*` are now documented compatibility tags. The canonical typed
  fields are `family`, `cause`, and `reverted_at_sha` when present in the body.
- `ee decide record/list/revisit` is the public micro-ADR workflow over
  decision-kind memories. `record` writes ordinary decision memories with typed
  fields and supersede links; `list` and `revisit` are read-only.
- See [`docs/memory-typed-fields.md`](memory-typed-fields.md) for the full
  registry and [`docs/agent-ux/decide.md`](agent-ux/decide.md) for agent
  workflow guidance.

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

**Degraded output excerpt:** this is an abbreviated command payload, not the
canonical JSON Schema document for the causal command family.
<!-- contract-drift-allow: abbreviated causal promote-plan runtime excerpt; not a docs/schemas contract -->
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
  "schema": "ee.error.v2",
  "error": {
    "code": "unsatisfied_degraded_mode",
    "message": "Experiment execution requires persisted experiment definitions from an evaluation registry.",
    "severity": "medium",
    "repair": "Provide explicit input datasets or use skill workflows for experiment orchestration.",
    "details": {
      "recovery": [
        {
          "priority": 0,
          "kind": "seed",
          "rationale": "The CLI cannot synthesize missing experiment inputs.",
          "example": "Provide an evaluation registry entry with input datasets before rerunning the experiment command.",
          "resultsIn": "Experiment execution has concrete persisted inputs to evaluate."
        }
      ]
    }
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

**Degraded output excerpt:** this is an abbreviated command payload, not the
canonical JSON Schema document for the preflight command family.
<!-- contract-drift-allow: abbreviated preflight runtime excerpt; not a docs/schemas contract -->
```json
{
  "schema": "ee.preflight.report.v1",
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
  "schema": "ee.error.v2",
  "error": {
    "code": "situation_skill_required",
    "message": "Situation classification requires skill interpretation.",
    "severity": "medium",
    "repair": "Use skills/situation-framing/SKILL.md workflow.",
    "details": {
      "recovery": [
        {
          "priority": 0,
          "kind": "none",
          "rationale": "This route intentionally hands interpretation to the documented skill workflow.",
          "resultsIn": "The user follows the skill-owned classification path instead of a fabricated CLI result."
        }
      ]
    }
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
  "schema": "ee.error.v2",
  "error": {
    "code": "rehearsal_unavailable",
    "message": "Rehearsal requires isolated sandbox implementation.",
    "severity": "high",
    "repair": "Rehearsal is not yet implemented.",
    "details": {
      "recovery": [
        {
          "priority": 0,
          "kind": "none",
          "rationale": "No CLI recovery can create the missing isolated sandbox implementation.",
          "resultsIn": "Callers stop relying on rehearsal until the feature lands."
        }
      ]
    }
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

## v0.3.0: Optional Tailscale Mesh Auto-Enrollment (SRR6.46)

The v0.3.0 release introduces an opt-in **mesh auto-enrollment** UX layer on
top of the SRR6 mesh primitives (transport, peer trust, workspace scope,
peer enrollment). When the user has `tailscaled` running and authenticated,
running `ee mesh auto-enroll --workspace . --json` materializes the
peer-group binding in one command — no manual peer-list editing, no
identity-key generation, no policy hand-rolling.

Mesh defaults to OFF. With `EE_MESH_ENABLED=0` (the default), no v0.3.0
code path executes, and ordinary `ee remember/search/context/why` output
is byte-identical to v0.2.x. This invariant is gated by
`tests/mesh_off_no_network.rs` and remains release-blocking.

### Quick upgrade decision tree

- **You don't run multiple machines that share an ee tailnet** — set
  nothing. The default off state is the right state. v0.3.0 is identical
  to v0.2.x for you.
- **You have multiple machines on a tailnet and want shared memory** —
  set `EE_MESH_ENABLED=1`, run `tailscale up` if not already
  authenticated, run `ee daemon --foreground` (so peers can discover you),
  then run `ee mesh auto-enroll --workspace . --json` once per workspace.
- **You had a manual mesh peer-group from v0.2.x** — your manual config
  continues to work as-is. v0.3.0 auto-enrollment will REFUSE to overwrite
  it; opt in to migration via `ee mesh auto-enroll
  --replace-manual-with-auto --workspace .`. The migration is audited
  via `mesh.manual_to_auto_migration_intended`.
- **You restore an ee database from a backup taken on a different
  machine** — v0.3.0 detects the node-key mismatch and refuses
  auto-enrollment with `auto_enrollment_node_key_changed` (medium
  severity). Reversal is `ee mesh disable --reason "restored from
  different machine" && ee mesh auto-enroll`.

### Environment variables added

| Env var | Default | Owner bead | Purpose |
|---|---|---|---|
| `EE_MESH_DISCOVERY_CACHE_TTL_SECONDS` | `30` | bd-36bbk.1.13 | Autodiscovery cache TTL for `ee mesh status` and auto-enroll readiness checks |
| `EE_MESH_DRIFT_SOFT_STALE_AFTER` | `1` | bd-36bbk.1.13 | Missed hello-probe count before a peer enters soft-stale drift grace |
| `EE_MESH_DRIFT_SOFT_STALE_AFTER_SECONDS` | `300` | bd-36bbk.1.13 | Minimum seconds since last success before soft-stale drift grace applies |
| `EE_MESH_DRIFT_HARD_STALE_AFTER` | `3` | bd-36bbk.1.13 | Missed hello-probe count before a peer enters hard-stale drift |
| `EE_MESH_DRIFT_HARD_STALE_AFTER_SECONDS` | `3600` | bd-36bbk.1.13 | Minimum seconds since last success before hard-stale drift applies |
| `EE_MESH_ENABLED` | `0` | bd-36bbk umbrella | Master kill-switch; `0` disables every mesh code path |
| `EE_MESH_HELLO_PORT` | `41888` | bd-36bbk.1.12 | Tailnet-only hello responder port used by `ee daemon` |
| `EE_MESH_HELLO_RESPONDER_DISABLED` | `0` | bd-36bbk.1.12 | Emergency switch to keep mesh enabled but prevent the responder from binding |
| `EE_MESH_MODE` | `off` | bd-36bbk umbrella | Default mesh command mode; accepted values are `off`, `cache`, `revisable`, and `blocking` |
| `EE_TAILSCALE_BINARY_OVERRIDE` | unset | bd-36bbk.1.1 | Test-only: pin the `tailscale` binary path (production code rejects relative paths) |
| `EE_TAILSCALE_PROBE_SOCKET_OVERRIDE` | unset | bd-36bbk.1.1 | Test-only: pin the tailscaled socket path |
| `EE_TAILSCALE_PROBE_TIMEOUT_MS` | `1500` | bd-36bbk.1.1 | Hard budget for the local probe subprocess/socket call |
| `EE_TAILSCALE_DISCOVERY_MODE` | `service_tag` | bd-36bbk.1.7 | Caller-side mesh peer discovery policy; accepted values are `service_tag`, `auto_admit`, `allowlist` |
| `EE_TAILSCALE_RESPOND_MODE` | `service_tag` | bd-36bbk.1.7 | Responder-side mesh discovery consent policy; same value space as `EE_TAILSCALE_DISCOVERY_MODE` |

Future v0.3.x point releases may register further `EE_MESH_*` and
`EE_TAILSCALE_*` env vars as the remaining SRR6.46 sub-beads land
(steward interval, daily cap, etc.). Every new var is registered in
`src/config/env_registry.rs` and documented in `docs/env_vars.md`.

### Schemas added

| Schema | Owner bead | Surface |
|---|---|---|
| `ee.completion_audit.report.v2` | bd-3d6ko.6.1 | Completion-audit report with `localBuildPolicy` state for local Cargo bypass attempts, remote-required blockers, and remote RCH verification |
| `ee.tailscale.local.v1` | bd-36bbk.1.1 | Local `tailscaled` probe report block on `ee status` |
| `ee.mesh.auto_status.v1` | bd-36bbk.1.4 | Auto-enrollment readiness, materialized peer-group, discovery cache, and drift status block |
| `ee.mesh.auto_enrollment_summary.v1` | bd-36bbk.1.5 | Forensic audit-row payload before any peer-group write |
| `ee.mesh.discovery_policy.v1` | bd-36bbk.1.7 | Service-tag, allowlist, denylist, and discovery consent policy |
| `ee.mesh.hello.v1` | bd-36bbk.1.2 (SRR6.46.2) | Tiny bounded handshake request sent to candidate peers (read-only; payload ≤ 4096 bytes) |
| `ee.mesh.hello.response.v1` | bd-36bbk.1.12 (SRR6.46.12) | Hello-handshake success response from the responder when discovery policy grants consent |
| `ee.mesh.hello.error.v1` | bd-36bbk.1.12 (SRR6.46.12) | Hello-handshake decline response; privacy-invariant — must NOT carry responder-side metadata |
| `ee.mesh.hello_responder.status.v1` | bd-36bbk.1.12 (SRR6.46.12) | Read-only hello-responder lifecycle status emitted by `ee mesh hello-responder status` |
| `ee.mesh.lane_grant_preview.v1` | bd-36bbk.1.17 (SRR6.46.17) | Pre-grant lane visibility audit; read-only by construction (`src/mesh/lane_grant_preview.rs`) |
| `ee.repair_action_graph.v1` | bd-36bbk.1.16 (SRR6.46.16) | Shared machine-readable repair-action graph consumed by doctor and mesh status surfaces |
| `ee.mesh.peer_group_binding.v1` | bd-2jb3s (SRR6.30) | Workspace-scoped peer-group authorization record |
| `ee.mesh.peer_policy.v1` | bd-29ulx (SRR6.5) | Per-peer trust, lane policy, and redaction grant |
| `ee.mesh.policy_decision.v1` | bd-29ulx | Per-call policy evaluation outcome |
| `ee.mesh.policy_failure_surface.v1` | bd-29ulx | Structured redaction-safe policy failure surface |
| `ee.mesh.event.v1` | bd-2gtjn (SRR6.3) | Mesh event envelope (append-only export/import) |
| `ee.mesh.storage_status.v1` | bd-2cndm (SRR6.4) | Mesh peer/cursor/import-ledger storage posture |

Each schema has a JSON Schema file at `docs/schemas/<name>.json` and is
covered by the J6 drift gate (`tests/contracts/swarm_schema_lifecycle.rs`
or the equivalent for non-swarm schemas). Adding a new schema without
the corresponding fixture and drift entry will fail CI.

The ADR also names the future audit back-fill payload
`ee.mesh.auto_enrollment_outcome.v1`; document it in this table only after
the JSON Schema file lands under `docs/schemas/`.

### Audit event types added

| Event type | Owner bead | When emitted |
|---|---|---|
| `mesh.auto_enrollment_intended` | bd-36bbk.1.5 | BEFORE any durable peer-group write; fail-closed if this row fails |
| `mesh.auto_enrollment_outcome_recorded` | bd-36bbk.1.5 | Back-fill once SRR6.46.3 knows materialized/rolled_back/dry_run/audit_only |
| `mesh.hello_responder_started` | bd-36bbk.1.12 | Foreground daemon started the supervised hello responder |
| `mesh.hello_responder_stopped` | bd-36bbk.1.12 | Foreground daemon stopped the supervised hello responder cleanly |
| `mesh.hello_responder_crashed_restarted` | bd-36bbk.1.12 | Supervision restarted the responder after a crash |

Future v0.3.x will register additional event types as further sub-beads
land (`mesh.auto_enrollment_materialized`, `mesh.auto_enrollment_rolled_back`,
`mesh.peer_revoked`, `mesh.manual_to_auto_migration_intended`,
`mesh.steward_reconciliation_skipped/triggered/refused/daily_cap_reached`,
`mesh.discovery_policy_changed`, etc.).

Every new event type is added to the `audit_actions` module in
`src/db/mod.rs` and surfaces through `ee audit timeline
--event-type <name> --json`.

### Degraded codes added (highlights)

Full catalog lives in `docs/degraded_code_taxonomy.md`. The high-level
families introduced in v0.3.0 are:

- `tailscale_*` — probe-side issues (`not_installed`, `daemon_unreachable`,
  `not_authenticated`, `binary_inauthentic`, `shields_up`, `probe_timeout`,
  `probe_unavailable`).
- `hello_responder_*` — daemon-side responder lifecycle
  (`not_running`, `port_in_use`, `no_tailscale_ip`, `crash_loop`,
  `rate_limited_storm`, `node_key_mismatch`).
- `auto_enrollment_*` — orchestrator outcomes
  (`no_eligible_peers`, `partial_failure`, `blocked_by_policy`,
  `already_complete`, `concurrent_attempt`, `tailnet_changed`,
  `node_key_changed`, `manual_config_present`, `audit_failed`,
  `sync_once_failed`, `invalid_override_node_key`,
  `manual_migration_unmatched_peer_set`).
- `mesh_disable_*` and `mesh_revoke_*` — rollback path outcomes.
- `discovery_policy_*` — service-tag / allowlist / denylist surfacing.
- `steward_auto_enroll_*` — steward periodic reconciliation outcomes.
- `lane_grant_preview_*` — pre-grant visibility audit outcomes.
- `mesh_outbound_policy_denied/quarantined/rejected` and
  `mesh_peer_policy_denied/quarantined/rejected` — SRR6.5 policy
  failure surfaces.

Every code has a fixture under
`tests/fixtures/failure_modes/<code>.json` matching
`ee.failure_mode_fixture.v1`, gated by the J6 catalog validator.

### Database schema migration

No manual migration step is required.

New tables created idempotently on first `ee mesh status` invocation
after upgrade:

- `ee_mesh_discovery_cache` — TTL cache rows keyed by
  (workspace_id, tailnet_id) for autodiscovery results (SRR6.46.13).
- `ee_mesh_peer_state` — per-peer state machine rows tracking
  active / soft_stale / hard_stale / denylisted with consecutive missed
  probe count + first observed at (SRR6.46.13).

New columns added to existing tables (additive only):

- Peer-group table gains `materialized_on_node_key`, `peer_set_hash`,
  `enrollment_source` (auto / manual / auto_replaced_manual).

Rollback: `ee mesh disable --workspace .` removes the peer-group +
trust + workspace-binding rows transactionally. Per-peer revocation
is via `ee mesh revoke <node-key>`.

### Backward compatibility

- **Mesh-off byte-stability**: the project's release-blocking
  invariant. With `EE_MESH_ENABLED=0`, `ee remember`, `ee search`,
  `ee context`, `ee why`, `ee status` produce byte-identical JSON
  to v0.2.x. Gated by `tests/mesh_off_no_network.rs`.
- **Manual peer-group preservation**: v0.2.x manual `ee mesh enroll`
  configurations continue to work. v0.3.0 auto-enrollment refuses to
  overwrite manual config without an explicit
  `--replace-manual-with-auto` flag.
- **Network-off-with-mesh-enabled byte-stability**: even with
  `EE_MESH_ENABLED=1`, if the tailscale socket is unreachable,
  ordinary `ee` commands still produce byte-identical output and
  exit 0. The probe degrades; it does not block.
- **No new forbidden dependencies**: the SRR6.46 surface uses
  `std::process::Command` + raw socket I/O. No Tokio/Hyper/Axum/Reqwest
  was added. Audited by the existing forbidden-dep gate.

### Known limitations in v0.3.0

- Opt-in real-tailscale smoke test (`bd-36bbk.1.11`) is gated behind
  `EE_E2E_REAL_TAILSCALE=1`; not part of default CI.
- Idle daemon 24h memory slope test (`bd-36bbk.1.15`) runs nightly
  with `EE_E2E_NIGHTLY=1`.
- The 500-peer scale workload is documented but advisory; budgets are
  calibrated on `mac-m3-pro` hardware class.
- Steward periodic reconciliation (`bd-36bbk.1.14`) currently has pure
  decision logic and audit vocabulary only. The scheduler/CLI wiring and
  opt-in env vars are follow-up surface; do not treat
  `EE_MESH_AUTO_ENROLL_ON_DEMAND` as a registered v0.3.0 variable until
  it appears in `src/config/env_registry.rs` and `docs/env_vars.md`.

### Related ADRs

- `docs/adr/0037-optional-mesh-memory.md` — SRR6 mesh umbrella; the
  load-bearing invariants for any optional mesh feature.
- `docs/adr/0038-auto-enrollment-zero-touch.md` — SRR6.46 design
  decisions (wizard rejected; consent via forensic audit row;
  multi-workspace per peer-group; hello responder in `ee daemon`;
  identity guard covers tailnet + node-key; discovery cache + drift
  grace; shared `ee.repair_action_graph.v1`; conservative default
  lane policy; pre-grant lane visibility preview).

See `docs/agent-ux/auto_enrollment_onboarding.md` for the agent-facing
walkthrough.

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
