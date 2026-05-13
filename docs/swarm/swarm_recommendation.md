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

Tracking Bead: `bd-2nkbn`
