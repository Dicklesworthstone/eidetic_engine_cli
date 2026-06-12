# Source-Authority Snapshot (`ee.source_authority.snapshot.v1`)

Tracking bead: `bd-3w4pv.1` (contract) / `bd-3w4pv.2` (read-only
collectors). Status: collector-shipped; claim-gate and unsafe-plan integration
lands under `bd-3w4pv.4`.

The source-authority snapshot is the deterministic, redaction-safe record of
**what each coordination source actually said** when a claim gate or
unsafe-claim planner made a decision. It exists because crowded checkouts kept
collapsing distinct failure classes — a Beads read timing out, a stale-safe
fallback answering, an Agent Mail store in corrupt-recovery — into a single
ambiguous `candidate_not_found` or `source_unavailable`, which made fail-closed
behavior impossible to audit and impossible to test.

## Sources

Every snapshot carries one record per source, sorted by `sourceKind` ascending
byte order:

| `sourceKind` | What it covers |
| --- | --- |
| `actionable_queue` | The safe claimable-leaf queue: `scripts/br_retry.sh actionable --json` (bd-3w4pv.7) |
| `agent_mail` | Reservations, inbox, roster, durability/recovery posture |
| `beads` | Tracker rows, ready queue, claim-candidate lookups |
| `bv` | Graph-aware triage ranking (advisory) |
| `git` | Branch, dirty-state, HEAD/origin divergence |
| `host_profile` | Resource pressure and host-class posture |
| `installed_binary` | Installed `ee` freshness vs source contract |
| `memory_drift` | EE memory vs tracker/coordination drift probes |
| `rch` | Remote-verification admission and proof posture |
| `support_bundle` | Optional embedded handoff/support-bundle evidence |
| `workspace_hygiene` | Dirty-path classification posture |

A source that was not consulted still appears with `state=unavailable` and a
`statusDetail` explaining why. Absence of a record is never meaningful.

## Collector projection

The shipped collector is the work-packet projection in
`src/core/swarm_next_action.rs`:
`collect_source_authority_snapshot_for_work_packet` runs the existing
read-only work-packet collectors, and
`SwarmWorkPacket::source_authority_snapshot(<candidate>)` projects the already
collected evidence into `ee.source_authority.snapshot.v1` without spawning any
additional Beads, BV, Agent Mail, RCH, Cargo, git, or memory commands.

The projection always emits the full source vector above. Existing
`swarm brief` evidence supplies Beads, BV, Agent Mail, RCH, git, memory drift,
host profile, and workspace hygiene posture; the work-packet actionable-queue
probe supplies `sourceKind=actionable_queue`; the offline install-freshness
check supplies `sourceKind=installed_binary`; optional support-bundle evidence
is represented as unavailable when not supplied. Per-source command budget,
timeout, fallback, and repair fields are data in the snapshot, not reasons to
drop the source record.

## Actionable queue (`scripts/br_retry.sh actionable --json`)

Tracking bead: `bd-3w4pv.7`. AGENTS.md/README establish
`scripts/br_retry.sh actionable --json` as the **safe claimable-leaf queue**
(open, unassigned, non-epic rows served through the transient-read retry
guard), while raw `br ready` and `bv --robot-next` are broader advisory views
(live evidence class: BV recommended blocked `bd-37ugy` with a copy-paste
claim command while `br` showed it blocked). The queue is therefore modeled
as its own first-class source instead of being folded into generic Beads
health.

`sourceKind=actionable_queue` records carry an `actionableQueue` extension
block: command id (`beads_actionable_queue`), command template, row count,
bounded sorted candidate ids (max 32, with `truncatedCandidateCount`), and
the static filter contract flags (`excludesEpics`, `excludesAssigned`,
`excludesBlocked`, `excludesDeferred`, `excludesInProgress`). Budget,
timeout, exit class, freshness, and fallback semantics stay on the shared
`sourceRecord` fields — that per-source granularity is why the queue is a
new `sourceKind` rather than a sub-object on the `beads` record.

The claim gate (`ee.swarm.work_packet.claim_gate.v1`, `actionableQueue`
field) consumes this evidence and emits candidate-conditional states:

| State | Meaning |
| --- | --- |
| `candidate_present_actionable` | Queue ready and contains the candidate (necessary, not sufficient) |
| `candidate_absent_from_actionable` | Queue ready and confirms absence |
| `actionable_queue_unavailable` | Spawn/parse failure or skipped; no queue answer exists |
| `actionable_queue_timed_out` | Budget exhausted; absence NOT confirmed |
| `actionable_queue_stale_fallback` | Only degraded/stale fallback evidence answered; advisory |
| `bv_advisory_contradiction` | BV recommends an id Beads marks blocked or absent from the queue |
| `tracker_authority_degraded` | Queue evaluated while tracker reads were not authoritative |

### Precedence rules

The rules below are implemented in
`src/core/swarm_next_action.rs` (`work_packet_actionable_queue_allows_claim`,
`work_packet_actionable_queue_blocking_verdict`) and pinned by
`tests/contracts/swarm_work_packet_claim_gate_conformance.rs`:

1. **Actionable presence is necessary but not sufficient.** A candidate in
   the queue still needs tracker authority, Agent Mail reservation
   evidence, RCH posture, and the conflict gate to agree before
   `claimCommandAction` is emitted.
