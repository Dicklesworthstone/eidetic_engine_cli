# Mechanical CLI Boundary Command Inventory

Bead: `eidetic_engine_cli-i6vu`

Generated from static inspection on 2026-05-03. The authoritative command source is the Clap
surface in `src/cli/mod.rs`: global flags live on `Cli` at `src/cli/mod.rs:111`, top-level
commands start at `src/cli/mod.rs:253`, and diagnostic command-path extraction is maintained in
`CliInvocationContext::extract_command_path` at `src/cli/mod.rs:13314`.

`--help-json` is a global exit path at `src/cli/mod.rs:164` and is dispatched before subcommands
at `src/cli/mod.rs:3778`; it is not counted as a command path. The inventory below covers the
145 stable command paths returned by the command-path extractor.

E2E audit record for this inventory:

- Command source: `src/cli/mod.rs`
- Command path count: 145
- Unmapped command count: 0
- Stdout/stderr contract: this file is static documentation; the companion test only reads source
  and docs through `include_str!`, so it does not write stdout artifacts or emit diagnostics.
- First failure diagnosis: missing command paths are reported by
  `tests/mechanical_boundary_inventory.rs`.

## Disposition Legend

- `keep mechanical`: deterministic local computation, file inspection, schema rendering, DB reads or
  writes, index work, hash checks, or renderer output.
- `split`: keep a narrow Rust command for persisted evidence and deterministic reports, and move
  qualitative synthesis or workflow guidance to a project-local skill.
- `move to skill`: command name describes agent judgment rather than local state mutation or
  deterministic computation.
- `degrade/unavailable`: keep the command shape only if it emits stable degraded JSON until a real
  mechanical implementation exists.
- `fix backing data`: command can stay mechanical, but current code uses mock, sample, seed, or
  placeholder data and must be backed by storage or explicit input.

## Boundary Summary

The core CLI is mostly in-bounds: workspace, memory, search, pack, schema, import, backup, install,
graph, curation, rule, artifact, support, and diagnostics are mechanical when they operate over
explicit local inputs and persisted EE state.

The boundary risks cluster in command families that sound like an agent brain: `causal`, `learn`,
`procedure`, `preflight`, `plan`, `rehearse`, `situation`, and parts of `economy`, `certificate`,
`tripwire`, `eval`, and `memory revise` internals. Those surfaces should not invent conclusions from
sample data. They should either return conservative degraded reports, expose raw evidence and
computed facts, or hand off the qualitative work to project-local skills.

## Command Boundary Matrix

This matrix is the regression artifact for `eidetic_engine_cli-frv3`. It maps every public command
family in the Clap surface to its boundary classification and the proof surface required before that
family can be treated as implemented. The full command inventory below still lists every concrete
command path; this matrix records the cross-cutting contract metadata that must be updated when any
command path is added, removed, renamed, or reclassified.

