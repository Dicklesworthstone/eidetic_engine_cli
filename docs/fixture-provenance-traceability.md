# Fixture Provenance And Scenario Traceability

This document defines the first `fixture_manifest.v1` and
`scenario_traceability.v1` contracts for `ee` tests, golden outputs, replay
artifacts, and evaluation fixtures.

The goal is attribution. A future agent should be able to start from a failing
test, scenario, context pack, or release claim and answer:

- which user outcome the fixture proves
- which bead or gate owns the fixture
- where the source evidence came from
- which secrets are synthetic and which redactions are expected
- which deterministic clocks, IDs, seeds, and workspace fingerprints were used
- which command effects are allowed
- which generated artifacts should exist after a run

This is a docs-only contract. The executable validator and fixture files are
owned by the follow-up beads named below.

## Contract Names

| Contract | Schema Name | Purpose |
| --- | --- | --- |
| Fixture manifest | `fixture_manifest.v1` | Tracks concrete fixture inputs, staged placeholders, provenance, safety posture, deterministic controls, expected effects, and artifact references. |
| Scenario traceability report | `scenario_traceability.v1` | Maps north-star scenarios to fixture IDs, command sequences, golden/schema contracts, degraded branches, effect expectations, implementation beads, and readiness gates. |
| Manifest validation summary | `ee.fixture_manifest.validation.v1` | Future machine-readable output for the local validation command. |

## Canonical Locations

The planned file layout is:

```text
tests/fixtures/manifest.toml
tests/fixtures/scenario_traceability.toml
tests/fixtures/<family>/<fixture-id>/README.md
tests/golden/<command-or-scenario>/
target/ee-e2e/<scenario>/<run-id>/
```

Tracked files should include manifests, schemas, scripts, source fixtures, and
intentional goldens. Generated run artifacts stay under `target/` unless a bead
explicitly promotes an artifact into a golden fixture.

## `fixture_manifest.v1`

The manifest is strict and versioned. The first implementation may use TOML,
YAML, or JSON, but it must serialize canonically before hashing.

Required top-level fields:

| Field | Meaning |
| --- | --- |
| `schema` | Must be `fixture_manifest.v1`. |
| `manifest_id` | Stable manifest handle, initially `ee-fixtures-main`. |
| `manifest_version` | Monotonic integer for intentional format changes. |
| `content_hash` | Hash of the canonical manifest with this field blanked. |
| `generated_artifacts_root` | Normally `target/ee-e2e`. |
| `fixtures` | Ordered fixture entries. |

Required fixture fields:

| Field | Meaning |
| --- | --- |
| `fixture_id` | Stable ID searched from tests, logs, closure dossiers, and scenario reports. |
| `fixture_family` | Family from `docs/testing-strategy.md` or `docs/agent-outcome-scenarios.md`. |
| `coverage_state` | `implemented`, `staged_skip`, or `blocked`. |
| `owning_bead_ids` | Beads that own implementation, drift repair, or staged conversion. |
| `owning_gate_ids` | Readiness gates that require this fixture. |
| `scenario_ids` | North-star scenarios that consume this fixture, or empty for support-only fixtures. |
| `command_family` | Primary command group such as `status`, `context`, `search`, `cass_import`, `redaction`, or `mcp`. |
| `command_sequence` | Exact planned argv templates with deterministic workspace placeholders. |
| `source_kind` | `synthetic`, `derived`, `imported`, or `captured`. |
| `source_hash` | Hash of source fixture data, or `staged:<owner-bead>` until source exists. |
| `synthetic_secret_policy` | `none`, `synthetic_only`, or `forbidden`. |
| `secret_leak_assertions` | Assertions that raw secrets are absent from stdout, exports, failures, and logs. |
| `redaction_classes_expected` | Expected classes, counts, or stable placeholder hashes. |
| `deterministic_seed` | Seed for ranking, graph, fuzz, or run ordering when relevant. |
| `fixed_clock` | Fixed RFC 3339 timestamp or explicit `not_applicable` reason. |
| `stable_ids` | Fixed IDs or deterministic ID-provider policy. |
| `workspace_fingerprints` | Expected workspace identity, symlink, and path-normalization policy. |
| `external_io_posture` | `none`, `local_binary_only`, `local_files_only`, or a future explicit opt-in. |
| `expected_effect_class` | `read_only`, `dry_run`, `idempotent_write`, `audited_write`, or `denied`. |
| `allowed_derived_writes` | Derived paths such as index dirs, pack records, or artifact dirs. |
| `expected_degradation_codes` | Stable degradation codes expected for this fixture. |
| `golden_paths` | Golden outputs that should move with this fixture. |
| `schema_paths` | JSON Schema or contract files that validate output. |
| `artifact_path_pattern` | Expected generated artifact path pattern. |
| `replay_hint` | Command or note for reproducing a failing fixture. |
| `staged_until_bead_ids` | Beads that turn staged placeholders into executable coverage. |
| `skip_until` | Explicit reason a staged fixture is not yet executable. |

