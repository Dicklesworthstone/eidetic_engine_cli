# Agent Triad Compatibility Plan

Status: design plan for `bd-17c65.9.2`

Inputs:

- `bd-17c65.9.1` spike result: promote the triad.
- `tests/logs/active/triad_spike_outcome.json`: coverage `1.0`, `sloc_reduction` `0.4007`, `pack_hash_parity` true, `discoverability_pass` true.
- `bd-17c65.9.3` criteria: promote only when coverage, SLoC, inference, parity, and discoverability gates pass.

## Policy

No command is removed in the promotion milestone. The triad becomes the preferred agent-facing surface:

- `ee note "<text>"` for common memory capture.
- `ee pack "<task>"` for common retrieval plus context packing.
- `ee why <id>` for storage, retrieval, history, and link explanation.

Verbose commands stay available for explicit workflows, debugging, audit, and human operation. Commands marked `aliased` should emit a `degraded.deprecated_alias` entry in JSON responses after triad promotion. The degraded entry must include the replacement command and a repair hint, but the command must still execute its current behavior during the soft-deprecation period.

Deprecation window: keep aliases for at least two minor milestones after triad promotion. Re-evaluate before any removal.

## Disposition Table

| Command | Disposition | Compatibility behavior after triad promotion |
|---|---|---|
| `ee agent` | kept | Agent inventory and connector diagnostics remain explicit support surfaces. |
| `ee analyze` | kept | Diagnostic analysis remains a specialist surface. |
| `ee agent-docs` | kept | Long-form docs stay for onboarding and audits. |
| `ee audit` | kept | Audit inspection is not part of the triad common path. |
| `ee artifact` | kept | Artifact registration remains a narrow support surface. |
| `ee backup` | kept | Backup operations stay explicit and never alias to triad commands. |
| `ee capabilities` | kept | Capability discovery remains explicit. |
| `ee check` | kept | Quick posture checks remain explicit. |
| `ee certificate` | kept | Certificate inspection remains explicit. |
| `ee causal` | kept | Causal tracing remains an advanced surface. |
| `ee claim` | kept | Executable claim management remains explicit. |
| `ee context "<task>"` | aliased | Continue current behavior, emit `degraded.deprecated_alias` with replacement `ee pack "<task>"`. |
| `ee completion` | kept | Shell completion generation remains explicit. |
| `ee curate` | kept | Curation review and apply workflows remain explicit. |
| `ee diag` | kept | Diagnostics remain explicit. |
| `ee demo` | kept | Demo listing and verification remain explicit. |
| `ee db` | kept | Database inspection remains explicit. |
| `ee migrate` | kept | Schema migration remains explicit and never hidden behind triad. |
| `ee daemon` | kept | Daemon operation remains an advanced maintenance surface. |
| `ee doctor` | kept | Human/debug health checks remain explicit. |
| `ee maintenance` | kept | Maintenance jobs remain explicit. |
| `ee note "<text>"` | canonical | Preferred agent capture command. Keep gated until promotion, then make always available. |
| `ee job` | kept | Durable maintenance job history remains explicit. |
| `ee economy` | kept | Memory economics remains an advanced analysis surface. |
| `ee eval` | kept | Evaluation scenarios remain explicit. |
| `ee export` | kept | Export remains explicit. |
| `ee focus` | kept | Active-memory focus remains explicit. |
| `ee handoff` | kept | Handoff capsules remain explicit. |
| `ee health` | kept | Quick health verdict remains explicit. |
| `ee help` | kept | Help remains explicit. |
| `ee graph` | kept | Graph analytics remain explicit. |
| `ee init` | kept | Workspace setup remains explicit. |
| `ee import` | kept | Import remains explicit. |
| `ee install` | kept | Installation checks remain explicit. |
| `ee introspect` | kept | Command/schema introspection remains explicit. |
| `ee index` | kept | Index management remains explicit. |
| `ee lab` | kept | Counterfactual lab workflows remain explicit. |
| `ee learn` | kept | Learning agenda and uncertainty workflows remain explicit. |
| `ee memory` | kept | Detailed memory operations remain explicit. |
| `ee show <id>` | kept | Top-level detail alias remains a useful support shortcut. |
| `ee link ...` | kept | Top-level link alias remains a useful support shortcut. |
| `ee tag ...` | kept | Top-level tag alias remains a useful support shortcut. |
| `ee history <id>` | kept | Top-level history alias remains a useful support shortcut. |
| `ee mcp` | kept | Optional MCP adapter inspection remains explicit. |
| `ee model` | kept | Model registry inspection remains explicit. |
| `ee outcome` | kept | Feedback recording remains explicit. |
| `ee outcome-quarantine` | kept | Harmful-feedback quarantine review remains explicit. |
| `ee pack "<task>"` | canonical | Preferred agent context command. It remains byte-thin over `ee context`. |
| `ee pack build --query-file <path>` | kept | Query-file packing remains explicit for reproducible jobs. |
| `ee pack replay <pack-id>` | kept | Replay remains explicit for audit and debugging. |
| `ee pack diff <left> <right>` | kept | Diff remains explicit for audit and debugging. |
| `ee perf` | kept | Performance artifact comparison remains explicit. |
| `ee preflight` | kept | Risk assessment remains explicit. |
| `ee plan` | kept | Planner/recipe resolution remains explicit. |
| `ee playbook` | kept | Playbook extraction remains explicit. |
| `ee profile` | kept | Host profile configuration remains explicit. |
| `ee procedure` | kept | Procedure management remains explicit. |
| `ee recorder` | kept | Recorder operations remain explicit. |
| `ee rationale` | kept | Rationale trace operations remain explicit. |
| `ee rehearse` | kept | Rehearsal workflows remain explicit. |
| `ee remember "<text>"` | aliased | Continue current explicit-capture contract. Emit `degraded.deprecated_alias` with replacement `ee note "<text>"`; do not apply inference when invoking `remember`. |
| `ee review` | kept | Session review remains explicit. |
| `ee rule` | kept | Direct rule management remains explicit. |
| `ee schema` | kept | Schema listing/export remains explicit. |
| `ee search "<query>"` | kept | Fine-grained search remains useful outside the common pack path. |
| `ee situation` | kept | Situation analysis remains explicit. |
| `ee status` | kept | Readiness reporting remains explicit. |
| `ee support` | kept | Support bundle creation remains explicit. |
| `ee swarm` | kept | Swarm coordination snapshots remain explicit. |
| `ee task-frame` | kept | Durable task frames remain explicit. |
| `ee tripwire` | kept | Tripwire listing and checks remain explicit. |
| `ee verify` | kept | Verification evidence recording remains explicit. |
| `ee verification` | kept | Verification guidance remains explicit. |
| `ee version` | kept | Version reporting remains explicit. |
| `ee update` | kept | Update planning remains explicit. |
| `ee workspace` | kept | Workspace identity management remains explicit. |
| `ee workflow` | kept | Workflow lifecycle groups remain explicit. |
| `ee why <id>` | canonical | Keep the same command name; promoted output includes storage, retrieval, history, and links. |