| Surface | Classification | Owner / ADR | README workflow(s) | Mechanical data source | Skill handoff | Degraded code if unavailable | Side-effect / idempotency | Runtime / cancellation posture | Fixture / golden coverage | JSON schema expectation | Required coverage owner |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| `agent` | mechanical CLI | `eidetic_engine_cli-71ep`, ADR 0011 | workspace diagnostics | local agent roots and source probes | none | `agent_detection_unavailable` | class=read_only; idempotency=fully_idempotent; mutation=none | runtime=bounded_read; budget=filesystem_scan; cancel=checkpoint; partial=none; outcome=success_or_degraded | agent golden docs/status fixtures | `ee.response.v1` | unit plus command-matrix contract |
| `analyze` | mechanical CLI | `eidetic_engine_cli-71ep`, ADR 0011 | diagnostics | local feature availability | none | `analysis_unavailable` | class=read_only; idempotency=fully_idempotent; mutation=none | runtime=bounded_read; budget=status_probe; cancel=checkpoint; partial=none; outcome=success_or_degraded | status golden fixture | `ee.response.v1` | contract |
| `agent-docs` | mechanical CLI | `eidetic_engine_cli-frv3`, ADR 0011 | agent command discovery | static embedded docs | none | none | class=read_only; idempotency=fully_idempotent; mutation=none | runtime=immediate; budget=none; cancel=not_applicable; partial=none; outcome=success | agent docs golden fixture | `ee.response.v1` | golden |
| `audit` | mechanical CLI | `eidetic_engine_cli-s43e`, ADR 0011 | audit inspection | audit log records and hashes | none | `audit_log_unavailable` | class=read_only; idempotency=fully_idempotent; mutation=none | runtime=bounded_read; budget=db_file_read; cancel=checkpoint; partial=none; outcome=success_or_storage_error | `eidetic_engine_cli-uiy3` audit fixture corpus | `ee.response.v1` | contract plus e2e |
| `artifact` | mechanical CLI | `eidetic_engine_cli-s43e`, ADR 0011 | artifact registration | artifact paths, hashes, DB links | none | `artifact_store_unavailable` | class=append_only; register writes DB row keyed by content hash with audit; list/inspect read-only; retry returns existing row | runtime=bounded_write; budget=file_hash_db_txn; cancel=pre_write_checkpoint; partial=rollback_or_existing_record; outcome=success_or_storage_error | `eidetic_engine_cli-uiy3` artifact fixture corpus | `ee.response.v1` | unit plus e2e |
| `backup` | mechanical CLI | `eidetic_engine_cli-s43e`, ADR 0011 | backup/export | redacted JSONL backup and manifest files | none | `backup_unavailable` | class=side_path_artifact; create writes backup side path with manifest audit; restore requires explicit destination/import transaction and no-delete/no-overwrite checks; list/inspect/verify read-only | runtime=side_path_artifact; budget=file_io; cancel=checkpoint; partial=side_path_cleanup_or_blocked; outcome=success_or_degraded | backup fixtures | `ee.response.v1` | e2e plus redaction contract |
| `capabilities`, `check`, `health`, `status` | mechanical CLI | `eidetic_engine_cli-5g6d`, ADR 0011 | installation/status | config, DB/index generations, capability probes | none | `status_degraded` | class=read_only; idempotency=fully_idempotent; mutation=none | runtime=bounded_read; budget=status_probe; cancel=checkpoint; partial=none; outcome=success_or_degraded | agent status/capabilities goldens | `ee.response.v1` or `ee.error.v1` | golden plus contract |
| `certificate` | fix backing data | `eidetic_engine_cli-v76q`, ADR 0011 | claim/certificate verification | certificate manifests and artifact hashes | certificate-review skill for interpretation only | `certificate_store_unavailable` | class=read_only_now; verify read-only; future register is append_only manifest store with audit and idempotency key | runtime=bounded_read; budget=manifest_hash; cancel=checkpoint; partial=none; outcome=success_or_degraded | `eidetic_engine_cli-v76q` manifest fixture | `ee.response.v1` | contract plus manifest e2e |
| `causal` | split | `eidetic_engine_cli-dz00`, ADR 0011 | causal evidence review | recorder, pack, preflight, tripwire, procedure, and experiment evidence ledgers | causal credit review skill | `causal_evidence_unavailable` | class=report_only; read-only evidence reports; promote-plan is dry-run plan only; mutation=none | runtime=bounded_query; budget=evidence_query; cancel=checkpoint; partial=none; outcome=success_or_degraded | causal golden contracts | `ee.response.v1` with conservative evidence fields | contract plus no-fake-reasoning |
| `claim` | mechanical CLI | `eidetic_engine_cli-v76q`, ADR 0011 | claim verification | claim manifest, artifact paths, hashes | claim-review skill for interpretation only | `claim_manifest_unavailable` | class=read_only; idempotency=fully_idempotent; mutation=none | runtime=bounded_read; budget=manifest_hash; cancel=checkpoint; partial=none; outcome=success_or_degraded | `eidetic_engine_cli-v76q` claim fixture | `ee.response.v1` | contract |
| `demo` | mechanical CLI | `eidetic_engine_cli-gcru`, ADR 0011 | demo workflows | demo manifests and artifacts | none | `demo_manifest_unavailable` | class=side_path_artifact; list/verify read-only; run --dry-run no-op; non-dry-run unavailable until isolated side-path artifact ledger, audit, and no-delete/no-overwrite checks exist | runtime=side_path_artifact; budget=process_file_checks; cancel=checkpoint; partial=side_path_artifact_only; outcome=success_or_degraded | `eidetic_engine_cli-gcru` demo fixture | `ee.response.v1` | e2e |
| `daemon` | optional adapter wrapper | `eidetic_engine_cli-5g6d`, ADR 0011 | maintenance | configured steward jobs and local DB | none | `daemon_unavailable` | class=supervised_jobs; unavailable now; future jobs mutate only through explicit job ledger, audit, runtime budget, and cancellation | runtime=supervised; budget=job_runtime; cancel=job_signal; partial=job_ledger_audit_or_no_start; outcome=success_or_degraded | `eidetic_engine_cli-v6h4` runtime fixture | `ee.response.v1` | runtime cancellation test |
| `context`, `pack`, `search`, `why` | mechanical CLI | `eidetic_engine_cli-tzmg`, ADR 0011 | core retrieval/context pack | FrankenSQLite memories, Frankensearch index, pack records | none | `storage`, `search_index_unavailable`, `context_unavailable` | class=mixed; context/pack append pack records with audit by pack hash; search/why read-only; failed pack rollback leaves no record | runtime=bounded_query; budget=query_pack_tokens; cancel=checkpoint; partial=rollback_pack_record; outcome=success_or_search_storage_error | walking skeleton goldens | `ee.response.v1` or `ee.error.v1` | smoke, golden, e2e |
| `curate` | mechanical CLI | `eidetic_engine_cli-ynzg`, ADR 0011 | curation queue | review queue records, rule evidence, audit log | curation skill only for judgment-heavy summaries | `curation_store_unavailable` | class=audited_mutation; apply/accept/reject/snooze/merge mutate in one DB transaction with audit; candidates/validate read-only; dry-run no-op when offered | runtime=bounded_write; budget=db_transaction; cancel=pre_commit_checkpoint; partial=rollback_with_audit; outcome=success_or_storage_error | `eidetic_engine_cli-ynzg` queue fixture | `ee.response.v1` | unit plus mutation contract |
| `diag`, `doctor` | mechanical CLI | `eidetic_engine_cli-5g6d`, ADR 0011 | diagnostics/repair | dependency graph, integrity checks, local config, DB/index status | none | `diagnostics_unavailable` | class=read_only; idempotency=fully_idempotent; mutation=none | runtime=bounded_read; budget=diagnostic_probe; cancel=checkpoint; partial=none; outcome=success_or_degraded | `eidetic_engine_cli-5g6d` diagnostic golden | `ee.response.v1` | contract |
| `economy` | fix backing data | `eidetic_engine_cli-ve0w`, ADR 0011 | memory economy | DB-backed utility, cost, debt, and budget metrics | memory-economy review skill for qualitative tradeoffs | `economy_metrics_unavailable` | class=report_only; report/score/simulate/prune-plan read-only plans; mutation=none until explicit apply command exists | runtime=bounded_query; budget=metric_query; cancel=checkpoint; partial=none; outcome=success_or_degraded | `eidetic_engine_cli-ve0w` metric fixture | `ee.response.v1` | contract plus no-seed-data test |
| `eval` | fix backing data | `eidetic_engine_cli-uiy3`, ADR 0011 | evaluation | deterministic fixture registry and evaluation reports | none | `eval_fixtures_unavailable` | class=side_path_artifact; run writes evaluation report side path keyed by fixture hash/run id with audit and no-overwrite; list read-only; degraded until fixture registry exists | runtime=side_path_artifact; budget=fixture_runner; cancel=checkpoint; partial=side_path_report_or_blocked; outcome=success_or_degraded | eval fixtures and goldens | `ee.response.v1` | fixture/e2e |
| `handoff` | mechanical CLI | `eidetic_engine_cli-g9dq`, ADR 0011 | handoff/resume | redacted continuity capsule over explicit evidence | handoff review skill consumes `ee.skill_evidence_bundle.v1` with provenance, redaction, trust, degraded, and prompt-injection quarantine fields only; no direct DB scraping or durable mutation outside explicit `ee` commands | `handoff_unavailable` | class=side_path_artifact; create writes redacted capsule side path plus audit; preview/inspect/resume read-only or explicit import with no-overwrite | runtime=side_path_artifact; budget=redaction_db_read; cancel=checkpoint; partial=capsule_side_path_or_blocked; outcome=success_or_degraded | `eidetic_engine_cli-g9dq` redaction, prompt-injection quarantine, stale bundle, missing provenance, malformed bundle, and degraded CLI fixtures | `ee.response.v1` plus `ee.skill_evidence_bundle.v1` for skill handoff | redaction plus e2e |
| `help`, `version`, `introspect`, `schema`, `mcp`, `model` | mechanical CLI or optional adapter wrapper | `eidetic_engine_cli-frv3`, ADR 0011 | discovery/schema | static metadata, model registry, optional MCP manifest | none | `adapter_unavailable` only for optional adapters | class=read_only; idempotency=fully_idempotent; mutation=none | runtime=immediate; budget=none; cancel=not_applicable; partial=none; outcome=success_or_degraded | skeleton goldens | `ee.response.v1` | golden |
| `init`, `workspace` | mechanical CLI | `eidetic_engine_cli-hy6y`, ADR 0011 | workspace setup | workspace filesystem, config, registry | none | `workspace_unavailable` | class=audited_mutation; init/alias idempotently create/update EE-owned workspace records in one transaction with audit; resolve/list read-only; no-delete/no-overwrite | runtime=bounded_write; budget=filesystem_db_init; cancel=pre_write_checkpoint; partial=rollback_no_overwrite; outcome=success_or_config_storage_error | M1 storage/status gates | `ee.response.v1` | e2e |
| `import` | mechanical CLI | `eidetic_engine_cli-hy6y`, ADR 0011 | import history | CASS robot JSON, JSONL, legacy export files | none | `import_source_unavailable` | class=append_only; import writes DB records keyed by source hashes in one transaction with audit; duplicate import is idempotent | runtime=bounded_write; budget=import_records; cancel=checkpoint; partial=rollback_transaction; outcome=success_or_import_error | `eidetic_engine_cli-hy6y` import fixture | `ee.response.v1` | e2e plus fixture |
| `install`, `update` | mechanical CLI | `eidetic_engine_cli-hy6y`, ADR 0011 | install/update | local manifest, release metadata, filesystem checks | none | `install_manifest_unavailable` | class=side_path_artifact; plan/check read-only; update writes verified side path then atomic replace with no-delete/no-silent-overwrite and rollback | runtime=side_path_artifact; budget=manifest_file_plan; cancel=checkpoint; partial=rollback_side_path_or_blocked; outcome=success_or_degraded | install workflow tests | `ee.response.v1` | contract |
| `graph` | mechanical CLI | `eidetic_engine_cli-hy6y`, ADR 0011 | graph analytics | DB records projected through FrankenNetworkX | none | `graph_unavailable` | class=derived_asset_rebuild; refresh mutates derived graph/index by DB generation; export/neighborhood read-only; source DB unchanged | runtime=derived_rebuild; budget=graph_projection; cancel=checkpoint; partial=derived_asset_discard; outcome=success_or_degraded | graph smoke tests | `ee.response.v1` | unit plus smoke |
| `index` | mechanical CLI | `eidetic_engine_cli-hy6y`, ADR 0011 | index management | FrankenSQLite source of truth, Frankensearch derived index | none | `search_index_unavailable` | class=derived_asset_rebuild; rebuild/reembed mutate derived index idempotently by source generation; status read-only; source DB unchanged | runtime=derived_rebuild; budget=index_rebuild; cancel=checkpoint; partial=derived_asset_discard; outcome=success_or_search_error | search/index smoke tests | `ee.response.v1` | e2e |
| `lab` | split | `eidetic_engine_cli-db4z`, ADR 0011 | failure lab | captured episodes, replay artifacts, counterfactual inputs | counterfactual failure-analysis skill | `lab_evidence_unavailable` | class=side_path_artifact; capture/replay write explicit episode/replay side-path artifacts with audit and no-overwrite; counterfactual report read-only or degraded | runtime=side_path_artifact; budget=replay; cancel=checkpoint; partial=episode_replay_side_path_only; outcome=success_or_degraded | `eidetic_engine_cli-db4z` lab fixture | `ee.response.v1` | e2e plus no-fake-reasoning |
| `learn` | split | `eidetic_engine_cli-evah`, ADR 0011 | active learning | observation ledgers, experiment registry, evaluation snapshots | experiment planner skill | `learning_ledger_unavailable` | class=audited_mutation; observe/close/run mutate ledgers in DB transaction with audit/idempotency IDs; agenda/uncertainty/summary read-only | runtime=bounded_write; budget=db_eval; cancel=pre_commit_checkpoint; partial=rollback_with_audit; outcome=success_or_degraded | `eidetic_engine_cli-evah` ledger fixture | `ee.response.v1` | contract plus mutation test |
| `memory`, `remember` | mechanical CLI | `eidetic_engine_cli-6956`, ADR 0011 | manual memory | memory DB records, revisions, provenance | none | `storage` | class=audited_mutation; remember/revise mutate memory/revision records with audit in one transaction; list/show/history read-only; dry-run no-op when offered | runtime=bounded_write; budget=db_transaction; cancel=pre_commit_checkpoint; partial=rollback_with_audit; outcome=success_or_storage_error | walking skeleton and memory tests | `ee.response.v1` or `ee.error.v1` | unit plus e2e |
| `outcome` | mechanical CLI | `eidetic_engine_cli-s43e`, ADR 0011 | feedback/outcomes | feedback events, quarantine records, audit log | none | `outcome_store_unavailable` | class=audited_mutation; record/quarantine release mutate feedback/quarantine rows with audit and caller idempotency IDs in one transaction; list read-only | runtime=bounded_write; budget=db_transaction; cancel=pre_commit_checkpoint; partial=rollback_with_audit; outcome=success_or_storage_error | outcome goldens | `ee.response.v1` | contract |
| `preflight` | split | `eidetic_engine_cli-bijm`, ADR 0011 | pre-task risk review | evidence matches, stored preflight runs, tripwire records | preflight risk-review skill | `preflight_evidence_unavailable` | class=audited_mutation; run/close mutate preflight ledger in one transaction with audit/idempotency ID; show read-only; degraded until persisted evidence exists | runtime=bounded_query; budget=evidence_query; cancel=checkpoint; partial=rollback_preflight_record; outcome=success_or_degraded | preflight goldens | `ee.response.v1` | e2e plus no-fake-reasoning |
| `plan` | move to skill unless static lookup | `eidetic_engine_cli-6cks`, ADR 0011 | task planning | static recipe registry only if kept in Rust | situation/command-planning skill | `planning_skill_required` | class=read_only_or_unavailable; static recipe reads only with mutation=none; judgment workflow unavailable or skill handoff | runtime=immediate; budget=none; cancel=not_applicable; partial=none; outcome=success_or_degraded | `eidetic_engine_cli-6cks` static recipe fixture | `ee.response.v1` with skill handoff | skill-boundary contract |
| `procedure` | split | `eidetic_engine_cli-q5vf`, ADR 0011 | procedure lifecycle | stored procedure records, eval fixtures, repro packs, claim evidence | procedure distillation skill | `procedure_evidence_unavailable` | class=audited_mutation; propose/promote mutate candidate/procedure records with audit in one transaction; show/list/export/verify/drift read-only; dry-run plan before mutation | runtime=bounded_write; budget=verification_db_txn; cancel=pre_commit_checkpoint; partial=rollback_with_audit; outcome=success_or_degraded | `eidetic_engine_cli-q5vf` procedure fixture | `ee.response.v1` | contract plus fixture |
| `recorder` | mechanical CLI | `eidetic_engine_cli-6xzc`, ADR 0011 | session recorder | persisted event spine and import plan records | none | `recorder_store_unavailable` | class=append_only; start/event/finish/import append event-store records with run/event IDs and audit; tail read-only | runtime=streaming; budget=event_import_tail; cancel=job_signal; partial=append_checkpoint_or_no_write; outcome=success_or_degraded | `eidetic_engine_cli-6xzc` recorder golden | `ee.response.v1` | e2e plus runtime |
| `rehearse` | degrade/unavailable pending implementation | `eidetic_engine_cli-nd65`, ADR 0011 | rehearsal | real dry-run sandbox artifacts when implemented | rehearsal/promotion review skill | `rehearsal_unavailable` | class=degraded_unavailable; no mutation now; plan/inspect read-only; run writes sandbox side path only after real isolation, audit, and no-overwrite checks exist | runtime=degraded_unavailable; budget=sandbox_runtime; cancel=checkpoint; partial=none_until_real_isolation; outcome=degraded_error | `eidetic_engine_cli-nd65` rehearsal fixture | `ee.response.v1` or degraded `ee.error.v1` | no-fake-success e2e |
| `review` | split | `eidetic_engine_cli-0hjw`, ADR 0011 | session review | CASS session evidence and candidate records | session review/distillation skill | `review_evidence_unavailable` | class=report_only_or_append; evidence extraction read-only; candidate write requires explicit append_only DB/audit path | runtime=bounded_query; budget=cass_evidence_read; cancel=checkpoint; partial=candidate_append_rollback_or_none; outcome=success_or_degraded | `eidetic_engine_cli-0hjw` session fixture | `ee.response.v1` | skill-boundary contract |
| `rule` | mechanical CLI | `eidetic_engine_cli-ynzg`, ADR 0011 | procedural rules | rule DB records, protection metadata, audit log | none | `rule_store_unavailable` | class=audited_mutation; add/protect mutate rule records in one transaction with audit and idempotency by rule key; list/show read-only | runtime=bounded_write; budget=db_transaction; cancel=pre_commit_checkpoint; partial=rollback_with_audit; outcome=success_or_storage_error | rule tests | `ee.response.v1` | unit plus contract |
| `situation` | move to skill or split deterministic tagging | `eidetic_engine_cli-6cks`, ADR 0011 | situation framing | persisted situation records and deterministic tag features only | situation framing skill | `situation_skill_required` | class=report_only_or_audited_mutation; classify/compare/show/explain read-only or degraded; link mutates relation records only through explicit audited transaction | runtime=bounded_query; budget=feature_query; cancel=checkpoint; partial=rollback_relation_write; outcome=success_or_degraded | `eidetic_engine_cli-6cks` situation fixture | `ee.response.v1` with skill handoff | skill-boundary contract |
| `support` | mechanical CLI | `eidetic_engine_cli-hy6y`, ADR 0011 | support bundle | redacted diagnostics, config, logs, manifests | none | `support_bundle_unavailable` | class=side_path_artifact; bundle create writes redacted artifact side path with manifest audit and no-overwrite; inspect read-only | runtime=side_path_artifact; budget=redaction_file_io; cancel=checkpoint; partial=bundle_side_path_or_blocked; outcome=success_or_degraded | `eidetic_engine_cli-hy6y` support fixture | `ee.response.v1` | redaction plus e2e |
| `tripwire` | fix backing data | `eidetic_engine_cli-qmu0`, ADR 0011 | tripwire check | persisted rules/tripwires and explicit event payloads | preflight risk-review skill may interpret results | `tripwire_store_unavailable` | class=read_only; idempotency=fully_idempotent; check/list read persisted rules over explicit payload; mutation=none | runtime=bounded_read; budget=rule_evaluation; cancel=checkpoint; partial=none; outcome=success_or_degraded | tripwire goldens | `ee.response.v1` | contract plus fixture |

