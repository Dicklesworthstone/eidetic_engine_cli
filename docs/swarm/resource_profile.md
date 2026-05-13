# Resource Profile

Schema: `ee.resource.profile.v1`

Resource profiles name the budget class used by pack assembly: `lean`,
`standard`, or `swarm_heavy`. Agents use them to select predictable local
resource envelopes before launching expensive searches or packs.

Example:

```bash
ee context "handoff" --resource-profile swarm_heavy --json | jq '.data.pack.slo.profile'
```

Related schemas: `ee.pack.slo.v1`, `ee.trust_lane.v1`.

Non-goals: resource profiles are not host auto-scaling instructions and do not
start remote workers.

Tracking Bead: `bd-1zb7k.5`
