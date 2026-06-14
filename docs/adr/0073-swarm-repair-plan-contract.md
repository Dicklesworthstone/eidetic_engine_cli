# ADR 0073: Swarm Repair Plan Contract

Status: Accepted

Date: 2026-06-14

Tracking bead: `bd-22po3.1`

## Context

Crowded swarm sessions often fail closed before implementation work can start:
the actionable Beads queue can be empty, BV can contradict tracker authority,
Agent Mail can be green at transport level but non-authoritative for inbox or
reservation semantics, RCH can be remote-ready but not admitted for a specific
proof, and memory-drift evidence can be blocked by local lock contention.

`ee.swarm.work_packet.claim_gate.v1` answers whether a candidate is safe to
claim. `ee.swarm.unsafe_claim_plan.v1` explains one unsafe claim gate. Neither
surface defines a shared recovery vocabulary for the broader degraded stack.
Agents were therefore repeating ad hoc wording for actions such as retrying a
snapshot, refreshing BV, waiting for RCH, inspecting Beads metadata, and asking
for human approval before destructive repair.

## Decision

Introduce `ee.swarm.repair_plan.v1` and a read-only CLI producer:

```bash
ee swarm repair-plan --workspace . --include-rch --candidate <bead> --json
```

The repair plan is derived from a work packet and claim gate. It preserves the
gate summary, projects bounded source evidence, defines the full repair action
vocabulary, emits ordered advisory actions, and carries stop conditions plus a
non-mutation policy.

The initial vocabulary is:

- `wait_for_rch_build`
- `message_holder`
- `repair_agent_mail_archive`
- `rerun_snapshot`
- `refresh_bv_bounded`
- `inspect_beads_doctor`
- `rerun_claim_gate`
- `ask_human_for_destructive_repair`

Every action has an explicit safety class. Read-only probes may expose
structured argv. Coordination, external repair, and human-only actions are
manual/display guidance unless a future contract gives them a separate safe
execution path.

## Consequences

Agents get one contract for degraded-stack recovery wording and can distinguish
read-only probes from coordination mutations, external repairs, tracker
mutations, and human-only destructive approvals.

The CLI remains harness-agnostic and side-effect-free. The plan can suggest
read-only inspection commands, but it cannot mutate Beads, Agent Mail, git,
RCH, or the workspace.

Future planner work (`bd-22po3.2`) can consume this schema instead of inventing
new action names or safety classes. Support-bundle and handoff rendering can
quote this plan without repeating raw logs.

## Non-goals

- Do not make an unsafe claim safe.
- Do not replace the claim gate.
- Do not execute repairs.
- Do not post comments, send Agent Mail, claim Beads, reserve files, close
  beads, stage Git, delete files, or run Cargo.
- Do not use local Cargo when remote proof is required.
- Do not include raw mail bodies, raw stdout/stderr, raw diffs, secrets, or
  private absolute paths.