### Matrix Maintenance Rules

- Every command path in the inventory below must appear either directly or through its family in the
  matrix above.
- The `Side-effect / idempotency` cell must start with one of the class labels in
  `Side-Effect Contract Vocabulary` below. Free-form prose is not enough.
- The `Runtime / cancellation posture` cell must start with one of the class labels in
  `Runtime / Cancellation Contract Vocabulary` below and include `budget=`, `cancel=`,
  `partial=`, and `outcome=` fields.
- A row may say `none` for skill handoff only when the command remains fully mechanical.
- A row may say `none` for degraded code only when the command is static/read-only and cannot depend
  on absent local state.
- A row marked `move to skill` must include a project-local skill handoff and a skill-boundary test
  owner.
- A row marked `fix backing data`, `split`, or `degrade/unavailable pending implementation` must name a
  degraded code and a follow-up bead that owns the real implementation.

### Side-Effect Contract Vocabulary

Every matrix row must use one of these side-effect classes:

- `class=read_only`: no DB, index, cache, or filesystem mutation; retries are fully idempotent.
- `class=read_only_now`: the current public command is read-only or degraded; any future write path
  must name a new audited class before it ships.
- `class=report_only`: output is computed from explicit inputs and may include dry-run plans, but it
  must not write durable state.
- `class=read_only_or_unavailable`: static catalog reads are allowed; judgment-heavy or missing
  implementations return degraded output or hand off to skills.
