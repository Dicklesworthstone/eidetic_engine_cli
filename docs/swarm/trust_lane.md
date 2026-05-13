# Trust Lane

Schema: `ee.trust_lane.v1`

Trust lanes describe how many candidate memories survived a requested scope such
as `self`, `team`, `workspace`, `verified`, or `swarm`. Agents use the counts to
detect when a pack is intentionally narrow instead of treating empty results as
global absence.

Example:

```bash
ee context "release gates" --memory-scope verified --strict-scope --json | jq '.data.scopeStats'
```

Related schemas: `ee.producer.metadata.v1`, `ee.pack.slo.v1`.

Non-goals: trust lanes do not rewrite memory confidence and do not hide
provenance.

Tracking Bead: `bd-1zb7k.2`
