# Conflict

Schema: `ee.conflict.v1`

Conflict records identify packed memories that disagree directly, replace stale
guidance, or partially overlap in a risky way. Agents use the recommended action
to decide whether to review, promote, tombstone, or ask for clarification.

Example:

```bash
ee context "cargo policy" --json | jq '.data.conflicts'
```

Related schemas: `ee.consensus.v1`, `ee.trust_lane.v1`.

Non-goals: conflict output does not mutate memories and does not pick a winner
without review.

Tracking Bead: `bd-1zb7k.9`