- `class=append_only`: writes only new records or returns an existing record by an idempotency key
  such as content hash, source hash, caller ID, run ID, or rule key.
- `class=audited_mutation`: changes durable state in one named DB transaction with rollback,
  audit record, idempotency behavior, and dry-run no-op where the command exposes `--dry-run`.
- `class=derived_asset_rebuild`: mutates only rebuildable derived assets keyed by source DB/index
  generation; source DB records remain unchanged.
- `class=side_path_artifact`: creates or verifies a new side-path artifact such as a backup,
  capsule, report, support bundle, or sandbox artifact; tests must prove no delete, no silent
  overwrite, and rollback or blocked status on verification failure.
- `class=supervised_jobs`: long-running mutation is allowed only through an explicit job ledger,
  audit record, runtime budget, and cancellation path. Core CLI commands must not require a daemon.
- `class=mixed`: a family contains both read-only and append/audited subcommands; the cell must name
  which concrete paths write and which stay read-only.
- `class=degraded_unavailable`: no mutation occurs until the real implementation exists; public JSON
  reports the degraded code and repair path.
- `class=report_only_or_append`: current evidence extraction is read-only; candidate writes require
  an explicit append-only/audited path.
- `class=report_only_or_audited_mutation`: deterministic reports are read-only; relation writes use
  an explicit audited transaction.

Mutating classes must name transaction scope, audit behavior, DB generation/index generation effect
where applicable, retry/idempotency behavior, dry-run/no-op behavior where exposed, and recovery or
degraded behavior. Side-path artifact classes must name no-delete/no-overwrite behavior. Read-only
classes must explicitly state `mutation=none`.

### Runtime / Cancellation Contract Vocabulary

Every matrix row must use one of these runtime classes:

- `runtime=immediate`: static metadata, embedded docs, or recipe lookup with no meaningful I/O
  budget; cancellation is not applicable before output construction.
- `runtime=bounded_read`: bounded filesystem, DB, manifest, hash, probe, or rule-evaluation read
  with cooperative cancellation checkpoints and no partial durable state.
- `runtime=bounded_query`: bounded query, search, pack, feature, evidence, or CASS read path with an
  explicit query/token/evidence budget and deterministic degraded/error mapping on cancellation.
- `runtime=bounded_write`: bounded DB or filesystem-backed mutation with cancellation checkpoints
  before commit and a named rollback/idempotency posture.
- `runtime=side_path_artifact`: command may create a backup, bundle, capsule, report, demo, replay,
  or install/update side path; cancellation must leave only audited, blocked, or discarded side-path
  artifacts and no silent overwrite.
- `runtime=derived_rebuild`: command mutates only rebuildable derived graph/search/index artifacts
  keyed by source generation; cancellation discards or marks incomplete derived output while leaving
  source DB records unchanged.
- `runtime=supervised`: long-running job path with explicit job runtime budget, cancellation signal,
  job ledger/audit record, and deterministic `Outcome` mapping.
- `runtime=streaming`: tail/follow/import stream path with event or import budgets, cancellation
  signal, append checkpoints, and stable partial-progress reporting.
- `runtime=degraded_unavailable`: no real execution happens until implementation exists; the command
  returns stable degraded JSON/error output and does not mutate durable state.

Every runtime cell must include:

- `budget=<name>` for the runtime, token, query, filesystem, transaction, job, or side-path budget.
- `cancel=<checkpoint>` for the cancellation injection/observation point.
- `partial=<policy>` for rollback, no-write, side-path-only, derived-asset discard, audit, or none.
- `outcome=<mapping>` for the observed `Outcome`/exit-code family on success, cancellation,
  budget exhaustion, storage/index failure, or degraded unavailability.

### E2E Matrix Log Schema

Boundary-matrix E2E logs use schema `ee.command_boundary_matrix.e2e_log.v1` and must record:

- generated command list;
- matrix path and BLAKE3 hash;
- missing and extra command rows;
- classification summary;
- side-effect coverage summary;
- schema coverage summary;
- workflow parity coverage;
- fixture/evidence bundle hashes where fixtures are required;
- runtime budget, deadline, and budget exhaustion signal;
- cancellation injection point and observed cancellation phase;
- observed `Outcome` and process exit code;
- before/after DB and index generation when applicable;
- changed record IDs and audit IDs when a command mutates durable state;
- records written, rolled back, or audited for mutating commands;
- filesystem artifacts created and forbidden filesystem operations checked;
- stdout and stderr artifact paths;
- parsed JSON schema and golden validation result when a command is executed;
- first-failure diagnosis.

## README Workflow Parity Matrix

Bead `eidetic_engine_cli-myk6` owns this matrix. It maps the workflows advertised in
`README.md` to the post-migration user path after the mechanical CLI boundary split. The contract
is explicit: no feature was dropped silently. A workflow is either covered by a mechanical `ee`
command path, covered by `ee` plus a project-local skill handoff, honestly degraded with a repair
command, or intentionally deferred with an owning bead and rationale. If README examples, command
names, skill paths, or degraded behavior change, this matrix and
`tests/mechanical_boundary_inventory.rs` must change in the same patch.

E2E workflow scripts that exercise rows from this matrix must log schema
`ee.workflow_parity.e2e_log.v1` with the workflow ID, generated command list, commands run, skill
paths used, degraded states observed, artifact paths, stdout and stderr artifact paths, parsed JSON
schema or golden status, and first-failure diagnosis.

