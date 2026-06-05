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
| `regress explain` | `models::regression_causality` + CLI capsule builder | Mechanical | explicit JSON artifact files | Read-only regression-causality capsule; hashes inputs and emits deterministic hypotheses without opening the DB |
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

Diagnostic sub-surface note: `diag environment-attestation` is classified as
Mechanical. It is read-only and daemon-optional; it reads explicit workspace
diagnostic state, Beads/BV readiness, Agent Mail probe or redacted snapshot
state, RCH/build-admission posture, source-tree status, and file-reservation
metadata. It must not claim Beads, reserve files, send Agent Mail, run Cargo,
rebuild binaries, mutate git, or mutate the EE store. Qualitative interpretation
belongs to an agent skill that consumes the JSON report.

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
| `audit timeline/show/diff/verify` | `src/core/audit.rs` + `audit_log` | Generated/sample audit operation data could look persisted. | Reads persisted audit rows and verifies the chain-hashed audit log; implemented by `eidetic_engine_cli-ar5o`. |
| `support bundle/inspect` | `src/cli/mod.rs` support bundle handlers | Placeholder bundle paths and unconditional inspection success. | Creates and inspects redacted diagnostic bundles; DH-05 guards against placeholder bundle output. |
| `certificate list/show/verify` | `src/core/certificate.rs` certificate reports | Mock certificate validity or hash verification. | Uses DB/manifest-backed records where available and otherwise returns honest empty/not-found reports; DH-06 guards against mock verification success. |
| `claim list/show/verify` | `src/core/claims.rs` + `.ee/claims.yaml` | Empty placeholder claim lists and zero-result verification. | Parses executable claim records and verifies file-hash, command-exit, memory-presence, and rule-status evidence without mutating source records. |
| `diag quarantine` | `src/cli/mod.rs` quarantine handlers | Empty placeholder trust-state posture could look healthy. | Reports persisted quarantine state and source counts; DH-08 guards against placeholder health output. |
| `rehearse plan/run/inspect/promote-plan` | `src/core/rehearse.rs` sandbox artifacts | Simulated plan/run IDs and sandbox artifact success. | Prepares real workspace snapshots/artifacts or honest unavailable/degraded JSON; DH-09 guards against degraded stub success. |
| `learn agenda/uncertainty/summary/experiment propose/run` | `src/core/learn.rs` learning ledgers | Hard-coded learning templates and experiment proposals. | Reads persisted learning ledgers for reports and uses explicit missing-proposal errors for run paths; DH-10 guards against removed learning sentinels. |
| `lab capture/replay/counterfactual` | `src/cli/mod.rs` lab handlers + `src/core/lab.rs` evidence reports | Generated replay/counterfactual success without episode evidence. | Emits evidence-only capture metadata, missing-frozen-input replay reports, and hypothesis-only counterfactual pack diffs; follow-up `eidetic_engine_cli-db4z`. |
| `economy report/score/simulate/prune-plan` | `src/core/economy.rs` DB-backed metrics | Static seed metrics could look workspace-backed. | Reads persisted memory/feedback rows, returns conservative abstain reports for empty metrics, and uses `unsatisfied_degraded_mode` for missing or unreadable storage. Implemented by `eidetic_engine_cli-ve0w`. |
| `causal trace/estimate/compare/promote-plan` | `src/core/causal.rs` + `causal_evidence` | Fixture causal chains, uplift, and confidence claims. | Reads persisted causal evidence edges; missing evidence returns an empty/degraded report instead of generated chains. Implemented by `eidetic_engine_cli-hnrm`. |
| `procedure propose/show/list/export/promote/verify/drift` | `src/core/procedure.rs` procedure store | Generated lifecycle/procedure fixture records. | Uses persisted procedure records and verification evidence gates; DH-14 guards read-only list output against the removed store-unavailable sentinel. |
| `situation show/explain` | `src/core/situation.rs` `SITUATION_DECISIONING_UNAVAILABLE_CODE` | Missing persisted situation storage could be misreported as an ordinary missing ID. | Returns `situation_decisioning_unavailable`; implementation follow-up `bd-14tio`. |
| `plan goal/explain` | `src/core/plan.rs` recipe catalog reasoning | Built-in goal classification and recipe reasoning. | Emits catalog-backed recommendations/explanations without plan mutation; DH-16 guards against unavailable-sentinel regressions. |
| `preflight run/show/close` | `src/cli/mod.rs` `PREFLIGHT_UNAVAILABLE_CODE` | Task-text heuristics and generated preflight run state. | Returns `preflight_evidence_unavailable`; follow-up `eidetic_engine_cli-bijm`. |
| `tripwire list/check` | `src/cli/mod.rs` tripwire store queries | Generated tripwire samples could look persisted. | Reads persisted tripwire state and explicit event payloads; DH-19 guards against the removed `tripwire_store_unavailable` sentinel. |
| `eval run/list` | `src/cli/mod.rs` fixture runner | No-scenario stub success. | Reads deterministic fixture metadata and emits `ee.eval.report.v1` metrics or fixture lists; DH-17 guards against the removed `eval_fixtures_unavailable` sentinel. |
| `review session --propose` | `src/cli/mod.rs` review storage path | Empty generated curation proposal set. | Fails with storage errors when evidence is absent instead of generating proposals; DH-18 guards against the removed review sentinel. |
| `handoff create` | `src/core/handoff.rs` redacted capsule writer | Placeholder continuity capsule creation. | Writes redacted continuity capsules to explicit side-path artifacts and records effect metadata; handoff no-mock E2E and effect contracts guard against placeholder capsules and the removed sentinel. |
| `daemon` | `src/daemon/` foreground health job | Simulated scheduler ticks and processed item counts. | Foreground mode runs a real bounded health job and reports build-time capability gaps separately; DH-21 guards against unavailable-sentinel regressions. |
| `recorder start/event/finish` | `src/core/recorder.rs` recorder store | Generated run/event IDs. | Persists recorder runs/events and finalization state; DH-22 guards against generated recorder IDs. |
| `recorder tail/follow` | `src/core/recorder.rs` + V027 `recorder_events` | Read-only persisted recorder event stream. | Reads real events with deterministic tail windows and JSONL follow output; implemented by `eidetic_engine_cli-qow7`. |
| `demo list/run/show/verify` | `src/cli/mod.rs` demo handlers + `audit_log` | Empty timestamped demo placeholders. | `run --no-dry-run` executes safe manifest steps, writes per-step audit rows and evidence artifacts; `list`/`show` read persisted runs; `verify` checks declared artifact evidence. Implemented by `eidetic_engine_cli-z58u`. |

Executable coverage lives primarily in `tests/degraded_honesty.rs`, with
supporting unit and contract coverage in `src/models/demo.rs` and
`tests/contracts/demo_manifests.rs` for the real demo manifest/artifact slice.

## Summary Statistics

- **Total command families**: 47
- **Mechanical**: 33 (70%)
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
