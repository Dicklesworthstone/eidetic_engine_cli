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

Verbose commands stay available for explicit workflows, debugging, audit, and human operation. `ee pack "<task>"` is the canonical context-pack surface. `ee context "<task>"` is retained only as a soft-deprecated compatibility alias routed through the same `run_context_pack` engine, and emits an info-severity `deprecated_alias` degraded entry so agent harnesses can migrate intentionally.

## Disposition Table

| Command | Disposition | Compatibility behavior after triad promotion |
|---|---|---|
| `ee agent` | kept | Agent inventory and connector diagnostics remain explicit support surfaces. |
| `ee analyze` | kept | Diagnostic analysis remains a specialist surface. |
| `ee agent-docs` | kept | Long-form docs stay for onboarding and audits. |
| `ee ask` | kept | Deterministic extractive question answering with citations and honest abstention (ADR 0067). Remains available as an explicit surface. |
| `ee attest` | kept | Emit redaction-safe local provenance attestation bundles. Remains available as an explicit surface. |
| `ee audit` | kept | Audit inspection is not part of the triad common path. |
| `ee artifact` | kept | Artifact registration remains a narrow support surface. |
| `ee backup` | kept | Backup operations stay explicit and never alias to triad commands. |
| `ee bootstrap` | kept | Compile docs into reviewable bootstrap candidates. Remains available as an explicit surface. |
| `ee cache` | kept | Derived cache inspection and explicit prewarm planning. Remains available as an explicit surface. |
| `ee capabilities` | kept | Capability discovery remains explicit. |
| `ee capture` | kept | Suggest high-value memories from session evidence without storing them. Remains available as an explicit surface. |
| `ee check` | kept | Quick posture checks remain explicit. |
| `ee certificate` | kept | Certificate inspection remains explicit. |
| `ee causal` | kept | Causal tracing remains an advanced surface. |
| `ee claim` | kept | Executable claim management remains explicit. |
| `ee config` | kept | Inspect and update workspace configuration. Remains available as an explicit surface. |
| `ee conflict` | kept | Surface memory contradictions (read-only): list / explain / cluster. Remains available as an explicit surface. |
| `ee context "<task>"` | soft-deprecated alias | Runs the identical `run_context_pack` engine as canonical `ee pack "<task>"` and emits an info-severity `deprecated_alias` degraded entry. Prefer `ee pack` in new scripts, harnesses, and docs. |
| `ee completion` | kept | Shell completion generation remains explicit. |
| `ee context-show` | kept | Retrieve a previously persisted context pack by ID. Remains available as an explicit surface. |
| `ee coordination` | kept | Persist redaction-safe coordination fallback evidence. Remains available as an explicit surface. |
| `ee curate` | kept | Curation review and apply workflows remain explicit. |
| `ee decide` | kept | Record, list, and revisit durable typed decision memories. Remains available as an explicit surface. |
| `ee diag` | kept | Diagnostics remain explicit. |
| `ee demo` | kept | Demo listing and verification remain explicit. |
| `ee db` | kept | Database inspection remains explicit. |
| `ee diagnose-error` | kept | Diagnose a tool error against the fingerprint recall store (error-recall). Remains available as an explicit surface. |
| `ee hook` | kept | Generate agent-harness memory-context and advisory helpers. Remains available as an explicit surface. |
| `ee impact` | kept | Find memories attached to a path, symbol, command, env var, or schema. Remains available as an explicit surface. |
| `ee insights` | kept | Bundle read-only operational insight sections for agents. Remains available as an explicit surface. |
| `ee journal` | kept | Append-only agent observation journal (append, list, show). Remains available as an explicit surface. |
| `ee lens` | kept | Inspect task lens policy overlays. Remains available as an explicit surface. |
| `ee mesh` | kept | Foreground local mesh operations for peers, status, export/import, and sync-once. Remains available as an explicit surface. |
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
| `ee orient` | kept | Read-only orientation bundle for an agent starting a task. Remains available as an explicit surface. |
| `ee primer` | kept | Deterministic cached workspace charter from highest-value memory (ADR 0065). Remains available as an explicit surface. |
| `ee proof` | kept | Read-only proof-broker admission and status decisions. Remains available as an explicit surface. |
| `ee proximity` | kept | Report pairwise min-cut proximity between two memories. Remains available as an explicit surface. |
| `ee recall` | kept | Code-anchored memory recall: reverse lookup from paths, symbols, or a git diff to anchored memories (ADR 0064). Remains available as an explicit surface. |
| `ee reflect` | kept | Create and inspect external reflection request handshakes. Remains available as an explicit surface. |
| `ee regress` | kept | Explain likely regression causes from existing structured artifacts. Remains available as an explicit surface. |
| `ee resume` | kept | Where was I — recent sessions, open decisions, queued work, staleness flags. Remains available as an explicit surface. |
| `ee sandbox` | kept | What-If Memory Sandbox (no durable mutation): remember / import / curate / diff. Remains available as an explicit surface. |
| `ee sentinel` | kept | Attach, explain, and run deterministic memory sentinel checks. Remains available as an explicit surface. |
| `ee serve` | kept | Report localhost HTTP/SSE adapter availability. Remains available as an explicit surface. |
| `ee session-budget` | kept | Opt-in session-budget ledger planning and diagnostics. Remains available as an explicit surface. |
| `ee shadow` | kept | Execute shadowable policy evaluators offline (side-effect-free). Remains available as an explicit surface. |
| `ee share` | kept | Preview and consent-check outbound mesh sharing. Remains available as an explicit surface. |
| `ee show <id>` | kept | Top-level detail alias remains a useful support shortcut. |
| `ee link ...` | kept | Top-level link alias remains a useful support shortcut. |
| `ee similar` | kept | Find memories semantically similar to a selected memory. Remains available as an explicit surface. |
| `ee subscribe` | kept | Subscribe to memory change deltas by cursor or foreground stream. Remains available as an explicit surface. |
| `ee tag ...` | kept | Top-level tag alias remains a useful support shortcut. |
| `ee history <id>` | kept | Top-level history alias remains a useful support shortcut. |
| `ee mcp` | kept | Optional MCP adapter inspection remains explicit. |
| `ee model` | kept | Model registry inspection remains explicit. |
| `ee outcome` | kept | Feedback recording remains explicit. |
| `ee outcome-quarantine` | kept | Harmful-feedback quarantine review remains explicit. |
| `ee pack "<task>"` | canonical | Preferred agent context command. It remains byte-thin over the shared context-pack engine. |
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
| `ee remember "<text>"` | kept | Continue current explicit-capture contract without alias degradation; `ee note` remains the inference wrapper. |
| `ee review` | kept | Session review remains explicit. |
| `ee rule` | kept | Direct rule management remains explicit. |
| `ee schema` | kept | Schema listing/export remains explicit. |
| `ee search "<query>"` | kept | Fine-grained search remains useful outside the common pack path. |
| `ee situation` | kept | Situation analysis remains explicit. |
| `ee status` | kept | Readiness reporting remains explicit. |
| `ee support` | kept | Support bundle creation remains explicit. |
| `ee swarm` | kept | Swarm coordination snapshots remain explicit. |
| `ee task-frame` | kept | Durable task frames remain explicit. |
| `ee team` | kept | Team confederation over mesh primitives. Remains available as an explicit surface. |
| `ee timeline` | kept | Reconstruct what was known about a topic at an RFC3339 as-of time. Remains available as an explicit surface. |
| `ee tripwire` | kept | Tripwire listing and checks remain explicit. |
| `ee trust` | kept | Audit memory confidence calibration and outcome-backed reliability. Remains available as an explicit surface. |
| `ee verify` | kept | Verification evidence recording remains explicit. |
| `ee verification` | kept | Verification guidance remains explicit. |
| `ee version` | kept | Version reporting remains explicit. |
| `ee update` | kept | Update planning remains explicit. |
| `ee workspace` | kept | Workspace identity management remains explicit. |
| `ee workflow` | kept | Workflow lifecycle groups remain explicit. |
| `ee why <id>` | canonical | Keep the same command name; promoted output includes storage, retrieval, history, and links. |
| `ee why-not <id> --task "<task>"` | new | Counterfactual reverse of `ee why`: explains why a memory was not selected for a task's context pack (read-only, no pack record persisted). |

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