| Workflow ID | README surface | Post-migration user path | Required ee commands | Project-local skill | Degraded / unavailable behavior | Repair command | Owning bead IDs | Test / E2E coverage | No-feature-loss status |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| install-verify | Installation and Verify | Install or build `ee`, then verify local posture before using memory workflows. | `version`, `doctor`, `status`, `install check`, `install plan`, `update` | none | `install_manifest_unavailable`, `diagnostics_unavailable`, or `status_degraded` reports without mutating user files. | `ee doctor --json`, `ee install check --json`, then rerun `ee status --json`. | `eidetic_engine_cli-hy6y`, `eidetic_engine_cli-k6ob`, `eidetic_engine_cli-5g6d` | install workflow contract tests plus status/doctor goldens | covered; no feature was dropped because install/update remain mechanical and unavailable metadata degrades with repair output. |
| quick-example-context-loop | TLDR and Quick Example | Initialize, remember or import evidence, retrieve task context, inspect why a memory was selected, then record outcome feedback. | `init`, `remember`, `import cass`, `context`, `why`, `outcome`, `search` | none | `storage`, `search_index_unavailable`, `context_unavailable`, or `import_source_unavailable`; explicit memory still works when CASS is absent. | `ee init --workspace . --json`, `ee index rebuild --workspace .`, `ee status --json`. | `eidetic_engine_cli-6956`, `eidetic_engine_cli-tzmg`, `eidetic_engine_cli-s43e`, `eidetic_engine_cli-hy6y` | walking-skeleton e2e, search/context/why goldens, mechanical-boundary parity contract | covered; no feature was dropped because the headline context loop remains direct CLI behavior. |
| quick-start-core-loop | Quick Start | Run the six-step README loop: init, optional CASS import, context, remember, review/curate, search. | `init`, `import cass`, `context`, `remember`, `review session`, `curate candidates`, `curate apply`, `search` | session-review/distillation skill only for qualitative session summaries; curation writes stay in `ee`. | `import_source_unavailable`, `curation_store_unavailable`, `storage`, or `search_index_unavailable` with stable JSON errors. | `ee status --json`, `ee import cass --dry-run --json`, `ee curate candidates --workspace .`. | `eidetic_engine_cli-hy6y`, `eidetic_engine_cli-6956`, `eidetic_engine_cli-0hjw`, `eidetic_engine_cli-ynzg`, `eidetic_engine_cli-tzmg` | advanced e2e workflow logs plus curation/search contract tests | covered; no feature was dropped because qualitative review is split to a skill while persisted curation remains mechanical. |
| core-command-reference | Command Reference core workflow | Keep the README core command table mapped to mechanical commands and machine schemas. | `init`, `status`, `doctor`, `context`, `search`, `remember`, `outcome`, `why`, `pack` | none | per-command degraded codes from the command boundary matrix: `storage`, `status_degraded`, `diagnostics_unavailable`, `search_index_unavailable`, `context_unavailable`. | `ee doctor --json`, `ee status --json`, `ee index rebuild --workspace .`. | `eidetic_engine_cli-frv3`, `eidetic_engine_cli-myk6`, `eidetic_engine_cli-tzmg`, `eidetic_engine_cli-6956` | command matrix contract plus README workflow parity contract | covered; no feature was dropped because every README core command is present in the command extractor. |
| import-ingestion | Command Reference Import and ingestion | Import explicit external evidence from CASS robot JSON, JSONL records, or legacy artifacts without duplicating external stores. | `import cass`, `import jsonl`, `import eidetic-legacy`, `review session` | session-review/distillation skill for proposed memories after deterministic extraction. | `import_source_unavailable`; import is append-only and idempotent by source hash. | `ee import cass --workspace . --limit 50 --dry-run --json`, then install or configure `cass` if missing. | `eidetic_engine_cli-hy6y`, `eidetic_engine_cli-0hjw`, `eidetic_engine_cli-gcru` | import fixture e2e plus workflow parity contract | covered; no feature was dropped because every advertised import path remains an explicit CLI adapter or skill-assisted distillation step. |
| curation-rules | Curation and rules | List, validate, apply, accept, reject, snooze, merge, and protect procedural candidates through audited mutations. | `curate candidates`, `curate validate`, `curate apply`, `curate accept`, `curate reject`, `curate snooze`, `curate merge`, `curate disposition`, `rule add`, `rule list`, `rule show`, `rule protect` | curation skill only for judgment-heavy summaries before candidate mutation. | `curation_store_unavailable`, `rule_store_unavailable`, or `outcome_store_unavailable`; no silent promotion. | `ee curate candidates --workspace . --json`, `ee doctor --json`. | `eidetic_engine_cli-ynzg`, `eidetic_engine_cli-s43e`, `eidetic_engine_cli-g9dq` | curation mutation contracts plus redacted handoff fixtures | covered; no feature was dropped because candidate lifecycle writes stay mechanical and judgment is explicitly skill-scoped. |
| memory-inspection | Memory inspection | Inspect memories, history, provenance, revisions, and why output without mutating state unless an explicit revise path is accepted. | `memory show`, `memory list`, `memory history`, `memory revise`, `why` | none | `storage`; `memory revise` is dry-run or policy denied until immutable revision storage is available. | `ee memory show <id> --json`, `ee status --json`. | `eidetic_engine_cli-6956`, `eidetic_engine_cli-tzmg` | memory unit/e2e tests plus why explanation goldens | covered; no feature was dropped because read paths remain direct and revise is honestly constrained. |
| graph-index-derived-assets | Graph and Index | Rebuild or inspect derived graph/search assets from FrankenSQLite source truth. | `graph export`, `graph neighborhood`, `graph centrality-refresh`, `graph feature-enrichment`, `index status`, `index rebuild`, `index reembed` | none | `graph_unavailable` or `search_index_unavailable`; derived assets can be discarded and rebuilt without source DB loss. | `ee index rebuild --workspace .`, `ee graph feature-enrichment --dry-run --json`, `ee status --json`. | `eidetic_engine_cli-hy6y`, `eidetic_engine_cli-tzmg` | graph/index smoke tests, search/context e2e, workflow parity contract | covered; no feature was dropped because graph/search are derived mechanical assets with explicit degraded repair paths. |
| workspace-model-schema-adapters | Workspace, models, schemas, and MCP | Resolve workspace identity, inspect model/schema posture, and expose optional MCP metadata while keeping CLI as the compatibility contract. | `workspace resolve`, `workspace list`, `workspace alias`, `model status`, `model list`, `schema list`, `schema export`, `mcp manifest`, `agent-docs`, `help`, `introspect` | none | `workspace_unavailable` or `adapter_unavailable`; optional adapters do not block CLI usage. | `ee workspace resolve --workspace . --json`, `ee schema list --json`, `ee mcp manifest --json`. | `eidetic_engine_cli-hy6y`, `eidetic_engine_cli-frv3` | schema/help/version/model goldens plus command matrix contract | covered; no feature was dropped because optional adapters are metadata wrappers over CLI schemas. |
| backup-restore | Backup and Restore | Create, list, inspect, verify, and restore backups into isolated side paths with no silent overwrite. | `backup create`, `backup list`, `backup inspect`, `backup verify`, `backup restore` | none | `backup_unavailable`; manifest/path validation failure is reported before mutation. | `ee backup verify <id> --json`, `ee backup restore <id> --side-path <path>`. | `eidetic_engine_cli-hy6y`, `eidetic_engine_cli-s43e` | backup manifest and restore side-path validation tests | covered; no feature was dropped because backup/restore remain side-path mechanical artifacts. |
| diagnostics-eval-ops | Diagnostics, eval, and ops | Report capability posture, run deterministic fixture evaluations, and expose optional daemon mode without requiring it for core workflows. | `capabilities`, `check`, `health`, `doctor`, `diag claims`, `diag dependencies`, `diag graph`, `diag integrity`, `diag quarantine`, `diag streams`, `eval run`, `eval list`, `daemon`, `analyze science-status` | none | `status_degraded`, `diagnostics_unavailable`, `eval_fixtures_unavailable`, or `daemon_unavailable`. | `ee doctor --json`, `ee diag integrity --json`, `ee eval list --json`. | `eidetic_engine_cli-5g6d`, `eidetic_engine_cli-uiy3`, `eidetic_engine_cli-v6h4` | diagnostic degraded contracts, eval fixture contracts, runtime cancellation tests | covered; no feature was dropped because optional daemon/eval surfaces degrade until fixtures or jobs exist. |
| configuration-context-profiles | Configuration and Context Profiles | Select context profiles through CLI flags/env/config while preserving trust, privacy, and deterministic packing rules. | `context`, `pack`, `status`, `workspace resolve` | none | invalid config or profile returns configuration error; semantic model loss degrades to lexical-only status. | `ee context "<task>" --profile balanced --json`, `ee status --json`, `ee workspace resolve --json`. | `eidetic_engine_cli-tzmg`, `eidetic_engine_cli-frv3` | context profile golden tests plus pack schema contracts | covered; no feature was dropped because profiles remain command options, not separate hidden commands. |
| cass-integration | CASS Integration | Probe CASS, dry-run imports, import session evidence, and optionally distill a session into candidates. | `import cass`, `review session`, `status`, `doctor` | session-review/distillation skill for qualitative memory proposals. | `import_source_unavailable`; `ee remember`, `ee context`, and `ee search` remain available without CASS. | `ee import cass --workspace . --limit 50 --dry-run --json`, `ee status --json`. | `eidetic_engine_cli-hy6y`, `eidetic_engine_cli-0hjw`, `eidetic_engine_cli-6956`, `eidetic_engine_cli-tzmg` | CASS import fixture e2e plus degraded-status contract | covered; no feature was dropped because CASS is explicitly optional evidence, not a hard dependency. |
| agent-harness-integration | Agent Harness Integration | Harnesses shell out to stable CLI commands; optional MCP manifests mirror CLI schemas without replacing them. | `context`, `remember`, `outcome`, `curate candidates`, `memory show`, `mcp manifest`, `handoff create`, `handoff inspect`, `handoff resume` | handoff review skill consumes `ee.skill_evidence_bundle.v1` only for agent-facing review. | `adapter_unavailable` or `handoff_unavailable`; CLI commands remain usable from plain shells. | `ee context "<task>" --workspace . --json`, `ee mcp manifest --json`, `ee handoff create --json`. | `eidetic_engine_cli-g9dq`, `eidetic_engine_cli-frv3`, `eidetic_engine_cli-tzmg` | handoff redaction e2e plus MCP/schema goldens | covered; no feature was dropped because harness integration is still shell-first and skill handoff is explicit. |
| privacy-trust | Privacy and Trust | Redact secrets, assign trust classes, quarantine prompt-injection candidates, and expose provenance/audit trails. | `remember`, `outcome`, `curate candidates`, `rule protect`, `handoff create`, `handoff preview`, `why` | handoff review skill and curation skill operate only on redacted evidence bundles. | `handoff_unavailable`, `curation_store_unavailable`, `outcome_store_unavailable`, or policy-denied mutation; suspicious memories quarantine instead of promoting. | `ee handoff preview --json`, `ee curate candidates --json`, `ee why <memory-id> --json`. | `eidetic_engine_cli-g9dq`, `eidetic_engine_cli-ynzg`, `eidetic_engine_cli-s43e`, `eidetic_engine_cli-7pq.1` | redaction, quarantine, trust, and outcome contract fixtures | covered; no feature was dropped because privacy/trust behavior is preserved as mechanical policy plus audited skill input. |
| troubleshooting | Troubleshooting | Resolve README error cases with explicit status, repair commands, and degraded capabilities. | `index rebuild`, `index reembed`, `import cass`, `init`, `workspace list`, `workspace alias`, `status`, `doctor`, `model status` | none | `search_index_unavailable`, `import_source_unavailable`, `migration_required`, `workspace_unavailable`, `adapter_unavailable`, or lexical-only model degradation. | `ee index rebuild --workspace .`, `ee init --workspace . --json`, `ee workspace list`, `ee model status --json`. | `eidetic_engine_cli-hy6y`, `eidetic_engine_cli-5g6d`, `eidetic_engine_cli-tzmg` | diagnostic degraded goldens plus workflow parity contract | covered; no feature was dropped because each README error has a mapped command and repair path. |
| limitations-faq-docs | Limitations, FAQ, and Documentation | Keep non-goals honest: no agent harness replacement, no chat UI, no mandatory daemon, no paid API dependency, and MCP remains optional. | `status`, `agent-docs`, `doctor`, `mcp manifest`, `backup create`, `index rebuild` | none | intentionally deferred behavior is unavailable by design; future web UI, daemon-first operation, or distributed multi-writer surfaces require new beads before implementation. | `ee status --json`, `ee agent-docs --json`, `ee doctor --json`. | `eidetic_engine_cli-lp4p`, `eidetic_engine_cli-myk6`, `eidetic_engine_cli-3c93` | docs parity contract plus roadmap/deprecation documentation follow-up | deferred with rationale; no feature was dropped because README limitations are documented non-goals and deferred behavior names owning beads. |

