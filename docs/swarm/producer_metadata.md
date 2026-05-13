# Producer Metadata

Schema: `ee.producer.metadata.v1`

Producer metadata records who or what observed a durable fact. Agents use it to
separate human input, imported CASS evidence, verification records, and
unobserved CLI writes without assuming any one harness controls the work.

Example:

```bash
jq '.producer.schema' verification.json
```

Related schemas: `ee.verification.evidence.v1`, `ee.consensus.v1`.

Non-goals: producer metadata is provenance, not authorization and not a scheduler
identity registry.

Tracking Bead: `bd-1zb7k.1`
