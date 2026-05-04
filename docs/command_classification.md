# EE Command Classification Inventory

Generated: 2026-05-03
Bead: eidetic_engine_cli-i6vu

## Classification Categories

| Category | Definition |
|----------|------------|
| **Mechanical** | Deterministic computation over explicit inputs, persisted DB rows, local files, generated indexes, frozen fixtures, hashes, schemas, and graph projections. No LLM required. |
| **Agent Skill** | Qualitative synthesis, task planning, procedure authoring, causal interpretation, preflight questioning, learning experiment design. Requires intelligence around evidence. |
| **Mixed** | Command must be split into mechanical Rust sub-surface plus project-local skill workflow. |
| **Degraded** | Command shape remains but returns stable degraded JSON until honest implementation exists. |

## Top-Level Command Inventory

| Command | Handler Module | Classification | Data Sources | Notes |
|---------|----------------|----------------|--------------|-------|
| `agent detect` | `core::agent_detect` | Mechanical | filesystem | Scans paths for agent installations |
| `agent status` | `core::agent_detect` | Mechanical | DB, filesystem | Reports agent inventory |
| `agent sources` | `core::agent_detect` | Mechanical | static config | Lists known agent connectors |
| `agent scan` | `core::agent_detect` | Mechanical | filesystem | Probe path enumeration |
| `agent-docs` | `core::agent_docs` | Mechanical | static docs | Documentation renderer |
| `analyze *` | `core::analyze` | Mechanical | DB, indexes | Subsystem readiness metrics |
| `artifact *` | `core::artifact` | Mechanical | DB, filesystem | File registration/inspection |
| `audit *` | `core::audit` | Mechanical | DB | Audit timeline read-only |
| `backup *` | `core::backup` | Mechanical | DB, filesystem | Backup create/verify/inspect |
| `capabilities` | `core::capabilities` | Mechanical | static/DB | Feature availability report |
| `certificate *` | `core::certificate` | Mechanical | DB | Certificate records |
| `check` | `core::check` | Mechanical | DB | Posture summary |
| `context` | `core::context` | Mechanical | DB, indexes | Context pack assembly |
| `daemon` | `core::steward` | Mechanical | n/a | Daemon runner |
| `demo *` | `core::demo` | Mechanical | fixtures | Demo execution |
| `diag *` | `core::diag` | Mechanical | DB, runtime | Diagnostics |
| `doctor` | `core::doctor` | Mechanical | DB, filesystem | Health checks |
| `eval *` | `core::eval` | Mechanical | DB, fixtures | Evaluation scenarios |
| `graph *` | `core::graph` | Mechanical | DB, graph projection | Graph analytics/export |
| `handoff *` | `core::handoff` | Mechanical | DB | Session capsules |
| `health` | `core::health` | Mechanical | DB | Quick health verdict |
| `help` | clap | Mechanical | static | Help text |
| `import *` | `core::import` | Mechanical | external sources | Import evidence |
| `index *` | `core::index` | Mechanical | DB, indexes | Index management |
| `init` | `core::init` | Mechanical | filesystem | Workspace initialization |
| `install *` | `core::install` | Mechanical | filesystem | Installation checks |
| `introspect` | `core::introspect` | Mechanical | static | Command/schema maps |
| `memory *` | `core::memory` | Mechanical | DB | Memory show/list/history |
| `mcp *` | `core::mcp` | Mechanical | DB, config | MCP adapter inspection |
| `model *` | `core::model` | Mechanical | DB | Model registry |
| `outcome` | `core::outcome` | Mechanical | DB | Record feedback |
| `outcome-quarantine *` | `core::quarantine` | Mechanical | DB | Quarantine review |
| `pack` | `core::pack` | Mechanical | DB, indexes | Context pack from query doc |
| `recorder *` | `core::recorder` | Mechanical | DB, filesystem | Activity recording |
| `remember` | `core::remember` | Mechanical | DB | Store memory |
| `rule *` | `core::rule` | Mechanical | DB | Procedural rule management |
| `schema *` | `core::schema` | Mechanical | static | Schema list/export |
| `search` | `core::search` | Mechanical | DB, indexes | Search memories |
| `status` | `core::status` | Mechanical | DB | Workspace readiness |
| `support *` | `core::support` | Mechanical | DB, filesystem | Diagnostic bundles |
| `update` | `core::update` | Mechanical | filesystem | Update planning |
| `version` | static | Mechanical | static | Version info |
| `workspace *` | `core::workspace` | Mechanical | DB, filesystem | Workspace management |
| `why` | `core::why` | Mechanical | DB | Explain storage/retrieval |

