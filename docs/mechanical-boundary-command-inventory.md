# Mechanical CLI Boundary Command Inventory

Bead: `eidetic_engine_cli-i6vu`

Generated from static inspection on 2026-05-03. The authoritative command source is the Clap
surface in `src/cli/mod.rs`: global flags live on `Cli` at `src/cli/mod.rs:111`, top-level
commands start at `src/cli/mod.rs:253`, and diagnostic command-path extraction is maintained in
`CliInvocationContext::extract_command_path` at `src/cli/mod.rs:13314`.

`--help-json` is a global exit path at `src/cli/mod.rs:164` and is dispatched before subcommands
at `src/cli/mod.rs:3778`; it is not counted as a command path. The inventory below covers the
144 stable command paths returned by the command-path extractor.

E2E audit record for this inventory:

- Command source: `src/cli/mod.rs`
- Command path count: 144
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
| `agent` | mechanical CLI | `eidetic_engine_cli-71ep`, ADR 0011 | workspace diagnostics | local agent roots and source probes | none | `agent_detection_unavailable` | read-only, idempotent | bounded filesystem scan, cancellable | agent golden docs/status fixtures | `ee.response.v1` | unit plus command-matrix contract |
| `analyze` | mechanical CLI | `eidetic_engine_cli-71ep`, ADR 0011 | diagnostics | local feature availability | none | `analysis_unavailable` | read-only, idempotent | bounded status read, cancellable | status golden fixture | `ee.response.v1` | contract |
| `agent-docs` | mechanical CLI | `eidetic_engine_cli-frv3`, ADR 0011 | agent command discovery | static embedded docs | none | none | read-only, idempotent | immediate | agent docs golden fixture | `ee.response.v1` | golden |
| `audit` | mechanical CLI | `eidetic_engine_cli-s43e`, ADR 0011 | audit inspection | audit log records and hashes | none | `audit_log_unavailable` | read-only, idempotent | bounded file/DB read, cancellable | `eidetic_engine_cli-uiy3` audit fixture corpus | `ee.response.v1` | contract plus e2e |
| `artifact` | mechanical CLI | `eidetic_engine_cli-s43e`, ADR 0011 | artifact registration | artifact paths, hashes, DB links | none | `artifact_store_unavailable` | register mutates once by content hash; list/inspect read-only | bounded file hash with cancellation | `eidetic_engine_cli-uiy3` artifact fixture corpus | `ee.response.v1` | unit plus e2e |
| `backup` | mechanical CLI | `eidetic_engine_cli-s43e`, ADR 0011 | backup/export | redacted JSONL backup and manifest files | none | `backup_unavailable` | create/restore mutate with manifest audit; list/inspect/verify read-only | bounded file IO, cancellable | backup fixtures | `ee.response.v1` | e2e plus redaction contract |
| `capabilities`, `check`, `health`, `status` | mechanical CLI | `eidetic_engine_cli-5g6d`, ADR 0011 | installation/status | config, DB/index generations, capability probes | none | `status_degraded` | read-only, idempotent | immediate to bounded DB probe | agent status/capabilities goldens | `ee.response.v1` or `ee.error.v1` | golden plus contract |
| `certificate` | fix backing data | `eidetic_engine_cli-v76q`, ADR 0011 | claim/certificate verification | certificate manifests and artifact hashes | certificate-review skill for interpretation only | `certificate_store_unavailable` | verify read-only; future register mutates manifest store | bounded manifest hash checks, cancellable | `eidetic_engine_cli-v76q` manifest fixture | `ee.response.v1` | contract plus manifest e2e |
| `causal` | split | `eidetic_engine_cli-dz00`, ADR 0011 | causal evidence review | recorder, pack, preflight, tripwire, procedure, and experiment evidence ledgers | causal credit review skill | `causal_evidence_unavailable` | read-only reports; promote-plan emits plan only | bounded evidence query, cancellable | causal golden contracts | `ee.response.v1` with conservative evidence fields | contract plus no-fake-reasoning |
| `claim` | mechanical CLI | `eidetic_engine_cli-v76q`, ADR 0011 | claim verification | claim manifest, artifact paths, hashes | claim-review skill for interpretation only | `claim_manifest_unavailable` | list/show/verify read-only | bounded manifest hash checks, cancellable | `eidetic_engine_cli-v76q` claim fixture | `ee.response.v1` | contract |
| `demo` | mechanical CLI | `eidetic_engine_cli-gcru`, ADR 0011 | demo workflows | demo manifests and artifacts | none | `demo_manifest_unavailable` | run may create isolated artifacts; list/verify read-only | bounded process/file checks, cancellable | `eidetic_engine_cli-gcru` demo fixture | `ee.response.v1` | e2e |
| `daemon` | optional adapter wrapper | `eidetic_engine_cli-5g6d`, ADR 0011 | maintenance | configured steward jobs and local DB | none | `daemon_unavailable` | long-running supervised mutation through explicit jobs | cancellable runtime budget required | `eidetic_engine_cli-v6h4` runtime fixture | `ee.response.v1` | runtime cancellation test |
| `context`, `pack`, `search`, `why` | mechanical CLI | `eidetic_engine_cli-tzmg`, ADR 0011 | core retrieval/context pack | FrankenSQLite memories, Frankensearch index, pack records | none | `storage`, `search_index_unavailable`, `context_unavailable` | context/pack persist pack records; search/why read-only | bounded query/packing budget, cancellable | walking skeleton goldens | `ee.response.v1` or `ee.error.v1` | smoke, golden, e2e |
| `curate` | mechanical CLI | `eidetic_engine_cli-ynzg`, ADR 0011 | curation queue | review queue records, rule evidence, audit log | curation skill only for judgment-heavy summaries | `curation_store_unavailable` | apply/accept/reject/snooze/merge mutate with audit; candidates/validate read-only | bounded DB transaction, cancellable | `eidetic_engine_cli-ynzg` queue fixture | `ee.response.v1` | unit plus mutation contract |
| `diag`, `doctor` | mechanical CLI | `eidetic_engine_cli-5g6d`, ADR 0011 | diagnostics/repair | dependency graph, integrity checks, local config, DB/index status | none | `diagnostics_unavailable` | read-only, idempotent | bounded probes, cancellable | `eidetic_engine_cli-5g6d` diagnostic golden | `ee.response.v1` | contract |
| `economy` | fix backing data | `eidetic_engine_cli-ve0w`, ADR 0011 | memory economy | DB-backed utility, cost, debt, and budget metrics | memory-economy review skill for qualitative tradeoffs | `economy_metrics_unavailable` | report/score/simulate/prune-plan read-only plans until explicit apply exists | bounded metric query, cancellable | `eidetic_engine_cli-ve0w` metric fixture | `ee.response.v1` | contract plus no-seed-data test |
| `eval` | fix backing data | `eidetic_engine_cli-uiy3`, ADR 0011 | evaluation | deterministic fixture registry and evaluation reports | none | `eval_fixtures_unavailable` | run writes evaluation report; list read-only | bounded fixture runner, cancellable | eval fixtures and goldens | `ee.response.v1` | fixture/e2e |
| `handoff` | mechanical CLI | `eidetic_engine_cli-g9dq`, ADR 0011 | handoff/resume | redacted continuity capsule over explicit evidence | handoff review skill may consume capsule | `handoff_unavailable` | create mutates capsule/audit; preview/inspect/resume read-only or explicit import | bounded redaction and DB read, cancellable | `eidetic_engine_cli-g9dq` redaction fixture | `ee.response.v1` | redaction plus e2e |
| `help`, `version`, `introspect`, `schema`, `mcp`, `model` | mechanical CLI or optional adapter wrapper | `eidetic_engine_cli-frv3`, ADR 0011 | discovery/schema | static metadata, model registry, optional MCP manifest | none | `adapter_unavailable` only for optional adapters | read-only, idempotent | immediate | skeleton goldens | `ee.response.v1` | golden |
| `init`, `workspace` | mechanical CLI | `eidetic_engine_cli-hy6y`, ADR 0011 | workspace setup | workspace filesystem, config, registry | none | `workspace_unavailable` | init/alias mutate idempotently; resolve/list read-only | bounded filesystem/DB init, cancellable | M1 storage/status gates | `ee.response.v1` | e2e |
| `import` | mechanical CLI | `eidetic_engine_cli-hy6y`, ADR 0011 | import history | CASS robot JSON, JSONL, legacy export files | none | `import_source_unavailable` | import mutates DB with idempotent source hashes | bounded import budget, cancellable | `eidetic_engine_cli-hy6y` import fixture | `ee.response.v1` | e2e plus fixture |
| `install`, `update` | mechanical CLI | `eidetic_engine_cli-hy6y`, ADR 0011 | install/update | local manifest, release metadata, filesystem checks | none | `install_manifest_unavailable` | plan/check read-only; update mutates only with explicit command semantics | bounded file/network-disabled plan, cancellable | install workflow tests | `ee.response.v1` | contract |
| `graph` | mechanical CLI | `eidetic_engine_cli-hy6y`, ADR 0011 | graph analytics | DB records projected through FrankenNetworkX | none | `graph_unavailable` | refresh mutates derived graph/index; export/neighborhood read-only | bounded graph budget, cancellable | graph smoke tests | `ee.response.v1` | unit plus smoke |
| `index` | mechanical CLI | `eidetic_engine_cli-hy6y`, ADR 0011 | index management | FrankenSQLite source of truth, Frankensearch derived index | none | `search_index_unavailable` | rebuild/reembed mutate derived index idempotently by generation; status read-only | cancellable runtime budget required | search/index smoke tests | `ee.response.v1` | e2e |
| `lab` | split | `eidetic_engine_cli-db4z`, ADR 0011 | failure lab | captured episodes, replay artifacts, counterfactual inputs | counterfactual failure-analysis skill | `lab_evidence_unavailable` | capture/replay mutate explicit artifacts; counterfactual report read-only or degraded | bounded replay budget, cancellable | `eidetic_engine_cli-db4z` lab fixture | `ee.response.v1` | e2e plus no-fake-reasoning |
| `learn` | split | `eidetic_engine_cli-evah`, ADR 0011 | active learning | observation ledgers, experiment registry, evaluation snapshots | experiment planner skill | `learning_ledger_unavailable` | observe/close/run mutate ledgers; agenda/uncertainty/summary read-only | bounded DB/eval budget, cancellable | `eidetic_engine_cli-evah` ledger fixture | `ee.response.v1` | contract plus mutation test |
| `memory`, `remember` | mechanical CLI | `eidetic_engine_cli-6956`, ADR 0011 | manual memory | memory DB records, revisions, provenance | none | `storage` | remember/revise mutate with audit; list/show/history read-only | bounded DB transaction, cancellable | walking skeleton and memory tests | `ee.response.v1` or `ee.error.v1` | unit plus e2e |
| `outcome` | mechanical CLI | `eidetic_engine_cli-s43e`, ADR 0011 | feedback/outcomes | feedback events, quarantine records, audit log | none | `outcome_store_unavailable` | record/quarantine release mutate with audit; list read-only | bounded DB transaction, cancellable | outcome goldens | `ee.response.v1` | contract |
| `preflight` | split | `eidetic_engine_cli-bijm`, ADR 0011 | pre-task risk review | evidence matches, stored preflight runs, tripwire records | preflight risk-review skill | `preflight_evidence_unavailable` | run/close mutate preflight ledger; show read-only | bounded evidence query, cancellable | preflight goldens | `ee.response.v1` | e2e plus no-fake-reasoning |
| `plan` | move to skill unless static lookup | `eidetic_engine_cli-6cks`, ADR 0011 | task planning | static recipe registry only if kept in Rust | situation/command-planning skill | `planning_skill_required` | read-only if static; otherwise unavailable | immediate static lookup | `eidetic_engine_cli-6cks` static recipe fixture | `ee.response.v1` with skill handoff | skill-boundary contract |
| `procedure` | split | `eidetic_engine_cli-q5vf`, ADR 0011 | procedure lifecycle | stored procedure records, eval fixtures, repro packs, claim evidence | procedure distillation skill | `procedure_evidence_unavailable` | propose/promote may mutate candidate/procedure records; show/list/export/verify/drift read-only | bounded verification budget, cancellable | `eidetic_engine_cli-q5vf` procedure fixture | `ee.response.v1` | contract plus fixture |
| `recorder` | mechanical CLI | `eidetic_engine_cli-6xzc`, ADR 0011 | session recorder | persisted event spine and import plan records | none | `recorder_store_unavailable` | start/event/finish/import mutate event store; tail read-only | streaming tail cancellable, bounded import | `eidetic_engine_cli-6xzc` recorder golden | `ee.response.v1` | e2e plus runtime |
| `rehearse` | degrade/unavailable pending implementation | `eidetic_engine_cli-nd65`, ADR 0011 | rehearsal | real dry-run sandbox artifacts when implemented | rehearsal/promotion review skill | `rehearsal_unavailable` | plan/inspect read-only; run mutates sandbox artifacts only when real | cancellable sandbox runtime required | `eidetic_engine_cli-nd65` rehearsal fixture | `ee.response.v1` or degraded `ee.error.v1` | no-fake-success e2e |
| `review` | split | `eidetic_engine_cli-0hjw`, ADR 0011 | session review | CASS session evidence and candidate records | session review/distillation skill | `review_evidence_unavailable` | read-only extraction unless explicit candidate write | bounded CASS/evidence read, cancellable | `eidetic_engine_cli-0hjw` session fixture | `ee.response.v1` | skill-boundary contract |
| `rule` | mechanical CLI | `eidetic_engine_cli-ynzg`, ADR 0011 | procedural rules | rule DB records, protection metadata, audit log | none | `rule_store_unavailable` | add/protect mutate with audit; list/show read-only | bounded DB transaction, cancellable | rule tests | `ee.response.v1` | unit plus contract |
| `situation` | move to skill or split deterministic tagging | `eidetic_engine_cli-6cks`, ADR 0011 | situation framing | persisted situation records and deterministic tag features only | situation framing skill | `situation_skill_required` | link may mutate deterministic relation records; classify/compare/show/explain read-only or degraded | bounded feature query, cancellable | `eidetic_engine_cli-6cks` situation fixture | `ee.response.v1` with skill handoff | skill-boundary contract |
| `support` | mechanical CLI | `eidetic_engine_cli-hy6y`, ADR 0011 | support bundle | redacted diagnostics, config, logs, manifests | none | `support_bundle_unavailable` | bundle create mutates artifact; inspect read-only | bounded redaction/file IO, cancellable | `eidetic_engine_cli-hy6y` support fixture | `ee.response.v1` | redaction plus e2e |
| `tripwire` | fix backing data | `eidetic_engine_cli-qmu0`, ADR 0011 | tripwire check | persisted rules/tripwires and explicit event payloads | preflight risk-review skill may interpret results | `tripwire_store_unavailable` | check read-only over explicit payload; list read-only | bounded rule evaluation, cancellable | tripwire goldens | `ee.response.v1` | contract plus fixture |

### Matrix Maintenance Rules

- Every command path in the inventory below must appear either directly or through its family in the
  matrix above.
- A row may say `none` for skill handoff only when the command remains fully mechanical.
- A row may say `none` for degraded code only when the command is static/read-only and cannot depend
  on absent local state.
- A row marked `move to skill` must include a project-local skill handoff and a skill-boundary test
  owner.
- A row marked `fix backing data`, `split`, or `degrade/unavailable pending implementation` must name a
  degraded code and a follow-up bead that owns the real implementation.

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
- stdout and stderr artifact paths;
- parsed JSON schema and golden validation result when a command is executed;
- first-failure diagnosis.

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
| `memory list`, `memory show`, `memory history`, `remember` | `src/cli/mod.rs:4284`, `src/cli/mod.rs:4471`, `src/core/memory.rs:164` | Memory DB reads/writes and audit history. | keep mechanical |
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