Example entry:

```toml
[[fixtures]]
fixture_id = "fx.release_failure.v1"
fixture_family = "release_failure"
coverage_state = "staged_skip"
owning_bead_ids = ["eidetic_engine_cli-gbp2", "eidetic_engine_cli-57k1"]
owning_gate_ids = ["gate.m2.walking_skeleton"]
scenario_ids = ["usr_pre_task_brief"]
command_family = "context"
command_sequence = [
  "ee init --workspace <tmp> --json",
  "ee remember --workspace <tmp> --level procedural --kind rule <fixture-text> --json",
  "ee context \"prepare release\" --workspace <tmp> --max-tokens 4000 --json"
]
source_kind = "synthetic"
source_hash = "staged:eidetic_engine_cli-gbp2"
synthetic_secret_policy = "none"
secret_leak_assertions = []
redaction_classes_expected = []
deterministic_seed = "seed.release_failure.v1"
fixed_clock = "2026-04-29T00:00:00Z"
stable_ids = ["mem_release_rule_001", "pack_release_001"]
workspace_fingerprints = ["tmp-workspace-canonical-path"]
external_io_posture = "none"
expected_effect_class = "audited_write"
allowed_derived_writes = ["target/ee-e2e/usr_pre_task_brief/<run-id>/"]
expected_degradation_codes = ["semantic_disabled", "graph_snapshot_stale"]
golden_paths = ["tests/golden/skeleton/context_pack.json"]
schema_paths = ["tests/contracts/schemas/ee.context.v1.schema.json"]
artifact_path_pattern = "target/ee-e2e/usr_pre_task_brief/<run-id>/"
replay_hint = "ee repro fixture fx.release_failure.v1 --json"
staged_until_bead_ids = ["eidetic_engine_cli-gbp2", "eidetic_engine_cli-57k1"]
skip_until = "walking-skeleton context command and logged e2e runner exist"
```

## Manifest Hashing

The manifest hash is computed from canonical serialized data:

1. Normalize paths to repository-relative POSIX strings.
2. Sort fixture entries by `fixture_id`.
3. Sort string arrays unless their order is explicitly semantic.
4. Blank `content_hash`.
5. Serialize with stable field ordering.
6. Hash with BLAKE3 and store lowercase hex.

Generated e2e artifacts, closure dossiers, and replay summaries must record the
manifest schema and content hash. If a failure report does not include the
manifest hash, the fixture is not attributable enough for a release claim.

## Validation Rules

The future validator should fail closed on:

