# Swarm Brief

Schema: `ee.swarm.brief.v1`

`ee swarm brief --json` emits a read-only coordination posture report for a
crowded checkout. The ownership posture surface is `fileSurfaceRisks[]`, which
keeps raw file contents and mail bodies out of the report while exposing the
path pattern, git status bucket, reservation holder, related Bead IDs, risk
factors, evidence labels, severity, score, and suggested coordination commands.

Versioning: field renames or incompatible ownership-risk semantics require a
new schema version and a migration note. Additive fields may remain in
`ee.swarm.brief.v1` only when existing consumers can safely ignore them.

Redaction rules: paths, reservation subjects, counts, Bead IDs, status buckets,
and command labels are allowed. Mail bodies, raw logs, raw memory content, env
dumps, file contents, and secret-like tokens are not allowed.

Example:

```bash
ee swarm brief --json | jq '.data.fileSurfaceRisks'
```

Fixture catalog: `tests/fixtures/swarm/ownership_posture_cases.json` covers the
healthy, degraded-source, and unattributed-blocker cases that downstream agents
should handle.

Related schemas: `ee.support_bundle.swarm_brief_summary.v1`,
`ee.swarm.recommendation.v1`, `ee.coordination_snapshot.v1`.

Non-goals: swarm brief does not claim work, reserve files, mutate Beads, send
Agent Mail, or run verification.

Tracking Bead: `bd-1zb7k.16.4`
