# Unsafe-Claim Plan (`ee.swarm.unsafe_claim_plan.v1`)

Tracking bead: `bd-1n3x1.16.1`. Status: contract-first;
`x-ee-status.shipped=false` until the planner implementation lands under the
`bd-1n3x1.16` follow-up beads.

The unsafe-claim plan is a companion projection for an
`ee.swarm.work_packet.claim_gate.v1` result where `safeToClaim=false`. It does
not replace the claim gate, weaken its invariants, or make an unsafe candidate
safe. Its job is to preserve the original gate evidence and turn that evidence
into a deterministic, redaction-safe set of advisory next steps.

## Source Gate Preservation

`sourceGate` carries the fields agents need from the original claim gate
without lossy summarization:

- `gateId`, `packetId`, `requestedCandidateId`, `selectedCandidateId`
- `verdict`, `safeToClaim`, `recommendedAction`, `recommendedSafeToClaim`
- `claimCommandAction`
- `unsafeReasons`, `staleReasons`, `degradedCodes`, `sourceRefs`
- `nextCommandActions`

For this schema, `sourceGate.safeToClaim` is pinned to `false` and
`sourceGate.claimCommandAction` is pinned to `null`. If a future gate returns a
safe claim, consumers should use the claim gate directly instead of routing it
through this unsafe-plan surface.

## Reason Taxonomy

`reasonGroups[]` groups raw gate blockers into stable categories. The current
taxonomy is:

| Category | Covers |
| --- | --- |
| `tracker_authority` | Beads or actionable-queue authority is stale, contradictory, or unavailable |
| `agent_mail_readiness` | Agent Mail roster, reservation, inbox, or durability evidence is not authoritative |
| `source_overlap` | Related work or file-surface collisions need coordination |
| `dirty_checkout` | Git/workspace dirt affects source authority or collision risk |
| `rch_proof_admission` | Remote proof admission is unavailable, blocked, or stale |
| `installed_binary_freshness` | Installed `ee` may not match source contracts |
| `reservation_conflict` | File reservation evidence blocks mutation |
| `bv_staleness` | BV timed out, returned stale evidence, or contradicted tracker authority |
| `recommendation_mismatch` | Requested candidate and packet recommendation do not align |
| `memory_source_drift` | Memory/source-drift probes are unavailable or non-authoritative |
| `resource_admission` | Resource profile admission recommends waiting or degradation |
| `action_suppression` | The gate suppressed claim actions or exposed only read-only actions |
| `unknown` | Future or unclassified gate reasons, preserved verbatim in bounded form |

Unknown future gate reasons must stay visible as `category=unknown` with
`preservesUnknown=true`. Consumers must not filter or reinterpret unknown
reasons as safe.

Deterministic ordering is part of the contract:

1. `reasonGroups[]` sort by the schema's `reasonCategory` enum order, then
   `groupId` ascending byte order.
2. `candidatePlans[]` sort by `candidateId` ascending byte order.
3. `plannerActions[]` sort by the schema's `plannerActionKind` enum order,
   then `actionId` ascending byte order.
4. `evidenceSources[]` sort by `sourceId` ascending byte order.

## Planner Actions

Planner actions are advisory only. The allowed `kind` values are:

- `inspect`
- `comment_template`
- `decompose_candidate`
- `alternate_candidate`
- `retry_with_snapshot`
- `wait_or_coordinate`
- `stop`

Every `plannerActions[]` entry sets `mutatesState=false` and
`advisoryOnly=true`. A `comment_template` can include a bounded
`bodyTemplate`, but the planner does not post Beads comments, send Agent Mail,
claim work, reserve files, stage git changes, run Cargo, launch RCH proof, or
delete files. Separate human or agent commands perform any mutation only after
fresh authority says it is safe.

## Redaction

`redactionStatus` is pinned to
`counts_ids_statuses_path_patterns_command_templates_no_mail_body_no_file_content`.
Plans may include bounded ids, statuses, path-pattern summaries, degraded codes,
source refs, and command templates. They must not include raw mail bodies, raw
stdout/stderr, raw diffs, private absolute paths, environment dumps, secrets,
or unbounded file content.

## Fixtures

The fixture entry in
`tests/fixtures/swarm_schemas/all_examples.json` covers:

- a representative `unsafe_due_to_conflict` source gate
- grouped tracker authority, Agent Mail readiness, source overlap, dirty
  checkout, RCH, BV, and unknown blockers
- read-only planner and command actions
- unknown reason preservation
- `claimCommandAction=null` and `mayEmitClaimCommand=false`

The lifecycle test
`tests/swarm_schema_lifecycle.rs` (`unsafe_claim_plan_contract_pins_reason_taxonomy_and_non_mutation`)
keeps the schema, fixture, taxonomy, ordering, redaction, and non-mutation
rules aligned.

## Non-goals

- The unsafe-claim plan does not authorize a claim.
- It does not mutate Beads, Agent Mail, git, reservations, or the EE store.
- It does not replace `ee.swarm.work_packet.claim_gate.v1`; it preserves and
  explains unsafe gate results.
- It does not use local Cargo as a fallback when RCH proof is unavailable.
