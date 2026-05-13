# Pack SLO

Schema: `ee.pack.slo.v1`

Pack SLO records compare actual pack work against the selected resource profile.
Agents use them to tell whether a pack was within budget, merely warning, or
failed a resource constraint.

Example:

```bash
ee context "release" --json | jq '.data.pack.slo'
```

Related schemas: `ee.resource.profile.v1`, `ee.coordination_snapshot.v1`.

Non-goals: SLO output is a report, not a retry loop or remote execution policy.

Tracking Bead: `bd-1zb7k.5`
