# Swarm Fixture Corpus

Schema: `ee.swarm_fixture_corpus.v1`

The fixture corpus manifest is the planned contract for deterministic swarm load
fixtures. Agents will use it to reproduce memory counts, graph sizes, and seed
families when testing 64+ core scale behavior.

Example:

```bash
jq '.profiles[].memoryCount' tests/fixtures/swarm_schemas/all_examples.json
```

Related schemas: `ee.resource.profile.v1`, `ee.pack.slo.v1`.

Non-goals: this schema is documentation for a future corpus surface; it is not
available in the current build.

Tracking Bead: `bd-1zb7k.6`