## Adjacent Detail Surfaces

These subcommands stay because they expose detail that the triad intentionally summarizes:

| Command | Disposition | Reason |
|---|---|---|
| `ee memory show <id>` | kept | Full detail view for one memory. |
| `ee memory history <id>` | kept | Raw audit timeline. |
| `ee memory link <id>` | kept | Link listing and mutation. |
| `ee memory tags <id>` | kept | Tag mutation and inspection. |
| `ee memory list` | kept | Inventory and filtering. |
| `ee memory expire <id>` | kept | Explicit lifecycle mutation. |
| `ee memory revise <id>` | kept | Immutable revision workflow. |

## Deprecated Alias Envelope

When an aliased command returns JSON, append a degraded entry:

```json
{
  "code": "deprecated_alias",
  "severity": "low",
  "message": "`ee context` is a compatibility alias for the promoted triad command.",
  "repair": "Use `ee pack \"<task>\"`."
}
```

The alias must not change the command's existing semantic contract. For example, `ee remember` remains explicit: provided `--level`, `--kind`, and `--tags` values are honored exactly, and `ee note` inference is not applied behind the user's back.

## Promotion Checklist

1. Implemented in `bd-17c65.15`: remove the hidden gating behavior from `--experimental-triad`; keep the flag as a no-op compatibility flag for one milestone.
2. Implemented in `bd-17c65.15`: add `deprecated_alias` emission to `ee context` and `ee remember` JSON responses.
3. Keep `ee search` and all detail/debug surfaces because they serve workflows outside the common agent path.
4. Implemented in `bd-17c65.15`: update `ee --help` so `note`, `pack`, and `why` are the first agent-facing commands.
5. Re-run `scripts/e2e_overhaul/agent_triad.sh`; promotion remains blocked if `pack_hash_parity` or any promote condition fails.
