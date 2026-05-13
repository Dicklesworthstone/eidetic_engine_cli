# Consensus

Schema: `ee.consensus.v1`

Consensus records identify multiple memories that support the same subject.
Agents use them as compact evidence that a pack contains reinforced knowledge
rather than a single isolated assertion.

Example:

```bash
ee context "verification policy" --json | jq '.data.consensus'
```

Related schemas: `ee.conflict.v1`, `ee.producer.metadata.v1`.

Non-goals: consensus is not truth by vote; provenance and freshness still matter.

Tracking Bead: `bd-1zb7k.9`