## Commands Requiring Careful Boundary Review

These commands have names or descriptions suggesting agent-skill work but may be implementable mechanically:

| Command | Classification | Boundary Concern | Recommended Disposition |
|---------|----------------|------------------|-------------------------|
| `causal *` | **Mixed** | Traces causal chains - the projection is mechanical, but "credit assignment" language suggests interpretation | Split: mechanical projection + skill interpretation |
| `claim *` | **Mixed** | "Executable claims" storage is mechanical; claim validation may need judgment | Split: storage mechanical, validation skill |
| `curate *` | **Agent Skill** | "Review proposals" requires judgment about what to promote | Move to skill workflow |
| `economy *` | **Mixed** | Utility/attention math is mechanical; "debt" interpretation may need judgment | Keep mechanical with documented thresholds |
| `lab *` | **Agent Skill** | "Counterfactual" reasoning requires intelligence | Move to skill workflow |
| `learn *` | **Agent Skill** | "Active learning agenda" and "experiment design" are intelligence tasks | Move to skill workflow |
| `plan *` | **Agent Skill** | "Goal planner" and "recipe resolver" require task understanding | Move to skill workflow |
| `preflight *` | **Mixed** | Risk data collection is mechanical; risk "assessment" wording implies judgment | Split: data collection mechanical, risk language skill |
| `procedure *` | **Agent Skill** | "Distilled procedures" and "skill capsules" are synthesis tasks | Move to skill workflow |
| `rehearse *` | **Mixed** | Sandbox execution is mechanical; choosing what to rehearse is skill | Keep mechanical (execution only) |
| `review *` | **Agent Skill** | "Propose curation candidates" requires judgment | Move to skill workflow |
| `situation *` | **Mixed** | Classification storage is mechanical; "explain" and "compare" may need interpretation | Split: storage mechanical, explanation skill |
| `tripwire *` | **Mixed** | Tripwire matching is mechanical; "risk" language in output needs review | Keep mechanical, audit output language |

## Degraded-Honesty Migration Status

The inventory above records each command family's intended boundary. The table
below records command paths that previously emitted, or were at risk of
emitting, placeholder/example/stubbed data as if it were real production output.
These paths now either return stable degraded JSON or expose a narrowed
mechanical sub-surface with concrete evidence.