- duplicate fixture IDs
- fixture families not listed in the strategy or scenario matrix
- orphan fixtures with no owner bead or gate
- scenario IDs that do not appear in `scenario_traceability.v1`
- missing redaction posture
- synthetic secrets without leak assertions
- missing deterministic clock, seed, or stable ID policy where relevant
- undeclared external I/O
- missing expected effect class
- generated artifact paths outside an approved root
- golden or schema paths that do not exist after the owning bead is executable
- staged fixtures with no `skip_until` and no `staged_until_bead_ids`
- scenario rows with neither executable coverage nor a staged-skip rationale
- stale manifest hashes in e2e logs or closure dossiers

Suggested future command:

```bash
ee fixtures validate --manifest tests/fixtures/manifest.toml \
  --traceability tests/fixtures/scenario_traceability.toml \
  --json
```

Suggested JSON summary:

```json
{
  "schema": "ee.fixture_manifest.validation.v1",
  "success": true,
  "data": {
    "manifestHash": "blake3:<hex>",
    "fixturesTotal": 0,
    "implemented": 0,
    "stagedSkip": 0,
    "blocked": 0,
    "scenariosCovered": 0,
    "scenariosStaged": 0
  },
  "degraded": []
}
```

## Fixture Family Baseline

Every fixture family named by the testing strategy or scenario matrix starts in
one of the states below. All current entries are staged because this repository
is still in the foundation slice.

| Fixture Family | Fixture ID | State | Owner / Conversion Beads | Planned Surface |
| --- | --- | --- | --- | --- |
| `empty_workspace` | `fx.empty_workspace.v1` | `staged_skip` | `eidetic_engine_cli-f69h`, `eidetic_engine_cli-57k1` | `ee status`, `ee init` |
| `fresh_workspace` | `fx.fresh_workspace.v1` | `staged_skip` | `eidetic_engine_cli-f69h`, `eidetic_engine_cli-gbp2` | `ee init`, `ee context` |
| `manual_memory` | `fx.manual_memory.v1` | `staged_skip` | `eidetic_engine_cli-57k1`, `eidetic_engine_cli-gbp2` | `ee remember`, `ee search`, `ee context` |
| `stale_index` | `fx.stale_index.v1` | `staged_skip` | `eidetic_engine_cli-g2jl`, Gate 4 | `ee search`, `ee doctor` |
| `offline_degraded` | `fx.offline_degraded.v1` | `staged_skip` | `eidetic_engine_cli-r8r0`, Gate 1 | `ee status`, lexical search |
| `locked_writer` | `fx.locked_writer.v1` | `staged_skip` | `eidetic_engine_cli-oul.1` | writer contention contract |
| `migration_required` | `fx.migration_required.v1` | `staged_skip` | `eidetic_engine_cli-koat`, Gate 2 | migration status and repair |
| `release_failure` | `fx.release_failure.v1` | `staged_skip` | `eidetic_engine_cli-gbp2`, `eidetic_engine_cli-57k1` | release context pack |
| `async_migration` | `fx.async_migration.v1` | `staged_skip` | `eidetic_engine_cli-gbp2`, `eidetic_engine_cli-2crw` | Asupersync guidance ranking |
| `ci_clippy_failure` | `fx.ci_clippy_failure.v1` | `staged_skip` | `eidetic_engine_cli-g2jl` | search and recovery |
| `dangerous_cleanup` | `fx.dangerous_cleanup.v1` | `staged_skip` | `eidetic_engine_cli-g2jl`, `eidetic_engine_cli-9sd5` | tripwire and redaction |
| `secret_redaction` | `fx.secret_redaction.v1` | `staged_skip` | `eidetic_engine_cli-9sd5`, `eidetic_engine_cli-lp4p.1` | redaction storage/render/export |
| `stale_rule` | `fx.stale_rule.v1` | `staged_skip` | `eidetic_engine_cli-1mlo` | curation and confidence decay |
| `graph_linked_decision` | `fx.graph_linked_decision.v1` | `staged_skip` | `eidetic_engine_cli-r8r0`, Gate 4 | graph-stale degradation |
| `conflicting_evidence` | `fx.conflicting_evidence.v1` | `staged_skip` | `eidetic_engine_cli-1mlo` | trust and contradiction handling |
| `false_alarm` | `fx.false_alarm.v1` | `staged_skip` | `eidetic_engine_cli-1mlo` | harmful feedback demotion |
| `procedure_drift` | `fx.procedure_drift.v1` | `staged_skip` | `eidetic_engine_cli-1mlo` | procedure revalidation |
| `causal_confounding` | `fx.causal_confounding.v1` | `staged_skip` | `eidetic_engine_cli-1mlo` | causal-credit uncertainty |
| `cass/v1` | `fx.cass_v1.v1` | `staged_skip` | `eidetic_engine_cli-s67f`, `eidetic_engine_cli-851n` | CASS robot JSON adapter |
| `agent-detect/codex` | `fx.agent_detect_codex.v1` | `staged_skip` | future agent-detect bead | local harness detection |
| `agent-detect/claude` | `fx.agent_detect_claude.v1` | `staged_skip` | future agent-detect bead | local harness detection |
| `mcp/stdio` | `fx.mcp_stdio.v1` | `staged_skip` | future MCP adapter bead | optional read-only MCP adapter |
| `toon` | `fx.toon.v1` | `staged_skip` | future output renderer bead | TOON parity and errors |
| `privacy_export` | `fx.privacy_export.v1` | `staged_skip` | `eidetic_engine_cli-9sd5` | export and backup |
| `multi_workspace` | `fx.multi_workspace.v1` | `staged_skip` | `eidetic_engine_cli-jqhn` | workspace isolation |

