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

If live Agent Mail evidence is missing, `ee swarm brief` and
`ee swarm work-packet` can consume a redacted `ee.agent_mail.snapshot.v1` file
with `--agent-mail-snapshot`. The brief reports coordination posture only. A
snapshot-backed claim decision still belongs to
`ee swarm work-packet --claim-gate --candidate <id> --agent-mail-snapshot <path> --json`;
the snapshot is read-only evidence and never a reservation, claim, or closeout
authorization by itself.

## Adaptive Posture

SRR5 adaptive scheduling is a daemon/swarm concern, not a pack-budget concern.
When swarm brief reports adaptive evidence, it must stay read-only and
redaction-safe: per-agent labels, percentiles, advisory backoff status,
prefetch counters, and degraded codes are allowed; raw task text, query text,
CASS evidence, mail bodies, command output, env dumps, and absolute host paths
are not.

The current `ee.swarm.brief.v1` schema has no required dedicated adaptive
object. Until an additive field lands, consumers should read SRR5 posture from
existing `degraded[]`, `sources[]`, and `recommendations[]` entries. The
canonical degraded codes are `adaptive_backoff_applied` and
`cass_prefetch_budget_exceeded`; both are advisory signals and must not be
treated as failed retrieval, failed pack assembly, or proof that local Cargo is
acceptable.

Fixture catalog: `tests/fixtures/swarm/ownership_posture_cases.json` covers the
healthy, degraded-source, and unattributed-blocker cases that downstream agents
should handle.

Related schemas: `ee.support_bundle.swarm_brief_summary.v1`,
`ee.swarm.recommendation.v1`, `ee.coordination_snapshot.v1`.

Non-goals: swarm brief does not claim work, reserve files, mutate Beads, send
Agent Mail, or run verification.

Tracking Bead: `bd-1zb7k.16.4`
