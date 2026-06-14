# Swarm Repair Plan (`ee.swarm.repair_plan.v1`)

Tracking bead: `bd-22po3.1`. Status: shipped with the read-only
`ee swarm repair-plan --json` producer.

The repair plan is the shared degraded-stack recovery contract for crowded
agent sessions. It consumes the same evidence as `ee swarm work-packet` and its
claim gate, then emits a deterministic list of advisory repair actions. It
does not make an unsafe claim safe; it explains which substrate needs repair
or refreshed evidence before a fresh claim gate can authorize mutation.

## Source Gate Preservation

`sourceGate` carries the decision facts from the claim gate without raw logs:

- `gateId`, `packetId`, `requestedCandidateId`, `selectedCandidateId`
- `verdict`, `safeToClaim`, `recommendedAction`, `recommendedSafeToClaim`
- reason counts, degraded codes, and whether a claim command was suppressed

If `sourceGate.safeToClaim=false`, the repair plan must not expose a Beads
claim command as an action. Consumers rerun `ee swarm work-packet --claim-gate`
after repair evidence changes, then follow that fresh gate.

## Action Vocabulary

The action vocabulary is stable and ordered by schema enum order:

| Kind | Safety class | Purpose |
| --- | --- | --- |
| `wait_for_rch_build` | `read_only_or_wait` | Wait for remote proof capacity or run read-only RCH lane probes. |
| `message_holder` | `coordination_mutation` | Ask the current holder or owner for a handoff; no source mutation. |
| `repair_agent_mail_archive` | `external_repair` | Describe Agent Mail archive repair that must happen outside ee. |
| `rerun_snapshot` | `read_only_probe` | Regenerate redacted coordination or memory-drift evidence. |
| `refresh_bv_bounded` | `read_only_probe` | Refresh BV only through robot-safe triage; BV remains advisory. |
| `inspect_beads_doctor` | `read_only_probe` | Inspect tracker authority without importing or claiming. |
| `rerun_claim_gate` | `read_only_probe` | Recompute claim authority after evidence changes. |
| `ask_human_for_destructive_repair` | `human_approval_required` | Stop and request explicit approval for destructive or policy-suppressed repairs. |

Every `actions[]` item includes a `safety` block with mutation booleans,
copy-safety, human-approval requirement, preflight requirement, and execution
boundary. Read-only command actions may be copied as structured argv.
Coordination or external repair actions are display/manual steps unless a
future contract gives them a safe execution boundary.

## Source Evidence

`sourceEvidence[]` is a bounded projection of the source-authority snapshot:
one row per source kind, sorted by `sourceKind`. Rows include state,
authoritative flag, freshness, timeout, exit class, bounded detail, and related
degraded codes. The contract intentionally keeps raw Agent Mail bodies, raw
stdout/stderr, raw diffs, and private absolute paths out of the plan.

## Stop Conditions

Repair plans carry stop conditions so agents do not improvise around failed
authority:

- `fresh_claim_gate_safe_to_claim`: stop repairing and use the fresh claim
  gate when it returns `safeToClaim=true`.
- `source_authority_fail_closed`: repair evidence first; do not claim through
  failed-closed source authority.
- `no_source_verdict_without_rch_cargo`: RCH admission failure is not a source
  verdict unless RCH reached Cargo.
- `human_approval_required_before_destructive_repair`: stop before destructive
  or external repair unless the human approves the exact action.
- `agent_mail_or_tracker_not_authoritative`: do not claim while Agent Mail or
  tracker evidence is non-authoritative.

## Non-Mutation Policy

`nonMutationPolicy` is pinned to advisory-only behavior. The producer must not
claim Beads, reserve files, send Agent Mail, mutate tracker state, run Cargo,
stage Git, delete files, or execute repairs. A repair plan can describe a
manual or external repair, but it cannot perform it.

## Non-goals

- The repair plan does not authorize a claim.
- It does not replace `ee.swarm.work_packet.claim_gate.v1`.
- It does not repair Agent Mail archives, Beads metadata, RCH workers, memory
  drift locks, or git state.
- It does not post Beads comments, send Agent Mail, create reservations, or
  close beads.
- It does not run local Cargo as a fallback for remote-required proof.
- It does not carry raw mail bodies, raw logs, raw diffs, secrets, or private
  absolute paths.
