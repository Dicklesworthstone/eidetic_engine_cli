# Coordination Snapshot

Schema: `ee.coordination_snapshot.v1`

Coordination snapshots let context packs include a deterministic, redacted view
of Beads and Agent Mail state without requiring live services during pack
assembly. Agents provide a JSON snapshot path; `ee` reads it side-effect free.

Example:

```bash
ee context "next bead" --coordination-snapshot coordination.json --json | jq '.data.pack.coordination'
```

Related schemas: `ee.swarm.recommendation.v1`, `ee.trust_lane.v1`.

Non-goals: the snapshot is not a lock manager and does not send Agent Mail.

Tracking Bead: `bd-1zb7k.4`
