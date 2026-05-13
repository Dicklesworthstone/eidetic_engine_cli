# Verification Evidence

Schema: `ee.verification.evidence.v1`

Verification evidence records the command, status, offload posture, artifacts,
and producer for a closure gate. Agents should prefer this ledger over prose
claims when deciding whether a Bead can be closed.

Example:

```bash
ee verification ingest --stdin --json < verification-record.json
```

Related schemas: `ee.producer.metadata.v1`, `ee.swarm.recommendation.v1`.

Non-goals: a command invocation alone is not proof. Remote-required gates that
fall back locally remain explicit non-passing evidence.

Tracking Bead: `bd-1zb7k.3`
