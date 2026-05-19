# Swarm Recommendation

Schema: `ee.swarm.recommendation.v1`

Swarm recommendations are read-only suggestions from `ee swarm brief`. Agents
use them to pick next actions, notice unavailable coordination sources, and keep
forbidden actions visible in machine-readable output.

Example:

```bash
ee swarm brief --json | jq '.data.recommendations[0]'
```

Related schemas: `ee.coordination_snapshot.v1`, `ee.verification.evidence.v1`.

Non-goals: recommendations do not claim work, close Beads, reserve files, or send
Agent Mail.

## Next-Action Recommendation Cards

`ee swarm next-action --json` emits `data.recommendationCards[]` under schema
`ee.swarm_next_action.v1`. A card is a read-only explanation for why the next
action should create a new bead, refine an existing bead, reject a duplicate, or
coordinate with another owner first.

When an agent uses a card to start or close work, cite these fields in the Beads
comment, Agent Mail handoff, or closeout:

- `cardId`
- `decision`
- `candidateId`
- `candidateSource`
- `confidence`
- `overlap.matchedExistingBeads`
- `overlap.rejectedDuplicateReason`
- `proofObligations`
- `evidenceCaveats`
- `fallbackDecision`

Example Beads closeout fragment:

```text
Recommendation card: cardId=refine_existing_bead:bd-3vwx0.9,
decision=refine_existing_bead, candidateId=bd-3vwx0.9,
matchedExistingBeads=[bd-3vwx0.9],
proofObligations=[record_overlap_decision_in_closeout,
reserve_files_before_editing, use_rch_for_cargo_verification],
evidenceCaveats=[].
```

The card is advisory evidence, not proof that work was performed. Agents still
need separate file reservations, verification output, and Beads or Agent Mail
updates. If `decision=refine_existing_bead`, continue the named bead and record
the overlap decision instead of opening a duplicate. If
`decision=duplicate_rejected`, do not create a new bead unless a human explicitly
overrides the card with new evidence.

Tracking Bead: `bd-2nkbn`
