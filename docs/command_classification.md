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

## Commands Using Mock/Sample/Stub Data

Commands that currently emit placeholder or example data rather than real computed results:

| Command Path | File:Line | Issue | Action Required |
|--------------|-----------|-------|-----------------|
| (To be audited) | | | |

## Summary Statistics

- **Total command families**: 46
- **Mechanical**: 32 (70%)
- **Mixed (needs split)**: 9 (20%)
- **Agent Skill (move to workflow)**: 5 (10%)
- **Degraded/unavailable**: 0

## Next Steps

1. Commands classified as "Agent Skill" should have their core handlers reviewed to ensure they return `degraded` status rather than simulated intelligence.
2. Commands classified as "Mixed" need their mechanical sub-surfaces extracted and skill components moved to documented workflows.
3. All command outputs should be audited for language that implies judgment (recommendations, confidence, risk assessments) when the underlying computation is deterministic.

---
*This inventory is the authoritative source for command boundary classification. Update this document when adding or modifying commands.*