| Command Path | Current Code Anchor | Prior Risk | Current Contract / Follow-Up |
|--------------|---------------------|------------|------------------------------|
| `audit timeline/show/diff/verify` | `src/cli/mod.rs` `AUDIT_UNAVAILABLE_CODE` | Generated/sample audit operation data could look persisted. | Returns `audit_log_unavailable`; follow-up `eidetic_engine_cli-s43e`. |
| `support bundle/inspect` | `src/cli/mod.rs` `SUPPORT_BUNDLE_UNAVAILABLE_CODE` | Placeholder bundle paths and unconditional inspection success. | Returns `support_bundle_unavailable`; follow-up `eidetic_engine_cli-5g6d`. |
| `certificate list/show/verify` | `src/cli/mod.rs` `CERTIFICATE_STORE_UNAVAILABLE_CODE` | Mock certificate validity or hash verification. | Returns `certificate_store_unavailable`; follow-up claim/certificate manifest work. |
| `claim list/show/verify` | `src/cli/mod.rs` `CLAIM_UNAVAILABLE_CODE` | Empty placeholder claim lists and zero-result verification. | Returns `claim_verification_unavailable`; follow-up `eidetic_engine_cli-v76q`. |
| `diag quarantine` | `src/cli/mod.rs` `DIAG_QUARANTINE_UNAVAILABLE_CODE` | Empty placeholder trust-state posture could look healthy. | Returns `quarantine_trust_state_unavailable`; follow-up `eidetic_engine_cli-5g6d`. |
| `rehearse plan/run/inspect/promote-plan` | `src/cli/mod.rs` `REHEARSAL_UNAVAILABLE_CODE` | Simulated plan/run IDs and sandbox artifact success. | Returns `rehearsal_unavailable`; follow-up `eidetic_engine_cli-nd65`. |
| `learn agenda/uncertainty/summary/experiment propose/run` | `src/cli/mod.rs` `LEARN_UNAVAILABLE_CODE` | Hard-coded learning templates and experiment proposals. | Returns `learning_records_unavailable`; follow-up `eidetic_engine_cli-evah`. |
| `lab capture/replay/counterfactual` | `src/cli/mod.rs` lab handlers + `src/core/lab.rs` evidence reports | Generated replay/counterfactual success without episode evidence. | Emits evidence-only capture metadata, missing-frozen-input replay reports, and hypothesis-only counterfactual pack diffs; follow-up `eidetic_engine_cli-db4z`. |
| `economy report/score/simulate/prune-plan` | `src/cli/mod.rs` `ECONOMY_UNAVAILABLE_CODE` | Static seed metrics could look workspace-backed. | Returns `economy_metrics_unavailable`; follow-up `eidetic_engine_cli-ve0w`. |
| `causal trace/estimate/compare/promote-plan` | `src/cli/mod.rs` `CAUSAL_UNAVAILABLE_CODE` | Fixture causal chains, uplift, and confidence claims. | Returns `causal_evidence_unavailable`; follow-up `eidetic_engine_cli-dz00`. |
| `procedure propose/show/list/export/promote/verify/drift` | `src/cli/mod.rs` `PROCEDURE_UNAVAILABLE_CODE` | Generated lifecycle/procedure fixture records. | Returns `procedure_store_unavailable`; follow-up `eidetic_engine_cli-q5vf`. |
| `situation classify/compare/link/show/explain` | `src/cli/mod.rs` `SITUATION_UNAVAILABLE_CODE` | Built-in routing fixture IDs could look like real situation evidence. | Returns `situation_decisioning_unavailable`; follow-up `eidetic_engine_cli-6cks`. |
| `plan goal/explain` | `src/cli/mod.rs` `PLAN_DECISIONING_UNAVAILABLE_CODE` | Built-in goal classification and recipe reasoning. | Returns `plan_decisioning_unavailable`; recipe list/show remain mechanical catalog reads. |
| `preflight run/show/close` | `src/cli/mod.rs` `PREFLIGHT_UNAVAILABLE_CODE` | Task-text heuristics and generated preflight run state. | Returns `preflight_evidence_unavailable`; follow-up `eidetic_engine_cli-bijm`. |
| `tripwire list/check` | `src/cli/mod.rs` `TRIPWIRE_UNAVAILABLE_CODE` | Generated tripwire samples could look persisted. | Returns `tripwire_store_unavailable`; follow-up `eidetic_engine_cli-qmu0`. |
| `eval run/list` | `src/cli/mod.rs` `EVAL_UNAVAILABLE_CODE` | No-scenario stub success. | Returns `eval_fixtures_unavailable`; follow-up `eidetic_engine_cli-uiy3`. |
| `review session --propose` | `src/cli/mod.rs` `REVIEW_UNAVAILABLE_CODE` | Empty generated curation proposal set. | Returns `review_evidence_unavailable`; follow-up CASS evidence import/review work. |
| `handoff create` | `src/cli/mod.rs` `HANDOFF_UNAVAILABLE_CODE` | Placeholder continuity capsule creation. | Returns `handoff_unavailable`; follow-up `eidetic_engine_cli-g9dq`. |
| `daemon` | `src/cli/mod.rs` `DAEMON_UNAVAILABLE_CODE` | Simulated scheduler ticks and processed item counts. | Returns `daemon_jobs_unavailable`; follow-up `eidetic_engine_cli-5g6d`. |
| `recorder start/event/finish/tail` | `src/cli/mod.rs` recorder unavailable handlers | Generated run/event IDs and stubbed empty tail state. | Returns `recorder_store_unavailable` or `recorder_tail_unavailable`; follow-up `eidetic_engine_cli-6xzc`. |
| `demo list/run/verify` | `src/cli/mod.rs` demo handlers | Empty timestamped demo placeholders. | `list`, `run --dry-run`, and `verify` parse real `demo.yaml` / artifact evidence; non-dry-run execution returns `demo_command_execution_unavailable`; follow-up `eidetic_engine_cli-jp06.1`. |

Executable coverage lives primarily in `tests/degraded_honesty.rs`, with
supporting unit and contract coverage in `src/models/demo.rs` and
`tests/contracts/demo_manifests.rs` for the real demo manifest/artifact slice.

## Summary Statistics

- **Total command families**: 46
- **Mechanical**: 32 (70%)
- **Mixed (needs split)**: 9 (20%)
- **Agent Skill (move to workflow)**: 5 (10%)
- **Degraded/unavailable public command families with active contracts**: 21
  (overlaps the classification totals above; this is migration state, not a
  separate classification bucket)

## Next Steps

1. Keep `tests/degraded_honesty.rs` authoritative for public false-success regressions.
2. Close or update follow-up beads as each degraded command is backed by real persisted evidence or a narrowed mechanical contract.
3. All new command output must be audited for language that implies judgment, verification, persistence, replay, or validity when the underlying computation has not produced concrete evidence.

---
*This inventory is the authoritative source for command boundary classification. Update this document when adding or modifying commands.*