## Baseline Infrastructure Coverage Ledger

Bead `eidetic_engine_cli-hy6y` uses this ledger to keep the boring command families visible as
first-class mechanical surfaces. The ledger is intentionally narrower than the full matrix: it only
covers infrastructure commands that agents depend on for setup, state mutation, import/export,
indexing, status, and deterministic fixture execution.

The current Clap command-path extractor has no standalone `profile list`, `profile show`, `db
status`, `db migrate`, `db check`, `db backup`, `index vacuum`, `restore`, `export jsonl`, `eval
report`, `completion`, or `config` command paths. Those names are therefore tracked as absence
findings here rather than counted as implemented commands. If any of them are added later, this
ledger and the command-boundary matrix must gain concrete rows before the new path ships.

| Baseline surface | Actual command paths or absence finding | Mechanical source | Side-effect contract | Runtime contract | Degraded / repair posture | Evidence required |
| --- | --- | --- | --- | --- | --- | --- |
| workspace setup and registry | `init`, `workspace resolve`, `workspace list`, `workspace alias` | workspace filesystem, EE config, workspace registry | class=audited_mutation for `init`/`workspace alias`; read-only for resolve/list; no-delete/no-overwrite | runtime=bounded_write; budget=filesystem_db_init; cancel=pre_write_checkpoint; partial=rollback_no_overwrite; outcome=success_or_config_storage_error | `workspace_unavailable`; repair via `ee init --workspace <path>` or config diagnostics | fresh-workspace e2e plus path-canonicalization unit coverage |
| manual memory write/read | `remember`, `memory list`, `memory show`, `memory history`, `memory revise` | memory DB rows, provenance, revision history | class=audited_mutation for `remember`; dry-run preview or policy denial for `memory revise` until immutable revision storage exists; read-only for list/show/history; rollback with audit | runtime=bounded_write; budget=db_transaction; cancel=pre_commit_checkpoint; partial=rollback_with_audit; outcome=success_or_storage_error_or_policy_denied | `storage`; repair via `ee status --json` and DB diagnostics | walking-skeleton e2e plus JSON/golden response coverage |
| outcome feedback and quarantine | `outcome`, `outcome quarantine list`, `outcome quarantine release` | feedback events, quarantine records, audit log | class=audited_mutation for record/release; list read-only; caller idempotency IDs | runtime=bounded_write; budget=db_transaction; cancel=pre_commit_checkpoint; partial=rollback_with_audit; outcome=success_or_storage_error | `outcome_store_unavailable`; repair via status/doctor storage checks | formulaic outcome unit tests plus quarantine contract coverage |
| explicit imports | `import cass`, `import jsonl`, `import eidetic-legacy` | CASS robot JSON, JSONL files, legacy export files | class=append_only; source-hash idempotency; duplicate import returns existing records | runtime=bounded_write; budget=import_records; cancel=checkpoint; partial=rollback_transaction; outcome=success_or_import_error | `import_source_unavailable`; repair names missing source path or command | import idempotency tests plus fixture e2e |
| derived search index | `index status`, `index rebuild`, `index reembed` | FrankenSQLite source generations and Frankensearch derived index | class=derived_asset_rebuild; source DB unchanged; rebuild/reembed keyed by generation | runtime=derived_rebuild; budget=index_rebuild; cancel=checkpoint; partial=derived_asset_discard; outcome=success_or_search_error | `search_index_unavailable`; repair via `ee index rebuild --workspace <path>` | index status/rebuild e2e plus stale-index contract |
| backup and restore side paths | `backup create`, `backup list`, `backup inspect`, `backup verify`, `backup restore` | redacted JSONL backup and manifest files | class=side_path_artifact; create/restore side paths with manifest audit, no-delete, no-overwrite | runtime=side_path_artifact; budget=file_io; cancel=checkpoint; partial=side_path_cleanup_or_blocked; outcome=success_or_degraded | `backup_unavailable`; repair names manifest/path validation failure | backup manifest verification and restore side-path validation tests |
| export renderers currently present | `schema export`, `graph export`, `procedure export` | schema registry, graph snapshots, procedure records/artifacts | class=read_only or side_path_artifact as owned matrix row states; no standalone export store | runtime=bounded_read; budget=renderer_or_graph_export; cancel=checkpoint; partial=none_or_side_path_only; outcome=success_or_degraded | `adapter_unavailable`, `graph_unavailable`, or `procedure_evidence_unavailable` by surface | schema/graph/procedure export goldens and fixture contracts |
| deterministic evaluation entrypoint | `eval run`, `eval list` | deterministic fixture registry and evaluation reports | class=side_path_artifact; currently degraded until fixture registry exists; no sample success | runtime=side_path_artifact; budget=fixture_runner; cancel=checkpoint; partial=side_path_report_or_blocked; outcome=success_or_degraded | `eval_fixtures_unavailable`; repair names missing fixture/report registry | eval unavailable contract now; fixture e2e before implementation |
| status, health, and config-sensitive probes | `status`, `health`, `check`, `capabilities`, `doctor`, `diag claims`, `diag dependencies`, `diag graph`, `diag integrity`, `diag quarantine`, `diag streams` | config, DB/index generations, dependency probes, integrity checks | class=read_only; idempotency=fully_idempotent; mutation=none | runtime=bounded_read; budget=status_probe; cancel=checkpoint; partial=none; outcome=success_or_degraded | `status_degraded` or `diagnostics_unavailable`; repair command included when one exists | status/capabilities goldens plus diagnostic degraded contracts |
| static discovery and schemas | `help`, `version`, `introspect`, `schema list`, `model status`, `model list`, `mcp manifest`, `agent-docs` | static metadata, embedded docs, schema registry, optional adapter manifest | class=read_only; idempotency=fully_idempotent; mutation=none | runtime=immediate; budget=none; cancel=not_applicable; partial=none; outcome=success_or_degraded | `adapter_unavailable` only for optional adapter metadata | schema/help/version/model golden coverage |
| non-present baseline terms | `profile list`, `profile show`, `db status`, `db migrate`, `db check`, `db backup`, `index vacuum`, `restore`, `export jsonl`, `eval report`, `completion`, `config` are not present in the 145 command-path extractor | absence finding from `CliInvocationContext::extract_command_path` | class=read_only_now; no public command mutation exists; future command must define audit/idempotency first | runtime=immediate; budget=none; cancel=not_applicable; partial=none; outcome=degraded_or_not_present | no degraded code because no public command exists; if added, must name one before shipping | command inventory contract must fail until matrix and ledger rows are added |

