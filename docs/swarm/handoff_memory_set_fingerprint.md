# Handoff Memory Set Fingerprint

Schema: `ee.handoff.memory_set_fingerprint.v1`

The memory-set fingerprint is the planned handoff proof for exactly which
memories were included in a capsule or resume context. Agents will use it to
detect stale handoffs and pack mismatches.

Example:

```bash
jq '.memorySetHash' handoff-fingerprint.json
```

Related schemas: `ee.producer.metadata.v1`, `ee.pack.slo.v1`.

Non-goals: this schema is not shipped yet and should not be treated as emitted
runtime output.

Tracking Bead: `bd-17c65.13.5`