2. **BV recommendations are advisory** unless the id is in the actionable
   queue AND the gate passes. Contradictions surface as bounded id-only
   evidence (`bv_recommends_blocked_id:<id>`,
   `bv_recommends_id_absent_from_actionable_queue:<id>`), never claims.
3. **Raw `br ready` never overrides the actionable queue or gate safety.**
   Rows the queue excludes appear only in the exclusion accounting
   (`rawReadyCount` plus epic/assigned/blocked/deferred/in-progress/other
   counts).
4. **Timeout is not absence** here either: a timed-out queue read keeps the
   distinct `timed_out` state and fails closed.
5. `claimCommandAction` stays `null` unless actionable queue, source
   authority, reservations, and the gate all agree.

Workspaces that do not ship the script fall back to applying the same
filter contract in-process to the brief's fresh `br ready` rows
(`collectionMode=brief_ready_filter`); the fallback is marked
`stale_fallback` (advisory) when the underlying Beads read was itself
degraded or stale. Collection is strictly read-only: the only command the
collector may spawn is the queue probe, pinned by the
`no_mutation_read_only` fixture.

## Source-state taxonomy

| `state` | Meaning | Authoritative? |
| --- | --- | --- |
| `ready` | Live read succeeded inside budget | Yes (unless contradicted) |
| `degraded_read_only` | Source answered but must not authorize mutation | No |
| `unavailable` | Absent, unreachable, or skipped; no evidence captured | No |
| `timed_out` | Budget exhausted before an answer | No |
| `stale_fallback` | Live read failed; bounded stale-safe snapshot answered | No (advisory) |
| `corrupt_recovery` | Store present but integrity-failed or mid-recovery | No |
| `contradicted` | Evidence conflicts with another source, unresolved | No |

Two invariants the taxonomy enforces:

1. **Timeout is not absence.** `timed_out` must never be collapsed into
   `unavailable`, and a candidate lookup that timed out must never be reported
   as `candidate_absent_confirmed`.
2. **Stale fallback is not live truth.** Evidence served from a stale-safe
   snapshot keeps `fallback.active=true` and the source stays
   non-authoritative, even when the fallback contains the candidate.

## Candidate evidence

When the snapshot is taken for a specific claim candidate, `candidateEvidence`
distinguishes the cases consumers historically conflated:

| `lookupOutcome` | Meaning |
| --- | --- |
| `candidate_present` | An authoritative source confirms the candidate |
| `candidate_absent_confirmed` | Every authoritative source answered; none has it |
| `candidate_lookup_unavailable` | The sources that could answer were unavailable |
| `candidate_lookup_timed_out` | Budget exhausted; absence NOT confirmed |
| `candidate_stale_fallback_only` | Only stale-safe fallback evidence has it |
| `candidate_contradicted` | Authoritative sources disagree |

`staleFallbackPresence` records candidate presence in fallback evidence
separately from live presence, so "present in stale-safe Beads but missing from
the live claim-gate packet because Beads timed out" is representable — and the
fixture `tests/fixtures/source_authority/candidate_beads_timeout.json` pins
exactly that case.

## Determinism and redaction

- Producers sort `sources` by `sourceKind`, evidence ids and contradictions by
  id, ascending byte order, before writing. Identical inputs produce identical
  snapshots (`provenanceHash` is the replay/dedup key).
- `redactionStatus` is pinned to `paths_counts_subjects_only_no_content`: the
  snapshot keeps states, counts, budgets, exit classes, evidence ids, hashes,
  and repair-command templates. It must not contain raw mail bodies, raw memory
  bodies, host-private absolute paths (`/Users/...`, `/home/...`), captured
  argv, or environment dumps. Repair commands are templates (for example
  `scripts/br_retry.sh actionable --json`), never replayed captures.

## Fixtures

| Fixture | Pins |
| --- | --- |
| `tests/fixtures/source_authority/all_source_states.json` | Every `state` value across the sources |
| `tests/fixtures/source_authority/candidate_beads_timeout.json` | Candidate in stale-safe Beads, live lookup timed out, gate fails closed |
| `tests/fixtures/source_authority/redaction_proof.json` | Redaction posture: forbidden content classes absent |
| `tests/fixtures/swarm_work_packet/actionable_queue/present_but_gate_refuses.json` | Queue presence necessary but not sufficient; claim stays null |
| `tests/fixtures/swarm_work_packet/actionable_queue/bv_advisory_contradiction.json` | BV recommends a blocked id absent from the queue |
| `tests/fixtures/swarm_work_packet/actionable_queue/ready_epic_exclusion.json` | Exclusion accounting + filter contract golden |
| `tests/fixtures/swarm_work_packet/actionable_queue/failure_states.json` | Distinct spawn/timeout/parse/stale failure states fail closed |
| `tests/fixtures/swarm_work_packet/actionable_queue/no_mutation_read_only.json` | Collection issues only the read-only queue probe |

The contract test `tests/swarm_schema_lifecycle.rs`
(`source_authority_snapshot_contract_covers_source_state_taxonomy`) keeps the
schema, the taxonomy, and the fixtures from drifting.

## Non-goals

- The snapshot does not decide claims; the claim gate consumes it.
- It does not replace `ee.source_run_evidence.v1` (per-command run evidence);
  it aggregates per-source authority for one decision point.
- It never mutates Beads, Agent Mail, git, or the EE store.