## Full Command Inventory

| Command path(s) | Handler/core anchor | Current source shape | Target disposition |
| --- | --- | --- | --- |
| `agent detect`, `agent status`, `agent sources`, `agent scan` | `src/cli/mod.rs:3788`, `src/cli/mod.rs:11228` | Local filesystem/probe inspection. Optional origin fixtures are explicit. | keep mechanical |
| `analyze science-status` | `src/cli/mod.rs:3791` | Local availability/status report. | keep mechanical |
| `agent-docs` | `src/cli/mod.rs:3803`, `src/cli/mod.rs:5569` | Static command docs. | keep mechanical |
| `audit timeline`, `audit show`, `audit diff`, `audit verify` | `src/cli/mod.rs:3804` | Audit log readers/verifiers. | keep mechanical |
| `artifact register`, `artifact inspect`, `artifact list` | `src/cli/mod.rs:3816`, `src/cli/mod.rs:12809` | Local artifact metadata, hashes, DB links. | keep mechanical |
| `backup create`, `backup list`, `backup inspect`, `backup restore`, `backup verify` | `src/cli/mod.rs:3825`, `src/core/backup.rs:507` | Redacted JSONL backup and manifest inspection/verification. | keep mechanical |
| `capabilities`, `check`, `health`, `status` | `src/cli/mod.rs:3840`, `src/cli/mod.rs:3859`, `src/cli/mod.rs:4159`, `src/cli/mod.rs:4575` | Local posture reports. `status.memory_health` must remain DB-backed or honest degraded. | keep mechanical; fix backing data where health is synthetic |
| `certificate list`, `certificate show`, `certificate verify` | `src/cli/mod.rs:3878`, `src/core/certificate.rs:360` | Currently uses mock certificate records. | fix backing data |
| `causal trace`, `causal compare`, `causal estimate`, `causal promote-plan` | `src/cli/mod.rs:3885`, `src/cli/mod.rs:11994` | Causal reports over recorder, pack, preflight, tripwire, procedure, and experiment evidence. Several paths currently fabricate sample conclusions. | split; degrade/unavailable until evidence-backed |
| `claim list`, `claim show`, `claim verify` | `src/cli/mod.rs:3893`, `src/cli/mod.rs:12295` | Claims manifest and artifact verification. | keep mechanical |
| `demo list`, `demo run`, `demo verify` | `src/cli/mod.rs:3898`, `src/cli/mod.rs:12465` | Explicit demo manifests and artifacts. | keep mechanical |
| `daemon` | `src/cli/mod.rs:3903` | Foreground steward loop over configured jobs. | keep mechanical; optional only |
| `context`, `pack`, `search`, `why` | `src/cli/mod.rs:3904`, `src/cli/mod.rs:4351`, `src/cli/mod.rs:4559`, `src/cli/mod.rs:4635` | DB/search/packing/explanation over persisted memories and indexes. | keep mechanical |
| `curate candidates`, `curate validate`, `curate apply`, `curate accept`, `curate reject`, `curate snooze`, `curate merge`, `curate disposition` | `src/cli/mod.rs:4484` | Explicit review queue and audited candidate mutations. | keep mechanical |
| `diag claims`, `diag dependencies`, `diag graph`, `diag integrity`, `diag quarantine`, `diag streams` | `src/cli/mod.rs:3905`, `src/cli/mod.rs:8248` | Local diagnostics and stream separation checks. | keep mechanical |
| `doctor` | `src/cli/mod.rs:3966` | Health and fix-plan reporting. Fix plans must remain deterministic repair suggestions, not agent planning. | keep mechanical |
| `economy report`, `economy score`, `economy simulate`, `economy prune-plan` | `src/cli/mod.rs:4206`, `src/cli/mod.rs:13051` | Utility, budget, debt, and retire/compact/merge reports. Current core includes seed data. | fix backing data; split recommendation language if qualitative |
| `eval run`, `eval list` | `src/cli/mod.rs:4218`, `src/output/mod.rs:4092` | Evaluation renderer exists, but the default path is a stub/no-scenarios report. | fix backing data |
| `handoff preview`, `handoff create`, `handoff inspect`, `handoff resume` | `src/cli/mod.rs:4033` | Redacted continuity capsules over explicit local evidence. | keep mechanical |
| `help`, `version`, `introspect`, `schema list`, `schema export`, `mcp manifest`, `model status`, `model list` | `src/cli/mod.rs:3787`, `src/cli/mod.rs:4617`, `src/cli/mod.rs:4270`, `src/cli/mod.rs:4438`, `src/cli/mod.rs:4421`, `src/cli/mod.rs:4435` | Static/help/schema/version/model-registry surfaces. | keep mechanical |
| `init`, `workspace resolve`, `workspace list`, `workspace alias` | `src/cli/mod.rs:4178`, `src/cli/mod.rs:4610`, `src/cli/mod.rs:5265` | Workspace filesystem and registry operations. | keep mechanical |
| `import cass`, `import jsonl`, `import eidetic-legacy` | `src/cli/mod.rs:4255` | Import adapters over explicit external command/file outputs. | keep mechanical |
| `install check`, `install plan`, `update` | `src/cli/mod.rs:4264`, `src/cli/mod.rs:4634`, `src/core/install.rs:62` | Offline manifest and install planning. No apply path in this slice. | keep mechanical |
| `graph export`, `graph centrality-refresh`, `graph feature-enrichment`, `graph neighborhood` | `src/cli/mod.rs:4332`, `src/cli/mod.rs:8335` | Derived graph projections and bounded features. | keep mechanical |
| `index rebuild`, `index reembed`, `index status` | `src/cli/mod.rs:4293`, `src/cli/mod.rs:5785` | Derived index management. | keep mechanical |
| `lab capture`, `lab replay`, `lab counterfactual` | `src/cli/mod.rs:4302`, `src/core/lab.rs:501` | Episode capture/replay/counterfactual surfaces. Capture can be mechanical; interpretation of counterfactual impact is agent-skill territory unless fully evidence-backed. | split |
| `learn agenda`, `learn uncertainty`, `learn experiment propose`, `learn experiment run`, `learn observe`, `learn close`, `learn summary` | `src/cli/mod.rs:4311`, `src/core/learn.rs:89` | Learning ledgers, observations, and experiment records. Proposal language and agenda synthesis can cross into agent judgment. | split |
| `memory list`, `memory show`, `memory history`, `memory revise`, `remember` | `src/cli/mod.rs:4284`, `src/cli/mod.rs:4471`, `src/core/memory.rs:164` | Memory DB reads/writes, dry-run revision previews, and audit history. | keep mechanical; `memory revise` remains dry-run/policy-denied until revision storage exists |
| `outcome`, `outcome quarantine list`, `outcome quarantine release` | `src/cli/mod.rs:4344`, `src/cli/mod.rs:10072` | Explicit feedback events and quarantine review. | keep mechanical |
| `preflight run`, `preflight show`, `preflight close` | `src/cli/mod.rs:4352`, `src/core/preflight.rs:798` | Risk briefs, prompts, tripwires, and close feedback. Current run uses task-text heuristics; show lacks persistence. | split; fix backing data |
| `plan goal`, `plan recipe list`, `plan recipe show`, `plan explain` | `src/cli/mod.rs:4361` | Goal-to-recipe routing and explanations. | move to skill unless restricted to static recipe lookup |
| `procedure propose`, `procedure show`, `procedure list`, `procedure export`, `procedure promote`, `procedure verify`, `procedure drift` | `src/cli/mod.rs:4373`, `src/core/procedure.rs:61` | Procedure records, exports, promotion plans, verification, and drift. Verification currently includes mock fixture results. | split; fix backing data |
| `recorder start`, `recorder event`, `recorder finish`, `recorder tail`, `recorder import` | `src/cli/mod.rs:4394`, `src/cli/mod.rs:7638` | Persisted event capture and read-only import planning. Tail/follow must read real events or report unavailable. | keep mechanical; fix backing data for tail/follow gaps |
| `rehearse plan`, `rehearse run`, `rehearse inspect`, `rehearse promote-plan` | `src/cli/mod.rs:4409`, `src/core/rehearse.rs:344` | Rehearsal planning/inspection and claimed sandbox execution. Current run simulates execution and hashes. | degrade/unavailable until real isolation exists |
| `review session` | `src/cli/mod.rs:4556` | Session review and candidate proposals. | split; keep only deterministic evidence extraction in Rust |
| `rule add`, `rule list`, `rule show`, `rule protect` | `src/cli/mod.rs:4544`, `src/core/rule.rs:551` | Procedural rule storage, listing, and protection audit. | keep mechanical |
| `situation classify`, `situation compare`, `situation link`, `situation show`, `situation explain` | `src/cli/mod.rs:4560`, `src/core/situation.rs:842` | Task classification, relation scoring, curation link planning, and explanations. Show/explain are stubs. | move to skill or split into deterministic tagging plus skill workflow |
| `support bundle`, `support inspect` | `src/cli/mod.rs:4604`, `src/cli/mod.rs:12941` | Redacted diagnostic bundle creation/inspection. | keep mechanical |
| `tripwire list`, `tripwire check` | `src/cli/mod.rs:4611`, `src/core/tripwire.rs:247` | Tripwire listing/checking. Current implementation uses sample tripwires and string conditions. | fix backing data |