## Alias Compatibility

The `ee context` compatibility alias remains available as a soft-deprecated
surface only. It emits a `deprecated_alias` degraded entry while executing the
same retrieval and packing behavior as canonical `ee pack "<task>"`. New agent
harnesses, scripts, docs, and examples should use `ee pack`.

Other explicit commands keep their existing semantic contract. For example, `ee remember` remains explicit: provided `--level`, `--kind`, and `--tags` values are honored exactly, and `ee note` inference is not applied behind the user's back.

## Promotion Checklist

1. Implemented in `bd-17c65.15`: remove the hidden gating behavior from `--experimental-triad`; keep the flag as a no-op compatibility flag for one milestone.
2. Implemented in `bd-17c65.15`, revised by `bd-2xdom.2`, then restored for agent ergonomics: the `ee context` alias remains soft-deprecated and emits `deprecated_alias`; keep `ee remember` quiet because canonical docs still present it as explicit capture.
3. Keep `ee search` and all detail/debug surfaces because they serve workflows outside the common agent path.
4. Implemented in `bd-17c65.15`: update `ee --help` so `note`, `pack`, and `why` are the first agent-facing commands.
5. Re-run `scripts/e2e_overhaul/agent_triad.sh`; promotion remains blocked if `pack_hash_parity` or any promote condition fails.