Unknown owner IDs in this baseline are intentionally named as future beads
rather than silently omitted. The validator should allow these only while the
fixture state is `staged_skip` and should require real bead IDs before the
corresponding fixture becomes executable.

## `scenario_traceability.v1`

Each row maps an agent journey to its proof surface. `coverage_state` is
`executable_e2e` when the journey has a real command or golden/schema proof
surface, `contract_partial` when only part of the journey is executable, and
`staged_skip` only for fixture branches that still await implementation.

| Scenario ID | Coverage State | Fixture IDs | Commands | Golden / Schema Contracts | Degraded Branches | Effect Expectation | Owners / Gates |
| --- | --- | --- | --- | --- | --- | --- | --- |
| `usr_pre_task_brief` | `executable_e2e` | `fx.fresh_workspace.v1`, `fx.manual_memory.v1`, `fx.release_failure.v1`, `fx.async_migration.v1` | `init`, `remember`, `context --format markdown`, `context --json` | `tests/usr002_pre_task_brief_scenario.rs`, Markdown pack golden, `ee.context.v1`, response envelope | `semantic_disabled`, `graph_snapshot_stale`, empty memory set, reduced token budget | audited writes for init/remember/pack record; read-only context rendering | `eidetic_engine_cli-gbp2`, Gate 7, Gate 14 |
| `usr_in_task_recovery` | `executable_e2e` | `fx.stale_index.v1`, `fx.ci_clippy_failure.v1`, `fx.dangerous_cleanup.v1`, `fx.locked_writer.v1` | `search --explain --json`, `why --json`, `doctor --json`, `preflight --json` contracts | `tests/usr003_in_task_scenario.rs`, search result golden, `ee.why.v1`, doctor repair plan | `search_index_stale`, `lock_contention`, missing memory ID, stale graph | read-only search/why/doctor; denied destructive preflight when risky | `eidetic_engine_cli-g2jl`, Gate 4, Gate 15, Gate 16 |
| `usr_post_task_learning` | `executable_e2e` | `fx.manual_memory.v1`, `fx.procedure_drift.v1`, `fx.false_alarm.v1`, `fx.causal_confounding.v1`, `fx.conflicting_evidence.v1`, `fx.stale_rule.v1` | `outcome`, `review session`, `curate candidates`, `procedure verify`, `learn agenda` | `tests/advanced_e2e.rs::post_task_outcome_scenario_commands_emit_machine_data`, outcome event JSON, curation candidate JSON, audit entry | harmful feedback, contradicted evidence, duplicate candidate, stale procedure | audited writes only; no silent promotion or tombstone | `eidetic_engine_cli-1mlo`, Gate 18, Gate 21, Gate 22 |
| `usr_degraded_offline_trust` | `executable_e2e` | `fx.offline_degraded.v1`, `fx.cass_v1.v1`, `fx.graph_linked_decision.v1`, `fx.manual_memory.v1` | `status --json`, `import cass --dry-run --json`, `search --json`, `context --json` | `tests/usr005_degraded_scenario.rs`, status envelope, import dry-run report, lexical-only search golden | `cass_unavailable`, `external_adapter_schema_mismatch`, `semantic_disabled`, `agent_detector_unavailable`, `graph_snapshot_stale` | dry-run import; read-only degraded search/context | `eidetic_engine_cli-r8r0`, Gate 1, Gate 4, Gate 6 |
| `usr_privacy_export` | `executable_e2e` | `fx.secret_redaction.v1`, `fx.privacy_export.v1`, `fx.dangerous_cleanup.v1` | `remember`, `context --json`, `backup create --json`, backup list/verify/restore | `tests/usr006_privacy_redaction_backup_scenario.rs`, redaction report, context golden with placeholders, backup manifest and record JSONL | `redaction_applied`, blocked secret storage, unsupported shareable export, backup failure | audited writes with explicit redaction; backup records must omit raw secrets | `eidetic_engine_cli-9sd5`, Gate 19, export/backup gates |
| `usr_workspace_continuity` | `executable_e2e` | `fx.multi_workspace.v1`, `fx.fresh_workspace.v1`, `fx.manual_memory.v1`, `fx.cass_v1.v1` | two `init` runs, scoped `remember`, scoped `context`, `workspace list --json` | `tests/smoke.rs::workspace_continuity_scenario_keeps_context_scoped`, workspace registry JSON, scoped context goldens | ambiguous workspace, symlink isolation, moved workspace, missing CASS connector | audited per-workspace writes; read-only list/context | `eidetic_engine_cli-jqhn`, Gate 2, Gate 7 |