## Mock, Sample, Stub, Or Simulated Data Anchors

These anchors are the highest-priority follow-up targets because they can make a command look more
implemented than it is.

| Surface | Anchor | Boundary problem | Required target |
| --- | --- | --- | --- |
| Causal trace | `src/core/causal.rs:335`, `src/core/causal.rs:379`, `src/core/causal.rs:387` | Placeholder reports build a sample chain when filters are present. | Return empty/degraded or query persisted recorder, pack, preflight, tripwire, and procedure evidence. |
| Causal estimate | `src/core/causal.rs:859`, `src/core/causal.rs:892`, `src/core/causal.rs:920` | Sample estimates contain invented uplift, confidence, and sample size. | Only compute from explicit evidence or return degraded. |
| Procedure verify | `src/core/procedure.rs:1174`, `src/core/procedure.rs:1176`, `src/core/procedure.rs:1179` | Verification can pass against mock fixture IDs. | Verify real eval fixtures, repro packs, claim evidence, or recorder runs. |
| Rehearse run/inspect | `src/core/rehearse.rs:361`, `src/core/rehearse.rs:364`, `src/core/rehearse.rs:372`, `src/core/rehearse.rs:825` | Command execution and hashes are simulated. | Either run real isolated dry-run execution or return stable unavailable/degraded JSON. |
| Eval run/list | `src/output/mod.rs:4092`, `src/output/mod.rs:4103`, `src/output/mod.rs:4290` | Renderers return a stub/no-scenarios report when no runner report exists. | Wire `eval run` to fixture discovery/execution or mark unavailable. |
| Tripwire list/check | `src/core/tripwire.rs:247`, `src/core/tripwire.rs:309`, `src/core/tripwire.rs:376` | Uses generated sample tripwires, and `check` evaluates string markers rather than an event payload. | Read persisted rules/tripwires and require explicit event payloads for checks. |
| Preflight show | `src/core/preflight.rs:870`, `src/core/preflight.rs:886`, `src/core/preflight.rs:900` | `show` returns a stub run and degraded stale-evidence marker. | Read persisted preflight runs or return not found/unavailable. |
| Preflight run | `src/core/preflight.rs:810`, `src/core/preflight.rs:817`, `src/core/preflight.rs:954` | Risk level and prompts are task-text heuristics. | Keep only deterministic evidence matching in Rust; move ask-now/advice phrasing into skills. |
| Situation show/explain | `src/core/situation.rs:1926`, `src/core/situation.rs:1931`, `src/core/situation.rs:1946`, `src/core/situation.rs:1951` | Stub details and recommendations are returned without stored state. | Query persisted situations or move explanation workflow to a skill. |
| Situation compare/link | `src/core/situation.rs:842`, `src/core/situation.rs:863`, `src/core/situation.rs:884` | Produces recommendation/confidence over text heuristics. | Keep deterministic similarity facts only, or move recommendation to a skill. |
| Certificate list/show/verify | `src/core/certificate.rs:360`, `src/core/certificate.rs:404`, `src/core/certificate.rs:418`, `src/core/certificate.rs:457` | Certificate commands use mock records. | Back by certificate storage/manifests or report unavailable. |
| Economy reports/plans | `src/core/economy.rs:359`, `src/core/economy.rs:524`, `src/core/economy.rs:735`, `src/core/economy.rs:822` | Economy reports rely on seed artifacts and seed prune recommendations. | Back by DB metrics or conservative abstention. |
| Memory revise internal | `src/core/memory.rs:1572`, `src/core/memory.rs:1578` | Internal revision flow returns stub success without writing revision storage. | Do not expose as successful mutation until revision storage is real. |

## Immediate Follow-Up Map

| Follow-up bead | Inventory finding it should consume |
| --- | --- |
| `eidetic_engine_cli-dape` | Write the mechanical-boundary ADR using this file as command inventory. |
| `eidetic_engine_cli-jp06` | Add degraded-honesty contracts for all `degrade/unavailable` and `fix backing data` rows. |
| `eidetic_engine_cli-bijm` | Re-scope `preflight` to deterministic evidence matching and move prompts/advice to skills. |
| `eidetic_engine_cli-qmu0` | Replace sample tripwires with persisted rule/event evaluation. |
| `eidetic_engine_cli-q5vf` | Split procedure storage/export from procedure authoring and mock verification. |
| `eidetic_engine_cli-dz00` | Replace causal sample chains/estimates with evidence ledgers or degraded output. |
| `eidetic_engine_cli-db4z` | Keep lab capture/replay mechanical and move counterfactual interpretation out of core. |
| `eidetic_engine_cli-6xzc` | Wire recorder tail/follow/import planning to real persisted event data. |
| `eidetic_engine_cli-6956` | Ensure memory revise and status health persist real state or degrade honestly. |
| `eidetic_engine_cli-6cks` | Move situation classification/planning recipes out of core decision-making. |
| `eidetic_engine_cli-ve0w` | Replace memory economy seeds with DB-backed metrics. |
| `eidetic_engine_cli-v76q` | Replace mock certificate verification with real manifest/hash checks. |
| `eidetic_engine_cli-nd65` | Make rehearsal either execute real dry-run isolation or report unavailable. |
| `eidetic_engine_cli-evah` | Keep learning observation ledgers mechanical and move experiment design to skills. |