## Artifact Requirements

Every e2e, golden/schema, replay, evaluation, and closure-dossier artifact that
uses these fixtures must record:

- `fixture_id`
- `scenario_id` when applicable
- `manifest_schema`
- `manifest_content_hash`
- command argv
- cwd and resolved `--workspace`
- sanitized environment overrides
- elapsed time
- exit code
- stdout and stderr artifact paths
- schema/golden validation status
- redaction status
- expected effect class
- degradation codes observed
- first-failure diagnosis

Generated artifacts should never become anonymous blobs. If an artifact cannot
be mapped back to a fixture ID and manifest hash, it should not be used as
release evidence.

## Closure And Follow-Up

This contract has graduated from future-only traceability to an executable
acceptance-pack index. Closeout should cite the `scenario_traceability.v1` rows
above and the scenario matrix in `docs/agent-outcome-scenarios.md`.

Remaining follow-ups are validator and orchestration hardening, not blockers for
the scenario acceptance pack:

- `eidetic_engine_cli-4wj5` / EE-TST-003: golden/schema contract runner
- `eidetic_engine_cli-57k1` / EE-TST-004: logged walking-skeleton e2e script
- `eidetic_engine_cli-z6kq` / EE-TST-007: verification orchestration and artifact policy
- `eidetic_engine_cli-hkk7` / EE-TST-008: closure evidence dossier
- `eidetic_engine_cli-wgfv` / EE-TST-011: adversarial fuzz/property harness
- `eidetic_engine_cli-gbp2` through `eidetic_engine_cli-jqhn`: executable user scenario beads
